//! App-level asynchronous backend orchestrator bridging `bitchord-audio`,
//! `bitchord-queue`, `bitchord-innertube`, `bitchord-lyrics`, `bitchord-storage`,
//! `bitchord-integrations`, and `MprisService` to GPUI views via lock-free
//! `async_channel` streams.
//!
//! Lives in GPUI's `App` global state so closing the window to the system tray
//! keeps playback, gapless preload, Listen Together, scrobbling, and MPRIS2 alive.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use bitchord_audio::{
    AlsaDeviceInfo, AudioDecoder, AudioEngine, AudioOutputMode, DecodedPacket, EqTuningSnapshot,
    MprisCommand, MprisService, MprisState, OUTPUT_SAMPLE_RATE, PlaybackSnapshot, PlaybackState,
    PlaybackStatus, RefusalCallback, build_mpris_metadata, enumerate_alsa_devices,
    open_http_decoder,
};
use bitchord_dsp_core::{EqCurve, EqLayout, EqualizerPreset, manual_curve};
use bitchord_innertube::{
    BrowseHeader, InnertubeClient, InnertubeParser, NativeStreamBackend, ResolveRequest,
    ResolvedStream, StreamBackend, StreamResolver, UploadedTracks, YtMusicRepository,
    build_shared_http_client,
};
#[cfg(feature = "ytdlp-bridge")]
use bitchord_innertube::{FallbackStreamBackend, YtDlpStreamBackend};
use bitchord_integrations::{
    Connection, DiscordActivity, DiscordIpcClient, ListenTogetherClient, ListenTogetherState,
    SpotifyImporter, discord_audio_quality_line,
};
use bitchord_jam::{JoinRequest, parse_invite_url};
use bitchord_lyrics::{
    LyricLine, LyricsCallbacks, LyricsRepository, LyricsRequest, LyricsSource, LyricsTranslator,
    TranslationResult,
};
use bitchord_model::{
    BrowseType, EntityType, HomeFeed, PlaybackSourceType, QueueTier, SearchFilter,
    SearchHistoryEntity, SearchResult, Song, duration_millis,
};
use bitchord_netstorage::{
    SmbClient, SmbConfig, WebDavClient, WebDavConfig, remote_song::extension_lower,
};
use bitchord_queue::{QueueSource, QueueTimeline};
use bitchord_storage::{
    AppPaths, AppSettingsData, AudioOutputPreference, DbStats, KeyringSecretStore, LibraryDb,
    LocalPlaylist, SavedSongMetadata, SecretKey, SecretStore, SettingsStore, SmbSettings,
    WebDavSettings, read_local_audio_tags,
};
use gpui::Global;
use reqwest::Client;
use tokio::runtime::Handle;

use crate::artwork::{RemoteArtworkAuth, song_thumbnail_url};

/// Selection of stream resolver mode (`ADR-004`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamBackendChoice {
    #[default]
    Auto,
    NativeOnly,
    #[cfg(feature = "ytdlp-bridge")]
    YtDlpOnly,
}

impl StreamBackendChoice {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (Native + yt-dlp fallback)",
            Self::NativeOnly => "Native InnerTube + QuickJS-NG",
            #[cfg(feature = "ytdlp-bridge")]
            Self::YtDlpOnly => "yt-dlp dev bridge",
        }
    }
}

/// Snapshot of queue state pushed to GPUI views.
#[derive(Debug, Clone, Default)]
pub struct QueueViewSnapshot {
    pub items: Vec<Song>,
    pub current_index: Option<usize>,
    pub shuffle: bool,
    pub repeat_all: bool,
    pub repeat_one: bool,
}

/// Snapshot of local library + downloads + history persisted in SQLite.
#[derive(Debug, Clone, Default)]
pub struct LibrarySnapshot {
    pub stats: DbStats,
    pub songs: Vec<Song>,
    pub downloads: Vec<SavedSongMetadata>,
    pub playlists: Vec<LocalPlaylist>,
    pub search_history: Vec<SearchHistoryEntity>,
}

/// Detail screen payload for an album, playlist, or artist browseId.
#[derive(Debug, Clone)]
pub struct DetailPayload {
    pub browse_id: String,
    pub header: Option<BrowseHeader>,
    pub songs: Vec<Song>,
    pub subtitle: String,
}

/// Resolved lyrics payload for the active track.
#[derive(Debug, Clone)]
pub struct LyricsPayload {
    pub video_id: String,
    pub source: LyricsSource,
    pub lines: Vec<LyricLine>,
    pub translated_lines: Option<Vec<LyricLine>>,
    pub translated_lang: Option<String>,
}

/// Live search suggestions + matching tracks/albums/artists while typing (`notes.md` #7).
#[derive(Debug, Clone, Default)]
pub struct SearchSuggestionsPayload {
    pub query: String,
    pub queries: Vec<String>,
    pub items: Vec<SearchResult>,
}

/// Inbound command sent by GPUI views, shortcuts, or tray menu to the backend.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    LoadHome,
    Search {
        query: String,
        filter: SearchFilter,
    },
    SearchSuggest(String),
    RecordSearchSelection(SearchHistoryEntity),
    RemoveSearchHistory(String),
    ClearSearchHistory,
    LoadDetail {
        browse_id: String,
        kind: BrowseType,
        fallback_title: String,
    },
    PlayContext {
        songs: Vec<Song>,
        start_index: usize,
        source_title: String,
        source_type: PlaybackSourceType,
        source_id: Option<String>,
    },
    PlayNext(Box<Song>),
    QueueJump(usize),
    QueueRemove(usize),
    QueueClear,
    ToggleShuffle,
    CycleRepeat,
    PlayPause,
    NextTrack,
    PrevTrack,
    SeekMs(u64),
    SeekDeltaMs(i64),
    SetVolume(f32),
    ToggleMute,
    SetSpeed(f32),
    SetSkipSilence(bool),
    SetSpatial(bool),
    SetCrossfadeSeconds(u32),
    SetEq {
        enabled: bool,
        preset: EqualizerPreset,
        bands_db: [f32; EqLayout::MANUAL_COUNT],
    },
    StartSleepTimerMinutes(u32),
    StartSleepAfterTrack,
    CancelSleepTimer,
    RefreshLibrary,
    ScanLocalFolder(PathBuf),
    TranslateLyrics {
        target_lang: String,
    },
    SaveYoutubeCookie(String),
    ClearYoutubeCookie,
    SaveWebDav {
        url: String,
        username: String,
        password: String,
    },
    TestWebDav {
        url: String,
        username: String,
        password: String,
    },
    SyncWebDav,
    SaveSmb {
        host: String,
        share: String,
        base_path: String,
        username: String,
        password: String,
    },
    TestSmb {
        host: String,
        share: String,
        base_path: String,
        username: String,
        password: String,
    },
    SyncSmb,
    SetStreamBackend(StreamBackendChoice),
    SetAudioOutput {
        mode: AudioOutputPreference,
        alsa_device: String,
    },
    RefreshAudioDevices,
    SetDiscordRpc(bool),
    ImportSpotifyPlaylist(String),
    PartyProbeServer(String),
    PartyCreate {
        server_url: String,
        nickname: String,
    },
    PartyJoin {
        server_url: String,
        code_or_url: String,
        nickname: String,
    },
    PartyLeave,
    Shutdown,
}

/// Outbound event pushed to GPUI views on discrete state transitions.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    AudioBackendReady {
        pipewire_active: bool,
        message: String,
    },
    AudioDevicesLoaded(Vec<AlsaDeviceInfo>),
    PlaybackUpdated(Box<PlaybackSnapshot>),
    QueueUpdated(QueueViewSnapshot),
    HomeLoading,
    HomeLoaded(Result<HomeFeed, String>),
    SearchLoading {
        query: String,
    },
    SearchLoaded {
        query: String,
        result: Result<Vec<SearchResult>, String>,
    },
    SearchSuggestionsLoaded(SearchSuggestionsPayload),
    DetailLoading,
    DetailLoaded(Result<DetailPayload, String>),
    LyricsLoading,
    LyricsLoaded {
        video_id: String,
        result: Result<LyricsPayload, String>,
    },
    LibraryLoaded(LibrarySnapshot),
    PartyServerProbed {
        server: String,
        online: bool,
        latency_ms: i64,
    },
    PartyUpdated(Box<ListenTogetherState>),
    StatusNotice(String),
    ErrorNotice(String),
}

/// Global handle stored inside GPUI `App` so windows can close/reopen without stopping playback.
#[derive(Clone)]
pub struct AppBackend {
    pub cmd_tx: Sender<BackendCommand>,
    pub evt_rx: Receiver<BackendEvent>,
    pub http: Client,
    pub paths: AppPaths,
    pub initial_settings: AppSettingsData,
    pub remote_auth: Arc<RwLock<RemoteArtworkAuth>>,
}

impl Global for AppBackend {}

impl AppBackend {
    /// Launches the background audio + network + storage + MPRIS2 runtime on Tokio.
    pub fn start(rt: Handle) -> Self {
        let paths = AppPaths::resolve();
        let settings_store = SettingsStore::new(paths.settings_file());
        let initial_settings = settings_store.load().unwrap_or_default();
        let http = build_shared_http_client();
        let remote_auth = Arc::new(RwLock::new(RemoteArtworkAuth {
            webdav: WebDavConfig::new(
                initial_settings.webdav.url.clone(),
                initial_settings.webdav.username.clone(),
                String::new(),
            ),
            smb: SmbConfig::new(
                initial_settings.smb.host.clone(),
                initial_settings.smb.share.clone(),
                initial_settings.smb.base_path.clone(),
                initial_settings.smb.username.clone(),
                String::new(),
            ),
        }));

        let (cmd_tx, cmd_rx) = async_channel::bounded::<BackendCommand>(128);
        let (evt_tx, evt_rx) = async_channel::bounded::<BackendEvent>(128);

        let worker_http = http.clone();
        let worker_paths = paths.clone();
        let worker_settings = initial_settings.clone();
        let worker_rt = rt.clone();
        let worker_remote_auth = Arc::clone(&remote_auth);

        thread::Builder::new()
            .name("bitchord-backend".to_owned())
            .spawn(move || {
                run_backend_thread(
                    worker_rt,
                    worker_http,
                    worker_paths,
                    worker_settings,
                    worker_remote_auth,
                    cmd_rx,
                    evt_tx,
                );
            })
            .expect("spawn bitchord backend thread");

        Self {
            cmd_tx,
            evt_rx,
            http,
            paths,
            initial_settings,
            remote_auth,
        }
    }

    pub fn send(&self, cmd: BackendCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }
}

struct PartyRuntime {
    client: ListenTogetherClient,
    code: String,
    token: String,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
}

/// Minimum decoded audio buffered off-thread before starting PipeWire playback
/// so the opening notes play cleanly from exact sample 0 without skipping or stuttering.
const INITIAL_PREBUFFER_MS: u64 = 600;

enum InternalTrackMsg {
    Ready {
        generation: u64,
        song: Song,
        decoder: Box<AudioDecoder>,
        prebuffered: Vec<DecodedPacket>,
        loudness_db: Option<f64>,
        source_label: String,
    },
    Failed {
        generation: u64,
        song_title: String,
        error: String,
    },
    PreloadReady {
        generation: u64,
        song: Song,
        decoder: Box<AudioDecoder>,
        prebuffered: Vec<DecodedPacket>,
        loudness_db: Option<f64>,
        source_label: String,
    },
}

fn to_engine_output_mode(pref: AudioOutputPreference, alsa_device: &str) -> AudioOutputMode {
    match pref {
        AudioOutputPreference::PipeWireAuto => AudioOutputMode::PipeWireAuto,
        AudioOutputPreference::PipeWireForceRate => AudioOutputMode::PipeWireForceRate,
        AudioOutputPreference::AlsaExclusive => {
            let dev = if alsa_device.trim().is_empty() {
                "hw:0,0".to_owned()
            } else {
                alsa_device.trim().to_owned()
            };
            AudioOutputMode::AlsaExclusive { device: dev }
        }
    }
}

fn build_stream_resolver(
    http: &Arc<Client>,
    innertube: &Arc<InnertubeClient>,
    uploads: &Arc<UploadedTracks>,
    choice: StreamBackendChoice,
) -> Arc<StreamResolver> {
    let backend: Arc<dyn StreamBackend> = match choice {
        StreamBackendChoice::NativeOnly => {
            Arc::new(NativeStreamBackend::new(Arc::clone(innertube)))
        }
        #[cfg(feature = "ytdlp-bridge")]
        StreamBackendChoice::YtDlpOnly => Arc::new(YtDlpStreamBackend::new()),
        StreamBackendChoice::Auto => {
            #[cfg(feature = "ytdlp-bridge")]
            {
                Arc::new(FallbackStreamBackend::new(
                    NativeStreamBackend::new(Arc::clone(innertube)),
                    YtDlpStreamBackend::new(),
                ))
            }
            #[cfg(not(feature = "ytdlp-bridge"))]
            {
                Arc::new(NativeStreamBackend::new(Arc::clone(innertube)))
            }
        }
    };
    Arc::new(StreamResolver::new(
        Arc::clone(http),
        backend,
        Arc::clone(uploads),
    ))
}

fn run_backend_thread(
    rt: Handle,
    http: Client,
    paths: AppPaths,
    mut settings: AppSettingsData,
    remote_auth: Arc<RwLock<RemoteArtworkAuth>>,
    cmd_rx: Receiver<BackendCommand>,
    evt_tx: Sender<BackendEvent>,
) {
    let desired_mode =
        to_engine_output_mode(settings.audio_output_mode, &settings.alsa_exclusive_device);
    let (mut engine, pipewire_active, audio_msg) =
        match AudioEngine::with_output_mode(desired_mode.clone(), OUTPUT_SAMPLE_RATE, None) {
            Ok(eng) => (eng, true, format!("{} active", desired_mode.label())),
            Err(primary_err) => match AudioEngine::with_pipewire() {
                Ok(eng) => (
                    eng,
                    true,
                    format!(
                        "{} unavailable ({primary_err}); fell back to PipeWire Auto",
                        desired_mode.label()
                    ),
                ),
                Err(err) => (
                    AudioEngine::with_null_sink(),
                    false,
                    format!("PipeWire unavailable ({err}); running in silent null-sink mode"),
                ),
            },
        };
    engine.set_volume(settings.volume);
    engine.set_muted(settings.muted);
    engine.set_crossfade_seconds(settings.crossfade_seconds);

    let _ = evt_tx.send_blocking(BackendEvent::AudioBackendReady {
        pipewire_active,
        message: audio_msg,
    });
    let _ = evt_tx.send_blocking(BackendEvent::AudioDevicesLoaded(enumerate_alsa_devices()));

    let http_arc = Arc::new(http.clone());
    let innertube = Arc::new(InnertubeClient::with_http_client(Arc::clone(&http_arc)));
    let parser = Arc::new(InnertubeParser::default());
    let yt_repo = Arc::new(YtMusicRepository::new(
        Arc::clone(&innertube),
        Arc::clone(&parser),
    ));
    let yt_lyrics_backend: Arc<dyn bitchord_lyrics::YouTubeLyricsBackend> = innertube.clone();
    let lyrics_repo = Arc::new(LyricsRepository::new().with_youtube_backend(yt_lyrics_backend));
    let lyrics_translator = Arc::new(LyricsTranslator::new(Some(
        paths.cache_dir.join("lyrics_translations"),
    )));
    let uploads = Arc::new(UploadedTracks::default());
    let settings_store = SettingsStore::new(paths.settings_file());

    // Persistent StreamResolver keeps the 20-minute URL cache & refused-profile exclusions warm!
    let mut resolver =
        build_stream_resolver(&http_arc, &innertube, &uploads, StreamBackendChoice::Auto);

    // Load saved credentials from Linux Secret Service (best-effort).
    let secret_store = rt.block_on(KeyringSecretStore::open()).ok();
    if let Some(ref store) = secret_store {
        if let Ok(Some(cookie)) = rt.block_on(store.get(SecretKey::YoutubeCookie)) {
            innertube.set_cookie(Some(cookie.expose().to_owned()));
        }
        let webdav_pw = rt
            .block_on(store.get(SecretKey::WebDavPassword))
            .ok()
            .flatten()
            .map(|s| s.expose().to_owned())
            .unwrap_or_default();
        let smb_pw = rt
            .block_on(store.get(SecretKey::SmbPassword))
            .ok()
            .flatten()
            .map(|s| s.expose().to_owned())
            .unwrap_or_default();
        if let Ok(mut guard) = remote_auth.write() {
            guard.webdav.password = webdav_pw;
            guard.smb.password = smb_pw;
        }
    }

    // Start MPRIS2 D-Bus server (`org.mpris.MediaPlayer2.bitchord`).
    let mpris_state = rt.block_on(async { MprisService::start().await.ok() });
    let (mpris_svc, mut mpris_rx) = match mpris_state {
        Some((svc, rx)) => (Some(Arc::new(svc)), Some(rx)),
        None => (None, None),
    };

    let (track_tx, track_rx) = async_channel::bounded::<InternalTrackMsg>(16);
    let load_gen = Arc::new(AtomicU64::new(0));

    let mut queue_items: Vec<Song> = Vec::new();
    let mut current_index: Option<usize> = None;
    let mut shuffle = false;
    let mut repeat_all = false;
    let mut repeat_one = false;
    let current_lyrics: Arc<Mutex<Option<LyricsPayload>>> = Arc::new(Mutex::new(None));
    let mut party_runtime: Option<PartyRuntime> = None;
    let party_state_shared: Arc<Mutex<ListenTogetherState>> =
        Arc::new(Mutex::new(ListenTogetherState::default()));

    // Initial SQLite library load.
    emit_library_snapshot(&paths, &evt_tx);

    let mut last_tick = std::time::Instant::now();
    let mut last_state = PlaybackState::Stopped;
    let mut last_song_vid: Option<String> = None;
    let mut last_published_pos_ms = 0u64;

    loop {
        // 1. Wait reactively for either an inbound UI command or an off-thread track load completion.
        let wait_ms = if engine.snapshot().state == PlaybackState::Playing {
            35
        } else {
            90
        };
        let mut cmds = Vec::new();
        let mut track_msgs = Vec::new();

        rt.block_on(async {
            tokio::select! {
                res = cmd_rx.recv() => {
                    if let Ok(c) = res {
                        cmds.push(c);
                    }
                }
                res = track_rx.recv() => {
                    if let Ok(m) = res {
                        track_msgs.push(m);
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
            }
        });
        while let Ok(c) = cmd_rx.try_recv() {
            cmds.push(c);
        }
        while let Ok(m) = track_rx.try_recv() {
            track_msgs.push(m);
        }

        // 2. Drain inbound MPRIS2 D-Bus commands.
        if let Some(ref mut rx) = mpris_rx {
            while let Ok(mcmd) = rx.try_recv() {
                match mcmd {
                    MprisCommand::Play => engine.play(),
                    MprisCommand::Pause => engine.pause(),
                    MprisCommand::PlayPause => engine.toggle_play_pause(),
                    MprisCommand::Stop => {
                        load_gen.fetch_add(1, Ordering::AcqRel);
                        engine.stop();
                    }
                    MprisCommand::Next => cmds.push(BackendCommand::NextTrack),
                    MprisCommand::Previous => cmds.push(BackendCommand::PrevTrack),
                    MprisCommand::SetVolume(v) => cmds.push(BackendCommand::SetVolume(v as f32)),
                    MprisCommand::SetRate(r) => cmds.push(BackendCommand::SetSpeed(r as f32)),
                    MprisCommand::SeekRelativeUs(us) => {
                        cmds.push(BackendCommand::SeekDeltaMs(us / 1_000));
                    }
                    MprisCommand::SetPositionUs { position_us, .. } => {
                        cmds.push(BackendCommand::SeekMs((position_us / 1_000).max(0) as u64));
                    }
                    _ => {}
                }
            }
        }

        // 3. Apply completed off-thread track loads / preloads.
        for msg in track_msgs {
            match msg {
                InternalTrackMsg::Ready {
                    generation,
                    mut song,
                    decoder,
                    prebuffered,
                    loudness_db,
                    source_label,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation {
                        continue; // Superseded by a newer user click!
                    }
                    if song.duration_text.as_deref().is_none_or(str::is_empty)
                        && let Some(dur_ms) = decoder.info().duration_ms.filter(|&d| d > 0)
                    {
                        let secs = dur_ms / 1_000;
                        song.duration_text = Some(format!("{}:{:02}", secs / 60, secs % 60));
                    }
                    match engine.load_prepared_decoder(
                        *decoder,
                        prebuffered,
                        song.clone(),
                        loudness_db,
                        Some(&source_label),
                    ) {
                        Ok(info) => {
                            // Confirm pre-decoded opening frames are committed to rtrb
                            // (after RT FlushDeck ack!) before unpausing PipeWire.
                            engine.wait_prebuffer(500, Duration::from_millis(600));
                            engine.play();
                            let snap = engine.snapshot();
                            last_state = snap.state;
                            last_song_vid = Some(song.video_id.clone());
                            last_published_pos_ms = snap.position_ms;
                            let _ = evt_tx
                                .try_send(BackendEvent::PlaybackUpdated(Box::new(snap.clone())));
                            let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                                "Playing {} - {}",
                                song.title, song.artist
                            )));

                            record_track_started(
                                &rt,
                                &paths,
                                &song,
                                info.duration_ms,
                                settings.discord_rpc_enabled,
                                &snap,
                                &evt_tx,
                            );

                            // Quietly pre-resolve & preload the next track in the queue onto standby deck.
                            if let Some(next_idx) = compute_next_index(
                                current_index,
                                queue_items.len(),
                                repeat_all,
                                repeat_one,
                                true,
                            ) && Some(next_idx) != current_index
                                && let Some(next_song) = queue_items.get(next_idx).cloned()
                            {
                                spawn_preload_next(
                                    generation,
                                    Arc::clone(&load_gen),
                                    &rt,
                                    Arc::clone(&http_arc),
                                    Arc::clone(&resolver),
                                    Arc::clone(&remote_auth),
                                    next_song,
                                    track_tx.clone(),
                                );
                            }
                        }
                        Err(err) => {
                            let _ = evt_tx.try_send(BackendEvent::ErrorNotice(format!(
                                "Playback failed for \"{}\": {err}",
                                song.title
                            )));
                        }
                    }
                }
                InternalTrackMsg::Failed {
                    generation,
                    song_title,
                    error,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation {
                        continue;
                    }
                    let _ = evt_tx.try_send(BackendEvent::ErrorNotice(format!(
                        "Playback failed for \"{song_title}\": {error}"
                    )));
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                InternalTrackMsg::PreloadReady {
                    generation,
                    song,
                    decoder,
                    prebuffered,
                    loudness_db,
                    source_label,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation {
                        continue;
                    }
                    let _ = engine.preload_prepared_decoder(
                        *decoder,
                        prebuffered,
                        song,
                        loudness_db,
                        Some(&source_label),
                    );
                }
            }
        }

        // 4. Process all queued commands.
        for cmd in cmds {
            match cmd {
                BackendCommand::Shutdown => {
                    load_gen.fetch_add(1, Ordering::AcqRel);
                    engine.stop();
                    if let Some(pr) = party_runtime.take() {
                        pr.stop_flag.store(true, Ordering::Release);
                    }
                    return;
                }
                BackendCommand::LoadHome => {
                    let _ = evt_tx.try_send(BackendEvent::HomeLoading);
                    let repo = Arc::clone(&yt_repo);
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let res = repo.home().await.map_err(|e| e.to_string());
                        let _ = tx.send(BackendEvent::HomeLoaded(res)).await;
                    });
                }
                BackendCommand::Search { query, filter } => {
                    let trimmed = query.trim().to_owned();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let _ = evt_tx.try_send(BackendEvent::SearchLoading {
                        query: trimmed.clone(),
                    });
                    let repo = Arc::clone(&yt_repo);
                    let tx = evt_tx.clone();
                    let paths_c = paths.clone();
                    rt.spawn(async move {
                        let res = repo
                            .search(&trimmed, filter, false)
                            .await
                            .map(|p| p.rows)
                            .map_err(|e| e.to_string());

                        // Record query search enriched with top result's artwork & metadata when available!
                        if let Ok(ref rows) = res {
                            let (art_url, subtitle) = match rows.first() {
                                Some(SearchResult::TopTrack { song })
                                | Some(SearchResult::Track { song }) => (
                                    song_thumbnail_url(song),
                                    format!("Search · Top: {} - {}", song.title, song.artist),
                                ),
                                Some(SearchResult::Browse { item }) => (
                                    item.thumbnail_url.clone(),
                                    format!("Search · {}", item.title),
                                ),
                                None => (None, "Search query".to_owned()),
                            };
                            if let Ok(db) = LibraryDb::open(&paths_c.db_file()) {
                                let _ = db.record_search_history(&SearchHistoryEntity {
                                    id: format!("q:{trimmed}"),
                                    title: trimmed.clone(),
                                    subtitle,
                                    artwork_url: art_url,
                                    entity_type: EntityType::Other,
                                    timestamp: unix_now_ms(),
                                });
                            }
                            emit_library_snapshot(&paths_c, &tx);
                        }

                        let _ = tx
                            .send(BackendEvent::SearchLoaded {
                                query: trimmed,
                                result: res,
                            })
                            .await;
                    });
                }
                BackendCommand::SearchSuggest(query) => {
                    let trimmed = query.trim().to_owned();
                    if trimmed.len() < 2 {
                        let _ = evt_tx.try_send(BackendEvent::SearchSuggestionsLoaded(
                            SearchSuggestionsPayload::default(),
                        ));
                        continue;
                    }
                    let repo = Arc::clone(&yt_repo);
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let (queries_res, typeahead_res) = tokio::join!(
                            repo.search_suggestions(&trimmed),
                            repo.search_typeahead(&trimmed)
                        );
                        let queries = queries_res.unwrap_or_default();
                        let mut items = typeahead_res.map(|p| p.rows).unwrap_or_default();
                        items.truncate(6);
                        let _ = tx
                            .send(BackendEvent::SearchSuggestionsLoaded(
                                SearchSuggestionsPayload {
                                    query: trimmed,
                                    queries,
                                    items,
                                },
                            ))
                            .await;
                    });
                }
                BackendCommand::RecordSearchSelection(mut entity) => {
                    entity.timestamp = unix_now_ms();
                    if let Ok(db) = LibraryDb::open(&paths.db_file()) {
                        let _ = db.record_search_history(&entity);
                    }
                    emit_library_snapshot(&paths, &evt_tx);
                }
                BackendCommand::RemoveSearchHistory(id) => {
                    if let Ok(db) = LibraryDb::open(&paths.db_file()) {
                        let _ = db.remove_search_history(&id);
                    }
                    emit_library_snapshot(&paths, &evt_tx);
                }
                BackendCommand::ClearSearchHistory => {
                    if let Ok(db) = LibraryDb::open(&paths.db_file()) {
                        let _ = db.clear_search_history();
                    }
                    emit_library_snapshot(&paths, &evt_tx);
                }
                BackendCommand::LoadDetail {
                    browse_id,
                    kind,
                    fallback_title,
                } => {
                    let _ = evt_tx.try_send(BackendEvent::DetailLoading);
                    let repo = Arc::clone(&yt_repo);
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let res = if kind == BrowseType::Artist {
                            repo.artist_page(&browse_id).await.map(|page| {
                                let title = page.name.unwrap_or(fallback_title);
                                let subtitle = page
                                    .subscriber_count_text
                                    .unwrap_or_else(|| "Artist".to_owned());
                                DetailPayload {
                                    browse_id: browse_id.clone(),
                                    header: Some(BrowseHeader {
                                        title,
                                        subtitle: subtitle.clone(),
                                        thumbnail_url: page.thumbnail_url,
                                    }),
                                    songs: page.songs,
                                    subtitle,
                                }
                            })
                        } else {
                            repo.browse_songs(&browse_id, None)
                                .await
                                .map(|(header, songs, _)| {
                                    let subtitle = header
                                        .as_ref()
                                        .map(|h| h.subtitle.clone())
                                        .unwrap_or_default();
                                    DetailPayload {
                                        browse_id: browse_id.clone(),
                                        header,
                                        songs,
                                        subtitle,
                                    }
                                })
                        };
                        let _ = tx
                            .send(BackendEvent::DetailLoaded(res.map_err(|e| e.to_string())))
                            .await;
                    });
                }
                BackendCommand::PlayContext {
                    songs,
                    start_index,
                    source_title,
                    source_type,
                    source_id,
                } => {
                    if songs.is_empty() {
                        continue;
                    }
                    let clamped_start = start_index.min(songs.len() - 1);
                    let source = QueueSource {
                        title: source_title,
                        r#type: source_type,
                        id: source_id,
                    };
                    let ctx = QueueTimeline::build_context_queue(
                        &queue_items,
                        current_index.map_or(-1, |i| i as isize),
                        &songs,
                        clamped_start,
                        &source,
                    );
                    queue_items = ctx.timeline;
                    current_index = Some(ctx.start_index);
                    emit_queue(
                        &queue_items,
                        current_index,
                        shuffle,
                        repeat_all,
                        repeat_one,
                        &evt_tx,
                    );
                    if let Some(song) = current_index.and_then(|i| queue_items.get(i)).cloned() {
                        start_async_song_load(
                            &mut engine,
                            &load_gen,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &lyrics_repo,
                            &current_lyrics,
                            song,
                            &track_tx,
                            &evt_tx,
                        );
                    }
                }
                BackendCommand::PlayNext(song) => {
                    let entry = QueueTimeline::as_queue_entry(*song, QueueTier::UserQueue);
                    if queue_items.is_empty() {
                        queue_items.push(entry);
                        current_index = Some(0);
                        emit_queue(
                            &queue_items,
                            current_index,
                            shuffle,
                            repeat_all,
                            repeat_one,
                            &evt_tx,
                        );
                        if let Some(s) = queue_items.first().cloned() {
                            start_async_song_load(
                                &mut engine,
                                &load_gen,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &lyrics_repo,
                                &current_lyrics,
                                s,
                                &track_tx,
                                &evt_tx,
                            );
                        }
                    } else {
                        let insert_at = QueueTimeline::find_user_queue_insertion_index(
                            &queue_items,
                            current_index.map_or(-1, |i| i as isize),
                            true,
                        );
                        queue_items.insert(insert_at, entry);
                        emit_queue(
                            &queue_items,
                            current_index,
                            shuffle,
                            repeat_all,
                            repeat_one,
                            &evt_tx,
                        );
                        let _ = evt_tx
                            .try_send(BackendEvent::StatusNotice("Added to Play Next".to_owned()));
                    }
                }
                BackendCommand::QueueJump(idx) => {
                    if idx < queue_items.len() {
                        current_index = Some(idx);
                        emit_queue(
                            &queue_items,
                            current_index,
                            shuffle,
                            repeat_all,
                            repeat_one,
                            &evt_tx,
                        );
                        let song = queue_items[idx].clone();
                        start_async_song_load(
                            &mut engine,
                            &load_gen,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &lyrics_repo,
                            &current_lyrics,
                            song,
                            &track_tx,
                            &evt_tx,
                        );
                    }
                }
                BackendCommand::QueueRemove(idx) => {
                    if idx < queue_items.len() {
                        queue_items.remove(idx);
                        if let Some(cur) = current_index {
                            if queue_items.is_empty() {
                                current_index = None;
                                load_gen.fetch_add(1, Ordering::AcqRel);
                                engine.stop();
                            } else if idx < cur {
                                current_index = Some(cur - 1);
                            } else if idx == cur {
                                let next_i = cur.min(queue_items.len() - 1);
                                current_index = Some(next_i);
                                let song = queue_items[next_i].clone();
                                start_async_song_load(
                                    &mut engine,
                                    &load_gen,
                                    &rt,
                                    &http_arc,
                                    &resolver,
                                    &remote_auth,
                                    &lyrics_repo,
                                    &current_lyrics,
                                    song,
                                    &track_tx,
                                    &evt_tx,
                                );
                            }
                        }
                        emit_queue(
                            &queue_items,
                            current_index,
                            shuffle,
                            repeat_all,
                            repeat_one,
                            &evt_tx,
                        );
                    }
                }
                BackendCommand::QueueClear => {
                    queue_items.clear();
                    current_index = None;
                    load_gen.fetch_add(1, Ordering::AcqRel);
                    engine.stop();
                    emit_queue(
                        &queue_items,
                        current_index,
                        shuffle,
                        repeat_all,
                        repeat_one,
                        &evt_tx,
                    );
                }
                BackendCommand::ToggleShuffle => {
                    shuffle = !shuffle;
                    if shuffle && queue_items.len() > 2 {
                        let cur = current_index.unwrap_or(0);
                        let seed = unix_now_ms() as u64;
                        queue_items =
                            QueueTimeline::shuffled_starting_order(&queue_items, cur, seed);
                        current_index = Some(0);
                    }
                    emit_queue(
                        &queue_items,
                        current_index,
                        shuffle,
                        repeat_all,
                        repeat_one,
                        &evt_tx,
                    );
                }
                BackendCommand::CycleRepeat => {
                    match (repeat_all, repeat_one) {
                        (false, false) => {
                            repeat_all = true;
                            repeat_one = false;
                        }
                        (true, false) => {
                            repeat_all = false;
                            repeat_one = true;
                        }
                        _ => {
                            repeat_all = false;
                            repeat_one = false;
                        }
                    }
                    emit_queue(
                        &queue_items,
                        current_index,
                        shuffle,
                        repeat_all,
                        repeat_one,
                        &evt_tx,
                    );
                }
                BackendCommand::PlayPause => {
                    engine.toggle_play_pause();
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::NextTrack => {
                    if let Some(next_idx) = compute_next_index(
                        current_index,
                        queue_items.len(),
                        repeat_all,
                        repeat_one,
                        false,
                    ) {
                        current_index = Some(next_idx);
                        emit_queue(
                            &queue_items,
                            current_index,
                            shuffle,
                            repeat_all,
                            repeat_one,
                            &evt_tx,
                        );
                        let song = queue_items[next_idx].clone();
                        start_async_song_load(
                            &mut engine,
                            &load_gen,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &lyrics_repo,
                            &current_lyrics,
                            song,
                            &track_tx,
                            &evt_tx,
                        );
                    }
                }
                BackendCommand::PrevTrack => {
                    if engine.position_ms() > 3_500 {
                        let _ = engine.seek_ms(0);
                    } else if let Some(cur) = current_index {
                        let prev = if cur > 0 {
                            cur - 1
                        } else if repeat_all && !queue_items.is_empty() {
                            queue_items.len() - 1
                        } else {
                            0
                        };
                        current_index = Some(prev);
                        emit_queue(
                            &queue_items,
                            current_index,
                            shuffle,
                            repeat_all,
                            repeat_one,
                            &evt_tx,
                        );
                        if let Some(song) = queue_items.get(prev).cloned() {
                            start_async_song_load(
                                &mut engine,
                                &load_gen,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &lyrics_repo,
                                &current_lyrics,
                                song,
                                &track_tx,
                                &evt_tx,
                            );
                        }
                    }
                }
                BackendCommand::SeekMs(ms) => {
                    if let Ok(actual) = engine.seek_ms(ms) {
                        if let Some(ref svc) = mpris_svc {
                            let s = Arc::clone(svc);
                            rt.spawn(async move {
                                let _ = s.notify_seeked(actual).await;
                            });
                        }
                        let _ = evt_tx
                            .try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                }
                BackendCommand::SeekDeltaMs(delta) => {
                    let cur = engine.position_ms() as i64;
                    let target = (cur + delta).max(0) as u64;
                    if let Ok(actual) = engine.seek_ms(target) {
                        if let Some(ref svc) = mpris_svc {
                            let s = Arc::clone(svc);
                            rt.spawn(async move {
                                let _ = s.notify_seeked(actual).await;
                            });
                        }
                        let _ = evt_tx
                            .try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                }
                BackendCommand::SetVolume(vol) => {
                    engine.set_volume(vol);
                    settings.volume = vol.clamp(0.0, 1.0);
                    let _ = settings_store.save(&settings);
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::ToggleMute => {
                    let next_mute = !engine.snapshot().muted;
                    engine.set_muted(next_mute);
                    settings.muted = next_mute;
                    let _ = settings_store.save(&settings);
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetSpeed(speed) => {
                    engine.set_speed(speed);
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetSkipSilence(on) => {
                    engine.set_skip_silence(on);
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetSpatial(on) => {
                    engine.set_spatial_audio(on);
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetCrossfadeSeconds(secs) => {
                    engine.set_crossfade_seconds(secs);
                    settings.crossfade_seconds = secs;
                    let _ = settings_store.save(&settings);
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetEq {
                    enabled,
                    preset,
                    bands_db,
                } => {
                    let curve: EqCurve = if preset == EqualizerPreset::Custom {
                        manual_curve(&bands_db)
                    } else {
                        manual_curve(preset.bands_db())
                    };
                    engine.set_eq(EqTuningSnapshot::from_curve(enabled, &curve, 0.0));
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::StartSleepTimerMinutes(mins) => {
                    engine.sleep_timer_mut().start(mins);
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                        "Sleep timer armed for {mins} min"
                    )));
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::StartSleepAfterTrack => {
                    engine.sleep_timer_mut().start_after_track();
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(
                        "Sleep timer armed: pause after current track".to_owned(),
                    ));
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::CancelSleepTimer => {
                    engine.sleep_timer_mut().cancel();
                    let _ =
                        evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::RefreshLibrary => {
                    emit_library_snapshot(&paths, &evt_tx);
                }
                BackendCommand::ScanLocalFolder(folder) => {
                    let count = scan_local_folder_into_db(&folder, &paths);
                    match count {
                        Ok(n) => {
                            if !settings.local_music_folders.contains(&folder) {
                                settings.local_music_folders.push(folder.clone());
                                let _ = settings_store.save(&settings);
                            }
                            emit_library_snapshot(&paths, &evt_tx);
                            let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                                "Scanned {} audio file(s) from {}",
                                n,
                                folder.display()
                            )));
                        }
                        Err(err) => {
                            let _ = evt_tx.try_send(BackendEvent::ErrorNotice(err));
                        }
                    }
                }
                BackendCommand::TranslateLyrics { target_lang } => {
                    let current = current_lyrics
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    if let Some(payload) = current {
                        let translator = Arc::clone(&lyrics_translator);
                        let tx = evt_tx.clone();
                        let lyrics_slot = Arc::clone(&current_lyrics);
                        rt.spawn(async move {
                            match translator
                                .translate(&payload.video_id, &payload.lines, &target_lang)
                                .await
                            {
                                TranslationResult::Translated { lines, .. } => {
                                    let updated = LyricsPayload {
                                        translated_lines: Some(lines),
                                        translated_lang: Some(target_lang),
                                        ..payload
                                    };
                                    *lyrics_slot.lock().unwrap_or_else(|e| e.into_inner()) =
                                        Some(updated.clone());
                                    let _ = tx
                                        .send(BackendEvent::LyricsLoaded {
                                            video_id: updated.video_id.clone(),
                                            result: Ok(updated),
                                        })
                                        .await;
                                }
                                TranslationResult::SameLanguage { language } => {
                                    let _ = tx
                                        .send(BackendEvent::StatusNotice(format!(
                                            "Lyrics are already in {language}"
                                        )))
                                        .await;
                                }
                                TranslationResult::Unavailable => {
                                    let _ = tx
                                        .send(BackendEvent::ErrorNotice(
                                            "Lyrics translation unavailable for this track."
                                                .to_owned(),
                                        ))
                                        .await;
                                }
                            }
                        });
                    }
                }
                BackendCommand::SaveYoutubeCookie(cookie) => {
                    let trimmed = cookie.trim().to_owned();
                    if trimmed.is_empty() {
                        continue;
                    }
                    innertube.set_cookie(Some(trimmed.clone()));
                    let res = match secret_store {
                        Some(ref store) => {
                            rt.block_on(store.set(SecretKey::YoutubeCookie, &trimmed))
                        }
                        None => Err(bitchord_storage::SecretError::Keyring(
                            "Secret Service daemon unavailable".to_owned(),
                        )),
                    };
                    match res {
                        Ok(()) => {
                            let _ = evt_tx.try_send(BackendEvent::StatusNotice(
                                "YouTube Music cookie saved in Linux Secret Service".to_owned(),
                            ));
                        }
                        Err(err) => {
                            let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                                "Cookie active for this session (keyring unavailable: {err})"
                            )));
                        }
                    }
                }
                BackendCommand::ClearYoutubeCookie => {
                    innertube.set_cookie(None);
                    if let Some(ref store) = secret_store {
                        let _ = rt.block_on(store.delete(SecretKey::YoutubeCookie));
                    }
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(
                        "YouTube Music session cleared".to_owned(),
                    ));
                }
                BackendCommand::SaveWebDav {
                    url,
                    username,
                    password,
                } => {
                    let norm_url = WebDavConfig::normalize_url(&url);
                    let user_trimmed = username.trim().to_owned();
                    settings.webdav = WebDavSettings {
                        url: norm_url.clone(),
                        username: user_trimmed.clone(),
                    };
                    let _ = settings_store.save(&settings);
                    let effective_pw = if password.is_empty() {
                        remote_auth
                            .read()
                            .map(|g| g.webdav.password.clone())
                            .unwrap_or_default()
                    } else {
                        if let Some(ref store) = secret_store {
                            let _ = rt.block_on(store.set(SecretKey::WebDavPassword, &password));
                        }
                        password
                    };
                    if let Ok(mut guard) = remote_auth.write() {
                        guard.webdav = WebDavConfig::new(norm_url, user_trimmed, effective_pw);
                    }
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(
                        "WebDAV configuration saved (password stored in Secret Service)".to_owned(),
                    ));
                }
                BackendCommand::TestWebDav {
                    url,
                    username,
                    password,
                } => {
                    let effective_pw = if password.is_empty() {
                        remote_auth
                            .read()
                            .map(|g| g.webdav.password.clone())
                            .unwrap_or_default()
                    } else {
                        password
                    };
                    let cfg = WebDavConfig::new(url, username.trim(), effective_pw);
                    let http_c = http.clone();
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let client = WebDavClient::with_http(http_c);
                        match client.test_connection(&cfg).await {
                            Ok(()) => {
                                let _ = tx
                                    .send(BackendEvent::StatusNotice(format!(
                                        "WebDAV connected: {}",
                                        WebDavConfig::normalize_url(&cfg.url)
                                    )))
                                    .await;
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(BackendEvent::ErrorNotice(format!(
                                        "WebDAV test failed: {err}"
                                    )))
                                    .await;
                            }
                        }
                    });
                }
                BackendCommand::SyncWebDav => {
                    let cfg = remote_auth
                        .read()
                        .map(|g| g.webdav.clone())
                        .unwrap_or_default();
                    let http_c = http.clone();
                    let paths_c = paths.clone();
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let _ = tx
                            .send(BackendEvent::StatusNotice(
                                "Syncing WebDAV music library...".to_owned(),
                            ))
                            .await;
                        let client = WebDavClient::with_http(http_c);
                        match client
                            .list_songs(&cfg, bitchord_netstorage::webdav::DEFAULT_MAX_FILES)
                            .await
                        {
                            Ok(songs) => {
                                let count = match LibraryDb::open(&paths_c.db_file()) {
                                    Ok(db) => {
                                        db.replace_prefixed_songs("webdav:", &songs).unwrap_or(0)
                                    }
                                    Err(_) => 0,
                                };
                                emit_library_snapshot(&paths_c, &tx);
                                let _ = tx
                                    .send(BackendEvent::StatusNotice(format!(
                                        "Synced {count} WebDAV track(s) into Library"
                                    )))
                                    .await;
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(BackendEvent::ErrorNotice(format!(
                                        "WebDAV sync failed: {err}"
                                    )))
                                    .await;
                            }
                        }
                    });
                }
                BackendCommand::SaveSmb {
                    host,
                    share,
                    base_path,
                    username,
                    password,
                } => {
                    let host_t = host.trim().to_owned();
                    let share_t = share.trim().trim_matches(['/', '\\']).to_owned();
                    let base_t = base_path
                        .trim()
                        .replace('\\', "/")
                        .trim_matches('/')
                        .to_owned();
                    let user_t = username.trim().to_owned();
                    settings.smb = SmbSettings {
                        host: host_t.clone(),
                        share: share_t.clone(),
                        base_path: base_t.clone(),
                        username: user_t.clone(),
                    };
                    let _ = settings_store.save(&settings);
                    let effective_pw = if password.is_empty() {
                        remote_auth
                            .read()
                            .map(|g| g.smb.password.clone())
                            .unwrap_or_default()
                    } else {
                        if let Some(ref store) = secret_store {
                            let _ = rt.block_on(store.set(SecretKey::SmbPassword, &password));
                        }
                        password
                    };
                    if let Ok(mut guard) = remote_auth.write() {
                        guard.smb = SmbConfig::new(host_t, share_t, base_t, user_t, effective_pw);
                    }
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(
                        "SMB configuration saved (password stored in Secret Service)".to_owned(),
                    ));
                }
                BackendCommand::TestSmb {
                    host,
                    share,
                    base_path,
                    username,
                    password,
                } => {
                    let effective_pw = if password.is_empty() {
                        remote_auth
                            .read()
                            .map(|g| g.smb.password.clone())
                            .unwrap_or_default()
                    } else {
                        password
                    };
                    let cfg = SmbConfig::new(host, share, base_path, username.trim(), effective_pw);
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        match SmbClient::new().test_connection(&cfg).await {
                            Ok(()) => {
                                let _ = tx
                                    .send(BackendEvent::StatusNotice(format!(
                                        "SMB connected: //{}/{}",
                                        SmbConfig::normalize_host(&cfg.host),
                                        cfg.normalized_share()
                                    )))
                                    .await;
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(BackendEvent::ErrorNotice(format!(
                                        "SMB test failed: {err}"
                                    )))
                                    .await;
                            }
                        }
                    });
                }
                BackendCommand::SyncSmb => {
                    let cfg = remote_auth
                        .read()
                        .map(|g| g.smb.clone())
                        .unwrap_or_default();
                    let paths_c = paths.clone();
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let _ = tx
                            .send(BackendEvent::StatusNotice(
                                "Syncing SMB2/3 music share...".to_owned(),
                            ))
                            .await;
                        match SmbClient::new()
                            .list_songs(&cfg, bitchord_netstorage::smb::SMB_DEFAULT_MAX_FILES)
                            .await
                        {
                            Ok(songs) => {
                                let count = match LibraryDb::open(&paths_c.db_file()) {
                                    Ok(db) => {
                                        db.replace_prefixed_songs("smb:", &songs).unwrap_or(0)
                                    }
                                    Err(_) => 0,
                                };
                                emit_library_snapshot(&paths_c, &tx);
                                let _ = tx
                                    .send(BackendEvent::StatusNotice(format!(
                                        "Synced {count} SMB track(s) into Library"
                                    )))
                                    .await;
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(BackendEvent::ErrorNotice(format!(
                                        "SMB sync failed: {err}"
                                    )))
                                    .await;
                            }
                        }
                    });
                }
                BackendCommand::SetStreamBackend(choice) => {
                    resolver = build_stream_resolver(&http_arc, &innertube, &uploads, choice);
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                        "Stream resolver set to {}",
                        choice.label()
                    )));
                }
                BackendCommand::SetAudioOutput { mode, alsa_device } => {
                    let clean_dev = alsa_device.trim().to_owned();
                    let engine_mode = to_engine_output_mode(mode, &clean_dev);
                    match engine.set_output_mode(engine_mode.clone(), None, None) {
                        Ok(()) => {
                            settings.audio_output_mode = mode;
                            if !clean_dev.is_empty() {
                                settings.alsa_exclusive_device = clean_dev;
                            }
                            let _ = settings_store.save(&settings);
                            let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                                "Audio output switched to {}",
                                engine_mode.label()
                            )));
                            let _ = evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(
                                engine.snapshot(),
                            )));
                        }
                        Err(err) => {
                            let _ = evt_tx.try_send(BackendEvent::ErrorNotice(format!(
                                "Cannot switch output to {}: {err}",
                                engine_mode.label()
                            )));
                        }
                    }
                }
                BackendCommand::RefreshAudioDevices => {
                    let devices = enumerate_alsa_devices();
                    let count = devices.len();
                    let _ = evt_tx.try_send(BackendEvent::AudioDevicesLoaded(devices));
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                        "Detected {count} ALSA hardware playback endpoint(s)"
                    )));
                }
                BackendCommand::SetDiscordRpc(enabled) => {
                    settings.discord_rpc_enabled = enabled;
                    let _ = settings_store.save(&settings);
                    if !enabled {
                        rt.spawn(async {
                            if let Ok(mut client) = DiscordIpcClient::connect_default().await {
                                let _ = client.clear_activity().await;
                            }
                        });
                    }
                }
                BackendCommand::ImportSpotifyPlaylist(url_or_id) => {
                    let http_c = http.clone();
                    let db_path = paths.db_file();
                    let paths_c = paths.clone();
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        let pid = SpotifyImporter::extract_playlist_id(&url_or_id)
                            .unwrap_or_else(|| url_or_id.trim().to_owned());
                        match SpotifyImporter::fetch_playlist_tracks(&http_c, &pid).await {
                            Ok((title, tracks)) => {
                                let imported_count = tracks.len();
                                if let Ok(mut db) = LibraryDb::open(&db_path) {
                                    let now = unix_now_ms();
                                    let pl_id = format!("spotify:{pid}");
                                    let _ = db.upsert_playlist(&LocalPlaylist {
                                        id: pl_id.clone(),
                                        title: title.clone(),
                                        description: format!(
                                            "Imported from Spotify ({imported_count} tracks)"
                                        ),
                                        thumbnail_url: None,
                                        source: "spotify".to_owned(),
                                        created_at: now,
                                        updated_at: now,
                                    });
                                    let mut ids = Vec::with_capacity(tracks.len());
                                    for (idx, tr) in tracks.into_iter().enumerate() {
                                        let vid = format!("sp:{pid}:{idx}");
                                        let song = Song::new(&vid, tr.title, tr.artist, None);
                                        let _ = db.upsert_song(&song);
                                        ids.push(vid);
                                    }
                                    let _ = db.set_playlist_tracks(&pl_id, &ids);
                                }
                                emit_library_snapshot(&paths_c, &tx);
                                let _ = tx
                                    .send(BackendEvent::StatusNotice(format!(
                                        "Imported Spotify playlist \"{title}\" ({imported_count} tracks)"
                                    )))
                                    .await;
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(BackendEvent::ErrorNotice(format!(
                                        "Spotify import failed: {err}"
                                    )))
                                    .await;
                            }
                        }
                    });
                }
                BackendCommand::PartyProbeServer(server_url) => {
                    let http_c = http.clone();
                    let tx = evt_tx.clone();
                    rt.spawn(async move {
                        match ListenTogetherClient::new(http_c, &server_url) {
                            Ok(client) => {
                                let probe = client.healthz(Duration::from_secs(6)).await;
                                let _ = tx
                                    .send(BackendEvent::PartyServerProbed {
                                        server: client.base_url().to_owned(),
                                        online: probe.is_online,
                                        latency_ms: probe.latency_ms,
                                    })
                                    .await;
                            }
                            Err(err) => {
                                let _ = tx.send(BackendEvent::ErrorNotice(err.to_string())).await;
                            }
                        }
                    });
                }
                BackendCommand::PartyCreate {
                    server_url,
                    nickname,
                } => {
                    if let Some(prev) = party_runtime.take() {
                        prev.stop_flag.store(true, Ordering::Release);
                    }
                    settings.listen_together_server = server_url.clone();
                    settings.listen_together_nickname = nickname.clone();
                    let _ = settings_store.save(&settings);

                    let req = JoinRequest {
                        user_id: settings.device_id.clone(),
                        device_id: settings.device_id.clone(),
                        display_name: if nickname.trim().is_empty() {
                            "Myusic Linux".to_owned()
                        } else {
                            nickname.trim().to_owned()
                        },
                        avatar_url: None,
                        max_members: None,
                        autoplay_enabled: None,
                    };
                    match ListenTogetherClient::new(http.clone(), &server_url) {
                        Ok(client) => match rt.block_on(client.create_party(&req)) {
                            Ok(membership) => {
                                party_runtime = Some(spawn_party_ws_loop(
                                    &rt,
                                    client,
                                    membership.party.code.clone(),
                                    membership.token.clone(),
                                    Arc::clone(&party_state_shared),
                                    evt_tx.clone(),
                                ));
                                let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                                    "Created Listen Together party {}",
                                    membership.party.code
                                )));
                            }
                            Err(err) => {
                                let _ = evt_tx.try_send(BackendEvent::ErrorNotice(err.to_string()));
                            }
                        },
                        Err(err) => {
                            let _ = evt_tx.try_send(BackendEvent::ErrorNotice(err.to_string()));
                        }
                    }
                }
                BackendCommand::PartyJoin {
                    server_url,
                    code_or_url,
                    nickname,
                } => {
                    let code = parse_invite_url(Some(&code_or_url))
                        .unwrap_or_else(|| code_or_url.trim().to_uppercase());
                    if let Some(prev) = party_runtime.take() {
                        prev.stop_flag.store(true, Ordering::Release);
                    }
                    settings.listen_together_server = server_url.clone();
                    settings.listen_together_nickname = nickname.clone();
                    let _ = settings_store.save(&settings);

                    let req = JoinRequest {
                        user_id: settings.device_id.clone(),
                        device_id: settings.device_id.clone(),
                        display_name: if nickname.trim().is_empty() {
                            "Myusic Linux".to_owned()
                        } else {
                            nickname.trim().to_owned()
                        },
                        avatar_url: None,
                        max_members: None,
                        autoplay_enabled: None,
                    };
                    match ListenTogetherClient::new(http.clone(), &server_url) {
                        Ok(client) => match rt.block_on(client.join_party(&code, &req)) {
                            Ok(membership) => {
                                party_runtime = Some(spawn_party_ws_loop(
                                    &rt,
                                    client,
                                    membership.party.code.clone(),
                                    membership.token.clone(),
                                    Arc::clone(&party_state_shared),
                                    evt_tx.clone(),
                                ));
                                let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
                                    "Joined Listen Together party {}",
                                    membership.party.code
                                )));
                            }
                            Err(err) => {
                                let _ = evt_tx.try_send(BackendEvent::ErrorNotice(err.to_string()));
                            }
                        },
                        Err(err) => {
                            let _ = evt_tx.try_send(BackendEvent::ErrorNotice(err.to_string()));
                        }
                    }
                }
                BackendCommand::PartyLeave => {
                    if let Some(pr) = party_runtime.take() {
                        pr.stop_flag.store(true, Ordering::Release);
                        let _ = rt.block_on(pr.client.leave_party(&pr.code, &pr.token));
                    }
                    let cleared = ListenTogetherState::default();
                    *party_state_shared.lock().unwrap_or_else(|e| e.into_inner()) = cleared.clone();
                    let _ = evt_tx.try_send(BackendEvent::PartyUpdated(Box::new(cleared)));
                    let _ = evt_tx.try_send(BackendEvent::StatusNotice(
                        "Left Listen Together party".to_owned(),
                    ));
                }
            }
        }

        // 5. Advance audio engine clock (crossfade, gapless, sleep timer).
        let elapsed_ms = last_tick.elapsed().as_millis().min(250) as u64;
        last_tick = std::time::Instant::now();
        engine.tick(elapsed_ms.max(1));

        let snap = engine.snapshot();

        // Detect gapless/crossfade automatic deck handoff to the preloaded standby song!
        if snap.state == PlaybackState::Playing
            && let Some(ref active_song) = snap.current_song
            && last_song_vid.as_deref() != Some(active_song.video_id.as_str())
        {
            last_song_vid = Some(active_song.video_id.clone());
            if let Some(next_idx) = compute_next_index(
                current_index,
                queue_items.len(),
                repeat_all,
                repeat_one,
                true,
            ) && queue_items
                .get(next_idx)
                .is_some_and(|s| s.video_id == active_song.video_id)
            {
                current_index = Some(next_idx);
                emit_queue(
                    &queue_items,
                    current_index,
                    shuffle,
                    repeat_all,
                    repeat_one,
                    &evt_tx,
                );
            }
            record_track_started(
                &rt,
                &paths,
                active_song,
                (snap.duration_ms > 0).then_some(snap.duration_ms),
                settings.discord_rpc_enabled,
                &snap,
                &evt_tx,
            );
            spawn_lyrics_lookup(
                &rt,
                &lyrics_repo,
                &current_lyrics,
                active_song.clone(),
                snap.duration_ms,
                &evt_tx,
            );
            let cur_gen = load_gen.load(Ordering::Acquire);
            if let Some(upcoming_idx) = compute_next_index(
                current_index,
                queue_items.len(),
                repeat_all,
                repeat_one,
                true,
            ) && Some(upcoming_idx) != current_index
                && let Some(next_song) = queue_items.get(upcoming_idx).cloned()
            {
                spawn_preload_next(
                    cur_gen,
                    Arc::clone(&load_gen),
                    &rt,
                    Arc::clone(&http_arc),
                    Arc::clone(&resolver),
                    Arc::clone(&remote_auth),
                    next_song,
                    track_tx.clone(),
                );
            }
        }

        // Auto-advance queue when track cleanly finishes without a preloaded standby deck.
        if snap.state == PlaybackState::Ended
            && last_state == PlaybackState::Playing
            && let Some(next_idx) = compute_next_index(
                current_index,
                queue_items.len(),
                repeat_all,
                repeat_one,
                true,
            )
        {
            current_index = Some(next_idx);
            emit_queue(
                &queue_items,
                current_index,
                shuffle,
                repeat_all,
                repeat_one,
                &evt_tx,
            );
            let song = queue_items[next_idx].clone();
            start_async_song_load(
                &mut engine,
                &load_gen,
                &rt,
                &http_arc,
                &resolver,
                &remote_auth,
                &lyrics_repo,
                &current_lyrics,
                song,
                &track_tx,
                &evt_tx,
            );
        }

        let snap = engine.snapshot();
        let state_changed = snap.state != last_state;
        let pos_changed = snap.position_ms.abs_diff(last_published_pos_ms) >= 100;
        if state_changed || (snap.state == PlaybackState::Playing && pos_changed) {
            last_state = snap.state;
            last_published_pos_ms = snap.position_ms;
            let _ = evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(snap.clone())));

            if let Some(ref svc) = mpris_svc {
                let s = Arc::clone(svc);
                let snap_c = snap.clone();
                rt.spawn(async move {
                    let mut ms = MprisState {
                        playback_status: match snap_c.state {
                            PlaybackState::Playing | PlaybackState::Buffering => {
                                PlaybackStatus::Playing
                            }
                            PlaybackState::Paused => PlaybackStatus::Paused,
                            PlaybackState::Stopped
                            | PlaybackState::Ended
                            | PlaybackState::Error => PlaybackStatus::Stopped,
                        },
                        rate: f64::from(snap_c.speed),
                        volume: f64::from(snap_c.volume),
                        position_us: (snap_c.position_ms as i64).saturating_mul(1_000),
                        can_seek: snap_c.duration_ms > 0,
                        can_go_next: true,
                        can_go_previous: true,
                        ..MprisState::default()
                    };
                    if let Some(ref song) = snap_c.current_song {
                        ms.metadata = build_mpris_metadata(song, snap_c.duration_ms, None);
                    }
                    let _ = s.publish_state(ms).await;
                    s.set_position_ms(snap_c.position_ms);
                });
            }
        }
    }
}

/// Immediately updates UI to `Buffering` for `song`, starts parallel lyrics lookup,
/// and resolves + opens the audio decoder asynchronously off the backend control loop.
#[allow(clippy::too_many_arguments)]
fn start_async_song_load(
    engine: &mut AudioEngine,
    load_gen: &Arc<AtomicU64>,
    rt: &Handle,
    http: &Arc<Client>,
    resolver: &Arc<StreamResolver>,
    remote_auth: &Arc<RwLock<RemoteArtworkAuth>>,
    lyrics_repo: &Arc<LyricsRepository>,
    current_lyrics: &Arc<Mutex<Option<LyricsPayload>>>,
    song: Song,
    track_tx: &Sender<InternalTrackMsg>,
    evt_tx: &Sender<BackendEvent>,
) {
    let generation = load_gen.fetch_add(1, Ordering::AcqRel) + 1;
    engine.stop();

    // Publish an instant Buffering snapshot so the bottom bar & Now Playing respond in <1 ms!
    let mut buffering_snap = engine.snapshot();
    buffering_snap.state = PlaybackState::Buffering;
    buffering_snap.duration_ms = duration_millis(song.duration_text.as_deref());
    buffering_snap.position_ms = 0;
    buffering_snap.current_song = Some(song.clone());
    let _ = evt_tx.try_send(BackendEvent::PlaybackUpdated(Box::new(buffering_snap)));
    let _ = evt_tx.try_send(BackendEvent::StatusNotice(format!(
        "Loading {} - {}",
        song.title, song.artist
    )));

    // Start 17-source lyrics race immediately in parallel with stream loading!
    let est_dur_ms = duration_millis(song.duration_text.as_deref());
    spawn_lyrics_lookup(
        rt,
        lyrics_repo,
        current_lyrics,
        song.clone(),
        est_dur_ms,
        evt_tx,
    );

    let gen_ref = Arc::clone(load_gen);
    let http_c = Arc::clone(http);
    let resolver_c = Arc::clone(resolver);
    let remote_c = Arc::clone(remote_auth);
    let rt_c = rt.clone();
    let tx = track_tx.clone();

    rt.spawn(async move {
        let prepared = prepare_track_decoder(
            &http_c,
            &rt_c,
            &resolver_c,
            &remote_c,
            &song,
            &gen_ref,
            generation,
        )
        .await;
        if gen_ref.load(Ordering::Acquire) != generation {
            return;
        }
        match prepared {
            Ok((decoder, prebuffered, loudness_db, source_label)) => {
                let _ = tx
                    .send(InternalTrackMsg::Ready {
                        generation,
                        song,
                        decoder: Box::new(decoder),
                        prebuffered,
                        loudness_db,
                        source_label,
                    })
                    .await;
            }
            Err(error) => {
                let _ = tx
                    .send(InternalTrackMsg::Failed {
                        generation,
                        song_title: song.title,
                        error,
                    })
                    .await;
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_preload_next(
    generation: u64,
    load_gen: Arc<AtomicU64>,
    rt: &Handle,
    http: Arc<Client>,
    resolver: Arc<StreamResolver>,
    remote_auth: Arc<RwLock<RemoteArtworkAuth>>,
    next_song: Song,
    track_tx: Sender<InternalTrackMsg>,
) {
    let rt_c = rt.clone();
    rt.spawn(async move {
        // Brief courtesy delay so the current track's initial chunks have full bandwidth priority.
        tokio::time::sleep(Duration::from_millis(900)).await;
        if load_gen.load(Ordering::Acquire) != generation {
            return;
        }
        if let Ok((decoder, prebuffered, loudness_db, source_label)) = prepare_track_decoder(
            &http,
            &rt_c,
            &resolver,
            &remote_auth,
            &next_song,
            &load_gen,
            generation,
        )
        .await
            && load_gen.load(Ordering::Acquire) == generation
        {
            let _ = track_tx
                .send(InternalTrackMsg::PreloadReady {
                    generation,
                    song: next_song,
                    decoder: Box::new(decoder),
                    prebuffered,
                    loudness_db,
                    source_label,
                })
                .await;
        }
    });
}

type PreparedDecoderResult = (AudioDecoder, Vec<DecodedPacket>, Option<f64>, String);

async fn prepare_track_decoder(
    http: &Arc<Client>,
    rt: &Handle,
    resolver: &Arc<StreamResolver>,
    remote_auth: &Arc<RwLock<RemoteArtworkAuth>>,
    song: &Song,
    load_gen: &Arc<AtomicU64>,
    generation: u64,
) -> Result<PreparedDecoderResult, String> {
    // 1. Local file path (instant, offline; pre-decode opening packets cleanly).
    if let Some(ref local) = song.local_path
        && Path::new(local).exists()
    {
        let p = PathBuf::from(local);
        let gen_check = Arc::clone(load_gen);
        let (dec, prebuf) = tokio::task::spawn_blocking(move || {
            let mut dec = AudioDecoder::open_path(&p).map_err(|e| e.to_string())?;
            let prebuf = dec
                .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                    gen_check.load(Ordering::Acquire) != generation
                })
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((dec, prebuf))
        })
        .await
        .map_err(|e| format!("decode task join error: {e}"))??;
        return Ok((dec, prebuf, None, "Local".to_owned()));
    }
    if Path::new(&song.video_id).exists() {
        let p = PathBuf::from(&song.video_id);
        let gen_check = Arc::clone(load_gen);
        let (dec, prebuf) = tokio::task::spawn_blocking(move || {
            let mut dec = AudioDecoder::open_path(&p).map_err(|e| e.to_string())?;
            let prebuf = dec
                .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                    gen_check.load(Ordering::Acquire) != generation
                })
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((dec, prebuf))
        })
        .await
        .map_err(|e| format!("decode task join error: {e}"))??;
        return Ok((dec, prebuf, None, "Local".to_owned()));
    }

    // 2. WebDAV stream (authenticated HTTP Range via ChunkedRangeSource).
    if WebDavConfig::is_webdav_id(&song.video_id) {
        let file_url = WebDavConfig::file_url_of(&song.video_id)
            .or(song.local_uri.as_deref())
            .ok_or_else(|| "WebDAV track missing stream URL".to_owned())?
            .to_owned();
        let webdav_cfg = remote_auth
            .read()
            .map(|g| g.webdav.clone())
            .unwrap_or_default();
        let mut headers = std::collections::BTreeMap::new();
        if let Some(auth_hdr) = webdav_cfg.auth_header_for_url(&file_url) {
            headers.insert("Authorization".to_owned(), auth_hdr);
        }
        let ext = extension_lower(&file_url);
        let mime = match ext.as_str() {
            "flac" => "audio/flac",
            "wav" => "audio/wav",
            "mp3" => "audio/mpeg",
            "ogg" | "opus" => "audio/ogg",
            "m4a" | "aac" => "audio/mp4",
            "webm" => "audio/webm",
            "dsf" => "audio/x-dsf",
            "dff" => "audio/x-dff",
            _ => "application/octet-stream",
        };
        let stream = ResolvedStream {
            video_id: song.video_id.clone(),
            url: file_url,
            kbps: 0,
            mime_type: mime.to_owned(),
            ext,
            content_length: None,
            duration_ms: None,
            loudness_db: None,
            is_live_hls: false,
            client_name: "WebDAV".to_owned(),
            profile_id: "webdav".to_owned(),
            media_headers: headers,
            range_chunk_bytes: None,
        };
        let http_clone = (**http).clone();
        let rt_clone = rt.clone();
        let gen_check = Arc::clone(load_gen);
        let (dec, prebuf) = tokio::task::spawn_blocking(move || {
            let mut dec = open_http_decoder(http_clone, rt_clone, &stream, None)
                .map_err(|e| e.to_string())?;
            let prebuf = dec
                .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                    gen_check.load(Ordering::Acquire) != generation
                })
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((dec, prebuf))
        })
        .await
        .map_err(|e| format!("WebDAV decode join error: {e}"))??;
        return Ok((dec, prebuf, None, "WebDAV".to_owned()));
    }

    // 3. SMB2/3 share stream (pure-Rust chunk-cached SmbRangeReader).
    if SmbConfig::is_smb_id(&song.video_id)
        || song.local_uri.as_deref().is_some_and(SmbConfig::is_smb_url)
    {
        let uri = song
            .local_uri
            .as_deref()
            .unwrap_or(&song.video_id)
            .to_owned();
        let smb_cfg = remote_auth
            .read()
            .map(|g| g.smb.clone())
            .unwrap_or_default();
        let reader = SmbClient::new()
            .open_range_reader(&smb_cfg, &uri, rt.clone())
            .await
            .map_err(|e| e.to_string())?;
        let byte_len = reader.byte_len();
        let ext = extension_lower(&uri);
        let gen_check = Arc::clone(load_gen);
        let (dec, prebuf) = tokio::task::spawn_blocking(move || {
            let mut dec = AudioDecoder::open_seekable_reader(reader, Some(byte_len), Some(&ext))
                .map_err(|e| e.to_string())?;
            let prebuf = dec
                .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                    gen_check.load(Ordering::Acquire) != generation
                })
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((dec, prebuf))
        })
        .await
        .map_err(|e| format!("SMB decode join error: {e}"))??;
        return Ok((dec, prebuf, None, "SMB".to_owned()));
    }

    // 2. Remote YouTube Music stream with automatic 403/404/410 profile-refusal retry
    //    and off-thread 600 ms opening prebuffer so playback never clips the intro.
    let mut last_err = String::new();
    for attempt in 0..2 {
        if load_gen.load(Ordering::Acquire) != generation {
            return Err("cancelled".to_owned());
        }
        let req = ResolveRequest::for_playback(&song.video_id, u32::MAX);
        let stream = resolver.resolve(req).await.map_err(|e| e.to_string())?;

        if load_gen.load(Ordering::Acquire) != generation {
            return Err("cancelled".to_owned());
        }

        let refused_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let refused_cb_flag = Arc::clone(&refused_flag);
        let resolver_cb = Arc::clone(resolver);
        let vid_cb = song.video_id.clone();
        let profile_cb = stream.profile_id.clone();
        let on_refused: RefusalCallback = Arc::new(move |url, _status| {
            refused_cb_flag.store(true, Ordering::Release);
            resolver_cb.on_playback_refused(&vid_cb, &profile_cb, url);
        });

        let http_clone = (**http).clone();
        let rt_clone = rt.clone();
        let loudness = stream.loudness_db;
        let label = stream.client_name.clone();
        let stream_clone = stream.clone();
        let gen_check = Arc::clone(load_gen);

        let open_res = tokio::task::spawn_blocking(move || {
            let mut dec = open_http_decoder(http_clone, rt_clone, &stream_clone, Some(on_refused))?;
            let prebuf = dec.prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                gen_check.load(Ordering::Acquire) != generation
            })?;
            Ok::<_, bitchord_audio::EngineError>((dec, prebuf))
        })
        .await
        .map_err(|e| format!("decoder open join error: {e}"))?;

        match open_res {
            Ok((dec, prebuf)) => return Ok((dec, prebuf, loudness, label)),
            Err(err) => {
                last_err = err.to_string();
                let was_refused = refused_flag.load(Ordering::Acquire)
                    || last_err.contains("HTTP 403")
                    || last_err.contains("HTTP 404")
                    || last_err.contains("HTTP 410");
                if was_refused && attempt == 0 {
                    resolver.on_playback_refused(&song.video_id, &stream.profile_id, &stream.url);
                    continue;
                }
                return Err(last_err);
            }
        }
    }
    Err(last_err)
}

fn record_track_started(
    rt: &Handle,
    paths: &AppPaths,
    song: &Song,
    duration_ms: Option<u64>,
    discord_enabled: bool,
    snap: &PlaybackSnapshot,
    evt_tx: &Sender<BackendEvent>,
) {
    if let Ok(db) = LibraryDb::open(&paths.db_file()) {
        let _ = db.upsert_song(song);
        let dur =
            duration_ms.unwrap_or_else(|| duration_millis(song.duration_text.as_deref())) as i64;
        let _ = db.record_play(&song.video_id, unix_now_ms(), dur);
    }
    emit_library_snapshot(paths, evt_tx);

    if discord_enabled {
        let quality_line = discord_audio_quality_line(snap.nerd_stats.as_ref());
        let state_text = match quality_line {
            Some(q) => format!("{} · {q}", song.artist),
            None => song.artist.clone(),
        };
        let activity = DiscordActivity::listening(song.title.clone(), state_text);
        rt.spawn(async move {
            if let Ok(mut client) = DiscordIpcClient::connect_default().await {
                let _ = client.set_activity(&activity).await;
            }
        });
    }
}

fn spawn_lyrics_lookup(
    rt: &Handle,
    lyrics_repo: &Arc<LyricsRepository>,
    current_lyrics: &Arc<Mutex<Option<LyricsPayload>>>,
    song: Song,
    duration_ms: u64,
    evt_tx: &Sender<BackendEvent>,
) {
    *current_lyrics.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let vid = song.video_id.clone();
    let _ = evt_tx.try_send(BackendEvent::LyricsLoading);
    let repo = Arc::clone(lyrics_repo);
    let tx = evt_tx.clone();
    let lyrics_slot = Arc::clone(current_lyrics);
    let dur_ms = duration_ms as i64;

    rt.spawn(async move {
        let mut req = LyricsRequest::new(&song.video_id, &song.title, &song.artist, dur_ms);
        req.album = song.album_name.as_deref();
        req.prioritize_syllable_sync = true;
        let res = match repo.lyrics(&req, &LyricsCallbacks::default()).await {
            Some(found) => {
                let payload = LyricsPayload {
                    video_id: vid.clone(),
                    source: found.source,
                    lines: found.lines,
                    translated_lines: None,
                    translated_lang: None,
                };
                *lyrics_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload.clone());
                Ok(payload)
            }
            None => Err("No synced or plain lyrics available for this track.".to_owned()),
        };
        let _ = tx
            .send(BackendEvent::LyricsLoaded {
                video_id: vid,
                result: res,
            })
            .await;
    });
}

fn spawn_party_ws_loop(
    rt: &Handle,
    client: ListenTogetherClient,
    code: String,
    token: String,
    shared_state: Arc<Mutex<ListenTogetherState>>,
    evt_tx: Sender<BackendEvent>,
) -> PartyRuntime {
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_c = Arc::clone(&stop_flag);
    let client_c = client.clone();
    let code_c = code.clone();
    let token_c = token.clone();

    rt.spawn(async move {
        let mut session = bitchord_integrations::ListenTogetherSession::default();
        session.state.code = Some(code_c.clone());
        session.state.connection = Connection::Connecting;
        *shared_state.lock().unwrap_or_else(|e| e.into_inner()) = session.state.clone();
        let _ = evt_tx
            .send(BackendEvent::PartyUpdated(Box::new(session.state.clone())))
            .await;

        match client_c.connect_ws(&code_c, &token_c).await {
            Ok(mut ws) => {
                while !stop_c.load(Ordering::Acquire) {
                    match tokio::time::timeout(Duration::from_millis(250), ws.next_frame()).await {
                        Ok(Ok(Some(frame))) => {
                            let effects = session.apply_server_frame(frame, unix_now_ms());
                            *shared_state.lock().unwrap_or_else(|e| e.into_inner()) =
                                session.state.clone();
                            let _ = evt_tx
                                .send(BackendEvent::PartyUpdated(Box::new(session.state.clone())))
                                .await;
                            if effects.leave_party {
                                break;
                            }
                        }
                        Ok(Ok(None)) => {
                            session.on_socket_dropped();
                            let _ = evt_tx
                                .send(BackendEvent::PartyUpdated(Box::new(session.state.clone())))
                                .await;
                            break;
                        }
                        Ok(Err(err)) => {
                            session.state.error = Some(err.to_string());
                            session.state.connection = Connection::Offline;
                            let _ = evt_tx
                                .send(BackendEvent::PartyUpdated(Box::new(session.state.clone())))
                                .await;
                            break;
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(err) => {
                session.state.error = Some(err.to_string());
                session.state.connection = Connection::Offline;
                let _ = evt_tx
                    .send(BackendEvent::PartyUpdated(Box::new(session.state.clone())))
                    .await;
            }
        }
    });

    PartyRuntime {
        client,
        code,
        token,
        stop_flag,
    }
}

fn scan_local_folder_into_db(folder: &Path, paths: &AppPaths) -> Result<usize, String> {
    if !folder.exists() || !folder.is_dir() {
        return Err(format!(
            "Folder does not exist or is not a directory: {}",
            folder.display()
        ));
    }
    let db = LibraryDb::open(&paths.db_file()).map_err(|e| e.to_string())?;
    let mut stack = vec![folder.to_path_buf()];
    let mut imported = 0usize;

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if !matches!(
                ext.as_str(),
                "flac" | "wav" | "mp3" | "opus" | "ogg" | "m4a" | "aac" | "aiff" | "dsf" | "dff"
            ) {
                continue;
            }
            let path_str = p.to_string_lossy().into_owned();
            let tags = read_local_audio_tags(&p);
            let title = tags
                .as_ref()
                .and_then(|t| t.title.clone())
                .or_else(|| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| "Untitled Track".to_owned());
            let artist = tags
                .as_ref()
                .and_then(|t| t.artist.clone())
                .unwrap_or_else(|| "Local Library".to_owned());
            let mut song = Song::new(format!("local:{path_str}"), title, artist, None);
            song.local_path = Some(path_str);
            if let Some(ref t) = tags {
                song.album_name = t.album.clone();
                if t.duration_ms > 0 {
                    let secs = t.duration_ms / 1_000;
                    song.duration_text = Some(format!("{}:{:02}", secs / 60, secs % 60));
                }
            }
            if db.upsert_song(&song).is_ok() {
                imported += 1;
            }
        }
    }
    Ok(imported)
}

fn emit_library_snapshot(paths: &AppPaths, evt_tx: &Sender<BackendEvent>) {
    let Ok(db) = LibraryDb::open(&paths.db_file()) else {
        return;
    };
    let stats = db.stats().unwrap_or_default();
    let songs = db.search_cached_songs("", 200).unwrap_or_default();
    let downloads = db.list_downloads().unwrap_or_default();
    let playlists = db.list_playlists().unwrap_or_default();
    let search_history = db.list_search_history().unwrap_or_default();
    let _ = evt_tx.try_send(BackendEvent::LibraryLoaded(LibrarySnapshot {
        stats,
        songs,
        downloads,
        playlists,
        search_history,
    }));
}

fn emit_queue(
    items: &[Song],
    current_index: Option<usize>,
    shuffle: bool,
    repeat_all: bool,
    repeat_one: bool,
    evt_tx: &Sender<BackendEvent>,
) {
    let _ = evt_tx.try_send(BackendEvent::QueueUpdated(QueueViewSnapshot {
        items: items.to_vec(),
        current_index,
        shuffle,
        repeat_all,
        repeat_one,
    }));
}

fn compute_next_index(
    current: Option<usize>,
    len: usize,
    repeat_all: bool,
    repeat_one: bool,
    natural_eof: bool,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cur = current?;
    if natural_eof && repeat_one {
        return Some(cur);
    }
    if cur + 1 < len {
        Some(cur + 1)
    } else if repeat_all {
        Some(0)
    } else {
        None
    }
}

fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_advancement_respects_repeat_one_repeat_all_and_bounds() {
        assert_eq!(compute_next_index(Some(0), 3, false, false, true), Some(1));
        assert_eq!(compute_next_index(Some(2), 3, false, false, true), None);
        assert_eq!(compute_next_index(Some(2), 3, true, false, true), Some(0));
        assert_eq!(compute_next_index(Some(1), 3, false, true, true), Some(1));
        assert_eq!(compute_next_index(Some(1), 3, false, true, false), Some(2));
        assert_eq!(compute_next_index(None, 0, true, true, true), None);
    }
}
