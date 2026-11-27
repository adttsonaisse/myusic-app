//! Dedicated persistent SQLite worker actor for `bitchord-desktop`.
//!
//! Opens [`LibraryDb`] once on a dedicated background thread (`bitchord-db`),
//! prunes expired stream/lyrics caches at startup, serves all read/write
//! operations off the 35ms backend control loop and UI thread, and performs a
//! passive WAL checkpoint on clean shutdown.

use std::path::PathBuf;
use std::thread;

use async_channel::{Sender, unbounded};
use bitchord_automix::TrackAnalysis;
use bitchord_lyrics::{LyricLine, LyricsSource};
use bitchord_model::{SearchHistoryEntity, Song};
use bitchord_storage::{
    CachedLyrics, CachedStream, DbStats, LibraryDb, LocalPlaylist, SavedSongMetadata, SongMetadata,
};

use crate::backend::LibrarySnapshot;

enum DbOp {
    FetchLibrarySnapshot {
        reply: Sender<LibrarySnapshot>,
    },
    RecordSearchHistory {
        entity: SearchHistoryEntity,
        reply: Sender<Vec<SearchHistoryEntity>>,
    },
    RemoveSearchHistory {
        id: String,
        reply: Sender<Vec<SearchHistoryEntity>>,
    },
    ClearSearchHistory {
        reply: Sender<Vec<SearchHistoryEntity>>,
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
                }

                while let Ok(op) = rx.recv_blocking() {
                    match op {
                        DbOp::FetchLibrarySnapshot { reply } => {
                            let snap = db.as_ref().map(read_library_snapshot).unwrap_or_default();
                            let _ = reply.send_blocking(snap);
                        }
                        DbOp::RecordSearchHistory { entity, reply } => {
                            let list = if let Some(ref conn) = db {
                                let _ = conn.record_search_history(&entity);
                                conn.list_search_history().unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let _ = reply.send_blocking(list);
                        }
                        DbOp::RemoveSearchHistory { id, reply } => {
                            let list = if let Some(ref conn) = db {
                                let _ = conn.remove_search_history(&id);
                                conn.list_search_history().unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let _ = reply.send_blocking(list);
                        }
                        DbOp::ClearSearchHistory { reply } => {
                            let list = if let Some(ref conn) = db {
                                let _ = conn.clear_search_history();
                                conn.list_search_history().unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let _ = reply.send_blocking(list);
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
        entity: SearchHistoryEntity,
    ) -> Vec<SearchHistoryEntity> {
        let (reply, rx) = async_channel::bounded(1);
        if self
            .tx
            .send(DbOp::RecordSearchHistory { entity, reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.recv().await.unwrap_or_default()
    }

    pub async fn remove_search_history(&self, id: String) -> Vec<SearchHistoryEntity> {
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

    pub async fn clear_search_history(&self) -> Vec<SearchHistoryEntity> {
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

fn read_library_snapshot(db: &LibraryDb) -> LibrarySnapshot {
    LibrarySnapshot {
        stats: db.stats().unwrap_or_default(),
        songs: db.search_cached_songs("", 200).unwrap_or_default(),
        downloads: db.list_downloads().unwrap_or_default(),
        playlists: db.list_playlists().unwrap_or_default(),
        search_history: db.list_search_history().unwrap_or_default(),
    }
}
