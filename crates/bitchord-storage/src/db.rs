//! SQLite local database for library songs, local/imported playlists, search
//! history, listening history, and download/collection indexes.
//!
//! Links system `libsqlite3` (`rusqlite 0.43`) with WAL mode and explicit
//! `PRAGMA user_version` schema migrations.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bitchord_automix::TrackAnalysis;
use bitchord_model::{EntityType, SearchHistoryEntity, Song};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::downloads::{SavedCollection, SavedSongMetadata};

const SCHEMA_VERSION: i32 = 2;
const MAX_SEARCH_HISTORY: usize = 20;

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

/// Persistent SQLite storage for BitChord Linux Desktop.
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

    fn configure_and_migrate(&self, enable_wal: bool) -> Result<(), DbError> {
        if enable_wal {
            self.conn.pragma_update(None, "journal_mode", "WAL")?;
            self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        self.conn.pragma_update(None, "foreign_keys", "ON")?;

        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 1 {
            self.conn.execute_batch(
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
        }
        if version < 2 {
            self.conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS track_analysis (
                    video_id TEXT PRIMARY KEY,
                    analysis_json TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                "#,
            )?;
            self.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
        self.conn.execute(
            r#"
            INSERT INTO songs (
                video_id, title, artist, thumbnail_url, duration_text,
                album_name, album_id, artist_id,
                is_explicit, is_video, local_uri, local_path, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
                updated_at = excluded.updated_at
            "#,
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
            ],
        )?;
        Ok(())
    }

    pub fn get_song(&self, video_id: &str) -> Result<Option<Song>, DbError> {
        let row = self
            .conn
            .query_row(
                r#"
                SELECT video_id, title, artist, thumbnail_url, duration_text,
                       album_name, album_id, artist_id,
                       is_explicit, is_video, local_uri, local_path
                FROM songs WHERE video_id = ?1
                "#,
                params![video_id],
                row_to_song,
            )
            .optional()?;
        Ok(row)
    }

    pub fn search_cached_songs(&self, query: &str, limit: usize) -> Result<Vec<Song>, DbError> {
        let pattern = format!("%{}%", query.trim());
        let mut stmt = self.conn.prepare(
            r#"
            SELECT video_id, title, artist, thumbnail_url, duration_text,
                   album_name, album_id, artist_id,
                   is_explicit, is_video, local_uri, local_path
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
        for song in songs {
            if !song.video_id.starts_with(prefix) {
                continue;
            }
            let is_explicit: Option<i32> = song.is_explicit.map(i32::from);
            tx.execute(
                r#"
                INSERT INTO songs (
                    video_id, title, artist, thumbnail_url, duration_text,
                    album_name, album_id, artist_id,
                    is_explicit, is_video, local_uri, local_path, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
                    updated_at = excluded.updated_at
                "#,
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
                ],
            )?;
            count += 1;
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
        self.conn.execute(
            "DELETE FROM search_history WHERE lower(id) = lower(?1)",
            params![entity.id],
        )?;
        self.conn.execute(
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
        self.conn.execute(
            r#"
            DELETE FROM search_history
            WHERE id NOT IN (
                SELECT id FROM search_history ORDER BY timestamp DESC LIMIT ?1
            )
            "#,
            params![MAX_SEARCH_HISTORY as i64],
        )?;
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

    pub fn stats(&self) -> Result<DbStats, DbError> {
        let count = |table: &str| -> Result<usize, DbError> {
            let n: i64 =
                self.conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
            Ok(n.max(0) as usize)
        };
        Ok(DbStats {
            songs: count("songs")?,
            playlists: count("local_playlists")?,
            search_history: count("search_history")?,
            listening_history: count("listening_history")?,
            downloads: count("downloads")?,
            downloaded_collections: count("downloaded_collections")?,
        })
    }
}

fn row_to_song(row: &rusqlite::Row<'_>) -> rusqlite::Result<Song> {
    let mut song = Song::new(
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, Option<String>>(3)?,
    );
    song.duration_text = row.get(4)?;
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
}
