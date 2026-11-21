use std::sync::LazyLock;

use regex::Regex;

use crate::http::{LyricsHttp, build_url};
use crate::{EnhancedLrc, LyricLine, parse_lrc};

const BASE: &str = "https://www.megalobiz.com";

static LRC_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)href=["'](/lrc/maker/download/[^"']+)["']"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid LRC_LINK regex")
});

static LRC_BODY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)id=["']lrc_[^"']*_details["'][^>]*>(.*?)</span>"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid LRC_BODY regex")
});

static BR_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<br\s*/?>")
        // INFALLIBLE: static regex pattern.
        .expect("valid BR_TAG regex")
});

static ANY_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<[^>]+>")
        // INFALLIBLE: static regex pattern.
        .expect("valid ANY_TAG regex")
});

/// Line-synced community LRC scraped from Megalobiz.
pub struct Megalobiz;

impl Megalobiz {
    pub async fn lyrics(http: &LyricsHttp, title: &str, artist: &str) -> Option<Vec<LyricLine>> {
        Self::lyrics_at(http, BASE, title, artist).await
    }

    pub(crate) async fn lyrics_at(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
    ) -> Option<Vec<LyricLine>> {
        let query = format!("{artist} {title}");
        let search_url = build_url(&format!("{base}/searchall"), &[("qry", query.trim())])?;
        let results = http.get(&search_url).await?;
        let path = LRC_LINK.captures(&results)?.get(1)?.as_str();
        let page_url = format!("{base}{}", path.replace("&amp;", "&"));
        let page = http.get(&page_url).await?;
        let raw = LRC_BODY.captures(&page)?.get(1)?.as_str();
        let with_newlines = BR_TAG.replace_all(raw, "\n");
        let stripped = ANY_TAG.replace_all(&with_newlines, "");
        let decoded = EnhancedLrc::decode_entities(&stripped);
        let lines = parse_lrc(&decoded);
        if lines.iter().any(|l| !l.text.trim().is_empty()) {
            Some(lines)
        } else {
            None
        }
    }
}
