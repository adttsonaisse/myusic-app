//! Root GPUI window view [`BitChordApp`] coordinating navigation, theme,
//! bounded LRU artwork cache with automatic lazy viewport loading, smooth
//! auto-scrolling lyrics, debounced search suggestions, backend event
//! consumption, and view routing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bitchord_audio::{
    AlsaDeviceInfo, AudioOutputMode, OutputStreamHealth, PlaybackSnapshot, PlaybackState,
};
use bitchord_dsp_core::{EqLayout, EqualizerPreset};
use bitchord_integrations::ListenTogetherState;
use bitchord_model::{
    BrowseItem, BrowseType, EntityType, HomeFeed, PlaybackSourceType, SearchFilter, SearchResult,
    Song,
};
use bitchord_storage::{AudioOutputPreference, SearchHistoryRecord, format_duration_ms};
use gpui::prelude::*;
use gpui::{
    App, Context, Decorations, Entity, FocusHandle, Focusable, ObjectFit, Render, Task, Window,
    div, img, px,
};

use crate::actions::{
    CloseWindowAction, FocusSearch, NextTrack, PlayPause, PrevTrack, QuitApp, Route, SeekBack,
    SeekForward, ToggleLyrics, ToggleMute, ToggleNowPlaying, ToggleQueue, ToggleTheme, VolumeDown,
    VolumeUp,
};
use crate::artwork::{
    ArtworkCache, ArtworkTier, fetch_and_decode_artwork_with_tier, song_thumbnail_url,
};
use crate::backend::{
    AppBackend, BackendCommand, BackendEvent, DetailPayload, LibrarySnapshot, LyricsSyncState,
    QueueViewSnapshot, SearchSuggestionsPayload, StreamBackendChoice,
};
use crate::branding;
use crate::icons;
use crate::pill_slider::PillSliderState;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{ArtworkPalette, BitChordTheme, ThemeMode};
use crate::views::lyrics_view::{LyricsClock, LyricsView, LyricsViewEvent, LyricsViewState};
use crate::views::player_bar::resolve_effective_duration_ms;

/// Async fetch state for any remote or local panel (`R-27`).
#[derive(Debug, Clone)]
pub enum LoadState<T> {
    Idle,
    Loading,
    Ready(T),
    Error(String),
}

/// Source filter tabs for the unified Library screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibrarySourceFilter {
    #[default]
    All,
    Local,
    WebDav,
    Smb,
    YouTube,
}

impl LibrarySourceFilter {
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Local,
        Self::WebDav,
        Self::Smb,
        Self::YouTube,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Local => "Local",
            Self::WebDav => "WebDAV",
            Self::Smb => "SMB",
            Self::YouTube => "YouTube",
        }
    }

    pub fn matches(self, song: &Song) -> bool {
        match self {
            Self::All => true,
            Self::Local => song.local_path.is_some(),
            Self::WebDav => song.video_id.starts_with("webdav:"),
            Self::Smb => song.video_id.starts_with("smb:"),
            Self::YouTube => {
                song.local_path.is_none()
                    && !song.video_id.starts_with("webdav:")
                    && !song.video_id.starts_with("smb:")
            }
        }
    }
}

/// Root window view entity mounted onto the running [`AppBackend`] global.
pub struct BitChordApp {
    pub focus_handle: FocusHandle,
    pub theme_mode: ThemeMode,
    pub route: Route,
    pub previous_route: Route,
    pub sidebar_collapsed: bool,
    pub queue_open: bool,
    pub nerd_stats_open: bool,
    pub close_to_tray: bool,
    pub pipewire_active: bool,
    pub audio_banner: Option<String>,

    // Real-time playback & interpolation state
    pub playback: PlaybackSnapshot,
    pub playback_synced_at: Instant,
    pub queue: QueueViewSnapshot,

    // Continuous pill slider states & drag seek throttling (`notes.md` #4)
    pub bar_seek_slider: PillSliderState,
    pub np_seek_slider: PillSliderState,
    pub vol_slider: PillSliderState,
    pub last_drag_seek_at: Instant,

    // Screen data (explicit Loading / Empty / Error states)
    pub home: LoadState<HomeFeed>,
    pub search_filter: SearchFilter,
    pub search_results: LoadState<Vec<SearchResult>>,
    pub search_suggestions: SearchSuggestionsPayload,
    pub search_input_text: String,
    pub search_suggestions_open: bool,
    pub search_suggest_selected_idx: Option<usize>,
    pub search_suggest_generation: u64,
    pub last_search_query: String,
    pub detail: LoadState<DetailPayload>,
    pub lyrics_view: Entity<LyricsView>,
    pub show_translated_lyrics: bool,
    pub library: LibrarySnapshot,
    pub liked_video_ids: HashSet<String>,
    pub library_source_filter: LibrarySourceFilter,
    pub party: ListenTogetherState,
    pub party_probe_status: Option<String>,

    // Audio DSP UI controls
    pub eq_enabled: bool,
    pub eq_preset: EqualizerPreset,
    pub eq_bands_db: [f32; EqLayout::MANUAL_COUNT],
    pub stream_backend: StreamBackendChoice,
    pub audio_output_pref: AudioOutputPreference,
    pub selected_alsa_device: String,
    pub audio_devices: Vec<AlsaDeviceInfo>,
    pub automix_mode: bitchord_automix::AutomixMode,
    pub automix_phrase_bars: u32,
    pub onnx_automix_status: bitchord_automix::OnnxAutomixStatus,
    pub discord_rpc_enabled: bool,

    // Artwork LRU cache + active Now Playing mesh backdrop
    pub artwork_cache: ArtworkCache,
    pub default_mesh: std::sync::Arc<gpui::RenderImage>,
    pending_art_urls: RefCell<Vec<(String, ArtworkTier)>>,

    // Transient feedback banner
    pub status_notice: Option<String>,
    pub error_notice: Option<String>,

    // Reusable keyboard-accessible inputs
    pub search_input: Entity<TextInput>,
    pub folder_input: Entity<TextInput>,
    pub party_server_input: Entity<TextInput>,
    pub party_code_input: Entity<TextInput>,
    pub party_nick_input: Entity<TextInput>,
    pub cookie_input: Entity<TextInput>,
    pub spotify_input: Entity<TextInput>,
    pub webdav_url_input: Entity<TextInput>,
    pub webdav_user_input: Entity<TextInput>,
    pub webdav_pass_input: Entity<TextInput>,
    pub smb_host_input: Entity<TextInput>,
    pub smb_share_input: Entity<TextInput>,
    pub smb_path_input: Entity<TextInput>,
    pub smb_user_input: Entity<TextInput>,
    pub smb_pass_input: Entity<TextInput>,

    // Subscriptions & background tasks owned by the window
    _subscriptions: Vec<gpui::Subscription>,
    _event_task: Task<()>,
    art_tasks: HashMap<String, Task<()>>,
    search_suggest_task: Option<Task<()>>,
    notice_clear_task: Option<Task<()>>,
}

impl BitChordApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = cx.global::<AppBackend>().clone();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let search_input = cx.new(|cx| {
            TextInput::new("Search songs, artists, albums...", cx).with_leading_icon(icons::SEARCH)
        });
        let lyrics_view = cx.new(LyricsView::new);
        let default_music_dir = backend
            .initial_settings
            .local_music_folders
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| {
                directories::UserDirs::new()
                    .and_then(|u| u.audio_dir().map(|p| p.display().to_string()))
                    .unwrap_or_else(|| "/home".to_owned())
            });
        let folder_input =
            cx.new(|cx| TextInput::new("/path/to/Music", cx).with_initial(default_music_dir));
        let party_server = if backend.initial_settings.listen_together_server.is_empty() {
            "https://api.bitchord.kushagrasingh.in".to_owned()
        } else {
            backend.initial_settings.listen_together_server.clone()
        };
        let party_server_input = cx.new(|cx| {
            TextInput::new("https://api.bitchord.kushagrasingh.in", cx).with_initial(party_server)
        });
        let party_code_input = cx.new(|cx| TextInput::new("6-char room code or invite link", cx));
        let party_nick_input = cx.new(|cx| {
            TextInput::new("Display nickname", cx).with_initial(
                if backend.initial_settings.listen_together_nickname.is_empty() {
                    branding::DEFAULT_PARTY_NICKNAME.to_owned()
                } else {
                    backend.initial_settings.listen_together_nickname.clone()
                },
            )
        });
        let cookie_input = cx.new(|cx| {
            TextInput::new(
                "Paste YouTube Music Cookie header (stored in Secret Service)",
                cx,
            )
            .with_masked(true)
        });
        let spotify_input =
            cx.new(|cx| TextInput::new("https://open.spotify.com/playlist/...", cx));
        let webdav_url_input = cx.new(|cx| {
            TextInput::new(
                "https://cloud.example.com/remote.php/dav/files/user/Music",
                cx,
            )
            .with_initial(backend.initial_settings.webdav.url.clone())
        });
        let webdav_user_input = cx.new(|cx| {
            TextInput::new("Username (optional)", cx)
                .with_initial(backend.initial_settings.webdav.username.clone())
        });
        let webdav_pass_input = cx
            .new(|cx| TextInput::new("Password (stored in Secret Service)", cx).with_masked(true));
        let smb_host_input = cx.new(|cx| {
            TextInput::new("nas.local or 192.168.1.50:445", cx)
                .with_initial(backend.initial_settings.smb.host.clone())
        });
        let smb_share_input = cx.new(|cx| {
            TextInput::new("Share name (e.g. Music)", cx)
                .with_initial(backend.initial_settings.smb.share.clone())
        });
        let smb_path_input = cx.new(|cx| {
            TextInput::new("Subfolder path (optional)", cx)
                .with_initial(backend.initial_settings.smb.base_path.clone())
        });
        let smb_user_input = cx.new(|cx| {
            TextInput::new("Username (blank = guest)", cx)
                .with_initial(backend.initial_settings.smb.username.clone())
        });
        let smb_pass_input = cx
            .new(|cx| TextInput::new("Password (stored in Secret Service)", cx).with_masked(true));

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe(&search_input, |this, _, event, cx| match event {
                TextInputEvent::Changed(query) => {
                    this.search_suggest_selected_idx = None;
                    this.on_search_query_changed(query.clone(), cx);
                }
                TextInputEvent::Submitted(query) => {
                    if !this.activate_selected_search_suggestion(cx) {
                        this.submit_search(query.clone(), cx);
                    }
                }
                TextInputEvent::ArrowDown => {
                    this.move_search_suggestion_selection(1, cx);
                }
                TextInputEvent::ArrowUp => {
                    this.move_search_suggestion_selection(-1, cx);
                }
                TextInputEvent::Escape => {
                    this.search_suggestions_open = false;
                    this.search_suggest_selected_idx = None;
                    cx.notify();
                }
                TextInputEvent::Focused => {
                    if this.search_input_text.trim().len() >= 2
                        && (!this.search_suggestions.queries.is_empty()
                            || !this.search_suggestions.items.is_empty())
                    {
                        this.search_suggestions_open = true;
                        cx.notify();
                    }
                }
            }),
        );
        subscriptions.push(
            cx.subscribe(&lyrics_view, |this, _, event, cx| match event {
                LyricsViewEvent::SeekMs(ms) => {
                    this.playback.position_ms = *ms;
                    this.playback_synced_at = Instant::now();
                    this.sync_lyrics_clock(cx);
                    this.send(BackendCommand::SeekMs(*ms), cx);
                    cx.notify();
                }
                LyricsViewEvent::ToggleTranslate => {
                    this.show_translated_lyrics = !this.show_translated_lyrics;
                    let show = this.show_translated_lyrics;
                    this.lyrics_view
                        .update(cx, |lv, cx| lv.set_show_translated(show, cx));
                    if show {
                        this.send(
                            BackendCommand::TranslateLyrics {
                                target_lang: "en".to_owned(),
                            },
                            cx,
                        );
                    }
                    cx.notify();
                }
            }),
        );
        subscriptions.push(cx.subscribe(&folder_input, |this, _, event, cx| {
            if let TextInputEvent::Submitted(path) = event {
                this.scan_folder(path.clone(), cx);
            }
        }));
        subscriptions.push(cx.subscribe(&party_code_input, |this, _, event, cx| {
            if let TextInputEvent::Submitted(code) = event {
                this.join_party(code.clone(), cx);
            }
        }));
        subscriptions.push(cx.subscribe(&spotify_input, |this, _, event, cx| {
            if let TextInputEvent::Submitted(url) = event {
                this.import_spotify(url.clone(), cx);
            }
        }));

        // Discard stale events queued while closed to tray, then consume live events
        // and request a complete state snapshot from the running background backend.
        backend.drain_stale_events();
        let evt_rx = backend.evt_rx.clone();
        let event_task = cx.spawn(async move |this, cx| {
            while let Ok(evt) = evt_rx.recv().await {
                let alive = this.update(cx, |app, cx| {
                    app.handle_backend_event(evt, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let initial_theme = match backend.initial_settings.theme_mode {
            bitchord_storage::ThemeMode::Light => ThemeMode::Light,
            _ => ThemeMode::Dark,
        };

        let playback = PlaybackSnapshot {
            state: PlaybackState::Stopped,
            current_song: None,
            position_ms: 0,
            duration_ms: 0,
            volume: backend.initial_settings.volume,
            muted: backend.initial_settings.muted,
            speed: 1.0,
            skip_silence: false,
            spatial_audio: false,
            eq_enabled: false,
            crossfade_seconds: backend.initial_settings.crossfade_seconds,
            crossfade_phase: bitchord_audio::CrossfadePhase::Idle,
            active_deck: bitchord_audio::DeckId::A,
            nerd_stats: None,
            sleep_timer_remaining_ms: None,
            sleep_after_track: false,
            underruns: 0,
            peak_level: 0.0,
            skipped_silence_ms: 0,
            last_error: None,
            output_mode: AudioOutputMode::PipeWireAuto,
            output_device_label: "PipeWire".to_owned(),
            output_format_label: "F32LE".to_owned(),
            output_sample_rate_hz: bitchord_audio::OUTPUT_SAMPLE_RATE,
            bit_perfect_active: false,
            automix_plan_summary: None,
            output_health: OutputStreamHealth::default(),
            needs_output_reconnect: false,
            output_error: None,
        };

        // Sync full live backend state (restores playback, queue, lyrics, devices, and
        // cached Home/Library instantaneously on reopen from tray, or loads them on startup).
        backend.send(BackendCommand::SyncFullState);

        let now = Instant::now();
        Self {
            focus_handle,
            theme_mode: initial_theme,
            route: Route::Home,
            previous_route: Route::Home,
            sidebar_collapsed: false,
            queue_open: false,
            nerd_stats_open: false,
            close_to_tray: true,
            pipewire_active: true,
            audio_banner: None,
            playback,
            playback_synced_at: now,
            queue: QueueViewSnapshot::default(),
            bar_seek_slider: PillSliderState::new(),
            np_seek_slider: PillSliderState::new(),
            vol_slider: PillSliderState::new(),
            last_drag_seek_at: now,
            home: LoadState::Loading,
            search_filter: SearchFilter::Songs,
            search_results: LoadState::Idle,
            search_suggestions: SearchSuggestionsPayload::default(),
            search_input_text: String::new(),
            search_suggestions_open: false,
            search_suggest_selected_idx: None,
            search_suggest_generation: 0,
            last_search_query: String::new(),
            detail: LoadState::Idle,
            lyrics_view,
            show_translated_lyrics: false,
            library: LibrarySnapshot::default(),
            liked_video_ids: HashSet::new(),
            library_source_filter: LibrarySourceFilter::All,
            party: ListenTogetherState::default(),
            party_probe_status: None,
            eq_enabled: false,
            eq_preset: EqualizerPreset::Flat,
            eq_bands_db: [0.0; EqLayout::MANUAL_COUNT],
            stream_backend: StreamBackendChoice::Auto,
            audio_output_pref: backend.initial_settings.audio_output_mode,
            selected_alsa_device: if backend.initial_settings.alsa_exclusive_device.is_empty() {
                "hw:0,0".to_owned()
            } else {
                backend.initial_settings.alsa_exclusive_device.clone()
            },
            audio_devices: Vec::new(),
            automix_mode: backend.initial_settings.automix_mode,
            automix_phrase_bars: backend.initial_settings.automix_phrase_bars,
            onnx_automix_status: backend.onnx_automix_status.clone(),
            discord_rpc_enabled: backend.initial_settings.discord_rpc_enabled,
            artwork_cache: ArtworkCache::default(),
            default_mesh: ArtworkPalette::default().build_mesh_render_image(),
            pending_art_urls: RefCell::new(Vec::new()),
            status_notice: None,
            error_notice: None,
            search_input,
            folder_input,
            party_server_input,
            party_code_input,
            party_nick_input,
            cookie_input,
            spotify_input,
            webdav_url_input,
            webdav_user_input,
            webdav_pass_input,
            smb_host_input,
            smb_share_input,
            smb_path_input,
            smb_user_input,
            smb_pass_input,
            _subscriptions: subscriptions,
            _event_task: event_task,
            art_tasks: HashMap::new(),
            search_suggest_task: None,
            notice_clear_task: None,
        }
    }

    pub fn theme(&self) -> BitChordTheme {
        BitChordTheme::for_mode(self.theme_mode)
    }

    pub fn send(&self, cmd: BackendCommand, cx: &App) {
        cx.global::<AppBackend>().send(cmd);
    }

    /// Best known duration (ms) for the active track, falling back to `Song::duration_millis()`
    /// and matching queue/library entries when the audio decoder reports `0` (`notes.md` §2 #4).
    pub fn effective_duration_ms(&self) -> u64 {
        resolve_effective_duration_ms(
            self.playback.duration_ms,
            self.playback.current_song.as_ref(),
            &self.queue.items,
            &self.library.songs,
        )
    }

    /// Interpolates current playhead smoothly between backend ticks for 60fps lyrics/scrubber.
    pub fn live_position_ms(&self) -> u64 {
        if self.playback.state != PlaybackState::Playing {
            return self.playback.position_ms;
        }
        let elapsed = (self.playback_synced_at.elapsed().as_millis() as f64
            * f64::from(self.playback.speed))
        .round() as u64;
        let est = self.playback.position_ms.saturating_add(elapsed);
        let dur_ms = self.effective_duration_ms();
        if dur_ms > 0 { est.min(dur_ms) } else { est }
    }

    /// Pushes the latest playback timing snapshot to [`LyricsView`].
    fn sync_lyrics_clock(&self, cx: &mut Context<Self>) {
        let clock = LyricsClock {
            position_ms: self.playback.position_ms,
            duration_ms: self.effective_duration_ms(),
            speed: self.playback.speed,
            playing: self.playback.state == PlaybackState::Playing,
            synced_at: self.playback_synced_at,
        };
        self.lyrics_view.update(cx, |lv, cx| lv.set_clock(clock, cx));
    }

    /// Seeks immediately on click/release, or throttled (~90 ms) while dragging a pill scrubber.
    pub fn seek_fraction(&mut self, frac: f32, is_drag: bool, cx: &mut Context<Self>) {
        let dur_ms = self.effective_duration_ms();
        if dur_ms == 0 {
            return;
        }
        let clamped = frac.clamp(0.0, 1.0);
        let target_ms = (f64::from(clamped) * dur_ms as f64).round() as u64;
        // Optimistically update local position so the continuous pill follows pointer at 60fps.
        self.playback.position_ms = target_ms;
        self.playback_synced_at = Instant::now();
        self.sync_lyrics_clock(cx);
        if !is_drag || self.last_drag_seek_at.elapsed() >= Duration::from_millis(90) {
            self.last_drag_seek_at = Instant::now();
            self.send(BackendCommand::SeekMs(target_ms), cx);
        }
        cx.notify();
    }

    pub fn navigate(&mut self, next: Route, cx: &mut Context<Self>) {
        if self.route != Route::NowPlaying {
            self.previous_route = self.route.clone();
        }
        self.route = next;
        cx.notify();
    }

    pub fn open_detail(
        &mut self,
        browse_id: String,
        title: String,
        subtitle: String,
        kind: BrowseType,
        cx: &mut Context<Self>,
    ) {
        self.detail = LoadState::Loading;
        self.send(
            BackendCommand::LoadDetail {
                browse_id: browse_id.clone(),
                kind,
                fallback_title: title.clone(),
            },
            cx,
        );
        self.navigate(
            Route::Detail {
                browse_id,
                title,
                subtitle,
                kind,
            },
            cx,
        );
    }

    /// Debounces live YouTube Music query + song typeahead suggestions as the user types,
    /// using a generation guard + backend cancellation so stale responses never overwrite newer input.
    pub fn on_search_query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_input_text = query.clone();
        self.search_suggest_generation = self.search_suggest_generation.wrapping_add(1);
        let generation = self.search_suggest_generation;
        let trimmed = query.trim().to_owned();
        if trimmed.is_empty() {
            self.search_suggest_task = None;
            self.search_suggestions = SearchSuggestionsPayload::default();
            self.search_suggestions_open = false;
            self.search_results = LoadState::Idle;
            self.last_search_query.clear();
            self.send(BackendCommand::SearchSuggest(String::new()), cx);
            cx.notify();
            return;
        }
        if trimmed.len() < 2 {
            self.search_suggest_task = None;
            self.search_suggestions = SearchSuggestionsPayload::default();
            self.search_suggestions_open = false;
            self.send(BackendCommand::SearchSuggest(String::new()), cx);
            cx.notify();
            return;
        }
        self.search_suggestions_open = true;
        let q = trimmed.clone();
        self.search_suggest_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(140))
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.search_suggest_generation == generation
                    && app.search_input_text.trim() == q
                {
                    app.send(BackendCommand::SearchSuggest(q), cx);
                }
            });
        }));
        cx.notify();
    }

    pub fn submit_search(&mut self, query: String, cx: &mut Context<Self>) {
        let trimmed = query.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        self.search_input_text = trimmed.clone();
        self.search_suggestions_open = false;
        self.search_suggest_selected_idx = None;
        self.search_suggest_generation = self.search_suggest_generation.wrapping_add(1);
        self.search_suggest_task = None;
        self.last_search_query = trimmed.clone();
        self.search_results = LoadState::Loading;
        self.navigate(Route::Search, cx);
        self.send(
            BackendCommand::Search {
                query: trimmed,
                filter: self.search_filter,
            },
            cx,
        );
    }

    pub fn record_song_search_selection(&self, song: &Song, cx: &App) {
        let mut record = SearchHistoryRecord::from_song(song, 0);
        record.artwork_url = song_thumbnail_url(song);
        self.send(BackendCommand::RecordSearchSelection(record), cx);
    }

    pub fn record_browse_search_selection(&self, item: &BrowseItem, cx: &App) {
        let entity_type = match item.r#type {
            BrowseType::Album => EntityType::Album,
            BrowseType::Artist => EntityType::Artist,
            BrowseType::Playlist => EntityType::Playlist,
            BrowseType::Other => EntityType::Other,
        };
        self.send(
            BackendCommand::RecordSearchSelection(SearchHistoryRecord {
                id: item.browse_id.clone(),
                title: item.title.clone(),
                subtitle: if item.subtitle.is_empty() {
                    format!("{:?}", item.r#type)
                } else {
                    item.subtitle.clone()
                },
                artwork_url: item.thumbnail_url.clone(),
                entity_type,
                timestamp: 0,
                album: None,
                duration_text: None,
                duration_ms: None,
            }),
            cx,
        );
    }

    /// Returns whether `video_id` is currently marked as liked/favorited.
    pub fn is_song_liked(&self, video_id: &str) -> bool {
        self.liked_video_ids.contains(video_id)
    }

    /// Returns whether the currently loaded track is liked/favorited.
    pub fn is_current_song_liked(&self) -> bool {
        self.playback
            .current_song
            .as_ref()
            .is_some_and(|s| self.liked_video_ids.contains(&s.video_id))
    }

    /// Optimistically toggles the liked status of `song` in UI state and persists via SQLite.
    pub fn toggle_like_song(&mut self, song: Song, cx: &mut Context<Self>) {
        if self.liked_video_ids.contains(&song.video_id) {
            self.liked_video_ids.remove(&song.video_id);
        } else {
            self.liked_video_ids.insert(song.video_id.clone());
        }
        self.library.liked_video_ids = self.liked_video_ids.clone();
        self.send(BackendCommand::ToggleLikeSong(Box::new(song)), cx);
        cx.notify();
    }

    /// Toggles liked status for the currently active track (no-op if no track is loaded).
    pub fn toggle_like_current_song(&mut self, cx: &mut Context<Self>) {
        if let Some(song) = self.playback.current_song.clone() {
            self.toggle_like_song(song, cx);
        }
    }

    pub fn play_songs(
        &mut self,
        songs: Vec<Song>,
        start_index: usize,
        source_title: impl Into<String>,
        source_type: PlaybackSourceType,
        source_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.send(
            BackendCommand::PlayContext {
                songs,
                start_index,
                source_title: source_title.into(),
                source_type,
                source_id,
            },
            cx,
        );
        cx.notify();
    }

    pub fn scan_folder(&mut self, raw: String, cx: &mut Context<Self>) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.error_notice =
                Some("Enter a folder path to scan for local audio files.".to_owned());
            cx.notify();
            return;
        }
        self.error_notice = None;
        self.send(BackendCommand::ScanLocalFolder(PathBuf::from(trimmed)), cx);
    }

    pub fn join_party(&mut self, code: String, cx: &mut Context<Self>) {
        let server_url = self.party_server_input.read(cx).text().to_owned();
        let nickname = self.party_nick_input.read(cx).text().to_owned();
        self.send(
            BackendCommand::PartyJoin {
                server_url,
                code_or_url: code,
                nickname,
            },
            cx,
        );
    }

    pub fn import_spotify(&mut self, url_or_id: String, cx: &mut Context<Self>) {
        if url_or_id.trim().is_empty() {
            return;
        }
        self.send(BackendCommand::ImportSpotifyPlaylist(url_or_id), cx);
    }

    /// Starts an asynchronous Thumbnail-tier (`256px`) artwork fetch without aborting other tasks.
    pub fn ensure_artwork_loaded(&mut self, url: &str, cx: &mut Context<Self>) {
        self.ensure_artwork_loaded_tier(url, ArtworkTier::Thumbnail, cx);
    }

    /// Starts an asynchronous tier-aware (`256px` Thumbnail or `512px` Hero) artwork fetch,
    /// backed by SQLite v4 LRU disk cache indexing via [`AppBackend::db_worker`].
    pub fn ensure_artwork_loaded_tier(
        &mut self,
        url: &str,
        tier: ArtworkTier,
        cx: &mut Context<Self>,
    ) {
        let Some((cache_key, normalized)) = self.artwork_cache.begin_fetch_tier(url, tier) else {
            return;
        };
        let backend = cx.global::<AppBackend>().clone();
        let http = backend.http.clone();
        let cache_dir = backend.paths.cache_dir.clone();
        let db_worker = backend.db_worker.clone();
        let auth_snapshot = backend.remote_auth.read().ok().map(|g| g.clone());
        let task_key = cache_key.clone();
        let url_for_state = normalized.clone();

        let task = cx.spawn(async move |this, cx| {
            let url_for_fetch = normalized;
            let res = gpui_ce_tokio::Tokio::spawn(cx, async move {
                fetch_and_decode_artwork_with_tier(
                    &http,
                    &cache_dir,
                    &url_for_fetch,
                    tier,
                    auth_snapshot.as_ref(),
                    Some(&db_worker),
                )
                .await
            })
            .await;

            let _ = this.update(cx, |app, cx| {
                app.art_tasks.remove(&task_key);
                match res {
                    Ok(Ok(art)) => {
                        let evicted = app.artwork_cache.insert_tier(&url_for_state, tier, art);
                        for old in evicted {
                            cx.drop_image(old.image, None);
                            cx.drop_image(old.mesh, None);
                        }
                    }
                    _ => {
                        app.artwork_cache.mark_failed_tier(&url_for_state, tier);
                    }
                }
                cx.notify();
            });
        });
        self.art_tasks.insert(cache_key, task);
    }

    fn flush_pending_artwork(&mut self, cx: &mut Context<Self>) {
        let queued: Vec<(String, ArtworkTier)> = self.pending_art_urls.borrow_mut().drain(..).collect();
        for (url, tier) in queued {
            self.ensure_artwork_loaded_tier(&url, tier, cx);
        }
    }

    fn schedule_notice_auto_clear(&mut self, cx: &mut Context<Self>) {
        self.notice_clear_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |app, cx| {
                app.status_notice = None;
                cx.notify();
            });
        }));
    }

    fn handle_backend_event(&mut self, evt: BackendEvent, cx: &mut Context<Self>) {
        match evt {
            BackendEvent::FullStateSynced(full) => {
                let full = *full;
                self.pipewire_active = full.pipewire_active;
                self.audio_banner = full.audio_banner;
                if self.selected_alsa_device == "hw:0,0"
                    && let Some(usb) = full.audio_devices.iter().find(|d| d.is_usb_dac)
                {
                    self.selected_alsa_device = usb.hw_id.clone();
                } else if !full.selected_alsa_device.is_empty() {
                    self.selected_alsa_device = full.selected_alsa_device;
                }
                self.audio_devices = full.audio_devices;

                let mut prefetch_urls = Vec::new();
                let current_art_url = full.playback.current_song.as_ref().and_then(song_thumbnail_url);
                if let Some(ref url) = current_art_url {
                    prefetch_urls.push(url.clone());
                }
                for song in full.queue.items.iter().take(24) {
                    if let Some(url) = song_thumbnail_url(song) {
                        prefetch_urls.push(url);
                    }
                }
                self.playback = full.playback;
                self.playback_synced_at = Instant::now();
                self.queue = full.queue;

                let lyrics_state = match full.lyrics {
                    LyricsSyncState::Idle => LyricsViewState::Idle,
                    LyricsSyncState::Loading => LyricsViewState::Loading,
                    LyricsSyncState::Ready(payload) => LyricsViewState::Ready(Box::new(payload)),
                    LyricsSyncState::Error(err) => LyricsViewState::Error(err),
                };
                self.lyrics_view
                    .update(cx, |lv, cx| lv.set_state(lyrics_state, cx));

                if let Some(feed) = full.home {
                    for shelf in &feed.shelves {
                        for item in shelf.items.iter().take(10) {
                            if let Some(ref u) = item.thumbnail_url {
                                prefetch_urls.push(u.clone());
                            } else if let Some(ref vid) = item.video_id {
                                let tmp = Song::new(vid.clone(), "", "", None);
                                if let Some(fb) = song_thumbnail_url(&tmp) {
                                    prefetch_urls.push(fb);
                                }
                            }
                        }
                    }
                    self.home = LoadState::Ready(feed);
                }
                if let Some(lib) = full.library {
                    for h in &lib.search_history {
                        if let Some(ref u) = h.artwork_url {
                            prefetch_urls.push(u.clone());
                        }
                    }
                    for s in lib.songs.iter().take(18) {
                        if let Some(u) = song_thumbnail_url(s) {
                            prefetch_urls.push(u);
                        }
                    }
                    self.liked_video_ids = lib.liked_video_ids.clone();
                    self.library = lib;
                }

                self.party = full.party;
                self.eq_enabled = full.eq_enabled;
                self.eq_preset = full.eq_preset;
                self.eq_bands_db = full.eq_bands_db;
                self.stream_backend = full.stream_backend;
                self.audio_output_pref = full.audio_output_pref;
                self.automix_mode = full.automix_mode;
                self.automix_phrase_bars = full.automix_phrase_bars;
                self.discord_rpc_enabled = full.discord_rpc_enabled;
                self.sync_lyrics_clock(cx);

                if let Some(ref hero_url) = current_art_url {
                    self.ensure_artwork_loaded_tier(hero_url, ArtworkTier::Hero, cx);
                }
                for u in prefetch_urls {
                    self.ensure_artwork_loaded(&u, cx);
                }
            }
            BackendEvent::AudioBackendReady {
                pipewire_active,
                message,
            } => {
                self.pipewire_active = pipewire_active;
                if !pipewire_active {
                    self.audio_banner = Some(message);
                }
            }
            BackendEvent::AudioDevicesLoaded(devices) => {
                if self.selected_alsa_device == "hw:0,0"
                    && let Some(usb) = devices.iter().find(|d| d.is_usb_dac)
                {
                    self.selected_alsa_device = usb.hw_id.clone();
                }
                self.audio_devices = devices;
            }
            BackendEvent::PlaybackUpdated(snap) => {
                let snap = *snap;
                if let Some(ref song) = snap.current_song
                    && let Some(url) = song_thumbnail_url(song)
                {
                    self.ensure_artwork_loaded(&url, cx);
                    self.ensure_artwork_loaded_tier(&url, ArtworkTier::Hero, cx);
                }
                self.playback = snap;
                self.playback_synced_at = Instant::now();
                self.sync_lyrics_clock(cx);
            }
            BackendEvent::QueueUpdated(q) => {
                let urls: Vec<String> = q
                    .items
                    .iter()
                    .take(24)
                    .filter_map(song_thumbnail_url)
                    .collect();
                self.queue = q;
                self.sync_lyrics_clock(cx);
                for u in urls {
                    self.ensure_artwork_loaded(&u, cx);
                }
            }
            BackendEvent::HomeLoading => {
                self.home = LoadState::Loading;
            }
            BackendEvent::HomeLoaded(res) => match res {
                Ok(feed) => {
                    // Warm visible thumbnails across all home shelves (bounded by artwork semaphore).
                    let mut prefetch = Vec::new();
                    for shelf in &feed.shelves {
                        for item in shelf.items.iter().take(10) {
                            if let Some(ref u) = item.thumbnail_url {
                                prefetch.push(u.clone());
                            } else if let Some(ref vid) = item.video_id {
                                let tmp = Song::new(vid.clone(), "", "", None);
                                if let Some(fb) = song_thumbnail_url(&tmp) {
                                    prefetch.push(fb);
                                }
                            }
                        }
                    }
                    self.home = LoadState::Ready(feed);
                    for u in prefetch {
                        self.ensure_artwork_loaded(&u, cx);
                    }
                }
                Err(err) => {
                    self.home = LoadState::Error(err);
                }
            },
            BackendEvent::SearchLoading { query } => {
                self.last_search_query = query;
                self.search_results = LoadState::Loading;
            }
            BackendEvent::SearchLoaded { query, result } => {
                self.last_search_query = query;
                match result {
                    Ok(rows) => {
                        let urls: Vec<String> =
                            rows.iter()
                                .take(24)
                                .filter_map(|r| match r {
                                    SearchResult::TopTrack { song }
                                    | SearchResult::Track { song } => song_thumbnail_url(song),
                                    SearchResult::Browse { item } => item.thumbnail_url.clone(),
                                })
                                .collect();
                        self.search_results = LoadState::Ready(rows);
                        for u in urls {
                            self.ensure_artwork_loaded(&u, cx);
                        }
                    }
                    Err(err) => {
                        self.search_results = LoadState::Error(err);
                    }
                }
            }
            BackendEvent::SearchSuggestionsLoaded(payload) => {
                if payload.query.is_empty()
                    || (self.search_suggestions_open
                        && self
                            .search_input_text
                            .trim()
                            .eq_ignore_ascii_case(&payload.query))
                {
                    let urls: Vec<String> = payload
                        .items
                        .iter()
                        .filter_map(|r| match r {
                            SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                                song_thumbnail_url(song)
                            }
                            SearchResult::Browse { item } => item.thumbnail_url.clone(),
                        })
                        .collect();
                    self.search_suggestions = payload;
                    self.search_suggest_selected_idx = None;
                    for u in urls {
                        self.ensure_artwork_loaded(&u, cx);
                    }
                }
            }
            BackendEvent::DetailLoading => {
                self.detail = LoadState::Loading;
            }
            BackendEvent::DetailLoaded(res) => match res {
                Ok(payload) => {
                    let hero_url = payload
                        .header
                        .as_ref()
                        .and_then(|h| h.thumbnail_url.clone())
                        .or_else(|| payload.songs.first().and_then(song_thumbnail_url));
                    let mut urls = Vec::new();
                    for s in payload.songs.iter().take(24) {
                        if let Some(u) = song_thumbnail_url(s) {
                            urls.push(u);
                        }
                    }
                    self.detail = LoadState::Ready(payload);
                    if let Some(ref h_url) = hero_url {
                        self.ensure_artwork_loaded_tier(h_url, ArtworkTier::Hero, cx);
                    }
                    for u in urls {
                        self.ensure_artwork_loaded(&u, cx);
                    }
                }
                Err(err) => {
                    self.detail = LoadState::Error(err);
                }
            },
            BackendEvent::LyricsLoading => {
                self.lyrics_view
                    .update(cx, |lv, cx| lv.set_state(LyricsViewState::Loading, cx));
            }
            BackendEvent::LyricsLoaded { video_id, result } => {
                let current_vid = self
                    .playback
                    .current_song
                    .as_ref()
                    .map(|s| s.video_id.as_str());
                if current_vid.is_some() && current_vid != Some(video_id.as_str()) {
                    return;
                }
                let next_state = match result {
                    Ok(payload) => LyricsViewState::Ready(Box::new(payload)),
                    Err(err) => LyricsViewState::Error(err),
                };
                self.lyrics_view
                    .update(cx, |lv, cx| lv.set_state(next_state, cx));
                self.sync_lyrics_clock(cx);
            }
            BackendEvent::LibraryLoaded(lib) => {
                let mut urls = Vec::new();
                for h in &lib.search_history {
                    if let Some(ref u) = h.artwork_url {
                        urls.push(u.clone());
                    }
                }
                for s in lib.songs.iter().take(18) {
                    if let Some(u) = song_thumbnail_url(s) {
                        urls.push(u);
                    }
                }
                self.liked_video_ids = lib.liked_video_ids.clone();
                self.library = lib;
                for u in urls {
                    self.ensure_artwork_loaded(&u, cx);
                }
            }
            BackendEvent::SearchHistoryUpdated(history) => {
                let urls: Vec<String> = history
                    .iter()
                    .filter_map(|h| h.artwork_url.clone())
                    .collect();
                self.library.stats.search_history = history.len();
                self.library.search_history = history;
                for u in urls {
                    self.ensure_artwork_loaded(&u, cx);
                }
            }
            BackendEvent::LikedSongsUpdated(liked_ids) => {
                self.liked_video_ids = liked_ids.clone();
                self.library.liked_video_ids = liked_ids;
            }
            BackendEvent::LibraryTrackRecorded { song, stats } => {
                let art_url = song_thumbnail_url(&song);
                self.library.stats = stats;
                self.library.songs.retain(|s| s.video_id != song.video_id);
                self.library.songs.insert(0, *song);
                self.library.songs.truncate(200);
                if let Some(u) = art_url {
                    self.ensure_artwork_loaded(&u, cx);
                }
            }
            BackendEvent::PartyServerProbed {
                server,
                online,
                latency_ms,
            } => {
                self.party_probe_status = Some(if online {
                    format!("{server}: Online ({latency_ms} ms)")
                } else {
                    format!("{server}: Unreachable")
                });
            }
            BackendEvent::PartyUpdated(state) => {
                self.party = *state;
            }
            BackendEvent::StatusNotice(msg) => {
                // Keep transient status banners quiet for routine play/loading transitions
                // (already reflected cleanly in the bottom player bar).
                if msg.starts_with("Playing ") || msg.starts_with("Loading ") {
                    self.error_notice = None;
                } else {
                    self.status_notice = Some(msg);
                    self.error_notice = None;
                    self.schedule_notice_auto_clear(cx);
                }
            }
            BackendEvent::ErrorNotice(msg) => {
                self.error_notice = Some(msg);
            }
        }
        cx.notify();
    }

    pub fn active_mesh_image(&self) -> std::sync::Arc<gpui::RenderImage> {
        self.playback
            .current_song
            .as_ref()
            .and_then(song_thumbnail_url)
            .and_then(|u| self.artwork_cache.get_best_for_tier(&u, ArtworkTier::Hero))
            .map(|a| a.mesh.clone())
            .unwrap_or_else(|| self.default_mesh.clone())
    }

    /// Renders an album/track cover thumbnail (with automatic on-demand lazy loading
    /// using `256px` Thumbnail or `512px` Hero tier based on `size_px`) and crisp SVG fallback.
    pub fn render_art_box(
        &self,
        url: Option<&str>,
        size_px: f32,
        radius_px: f32,
        fallback_icon: &'static [u8],
        theme: BitChordTheme,
    ) -> impl IntoElement {
        let tier = ArtworkTier::for_display_size(size_px);
        self.render_art_box_with_tier(url, size_px, radius_px, tier, fallback_icon, theme)
    }

    /// Renders cover artwork with an explicit [`ArtworkTier`], displaying any already-cached
    /// alternate tier immediately while upgrading to the requested tier in the background.
    pub fn render_art_box_with_tier(
        &self,
        url: Option<&str>,
        size_px: f32,
        radius_px: f32,
        tier: ArtworkTier,
        fallback_icon: &'static [u8],
        theme: BitChordTheme,
    ) -> impl IntoElement {
        if let Some(u) = url {
            if !self.artwork_cache.is_settled_tier(u, tier) {
                self.pending_art_urls
                    .borrow_mut()
                    .push((u.to_owned(), tier));
            }
            if let Some(art) = self.artwork_cache.get_best_for_tier(u, tier) {
                return div()
                    .w(px(size_px))
                    .h(px(size_px))
                    .rounded(px(radius_px))
                    .overflow_hidden()
                    .flex_shrink_0()
                    .bg(theme.surface_elevated)
                    .child(
                        img(art.image.clone())
                            .w(px(size_px))
                            .h(px(size_px))
                            .object_fit(ObjectFit::Cover),
                    );
            }
        }

        let icon_sz = (size_px * 0.40).clamp(14.0, 44.0);
        div()
            .w(px(size_px))
            .h(px(size_px))
            .rounded(px(radius_px))
            .flex_shrink_0()
            .bg(theme.surface_elevated)
            .border_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .justify_center()
            .child(icons::icon(fallback_icon, icon_sz, theme.text_muted))
    }

    // ---- GPUI Action Handlers -----------------------------------------------

    fn on_play_pause(&mut self, _: &PlayPause, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::PlayPause, cx);
    }

    fn on_next_track(&mut self, _: &NextTrack, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::NextTrack, cx);
    }

    fn on_prev_track(&mut self, _: &PrevTrack, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::PrevTrack, cx);
    }

    fn on_seek_forward(&mut self, _: &SeekForward, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::SeekDeltaMs(10_000), cx);
    }

    fn on_seek_back(&mut self, _: &SeekBack, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::SeekDeltaMs(-10_000), cx);
    }

    fn on_volume_up(&mut self, _: &VolumeUp, _w: &mut Window, cx: &mut Context<Self>) {
        let next = (self.playback.volume + 0.05).clamp(0.0, 1.0);
        self.send(BackendCommand::SetVolume(next), cx);
    }

    fn on_volume_down(&mut self, _: &VolumeDown, _w: &mut Window, cx: &mut Context<Self>) {
        let next = (self.playback.volume - 0.05).clamp(0.0, 1.0);
        self.send(BackendCommand::SetVolume(next), cx);
    }

    fn on_toggle_mute(&mut self, _: &ToggleMute, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::ToggleMute, cx);
    }

    fn on_toggle_now_playing(
        &mut self,
        _: &ToggleNowPlaying,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.route == Route::NowPlaying {
            let prev = self.previous_route.clone();
            self.route = prev;
        } else {
            self.navigate(Route::NowPlaying, cx);
            return;
        }
        cx.notify();
    }

    fn on_toggle_queue(&mut self, _: &ToggleQueue, _w: &mut Window, cx: &mut Context<Self>) {
        self.queue_open = !self.queue_open;
        cx.notify();
    }

    fn on_toggle_lyrics(&mut self, _: &ToggleLyrics, _w: &mut Window, cx: &mut Context<Self>) {
        if self.route == Route::NowPlaying {
            self.route = self.previous_route.clone();
        } else {
            self.navigate(Route::NowPlaying, cx);
            return;
        }
        cx.notify();
    }

    fn on_toggle_theme(&mut self, _: &ToggleTheme, _w: &mut Window, cx: &mut Context<Self>) {
        self.theme_mode = self.theme_mode.toggled();
        cx.set_global(crate::theme::ActiveTheme(self.theme_mode));
        cx.notify();
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Route::Search, cx);
        let handle = self.search_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    fn on_escape_close(&mut self, _: &CloseWindowAction, _w: &mut Window, cx: &mut Context<Self>) {
        if self.search_suggestions_open {
            self.search_suggestions_open = false;
            self.search_suggest_selected_idx = None;
            cx.notify();
        } else if self.queue_open {
            self.queue_open = false;
            cx.notify();
        } else if self.route == Route::NowPlaying {
            self.route = self.previous_route.clone();
            cx.notify();
        }
    }

    fn on_quit(&mut self, _: &QuitApp, _w: &mut Window, cx: &mut Context<Self>) {
        self.send(BackendCommand::Shutdown, cx);
        cx.quit();
    }
}

impl Focusable for BitChordApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BitChordApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let is_csd = matches!(window.window_decorations(), Decorations::Client { .. });

        let content_area = match &self.route {
            Route::Home => self.render_home(cx).into_any_element(),
            Route::Search => self.render_search(window, cx).into_any_element(),
            Route::Detail { .. } => self.render_detail(cx).into_any_element(),
            Route::Library => self.render_library(window, cx).into_any_element(),
            Route::Downloads => self.render_downloads(cx).into_any_element(),
            Route::ListenTogether => self.render_party(window, cx).into_any_element(),
            Route::Settings => self.render_settings(window, cx).into_any_element(),
            Route::NowPlaying => self.render_now_playing(cx).into_any_element(),
        };

        let titlebar = self.render_titlebar(is_csd, cx).into_any_element();
        let sidebar = self.render_sidebar(cx).into_any_element();
        let queue_drawer = self
            .queue_open
            .then(|| self.render_queue_drawer(cx).into_any_element());
        let player_bar = self.render_player_bar(cx).into_any_element();

        // Flush any newly rendered visible artwork URLs queued lazily during render_art_box().
        self.flush_pending_artwork(cx);

        div()
            .id("bitchord-root")
            .key_context("Player")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_play_pause))
            .on_action(cx.listener(Self::on_next_track))
            .on_action(cx.listener(Self::on_prev_track))
            .on_action(cx.listener(Self::on_seek_forward))
            .on_action(cx.listener(Self::on_seek_back))
            .on_action(cx.listener(Self::on_volume_up))
            .on_action(cx.listener(Self::on_volume_down))
            .on_action(cx.listener(Self::on_toggle_mute))
            .on_action(cx.listener(Self::on_toggle_now_playing))
            .on_action(cx.listener(Self::on_toggle_queue))
            .on_action(cx.listener(Self::on_toggle_lyrics))
            .on_action(cx.listener(Self::on_toggle_theme))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_escape_close))
            .on_action(cx.listener(Self::on_quit))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.text_primary)
            .overflow_hidden()
            // Top bar (adapts automatically to Wayland CSD vs X11/server titlebar)
            .child(titlebar)
            // Middle body: sidebar rail + active route content + optional Queue slide-over drawer
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(sidebar)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w(px(0.0))
                            .min_h(px(0.0))
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .when_some(self.audio_banner.clone(), |el, msg| {
                                el.child(
                                    div()
                                        .w_full()
                                        .px(px(16.0))
                                        .py(px(6.0))
                                        .bg(theme.surface_elevated)
                                        .border_b_1()
                                        .border_color(theme.border)
                                        .text_size(px(12.0))
                                        .text_color(theme.warning)
                                        .child(msg),
                                )
                            })
                            .when_some(self.error_notice.clone(), |el, msg| {
                                el.child(
                                    div()
                                        .w_full()
                                        .px(px(16.0))
                                        .py(px(8.0))
                                        .bg(theme.surface_elevated)
                                        .border_b_1()
                                        .border_color(theme.danger)
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .text_size(px(12.5))
                                        .text_color(theme.danger)
                                        .child(msg)
                                        .child(
                                            div()
                                                .id("dismiss-error")
                                                .w(px(24.0))
                                                .h(px(24.0))
                                                .rounded(px(6.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.error_notice = None;
                                                    cx.notify();
                                                }))
                                                .child(icons::icon(
                                                    icons::CLOSE,
                                                    14.0,
                                                    theme.text_secondary,
                                                )),
                                        ),
                                )
                            })
                            .when_some(self.status_notice.clone(), |el, msg| {
                                el.child(
                                    div()
                                        .w_full()
                                        .px(px(16.0))
                                        .py(px(6.0))
                                        .bg(theme.surface)
                                        .border_b_1()
                                        .border_color(theme.border)
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .text_size(px(12.0))
                                        .text_color(theme.text_secondary)
                                        .child(msg)
                                        .child(
                                            div()
                                                .id("dismiss-notice")
                                                .w(px(22.0))
                                                .h(px(22.0))
                                                .rounded(px(5.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.status_notice = None;
                                                    cx.notify();
                                                }))
                                                .child(icons::icon(
                                                    icons::CLOSE,
                                                    13.0,
                                                    theme.text_muted,
                                                )),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .w_full()
                                    .overflow_hidden()
                                    .child(content_area),
                            ),
                    )
                    .when_some(queue_drawer, |el, drawer| el.child(drawer)),
            )
            // Persistent bottom floating Now Playing bar (glass surface 1 of 2)
            .child(player_bar)
    }
}

/// Formats milliseconds as `m:ss` or `h:mm:ss` (tabular alignment).
pub fn format_time_ms(ms: u64) -> String {
    format_duration_ms(ms)
}
