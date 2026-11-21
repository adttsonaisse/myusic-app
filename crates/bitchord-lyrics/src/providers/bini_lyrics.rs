use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, TtmlLyrics};

const BASE: &str = "https://lyrics-api.binimum.org/";

/// Lyrics and the ISRC of the recording they were matched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiniMatch {
    pub isrc: Option<String>,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiniResponse {
    pub total: Option<i32>,
    pub source: Option<String>,
    pub results: Option<Vec<BiniHit>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiniHit {
    #[serde(rename = "track_name")]
    pub track_name: Option<String>,
    #[serde(rename = "artist_name")]
    pub artist_name: Option<String>,
    #[serde(rename = "album_name")]
    pub album_name: Option<String>,
    pub duration: Option<i32>,
    pub isrc: Option<String>,
    #[serde(rename = "timing_type")]
    pub timing_type: Option<String>,
    #[serde(rename = "lyricsUrl")]
    pub lyrics_url: Option<String>,
}

/// Apple Music TTML via BiniLyrics — also identifies recordings by ISRC so
/// downstream sources (`lrc.red`, `LyricsPlus`) can query the exact recording.
pub struct BiniLyrics;

impl BiniLyrics {
    pub async fn identify(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
        isrc: Option<&str>,
    ) -> Option<BiniHit> {
        Self::identify_at(http, BASE, title, artist, duration_ms, album, isrc).await
    }

    pub(crate) async fn identify_at(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
        isrc: Option<&str>,
    ) -> Option<BiniHit> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(code) = isrc.filter(|s| !s.trim().is_empty()) {
            params.push(("isrc", code.to_owned()));
        } else {
            params.push(("track", title.to_owned()));
            params.push(("artist", artist.to_owned()));
            if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
                params.push(("album", alb.to_owned()));
            }
            let seconds = duration_ms / 1_000;
            if seconds > 0 {
                params.push(("duration", seconds.to_string()));
            }
        }
        let borrowed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = build_url(base, &borrowed)?;
        let body = http.get(&url).await?;
        let resp: BiniResponse = serde_json::from_str(&body).ok()?;
        resp.results?.into_iter().next()
    }

    /// Fetches and parses the TTML document referenced by a prior [`BiniHit`].
    pub async fn lyrics_for<F, Fut>(hit: &BiniHit, mut fetch_doc: F) -> Option<BiniMatch>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        let document = hit.lyrics_url.as_deref().filter(|s| !s.trim().is_empty())?;
        let ttml = fetch_doc(document.to_owned()).await?;
        let lines = TtmlLyrics::parse(&ttml);
        if lines.is_empty() {
            return None;
        }
        let isrc = hit
            .isrc
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned);
        Some(BiniMatch { isrc, lines })
    }

    pub async fn lyrics<F, Fut>(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
        isrc: Option<&str>,
        fetch_doc: F,
    ) -> Option<BiniMatch>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        let hit = Self::identify(http, title, artist, duration_ms, album, isrc).await?;
        Self::lyrics_for(&hit, fetch_doc).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINI_SEARCH: &str = concat!(
        r#"{"total":1,"source":"HIT-EXACT","results":[{"id":"69b0336e00361778ec55","#,
        r#""track_name":"golden hour","artist_name":"JVKE","#,
        r#""album_name":"golden hour - Single","duration":209,"isrc":"GBKPL2204171","#,
        r#""timing_type":"word","#,
        r#""lyricsUrl":"https://lyrics-storage.binimum.org/GBKPL2204171.ttml"}]}"#,
    );

    #[test]
    fn reads_a_binilyrics_search_hit() {
        let resp: BiniResponse = serde_json::from_str(BINI_SEARCH).expect("valid");
        let results = resp.results.expect("results");
        assert_eq!(results.len(), 1);
        let hit = &results[0];
        assert_eq!(resp.source.as_deref(), Some("HIT-EXACT"));
        assert_eq!(hit.isrc.as_deref(), Some("GBKPL2204171"));
        assert_eq!(hit.timing_type.as_deref(), Some("word"));
        assert_eq!(hit.track_name.as_deref(), Some("golden hour"));
        assert_eq!(hit.duration, Some(209));
        assert_eq!(
            hit.lyrics_url.as_deref(),
            Some("https://lyrics-storage.binimum.org/GBKPL2204171.ttml")
        );
    }

    #[test]
    fn binilyrics_response_with_no_results_is_not_a_match() {
        let resp: BiniResponse = serde_json::from_str(r#"{"total":0}"#).expect("valid");
        assert!(resp.results.and_then(|r| r.into_iter().next()).is_none());
    }
}
