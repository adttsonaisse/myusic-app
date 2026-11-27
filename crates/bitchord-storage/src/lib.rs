#![forbid(unsafe_code)]

//! Persistence, secrets, download store, and metadata tagging for BitChord Linux Desktop.
//!
//! Includes:
//! - [`paths`]: Linux XDG Base Directory paths (`~/.config/bitchord`, `~/.local/share/bitchord`, `~/.cache/bitchord`, `~/Music/BitChord`)
//! - [`settings`]: atomic JSON-persisted desktop preferences (`settings.json`)
//! - [`secrets`]: Linux Secret Service credential store (`oo7` with pure-Rust crypto) + in-memory test store
//! - [`db`]: SQLite library, playlists, search history, listening history, and download index (`rusqlite` linking system `libsqlite3`)
//! - [`downloads`]: filename sanitization, magic-byte container/manifest sniffing, and atomic `.part` download staging
//! - [`tagger`]: FLAC/MP4/WebM metadata & word-synced lyrics taggers, offline sidecar/embedded lyrics reader, and `lofty` local file tag scanning

pub mod db;
pub mod downloads;
pub mod paths;
pub mod secrets;
pub mod settings;
pub mod tagger;

pub use db::{
    ArtworkIndexEntry, CachedLyrics, CachedStream, DEFAULT_ARTWORK_CACHE_MAX_BYTES,
    DEFAULT_LYRICS_CACHE_TTL_MS, DEFAULT_LYRICS_NOT_FOUND_TTL_MS, DEFAULT_STREAM_CACHE_TTL_MS,
    DbError, DbStats, LibraryDb, ListeningRecord, LocalPlaylist, SearchHistoryRecord,
    SongMetadata, format_duration_ms,
};
pub use downloads::{
    Container, DownloadStore, DownloadStoreError, MAX_STEM_CHARS, PendingDownload, Probe,
    SavedCollection, SavedSongMetadata, Storable, estimated_kbps, resolved_download_dates, sniff,
    youtube_download_badge,
};
pub use paths::{AppPaths, MUSIC_DOWNLOAD_FOLDER};
pub use secrets::{
    InMemorySecretStore, KeyringSecretStore, SecretError, SecretKey, SecretStore, SecretValue,
};
pub use settings::{
    AppFont, AppSettingsData, AudioOutputPreference, DEFAULT_UPGRADE_LENGTH_SLACK_SECONDS,
    DownloadQuality, EqualizerMode, LocalMusicSort, OutputPcmMode, SettingsError, SettingsStore,
    SmbSettings, SongSort, StreamRequest, ThemeMode, WebDavSettings, request_for_download,
};
pub use tagger::{
    EmbeddedLyrics, FlacHeaderRewrite, FlacTagger, LocalAudioTags, Mp4Tagger, WebmTagger,
    carries_tags, read_local_audio_tags,
};
