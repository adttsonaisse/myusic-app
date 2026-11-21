use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, RwLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::models::ArtistRef;

const JOINER_WORD_SIGHTINGS: usize = 2;

/// Punctuation marks a byline joins credits with across scripts (comma, full-width comma,
/// ampersands, bullet, middle dots, ideographic comma).
static JOINER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*,\s*|\s*，\s*|\s+&\s+|\s+＆\s+|\s*•\s*|\s*·\s*|\s*・\s*|\s*、\s*")
        .expect("valid static joiner regex")
});

/// Folds an artist name to its canonical exact-match form: NFD decomposition,
/// combining diacritical marks stripped (U+0300..=U+036F), Unicode lowercase,
/// collapsed whitespace, trimmed. Preserves all scripts and punctuation.
pub fn normalized_artist_name(raw: &str) -> String {
    let stripped: String = raw
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect();
    let lower = stripped.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_ws = false;
    for ch in lower.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Returns true iff `answer` matches `wanted` exactly after [`normalized_artist_name`] folding.
/// Blank `wanted` is always refused.
pub fn is_exact_artist_match(answer: &str, wanted: &str) -> bool {
    let name = normalized_artist_name(wanted);
    if name.is_empty() {
        return false;
    }
    normalized_artist_name(answer) == name
}

/// One name in a credit line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistCredit {
    pub name: String,
    #[serde(default)]
    pub browse_id: Option<String>,
}

impl ArtistCredit {
    pub fn new(name: impl Into<String>, browse_id: Option<String>) -> Self {
        Self {
            name: name.into(),
            browse_id,
        }
    }
}

/// One stretch of a credit line: either a tappable credit or plain separator text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditSegment {
    pub text: String,
    #[serde(default)]
    pub credit: Option<ArtistCredit>,
}

/// Splits `text` on universal credit joiner punctuation marks.
pub fn split_credit_joiners(text: &str) -> Vec<&str> {
    JOINER.split(text).collect()
}

/// Cuts `text` into stretches, marking each credited artist while preserving the line's
/// original spelling and punctuation character for character. Longest name wins at any position.
pub fn credit_segments(text: &str, credits: &[ArtistCredit]) -> Vec<CreditSegment> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut names: Vec<&ArtistCredit> = credits
        .iter()
        .filter(|c| !c.name.trim().is_empty())
        .filter(|c| seen.insert(normalized_artist_name(&c.name)))
        .collect();
    // Match Kotlin's `sortedByDescending { it.name.length }` (UTF-16 code-unit length).
    names.sort_by(|a, b| {
        b.name
            .encode_utf16()
            .count()
            .cmp(&a.name.encode_utf16().count())
    });

    if names.is_empty() {
        return vec![CreditSegment {
            text: text.to_owned(),
            credit: None,
        }];
    }

    let mut out = Vec::new();
    let mut plain = String::new();
    let mut byte_idx = 0;

    while byte_idx < text.len() {
        let rest = &text[byte_idx..];
        let matched = names.iter().find_map(|credit| {
            match_prefix_ignore_case(rest, &credit.name).map(|len| (*credit, len))
        });

        match matched {
            None => {
                // INFALLIBLE: `byte_idx < text.len()` at a char boundary.
                let ch = rest.chars().next().unwrap_or_default();
                plain.push(ch);
                byte_idx += ch.len_utf8();
            }
            Some((credit, matched_bytes)) => {
                if !plain.is_empty() {
                    out.push(CreditSegment {
                        text: std::mem::take(&mut plain),
                        credit: None,
                    });
                }
                out.push(CreditSegment {
                    text: rest[..matched_bytes].to_owned(),
                    credit: Some(credit.clone()),
                });
                byte_idx += matched_bytes;
            }
        }
    }

    if !plain.is_empty() {
        out.push(CreditSegment {
            text: plain,
            credit: None,
        });
    }

    out
}

/// Checks whether `haystack` starts with `needle` ignoring case (per-char Unicode lowercase),
/// returning the exact byte length consumed in `haystack`.
fn match_prefix_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let mut h_chars = haystack.char_indices();
    let mut n_chars = needle.chars();
    let mut end_byte = 0usize;

    for nc in &mut n_chars {
        let (idx, hc) = h_chars.next()?;
        if !hc.to_lowercase().eq(nc.to_lowercase()) {
            return None;
        }
        end_byte = idx + hc.len_utf8();
    }
    Some(end_byte)
}

fn eq_ignore_case_unicode(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

#[derive(Default)]
struct IndexInner {
    channels: HashMap<String, HashSet<String>>,
    joiners: HashSet<String>,
    joiner_texts: HashSet<String>,
    joiner_sightings: HashMap<String, usize>,
}

/// Thread-safe index of artist channel ids and locale joiners learned from linked rows.
#[derive(Default)]
pub struct ArtistNameIndex {
    inner: RwLock<IndexInner>,
}

impl ArtistNameIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes every linked artist under its normalized name.
    pub fn record(&self, refs: &[ArtistRef]) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        for r in refs {
            let name = normalized_artist_name(&r.name);
            let Some(id) = r.browse_id.as_deref().filter(|id| !id.trim().is_empty()) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            inner
                .channels
                .entry(name)
                .or_default()
                .insert(id.to_owned());
        }
    }

    /// Notes separator text appearing between two credited artists.
    /// Punctuation marks are trusted on sight; single words without spaces/digits require 2 sightings.
    pub fn record_joiners(&self, segments: &[CreditSegment]) {
        let credit_positions: Vec<usize> = segments
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.credit.as_ref().map(|_| i))
            .collect();
        if credit_positions.len() < 2 {
            return;
        }
        let first = credit_positions[0];
        let last = credit_positions[credit_positions.len() - 1];

        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        for segment in &segments[(first + 1)..last] {
            if segment.credit.is_some() {
                continue;
            }
            let key = normalized_artist_name(&segment.text);
            if key.is_empty() {
                continue;
            }
            if segment.text.chars().any(char::is_alphanumeric) {
                if key.chars().any(char::is_whitespace) || key.chars().any(char::is_numeric) {
                    continue;
                }
                let seen = inner.joiner_sightings.entry(key.clone()).or_insert(0);
                *seen += 1;
                if *seen < JOINER_WORD_SIGHTINGS {
                    continue;
                }
            }
            inner.joiners.insert(key);
            inner.joiner_texts.insert(segment.text.trim().to_owned());
        }
    }

    /// Returns the unambiguous channel id recorded for `name`, or `None` if unknown or ambiguous.
    pub fn lookup(&self, name: &str) -> Option<String> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let ids = inner.channels.get(&normalized_artist_name(name))?;
        if ids.len() == 1 {
            ids.iter().next().cloned()
        } else {
            None
        }
    }

    /// Whether `text` has been learned as a credit joiner.
    pub fn is_joiner(&self, text: &str) -> bool {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.joiners.contains(&normalized_artist_name(text))
    }

    /// Raw trimmed texts of learned joiners for splitting unlinked stretches.
    pub fn joiner_texts(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut texts: Vec<String> = inner.joiner_texts.iter().cloned().collect();
        texts.sort();
        texts
    }

    /// Clears all learned channels and joiners.
    pub fn forget(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.channels.clear();
        inner.joiners.clear();
        inner.joiner_texts.clear();
        inner.joiner_sightings.clear();
    }
}

/// Completes a byline's credits from previously learned channels in `index`, preserving line order.
pub fn complete_artist_credits(
    index: &ArtistNameIndex,
    line: &str,
    stated: &[ArtistRef],
) -> Vec<ArtistRef> {
    if line.trim().is_empty() || stated.is_empty() {
        return stated.to_vec();
    }
    let named: Vec<&ArtistRef> = stated
        .iter()
        .filter(|r| !r.name.trim().is_empty())
        .collect();
    let credit_inputs: Vec<ArtistCredit> = named
        .iter()
        .map(|r| ArtistCredit::new(r.name.clone(), r.browse_id.clone()))
        .collect();
    let segments = credit_segments(line, &credit_inputs);
    index.record_joiners(&segments);

    let mut used_ids: HashSet<String> = named.iter().filter_map(|r| r.browse_id.clone()).collect();
    let learned_words: Vec<String> = index
        .joiner_texts()
        .into_iter()
        .filter(|w| !w.trim().is_empty())
        .collect();

    let mut out = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();

    for segment in &segments {
        if let Some(credit) = &segment.credit {
            let resolved = named
                .iter()
                .find(|r| eq_ignore_case_unicode(&r.name, &credit.name))
                .map(|r| (*r).clone())
                .unwrap_or_else(|| ArtistRef::new(credit.name.clone(), credit.browse_id.clone()));
            if placed.insert(normalized_artist_name(&resolved.name)) {
                out.push(resolved);
            }
            continue;
        }
        for fragment in fragments_of(&segment.text, &learned_words) {
            if index.is_joiner(&fragment) {
                continue;
            }
            let Some(id) = index.lookup(&fragment) else {
                continue;
            };
            if !used_ids.insert(id.clone()) {
                continue;
            }
            if placed.insert(normalized_artist_name(&fragment)) {
                out.push(ArtistRef::new(fragment, Some(id)));
            }
        }
    }

    for r in named {
        if placed.insert(normalized_artist_name(&r.name)) {
            out.push(r.clone());
        }
    }
    out
}

/// Splits an unlinked stretch on universal punctuation joiners plus standalone whitespace-bounded
/// learned joiner words (equivalent to Kotlin's `fragmentCutter`, without lookaround regexes).
fn fragments_of(stretch: &str, learned_words: &[String]) -> Vec<String> {
    let mut sorted_words: Vec<&str> = learned_words.iter().map(String::as_str).collect();
    sorted_words.sort_by_key(|w| std::cmp::Reverse(w.len()));

    let mut out = Vec::new();
    for piece in JOINER.split(stretch) {
        for sub in split_on_standalone_words(piece, &sorted_words) {
            let trimmed = sub.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_owned());
            }
        }
    }
    out
}

fn split_on_standalone_words<'a>(piece: &'a str, words: &[&str]) -> Vec<&'a str> {
    if words.is_empty() || piece.is_empty() {
        return vec![piece];
    }

    let mut parts = Vec::new();
    let mut seg_start = 0usize;
    let mut idx = 0usize;

    while idx < piece.len() {
        let prev_is_boundary = idx == 0
            || piece[..idx]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);

        if prev_is_boundary {
            let rest = &piece[idx..];
            let mut matched_span = None;
            for &word in words {
                if let Some(word_bytes) = match_prefix_ignore_case(rest, word) {
                    let after = &rest[word_bytes..];
                    let next_is_boundary =
                        after.is_empty() || after.chars().next().is_some_and(char::is_whitespace);
                    if next_is_boundary {
                        matched_span = Some(word_bytes);
                        break;
                    }
                }
            }
            if let Some(word_bytes) = matched_span {
                parts.push(&piece[seg_start..idx]);
                idx += word_bytes;
                seg_start = idx;
                continue;
            }
        }

        // INFALLIBLE: `idx < piece.len()` at a char boundary.
        let ch = piece[idx..].chars().next().unwrap_or_default();
        idx += ch.len_utf8();
    }

    parts.push(&piece[seg_start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stated(name: &str, id: &str) -> ArtistRef {
        ArtistRef::new(name, Some(id.to_owned()))
    }

    // ---- ArtistNameMatchTest replication ----

    #[test]
    fn exact_match_handles_case_spacing_accents_scripts_and_punctuation() {
        assert!(is_exact_artist_match("Monday Waxie", "Monday Waxie"));
        assert!(is_exact_artist_match("mata", "Mata"));
        assert!(is_exact_artist_match("MONDAY WAXIE", "Monday Waxie"));
        assert!(is_exact_artist_match("  Monday Waxie  ", "Monday Waxie"));
        assert!(is_exact_artist_match("Monday  Waxie", "Monday Waxie"));
        assert!(is_exact_artist_match("Jay   Z", "Jay Z"));
        assert!(is_exact_artist_match("Beyonce", "Beyoncé"));
        assert!(is_exact_artist_match("Björk", "Bjork"));
        assert!(is_exact_artist_match("Mötley Crüe", "Motley Crue"));

        // Punctuation is preserved (not folded).
        assert!(!is_exact_artist_match("Jay-Z", "Jay Z"));
        assert!(!is_exact_artist_match("Mata.", "Mata"));

        // Fragments & longer names never match.
        for other in ["White 2115", "Bedoes 2115", "Blacha 2115", "Kuqe 2115"] {
            assert!(!is_exact_artist_match(other, "2115"));
        }
        assert!(!is_exact_artist_match("Mate", "Mata"));
        assert!(!is_exact_artist_match("vkie fan", "vkie"));

        // CJK, Cyrillic, Devanagari, Greek remain distinct.
        assert!(!is_exact_artist_match("美嘉", "中島美嘉"));
        assert!(is_exact_artist_match("中島美嘉", "中島美嘉"));
        assert!(!is_exact_artist_match("Мата", "Мета"));
        assert!(!is_exact_artist_match("माता", "मेता"));
        assert!(!is_exact_artist_match("Μάτα", "Μέτα"));
        assert!(is_exact_artist_match("Мата", "Мата"));

        // Blanks are always refused.
        assert_eq!("", normalized_artist_name("  "));
        assert!(!is_exact_artist_match("", ""));
        assert!(!is_exact_artist_match("   ", ""));
        assert!(!is_exact_artist_match("Monday Waxie", "  "));
        assert!(!is_exact_artist_match("  ", "Monday Waxie"));
    }

    // ---- ArtistCreditsTest (creditSegments) replication ----

    #[test]
    fn credit_segments_preserves_line_and_longest_name_wins() {
        let credits = vec![
            ArtistCredit::new("2115", Some("UCeL9Nc_StEiSMV9HVYhkMMA".to_owned())),
            ArtistCredit::new("Bedoes 2115", Some("UCLLUIzfV9aOdnZN9ViIfSDQ".to_owned())),
            ArtistCredit::new("White 2115", Some("UC8Yuo0-S4BMCXWzYajCbMug".to_owned())),
        ];
        let segments = credit_segments("2115, Bedoes 2115, White 2115", &credits);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(vec!["2115", ", ", "Bedoes 2115", ", ", "White 2115"], texts);

        let ids: Vec<&str> = segments
            .iter()
            .filter_map(|s| s.credit.as_ref()?.browse_id.as_deref())
            .collect();
        assert_eq!(
            vec![
                "UCeL9Nc_StEiSMV9HVYhkMMA",
                "UCLLUIzfV9aOdnZN9ViIfSDQ",
                "UC8Yuo0-S4BMCXWzYajCbMug"
            ],
            ids
        );

        // Case-insensitive match draws the line's own spelling.
        let spelled = credit_segments(
            "Bedeosa, 2115",
            &[
                ArtistCredit::new("BedeoSa", Some("UC_a".to_owned())),
                ArtistCredit::new("2115", Some("UC_b".to_owned())),
            ],
        );
        let rebuilt: String = spelled.iter().map(|s| s.text.as_str()).collect();
        assert_eq!("Bedeosa, 2115", rebuilt);

        // Empty and uncredited lines.
        assert!(credit_segments("", &credits).is_empty());
        let plain = credit_segments("BedeoSa, 2115", &[]);
        assert_eq!(1, plain.len());
        assert!(plain[0].credit.is_none());
    }

    // ---- ArtistNameIndexTest + JoinerLanguageTest replication ----

    #[test]
    fn recorded_channel_fills_unlinked_artist_and_rejects_ambiguous() {
        let index = ArtistNameIndex::new();
        index.record(&[stated("White 2115", "UC_white")]);
        let completed = complete_artist_credits(
            &index,
            "CBW, White 2115, slowez",
            &[stated("CBW", "UC_cbw"), stated("slowez", "UC_slowez")],
        );
        assert_eq!(
            vec!["CBW", "White 2115", "slowez"],
            completed
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            Some("UC_white"),
            completed
                .iter()
                .find(|r| r.name == "White 2115")
                .and_then(|r| r.browse_id.as_deref())
        );

        // Two channels under one name => ambiguous => None.
        index.record(&[stated("Mate", "UC_one"), stated("Mate", "UC_two")]);
        assert_eq!(None, index.lookup("Mate"));
        let completed2 =
            complete_artist_credits(&index, "Bedoes & Mate", &[stated("Bedoes", "UC_b")]);
        assert_eq!(1, completed2.len());
    }

    #[test]
    fn word_joiner_in_eight_languages_learned_on_second_sighting() {
        for word in ["and", "und", "et", "y", "e", "och", "i", "a"] {
            let index = ArtistNameIndex::new();
            let line = format!("Alpha {word} Beta");
            let both = vec![stated("Alpha", "UC_a"), stated("Beta", "UC_b")];
            index.record(&both);

            // First sighting does not confirm a word joiner.
            complete_artist_credits(&index, &line, &both);
            assert!(!index.is_joiner(word), "`{word}` learned too early");

            // Second sighting confirms it.
            complete_artist_credits(&index, &line, &both);
            assert!(
                index.is_joiner(word),
                "`{word}` not learned on 2nd sighting"
            );

            // Must not slice inside `Beta` or `Alpha` even for `et` or `a`!
            let completed = complete_artist_credits(&index, &line, &[stated("Alpha", "UC_a")]);
            assert_eq!(
                vec!["Alpha", "Beta"],
                completed
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>(),
                "word joiner `{word}` failed to recover Beta cleanly"
            );
            assert_eq!(Some("UC_b"), completed[1].browse_id.as_deref());
        }
    }

    #[test]
    fn cjk_name_and_mark_joiner_distinguished_by_repetition() {
        let index = ArtistNameIndex::new();
        index.record(&[
            stated("宇多田ヒカル", "UC_uta"),
            stated("中島美嘉", "UC_naka"),
        ]);
        let completed = complete_artist_credits(
            &index,
            "宇多田ヒカル・YOASOBI・中島美嘉",
            &[stated("宇多田ヒカル", "UC_uta"), stated("YOASOBI", "UC_y")],
        );
        assert!(!index.is_joiner("中島美嘉"));
        assert!(index.is_joiner("・"));
        assert_eq!(
            Some("UC_naka"),
            completed
                .iter()
                .find(|r| r.name == "中島美嘉")
                .and_then(|r| r.browse_id.as_deref())
        );

        // Phrase with whitespace is never learned as a joiner.
        let phrase_idx = ArtistNameIndex::new();
        for _ in 0..5 {
            complete_artist_credits(
                &phrase_idx,
                "A and Zed",
                &[stated("A", "UC_a"), stated("Zed", "UC_z")],
            );
        }
        assert!(!phrase_idx.is_joiner("and Zed"));
    }
}
