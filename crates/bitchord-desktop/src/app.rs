//! Root GPUI window view [`BitChordApp`] coordinating navigation, theme,
//! bounded LRU artwork cache with automatic lazy viewport loading, smooth
//! auto-scrolling lyrics, debounced search suggestions, backend event
//! consumption, and view routing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use bitchord_audio::{PlaybackSnapshot, PlaybackState};
use bitchord_dsp_core::{EqLayout, EqualizerPreset};
use bitchord_integrations::ListenTogetherState;
use bitchord_model::{
    BrowseItem, BrowseType, EntityType, HomeFeed, PlaybackSourceType, SearchFilter,
    SearchHistoryEntity, SearchResult, Song,
};
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Decorations, Entity, FocusHandle, Focusable, ListAlignment, ListOffset,
    ListState, ObjectFit, Pixels, Render, Task, Window, div, img, px,
};

use crate::actions::{
    CloseWindowAction, FocusSearch, NextTrack, PlayPause, PrevTrack, QuitApp, Route, SeekBack,
    SeekForward, ToggleLyrics, ToggleMute, ToggleNowPlaying, ToggleQueue, ToggleTheme, VolumeDown,
    VolumeUp,
};
use crate::artwork::{ArtworkCache, fetch_and_decode_artwork, song_thumbnail_url};
use crate::backend::{
    AppBackend, BackendCommand, BackendEvent, DetailPayload, LibrarySnapshot, LyricsPayload,
    QueueViewSnapshot, SearchSuggestionsPayload, StreamBackendChoice,
};
use crate::icons;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme::{ArtworkPalette, BitChordTheme, ThemeMode};

/// Async fetch state for any remote or local panel (`R-27`).
#[derive(Debug, Clone)]
pub enum LoadState<T> {
    Idle,
    Loading,
    Ready(T),
    Error(String),
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

    // Continuous pill slider bounds & drag seek throttling (`notes.md` #4)
    pub bar_seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub np_seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub vol_slider_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub last_drag_seek_at: Instant,

    // Screen data (explicit Loading / Empty / Error states)
    pub home: LoadState<HomeFeed>,
    pub search_filter: SearchFilter,
    pub search_results: LoadState<Vec<SearchResult>>,
    pub search_suggestions: SearchSuggestionsPayload,
    pub search_input_text: String,
    pub search_suggestions_open: bool,
    pub last_search_query: String,
    pub detail: LoadState<DetailPayload>,
    pub lyrics: LoadState<LyricsPayload>,
    pub lyrics_list_state: ListState,
    pub show_translated_lyrics: bool,
    pub active_lyric_idx: Option<usize>,
    pub prev_lyric_idx: Option<usize>,
    pub lyric_transition_at: Instant,
    pub lyrics_last_frame_at: Instant,
    pub lyrics_manual_scroll_until: Option<Instant>,
    pub library: LibrarySnapshot,
    pub party: ListenTogetherState,
    pub party_probe_status: Option<String>,

    // Audio DSP UI controls
    pub eq_enabled: bool,
    pub eq_preset: EqualizerPreset,
    pub eq_bands_db: [f32; EqLayout::MANUAL_COUNT],
    pub stream_backend: StreamBackendChoice,
    pub discord_rpc_enabled: bool,

    // Artwork LRU cache + active Now Playing mesh backdrop
    pub artwork_cache: ArtworkCache,
    pub default_mesh: std::sync::Arc<gpui::RenderImage>,
    pending_art_urls: RefCell<Vec<String>>,

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

        let search_input = cx.new(|cx| TextInput::new("Search songs, artists, albums...", cx));
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
                    "BitChord Linux".to_owned()
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

        let mut subscriptions = Vec::new();
        subscriptions.push(
            cx.subscribe(&search_input, |this, _, event, cx| match event {
                TextInputEvent::Changed(query) => {
                    this.on_search_query_changed(query.clone(), cx);
                }
                TextInputEvent::Submitted(query) => {
                    this.submit_search(query.clone(), cx);
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

        // Consume discrete backend events without polling timers (`gpui-linux.md` §3.3).
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
        };

        // Trigger initial Home feed + Library load immediately.
        backend.send(BackendCommand::LoadHome);
        backend.send(BackendCommand::RefreshLibrary);

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
            bar_seek_bounds: Rc::new(Cell::new(Bounds::default())),
            np_seek_bounds: Rc::new(Cell::new(Bounds::default())),
            vol_slider_bounds: Rc::new(Cell::new(Bounds::default())),
            last_drag_seek_at: now,
            home: LoadState::Loading,
            search_filter: SearchFilter::Songs,
            search_results: LoadState::Idle,
            search_suggestions: SearchSuggestionsPayload::default(),
            search_input_text: String::new(),
            search_suggestions_open: false,
            last_search_query: String::new(),
            detail: LoadState::Idle,
            lyrics: LoadState::Idle,
            lyrics_list_state: ListState::new(0, ListAlignment::Top, px(260.0)),
            show_translated_lyrics: false,
            active_lyric_idx: None,
            prev_lyric_idx: None,
            lyric_transition_at: now,
            lyrics_last_frame_at: now,
            lyrics_manual_scroll_until: None,
            library: LibrarySnapshot::default(),
            party: ListenTogetherState::default(),
            party_probe_status: None,
            eq_enabled: false,
            eq_preset: EqualizerPreset::Flat,
            eq_bands_db: [0.0; EqLayout::MANUAL_COUNT],
            stream_backend: StreamBackendChoice::Auto,
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

    /// Interpolates current playhead smoothly between backend ticks for 60fps lyrics/scrubber.
    pub fn live_position_ms(&self) -> u64 {
        if self.playback.state != PlaybackState::Playing {
            return self.playback.position_ms;
        }
        let elapsed = (self.playback_synced_at.elapsed().as_millis() as f64
            * f64::from(self.playback.speed))
        .round() as u64;
        let est = self.playback.position_ms.saturating_add(elapsed);
        if self.playback.duration_ms > 0 {
            est.min(self.playback.duration_ms)
        } else {
            est
        }
    }

    /// Seeks immediately on click, or throttled (~90 ms) while dragging a pill scrubber.
    pub fn seek_fraction(&mut self, frac: f32, is_drag: bool, cx: &mut Context<Self>) {
        let dur_ms = self.playback.duration_ms;
        if dur_ms == 0 {
            return;
        }
        let clamped = frac.clamp(0.0, 1.0);
        let target_ms = (f64::from(clamped) * dur_ms as f64).round() as u64;
        // Optimistically update local position so the continuous pill follows pointer at 60fps.
        self.playback.position_ms = target_ms;
        self.playback_synced_at = Instant::now();
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

    /// Debounces live YouTube Music query + song typeahead suggestions as the user types.
    pub fn on_search_query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_input_text = query.clone();
        let trimmed = query.trim().to_owned();
        if trimmed.is_empty() {
            self.search_suggest_task = None;
            self.search_suggestions = SearchSuggestionsPayload::default();
            self.search_suggestions_open = false;
            self.search_results = LoadState::Idle;
            self.last_search_query.clear();
            cx.notify();
            return;
        }
        if trimmed.len() < 2 {
            self.search_suggest_task = None;
            self.search_suggestions = SearchSuggestionsPayload::default();
            self.search_suggestions_open = false;
            cx.notify();
            return;
        }
        self.search_suggestions_open = true;
        let q = trimmed.clone();
        self.search_suggest_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.search_input_text.trim() == q {
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
        let mut subtitle = song.artist.clone();
        if let Some(ref dur) = song.duration_text
            && !dur.is_empty()
        {
            subtitle = format!("{} · {dur}", song.artist);
        }
        self.send(
            BackendCommand::RecordSearchSelection(SearchHistoryEntity {
                id: song.video_id.clone(),
                title: song.title.clone(),
                subtitle,
                artwork_url: song_thumbnail_url(song),
                entity_type: EntityType::Track,
                timestamp: 0,
            }),
            cx,
        );
    }

    pub fn record_browse_search_selection(&self, item: &BrowseItem, cx: &App) {
        let entity_type = match item.r#type {
            BrowseType::Album => EntityType::Album,
            BrowseType::Artist => EntityType::Artist,
            BrowseType::Playlist => EntityType::Playlist,
            BrowseType::Other => EntityType::Other,
        };
        self.send(
            BackendCommand::RecordSearchSelection(SearchHistoryEntity {
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
            }),
            cx,
        );
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

    /// Starts an asynchronous artwork download without ever aborting other in-flight tasks.
    pub fn ensure_artwork_loaded(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(normalized) = self.artwork_cache.begin_fetch(url) else {
            return;
        };
        let backend = cx.global::<AppBackend>().clone();
        let http = backend.http.clone();
        let cache_dir = backend.paths.cache_dir.clone();
        let key_for_task = normalized.clone();

        let task = cx.spawn(async move |this, cx| {
            let url_for_fetch = key_for_task.clone();
            let res = gpui_ce_tokio::Tokio::spawn(cx, async move {
                fetch_and_decode_artwork(&http, &cache_dir, &url_for_fetch).await
            })
            .await;

            let _ = this.update(cx, |app, cx| {
                app.art_tasks.remove(&key_for_task);
                match res {
                    Ok(Ok(art)) => {
                        let evicted = app.artwork_cache.insert(key_for_task, art);
                        for old in evicted {
                            cx.drop_image(old.image, None);
                            cx.drop_image(old.mesh, None);
                        }
                    }
                    _ => {
                        app.artwork_cache.mark_failed(&key_for_task);
                    }
                }
                cx.notify();
            });
        });
        self.art_tasks.insert(normalized, task);
    }

    fn flush_pending_artwork(&mut self, cx: &mut Context<Self>) {
        let queued: Vec<String> = self.pending_art_urls.borrow_mut().drain(..).collect();
        for url in queued {
            self.ensure_artwork_loaded(&url, cx);
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

    /// Smoothly auto-scrolls the Now Playing lyrics list whenever the active verse advances
    /// (`notes.md` #3), respecting temporary manual user scrolling.
    fn sync_lyrics_scroll(&mut self) -> bool {
        if self.route != Route::NowPlaying {
            return false;
        }
        let LoadState::Ready(ref payload) = self.lyrics else {
            return false;
        };
        if payload.lines.is_empty() {
            return false;
        }

        let pos_ms = self.live_position_ms() as i64;
        let active_idx = payload.lines.iter().rposition(|l| pos_ms >= l.time_ms);

        let now = Instant::now();
        let dt = now
            .duration_since(self.lyrics_last_frame_at)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.lyrics_last_frame_at = now;

        if active_idx != self.active_lyric_idx {
            self.prev_lyric_idx = self.active_lyric_idx;
            self.active_lyric_idx = active_idx;
            self.lyric_transition_at = now;
        }

        let mut animating = self.lyric_transition_at.elapsed() < Duration::from_millis(520);
        let manual_paused = self
            .lyrics_manual_scroll_until
            .is_some_and(|until| now < until);

        if !manual_paused && let Some(idx) = active_idx {
            let vp = self.lyrics_list_state.viewport_bounds();
            let vp_h = f32::from(vp.size.height);
            if vp_h > 40.0 {
                // Keep active verse comfortably positioned ~28% down the lyrics viewport.
                let target_y = vp.origin.y + px(vp_h * 0.28);
                if let Some(item_b) = self.lyrics_list_state.bounds_for_item(idx) {
                    let delta = f32::from(item_b.origin.y - target_y);
                    if delta.abs() > 0.75 {
                        // Exponential ease-out: silky ~320 ms glide on verse change!
                        let alpha = 1.0 - (-9.0 * dt).exp();
                        let step = if delta.abs() < 1.5 {
                            delta
                        } else {
                            delta * alpha
                        };
                        self.lyrics_list_state.scroll_by(px(step));
                        animating = true;
                    }
                } else {
                    // Active line is off-screen (e.g., seek jump or opened mid-track):
                    // jump close so next frame glides smoothly into exact 28% focal alignment.
                    self.lyrics_list_state.scroll_to(ListOffset {
                        item_ix: idx.saturating_sub(2),
                        offset_in_item: px(0.0),
                    });
                    animating = true;
                }
            }
        }
        animating
    }

    fn handle_backend_event(&mut self, evt: BackendEvent, cx: &mut Context<Self>) {
        match evt {
            BackendEvent::AudioBackendReady {
                pipewire_active,
                message,
            } => {
                self.pipewire_active = pipewire_active;
                if !pipewire_active {
                    self.audio_banner = Some(message);
                }
            }
            BackendEvent::PlaybackUpdated(snap) => {
                let snap = *snap;
                if let Some(ref song) = snap.current_song
                    && let Some(url) = song_thumbnail_url(song)
                {
                    self.ensure_artwork_loaded(&url, cx);
                }
                self.playback = snap;
                self.playback_synced_at = Instant::now();
            }
            BackendEvent::QueueUpdated(q) => {
                let urls: Vec<String> = q
                    .items
                    .iter()
                    .take(24)
                    .filter_map(song_thumbnail_url)
                    .collect();
                self.queue = q;
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
                    || self
                        .search_input_text
                        .trim()
                        .eq_ignore_ascii_case(&payload.query)
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
                    let mut urls = Vec::new();
                    if let Some(ref h) = payload.header
                        && let Some(ref u) = h.thumbnail_url
                    {
                        urls.push(u.clone());
                    }
                    for s in payload.songs.iter().take(24) {
                        if let Some(u) = song_thumbnail_url(s) {
                            urls.push(u);
                        }
                    }
                    self.detail = LoadState::Ready(payload);
                    for u in urls {
                        self.ensure_artwork_loaded(&u, cx);
                    }
                }
                Err(err) => {
                    self.detail = LoadState::Error(err);
                }
            },
            BackendEvent::LyricsLoading => {
                self.lyrics = LoadState::Loading;
                self.active_lyric_idx = None;
                self.prev_lyric_idx = None;
                self.lyrics_manual_scroll_until = None;
                self.lyrics_list_state = ListState::new(0, ListAlignment::Top, px(260.0));
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
                match result {
                    Ok(payload) => {
                        let count = payload.lines.len();
                        self.active_lyric_idx = None;
                        self.prev_lyric_idx = None;
                        self.lyrics_manual_scroll_until = None;
                        self.lyrics_list_state =
                            ListState::new(count, ListAlignment::Top, px(260.0));
                        self.lyrics = LoadState::Ready(payload);
                    }
                    Err(err) => {
                        self.lyrics = LoadState::Error(err);
                        self.active_lyric_idx = None;
                        self.prev_lyric_idx = None;
                        self.lyrics_list_state = ListState::new(0, ListAlignment::Top, px(260.0));
                    }
                }
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
                self.library = lib;
                for u in urls {
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
            .and_then(|u| self.artwork_cache.get(&u))
            .map(|a| a.mesh.clone())
            .unwrap_or_else(|| self.default_mesh.clone())
    }

    /// Renders an album/track cover thumbnail (with automatic on-demand lazy loading
    /// for any visible URL not yet in cache) and crisp vector SVG fallback.
    pub fn render_art_box(
        &self,
        url: Option<&str>,
        size_px: f32,
        radius_px: f32,
        fallback_icon: &'static [u8],
        theme: BitChordTheme,
    ) -> impl IntoElement {
        if let Some(u) = url {
            if let Some(art) = self.artwork_cache.get(u) {
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
            if !self.artwork_cache.is_settled(u) {
                self.pending_art_urls.borrow_mut().push(u.to_owned());
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

        // Smoothly drive lyrics auto-scroll & verse transitions when Now Playing is visible.
        let lyrics_animating = self.sync_lyrics_scroll();
        if self.route == Route::NowPlaying
            && (self.playback.state == PlaybackState::Playing || lyrics_animating)
        {
            window.request_animation_frame();
        }

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

/// Formats milliseconds as `m:ss` (tabular alignment).
pub fn format_time_ms(ms: u64) -> String {
    let total_secs = ms / 1_000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}
