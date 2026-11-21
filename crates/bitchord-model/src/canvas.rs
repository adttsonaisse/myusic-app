use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

static ARTIST_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\s*,\s*|\s*&\s*|\s+×\s+|\s+x\s+|\bfeat\.?\b|\bft\.?\b|\bfeaturing\b|\bwith\b)",
    )
    .expect("valid static regex")
});
static VIDEO_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.mp4(?:[?#]|$)").expect("valid static regex"));
static OPERATION_DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["'](\w+)["']\s*,\s*["']query["']\s*,\s*["']([0-9a-f]{64})["']"#)
        .expect("valid static regex")
});
static WEB_PLAYER_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https://open\.spotifycdn\.com/cdn/build/web-player/[A-Za-z0-9._~-]+\.js")
        .expect("valid static regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CanvasSource {
    Spotify,
    #[default]
    Other,
}

/// A looping motion-artwork video (Spotify Canvas or Apple motion artwork).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasArtwork {
    pub url: String,
    #[serde(default)]
    pub fallback_url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub source: CanvasSource,
    #[serde(default)]
    pub portrait_url: Option<String>,
}

impl CanvasArtwork {
    /// Checks whether this clip truly belongs to the requested track.
    pub fn matches(&self, want_title: &str, want_artist: &str, want_album: Option<&str>) -> bool {
        let title_ok = match self.title.as_deref() {
            None => true,
            Some(_) if want_title.trim().is_empty() => true,
            Some(t) => normalize_for_match(t) == normalize_for_match(want_title),
        };

        let title_artists = split_artists(want_artist);
        let our_artists = split_artists(self.artist.as_deref().unwrap_or(""));
        let artist_ok = match self.artist.as_deref() {
            None => true,
            Some(_) if want_artist.trim().is_empty() => true,
            Some(_) => {
                !title_artists.is_empty()
                    && !our_artists.is_empty()
                    && title_artists.iter().all(|w| our_artists.contains(w))
            }
        };

        let album_ok = match (
            self.album.as_deref().filter(|a| !a.trim().is_empty()),
            want_album.filter(|a| !a.trim().is_empty()),
        ) {
            (Some(a), Some(w)) => normalize_for_match(a) == normalize_for_match(w),
            _ => true,
        };

        title_ok && artist_ok && album_ok
    }
}

/// Folds case, combining accents, and non-ASCII-alphanumeric punctuation away for canvas matching.
/// Note: deliberately ASCII-only (`[^a-z0-9\s]`), matching BitChord's `CanvasArtwork.kt`.
pub fn normalize_for_match(raw: &str) -> String {
    let stripped: String = raw
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect();
    let lower = stripped.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_ws = false;
    for ch in lower.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            in_ws = false;
        } else if !out.is_empty() && !in_ws {
            out.push(' ');
            in_ws = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Splits a credit string into normalized individual artist names for canvas matching.
pub fn split_artists(raw: &str) -> Vec<String> {
    ARTIST_SEPARATORS
        .split(raw)
        .map(normalize_for_match)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Result of parsing a Spotify Pathfinder v2 `canvas` GraphQL response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotifyCanvasAnswer {
    Found {
        url: String,
        media_type: Option<String>,
    },
    NoCanvas {
        detail: String,
    },
    Failed {
        reason: String,
        stale_hash: bool,
    },
}

/// Pure request/response helpers for Spotify Pathfinder v2 `canvas` GraphQL query.
pub struct SpotifyCanvasQuery;

impl SpotifyCanvasQuery {
    pub const ENDPOINT: &'static str = "https://api-partner.spotify.com/pathfinder/v2/query";
    pub const OPERATION: &'static str = "canvas";
    pub const KNOWN_CANVAS_HASH: &'static str =
        "575138ab27cd5c1b3e54da54d0a7cc8d85485402de26340c2145f0f6bb5e7a9f";

    pub fn request_body(track_uri: &str, hash: &str) -> String {
        json!({
            "operationName": Self::OPERATION,
            "variables": { "trackUri": track_uri },
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": hash,
                }
            }
        })
        .to_string()
    }

    pub fn parse(body: &str) -> SpotifyCanvasAnswer {
        let Ok(Value::Object(root)) = serde_json::from_str::<Value>(body) else {
            return SpotifyCanvasAnswer::Failed {
                reason: "not JSON".to_owned(),
                stale_hash: false,
            };
        };

        let errors: Vec<String> = root
            .get("errors")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_object()?.get("message")?.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let Some(data) = root.get("data").and_then(Value::as_object) else {
            let reason = if errors.is_empty() {
                "no data".to_owned()
            } else {
                errors.join("; ")
            };
            let stale_hash = errors
                .iter()
                .any(|e| e.to_ascii_lowercase().contains("persistedquerynotfound"));
            return SpotifyCanvasAnswer::Failed { reason, stale_hash };
        };

        let Some(canvas) = data
            .get("trackUnion")
            .and_then(Value::as_object)
            .and_then(|tu| tu.get("canvas"))
            .and_then(Value::as_object)
        else {
            return SpotifyCanvasAnswer::NoCanvas {
                detail: "canvas null".to_owned(),
            };
        };

        let media_type = json_text(canvas.get("type"));
        let file_id = json_text(canvas.get("fileId"));
        let url_opt = json_text(canvas.get("url"));
        let detail = format!(
            "type={} fileId={} url={}",
            media_type.as_deref().unwrap_or("null"),
            file_id.as_deref().unwrap_or("null"),
            url_opt.as_deref().unwrap_or("null"),
        );

        let Some(url) = url_opt.filter(|u| u.starts_with("https://")) else {
            return SpotifyCanvasAnswer::NoCanvas { detail };
        };
        if !VIDEO_URL.is_match(&url) {
            return SpotifyCanvasAnswer::NoCanvas { detail };
        }
        SpotifyCanvasAnswer::Found { url, media_type }
    }

    pub fn find_query_hash(script: &str, operation: &str) -> Option<String> {
        OPERATION_DECLARATION
            .captures_iter(script)
            .find(|caps| caps.get(1).is_some_and(|m| m.as_str() == operation))
            .and_then(|caps| caps.get(2).map(|m| m.as_str().to_owned()))
    }

    pub fn web_player_scripts(html: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for m in WEB_PLAYER_SCRIPT.find_iter(html) {
            let s = m.as_str().to_owned();
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
        out
    }
}

fn json_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

struct HashCacheState {
    hash: Option<String>,
    checked_at_ms: i64,
}

/// Caches the live Spotify Pathfinder `canvas` persisted-query hash fetched via an injected GET closure.
pub struct QueryHashes<F, N = fn() -> i64> {
    fetch: F,
    now_ms: N,
    max_scripts: usize,
    state: Mutex<HashCacheState>,
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl<F: Fn(&str) -> Option<String>> QueryHashes<F, fn() -> i64> {
    pub fn new(fetch: F) -> Self {
        Self::with_clock(fetch, system_now_ms, 8)
    }
}

impl<F: Fn(&str) -> Option<String>, N: Fn() -> i64> QueryHashes<F, N> {
    const FOUND_TTL_MS: i64 = 12 * 60 * 60 * 1000;
    const MISSED_TTL_MS: i64 = 30 * 60 * 1000;

    pub fn with_clock(fetch: F, now_ms: N, max_scripts: usize) -> Self {
        Self {
            fetch,
            now_ms,
            max_scripts,
            state: Mutex::new(HashCacheState {
                hash: None,
                checked_at_ms: 0,
            }),
        }
    }

    pub fn canvas_hash(&self, force_refresh: bool) -> String {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.now_ms)();
        let age = now - state.checked_at_ms;
        let ttl = if state.hash.is_some() {
            Self::FOUND_TTL_MS
        } else {
            Self::MISSED_TTL_MS
        };
        if !force_refresh && state.checked_at_ms != 0 && age < ttl {
            return state
                .hash
                .clone()
                .unwrap_or_else(|| SpotifyCanvasQuery::KNOWN_CANVAS_HASH.to_owned());
        }

        state.checked_at_ms = now;
        if let Some(found) = self.look_up() {
            state.hash = Some(found);
        }
        state
            .hash
            .clone()
            .unwrap_or_else(|| SpotifyCanvasQuery::KNOWN_CANVAS_HASH.to_owned())
    }

    fn look_up(&self) -> Option<String> {
        let html = (self.fetch)("https://open.spotify.com/")?;
        for script in SpotifyCanvasQuery::web_player_scripts(&html)
            .into_iter()
            .take(self.max_scripts)
        {
            if let Some(body) = (self.fetch)(&script)
                && let Some(hash) =
                    SpotifyCanvasQuery::find_query_hash(&body, SpotifyCanvasQuery::OPERATION)
            {
                return Some(hash);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_for_match_folds_accents_punctuation_and_non_latin() {
        assert_eq!(
            "beyonce crazy in love feat jay z",
            normalize_for_match("Beyoncé - CRAZY IN LOVE (feat. JAY-Z)")
        );
        // Non-Latin scripts fold to empty, matching ArtistNameMatchTest's invariant.
        for name in ["Мата", "Мета", "中島美嘉", "माता"] {
            assert_eq!("", normalize_for_match(name));
        }
    }

    #[test]
    fn canvas_artwork_matches_metadata() {
        let art = CanvasArtwork {
            url: "https://canvaz.scdn.co/upload/video.mp4".to_owned(),
            fallback_url: None,
            title: Some("Crazy in Love".to_owned()),
            artist: Some("Beyoncé, JAY-Z".to_owned()),
            album: Some("Dangerously in Love".to_owned()),
            source: CanvasSource::Spotify,
            portrait_url: None,
        };
        assert!(art.matches("CRAZY IN LOVE!", "Beyonce", None));
        assert!(art.matches(
            "Crazy in Love",
            "Beyoncé & Jay-Z",
            Some("Dangerously in Love")
        ));
        assert!(!art.matches("Halo", "Beyoncé", None));
        assert!(!art.matches("Crazy in Love", "Beyoncé", Some("Lemonade")));
    }

    #[test]
    fn spotify_canvas_query_parses_found_no_canvas_and_stale_hash() {
        let found = r#"{"data":{"trackUnion":{"canvas":{"url":"https://canvaz.scdn.co/a.mp4?x=1","type":"VIDEO_LOOPING_RANDOM"}}}}"#;
        assert_eq!(
            SpotifyCanvasAnswer::Found {
                url: "https://canvaz.scdn.co/a.mp4?x=1".to_owned(),
                media_type: Some("VIDEO_LOOPING_RANDOM".to_owned()),
            },
            SpotifyCanvasQuery::parse(found)
        );

        let image_only = r#"{"data":{"trackUnion":{"canvas":{"url":"https://canvaz.scdn.co/a.jpg","type":"IMAGE"}}}}"#;
        assert!(matches!(
            SpotifyCanvasQuery::parse(image_only),
            SpotifyCanvasAnswer::NoCanvas { .. }
        ));

        let stale = r#"{"errors":[{"message":"PersistedQueryNotFound"}]}"#;
        assert_eq!(
            SpotifyCanvasAnswer::Failed {
                reason: "PersistedQueryNotFound".to_owned(),
                stale_hash: true,
            },
            SpotifyCanvasQuery::parse(stale)
        );

        let hash = "a".repeat(64);
        let script = format!(r#"define("canvas","query","{hash}")"#);
        assert_eq!(
            Some(hash.clone()),
            SpotifyCanvasQuery::find_query_hash(&script, "canvas")
        );

        let hashes = QueryHashes::with_clock(
            |url| {
                if url == "https://open.spotify.com/" {
                    Some(
                        r#"<script src="https://open.spotifycdn.com/cdn/build/web-player/web-player.abc.js"></script>"#
                            .to_owned(),
                    )
                } else {
                    Some(script.clone())
                }
            },
            || 10_000,
            8,
        );
        assert_eq!(hash, hashes.canvas_hash(false));
    }
}
