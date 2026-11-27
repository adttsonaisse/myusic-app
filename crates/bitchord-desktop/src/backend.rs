//! App-level asynchronous backend orchestrator bridging `bitchord-audio`,
//! `bitchord-queue`, `bitchord-innertube`, `bitchord-lyrics`, `bitchord-storage`,
//! `bitchord-integrations`, and `MprisService` to GPUI views via lock-free
//! `async_channel` streams.
//!
//! Lives in GPUI's `App` global state so closing the window to the system tray
//! keeps playback, gapless preload, Listen Together, scrobbling, and MPRIS2 alive.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender, TrySendError};
use bitchord_audio::{
    AlsaDeviceInfo, AudioDecoder, AudioEngine, AudioOutputMode, DecodedPacket, EqTuningSnapshot,
    FallbackResumeState, LoopStatus, MprisCommand, MprisService, MprisState, OUTPUT_SAMPLE_RATE,
    PlaybackSnapshot, PlaybackState, PlaybackStatus, RefusalCallback, StreamCancelToken,
    analyze_audio_file, build_mpris_metadata, enumerate_alsa_devices, open_http_decoder_with_cancel,
};
use bitchord_automix::{
    AutomixMode, OnnxAutomixStatus, OnnxModelPaths, TrackAnalysis, TransitionPlan, plan_transition,
};
use bitchord_dsp_core::{EqCurve, EqLayout, EqualizerPreset, manual_curve};
use bitchord_innertube::{
    BrowseHeader, CipherSandbox, InnertubeClient, InnertubeParser, NativeStreamBackend,
    ProbePolicy, ResolveRequest, ResolvedStream, StreamBackend, StreamResolver, UploadedTracks,
    YtMusicRepository, build_shared_http_client,
};
#[cfg(feature = "ytdlp-bridge")]
use bitchord_innertube::{FallbackStreamBackend, YtDlpStreamBackend};
use bitchord_integrations::discord::DiscordAssets;
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
    BrowseType, EntityType, HomeFeed, PlaybackSourceType, SearchFilter, SearchResult, Song,
    duration_millis,
};
use bitchord_netstorage::{
    SmbClient, SmbConfig, WebDavClient, WebDavConfig, remote_song::extension_lower,
};
use bitchord_queue::QueueSource;
use bitchord_storage::{
    AppPaths, AppSettingsData, AudioOutputPreference, CachedStream, DEFAULT_LYRICS_CACHE_TTL_MS,
    DEFAULT_LYRICS_NOT_FOUND_TTL_MS, DEFAULT_STREAM_CACHE_TTL_MS, DbStats, KeyringSecretStore,
    LocalPlaylist, SavedSongMetadata, SearchHistoryRecord, SecretKey, SecretStore, SettingsStore,
    SmbSettings, WebDavSettings, format_duration_ms, read_local_audio_tags,
};
use gpui::Global;
use reqwest::Client;
use tokio::runtime::Handle;

use crate::artwork::{RemoteArtworkAuth, song_thumbnail_url};
use crate::branding;
use crate::db_worker::DbWorker;
use crate::queue_state::{PlayNextOutcome, QueueController, QueueRemoveOutcome};

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
    pub search_history: Vec<SearchHistoryRecord>,
    pub liked_video_ids: HashSet<String>,
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

/// Serializable-in-memory lyrics state preserved across tray close/reopen.
#[derive(Debug, Clone, Default)]
pub enum LyricsSyncState {
    #[default]
    Idle,
    Loading,
    Ready(LyricsPayload),
    Error(String),
}

/// Live search suggestions + matching tracks/albums/artists while typing (`notes.md` #7).
#[derive(Debug, Clone, Default)]
pub struct SearchSuggestionsPayload {
    pub query: String,
    pub queries: Vec<String>,
    pub items: Vec<SearchResult>,
}

/// Complete live backend snapshot delivered on [`BackendCommand::SyncFullState`]
/// (initial mount and reopening the window from the Linux system tray).
#[derive(Debug, Clone)]
pub struct FullStateSnapshot {
    pub pipewire_active: bool,
    pub audio_banner: Option<String>,
    pub audio_devices: Vec<AlsaDeviceInfo>,
    pub playback: PlaybackSnapshot,
    pub queue: QueueViewSnapshot,
    pub lyrics: LyricsSyncState,
    pub home: Option<HomeFeed>,
    pub library: Option<LibrarySnapshot>,
    pub party: ListenTogetherState,
    pub eq_enabled: bool,
    pub eq_preset: EqualizerPreset,
    pub eq_bands_db: [f32; EqLayout::MANUAL_COUNT],
    pub stream_backend: StreamBackendChoice,
    pub audio_output_pref: AudioOutputPreference,
    pub selected_alsa_device: String,
    pub automix_mode: AutomixMode,
    pub automix_phrase_bars: u32,
    pub discord_rpc_enabled: bool,
}

/// Inbound command sent by GPUI views, shortcuts, or tray menu to the backend.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    /// Requests a full state sync snapshot (used on initial mount and tray reopen).
    SyncFullState,
    LoadHome,
    Search {
        query: String,
        filter: SearchFilter,
    },
    SearchSuggest(String),
    RecordSearchSelection(SearchHistoryRecord),
    RemoveSearchHistory(String),
    ClearSearchHistory,
    ToggleLikeSong(Box<Song>),
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
    SetAutomix {
        mode: AutomixMode,
        phrase_bars: u32,
    },
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
    FullStateSynced(Box<FullStateSnapshot>),
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
    SearchHistoryUpdated(Vec<SearchHistoryRecord>),
    LikedSongsUpdated(HashSet<String>),
    LibraryTrackRecorded {
        song: Box<Song>,
        stats: DbStats,
    },
    PartyServerProbed {
        server: String,
        online: bool,
        latency_ms: i64,
    },
    PartyUpdated(Box<ListenTogetherState>),
    StatusNotice(String),
    ErrorNotice(String),
}

/// Bounded outbound event ring that never blocks the backend loop when the window is
/// closed to tray (evicts the oldest unread event instead of dropping the newest one).
#[derive(Clone)]
struct EventHub {
    tx: Sender<BackendEvent>,
    rx_evict: Receiver<BackendEvent>,
}

impl EventHub {
    fn new(capacity: usize) -> (Self, Receiver<BackendEvent>) {
        let (tx, rx) = async_channel::bounded(capacity.max(16));
        (
            Self {
                tx,
                rx_evict: rx.clone(),
            },
            rx,
        )
    }

    fn emit(&self, mut evt: BackendEvent) {
        for _ in 0..4 {
            match self.tx.try_send(evt) {
                Ok(()) => return,
                Err(TrySendError::Full(returned)) => {
                    let _ = self.rx_evict.try_recv();
                    evt = returned;
                }
                Err(TrySendError::Closed(_)) => return,
            }
        }
    }
}

/// Global handle stored inside GPUI `App` so windows can close/reopen without stopping playback.
#[derive(Clone)]
pub struct AppBackend {
    pub cmd_tx: Sender<BackendCommand>,
    pub evt_rx: Receiver<BackendEvent>,
    pub http: Client,
    pub paths: AppPaths,
    pub db_worker: DbWorker,
    pub initial_settings: AppSettingsData,
    pub onnx_automix_status: OnnxAutomixStatus,
    pub remote_auth: Arc<RwLock<RemoteArtworkAuth>>,
}

impl Global for AppBackend {}

impl AppBackend {
    /// Launches the background audio + network + storage + MPRIS2 runtime on Tokio.
    pub fn start(rt: Handle) -> Self {
        let paths = AppPaths::resolve();
        // Dedicated persistent SQLite worker shared across backend & artwork LRU disk cache.
        let db_worker = DbWorker::start(paths.db_file());
        let settings_store = SettingsStore::new(paths.settings_file());
        let initial_settings = settings_store.load().unwrap_or_default();
        let onnx_automix_status = OnnxModelPaths::discover(&paths.models_dir()).status();
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

        // Unbounded command channel guarantees UI/tray commands are never silently dropped.
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<BackendCommand>();
        let (events, evt_rx) = EventHub::new(256);

        let worker_http = http.clone();
        let worker_paths = paths.clone();
        let worker_db = db_worker.clone();
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
                    worker_db,
                    worker_settings,
                    worker_remote_auth,
                    cmd_rx,
                    events,
                );
            })
            .expect("spawn bitchord backend thread");

        Self {
            cmd_tx,
            evt_rx,
            http,
            paths,
            db_worker,
            initial_settings,
            onnx_automix_status,
            remote_auth,
        }
    }

    pub fn send(&self, cmd: BackendCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    /// Discards stale queued events accumulated while the window was closed to tray,
    /// right before the reopened window sends [`BackendCommand::SyncFullState`].
    pub fn drain_stale_events(&self) {
        while self.evt_rx.try_recv().is_ok() {}
    }
}

struct PartyRuntime {
    client: ListenTogetherClient,
    code: String,
    token: String,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
}

/// Minimum decoded audio buffered off-thread before starting PipeWire playback
/// so opening notes start quickly (~240ms) without stalling startup or clipping intros.
const INITIAL_PREBUFFER_MS: u64 = 240;
/// Short courtesy delay before hitting remote lyrics providers so opening audio chunks
/// keep full network/CPU priority and rapid track skipping never spams lyrics APIs.
const LYRICS_REMOTE_DELAY_MS: u64 = 350;
/// Maximum automatic playback retries after transient mid-stream network errors.
const MAX_PLAYBACK_AUTO_RETRIES: u8 = 3;

#[derive(Debug, Default, Clone)]
struct PlaybackErrorRetry {
    video_id: Option<String>,
    attempts: u8,
    resume_ms: u64,
    retry_at: Option<Instant>,
    active: bool,
    exhausted: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetryScheduleOutcome {
    Scheduled { attempt_number: u8, max_attempts: u8 },
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RetryAttemptPlan {
    resume_ms: u64,
    bypass_cache: bool,
}

fn is_refusal_error(err_msg: &str) -> bool {
    err_msg.contains("HTTP 403") || err_msg.contains("HTTP 404") || err_msg.contains("HTTP 410")
}

impl PlaybackErrorRetry {
    fn reset(&mut self) {
        self.video_id = None;
        self.attempts = 0;
        self.resume_ms = 0;
        self.retry_at = None;
        self.active = false;
        self.exhausted = false;
        self.last_error = None;
    }

    fn backoff_for_attempt(attempt: u8) -> Duration {
        match attempt {
            0 => Duration::from_millis(800),
            1 => Duration::from_millis(2_000),
            _ => Duration::from_millis(4_000),
        }
    }

    fn is_waiting_for_retry(&self) -> bool {
        self.active && self.retry_at.is_some()
    }

    fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    fn resume_ms_for(&self, video_id: &str) -> Option<u64> {
        (self.video_id.as_deref() == Some(video_id)).then_some(self.resume_ms)
    }

    fn had_failure_for(&self, video_id: &str) -> bool {
        self.video_id.as_deref() == Some(video_id)
            && (self.attempts > 0 || self.active || self.exhausted)
    }

    fn on_engine_error(
        &mut self,
        video_id: &str,
        position_ms: u64,
        err_msg: Option<&str>,
        now: Instant,
    ) -> RetryScheduleOutcome {
        if self.video_id.as_deref() != Some(video_id) {
            self.video_id = Some(video_id.to_owned());
            self.attempts = 0;
            self.resume_ms = position_ms;
        } else if position_ms > 0 {
            self.resume_ms = position_ms;
        }
        self.last_error = err_msg.map(ToOwned::to_owned);
        self.exhausted = false;
        if self.attempts < MAX_PLAYBACK_AUTO_RETRIES {
            self.active = true;
            self.retry_at = Some(now + Self::backoff_for_attempt(self.attempts));
            RetryScheduleOutcome::Scheduled {
                attempt_number: self.attempts + 1,
                max_attempts: MAX_PLAYBACK_AUTO_RETRIES,
            }
        } else {
            self.active = false;
            self.retry_at = None;
            self.exhausted = true;
            RetryScheduleOutcome::Exhausted
        }
    }

    fn take_due_attempt(
        &mut self,
        now: Instant,
        current_error: Option<&str>,
    ) -> Option<RetryAttemptPlan> {
        if !self.active || !self.retry_at.is_some_and(|at| now >= at) {
            return None;
        }
        self.retry_at = None;
        self.attempts = self.attempts.saturating_add(1);
        let err_ref = current_error
            .or(self.last_error.as_deref())
            .unwrap_or("");
        let bypass_cache = self.attempts >= 2 || is_refusal_error(err_ref);
        Some(RetryAttemptPlan {
            resume_ms: self.resume_ms,
            bypass_cache,
        })
    }

    fn on_async_load_failed(
        &mut self,
        video_id: &str,
        err_msg: &str,
        now: Instant,
    ) -> Option<RetryScheduleOutcome> {
        if !self.active || self.video_id.as_deref() != Some(video_id) {
            return None;
        }
        self.last_error = Some(err_msg.to_owned());
        if self.attempts < MAX_PLAYBACK_AUTO_RETRIES {
            self.retry_at = Some(now + Self::backoff_for_attempt(self.attempts));
            Some(RetryScheduleOutcome::Scheduled {
                attempt_number: self.attempts + 1,
                max_attempts: MAX_PLAYBACK_AUTO_RETRIES,
            })
        } else {
            self.active = false;
            self.retry_at = None;
            self.exhausted = true;
            Some(RetryScheduleOutcome::Exhausted)
        }
    }

    fn on_load_succeeded(&mut self, video_id: &str) {
        if self.video_id.as_deref() != Some(video_id) {
            self.reset();
        } else {
            self.retry_at = None;
            self.exhausted = false;
        }
    }
}

#[derive(Debug, Clone)]
struct InFlightTrackLoad {
    generation: u64,
    song: Song,
    resume_position_ms: u64,
}

#[derive(Debug)]
struct PlaybackLoopState {
    last_state: PlaybackState,
    last_song_vid: Option<String>,
    last_published_pos_ms: u64,
    in_flight: Option<InFlightTrackLoad>,
}

impl Default for PlaybackLoopState {
    fn default() -> Self {
        Self {
            last_state: PlaybackState::Stopped,
            last_song_vid: None,
            last_published_pos_ms: 0,
            in_flight: None,
        }
    }
}

impl PlaybackLoopState {
    fn begin_async_load(&mut self, generation: u64, song: Song, resume_position_ms: u64) {
        self.in_flight = Some(InFlightTrackLoad {
            generation,
            song,
            resume_position_ms,
        });
        self.last_state = PlaybackState::Buffering;
        self.last_published_pos_ms = resume_position_ms;
    }

    fn clear_in_flight(&mut self) {
        self.in_flight = None;
    }

    fn finish_in_flight_if_matches(&mut self, generation: u64) -> Option<InFlightTrackLoad> {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|load| load.generation == generation)
        {
            self.in_flight.take()
        } else {
            None
        }
    }

    fn sync_stopped(&mut self) {
        self.in_flight = None;
        self.last_state = PlaybackState::Stopped;
        self.last_song_vid = None;
        self.last_published_pos_ms = 0;
    }

    /// Returns whether the periodic loop tick should emit `snap` to the UI.
    /// Suppresses intermediate `Stopped` snapshots while a non-standby async track load
    /// or auto-retry cycle owns the player bar so the UI never blanks out or resets
    /// `last_published_pos_ms` to `0`.
    fn should_publish_tick_snapshot(
        &mut self,
        snap: &PlaybackSnapshot,
        retry: &PlaybackErrorRetry,
    ) -> bool {
        if snap.state == PlaybackState::Stopped
            && (self.in_flight.is_some() || retry.is_waiting_for_retry() || retry.is_exhausted())
        {
            return false;
        }
        let state_changed = snap.state != self.last_state;
        let pos_changed = snap.position_ms.abs_diff(self.last_published_pos_ms) >= 100;
        if state_changed || (snap.state == PlaybackState::Playing && pos_changed) {
            self.last_state = snap.state;
            self.last_published_pos_ms = snap.position_ms;
            true
        } else {
            false
        }
    }

    /// Projects the observable playback snapshot for `SyncFullState` so reopening from
    /// tray during an in-flight track load or auto-retry cycle restores `Buffering`/`Error`
    /// with the active song and resume position instead of a blank `Stopped` snapshot.
    fn effective_snapshot(
        &self,
        mut engine_snap: PlaybackSnapshot,
        retry: &PlaybackErrorRetry,
        current_song: Option<&Song>,
    ) -> PlaybackSnapshot {
        if engine_snap.state == PlaybackState::Stopped {
            if let Some(ref load) = self.in_flight {
                engine_snap.state = PlaybackState::Buffering;
                engine_snap.current_song = Some(load.song.clone());
                engine_snap.position_ms = load.resume_position_ms;
                engine_snap.duration_ms = duration_millis(load.song.duration_text.as_deref());
            } else if retry.is_waiting_for_retry()
                && let Some(song) = current_song
                && retry.video_id.as_deref() == Some(song.video_id.as_str())
            {
                engine_snap.state = PlaybackState::Buffering;
                engine_snap.current_song = Some(song.clone());
                engine_snap.position_ms = retry.resume_ms;
                engine_snap.duration_ms = duration_millis(song.duration_text.as_deref());
                engine_snap.last_error = retry.last_error.clone();
            } else if retry.is_exhausted()
                && let Some(song) = current_song
                && retry.video_id.as_deref() == Some(song.video_id.as_str())
            {
                engine_snap.state = PlaybackState::Error;
                engine_snap.current_song = Some(song.clone());
                engine_snap.position_ms = retry.resume_ms;
                engine_snap.duration_ms = duration_millis(song.duration_text.as_deref());
                engine_snap.last_error = retry.last_error.clone();
            }
        }
        engine_snap
    }
}

#[derive(Debug, Default)]
struct SearchSuggestGate {
    generation: Arc<AtomicU64>,
    active_task: Option<tokio::task::JoinHandle<()>>,
}

impl SearchSuggestGate {
    /// Cancels any previous in-flight search suggestion request and advances the generation.
    fn cancel(&mut self) -> u64 {
        if let Some(handle) = self.active_task.take() {
            handle.abort();
        }
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Starts a new search suggestion generation, aborting any previous request.
    fn begin(&mut self) -> (u64, Arc<AtomicU64>) {
        let generation = self.cancel();
        (generation, Arc::clone(&self.generation))
    }

    fn set_active(&mut self, handle: tokio::task::JoinHandle<()>) {
        self.active_task = Some(handle);
    }

    fn is_current(counter: &AtomicU64, expected: u64) -> bool {
        counter.load(Ordering::Acquire) == expected
    }
}

#[derive(Debug, Default)]
struct HomeFetchState {
    in_flight: bool,
    cached: Option<HomeFeed>,
}

impl HomeFetchState {
    fn should_start_on_sync(&self) -> bool {
        self.cached.is_none() && !self.in_flight
    }

    fn mark_started(&mut self) {
        self.in_flight = true;
    }

    fn mark_finished(&mut self, feed: Option<HomeFeed>) {
        self.in_flight = false;
        if let Some(feed) = feed {
            self.cached = Some(feed);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualResumeTarget {
    song: Song,
    resume_ms: u64,
    bypass_cache: bool,
}

fn resolve_manual_resume_target(
    fallback_resume: Option<FallbackResumeState>,
    active_song: Option<Song>,
    retry: &PlaybackErrorRetry,
    last_published_pos_ms: u64,
) -> Option<ManualResumeTarget> {
    if let Some(fb) = fallback_resume {
        return Some(ManualResumeTarget {
            song: fb.song,
            resume_ms: fb.position_ms,
            bypass_cache: false,
        });
    }
    let song = active_song?;
    let resume_ms = retry
        .resume_ms_for(&song.video_id)
        .unwrap_or(last_published_pos_ms);
    let bypass_cache = retry.had_failure_for(&song.video_id);
    Some(ManualResumeTarget {
        song,
        resume_ms,
        bypass_cache,
    })
}

/// Comparable key for MPRIS2 D-Bus properties so `publish_state` fires ONLY on real
/// status/track/metadata/volume/rate/queue changes (never 10x/sec on position ticks).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MprisPublishKey {
    status: u8,
    video_id: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    duration_ms: u64,
    rate_bits: u32,
    volume_bits: u32,
    shuffle: bool,
    repeat_all: bool,
    repeat_one: bool,
    can_seek: bool,
    can_go_next: bool,
    can_go_previous: bool,
}

impl MprisPublishKey {
    fn from_state(snap: &PlaybackSnapshot, queue: &QueueController) -> Self {
        let status = match snap.state {
            PlaybackState::Playing | PlaybackState::Buffering => 1,
            PlaybackState::Paused => 2,
            PlaybackState::Stopped | PlaybackState::Ended | PlaybackState::Error => 0,
        };
        let (video_id, title, artist) = match snap.current_song.as_ref() {
            Some(s) => (
                Some(s.video_id.clone()),
                Some(s.title.clone()),
                Some(s.artist.clone()),
            ),
            None => (None, None, None),
        };
        Self {
            status,
            video_id,
            title,
            artist,
            duration_ms: snap.duration_ms,
            rate_bits: snap.speed.to_bits(),
            volume_bits: snap.volume.to_bits(),
            shuffle: queue.shuffle(),
            repeat_all: queue.repeat_all(),
            repeat_one: queue.repeat_one(),
            can_seek: snap.duration_ms > 0,
            can_go_next: queue.next_index(false).is_some(),
            can_go_previous: !queue.is_empty(),
        }
    }
}

enum InternalMsg {
    Ready {
        generation: u64,
        song: Song,
        decoder: Box<AudioDecoder>,
        prebuffered: Vec<DecodedPacket>,
        loudness_db: Option<f64>,
        source_label: String,
        resolved_duration_ms: Option<u64>,
        resume_position_ms: u64,
    },
    Failed {
        generation: u64,
        song: Song,
        error: String,
    },
    PreloadReady {
        generation: u64,
        preload_generation: u64,
        song: Song,
        decoder: Box<AudioDecoder>,
        prebuffered: Vec<DecodedPacket>,
        loudness_db: Option<f64>,
        source_label: String,
        resolved_duration_ms: Option<u64>,
    },
    AutomixPlanned {
        generation: u64,
        preload_generation: u64,
        plan: TransitionPlan,
    },
    LocalFolderScanned {
        folder: PathBuf,
        count: usize,
    },
    PartyConnected {
        generation: u64,
        runtime: PartyRuntime,
        message: String,
    },
    HomeFetchFinished(Option<HomeFeed>),
    LibraryCached(LibrarySnapshot),
    SearchHistoryCached(Vec<SearchHistoryRecord>),
    LikedSongsCached(HashSet<String>),
    LibraryTrackUpdated {
        song: Box<Song>,
        stats: DbStats,
    },
    LyricsStateUpdated {
        video_id: String,
        state: LyricsSyncState,
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
    cipher_sandbox: &Arc<CipherSandbox>,
    choice: StreamBackendChoice,
) -> Arc<StreamResolver> {
    let make_native = || {
        NativeStreamBackend::new(Arc::clone(innertube))
            .with_cipher_sandbox(Arc::clone(cipher_sandbox))
    };
    let backend: Arc<dyn StreamBackend> = match choice {
        StreamBackendChoice::NativeOnly => Arc::new(make_native()),
        #[cfg(feature = "ytdlp-bridge")]
        StreamBackendChoice::YtDlpOnly => Arc::new(YtDlpStreamBackend::new()),
        StreamBackendChoice::Auto => {
            #[cfg(feature = "ytdlp-bridge")]
            {
                Arc::new(FallbackStreamBackend::new(
                    make_native(),
                    YtDlpStreamBackend::new(),
                ))
            }
            #[cfg(not(feature = "ytdlp-bridge"))]
            {
                Arc::new(make_native())
            }
        }
    };
    Arc::new(
        StreamResolver::new(Arc::clone(http), backend, Arc::clone(uploads))
            .with_probe_policy(ProbePolicy::Optimistic),
    )
}

fn save_settings_async(rt: &Handle, settings_path: &Path, settings: &AppSettingsData) {
    let path = settings_path.to_path_buf();
    let data = settings.clone();
    rt.spawn_blocking(move || {
        let store = SettingsStore::new(path);
        let _ = store.save(&data);
    });
}

#[allow(clippy::too_many_arguments)]
fn run_backend_thread(
    rt: Handle,
    http: Client,
    paths: AppPaths,
    db_worker: DbWorker,
    mut settings: AppSettingsData,
    remote_auth: Arc<RwLock<RemoteArtworkAuth>>,
    cmd_rx: Receiver<BackendCommand>,
    events: EventHub,
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

    let audio_banner = (!pipewire_active).then(|| audio_msg.clone());
    let mut cached_audio_devices = enumerate_alsa_devices();

    events.emit(BackendEvent::AudioBackendReady {
        pipewire_active,
        message: audio_msg,
    });
    events.emit(BackendEvent::AudioDevicesLoaded(
        cached_audio_devices.clone(),
    ));

    let http_arc = Arc::new(http.clone());
    let innertube = Arc::new(InnertubeClient::with_http_client(Arc::clone(&http_arc)));
    // Pre-warm visitorData in the background so the first play avoids `/sw.js_data` latency.
    {
        let _rt_guard = rt.enter();
        std::mem::drop(innertube.spawn_prewarm_visitor_data());
    }

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
    let cipher_sandbox = Arc::new(CipherSandbox::new(Some(paths.cache_dir.join("cipher"))));
    let settings_path = paths.settings_file();

    let mut stream_backend_choice = StreamBackendChoice::Auto;
    let mut resolver = build_stream_resolver(
        &http_arc,
        &innertube,
        &uploads,
        &cipher_sandbox,
        stream_backend_choice,
    );

    // Best-effort initial Secret Service load with bounded timeout so locked/headless
    // desktops never hang startup; all subsequent keyring writes run asynchronously.
    let secret_store: Option<Arc<KeyringSecretStore>> = rt
        .block_on(async {
            tokio::time::timeout(Duration::from_millis(800), KeyringSecretStore::open())
                .await
                .ok()
                .and_then(Result::ok)
        })
        .map(Arc::new);
    if let Some(ref store) = secret_store {
        let store_c = Arc::clone(store);
        let innertube_c = Arc::clone(&innertube);
        let remote_auth_c = Arc::clone(&remote_auth);
        let _ = rt.block_on(tokio::time::timeout(Duration::from_millis(800), async move {
            if let Ok(Some(cookie)) = store_c.get(SecretKey::YoutubeCookie).await {
                innertube_c.set_cookie(Some(cookie.expose().to_owned()));
            }
            let webdav_pw = store_c
                .get(SecretKey::WebDavPassword)
                .await
                .ok()
                .flatten()
                .map(|s| s.expose().to_owned())
                .unwrap_or_default();
            let smb_pw = store_c
                .get(SecretKey::SmbPassword)
                .await
                .ok()
                .flatten()
                .map(|s| s.expose().to_owned())
                .unwrap_or_default();
            if let Ok(mut guard) = remote_auth_c.write() {
                guard.webdav.password = webdav_pw;
                guard.smb.password = smb_pw;
            }
        }));
    }

    // Start MPRIS2 D-Bus server (`org.mpris.MediaPlayer2.bitchord`).
    let mpris_state = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), MprisService::start())
            .await
            .ok()
            .and_then(Result::ok)
    });
    let (mpris_svc, mut mpris_rx) = match mpris_state {
        Some((svc, rx)) => (Some(Arc::new(svc)), Some(rx)),
        None => (None, None),
    };

    let (internal_tx, internal_rx) = async_channel::unbounded::<InternalMsg>();
    let load_gen = Arc::new(AtomicU64::new(0));
    let preload_gen = Arc::new(AtomicU64::new(0));
    let party_gen = Arc::new(AtomicU64::new(0));
    let analysis_sem = Arc::new(tokio::sync::Semaphore::new(1));
    let onnx_paths = Arc::new(OnnxModelPaths::discover(&paths.models_dir()));

    let mut active_load_cancel: Option<StreamCancelToken> = None;
    let mut preload_cancel: Option<StreamCancelToken> = None;

    let mut queue = QueueController::default();
    let mut current_playing_song: Option<Song> = None;
    let mut preloaded_next_song: Option<Song> = None;
    let current_lyrics: Arc<Mutex<Option<LyricsPayload>>> = Arc::new(Mutex::new(None));
    let mut lyrics_sync_state = LyricsSyncState::Idle;
    let mut home_fetch = HomeFetchState::default();
    let mut suggest_gate = SearchSuggestGate::default();
    let mut cached_library: Option<LibrarySnapshot> = None;

    let mut eq_enabled = false;
    let mut eq_preset = EqualizerPreset::Flat;
    let mut eq_bands_db = [0.0_f32; EqLayout::MANUAL_COUNT];

    let mut party_runtime: Option<PartyRuntime> = None;
    let party_state_shared: Arc<Mutex<ListenTogetherState>> =
        Arc::new(Mutex::new(ListenTogetherState::default()));

    let mut error_retry = PlaybackErrorRetry::default();
    let mut last_output_error: Option<String> = None;
    let mut last_reconnect_attempt: Option<Instant> = None;
    let mut last_mpris_key: Option<MprisPublishKey> = None;

    // Initial asynchronous SQLite library snapshot load (never blocks backend startup).
    spawn_library_refresh(&rt, &db_worker, &internal_tx, &events);

    let mut last_tick = Instant::now();
    let mut loop_state = PlaybackLoopState::default();

    loop {
        // 1. Wait reactively for an inbound UI command or an async internal completion.
        let wait_ms = if engine.snapshot().state == PlaybackState::Playing {
            35
        } else {
            90
        };
        let mut cmds = Vec::new();
        let mut internal_msgs = Vec::new();

        rt.block_on(async {
            tokio::select! {
                res = cmd_rx.recv() => {
                    if let Ok(c) = res {
                        cmds.push(c);
                    }
                }
                res = internal_rx.recv() => {
                    if let Ok(m) = res {
                        internal_msgs.push(m);
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
            }
        });
        while let Ok(c) = cmd_rx.try_recv() {
            cmds.push(c);
        }
        while let Ok(m) = internal_rx.try_recv() {
            internal_msgs.push(m);
        }

        // 2. Drain inbound MPRIS2 D-Bus commands.
        if let Some(ref mut rx) = mpris_rx {
            while let Ok(mcmd) = rx.try_recv() {
                match mcmd {
                    MprisCommand::Play => {
                        if engine.snapshot().state != PlaybackState::Playing {
                            cmds.push(BackendCommand::PlayPause);
                        }
                    }
                    MprisCommand::Pause => {
                        engine.pause();
                        error_retry.reset();
                    }
                    MprisCommand::PlayPause => cmds.push(BackendCommand::PlayPause),
                    MprisCommand::Stop => {
                        cancel_all_loads(
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                        );
                        loop_state.clear_in_flight();
                        error_retry.reset();
                        engine.stop();
                    }
                    MprisCommand::Next => cmds.push(BackendCommand::NextTrack),
                    MprisCommand::Previous => cmds.push(BackendCommand::PrevTrack),
                    MprisCommand::SetVolume(v) => cmds.push(BackendCommand::SetVolume(v as f32)),
                    MprisCommand::SetRate(r) => cmds.push(BackendCommand::SetSpeed(r as f32)),
                    MprisCommand::SetShuffle(want) => {
                        if queue.shuffle() != want {
                            cmds.push(BackendCommand::ToggleShuffle);
                        }
                    }
                    MprisCommand::SetLoopStatus(status) => {
                        let target = match status {
                            LoopStatus::None => (false, false),
                            LoopStatus::Playlist => (true, false),
                            LoopStatus::Track => (false, true),
                        };
                        let mut guard = 0;
                        while (queue.repeat_all(), queue.repeat_one()) != target && guard < 3 {
                            queue.cycle_repeat();
                            guard += 1;
                        }
                        emit_queue(&queue, &events);
                        invalidate_and_repreload(
                            &mut engine,
                            &queue,
                            &load_gen,
                            &preload_gen,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &internal_tx,
                        );
                    }
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

        // 3. Apply completed off-thread track loads, preloads, and async background results.
        for msg in internal_msgs {
            match msg {
                InternalMsg::Ready {
                    generation,
                    mut song,
                    decoder,
                    prebuffered,
                    loudness_db,
                    source_label,
                    resolved_duration_ms,
                    resume_position_ms,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation {
                        continue; // Superseded by a newer user click!
                    }
                    loop_state.finish_in_flight_if_matches(generation);
                    let effective_dur_ms = decoder
                        .info()
                        .duration_ms
                        .filter(|&d| d > 0)
                        .or(resolved_duration_ms.filter(|&d| d > 0))
                        .unwrap_or_else(|| duration_millis(song.duration_text.as_deref()));
                    if effective_dur_ms > 0 {
                        let dur_text = format_duration_ms(effective_dur_ms);
                        if song.duration_text.as_deref().is_none_or(str::is_empty) {
                            song.duration_text = Some(dur_text.clone());
                        }
                        if queue.update_duration_text(&song.video_id, &dur_text) {
                            emit_queue(&queue, &events);
                        }
                        db_worker.update_song_duration(
                            song.video_id.clone(),
                            effective_dur_ms,
                            song.duration_text.clone(),
                        );
                    }

                    match engine.load_prepared_decoder(
                        *decoder,
                        prebuffered,
                        song.clone(),
                        loudness_db,
                        Some(&source_label),
                    ) {
                        Ok(info) => {
                            if resume_position_ms > 0 {
                                let _ = engine.seek_ms(resume_position_ms);
                            }
                            // Pre-decoded packets are pushed immediately by the worker; wait
                            // only briefly for the ring commit without stalling the 35ms loop.
                            engine.wait_prebuffer(200, Duration::from_millis(80));
                            engine.play();
                            let snap = engine.snapshot();
                            loop_state.last_state = snap.state;
                            loop_state.last_song_vid = Some(song.video_id.clone());
                            current_playing_song = Some(song.clone());
                            preloaded_next_song = None;
                            loop_state.last_published_pos_ms = snap.position_ms;
                            error_retry.on_load_succeeded(&song.video_id);

                            events.emit(BackendEvent::PlaybackUpdated(Box::new(snap.clone())));
                            events.emit(BackendEvent::StatusNotice(format!(
                                "Playing {} - {}",
                                song.title, song.artist
                            )));

                            sync_mpris_if_changed(
                                &rt,
                                mpris_svc.as_ref(),
                                &mut last_mpris_key,
                                &snap,
                                &queue,
                            );

                            let track_dur_ms = info
                                .duration_ms
                                .filter(|&d| d > 0)
                                .or((effective_dur_ms > 0).then_some(effective_dur_ms));
                            record_track_started_async(
                                &rt,
                                &db_worker,
                                &song,
                                track_dur_ms,
                                settings.discord_rpc_enabled,
                                &snap,
                                &internal_tx,
                                &events,
                            );

                            // Start cache-first, delayed lyrics lookup now that playback started
                            // and accurate duration is known.
                            lyrics_sync_state = LyricsSyncState::Loading;
                            spawn_lyrics_lookup(
                                generation,
                                Arc::clone(&load_gen),
                                &rt,
                                &db_worker,
                                &lyrics_repo,
                                &current_lyrics,
                                song.clone(),
                                track_dur_ms.unwrap_or(snap.duration_ms),
                                &internal_tx,
                                &events,
                            );

                            // Quietly pre-resolve & preload the next track onto standby deck.
                            spawn_standby_preload_for_queue(
                                generation,
                                &load_gen,
                                &preload_gen,
                                &mut preload_cancel,
                                &queue,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &internal_tx,
                            );
                        }
                        Err(err) => {
                            let err_str = err.to_string();
                            handle_async_load_failure(
                                &engine,
                                &mut loop_state,
                                &mut error_retry,
                                &song,
                                err_str,
                                Instant::now(),
                                &events,
                            );
                        }
                    }
                }
                InternalMsg::Failed {
                    generation,
                    song,
                    error,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation {
                        continue;
                    }
                    loop_state.finish_in_flight_if_matches(generation);
                    handle_async_load_failure(
                        &engine,
                        &mut loop_state,
                        &mut error_retry,
                        &song,
                        error,
                        Instant::now(),
                        &events,
                    );
                }
                InternalMsg::PreloadReady {
                    generation,
                    preload_generation,
                    mut song,
                    decoder,
                    prebuffered,
                    loudness_db,
                    source_label,
                    resolved_duration_ms,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation
                        || preload_gen.load(Ordering::Acquire) != preload_generation
                    {
                        continue;
                    }
                    // Verify queue didn't shift underneath this preload before committing standby.
                    let expected_vid = queue.preload_candidate().map(|s| s.video_id);
                    if expected_vid.as_deref() != Some(song.video_id.as_str()) {
                        continue;
                    }
                    let effective_dur_ms = decoder
                        .info()
                        .duration_ms
                        .filter(|&d| d > 0)
                        .or(resolved_duration_ms.filter(|&d| d > 0))
                        .unwrap_or(0);
                    if effective_dur_ms > 0 {
                        let dur_text = format_duration_ms(effective_dur_ms);
                        if song.duration_text.as_deref().is_none_or(str::is_empty) {
                            song.duration_text = Some(dur_text.clone());
                        }
                        if queue.update_duration_text(&song.video_id, &dur_text) {
                            emit_queue(&queue, &events);
                        }
                        db_worker.update_song_duration(
                            song.video_id.clone(),
                            effective_dur_ms,
                            song.duration_text.clone(),
                        );
                    }
                    let next_for_plan = song.clone();
                    if engine
                        .preload_prepared_decoder(
                            *decoder,
                            prebuffered,
                            song,
                            loudness_db,
                            Some(&source_label),
                        )
                        .is_ok()
                    {
                        preloaded_next_song = Some(next_for_plan.clone());
                        if settings.automix_mode != AutomixMode::Off
                            && let Some(ref cur_song) = current_playing_song
                        {
                            spawn_automix_planning(
                                generation,
                                preload_generation,
                                Arc::clone(&load_gen),
                                Arc::clone(&preload_gen),
                                &rt,
                                Arc::clone(&analysis_sem),
                                db_worker.clone(),
                                Arc::clone(&onnx_paths),
                                settings.automix_mode,
                                settings.automix_phrase_bars,
                                settings.crossfade_seconds,
                                cur_song.clone(),
                                next_for_plan,
                                internal_tx.clone(),
                            );
                        }
                    }
                }
                InternalMsg::AutomixPlanned {
                    generation,
                    preload_generation,
                    plan,
                } => {
                    if load_gen.load(Ordering::Acquire) != generation
                        || preload_gen.load(Ordering::Acquire) != preload_generation
                    {
                        continue;
                    }
                    if plan.is_active() {
                        engine.set_transition_plan(Some(plan));
                        events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                }
                InternalMsg::LocalFolderScanned { folder, count } => {
                    if !settings.local_music_folders.contains(&folder) {
                        settings.local_music_folders.push(folder.clone());
                        save_settings_async(&rt, &settings_path, &settings);
                    }
                    spawn_library_refresh(&rt, &db_worker, &internal_tx, &events);
                    events.emit(BackendEvent::StatusNotice(format!(
                        "Scanned {count} audio file(s) from {}",
                        folder.display()
                    )));
                }
                InternalMsg::PartyConnected {
                    generation,
                    runtime,
                    message,
                } => {
                    if party_gen.load(Ordering::Acquire) != generation {
                        runtime.stop_flag.store(true, Ordering::Release);
                        continue;
                    }
                    if let Some(prev) = party_runtime.replace(runtime) {
                        prev.stop_flag.store(true, Ordering::Release);
                    }
                    events.emit(BackendEvent::StatusNotice(message));
                }
                InternalMsg::HomeFetchFinished(feed) => {
                    home_fetch.mark_finished(feed);
                }
                InternalMsg::LibraryCached(lib) => {
                    cached_library = Some(lib);
                }
                InternalMsg::SearchHistoryCached(history) => {
                    if let Some(ref mut lib) = cached_library {
                        lib.stats.search_history = history.len();
                        lib.search_history = history;
                    }
                }
                InternalMsg::LikedSongsCached(liked_ids) => {
                    if let Some(ref mut lib) = cached_library {
                        lib.liked_video_ids = liked_ids;
                    }
                }
                InternalMsg::LibraryTrackUpdated { song, stats } => {
                    if let Some(ref mut lib) = cached_library {
                        lib.stats = stats;
                        lib.songs.retain(|s| s.video_id != song.video_id);
                        lib.songs.insert(0, *song);
                        lib.songs.truncate(200);
                    }
                }
                InternalMsg::LyricsStateUpdated { video_id, state } => {
                    if current_playing_song
                        .as_ref()
                        .is_none_or(|s| s.video_id == video_id)
                    {
                        lyrics_sync_state = state;
                    }
                }
            }
        }

        // 4. Process all queued commands (zero blocking I/O inside this loop!).
        for cmd in cmds {
            match cmd {
                BackendCommand::Shutdown => {
                    cancel_all_loads(
                        &load_gen,
                        &preload_gen,
                        &mut active_load_cancel,
                        &mut preload_cancel,
                    );
                    engine.stop();
                    if let Some(pr) = party_runtime.take() {
                        pr.stop_flag.store(true, Ordering::Release);
                    }
                    db_worker.shutdown_blocking();
                    return;
                }
                BackendCommand::SyncFullState => {
                    let party_snap = party_state_shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let full = FullStateSnapshot {
                        pipewire_active,
                        audio_banner: audio_banner.clone(),
                        audio_devices: cached_audio_devices.clone(),
                        playback: loop_state.effective_snapshot(
                            engine.snapshot(),
                            &error_retry,
                            current_playing_song.as_ref(),
                        ),
                        queue: queue.snapshot(),
                        lyrics: lyrics_sync_state.clone(),
                        home: home_fetch.cached.clone(),
                        library: cached_library.clone(),
                        party: party_snap,
                        eq_enabled,
                        eq_preset,
                        eq_bands_db,
                        stream_backend: stream_backend_choice,
                        audio_output_pref: settings.audio_output_mode,
                        selected_alsa_device: settings.alsa_exclusive_device.clone(),
                        automix_mode: settings.automix_mode,
                        automix_phrase_bars: settings.automix_phrase_bars,
                        discord_rpc_enabled: settings.discord_rpc_enabled,
                    };
                    events.emit(BackendEvent::FullStateSynced(Box::new(full)));

                    if home_fetch.should_start_on_sync() {
                        home_fetch.mark_started();
                        spawn_load_home(&rt, &yt_repo, &internal_tx, &events);
                    }
                    if cached_library.is_none() {
                        spawn_library_refresh(&rt, &db_worker, &internal_tx, &events);
                    }
                }
                BackendCommand::LoadHome => {
                    home_fetch.mark_started();
                    spawn_load_home(&rt, &yt_repo, &internal_tx, &events);
                }
                BackendCommand::Search { query, filter } => {
                    let trimmed = query.trim().to_owned();
                    if trimmed.is_empty() {
                        continue;
                    }
                    suggest_gate.cancel();
                    events.emit(BackendEvent::SearchLoading {
                        query: trimmed.clone(),
                    });
                    let repo = Arc::clone(&yt_repo);
                    let events_c = events.clone();
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    rt.spawn(async move {
                        let res = repo
                            .search(&trimmed, filter, false)
                            .await
                            .map(|p| p.rows)
                            .map_err(|e| e.to_string());

                        if let Ok(ref rows) = res {
                            let record = build_query_history_record(&trimmed, rows, unix_now_ms());
                            let history = db_c.record_search_history(record).await;
                            let _ = itx
                                .send(InternalMsg::SearchHistoryCached(history.clone()))
                                .await;
                            events_c.emit(BackendEvent::SearchHistoryUpdated(history));
                        }

                        events_c.emit(BackendEvent::SearchLoaded {
                            query: trimmed,
                            result: res,
                        });
                    });
                }
                BackendCommand::SearchSuggest(query) => {
                    let trimmed = query.trim().to_owned();
                    if trimmed.len() < 2 {
                        suggest_gate.cancel();
                        events.emit(BackendEvent::SearchSuggestionsLoaded(
                            SearchSuggestionsPayload::default(),
                        ));
                        continue;
                    }
                    let (suggest_gen, suggest_gen_ref) = suggest_gate.begin();
                    let repo = Arc::clone(&yt_repo);
                    let events_c = events.clone();
                    let handle = rt.spawn(async move {
                        let structured = repo
                            .search_suggestions_structured(&trimmed)
                            .await
                            .unwrap_or_default();
                        if !SearchSuggestGate::is_current(&suggest_gen_ref, suggest_gen) {
                            return;
                        }
                        let mut items = structured.items;
                        items.truncate(6);
                        events_c.emit(BackendEvent::SearchSuggestionsLoaded(
                            SearchSuggestionsPayload {
                                query: trimmed,
                                queries: structured.queries,
                                items,
                            },
                        ));
                    });
                    suggest_gate.set_active(handle);
                }
                BackendCommand::RecordSearchSelection(mut record) => {
                    record.timestamp = unix_now_ms();
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let history = db_c.record_search_history(record).await;
                        let _ = itx
                            .send(InternalMsg::SearchHistoryCached(history.clone()))
                            .await;
                        events_c.emit(BackendEvent::SearchHistoryUpdated(history));
                    });
                }
                BackendCommand::RemoveSearchHistory(id) => {
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let history = db_c.remove_search_history(id).await;
                        let _ = itx
                            .send(InternalMsg::SearchHistoryCached(history.clone()))
                            .await;
                        events_c.emit(BackendEvent::SearchHistoryUpdated(history));
                    });
                }
                BackendCommand::ClearSearchHistory => {
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let history = db_c.clear_search_history().await;
                        let _ = itx
                            .send(InternalMsg::SearchHistoryCached(history.clone()))
                            .await;
                        events_c.emit(BackendEvent::SearchHistoryUpdated(history));
                    });
                }
                BackendCommand::ToggleLikeSong(song) => {
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let (_liked, liked_ids) = db_c.toggle_song_liked(*song).await;
                        let _ = itx
                            .send(InternalMsg::LikedSongsCached(liked_ids.clone()))
                            .await;
                        events_c.emit(BackendEvent::LikedSongsUpdated(liked_ids));
                    });
                }
                BackendCommand::LoadDetail {
                    browse_id,
                    kind,
                    fallback_title,
                } => {
                    events.emit(BackendEvent::DetailLoading);
                    let repo = Arc::clone(&yt_repo);
                    let events_c = events.clone();
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
                        events_c.emit(BackendEvent::DetailLoaded(res.map_err(|e| e.to_string())));
                    });
                }
                BackendCommand::PlayContext {
                    songs,
                    start_index,
                    source_title,
                    source_type,
                    source_id,
                } => {
                    let source = QueueSource {
                        title: source_title,
                        r#type: source_type,
                        id: source_id,
                    };
                    if let Some(song) =
                        queue.play_context(&songs, start_index, &source, unix_now_ms() as u64)
                    {
                        error_retry.reset();
                        emit_queue(&queue, &events);
                        lyrics_sync_state = LyricsSyncState::Loading;
                        start_async_song_load_at(
                            &mut engine,
                            &mut loop_state,
                            &mut current_playing_song,
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &current_lyrics,
                            song,
                            0,
                            false,
                            &internal_tx,
                            &events,
                        );
                    }
                }
                BackendCommand::PlayNext(song) => match queue.play_next(*song) {
                    PlayNextOutcome::StartedImmediately(s) => {
                        error_retry.reset();
                        emit_queue(&queue, &events);
                        lyrics_sync_state = LyricsSyncState::Loading;
                        start_async_song_load_at(
                            &mut engine,
                            &mut loop_state,
                            &mut current_playing_song,
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &current_lyrics,
                            *s,
                            0,
                            false,
                            &internal_tx,
                            &events,
                        );
                    }
                    PlayNextOutcome::InsertedNext => {
                        emit_queue(&queue, &events);
                        invalidate_and_repreload(
                            &mut engine,
                            &queue,
                            &load_gen,
                            &preload_gen,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &internal_tx,
                        );
                        events.emit(BackendEvent::StatusNotice("Added to Play Next".to_owned()));
                    }
                },
                BackendCommand::QueueJump(idx) => {
                    if let Some(song) = queue.jump(idx) {
                        error_retry.reset();
                        emit_queue(&queue, &events);
                        lyrics_sync_state = LyricsSyncState::Loading;
                        start_async_song_load_at(
                            &mut engine,
                            &mut loop_state,
                            &mut current_playing_song,
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &current_lyrics,
                            song,
                            0,
                            false,
                            &internal_tx,
                            &events,
                        );
                    }
                }
                BackendCommand::QueueRemove(idx) => match queue.remove(idx) {
                    QueueRemoveOutcome::OutOfBounds => {}
                    QueueRemoveOutcome::Emptied => {
                        cancel_all_loads(
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                        );
                        error_retry.reset();
                        preloaded_next_song = None;
                        current_playing_song = None;
                        engine.stop();
                        loop_state.sync_stopped();
                        emit_queue(&queue, &events);
                        events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                    QueueRemoveOutcome::ActiveReplaced(song) => {
                        error_retry.reset();
                        emit_queue(&queue, &events);
                        lyrics_sync_state = LyricsSyncState::Loading;
                        start_async_song_load_at(
                            &mut engine,
                            &mut loop_state,
                            &mut current_playing_song,
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &current_lyrics,
                            *song,
                            0,
                            false,
                            &internal_tx,
                            &events,
                        );
                    }
                    QueueRemoveOutcome::Updated => {
                        emit_queue(&queue, &events);
                        invalidate_and_repreload(
                            &mut engine,
                            &queue,
                            &load_gen,
                            &preload_gen,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &internal_tx,
                        );
                    }
                },
                BackendCommand::QueueClear => {
                    queue.clear();
                    cancel_all_loads(
                        &load_gen,
                        &preload_gen,
                        &mut active_load_cancel,
                        &mut preload_cancel,
                    );
                    error_retry.reset();
                    preloaded_next_song = None;
                    current_playing_song = None;
                    engine.stop();
                    loop_state.sync_stopped();
                    emit_queue(&queue, &events);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::ToggleShuffle => {
                    let _ = queue.toggle_shuffle(unix_now_ms() as u64);
                    emit_queue(&queue, &events);
                    invalidate_and_repreload(
                        &mut engine,
                        &queue,
                        &load_gen,
                        &preload_gen,
                        &mut preload_cancel,
                        &mut preloaded_next_song,
                        &rt,
                        &http_arc,
                        &resolver,
                        &remote_auth,
                        &db_worker,
                        &internal_tx,
                    );
                }
                BackendCommand::CycleRepeat => {
                    queue.cycle_repeat();
                    emit_queue(&queue, &events);
                    invalidate_and_repreload(
                        &mut engine,
                        &queue,
                        &load_gen,
                        &preload_gen,
                        &mut preload_cancel,
                        &mut preloaded_next_song,
                        &rt,
                        &http_arc,
                        &resolver,
                        &remote_auth,
                        &db_worker,
                        &internal_tx,
                    );
                }
                BackendCommand::PlayPause => {
                    let snap_before = engine.snapshot();
                    if let Some(fb) = engine.take_fallback_resume_state() {
                        // Resuming after an ALSA USB DAC unplug -> PipeWire fallback:
                        // reload and seek directly to the preserved playhead position.
                        error_retry.reset();
                        start_async_song_load_at(
                            &mut engine,
                            &mut loop_state,
                            &mut current_playing_song,
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &current_lyrics,
                            fb.song,
                            fb.position_ms,
                            false,
                            &internal_tx,
                            &events,
                        );
                    } else if snap_before.state == PlaybackState::Error {
                        let err_msg = snap_before.last_error.as_deref().unwrap_or("");
                        let candidate_song = current_playing_song
                            .clone()
                            .or_else(|| queue.current_song().cloned());
                        let resume_ms = if snap_before.position_ms > 0 {
                            snap_before.position_ms
                        } else {
                            candidate_song
                                .as_ref()
                                .and_then(|s| error_retry.resume_ms_for(&s.video_id))
                                .unwrap_or(loop_state.last_published_pos_ms)
                        };
                        let needs_fresh_resolve =
                            snap_before.current_song.is_none() || is_refusal_error(err_msg);
                        error_retry.reset();
                        if needs_fresh_resolve && let Some(song) = candidate_song {
                            resolver.invalidate_video(&song.video_id);
                            db_worker.delete_stream_cache_for_video(song.video_id.clone());
                            start_async_song_load_at(
                                &mut engine,
                                &mut loop_state,
                                &mut current_playing_song,
                                &load_gen,
                                &preload_gen,
                                &mut active_load_cancel,
                                &mut preload_cancel,
                                &mut preloaded_next_song,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &current_lyrics,
                                song,
                                resume_ms,
                                true,
                                &internal_tx,
                                &events,
                            );
                        } else {
                            engine.play();
                            events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                        }
                    } else if snap_before.current_song.is_none() {
                        if loop_state.in_flight.is_none()
                            && let Some(target) = resolve_manual_resume_target(
                                None,
                                current_playing_song
                                    .clone()
                                    .or_else(|| queue.current_song().cloned()),
                                &error_retry,
                                loop_state.last_published_pos_ms,
                            )
                        {
                            error_retry.reset();
                            if target.bypass_cache {
                                resolver.invalidate_video(&target.song.video_id);
                                db_worker
                                    .delete_stream_cache_for_video(target.song.video_id.clone());
                            }
                            start_async_song_load_at(
                                &mut engine,
                                &mut loop_state,
                                &mut current_playing_song,
                                &load_gen,
                                &preload_gen,
                                &mut active_load_cancel,
                                &mut preload_cancel,
                                &mut preloaded_next_song,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &current_lyrics,
                                target.song,
                                target.resume_ms,
                                target.bypass_cache,
                                &internal_tx,
                                &events,
                            );
                        }
                    } else {
                        engine.toggle_play_pause();
                        events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                }
                BackendCommand::NextTrack => {
                    if let Some(next_idx) = queue.next_index(false)
                        && let Some(next_song) = queue.song_at(next_idx).cloned()
                    {
                        error_retry.reset();
                        let _ = queue.jump(next_idx);
                        emit_queue(&queue, &events);

                        // Fast path: promote already-preloaded standby deck instantly without
                        // tearing down audio or re-resolving the stream!
                        if engine.has_standby_for(&next_song.video_id)
                            && engine.activate_standby_now()
                        {
                            let new_gen = cancel_all_loads(
                                &load_gen,
                                &preload_gen,
                                &mut active_load_cancel,
                                &mut preload_cancel,
                            );
                            let snap = engine.snapshot();
                            let mut promoted =
                                snap.current_song.clone().unwrap_or_else(|| next_song.clone());
                            if snap.duration_ms > 0 {
                                let dur_text = format_duration_ms(snap.duration_ms);
                                if promoted.duration_text.as_deref().is_none_or(str::is_empty) {
                                    promoted.duration_text = Some(dur_text.clone());
                                }
                                if queue.update_duration_text(&promoted.video_id, &dur_text) {
                                    emit_queue(&queue, &events);
                                }
                                db_worker.update_song_duration(
                                    promoted.video_id.clone(),
                                    snap.duration_ms,
                                    promoted.duration_text.clone(),
                                );
                            }
                            loop_state.clear_in_flight();
                            loop_state.last_state = snap.state;
                            loop_state.last_song_vid = Some(promoted.video_id.clone());
                            current_playing_song = Some(promoted.clone());
                            preloaded_next_song = None;
                            loop_state.last_published_pos_ms = snap.position_ms;

                            events.emit(BackendEvent::PlaybackUpdated(Box::new(snap.clone())));
                            events.emit(BackendEvent::StatusNotice(format!(
                                "Playing {} - {}",
                                promoted.title, promoted.artist
                            )));
                            sync_mpris_if_changed(
                                &rt,
                                mpris_svc.as_ref(),
                                &mut last_mpris_key,
                                &snap,
                                &queue,
                            );

                            record_track_started_async(
                                &rt,
                                &db_worker,
                                &promoted,
                                (snap.duration_ms > 0).then_some(snap.duration_ms),
                                settings.discord_rpc_enabled,
                                &snap,
                                &internal_tx,
                                &events,
                            );
                            lyrics_sync_state = LyricsSyncState::Loading;
                            spawn_lyrics_lookup(
                                new_gen,
                                Arc::clone(&load_gen),
                                &rt,
                                &db_worker,
                                &lyrics_repo,
                                &current_lyrics,
                                promoted,
                                snap.duration_ms,
                                &internal_tx,
                                &events,
                            );
                            spawn_standby_preload_for_queue(
                                new_gen,
                                &load_gen,
                                &preload_gen,
                                &mut preload_cancel,
                                &queue,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &internal_tx,
                            );
                        } else {
                            lyrics_sync_state = LyricsSyncState::Loading;
                            start_async_song_load_at(
                                &mut engine,
                                &mut loop_state,
                                &mut current_playing_song,
                                &load_gen,
                                &preload_gen,
                                &mut active_load_cancel,
                                &mut preload_cancel,
                                &mut preloaded_next_song,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &current_lyrics,
                                next_song,
                                0,
                                false,
                                &internal_tx,
                                &events,
                            );
                        }
                    }
                }
                BackendCommand::PrevTrack => {
                    if engine.position_ms() > 3_500 {
                        if let Some(fb) = engine.take_fallback_resume_state() {
                            error_retry.reset();
                            start_async_song_load_at(
                                &mut engine,
                                &mut loop_state,
                                &mut current_playing_song,
                                &load_gen,
                                &preload_gen,
                                &mut active_load_cancel,
                                &mut preload_cancel,
                                &mut preloaded_next_song,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &current_lyrics,
                                fb.song,
                                0,
                                false,
                                &internal_tx,
                                &events,
                            );
                        } else if let Ok(actual) = engine.seek_ms(0) {
                            notify_mpris_seeked(&rt, mpris_svc.as_ref(), actual);
                            events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                        }
                    } else if let Some(prev_idx) = queue.prev_index()
                        && let Some(song) = queue.jump(prev_idx)
                    {
                        error_retry.reset();
                        emit_queue(&queue, &events);
                        lyrics_sync_state = LyricsSyncState::Loading;
                        start_async_song_load_at(
                            &mut engine,
                            &mut loop_state,
                            &mut current_playing_song,
                            &load_gen,
                            &preload_gen,
                            &mut active_load_cancel,
                            &mut preload_cancel,
                            &mut preloaded_next_song,
                            &rt,
                            &http_arc,
                            &resolver,
                            &remote_auth,
                            &db_worker,
                            &current_lyrics,
                            song,
                            0,
                            false,
                            &internal_tx,
                            &events,
                        );
                    }
                }
                BackendCommand::SeekMs(ms) => {
                    if let Ok(actual) = engine.seek_ms(ms) {
                        notify_mpris_seeked(&rt, mpris_svc.as_ref(), actual);
                        events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                }
                BackendCommand::SeekDeltaMs(delta) => {
                    let cur = engine.position_ms() as i64;
                    let target = (cur + delta).max(0) as u64;
                    if let Ok(actual) = engine.seek_ms(target) {
                        notify_mpris_seeked(&rt, mpris_svc.as_ref(), actual);
                        events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                    }
                }
                BackendCommand::SetVolume(vol) => {
                    engine.set_volume(vol);
                    settings.volume = vol.clamp(0.0, 1.0);
                    save_settings_async(&rt, &settings_path, &settings);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::ToggleMute => {
                    let next_mute = !engine.snapshot().muted;
                    engine.set_muted(next_mute);
                    settings.muted = next_mute;
                    save_settings_async(&rt, &settings_path, &settings);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetSpeed(speed) => {
                    engine.set_speed(speed);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetSkipSilence(on) => {
                    engine.set_skip_silence(on);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetSpatial(on) => {
                    engine.set_spatial_audio(on);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetCrossfadeSeconds(secs) => {
                    engine.set_crossfade_seconds(secs);
                    settings.crossfade_seconds = secs;
                    save_settings_async(&rt, &settings_path, &settings);
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetAutomix { mode, phrase_bars } => {
                    let bars = match phrase_bars {
                        8 | 16 => phrase_bars,
                        _ => 0,
                    };
                    settings.automix_mode = mode;
                    settings.automix_phrase_bars = bars;
                    save_settings_async(&rt, &settings_path, &settings);
                    if mode == AutomixMode::Off {
                        engine.set_transition_plan(None);
                    } else if let (Some(cur_song), Some(next_song)) =
                        (current_playing_song.clone(), preloaded_next_song.clone())
                    {
                        let generation = load_gen.load(Ordering::Acquire);
                        let pgen = preload_gen.load(Ordering::Acquire);
                        spawn_automix_planning(
                            generation,
                            pgen,
                            Arc::clone(&load_gen),
                            Arc::clone(&preload_gen),
                            &rt,
                            Arc::clone(&analysis_sem),
                            db_worker.clone(),
                            Arc::clone(&onnx_paths),
                            mode,
                            bars,
                            settings.crossfade_seconds,
                            cur_song,
                            next_song,
                            internal_tx.clone(),
                        );
                    }
                    events.emit(BackendEvent::StatusNotice(format!(
                        "Automix mode: {}",
                        mode.label()
                    )));
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::SetEq {
                    enabled,
                    preset,
                    bands_db,
                } => {
                    eq_enabled = enabled;
                    eq_preset = preset;
                    eq_bands_db = bands_db;
                    let curve: EqCurve = if preset == EqualizerPreset::Custom {
                        manual_curve(&bands_db)
                    } else {
                        manual_curve(preset.bands_db())
                    };
                    engine.set_eq(EqTuningSnapshot::from_curve(enabled, &curve, 0.0));
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::StartSleepTimerMinutes(mins) => {
                    engine.sleep_timer_mut().start(mins);
                    events.emit(BackendEvent::StatusNotice(format!(
                        "Sleep timer armed for {mins} min"
                    )));
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::StartSleepAfterTrack => {
                    engine.sleep_timer_mut().start_after_track();
                    events.emit(BackendEvent::StatusNotice(
                        "Sleep timer armed: pause after current track".to_owned(),
                    ));
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::CancelSleepTimer => {
                    engine.sleep_timer_mut().cancel();
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                }
                BackendCommand::RefreshLibrary => {
                    spawn_library_refresh(&rt, &db_worker, &internal_tx, &events);
                }
                BackendCommand::ScanLocalFolder(folder) => {
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    events.emit(BackendEvent::StatusNotice(format!(
                        "Scanning {}...",
                        folder.display()
                    )));
                    rt.spawn(async move {
                        let scan_dir = folder.clone();
                        let scanned =
                            tokio::task::spawn_blocking(move || scan_local_folder_songs(&scan_dir))
                                .await
                                .map_err(|e| format!("Local folder scan failed: {e}"))
                                .and_then(|r| r);
                        match scanned {
                            Ok(songs) => match db_c.upsert_songs_batch(songs).await {
                                Ok(count) => {
                                    let _ = itx
                                        .send(InternalMsg::LocalFolderScanned { folder, count })
                                        .await;
                                }
                                Err(err) => events_c.emit(BackendEvent::ErrorNotice(err)),
                            },
                            Err(err) => events_c.emit(BackendEvent::ErrorNotice(err)),
                        }
                    });
                }
                BackendCommand::TranslateLyrics { target_lang } => {
                    let current = current_lyrics
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    if let Some(payload) = current {
                        let translator = Arc::clone(&lyrics_translator);
                        let events_c = events.clone();
                        let lyrics_slot = Arc::clone(&current_lyrics);
                        let itx = internal_tx.clone();
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
                                    let _ = itx
                                        .send(InternalMsg::LyricsStateUpdated {
                                            video_id: updated.video_id.clone(),
                                            state: LyricsSyncState::Ready(updated.clone()),
                                        })
                                        .await;
                                    events_c.emit(BackendEvent::LyricsLoaded {
                                        video_id: updated.video_id.clone(),
                                        result: Ok(updated),
                                    });
                                }
                                TranslationResult::SameLanguage { language } => {
                                    events_c.emit(BackendEvent::StatusNotice(format!(
                                        "Lyrics are already in {language}"
                                    )));
                                }
                                TranslationResult::Unavailable => {
                                    events_c.emit(BackendEvent::ErrorNotice(
                                        "Lyrics translation unavailable for this track.".to_owned(),
                                    ));
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
                    resolver.on_session_changed();
                    db_worker.clear_stream_cache();

                    let store_opt = secret_store.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let res = match store_opt {
                            Some(store) => store.set(SecretKey::YoutubeCookie, &trimmed).await,
                            None => Err(bitchord_storage::SecretError::Keyring(
                                "Secret Service daemon unavailable".to_owned(),
                            )),
                        };
                        match res {
                            Ok(()) => {
                                events_c.emit(BackendEvent::StatusNotice(
                                    "YouTube Music cookie saved in Linux Secret Service".to_owned(),
                                ));
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::StatusNotice(format!(
                                    "Cookie active for this session (keyring unavailable: {err})"
                                )));
                            }
                        }
                    });
                }
                BackendCommand::ClearYoutubeCookie => {
                    innertube.set_cookie(None);
                    resolver.on_session_changed();
                    db_worker.clear_stream_cache();
                    if let Some(store) = secret_store.clone() {
                        rt.spawn(async move {
                            let _ = store.delete(SecretKey::YoutubeCookie).await;
                        });
                    }
                    events.emit(BackendEvent::StatusNotice(
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
                    save_settings_async(&rt, &settings_path, &settings);
                    let effective_pw = if password.is_empty() {
                        remote_auth
                            .read()
                            .map(|g| g.webdav.password.clone())
                            .unwrap_or_default()
                    } else {
                        if let Some(store) = secret_store.clone() {
                            let pw_c = password.clone();
                            rt.spawn(async move {
                                let _ = store.set(SecretKey::WebDavPassword, &pw_c).await;
                            });
                        }
                        password
                    };
                    if let Ok(mut guard) = remote_auth.write() {
                        guard.webdav = WebDavConfig::new(norm_url, user_trimmed, effective_pw);
                    }
                    events.emit(BackendEvent::StatusNotice(
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
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let client = WebDavClient::with_http(http_c);
                        match client.test_connection(&cfg).await {
                            Ok(()) => {
                                events_c.emit(BackendEvent::StatusNotice(format!(
                                    "WebDAV connected: {}",
                                    WebDavConfig::normalize_url(&cfg.url)
                                )));
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(format!(
                                    "WebDAV test failed: {err}"
                                )));
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
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        events_c.emit(BackendEvent::StatusNotice(
                            "Syncing WebDAV music library...".to_owned(),
                        ));
                        let client = WebDavClient::with_http(http_c);
                        match client
                            .list_songs(&cfg, bitchord_netstorage::webdav::DEFAULT_MAX_FILES)
                            .await
                        {
                            Ok(songs) => {
                                let count = db_c
                                    .replace_prefixed_songs("webdav:".to_owned(), songs)
                                    .await
                                    .unwrap_or(0);
                                let lib = db_c.fetch_library_snapshot().await;
                                let _ = itx.send(InternalMsg::LibraryCached(lib.clone())).await;
                                events_c.emit(BackendEvent::LibraryLoaded(lib));
                                events_c.emit(BackendEvent::StatusNotice(format!(
                                    "Synced {count} WebDAV track(s) into Library"
                                )));
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(format!(
                                    "WebDAV sync failed: {err}"
                                )));
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
                    save_settings_async(&rt, &settings_path, &settings);
                    let effective_pw = if password.is_empty() {
                        remote_auth
                            .read()
                            .map(|g| g.smb.password.clone())
                            .unwrap_or_default()
                    } else {
                        if let Some(store) = secret_store.clone() {
                            let pw_c = password.clone();
                            rt.spawn(async move {
                                let _ = store.set(SecretKey::SmbPassword, &pw_c).await;
                            });
                        }
                        password
                    };
                    if let Ok(mut guard) = remote_auth.write() {
                        guard.smb = SmbConfig::new(host_t, share_t, base_t, user_t, effective_pw);
                    }
                    events.emit(BackendEvent::StatusNotice(
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
                    let events_c = events.clone();
                    rt.spawn(async move {
                        match SmbClient::new().test_connection(&cfg).await {
                            Ok(()) => {
                                events_c.emit(BackendEvent::StatusNotice(format!(
                                    "SMB connected: //{}/{}",
                                    SmbConfig::normalize_host(&cfg.host),
                                    cfg.normalized_share()
                                )));
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(format!(
                                    "SMB test failed: {err}"
                                )));
                            }
                        }
                    });
                }
                BackendCommand::SyncSmb => {
                    let cfg = remote_auth
                        .read()
                        .map(|g| g.smb.clone())
                        .unwrap_or_default();
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        events_c.emit(BackendEvent::StatusNotice(
                            "Syncing SMB2/3 music share...".to_owned(),
                        ));
                        match SmbClient::new()
                            .list_songs(&cfg, bitchord_netstorage::smb::SMB_DEFAULT_MAX_FILES)
                            .await
                        {
                            Ok(songs) => {
                                let count = db_c
                                    .replace_prefixed_songs("smb:".to_owned(), songs)
                                    .await
                                    .unwrap_or(0);
                                let lib = db_c.fetch_library_snapshot().await;
                                let _ = itx.send(InternalMsg::LibraryCached(lib.clone())).await;
                                events_c.emit(BackendEvent::LibraryLoaded(lib));
                                events_c.emit(BackendEvent::StatusNotice(format!(
                                    "Synced {count} SMB track(s) into Library"
                                )));
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(format!(
                                    "SMB sync failed: {err}"
                                )));
                            }
                        }
                    });
                }
                BackendCommand::SetStreamBackend(choice) => {
                    stream_backend_choice = choice;
                    resolver = build_stream_resolver(
                        &http_arc,
                        &innertube,
                        &uploads,
                        &cipher_sandbox,
                        choice,
                    );
                    events.emit(BackendEvent::StatusNotice(format!(
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
                            save_settings_async(&rt, &settings_path, &settings);
                            events.emit(BackendEvent::StatusNotice(format!(
                                "Audio output switched to {}",
                                engine_mode.label()
                            )));
                            events.emit(BackendEvent::PlaybackUpdated(Box::new(engine.snapshot())));
                        }
                        Err(err) => {
                            events.emit(BackendEvent::ErrorNotice(format!(
                                "Cannot switch output to {}: {err}",
                                engine_mode.label()
                            )));
                        }
                    }
                }
                BackendCommand::RefreshAudioDevices => {
                    let devices = enumerate_alsa_devices();
                    let count = devices.len();
                    cached_audio_devices = devices.clone();
                    events.emit(BackendEvent::AudioDevicesLoaded(devices));
                    events.emit(BackendEvent::StatusNotice(format!(
                        "Detected {count} ALSA hardware playback endpoint(s)"
                    )));
                }
                BackendCommand::SetDiscordRpc(enabled) => {
                    settings.discord_rpc_enabled = enabled;
                    save_settings_async(&rt, &settings_path, &settings);
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
                    let db_c = db_worker.clone();
                    let itx = internal_tx.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        let pid = SpotifyImporter::extract_playlist_id(&url_or_id)
                            .unwrap_or_else(|| url_or_id.trim().to_owned());
                        match SpotifyImporter::fetch_playlist_tracks(&http_c, &pid).await {
                            Ok((title, tracks)) => {
                                let imported_count = tracks.len();
                                let now = unix_now_ms();
                                let pl_id = format!("spotify:{pid}");
                                let playlist = LocalPlaylist {
                                    id: pl_id,
                                    title: title.clone(),
                                    description: format!(
                                        "Imported from Spotify ({imported_count} tracks)"
                                    ),
                                    thumbnail_url: None,
                                    source: "spotify".to_owned(),
                                    created_at: now,
                                    updated_at: now,
                                };
                                let mut songs = Vec::with_capacity(tracks.len());
                                let mut ids = Vec::with_capacity(tracks.len());
                                for (idx, tr) in tracks.into_iter().enumerate() {
                                    let vid = format!("sp:{pid}:{idx}");
                                    songs.push(Song::new(&vid, tr.title, tr.artist, None));
                                    ids.push(vid);
                                }
                                let _ = db_c.import_spotify_playlist(playlist, songs, ids).await;
                                let lib = db_c.fetch_library_snapshot().await;
                                let _ = itx.send(InternalMsg::LibraryCached(lib.clone())).await;
                                events_c.emit(BackendEvent::LibraryLoaded(lib));
                                events_c.emit(BackendEvent::StatusNotice(format!(
                                    "Imported Spotify playlist \"{title}\" ({imported_count} tracks)"
                                )));
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(format!(
                                    "Spotify import failed: {err}"
                                )));
                            }
                        }
                    });
                }
                BackendCommand::PartyProbeServer(server_url) => {
                    let http_c = http.clone();
                    let events_c = events.clone();
                    rt.spawn(async move {
                        match ListenTogetherClient::new(http_c, &server_url) {
                            Ok(client) => {
                                let probe = client.healthz(Duration::from_secs(6)).await;
                                events_c.emit(BackendEvent::PartyServerProbed {
                                    server: client.base_url().to_owned(),
                                    online: probe.is_online,
                                    latency_ms: probe.latency_ms,
                                });
                            }
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(err.to_string()));
                            }
                        }
                    });
                }
                BackendCommand::PartyCreate {
                    server_url,
                    nickname,
                } => {
                    let pgen = party_gen.fetch_add(1, Ordering::AcqRel) + 1;
                    if let Some(prev) = party_runtime.take() {
                        prev.stop_flag.store(true, Ordering::Release);
                    }
                    settings.listen_together_server = server_url.clone();
                    settings.listen_together_nickname = nickname.clone();
                    save_settings_async(&rt, &settings_path, &settings);

                    let req = JoinRequest {
                        user_id: settings.device_id.clone(),
                        device_id: settings.device_id.clone(),
                        display_name: if nickname.trim().is_empty() {
                            branding::DEFAULT_PARTY_NICKNAME.to_owned()
                        } else {
                            nickname.trim().to_owned()
                        },
                        avatar_url: None,
                        max_members: None,
                        autoplay_enabled: None,
                    };
                    let http_c = http.clone();
                    let rt_c = rt.clone();
                    let shared_c = Arc::clone(&party_state_shared);
                    let events_c = events.clone();
                    let itx = internal_tx.clone();
                    let pgen_ref = Arc::clone(&party_gen);
                    rt.spawn(async move {
                        match ListenTogetherClient::new(http_c, &server_url) {
                            Ok(client) => match client.create_party(&req).await {
                                Ok(membership) => {
                                    if pgen_ref.load(Ordering::Acquire) != pgen {
                                        return;
                                    }
                                    let code = membership.party.code.clone();
                                    let runtime = spawn_party_ws_loop(
                                        &rt_c,
                                        client,
                                        code.clone(),
                                        membership.token.clone(),
                                        shared_c,
                                        events_c,
                                    );
                                    let _ = itx
                                        .send(InternalMsg::PartyConnected {
                                            generation: pgen,
                                            runtime,
                                            message: format!("Created Listen Together party {code}"),
                                        })
                                        .await;
                                }
                                Err(err) => {
                                    events_c.emit(BackendEvent::ErrorNotice(err.to_string()));
                                }
                            },
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(err.to_string()));
                            }
                        }
                    });
                }
                BackendCommand::PartyJoin {
                    server_url,
                    code_or_url,
                    nickname,
                } => {
                    let pgen = party_gen.fetch_add(1, Ordering::AcqRel) + 1;
                    let code = parse_invite_url(Some(&code_or_url))
                        .unwrap_or_else(|| code_or_url.trim().to_uppercase());
                    if let Some(prev) = party_runtime.take() {
                        prev.stop_flag.store(true, Ordering::Release);
                    }
                    settings.listen_together_server = server_url.clone();
                    settings.listen_together_nickname = nickname.clone();
                    save_settings_async(&rt, &settings_path, &settings);

                    let req = JoinRequest {
                        user_id: settings.device_id.clone(),
                        device_id: settings.device_id.clone(),
                        display_name: if nickname.trim().is_empty() {
                            branding::DEFAULT_PARTY_NICKNAME.to_owned()
                        } else {
                            nickname.trim().to_owned()
                        },
                        avatar_url: None,
                        max_members: None,
                        autoplay_enabled: None,
                    };
                    let http_c = http.clone();
                    let rt_c = rt.clone();
                    let shared_c = Arc::clone(&party_state_shared);
                    let events_c = events.clone();
                    let itx = internal_tx.clone();
                    let pgen_ref = Arc::clone(&party_gen);
                    rt.spawn(async move {
                        match ListenTogetherClient::new(http_c, &server_url) {
                            Ok(client) => match client.join_party(&code, &req).await {
                                Ok(membership) => {
                                    if pgen_ref.load(Ordering::Acquire) != pgen {
                                        return;
                                    }
                                    let joined_code = membership.party.code.clone();
                                    let runtime = spawn_party_ws_loop(
                                        &rt_c,
                                        client,
                                        joined_code.clone(),
                                        membership.token.clone(),
                                        shared_c,
                                        events_c,
                                    );
                                    let _ = itx
                                        .send(InternalMsg::PartyConnected {
                                            generation: pgen,
                                            runtime,
                                            message: format!(
                                                "Joined Listen Together party {joined_code}"
                                            ),
                                        })
                                        .await;
                                }
                                Err(err) => {
                                    events_c.emit(BackendEvent::ErrorNotice(err.to_string()));
                                }
                            },
                            Err(err) => {
                                events_c.emit(BackendEvent::ErrorNotice(err.to_string()));
                            }
                        }
                    });
                }
                BackendCommand::PartyLeave => {
                    party_gen.fetch_add(1, Ordering::AcqRel);
                    if let Some(pr) = party_runtime.take() {
                        pr.stop_flag.store(true, Ordering::Release);
                        rt.spawn(async move {
                            let _ = pr.client.leave_party(&pr.code, &pr.token).await;
                        });
                    }
                    let cleared = ListenTogetherState::default();
                    *party_state_shared.lock().unwrap_or_else(|e| e.into_inner()) = cleared.clone();
                    events.emit(BackendEvent::PartyUpdated(Box::new(cleared)));
                    events.emit(BackendEvent::StatusNotice(
                        "Left Listen Together party".to_owned(),
                    ));
                }
            }
        }

        // 5. Advance audio engine clock (crossfade, gapless, sleep timer, ALSA fallback).
        let elapsed_ms = last_tick.elapsed().as_millis().min(250) as u64;
        last_tick = Instant::now();
        engine.tick(elapsed_ms.max(1));

        let mut snap = engine.snapshot();

        // 5a. Surface output backend disconnect / ALSA->PipeWire fallback notices once.
        if snap.output_error != last_output_error {
            last_output_error = snap.output_error.clone();
            if let Some(ref msg) = snap.output_error {
                events.emit(BackendEvent::StatusNotice(msg.clone()));
            }
        }

        // 5b. Auto-recover PipeWire/TWS disconnect or audio output fault (rate-limited).
        if engine.needs_output_reconnect()
            && last_reconnect_attempt.is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            last_reconnect_attempt = Some(Instant::now());
            let was_playing = matches!(
                loop_state.last_state,
                PlaybackState::Playing | PlaybackState::Buffering
            );
            let resume_pos = engine
                .fallback_resume_state()
                .map_or(snap.position_ms, |fb| fb.position_ms);
            let active_song = engine
                .fallback_resume_state()
                .map(|fb| fb.song.clone())
                .or_else(|| snap.current_song.clone())
                .or_else(|| current_playing_song.clone());
            let was_alsa = matches!(engine.output_mode(), AudioOutputMode::AlsaExclusive { .. });

            match engine.recover_output_backend() {
                Ok(true) => {
                    let notice = if was_alsa {
                        "ALSA device disconnected; switched to PipeWire Auto".to_owned()
                    } else {
                        "Audio output reconnected".to_owned()
                    };
                    events.emit(BackendEvent::StatusNotice(notice));
                    snap = engine.snapshot();
                    loop_state.last_state = snap.state;
                    loop_state.last_published_pos_ms = snap.position_ms;
                    events.emit(BackendEvent::PlaybackUpdated(Box::new(snap.clone())));
                    if was_playing {
                        let resume_target = engine
                            .take_fallback_resume_state()
                            .map(|fb| (fb.song, fb.position_ms))
                            .or_else(|| active_song.map(|s| (s, resume_pos)));
                        if let Some((song, pos_ms)) = resume_target {
                            start_async_song_load_at(
                                &mut engine,
                                &mut loop_state,
                                &mut current_playing_song,
                                &load_gen,
                                &preload_gen,
                                &mut active_load_cancel,
                                &mut preload_cancel,
                                &mut preloaded_next_song,
                                &rt,
                                &http_arc,
                                &resolver,
                                &remote_auth,
                                &db_worker,
                                &current_lyrics,
                                song,
                                pos_ms,
                                false,
                                &internal_tx,
                                &events,
                            );
                            snap = engine.snapshot();
                        }
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    events.emit(BackendEvent::ErrorNotice(format!(
                        "Audio output recovery failed: {err}"
                    )));
                }
            }
        }

        // 5c. Auto-retry mid-stream network errors from the last known position (max 3x).
        if snap.state == PlaybackState::Error
            && loop_state.last_state != PlaybackState::Error
            && let Some(ref song) = current_playing_song
            && is_retryable_remote_song(song)
        {
            match error_retry.on_engine_error(
                &song.video_id,
                snap.position_ms,
                snap.last_error.as_deref(),
                Instant::now(),
            ) {
                RetryScheduleOutcome::Scheduled {
                    attempt_number,
                    max_attempts,
                } => {
                    events.emit(BackendEvent::StatusNotice(format!(
                        "Connection interrupted — retrying playback ({attempt_number}/{max_attempts})..."
                    )));
                }
                RetryScheduleOutcome::Exhausted => {
                    let detail = snap
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "Stream interrupted".to_owned());
                    events.emit(BackendEvent::ErrorNotice(format!(
                        "Playback interrupted for \"{}\": {detail}",
                        song.title
                    )));
                }
            }
        }

        if loop_state.in_flight.is_none()
            && matches!(snap.state, PlaybackState::Error | PlaybackState::Stopped)
            && let Some(plan) =
                error_retry.take_due_attempt(Instant::now(), snap.last_error.as_deref())
            && let Some(song) = current_playing_song.clone()
        {
            if plan.bypass_cache {
                resolver.invalidate_video(&song.video_id);
                db_worker.delete_stream_cache_for_video(song.video_id.clone());
            }
            start_async_song_load_at(
                &mut engine,
                &mut loop_state,
                &mut current_playing_song,
                &load_gen,
                &preload_gen,
                &mut active_load_cancel,
                &mut preload_cancel,
                &mut preloaded_next_song,
                &rt,
                &http_arc,
                &resolver,
                &remote_auth,
                &db_worker,
                &current_lyrics,
                song,
                plan.resume_ms,
                plan.bypass_cache,
                &internal_tx,
                &events,
            );
            snap = engine.snapshot();
        } else if snap.state == PlaybackState::Playing
            && (error_retry.attempts > 0 || error_retry.active || error_retry.exhausted)
            && snap.position_ms >= error_retry.resume_ms.saturating_add(1_500)
        {
            error_retry.reset();
        }

        // 5d. Detect gapless/crossfade automatic deck handoff to the preloaded standby song!
        if snap.state == PlaybackState::Playing
            && let Some(ref active_song) = snap.current_song
            && loop_state.last_song_vid.as_deref() != Some(active_song.video_id.as_str())
        {
            loop_state.last_song_vid = Some(active_song.video_id.clone());
            current_playing_song = Some(active_song.clone());
            preloaded_next_song = None;
            error_retry.reset();

            if queue.advance_to_matching_next(&active_song.video_id) {
                emit_queue(&queue, &events);
            }
            if snap.duration_ms > 0 {
                let dur_text = format_duration_ms(snap.duration_ms);
                if queue.update_duration_text(&active_song.video_id, &dur_text) {
                    emit_queue(&queue, &events);
                }
                db_worker.update_song_duration(
                    active_song.video_id.clone(),
                    snap.duration_ms,
                    Some(dur_text),
                );
            }
            record_track_started_async(
                &rt,
                &db_worker,
                active_song,
                (snap.duration_ms > 0).then_some(snap.duration_ms),
                settings.discord_rpc_enabled,
                &snap,
                &internal_tx,
                &events,
            );
            let cur_gen = load_gen.load(Ordering::Acquire);
            lyrics_sync_state = LyricsSyncState::Loading;
            spawn_lyrics_lookup(
                cur_gen,
                Arc::clone(&load_gen),
                &rt,
                &db_worker,
                &lyrics_repo,
                &current_lyrics,
                active_song.clone(),
                snap.duration_ms,
                &internal_tx,
                &events,
            );
            spawn_standby_preload_for_queue(
                cur_gen,
                &load_gen,
                &preload_gen,
                &mut preload_cancel,
                &queue,
                &rt,
                &http_arc,
                &resolver,
                &remote_auth,
                &db_worker,
                &internal_tx,
            );
        }

        // 5e. Auto-advance queue when track cleanly finishes without a preloaded standby deck.
        if snap.state == PlaybackState::Ended
            && loop_state.last_state == PlaybackState::Playing
            && let Some(next_idx) = queue.next_index(true)
            && let Some(song) = queue.jump(next_idx)
        {
            error_retry.reset();
            emit_queue(&queue, &events);
            lyrics_sync_state = LyricsSyncState::Loading;
            start_async_song_load_at(
                &mut engine,
                &mut loop_state,
                &mut current_playing_song,
                &load_gen,
                &preload_gen,
                &mut active_load_cancel,
                &mut preload_cancel,
                &mut preloaded_next_song,
                &rt,
                &http_arc,
                &resolver,
                &remote_auth,
                &db_worker,
                &current_lyrics,
                song,
                0,
                false,
                &internal_tx,
                &events,
            );
            snap = engine.snapshot();
        }

        if loop_state.should_publish_tick_snapshot(&snap, &error_retry) {
            events.emit(BackendEvent::PlaybackUpdated(Box::new(snap.clone())));
            // Updates MPRIS cached position synchronously; only emits D-Bus PropertiesChanged
            // when playback status, track/metadata, rate, volume, or queue flags actually change.
            sync_mpris_if_changed(&rt, mpris_svc.as_ref(), &mut last_mpris_key, &snap, &queue);
        }
    }
}

fn is_retryable_remote_song(song: &Song) -> bool {
    song.local_path.is_none() && !Path::new(&song.video_id).exists()
}

fn cancel_preload_only(
    preload_gen: &Arc<AtomicU64>,
    preload_cancel: &mut Option<StreamCancelToken>,
) -> u64 {
    let next = preload_gen.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(tok) = preload_cancel.take() {
        tok.cancel();
    }
    next
}

fn cancel_all_loads(
    load_gen: &Arc<AtomicU64>,
    preload_gen: &Arc<AtomicU64>,
    active_load_cancel: &mut Option<StreamCancelToken>,
    preload_cancel: &mut Option<StreamCancelToken>,
) -> u64 {
    let next = load_gen.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(tok) = active_load_cancel.take() {
        tok.cancel();
    }
    let _ = cancel_preload_only(preload_gen, preload_cancel);
    next
}

#[allow(clippy::too_many_arguments)]
fn invalidate_and_repreload(
    engine: &mut AudioEngine,
    queue: &QueueController,
    load_gen: &Arc<AtomicU64>,
    preload_gen: &Arc<AtomicU64>,
    preload_cancel: &mut Option<StreamCancelToken>,
    preloaded_next_song: &mut Option<Song>,
    rt: &Handle,
    http: &Arc<Client>,
    resolver: &Arc<StreamResolver>,
    remote_auth: &Arc<RwLock<RemoteArtworkAuth>>,
    db_worker: &DbWorker,
    internal_tx: &Sender<InternalMsg>,
) {
    let _ = cancel_preload_only(preload_gen, preload_cancel);
    engine.clear_standby();
    *preloaded_next_song = None;

    if matches!(
        engine.snapshot().state,
        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
    ) {
        let cur_gen = load_gen.load(Ordering::Acquire);
        spawn_standby_preload_for_queue(
            cur_gen,
            load_gen,
            preload_gen,
            preload_cancel,
            queue,
            rt,
            http,
            resolver,
            remote_auth,
            db_worker,
            internal_tx,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_standby_preload_for_queue(
    generation: u64,
    load_gen: &Arc<AtomicU64>,
    preload_gen: &Arc<AtomicU64>,
    preload_cancel: &mut Option<StreamCancelToken>,
    queue: &QueueController,
    rt: &Handle,
    http: &Arc<Client>,
    resolver: &Arc<StreamResolver>,
    remote_auth: &Arc<RwLock<RemoteArtworkAuth>>,
    db_worker: &DbWorker,
    internal_tx: &Sender<InternalMsg>,
) {
    let Some(next_song) = queue.preload_candidate() else {
        return;
    };
    let pgen = cancel_preload_only(preload_gen, preload_cancel);
    let cancel_tok = StreamCancelToken::new();
    *preload_cancel = Some(cancel_tok.clone());

    spawn_preload_next(
        generation,
        pgen,
        Arc::clone(load_gen),
        Arc::clone(preload_gen),
        cancel_tok,
        rt,
        Arc::clone(http),
        Arc::clone(resolver),
        Arc::clone(remote_auth),
        db_worker.clone(),
        next_song,
        internal_tx.clone(),
    );
}

fn sync_mpris_if_changed(
    rt: &Handle,
    mpris_svc: Option<&Arc<MprisService>>,
    last_key: &mut Option<MprisPublishKey>,
    snap: &PlaybackSnapshot,
    queue: &QueueController,
) {
    let Some(svc) = mpris_svc else {
        return;
    };
    // Always keep cached D-Bus position up to date without emitting PropertiesChanged.
    svc.set_position_ms(snap.position_ms);

    let key = MprisPublishKey::from_state(snap, queue);
    if last_key.as_ref() == Some(&key) {
        return;
    }
    *last_key = Some(key);

    let loop_status = if queue.repeat_one() {
        LoopStatus::Track
    } else if queue.repeat_all() {
        LoopStatus::Playlist
    } else {
        LoopStatus::None
    };
    let mut ms = MprisState {
        playback_status: match snap.state {
            PlaybackState::Playing | PlaybackState::Buffering => PlaybackStatus::Playing,
            PlaybackState::Paused => PlaybackStatus::Paused,
            PlaybackState::Stopped | PlaybackState::Ended | PlaybackState::Error => {
                PlaybackStatus::Stopped
            }
        },
        loop_status,
        shuffle: queue.shuffle(),
        rate: f64::from(snap.speed),
        volume: f64::from(snap.volume),
        position_us: (snap.position_ms as i64).saturating_mul(1_000),
        can_seek: snap.duration_ms > 0,
        can_go_next: queue.next_index(false).is_some(),
        can_go_previous: !queue.is_empty(),
        ..MprisState::default()
    };
    if let Some(ref song) = snap.current_song {
        ms.metadata = build_mpris_metadata(song, snap.duration_ms, None);
    }

    let svc_c = Arc::clone(svc);
    rt.spawn(async move {
        let _ = svc_c.publish_state(ms).await;
    });
}

fn notify_mpris_seeked(rt: &Handle, mpris_svc: Option<&Arc<MprisService>>, position_ms: u64) {
    if let Some(svc) = mpris_svc {
        let s = Arc::clone(svc);
        rt.spawn(async move {
            let _ = s.notify_seeked(position_ms).await;
        });
    }
}

fn spawn_load_home(
    rt: &Handle,
    yt_repo: &Arc<YtMusicRepository>,
    internal_tx: &Sender<InternalMsg>,
    events: &EventHub,
) {
    events.emit(BackendEvent::HomeLoading);
    let repo = Arc::clone(yt_repo);
    let itx = internal_tx.clone();
    let events_c = events.clone();
    rt.spawn(async move {
        let res = repo.home().await.map_err(|e| e.to_string());
        let cached = res.as_ref().ok().cloned();
        let _ = itx.send(InternalMsg::HomeFetchFinished(cached)).await;
        events_c.emit(BackendEvent::HomeLoaded(res));
    });
}

fn spawn_library_refresh(
    rt: &Handle,
    db_worker: &DbWorker,
    internal_tx: &Sender<InternalMsg>,
    events: &EventHub,
) {
    let db_c = db_worker.clone();
    let itx = internal_tx.clone();
    let events_c = events.clone();
    rt.spawn(async move {
        let snap = db_c.fetch_library_snapshot().await;
        let _ = itx.send(InternalMsg::LibraryCached(snap.clone())).await;
        events_c.emit(BackendEvent::LibraryLoaded(snap));
    });
}

fn handle_async_load_failure(
    engine: &AudioEngine,
    loop_state: &mut PlaybackLoopState,
    error_retry: &mut PlaybackErrorRetry,
    song: &Song,
    error: String,
    now: Instant,
    events: &EventHub,
) {
    match error_retry.on_async_load_failed(&song.video_id, &error, now) {
        Some(RetryScheduleOutcome::Scheduled {
            attempt_number,
            max_attempts,
        }) => {
            loop_state.last_state = PlaybackState::Buffering;
            loop_state.last_published_pos_ms = error_retry.resume_ms;
            let mut retrying_snap = engine.snapshot();
            retrying_snap.state = PlaybackState::Buffering;
            retrying_snap.current_song = Some(song.clone());
            retrying_snap.position_ms = error_retry.resume_ms;
            retrying_snap.duration_ms = duration_millis(song.duration_text.as_deref());
            retrying_snap.last_error = Some(error);
            events.emit(BackendEvent::StatusNotice(format!(
                "Connection interrupted — retrying playback ({attempt_number}/{max_attempts})..."
            )));
            events.emit(BackendEvent::PlaybackUpdated(Box::new(retrying_snap)));
        }
        Some(RetryScheduleOutcome::Exhausted) => {
            loop_state.last_state = PlaybackState::Error;
            loop_state.last_published_pos_ms = error_retry.resume_ms;
            let mut err_snap = engine.snapshot();
            err_snap.state = PlaybackState::Error;
            err_snap.current_song = Some(song.clone());
            err_snap.position_ms = error_retry.resume_ms;
            err_snap.duration_ms = duration_millis(song.duration_text.as_deref());
            err_snap.last_error = Some(error.clone());
            events.emit(BackendEvent::ErrorNotice(format!(
                "Playback failed for \"{}\": {error}",
                song.title
            )));
            events.emit(BackendEvent::PlaybackUpdated(Box::new(err_snap)));
        }
        None => {
            let snap = engine.snapshot();
            loop_state.last_state = snap.state;
            loop_state.last_published_pos_ms = snap.position_ms;
            events.emit(BackendEvent::ErrorNotice(format!(
                "Playback failed for \"{}\": {error}",
                song.title
            )));
            events.emit(BackendEvent::PlaybackUpdated(Box::new(snap)));
        }
    }
}

/// Immediately updates UI to `Buffering` for `song` and resolves + opens the audio decoder
/// asynchronously off the backend control loop (cancelling any prior stalled HTTP fetch).
#[allow(clippy::too_many_arguments)]
fn start_async_song_load_at(
    engine: &mut AudioEngine,
    loop_state: &mut PlaybackLoopState,
    current_playing_song: &mut Option<Song>,
    load_gen: &Arc<AtomicU64>,
    preload_gen: &Arc<AtomicU64>,
    active_load_cancel: &mut Option<StreamCancelToken>,
    preload_cancel: &mut Option<StreamCancelToken>,
    preloaded_next_song: &mut Option<Song>,
    rt: &Handle,
    http: &Arc<Client>,
    resolver: &Arc<StreamResolver>,
    remote_auth: &Arc<RwLock<RemoteArtworkAuth>>,
    db_worker: &DbWorker,
    current_lyrics: &Arc<Mutex<Option<LyricsPayload>>>,
    song: Song,
    resume_position_ms: u64,
    bypass_stream_cache: bool,
    internal_tx: &Sender<InternalMsg>,
    events: &EventHub,
) {
    let generation = cancel_all_loads(load_gen, preload_gen, active_load_cancel, preload_cancel);
    *preloaded_next_song = None;
    *current_playing_song = Some(song.clone());
    engine.stop();
    loop_state.begin_async_load(generation, song.clone(), resume_position_ms);

    let cancel_tok = StreamCancelToken::new();
    *active_load_cancel = Some(cancel_tok.clone());

    // Clear stale lyrics immediately so the previous track's lines never linger while loading.
    *current_lyrics.lock().unwrap_or_else(|e| e.into_inner()) = None;
    events.emit(BackendEvent::LyricsLoading);

    // Publish an instant Buffering snapshot so the bottom bar & Now Playing respond in <1 ms.
    let mut buffering_snap = engine.snapshot();
    buffering_snap.state = PlaybackState::Buffering;
    buffering_snap.duration_ms = duration_millis(song.duration_text.as_deref());
    buffering_snap.position_ms = resume_position_ms;
    buffering_snap.current_song = Some(song.clone());
    events.emit(BackendEvent::PlaybackUpdated(Box::new(buffering_snap)));
    events.emit(BackendEvent::StatusNotice(format!(
        "Loading {} - {}",
        song.title, song.artist
    )));

    let gen_ref = Arc::clone(load_gen);
    let http_c = Arc::clone(http);
    let resolver_c = Arc::clone(resolver);
    let remote_c = Arc::clone(remote_auth);
    let db_c = db_worker.clone();
    let rt_c = rt.clone();
    let tx = internal_tx.clone();
    let song_for_err = song.clone();

    rt.spawn(async move {
        let prepared = prepare_track_decoder(
            &http_c,
            &rt_c,
            &resolver_c,
            &remote_c,
            &db_c,
            song,
            &gen_ref,
            generation,
            cancel_tok,
            bypass_stream_cache,
        )
        .await;
        if gen_ref.load(Ordering::Acquire) != generation {
            return;
        }
        match prepared {
            Ok((enriched_song, decoder, prebuffered, loudness_db, source_label, resolved_dur)) => {
                let _ = tx
                    .send(InternalMsg::Ready {
                        generation,
                        song: enriched_song,
                        decoder: Box::new(decoder),
                        prebuffered,
                        loudness_db,
                        source_label,
                        resolved_duration_ms: resolved_dur,
                        resume_position_ms,
                    })
                    .await;
            }
            Err((_song_title, error)) => {
                let _ = tx
                    .send(InternalMsg::Failed {
                        generation,
                        song: song_for_err,
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
    preload_generation: u64,
    load_gen: Arc<AtomicU64>,
    preload_gen: Arc<AtomicU64>,
    cancel_tok: StreamCancelToken,
    rt: &Handle,
    http: Arc<Client>,
    resolver: Arc<StreamResolver>,
    remote_auth: Arc<RwLock<RemoteArtworkAuth>>,
    db_worker: DbWorker,
    next_song: Song,
    internal_tx: Sender<InternalMsg>,
) {
    let rt_c = rt.clone();
    rt.spawn(async move {
        // Brief courtesy delay so the active track's opening chunks keep full bandwidth priority.
        tokio::time::sleep(Duration::from_millis(900)).await;
        if cancel_tok.is_cancelled()
            || load_gen.load(Ordering::Acquire) != generation
            || preload_gen.load(Ordering::Acquire) != preload_generation
        {
            return;
        }
        if let Ok((enriched, decoder, prebuffered, loudness_db, source_label, resolved_dur)) =
            prepare_track_decoder(
                &http,
                &rt_c,
                &resolver,
                &remote_auth,
                &db_worker,
                next_song,
                &load_gen,
                generation,
                cancel_tok.clone(),
                false,
            )
            .await
            && !cancel_tok.is_cancelled()
            && load_gen.load(Ordering::Acquire) == generation
            && preload_gen.load(Ordering::Acquire) == preload_generation
        {
            let _ = internal_tx
                .send(InternalMsg::PreloadReady {
                    generation,
                    preload_generation,
                    song: enriched,
                    decoder: Box::new(decoder),
                    prebuffered,
                    loudness_db,
                    source_label,
                    resolved_duration_ms: resolved_dur,
                })
                .await;
        }
    });
}

/// Runs throttled (max 1 concurrent job) Automix musical analysis for the outgoing and
/// preloaded incoming tracks, consulting and populating SQLite's `track_analysis` cache
/// via [`DbWorker`], then plans a transition and sends it back to the dual-deck engine.
#[allow(clippy::too_many_arguments)]
fn spawn_automix_planning(
    generation: u64,
    preload_generation: u64,
    load_gen: Arc<AtomicU64>,
    preload_gen: Arc<AtomicU64>,
    rt: &Handle,
    sem: Arc<tokio::sync::Semaphore>,
    db_worker: DbWorker,
    onnx_paths: Arc<OnnxModelPaths>,
    mode: AutomixMode,
    phrase_bars: u32,
    crossfade_seconds: u32,
    outgoing: Song,
    incoming: Song,
    internal_tx: Sender<InternalMsg>,
) {
    rt.spawn(async move {
        let Ok(_permit) = sem.acquire().await else {
            return;
        };
        if load_gen.load(Ordering::Acquire) != generation
            || preload_gen.load(Ordering::Acquire) != preload_generation
        {
            return;
        }
        let res = tokio::task::spawn_blocking(move || {
            let out_a = load_or_analyze_song(&db_worker, &onnx_paths, &outgoing);
            let in_a = load_or_analyze_song(&db_worker, &onnx_paths, &incoming);
            plan_transition(mode, phrase_bars, crossfade_seconds, &out_a, &in_a)
        })
        .await;
        if load_gen.load(Ordering::Acquire) != generation
            || preload_gen.load(Ordering::Acquire) != preload_generation
        {
            return;
        }
        if let Ok(plan) = res
            && plan.is_active()
        {
            let _ = internal_tx
                .send(InternalMsg::AutomixPlanned {
                    generation,
                    preload_generation,
                    plan,
                })
                .await;
        }
    });
}

fn load_or_analyze_song(
    db_worker: &DbWorker,
    onnx_paths: &OnnxModelPaths,
    song: &Song,
) -> TrackAnalysis {
    if let Some(cached) = db_worker.get_track_analysis_blocking(song.video_id.clone())
        && cached.duration > 1.0
    {
        return cached;
    }

    let local_file: Option<PathBuf> = song
        .local_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| {
            db_worker
                .get_download_blocking(song.video_id.clone())
                .and_then(|meta| local_path_from_uri(&meta.uri))
                .filter(|p| p.is_file())
        });

    if let Some(path) = local_file
        && let Ok(analysis) = analyze_audio_file(&path, Some(onnx_paths))
    {
        db_worker.upsert_track_analysis(song.video_id.clone(), analysis.clone());
        return analysis;
    }

    let dur_sec = (duration_millis(song.duration_text.as_deref()) as f64 / 1_000.0).max(0.0);
    TrackAnalysis {
        key: -1,
        duration: dur_sec,
        audible_start_time: 0.0,
        content_end_time: dur_sec,
        ..TrackAnalysis::default()
    }
}

fn local_path_from_uri(uri: &str) -> Option<PathBuf> {
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path_str = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    Some(PathBuf::from(path_str))
}

fn parse_url_expire_ms(url: &str) -> Option<i64> {
    let (_, query) = url.split_once('?')?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == "expire"
            && let Ok(secs) = v.parse::<i64>()
            && secs > 0
        {
            return Some(secs.saturating_mul(1_000));
        }
    }
    None
}

fn resolved_to_cached_stream(cache_key: &str, stream: &ResolvedStream, now_ms: i64) -> CachedStream {
    const SAFETY_MARGIN_MS: i64 = 5 * 60 * 1_000;
    const MIN_CACHE_TTL_MS: i64 = 60 * 1_000;
    const MAX_CACHE_TTL_MS: i64 = 6 * 60 * 60 * 1_000;

    let expires_at = parse_url_expire_ms(&stream.url)
        .map(|exp| exp.saturating_sub(SAFETY_MARGIN_MS))
        .filter(|&exp| exp > now_ms.saturating_add(MIN_CACHE_TTL_MS))
        .map(|exp| exp.min(now_ms.saturating_add(MAX_CACHE_TTL_MS)))
        .unwrap_or_else(|| now_ms.saturating_add(DEFAULT_STREAM_CACHE_TTL_MS));

    CachedStream {
        cache_key: cache_key.to_owned(),
        video_id: stream.video_id.clone(),
        url: stream.url.clone(),
        mime_type: stream.mime_type.clone(),
        ext: stream.ext.clone(),
        kbps: stream.kbps,
        content_length: stream.content_length,
        duration_ms: stream.duration_ms,
        loudness_db: stream.loudness_db,
        is_live_hls: stream.is_live_hls,
        client_name: stream.client_name.clone(),
        profile_id: stream.profile_id.clone(),
        media_headers: stream.media_headers.clone(),
        range_chunk_bytes: stream.range_chunk_bytes,
        expires_at,
        updated_at: now_ms,
    }
}

fn cached_to_resolved_stream(cached: CachedStream) -> ResolvedStream {
    ResolvedStream {
        video_id: cached.video_id,
        url: cached.url,
        kbps: cached.kbps,
        mime_type: cached.mime_type,
        ext: cached.ext,
        content_length: cached.content_length,
        duration_ms: cached.duration_ms,
        loudness_db: cached.loudness_db,
        is_live_hls: cached.is_live_hls,
        client_name: cached.client_name,
        profile_id: cached.profile_id,
        media_headers: cached.media_headers,
        range_chunk_bytes: cached.range_chunk_bytes,
    }
}

type PreparedDecoderResult = (
    Song,
    AudioDecoder,
    Vec<DecodedPacket>,
    Option<f64>,
    String,
    Option<u64>,
);

#[allow(clippy::too_many_arguments)]
async fn prepare_track_decoder(
    http: &Arc<Client>,
    rt: &Handle,
    resolver: &Arc<StreamResolver>,
    remote_auth: &Arc<RwLock<RemoteArtworkAuth>>,
    db_worker: &DbWorker,
    mut song: Song,
    load_gen: &Arc<AtomicU64>,
    generation: u64,
    cancel_tok: StreamCancelToken,
    bypass_stream_cache: bool,
) -> Result<PreparedDecoderResult, (String, String)> {
    let title_for_err = song.title.clone();
    let map_err = |msg: String| (title_for_err.clone(), msg);

    if cancel_tok.is_cancelled() || load_gen.load(Ordering::Acquire) != generation {
        return Err(map_err("cancelled".to_owned()));
    }

    // 1. Direct local file path (instant, offline).
    if let Some(local_path) = song.local_path.as_deref().map(PathBuf::from)
        && local_path.is_file()
    {
        return open_local_prepared(song, local_path, "Local", load_gen, generation, cancel_tok)
            .await
            .map_err(map_err);
    }
    if Path::new(&song.video_id).is_file() {
        let p = PathBuf::from(&song.video_id);
        return open_local_prepared(song, p, "Local", load_gen, generation, cancel_tok)
            .await
            .map_err(map_err);
    }

    // 2. Check offline downloads table in SQLite before touching network!
    if let Some(dl) = db_worker.get_download(song.video_id.clone()).await {
        if song.duration_text.as_deref().is_none_or(str::is_empty) {
            song.duration_text = dl.duration_text.clone();
        }
        if song.thumbnail_url.as_deref().is_none_or(str::is_empty) {
            song.thumbnail_url = dl.thumbnail_url.clone();
        }
        if let Some(dl_path) = local_path_from_uri(&dl.uri)
            && dl_path.is_file()
        {
            return open_local_prepared(song, dl_path, "Offline", load_gen, generation, cancel_tok)
                .await
                .map_err(map_err);
        }
    }

    // 3. Enrich missing duration/thumbnail/local_path from cached `songs` metadata.
    if let Some(meta) = db_worker.get_song_metadata(song.video_id.clone()).await {
        if song.duration_text.as_deref().is_none_or(str::is_empty) {
            song.duration_text = meta.duration_text.clone();
        }
        if song.thumbnail_url.as_deref().is_none_or(str::is_empty) {
            song.thumbnail_url = meta.thumbnail_url.clone();
        }
        if song.album_name.is_none() {
            song.album_name = meta.album_name.clone();
        }
        if let Some(local_p) = meta.local_path
            && Path::new(&local_p).is_file()
        {
            song.local_path = Some(local_p.clone());
            return open_local_prepared(
                song,
                PathBuf::from(local_p),
                "Local",
                load_gen,
                generation,
                cancel_tok,
            )
            .await
            .map_err(map_err);
        }
    }

    // 4. WebDAV stream (authenticated cancelable HTTP Range via ChunkedRangeSource).
    if WebDavConfig::is_webdav_id(&song.video_id) {
        let file_url = WebDavConfig::file_url_of(&song.video_id)
            .or(song.local_uri.as_deref())
            .ok_or_else(|| map_err("WebDAV track missing stream URL".to_owned()))?
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
        let cancel_open = cancel_tok.clone();
        let cancel_prebuf = cancel_tok.clone();
        let (dec, prebuf) = tokio::task::spawn_blocking(move || {
            let mut dec = open_http_decoder_with_cancel(
                http_clone,
                rt_clone,
                &stream,
                None,
                cancel_open,
            )
            .map_err(|e| e.to_string())?;
            let prebuf = dec
                .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                    cancel_prebuf.is_cancelled() || gen_check.load(Ordering::Acquire) != generation
                })
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((dec, prebuf))
        })
        .await
        .map_err(|e| map_err(format!("WebDAV decode join error: {e}")))?
        .map_err(map_err)?;
        let dur = dec.info().duration_ms;
        return Ok((song, dec, prebuf, None, "WebDAV".to_owned(), dur));
    }

    // 5. SMB2/3 share stream (pure-Rust chunk-cached SmbRangeReader).
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
            .map_err(|e| map_err(e.to_string()))?;
        let byte_len = reader.byte_len();
        let ext = extension_lower(&uri);
        let gen_check = Arc::clone(load_gen);
        let cancel_prebuf = cancel_tok.clone();
        let (dec, prebuf) = tokio::task::spawn_blocking(move || {
            let mut dec = AudioDecoder::open_seekable_reader(reader, Some(byte_len), Some(&ext))
                .map_err(|e| e.to_string())?;
            let prebuf = dec
                .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                    cancel_prebuf.is_cancelled() || gen_check.load(Ordering::Acquire) != generation
                })
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((dec, prebuf))
        })
        .await
        .map_err(|e| map_err(format!("SMB decode join error: {e}")))?
        .map_err(map_err)?;
        let dur = dec.info().duration_ms;
        return Ok((song, dec, prebuf, None, "SMB".to_owned(), dur));
    }

    // 6. Remote YouTube Music stream: SQLite persistent stream_cache -> StreamResolver
    //    (with CipherSandbox & optimistic probing) -> cancelable ChunkedRangeSource.
    let mut last_err = String::new();
    for attempt in 0..2 {
        if cancel_tok.is_cancelled() || load_gen.load(Ordering::Acquire) != generation {
            return Err(map_err("cancelled".to_owned()));
        }
        let req = ResolveRequest::for_playback(&song.video_id, u32::MAX);
        let cache_key = StreamResolver::cache_key_for(&req);

        let mut seeded_from_db = false;
        if attempt == 0 {
            if bypass_stream_cache {
                // Evict both RAM (`StreamResolver::recent`) and SQLite stream caches so retries
                // truly fetch a fresh URL instead of reusing a stale in-memory entry.
                resolver.invalidate_video(&song.video_id);
                db_worker.delete_stream_cache_for_video(song.video_id.clone());
            } else if let Some(cached) = db_worker.get_stream_cache(cache_key.clone()).await {
                resolver.seed_cached_stream(&req, cached_to_resolved_stream(cached));
                seeded_from_db = true;
            }
        }

        let stream = resolver
            .resolve(req)
            .await
            .map_err(|e| map_err(e.to_string()))?;

        if cancel_tok.is_cancelled() || load_gen.load(Ordering::Acquire) != generation {
            return Err(map_err("cancelled".to_owned()));
        }

        if !seeded_from_db && !stream.is_live_hls {
            db_worker.put_stream_cache(resolved_to_cached_stream(
                &cache_key,
                &stream,
                unix_now_ms(),
            ));
        }

        let refused_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let refused_cb_flag = Arc::clone(&refused_flag);
        let resolver_cb = Arc::clone(resolver);
        let db_cb = db_worker.clone();
        let vid_cb = song.video_id.clone();
        let profile_cb = stream.profile_id.clone();
        let on_refused: RefusalCallback = Arc::new(move |url, _status| {
            refused_cb_flag.store(true, Ordering::Release);
            resolver_cb.on_playback_refused(&vid_cb, &profile_cb, url);
            db_cb.delete_stream_cache_for_video(vid_cb.clone());
        });

        let http_clone = (**http).clone();
        let rt_clone = rt.clone();
        let loudness = stream.loudness_db;
        let label = stream.client_name.clone();
        let stream_dur_ms = stream.duration_ms;
        let stream_clone = stream.clone();
        let gen_check = Arc::clone(load_gen);
        let cancel_open = cancel_tok.clone();
        let cancel_prebuf = cancel_tok.clone();

        let open_res = tokio::task::spawn_blocking(move || {
            let mut dec = open_http_decoder_with_cancel(
                http_clone,
                rt_clone,
                &stream_clone,
                Some(on_refused),
                cancel_open,
            )?;
            let prebuf = dec.prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                cancel_prebuf.is_cancelled() || gen_check.load(Ordering::Acquire) != generation
            })?;
            Ok::<_, bitchord_audio::EngineError>((dec, prebuf))
        })
        .await
        .map_err(|e| map_err(format!("decoder open join error: {e}")))?;

        match open_res {
            Ok((dec, prebuf)) => {
                let dur = dec.info().duration_ms.filter(|&d| d > 0).or(stream_dur_ms);
                return Ok((song, dec, prebuf, loudness, label, dur));
            }
            Err(err) => {
                last_err = err.to_string();
                if cancel_tok.is_cancelled() {
                    return Err(map_err("cancelled".to_owned()));
                }
                let was_refused = refused_flag.load(Ordering::Acquire)
                    || is_refusal_error(&last_err);
                if was_refused && attempt == 0 {
                    resolver.on_playback_refused(&song.video_id, &stream.profile_id, &stream.url);
                    db_worker.delete_stream_cache_for_video(song.video_id.clone());
                    continue;
                }
                return Err(map_err(last_err));
            }
        }
    }
    Err(map_err(last_err))
}

async fn open_local_prepared(
    song: Song,
    path: PathBuf,
    source_label: &str,
    load_gen: &Arc<AtomicU64>,
    generation: u64,
    cancel_tok: StreamCancelToken,
) -> Result<PreparedDecoderResult, String> {
    let gen_check = Arc::clone(load_gen);
    let (dec, prebuf) = tokio::task::spawn_blocking(move || {
        let mut dec = AudioDecoder::open_path(&path).map_err(|e| e.to_string())?;
        let prebuf = dec
            .prebuffer_ms(INITIAL_PREBUFFER_MS, || {
                cancel_tok.is_cancelled() || gen_check.load(Ordering::Acquire) != generation
            })
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((dec, prebuf))
    })
    .await
    .map_err(|e| format!("decode task join error: {e}"))??;
    let dur = dec.info().duration_ms;
    Ok((song, dec, prebuf, None, source_label.to_owned(), dur))
}

#[allow(clippy::too_many_arguments)]
fn record_track_started_async(
    rt: &Handle,
    db_worker: &DbWorker,
    song: &Song,
    duration_ms: Option<u64>,
    discord_enabled: bool,
    snap: &PlaybackSnapshot,
    internal_tx: &Sender<InternalMsg>,
    events: &EventHub,
) {
    let song_c = song.clone();
    let dur = duration_ms.unwrap_or_else(|| duration_millis(song.duration_text.as_deref())) as i64;
    let db_c = db_worker.clone();
    let itx = internal_tx.clone();
    let events_c = events.clone();

    rt.spawn(async move {
        let stats = db_c
            .record_track_play(song_c.clone(), unix_now_ms(), dur)
            .await;
        let _ = itx
            .send(InternalMsg::LibraryTrackUpdated {
                song: Box::new(song_c.clone()),
                stats,
            })
            .await;
        events_c.emit(BackendEvent::LibraryTrackRecorded {
            song: Box::new(song_c),
            stats,
        });
    });

    if discord_enabled {
        let quality_line = discord_audio_quality_line(snap.nerd_stats.as_ref());
        let state_text = match quality_line {
            Some(q) => format!("{} · {q}", song.artist),
            None => song.artist.clone(),
        };
        let mut activity = DiscordActivity::listening(song.title.clone(), state_text);
        activity.assets = Some(DiscordAssets {
            large_text: Some(branding::DISCORD_APP_LABEL.to_owned()),
            ..DiscordAssets::default()
        });
        rt.spawn(async move {
            if let Ok(mut client) = DiscordIpcClient::connect_default().await {
                let _ = client.set_activity(&activity).await;
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_lyrics_lookup(
    generation: u64,
    load_gen: Arc<AtomicU64>,
    rt: &Handle,
    db_worker: &DbWorker,
    lyrics_repo: &Arc<LyricsRepository>,
    current_lyrics: &Arc<Mutex<Option<LyricsPayload>>>,
    song: Song,
    duration_ms: u64,
    internal_tx: &Sender<InternalMsg>,
    events: &EventHub,
) {
    *current_lyrics.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let vid = song.video_id.clone();
    events.emit(BackendEvent::LyricsLoading);

    let repo = Arc::clone(lyrics_repo);
    let db_c = db_worker.clone();
    let events_c = events.clone();
    let lyrics_slot = Arc::clone(current_lyrics);
    let itx = internal_tx.clone();
    let dur_ms = duration_ms as i64;

    rt.spawn(async move {
        // 1. Check persistent SQLite lyrics cache first (instant hit, zero network).
        if let Some(cached) = db_c.get_lyrics_cache(vid.clone()).await {
            if load_gen.load(Ordering::Acquire) != generation {
                return;
            }
            if cached.not_found {
                let err_msg = "No synced or plain lyrics available for this track.".to_owned();
                let _ = itx
                    .send(InternalMsg::LyricsStateUpdated {
                        video_id: vid.clone(),
                        state: LyricsSyncState::Error(err_msg.clone()),
                    })
                    .await;
                events_c.emit(BackendEvent::LyricsLoaded {
                    video_id: vid,
                    result: Err(err_msg),
                });
                return;
            }
            if let Some(source) = cached.source {
                let payload = LyricsPayload {
                    video_id: vid.clone(),
                    source,
                    lines: cached.lines,
                    translated_lines: None,
                    translated_lang: None,
                };
                *lyrics_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload.clone());
                let _ = itx
                    .send(InternalMsg::LyricsStateUpdated {
                        video_id: vid.clone(),
                        state: LyricsSyncState::Ready(payload.clone()),
                    })
                    .await;
                events_c.emit(BackendEvent::LyricsLoaded {
                    video_id: vid,
                    result: Ok(payload),
                });
                return;
            }
        }

        // 2. Cache miss: brief delay so initial audio streaming stays uncontested.
        tokio::time::sleep(Duration::from_millis(LYRICS_REMOTE_DELAY_MS)).await;
        if load_gen.load(Ordering::Acquire) != generation {
            return;
        }

        let mut req = LyricsRequest::new(&song.video_id, &song.title, &song.artist, dur_ms);
        req.album = song.album_name.as_deref();
        req.prioritize_syllable_sync = true;
        let res = match repo.lyrics(&req, &LyricsCallbacks::default()).await {
            Some(found) => {
                db_c.put_lyrics_found(
                    vid.clone(),
                    found.source,
                    found.lines.clone(),
                    DEFAULT_LYRICS_CACHE_TTL_MS,
                );
                let payload = LyricsPayload {
                    video_id: vid.clone(),
                    source: found.source,
                    lines: found.lines,
                    translated_lines: None,
                    translated_lang: None,
                };
                Ok(payload)
            }
            None => {
                db_c.put_lyrics_not_found(vid.clone(), DEFAULT_LYRICS_NOT_FOUND_TTL_MS);
                Err("No synced or plain lyrics available for this track.".to_owned())
            }
        };

        if load_gen.load(Ordering::Acquire) != generation {
            return;
        }

        let sync_state = match &res {
            Ok(payload) => {
                *lyrics_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(payload.clone());
                LyricsSyncState::Ready(payload.clone())
            }
            Err(err) => LyricsSyncState::Error(err.clone()),
        };
        let _ = itx
            .send(InternalMsg::LyricsStateUpdated {
                video_id: vid.clone(),
                state: sync_state,
            })
            .await;
        events_c.emit(BackendEvent::LyricsLoaded {
            video_id: vid,
            result: res,
        });
    });
}

fn spawn_party_ws_loop(
    rt: &Handle,
    client: ListenTogetherClient,
    code: String,
    token: String,
    shared_state: Arc<Mutex<ListenTogetherState>>,
    events: EventHub,
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
        events.emit(BackendEvent::PartyUpdated(Box::new(session.state.clone())));

        match client_c.connect_ws(&code_c, &token_c).await {
            Ok(mut ws) => {
                while !stop_c.load(Ordering::Acquire) {
                    match tokio::time::timeout(Duration::from_millis(250), ws.next_frame()).await {
                        Ok(Ok(Some(frame))) => {
                            let effects = session.apply_server_frame(frame, unix_now_ms());
                            *shared_state.lock().unwrap_or_else(|e| e.into_inner()) =
                                session.state.clone();
                            events.emit(BackendEvent::PartyUpdated(Box::new(
                                session.state.clone(),
                            )));
                            if effects.leave_party {
                                break;
                            }
                        }
                        Ok(Ok(None)) => {
                            session.on_socket_dropped();
                            *shared_state.lock().unwrap_or_else(|e| e.into_inner()) =
                                session.state.clone();
                            events.emit(BackendEvent::PartyUpdated(Box::new(
                                session.state.clone(),
                            )));
                            break;
                        }
                        Ok(Err(err)) => {
                            session.state.error = Some(err.to_string());
                            session.state.connection = Connection::Offline;
                            *shared_state.lock().unwrap_or_else(|e| e.into_inner()) =
                                session.state.clone();
                            events.emit(BackendEvent::PartyUpdated(Box::new(
                                session.state.clone(),
                            )));
                            break;
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(err) => {
                session.state.error = Some(err.to_string());
                session.state.connection = Connection::Offline;
                *shared_state.lock().unwrap_or_else(|e| e.into_inner()) = session.state.clone();
                events.emit(BackendEvent::PartyUpdated(Box::new(session.state.clone())));
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

/// Scans a local folder on a blocking thread and collects tagged [`Song`] entries
/// ready for a single transactional `upsert_songs_batch` call.
fn scan_local_folder_songs(folder: &Path) -> Result<Vec<Song>, String> {
    if !folder.exists() || !folder.is_dir() {
        return Err(format!(
            "Folder does not exist or is not a directory: {}",
            folder.display()
        ));
    }
    let mut stack = vec![folder.to_path_buf()];
    let mut songs = Vec::new();

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
                    song.duration_text = Some(format_duration_ms(t.duration_ms));
                }
            }
            songs.push(song);
        }
    }
    Ok(songs)
}

fn build_query_history_record(
    query: &str,
    rows: &[SearchResult],
    timestamp: i64,
) -> SearchHistoryRecord {
    match rows.first() {
        Some(SearchResult::TopTrack { song }) | Some(SearchResult::Track { song }) => {
            let dur_ms = {
                let ms = song.duration_millis();
                (ms > 0).then_some(ms)
            };
            let dur_text = song
                .duration_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| dur_ms.map(format_duration_ms));
            SearchHistoryRecord {
                id: format!("q:{query}"),
                title: query.to_owned(),
                subtitle: format!("Search · Top: {} - {}", song.title, song.artist),
                artwork_url: song_thumbnail_url(song),
                entity_type: EntityType::Other,
                timestamp,
                album: song.album_name.clone(),
                duration_text: dur_text,
                duration_ms: dur_ms,
            }
        }
        Some(SearchResult::Browse { item }) => SearchHistoryRecord {
            id: format!("q:{query}"),
            title: query.to_owned(),
            subtitle: format!("Search · {}", item.title),
            artwork_url: item.thumbnail_url.clone(),
            entity_type: EntityType::Other,
            timestamp,
            album: None,
            duration_text: None,
            duration_ms: None,
        },
        None => SearchHistoryRecord {
            id: format!("q:{query}"),
            title: query.to_owned(),
            subtitle: "Search query".to_owned(),
            artwork_url: None,
            entity_type: EntityType::Other,
            timestamp,
            album: None,
            duration_text: None,
            duration_ms: None,
        },
    }
}

fn emit_queue(queue: &QueueController, events: &EventHub) {
    events.emit(BackendEvent::QueueUpdated(queue.snapshot()));
}

fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue_state::compute_next_index;

    fn sample_songs(count: usize) -> Vec<Song> {
        (0..count)
            .map(|i| Song::new(format!("vid_{i}"), format!("Track {i}"), "Artist", None))
            .collect()
    }

    fn test_source() -> QueueSource {
        QueueSource {
            title: "Test".to_owned(),
            r#type: PlaybackSourceType::Browse,
            id: None,
        }
    }

    #[test]
    fn queue_advancement_respects_repeat_one_repeat_all_and_bounds() {
        assert_eq!(compute_next_index(Some(0), 3, false, false, true), Some(1));
        assert_eq!(compute_next_index(Some(2), 3, false, false, true), None);
        assert_eq!(compute_next_index(Some(2), 3, true, false, true), Some(0));
        assert_eq!(compute_next_index(Some(1), 3, false, true, true), Some(1));
        assert_eq!(compute_next_index(Some(1), 3, false, true, false), Some(2));
        assert_eq!(compute_next_index(None, 0, true, true, true), None);
    }

    #[test]
    fn shuffle_and_unshuffle_preserves_original_order_and_active_track() {
        let mut q = QueueController::default();
        let songs = sample_songs(6);
        let original_ids: Vec<String> = songs.iter().map(|s| s.video_id.clone()).collect();
        let started = q.play_context(&songs, 2, &test_source(), 42);
        assert_eq!(started.as_ref().map(|s| s.video_id.as_str()), Some("vid_2"));
        assert_eq!(q.current_index(), Some(2));

        assert!(q.toggle_shuffle(999));
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(q.current_song().map(|s| s.video_id.as_str()), Some("vid_2"));

        // Move to a different track while shuffled, then unshuffle: active track stays anchored.
        let jumped = q.jump(3).expect("valid jump");
        let jumped_id = jumped.video_id.clone();

        assert!(!q.toggle_shuffle(999));
        let restored_ids: Vec<String> = q
            .snapshot()
            .items
            .iter()
            .map(|s| s.video_id.clone())
            .collect();
        assert_eq!(restored_ids, original_ids);
        assert_eq!(
            q.current_song().map(|s| s.video_id.as_str()),
            Some(jumped_id.as_str())
        );
    }

    #[test]
    fn queue_mutations_update_preload_candidate_and_are_bounds_safe() {
        let mut q = QueueController::default();
        let songs = sample_songs(3);
        q.play_context(&songs, 0, &test_source(), 1);
        assert_eq!(
            q.preload_candidate().map(|s| s.video_id),
            Some("vid_1".to_owned())
        );

        // PlayNext inserts right after active item -> standby candidate changes immediately.
        let inserted = Song::new("vid_next", "Up Next", "Artist", None);
        assert_eq!(q.play_next(inserted), PlayNextOutcome::InsertedNext);
        assert_eq!(
            q.preload_candidate().map(|s| s.video_id),
            Some("vid_next".to_owned())
        );

        // Removing the queued item shifts preload candidate back to vid_1 without panicking.
        assert_eq!(q.remove(1), QueueRemoveOutcome::Updated);
        assert_eq!(
            q.preload_candidate().map(|s| s.video_id),
            Some("vid_1".to_owned())
        );

        // Out-of-bounds removal and jump never panic.
        assert_eq!(q.remove(99), QueueRemoveOutcome::OutOfBounds);
        assert!(q.jump(99).is_none());

        // Repeat One suppresses standby preload of another song on natural EOF.
        q.cycle_repeat(); // repeat_all
        assert!(q.repeat_all());
        q.cycle_repeat(); // repeat_one
        assert!(q.repeat_one());
        assert!(q.preload_candidate().is_none());
    }

    #[test]
    fn gapless_advance_and_duration_enrichment_stay_in_sync() {
        let mut q = QueueController::default();
        let songs = sample_songs(3);
        q.play_context(&songs, 0, &test_source(), 7);

        assert!(!q.advance_to_matching_next("wrong_vid"));
        assert_eq!(q.current_index(), Some(0));

        assert!(q.advance_to_matching_next("vid_1"));
        assert_eq!(q.current_index(), Some(1));

        assert!(q.update_duration_text("vid_1", "1:02:05"));
        assert_eq!(
            q.current_song().and_then(|s| s.duration_text.as_deref()),
            Some("1:02:05")
        );
    }

    #[test]
    fn event_hub_evicts_oldest_when_full_without_blocking() {
        let (hub, rx) = EventHub::new(16);
        for i in 0..40 {
            hub.emit(BackendEvent::StatusNotice(format!("notice-{i}")));
        }
        let mut received = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            if let BackendEvent::StatusNotice(msg) = evt {
                received.push(msg);
            }
        }
        assert_eq!(received.len(), 16);
        assert_eq!(received.last().map(String::as_str), Some("notice-39"));
    }

    #[test]
    fn mpris_publish_key_ignores_position_ticks_and_detects_real_changes() {
        let mut q = QueueController::default();
        let songs = sample_songs(2);
        q.play_context(&songs, 0, &test_source(), 1);

        let engine = AudioEngine::with_null_sink();
        let mut snap_a = engine.snapshot();
        snap_a.state = PlaybackState::Playing;
        snap_a.current_song = Some(songs[0].clone());
        snap_a.duration_ms = 180_000;
        snap_a.position_ms = 1_000;

        let mut snap_b = snap_a.clone();
        snap_b.position_ms = 15_500; // Position tick only -> identical publish key!

        assert_eq!(
            MprisPublishKey::from_state(&snap_a, &q),
            MprisPublishKey::from_state(&snap_b, &q)
        );

        let mut snap_paused = snap_b.clone();
        snap_paused.state = PlaybackState::Paused;
        assert_ne!(
            MprisPublishKey::from_state(&snap_b, &q),
            MprisPublishKey::from_state(&snap_paused, &q)
        );
    }

    #[test]
    fn sync_full_state_restores_queue_playback_and_lyrics_snapshot() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");

        let engine = AudioEngine::with_null_sink();
        let mut q = QueueController::default();
        let songs = sample_songs(2);
        q.play_context(&songs, 0, &test_source(), 1);

        let lyrics_payload = LyricsPayload {
            video_id: "vid_0".to_owned(),
            source: LyricsSource::LrcLib,
            lines: vec![LyricLine {
                time_ms: 1_000,
                text: "hello world".to_owned(),
                words: Vec::new(),
                sung_until_ms: None,
                background: None,
                alignment: Default::default(),
                timing_source: None,
            }],
            translated_lines: None,
            translated_lang: None,
        };
        let mut snap = engine.snapshot();
        snap.state = PlaybackState::Playing;
        snap.current_song = Some(songs[0].clone());
        snap.position_ms = 42_000;
        snap.duration_ms = 210_000;

        let full = FullStateSnapshot {
            pipewire_active: true,
            audio_banner: None,
            audio_devices: Vec::new(),
            playback: snap,
            queue: q.snapshot(),
            lyrics: LyricsSyncState::Ready(lyrics_payload),
            home: None,
            library: Some(LibrarySnapshot::default()),
            party: ListenTogetherState::default(),
            eq_enabled: true,
            eq_preset: EqualizerPreset::BassBoost,
            eq_bands_db: [0.0; EqLayout::MANUAL_COUNT],
            stream_backend: StreamBackendChoice::Auto,
            audio_output_pref: AudioOutputPreference::PipeWireAuto,
            selected_alsa_device: "hw:0,0".to_owned(),
            automix_mode: AutomixMode::Off,
            automix_phrase_bars: 0,
            discord_rpc_enabled: false,
        };

        let (hub, rx) = EventHub::new(16);
        hub.emit(BackendEvent::FullStateSynced(Box::new(full)));

        rt.block_on(async {
            let evt = rx.recv().await.expect("receive SyncFullState event");
            match evt {
                BackendEvent::FullStateSynced(restored) => {
                    assert_eq!(restored.playback.state, PlaybackState::Playing);
                    assert_eq!(restored.playback.position_ms, 42_000);
                    assert_eq!(restored.queue.items.len(), 2);
                    assert_eq!(restored.queue.current_index, Some(0));
                    assert!(matches!(restored.lyrics, LyricsSyncState::Ready(_)));
                    assert!(restored.eq_enabled);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        });
    }

    #[test]
    fn resolved_stream_cache_conversion_respects_url_expire_and_fallback() {
        let now_ms = 1_700_000_000_000_i64;
        let expire_secs = (now_ms / 1_000) + 3_600;
        let stream = ResolvedStream {
            video_id: "abc12345678".to_owned(),
            url: format!("https://rr1.googlevideo.com/videoplayback?expire={expire_secs}&itag=251"),
            kbps: 160,
            mime_type: "audio/webm".to_owned(),
            ext: "webm".to_owned(),
            content_length: Some(4_000_000),
            duration_ms: Some(200_000),
            loudness_db: Some(-12.5),
            is_live_hls: false,
            client_name: "VISIONOS".to_owned(),
            profile_id: "native:VISIONOS".to_owned(),
            media_headers: Default::default(),
            range_chunk_bytes: Some(131_072),
        };
        let cached = resolved_to_cached_stream("abc12345678:max:Any", &stream, now_ms);
        assert_eq!(cached.expires_at, expire_secs * 1_000 - 300_000);

        let no_expire = ResolvedStream {
            url: "https://rr1.googlevideo.com/videoplayback?itag=251".to_owned(),
            ..stream
        };
        let fallback = resolved_to_cached_stream("abc12345678:max:Any", &no_expire, now_ms);
        assert_eq!(fallback.expires_at, now_ms + DEFAULT_STREAM_CACHE_TTL_MS);
    }

    #[test]
    fn db_worker_round_trips_caches_batch_upsert_and_shutdown() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");

        let tmp_dir = std::env::temp_dir().join(format!(
            "bitchord_db_worker_test_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        let db_path = tmp_dir.join("library.db");
        let worker = DbWorker::start(db_path);

        rt.block_on(async {
            let count = worker
                .upsert_songs_batch(sample_songs(4))
                .await
                .expect("batch upsert");
            assert_eq!(count, 4);

            worker.update_song_duration("vid_0".to_owned(), 3_723_000, None);
            let meta = worker
                .get_song_metadata("vid_0".to_owned())
                .await
                .expect("metadata hit");
            assert_eq!(meta.duration_ms, Some(3_723_000));
            assert_eq!(meta.duration_text.as_deref(), Some("1:02:03"));

            worker.put_lyrics_not_found("vid_1".to_owned(), DEFAULT_LYRICS_NOT_FOUND_TTL_MS);
            let lyr = worker
                .get_lyrics_cache("vid_1".to_owned())
                .await
                .expect("negative lyrics cache hit");
            assert!(lyr.not_found);
        });

        worker.shutdown_blocking();
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn non_standby_song_switch_does_not_overwrite_buffering_with_stopped() {
        let mut engine = AudioEngine::with_null_sink();
        let retry = PlaybackErrorRetry::default();
        let songs = sample_songs(2);
        let mut loop_state = PlaybackLoopState {
            last_state: PlaybackState::Playing,
            last_song_vid: Some(songs[0].video_id.clone()),
            last_published_pos_ms: 52_000,
            in_flight: None,
        };

        // Simulate `start_async_song_load_at` stopping the old track and emitting Buffering
        // for track B while the decoder opens off-thread.
        engine.stop();
        loop_state.begin_async_load(1, songs[1].clone(), 0);
        let stopped_snap = engine.snapshot();
        assert_eq!(stopped_snap.state, PlaybackState::Stopped);
        assert!(stopped_snap.current_song.is_none());

        // End-of-iteration step 5 must NOT overwrite Buffering with Stopped/None.
        assert!(!loop_state.should_publish_tick_snapshot(&stopped_snap, &retry));
        assert_eq!(loop_state.last_state, PlaybackState::Buffering);

        // Reopening from tray (`SyncFullState`) during in-flight load also sees Buffering + song B.
        let projected = loop_state.effective_snapshot(stopped_snap.clone(), &retry, Some(&songs[1]));
        assert_eq!(projected.state, PlaybackState::Buffering);
        assert_eq!(
            projected.current_song.as_ref().map(|s| s.video_id.as_str()),
            Some("vid_1")
        );

        // Also verify resuming mid-track preserves `last_published_pos_ms` instead of zeroing it.
        loop_state.begin_async_load(2, songs[0].clone(), 45_000);
        assert!(!loop_state.should_publish_tick_snapshot(&stopped_snap, &retry));
        assert_eq!(loop_state.last_published_pos_ms, 45_000);

        // Once the in-flight load is explicitly cancelled/cleared (e.g. Stop), Stopped publishes cleanly.
        loop_state.clear_in_flight();
        assert!(loop_state.should_publish_tick_snapshot(&stopped_snap, &retry));
        assert_eq!(loop_state.last_state, PlaybackState::Stopped);
    }

    #[test]
    fn auto_retry_runs_all_three_attempts_and_preserves_resume_ms_for_manual_play() {
        let engine = AudioEngine::with_null_sink();
        let stopped_snap = engine.snapshot();
        let song = Song::new("vid_net", "Network Track", "Artist", None);
        let (hub, rx) = EventHub::new(32);

        let mut loop_state = PlaybackLoopState {
            last_state: PlaybackState::Playing,
            last_song_vid: Some(song.video_id.clone()),
            last_published_pos_ms: 45_000,
            in_flight: None,
        };
        let mut retry = PlaybackErrorRetry::default();
        let t0 = Instant::now();

        // Engine hits mid-stream network error at 45s -> schedules retry 1/3 after 800ms.
        assert_eq!(
            retry.on_engine_error(&song.video_id, 45_000, Some("HTTP range error"), t0),
            RetryScheduleOutcome::Scheduled {
                attempt_number: 1,
                max_attempts: 3,
            }
        );
        assert!(retry.take_due_attempt(t0 + Duration::from_millis(500), None).is_none());

        // Attempt 1 fires at +800ms (non-403 attempt 1 keeps cache; engine.stop() moves to Stopped).
        let plan1 = retry
            .take_due_attempt(t0 + Duration::from_millis(800), Some("HTTP range error"))
            .expect("attempt 1 due");
        assert_eq!(
            plan1,
            RetryAttemptPlan {
                resume_ms: 45_000,
                bypass_cache: false,
            }
        );
        loop_state.begin_async_load(1, song.clone(), plan1.resume_ms);
        assert!(!loop_state.should_publish_tick_snapshot(&stopped_snap, &retry));

        // Attempt 1 fails while Wi-Fi is still down -> schedules retry 2/3 after 2s without resetting pos.
        loop_state.finish_in_flight_if_matches(1);
        let t1 = t0 + Duration::from_millis(1_000);
        handle_async_load_failure(
            &engine,
            &mut loop_state,
            &mut retry,
            &song,
            "network unreachable".to_owned(),
            t1,
            &hub,
        );
        assert!(retry.is_waiting_for_retry());
        assert_eq!(loop_state.last_published_pos_ms, 45_000);
        assert!(!loop_state.should_publish_tick_snapshot(&stopped_snap, &retry));

        // Attempt 2 fires at t1 + 2s EVEN THOUGH engine is in Stopped state, and forces cache bypass.
        assert!(retry.take_due_attempt(t1 + Duration::from_millis(1_500), None).is_none());
        let plan2 = retry
            .take_due_attempt(t1 + Duration::from_millis(2_000), None)
            .expect("attempt 2 due");
        assert_eq!(
            plan2,
            RetryAttemptPlan {
                resume_ms: 45_000,
                bypass_cache: true,
            }
        );
        loop_state.begin_async_load(2, song.clone(), plan2.resume_ms);

        // Attempt 2 fails -> schedules retry 3/3 after 4s.
        loop_state.finish_in_flight_if_matches(2);
        let t2 = t1 + Duration::from_millis(2_200);
        handle_async_load_failure(
            &engine,
            &mut loop_state,
            &mut retry,
            &song,
            "network unreachable".to_owned(),
            t2,
            &hub,
        );
        assert!(retry.is_waiting_for_retry());
        assert_eq!(loop_state.last_published_pos_ms, 45_000);

        // Attempt 3 fires at t2 + 4s with bypass_cache = true.
        let plan3 = retry
            .take_due_attempt(t2 + Duration::from_millis(4_000), None)
            .expect("attempt 3 due");
        assert_eq!(
            plan3,
            RetryAttemptPlan {
                resume_ms: 45_000,
                bypass_cache: true,
            }
        );
        loop_state.begin_async_load(3, song.clone(), plan3.resume_ms);

        // Attempt 3 fails -> retry cycle exhausted, Error snapshot emitted with 45s position preserved.
        loop_state.finish_in_flight_if_matches(3);
        let t3 = t2 + Duration::from_millis(4_200);
        handle_async_load_failure(
            &engine,
            &mut loop_state,
            &mut retry,
            &song,
            "network unreachable".to_owned(),
            t3,
            &hub,
        );
        assert!(retry.is_exhausted());
        assert!(!retry.is_waiting_for_retry());
        assert_eq!(loop_state.last_published_pos_ms, 45_000);
        assert!(!loop_state.should_publish_tick_snapshot(&stopped_snap, &retry));

        // Verify SyncFullState reflects Error at 45s instead of Stopped at 0.
        let synced = loop_state.effective_snapshot(stopped_snap, &retry, Some(&song));
        assert_eq!(synced.state, PlaybackState::Error);
        assert_eq!(synced.position_ms, 45_000);
        assert_eq!(
            synced.current_song.as_ref().map(|s| s.video_id.as_str()),
            Some("vid_net")
        );

        // Manual PlayPause after retries exhausted must resume from 45s with fresh URL resolution.
        let manual = resolve_manual_resume_target(
            None,
            Some(song.clone()),
            &retry,
            0, // even if caller lost last_published_pos_ms, retry remembers 45_000
        )
        .expect("manual resume target");
        assert_eq!(manual.resume_ms, 45_000);
        assert!(manual.bypass_cache);

        // Sanity-check that retry notices + final error notice were emitted.
        let mut notices = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            match evt {
                BackendEvent::StatusNotice(s) => notices.push(s),
                BackendEvent::ErrorNotice(e) => notices.push(e),
                _ => {}
            }
        }
        assert!(notices.iter().any(|m| m.contains("(2/3)")));
        assert!(notices.iter().any(|m| m.contains("(3/3)")));
        assert!(notices.iter().any(|m| m.contains("Playback failed")));
    }

    #[test]
    fn home_fetch_state_resets_in_flight_on_failure_so_sync_can_retry() {
        let mut home = HomeFetchState::default();
        assert!(home.should_start_on_sync());

        // First startup fetch begins while offline -> fails (None).
        home.mark_started();
        assert!(!home.should_start_on_sync());
        home.mark_finished(None);
        assert!(!home.in_flight);
        assert!(home.cached.is_none());
        assert!(
            home.should_start_on_sync(),
            "failed home fetch must clear in_flight so SyncFullState can retry"
        );

        // Subsequent retry succeeds -> cached and no longer auto-restarts on SyncFullState.
        home.mark_started();
        home.mark_finished(Some(HomeFeed {
            shelves: Vec::new(),
            continuation: None,
        }));
        assert!(!home.in_flight);
        assert!(home.cached.is_some());
        assert!(!home.should_start_on_sync());

        // Manual refresh that fails later resets in_flight without wiping existing cached feed.
        home.mark_started();
        home.mark_finished(None);
        assert!(!home.in_flight);
        assert!(home.cached.is_some());
    }

    #[test]
    fn manual_resume_target_prefers_fallback_resume_then_exhausted_retry() {
        let dac_song = Song::new("vid_dac", "Hi-Res Track", "Artist", None);
        let fallback = FallbackResumeState {
            song: dac_song.clone(),
            position_ms: 123_456,
            duration_ms: 240_000,
        };
        let retry = PlaybackErrorRetry::default();

        let from_fallback = resolve_manual_resume_target(Some(fallback), None, &retry, 0)
            .expect("fallback resume target");
        assert_eq!(from_fallback.song.video_id, "vid_dac");
        assert_eq!(from_fallback.resume_ms, 123_456);
        assert!(!from_fallback.bypass_cache);

        let from_last_pos = resolve_manual_resume_target(None, Some(dac_song), &retry, 88_000)
            .expect("last published pos target");
        assert_eq!(from_last_pos.resume_ms, 88_000);
        assert!(!from_last_pos.bypass_cache);
    }

    #[test]
    fn search_suggest_gate_cancels_previous_and_drops_stale_generation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");

        rt.block_on(async {
            let mut gate = SearchSuggestGate::default();
            let (gen1, counter1) = gate.begin();
            let (tx1, rx1) = tokio::sync::oneshot::channel::<()>();
            let h1 = tokio::spawn(async move {
                let _ = rx1.await;
            });
            gate.set_active(h1);
            assert!(SearchSuggestGate::is_current(&counter1, gen1));

            // New keystroke starts gen2 -> cancels gen1 task and invalidates gen1 guard.
            let (gen2, counter2) = gate.begin();
            assert_ne!(gen1, gen2);
            assert!(!SearchSuggestGate::is_current(&counter1, gen1));
            assert!(SearchSuggestGate::is_current(&counter2, gen2));
            // Give aborted task a tick to drop rx1.
            tokio::task::yield_now().await;
            assert!(tx1.is_closed());

            // Explicit cancel (e.g. search submitted or input cleared) invalidates gen2 too.
            gate.cancel();
            assert!(!SearchSuggestGate::is_current(&counter2, gen2));
        });
    }

    #[test]
    fn db_worker_rich_search_history_v4_dedup_and_liked_songs_round_trip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");

        let tmp_dir = std::env::temp_dir().join(format!(
            "myusic_db_v4_test_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        let db = DbWorker::start(tmp_dir.join("library.db"));

        rt.block_on(async {
            let mut song = Song::new(
                "MV_3Dpw-BRY",
                "Nightcall",
                "Kavinsky",
                Some("https://i.ytimg.com/vi/MV_3Dpw-BRY/hqdefault.jpg".to_owned()),
            );
            song.album_name = Some("OutRun".to_owned());
            song.duration_text = Some("4:18".to_owned());

            // 1. Record raw query search, then record clicking the matching song -> query dedups!
            let query_rec = build_query_history_record(
                "nightcall",
                &[SearchResult::TopTrack { song: song.clone() }],
                1_000,
            );
            assert_eq!(query_rec.album.as_deref(), Some("OutRun"));
            assert_eq!(query_rec.duration_text.as_deref(), Some("4:18"));
            assert_eq!(query_rec.duration_ms, Some(258_000));

            let after_query = db.record_search_history(query_rec).await;
            assert_eq!(after_query.len(), 1);

            let mut song_rec = SearchHistoryRecord::from_song(&song, 2_000);
            song_rec.artwork_url = song_thumbnail_url(&song);
            let after_track = db.record_search_history(song_rec).await;
            assert_eq!(
                after_track.len(),
                1,
                "duplicate query row should be evicted when selecting matching song"
            );
            assert_eq!(after_track[0].id, "MV_3Dpw-BRY");
            assert_eq!(after_track[0].album.as_deref(), Some("OutRun"));
            assert_eq!(after_track[0].duration_text.as_deref(), Some("4:18"));
            assert_eq!(after_track[0].duration_ms, Some(258_000));

            // 2. Toggle like on a song not yet in `songs` table -> auto-upserts & marks liked.
            let (is_liked, liked_ids) = db.toggle_song_liked(song.clone()).await;
            assert!(is_liked);
            assert!(liked_ids.contains("MV_3Dpw-BRY"));

            let snap = db.fetch_library_snapshot().await;
            assert!(snap.liked_video_ids.contains("MV_3Dpw-BRY"));
            assert_eq!(snap.search_history.len(), 1);
            assert_eq!(snap.search_history[0].album.as_deref(), Some("OutRun"));

            // 3. Toggle again -> unliked.
            let (is_liked2, liked_ids2) = db.toggle_song_liked(song).await;
            assert!(!is_liked2);
            assert!(!liked_ids2.contains("MV_3Dpw-BRY"));
        });

        db.shutdown_blocking();
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
