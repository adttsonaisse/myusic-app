use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, ProviderLyrics};

const BASE: &str = "https://lyrics-api.boidu.dev/getLyrics";
const PORTATO: &str = "https://lyrics-api.boidu.dev/qq/getLyrics";

/// Word-timed lyrics from BetterLyrics (Apple Music TTML and QQ Portato).
pub struct BetterLyrics;

impl BetterLyrics {
    pub async fn lyrics(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        Self::fetch(http, BASE, title, artist, duration_ms, album).await
    }

    /// QQ Music karaoke timings through BetterLyrics' Portato endpoint.
    pub async fn portato(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        Self::fetch(http, PORTATO, title, artist, duration_ms, album).await
    }

    pub(crate) async fn fetch(
        http: &LyricsHttp,
        endpoint: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let mut params: Vec<(&str, String)> =
            vec![("s", title.to_owned()), ("a", artist.to_owned())];
        let seconds = duration_ms / 1_000;
        if seconds > 0 {
            params.push(("d", seconds.to_string()));
        }
        if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
            params.push(("al", alb.to_owned()));
        }
        let borrowed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = build_url(endpoint, &borrowed)?;
        let body = http.get(&url).await?;
        ProviderLyrics::parse(&body)
    }
}
