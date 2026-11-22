//! Root GPUI window view [`BitChordApp`] coordinating navigation, theme,
//! bounded LRU artwork cache, backend event consumption, and view routing.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use bitchord_audio::{PlaybackSnapshot, PlaybackState};
use bitchord_dsp_core::{EqLayout, EqualizerPreset};
use bitchord_integrations::ListenTogetherState;
use bitchord_model::{BrowseType, HomeFeed, PlaybackSourceType, SearchFilter, SearchResult, Song};
use gpui::prelude::*;
use gpui::{
    App, Context, Decorations, Entity, FocusHandle, Focusable, ListAlignment, ListState, ObjectFit,
    Render, Task, Window, div, img, px,
};

use crate::actions::{
    CloseWindowAction, FocusSearch, NextTrack, PlayPause, PrevTrack, QuitApp, Route, SeekBack,
    SeekForward, ToggleLyrics, ToggleMute, ToggleNowPlaying, ToggleQueue, ToggleTheme, VolumeDown,
    VolumeUp,
};
use crate::artwork::{ArtworkCache, fetch_and_decode_artwork};
use crate::backend::{
    AppBackend, BackendCommand, BackendEvent, DetailPayload, LibrarySnapshot, LyricsPayload,
    QueueViewSnapshot, StreamBackendChoice,
};
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

    // Screen data (explicit Loading / Empty / Error states)
    pub home: LoadState<HomeFeed>,
    pub search_filter: SearchFilter,
    pub search_results: LoadState<Vec<SearchResult>>,
    pub last_search_query: String,
    pub detail: LoadState<DetailPayload>,
    pub lyrics: LoadState<LyricsPayload>,
    pub lyrics_list_state: ListState,
    pub show_translated_lyrics: bool,
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
    _art_tasks: Vec<Task<()>>,
    _requested_urls: HashSet<String>,
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
        subscriptions.push(cx.subscribe(&search_input, |this, _, event, cx| {
            let TextInputEvent::Submitted(query) = event;
            this.submit_search(query.clone(), cx);
        }));
        subscriptions.push(cx.subscribe(&folder_input, |this, _, event, cx| {
            let TextInputEvent::Submitted(path) = event;
            this.scan_folder(path.clone(), cx);
        }));
        subscriptions.push(cx.subscribe(&party_code_input, |this, _, event, cx| {
            let TextInputEvent::Submitted(code) = event;
            this.join_party(code.clone(), cx);
        }));
        subscriptions.push(cx.subscribe(&spotify_input, |this, _, event, cx| {
            let TextInputEvent::Submitted(url) = event;
            this.import_spotify(url.clone(), cx);
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
            playback_synced_at: Instant::now(),
            queue: QueueViewSnapshot::default(),
            home: LoadState::Loading,
            search_filter: SearchFilter::Songs,
            search_results: LoadState::Idle,
            last_search_query: String::new(),
            detail: LoadState::Idle,
            lyrics: LoadState::Idle,
            lyrics_list_state: ListState::new(0, ListAlignment::Top, px(220.0)),
            show_translated_lyrics: false,
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
            _art_tasks: Vec::new(),
            _requested_urls: HashSet::new(),
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

    pub fn submit_search(&mut self, query: String, cx: &mut Context<Self>) {
        let trimmed = query.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
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

    pub fn ensure_artwork_loaded(&mut self, url: &str, cx: &mut Context<Self>) {
        if url.is_empty() || !self.artwork_cache.begin_fetch(url) {
            return;
        }
        let backend = cx.global::<AppBackend>().clone();
        let http = backend.http.clone();
        let cache_dir = backend.paths.cache_dir.clone();
        let url_owned = url.to_owned();

        let task = cx.spawn(async move |this, cx| {
            let url_for_fetch = url_owned.clone();
            let res = gpui_ce_tokio::Tokio::spawn(cx, async move {
                fetch_and_decode_artwork(&http, &cache_dir, &url_for_fetch).await
            })
            .await;

            let _ = this.update(cx, |app, cx| {
                match res {
                    Ok(Ok(art)) => {
                        let evicted = app.artwork_cache.insert(url_owned, art);
                        for old in evicted {
                            cx.drop_image(old.image, None);
                            cx.drop_image(old.mesh, None);
                        }
                    }
                    _ => {
                        app.artwork_cache.mark_failed(&url_owned);
                    }
                }
                cx.notify();
            });
        });
        // Keep bounded task handles so eviction doesn't leak handles.
        if self._art_tasks.len() >= 96 {
            self._art_tasks.clear();
        }
        self._art_tasks.push(task);
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
                    && let Some(ref art) = song.thumbnail_url
                {
                    let url = art.clone();
                    self.ensure_artwork_loaded(&url, cx);
                }
                self.playback = snap;
                self.playback_synced_at = Instant::now();
            }
            BackendEvent::QueueUpdated(q) => {
                self.queue = q;
            }
            BackendEvent::HomeLoading => {
                self.home = LoadState::Loading;
            }
            BackendEvent::HomeLoaded(res) => match res {
                Ok(feed) => {
                    // Warm thumbnails for the first visible shelf items.
                    let mut prefetch = Vec::new();
                    for shelf in feed.shelves.iter().take(3) {
                        for item in shelf.items.iter().take(6) {
                            if let Some(ref u) = item.thumbnail_url {
                                prefetch.push(u.clone());
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
                                .take(12)
                                .filter_map(|r| match r {
                                    SearchResult::TopTrack { song }
                                    | SearchResult::Track { song } => song.thumbnail_url.clone(),
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
            BackendEvent::DetailLoading => {
                self.detail = LoadState::Loading;
            }
            BackendEvent::DetailLoaded(res) => match res {
                Ok(payload) => {
                    if let Some(ref h) = payload.header
                        && let Some(ref u) = h.thumbnail_url
                    {
                        let url = u.clone();
                        self.ensure_artwork_loaded(&url, cx);
                    }
                    self.detail = LoadState::Ready(payload);
                }
                Err(err) => {
                    self.detail = LoadState::Error(err);
                }
            },
            BackendEvent::LyricsLoading => {
                self.lyrics = LoadState::Loading;
                self.lyrics_list_state = ListState::new(0, ListAlignment::Top, px(220.0));
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
                        self.lyrics_list_state =
                            ListState::new(count, ListAlignment::Top, px(220.0));
                        self.lyrics = LoadState::Ready(payload);
                    }
                    Err(err) => {
                        self.lyrics = LoadState::Error(err);
                        self.lyrics_list_state = ListState::new(0, ListAlignment::Top, px(220.0));
                    }
                }
            }
            BackendEvent::LibraryLoaded(lib) => {
                self.library = lib;
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
                self.status_notice = Some(msg);
                self.error_notice = None;
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
            .and_then(|s| s.thumbnail_url.as_deref())
            .and_then(|u| self.artwork_cache.get(u))
            .map(|a| a.mesh.clone())
            .unwrap_or_else(|| self.default_mesh.clone())
    }

    pub fn render_art_box(
        &self,
        url: Option<&str>,
        size_px: f32,
        radius_px: f32,
        fallback_glyph: &str,
        theme: BitChordTheme,
    ) -> impl IntoElement {
        if let Some(art) = url.and_then(|u| self.artwork_cache.get(u)) {
            div()
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
                )
        } else {
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
                .text_size(px((size_px * 0.34).clamp(11.0, 28.0)))
                .text_color(theme.text_muted)
                .child(fallback_glyph.to_owned())
        }
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
        if self.queue_open {
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

        // Drive smooth 60fps frames ONLY when Now Playing word-synced lyrics are visible & playing.
        if self.route == Route::NowPlaying && self.playback.state == PlaybackState::Playing {
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
            .child(self.render_titlebar(is_csd, cx))
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
                    .child(self.render_sidebar(cx))
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
                                                .px(px(8.0))
                                                .py(px(2.0))
                                                .rounded(px(4.0))
                                                .cursor_pointer()
                                                .text_color(theme.text_secondary)
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.error_notice = None;
                                                    cx.notify();
                                                }))
                                                .child("Dismiss"),
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
                                                .px(px(8.0))
                                                .py(px(2.0))
                                                .rounded(px(4.0))
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.status_notice = None;
                                                    cx.notify();
                                                }))
                                                .child("Close"),
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
                    .when(self.queue_open, |el| el.child(self.render_queue_drawer(cx))),
            )
            // Persistent bottom floating Now Playing bar (glass surface 1 of 2)
            .child(self.render_player_bar(cx))
    }
}

/// Formats milliseconds as `m:ss` (tabular alignment).
pub fn format_time_ms(ms: u64) -> String {
    let total_secs = ms / 1_000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}
