#![forbid(unsafe_code)]

//! Native YouTube Music InnerTube API client, parser, QuickJS-NG cipher
//! sandbox, and layered `StreamResolver` (native-first with feature-gated
//! `yt-dlp` dev bridge per ADR-004).

pub mod auth;
pub mod cipher;
pub mod client;
pub mod error;
pub mod locale;
pub mod parser;
pub mod player_client;
pub mod repository;
pub mod song_info;
pub mod stream;
pub mod uploaded;

pub use auth::{
    ChannelSelection, MUSIC_ORIGIN, SessionScope, YOUTUBE_ORIGIN, has_auth_cookie,
    normalize_data_sync_id, parse_session_scope_html, sapisid_from_cookie, sapisid_hash,
    sapisid_hash_at,
};
pub use cipher::CipherSandbox;
pub use client::{InnertubeClient, PlaybackTracking, build_shared_http_client};
pub use error::{InnertubeError, PermanentReason, StreamError};
pub use locale::LocaleTags;
pub use parser::{BrowseHeader, InnertubeParser, LibraryItemPage, PlaylistShelfPage, SearchPage};
pub use player_client::{PlayerClient, WEB_USER_AGENT};
pub use repository::{YtMusicRepository, browse_ids};
pub use song_info::{SongInfo, SongInfoParser, SongInfoRepository, compact_count};
#[cfg(feature = "ytdlp-bridge")]
pub use stream::YtDlpStreamBackend;
pub use stream::{
    AUTH_BOUNDARY_BYTES, BgUtilHttpProvider, BoxFuture, ContainerPreference, FallbackStreamBackend,
    NativeStreamBackend, NoopPoTokenProvider, PROBE_READ_BYTES, PoTokenPair, PoTokenProvider,
    ProbePolicy, ResolveRequest, ResolvedStream, StreamBackend, StreamResolver,
    classify_permanent_reason,
};
pub use uploaded::UploadedTracks;
