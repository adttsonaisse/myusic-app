//! Settings, 7-band SVF Equalizer, Crossfade, Sleep Timer, Stream Backend,
//! YouTube Auth (Secret Service), Spotify Importer, and Linux XDG paths (`W4-3`)
//! with crisp vector SVG section headers and clean interactive controls.

use bitchord_audio::PRESETS as SLEEP_PRESETS;
use bitchord_dsp_core::{EqLayout, EqualizerPreset};
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, div, px};

use crate::app::BitChordApp;
use crate::backend::{AppBackend, BackendCommand, StreamBackendChoice};
use crate::icons;
use crate::theme::{ActiveTheme, BitChordTheme};

const BAND_LABELS: [&str; EqLayout::MANUAL_COUNT] = [
    "60 Hz", "150 Hz", "400 Hz", "1 kHz", "2.4 kHz", "6 kHz", "14 kHz",
];

const PRESET_CHOICES: [(EqualizerPreset, &str); 10] = [
    (EqualizerPreset::Flat, "Flat"),
    (EqualizerPreset::Acoustic, "Acoustic"),
    (EqualizerPreset::BassBoost, "Bass Boost"),
    (EqualizerPreset::Vocal, "Vocal"),
    (EqualizerPreset::TrebleBoost, "Treble Boost"),
    (EqualizerPreset::Loudness, "Loudness"),
    (EqualizerPreset::Electronic, "Electronic"),
    (EqualizerPreset::Rock, "Rock"),
    (EqualizerPreset::Jazz, "Jazz"),
    (EqualizerPreset::LateNight, "Late Night"),
];

impl BitChordApp {
    pub fn render_settings(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let paths = cx.global::<AppBackend>().paths.clone();
        let crossfade = self.playback.crossfade_seconds;
        let sleep_rem = self.playback.sleep_timer_remaining_ms;
        let sleep_after = self.playback.sleep_after_track;

        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .px(px(28.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .max_w(px(860.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(icons::icon(icons::SETTINGS, 22.0, theme.accent))
                    .child(
                        div()
                            .text_size(px(24.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.text_primary)
                            .child("Settings & Audio DSP"),
                    ),
            )
            // 1. Appearance & Desktop Integration
            .child(
                section_card("Appearance & Linux Desktop", icons::SPARKLES, theme).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(10.0))
                        .child(toggle_chip(
                            "set-theme",
                            format!("Theme: {}", self.theme_mode.label()),
                            false,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.theme_mode = this.theme_mode.toggled();
                                cx.set_global(ActiveTheme(this.theme_mode));
                                cx.notify();
                            }),
                        ))
                        .child(toggle_chip(
                            "set-close-tray",
                            if self.close_to_tray {
                                "Close to System Tray: On"
                            } else {
                                "Close to System Tray: Off"
                            },
                            self.close_to_tray,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.close_to_tray = !this.close_to_tray;
                                cx.notify();
                            }),
                        ))
                        .child(toggle_chip(
                            "set-discord-rpc",
                            if self.discord_rpc_enabled {
                                "Discord IPC Presence: On"
                            } else {
                                "Discord IPC Presence: Off"
                            },
                            self.discord_rpc_enabled,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.discord_rpc_enabled = !this.discord_rpc_enabled;
                                this.send(
                                    BackendCommand::SetDiscordRpc(this.discord_rpc_enabled),
                                    cx,
                                );
                                cx.notify();
                            }),
                        )),
                ),
            )
            // 2. Playback & Crossfade + Stream Backend (ADR-004)
            .child(
                section_card("Playback, Crossfade & Stream Backend", icons::DISC, theme)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.text_secondary)
                                    .child(format!(
                                        "Equal-power crossfade: {}",
                                        if crossfade == 0 {
                                            "Off (gapless)".to_owned()
                                        } else {
                                            format!("{crossfade} s")
                                        }
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(toggle_chip(
                                        "set-cf-minus",
                                        "- 1s",
                                        false,
                                        theme,
                                        cx.listener(move |this, _, _, cx| {
                                            this.send(
                                                BackendCommand::SetCrossfadeSeconds(
                                                    crossfade.saturating_sub(1),
                                                ),
                                                cx,
                                            );
                                        }),
                                    ))
                                    .child(toggle_chip(
                                        "set-cf-plus",
                                        "+ 1s",
                                        false,
                                        theme,
                                        cx.listener(move |this, _, _, cx| {
                                            this.send(
                                                BackendCommand::SetCrossfadeSeconds(
                                                    (crossfade + 1).min(12),
                                                ),
                                                cx,
                                            );
                                        }),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(toggle_chip(
                                "set-backend-auto",
                                StreamBackendChoice::Auto.label(),
                                self.stream_backend == StreamBackendChoice::Auto,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.stream_backend = StreamBackendChoice::Auto;
                                    this.send(
                                        BackendCommand::SetStreamBackend(StreamBackendChoice::Auto),
                                        cx,
                                    );
                                    cx.notify();
                                }),
                            ))
                            .child(toggle_chip(
                                "set-backend-native",
                                StreamBackendChoice::NativeOnly.label(),
                                self.stream_backend == StreamBackendChoice::NativeOnly,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.stream_backend = StreamBackendChoice::NativeOnly;
                                    this.send(
                                        BackendCommand::SetStreamBackend(
                                            StreamBackendChoice::NativeOnly,
                                        ),
                                        cx,
                                    );
                                    cx.notify();
                                }),
                            ))
                            .when(cfg!(feature = "ytdlp-bridge"), |el| {
                                #[cfg(feature = "ytdlp-bridge")]
                                {
                                    el.child(toggle_chip(
                                        "set-backend-ytdlp",
                                        StreamBackendChoice::YtDlpOnly.label(),
                                        self.stream_backend == StreamBackendChoice::YtDlpOnly,
                                        theme,
                                        cx.listener(|this, _, _, cx| {
                                            this.stream_backend = StreamBackendChoice::YtDlpOnly;
                                            this.send(
                                                BackendCommand::SetStreamBackend(
                                                    StreamBackendChoice::YtDlpOnly,
                                                ),
                                                cx,
                                            );
                                            cx.notify();
                                        }),
                                    ))
                                }
                                #[cfg(not(feature = "ytdlp-bridge"))]
                                {
                                    el
                                }
                            }),
                    ),
            )
            // 3. 7-Band SVF Equalizer
            .child(
                section_card("Equalizer (10-Slot Float32 SVF)", icons::SLIDERS, theme)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(toggle_chip(
                                "eq-enable-toggle",
                                if self.eq_enabled {
                                    "Equalizer: Enabled"
                                } else {
                                    "Equalizer: Bypass (Bit-Exact)"
                                },
                                self.eq_enabled,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.eq_enabled = !this.eq_enabled;
                                    this.send(
                                        BackendCommand::SetEq {
                                            enabled: this.eq_enabled,
                                            preset: this.eq_preset,
                                            bands_db: this.eq_bands_db,
                                        },
                                        cx,
                                    );
                                    cx.notify();
                                }),
                            ))
                            .children(PRESET_CHOICES.into_iter().enumerate().map(
                                |(idx, (preset, label))| {
                                    let active = self.eq_enabled && self.eq_preset == preset;
                                    toggle_chip(
                                        ("eq-preset", idx),
                                        label,
                                        active,
                                        theme,
                                        cx.listener(move |this, _, _, cx| {
                                            this.eq_enabled = preset != EqualizerPreset::Flat;
                                            this.eq_preset = preset;
                                            let src = preset.bands_db();
                                            if src.len() == EqLayout::MANUAL_COUNT {
                                                this.eq_bands_db.copy_from_slice(src);
                                            }
                                            this.send(
                                                BackendCommand::SetEq {
                                                    enabled: this.eq_enabled,
                                                    preset,
                                                    bands_db: this.eq_bands_db,
                                                },
                                                cx,
                                            );
                                            cx.notify();
                                        }),
                                    )
                                },
                            )),
                    )
                    // 7 interactive band dB steppers
                    .child(div().pt(px(6.0)).flex().flex_wrap().gap(px(8.0)).children(
                        (0..EqLayout::MANUAL_COUNT).map(|band| {
                            let db = self.eq_bands_db[band];
                            div()
                                .w(px(104.0))
                                .p(px(9.0))
                                .rounded(px(10.0))
                                .bg(theme.surface_elevated)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme.text_muted)
                                        .child(BAND_LABELS[band]),
                                )
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text_primary)
                                        .child(format!("{:+.1} dB", db)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .id(("eq-band-dn", band))
                                                .w(px(26.0))
                                                .h(px(24.0))
                                                .rounded(px(6.0))
                                                .bg(theme.surface)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .text_color(theme.text_primary)
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.eq_enabled = true;
                                                    this.eq_preset = EqualizerPreset::Custom;
                                                    this.eq_bands_db[band] =
                                                        (this.eq_bands_db[band] - 1.0)
                                                            .clamp(-12.0, 12.0);
                                                    this.send(
                                                        BackendCommand::SetEq {
                                                            enabled: true,
                                                            preset: EqualizerPreset::Custom,
                                                            bands_db: this.eq_bands_db,
                                                        },
                                                        cx,
                                                    );
                                                    cx.notify();
                                                }))
                                                .child(icons::icon(
                                                    icons::WIN_MIN,
                                                    12.0,
                                                    theme.text_primary,
                                                )),
                                        )
                                        .child(
                                            div()
                                                .id(("eq-band-up", band))
                                                .w(px(26.0))
                                                .h(px(24.0))
                                                .rounded(px(6.0))
                                                .bg(theme.surface)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .text_color(theme.text_primary)
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.eq_enabled = true;
                                                    this.eq_preset = EqualizerPreset::Custom;
                                                    this.eq_bands_db[band] =
                                                        (this.eq_bands_db[band] + 1.0)
                                                            .clamp(-12.0, 12.0);
                                                    this.send(
                                                        BackendCommand::SetEq {
                                                            enabled: true,
                                                            preset: EqualizerPreset::Custom,
                                                            bands_db: this.eq_bands_db,
                                                        },
                                                        cx,
                                                    );
                                                    cx.notify();
                                                }))
                                                .child(icons::icon(
                                                    icons::PLUS,
                                                    12.0,
                                                    theme.text_primary,
                                                )),
                                        ),
                                )
                        }),
                    )),
            )
            // 4. Monotonic Sleep Timer
            .child(
                section_card("Sleep Timer", icons::TIMER, theme).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.0))
                        .children(SLEEP_PRESETS.into_iter().map(|mins| {
                            toggle_chip(
                                ("sleep-preset", mins as usize),
                                format!("{mins} min"),
                                false,
                                theme,
                                cx.listener(move |this, _, _, cx| {
                                    this.send(BackendCommand::StartSleepTimerMinutes(mins), cx);
                                }),
                            )
                        }))
                        .child(toggle_chip(
                            "sleep-after-track",
                            "After Current Track",
                            sleep_after,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.send(BackendCommand::StartSleepAfterTrack, cx);
                            }),
                        ))
                        .when(sleep_rem.is_some() || sleep_after, |el| {
                            el.child(toggle_chip(
                                "sleep-cancel",
                                "Cancel Timer",
                                true,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.send(BackendCommand::CancelSleepTimer, cx);
                                }),
                            ))
                        }),
                ),
            )
            // 5. YouTube Music Auth (Linux Secret Service) + Spotify Playlist Import
            .child(
                section_card("YouTube Music Auth & Spotify Import", icons::INFO, theme)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().flex_1().child(self.cookie_input.clone()))
                            .child(toggle_chip(
                                "auth-save-cookie",
                                "Save Cookie",
                                true,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    let val = this.cookie_input.read(cx).text().to_owned();
                                    this.send(BackendCommand::SaveYoutubeCookie(val), cx);
                                    this.cookie_input.update(cx, |inp, cx| inp.set_text("", cx));
                                }),
                            ))
                            .child(toggle_chip(
                                "auth-clear-cookie",
                                "Sign Out",
                                false,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.send(BackendCommand::ClearYoutubeCookie, cx);
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().flex_1().child(self.spotify_input.clone()))
                            .child(toggle_chip(
                                "spotify-import-btn",
                                "Import Spotify Playlist",
                                false,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    let url = this.spotify_input.read(cx).text().to_owned();
                                    this.import_spotify(url, cx);
                                }),
                            )),
                    ),
            )
            // 6. XDG Linux Storage Paths
            .child(
                section_card("Linux XDG Directories", icons::FOLDER, theme).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(format!("Config: {}", paths.config_dir.display()))
                        .child(format!("Database: {}", paths.db_file().display()))
                        .child(format!("Cache: {}", paths.cache_dir.display()))
                        .child(format!(
                            "Downloads: {}",
                            paths.downloads_exported_dir.display()
                        )),
                ),
            )
    }
}

fn section_card(title: &'static str, icon_bytes: &'static [u8], theme: BitChordTheme) -> gpui::Div {
    div()
        .p(px(16.0))
        .rounded(px(12.0))
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(icons::icon(icon_bytes, 16.0, theme.accent))
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_primary)
                        .child(title),
                ),
        )
}

fn toggle_chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    active: bool,
    theme: BitChordTheme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(32.0))
        .px(px(12.0))
        .rounded(px(8.0))
        .bg(if active {
            theme.accent_soft
        } else {
            theme.surface_elevated
        })
        .border_1()
        .border_color(if active { theme.accent } else { theme.border })
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(px(12.5))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            theme.accent
        } else {
            theme.text_primary
        })
        .hover(move |s| s.bg(theme.surface_hover))
        .on_click(on_click)
        .child(label.into())
}
