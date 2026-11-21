use std::collections::HashSet;
use std::future::Future;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, TtmlLyrics};

const BASE: &str = "https://lrc.red/";
const DURATION_TOLERANCE_SECONDS: f64 = 3.0;

static ISRC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Z]{2}[A-Z0-9]{3}\d{7}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid ISRC regex")
});

static BRACKETED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[(\[][^)\]]*[)\]]")
        // INFALLIBLE: static regex pattern.
        .expect("valid BRACKETED regex")
});

static COMBINING_MARKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\p{Mn}+")
        // INFALLIBLE: static regex pattern.
        .expect("valid COMBINING_MARKS regex")
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+")
        // INFALLIBLE: static regex pattern.
        .expect("valid WHITESPACE regex")
});

static ARTIST_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*(?:,|&|;|/|\s+and\s+|\s+x\s+|\s+with\s+|\s+feat\.?\s+|\s+ft\.?\s+)\s*")
        // INFALLIBLE: static regex pattern.
        .expect("valid ARTIST_SEPARATORS regex")
});

const VERSION_WORDS: &[&str] = &[
    "live",
    "remix",
    "remixed",
    "mix",
    "acoustic",
    "unplugged",
    "instrumental",
    "karaoke",
    "cappella",
    "acapella",
    "demo",
    "edit",
    "version",
    "cover",
    "sped",
    "slowed",
    "reverb",
    "nightcore",
    "lofi",
    "orchestral",
    "extended",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LrcRedResponse {
    pub hits: Option<Vec<LrcRedHit>>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LrcRedHit {
    pub isrc: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i32>,
    /// Seconds, fractional.
    pub duration: Option<f64>,
}

/// `lrc.red` — Apple Music TTML, per-syllable, filed by ISRC.
pub struct LrcRed;

impl LrcRed {
    /// Returns `https://lrc.red/s/{ISRC}.ttml` only for well-formed 12-character
    /// ISRCs (guards against path traversal).
    pub fn document_url(isrc: &str) -> Option<String> {
        Self::document_url_with_base(BASE, isrc)
    }

    pub(crate) fn document_url_with_base(base: &str, isrc: &str) -> Option<String> {
        let clean = isrc.trim().to_ascii_uppercase();
        if ISRC_RE.is_match(&clean) {
            Some(format!("{base}s/{clean}.ttml"))
        } else {
            None
        }
    }

    /// Fetches TTML lyrics from `lrc.red`, trying direct ISRC lookup first and
    /// falling back to strict `/search.json` matching.
    pub async fn lyrics<F, Fut>(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        isrc: Option<&str>,
        mut fetch_doc: F,
    ) -> Option<Vec<LyricLine>>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        Self::lyrics_at(http, BASE, title, artist, duration_ms, isrc, &mut fetch_doc).await
    }

    pub(crate) async fn lyrics_at<F, Fut>(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
        isrc: Option<&str>,
        fetch_doc: &mut F,
    ) -> Option<Vec<LyricLine>>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        let known = isrc.and_then(|code| Self::document_url_with_base(base, code));
        if let Some(ref url) = known
            && let Some(lines) = Self::document(url.clone(), fetch_doc).await
        {
            return Some(lines);
        }

        let hit = Self::search(http, base, title, artist, duration_ms).await?;
        let found = hit
            .isrc
            .as_deref()
            .and_then(|code| Self::document_url_with_base(base, code))?;
        if known.as_deref() == Some(found.as_str()) {
            return None;
        }
        Self::document(found, fetch_doc).await
    }

    async fn document<F, Fut>(url: String, fetch_doc: &mut F) -> Option<Vec<LyricLine>>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Option<String>>,
    {
        let ttml = fetch_doc(url).await?;
        let lines = TtmlLyrics::parse(&ttml);
        if lines.is_empty() { None } else { Some(lines) }
    }

    async fn search(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<LrcRedHit> {
        let query = format!("{title} {artist}");
        let url = build_url(&format!("{base}search.json"), &[("q", query.trim())])?;
        let body = http.get(&url).await?;
        let resp: LrcRedResponse = serde_json::from_str(&body).ok()?;
        let hits = resp.hits?;
        Self::best(&hits, title, artist, duration_ms).cloned()
    }

    /// Selects the single hit that strictly matches this recording (core title,
    /// version markers, shared credited artist, and ≤ 3.0 s duration delta).
    pub fn best<'a>(
        hits: &'a [LrcRedHit],
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<&'a LrcRedHit> {
        let wanted_title = core_of(title);
        let wanted_version = version_of(title);
        let wanted_artists = artists_of(artist);
        let seconds = duration_ms as f64 / 1_000.0;

        let distance = |hit: &LrcRedHit| -> f64 {
            match (duration_ms > 0, hit.duration) {
                (true, Some(dur)) => (dur - seconds).abs(),
                _ => 0.0,
            }
        };

        hits.iter()
            .filter(|hit| {
                let Some(name) = hit.title.as_deref() else {
                    return false;
                };
                let valid_isrc = hit.isrc.as_deref().and_then(Self::document_url).is_some();
                if !valid_isrc {
                    return false;
                }
                if core_of(name) != wanted_title || version_of(name) != wanted_version {
                    return false;
                }
                if !wanted_artists.is_empty() {
                    let hit_artists = artists_of(hit.artist.as_deref().unwrap_or_default());
                    if !hit_artists.iter().any(|a| wanted_artists.contains(a)) {
                        return false;
                    }
                }
                distance(hit) <= DURATION_TOLERANCE_SECONDS
            })
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
    }
}

fn core_of(title: &str) -> String {
    let stripped = BRACKETED.replace_all(title, " ");
    let before_dash = stripped.split(" - ").next().unwrap_or(&stripped);
    let norm = normalized(before_dash);
    if norm.is_empty() {
        normalized(title)
    } else {
        norm
    }
}

fn version_of(title: &str) -> HashSet<String> {
    let mut extras = String::new();
    for m in BRACKETED.find_iter(title) {
        if !extras.is_empty() {
            extras.push(' ');
        }
        extras.push_str(m.as_str());
    }
    if let Some((_, after)) = title.split_once(" - ") {
        extras.push(' ');
        extras.push_str(after);
    }
    normalized(&extras)
        .split(' ')
        .filter(|w| VERSION_WORDS.contains(w))
        .map(ToOwned::to_owned)
        .collect()
}

fn artists_of(artist: &str) -> HashSet<String> {
    ARTIST_SEPARATORS
        .split(artist)
        .map(normalized)
        .filter(|s| !s.is_empty())
        .collect()
}

fn normalized(value: &str) -> String {
    let decomposed: String = value.nfd().collect();
    let without_marks = COMBINING_MARKS.replace_all(&decomposed, "");
    let lowered = without_marks.to_lowercase();
    let alnum_only: String = lowered
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect();
    WHITESPACE.replace_all(&alnum_only, " ").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOHEMIAN_SEARCH: &str = r##"{"hits":[
        {"isrc":"GBUM71100986","title":"Bohemian Rhapsody (Operatic Section / 2011 A Cappella Mix)",
         "artist":"Queen","album":"Queen 40: Limited Edition Collector's Box Set, Vol. 1","year":1975,
         "duration":64.969,"cover":"https://lrc.red/s/GBUM71100986.webp"},
        {"isrc":"GBUM71029604","title":"Bohemian Rhapsody (Remastered 2011)","artist":"Queen",
         "album":"A Night at the Opera","year":1975,"duration":356.52,
         "cover":"https://lrc.red/s/GBUM71029604.webp"},
        {"isrc":"GBCEG0100046","title":"Bohemian Rhapsody (Live)","artist":"Queen","album":"Live Killers",
         "year":1979,"duration":352.04,"cover":"https://lrc.red/s/GBCEG0100046.webp","color":"#d3714d"}
        ],"next":"p24"}"##;

    const TUM_HI_HO_SEARCH: &str = r#"{"hits":[
        {"isrc":"INS181303031","title":"Tum Hi Ho (From \"Aashiqui 2\")","artist":"Arijit Singh",
         "album":"Aashiqui 2","year":2013,"duration":261.974},
        {"isrc":"INS181303860","title":"Tum Hi Ho (Remix)","artist":"Arijit Singh","duration":237.338},
        {"isrc":"GB8KE2422340","title":"Tum Hi Ho","artist":"Imazee","duration":163.699}
        ],"next":null}"#;

    fn hits(json: &str) -> Vec<LrcRedHit> {
        serde_json::from_str::<LrcRedResponse>(json)
            .expect("valid json")
            .hits
            .expect("hits")
    }

    #[test]
    fn reads_a_search_page() {
        let resp: LrcRedResponse = serde_json::from_str(BOHEMIAN_SEARCH).expect("valid");
        let list = resp.hits.expect("hits");
        assert_eq!(list.len(), 3);
        assert_eq!(resp.next.as_deref(), Some("p24"));
        assert_eq!(list[1].duration, Some(356.52));
    }

    #[test]
    fn skips_a_cappella_mix_ranked_first() {
        let list = hits(BOHEMIAN_SEARCH);
        let best = LrcRed::best(&list, "Bohemian Rhapsody", "Queen", 355_000);
        assert_eq!(best.and_then(|h| h.isrc.as_deref()), Some("GBUM71029604"));
    }

    #[test]
    fn live_cut_close_in_length_is_not_studio_recording() {
        let list = hits(BOHEMIAN_SEARCH);
        let live = LrcRed::best(&list, "Bohemian Rhapsody (Live)", "Queen", 352_000);
        assert_eq!(live.and_then(|h| h.isrc.as_deref()), Some("GBCEG0100046"));
        assert!(LrcRed::best(&list, "Bohemian Rhapsody", "Queen", 352_000).is_none());
    }

    #[test]
    fn soundtrack_credit_in_brackets_and_shared_artist_match() {
        let list = hits(TUM_HI_HO_SEARCH);
        let best = LrcRed::best(&list, "Tum Hi Ho", "Mithoon, Arijit Singh", 262_000);
        assert_eq!(best.and_then(|h| h.isrc.as_deref()), Some("INS181303031"));
    }

    #[test]
    fn same_title_by_somebody_else_or_wrong_duration_is_rejected() {
        let list = hits(TUM_HI_HO_SEARCH);
        assert!(LrcRed::best(&list, "Tum Hi Ho", "Imazee Fan", 163_000).is_none());
        assert!(LrcRed::best(&list, "Tum Hi Ho", "Arijit Singh", 200_000).is_none());
    }

    #[test]
    fn only_well_formed_isrc_becomes_a_path() {
        assert_eq!(
            LrcRed::document_url(" gbum71029604 ").as_deref(),
            Some("https://lrc.red/s/GBUM71029604.ttml")
        );
        assert!(LrcRed::document_url("../search.json?q=x").is_none());
        assert!(LrcRed::document_url("").is_none());
    }
}
