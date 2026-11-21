use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::{Map, Value};

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, is_genius_section_header};

/// Deliberately plain User-Agent — Cloudflare challenges spoofed Chrome UAs
/// lacking matching TLS/sec-ch-ua fingerprints on genius.com.
const USER_AGENT: &str = "BitChord";
const ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,application/json,*/*;q=0.8";
const SEARCH_URL: &str = "https://genius.com/api/search/multi";

static TITLE_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*[-–—:]\s*")
        // INFALLIBLE: static regex pattern.
        .expect("valid TITLE_SEPARATOR regex")
});

static DECORATIVE_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[♪♫★☆【】《》「」~_]")
        // INFALLIBLE: static regex pattern.
        .expect("valid DECORATIVE_CHARS regex")
});

static PRODUCER_TAGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:prod(?:uced)?\.?(?:\s+by)?)\s+.*$")
        // INFALLIBLE: static regex pattern.
        .expect("valid PRODUCER_TAGS regex")
});

static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\s*[(\[]\s*(?:from|feat\.?|ft\.?|featuring|with|prod\.?|produced by|official|lyrical|video|audio|remix|music video|visualizer|mv|hd|4k|hq|full song)[^)\]]*[)\]]|",
        r"\s*\b(?:official\s+(?:music\s+)?(?:video|audio)|lyrical(?:\s+video)?|full\s+song|4k\s+video|hd\s+video|music\s+video)\b",
    ))
    // INFALLIBLE: static regex pattern.
    .expect("valid Genius NOISE regex")
});

static BRACKETED_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*[(\[].*?[)\]]")
        // INFALLIBLE: static regex pattern.
        .expect("valid BRACKETED_CONTENT regex")
});

static NON_ALPHANUMERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[^\p{L}\p{N}\s]")
        // INFALLIBLE: static regex pattern.
        .expect("valid NON_ALPHANUMERIC regex")
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+")
        // INFALLIBLE: static regex pattern.
        .expect("valid WHITESPACE regex")
});

static YOU_MIGHT_ALSO_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\d*You might also like")
        // INFALLIBLE: static regex pattern.
        .expect("valid YOU_MIGHT_ALSO_LIKE regex")
});

static TRAILING_EMBED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\d*Embed\s*$")
        // INFALLIBLE: static regex pattern.
        .expect("valid TRAILING_EMBED regex")
});

static MODERN_CONTAINER: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(r#"div[data-lyrics-container="true"]"#)
        // INFALLIBLE: static CSS selector.
        .expect("valid modern Genius selector")
});

static LEGACY_CONTAINER: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("div.lyrics")
        // INFALLIBLE: static CSS selector.
        .expect("valid legacy Genius selector")
});

struct SearchAttempt {
    query: String,
    title: String,
    artist: String,
}

/// Plain-text web scraper fallback for Genius.com lyrics.
pub struct Genius;

impl Genius {
    pub async fn lyrics(http: &LyricsHttp, title: &str, artist: &str) -> Option<Vec<LyricLine>> {
        Self::lyrics_at(http, SEARCH_URL, title, artist).await
    }

    pub(crate) async fn lyrics_at(
        http: &LyricsHttp,
        search_endpoint: &str,
        title: &str,
        artist: &str,
    ) -> Option<Vec<LyricLine>> {
        let attempts = build_search_attempts(title, artist);
        let mut song_url = None;
        for attempt in attempts {
            if let Some(url) = search_song_url(
                http,
                search_endpoint,
                &attempt.query,
                &attempt.title,
                &attempt.artist,
            )
            .await
            {
                song_url = Some(url);
                break;
            }
        }
        let song_url = song_url?;
        let html = http.get_custom(&song_url, USER_AGENT, ACCEPT, None).await?;
        if html.trim().is_empty() {
            return None;
        }
        Self::parse_html(&html).filter(|lines| !lines.is_empty())
    }

    /// Extracts clean multi-line lyrics and section headers from a Genius HTML
    /// page using `scraper`.
    pub fn parse_html(html: &str) -> Option<Vec<LyricLine>> {
        let document = Html::parse_document(html);
        let mut containers: Vec<ElementRef<'_>> = document.select(&MODERN_CONTAINER).collect();
        if containers.is_empty() {
            containers = document.select(&LEGACY_CONTAINER).collect();
        }
        if containers.is_empty() {
            return None;
        }

        let mut full_text = String::new();
        for container in containers {
            let mut text = String::new();
            extract_element_text(container, &mut text);
            if !text.trim().is_empty() {
                full_text.push_str(&text);
                full_text.push('\n');
            }
        }
        if full_text.trim().is_empty() {
            return None;
        }
        let cleaned = Self::strip_artifacts(&full_text);
        let lines = Self::text_to_lyric_lines(&cleaned);
        if lines.is_empty() { None } else { Some(lines) }
    }

    /// Removes Genius scraper artifacts (`You might also like`, trailing
    /// `<N>Embed`, NBSP / zero-width spaces / BOM).
    pub fn strip_artifacts(raw: &str) -> String {
        let normalized = raw.replace(['\u{00A0}', '\u{200B}', '\u{FEFF}'], " ");
        let without_recs = YOU_MIGHT_ALSO_LIKE.replace_all(&normalized, "");
        let trimmed = without_recs.trim();
        let without_embed = TRAILING_EMBED.replace_all(trimmed, "");
        without_embed.trim().to_owned()
    }

    /// Converts clean multi-line lyrics text into structured [`LyricLine`]s
    /// with stanza breaks represented as single gap lines.
    pub fn text_to_lyric_lines(text: &str) -> Vec<LyricLine> {
        let mut result: Vec<LyricLine> = Vec::new();
        let mut last_was_gap = false;

        for raw_line in text.lines() {
            let trimmed = raw_line.trim();
            let line = TRAILING_EMBED.replace_all(trimmed, "");
            let line = line.trim();
            if line.is_empty() {
                if !last_was_gap && !result.is_empty() {
                    result.push(LyricLine::new(0, ""));
                    last_was_gap = true;
                }
            } else {
                result.push(LyricLine::new(0, line));
                last_was_gap = false;
            }
        }

        while result.first().is_some_and(LyricLine::is_gap) {
            result.remove(0);
        }
        while result.last().is_some_and(LyricLine::is_gap) {
            result.pop();
        }
        result
    }

    /// Checks whether a line is a Genius section header like `[Verse 1]`.
    pub fn is_section_header(text: &str) -> bool {
        is_genius_section_header(text)
    }
}

fn is_excluded_element(elem: ElementRef<'_>) -> bool {
    let val = elem.value();
    let tag = val.name();
    if matches!(tag, "button" | "script" | "style") {
        return true;
    }
    if val.attr("data-exclude-from-selection") == Some("true") {
        return true;
    }
    val.classes().any(|cls| {
        matches!(
            cls,
            "LyricsHeader__Container" | "SongBioPreview__Container" | "InreadAd__Container"
        )
    })
}

fn extract_element_text(elem: ElementRef<'_>, out: &mut String) {
    for child in elem.children() {
        match child.value() {
            Node::Text(t) => out.push_str(&t.text),
            Node::Element(_) => {
                let Some(child_elem) = ElementRef::wrap(child) else {
                    continue;
                };
                if is_excluded_element(child_elem) {
                    continue;
                }
                match child_elem.value().name() {
                    "br" => out.push('\n'),
                    "p" => {
                        out.push('\n');
                        extract_element_text(child_elem, out);
                    }
                    _ => extract_element_text(child_elem, out),
                }
            }
            _ => {}
        }
    }
}

fn build_search_attempts(title: &str, artist: &str) -> Vec<SearchAttempt> {
    let clean_title = clean_query(title);
    let clean_artist = clean_query(artist);

    let title_parts: Option<(&str, &str)> = TITLE_SEPARATOR
        .find(&clean_title)
        .map(|m| (&clean_title[..m.start()], &clean_title[m.end()..]));

    let extracted_title = match title_parts {
        Some((left, right)) if left.trim().eq_ignore_ascii_case(&clean_artist) => {
            right.trim().to_owned()
        }
        Some((left, right)) if right.trim().eq_ignore_ascii_case(&clean_artist) => {
            left.trim().to_owned()
        }
        Some((left, right)) if !left.trim().is_empty() && !right.trim().is_empty() => {
            right.trim().to_owned()
        }
        _ => clean_title.clone(),
    };

    let extracted_artist = match title_parts {
        Some((left, _)) if left.trim().eq_ignore_ascii_case(&clean_artist) => clean_artist.clone(),
        Some((_, right)) if right.trim().eq_ignore_ascii_case(&clean_artist) => {
            clean_artist.clone()
        }
        Some((left, _)) if clean_artist.trim().is_empty() => left.trim().to_owned(),
        _ => clean_artist.clone(),
    };

    let no_brackets = BRACKETED_CONTENT.replace_all(&extracted_title, " ");
    let no_punct = NON_ALPHANUMERIC.replace_all(&no_brackets, " ");
    let title_without_brackets = WHITESPACE.replace_all(&no_punct, " ").trim().to_owned();

    let mut attempts = Vec::new();
    if !extracted_artist.trim().is_empty() && !extracted_title.trim().is_empty() {
        attempts.push(SearchAttempt {
            query: format!("{extracted_artist} {extracted_title}")
                .trim()
                .to_owned(),
            title: extracted_title.clone(),
            artist: extracted_artist.clone(),
        });
    }
    if !extracted_artist.trim().is_empty()
        && !title_without_brackets.trim().is_empty()
        && title_without_brackets != extracted_title
    {
        attempts.push(SearchAttempt {
            query: format!("{extracted_artist} {title_without_brackets}")
                .trim()
                .to_owned(),
            title: title_without_brackets.clone(),
            artist: extracted_artist.clone(),
        });
    }
    if clean_title != extracted_title {
        attempts.push(SearchAttempt {
            query: format!("{clean_artist} {clean_title}").trim().to_owned(),
            title: clean_title.clone(),
            artist: clean_artist.clone(),
        });
        attempts.push(SearchAttempt {
            query: clean_title,
            title: extracted_title.clone(),
            artist: extracted_artist.clone(),
        });
    }
    if !title_without_brackets.trim().is_empty() {
        attempts.push(SearchAttempt {
            query: title_without_brackets.clone(),
            title: title_without_brackets,
            artist: extracted_artist,
        });
    } else if !extracted_title.trim().is_empty() {
        attempts.push(SearchAttempt {
            query: extracted_title.clone(),
            title: extracted_title,
            artist: extracted_artist,
        });
    }

    let mut seen = HashSet::new();
    attempts.retain(|a| seen.insert(a.query.clone()));
    attempts
}

fn clean_query(text: &str) -> String {
    let s1 = DECORATIVE_CHARS.replace_all(text, " ");
    let s2 = NOISE.replace_all(&s1, " ");
    let s3 = PRODUCER_TAGS.replace_all(&s2, " ");
    let before_pipe = s3.split(" | ").next().unwrap_or(&s3);
    let collapsed = WHITESPACE.replace_all(before_pipe, " ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        text.trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

async fn search_song_url(
    http: &LyricsHttp,
    endpoint: &str,
    query: &str,
    target_title: &str,
    target_artist: &str,
) -> Option<String> {
    let url = build_url(endpoint, &[("q", query)])?;
    let body = http.get_custom(&url, USER_AGENT, ACCEPT, None).await?;
    let root: Value = serde_json::from_str(&body).ok()?;
    let sections = root
        .as_object()?
        .get("response")?
        .as_object()?
        .get("sections")?
        .as_array()?;
    let song_section = sections.iter().find_map(|s| {
        let obj = s.as_object()?;
        (obj.get("type").and_then(Value::as_str) == Some("song")).then_some(obj)
    })?;
    let hits = song_section.get("hits")?.as_array()?;
    let candidates: Vec<&Map<String, Value>> = hits
        .iter()
        .filter_map(|h| h.as_object()?.get("result")?.as_object())
        .collect();
    let best = best_match(&candidates, target_title, target_artist)?;
    best.get("url")?.as_str().map(ToOwned::to_owned)
}

fn best_match<'a>(
    candidates: &[&'a Map<String, Value>],
    target_title: &str,
    target_artist: &str,
) -> Option<&'a Map<String, Value>> {
    if candidates.is_empty() {
        return None;
    }
    let norm_title = target_title.to_lowercase();
    let norm_artist = target_artist.to_lowercase();

    let mut best: Option<(&'a Map<String, Value>, i32)> = None;
    for &item in candidates {
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let artist = item
            .get("artist_names")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let mut score = 0;

        let title_matches = !norm_title.trim().is_empty()
            && (title == norm_title || title.contains(&norm_title) || norm_title.contains(&title));
        let artist_matches = !norm_artist.trim().is_empty()
            && (artist == norm_artist
                || artist.contains(&norm_artist)
                || norm_artist.contains(&artist));

        if !title_matches && !artist_matches {
            continue;
        }
        if title == norm_title {
            score += 50;
        } else if title_matches {
            score += 25;
        }
        if artist_matches {
            if artist == norm_artist {
                score += 40;
            } else {
                score += 20;
            }
        }

        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        if path.contains("translation") && !norm_title.contains("translation") {
            score -= 30;
        }
        if path.contains("türkçe") || path.contains("polskie-tlumaczenie") {
            score -= 40;
        }
        if path.contains("tracklist") || path.contains("album-art") {
            score -= 50;
        }
        if score <= 0 {
            continue;
        }
        if best.as_ref().is_none_or(|(_, bs)| score > *bs) {
            best = Some((item, score));
        }
    }
    best.map(|(item, _)| item)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_GENIUS_HTML: &str = r#"<!DOCTYPE html>
<html>
<head><title>Queen - Bohemian Rhapsody Lyrics | Genius Lyrics</title></head>
<body>
  <div data-lyrics-container="true" class="Lyrics__Container">
    <div data-exclude-from-selection="true" class="LyricsHeader__Container">
      <button>523 Contributors</button>
      <div class="SongBioPreview__Container">Song Bio</div>
    </div>
    [Intro]<br>
    Is this the real life?<br>
    Is this just fantasy?<br>
    <br>
    [Verse 1]<br>
    Mama, just killed a man<br>
    Put a gun against his head, pulled my trigger, now he's dead<br>
    15You might also like<br>
  </div>
  <div data-lyrics-container="true" class="Lyrics__Container">
    [Chorus]<br>
    Mama, life had just begun<br>
    But now I've gone and thrown it all away<br>
    42Embed
  </div>
</body>
</html>"#;

    #[test]
    fn detects_section_headers_correctly() {
        assert!(Genius::is_section_header("[Verse 1]"));
        assert!(Genius::is_section_header("[Chorus]"));
        assert!(Genius::is_section_header("[Guitar Solo]"));
        assert!(Genius::is_section_header("[Bridge: Freddie Mercury]"));
        assert!(!Genius::is_section_header("Is this the real life?"));
        assert!(!Genius::is_section_header("[short"));
        assert!(!Genius::is_section_header("]short["));
    }

    #[test]
    fn parses_genius_html_and_extracts_clean_lyrics_and_sections() {
        let lines = Genius::parse_html(SAMPLE_GENIUS_HTML).expect("parsed");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();

        assert!(!texts.iter().any(|t| t.contains("Contributors")));
        assert!(!texts.iter().any(|t| t.contains("Song Bio")));
        assert!(!texts.iter().any(|t| t.contains("You might also like")));
        assert!(!texts.iter().any(|t| t.contains("Embed")));

        assert!(texts.contains(&"[Intro]"));
        assert!(texts.contains(&"Is this the real life?"));
        assert!(texts.contains(&"Is this just fantasy?"));
        assert!(texts.contains(&"[Verse 1]"));
        assert!(texts.contains(&"Mama, just killed a man"));
        assert!(texts.contains(&"[Chorus]"));
        assert!(texts.contains(&"Mama, life had just begun"));
        assert!(texts.contains(&"But now I've gone and thrown it all away"));
    }

    #[test]
    fn strips_artifacts_like_embed_counters_and_unicode_spaces() {
        let raw = "Some lyric line\u{00A0}with non-breaking spaces\n12You might also like\nAnother line\n345Embed";
        let cleaned = Genius::strip_artifacts(raw);

        assert!(!cleaned.contains("You might also like"));
        assert!(!cleaned.contains("Embed"));
        assert!(!cleaned.contains('\u{00A0}'));
        assert!(cleaned.contains("Some lyric line with non-breaking spaces"));
        assert!(cleaned.contains("Another line"));
    }

    #[test]
    fn converts_multi_line_text_into_lyric_lines_with_stanza_separation() {
        let input = "[Verse 1]\nLine 1\nLine 2\n\n[Chorus]\nLine 3";
        let lines = Genius::text_to_lyric_lines(input);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].text, "[Verse 1]");
        assert_eq!(lines[1].text, "Line 1");
        assert_eq!(lines[2].text, "Line 2");
        assert!(lines[3].is_gap());
        assert_eq!(lines[4].text, "[Chorus]");
        assert_eq!(lines[5].text, "Line 3");
    }
}
