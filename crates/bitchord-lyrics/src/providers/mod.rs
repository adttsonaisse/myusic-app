pub mod better_lyrics;
pub mod bini_lyrics;
pub mod genius;
pub mod kugou;
pub mod lrc_lib;
pub mod lrc_red;
pub mod lyrics_plus;
pub mod megalobiz;
pub mod musixmatch;
pub mod pax_senix;
pub mod simp_music;
pub mod unison;
pub mod youtube;

pub use better_lyrics::BetterLyrics;
pub use bini_lyrics::{BiniHit, BiniLyrics, BiniMatch, BiniResponse};
pub use genius::Genius;
pub use kugou::KuGou;
pub use lrc_lib::LrcLib;
pub use lrc_red::{LrcRed, LrcRedHit, LrcRedResponse};
pub use lyrics_plus::{
    LyricsPlus, LyricsPlusAgent, LyricsPlusLine, LyricsPlusMetadata, LyricsPlusResponse,
    LyricsPlusSyllable,
};
pub use megalobiz::Megalobiz;
pub use musixmatch::Musixmatch;
pub use pax_senix::{PaxSenix, normalize_paxsenix_api_key};
pub use simp_music::{SimpMusicLyrics, SimpMusicResponse, SimpMusicTrack};
pub use unison::{Unison, UnisonEntry, UnisonResponse};
pub use youtube::{
    BoxFuture, YouTubeLyricsBackend, YouTubeMusicLyrics, YouTubeTranscriptLyrics, youtube_strings,
};
