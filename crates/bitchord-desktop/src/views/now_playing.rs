//! Full-bleed Now Playing stage (`W4-3`) over the 64x64 BGRA dynamic artwork
//! mesh backdrop, combining 512px Hero cover art, Like toggle, single continuous
//! pill scrubber with hover preview (`notes.md` #4 & §2 #4), Lucide SVG controls
//! (`notes.md` #5), collapsible measured `NerdStats`, and the dedicated 60fps
//! smooth auto-scrolling [`crate::views::lyrics_view::LyricsView`] (`notes.md` #3 & §2 #1).

use bitchord_audio::PlaybackState;
use gpui::prelude::*;
use gpui::{Context, FontWeight, ObjectFit, div, img, px};

use crate::app::{BitChordApp, format_time_ms};
use crate::artwork::{ArtworkTier, song_thumbnail_url};
use crate::backend::BackendCommand;
use crate::icons;
use crate::pill_slider::{PillSliderStyle, pill_slider, progress_fraction};
use crate::theme::rgba_hex;
use crate::views::player_bar::format_duration_label;

const SPEEDS: [f32; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

impl BitChordApp {
    pub fn render_now_playing(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let mesh = self.active_mesh_image();
        let song = self.playback.current_song.as_ref();
        let art_url = song.and_then(song_thumbnail_url);
        let is_playing = self.playback.state == PlaybackState::Playing;
        let is_buffering = self.playback.state == PlaybackState::Buffering;
        let is_liked = self.is_current_song_liked();
        let pos_ms = self.live_position_ms();
        let dur_ms = self.effective_duration_ms();
        let progress = progress_fraction(pos_ms, dur_ms);

        // Text on the darkened Now Playing mesh backdrop stays crisp white (>7:1 contrast).
        let ink_primary = rgba_hex(0xFFFF_FFFF);
        let ink_secondary = rgba_hex(0xFFFF_FFB8);
        let ink_dim = rgba_hex(0xFFFF_FF78);
        let surface_chip = rgba_hex(0xFFFF_FF18);
        let surface_chip_hover = rgba_hex(0xFFFF_FF29);
        let chip_border = rgba_hex(0xFFFF_FF24);

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            // Layer 0: 64x64 bilinear-upscaled dynamic artwork mesh backdrop
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(img(mesh).size_full().object_fit(ObjectFit::Fill)),
            )
            // Layer 1: 2-column desktop stage (Artwork + Controls | Smooth Auto-Scrolling Lyrics)
            .child(
                div()
                    .relative()
                    .size_full()
                    .px(px(36.0))
                    .py(px(28.0))
                    .flex()
                    .flex_row()
                    .gap(px(44.0))
                    .overflow_hidden()
                    // Left column: Hero 512px album art + metadata + continuous pill scrubber + DSP controls
                    .child(
                        div()
                            .id("np-left-scroll")
                            .w(px(364.0))
                            .flex_shrink_0()
                            .h_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(16.0))
                            // Back to browsing button
                            .child(
                                div()
                                    .id("np-back-btn")
                                    .w(px(142.0))
                                    .h(px(32.0))
                                    .px(px(11.0))
                                    .rounded(px(8.0))
                                    .bg(surface_chip)
                                    .border_1()
                                    .border_color(chip_border)
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ink_primary)
                                    .hover(move |s| s.bg(surface_chip_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.route = this.previous_route.clone();
                                        cx.notify();
                                    }))
                                    .child(icons::icon(icons::CHEVRON_LEFT, 15.0, ink_primary))
                                    .child("Back to Browse"),
                            )
                            // 512px Hero artwork
                            .child(self.render_art_box_with_tier(
                                art_url.as_deref(),
                                320.0,
                                16.0,
                                ArtworkTier::Hero,
                                icons::DISC,
                                theme,
                            ))
                            // Title + Artist + Album + Like button
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .items_start()
                                    .justify_between()
                                    .gap(px(10.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .flex()
                                            .flex_col()
                                            .gap(px(4.0))
                                            .child(
                                                div()
                                                    .text_size(px(21.0))
                                                    .font_weight(FontWeight::BOLD)
                                                    .text_color(ink_primary)
                                                    .child(
                                                        song.map(|s| s.title.clone())
                                                            .unwrap_or_else(|| {
                                                                "No track loaded".to_owned()
                                                            }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(14.5))
                                                    .text_color(ink_secondary)
                                                    .child(if is_buffering {
                                                        format!(
                                                            "Loading · {}",
                                                            song.map(|s| s.artist.as_str())
                                                                .unwrap_or("")
                                                        )
                                                    } else {
                                                        song.map(|s| s.artist.clone())
                                                            .unwrap_or_else(|| {
                                                                "Select any song from Home, Search, or Library"
                                                                    .to_owned()
                                                            })
                                                    }),
                                            )
                                            .when_some(
                                                song.and_then(|s| s.album_name.clone()),
                                                |el, album| {
                                                    el.child(
                                                        div()
                                                            .text_size(px(12.5))
                                                            .text_color(ink_dim)
                                                            .child(album),
                                                    )
                                                },
                                            ),
                                    )
                                    .when(song.is_some(), |el| {
                                        el.child(
                                            div()
                                                .id("np-like-btn")
                                                .w(px(36.0))
                                                .h(px(36.0))
                                                .rounded_full()
                                                .bg(if is_liked {
                                                    rgba_hex(0xFFFF_FF22)
                                                } else {
                                                    surface_chip
                                                })
                                                .border_1()
                                                .border_color(if is_liked {
                                                    theme.accent
                                                } else {
                                                    chip_border
                                                })
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(surface_chip_hover))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_like_current_song(cx);
                                                }))
                                                .child(icons::icon(
                                                    if is_liked {
                                                        icons::HEART_FILLED
                                                    } else {
                                                        icons::HEART
                                                    },
                                                    17.0,
                                                    if is_liked {
                                                        theme.accent
                                                    } else {
                                                        ink_primary
                                                    },
                                                )),
                                        )
                                    }),
                            )
                            // Single continuous smooth pill scrubber with hover preview (`notes.md` #4 & §2 #4)
                            .child(
                                div()
                                    .w(px(320.0))
                                    .pt(px(8.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(pill_slider(
                                        "np-seek-pill",
                                        progress,
                                        PillSliderStyle {
                                            track_height_px: 6.5,
                                            hit_height_px: 20.0,
                                            track_color: rgba_hex(0xFFFF_FF30),
                                            fill_color: ink_primary,
                                            hover_fill_color: rgba_hex(0xFFFF_FF58),
                                            tooltip_bg: rgba_hex(0x1414_18E8),
                                            tooltip_text: ink_primary,
                                            preview_duration_ms: (dur_ms > 0).then_some(dur_ms),
                                        },
                                        self.np_seek_slider.clone(),
                                        cx.listener(|this, &frac: &f32, _w, cx| {
                                            this.seek_fraction(frac, false, cx);
                                        }),
                                        cx.listener(|this, &frac: &f32, _w, cx| {
                                            this.seek_fraction(frac, true, cx);
                                        }),
                                        cx.listener(|this, &frac: &f32, _w, cx| {
                                            this.seek_fraction(frac, false, cx);
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .text_size(px(11.5))
                                            .text_color(ink_secondary)
                                            .child(format_time_ms(pos_ms))
                                            .child(format_duration_label(dur_ms, song.is_some())),
                                    ),
                            )
                            // Lucide vector icon transport row
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .id("np-shuffle")
                                            .w(px(38.0))
                                            .h(px(38.0))
                                            .rounded(px(10.0))
                                            .bg(if self.queue.shuffle {
                                                surface_chip_hover
                                            } else {
                                                gpui::transparent_black()
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::ToggleShuffle, cx);
                                            }))
                                            .child(icons::icon(
                                                icons::SHUFFLE,
                                                17.0,
                                                if self.queue.shuffle {
                                                    ink_primary
                                                } else {
                                                    ink_dim
                                                },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("np-prev")
                                            .w(px(44.0))
                                            .h(px(44.0))
                                            .rounded(px(12.0))
                                            .bg(surface_chip)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::PrevTrack, cx);
                                            }))
                                            .child(icons::icon(icons::SKIP_PREV, 20.0, ink_primary)),
                                    )
                                    .child(
                                        div()
                                            .id("np-play-pause")
                                            .w(px(54.0))
                                            .h(px(54.0))
                                            .rounded_full()
                                            .bg(ink_primary)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::PlayPause, cx);
                                            }))
                                            .child(icons::icon(
                                                if is_playing {
                                                    icons::PAUSE
                                                } else {
                                                    icons::PLAY
                                                },
                                                22.0,
                                                rgba_hex(0x0D0D_0FFF),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("np-next")
                                            .w(px(44.0))
                                            .h(px(44.0))
                                            .rounded(px(12.0))
                                            .bg(surface_chip)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::NextTrack, cx);
                                            }))
                                            .child(icons::icon(icons::SKIP_NEXT, 20.0, ink_primary)),
                                    )
                                    .child(
                                        div()
                                            .id("np-repeat")
                                            .w(px(38.0))
                                            .h(px(38.0))
                                            .rounded(px(10.0))
                                            .bg(if self.queue.repeat_all || self.queue.repeat_one {
                                                surface_chip_hover
                                            } else {
                                                gpui::transparent_black()
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::CycleRepeat, cx);
                                            }))
                                            .child(icons::icon(
                                                if self.queue.repeat_one {
                                                    icons::REPEAT_ONE
                                                } else {
                                                    icons::REPEAT
                                                },
                                                17.0,
                                                if self.queue.repeat_all || self.queue.repeat_one {
                                                    ink_primary
                                                } else {
                                                    ink_dim
                                                },
                                            )),
                                    ),
                            )
                            // Quick DSP chips: Speed, Skip Silence, Spatial, Stats for Nerds
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .id("np-speed")
                                            .px(px(10.0))
                                            .h(px(30.0))
                                            .rounded(px(8.0))
                                            .bg(surface_chip)
                                            .border_1()
                                            .border_color(chip_border)
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let cur = this.playback.speed;
                                                let next = SPEEDS
                                                    .iter()
                                                    .copied()
                                                    .find(|&s| s > cur + 0.05)
                                                    .unwrap_or(SPEEDS[0]);
                                                this.send(BackendCommand::SetSpeed(next), cx);
                                            }))
                                            .child(icons::icon(icons::TIMER, 13.0, ink_secondary))
                                            .child(format!("{:.2}x", self.playback.speed)),
                                    )
                                    .child(
                                        div()
                                            .id("np-skip-silence")
                                            .px(px(10.0))
                                            .h(px(30.0))
                                            .rounded(px(8.0))
                                            .bg(if self.playback.skip_silence {
                                                rgba_hex(0xFFFF_FF30)
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.playback.skip_silence {
                                                ink_primary
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(
                                                    BackendCommand::SetSkipSilence(
                                                        !this.playback.skip_silence,
                                                    ),
                                                    cx,
                                                );
                                            }))
                                            .child("Skip Silence"),
                                    )
                                    .child(
                                        div()
                                            .id("np-spatial")
                                            .px(px(10.0))
                                            .h(px(30.0))
                                            .rounded(px(8.0))
                                            .bg(if self.playback.spatial_audio {
                                                rgba_hex(0xFFFF_FF30)
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.playback.spatial_audio {
                                                ink_primary
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(
                                                    BackendCommand::SetSpatial(
                                                        !this.playback.spatial_audio,
                                                    ),
                                                    cx,
                                                );
                                            }))
                                            .child(icons::icon(
                                                icons::SPARKLES,
                                                13.0,
                                                ink_secondary,
                                            ))
                                            .child("Spatial"),
                                    )
                                    .child(
                                        div()
                                            .id("np-nerd-toggle")
                                            .px(px(10.0))
                                            .h(px(30.0))
                                            .rounded(px(8.0))
                                            .bg(if self.nerd_stats_open {
                                                rgba_hex(0xFFFF_FF30)
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.nerd_stats_open {
                                                ink_primary
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.nerd_stats_open = !this.nerd_stats_open;
                                                cx.notify();
                                            }))
                                            .child(icons::icon(icons::INFO, 13.0, ink_secondary))
                                            .child("Stats"),
                                    ),
                            )
                            // Collapsible measured NerdStats panel
                            .when(self.nerd_stats_open, |el| {
                                let ns = self.playback.nerd_stats.as_ref();
                                el.child(
                                    div()
                                        .w(px(320.0))
                                        .p(px(12.0))
                                        .rounded(px(10.0))
                                        .bg(rgba_hex(0x0000_0078))
                                        .border_1()
                                        .border_color(chip_border)
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.0))
                                        .text_size(px(11.5))
                                        .text_color(ink_secondary)
                                        .child(format!(
                                            "Codec: {}",
                                            ns.and_then(|n| n.mime_type.as_deref()).unwrap_or("-")
                                        ))
                                        .child(format!(
                                            "Sample rate: {} · Bit depth: {}",
                                            ns.and_then(|n| n.sample_rate_hz)
                                                .map(|r| {
                                                    bitchord_integrations::megahertz_label(r)
                                                        .unwrap_or_else(|| format!("{r} Hz"))
                                                })
                                                .unwrap_or_else(|| "-".to_owned()),
                                            ns.and_then(|n| n.bit_depth)
                                                .map(|b| format!("{b}-bit"))
                                                .unwrap_or_else(|| "lossy".to_owned()),
                                        ))
                                        .child(format!(
                                            "Bitrate: {} kbps · Source: {}",
                                            ns.and_then(|n| n.bitrate_kbps)
                                                .map(|k| k.to_string())
                                                .unwrap_or_else(|| "-".to_owned()),
                                            ns.and_then(|n| n.source_name.as_deref())
                                                .unwrap_or("-"),
                                        ))
                                        .child(format!(
                                            "Pipeline: {} ({} Hz {}) · {}Underruns: {} · Skipped: {}ms",
                                            self.playback.output_device_label,
                                            self.playback.output_sample_rate_hz,
                                            self.playback.output_format_label,
                                            if self.playback.bit_perfect_active {
                                                "BIT-PERFECT · "
                                            } else {
                                                ""
                                            },
                                            self.playback.underruns,
                                            self.playback.skipped_silence_ms,
                                        ))
                                        .when_some(
                                            self.playback.automix_plan_summary.clone(),
                                            |el, plan_line| {
                                                el.child(format!("Automix: {plan_line}"))
                                            },
                                        ),
                                )
                            }),
                    )
                    // Right column: Dedicated 60fps smooth auto-scrolling LyricsView entity
                    .child(self.lyrics_view.clone()),
            )
    }
}
