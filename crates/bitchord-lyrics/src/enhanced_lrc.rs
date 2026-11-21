use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::gaps::{MIN_GAP_MS, with_instrumental_gaps};
use crate::line::{LyricAlignment, LyricLine, LyricWord};
use crate::syllable_runs::{TimedRun, words_from_runs};

const TAIL_MS: i64 = 800;

/// Marker written by [`crate::to_enhanced_lrc`] to round-trip RTL (`End`) alignment.
const ALIGNMENT_MARKER: &str = "<R>";

static ENHANCED_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[(\d{1,3}):(\d{2})[.:](\d{2,3})\](.*)$").expect("valid static regex")
});
static ENHANCED_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<(\d{1,3}):(\d{2})[.:](\d{2,3})>([^<]*)").expect("valid static regex")
});
static LRC_STAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(\d{1,3}):(\d{2})[.:](\d{2,3})\]").expect("valid static regex")
});
static WORD_STAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(\d{1,3}):(\d{2})[.:](\d{2,3})>").expect("valid static regex"));
static HEX_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&#x([0-9a-fA-F]+);").expect("valid static regex"));
static DEC_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&#(\d+);").expect("valid static regex"));

/// Enhanced ("A2") LRC parser — standard LRC lines with `<mm:ss.xx>` word stamps.
pub struct EnhancedLrc;

struct EnhancedRow {
    time_ms: i64,
    words: Vec<(i64, String)>,
    plain: String,
}

impl EnhancedLrc {
    /// Parses Enhanced ("A2") LRC into word-synced [`LyricLine`]s.
    ///
    /// Returns an empty `Vec` when `lrc` carries no word stamps so the caller
    /// can fall back to [`parse_lrc`].
    pub fn parse(lrc: &str) -> Vec<LyricLine> {
        let mut rows: Vec<EnhancedRow> = lrc
            .lines()
            .filter_map(|line| {
                let caps = ENHANCED_LINE.captures(line.trim())?;
                let time_ms = parse_stamp_caps(&caps);
                let body = caps.get(4).map_or("", |m| m.as_str());
                let words = ENHANCED_WORD
                    .captures_iter(body)
                    .map(|word_caps| {
                        let start = parse_stamp_caps(&word_caps);
                        let raw = word_caps
                            .get(4)
                            .map_or_else(String::new, |m| m.as_str().to_owned());
                        (start, raw)
                    })
                    .collect();
                Some(EnhancedRow {
                    time_ms,
                    words,
                    plain: body.trim().to_owned(),
                })
            })
            .collect();

        rows.sort_by_key(|row| row.time_ms);

        // Nothing word-stamped in here: this is an ordinary LRC file and the
        // caller is better served parsing it as one.
        if !rows.iter().any(|row| !row.words.is_empty()) {
            return Vec::new();
        }

        let mut lines = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            if row.words.is_empty() {
                let text = Self::decode_entities(&row.plain);
                if !text.is_empty() {
                    lines.push(LyricLine::new(row.time_ms, text));
                }
                continue;
            }
            // A word runs until the next one starts; the last runs until the
            // next line does. Without a next line, give it a beat rather than
            // zero so its sweep can run.
            let last_word_start = row.words.last().map_or(row.time_ms, |(t, _)| *t);
            let line_end = rows
                .get(index + 1)
                .map_or(last_word_start + TAIL_MS, |next| next.time_ms);

            // Kept as written, spacing and all: a run with no space after it
            // is a syllable of a word that carries on into the next one.
            let runs: Vec<TimedRun> = row
                .words
                .iter()
                .enumerate()
                .map(|(i, (word_start, raw))| {
                    let word_end = row.words.get(i + 1).map_or(line_end, |(t, _)| *t);
                    TimedRun::new(*word_start, word_end, Self::decode_entities(raw))
                })
                .collect();

            let words = words_from_runs(&runs, false);
            if words.is_empty() {
                continue;
            }
            let first_start = words[0].start_ms;
            let text = words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(LyricLine {
                time_ms: row.time_ms.min(first_start),
                text,
                words,
                ..LyricLine::default()
            });
        }
        with_instrumental_gaps(lines)
    }

    /// Decodes HTML entities commonly returned by rich-sync providers
    /// (`&#x27;`, `&#39;`, `&apos;`, `&quot;`, `&nbsp;`, `&lt;`, `&gt;`, `&amp;`).
    ///
    /// `&amp;` is decoded last so `&amp;#x27;` becomes `&#x27;` rather than
    /// double-decoding into an apostrophe.
    pub fn decode_entities(text: &str) -> String {
        if !text.contains('&') {
            return text.to_owned();
        }
        let hex_decoded = HEX_ENTITY.replace_all(text, |caps: &Captures<'_>| {
            caps.get(1)
                .and_then(|m| u32::from_str_radix(m.as_str(), 16).ok())
                .and_then(char::from_u32)
                .map_or_else(
                    || {
                        caps.get(0)
                            .map_or_else(String::new, |m| m.as_str().to_owned())
                    },
                    |ch| ch.to_string(),
                )
        });
        let dec_decoded = DEC_ENTITY.replace_all(&hex_decoded, |caps: &Captures<'_>| {
            caps.get(1)
                .and_then(|m| m.as_str().parse::<u32>().ok())
                .and_then(char::from_u32)
                .map_or_else(
                    || {
                        caps.get(0)
                            .map_or_else(String::new, |m| m.as_str().to_owned())
                    },
                    |ch| ch.to_string(),
                )
        });
        dec_decoded
            .replace("&apos;", "'")
            .replace("&quot;", "\"")
            .replace("&nbsp;", " ")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            // Last, so "&amp;#x27;" doesn't decode twice into an apostrophe.
            .replace("&amp;", "&")
    }
}

/// Parses standard `[mm:ss.xx]` LRC — including BitChord's offline enhanced A2
/// format (`<R>` alignment marker and `<mm:ss.xx>` word/syllable runs with a
/// closing end-time stamp) — into [`LyricLine`]s.
///
/// Mirrors Kotlin's `LrcLib.parseLrc`:
/// - Metadata tags without timestamps are ignored.
/// - Bare timestamps mark instrumental gaps; short intermediate gaps
///   (`< MIN_GAP_MS`) are dropped, while a trailing bare stamp (outro) is kept.
/// - When singing begins at or after `MIN_GAP_MS`, a leading intro gap at `0 ms`
///   is prepended.
pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut all: Vec<LyricLine> = lrc
        .lines()
        .filter_map(|line| {
            let caps = LRC_STAMP.captures(line)?;
            let full = caps.get(0)?;
            let time_ms = parse_stamp_caps(&caps);
            let body = &line[full.end()..];
            let (text_body, alignment) = strip_alignment_marker(body);
            let text = WORD_STAMP.replace_all(text_body, "").trim().to_owned();
            let words = parse_word_runs(text_body);
            Some(LyricLine {
                time_ms,
                text,
                words,
                alignment,
                ..LyricLine::default()
            })
        })
        .collect();

    all.sort_by_key(|line| line.time_ms);

    let mut kept = Vec::with_capacity(all.len() + 1);
    for (index, line) in all.iter().enumerate() {
        if !line.is_gap() {
            kept.push(line.clone());
            continue;
        }
        // A trailing stamp closes off the last line — that's the outro.
        match all.get(index + 1) {
            None => kept.push(line.clone()),
            Some(next) if next.time_ms - line.time_ms >= MIN_GAP_MS => kept.push(line.clone()),
            Some(_) => {}
        }
    }

    if let Some(first) = kept.first()
        && !first.is_gap()
        && first.time_ms >= MIN_GAP_MS
    {
        kept.insert(0, LyricLine::new(0, ""));
    }
    kept
}

fn strip_alignment_marker(body: &str) -> (&str, LyricAlignment) {
    if let Some(rest) = body.strip_prefix(ALIGNMENT_MARKER) {
        (rest, LyricAlignment::End)
    } else {
        (body, LyricAlignment::Start)
    }
}

fn parse_word_runs(body: &str) -> Vec<LyricWord> {
    let marks: Vec<(i64, usize, usize)> = WORD_STAMP
        .captures_iter(body)
        .filter_map(|caps| {
            let full = caps.get(0)?;
            Some((parse_stamp_caps(&caps), full.start(), full.end()))
        })
        .collect();
    if marks.is_empty() {
        return Vec::new();
    }
    let runs: Vec<TimedRun> = marks
        .iter()
        .enumerate()
        .map(|(index, &(start_ms, _, end_byte))| {
            let (next_start_ms, until_byte) = marks
                .get(index + 1)
                .map_or((start_ms, body.len()), |&(next_ms, next_start, _)| {
                    (next_ms, next_start)
                });
            TimedRun::new(start_ms, next_start_ms, &body[end_byte..until_byte])
        })
        .collect();
    words_from_runs(&runs, false)
}

fn parse_stamp_caps(caps: &Captures<'_>) -> i64 {
    let minutes = caps
        .get(1)
        .and_then(|m| m.as_str().parse::<i64>().ok())
        .unwrap_or(0);
    let seconds = caps
        .get(2)
        .and_then(|m| m.as_str().parse::<i64>().ok())
        .unwrap_or(0);
    let fraction_str = caps.get(3).map_or("", |m| m.as_str());
    let fraction = fraction_str.parse::<i64>().unwrap_or(0);
    // Two digits mean centiseconds, three mean milliseconds.
    let fraction_ms = match fraction_str.len() {
        2 => fraction * 10,
        3 => fraction,
        _ => 0,
    };
    minutes * 60_000 + seconds * 1_000 + fraction_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::LyricSyllable;

    fn sung(lines: Vec<LyricLine>) -> Vec<LyricLine> {
        lines.into_iter().filter(|l| !l.is_gap()).collect()
    }

    #[test]
    fn reads_word_timings_out_of_enhanced_lrc() {
        let lines = sung(EnhancedLrc::parse(
            "[00:27.39]<00:27.39>I <00:27.54>been <00:27.74>tryna <00:28.07>call\n\
             [00:30.18]<00:30.18>on <00:30.39>my <00:30.64>own",
        ));
        assert_eq!(2, lines.len());
        assert_eq!("I been tryna call", lines[0].text);
        assert_eq!(
            vec!["I", "been", "tryna", "call"],
            lines[0]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(27_540, lines[0].words[0].end_ms);
        assert_eq!(30_180, lines[0].words.last().expect("has words").end_ms);
    }

    #[test]
    fn plain_lrc_is_left_for_line_synced_parser() {
        assert!(
            EnhancedLrc::parse("[00:12.00] no word stamps here\n[00:15.00] none here either")
                .is_empty()
        );
    }

    #[test]
    fn decodes_html_entities_without_double_decoding_ampersands() {
        let line = sung(EnhancedLrc::parse(
            "[00:01.00]<00:01.00>don&#x27;t <00:01.50>stop",
        ))
        .remove(0);
        assert_eq!("don't stop", line.text);
        assert_eq!("&#x27;", EnhancedLrc::decode_entities("&amp;#x27;"));
    }

    #[test]
    fn enhanced_lrc_glues_syllables_instead_of_spacing_them_apart() {
        let line = sung(EnhancedLrc::parse(
            "[00:30.18]<00:30.189>long <00:31.839>e<00:31.996>nough<00:32.529>",
        ))
        .remove(0);
        assert_eq!("long enough", line.text);
        assert_eq!(
            vec![
                LyricSyllable::new(31_839, 31_996, 0, 1),
                LyricSyllable::new(31_996, 32_529, 1, 6),
            ],
            line.words[1].syllables
        );
    }

    #[test]
    fn enhanced_lrc_with_no_spaces_keeps_word_per_stamp() {
        let line = sung(EnhancedLrc::parse(
            "[00:01.00]<00:01.00>I<00:01.20>been<00:01.50>tryna<00:02.00>",
        ))
        .remove(0);
        assert_eq!(
            vec!["I", "been", "tryna"],
            line.words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!("I been tryna", line.text);
    }

    #[test]
    fn parse_lrc_parses_centisecond_and_millisecond_stamps() {
        let lines = sung(parse_lrc(
            "[00:32.07] first line\n\
             [01:05.50] second line",
        ));
        assert_eq!(2, lines.len());
        assert_eq!(32_070, lines[0].time_ms);
        assert_eq!("first line", lines[0].text);
        assert_eq!(65_500, lines[1].time_ms);

        let ms = sung(parse_lrc("[02:03.456] third line"));
        assert_eq!(1, ms.len());
        assert_eq!(123_456, ms[0].time_ms);
    }

    #[test]
    fn parse_lrc_drops_metadata_tags_and_short_gaps() {
        let lines = parse_lrc(
            "[ar:Arijit Singh]\n\
             [ti:Zaalima]\n\
             [00:10.00]\n\
             [00:12.00] real words\n",
        );
        let words: Vec<&str> = lines
            .iter()
            .filter(|l| !l.is_gap())
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(vec!["real words"], words);
        assert_eq!(1, lines.iter().filter(|l| l.is_gap()).count());
    }

    #[test]
    fn parse_lrc_keeps_long_gaps_outros_and_leading_intro_gaps() {
        let long = parse_lrc(
            "[00:00.00] intro words\n\
             [00:05.00]\n\
             [00:30.00] verse",
        );
        assert_eq!(3, long.len());
        assert!(long[1].is_gap());
        assert_eq!(5_000, long[1].time_ms);

        let outro = parse_lrc("[00:10.00] words\n[04:49.01] ");
        assert_eq!(289_010, outro.last().expect("has outro").time_ms);
        assert!(outro.last().expect("has outro").is_gap());

        let intro = parse_lrc("[00:32.07] first words");
        assert_eq!(2, intro.len());
        assert!(intro[0].is_gap());
        assert_eq!(0, intro[0].time_ms);
        assert_eq!("first words", intro[1].text);

        let immediate = parse_lrc("[00:01.00] straight in");
        assert_eq!(1, immediate.len());
        assert_eq!("straight in", immediate[0].text);
    }

    #[test]
    fn parse_lrc_sorts_out_of_order_stamps() {
        let lines = sung(parse_lrc("[00:30.00] later\n[00:10.00] earlier"));
        assert_eq!(
            vec!["earlier", "later"],
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>()
        );
        assert!(lines[0].time_ms < lines[1].time_ms);
    }
}
