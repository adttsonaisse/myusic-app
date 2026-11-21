#![forbid(unsafe_code)]

//! External service integrations for BitChord Linux Desktop.
//!
//! Wave 1 ships `presence` (live install count pings compatible with the
//! BitChord Go backend). Wave 2 expands this crate with Spotify playlist
//! import, Last.fm / ListenBrainz scrobbling, Discord Rich Presence, and the
//! Listen Together WebSocket client.

pub mod presence;

pub use presence::{DEFAULT_ENDPOINT as PRESENCE_ENDPOINT, Presence, PresencePlatform};
