use crate::line::{LyricLine, LyricWord};

/// Pulls the answering vocal out of each line and hangs it underneath as
/// [`LyricLine::background`].
///
/// Only Apple's TTML marks the answering vocal structurally (`ttm:role="x-bg"`).
/// Other providers write it inline as a trailing parenthetical bracket; split
/// out, it keeps its own word timings and draws beneath the lead without
/// dragging the lead's sweep past the next line's stamp.
///
/// The parentheses are kept on the background text so [`crate::to_lrc`] can
/// round-trip it cleanly for external players.
pub fn with_background_vocals(lines: Vec<LyricLine>) -> Vec<LyricLine> {
    lines.into_iter().map(split_trailing_bracket).collect()
}

/// Splits a single line's trailing parenthetical backing vocal into
/// [`LyricLine::background`], returning the line unchanged if it already has a
/// background vocal, is a gap, or has no qualifying trailing bracket.
pub fn split_trailing_bracket(line: LyricLine) -> LyricLine {
    // A source that marked its own backing vocal has already said everything
    // guessing from punctuation could, and better.
    if line.background.is_some() || line.is_gap() {
        return line;
    }

    let Some(open) = bracket_start(&line.text) else {
        return line;
    };
    let lead = line.text[..open].trim_end();
    let backing = line.text[open..].trim();
    if lead.is_empty() || !backing.chars().any(char::is_alphanumeric) {
        return line;
    }

    if line.words.is_empty() {
        // Line-synced: share the line's stamp and its stated end.
        let backing_line = LyricLine {
            time_ms: line.time_ms,
            text: backing.to_owned(),
            sung_until_ms: line.sung_until_ms,
            ..LyricLine::default()
        };
        return LyricLine {
            text: lead.to_owned(),
            background: Some(Box::new(backing_line)),
            ..line
        };
    }

    // `text` is words joined by single ASCII spaces in every word-synced parser
    // here, so the bracket's UTF-8 byte offset aligns with a word boundary
    // unless the bracket opens mid-word ("wait(ing)").
    let Some(split) = index_of_first_starting_at(&line.words, open) else {
        return line;
    };
    if split == 0 {
        return line;
    }

    let lead_owned = lead.to_owned();
    let backing_owned = backing.to_owned();
    let mut lead_words = line.words;
    let backing_words = lead_words.split_off(split);
    let backing_time_ms = backing_words
        .first()
        .map_or(line.time_ms, |word| word.start_ms);

    LyricLine {
        text: lead_owned,
        words: lead_words,
        background: Some(Box::new(LyricLine {
            time_ms: backing_time_ms,
            text: backing_owned,
            words: backing_words,
            ..LyricLine::default()
        })),
        ..line
    }
}

/// Index of the word starting at UTF-8 byte `offset` in `words.join(" ")`, or
/// `None` if no word starts there.
fn index_of_first_starting_at(words: &[LyricWord], offset: usize) -> Option<usize> {
    let mut at = 0usize;
    for (index, word) in words.iter().enumerate() {
        if at == offset {
            return Some(index);
        }
        if at > offset {
            return None;
        }
        // Plus the single ASCII space byte joining this word to the next one.
        at += word.text.len() + 1;
    }
    None
}

/// UTF-8 byte index where the bracket closing `text` opens, or `None` if
/// `text` does not end in a balanced trailing bracket after leading content.
///
/// Walked back from the end counting depth so nested brackets split at the
/// outer pair. A line that is entirely bracketed (`index == 0`) returns `None`.
fn bracket_start(text: &str) -> Option<usize> {
    if !text.ends_with(')') {
        return None;
    }
    let mut depth = 0usize;
    for (byte_index, ch) in text.char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return (byte_index > 0).then_some(byte_index);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_synced(spans: &[(i64, i64, &str)]) -> LyricLine {
        let words: Vec<LyricWord> = spans
            .iter()
            .map(|&(start, end, text)| LyricWord::new(start, end, text))
            .collect();
        let text = words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let time_ms = words.first().map_or(0, |w| w.start_ms);
        LyricLine {
            time_ms,
            text,
            words,
            ..LyricLine::default()
        }
    }

    #[test]
    fn trailing_bracket_becomes_answering_line() {
        let line = with_background_vocals(vec![LyricLine::new(1_000, "lead words (echoed words)")])
            .remove(0);
        assert_eq!("lead words", line.text);
        assert_eq!(
            Some("(echoed words)"),
            line.background.as_deref().map(|b| b.text.as_str())
        );
    }

    #[test]
    fn answering_line_takes_the_words_inside_the_bracket() {
        let line = with_background_vocals(vec![word_synced(&[
            (1_000, 1_400, "lead"),
            (1_400, 1_900, "words"),
            (2_100, 2_500, "(echoed"),
            (2_500, 3_200, "words)"),
        ])])
        .remove(0);

        assert_eq!("lead words", line.text);
        assert_eq!(
            vec!["lead", "words"],
            line.words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );

        let backing = line.background.as_deref().expect("background present");
        assert_eq!("(echoed words)", backing.text);
        assert_eq!(
            vec!["(echoed", "words)"],
            backing
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(2_100, backing.time_ms);
        assert!(backing.is_word_synced());
    }

    #[test]
    fn line_ends_when_answer_does_not_when_lead_does() {
        let line = with_background_vocals(vec![word_synced(&[
            (1_000, 1_400, "lead"),
            (2_100, 3_200, "(echo)"),
        ])])
        .remove(0);
        assert_eq!(3_200, line.end_ms());
    }

    #[test]
    fn entirely_bracketed_line_is_left_alone() {
        let line = with_background_vocals(vec![LyricLine::new(1_000, "(ooh ooh)")]).remove(0);
        assert_eq!("(ooh ooh)", line.text);
        assert!(line.background.is_none());
    }

    #[test]
    fn bracket_in_middle_of_line_is_left_alone() {
        let line = with_background_vocals(vec![LyricLine::new(1_000, "a (parenthetical) aside")])
            .remove(0);
        assert_eq!("a (parenthetical) aside", line.text);
        assert!(line.background.is_none());
    }

    #[test]
    fn bracket_opening_mid_word_is_not_a_second_voice() {
        let line = with_background_vocals(vec![word_synced(&[
            (1_000, 1_400, "still"),
            (1_400, 2_000, "wait(ing)"),
        ])])
        .remove(0);
        assert_eq!("still wait(ing)", line.text);
        assert!(line.background.is_none());
    }

    #[test]
    fn nesting_splits_at_outer_bracket() {
        let line =
            with_background_vocals(vec![LyricLine::new(1_000, "lead (echo (twice))")]).remove(0);
        assert_eq!("lead", line.text);
        assert_eq!(
            Some("(echo (twice))"),
            line.background.as_deref().map(|b| b.text.as_str())
        );
    }

    #[test]
    fn bracket_with_no_words_is_not_a_second_voice() {
        let line = with_background_vocals(vec![LyricLine::new(1_000, "lead words (!)")]).remove(0);
        assert_eq!("lead words (!)", line.text);
        assert!(line.background.is_none());
    }

    #[test]
    fn line_synced_answer_shares_stamp_and_stated_end() {
        let line = with_background_vocals(vec![LyricLine {
            time_ms: 1_000,
            text: "lead words (echo)".to_owned(),
            sung_until_ms: Some(4_000),
            ..LyricLine::default()
        }])
        .remove(0);
        assert_eq!("lead words", line.text);
        let bg = line.background.as_deref().expect("background present");
        assert_eq!(1_000, bg.time_ms);
        assert_eq!(Some(4_000), bg.sung_until_ms);
        assert_eq!(4_000, line.end_ms());
    }

    #[test]
    fn source_that_marked_its_own_answer_is_not_second_guessed() {
        let marked = LyricLine {
            time_ms: 1_000,
            text: "lead words (already split)".to_owned(),
            background: Some(Box::new(LyricLine::new(1_500, "(the real answer)"))),
            ..LyricLine::default()
        };
        let line = with_background_vocals(vec![marked.clone()]).remove(0);
        assert_eq!(marked, line);
    }

    #[test]
    fn instrumental_breaks_are_left_untouched() {
        let line = with_background_vocals(vec![LyricLine::new(1_000, "")]).remove(0);
        assert!(line.is_gap());
        assert!(line.background.is_none());
    }

    #[test]
    fn multibyte_utf8_word_synced_trailing_bracket_splits_cleanly() {
        let line = with_background_vocals(vec![word_synced(&[
            (1_000, 1_500, "café"),
            (1_600, 2_200, "(écho)"),
        ])])
        .remove(0);
        assert_eq!("café", line.text);
        assert_eq!(
            Some("(écho)"),
            line.background.as_deref().map(|b| b.text.as_str())
        );
    }
}
