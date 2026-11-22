#![forbid(unsafe_code)]

//! External service integrations for BitChord Linux Desktop.
//!
//! Includes:
//! - [`presence`]: live install count pings compatible with the BitChord Go backend
//! - [`nerd_stats`]: playback technical telemetry snapshots
//! - [`discord`]: native Linux Discord IPC Rich Presence over Unix domain sockets
//! - [`scrobbling`]: Last.fm (MD5 `api_sig`) & ListenBrainz scrobbling clients and threshold tracker
//! - [`spotify`]: public playlist embed/Pathfinder v1 importer and signed-in Pathfinder v2 library
//! - [`listen_together`]: Listen Together REST + WebSocket client and monotonic sync state machine

pub mod discord;
pub mod listen_together;
pub mod nerd_stats;
pub mod presence;
pub mod scrobbling;
pub mod spotify;

pub use discord::{
    DISCORD_APPLICATION_ID, DiscordActivity, DiscordError, DiscordIpcClient,
    discord_audio_quality_line,
};
pub use listen_together::{
    ClientFrame, Connection, ControlAction, FrameEffects, Health, ListenTogetherClient,
    ListenTogetherSession, ListenTogetherState, ListenTogetherWs, PartyError, ProbeResult,
    ServerConnectionState, ServerFrame, ServerUrlError, clean_party_code, derive_user_id,
    parse_and_normalize_server_url, redact_server_urls, resolve_switch_target,
    should_skip_idle_probe,
};
pub use nerd_stats::{
    NerdStats, NerdStatsSnapshot, TelemetryProvenance, codec_label, is_dolby_atmos_mime,
    is_lossless_mime, megahertz_label,
};
pub use presence::{DEFAULT_ENDPOINT as PRESENCE_ENDPOINT, Presence, PresencePlatform};
pub use scrobbling::{
    LastFmClient, LastFmSession, ListenBrainzClient, ScrobbleError, ScrobbleTracker,
    lastfm_api_sig, normalize_lastfm_endpoint, primary_artist,
};
pub use spotify::{
    LIKED_COVER, LIKED_ID, SPOTIFY_MISSING_PREFIX, SPOTIFY_PAGE_PREFIX, SPOTIFY_PENDING_PREFIX,
    SpotifyError, SpotifyImportResult, SpotifyImportTrack, SpotifyImporter, SpotifyLibrary,
    SpotifyPlaylist, SpotifyTokens, SpotifyTrack, is_match_missing, is_match_pending,
    is_unresolved_spotify, largest_source, parse_liked_page, parse_playlist_page, parse_track_page,
};
