#![forbid(unsafe_code)]

pub mod alignments;
pub mod background_vocals;
pub mod enhanced_lrc;
pub mod gaps;
pub mod http;
pub mod line;
pub mod lrc_writer;
pub mod provider_lyrics;
pub mod providers;
pub mod query;
pub mod repository;
pub mod sections;
pub mod source;
pub mod syllable_runs;
pub mod translation;
pub mod translation_languages;
pub mod ttml;

pub use alignments::line_alignments;
pub use background_vocals::{split_trailing_bracket, with_background_vocals};
pub use enhanced_lrc::{EnhancedLrc, parse_lrc};
pub use gaps::{MIN_GAP_MS, with_instrumental_gaps};
pub use http::{LYRICS_AGENT, LyricsHttp};
pub use line::{CharGrowth, GrowingWord, LyricAlignment, LyricLine, LyricSyllable, LyricWord};
pub use lrc_writer::{WORD_LYRICS_FIELD, to_enhanced_lrc, to_lrc};
pub use provider_lyrics::{KaraokeLrc, ProviderLyrics};
pub use providers::{
    BetterLyrics, BiniHit, BiniLyrics, BiniMatch, BiniResponse, BoxFuture, Genius, KuGou, LrcLib,
    LrcRed, LrcRedHit, LrcRedResponse, LyricsPlus, LyricsPlusAgent, LyricsPlusLine,
    LyricsPlusMetadata, LyricsPlusResponse, LyricsPlusSyllable, Megalobiz, Musixmatch, PaxSenix,
    SimpMusicLyrics, SimpMusicResponse, SimpMusicTrack, Unison, UnisonEntry, UnisonResponse,
    YouTubeLyricsBackend, YouTubeMusicLyrics, YouTubeTranscriptLyrics, normalize_paxsenix_api_key,
    youtube_strings,
};
pub use query::{artist_for_lyrics_search, for_lyrics_search};
pub use repository::{LyricsCallbacks, LyricsRepository, LyricsRequest, LyricsResult};
pub use sections::is_genius_section_header;
pub use source::{LyricsLookupKey, LyricsSource, lyrics_lookup_key};
pub use syllable_runs::{TimedRun, words_from_runs};
pub use translation::{
    LyricsTranslator, RomanizationResult, TranslationResult, canonical_language,
};
pub use translation_languages::{
    TRANSLATION_LANGUAGES, TranslationLanguage, find_translation_language,
    translation_language_name, translation_language_name_with_platform,
};
pub use ttml::TtmlLyrics;
