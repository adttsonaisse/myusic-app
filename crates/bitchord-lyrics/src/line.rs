use std::ops::Range;

use serde::{Deserialize, Serialize};

/// How long a word takes to rise, and to settle back down once it is past.
const RISE_MS: f32 = 700.0;
const RISE_MS_I64: i64 = 700;

/// The quickest a word ever rises: a 90 ms syllable snapping up reads as a twitch.
const RISE_MIN_MS: f32 = 250.0;

const LIFT_QUICK_MS: f32 = 150.0;
const LIFT_HELD_MS: f32 = 900.0;
const LIFT_FLOOR: f32 = 0.25;

/// Longest word worth running a wave through; past this it reads as a ripple.
const GROW_MAX_CHARS: usize = 7;

/// How long a word of each length has to be held before it qualifies.
const GROW_MIN_SOLO_MS: i64 = 1_100;
const GROW_MIN_SHORT_MS: i64 = 1_360;
const GROW_SHORT_STEP_MS: i64 = 140;
const GROW_MIN_FOUR_MS: i64 = 1_050;
const GROW_MIN_LONG_MS: i64 = 900;
const GROW_MS_PER_CHAR: i64 = 200;

const GROW_DECAY_LONG_CHARS: usize = 5;
const GROW_DECAY_QUICK_MS: f32 = 1_200.0;
const GROW_DECAY_QUICK_FLOOR_MS: f32 = 800.0;
const GROW_DECAY_LONG: f32 = 0.4;
const GROW_DECAY_QUICK: f32 = 0.3;
const GROW_DECAY_QUICK_TINY: f32 = 0.1;
const GROW_DECAY_MAX: f32 = 0.7;

/// Each letter starts this far — as a share of the word's own length — after
/// the one before it, so the swell travels rather than pulsing.
const GROW_STAGGER: f32 = 0.09;

/// The whole move runs half again as long as the word is held.
const GROW_SPAN: f32 = 1.5;

/// Up by `GROW_IN`, held to `GROW_HOLD`, settled by `GROW_OUT`.
const GROW_IN: f32 = 0.25;
const GROW_HOLD: f32 = 0.30;
const GROW_OUT: f32 = 0.75;

/// Where a letter comes to rest: the same small lift every sung word carries.
const GROW_REST: f32 = 1.0;

/// The window over which a longer hold earns more of the treatment.
const GROW_RAMP_MIN_MS: f32 = 400.0;
const GROW_RAMP_MAX_MS: f32 = 3_000.0;

/// Swell every qualifying letter gets, and the range the hold adds on top.
const GROW_BASE_SHORT: f32 = 0.05;
const GROW_BASE_LONG: f32 = 0.04;
const GROW_SCALE_RANGE: f32 = 0.08;

/// The most any letter swells, which is what [`GrowingWord`] reads the lift off.
const GROW_SCALE_CEILING: f32 = 0.1;

/// Taken back off the swell so it stops a hair short of the ceiling.
const GROW_SCALE_TRIM: f32 = 0.98;

/// Lean, as a share of the font size at full swell.
const GROW_SHIFT_EM: f32 = 25.0 / 34.0;

/// The bloom's floor, and what the hold adds to it.
const GROW_BLOOM_FLOOR: f32 = 0.35;
const GROW_BLOOM_RANGE: f32 = 0.45;
const GROW_BLOOM_PACE_MS: f32 = 1_500.0;
const GROW_BLOOM_PACE_MAX: f32 = 1.1;
const GROW_BLOOM_SHORT: f32 = 0.85;
const GROW_BLOOM_LONG: f32 = 1.1;

/// A word held this long lifts as far as it is going to.
const GROW_LIFT_PACE_MS: f32 = 2_000.0;
const GROW_LIFT_FLOOR: f32 = 0.3;

/// One separately timed piece of a [`LyricWord`]: the characters `start_char..end_char`
/// (Unicode scalar indices into the word's text), sung from `start_ms` to `end_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricSyllable {
    pub start_ms: i64,
    pub end_ms: i64,
    pub start_char: usize,
    pub end_char: usize,
}

impl LyricSyllable {
    pub const fn new(start_ms: i64, end_ms: i64, start_char: usize, end_char: usize) -> Self {
        Self {
            start_ms,
            end_ms,
            start_char,
            end_char,
        }
    }

    /// Tidies a word's syllables, as a parser collected them, into what
    /// [`LyricWord::chars_sung_at`] expects: in order, end to end with no
    /// characters between them, and covering the whole of a `length`-character
    /// word. Punctuation hanging off the last syllable belongs to it, and an
    /// untimed apostrophe between two goes to the first.
    ///
    /// Empty when there is nothing to split — one syllable is just the word.
    pub fn normalized(parts: &[Self], length: usize) -> Vec<Self> {
        let mut usable: Vec<Self> = parts
            .iter()
            .copied()
            .filter(|part| part.end_char > part.start_char && part.start_char < length)
            .collect();
        usable.sort_by_key(|part| part.start_char);
        if usable.len() < 2 {
            return Vec::new();
        }
        usable
            .iter()
            .enumerate()
            .map(|(index, part)| Self {
                start_ms: part.start_ms,
                end_ms: part.start_ms.max(part.end_ms),
                start_char: if index == 0 { 0 } else { part.start_char },
                end_char: usable.get(index + 1).map_or(length, |next| next.start_char),
            })
            .collect()
    }
}

/// One word of a line, with the stretch of the song it is sung over.
///
/// Apple's TTML splits long words into syllables. The word is still the unit
/// the line is laid out, lifted and lit in — `"e"` and `"nough"` are one word on
/// screen — so `start_ms` is the first syllable's start and `end_ms` the last
/// one's end. But the syllables keep their own timing in `syllables`, and the
/// sweep follows those: `"e"` is clipped and `"nough"` is held, and a wipe spread
/// evenly over the pair runs ahead of the first half and behind the second.
///
/// Empty for a word nobody split, which is most of them — the word is then its
/// own single syllable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricWord {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    #[serde(default)]
    pub syllables: Vec<LyricSyllable>,
}

impl LyricWord {
    pub fn new(start_ms: i64, end_ms: i64, text: impl Into<String>) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.into(),
            syllables: Vec::new(),
        }
    }

    pub fn with_syllables(
        start_ms: i64,
        end_ms: i64,
        text: impl Into<String>,
        syllables: Vec<LyricSyllable>,
    ) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.into(),
            syllables,
        }
    }

    /// How many of `text`'s characters have been sung at `position_ms`, as a
    /// fraction. Each syllable fills its own characters over its own time, and
    /// a pause between two of them rests on the boundary rather than creeping
    /// into the next one before it is sung.
    pub fn chars_sung_at(&self, position_ms: i64) -> f32 {
        let char_len = self.text.chars().count();
        if position_ms <= self.start_ms {
            return 0.0;
        }
        if position_ms >= self.end_ms {
            return char_len as f32;
        }
        if self.syllables.is_empty() {
            let span = (self.end_ms - self.start_ms).max(1) as f32;
            return (position_ms - self.start_ms) as f32 / span * char_len as f32;
        }
        for syllable in &self.syllables {
            if position_ms < syllable.start_ms {
                return syllable.start_char as f32;
            }
            if position_ms < syllable.end_ms {
                let span = (syllable.end_ms - syllable.start_ms).max(1) as f32;
                let through = (position_ms - syllable.start_ms) as f32 / span;
                return syllable.start_char as f32
                    + through * syllable.end_char.saturating_sub(syllable.start_char) as f32;
            }
        }
        char_len as f32
    }

    /// This word with every character `drop` matches taken out of its text, and
    /// the syllables moved to keep covering the same letters. `None` if nothing
    /// is left.
    pub fn without_chars(&self, drop: impl Fn(char) -> bool) -> Option<Self> {
        if !self.text.chars().any(&drop) {
            return Some(self.clone());
        }
        let orig_chars = self.text.chars().count();
        // Where each old character offset lands once the dropped characters are gone.
        let mut moved = Vec::with_capacity(orig_chars + 1);
        let mut kept = String::with_capacity(self.text.len());
        let mut kept_chars = 0usize;
        for ch in self.text.chars() {
            moved.push(kept_chars);
            if !drop(ch) {
                kept.push(ch);
                kept_chars += 1;
            }
        }
        moved.push(kept_chars);

        let lead = kept.chars().position(|ch| !ch.is_whitespace())?;
        let trimmed = kept.trim().to_owned();
        let trimmed_chars = trimmed.chars().count();

        let parts: Vec<LyricSyllable> = self
            .syllables
            .iter()
            .map(|part| {
                let start_idx = part.start_char.min(orig_chars);
                let end_idx = part.end_char.min(orig_chars);
                LyricSyllable {
                    start_ms: part.start_ms,
                    end_ms: part.end_ms,
                    start_char: moved[start_idx].saturating_sub(lead),
                    end_char: moved[end_idx].saturating_sub(lead).min(trimmed_chars),
                }
            })
            .collect();
        Some(Self {
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            text: trimmed,
            syllables: LyricSyllable::normalized(&parts, trimmed_chars),
        })
    }

    /// Whether this word is held long enough, and is short enough, to be
    /// animated a letter at a time.
    ///
    /// Scripts whose letters are not laid out side by side independently are
    /// left alone — Han, kana and Hangul draw as blocks, and right-to-left
    /// scripts join up, so moving one letter of a word would come apart rather
    /// than swell. A hyphenated word is really two words and would break at the
    /// hyphen.
    pub fn can_grow(&self) -> bool {
        let length = self.text.chars().count();
        if length == 0 || length > GROW_MAX_CHARS {
            return false;
        }
        if self
            .text
            .chars()
            .any(|ch| ch == '-' || is_block_script(ch) || is_joined_script(ch))
        {
            return false;
        }
        let held = self.end_ms - self.start_ms;
        match length {
            1 => held >= GROW_MIN_SOLO_MS,
            2 | 3 => held >= GROW_MIN_SHORT_MS + (length as i64 - 2) * GROW_SHORT_STEP_MS,
            4 => held >= GROW_MIN_FOUR_MS,
            _ => held >= GROW_MIN_LONG_MS && held >= (length as i64) * GROW_MS_PER_CHAR,
        }
    }
}

/// Which side of the panel a line is sung from.
///
/// A duet is written in TTML as a `ttm:agent` per line, and Apple lays the two
/// voices out on opposite sides so a call-and-response reads as two people
/// rather than one long verse. [`LyricAlignment::Start`] is the default and the
/// only side a single-voice song ever uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LyricAlignment {
    #[default]
    Start,
    End,
}

/// One synced line. `time_ms` is when it starts; a blank `text` is an
/// instrumental stretch — LRC files mark those with a bare timestamp.
///
/// `words` is populated only by providers that carry word-level timing.
/// `sung_until_ms` is the line's own end where a line-synced provider states
/// one, which lets an interlude be told apart from a slowly sung line.
///
/// `background` is the answering vocal — the `"(ooh)"` or echoed half-phrase a
/// second voice sings over the lead. Never nested: a background line's own
/// `background` is always `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub time_ms: i64,
    pub text: String,
    #[serde(default)]
    pub words: Vec<LyricWord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sung_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Box<LyricLine>>,
    #[serde(default)]
    pub alignment: LyricAlignment,
    /// Display-only translation: drive its sweep and bloom from the original vocal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_source: Option<Box<LyricLine>>,
}

impl LyricLine {
    pub fn new(time_ms: i64, text: impl Into<String>) -> Self {
        Self {
            time_ms,
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn is_gap(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_word_synced(&self) -> bool {
        !self.words.is_empty()
    }

    /// Where each of `words` sits in `text`, as half-open Unicode scalar ranges
    /// (`start_char..end_char`).
    ///
    /// Walked from where the last one ended rather than searched from the start,
    /// so a word repeated in the line lines up with its own occurrence.
    pub fn word_spans(&self) -> Vec<Range<usize>> {
        let mut byte_offset = 0usize;
        let mut char_offset = 0usize;
        let mut spans = Vec::with_capacity(self.words.len());
        for word in &self.words {
            let word_chars = word.text.chars().count();
            let (start_char, next_byte) =
                if let Some(rel) = self.text[byte_offset..].find(word.text.as_str()) {
                    let start_byte = byte_offset + rel;
                    let gap_chars = self.text[byte_offset..start_byte].chars().count();
                    (char_offset + gap_chars, start_byte + word.text.len())
                } else {
                    (
                        char_offset,
                        advance_bytes_by_chars(&self.text, byte_offset, word_chars),
                    )
                };
            let end_char = start_char + word_chars;
            byte_offset = next_byte;
            char_offset = end_char;
            spans.push(start_char..end_char);
        }
        spans
    }

    /// Where each separately timed piece of the line sits in `text`: a word's
    /// syllables where the provider split it, the word itself where it did not,
    /// as half-open Unicode scalar ranges (`start_char..end_char`).
    pub fn sweep_spans(&self) -> Vec<Range<usize>> {
        let word_spans = self.word_spans();
        let mut spans = Vec::with_capacity(word_spans.len());
        for (word, span) in self.words.iter().zip(word_spans) {
            if word.syllables.is_empty() {
                spans.push(span);
            } else {
                for syllable in &word.syllables {
                    spans
                        .push((span.start + syllable.start_char)..(span.start + syllable.end_char));
                }
            }
        }
        spans
    }

    /// Whether anything on this line is off the floor at `position_ms`.
    pub fn is_lifted(&self, position_ms: i64) -> bool {
        let Some(first) = self.words.first() else {
            return false;
        };
        if position_ms <= first.start_ms {
            return false;
        }
        let Some(last) = self.words.last() else {
            return false;
        };
        position_ms < last.end_ms.saturating_add(RISE_MS_I64) || self.is_growing(position_ms)
    }

    /// Start of the stretch of the song over which anything drawn for this line
    /// moves — the sweep, the lift and its settle, a held word's letters.
    pub fn animates_from_ms(&self) -> i64 {
        self.words
            .first()
            .map_or(self.time_ms, |first| self.time_ms.min(first.start_ms))
    }

    /// End of the stretch of the song over which anything drawn for this line
    /// moves.
    pub fn animates_until_ms(&self) -> i64 {
        let end = self.end_ms();
        let settled = self
            .words
            .last()
            .map_or(end, |last| last.end_ms.saturating_add(RISE_MS_I64));
        let grown = self
            .words
            .iter()
            .filter(|word| word.can_grow())
            .map(word_rests_at_ms)
            .max()
            .unwrap_or(i64::MIN);
        end.max(settled).max(grown)
    }

    /// How far the word at `index` has lifted at `position_ms`, in `0.0..=1.0`.
    pub fn word_lift(&self, index: usize, position_ms: i64) -> f32 {
        let Some(word) = self.words.get(index) else {
            return 0.0;
        };
        let rise_ms = ((word.end_ms - word.start_ms) as f32).clamp(RISE_MIN_MS, RISE_MS);
        let rising = ((position_ms - word.start_ms) as f32 / rise_ms).clamp(0.0, 1.0);
        let falling = (1.0 - (position_ms - word.end_ms) as f32 / RISE_MS).clamp(0.0, 1.0);
        lift_height(word) * smooth(rising.min(falling))
    }

    /// How much of a word's lift is left at `position_ms` — its full
    /// `lift_height` while it is being sung, easing to `0.0` over `RISE_MS`
    /// once it is past.
    pub fn word_fall(&self, index: usize, position_ms: i64) -> f32 {
        let Some(word) = self.words.get(index) else {
            return 0.0;
        };
        let falling = (1.0 - (position_ms - word.end_ms) as f32 / RISE_MS).clamp(0.0, 1.0);
        lift_height(word) * smooth(falling)
    }

    /// Whether anything actually told us when the singing stops, rather than
    /// only when it starts. Word timings carry it, and so does a provider that
    /// stamps the line's own end (`sung_until_ms`).
    pub fn has_known_end(&self) -> bool {
        !self.words.is_empty() || self.sung_until_ms.is_some()
    }

    /// When the last word finishes — or the line's own end where the provider
    /// gave one, or `time_ms` when nothing did. Includes `background`.
    pub fn end_ms(&self) -> i64 {
        let lead = self
            .words
            .last()
            .map(|word| word.end_ms)
            .or(self.sung_until_ms)
            .unwrap_or(self.time_ms);
        match self.background.as_deref() {
            Some(bg) => lead.max(bg.end_ms()),
            None => lead,
        }
    }

    /// How far through the line the singing has got, as a fractional character
    /// index into `text`. The sweep reveals up to this character.
    pub fn revealed_chars(&self, position_ms: i64) -> f32 {
        let total_chars = self.text.chars().count();
        if let Some(source) = self.timing_source.as_deref() {
            let source_chars = source.text.chars().count();
            if source_chars == 0 {
                return 0.0;
            }
            return (source.revealed_chars(position_ms) / source_chars as f32).clamp(0.0, 1.0)
                * total_chars as f32;
        }
        if self.words.is_empty() {
            return if position_ms >= self.time_ms {
                total_chars as f32
            } else {
                0.0
            };
        }

        let mut byte_offset = 0usize;
        let mut char_offset = 0usize;
        for (index, word) in self.words.iter().enumerate() {
            let word_chars = word.text.chars().count();
            let (start_char, end_byte) =
                if let Some(rel) = self.text[byte_offset..].find(word.text.as_str()) {
                    let start_byte = byte_offset + rel;
                    let gap_chars = self.text[byte_offset..start_byte].chars().count();
                    (char_offset + gap_chars, start_byte + word.text.len())
                } else {
                    (
                        char_offset,
                        advance_bytes_by_chars(&self.text, byte_offset, word_chars),
                    )
                };
            let end_char = start_char + word_chars;

            if position_ms < word.start_ms {
                return start_char as f32;
            }
            if position_ms < word.end_ms {
                return start_char as f32 + word.chars_sung_at(position_ms);
            }
            // Past this word: the trailing space fills over the pause before
            // the next one, so the highlight keeps creeping instead of resting
            // on the word's last letter.
            if let Some(next) = self.words.get(index + 1)
                && position_ms < next.start_ms
            {
                let gap_start_char =
                    if let Some(rel) = self.text[end_byte..].find(next.text.as_str()) {
                        end_char + self.text[end_byte..end_byte + rel].chars().count()
                    } else {
                        end_char
                    };
                let pause = (next.start_ms - word.end_ms).max(1) as f32;
                let through = (position_ms - word.end_ms) as f32 / pause;
                return end_char as f32 + through * gap_start_char.saturating_sub(end_char) as f32;
            }
            byte_offset = end_byte;
            char_offset = end_char;
        }
        total_chars as f32
    }

    /// The words held long enough to be worth animating letter by letter.
    pub fn growing_words(&self) -> Vec<GrowingWord> {
        self.words
            .iter()
            .enumerate()
            .filter(|(_, word)| word.can_grow())
            .map(|(index, word)| GrowingWord::new(index, word))
            .collect()
    }

    /// The letter-by-letter treatment for word `index`, where it has earned one.
    pub fn growing_at(&self, index: usize) -> Option<GrowingWord> {
        let word = self.words.get(index)?;
        word.can_grow().then(|| GrowingWord::new(index, word))
    }

    /// Whether any word on this line is mid-flight at `position_ms`.
    pub fn is_growing(&self, position_ms: i64) -> bool {
        self.words.iter().any(|word| {
            word.can_grow() && position_ms >= word.start_ms && position_ms <= word_rests_at_ms(word)
        })
    }
}

/// One word held long enough to be animated a letter at a time.
///
/// Every letter runs the same three-beat move — swell up and forward, hold,
/// then settle back to the small lift every sung word carries — staggered
/// along the word so the movement travels rather than pulsing at once.
#[derive(Debug, Clone, PartialEq)]
pub struct GrowingWord {
    /// Which of the line's words this is.
    pub index: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    /// When the last letter has finished moving and is just sitting lifted.
    pub rests_at_ms: i64,
    scale_peak: Vec<f32>,
    shift_peak: Vec<f32>,
    rise_peak: Vec<f32>,
    bloom_peak: Vec<f32>,
}

impl GrowingWord {
    pub(crate) fn new(index: usize, word: &LyricWord) -> Self {
        let start_ms = word.start_ms;
        let end_ms = word.end_ms;
        let chars = word.text.chars().count();
        let held = (end_ms - start_ms).max(1) as f32;

        // How much of the full treatment this word has earned (cubed).
        let ramp =
            ((held - GROW_RAMP_MIN_MS) / (GROW_RAMP_MAX_MS - GROW_RAMP_MIN_MS)).clamp(0.0, 1.0);
        let earned = ramp * ramp * ramp;
        let decay = decay_rate(chars, held);
        let bloom_pace = GROW_BLOOM_PACE_MAX.min(held / GROW_BLOOM_PACE_MS);
        let bloom_spread = if chars <= 3 {
            GROW_BLOOM_SHORT
        } else if chars >= 6 {
            GROW_BLOOM_LONG
        } else {
            1.0
        };
        let base = if chars <= 3 {
            GROW_BASE_SHORT
        } else {
            GROW_BASE_LONG
        };
        let lift_pace = (held / GROW_LIFT_PACE_MS).clamp(GROW_LIFT_FLOOR, 1.0);

        let mut scale_peak = Vec::with_capacity(chars);
        let mut shift_peak = Vec::with_capacity(chars);
        let mut rise_peak = Vec::with_capacity(chars);
        let mut bloom_peak = Vec::with_capacity(chars);

        for i in 0..chars {
            let place = if chars > 1 {
                i as f32 / (chars - 1) as f32
            } else {
                0.0
            };
            let reach = earned * (1.0 - place * decay);
            let scale = 1.0 + base + reach * GROW_SCALE_RANGE;
            scale_peak.push(scale * GROW_SCALE_TRIM);
            bloom_peak
                .push((GROW_BLOOM_FLOOR + reach * GROW_BLOOM_RANGE) * bloom_pace * bloom_spread);
            rise_peak.push(((scale - 1.0) / GROW_SCALE_CEILING) * lift_pace);
            let centre = (i as f32 + 0.5) / chars as f32;
            shift_peak.push((centre - 0.5) * 2.0 * (scale - 1.0) * GROW_SHIFT_EM * GROW_SCALE_TRIM);
        }

        let rests_at_ms = word_rests_at_ms(word);
        Self {
            index,
            start_ms,
            end_ms,
            rests_at_ms,
            scale_peak,
            shift_peak,
            rise_peak,
            bloom_peak,
        }
    }

    /// Number of characters in this growing word.
    pub fn char_count(&self) -> usize {
        self.scale_peak.len()
    }

    /// Writes where letter `char_index` has got to at `position_ms` into `into`.
    ///
    /// Out-of-bounds `char_index` resets `into` to its neutral resting state
    /// without panicking.
    pub fn sample_into(&self, char_index: usize, position_ms: i64, into: &mut CharGrowth) {
        let (Some(&peak_scale), Some(&peak_shift), Some(&peak_rise), Some(&peak_bloom)) = (
            self.scale_peak.get(char_index),
            self.shift_peak.get(char_index),
            self.rise_peak.get(char_index),
            self.bloom_peak.get(char_index),
        ) else {
            *into = CharGrowth::default();
            return;
        };

        let span = (self.end_ms - self.start_ms).max(1) as f32;
        let elapsed =
            (position_ms - self.start_ms) as f32 - char_index as f32 * span * GROW_STAGGER;
        let phase = (elapsed / (span * GROW_SPAN)).clamp(0.0, 1.0);

        if phase < GROW_IN {
            let t = smooth(phase / GROW_IN);
            into.scale = 1.0 + (peak_scale - 1.0) * t;
            into.shift = peak_shift * t;
            into.rise = peak_rise * t;
            into.bloom = peak_bloom * t;
        } else if phase < GROW_HOLD {
            into.scale = peak_scale;
            into.shift = peak_shift;
            into.rise = peak_rise;
            into.bloom = peak_bloom;
        } else if phase < GROW_OUT {
            let t = smooth((phase - GROW_HOLD) / (GROW_OUT - GROW_HOLD));
            into.scale = peak_scale + (1.0 - peak_scale) * t;
            into.shift = peak_shift * (1.0 - t);
            into.rise = peak_rise + (GROW_REST - peak_rise) * t;
            into.bloom = peak_bloom * (1.0 - t);
        } else {
            into.scale = 1.0;
            into.shift = 0.0;
            into.rise = GROW_REST;
            into.bloom = 0.0;
        }
    }

    /// Convenience wrapper returning a sampled [`CharGrowth`] value directly.
    pub fn sample(&self, char_index: usize, position_ms: i64) -> CharGrowth {
        let mut growth = CharGrowth::default();
        self.sample_into(char_index, position_ms, &mut growth);
        growth
    }
}

/// Where one letter of a [`GrowingWord`] is, filled in by [`GrowingWord::sample_into`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharGrowth {
    /// Swell, about the letter's own centre. `1.0` is the letter as laid out.
    pub scale: f32,
    /// Lean away from the middle of the word, in ems.
    pub shift: f32,
    /// Lift, in multiples of the ordinary sung-word rise.
    pub rise: f32,
    /// Bloom, `0.0..=1.0`, at its own peak partway up rather than at the top.
    pub bloom: f32,
}

impl CharGrowth {
    pub const fn new() -> Self {
        Self {
            scale: 1.0,
            shift: 0.0,
            rise: 0.0,
            bloom: 0.0,
        }
    }
}

impl Default for CharGrowth {
    fn default() -> Self {
        Self::new()
    }
}

/// Slices `s` by Unicode scalar indices `start_char..end_char`, clamping
/// cleanly to valid UTF-8 boundaries and never panicking.
pub(crate) fn slice_chars(s: &str, start_char: usize, end_char: usize) -> &str {
    if start_char >= end_char {
        return "";
    }
    let mut start_byte = s.len();
    let mut end_byte = s.len();
    for (char_idx, (byte_idx, _)) in s.char_indices().enumerate() {
        if char_idx == start_char {
            start_byte = byte_idx;
        }
        if char_idx == end_char {
            end_byte = byte_idx;
            break;
        }
    }
    if start_byte >= end_byte {
        ""
    } else {
        &s[start_byte..end_byte]
    }
}

fn advance_bytes_by_chars(s: &str, byte_offset: usize, count: usize) -> usize {
    if count == 0 || byte_offset >= s.len() {
        return byte_offset.min(s.len());
    }
    let rest = &s[byte_offset..];
    match rest.char_indices().nth(count) {
        Some((rel, _)) => byte_offset + rel,
        None => s.len(),
    }
}

fn word_rests_at_ms(word: &LyricWord) -> i64 {
    let chars = word.text.chars().count();
    let held = (word.end_ms - word.start_ms).max(1) as f32;
    let last = chars.saturating_sub(1) as f32 * GROW_STAGGER + GROW_SPAN;
    word.start_ms + (held * last) as i64
}

fn lift_height(word: &LyricWord) -> f32 {
    let held = (word.end_ms - word.start_ms) as f32;
    let through = ((held - LIFT_QUICK_MS) / (LIFT_HELD_MS - LIFT_QUICK_MS)).clamp(0.0, 1.0);
    LIFT_FLOOR + (1.0 - LIFT_FLOOR) * smooth(through)
}

fn smooth(fraction: f32) -> f32 {
    fraction * fraction * (3.0 - 2.0 * fraction)
}

fn decay_rate(length: usize, held_ms: f32) -> f32 {
    let long = length > GROW_DECAY_LONG_CHARS;
    let quick = held_ms < GROW_DECAY_QUICK_MS;
    if !long && !quick {
        return 0.0;
    }
    let mut strength = 0.0_f32;
    if long {
        strength += (((length - GROW_DECAY_LONG_CHARS) as f32) / 5.0).min(1.0) * GROW_DECAY_LONG;
    }
    if quick {
        let short = (1.0 - (held_ms - GROW_DECAY_QUICK_FLOOR_MS) / 400.0).max(0.0);
        strength += short
            * if length > 3 {
                GROW_DECAY_QUICK
            } else {
                GROW_DECAY_QUICK_TINY
            };
    }
    strength.min(GROW_DECAY_MAX)
}

fn is_block_script(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
    )
}

fn is_joined_script(ch: char) -> bool {
    matches!(ch, '\u{0590}'..='\u{08FF}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_approx(expected: f32, actual: f32, eps: f32) {
        assert!(
            (expected - actual).abs() <= eps,
            "expected {expected} ± {eps}, got {actual}"
        );
    }

    fn held(text: &str, held_ms: i64) -> LyricLine {
        LyricLine {
            time_ms: 0,
            text: text.to_owned(),
            words: vec![LyricWord::new(0, held_ms, text)],
            ..LyricLine::default()
        }
    }

    #[test]
    fn reveal_runs_across_a_word_over_that_words_own_span() {
        let line = LyricLine {
            time_ms: 1_000,
            text: "one two".to_owned(),
            words: vec![
                LyricWord::new(1_000, 1_500, "one"),
                LyricWord::new(2_000, 2_400, "two"),
            ],
            ..LyricLine::default()
        };
        assert_approx(0.0, line.revealed_chars(500), 0.01);
        assert_approx(0.0, line.revealed_chars(1_000), 0.01);
        assert_approx(1.5, line.revealed_chars(1_250), 0.01);
        assert_approx(3.0, line.revealed_chars(1_500), 0.01);
        // Pause between words fills the space between them.
        assert_approx(3.5, line.revealed_chars(1_750), 0.01);
        assert_approx(4.0, line.revealed_chars(2_000), 0.01);
        // Whole line once the last word is done.
        assert_approx(7.0, line.revealed_chars(2_400), 0.01);
        assert_approx(7.0, line.revealed_chars(99_000), 0.01);
    }

    #[test]
    fn repeated_word_lines_up_with_its_own_occurrence() {
        let repeated = LyricLine {
            time_ms: 0,
            text: "go go go".to_owned(),
            words: vec![
                LyricWord::new(0, 100, "go"),
                LyricWord::new(1_000, 1_100, "go"),
                LyricWord::new(2_000, 2_100, "go"),
            ],
            ..LyricLine::default()
        };
        assert_approx(6.0, repeated.revealed_chars(2_000), 0.01);
    }

    #[test]
    fn multibyte_utf8_word_spans_and_reveal_count_characters_not_bytes() {
        let line = LyricLine {
            time_ms: 0,
            text: "café café".to_owned(),
            words: vec![
                LyricWord::new(0, 400, "café"),
                LyricWord::new(1_000, 1_400, "café"),
            ],
            ..LyricLine::default()
        };
        assert_eq!(vec![0..4, 5..9], line.word_spans());
        assert_approx(2.0, line.revealed_chars(200), 0.01);
        assert_approx(4.5, line.revealed_chars(700), 0.01);
        assert_approx(7.0, line.revealed_chars(1_200), 0.01);
    }

    #[test]
    fn a_note_carried_earns_letter_by_letter_treatment_and_patter_does_not() {
        assert_eq!(1, held("hold", 1_500).growing_words().len());
        assert!(held("hold", 300).growing_words().is_empty());
    }

    #[test]
    fn how_long_a_word_must_be_held_depends_on_how_long_it_is() {
        assert!(!held("ah", 1_500).growing_words().is_empty());
        assert!(held("holding", 900).growing_words().is_empty());
        assert!(!held("holding", 1_500).growing_words().is_empty());
        assert!(held("standing", 4_000).growing_words().is_empty());
    }

    #[test]
    fn words_that_do_not_come_apart_into_letters_are_left_alone() {
        assert!(held("一二三", 2_000).growing_words().is_empty());
        assert!(held("re-do", 2_000).growing_words().is_empty());
    }

    #[test]
    fn swell_travels_along_word_rather_than_pulsing_at_once() {
        let growing = held("golden", 2_000).growing_words();
        assert_eq!(1, growing.len());
        let word = &growing[0];
        assert!(word.sample(0, 500).scale > word.sample(5, 500).scale);
        assert_approx(1.0, word.sample(5, 500).scale, 0.001);
        assert!(word.sample(5, 2_600).scale > word.sample(0, 2_600).scale);
    }

    #[test]
    fn every_letter_comes_to_rest_at_ordinary_sung_word_lift() {
        let word = held("golden", 2_000)
            .growing_at(0)
            .expect("golden held 2s grows");
        let settled = word.sample(0, word.rests_at_ms);
        assert_approx(1.0, settled.scale, 0.001);
        assert_approx(0.0, settled.shift, 0.001);
        assert_approx(1.0, settled.rise, 0.001);
        assert_approx(0.0, settled.bloom, 0.001);
    }

    #[test]
    fn bloom_peaks_partway_up_and_is_gone_before_settling() {
        let word = held("golden", 2_000)
            .growing_at(0)
            .expect("golden held 2s grows");
        assert_approx(0.0, word.sample(0, 0).bloom, 0.001);
        assert!(word.sample(0, 800).bloom > 0.3);
        assert_approx(0.0, word.sample(0, 2_400).bloom, 0.001);
    }

    #[test]
    fn letters_lean_away_from_middle_of_word_as_it_swells() {
        let word = held("golden", 2_000)
            .growing_at(0)
            .expect("golden held 2s grows");
        assert!(word.sample(0, 800).shift < 0.0);
        assert!(word.sample(5, 2_300).shift > 0.0);
    }

    #[test]
    fn ordinary_syllables_and_plain_lines_do_not_grow_and_plain_reveals_whole() {
        let patter = LyricLine {
            time_ms: 0,
            text: "one two three".to_owned(),
            words: vec![
                LyricWord::new(0, 200, "one"),
                LyricWord::new(200, 400, "two"),
                LyricWord::new(400, 700, "three"),
            ],
            ..LyricLine::default()
        };
        assert!(patter.growing_words().is_empty());
        assert!(!patter.is_growing(300));

        let plain = LyricLine::new(1_000, "no timings here");
        assert!(plain.growing_words().is_empty());
        assert!(!plain.is_growing(500));
        assert_approx(0.0, plain.revealed_chars(999), 0.01);
        assert_approx(15.0, plain.revealed_chars(1_000), 0.01);
    }

    #[test]
    fn pause_between_syllables_rests_on_boundary() {
        let word = LyricWord::with_syllables(
            1_000,
            2_000,
            "hello",
            vec![
                LyricSyllable::new(1_000, 1_200, 0, 3),
                LyricSyllable::new(1_600, 2_000, 3, 5),
            ],
        );
        assert_approx(3.0, word.chars_sung_at(1_400), 0.01);
        assert_approx(4.0, word.chars_sung_at(1_800), 0.01);
    }

    #[test]
    fn stripping_brackets_moves_syllables_with_letters() {
        let word = LyricWord::with_syllables(
            0,
            600,
            "(enough)",
            vec![
                LyricSyllable::new(0, 100, 0, 2),
                LyricSyllable::new(100, 600, 2, 8),
            ],
        );
        let stripped = word
            .without_chars(|ch| ch == '(' || ch == ')')
            .expect("non-empty word after stripping brackets");
        assert_eq!("enough", stripped.text);
        assert_eq!(
            vec![
                LyricSyllable::new(0, 100, 0, 1),
                LyricSyllable::new(100, 600, 1, 6),
            ],
            stripped.syllables
        );
        assert!(
            LyricWord::new(0, 1, "()")
                .without_chars(|ch| ch == '(' || ch == ')')
                .is_none()
        );

        // Multibyte UTF-8 characters also move cleanly.
        let utf8 = LyricWord::with_syllables(
            0,
            600,
            "(café)",
            vec![
                LyricSyllable::new(0, 200, 0, 4),
                LyricSyllable::new(200, 600, 4, 6),
            ],
        );
        let stripped_utf8 = utf8
            .without_chars(|ch| ch == '(' || ch == ')')
            .expect("non-empty utf8 word");
        assert_eq!("café", stripped_utf8.text);
        assert_eq!(
            vec![
                LyricSyllable::new(0, 200, 0, 3),
                LyricSyllable::new(200, 600, 3, 4),
            ],
            stripped_utf8.syllables
        );
    }

    #[test]
    fn quick_word_barely_lifts_and_held_word_goes_all_the_way_up() {
        let line = LyricLine {
            time_ms: 1_000,
            text: "a hold".to_owned(),
            words: vec![
                LyricWord::new(1_000, 1_100, "a"),
                LyricWord::new(2_000, 3_200, "hold"),
            ],
            ..LyricLine::default()
        };
        let quick_peak = (1_000_i64..=2_000_i64)
            .step_by(10)
            .map(|t| line.word_lift(0, t))
            .fold(0.0_f32, f32::max);
        let held_peak = (2_000_i64..=4_000_i64)
            .step_by(10)
            .map(|t| line.word_lift(1, t))
            .fold(0.0_f32, f32::max);
        assert!(quick_peak < 0.3, "quick word rose to {quick_peak}");
        assert_approx(1.0, held_peak, 0.01);
        assert_approx(line.word_fall(1, 3_400), line.word_lift(1, 3_400), 0.001);
    }

    #[test]
    fn animation_window_bounds_all_movement() {
        let lines = vec![
            LyricLine {
                time_ms: 10_000,
                text: "long enough".to_owned(),
                words: vec![
                    LyricWord::new(10_000, 10_400, "long"),
                    LyricWord::with_syllables(
                        10_400,
                        11_000,
                        "enough",
                        vec![
                            LyricSyllable::new(10_400, 10_550, 0, 1),
                            LyricSyllable::new(10_550, 11_000, 1, 6),
                        ],
                    ),
                ],
                ..LyricLine::default()
            },
            LyricLine {
                time_ms: 20_000,
                text: "oh hold".to_owned(),
                words: vec![
                    LyricWord::new(20_000, 20_200, "oh"),
                    LyricWord::new(20_200, 22_600, "hold"),
                ],
                ..LyricLine::default()
            },
            LyricLine {
                time_ms: 29_800,
                text: "call me".to_owned(),
                words: vec![
                    LyricWord::new(30_000, 30_300, "call"),
                    LyricWord::new(30_300, 30_700, "me"),
                ],
                background: Some(Box::new(LyricLine {
                    time_ms: 30_500,
                    text: "back".to_owned(),
                    words: vec![LyricWord::new(30_500, 32_000, "back")],
                    ..LyricLine::default()
                })),
                ..LyricLine::default()
            },
        ];

        let picture = |line: &LyricLine, pos: i64| {
            let lifts: Vec<(u32, u32)> = (0..line.words.len())
                .map(|i| {
                    (
                        line.word_lift(i, pos).to_bits(),
                        line.word_fall(i, pos).to_bits(),
                    )
                })
                .collect();
            (
                pos >= line.end_ms(),
                pos <= line.time_ms,
                line.revealed_chars(pos).to_bits(),
                line.is_lifted(pos),
                line.is_growing(pos),
                lifts,
            )
        };

        for line in &lines {
            let from = line.animates_from_ms();
            let before = picture(line, from - 1);
            for at in [0, from - 5_000, from - 2] {
                assert_eq!(before, picture(line, at));
            }
            assert!(!line.is_lifted(from - 1));

            let until = line.animates_until_ms();
            let after = picture(line, until + 1);
            for at in [until + 2, until + 5_000, 600_000] {
                assert_eq!(after, picture(line, at));
            }
            assert!(!line.is_lifted(until + 1));
            assert!(!line.is_growing(until + 1));
        }

        let held_line = &lines[1];
        let max_rest = held_line
            .growing_words()
            .iter()
            .map(|w| w.rests_at_ms)
            .max()
            .expect("held word grows");
        assert!(held_line.animates_until_ms() >= max_rest);

        let with_backing = &lines[2];
        assert!(
            with_backing.animates_until_ms()
                >= with_backing
                    .background
                    .as_deref()
                    .expect("background present")
                    .end_ms()
        );
        assert_eq!(with_backing.time_ms, with_backing.animates_from_ms());
    }
}
