use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, parse_lrc};

const BASE: &str = "https://lrclib.net/api";

static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\((?:from|feat\.?|official|lyrical|video|audio|remix)[^)]*\)|\[[^\]]*\]|\b(?:official (?:video|audio|music video)|lyrical|full song|4k video)\b",
    )
    // INFALLIBLE: static regex pattern verified by tests.
    .expect("valid LrcLib NOISE regex")
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+")
        // INFALLIBLE: static regex pattern.
        .expect("valid whitespace regex")
});

/// Lyrics from LRCLIB — a free, key-less community lyrics database.
///
/// Exact `get` keyed on artist + title + duration, falling back to fuzzy
/// `search` ranked by duration proximity.
pub struct LrcLib;

impl LrcLib {
    /// Synced lyrics for a track, or `None` when nothing usable is published.
    pub async fn lyrics(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        Self::lyrics_at(http, BASE, title, artist, duration_ms).await
    }

    pub(crate) async fn lyrics_at(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        let clean_title = clean(title);
        let clean_artist = clean(artist);
        let seconds = (duration_ms / 1_000) as i32;

        let synced = match Self::exact_match(http, base, &clean_title, &clean_artist, seconds).await
        {
            Some(lrc) => Some(lrc),
            None => Self::best_search_hit(http, base, &clean_title, &clean_artist, seconds).await,
        }?;

        let lines = parse_lrc(&synced);
        if lines.is_empty() { None } else { Some(lines) }
    }

    /// Re-export of [`crate::parse_lrc`] for parity with Kotlin `LrcLib.parseLrc`.
    pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
        parse_lrc(lrc)
    }

    async fn exact_match(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        seconds: i32,
    ) -> Option<String> {
        let seconds_str = seconds.to_string();
        let url = build_url(
            &format!("{base}/get"),
            &[
                ("track_name", title),
                ("artist_name", artist),
                ("duration", &seconds_str),
            ],
        )?;
        let body = http.get_slow(&url).await?;
        let val: Value = serde_json::from_str(&body).ok()?;
        val.as_object()?
            .get("syncedLyrics")?
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
    }

    async fn best_search_hit(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        seconds: i32,
    ) -> Option<String> {
        let url = build_url(
            &format!("{base}/search"),
            &[("track_name", title), ("artist_name", artist)],
        )?;
        let body = http.get_slow(&url).await?;
        let hits = serde_json::from_str::<Value>(&body).ok()?;
        let array = hits.as_array()?;
        let target = f64::from(seconds);

        array
            .iter()
            .filter_map(Value::as_object)
            .filter(|obj| {
                obj.get("syncedLyrics")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
            })
            .min_by(|a, b| {
                let da = (a.get("duration").and_then(Value::as_f64).unwrap_or(0.0) - target).abs();
                let db = (b.get("duration").and_then(Value::as_f64).unwrap_or(0.0) - target).abs();
                da.total_cmp(&db)
            })
            .and_then(|obj| obj.get("syncedLyrics"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }
}

fn clean(value: &str) -> String {
    let stripped = NOISE.replace_all(value, " ");
    let before_pipe = stripped.split(" | ").next().unwrap_or(&stripped);
    let collapsed = WHITESPACE.replace_all(before_pipe, " ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        value.to_owned()
    } else {
        trimmed.to_owned()
    }
}
