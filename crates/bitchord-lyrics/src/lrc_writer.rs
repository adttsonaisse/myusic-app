use crate::line::{LyricAlignment, LyricLine, LyricWord, slice_chars};

/// Custom container tag field used by BitChord for offline enhanced A2 lyrics.
pub const WORD_LYRICS_FIELD: &str = "BITCHORD_LYRICS";

/// Marker that survives round-trip through file tags and signals RTL (`End`) alignment.
const ALIGNMENT_MARKER: &str = "<R>";

/// Formats parsed lines as portable `[mm:ss.xx]` LRC (or plain text if all
/// timestamps are `<= 0`).
///
/// Word timings are deliberately dropped so third-party players without A2
/// support do not render `<mm:ss.xx>` angle-bracket noise. Any
/// [`LyricLine::background`] vocal is appended back onto the end of its lead
/// line.
pub fn to_lrc(lines: &[LyricLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    // Plain unsynced lyrics have no timestamps — emit clean text rows.
    if !lines.iter().any(|line| line.time_ms > 0) {
        return lines.iter().map(flattened).collect::<Vec<_>>().join("\n");
    }
    let mut sorted: Vec<&LyricLine> = lines.iter().collect();
    sorted.sort_by_key(|line| line.time_ms);
    sorted
        .into_iter()
        .map(|line| format!("{}{}", stamp(line.time_ms), flattened(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Formats word-synced lines as BitChord's enhanced A2 LRC (`<mm:ss.xx>` word
/// and syllable runs, `<R>` alignment marker, and a closing `<mm:ss.xx>` end
/// timestamp).
///
/// Returns an empty `String` when no line (nor its background) carries word
/// timings.
pub fn to_enhanced_lrc(lines: &[LyricLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let has_words = lines.iter().any(|line| {
        line.is_word_synced()
            || line
                .background
                .as_deref()
                .is_some_and(LyricLine::is_word_synced)
    });
    if !has_words {
        return String::new();
    }
    let mut sorted: Vec<&LyricLine> = lines.iter().collect();
    sorted.sort_by_key(|line| line.time_ms);
    sorted
        .into_iter()
        .map(|line| format!("{}{}", stamp(line.time_ms), enhanced_body(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn flattened(line: &LyricLine) -> String {
    match line.background.as_deref() {
        Some(bg) => format!("{} {}", line.text, bg.text).trim().to_owned(),
        None => line.text.clone(),
    }
}

fn enhanced_body(line: &LyricLine) -> String {
    let runs = timed_runs(line);
    if runs.is_empty() {
        return flattened(line);
    }
    let mut out = String::new();
    if line.alignment == LyricAlignment::End {
        out.push_str(ALIGNMENT_MARKER);
    }
    let mut previous = line.time_ms;
    let last_index = runs.len() - 1;
    for (index, word) in runs.iter().enumerate() {
        if word.syllables.is_empty() {
            let start = word.start_ms.max(previous);
            out.push_str(&word_stamp(start));
            out.push_str(&word.text);
            previous = start;
        } else {
            for syllable in &word.syllables {
                let start = syllable.start_ms.max(previous);
                out.push_str(&word_stamp(start));
                out.push_str(slice_chars(
                    &word.text,
                    syllable.start_char,
                    syllable.end_char,
                ));
                previous = start;
            }
        }
        if index != last_index {
            out.push(' ');
        }
    }
    // A line that is one split word has no space in it anywhere, and a line
    // with none is read back as one word per stamp; the space before the
    // closing stamp says the spacing is real.
    if runs.iter().any(|word| !word.syllables.is_empty()) {
        out.push(' ');
    }
    let max_end = runs
        .iter()
        .map(|word| word.end_ms)
        .max()
        .unwrap_or(previous);
    out.push_str(&word_stamp(max_end.max(previous)));
    out
}

fn timed_runs(line: &LyricLine) -> Vec<LyricWord> {
    if line.words.is_empty() {
        return Vec::new();
    }
    let mut runs = line.words.clone();
    if let Some(bg) = line.background.as_deref() {
        if !bg.words.is_empty() {
            runs.extend(bg.words.iter().cloned());
        } else if !bg.text.trim().is_empty() {
            runs.push(LyricWord::new(bg.time_ms, bg.end_ms(), bg.text.clone()));
        }
    }
    runs
}

fn stamp(time_ms: i64) -> String {
    format!("[{}]", clock(time_ms))
}

fn word_stamp(time_ms: i64) -> String {
    format!("<{}>", clock(time_ms))
}

fn clock(time_ms: i64) -> String {
    let total = time_ms.max(0);
    let minutes = total / 60_000;
    let seconds = (total % 60_000) / 1_000;
    let centiseconds = (total % 1_000) / 10;
    format!("{minutes:02}:{seconds:02}.{centiseconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enhanced_lrc::parse_lrc;
    use crate::line::LyricSyllable;

    #[test]
    fn each_line_is_stamped_as_mm_colon_ss_dot_centiseconds() {
        let lines = vec![
            LyricLine::new(0, "first line"),
            LyricLine::new(61_230, "second line"),
        ];
        assert_eq!(
            "[00:00.00]first line\n[01:01.23]second line",
            to_lrc(&lines)
        );
    }

    #[test]
    fn milliseconds_are_truncated_to_centiseconds_rather_than_rounded_up() {
        assert_eq!("[00:59.99]x", to_lrc(&[LyricLine::new(59_999, "x")]));
    }

    #[test]
    fn stamps_past_ninety_nine_minutes_overflow_to_three_digits() {
        assert_eq!("[100:00.00]x", to_lrc(&[LyricLine::new(6_000_000, "x")]));
    }

    #[test]
    fn instrumental_gap_is_written_as_bare_stamp() {
        let lines = vec![
            LyricLine::new(1_000, "first line"),
            LyricLine::new(8_000, ""),
            LyricLine::new(20_000, "second line"),
        ];
        assert_eq!(
            "[00:01.00]first line\n[00:08.00]\n[00:20.00]second line",
            to_lrc(&lines)
        );
    }

    #[test]
    fn lines_are_sorted_by_stamp_whatever_order_they_arrive_in() {
        let lines = vec![
            LyricLine::new(5_000, "later"),
            LyricLine::new(1_000, "earlier"),
        ];
        assert_eq!("[00:01.00]earlier\n[00:05.00]later", to_lrc(&lines));
    }

    #[test]
    fn word_timings_are_dropped_in_portable_lrc() {
        let lines = vec![LyricLine {
            time_ms: 1_000,
            text: "two words".to_owned(),
            words: vec![
                LyricWord::new(1_000, 1_400, "two"),
                LyricWord::new(1_400, 2_000, "words"),
            ],
            ..LyricLine::default()
        }];
        let written = to_lrc(&lines);
        assert_eq!("[00:01.00]two words", written);
        assert!(!written.contains('<'));
    }

    #[test]
    fn answering_vocal_is_written_back_onto_end_of_lead() {
        let lines = vec![LyricLine {
            time_ms: 1_000,
            text: "the lead line".to_owned(),
            background: Some(Box::new(LyricLine::new(1_600, "(the answer)"))),
            ..LyricLine::default()
        }];
        assert_eq!("[00:01.00]the lead line (the answer)", to_lrc(&lines));
    }

    #[test]
    fn empty_lines_yields_empty_text_and_unsynced_emits_plain_lines() {
        assert_eq!("", to_lrc(&[]));
        assert_eq!("", to_enhanced_lrc(&[]));
        assert_eq!(
            "line one\nline two",
            to_lrc(&[LyricLine::new(0, "line one"), LyricLine::new(0, "line two")])
        );
    }

    #[test]
    fn what_to_lrc_writes_parse_lrc_reads_back_unchanged() {
        let lines = vec![
            LyricLine::new(0, ""),
            LyricLine::new(6_120, "first line"),
            LyricLine::new(12_340, "second line"),
            LyricLine::new(18_000, ""),
            LyricLine::new(25_500, "third line"),
        ];
        let reparsed = parse_lrc(&to_lrc(&lines));
        assert_eq!(
            lines.iter().map(|l| l.time_ms).collect::<Vec<_>>(),
            reparsed.iter().map(|l| l.time_ms).collect::<Vec<_>>()
        );
        assert_eq!(
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            reparsed.iter().map(|l| l.text.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn enhanced_lrc_round_trips_syllables_utf8_and_alignment() {
        let enough_syllables = vec![
            LyricSyllable::new(31_839, 31_996, 0, 1),
            LyricSyllable::new(31_996, 32_529, 1, 6),
        ];
        let lines = vec![
            LyricLine {
                time_ms: 31_498,
                text: "long enough".to_owned(),
                words: vec![
                    LyricWord::new(31_498, 31_839, "long"),
                    LyricWord::with_syllables(31_839, 32_529, "enough", enough_syllables.clone()),
                ],
                ..LyricLine::default()
            },
            LyricLine {
                time_ms: 40_000,
                text: "enough".to_owned(),
                words: vec![LyricWord::with_syllables(
                    40_000,
                    40_690,
                    "enough",
                    enough_syllables
                        .iter()
                        .map(|s| {
                            LyricSyllable::new(
                                s.start_ms + 8_161,
                                s.end_ms + 8_161,
                                s.start_char,
                                s.end_char,
                            )
                        })
                        .collect(),
                )],
                alignment: LyricAlignment::End,
                ..LyricLine::default()
            },
            LyricLine {
                time_ms: 45_000,
                text: "café".to_owned(),
                words: vec![LyricWord::with_syllables(
                    45_000,
                    45_600,
                    "café",
                    vec![
                        LyricSyllable::new(45_000, 45_200, 0, 3),
                        LyricSyllable::new(45_200, 45_600, 3, 4),
                    ],
                )],
                ..LyricLine::default()
            },
        ];

        let encoded = to_enhanced_lrc(&lines);
        let back: Vec<LyricLine> = parse_lrc(&encoded)
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        assert_eq!(
            vec!["long enough", "enough", "café"],
            back.iter().map(|l| l.text.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(
            vec!["long", "enough"],
            back[0]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            vec![
                LyricSyllable::new(31_830, 31_990, 0, 1),
                LyricSyllable::new(31_990, 32_520, 1, 6),
            ],
            back[0].words[1].syllables
        );
        assert_eq!(
            vec!["enough"],
            back[1]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(2, back[1].words[0].syllables.len());
        assert_eq!(LyricAlignment::End, back[1].alignment);
        assert_eq!(
            vec![
                LyricSyllable::new(45_000, 45_200, 0, 3),
                LyricSyllable::new(45_200, 45_600, 3, 4),
            ],
            back[2].words[0].syllables
        );
    }
}
