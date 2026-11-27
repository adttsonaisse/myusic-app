//! SQLite local database for library songs, local/imported playlists, search
//! history, listening history, download/collection indexes, and persistent
//! stream/lyrics caches.
//!
//! Links system `libsqlite3` (`rusqlite 0.43`) with WAL mode, busy timeout,
//! and explicit `PRAGMA user_version` schema migrations.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitchord_automix::TrackAnalysis;
use bitchord_lyrics::{LyricLine, LyricsSource};
use bitchord_model::{EntityType, SearchHistoryEntity, Song, duration_millis};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::downloads::{SavedCollection, SavedSongMetadata};

const SCHEMA_VERSION: i32 = 3;
const MAX_SEARCH_HISTORY: usize = 20;
const BUSY_TIMEOUT_MS: u64 = 5_000;
const CACHE_SIZE_KIB: i64 = -16_000;

/// Default TTL (4 hours) for cached stream URLs when callers do not derive an
/// explicit `expire` timestamp from the URL.
pub const DEFAULT_STREAM_CACHE_TTL_MS: i64 = 4 * 60 * 60 * 1_000;
/// Default TTL (30 days) for positive lyrics cache entries.
pub const DEFAULT_LYRICS_CACHE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
/// Default TTL (24 hours) for negative ("not found") lyrics cache entries.
pub const DEFAULT_LYRICS_NOT_FOUND_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Local or Spotify-imported playlist header stored in SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPlaylist {
    pub id: String,
    pub title: String,
    pub description: String,
    pub thumbnail_url: Option<String>,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Recorded playback history entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningRecord {
    pub id: i64,
    pub video_id: String,
    pub played_at: i64,
    pub duration_ms: i64,
}

/// Lightweight cached track metadata used to enrich playback/UI without loading
/// the full `Song` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongMetadata {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub thumbnail_url: Option<String>,
    pub duration_text: Option<String>,
    pub duration_ms: Option<u64>,
    pub album_name: Option<String>,
    pub local_path: Option<String>,
}

/// Persistent SQLite cache entry for a resolved remote audio stream.
///
/// Timestamps (`expires_at`, `updated_at`) are Unix epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedStream {
    pub cache_key: String,
    pub video_id: String,
    pub url: String,
    pub mime_type: String,
    pub ext: String,
    pub kbps: u32,
    pub content_length: Option<u64>,
    pub duration_ms: Option<u64>,
    pub loudness_db: Option<f64>,
    pub is_live_hls: bool,
    pub client_name: String,
    pub profile_id: String,
    pub media_headers: BTreeMap<String, String>,
    pub range_chunk_bytes: Option<u64>,
    pub expires_at: i64,
    pub updated_at: i64,
}

/// Persistent SQLite cache entry for song lyrics (supports positive and
/// negative caching with explicit expiry timestamps in Unix epoch milliseconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedLyrics {
    pub video_id: String,
    pub source: Option<LyricsSource>,
    pub lines: Vec<LyricLine>,
    pub is_synced: bool,
    pub not_found: bool,
    pub fetched_at: i64,
    pub expires_at: i64,
}

/// High-level row counts for CLI diagnostics and settings storage summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbStats {
    pub songs: usize,
    pub playlists: usize,
    pub search_history: usize,
    pub listening_history: usize,
    pub downloads: usize,
    pub downloaded_collections: usize,
}

/// Formats a millisecond duration as `M:SS` or `H:MM:SS`.
pub fn format_duration_ms(ms: u64) -> String {
    let total_secs = ms / 1_000;
    let hours = total_secs / 3_600;
    let mins = (total_secs % 3_600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins}:{secs:02}")
    }
}

/// Persistent SQLite storage for BitChord Linux Desktop.
///
/// Designed to be opened once and owned by a dedicated background DB worker
/// thread (or guarded behind a `Mutex`) rather than reopened per operation.
#[derive(Debug)]
pub struct LibraryDb {
    conn: Connection,
}

impl LibraryDb {
    /// Opens (or creates) the SQLite database at `path` in WAL mode and runs migrations.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.configure_and_migrate(true)?;
        Ok(db)
    }

    /// Opens an isolated in-memory SQLite database (ideal for unit tests).
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.configure_and_migrate(false)?;
        Ok(db)
    }

    /// Returns the current SQLite `PRAGMA user_version`.
    pub fn schema_version(&self) -> Result<i32, DbError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Issues a non-blocking passive WAL checkpoint (useful during graceful
    /// shutdown or periodic maintenance on a persistent worker connection).
    pub fn checkpoint_passive(&self) -> Result<(), DbError> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }

    fn configure_and_migrate(&self, enable_wal: bool) -> Result<(), DbError> {
        self.conn
            .busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
        if enable_wal {
            let current_mode: String = self
                .conn
                .pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            if !current_mode.eq_ignore_ascii_case("wal") {
                self.conn.pragma_update(None, "journal_mode", "WAL")?;
            }
            self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        self.conn.pragma_update(None, "temp_store", "MEMORY")?;
        self.conn
            .pragma_update(None, "cache_size", CACHE_SIZE_KIB)?;

        let version = self.schema_version()?;
        if version < 1 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS songs (
                    video_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL,
                    thumbnail_url TEXT,
                    duration_text TEXT,
                    album_name TEXT,
                    album_id TEXT,
                    artist_id TEXT,
                    is_explicit INTEGER,
                    is_video INTEGER NOT NULL DEFAULT 0,
                    local_uri TEXT,
                    local_path TEXT,
                    updated_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS local_playlists (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    thumbnail_url TEXT,
                    source TEXT NOT NULL DEFAULT 'local',
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS local_playlist_tracks (
                    playlist_id TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    video_id TEXT NOT NULL,
                    PRIMARY KEY (playlist_id, position),
                    FOREIGN KEY (playlist_id) REFERENCES local_playlists(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS search_history (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    subtitle TEXT NOT NULL,
                    artwork_url TEXT,
                    entity_type TEXT NOT NULL,
                    timestamp INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS listening_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    video_id TEXT NOT NULL,
                    played_at INTEGER NOT NULL,
                    duration_ms INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_listening_history_played_at
                    ON listening_history (played_at DESC);
                CREATE INDEX IF NOT EXISTS idx_listening_history_video_id
                    ON listening_history (video_id);

                CREATE TABLE IF NOT EXISTS downloads (
                    video_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL,
                    thumbnail_url TEXT,
                    duration_text TEXT,
                    album_name TEXT,
                    uri TEXT NOT NULL,
                    download_format TEXT,
                    date_added_seconds INTEGER
                );

                CREATE TABLE IF NOT EXISTS downloaded_collections (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    subtitle TEXT NOT NULL DEFAULT '',
                    thumbnail_url TEXT,
                    playlist INTEGER NOT NULL DEFAULT 0,
                    video_ids_json TEXT NOT NULL DEFAULT '[]'
                );
                "#,
            )?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS track_analysis (
                    video_id TEXT PRIMARY KEY,
                    analysis_json TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                "#,
            )?;
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = self.conn.unchecked_transaction()?;
            if !table_has_column(&tx, "songs", "duration_ms")? {
                tx.execute_batch("ALTER TABLE songs ADD COLUMN duration_ms INTEGER;")?;
            }
            // Backfill duration_ms from existing duration_text rows (v2 -> v3).
            let to_backfill: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT video_id, duration_text FROM songs \
                     WHERE duration_ms IS NULL AND duration_text IS NOT NULL",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            if !to_backfill.is_empty() {
                let mut update_stmt =
                    tx.prepare("UPDATE songs SET duration_ms = ?2 WHERE video_id = ?1")?;
                for (video_id, duration_text) in to_backfill {
                    let ms = duration_millis(Some(&duration_text));
                    if ms > 0 {
                        update_stmt.execute(params![video_id, ms as i64])?;
                    }
                }
            }
            tx.execute_batch(
                r#"
                CREATE INDEX IF NOT EXISTS idx_songs_updated_at
                    ON songs (updated_at DESC);
                CREATE INDEX IF NOT EXISTS idx_search_history_timestamp
                    ON search_history (timestamp DESC);

                CREATE TABLE IF NOT EXISTS stream_cache (
                    cache_key TEXT PRIMARY KEY,
                    video_id TEXT NOT NULL,
                    url TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    ext TEXT NOT NULL,
                    kbps INTEGER NOT NULL,
                    content_length INTEGER,
                    duration_ms INTEGER,
                    loudness_db REAL,
                    is_live_hls INTEGER NOT NULL DEFAULT 0,
                    client_name TEXT NOT NULL DEFAULT '',
                    profile_id TEXT NOT NULL DEFAULT '',
                    headers_json TEXT NOT NULL DEFAULT '{}',
                    range_chunk_bytes INTEGER,
                    expires_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_stream_cache_expires_at
                    ON stream_cache (expires_at);
                CREATE INDEX IF NOT EXISTS idx_stream_cache_video_id
                    ON stream_cache (video_id);

                CREATE TABLE IF NOT EXISTS lyrics_cache (
                    video_id TEXT PRIMARY KEY,
                    source TEXT,
                    lines_json TEXT NOT NULL DEFAULT '[]',
                    is_synced INTEGER NOT NULL DEFAULT 0,
                    not_found INTEGER NOT NULL DEFAULT 0,
                    fetched_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_lyrics_cache_expires_at
                    ON lyrics_cache (expires_at);
                "#,
            )?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(())
    }

    // ---- Track Automix analysis cache --------------------------------------

    pub fn upsert_track_analysis(
        &self,
        video_id: &str,
        analysis: &TrackAnalysis,
    ) -> Result<(), DbError> {
        let now = unix_now_ms();
        let json = serde_json::to_string(analysis)?;
        self.conn.execute(
            r#"
            INSERT INTO track_analysis (video_id, analysis_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(video_id) DO UPDATE SET
                analysis_json = excluded.analysis_json,
                updated_at = excluded.updated_at
            "#,
            params![video_id, json, now],
        )?;
        Ok(())
    }

    pub fn get_track_analysis(&self, video_id: &str) -> Result<Option<TrackAnalysis>, DbError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT analysis_json FROM track_analysis WHERE video_id = ?1",
                params![video_id],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    // ---- Songs cache --------------------------------------------------------

    pub fn upsert_song(&self, song: &Song) -> Result<(), DbError> {
        let now = unix_now_ms();
        let is_explicit: Option<i32> = song.is_explicit.map(i32::from);
        let duration_ms = song_duration_ms_opt(song);
        self.conn.execute(
            UPSERT_SONG_SQL,
            params![
                song.video_id,
                song.title,
                song.artist,
                song.thumbnail_url,
                song.duration_text,
                song.album_name,
                song.album_id,
                song.artist_id,
                is_explicit,
                i32::from(song.is_video),
                song.local_uri,
                song.local_path,
                now,
                duration_ms,
            ],
        )?;
        Ok(())
    }

    /// Upserts a batch of songs inside a single SQLite transaction (ideal for
    /// local folder scans and bulk syncs).
    pub fn upsert_songs_batch(&self, songs: &[Song]) -> Result<usize, DbError> {
        if songs.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let now = unix_now_ms();
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(UPSERT_SONG_SQL)?;
            for song in songs {
                let is_explicit: Option<i32> = song.is_explicit.map(i32::from);
                let duration_ms = song_duration_ms_opt(song);
                stmt.execute(params![
                    song.video_id,
                    song.title,
                    song.artist,
                    song.thumbnail_url,
                    song.duration_text,
                    song.album_name,
                    song.album_id,
                    song.artist_id,
                    is_explicit,
                    i32::from(song.is_video),
                    song.local_uri,
                    song.local_path,
                    now,
                    duration_ms,
                ])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    pub fn get_song(&self, video_id: &str) -> Result<Option<Song>, DbError> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT video_id, title, artist, thumbnail_url, duration_text,
                       album_name, album_id, artist_id,
                       is_explicit, is_video, local_uri, local_path, duration_ms
                FROM songs WHERE video_id = ?1
                "#,
                params![video_id],
                row_to_song,
            )
            .optional()?;
        Ok(row)
    }

    /// Fetches lightweight cached metadata for a track (duration, thumbnail,
    /// title/artist, local path) by primary key without full table scans.
    pub fn get_song_metadata(&self, video_id: &str) -> Result<Option<SongMetadata>, DbError> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT video_id, title, artist, thumbnail_url, duration_text,
                       duration_ms, album_name, local_path
                FROM songs WHERE video_id = ?1
                "#,
                params![video_id],
                |row| {
                    let duration_text: Option<String> = row.get(4)?;
                    let stored_ms: Option<i64> = row.get(5)?;
                    let duration_ms = stored_ms
                        .and_then(|ms| (ms > 0).then_some(ms as u64))
                        .or_else(|| {
                            let parsed = duration_millis(duration_text.as_deref());
                            (parsed > 0).then_some(parsed)
                        });
                    let duration_text = duration_text
                        .or_else(|| duration_ms.map(format_duration_ms));
                    Ok(SongMetadata {
                        video_id: row.get(0)?,
                        title: row.get(1)?,
                        artist: row.get(2)?,
                        thumbnail_url: row.get(3)?,
                        duration_text,
                        duration_ms,
                        album_name: row.get(6)?,
                        local_path: row.get(7)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Updates a cached song's known duration (e.g. after stream resolution or
    /// decoder open). Automatically formats `duration_text` from `duration_ms`
    /// when `duration_text` is `None`. Returns `true` if a row was updated.
    pub fn update_song_duration(
        &self,
        video_id: &str,
        duration_ms: u64,
        duration_text: Option<&str>,
    ) -> Result<bool, DbError> {
        let formatted_text = duration_text
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| (duration_ms > 0).then(|| format_duration_ms(duration_ms)));
        let effective_ms = if duration_ms > 0 {
            Some(duration_ms as i64)
        } else {
            let parsed = duration_millis(formatted_text.as_deref());
            (parsed > 0).then_some(parsed as i64)
        };
        if effective_ms.is_none() && formatted_text.is_none() {
            return Ok(false);
        }
        let changed = self.conn.execute(
            r#"
            UPDATE songs
            SET duration_ms = COALESCE(?2, duration_ms),
                duration_text = COALESCE(?3, duration_text),
                updated_at = ?4
            WHERE video_id = ?1
            "#,
            params![video_id, effective_ms, formatted_text, unix_now_ms()],
        )?;
        Ok(changed > 0)
    }

    pub fn search_cached_songs(&self, query: &str, limit: usize) -> Result<Vec<Song>, DbError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            let mut stmt = self.conn.prepare(
                r#"
                SELECT video_id, title, artist, thumbnail_url, duration_text,
                       album_name, album_id, artist_id,
                       is_explicit, is_video, local_uri, local_path, duration_ms
                FROM songs
                ORDER BY updated_at DESC
                LIMIT ?1
                "#,
            )?;
            let rows = stmt.query_map(params![limit as i64], row_to_song)?;
            return Ok(rows.collect::<Result<Vec<_>, _>>()?);
        }

        let pattern = format!("%{trimmed}%");
        let mut stmt = self.conn.prepare(
            r#"
            SELECT video_id, title, artist, thumbnail_url, duration_text,
                   album_name, album_id, artist_id,
                   is_explicit, is_video, local_uri, local_path, duration_ms
            FROM songs
            WHERE title LIKE ?1 COLLATE NOCASE
               OR artist LIKE ?1 COLLATE NOCASE
               OR album_name LIKE ?1 COLLATE NOCASE
            ORDER BY updated_at DESC
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], row_to_song)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Atomically replaces all cached songs whose `video_id` starts with `prefix`
    /// (e.g. `"webdav:"` or `"smb:"`), removing stale remote entries without touching
    /// YouTube or local disk tracks.
    pub fn replace_prefixed_songs(&self, prefix: &str, songs: &[Song]) -> Result<usize, DbError> {
        let like_pattern = format!("{prefix}%");
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM songs WHERE video_id LIKE ?1",
            params![like_pattern],
        )?;
        let now = unix_now_ms();
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO songs (
                    video_id, title, artist, thumbnail_url, duration_text,
                    album_name, album_id, artist_id,
                    is_explicit, is_video, local_uri, local_path, updated_at, duration_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ON CONFLICT(video_id) DO UPDATE SET
                    title = excluded.title,
                    artist = excluded.artist,
                    thumbnail_url = excluded.thumbnail_url,
                    duration_text = COALESCE(excluded.duration_text, songs.duration_text),
                    album_name = excluded.album_name,
                    album_id = excluded.album_id,
                    artist_id = excluded.artist_id,
                    is_explicit = excluded.is_explicit,
                    is_video = excluded.is_video,
                    local_uri = excluded.local_uri,
                    local_path = excluded.local_path,
                    updated_at = excluded.updated_at,
                    duration_ms = COALESCE(excluded.duration_ms, songs.duration_ms)
                "#,
            )?;
            for song in songs {
                if !song.video_id.starts_with(prefix) {
                    continue;
                }
                let is_explicit: Option<i32> = song.is_explicit.map(i32::from);
                let duration_ms = song_duration_ms_opt(song);
                stmt.execute(params![
                    song.video_id,
                    song.title,
                    song.artist,
                    song.thumbnail_url,
                    song.duration_text,
                    song.album_name,
                    song.album_id,
                    song.artist_id,
                    is_explicit,
                    i32::from(song.is_video),
                    song.local_uri,
                    song.local_path,
                    now,
                    duration_ms,
                ])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    // ---- Search history (capped at 20, case-insensitive id dedup) -----------

    pub fn record_search_history(&self, entity: &SearchHistoryEntity) -> Result<(), DbError> {
        let entity = entity.clone().with_corrected_type();
        let entity_type = match entity.entity_type {
            EntityType::Track => "TRACK",
            EntityType::Album => "ALBUM",
            EntityType::Artist => "ARTIST",
            EntityType::Playlist => "PLAYLIST",
            EntityType::Other => "OTHER",
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM search_history WHERE lower(id) = lower(?1)",
            params![entity.id],
        )?;
        tx.execute(
            r#"
            INSERT INTO search_history (id, title, subtitle, artwork_url, entity_type, timestamp)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                entity.id,
                entity.title,
                entity.subtitle,
                entity.artwork_url,
                entity_type,
                entity.timestamp,
            ],
        )?;
        // Enforce MAX_SEARCH_HISTORY cap keeping most recent rows.
        tx.execute(
            r#"
            DELETE FROM search_history
            WHERE id NOT IN (
                SELECT id FROM search_history ORDER BY timestamp DESC LIMIT ?1
            )
            "#,
            params![MAX_SEARCH_HISTORY as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_search_history(&self) -> Result<Vec<SearchHistoryEntity>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, title, subtitle, artwork_url, entity_type, timestamp
            FROM search_history
            ORDER BY timestamp DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![MAX_SEARCH_HISTORY as i64], |row| {
            let raw_type: String = row.get(4)?;
            let entity_type = match raw_type.as_str() {
                "TRACK" => EntityType::Track,
                "ALBUM" => EntityType::Album,
                "ARTIST" => EntityType::Artist,
                "PLAYLIST" => EntityType::Playlist,
                _ => EntityType::Other,
            };
            Ok(SearchHistoryEntity {
                id: row.get(0)?,
                title: row.get(1)?,
                subtitle: row.get(2)?,
                artwork_url: row.get(3)?,
                entity_type,
                timestamp: row.get(5)?,
            }
            .with_corrected_type())
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn remove_search_history(&self, id: &str) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM search_history WHERE lower(id) = lower(?1)",
            params![id],
        )?;
        Ok(())
    }

    pub fn clear_search_history(&self) -> Result<(), DbError> {
        self.conn.execute("DELETE FROM search_history", [])?;
        Ok(())
    }

    // ---- Listening history --------------------------------------------------

    pub fn record_play(
        &self,
        video_id: &str,
        played_at_ms: i64,
        duration_ms: i64,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO listening_history (video_id, played_at, duration_ms) VALUES (?1, ?2, ?3)",
            params![video_id, played_at_ms, duration_ms],
        )?;
        Ok(())
    }

    pub fn recent_plays(&self, limit: usize) -> Result<Vec<ListeningRecord>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, video_id, played_at, duration_ms
            FROM listening_history
            ORDER BY played_at DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(ListeningRecord {
                id: row.get(0)?,
                video_id: row.get(1)?,
                played_at: row.get(2)?,
                duration_ms: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn play_count(&self, video_id: &str) -> Result<usize, DbError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM listening_history WHERE video_id = ?1",
            params![video_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    // ---- Local & imported playlists -----------------------------------------

    pub fn upsert_playlist(&self, playlist: &LocalPlaylist) -> Result<(), DbError> {
        self.conn.execute(
            r#"
            INSERT INTO local_playlists (id, title, description, thumbnail_url, source, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                thumbnail_url = excluded.thumbnail_url,
                source = excluded.source,
                updated_at = excluded.updated_at
            "#,
            params![
                playlist.id,
                playlist.title,
                playlist.description,
                playlist.thumbnail_url,
                playlist.source,
                playlist.created_at,
                playlist.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_playlists(&self) -> Result<Vec<LocalPlaylist>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, title, description, thumbnail_url, source, created_at, updated_at
            FROM local_playlists
            ORDER BY updated_at DESC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LocalPlaylist {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                thumbnail_url: row.get(3)?,
                source: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn set_playlist_tracks(
        &mut self,
        playlist_id: &str,
        video_ids: &[String],
    ) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM local_playlist_tracks WHERE playlist_id = ?1",
            params![playlist_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO local_playlist_tracks (playlist_id, position, video_id) VALUES (?1, ?2, ?3)",
            )?;
            for (pos, vid) in video_ids.iter().enumerate() {
                stmt.execute(params![playlist_id, pos as i64, vid])?;
            }
        }
        tx.execute(
            "UPDATE local_playlists SET updated_at = ?2 WHERE id = ?1",
            params![playlist_id, unix_now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT video_id FROM local_playlist_tracks
            WHERE playlist_id = ?1
            ORDER BY position ASC
            "#,
        )?;
        let rows = stmt.query_map(params![playlist_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_playlist(&self, playlist_id: &str) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM local_playlists WHERE id = ?1",
            params![playlist_id],
        )?;
        Ok(())
    }

    // ---- Downloads & collections --------------------------------------------

    pub fn upsert_download(&self, meta: &SavedSongMetadata) -> Result<(), DbError> {
        self.conn.execute(
            r#"
            INSERT INTO downloads (
                video_id, title, artist, thumbnail_url, duration_text,
                album_name, uri, download_format, date_added_seconds
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(video_id) DO UPDATE SET
                title = excluded.title,
                artist = excluded.artist,
                thumbnail_url = excluded.thumbnail_url,
                duration_text = excluded.duration_text,
                album_name = excluded.album_name,
                uri = excluded.uri,
                download_format = excluded.download_format,
                date_added_seconds = COALESCE(downloads.date_added_seconds, excluded.date_added_seconds)
            "#,
            params![
                meta.video_id,
                meta.title,
                meta.artist,
                meta.thumbnail_url,
                meta.duration_text,
                meta.album_name,
                meta.uri,
                meta.download_format,
                meta.date_added_seconds,
            ],
        )?;
        Ok(())
    }

    pub fn get_download(&self, video_id: &str) -> Result<Option<SavedSongMetadata>, DbError> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT video_id, title, artist, thumbnail_url, duration_text,
                       album_name, uri, download_format, date_added_seconds
                FROM downloads WHERE video_id = ?1
                "#,
                params![video_id],
                row_to_download,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_downloads(&self) -> Result<Vec<SavedSongMetadata>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT video_id, title, artist, thumbnail_url, duration_text,
                   album_name, uri, download_format, date_added_seconds
            FROM downloads
            ORDER BY COALESCE(date_added_seconds, 0) DESC, title ASC
            "#,
        )?;
        let rows = stmt.query_map([], row_to_download)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_download(&self, video_id: &str) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM downloads WHERE video_id = ?1",
            params![video_id],
        )?;
        Ok(())
    }

    pub fn upsert_collection(&self, collection: &SavedCollection) -> Result<(), DbError> {
        let video_ids_json = serde_json::to_string(&collection.video_ids)?;
        self.conn.execute(
            r#"
            INSERT INTO downloaded_collections (id, title, subtitle, thumbnail_url, playlist, video_ids_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                subtitle = excluded.subtitle,
                thumbnail_url = excluded.thumbnail_url,
                playlist = excluded.playlist,
                video_ids_json = excluded.video_ids_json
            "#,
            params![
                collection.id,
                collection.title,
                collection.subtitle,
                collection.thumbnail_url,
                i32::from(collection.playlist),
                video_ids_json,
            ],
        )?;
        Ok(())
    }

    pub fn list_collections(&self) -> Result<Vec<SavedCollection>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, title, subtitle, thumbnail_url, playlist, video_ids_json
            FROM downloaded_collections
            ORDER BY title ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            let ids_raw: String = row.get(5)?;
            let video_ids: Vec<String> = serde_json::from_str(&ids_raw).unwrap_or_default();
            Ok(SavedCollection {
                id: row.get(0)?,
                title: row.get(1)?,
                subtitle: row.get(2)?,
                thumbnail_url: row.get(3)?,
                playlist: row.get::<_, i32>(4)? != 0,
                video_ids,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---- Persistent stream URL cache ----------------------------------------

    /// Inserts or updates a resolved stream entry in `stream_cache`.
    pub fn put_stream_cache(&self, entry: &CachedStream) -> Result<(), DbError> {
        let headers_json = serde_json::to_string(&entry.media_headers)?;
        let updated_at = if entry.updated_at > 0 {
            entry.updated_at
        } else {
            unix_now_ms()
        };
        self.conn.execute(
            r#"
            INSERT INTO stream_cache (
                cache_key, video_id, url, mime_type, ext, kbps,
                content_length, duration_ms, loudness_db, is_live_hls,
                client_name, profile_id, headers_json, range_chunk_bytes,
                expires_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(cache_key) DO UPDATE SET
                video_id = excluded.video_id,
                url = excluded.url,
                mime_type = excluded.mime_type,
                ext = excluded.ext,
                kbps = excluded.kbps,
                content_length = excluded.content_length,
                duration_ms = excluded.duration_ms,
                loudness_db = excluded.loudness_db,
                is_live_hls = excluded.is_live_hls,
                client_name = excluded.client_name,
                profile_id = excluded.profile_id,
                headers_json = excluded.headers_json,
                range_chunk_bytes = excluded.range_chunk_bytes,
                expires_at = excluded.expires_at,
                updated_at = excluded.updated_at
            "#,
            params![
                entry.cache_key,
                entry.video_id,
                entry.url,
                entry.mime_type,
                entry.ext,
                i64::from(entry.kbps),
                entry.content_length.map(|v| v as i64),
                entry.duration_ms.map(|v| v as i64),
                entry.loudness_db,
                i32::from(entry.is_live_hls),
                entry.client_name,
                entry.profile_id,
                headers_json,
                entry.range_chunk_bytes.map(|v| v as i64),
                entry.expires_at,
                updated_at,
            ],
        )?;
        Ok(())
    }

    /// Returns a non-expired cached stream for `cache_key` at the current wall clock.
    pub fn get_stream_cache(&self, cache_key: &str) -> Result<Option<CachedStream>, DbError> {
        self.get_stream_cache_at(cache_key, unix_now_ms())
    }

    /// Returns a cached stream for `cache_key` if its `expires_at > now_ms`.
    pub fn get_stream_cache_at(
        &self,
        cache_key: &str,
        now_ms: i64,
    ) -> Result<Option<CachedStream>, DbError> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT cache_key, video_id, url, mime_type, ext, kbps,
                       content_length, duration_ms, loudness_db, is_live_hls,
                       client_name, profile_id, headers_json, range_chunk_bytes,
                       expires_at, updated_at
                FROM stream_cache
                WHERE cache_key = ?1 AND expires_at > ?2
                "#,
                params![cache_key, now_ms],
                row_to_cached_stream,
            )
            .optional()?;
        Ok(row)
    }

    /// Removes one cached stream entry by `cache_key`.
    pub fn delete_stream_cache(&self, cache_key: &str) -> Result<bool, DbError> {
        let deleted = self.conn.execute(
            "DELETE FROM stream_cache WHERE cache_key = ?1",
            params![cache_key],
        )?;
        Ok(deleted > 0)
    }

    /// Removes every cached stream variant for a `video_id` (e.g. after HTTP 403).
    pub fn delete_stream_cache_for_video(&self, video_id: &str) -> Result<usize, DbError> {
        let deleted = self.conn.execute(
            "DELETE FROM stream_cache WHERE video_id = ?1",
            params![video_id],
        )?;
        Ok(deleted)
    }

    /// Clears all entries in `stream_cache` (e.g. on account/session change).
    pub fn clear_stream_cache(&self) -> Result<usize, DbError> {
        Ok(self.conn.execute("DELETE FROM stream_cache", [])?)
    }

    /// Deletes all expired rows from `stream_cache` at the current wall clock.
    pub fn prune_expired_stream_cache(&self) -> Result<usize, DbError> {
        self.prune_expired_stream_cache_at(unix_now_ms())
    }

    /// Deletes all rows from `stream_cache` with `expires_at <= now_ms`.
    pub fn prune_expired_stream_cache_at(&self, now_ms: i64) -> Result<usize, DbError> {
        let deleted = self.conn.execute(
            "DELETE FROM stream_cache WHERE expires_at <= ?1",
            params![now_ms],
        )?;
        Ok(deleted)
    }

    // ---- Persistent lyrics cache (positive + negative TTL) ------------------

    /// Inserts or updates a lyrics cache entry (positive or negative).
    pub fn put_lyrics_cache(&self, entry: &CachedLyrics) -> Result<(), DbError> {
        let source_str = match entry.source {
            Some(src) => Some(lyrics_source_to_str(src)?),
            None => None,
        };
        let lines_json = serde_json::to_string(&entry.lines)?;
        self.conn.execute(
            r#"
            INSERT INTO lyrics_cache (
                video_id, source, lines_json, is_synced, not_found, fetched_at, expires_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(video_id) DO UPDATE SET
                source = excluded.source,
                lines_json = excluded.lines_json,
                is_synced = excluded.is_synced,
                not_found = excluded.not_found,
                fetched_at = excluded.fetched_at,
                expires_at = excluded.expires_at
            "#,
            params![
                entry.video_id,
                source_str,
                lines_json,
                i32::from(entry.is_synced),
                i32::from(entry.not_found),
                entry.fetched_at,
                entry.expires_at,
            ],
        )?;
        Ok(())
    }

    /// Convenience helper to store positive lyrics with automatic `is_synced`
    /// detection and TTL relative to the current wall clock.
    pub fn put_lyrics_found(
        &self,
        video_id: &str,
        source: LyricsSource,
        lines: &[LyricLine],
        ttl_ms: i64,
    ) -> Result<(), DbError> {
        let now = unix_now_ms();
        let is_synced = lines.iter().any(|l| l.time_ms > 0 || !l.words.is_empty());
        self.put_lyrics_cache(&CachedLyrics {
            video_id: video_id.to_owned(),
            source: Some(source),
            lines: lines.to_vec(),
            is_synced,
            not_found: false,
            fetched_at: now,
            expires_at: now.saturating_add(ttl_ms.max(0)),
        })
    }

    /// Convenience helper to store a negative ("no lyrics found") cache entry
    /// so repeated plays do not spam lyrics providers until `ttl_ms` elapses.
    pub fn put_lyrics_not_found(&self, video_id: &str, ttl_ms: i64) -> Result<(), DbError> {
        let now = unix_now_ms();
        self.put_lyrics_cache(&CachedLyrics {
            video_id: video_id.to_owned(),
            source: None,
            lines: Vec::new(),
            is_synced: false,
            not_found: true,
            fetched_at: now,
            expires_at: now.saturating_add(ttl_ms.max(0)),
        })
    }

    /// Returns a non-expired cached lyrics entry for `video_id`.
    pub fn get_lyrics_cache(&self, video_id: &str) -> Result<Option<CachedLyrics>, DbError> {
        self.get_lyrics_cache_at(video_id, unix_now_ms())
    }

    /// Returns a cached lyrics entry for `video_id` if `expires_at > now_ms`.
    pub fn get_lyrics_cache_at(
        &self,
        video_id: &str,
        now_ms: i64,
    ) -> Result<Option<CachedLyrics>, DbError> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT video_id, source, lines_json, is_synced, not_found, fetched_at, expires_at
                FROM lyrics_cache
                WHERE video_id = ?1 AND expires_at > ?2
                "#,
                params![video_id, now_ms],
                row_to_cached_lyrics,
            )
            .optional()?;
        Ok(row)
    }

    /// Deletes a cached lyrics entry for `video_id` (e.g. when user forces refresh).
    pub fn delete_lyrics_cache(&self, video_id: &str) -> Result<bool, DbError> {
        let deleted = self.conn.execute(
            "DELETE FROM lyrics_cache WHERE video_id = ?1",
            params![video_id],
        )?;
        Ok(deleted > 0)
    }

    /// Deletes all expired rows from `lyrics_cache` at the current wall clock.
    pub fn prune_expired_lyrics_cache(&self) -> Result<usize, DbError> {
        self.prune_expired_lyrics_cache_at(unix_now_ms())
    }

    /// Deletes all rows from `lyrics_cache` with `expires_at <= now_ms`.
    pub fn prune_expired_lyrics_cache_at(&self, now_ms: i64) -> Result<usize, DbError> {
        let deleted = self.conn.execute(
            "DELETE FROM lyrics_cache WHERE expires_at <= ?1",
            params![now_ms],
        )?;
        Ok(deleted)
    }

    pub fn stats(&self) -> Result<DbStats, DbError> {
        self.conn
            .query_row(
                r#"
                SELECT
                    (SELECT COUNT(*) FROM songs),
                    (SELECT COUNT(*) FROM local_playlists),
                    (SELECT COUNT(*) FROM search_history),
                    (SELECT COUNT(*) FROM listening_history),
                    (SELECT COUNT(*) FROM downloads),
                    (SELECT COUNT(*) FROM downloaded_collections)
                "#,
                [],
                |row| {
                    Ok(DbStats {
                        songs: row.get::<_, i64>(0)?.max(0) as usize,
                        playlists: row.get::<_, i64>(1)?.max(0) as usize,
                        search_history: row.get::<_, i64>(2)?.max(0) as usize,
                        listening_history: row.get::<_, i64>(3)?.max(0) as usize,
                        downloads: row.get::<_, i64>(4)?.max(0) as usize,
                        downloaded_collections: row.get::<_, i64>(5)?.max(0) as usize,
                    })
                },
            )
            .map_err(DbError::from)
    }
}

const UPSERT_SONG_SQL: &str = r#"
INSERT INTO songs (
    video_id, title, artist, thumbnail_url, duration_text,
    album_name, album_id, artist_id,
    is_explicit, is_video, local_uri, local_path, updated_at, duration_ms
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
ON CONFLICT(video_id) DO UPDATE SET
    title = excluded.title,
    artist = excluded.artist,
    thumbnail_url = COALESCE(excluded.thumbnail_url, songs.thumbnail_url),
    duration_text = COALESCE(excluded.duration_text, songs.duration_text),
    album_name = COALESCE(excluded.album_name, songs.album_name),
    album_id = COALESCE(excluded.album_id, songs.album_id),
    artist_id = COALESCE(excluded.artist_id, songs.artist_id),
    is_explicit = COALESCE(excluded.is_explicit, songs.is_explicit),
    is_video = excluded.is_video,
    local_uri = COALESCE(excluded.local_uri, songs.local_uri),
    local_path = COALESCE(excluded.local_path, songs.local_path),
    updated_at = excluded.updated_at,
    duration_ms = COALESCE(excluded.duration_ms, songs.duration_ms)
"#;

fn song_duration_ms_opt(song: &Song) -> Option<i64> {
    let ms = song.duration_millis();
    (ms > 0).then_some(ms as i64)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, DbError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name.eq_ignore_ascii_case(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn lyrics_source_to_str(source: LyricsSource) -> Result<String, DbError> {
    match serde_json::to_value(source)? {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

fn lyrics_source_from_str(raw: &str) -> Option<LyricsSource> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok()
}

fn row_to_song(row: &rusqlite::Row<'_>) -> rusqlite::Result<Song> {
    let mut song = Song::new(
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, Option<String>>(3)?,
    );
    let duration_text: Option<String> = row.get(4)?;
    let duration_ms: Option<i64> = row.get(12)?;
    song.duration_text = duration_text
        .or_else(|| duration_ms.and_then(|ms| (ms > 0).then(|| format_duration_ms(ms as u64))));
    song.album_name = row.get(5)?;
    song.album_id = row.get(6)?;
    song.artist_id = row.get(7)?;
    song.is_explicit = row.get::<_, Option<i32>>(8)?.map(|v| v != 0);
    song.is_video = row.get::<_, i32>(9)? != 0;
    song.local_uri = row.get(10)?;
    song.local_path = row.get(11)?;
    if song.video_id.starts_with("webdav:") {
        song.playback_source = Some("WebDAV".to_owned());
    } else if song.video_id.starts_with("smb:") {
        song.playback_source = Some("SMB".to_owned());
    } else if song.local_path.is_some() {
        song.playback_source = Some("Local".to_owned());
    }
    Ok(song)
}

fn row_to_cached_stream(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedStream> {
    let headers_raw: String = row.get(12)?;
    let media_headers: BTreeMap<String, String> =
        serde_json::from_str(&headers_raw).unwrap_or_default();
    Ok(CachedStream {
        cache_key: row.get(0)?,
        video_id: row.get(1)?,
        url: row.get(2)?,
        mime_type: row.get(3)?,
        ext: row.get(4)?,
        kbps: row.get::<_, i64>(5)?.max(0) as u32,
        content_length: row.get::<_, Option<i64>>(6)?.and_then(|v| (v >= 0).then_some(v as u64)),
        duration_ms: row.get::<_, Option<i64>>(7)?.and_then(|v| (v >= 0).then_some(v as u64)),
        loudness_db: row.get(8)?,
        is_live_hls: row.get::<_, i32>(9)? != 0,
        client_name: row.get(10)?,
        profile_id: row.get(11)?,
        media_headers,
        range_chunk_bytes: row.get::<_, Option<i64>>(13)?.and_then(|v| (v >= 0).then_some(v as u64)),
        expires_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn row_to_cached_lyrics(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedLyrics> {
    let raw_source: Option<String> = row.get(1)?;
    let source = raw_source.as_deref().and_then(lyrics_source_from_str);
    let raw_lines: String = row.get(2)?;
    let lines: Vec<LyricLine> = serde_json::from_str(&raw_lines).unwrap_or_default();
    Ok(CachedLyrics {
        video_id: row.get(0)?,
        source,
        lines,
        is_synced: row.get::<_, i32>(3)? != 0,
        not_found: row.get::<_, i32>(4)? != 0,
        fetched_at: row.get(5)?,
        expires_at: row.get(6)?,
    })
}

fn row_to_download(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedSongMetadata> {
    Ok(SavedSongMetadata {
        video_id: row.get(0)?,
        title: row.get(1)?,
        artist: row.get(2)?,
        thumbnail_url: row.get(3)?,
        duration_text: row.get(4)?,
        album_name: row.get(5)?,
        uri: row.get(6)?,
        download_format: row.get(7)?,
        date_added_seconds: row.get(8)?,
    })
}

fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_round_trips_songs_playlists_history_and_downloads() {
        let mut db = LibraryDb::open_in_memory().expect("open in-memory db");

        let mut song = Song::new("vid1", "Nightcall", "Kavinsky", Some("https://art".into()));
        song.album_name = Some("OutRun".into());
        song.is_explicit = Some(false);
        db.upsert_song(&song).expect("upsert song");

        let loaded = db.get_song("vid1").expect("get song").expect("present");
        assert_eq!(loaded.title, "Nightcall");
        assert_eq!(loaded.album_name.as_deref(), Some("OutRun"));
        assert_eq!(db.search_cached_songs("kavin", 10).unwrap().len(), 1);

        // Search history deduplicates case-insensitively and caps at 20.
        for i in 0..25 {
            db.record_search_history(&SearchHistoryEntity {
                id: format!("id-{i}"),
                title: format!("Item {i}"),
                subtitle: "Artist".into(),
                artwork_url: None,
                entity_type: EntityType::Track,
                timestamp: 100 + i,
            })
            .expect("record history");
        }
        let history = db.list_search_history().expect("list history");
        assert_eq!(history.len(), 20);
        assert_eq!(history[0].id, "id-24");

        // Listening history.
        db.record_play("vid1", 1_000, 258_000).expect("record play");
        db.record_play("vid1", 2_000, 258_000).expect("record play");
        assert_eq!(db.play_count("vid1").unwrap(), 2);
        assert_eq!(db.recent_plays(10).unwrap()[0].played_at, 2_000);

        // Local playlists + cascading delete.
        db.upsert_playlist(&LocalPlaylist {
            id: "pl-1".into(),
            title: "Drive".into(),
            description: "Synthwave".into(),
            thumbnail_url: None,
            source: "spotify".into(),
            created_at: 1,
            updated_at: 1,
        })
        .expect("upsert playlist");
        db.set_playlist_tracks("pl-1", &["vid1".into(), "vid2".into()])
            .expect("set tracks");
        assert_eq!(
            db.playlist_tracks("pl-1").unwrap(),
            vec!["vid1".to_owned(), "vid2".to_owned()]
        );

        // Downloads + collections preserve original date_added_seconds.
        db.upsert_download(&SavedSongMetadata {
            video_id: "vid1".into(),
            title: "Nightcall".into(),
            artist: "Kavinsky".into(),
            thumbnail_url: None,
            duration_text: Some("4:18".into()),
            album_name: Some("OutRun".into()),
            uri: "file:///music/Kavinsky - Nightcall.flac".into(),
            download_format: Some("FLAC".into()),
            date_added_seconds: Some(1_700_000_000),
        })
        .expect("upsert download");
        db.upsert_download(&SavedSongMetadata {
            video_id: "vid1".into(),
            title: "Nightcall".into(),
            artist: "Kavinsky".into(),
            thumbnail_url: None,
            duration_text: Some("4:18".into()),
            album_name: Some("OutRun".into()),
            uri: "file:///music/Kavinsky - Nightcall.flac".into(),
            download_format: Some("FLAC".into()),
            date_added_seconds: Some(1_800_000_000),
        })
        .expect("update download");
        assert_eq!(
            db.get_download("vid1")
                .unwrap()
                .expect("download")
                .date_added_seconds,
            Some(1_700_000_000)
        );

        db.upsert_collection(&SavedCollection {
            id: "MPREb_1".into(),
            title: "OutRun".into(),
            subtitle: "Kavinsky".into(),
            thumbnail_url: None,
            playlist: false,
            video_ids: vec!["vid1".into()],
        })
        .expect("upsert collection");

        let stats = db.stats().expect("stats");
        assert_eq!(stats.songs, 1);
        assert_eq!(stats.playlists, 1);
        assert_eq!(stats.search_history, 20);
        assert_eq!(stats.listening_history, 2);
        assert_eq!(stats.downloads, 1);
        assert_eq!(stats.downloaded_collections, 1);

        db.delete_playlist("pl-1").expect("delete playlist");
        assert!(db.playlist_tracks("pl-1").unwrap().is_empty());

        // Automix track_analysis JSON cache round-trip (schema v2).
        let analysis = TrackAnalysis {
            bpm: 124.5,
            beat_interval: 60.0 / 124.5,
            beat_confidence: 0.88,
            key: 21,
            key_confidence: 0.72,
            duration: 210.0,
            audible_start_time: 0.5,
            content_end_time: 206.0,
            ..TrackAnalysis::default()
        };
        db.upsert_track_analysis("vid1", &analysis)
            .expect("upsert analysis");
        let loaded_analysis = db
            .get_track_analysis("vid1")
            .expect("get analysis")
            .expect("cached analysis present");
        assert!((loaded_analysis.bpm - 124.5).abs() < 1e-4);
        assert_eq!(loaded_analysis.key, 21);
    }

    #[test]
    fn sqlite_v2_to_v3_migration_and_pragmas() {
        let dir = std::env::temp_dir().join(format!(
            "bitchord_storage_v2_to_v3_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db_path = dir.join("library.sqlite");

        // Build an authentic v2 database on disk (no duration_ms column, no v3 tables).
        {
            let conn = Connection::open(&db_path).expect("open raw v2 sqlite");
            conn.execute_batch(
                r#"
                CREATE TABLE songs (
                    video_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL,
                    thumbnail_url TEXT,
                    duration_text TEXT,
                    album_name TEXT,
                    album_id TEXT,
                    artist_id TEXT,
                    is_explicit INTEGER,
                    is_video INTEGER NOT NULL DEFAULT 0,
                    local_uri TEXT,
                    local_path TEXT,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE local_playlists (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    thumbnail_url TEXT,
                    source TEXT NOT NULL DEFAULT 'local',
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE local_playlist_tracks (
                    playlist_id TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    video_id TEXT NOT NULL,
                    PRIMARY KEY (playlist_id, position),
                    FOREIGN KEY (playlist_id) REFERENCES local_playlists(id) ON DELETE CASCADE
                );
                CREATE TABLE search_history (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    subtitle TEXT NOT NULL,
                    artwork_url TEXT,
                    entity_type TEXT NOT NULL,
                    timestamp INTEGER NOT NULL
                );
                CREATE TABLE listening_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    video_id TEXT NOT NULL,
                    played_at INTEGER NOT NULL,
                    duration_ms INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE downloads (
                    video_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL,
                    thumbnail_url TEXT,
                    duration_text TEXT,
                    album_name TEXT,
                    uri TEXT NOT NULL,
                    download_format TEXT,
                    date_added_seconds INTEGER
                );
                CREATE TABLE downloaded_collections (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    subtitle TEXT NOT NULL DEFAULT '',
                    thumbnail_url TEXT,
                    playlist INTEGER NOT NULL DEFAULT 0,
                    video_ids_json TEXT NOT NULL DEFAULT '[]'
                );
                CREATE TABLE track_analysis (
                    video_id TEXT PRIMARY KEY,
                    analysis_json TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                INSERT INTO songs (video_id, title, artist, duration_text, updated_at)
                VALUES
                    ('v2-short', 'Nightcall', 'Kavinsky', '4:18', 1000),
                    ('v2-long', 'Live Mix', 'DJ Set', '1:02:03', 2000),
                    ('v2-nodur', 'Unknown Len', 'Artist', NULL, 3000);
                PRAGMA user_version = 2;
                "#,
            )
            .expect("seed v2 schema");
        }

        // Open through LibraryDb: must migrate cleanly to v3 and set connection pragmas.
        let db = LibraryDb::open(&db_path).expect("migrate v2 -> v3");
        assert_eq!(db.schema_version().unwrap(), 3);

        let busy_timeout: i64 = db
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        let temp_store: i64 = db
            .conn
            .query_row("PRAGMA temp_store", [], |r| r.get(0))
            .unwrap();
        let cache_size: i64 = db
            .conn
            .query_row("PRAGMA cache_size", [], |r| r.get(0))
            .unwrap();
        let journal_mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        let foreign_keys: i64 = db
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();

        assert_eq!(busy_timeout, 5_000);
        assert_eq!(temp_store, 2); // MEMORY
        assert_eq!(cache_size, -16_000);
        assert!(journal_mode.eq_ignore_ascii_case("wal"));
        assert_eq!(foreign_keys, 1);

        // Verify v2 rows survived and duration_ms was backfilled.
        let short_meta = db.get_song_metadata("v2-short").unwrap().expect("short");
        assert_eq!(short_meta.duration_ms, Some(258_000));
        assert_eq!(short_meta.duration_text.as_deref(), Some("4:18"));

        let long_meta = db.get_song_metadata("v2-long").unwrap().expect("long");
        assert_eq!(long_meta.duration_ms, Some(3_723_000));
        assert_eq!(long_meta.duration_text.as_deref(), Some("1:02:03"));

        let nodur_meta = db.get_song_metadata("v2-nodur").unwrap().expect("nodur");
        assert_eq!(nodur_meta.duration_ms, None);
        assert_eq!(nodur_meta.duration_text, None);

        db.checkpoint_passive().expect("passive checkpoint");
        drop(db);

        // Re-opening an already-v3 DB must be idempotent.
        let reopened = LibraryDb::open(&db_path).expect("reopen v3");
        assert_eq!(reopened.schema_version().unwrap(), 3);
        assert_eq!(reopened.stats().unwrap().songs, 3);
        drop(reopened);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stream_cache_round_trip_expiry_and_invalidation() {
        let db = LibraryDb::open_in_memory().expect("open in-memory db");
        let mut headers = BTreeMap::new();
        headers.insert("User-Agent".into(), "BitChord/1.0".into());

        let make_entry = |cache_key: &str, video_id: &str, expires_at: i64| CachedStream {
            cache_key: cache_key.into(),
            video_id: video_id.into(),
            url: format!("https://rr1.googlevideo.com/videoplayback?id={cache_key}"),
            mime_type: "audio/webm; codecs=\"opus\"".into(),
            ext: "webm".into(),
            kbps: 160,
            content_length: Some(4_194_304),
            duration_ms: Some(258_000),
            loudness_db: Some(-7.25),
            is_live_hls: false,
            client_name: "VISIONOS".into(),
            profile_id: "visionos".into(),
            media_headers: headers.clone(),
            range_chunk_bytes: Some(131_072),
            expires_at,
            updated_at: 1_000,
        };

        let entry_a = make_entry("vidA:256:ANY", "vidA", 10_000);
        let entry_a_low = make_entry("vidA:128:ANY", "vidA", 20_000);
        let entry_b = make_entry("vidB:256:ANY", "vidB", 30_000);

        db.put_stream_cache(&entry_a).unwrap();
        db.put_stream_cache(&entry_a_low).unwrap();
        db.put_stream_cache(&entry_b).unwrap();

        let fetched = db
            .get_stream_cache_at("vidA:256:ANY", 9_999)
            .unwrap()
            .expect("valid before expiry");
        assert_eq!(fetched, entry_a);
        assert!(
            db.get_stream_cache_at("vidA:256:ANY", 10_000)
                .unwrap()
                .is_none(),
            "expired at boundary"
        );

        // Prune removes only entries with expires_at <= now_ms.
        assert_eq!(db.prune_expired_stream_cache_at(15_000).unwrap(), 1);
        assert!(
            db.get_stream_cache_at("vidA:128:ANY", 15_000)
                .unwrap()
                .is_some()
        );

        // Re-insert entry_a and verify per-video invalidation leaves other videos intact.
        db.put_stream_cache(&make_entry("vidA:256:ANY", "vidA", 25_000))
            .unwrap();
        assert_eq!(db.delete_stream_cache_for_video("vidA").unwrap(), 2);
        assert!(
            db.get_stream_cache_at("vidA:128:ANY", 15_000)
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_stream_cache_at("vidB:256:ANY", 15_000)
                .unwrap()
                .is_some()
        );

        assert!(db.delete_stream_cache("vidB:256:ANY").unwrap());
        assert!(!db.delete_stream_cache("vidB:256:ANY").unwrap());

        db.put_stream_cache(&entry_b).unwrap();
        assert_eq!(db.clear_stream_cache().unwrap(), 1);
    }

    #[test]
    fn lyrics_cache_positive_negative_and_pruning() {
        let db = LibraryDb::open_in_memory().expect("open in-memory db");

        let synced_lines = vec![
            LyricLine::new(12_500, "I'm giving you a night call"),
            LyricLine::new(18_000, "To tell you how I feel"),
        ];
        db.put_lyrics_found("vid-synced", LyricsSource::LrcLib, &synced_lines, 60_000)
            .unwrap();

        let cached_synced = db
            .get_lyrics_cache("vid-synced")
            .unwrap()
            .expect("synced cached");
        assert_eq!(cached_synced.source, Some(LyricsSource::LrcLib));
        assert!(cached_synced.is_synced);
        assert!(!cached_synced.not_found);
        assert_eq!(cached_synced.lines, synced_lines);

        let plain_lines = vec![LyricLine::new(0, "Plain verse line")];
        db.put_lyrics_found("vid-plain", LyricsSource::Genius, &plain_lines, 60_000)
            .unwrap();
        let cached_plain = db
            .get_lyrics_cache("vid-plain")
            .unwrap()
            .expect("plain cached");
        assert!(!cached_plain.is_synced);
        assert!(!cached_plain.not_found);

        // Negative cache with explicit timestamps for deterministic TTL testing.
        db.put_lyrics_cache(&CachedLyrics {
            video_id: "vid-miss".into(),
            source: None,
            lines: Vec::new(),
            is_synced: false,
            not_found: true,
            fetched_at: 1_000,
            expires_at: 5_000,
        })
        .unwrap();

        let miss = db
            .get_lyrics_cache_at("vid-miss", 4_999)
            .unwrap()
            .expect("negative cache hit");
        assert!(miss.not_found);
        assert_eq!(miss.source, None);
        assert!(miss.lines.is_empty());
        assert!(db.get_lyrics_cache_at("vid-miss", 5_000).unwrap().is_none());

        assert_eq!(db.prune_expired_lyrics_cache_at(5_000).unwrap(), 1);
        assert!(db.delete_lyrics_cache("vid-plain").unwrap());
        assert!(db.get_lyrics_cache("vid-plain").unwrap().is_none());

        // Convenience negative cache helper stores an active not_found entry.
        db.put_lyrics_not_found("vid-miss-2", DEFAULT_LYRICS_NOT_FOUND_TTL_MS)
            .unwrap();
        let miss2 = db
            .get_lyrics_cache("vid-miss-2")
            .unwrap()
            .expect("not_found helper hit");
        assert!(miss2.not_found);
    }

    #[test]
    fn upsert_songs_batch_and_duration_metadata_helpers() {
        let db = LibraryDb::open_in_memory().expect("open in-memory db");

        let mut s1 = Song::new("batch-1", "Track One", "Artist A", Some("https://art/1".into()));
        s1.duration_text = Some("3:45".into());
        let s2 = Song::new("batch-2", "Track Two", "Artist B", None);
        let s3 = Song::new("batch-3", "Long Mix", "Artist C", None);

        assert_eq!(db.upsert_songs_batch(&[]).unwrap(), 0);
        assert_eq!(db.upsert_songs_batch(&[s1, s2, s3]).unwrap(), 3);
        assert_eq!(db.stats().unwrap().songs, 3);

        // Upserting s1 again without duration_text/thumbnail preserves existing values.
        let s1_sparse = Song::new("batch-1", "Track One", "Artist A", None);
        db.upsert_song(&s1_sparse).unwrap();
        let meta1 = db.get_song_metadata("batch-1").unwrap().expect("meta1");
        assert_eq!(meta1.duration_ms, Some(225_000));
        assert_eq!(meta1.duration_text.as_deref(), Some("3:45"));
        assert_eq!(meta1.thumbnail_url.as_deref(), Some("https://art/1"));

        // Enrich missing duration via update_song_duration (auto-formats M:SS and H:MM:SS).
        assert!(db.update_song_duration("batch-2", 185_000, None).unwrap());
        let meta2 = db.get_song_metadata("batch-2").unwrap().expect("meta2");
        assert_eq!(meta2.duration_ms, Some(185_000));
        assert_eq!(meta2.duration_text.as_deref(), Some("3:05"));
        assert_eq!(
            db.get_song("batch-2").unwrap().unwrap().duration_text.as_deref(),
            Some("3:05")
        );

        assert!(db.update_song_duration("batch-3", 3_723_000, None).unwrap());
        let meta3 = db.get_song_metadata("batch-3").unwrap().expect("meta3");
        assert_eq!(meta3.duration_ms, Some(3_723_000));
        assert_eq!(meta3.duration_text.as_deref(), Some("1:02:03"));

        assert!(!db.update_song_duration("missing-id", 120_000, None).unwrap());
    }

    #[test]
    fn search_cached_songs_empty_query_uses_index_and_orders_by_updated_at() {
        let db = LibraryDb::open_in_memory().expect("open in-memory db");

        let batch = vec![
            Song::new("s-old", "Alpha", "Artist", None),
            Song::new("s-mid", "Beta", "Artist", None),
            Song::new("s-new", "Gamma", "Artist", None),
        ];
        db.upsert_songs_batch(&batch).unwrap();
        db.conn
            .execute("UPDATE songs SET updated_at = 100 WHERE video_id = 's-old'", [])
            .unwrap();
        db.conn
            .execute("UPDATE songs SET updated_at = 200 WHERE video_id = 's-mid'", [])
            .unwrap();
        db.conn
            .execute("UPDATE songs SET updated_at = 300 WHERE video_id = 's-new'", [])
            .unwrap();

        let top2 = db.search_cached_songs("", 2).unwrap();
        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0].video_id, "s-new");
        assert_eq!(top2[1].video_id, "s-mid");

        let top2_ws = db.search_cached_songs("   ", 2).unwrap();
        assert_eq!(top2_ws[0].video_id, "s-new");
        assert_eq!(top2_ws[1].video_id, "s-mid");

        // Confirm SQLite planner uses idx_songs_updated_at without temp B-tree sort.
        let plan_details: Vec<String> = {
            let mut stmt = db
                .conn
                .prepare(
                    "EXPLAIN QUERY PLAN \
                     SELECT video_id, title, artist, thumbnail_url, duration_text, \
                            album_name, album_id, artist_id, \
                            is_explicit, is_video, local_uri, local_path, duration_ms \
                     FROM songs ORDER BY updated_at DESC LIMIT 50",
                )
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(
            plan_details
                .iter()
                .any(|d| d.contains("idx_songs_updated_at")),
            "expected idx_songs_updated_at in query plan, got: {plan_details:?}"
        );
        assert!(
            !plan_details
                .iter()
                .any(|d| d.contains("USE TEMP B-TREE")),
            "unexpected temp B-tree sort in query plan: {plan_details:?}"
        );
    }
}
