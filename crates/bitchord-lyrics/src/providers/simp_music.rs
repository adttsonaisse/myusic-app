use serde::{Deserialize, Serialize};

use crate::http::LyricsHttp;
use crate::{EnhancedLrc, LyricLine, parse_lrc};

const BASE: &str = "https://api-lyrics.simpmusic.org/v1/";
const DURATION_TOLERANCE_SECONDS: i32 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpMusicResponse {
    #[serde(default)]
    pub success: bool,
    pub data: Option<Vec<SimpMusicTrack>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimpMusicTrack {
    pub duration: Option<i32>,
    pub rich_sync_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
    pub plain_lyrics: Option<String>,
}

/// Lyrics from SimpMusic's community database, keyed by YouTube video ID.
pub struct SimpMusicLyrics;

impl SimpMusicLyrics {
    pub async fn lyrics(
        http: &LyricsHttp,
        video_id: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        Self::lyrics_at(http, BASE, video_id, duration_ms).await
    }

    pub(crate) async fn lyrics_at(
        http: &LyricsHttp,
        base: &str,
        video_id: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        if video_id.trim().is_empty() {
            return None;
        }
        let body = http.get(&format!("{base}{video_id}")).await?;
        let resp: SimpMusicResponse = serde_json::from_str(&body).ok()?;
        if !resp.success {
            return None;
        }

        let seconds = (duration_ms / 1_000) as i32;
        let track = resp
            .data
            .unwrap_or_default()
            .into_iter()
            .filter(|t| {
                seconds <= 0
                    || (t.duration.unwrap_or(0) - seconds).abs() <= DURATION_TOLERANCE_SECONDS
            })
            .min_by_key(|t| (t.duration.unwrap_or(0) - seconds).abs())?;

        if let Some(rich) = track
            .rich_sync_lyrics
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let parsed = EnhancedLrc::parse(rich);
            if !parsed.is_empty() {
                return Some(parsed);
            }
        }
        let synced = track
            .synced_lyrics
            .as_deref()
            .filter(|s| !s.trim().is_empty())?;
        let parsed = parse_lrc(synced);
        if parsed.is_empty() {
            None
        } else {
            Some(parsed)
        }
    }
}
