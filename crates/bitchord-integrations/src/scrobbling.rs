use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bitchord_model::{Song, duration_millis};
use md5::{Digest, Md5};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;

pub const LASTFM_DEFAULT_ENDPOINT: &str = "https://ws.audioscrobbler.com/2.0/";
pub const LIBREFM_API_ENDPOINT: &str = "https://libre.fm/2.0/";
pub const LISTENBRAINZ_SUBMIT_URL: &str = "https://api.listenbrainz.org/1/submit-listens";

pub const DEFAULT_SCROBBLE_DELAY_PERCENT: f32 = 0.5;
pub const DEFAULT_SCROBBLE_MIN_SONG_DURATION_SECS: u32 = 30;
pub const DEFAULT_SCROBBLE_DELAY_SECONDS: u32 = 180;

const MIN_LISTEN_TS: i64 = 1_033_430_400;
const USER_AGENT: &str = "BitChord (https://github.com/kushagrasinghx/BitChord)";

static PRIMARY_ARTIST_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*,\s*|\s+&\s+|\s+＆\s+")
        // INFALLIBLE: static regex pattern.
        .expect("valid PRIMARY_ARTIST_SEPARATOR regex")
});

/// Returns the first credited artist for scrobbling services that reject joint
/// artist strings while preserving names such as `"AC/DC"`.
pub fn primary_artist(artist: &str) -> &str {
    let first = PRIMARY_ARTIST_SEPARATOR
        .splitn(artist, 2)
        .next()
        .unwrap_or(artist)
        .trim();
    if first.is_empty() { artist } else { first }
}

#[derive(Debug, Error)]
pub enum ScrobbleError {
    #[error("Last.fm API error {code}: {message}")]
    LastFmApi { code: i32, message: String },
    #[error("invalid endpoint URL: {0}")]
    InvalidEndpoint(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("decode error: {0}")]
    Decode(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastFmSession {
    pub name: String,
    pub key: String,
    pub subscriber: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastFmAuthResponse {
    pub session: LastFmSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastFmTokenResponse {
    pub token: String,
}

#[derive(Debug, Deserialize)]
struct LastFmApiErrorBody {
    error: i32,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmConfig {
    pub endpoint: String,
    pub api_key: String,
    pub secret: String,
    pub session_key: Option<String>,
}

impl Default for LastFmConfig {
    fn default() -> Self {
        Self {
            endpoint: LASTFM_DEFAULT_ENDPOINT.to_owned(),
            api_key: String::new(),
            secret: String::new(),
            session_key: None,
        }
    }
}

/// Validates and normalizes a Last.fm / Libre.fm / GNU FM API endpoint URL.
pub fn normalize_lastfm_endpoint(endpoint: &str) -> Result<String, ScrobbleError> {
    let trimmed = endpoint.trim();
    let parsed = Url::parse(trimmed).map_err(|e| ScrobbleError::InvalidEndpoint(e.to_string()))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(ScrobbleError::InvalidEndpoint(
            "scheme must be http or https".to_owned(),
        ));
    }
    if parsed.host_str().is_none_or(|h| h.is_empty()) {
        return Err(ScrobbleError::InvalidEndpoint("missing host".to_owned()));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ScrobbleError::InvalidEndpoint(
            "endpoint must not include query or fragment".to_owned(),
        ));
    }
    let path = match parsed.path() {
        "" | "/" => "/2.0/".to_owned(),
        p => format!("{}/", p.trim_end_matches('/')),
    };
    let mut normalized = parsed;
    normalized.set_path(&path);
    Ok(normalized.into())
}

/// Computes Last.fm's MD5 `api_sig` over alphabetically sorted key+value pairs
/// followed by `secret`.
pub fn lastfm_api_sig(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut input = String::new();
    for (k, v) in params {
        input.push_str(k);
        input.push_str(v);
    }
    input.push_str(secret);
    let digest = Md5::digest(input.as_bytes());
    let mut hex = String::with_capacity(32);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Last.fm / Libre.fm scrobbling client.
pub struct LastFmClient {
    http: Client,
    config: RwLock<LastFmConfig>,
}

impl Default for LastFmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LastFmClient {
    pub fn new() -> Self {
        Self::with_client(Client::new())
    }

    pub fn with_client(http: Client) -> Self {
        Self {
            http,
            config: RwLock::new(LastFmConfig::default()),
        }
    }

    pub fn configure(
        &self,
        endpoint: &str,
        api_key: impl Into<String>,
        secret: impl Into<String>,
        session_key: Option<String>,
    ) -> Result<(), ScrobbleError> {
        let normalized = normalize_lastfm_endpoint(endpoint)?;
        let mut cfg = self.config.write().unwrap_or_else(|e| e.into_inner());
        cfg.endpoint = normalized;
        cfg.api_key = api_key.into();
        cfg.secret = secret.into();
        cfg.session_key = session_key;
        Ok(())
    }

    pub fn set_session_key(&self, session_key: Option<String>) {
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .session_key = session_key;
    }

    pub fn config(&self) -> LastFmConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn is_initialized(&self) -> bool {
        let cfg = self.config();
        !cfg.api_key.is_empty() && !cfg.secret.is_empty()
    }

    pub fn auth_url(&self, token: &str) -> String {
        let cfg = self.config();
        if cfg.endpoint == LIBREFM_API_ENDPOINT {
            format!(
                "https://libre.fm/api/auth?api_key={}&token={token}",
                cfg.api_key
            )
        } else {
            format!(
                "https://www.last.fm/api/auth/?api_key={}&token={token}",
                cfg.api_key
            )
        }
    }

    pub async fn get_token(&self) -> Result<LastFmTokenResponse, ScrobbleError> {
        let text = self.post_signed("auth.getToken", None, &[]).await?;
        serde_json::from_str(&text).map_err(|e| ScrobbleError::Decode(e.to_string()))
    }

    pub async fn get_session(&self, token: &str) -> Result<LastFmAuthResponse, ScrobbleError> {
        let text = self
            .post_signed("auth.getSession", None, &[("token", token)])
            .await?;
        serde_json::from_str(&text).map_err(|e| ScrobbleError::Decode(e.to_string()))
    }

    pub async fn get_mobile_session(
        &self,
        username: &str,
        password: &str,
    ) -> Result<LastFmAuthResponse, ScrobbleError> {
        let text = self
            .post_signed(
                "auth.getMobileSession",
                None,
                &[("username", username), ("password", password)],
            )
            .await?;
        serde_json::from_str(&text).map_err(|e| ScrobbleError::Decode(e.to_string()))
    }

    pub async fn update_now_playing(
        &self,
        artist: &str,
        track: &str,
        album: Option<&str>,
        track_number: Option<u32>,
        duration_secs: Option<u32>,
    ) -> Result<String, ScrobbleError> {
        let sk = self.require_session_key()?;
        let mut owned: Vec<(&str, String)> =
            vec![("artist", artist.to_owned()), ("track", track.to_owned())];
        if let Some(alb) = album {
            owned.push(("album", alb.to_owned()));
        }
        if let Some(num) = track_number {
            owned.push(("trackNumber", num.to_string()));
        }
        if let Some(dur) = duration_secs {
            owned.push(("duration", dur.to_string()));
        }
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.post_signed("track.updateNowPlaying", Some(&sk), &borrowed)
            .await
    }

    pub async fn scrobble(
        &self,
        artist: &str,
        track: &str,
        timestamp_secs: i64,
        album: Option<&str>,
        track_number: Option<u32>,
        duration_secs: Option<u32>,
    ) -> Result<String, ScrobbleError> {
        let sk = self.require_session_key()?;
        let mut owned: Vec<(&str, String)> = vec![
            ("artist[0]", artist.to_owned()),
            ("track[0]", track.to_owned()),
            ("timestamp[0]", timestamp_secs.to_string()),
        ];
        if let Some(alb) = album {
            owned.push(("album[0]", alb.to_owned()));
        }
        if let Some(num) = track_number {
            owned.push(("trackNumber[0]", num.to_string()));
        }
        if let Some(dur) = duration_secs {
            owned.push(("duration[0]", dur.to_string()));
        }
        let borrowed: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.post_signed("track.scrobble", Some(&sk), &borrowed)
            .await
    }

    fn require_session_key(&self) -> Result<String, ScrobbleError> {
        self.config()
            .session_key
            .ok_or_else(|| ScrobbleError::LastFmApi {
                code: 9,
                message: "Session key missing".to_owned(),
            })
    }

    async fn post_signed(
        &self,
        method: &str,
        session_key: Option<&str>,
        extra: &[(&str, &str)],
    ) -> Result<String, ScrobbleError> {
        let cfg = self.config();
        let mut params = BTreeMap::new();
        params.insert("method".to_owned(), method.to_owned());
        params.insert("api_key".to_owned(), cfg.api_key.clone());
        if let Some(sk) = session_key {
            params.insert("sk".to_owned(), sk.to_owned());
        }
        for &(k, v) in extra {
            params.insert(k.to_owned(), v.to_owned());
        }
        let sig = lastfm_api_sig(&params, &cfg.secret);

        let mut form: Vec<(String, String)> = params.into_iter().collect();
        form.push(("api_sig".to_owned(), sig));
        form.push(("format".to_owned(), "json".to_owned()));

        let resp = self
            .http
            .post(&cfg.endpoint)
            .timeout(Duration::from_secs(15))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .form(&form)
            .send()
            .await
            .map_err(|e| ScrobbleError::Transport(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ScrobbleError::Transport(e.to_string()))?;

        if !status.is_success() {
            return Err(ScrobbleError::LastFmApi {
                code: i32::from(status.as_u16()),
                message: status.canonical_reason().unwrap_or("HTTP error").to_owned(),
            });
        }
        if let Ok(err) = serde_json::from_str::<LastFmApiErrorBody>(&text) {
            return Err(ScrobbleError::LastFmApi {
                code: err.error,
                message: err.message,
            });
        }
        Ok(text)
    }
}

/// ListenBrainz API v1 scrobbling client.
#[derive(Debug, Clone)]
pub struct ListenBrainzClient {
    http: Client,
    endpoint: String,
}

impl Default for ListenBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenBrainzClient {
    pub fn new() -> Self {
        Self::with_endpoint(Client::new(), LISTENBRAINZ_SUBMIT_URL)
    }

    pub fn with_endpoint(http: Client, endpoint: impl Into<String>) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
        }
    }

    pub async fn submit_playing_now(
        &self,
        token: &str,
        song: &Song,
        position_ms: u64,
        duration_ms_override: Option<u64>,
        primary_artist_only: bool,
    ) -> bool {
        if token.trim().is_empty() {
            return false;
        }
        let body = build_listenbrainz_playing_now(
            song,
            position_ms,
            duration_ms_override,
            primary_artist_only,
        );
        self.post_submit(token, &body).await
    }

    pub async fn submit_finished(
        &self,
        token: &str,
        song: &Song,
        start_ms: i64,
        end_ms: i64,
        duration_ms_override: Option<u64>,
        primary_artist_only: bool,
    ) -> bool {
        if token.trim().is_empty() {
            return false;
        }
        let body = build_listenbrainz_finished(
            song,
            start_ms,
            end_ms,
            duration_ms_override,
            primary_artist_only,
        );
        self.post_submit(token, &body).await
    }

    async fn post_submit(&self, token: &str, body: &Value) -> bool {
        self.http
            .post(&self.endpoint)
            .timeout(Duration::from_secs(15))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::AUTHORIZATION, format!("Token {token}"))
            .json(body)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

pub fn build_listenbrainz_playing_now(
    song: &Song,
    position_ms: u64,
    duration_ms_override: Option<u64>,
    primary_artist_only: bool,
) -> Value {
    let duration_ms =
        duration_ms_override.unwrap_or_else(|| duration_millis(song.duration_text.as_deref()));
    let artist = if primary_artist_only {
        primary_artist(&song.artist)
    } else {
        song.artist.as_str()
    };
    let mut additional = Map::new();
    if duration_ms > 0 {
        additional.insert("duration_ms".to_owned(), Value::from(duration_ms));
    }
    additional.insert("position_ms".to_owned(), Value::from(position_ms));
    additional.insert(
        "submission_client".to_owned(),
        Value::String("BitChord".to_owned()),
    );

    let mut meta = Map::new();
    meta.insert("artist_name".to_owned(), Value::String(artist.to_owned()));
    meta.insert("track_name".to_owned(), Value::String(song.title.clone()));
    if let Some(ref album) = song.album_name
        && !album.trim().is_empty()
    {
        meta.insert("release_name".to_owned(), Value::String(album.clone()));
    }
    meta.insert("additional_info".to_owned(), Value::Object(additional));

    json!({
        "listen_type": "playing_now",
        "payload": [{ "track_metadata": Value::Object(meta) }]
    })
}

pub fn build_listenbrainz_finished(
    song: &Song,
    start_ms: i64,
    end_ms: i64,
    duration_ms_override: Option<u64>,
    primary_artist_only: bool,
) -> Value {
    let duration_ms =
        duration_ms_override.unwrap_or_else(|| duration_millis(song.duration_text.as_deref()));
    let artist = if primary_artist_only {
        primary_artist(&song.artist)
    } else {
        song.artist.as_str()
    };
    let mut listened_at = start_ms / 1_000;
    if listened_at < MIN_LISTEN_TS {
        listened_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(MIN_LISTEN_TS, |d| d.as_secs() as i64);
    }

    let mut additional = Map::new();
    if duration_ms > 0 {
        additional.insert("duration_ms".to_owned(), Value::from(duration_ms));
    }
    additional.insert("start_ms".to_owned(), Value::from(start_ms));
    additional.insert("end_ms".to_owned(), Value::from(end_ms));
    additional.insert(
        "submission_client".to_owned(),
        Value::String("BitChord".to_owned()),
    );

    let mut meta = Map::new();
    meta.insert("artist_name".to_owned(), Value::String(artist.to_owned()));
    meta.insert("track_name".to_owned(), Value::String(song.title.clone()));
    if let Some(ref album) = song.album_name
        && !album.trim().is_empty()
    {
        meta.insert("release_name".to_owned(), Value::String(album.clone()));
    }
    meta.insert("additional_info".to_owned(), Value::Object(additional));

    json!({
        "listen_type": "single",
        "payload": [{
            "listened_at": listened_at,
            "track_metadata": Value::Object(meta),
        }]
    })
}

/// Pure pause/resume scrobble threshold tracker matching `ScrobbleManager.kt`.
#[derive(Debug, Clone)]
pub struct ScrobbleTracker {
    pub min_song_duration_secs: u32,
    pub scrobble_delay_percent: f32,
    pub scrobble_delay_seconds: u32,
    remaining: Option<Duration>,
    started_at: Option<Instant>,
    pub track_started_unix_secs: i64,
    scrobbled: bool,
}

impl Default for ScrobbleTracker {
    fn default() -> Self {
        Self {
            min_song_duration_secs: DEFAULT_SCROBBLE_MIN_SONG_DURATION_SECS,
            scrobble_delay_percent: DEFAULT_SCROBBLE_DELAY_PERCENT,
            scrobble_delay_seconds: DEFAULT_SCROBBLE_DELAY_SECONDS,
            remaining: None,
            started_at: None,
            track_started_unix_secs: 0,
            scrobbled: false,
        }
    }
}

impl ScrobbleTracker {
    /// Starts tracking a newly started song. Returns `Some(Duration)` until the
    /// scrobble threshold is reached, or `None` if the track is too short.
    pub fn on_start(&mut self, duration_secs: u32, now: Instant) -> Option<Duration> {
        self.scrobbled = false;
        self.track_started_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        if duration_secs <= self.min_song_duration_secs {
            self.remaining = None;
            self.started_at = None;
            return None;
        }
        let pct_ms = (f64::from(duration_secs) * 1_000.0 * f64::from(self.scrobble_delay_percent))
            .round() as u64;
        let cap_ms = u64::from(self.scrobble_delay_seconds) * 1_000;
        let delay = Duration::from_millis(pct_ms.min(cap_ms));
        self.remaining = Some(delay);
        self.started_at = Some(now);
        Some(delay)
    }

    pub fn on_pause(&mut self, now: Instant) {
        if let (Some(rem), Some(started)) = (self.remaining, self.started_at.take()) {
            self.remaining = Some(rem.saturating_sub(now.saturating_duration_since(started)));
        }
    }

    pub fn on_resume(&mut self, now: Instant) -> Option<Duration> {
        if self.scrobbled {
            return None;
        }
        let rem = self.remaining?;
        if rem.is_zero() {
            return None;
        }
        self.started_at = Some(now);
        Some(rem)
    }

    pub fn is_due(&self, now: Instant) -> bool {
        if self.scrobbled {
            return false;
        }
        match (self.remaining, self.started_at) {
            (Some(rem), Some(started)) => now.saturating_duration_since(started) >= rem,
            _ => false,
        }
    }

    pub fn mark_scrobbled(&mut self) {
        self.scrobbled = true;
        self.remaining = None;
        self.started_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_artist_splits_joint_credits_and_preserves_ac_dc() {
        assert_eq!(primary_artist("Arijit Singh, Mithoon"), "Arijit Singh");
        assert_eq!(primary_artist("Daft Punk & Pharrell"), "Daft Punk");
        assert_eq!(primary_artist("AC/DC"), "AC/DC");
    }

    #[test]
    fn lastfm_endpoint_normalization_and_md5_sig() {
        assert_eq!(
            normalize_lastfm_endpoint("https://ws.audioscrobbler.com").expect("valid"),
            "https://ws.audioscrobbler.com/2.0/"
        );
        assert_eq!(
            normalize_lastfm_endpoint("https://libre.fm/2.0").expect("valid"),
            "https://libre.fm/2.0/"
        );
        assert!(normalize_lastfm_endpoint("ftp://libre.fm/2.0").is_err());
        assert!(normalize_lastfm_endpoint("https://libre.fm/2.0?x=1").is_err());

        let mut params = BTreeMap::new();
        params.insert("method".to_owned(), "auth.getToken".to_owned());
        params.insert("api_key".to_owned(), "abc".to_owned());
        // md5("api_keyabcmethodauth.getTokensecret")
        let sig = lastfm_api_sig(&params, "secret");
        assert_eq!(sig.len(), 32);
        assert_eq!(
            sig,
            format!("{:x}", Md5::digest(b"api_keyabcmethodauth.getTokensecret"))
        );
    }

    #[test]
    fn listenbrainz_payload_omits_zero_duration_and_blank_album() {
        let song = Song {
            video_id: "v1".to_owned(),
            title: "Song".to_owned(),
            artist: "First, Second".to_owned(),
            duration_text: Some("3:20".to_owned()),
            album_name: Some("   ".to_owned()),
            ..Song::default()
        };
        let playing = build_listenbrainz_playing_now(&song, 15_000, None, true);
        assert_eq!(playing["listen_type"], "playing_now");
        let meta = &playing["payload"][0]["track_metadata"];
        assert_eq!(meta["artist_name"], "First");
        assert!(meta.get("release_name").is_none());
        assert_eq!(meta["additional_info"]["duration_ms"], 200_000);
        assert_eq!(meta["additional_info"]["submission_client"], "BitChord");

        let finished = build_listenbrainz_finished(&song, 0, 120_000, Some(0), false);
        assert_eq!(finished["listen_type"], "single");
        assert!(finished["payload"][0]["listened_at"].as_i64().unwrap_or(0) >= MIN_LISTEN_TS);
        assert!(
            finished["payload"][0]["track_metadata"]["additional_info"]
                .get("duration_ms")
                .is_none()
        );
    }

    #[test]
    fn scrobble_tracker_pauses_resumes_and_caps_at_180_seconds() {
        let mut tracker = ScrobbleTracker::default();
        let t0 = Instant::now();
        // 25s song is below 30s min duration.
        assert!(tracker.on_start(25, t0).is_none());
        // 600s song caps at 180s (instead of 300s).
        assert_eq!(tracker.on_start(600, t0), Some(Duration::from_secs(180)));
        tracker.on_pause(t0 + Duration::from_secs(60));
        assert!(!tracker.is_due(t0 + Duration::from_secs(200)));
        let rem = tracker.on_resume(t0 + Duration::from_secs(200));
        assert_eq!(rem, Some(Duration::from_secs(120)));
        assert!(tracker.is_due(t0 + Duration::from_secs(320)));
    }
}
