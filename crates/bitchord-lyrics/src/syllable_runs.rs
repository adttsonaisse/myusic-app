use crate::line::{LyricSyllable, LyricWord};

/// One stamped run of text as a provider wrote it, its spacing untouched: a
/// word, or one syllable of one. `raw` keeps whatever whitespace sat around
/// it, because that whitespace is the only thing saying where one word stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedRun {
    pub start_ms: i64,
    pub end_ms: i64,
    pub raw: String,
}

impl TimedRun {
    pub fn new(start_ms: i64, end_ms: i64, raw: impl Into<String>) -> Self {
        Self {
            start_ms,
            end_ms,
            raw: raw.into(),
        }
    }
}

#[derive(Default)]
struct RunWordAcc {
    words: Vec<LyricWord>,
    current: String,
    current_chars: usize,
    parts: Vec<LyricSyllable>,
    start: i64,
    end: i64,
}

impl RunWordAcc {
    fn flush(&mut self) {
        if !self.current.is_empty() {
            let text = std::mem::take(&mut self.current);
            let syllables = LyricSyllable::normalized(&self.parts, self.current_chars);
            self.words.push(LyricWord::with_syllables(
                self.start,
                self.start.max(self.end),
                text,
                syllables,
            ));
        }
        self.current_chars = 0;
        self.parts.clear();
    }
}

/// Glues `runs` into words, each one keeping its constituent runs as
/// [`LyricSyllable`]s.
///
/// Every format that times syllables says which ones belong together by
/// leaving the space out between them. A run that neither ends in whitespace
/// nor is followed by a run starting with whitespace is the front of a word
/// that carries on. A blank run ends the word in hand.
///
/// Two cases where a missing space says nothing:
/// - Han, kana, CJK punctuation, full-width forms, and Thai are written without
///   spaces between words, so each run stays its own word.
/// - A writer that puts no whitespace anywhere in the line is omitting word
///   boundaries rather than marking syllables; each run stays its own word
///   unless `spacing_is_explicit` is `true`.
pub fn words_from_runs(runs: &[TimedRun], spacing_is_explicit: bool) -> Vec<LyricWord> {
    let glue = spacing_is_explicit
        || runs
            .iter()
            .any(|run| run.raw.chars().any(char::is_whitespace));

    let mut acc = RunWordAcc::default();

    for run in runs {
        let content = run.raw.trim();
        if content.is_empty() {
            acc.flush();
            continue;
        }
        let starts_with_ws = run.raw.chars().next().is_some_and(char::is_whitespace);
        let prev_unspaced = acc
            .current
            .chars()
            .next_back()
            .is_some_and(is_unspaced_script);
        let next_unspaced = content.chars().next().is_some_and(is_unspaced_script);
        let joins =
            glue && !acc.current.is_empty() && !starts_with_ws && !prev_unspaced && !next_unspaced;
        if !joins {
            acc.flush();
        }
        if acc.current.is_empty() {
            acc.start = run.start_ms;
            acc.end = run.start_ms;
        }
        let from = acc.current_chars;
        acc.current.push_str(content);
        acc.current_chars += content.chars().count();
        let run_end = run.start_ms.max(run.end_ms);
        acc.parts.push(LyricSyllable::new(
            run.start_ms,
            run_end,
            from,
            acc.current_chars,
        ));
        acc.end = acc.end.max(run_end);

        let ends_with_ws = run.raw.chars().next_back().is_some_and(char::is_whitespace);
        if !glue || ends_with_ws {
            acc.flush();
        }
    }
    acc.flush();
    acc.words
}

/// Scripts written without spaces between words.
fn is_unspaced_script(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{9FFF}' // Han
            | '\u{3040}'..='\u{30FF}' // hiragana and katakana
            | '\u{3000}'..='\u{303F}' // CJK punctuation
            | '\u{FF00}'..='\u{FFEF}' // full-width forms
            | '\u{0E00}'..='\u{0E7F}' // Thai
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glues_adjacent_runs_into_one_word_with_syllables() {
        let runs = vec![
            TimedRun::new(30_189, 30_530, "long "),
            TimedRun::new(31_839, 31_996, "e"),
            TimedRun::new(31_996, 32_529, "nough"),
        ];
        let words = words_from_runs(&runs, false);
        assert_eq!(2, words.len());
        assert_eq!("long", words[0].text);
        assert!(words[0].syllables.is_empty());
        assert_eq!("enough", words[1].text);
        assert_eq!(
            vec![
                LyricSyllable::new(31_839, 31_996, 0, 1),
                LyricSyllable::new(31_996, 32_529, 1, 6),
            ],
            words[1].syllables
        );
    }

    #[test]
    fn unspaced_scripts_stay_one_word_per_run() {
        let runs = vec![
            TimedRun::new(1_000, 1_300, "我"),
            TimedRun::new(1_300, 1_600, "爱"),
            TimedRun::new(1_600, 1_900, "你"),
        ];
        let words = words_from_runs(&runs, true);
        assert_eq!(3, words.len());
        assert!(words.iter().all(|w| w.syllables.is_empty()));
    }

    #[test]
    fn explicit_spacing_allows_single_split_word_with_no_whitespace() {
        let runs = vec![
            TimedRun::new(1_000, 1_200, "caf"),
            TimedRun::new(1_200, 1_600, "é"),
        ];
        let words = words_from_runs(&runs, true);
        assert_eq!(1, words.len());
        assert_eq!("café", words[0].text);
        assert_eq!(
            vec![
                LyricSyllable::new(1_000, 1_200, 0, 3),
                LyricSyllable::new(1_200, 1_600, 3, 4),
            ],
            words[0].syllables
        );
    }

    #[test]
    fn missing_whitespace_everywhere_keeps_word_per_run_unless_explicit() {
        let runs = vec![
            TimedRun::new(1_000, 1_200, "I"),
            TimedRun::new(1_200, 1_500, "been"),
            TimedRun::new(1_500, 2_000, "tryna"),
        ];
        let words = words_from_runs(&runs, false);
        assert_eq!(
            vec!["I", "been", "tryna"],
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>()
        );
    }
}
