//! Dedicated persistent SQLite worker actor for `bitchord-desktop`.
//!
//! Opens [`LibraryDb`] once on a dedicated background thread (`bitchord-db`),
//! prunes expired stream/lyrics caches and enforces the v4 artwork disk LRU
//! budget at startup, serves all read/write operations off the 35ms backend
//! control loop and UI thread, and performs a passive WAL checkpoint on clean
//! shutdown.

use std::collections::HashSet;
use std::path::PathBuf;
use std::thread;

use async_channel::{Sender, unbounded};
use bitchord_automix::TrackAnalysis;
use bitchord_lyrics::{LyricLine, LyricsSource};
use bitchord_model::Song;
use bitchord_storage::{
    ArtworkIndexEntry, CachedLyrics, CachedStream, DEFAULT_ARTWORK_CACHE_MAX_BYTES, DbStats,
    LibraryDb, LocalPlaylist, SavedSongMetadata, SearchHistoryRecord, SongMetadata,
};

use crate::backend::LibrarySnapshot;

/// Run an automatic artwork LRU disk prune after this many cache insertions (or
/// immediately if a single entry exceeds 1 MiB).
const ARTWORK_PRUNE_INTERVAL: u32 = 16;
const LARGE_ARTWORK_BYTES: i64 = 1024 * 1024;

enum DbOp {
    FetchLibrarySnapshot {
        reply: Sender<LibrarySnapshot>,
    },
    RecordSearchHistory {
        entry: Box<SearchHistoryRecord>,
        reply: Sender<Vec<SearchHistoryRecord>>,
    },
    RemoveSearchHistory {
        id: String,
        reply: Sender<Vec<SearchHistoryRecord>>,
    },
    ClearSearchHistory {
        reply: Sender<Vec<SearchHistoryRecord>>,
    },
    ToggleSongLiked {
        song: Box<Song>,
        reply: Sender<(bool, HashSet<String>)>,
    },
    SetSongLiked {
        song: Box<Song>,
        liked: bool,
        reply: Sender<(bool, HashSet<String>)>,
    },
    ListLikedVideoIds {
        reply: Sender<HashSet<String>>,
    },
    ListLikedSongs {
        reply: Sender<Vec<Song>>,
    },
    UpsertArtworkEntry {
        entry: ArtworkIndexEntry,
    },
    TouchArtworkEntry {
        cache_key: String,
    },
    DeleteArtworkEntry {
        cache_key: String,
    },
    PruneArtworkLru {
        max_bytes: i64,
        reply: Option<Sender<Vec<String>>>,
    },
    RecordTrackPlay {
        song: Box<Song>,
        played_at_ms: i64,
        duration_ms: i64,
        reply: Sender<DbStats>,
    },
    UpdateSongDuration {
        video_id: String,
        duration_ms: u64,
        duration_text: Option<String>,
    },
    UpsertSongsBatch {
        songs: Vec<Song>,
        reply: Sender<Result<usize, String>>,
    },
    ReplacePrefixedSongs {
        prefix: String,
        songs: Vec<Song>,
        reply: Sender<Result<usize, String>>,
    },
    ImportSpotifyPlaylist {
        playlist: Box<LocalPlaylist>,
        songs: Vec<Song>,
        track_ids: Vec<String>,
        reply: Sender<Result<(), String>>,
    },
    GetDownload {
        video_id: String,
        reply: Sender<Option<SavedSongMetadata>>,
    },
    GetSongMetadata {
        video_id: String,
        reply: Sender<Option<SongMetadata>>,
    },
    GetStreamCache {
        cache_key: String,
        reply: Sender<Option<CachedStream>>,
    },
    PutStreamCache {
        entry: Box<CachedStream>,
    },
    DeleteStreamCacheForVideo {
        video_id: String,
    },
    ClearStreamCache,
    GetLyricsCache {
        video_id: String,
        reply: Sender<Option<CachedLyrics>>,
    },
    PutLyricsFound {
        video_id: String,
        source: LyricsSource,
        lines: Vec<LyricLine>,
        ttl_ms: i64,
    },
    PutLyricsNotFound {
        video_id: String,
        ttl_ms: i64,
    },
    GetTrackAnalysis {
        video_id: String,
        reply: Sender<Option<TrackAnalysis>>,
    },
    UpsertTrackAnalysis {
        video_id: String,
        analysis: Box<TrackAnalysis>,
    },
    Shutdown {
        ack: Sender<()>,
    },
}

/// Cloneable handle to the single persistent background SQLite worker thread.
#[derive(Clone)]
pub struct DbWorker {
    tx: Sender<DbOp>,
}

impl DbWorker {
    /// Spawns the dedicated `bitchord-db` thread, opens SQLite once, and prunes expired caches.
    pub fn start(db_path: PathBuf) -> Self {
        let (tx, rx) = unbounded::<DbOp>();
        thread::Builder::new()
            .name("bitchord-db".to_owned())
            .spawn(move || {
                let mut db = LibraryDb::open(&db_path).ok();
                if let Some(ref conn) = db {
                    let _ = conn.prune_expired_stream_cache();
                    let _ = conn.prune_expired_lyrics_cache();
                    prune_and_unlink_artwork(conn, DEFAULT_ARTWORK_CACHE_MAX_BYTES);
                }

                let mut artwork_writes_since_prune: u32 = 0;

                while let Ok(op) = rx.recv_blocking() {
                    match op {
                        DbOp::FetchLibrarySnapshot { reply } => {
                            let snap = db.as_ref().map(read_library_snapshot).unwrap_or_default();
                            let _ = reply.send_blocking(snap);
                        }
                        DbOp::RecordSearchHistory { entry, reply } => {
                            let list = if let Some(ref conn) = db {
                                let _ = conn.record_search_history_entry(&entry);
                                conn.list_search_history_entries().unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let _ = reply.send_blocking(list);
                        }
                        DbOp::RemoveSearchHistory { id, reply } => {
                            let list = if let Some(ref conn) = db {
                                let _ = conn.remove_search_history(&id);
                                conn.list_search_history_entries().unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let _ = reply.send_blocking(list);
                        }
                        DbOp::ClearSearchHistory { reply } => {
                            let list = if let Some(ref conn) = db {
                                let _ = conn.clear_search_history();
                                conn.list_search_history_entries().unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let _ = reply.send_blocking(list);
                        }
                        DbOp::ToggleSongLiked { song, reply } => {
                            let res = if let Some(ref conn) = db {
                                let liked = conn.toggle_song_liked(&song).unwrap_or(false);
                                let ids = conn
                                    .list_liked_video_ids()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .collect();
                                (liked, ids)
                            } else {
                                (false, HashSet::new())
                            };
                            let _ = reply.send_blocking(res);
                        }
                        DbOp::SetSongLiked { song, liked, reply } => {
                            let res = if let Some(ref conn) = db {
                                let now_liked =
                                    conn.set_song_liked(&song, liked).unwrap_or(false);
                                let ids = conn
                                    .list_liked_video_ids()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .collect();
                                (now_liked, ids)
                            } else {
                                (false, HashSet::new())
                            };
                            let _ = reply.send_blocking(res);
                        }
                        DbOp::ListLikedVideoIds { reply } => {
                            let ids = db
                                .as_ref()
                                .and_then(|conn| conn.list_liked_video_ids().ok())
                                .unwrap_or_default()
                                .into_iter()
                                .collect();
                            let _ = reply.send_blocking(ids);
                        }
                        DbOp::ListLikedSongs { reply } => {
                            let songs = db
                                .as_ref()
                                .and_then(|conn| conn.list_liked_songs().ok())
                                .unwrap_or_default();
                            let _ = reply.send_blocking(songs);
                        }
                        DbOp::UpsertArtworkEntry { entry } => {
                            if let Some(ref conn) = db {
                                let byte_size = entry.byte_size;
                                let _ = conn.upsert_artwork_entry(&entry);
                                artwork_writes_since_prune =
                                    artwork_writes_since_prune.saturating_add(1);
                                if artwork_writes_since_prune >= ARTWORK_PRUNE_INTERVAL
                                    || byte_size >= LARGE_ARTWORK_BYTES
                                {
                                    artwork_writes_since_prune = 0;
                                    prune_and_unlink_artwork(conn, DEFAULT_ARTWORK_CACHE_MAX_BYTES);
                                }
                            }
                        }
                        DbOp::TouchArtworkEntry { cache_key } => {
                            if let Some(ref conn) = db {
                                let _ = conn.touch_artwork_entry(&cache_key);
                            }
                        }
                        DbOp::DeleteArtworkEntry { cache_key } => {
                            if let Some(ref conn) = db {
                                let _ = conn.delete_artwork_entry(&cache_key);
                            }
                        }
                        DbOp::PruneArtworkLru { max_bytes, reply } => {
                            let evicted = db
                                .as_ref()
                                .map(|conn| prune_and_unlink_artwork(conn, max_bytes))
                                .unwrap_or_default();
                            if let Some(reply) = reply {
                                let _ = reply.send_blocking(evicted);
                            }
                        }
                        DbOp::RecordTrackPlay {
                            song,
                            played_at_ms,
                            duration_ms,
                            reply,
                        } => {
                            let stats = if let Some(ref conn) = db {
                                let _ = conn.upsert_song(&song);
                                let _ = conn.record_play(&song.video_id, played_at_ms, duration_ms);
                                conn.stats().unwrap_or_default()
                            } else {
                                DbStats::default()
                            };
                            let _ = reply.send_blocking(stats);
                        }
                        DbOp::UpdateSongDuration {
                            video_id,
                            duration_ms,
                            duration_text,
                        } => {
                            if let Some(ref conn) = db {
                                let _ = conn.update_song_duration(
                                    &video_id,
                                    duration_ms,
                                    duration_text.as_deref(),
                                );
                            }
                        }
                        DbOp::UpsertSongsBatch { songs, reply } => {
                            let res = match db.as_ref() {
                                Some(conn) => {
                                    conn.upsert_songs_batch(&songs).map_err(|e| e.to_string())
                                }
                                None => Err("Library database unavailable".to_owned()),
                            };
                            let _ = reply.send_blocking(res);
                        }
                        DbOp::ReplacePrefixedSongs {
                            prefix,
                            songs,
                            reply,
                        } => {
                            let res = match db.as_ref() {
                                Some(conn) => conn
                                    .replace_prefixed_songs(&prefix, &songs)
                                    .map_err(|e| e.to_string()),
                                None => Err("Library database unavailable".to_owned()),
                            };
                            let _ = reply.send_blocking(res);
                        }
                        DbOp::ImportSpotifyPlaylist {
                            playlist,
                            songs,
                            track_ids,
                            reply,
                        } => {
                            let res = match db.as_mut() {
                                Some(conn) => {
                                    let r = conn
                                        .upsert_playlist(&playlist)
                                        .and_then(|()| conn.upsert_songs_batch(&songs).map(|_| ()))
                                        .and_then(|()| {
                                            conn.set_playlist_tracks(&playlist.id, &track_ids)
                                        });
                                    r.map_err(|e| e.to_string())
                                }
                                None => Err("Library database unavailable".to_owned()),
                            };
                            let _ = reply.send_blocking(res);
                        }
                        DbOp::GetDownload { video_id, reply } => {
                            let hit = db
                                .as_ref()
                                .and_then(|conn| conn.get_download(&video_id).ok().flatten());
                            let _ = reply.send_blocking(hit);
                        }
                        DbOp::GetSongMetadata { video_id, reply } => {
                            let hit = db
                                .as_ref()
                                .and_then(|conn| conn.get_song_metadata(&video_id).ok().flatten());
                            let _ = reply.send_blocking(hit);
                        }
                        DbOp::GetStreamCache { cache_key, reply } => {
                            let hit = db
                                .as_ref()
                                .and_then(|conn| conn.get_stream_cache(&cache_key).ok().flatten());
                            let _ = reply.send_blocking(hit);
                        }
                        DbOp::PutStreamCache { entry } => {
                            if let Some(ref conn) = db {
                                let _ = conn.put_stream_cache(&entry);
                            }
                        }
                        DbOp::DeleteStreamCacheForVideo { video_id } => {
                            if let Some(ref conn) = db {
                                let _ = conn.delete_stream_cache_for_video(&video_id);
                            }
                        }
                        DbOp::ClearStreamCache => {
                            if let Some(ref conn) = db {
                                let _ = conn.clear_stream_cache();
                            }
                        }
                        DbOp::GetLyricsCache { video_id, reply } => {
                            let hit = db
                                .as_ref()
                                .and_then(|conn| conn.get_lyrics_cache(&video_id).ok().flatten());
                            let _ = reply.send_blocking(hit);
                        }
                        DbOp::PutLyricsFound {
                            video_id,
                            source,
                            lines,
                            ttl_ms,
                        } => {
                            if let Some(ref conn) = db {
                                let _ = conn.put_lyrics_found(&video_id, source, &lines, ttl_ms);
                            }
                        }
                        DbOp::PutLyricsNotFound { video_id, ttl_ms } => {
                            if let Some(ref conn) = db {
                                let _ = conn.put_lyrics_not_found(&video_id, ttl_ms);
                            }
                        }
                        DbOp::GetTrackAnalysis { video_id, reply } => {
                            let hit = db
                                .as_ref()
                                .and_then(|conn| conn.get_track_analysis(&video_id).ok().flatten());
                            let _ = reply.send_blocking(hit);
                        }
                        DbOp::UpsertTrackAnalysis { video_id, analysis } => {
                            if let Some(ref conn) = db {
                                let _ = conn.upsert_track_analysis(&video_id, &analysis);
                            }
                        }
                        DbOp::Shutdown { ack } => {
                            if let Some(ref conn) = db {
                                let _ = conn.checkpoint_passive();
                            }
                            let _ = ack.send_blocking(());
                            break;
                        }
                    }
                }
            })
            .expect("spawn bitchord db worker thread");

        Self { tx }
    }

    pub async fn fetch_library_snapshot(&self) -> LibrarySnapshot {
        let (reply, rx) = async_channel::bounded(1);
        if self.tx.send(DbOp::FetchLibrarySnapshot { reply }).await.is_err() {
            return LibrarySnapshot::default();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub async fn record_search_history(
        &self,
        entry: SearchHistoryRecord,
    ) -> Vec<SearchHistoryRecord> {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::RecordSearchHistory {
                entry: Box::new(entry),
                reply,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub async fn remove_search_history(&self, id: String) -> Vec<SearchHistoryRecord> {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::RemoveSearchHistory { id, reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub async fn clear_search_history(&self) -> Vec<SearchHistoryRecord> {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::ClearSearchHistory { reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub async fn toggle_song_liked(&self, song: Song) -> (bool, HashSet<String>) {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::ToggleSongLiked {
                song: Box::new(song),
                reply,
            })
            .await
            .is_err()
        {
            return (false, HashSet::new());
        }
        rx.recv().await.unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn set_song_liked(&self, song: Song, liked: bool) -> (bool, HashSet<String>) {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::SetSongLiked {
                song: Box::new(song),
                liked,
                reply,
            })
            .await
            .is_err()
        {
            return (false, HashSet::new());
        }
        rx.recv().await.unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn list_liked_video_ids(&self) -> HashSet<String> {
        let (reply, rx) = async_channel::bounded(1);
        if self.tx.send(DbOp::ListLikedVideoIds { reply }).await.is_err() {
            return HashSet::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn list_liked_songs(&self) -> Vec<Song> {
        let (reply, rx) = async_channel::bounded(1);
        if self.tx.send(DbOp::ListLikedSongs { reply }).await.is_err() {
            return Vec::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub fn upsert_artwork_entry(&self, entry: ArtworkIndexEntry) {
        let _ = self.tx.try_send(DbOp::UpsertArtworkEntry { entry });
    }

    pub fn touch_artwork_entry(&self, cache_key: String) {
        let _ = self.tx.try_send(DbOp::TouchArtworkEntry { cache_key });
    }

    pub fn delete_artwork_entry(&self, cache_key: String) {
        let _ = self.tx.try_send(DbOp::DeleteArtworkEntry { cache_key });
    }

    #[allow(dead_code)]
    pub async fn prune_artwork_lru(&self, max_bytes: i64) -> Vec<String> {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::PruneArtworkLru {
                max_bytes,
                reply: Some(reply),
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub async fn record_track_play(
        &self,
        song: Song,
        played_at_ms: i64,
        duration_ms: i64,
    ) -> DbStats {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::RecordTrackPlay {
                song: Box::new(song),
                played_at_ms,
                duration_ms,
                reply,
            })
            .await
            .is_err()
        {
            return DbStats::default();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub fn update_song_duration(
        &self,
        video_id: String,
        duration_ms: u64,
        duration_text: Option<String>,
    ) {
        let _ = self.tx.try_send(DbOp::UpdateSongDuration {
            video_id,
            duration_ms,
            duration_text,
        });
    }

    pub async fn upsert_songs_batch(&self, songs: Vec<Song>) -> Result<usize, String> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::UpsertSongsBatch { songs, reply })
            .await
            .map_err(|_| "DB worker closed".to_owned())?;
        rx.recv()
            .await
            .map_err(|_| "DB worker dropped reply".to_owned())?
    }

    pub async fn replace_prefixed_songs(
        &self,
        prefix: String,
        songs: Vec<Song>,
    ) -> Result<usize, String> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::ReplacePrefixedSongs {
                prefix,
                songs,
                reply,
            })
            .await
            .map_err(|_| "DB worker closed".to_owned())?;
        rx.recv()
            .await
            .map_err(|_| "DB worker dropped reply".to_owned())?
    }

    pub async fn import_spotify_playlist(
        &self,
        playlist: LocalPlaylist,
        songs: Vec<Song>,
        track_ids: Vec<String>,
    ) -> Result<(), String> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::ImportSpotifyPlaylist {
                playlist: Box::new(playlist),
                songs,
                track_ids,
                reply,
            })
            .await
            .map_err(|_| "DB worker closed".to_owned())?;
        rx.recv()
            .await
            .map_err(|_| "DB worker dropped reply".to_owned())?
    }

    pub async fn get_download(&self, video_id: String) -> Option<SavedSongMetadata> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::GetDownload { video_id, reply })
            .await
            .ok()?;
        rx.recv().await.ok().flatten()
    }

    pub fn get_download_blocking(&self, video_id: String) -> Option<SavedSongMetadata> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send_blocking(DbOp::GetDownload { video_id, reply })
            .ok()?;
        rx.recv_blocking().ok().flatten()
    }

    pub async fn get_song_metadata(&self, video_id: String) -> Option<SongMetadata> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::GetSongMetadata { video_id, reply })
            .await
            .ok()?;
        rx.recv().await.ok().flatten()
    }

    pub async fn get_stream_cache(&self, cache_key: String) -> Option<CachedStream> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::GetStreamCache { cache_key, reply })
            .await
            .ok()?;
        rx.recv().await.ok().flatten()
    }

    pub fn put_stream_cache(&self, entry: CachedStream) {
        let _ = self.tx.try_send(DbOp::PutStreamCache {
            entry: Box::new(entry),
        });
    }

    pub fn delete_stream_cache_for_video(&self, video_id: String) {
        let _ = self
            .tx
            .try_send(DbOp::DeleteStreamCacheForVideo { video_id });
    }

    pub fn clear_stream_cache(&self) {
        let _ = self.tx.try_send(DbOp::ClearStreamCache);
    }

    pub async fn get_lyrics_cache(&self, video_id: String) -> Option<CachedLyrics> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send(DbOp::GetLyricsCache { video_id, reply })
            .await
            .ok()?;
        rx.recv().await.ok().flatten()
    }

    pub fn put_lyrics_found(
        &self,
        video_id: String,
        source: LyricsSource,
        lines: Vec<LyricLine>,
        ttl_ms: i64,
    ) {
        let _ = self.tx.try_send(DbOp::PutLyricsFound {
            video_id,
            source,
            lines,
            ttl_ms,
        });
    }

    pub fn put_lyrics_not_found(&self, video_id: String, ttl_ms: i64) {
        let _ = self
            .tx
            .try_send(DbOp::PutLyricsNotFound { video_id, ttl_ms });
    }

    pub fn get_track_analysis_blocking(&self, video_id: String) -> Option<TrackAnalysis> {
        let (reply, rx) = async_channel::bounded(1);
        self.tx
            .send_blocking(DbOp::GetTrackAnalysis { video_id, reply })
            .ok()?;
        rx.recv_blocking().ok().flatten()
    }

    pub fn upsert_track_analysis(&self, video_id: String, analysis: TrackAnalysis) {
        let _ = self.tx.try_send(DbOp::UpsertTrackAnalysis {
            video_id,
            analysis: Box::new(analysis),
        });
    }

    /// Flushes WAL passively and terminates the DB worker cleanly during shutdown.
    pub fn shutdown_blocking(&self) {
        let (ack, rx) = async_channel::bounded(1);
        if self.tx.send_blocking(DbOp::Shutdown { ack }).is_ok() {
            let _ = rx.recv_blocking();
        }
    }
}

fn prune_and_unlink_artwork(db: &LibraryDb, max_bytes: i64) -> Vec<String> {
    let evicted = db.prune_artwork_lru(max_bytes).unwrap_or_default();
    for path in &evicted {
        let _ = std::fs::remove_file(path);
    }
    evicted
}

fn read_library_snapshot(db: &LibraryDb) -> LibrarySnapshot {
    LibrarySnapshot {
        stats: db.stats().unwrap_or_default(),
        songs: db.search_cached_songs("", 200).unwrap_or_default(),
        downloads: db.list_downloads().unwrap_or_default(),
        playlists: db.list_playlists().unwrap_or_default(),
        search_history: db.list_search_history_entries().unwrap_or_default(),
        liked_video_ids: db
            .list_liked_video_ids()
            .unwrap_or_default()
            .into_iter()
            .collect(),
    }
}
