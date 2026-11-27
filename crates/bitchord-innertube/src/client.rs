use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bitchord_lyrics::{BoxFuture as LyricsBoxFuture, YouTubeLyricsBackend};
use bitchord_model::{LikeStatus, PlaylistMove, PlaylistPrivacy};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

use crate::auth::{
    ChannelSelection, MUSIC_ORIGIN, SessionScope, YOUTUBE_ORIGIN, parse_session_scope_html,
    sapisid_from_cookie, sapisid_hash,
};
use crate::error::InnertubeError;
use crate::locale::LocaleTags;
use crate::player_client::WEB_USER_AGENT;

const MUSIC_BASE: &str = "https://music.youtube.com/youtubei/v1";
const WEB_REMIX_FALLBACK_VERSION: &str = "1.20250101.01.00";
const WEB_REMIX_CLIENT_ID: &str = "67";
const DEFAULT_ATR_SECONDS: i64 = 5;
const CPN_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";

static VISITOR_DATA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Cg[A-Za-z0-9_%-]{40,}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid VISITOR_DATA_RE")
});

/// Playback tracking URLs returned by an authenticated `WEB_REMIX` `/player` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackTracking {
    pub playback_url: String,
    pub watchtime_url: Option<String>,
    pub atr_url: Option<String>,
    pub atr_after_seconds: i64,
}

#[derive(Debug, Default)]
struct SessionState {
    cookie: Option<String>,
    scope: Option<SessionScope>,
    channel_override: Option<ChannelSelection>,
    visitor_data: Option<String>,
    visitor_data_is_session_bound: bool,
    app_language: Option<String>,
}

/// Shared primary HTTP client builder matching `Http.kt` & `youtube-extraction.md` §3.2
/// so InnerTube calls and media streaming share DNS/IPv4-IPv6 connection affinity.
pub fn build_shared_http_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(16)
        .pool_idle_timeout(Duration::from_secs(300))
        .http2_keep_alive_interval(Some(Duration::from_secs(15)))
        .http2_keep_alive_while_idle(true)
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// Native YouTube Music InnerTube (`WEB_REMIX`) client.
///
/// Implements [`YouTubeLyricsBackend`] directly so Wave 1's `LyricsRepository`
/// can resolve YouTube Music Lyrics tabs and timed captions seamlessly.
pub struct InnertubeClient {
    http: Arc<Client>,
    music_base: String,
    music_origin: String,
    youtube_origin: String,
    state: RwLock<SessionState>,
    scope_lock: Mutex<()>,
}

impl Default for InnertubeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl InnertubeClient {
    pub fn new() -> Self {
        Self::with_http_client(Arc::new(build_shared_http_client()))
    }

    pub fn with_http_client(http: Arc<Client>) -> Self {
        Self {
            http,
            music_base: MUSIC_BASE.to_owned(),
            music_origin: MUSIC_ORIGIN.to_owned(),
            youtube_origin: YOUTUBE_ORIGIN.to_owned(),
            state: RwLock::new(SessionState::default()),
            scope_lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_endpoints(
        http: Arc<Client>,
        music_base: String,
        music_origin: String,
        youtube_origin: String,
    ) -> Self {
        Self {
            http,
            music_base,
            music_origin,
            youtube_origin,
            state: RwLock::new(SessionState::default()),
            scope_lock: Mutex::new(()),
        }
    }

    pub fn http_client(&self) -> &Arc<Client> {
        &self.http
    }

    pub fn set_app_language(&self, tag: Option<String>) {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        st.app_language = tag.filter(|s| !s.trim().is_empty());
    }

    pub fn current_locale_tag(&self) -> String {
        let app_lang = {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            st.app_language.clone()
        };
        match app_lang {
            Some(tag) => LocaleTags::normalize_app_tag(Some(&tag)),
            None => LocaleTags::system_tag(),
        }
    }

    pub fn current_language(&self) -> String {
        LocaleTags::yt_hl(&self.current_locale_tag())
    }

    pub fn current_region(&self) -> &'static str {
        LocaleTags::yt_gl(&self.current_locale_tag())
    }

    fn search_region(&self) -> Option<&'static str> {
        LocaleTags::yt_search_gl(&self.current_locale_tag())
    }

    fn accept_language_header(&self) -> String {
        LocaleTags::accept_language(&self.current_locale_tag())
    }

    pub fn cookie(&self) -> Option<String> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .cookie
            .clone()
    }

    pub fn is_signed_in(&self) -> bool {
        self.cookie().is_some()
    }

    /// Replaces the active session cookie and atomically clears session-bound
    /// `scope`, `visitor_data`, and `channel_override` so no identity leaks
    /// across accounts.
    pub fn set_cookie(&self, value: Option<String>) {
        let normalized = value.filter(|s| !s.trim().is_empty());
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        if st.cookie != normalized {
            st.scope = None;
            st.visitor_data = None;
            st.visitor_data_is_session_bound = false;
            st.channel_override = None;
            st.cookie = normalized;
        }
    }

    pub fn visitor_data(&self) -> Option<String> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .visitor_data
            .clone()
    }

    pub fn web_remix_version(&self) -> String {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .scope
            .as_ref()
            .and_then(|s| s.client_version.clone())
            .unwrap_or_else(|| WEB_REMIX_FALLBACK_VERSION.to_owned())
    }

    pub fn select_channel(
        &self,
        page_id: Option<String>,
        data_sync_id: Option<String>,
        auth_user: Option<String>,
    ) {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        if page_id.is_none() && data_sync_id.is_none() {
            st.channel_override = None;
        } else {
            st.channel_override = Some(ChannelSelection {
                page_id,
                data_sync_id,
                auth_user,
            });
        }
    }

    pub fn adopt_session_scope(
        &self,
        page_id: Option<String>,
        data_sync_id: Option<String>,
        auth_user: Option<String>,
        visitor_data: Option<String>,
        client_version: Option<String>,
        logged_in: bool,
    ) {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        let version =
            client_version.or_else(|| st.scope.as_ref().and_then(|s| s.client_version.clone()));
        if !logged_in {
            st.scope = version.map(|ver| SessionScope {
                data_sync_id: None,
                page_id: None,
                auth_user: "0".to_owned(),
                client_version: Some(ver),
                visitor_data: None,
                logged_in: false,
            });
            return;
        }
        st.scope = Some(SessionScope {
            data_sync_id: data_sync_id.filter(|s| !s.trim().is_empty()),
            page_id: page_id.filter(|s| !s.trim().is_empty()),
            auth_user: auth_user
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "0".to_owned()),
            client_version: version,
            visitor_data: visitor_data.clone().filter(|s| !s.trim().is_empty()),
            logged_in: true,
        });
        if let Some(vd) = visitor_data.filter(|s| !s.trim().is_empty()) {
            st.visitor_data = Some(vd);
            st.visitor_data_is_session_bound = true;
        }
    }

    /// Pre-warms `visitor_data` asynchronously at startup so the first track
    /// playback does not pay an extra `/sw.js_data` round-trip.
    pub async fn prewarm_visitor_data(&self) -> Option<String> {
        self.ensure_visitor_data(false).await
    }

    /// Spawns a background task to pre-warm `visitor_data` without blocking startup.
    pub fn spawn_prewarm_visitor_data(self: &Arc<Self>) -> tokio::task::JoinHandle<Option<String>> {
        let client = Arc::clone(self);
        tokio::spawn(async move { client.prewarm_visitor_data().await })
    }

    /// Mints an anonymous visitor ID from `/sw.js_data` if none is cached (or
    /// when `refresh` is `true`). Never overwrites a session-bound visitor ID
    /// unless `refresh` is explicitly requested.
    pub async fn ensure_visitor_data(&self, refresh: bool) -> Option<String> {
        if !refresh && let Some(existing) = self.visitor_data() {
            return Some(existing);
        }
        if let Ok(fresh) = self.fetch_visitor_data().await
            && let Some(id) = fresh
        {
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            if refresh || !st.visitor_data_is_session_bound {
                st.visitor_data = Some(id);
                st.visitor_data_is_session_bound = false;
            }
        }
        self.visitor_data()
    }

    async fn fetch_visitor_data(&self) -> Result<Option<String>, InnertubeError> {
        let url = format!("{}/sw.js_data", self.youtube_origin);
        let body = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(15))
            .header(reqwest::header::USER_AGENT, WEB_USER_AGENT)
            .send()
            .await
            .map_err(|e| InnertubeError::Transport(e.to_string()))?
            .text()
            .await
            .map_err(|e| InnertubeError::Transport(e.to_string()))?;

        let stripped = body
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| body.get(5..).unwrap_or(&body));
        let payload: Value =
            serde_json::from_str(stripped).map_err(|e| InnertubeError::Json(e.to_string()))?;
        Ok(find_visitor_data(&payload))
    }

    /// Reads the active session identity (`ytcfg`) from `music.youtube.com/`
    /// once per cookie. Fails open if the shell cannot be fetched.
    pub async fn ensure_session_scope(&self) {
        let session = {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            if st.scope.is_some() {
                return;
            }
            match st.cookie.clone() {
                Some(c) => c,
                None => return,
            }
        };

        let _guard = self.scope_lock.lock().await;
        {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            if st.scope.is_some() || st.cookie.as_deref() != Some(session.as_str()) {
                return;
            }
        }

        if let Ok(Some(fresh)) = self.fetch_session_scope(&session).await {
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            if st.cookie.as_deref() != Some(session.as_str()) {
                return;
            }
            if let Some(ref vd) = fresh.visitor_data {
                st.visitor_data = Some(vd.clone());
                st.visitor_data_is_session_bound = true;
            }
            st.scope = Some(fresh);
        }
    }

    pub async fn refresh_session_scope(&self) {
        if self.cookie().is_none() {
            return;
        }
        {
            let _guard = self.scope_lock.lock().await;
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            st.scope = None;
        }
        self.ensure_session_scope().await;
    }

    async fn fetch_session_scope(
        &self,
        session: &str,
    ) -> Result<Option<SessionScope>, InnertubeError> {
        let mut req = self
            .http
            .get(format!("{}/", self.music_origin))
            .timeout(Duration::from_secs(20))
            .header(reqwest::header::USER_AGENT, WEB_USER_AGENT)
            .header(
                reqwest::header::ACCEPT_LANGUAGE,
                self.accept_language_header(),
            )
            .header(reqwest::header::COOKIE, session);
        if let Some(sapisid) = sapisid_from_cookie(session) {
            req = req.header(
                reqwest::header::AUTHORIZATION,
                sapisid_hash(&sapisid, &self.music_origin),
            );
        }
        let html = req
            .send()
            .await
            .map_err(|e| InnertubeError::Transport(e.to_string()))?
            .text()
            .await
            .map_err(|e| InnertubeError::Transport(e.to_string()))?;
        Ok(parse_session_scope_html(&html))
    }

    // ---- Public InnerTube Endpoints -----------------------------------------

    pub async fn browse(
        &self,
        browse_id: &str,
        params: Option<&str>,
    ) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("browseId".to_owned(), Value::String(browse_id.to_owned()));
        if let Some(p) = params {
            extras.insert("params".to_owned(), Value::String(p.to_owned()));
        }
        self.post_music("browse", &[], Some(self.current_region()), extras)
            .await
    }

    pub async fn browse_continuation(&self, token: &str) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("continuation".to_owned(), Value::String(token.to_owned()));
        self.post_music(
            "browse",
            &[("ctoken", token), ("continuation", token), ("type", "next")],
            Some(self.current_region()),
            extras,
        )
        .await
    }

    pub async fn account_menu(&self) -> Result<Value, InnertubeError> {
        self.post_music(
            "account/account_menu",
            &[],
            Some(self.current_region()),
            Map::new(),
        )
        .await
    }

    pub async fn accounts_list(&self) -> Result<Value, InnertubeError> {
        self.post_music(
            "account/accounts_list",
            &[],
            Some(self.current_region()),
            Map::new(),
        )
        .await
    }

    pub async fn account_switcher(&self) -> Result<Value, InnertubeError> {
        self.require_session()?;
        self.ensure_session_scope().await;

        let (cookie, auth_user) = {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            let auth = auth_user_for(&st);
            (st.cookie.clone(), auth)
        };
        let accept_lang = self.accept_language_header();
        let url = format!("{}/getAccountSwitcherEndpoint", self.youtube_origin);

        let text = self
            .with_retry(|| {
                let url = url.clone();
                let cookie = cookie.clone();
                let auth_user = auth_user.clone();
                let accept_lang = accept_lang.clone();
                async move {
                    let mut req = self
                        .http
                        .get(&url)
                        .timeout(Duration::from_secs(30))
                        .header(reqwest::header::USER_AGENT, WEB_USER_AGENT)
                        .header(reqwest::header::ACCEPT_LANGUAGE, &accept_lang)
                        .header("X-Origin", &self.youtube_origin)
                        .header(
                            reqwest::header::REFERER,
                            format!("{}/", self.youtube_origin),
                        );
                    if let Some(ref c) = cookie {
                        req = req
                            .header(reqwest::header::COOKIE, c)
                            .header("X-Goog-AuthUser", &auth_user);
                        if let Some(sapisid) = sapisid_from_cookie(c) {
                            req = req.header(
                                reqwest::header::AUTHORIZATION,
                                sapisid_hash(&sapisid, &self.youtube_origin),
                            );
                        }
                    }
                    let resp = req
                        .send()
                        .await
                        .map_err(|e| InnertubeError::Transport(e.to_string()))?;
                    let status = resp.status();
                    let body = resp
                        .text()
                        .await
                        .map_err(|e| InnertubeError::Transport(e.to_string()))?;
                    if !status.is_success() {
                        return Err(InnertubeError::HttpStatus {
                            status: status.as_u16(),
                            detail: truncate_detail(&body),
                        });
                    }
                    Ok(body)
                }
            })
            .await?;

        let stripped = text
            .split_once(")]}'")
            .map(|(_, rest)| rest)
            .unwrap_or(&text)
            .trim();
        serde_json::from_str(stripped).map_err(|e| InnertubeError::Json(e.to_string()))
    }

    pub async fn next(&self, video_id: &str) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("videoId".to_owned(), Value::String(video_id.to_owned()));
        extras.insert(
            "playlistId".to_owned(),
            Value::String(format!("RDAMVM{video_id}")),
        );
        extras.insert("isAudioOnly".to_owned(), Value::Bool(true));
        self.post_music("next", &[], Some(self.current_region()), extras)
            .await
    }

    pub async fn next_playlist(
        &self,
        playlist_id: &str,
        video_id: Option<&str>,
        params: Option<&str>,
    ) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        if let Some(v) = video_id {
            extras.insert("videoId".to_owned(), Value::String(v.to_owned()));
        }
        extras.insert(
            "playlistId".to_owned(),
            Value::String(playlist_id.to_owned()),
        );
        if let Some(p) = params {
            extras.insert("params".to_owned(), Value::String(p.to_owned()));
        }
        extras.insert("isAudioOnly".to_owned(), Value::Bool(true));
        self.post_music("next", &[], Some(self.current_region()), extras)
            .await
    }

    pub async fn transcript(&self, video_id: &str) -> Result<Value, InnertubeError> {
        let id_bytes = video_id.as_bytes();
        let mut proto = Vec::with_capacity(2 + id_bytes.len());
        proto.push(0x0A);
        proto.push(id_bytes.len().min(127) as u8);
        proto.extend_from_slice(id_bytes);
        let encoded = STANDARD.encode(&proto);
        let mut extras = Map::new();
        extras.insert("params".to_owned(), Value::String(encoded));
        self.post_music("get_transcript", &[], Some(self.current_region()), extras)
            .await
    }

    pub async fn search(
        &self,
        query: &str,
        params: Option<&str>,
        anonymous: bool,
    ) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("query".to_owned(), Value::String(query.to_owned()));
        if let Some(p) = params {
            extras.insert("params".to_owned(), Value::String(p.to_owned()));
        }
        let region = self.search_region();
        if anonymous {
            self.post_music_anonymous("search", region, extras).await
        } else {
            self.post_music("search", &[], region, extras).await
        }
    }

    pub async fn search_continuation(&self, token: &str) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("continuation".to_owned(), Value::String(token.to_owned()));
        self.post_music(
            "search",
            &[("ctoken", token), ("continuation", token), ("type", "next")],
            self.search_region(),
            extras,
        )
        .await
    }

    pub async fn search_suggestions(&self, input: &str) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("input".to_owned(), Value::String(input.to_owned()));
        self.post_music(
            "music/get_search_suggestions",
            &[],
            self.search_region(),
            extras,
        )
        .await
    }

    pub async fn search_typeahead(&self, query: &str) -> Result<Value, InnertubeError> {
        let mut extras = Map::new();
        extras.insert("query".to_owned(), Value::String(query.to_owned()));
        self.post_music_anonymous("search", self.search_region(), extras)
            .await
    }

    pub async fn playback_tracking(
        &self,
        video_id: &str,
        signature_timestamp: Option<i64>,
    ) -> Result<Option<PlaybackTracking>, InnertubeError> {
        if self.cookie().is_none() {
            return Ok(None);
        }
        self.ensure_session_scope().await;
        let mut content_ctx = Map::new();
        content_ctx.insert(
            "html5Preference".to_owned(),
            Value::String("HTML5_PREF_WANTS".to_owned()),
        );
        content_ctx.insert(
            "referer".to_owned(),
            Value::String(format!("{}/watch?v={video_id}", self.music_origin)),
        );
        if let Some(sts) = signature_timestamp {
            content_ctx.insert("signatureTimestamp".to_owned(), Value::from(sts));
        }
        let mut extras = Map::new();
        extras.insert("videoId".to_owned(), Value::String(video_id.to_owned()));
        extras.insert("contentCheckOk".to_owned(), Value::Bool(true));
        extras.insert("racyCheckOk".to_owned(), Value::Bool(true));
        extras.insert(
            "playbackContext".to_owned(),
            json!({ "contentPlaybackContext": Value::Object(content_ctx) }),
        );

        let response = self
            .post_music("player", &[], Some(self.current_region()), extras)
            .await?;
        let Some(tracking) = response.get("playbackTracking").and_then(Value::as_object) else {
            return Ok(None);
        };
        let Some(playback_url) = tracking_url(tracking, "videostatsPlaybackUrl") else {
            return Ok(None);
        };
        let atr_after_seconds = tracking
            .get("atrUrl")
            .and_then(Value::as_object)
            .and_then(|o| o.get("elapsedMediaTimeSeconds"))
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .unwrap_or(DEFAULT_ATR_SECONDS);
        Ok(Some(PlaybackTracking {
            playback_url,
            watchtime_url: tracking_url(tracking, "videostatsWatchtimeUrl"),
            atr_url: tracking_url(tracking, "atrUrl"),
            atr_after_seconds,
        }))
    }

    pub async fn ping_playback(&self, base_url: &str, cpn: &str) -> Result<u16, InnertubeError> {
        self.ping_stats(base_url, cpn, &[]).await
    }

    pub async fn ping_watchtime(
        &self,
        base_url: &str,
        cpn: &str,
        seconds: u64,
        is_final: bool,
    ) -> Result<u16, InnertubeError> {
        let secs = seconds.to_string();
        let state = if is_final { "paused" } else { "playing" };
        let mut extra = vec![
            ("st", "0"),
            ("et", secs.as_str()),
            ("cmt", secs.as_str()),
            ("state", state),
        ];
        if is_final {
            extra.push(("final", "1"));
        }
        self.ping_stats(base_url, cpn, &extra).await
    }

    pub async fn ping_atr(&self, base_url: &str, cpn: &str) -> Result<u16, InnertubeError> {
        if self.cookie().is_some() {
            self.ensure_session_scope().await;
        }
        let mut url =
            Url::parse(base_url).map_err(|e| InnertubeError::InvalidState(e.to_string()))?;
        url.query_pairs_mut().append_pair("cpn", cpn);
        let req = self.apply_stats_headers(self.http.get(url));
        let resp = req
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| InnertubeError::Transport(e.to_string()))?;
        Ok(resp.status().as_u16())
    }

    async fn ping_stats(
        &self,
        base_url: &str,
        cpn: &str,
        extras: &[(&str, &str)],
    ) -> Result<u16, InnertubeError> {
        if self.cookie().is_some() {
            self.ensure_session_scope().await;
        }
        let mut url =
            Url::parse(base_url).map_err(|e| InnertubeError::InvalidState(e.to_string()))?;
        let cver = self.web_remix_version();
        let hl = self.current_language().replace('-', "_");
        let cr = self.current_region();
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("ver", "2")
                .append_pair("c", "WEB_REMIX")
                .append_pair("cver", &cver)
                .append_pair("cpn", cpn)
                .append_pair("cplayer", "UNIPLAYER")
                .append_pair("cbr", "Chrome")
                .append_pair("cbrver", "141.0.0.0")
                .append_pair("cos", "Windows")
                .append_pair("cosver", "10.0")
                .append_pair("hl", &hl)
                .append_pair("cr", cr);
            for &(k, v) in extras {
                pairs.append_pair(k, v);
            }
        }
        let req = self.apply_stats_headers(self.http.get(url));
        let resp = req
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| InnertubeError::Transport(e.to_string()))?;
        Ok(resp.status().as_u16())
    }

    fn apply_stats_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        req = req
            .header("X-Origin", &self.music_origin)
            .header(reqwest::header::ORIGIN, &self.music_origin)
            .header(reqwest::header::REFERER, format!("{}/", self.music_origin))
            .header(reqwest::header::USER_AGENT, WEB_USER_AGENT);
        if let Some(ref vd) = st.visitor_data {
            req = req.header("X-Goog-Visitor-Id", vd);
        }
        if let Some(ref c) = st.cookie {
            req = req
                .header(reqwest::header::COOKIE, c)
                .header("X-Goog-AuthUser", auth_user_for(&st));
            if let Some(page_id) = page_id_for(&st) {
                req = req.header("X-Goog-PageId", page_id);
            }
            if let Some(sapisid) = sapisid_from_cookie(c) {
                req = req.header(
                    reqwest::header::AUTHORIZATION,
                    sapisid_hash(&sapisid, &self.music_origin),
                );
            }
        }
        req
    }

    // ---- Account Writes (strict HTTP 200 body error checks) -----------------

    fn require_session(&self) -> Result<(), InnertubeError> {
        if self.cookie().is_none() {
            Err(InnertubeError::NotSignedIn)
        } else {
            Ok(())
        }
    }

    pub async fn rate(&self, video_id: &str, status: LikeStatus) -> Result<(), InnertubeError> {
        self.require_session()?;
        let endpoint = match status {
            LikeStatus::Like => "like/like",
            LikeStatus::Dislike => "like/dislike",
            LikeStatus::Indifferent => "like/removelike",
        };
        let mut extras = Map::new();
        extras.insert("target".to_owned(), json!({ "videoId": video_id }));
        let resp = self
            .post_music(endpoint, &[], Some(self.current_region()), extras)
            .await?;
        check_api_error(&resp)
    }

    pub async fn rate_playlist(
        &self,
        playlist_id: &str,
        saved: bool,
    ) -> Result<(), InnertubeError> {
        self.require_session()?;
        let endpoint = if saved {
            "like/like"
        } else {
            "like/removelike"
        };
        let mut extras = Map::new();
        extras.insert("target".to_owned(), json!({ "playlistId": playlist_id }));
        let resp = self
            .post_music(endpoint, &[], Some(self.current_region()), extras)
            .await?;
        check_api_error(&resp)
    }

    pub async fn set_subscribed(
        &self,
        channel_id: &str,
        subscribed: bool,
    ) -> Result<(), InnertubeError> {
        self.require_session()?;
        let endpoint = if subscribed {
            "subscription/subscribe"
        } else {
            "subscription/unsubscribe"
        };
        let mut extras = Map::new();
        extras.insert("channelIds".to_owned(), json!([channel_id]));
        let resp = self
            .post_music(endpoint, &[], Some(self.current_region()), extras)
            .await?;
        check_api_error(&resp)
    }

    pub async fn send_feedback(&self, token: &str) -> Result<(), InnertubeError> {
        self.require_session()?;
        let mut extras = Map::new();
        extras.insert("feedbackTokens".to_owned(), json!([token]));
        let _ = self
            .post_music("feedback", &[], Some(self.current_region()), extras)
            .await?;
        Ok(())
    }

    pub async fn create_playlist(
        &self,
        title: &str,
        privacy: PlaylistPrivacy,
        description: Option<&str>,
        video_ids: &[String],
    ) -> Result<String, InnertubeError> {
        self.require_session()?;
        let mut extras = Map::new();
        extras.insert("title".to_owned(), Value::String(title.to_owned()));
        extras.insert(
            "description".to_owned(),
            Value::String(description.unwrap_or_default().to_owned()),
        );
        extras.insert(
            "privacyStatus".to_owned(),
            Value::String(privacy.api_value().to_owned()),
        );
        if !video_ids.is_empty() {
            extras.insert("videoIds".to_owned(), json!(video_ids));
        }
        let resp = self
            .post_music("playlist/create", &[], Some(self.current_region()), extras)
            .await?;
        check_api_error(&resp)?;
        resp.get("playlistId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| find_string(&resp, "playlistId"))
            .ok_or_else(|| {
                InnertubeError::ApiRefused("playlist created but no id came back".to_owned())
            })
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<(), InnertubeError> {
        self.require_session()?;
        let raw_id = playlist_id.strip_prefix("VL").unwrap_or(playlist_id);
        let mut extras = Map::new();
        extras.insert("playlistId".to_owned(), Value::String(raw_id.to_owned()));
        let _ = self
            .post_music("playlist/delete", &[], Some(self.current_region()), extras)
            .await?;
        Ok(())
    }

    async fn edit_playlist(
        &self,
        playlist_id: &str,
        actions: Vec<Value>,
    ) -> Result<Value, InnertubeError> {
        self.require_session()?;
        let raw_id = playlist_id.strip_prefix("VL").unwrap_or(playlist_id);
        let mut extras = Map::new();
        extras.insert("playlistId".to_owned(), Value::String(raw_id.to_owned()));
        extras.insert("actions".to_owned(), Value::Array(actions));
        let resp = self
            .post_music(
                "browse/edit_playlist",
                &[],
                Some(self.current_region()),
                extras,
            )
            .await?;
        check_api_error(&resp)?;
        if let Some(status) = resp.get("status").and_then(Value::as_str)
            && status != "STATUS_SUCCEEDED"
        {
            return Err(InnertubeError::ApiRefused(format!(
                "edit_playlist refused ({status})"
            )));
        }
        Ok(resp)
    }

    pub async fn add_to_playlist(
        &self,
        playlist_id: &str,
        video_ids: &[String],
    ) -> Result<HashMap<String, String>, InnertubeError> {
        let actions: Vec<Value> = video_ids
            .iter()
            .map(|vid| {
                json!({
                    "action": "ACTION_ADD_VIDEO",
                    "addedVideoId": vid,
                })
            })
            .collect();
        let resp = self.edit_playlist(playlist_id, actions).await?;
        let mut added_map = HashMap::new();
        if let Some(results) = resp.get("playlistEditResults").and_then(Value::as_array) {
            for item in results {
                let Some(data) = item
                    .as_object()
                    .and_then(|o| o.get("playlistEditVideoAddedResultData"))
                    .and_then(Value::as_object)
                else {
                    continue;
                };
                if let (Some(vid), Some(set_id)) = (
                    data.get("videoId").and_then(Value::as_str),
                    data.get("setVideoId").and_then(Value::as_str),
                ) {
                    added_map.insert(vid.to_owned(), set_id.to_owned());
                }
            }
        }
        Ok(added_map)
    }

    pub async fn remove_from_playlist(
        &self,
        playlist_id: &str,
        entries: &[(String, String)],
    ) -> Result<(), InnertubeError> {
        let actions: Vec<Value> = entries
            .iter()
            .map(|(set_video_id, video_id)| {
                json!({
                    "action": "ACTION_REMOVE_VIDEO",
                    "setVideoId": set_video_id,
                    "removedVideoId": video_id,
                })
            })
            .collect();
        let _ = self.edit_playlist(playlist_id, actions).await?;
        Ok(())
    }

    pub async fn move_playlist_items(
        &self,
        playlist_id: &str,
        moves: &[PlaylistMove],
    ) -> Result<(), InnertubeError> {
        if moves.is_empty() {
            return Ok(());
        }
        let actions: Vec<Value> = moves
            .iter()
            .map(|m| {
                let mut obj = Map::new();
                obj.insert(
                    "action".to_owned(),
                    Value::String("ACTION_MOVE_VIDEO_BEFORE".to_owned()),
                );
                obj.insert(
                    "setVideoId".to_owned(),
                    Value::String(m.set_video_id.clone()),
                );
                if let Some(ref before) = m.before {
                    obj.insert(
                        "movedSetVideoIdSuccessor".to_owned(),
                        Value::String(before.clone()),
                    );
                }
                Value::Object(obj)
            })
            .collect();
        let _ = self.edit_playlist(playlist_id, actions).await?;
        Ok(())
    }

    pub async fn rename_playlist(
        &self,
        playlist_id: &str,
        title: &str,
    ) -> Result<(), InnertubeError> {
        let actions = vec![json!({
            "action": "ACTION_SET_PLAYLIST_NAME",
            "playlistName": title,
        })];
        let _ = self.edit_playlist(playlist_id, actions).await?;
        Ok(())
    }

    /// Generates a fresh 16-character client playback nonce (`cpn`).
    pub fn new_cpn() -> String {
        let bytes = Uuid::new_v4().into_bytes();
        bytes
            .iter()
            .map(|&b| CPN_ALPHABET[(b as usize) % CPN_ALPHABET.len()] as char)
            .collect()
    }

    // ---- Internal Request Plumbing ------------------------------------------

    async fn post_music(
        &self,
        endpoint: &str,
        query: &[(&str, &str)],
        region: Option<&str>,
        extras: Map<String, Value>,
    ) -> Result<Value, InnertubeError> {
        if self.cookie().is_some() {
            self.ensure_session_scope().await;
        }
        self.post_music_inner(endpoint, query, region, extras, true)
            .await
    }

    async fn post_music_anonymous(
        &self,
        endpoint: &str,
        region: Option<&str>,
        extras: Map<String, Value>,
    ) -> Result<Value, InnertubeError> {
        self.post_music_inner(endpoint, &[], region, extras, false)
            .await
    }

    async fn post_music_inner(
        &self,
        endpoint: &str,
        query: &[(&str, &str)],
        region: Option<&str>,
        extras: Map<String, Value>,
        authenticated: bool,
    ) -> Result<Value, InnertubeError> {
        let (cookie, auth_user, page_id, data_sync_id, visitor_data, client_version) = {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            let ver = st
                .scope
                .as_ref()
                .and_then(|s| s.client_version.clone())
                .unwrap_or_else(|| WEB_REMIX_FALLBACK_VERSION.to_owned());
            if authenticated {
                (
                    st.cookie.clone(),
                    auth_user_for(&st),
                    page_id_for(&st),
                    data_sync_id_for(&st),
                    st.visitor_data.clone(),
                    ver,
                )
            } else {
                (
                    None,
                    "0".to_owned(),
                    None,
                    None,
                    st.visitor_data.clone(),
                    ver,
                )
            }
        };

        let hl = self.current_language();
        let accept_lang = self.accept_language_header();
        let mut url = Url::parse(&format!("{}/{endpoint}", self.music_base))
            .map_err(|e| InnertubeError::InvalidState(e.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("prettyPrint", "false");
            pairs.append_pair("hl", &hl);
            for &(k, v) in query {
                pairs.append_pair(k, v);
            }
        }

        let mut client_obj = Map::new();
        client_obj.insert(
            "clientName".to_owned(),
            Value::String("WEB_REMIX".to_owned()),
        );
        client_obj.insert(
            "clientVersion".to_owned(),
            Value::String(client_version.clone()),
        );
        client_obj.insert("hl".to_owned(), Value::String(hl));
        if let Some(gl) = region {
            client_obj.insert("gl".to_owned(), Value::String(gl.to_owned()));
        }
        if let Some(ref vd) = visitor_data {
            client_obj.insert("visitorData".to_owned(), Value::String(vd.clone()));
        }

        let mut user_obj = Map::new();
        user_obj.insert("lockedSafetyMode".to_owned(), Value::Bool(false));
        if authenticated && let Some(ref dsid) = data_sync_id {
            user_obj.insert("onBehalfOfUser".to_owned(), Value::String(dsid.clone()));
        }

        let mut body_obj = Map::new();
        body_obj.insert(
            "context".to_owned(),
            json!({
                "client": Value::Object(client_obj),
                "user": Value::Object(user_obj),
                "request": { "useSsl": true },
            }),
        );
        for (k, v) in extras {
            body_obj.insert(k, v);
        }
        let body_value = Value::Object(body_obj);

        let response = self
            .with_retry(|| {
                let url = url.clone();
                let body_value = body_value.clone();
                let accept_lang = accept_lang.clone();
                let client_version = client_version.clone();
                let visitor_data = visitor_data.clone();
                let cookie = cookie.clone();
                let auth_user = auth_user.clone();
                let page_id = page_id.clone();
                async move {
                    let mut req = self
                        .http
                        .post(url)
                        .timeout(Duration::from_secs(30))
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .header(reqwest::header::ACCEPT_LANGUAGE, &accept_lang)
                        .header("X-Origin", &self.music_origin)
                        .header(reqwest::header::ORIGIN, &self.music_origin)
                        .header(reqwest::header::REFERER, format!("{}/", self.music_origin))
                        .header("X-YouTube-Client-Name", WEB_REMIX_CLIENT_ID)
                        .header("X-YouTube-Client-Version", &client_version);
                    if let Some(ref vd) = visitor_data {
                        req = req.header("X-Goog-Visitor-Id", vd);
                    }
                    if let Some(ref c) = cookie {
                        req = req
                            .header(reqwest::header::COOKIE, c)
                            .header("X-Goog-AuthUser", &auth_user);
                        if let Some(ref pid) = page_id {
                            req = req.header("X-Goog-PageId", pid);
                        }
                        if let Some(sapisid) = sapisid_from_cookie(c) {
                            req = req.header(
                                reqwest::header::AUTHORIZATION,
                                sapisid_hash(&sapisid, &self.music_origin),
                            );
                        }
                    }

                    let resp = req.json(&body_value).send().await.map_err(|e| {
                        if e.is_timeout() {
                            InnertubeError::InvalidState(format!("timeout: {e}"))
                        } else {
                            InnertubeError::Transport(e.to_string())
                        }
                    })?;
                    let status = resp.status();
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| InnertubeError::Transport(e.to_string()))?;
                    if !status.is_success() {
                        return Err(InnertubeError::HttpStatus {
                            status: status.as_u16(),
                            detail: truncate_detail(&text),
                        });
                    }
                    serde_json::from_str::<Value>(&text)
                        .map_err(|e| InnertubeError::Json(e.to_string()))
                }
            })
            .await?;

        if authenticated
            && self.visitor_data().is_none()
            && let Some(vd) = response
                .get("responseContext")
                .and_then(Value::as_object)
                .and_then(|o| o.get("visitorData"))
                .and_then(Value::as_str)
        {
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            if st.visitor_data.is_none() {
                st.visitor_data = Some(vd.to_owned());
            }
        }

        Ok(response)
    }

    async fn with_retry<F, Fut, T>(&self, mut op: F) -> Result<T, InnertubeError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, InnertubeError>>,
    {
        let mut backoff_ms = 500u64;
        for attempt in 0..3 {
            match op().await {
                Ok(val) => return Ok(val),
                Err(InnertubeError::Transport(_)) if attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms *= 2;
                }
                Err(err) => return Err(err),
            }
        }
        unreachable!("loop returns on final attempt")
    }
}

/// Seamless Wave 1 `bitchord-lyrics` integration: plugs `InnertubeClient` into
/// `LyricsRepository::with_youtube_backend(...)`.
impl YouTubeLyricsBackend for InnertubeClient {
    fn next<'a>(&'a self, video_id: &'a str) -> LyricsBoxFuture<'a, Option<Value>> {
        Box::pin(async move { InnertubeClient::next(self, video_id).await.ok() })
    }

    fn browse<'a>(
        &'a self,
        browse_id: &'a str,
        params: Option<&'a str>,
    ) -> LyricsBoxFuture<'a, Option<Value>> {
        Box::pin(async move { InnertubeClient::browse(self, browse_id, params).await.ok() })
    }

    fn transcript<'a>(&'a self, video_id: &'a str) -> LyricsBoxFuture<'a, Option<Value>> {
        Box::pin(async move { InnertubeClient::transcript(self, video_id).await.ok() })
    }
}

fn page_id_for(st: &SessionState) -> Option<String> {
    if let Some(ref ov) = st.channel_override {
        ov.page_id.clone()
    } else {
        st.scope.as_ref().and_then(|s| s.page_id.clone())
    }
}

fn data_sync_id_for(st: &SessionState) -> Option<String> {
    if let Some(ref ov) = st.channel_override {
        ov.data_sync_id.clone()
    } else {
        st.scope.as_ref().and_then(|s| s.data_sync_id.clone())
    }
}

fn auth_user_for(st: &SessionState) -> String {
    st.channel_override
        .as_ref()
        .and_then(|o| o.auth_user.clone())
        .or_else(|| st.scope.as_ref().map(|s| s.auth_user.clone()))
        .unwrap_or_else(|| "0".to_owned())
}

fn tracking_url(tracking: &Map<String, Value>, key: &str) -> Option<String> {
    tracking
        .get(key)?
        .as_object()?
        .get("baseUrl")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn check_api_error(resp: &Value) -> Result<(), InnertubeError> {
    if let Some(err) = resp.get("error") {
        let msg = err
            .as_object()
            .and_then(|o| o.get("message"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| err.to_string());
        return Err(InnertubeError::ApiRefused(msg));
    }
    Ok(())
}

fn find_string(element: &Value, key: &str) -> Option<String> {
    match element {
        Value::Object(map) => {
            if let Some(s) = map.get(key).and_then(Value::as_str) {
                return Some(s.to_owned());
            }
            map.values().find_map(|v| find_string(v, key))
        }
        Value::Array(arr) => arr.iter().find_map(|v| find_string(v, key)),
        _ => None,
    }
}

fn find_visitor_data(element: &Value) -> Option<String> {
    match element {
        Value::Array(arr) => arr.iter().find_map(find_visitor_data),
        Value::String(s) if VISITOR_DATA_RE.is_match(s) => Some(s.clone()),
        _ => None,
    }
}

fn truncate_detail(s: &str) -> String {
    s.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_mock_innertube(
        responder: impl Fn(String, String, String) -> (u16, String) + Send + Sync + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let responder = Arc::new(responder);

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let responder = Arc::clone(&responder);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos;
                        }
                    };
                    let headers_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let req_line = headers_str.lines().next().unwrap_or_default().to_owned();
                    let content_len: usize = headers_str
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    while buf.len() < body_start + content_len {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(
                        &buf[body_start..(body_start + content_len).min(buf.len())],
                    )
                    .into_owned();
                    let (code, resp_body) = responder(req_line, headers_str, body);
                    let reason = if code == 200 { "OK" } else { "ERR" };
                    let resp = format!(
                        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        format!("http://{addr}")
    }

    #[tokio::test]
    async fn anonymous_search_strips_cookie_and_omits_gl_for_non_zh() {
        let base = spawn_mock_innertube(|req_line, headers, body| {
            if req_line.contains("/search") {
                assert!(
                    !headers.to_ascii_lowercase().contains("\ncookie:"),
                    "anonymous search must not send Cookie"
                );
                let parsed: Value = serde_json::from_str(&body).expect("json body");
                assert!(
                    parsed["context"]["client"].get("gl").is_none(),
                    "non-zh search should omit gl"
                );
                return (200, r#"{"contents":[]}"#.to_owned());
            }
            (200, "{}".to_owned())
        })
        .await;

        let client = InnertubeClient::with_endpoints(
            Arc::new(build_shared_http_client()),
            format!("{base}/youtubei/v1"),
            base.clone(),
            base,
        );
        client.set_app_language(Some("en".to_owned()));
        client.set_cookie(Some("SAPISID=secret123".to_owned()));

        let res = client
            .search("Bohemian Rhapsody", None, true)
            .await
            .expect("search ok");
        assert!(res.get("contents").is_some());
    }

    #[tokio::test]
    async fn writes_detect_http_200_api_refusal_and_harvest_visitor_data() {
        let base = spawn_mock_innertube(|req_line, headers, _body| {
            if req_line.starts_with("GET / ") {
                return (
                    200,
                    r#"{"LOGGED_IN":true,"INNERTUBE_CLIENT_VERSION":"1.20260707.12.00"}"#
                        .to_owned(),
                );
            }
            if req_line.contains("/like/like") {
                assert!(headers.to_ascii_lowercase().contains("authorization: sapisidhash "));
                return (
                    200,
                    r#"{"error":{"code":403,"message":"Rating refused"}}"#.to_owned(),
                );
            }
            if req_line.contains("/browse") {
                return (
                    200,
                    r#"{"responseContext":{"visitorData":"Cg1234567890123456789012345678901234567890"}}"#
                        .to_owned(),
                );
            }
            (200, "{}".to_owned())
        })
        .await;

        let client = InnertubeClient::with_endpoints(
            Arc::new(build_shared_http_client()),
            format!("{base}/youtubei/v1"),
            base.clone(),
            base,
        );
        assert!(matches!(
            client.rate("abc12345678", LikeStatus::Like).await,
            Err(InnertubeError::NotSignedIn)
        ));

        client.set_cookie(Some("SAPISID=mysecret".to_owned()));
        let err = client
            .rate("abc12345678", LikeStatus::Like)
            .await
            .expect_err("must reject 200 error body");
        assert!(matches!(err, InnertubeError::ApiRefused(msg) if msg.contains("Rating refused")));

        let _ = client.browse("FEmusic_home", None).await.expect("browse");
        assert_eq!(
            client.visitor_data().as_deref(),
            Some("Cg1234567890123456789012345678901234567890")
        );
    }

    #[tokio::test]
    async fn visitor_bootstrap_strips_xssi_prefix() {
        let base = spawn_mock_innertube(|req_line, _, _| {
            if req_line.contains("/sw.js_data") {
                return (
                    200,
                    ")]}'\n[[\"ignored\",[\"CgABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd\"]]]"
                        .to_owned(),
                );
            }
            (404, "{}".to_owned())
        })
        .await;
        let client = InnertubeClient::with_endpoints(
            Arc::new(build_shared_http_client()),
            format!("{base}/youtubei/v1"),
            base.clone(),
            base,
        );
        assert_eq!(
            client.ensure_visitor_data(false).await.as_deref(),
            Some("CgABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd")
        );
    }
}
