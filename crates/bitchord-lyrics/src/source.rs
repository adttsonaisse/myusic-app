use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The lyrics databases BitChord can query, in default priority order.
///
/// Declaration order is the out-of-the-box priority. Persisted by wire name
/// rather than position so the enum can be rearranged without disturbing a
/// user's saved ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LyricsSource {
    LrcRed,
    BiniLyrics,
    BetterLyrics,
    BetterLyricsPortato,
    #[serde(rename = "PAXSENIX")]
    PaxSenix,
    #[serde(rename = "PAXSENIX_SPOTIFY")]
    PaxSenixSpotify,
    #[serde(rename = "PAXSENIX_MUSIXMATCH")]
    PaxSenixMusixmatch,
    LyricsPlus,
    SimpMusic,
    Unison,
    YoutubeTranscript,
    YoutubeMusic,
    Megalobiz,
    #[serde(rename = "KUGOU")]
    KuGou,
    #[serde(rename = "LRCLIB")]
    LrcLib,
    Musixmatch,
    Genius,
}

impl LyricsSource {
    /// Every defined source, in declaration (default priority) order.
    pub const ALL: &'static [Self] = &[
        Self::LrcRed,
        Self::BiniLyrics,
        Self::BetterLyrics,
        Self::BetterLyricsPortato,
        Self::PaxSenix,
        Self::PaxSenixSpotify,
        Self::PaxSenixMusixmatch,
        Self::LyricsPlus,
        Self::SimpMusic,
        Self::Unison,
        Self::YoutubeTranscript,
        Self::YoutubeMusic,
        Self::Megalobiz,
        Self::KuGou,
        Self::LrcLib,
        Self::Musixmatch,
        Self::Genius,
    ];

    /// Every source a listener can see and choose, in default priority order.
    pub const OFFERED: &'static [Self] = &[
        Self::LrcRed,
        Self::BiniLyrics,
        Self::BetterLyrics,
        Self::BetterLyricsPortato,
        Self::PaxSenix,
        Self::PaxSenixSpotify,
        Self::PaxSenixMusixmatch,
        Self::LyricsPlus,
        Self::Unison,
        Self::YoutubeTranscript,
        Self::YoutubeMusic,
        Self::KuGou,
        Self::LrcLib,
        Self::Musixmatch,
        Self::Genius,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::LrcRed => "lrc.red",
            Self::BiniLyrics => "BiniLyrics",
            Self::BetterLyrics => "BetterLyrics",
            Self::BetterLyricsPortato => "BetterLyrics Portato",
            Self::PaxSenix => "PaxSenix",
            Self::PaxSenixSpotify => "PaxSenix: Spotify",
            Self::PaxSenixMusixmatch => "PaxSenix: Musixmatch",
            Self::LyricsPlus => "LyricsPlus",
            Self::SimpMusic => "SimpMusic",
            Self::Unison => "Unison",
            Self::YoutubeTranscript => "YouTube captions",
            Self::YoutubeMusic => "YouTube Music",
            Self::Megalobiz => "Megalobiz",
            Self::KuGou => "KuGou",
            Self::LrcLib => "LRCLIB",
            Self::Musixmatch => "Musixmatch",
            Self::Genius => "Genius",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::LrcRed => "Apple timings by recording, syllable by syllable",
            Self::BiniLyrics => "The same Apple timings, matched on the recording itself",
            Self::BetterLyrics => "Apple Music timings, word by word",
            Self::BetterLyricsPortato => "QQ Music karaoke timings through BetterLyrics",
            Self::PaxSenix => "Apple Music timings through the original keyless provider",
            Self::PaxSenixSpotify => "Spotify lyrics with PaxSenix fallback; API key required",
            Self::PaxSenixMusixmatch => {
                "Musixmatch timings with PaxSenix fallback; API key required"
            }
            Self::LyricsPlus => "Syllable by syllable, on community mirrors",
            Self::SimpMusic => "Matched on the video, so never the wrong edit",
            Self::Unison => "Contributed by listeners, so it has what nobody licensed",
            Self::YoutubeTranscript => "Timed captions matched to the exact playing video",
            Self::YoutubeMusic => "Plain lyrics from the playing video's Lyrics tab",
            Self::Megalobiz => "Community-made, whole-line LRC",
            Self::KuGou => "Whole lines, strong outside the English catalogue",
            Self::LrcLib => "Whole lines only, and always up",
            Self::Musixmatch => "Whole lines, from the biggest lyrics database there is",
            Self::Genius => "Plain text fallback, massive web catalogue",
        }
    }

    /// Whether this source can return per-word timings, or only whole lines.
    pub const fn word_synced(self) -> bool {
        matches!(
            self,
            Self::LrcRed
                | Self::BiniLyrics
                | Self::BetterLyrics
                | Self::BetterLyricsPortato
                | Self::PaxSenix
                | Self::PaxSenixMusixmatch
                | Self::LyricsPlus
                | Self::SimpMusic
                | Self::Unison
        )
    }

    /// Kept in code but off the list: not shown in Settings or the provider
    /// picker, and never queried even if present in a user's saved choices.
    pub const fn hidden(self) -> bool {
        matches!(self, Self::SimpMusic | Self::Megalobiz)
    }

    /// Canonical persisted name matching Kotlin's `LyricsSource.name`.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::LrcRed => "LRC_RED",
            Self::BiniLyrics => "BINI_LYRICS",
            Self::BetterLyrics => "BETTER_LYRICS",
            Self::BetterLyricsPortato => "BETTER_LYRICS_PORTATO",
            Self::PaxSenix => "PAXSENIX",
            Self::PaxSenixSpotify => "PAXSENIX_SPOTIFY",
            Self::PaxSenixMusixmatch => "PAXSENIX_MUSIXMATCH",
            Self::LyricsPlus => "LYRICS_PLUS",
            Self::SimpMusic => "SIMP_MUSIC",
            Self::Unison => "UNISON",
            Self::YoutubeTranscript => "YOUTUBE_TRANSCRIPT",
            Self::YoutubeMusic => "YOUTUBE_MUSIC",
            Self::Megalobiz => "MEGALOBIZ",
            Self::KuGou => "KUGOU",
            Self::LrcLib => "LRCLIB",
            Self::Musixmatch => "MUSIXMATCH",
            Self::Genius => "GENIUS",
        }
    }

    /// Parses a canonical persisted source name.
    pub fn from_wire_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.wire_name() == name)
    }

    /// Brings a saved source order up to date with this build: hidden sources
    /// are dropped, duplicates are removed, and any source added since it was
    /// saved is slotted in right after the closest preceding source already in
    /// the saved list — or at index `0` if nothing ahead of it is present.
    pub fn ordered(saved: &[Self]) -> Vec<Self> {
        let mut result = Vec::with_capacity(Self::OFFERED.len());
        for &source in saved {
            if !source.hidden() && !result.contains(&source) {
                result.push(source);
            }
        }
        for (index, &source) in Self::OFFERED.iter().enumerate() {
            if result.contains(&source) {
                continue;
            }
            let insert_at = Self::OFFERED[..index]
                .iter()
                .rev()
                .find_map(|before| result.iter().position(|x| x == before).map(|pos| pos + 1))
                .unwrap_or(0);
            result.insert(insert_at, source);
        }
        result
    }
}

/// Cache/invalidation key describing what one lyrics lookup for the playing
/// track was made under.
///
/// Reordering sources, toggling a source on or off, or switching the syllable
/// priority produces a different key and triggers a new lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LyricsLookupKey {
    pub video_id: String,
    pub sources: BTreeSet<LyricsSource>,
    pub order: Vec<LyricsSource>,
    pub prioritize_syllable_sync: bool,
}

pub fn lyrics_lookup_key(
    video_id: impl Into<String>,
    sources: impl IntoIterator<Item = LyricsSource>,
    order: Vec<LyricsSource>,
    prioritize_syllable_sync: bool,
) -> LyricsLookupKey {
    LyricsLookupKey {
        video_id: video_id.into(),
        sources: sources.into_iter().collect(),
        order,
        prioritize_syllable_sync,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn imported_providers_are_exposed() {
        assert_eq!(17, LyricsSource::ALL.len());
        for expected in [
            LyricsSource::BetterLyricsPortato,
            LyricsSource::PaxSenixSpotify,
            LyricsSource::PaxSenixMusixmatch,
            LyricsSource::YoutubeTranscript,
            LyricsSource::YoutubeMusic,
        ] {
            assert!(LyricsSource::OFFERED.contains(&expected));
        }
    }

    #[test]
    fn hidden_providers_are_kept_but_never_offered() {
        assert!(LyricsSource::ALL.contains(&LyricsSource::SimpMusic));
        assert!(LyricsSource::ALL.contains(&LyricsSource::Megalobiz));
        assert!(LyricsSource::OFFERED.iter().all(|s| !s.hidden()));
        assert_eq!(LyricsSource::LrcRed, LyricsSource::OFFERED[0]);
        assert_eq!(
            LyricsSource::ALL
                .iter()
                .copied()
                .filter(|s| !s.hidden())
                .collect::<Vec<_>>(),
            LyricsSource::OFFERED
        );
    }

    #[test]
    fn saved_order_drops_hidden_sources_and_puts_new_first_source_on_top() {
        let mut saved = vec![LyricsSource::LrcLib];
        saved.extend(
            LyricsSource::ALL
                .iter()
                .copied()
                .filter(|s| *s != LyricsSource::LrcRed && *s != LyricsSource::LrcLib),
        );
        let ordered = LyricsSource::ordered(&saved);
        assert_eq!(
            &[
                LyricsSource::LrcRed,
                LyricsSource::LrcLib,
                LyricsSource::BiniLyrics,
            ][..],
            &ordered[..3]
        );
        let offered_set: HashSet<LyricsSource> = LyricsSource::OFFERED.iter().copied().collect();
        let ordered_set: HashSet<LyricsSource> = ordered.iter().copied().collect();
        assert_eq!(offered_set, ordered_set);
        assert_eq!(ordered.len(), ordered_set.len());
    }

    #[test]
    fn new_source_lands_right_after_declared_neighbour() {
        let saved: Vec<LyricsSource> = LyricsSource::OFFERED
            .iter()
            .copied()
            .filter(|s| *s != LyricsSource::BetterLyricsPortato)
            .collect();
        assert_eq!(
            LyricsSource::OFFERED,
            LyricsSource::ordered(&saved).as_slice()
        );
    }

    #[test]
    fn serde_wire_names_match_kotlin_enum_names() {
        for &source in LyricsSource::ALL {
            let serialized = serde_json::to_string(&source).expect("LyricsSource serializes");
            assert_eq!(format!("\"{}\"", source.wire_name()), serialized);
            let parsed: LyricsSource =
                serde_json::from_str(&serialized).expect("LyricsSource deserializes");
            assert_eq!(source, parsed);
            assert_eq!(
                Some(source),
                LyricsSource::from_wire_name(source.wire_name())
            );
        }
    }

    #[test]
    fn lookup_key_reuses_same_settings_and_invalidates_on_order_or_syllable_change() {
        let sources = [LyricsSource::LrcLib, LyricsSource::Genius];
        let order = vec![LyricsSource::LrcLib, LyricsSource::Genius];
        let base = lyrics_lookup_key("abcdefghijk", sources, order.clone(), false);
        let same = lyrics_lookup_key("abcdefghijk", sources, order.clone(), false);
        assert_eq!(base, same);

        let reversed = lyrics_lookup_key(
            "abcdefghijk",
            sources,
            order.into_iter().rev().collect(),
            false,
        );
        assert_ne!(base, reversed);

        let prioritized = lyrics_lookup_key(
            "abcdefghijk",
            sources,
            vec![LyricsSource::LrcLib, LyricsSource::Genius],
            true,
        );
        assert_ne!(base, prioritized);
    }
}
