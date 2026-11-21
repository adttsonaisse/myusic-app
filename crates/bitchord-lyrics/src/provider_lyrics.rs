use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::enhanced_lrc::{EnhancedLrc, parse_lrc};
use crate::gaps::with_instrumental_gaps;
use crate::line::LyricLine;
use crate::syllable_runs::{TimedRun, words_from_runs};
use crate::ttml::TtmlLyrics;

const CONTENT_KEYS: &[&str] = &[
    "ttml",
    "ttmlContent",
    "lyrics",
    "lrc",
    "content",
    "text",
    "plainLyrics",
    "syncedLyrics",
    "line",
    "lines",
    "lyric",
    "data",
    "result",
    "response",
];

static TTML_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<tt(?:\s|>)").expect("valid static regex"));
static PLAIN_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:lyrics? (?:not found|unavailable)|error)\b").expect("valid static regex")
});
static METADATA_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[[A-Za-z]+:.*\]$").expect("valid static regex"));

static KARAOKE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\d{1,8}),(\d{1,8})\](.*)$").expect("valid static regex"));
static PREFIX_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\((\d{1,8}),(\d{1,8})(?:,\d{1,8})?\)([^()]*)").expect("valid static regex")
});
static SUFFIX_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([^()]*)\((\d{1,8}),(\d{1,8})(?:,\d{1,8})?\)").expect("valid static regex")
});
static WORD_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\d{1,8},\d{1,8}(?:,\d{1,8})?\)").expect("valid static regex"));
static LYRIC_CONTENT_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)LyricContent\s*=\s*"([^"]*)""#).expect("valid static regex")
});

/// Turns the heterogeneous wire formats used by smaller lyrics providers
/// (JSON envelopes, markdown fences, HTML-escaped TTML, QRC/YRC karaoke,
/// Enhanced A2 LRC, standard LRC, or plain text) into BitChord [`LyricLine`]s.
pub struct ProviderLyrics;

impl ProviderLyrics {
    pub fn parse(raw: &str) -> Option<Vec<LyricLine>> {
        let unwrapped = Self::unwrap(raw)?;
        let content = unescape_ttml(&unwrapped);
        let lines = if TTML_TAG.is_match(&content)
            || contains_ignore_ascii_case(&content, "http://www.w3.org/ns/ttml")
        {
            TtmlLyrics::parse(&content)
        } else if KaraokeLrc::looks_like(&content) {
            KaraokeLrc::parse(&content)
        } else if content.trim_start().starts_with('<') {
            Vec::new()
        } else {
            let enhanced = EnhancedLrc::parse(&content);
            if !enhanced.is_empty() {
                enhanced
            } else {
                let lrc = parse_lrc(&content);
                if !lrc.is_empty() {
                    lrc
                } else {
                    plain(&content)
                }
            }
        };
        lines
            .iter()
            .any(|line| !line.text.trim().is_empty())
            .then_some(lines)
    }

    /// Unwraps BOMs, markdown code fences, and one or more JSON envelopes to
    /// recover the raw lyric payload string.
    pub fn unwrap(raw: &str) -> Option<String> {
        let stripped = raw.replace('\u{FEFF}', "");
        let mut value = stripped.trim().to_owned();
        if value.starts_with("```") {
            let mut lines: Vec<&str> = value.lines().skip(1).collect();
            if lines.last().is_some_and(|line| line.trim() == "```") {
                lines.pop();
            }
            value = lines.join("\n").trim().to_owned();
        }
        if value.is_empty() {
            return None;
        }
        let Ok(json) = serde_json::from_str::<Value>(&value) else {
            return Some(value);
        };
        let extracted = extract(&json)?;
        let clean = extracted.trim();
        (!clean.is_empty()).then(|| clean.to_owned())
    }
}

fn plain(content: &str) -> Vec<LyricLine> {
    if PLAIN_ERROR.is_match(content) {
        return Vec::new();
    }
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !METADATA_TAG.is_match(line))
        .map(|line| LyricLine::new(0, line))
        .collect()
}

fn extract(element: &Value) -> Option<String> {
    match element {
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
        Value::String(raw) => {
            let text = raw.trim();
            if let Ok(nested) = serde_json::from_str::<Value>(text)
                && matches!(nested, Value::Array(_) | Value::Object(_))
            {
                return extract(&nested);
            }
            Some(text.to_owned())
        }
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(extract)
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.trim().is_empty()).then_some(joined)
        }
        Value::Object(map) => {
            let is_error = map.get("isError").is_some_and(|v| v == &Value::Bool(true));
            let ok_false = map.get("ok").is_some_and(|v| v == &Value::Bool(false));
            let has_error = map.get("error").is_some_and(|v| {
                !v.is_null() && v != &Value::Bool(false) && v.as_str() != Some("")
            });
            if is_error || ok_false || has_error {
                return None;
            }
            for key in CONTENT_KEYS {
                if let Some(found) = map.get(*key).and_then(extract) {
                    return Some(found);
                }
            }
            if let Some(meta) = map.get("metadata").filter(|v| v.is_object())
                && let Some(found) = extract(meta)
            {
                return Some(found);
            }
            map.get("words").and_then(extract)
        }
    }
}

fn unescape_ttml(value: &str) -> String {
    if contains_ignore_ascii_case(value, "&lt;tt") {
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    } else {
        value.to_owned()
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle_ascii_lower: &str) -> bool {
    let needle = needle_ascii_lower.as_bytes();
    if needle.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// QQ/QRC and NetEase YRC millisecond line and word range parser.
pub struct KaraokeLrc;

impl KaraokeLrc {
    pub fn looks_like(raw: &str) -> bool {
        let content = lyric_content(raw);
        content.lines().any(|line| {
            let Some(caps) = KARAOKE_LINE.captures(line.trim()) else {
                return false;
            };
            let body = caps.get(3).map_or("", |m| m.as_str());
            PREFIX_WORD.is_match(body) || SUFFIX_WORD.is_match(body)
        })
    }

    pub fn parse(raw: &str) -> Vec<LyricLine> {
        let content = lyric_content(raw);
        let mut rows: Vec<LyricLine> = content
            .lines()
            .filter_map(|source| {
                let caps = KARAOKE_LINE.captures(source.trim())?;
                let line_start = caps.get(1)?.as_str().parse::<i64>().ok()?;
                let line_duration = caps.get(2)?.as_str().parse::<i64>().ok()?;
                let body = caps.get(3).map_or("", |m| m.as_str());

                let prefixed_runs: Vec<TimedRun> = PREFIX_WORD
                    .captures_iter(body)
                    .filter_map(|word| {
                        timed_run(
                            word.get(3)?.as_str(),
                            word.get(1)?.as_str(),
                            word.get(2)?.as_str(),
                        )
                    })
                    .collect();
                let suffixed_runs: Vec<TimedRun> = SUFFIX_WORD
                    .captures_iter(body)
                    .filter_map(|word| {
                        timed_run(
                            word.get(1)?.as_str(),
                            word.get(2)?.as_str(),
                            word.get(3)?.as_str(),
                        )
                    })
                    .collect();

                let prefixed = words_from_runs(&prefixed_runs, false);
                let suffixed = words_from_runs(&suffixed_runs, false);

                let prefixed_chars: usize = prefixed.iter().map(|w| w.text.chars().count()).sum();
                let suffixed_chars: usize = suffixed.iter().map(|w| w.text.chars().count()).sum();
                let words = if prefixed_chars >= suffixed_chars {
                    prefixed
                } else {
                    suffixed
                };
                if words.is_empty() {
                    return None;
                }

                let stripped = WORD_TIME.replace_all(body, "");
                let text = EnhancedLrc::decode_entities(&stripped).trim().to_owned();
                if text.is_empty() {
                    return None;
                }

                let first_start = words[0].start_ms;
                Some(LyricLine {
                    time_ms: line_start.min(first_start),
                    text,
                    words,
                    sung_until_ms: (line_duration > 0).then_some(line_start + line_duration),
                    ..LyricLine::default()
                })
            })
            .collect();

        rows.sort_by_key(|line| line.time_ms);
        with_instrumental_gaps(rows)
    }
}

fn timed_run(text: &str, start: &str, duration: &str) -> Option<TimedRun> {
    let start_ms = start.parse::<i64>().ok()?;
    let duration_ms = duration.parse::<i64>().ok()?;
    Some(TimedRun::new(
        start_ms,
        start_ms + duration_ms,
        EnhancedLrc::decode_entities(text),
    ))
}

fn lyric_content(raw: &str) -> String {
    match LYRIC_CONTENT_ATTR
        .captures(raw)
        .and_then(|caps| caps.get(1))
    {
        Some(found) => found
            .as_str()
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
        None => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::LyricSyllable;

    fn single_sung(lines: Vec<LyricLine>) -> LyricLine {
        let mut sung: Vec<LyricLine> = lines.into_iter().filter(|l| !l.is_gap()).collect();
        assert_eq!(1, sung.len());
        sung.remove(0)
    }

    #[test]
    fn json_wrapped_ttml_keeps_apple_word_timing() {
        let raw = r#"{"content":"<tt><body><div><p begin=\"1.0\" end=\"2.0\"><span begin=\"1.0\" end=\"1.4\">sing </span><span begin=\"1.4\" end=\"2.0\">along</span></p></div></body></tt>"}"#;
        let line = single_sung(ProviderLyrics::parse(raw).expect("parses wrapped TTML"));
        assert_eq!("sing along", line.text);
        assert_eq!(2, line.words.len());
        assert_eq!(2_000, line.words.last().expect("has words").end_ms);
    }

    #[test]
    fn netease_prefix_yrc_gets_word_animation() {
        let line = single_sung(KaraokeLrc::parse(
            "[1000,900](1000,400,0)sing(1400,500,0)along",
        ));
        assert_eq!("singalong", line.text);
        assert_eq!(
            vec![1_000, 1_400],
            line.words.iter().map(|w| w.start_ms).collect::<Vec<_>>()
        );
        assert_eq!(1_900, line.words.last().expect("has words").end_ms);
    }

    #[test]
    fn qq_suffix_qrc_gets_word_animation() {
        let line = single_sung(KaraokeLrc::parse(
            "[1000,900]sing (1000,400,0)along(1400,500,0)",
        ));
        assert_eq!("sing along", line.text);
        assert_eq!(2, line.words.len());
        assert!(line.is_word_synced());
    }

    #[test]
    fn qq_xml_envelope_is_recognized_as_karaoke_rather_than_ttml() {
        let qrc = r#"<QrcInfos><LyricInfo LyricContent="[1000,900]sing (1000,400,0)along(1400,500,0)"/></QrcInfos>"#;
        let line = single_sung(ProviderLyrics::parse(qrc).expect("parses QRC XML"));
        assert_eq!("sing along", line.text);
        assert_eq!(2, line.words.len());
    }

    #[test]
    fn yrc_glues_syllables_written_without_space_into_one_word() {
        let line = single_sung(KaraokeLrc::parse(
            "[31000,1600](31000,839,0)long (31839,157,0)e(31996,533,0)nough",
        ));
        assert_eq!("long enough", line.text);
        assert_eq!(
            vec!["long", "enough"],
            line.words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            vec![
                LyricSyllable::new(31_839, 31_996, 0, 1),
                LyricSyllable::new(31_996, 32_529, 1, 6),
            ],
            line.words[1].syllables
        );
    }

    #[test]
    fn qrc_in_unspaced_script_keeps_word_per_stamp() {
        let line = single_sung(KaraokeLrc::parse(
            "[1000,900]我(1000,300,0)爱(1300,300,0)你(1600,300,0)",
        ));
        assert_eq!(3, line.words.len());
        assert!(line.words.iter().all(|w| w.syllables.is_empty()));
    }

    #[test]
    fn error_envelopes_html_pages_and_not_found_messages_return_none() {
        assert!(ProviderLyrics::parse(r#"{"isError":true,"content":"hello"}"#).is_none());
        assert!(ProviderLyrics::parse(r#"{"ok":false,"content":"hello"}"#).is_none());
        assert!(ProviderLyrics::parse(r#"{"error":"nope","content":"hello"}"#).is_none());
        assert!(ProviderLyrics::parse("<html><body>404</body></html>").is_none());
        assert!(ProviderLyrics::parse("Lyrics not found").is_none());
        assert!(ProviderLyrics::parse(r#"{"error":false,"content":"hello world"}"#).is_some());
    }

    #[test]
    fn markdown_fences_and_escaped_ttml_are_unwrapped() {
        let fenced = "```lrc\n[00:01.00] hello world\n```";
        let line = single_sung(ProviderLyrics::parse(fenced).expect("parses fenced LRC"));
        assert_eq!("hello world", line.text);
        assert_eq!(1_000, line.time_ms);

        let escaped = "&lt;tt&gt;&lt;body&gt;&lt;p begin=&quot;1.0&quot;&gt;hi&lt;/p&gt;&lt;/body&gt;&lt;/tt&gt;";
        let ttml_line = single_sung(ProviderLyrics::parse(escaped).expect("parses escaped TTML"));
        assert_eq!("hi", ttml_line.text);
    }
}
