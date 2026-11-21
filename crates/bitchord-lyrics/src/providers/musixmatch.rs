use std::sync::{LazyLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, TimedRun, parse_lrc, with_instrumental_gaps, words_from_runs};

const BASE: &str = "https://apic.musixmatch.com/ws/1.1";
const APP_ID: &str = "mobile-app-v1.0";
const SEARCH_PAGE: &str = "https://www.musixmatch.com/search";
const BROWSER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const FALLBACK_SIGNING_SECRET: &str = "f09016176ba43a1cfd1031fbd6b3d26c";

static APP_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)src=["']([^"']*/_next/static/chunks/pages/_app-[^"']+\.js)["']"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid APP_SCRIPT regex")
});

static ENCODED_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"from\(\s*["']([^"']+)["']\s*\.split"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid ENCODED_SECRET regex")
});

#[derive(Deserialize)]
struct Envelope<T> {
    message: Message<T>,
}

#[derive(Deserialize)]
struct Message<T> {
    header: Header,
    body: Option<T>,
}

#[derive(Deserialize)]
struct Header {
    #[serde(default)]
    status_code: i32,
}

#[derive(Deserialize)]
struct TokenBody {
    user_token: String,
}

#[derive(Deserialize)]
struct TrackSearchBody {
    #[serde(default)]
    track_list: Vec<TrackWrapper>,
}

#[derive(Deserialize)]
struct TrackWrapper {
    track: Track,
}

#[derive(Deserialize)]
struct Track {
    track_id: i64,
    track_name: String,
    #[serde(default)]
    artist_name: String,
    track_length: Option<i32>,
    has_subtitles: Option<i32>,
    has_richsync: Option<i32>,
}

#[derive(Deserialize)]
struct SubtitleBody {
    subtitle: Option<Subtitle>,
}

#[derive(Deserialize)]
struct Subtitle {
    subtitle_body: String,
}

#[derive(Deserialize)]
struct SubtitleLine {
    text: String,
    time: SubtitleTime,
}

#[derive(Deserialize)]
struct SubtitleTime {
    total: f64,
}

#[derive(Deserialize)]
struct RichSyncBody {
    richsync: Option<RichSync>,
}

#[derive(Deserialize)]
struct RichSync {
    richsync_body: Option<String>,
}

#[derive(Deserialize)]
struct RichSyncEntry {
    #[serde(rename = "ts")]
    start_seconds: f64,
    #[serde(rename = "te")]
    end_seconds: f64,
    #[serde(rename = "l", default)]
    fragments: Vec<RichSyncFragment>,
    #[serde(rename = "x", default)]
    text: String,
}

#[derive(Deserialize)]
struct RichSyncFragment {
    #[serde(rename = "c")]
    text: String,
    #[serde(rename = "o")]
    offset_seconds: f64,
}

/// Rich word timing, with line-synced fallback, from Musixmatch's web API.
#[derive(Debug)]
pub struct Musixmatch {
    token_mutex: Mutex<()>,
    cached_secret: RwLock<Option<String>>,
    cached_token: RwLock<Option<String>>,
    process_guid: String,
}

impl Default for Musixmatch {
    fn default() -> Self {
        Self::new()
    }
}

impl Musixmatch {
    pub fn new() -> Self {
        Self {
            token_mutex: Mutex::new(()),
            cached_secret: RwLock::new(None),
            cached_token: RwLock::new(None),
            process_guid: Uuid::new_v4().to_string(),
        }
    }

    pub async fn lyrics(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        let seconds = (duration_ms / 1_000) as i32;
        let track = self.best_track(http, title, artist, seconds).await?;

        if track.has_richsync.unwrap_or(0) != 0
            && let Some(body) = self.fetch_rich_sync(http, track.track_id).await
        {
            let rich = Self::parse_rich_sync_body(&body);
            if rich.iter().any(LyricLine::is_word_synced) {
                return Some(rich);
            }
        }

        if track.has_subtitles.unwrap_or(0) == 0 {
            return None;
        }
        let subtitle = self.fetch_subtitle(http, track.track_id).await?;
        let lrc = subtitle_to_lrc(&subtitle);
        if lrc.trim().is_empty() {
            return None;
        }
        let lines = parse_lrc(&lrc);
        if lines.is_empty() { None } else { Some(lines) }
    }

    async fn best_track(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        seconds: i32,
    ) -> Option<Track> {
        let tracks = self.search_track(http, title, artist).await?;
        let mut best: Option<(Track, f64)> = None;
        for track in tracks {
            let s = score_track(&track, title, artist, seconds);
            if best.as_ref().is_none_or(|(_, bs)| s > *bs) {
                best = Some((track, s));
            }
        }
        best.map(|(t, _)| t)
    }

    async fn search_track(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
    ) -> Option<Vec<Track>> {
        let response = self
            .signed_get(http, |token| {
                build_url(
                    &format!("{BASE}/track.search"),
                    &[
                        ("app_id", APP_ID),
                        ("format", "json"),
                        ("q_track", title),
                        ("q_artist", artist),
                        ("f_has_lyrics", "1"),
                        ("s_track_rating", "desc"),
                        ("quorum_factor", "1"),
                        ("page_size", "10"),
                        ("page", "1"),
                        ("usertoken", token),
                    ],
                )
            })
            .await?;
        let env: Envelope<TrackSearchBody> = serde_json::from_str(&response).ok()?;
        Some(
            env.message
                .body?
                .track_list
                .into_iter()
                .map(|w| w.track)
                .collect(),
        )
    }

    async fn fetch_subtitle(&self, http: &LyricsHttp, track_id: i64) -> Option<String> {
        let id_str = track_id.to_string();
        let response = self
            .signed_get(http, |token| {
                build_url(
                    &format!("{BASE}/track.subtitle.get"),
                    &[
                        ("app_id", APP_ID),
                        ("format", "json"),
                        ("track_id", &id_str),
                        ("subtitle_format", "mxm"),
                        ("usertoken", token),
                    ],
                )
            })
            .await?;
        let env: Envelope<SubtitleBody> = serde_json::from_str(&response).ok()?;
        Some(env.message.body?.subtitle?.subtitle_body)
    }

    async fn fetch_rich_sync(&self, http: &LyricsHttp, track_id: i64) -> Option<String> {
        let id_str = track_id.to_string();
        let response = self
            .signed_get(http, |token| {
                build_url(
                    &format!("{BASE}/track.richsync.get"),
                    &[
                        ("app_id", APP_ID),
                        ("format", "json"),
                        ("track_id", &id_str),
                        ("usertoken", token),
                    ],
                )
            })
            .await?;
        let env: Envelope<RichSyncBody> = serde_json::from_str(&response).ok()?;
        env.message.body?.richsync?.richsync_body
    }

    async fn signed_get<F>(&self, http: &LyricsHttp, build: F) -> Option<String>
    where
        F: Fn(&str) -> Option<String>,
    {
        let secret = self.get_secret(http).await;
        let token = self.get_token(http, &secret).await?;
        let url = build(&token)?;
        let first = api_get(http, &Self::sign(&url, &secret)).await;
        if let Some(body) = first
            && !looks_unauthorized(&body)
        {
            return Some(body);
        }

        if let Ok(mut g) = self.cached_token.write() {
            *g = None;
        }
        if let Ok(mut g) = self.cached_secret.write() {
            *g = None;
        }
        let fresh_secret = self.get_secret(http).await;
        let fresh_token = self.get_token(http, &fresh_secret).await?;
        let fresh_url = build(&fresh_token)?;
        api_get(http, &Self::sign(&fresh_url, &fresh_secret)).await
    }

    async fn get_token(&self, http: &LyricsHttp, secret: &str) -> Option<String> {
        if let Ok(guard) = self.cached_token.read()
            && let Some(tok) = guard.as_ref()
        {
            return Some(tok.clone());
        }
        let _lock = self.token_mutex.lock().await;
        if let Ok(guard) = self.cached_token.read()
            && let Some(tok) = guard.as_ref()
        {
            return Some(tok.clone());
        }
        let url = build_url(
            &format!("{BASE}/token.get"),
            &[
                ("app_id", APP_ID),
                ("guid", &self.process_guid),
                ("format", "json"),
            ],
        )?;
        let body = api_get(http, &Self::sign(&url, secret)).await?;
        let env: Envelope<TokenBody> = serde_json::from_str(&body).ok()?;
        let tok = env.message.body?.user_token;
        if let Ok(mut guard) = self.cached_token.write() {
            *guard = Some(tok.clone());
        }
        Some(tok)
    }

    async fn get_secret(&self, http: &LyricsHttp) -> String {
        if let Ok(guard) = self.cached_secret.read()
            && let Some(secret) = guard.as_ref()
        {
            return secret.clone();
        }
        let secret = scrape_signing_secret(http)
            .await
            .unwrap_or_else(|| FALLBACK_SIGNING_SECRET.to_owned());
        if let Ok(mut guard) = self.cached_secret.write() {
            *guard = Some(secret.clone());
        }
        secret
    }

    /// Decodes Musixmatch rich-sync JSON directly into word/syllable-timed
    /// [`LyricLine`]s without flattening through line LRC.
    pub fn parse_rich_sync_body(body: &str) -> Vec<LyricLine> {
        let Ok(entries) = serde_json::from_str::<Vec<RichSyncEntry>>(body) else {
            return Vec::new();
        };
        let mut lines: Vec<LyricLine> = entries
            .into_iter()
            .filter_map(|entry| {
                let line_start = seconds_to_ms(entry.start_seconds);
                let line_end = line_start.max(seconds_to_ms(entry.end_seconds));
                let mut previous_start = line_start;

                let mut runs = Vec::new();
                for (index, fragment) in entry.fragments.iter().enumerate() {
                    if fragment.text.is_empty() {
                        continue;
                    }
                    let start = line_start
                        .max(previous_start)
                        .max(seconds_to_ms(entry.start_seconds + fragment.offset_seconds));
                    let next = entry.fragments.get(index + 1).map_or(line_end, |f| {
                        seconds_to_ms(entry.start_seconds + f.offset_seconds)
                    });
                    previous_start = start;
                    runs.push(TimedRun::new(
                        start,
                        start.max(line_end.min(next)),
                        fragment.text.clone(),
                    ));
                }

                let words = words_from_runs(&runs, true);
                let trimmed = entry.text.trim();
                let text = if trimmed.is_empty() {
                    words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    trimmed.to_owned()
                };
                if text.is_empty() {
                    return None;
                }
                let time_ms = line_start.min(words.first().map_or(line_start, |w| w.start_ms));
                Some(LyricLine {
                    time_ms,
                    text,
                    words,
                    sung_until_ms: (line_end > line_start).then_some(line_end),
                    ..Default::default()
                })
            })
            .collect();

        lines.sort_by_key(|l| l.time_ms);
        with_instrumental_gaps(lines)
    }

    /// Signs `<normalized_url><UTC yyyyMMdd>` with HMAC-SHA256.
    pub fn sign(url: &str, secret: &str) -> String {
        let date = utc_yyyymmdd(SystemTime::now());
        Self::sign_for_date(url, secret, &date)
    }

    pub(crate) fn sign_for_date(url: &str, secret: &str, utc_date: &str) -> String {
        let normalized = url.replace("%20", "+").replace(' ', "+");
        let payload = format!("{normalized}{utc_date}");
        type HmacSha256 = Hmac<Sha256>;
        let signature = match HmacSha256::new_from_slice(secret.as_bytes()) {
            Ok(mut mac) => {
                mac.update(payload.as_bytes());
                STANDARD.encode(mac.finalize().into_bytes())
            }
            Err(_) => String::new(),
        };
        let encoded = encode_base64_for_query(&signature);
        format!("{normalized}&signature={encoded}&signature_protocol=sha256")
    }
}

async fn scrape_signing_secret(http: &LyricsHttp) -> Option<String> {
    let page = browser_get(http, SEARCH_PAGE, "text/html,application/xhtml+xml").await?;
    let script_ref = APP_SCRIPT.captures(&page)?.get(1)?.as_str();
    let script_url = Url::parse(SEARCH_PAGE).ok()?.join(script_ref).ok()?;
    let javascript = browser_get(http, script_url.as_str(), "*/*").await?;
    let encoded = ENCODED_SECRET.captures(&javascript)?.get(1)?.as_str();
    let reversed: String = encoded.chars().rev().collect();
    let decoded = STANDARD.decode(reversed.as_bytes()).ok()?;
    let secret = String::from_utf8(decoded).ok()?;
    if secret.trim().is_empty() {
        None
    } else {
        Some(secret)
    }
}

async fn api_get(http: &LyricsHttp, url: &str) -> Option<String> {
    http.get_custom(
        url,
        BROWSER_AGENT,
        "application/json, text/plain, */*",
        None,
    )
    .await
}

async fn browser_get(http: &LyricsHttp, url: &str, accept: &str) -> Option<String> {
    http.get_custom(url, BROWSER_AGENT, accept, Some("mxm_bab=AB"))
        .await
}

fn looks_unauthorized(body: &str) -> bool {
    serde_json::from_str::<Envelope<Value>>(body)
        .ok()
        .is_some_and(|env| matches!(env.message.header.status_code, 401 | 402))
}

fn score_track(track: &Track, title: &str, artist: &str, seconds: i32) -> f64 {
    let mut score = 0.0;
    let name = track.track_name.trim().to_lowercase();
    let target_title = title.trim().to_lowercase();
    if name == target_title {
        score += 80.0;
    } else if name.contains(&target_title) || target_title.contains(&name) {
        score += 40.0;
    }
    if track
        .artist_name
        .trim()
        .to_lowercase()
        .contains(&artist.trim().to_lowercase())
    {
        score += 40.0;
    }
    if let Some(length) = track.track_length {
        let diff = (length - seconds).abs();
        score += if diff <= 2 {
            30.0
        } else if diff <= 5 {
            15.0
        } else if diff <= 10 {
            5.0
        } else {
            -20.0
        };
    }
    score
}

fn subtitle_to_lrc(subtitle_body: &str) -> String {
    let Ok(lines) = serde_json::from_str::<Vec<SubtitleLine>>(subtitle_body) else {
        return String::new();
    };
    let mut out = String::new();
    for line in lines {
        if line.text.trim().is_empty() {
            continue;
        }
        let total_ms = seconds_to_ms(line.time.total).max(0);
        let minutes = total_ms / 1_000 / 60;
        let seconds = (total_ms / 1_000) % 60;
        let millis = total_ms % 1_000;
        out.push_str(&format!(
            "[{minutes:02}:{seconds:02}.{millis:03}]{}\n",
            line.text
        ));
    }
    out.trim().to_owned()
}

fn seconds_to_ms(seconds: f64) -> i64 {
    if !seconds.is_finite() {
        return 0;
    }
    (seconds * 1_000.0) as i64
}

fn encode_base64_for_query(sig: &str) -> String {
    let mut out = String::with_capacity(sig.len() + 8);
    for ch in sig.chars() {
        match ch {
            '+' => out.push_str("%2B"),
            '/' => out.push_str("%2F"),
            '=' => out.push_str("%3D"),
            _ => out.push(ch),
        }
    }
    out
}

/// Howard Hinnant's civil-from-days conversion for UTC `yyyyMMdd` without
/// pulling a full date/time crate.
fn utc_yyyymmdd(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    format!("{year:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_sync_keeps_word_timing_and_merges_syllable_fragments() {
        let body = r#"[{
            "ts":48.502,
            "te":50.813,
            "x":"I save dick by giving it CPR",
            "l":[
                {"c":"I","o":0.000},{"c":" ","o":0.120},
                {"c":"save ","o":0.180},{"c":"dick ","o":0.520},
                {"c":"by ","o":0.870},{"c":"giv","o":1.080},
                {"c":"ing ","o":1.240},{"c":"it ","o":1.560},
                {"c":"CPR","o":1.820}
            ]
        }]"#;

        let lines: Vec<LyricLine> = Musixmatch::parse_rich_sync_body(body)
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        assert_eq!(lines.len(), 1);
        let line = &lines[0];

        assert!(line.is_word_synced());
        assert_eq!(line.text, "I save dick by giving it CPR");
        let words: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            words,
            vec!["I", "save", "dick", "by", "giving", "it", "CPR"]
        );
        assert_eq!(line.time_ms, 48_502);
        assert_eq!(line.words[4].start_ms, 49_582);
        assert_eq!(line.words.last().map(|w| w.end_ms), Some(50_813));
    }

    #[test]
    fn rich_sync_never_emits_backward_word_times() {
        let body = r#"[{"ts":10.0,"te":12.0,"x":"one two three","l":[
            {"c":"one ","o":0.0},
            {"c":"two ","o":0.7},
            {"c":"three","o":0.65}
        ]}]"#;

        let lines: Vec<LyricLine> = Musixmatch::parse_rich_sync_body(body)
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        assert_eq!(lines.len(), 1);
        let words = &lines[0].words;
        let starts: Vec<i64> = words.iter().map(|w| w.start_ms).collect();
        assert_eq!(starts, vec![10_000, 10_700, 10_700]);
        assert!(words.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
        assert!(words.iter().all(|w| w.end_ms >= w.start_ms));
    }

    #[test]
    fn malformed_rich_sync_is_ignored() {
        assert!(Musixmatch::parse_rich_sync_body("not json").is_empty());
    }

    #[test]
    fn utc_date_conversion_matches_known_epochs() {
        use std::time::Duration;
        assert_eq!(utc_yyyymmdd(UNIX_EPOCH), "19700101");
        // 2026-10-04 00:00:00 UTC = 1791072000
        assert_eq!(
            utc_yyyymmdd(UNIX_EPOCH + Duration::from_secs(1_791_072_000)),
            "20261004"
        );
    }

    #[test]
    fn hmac_signing_normalizes_spaces_and_appends_protocol() {
        let signed = Musixmatch::sign_for_date(
            "https://apic.musixmatch.com/ws/1.1/track.search?q_track=hello%20world",
            "test-secret",
            "20261004",
        );
        assert!(signed.starts_with(
            "https://apic.musixmatch.com/ws/1.1/track.search?q_track=hello+world&signature="
        ));
        assert!(signed.ends_with("&signature_protocol=sha256"));
    }
}
