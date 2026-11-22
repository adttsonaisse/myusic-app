//! Persistent bottom Now Playing bar (`W4-1`) with selective `backdrop_blur`
//! (glass surface 1 of 2, `R-10`), live progress scrubber, transport controls,
//! lossless quality badge (from measured `NerdStats` only), and volume controls.

use bitchord_audio::PlaybackState;
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px, relative};

use crate::actions::Route;
use crate::app::{BitChordApp, format_time_ms};
use crate::backend::BackendCommand;

impl BitChordApp {
    pub fn render_player_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let song = self.playback.current_song.as_ref();
        let is_playing = self.playback.state == PlaybackState::Playing;
        let pos_ms = self.live_position_ms();
        let dur_ms = self.playback.duration_ms;
        let progress = if dur_ms > 0 {
            (pos_ms as f32 / dur_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let quality_label: Option<&'static str> =
            self.playback.nerd_stats.as_ref().and_then(|ns| {
                if ns.is_hi_res() {
                    Some("HI-RES")
                } else if ns.is_lossless() {
                    Some("LOSSLESS")
                } else {
                    None
                }
            });
        let vol_pct = if self.playback.muted {
            0
        } else {
            (self.playback.volume * 100.0).round() as u32
        };

        div()
            .w_full()
            .h(px(80.0))
            .flex_shrink_0()
            .bg(theme.glass_bar)
            .backdrop_blur(px(20.0))
            .border_t_1()
            .border_color(theme.border)
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.0))
            // Left: Artwork + Title/Artist (clicking opens full Now Playing stage)
            .child(
                div()
                    .id("bar-open-now-playing")
                    .w(px(280.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .rounded(px(8.0))
                    .p(px(4.0))
                    .hover(move |s| s.bg(theme.surface_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.route == Route::NowPlaying {
                            this.route = this.previous_route.clone();
                        } else {
                            this.navigate(Route::NowPlaying, cx);
                            return;
                        }
                        cx.notify();
                    }))
                    .child(self.render_art_box(
                        song.and_then(|s| s.thumbnail_url.as_deref()),
                        48.0,
                        6.0,
                        "BC",
                        theme,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text_primary)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(
                                                song.map(|s| s.title.clone()).unwrap_or_else(
                                                    || "Nothing playing".to_owned(),
                                                ),
                                            ),
                                    )
                                    .when_some(quality_label, |el, badge| {
                                        el.child(
                                            div()
                                                .px(px(5.0))
                                                .py(px(1.0))
                                                .rounded(px(4.0))
                                                .bg(theme.surface_elevated)
                                                .border_1()
                                                .border_color(theme.border)
                                                .text_size(px(9.5))
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(theme.text_secondary)
                                                .child(badge),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(song.map(|s| s.artist.clone()).unwrap_or_else(|| {
                                        "Pick a track from Home, Search, or Library".to_owned()
                                    })),
                            ),
                    ),
            )
            // Center: Transport buttons + interactive clickable progress scrubber
            .child(
                div()
                    .flex_1()
                    .max_w(px(520.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .id("bar-shuffle")
                                    .px(px(8.0))
                                    .h(px(28.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if self.queue.shuffle {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    })
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::ToggleShuffle, cx);
                                    }))
                                    .child("SHF"),
                            )
                            .child(
                                div()
                                    .id("bar-prev")
                                    .px(px(10.0))
                                    .h(px(30.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_primary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::PrevTrack, cx);
                                    }))
                                    .child("|<"),
                            )
                            .child(
                                div()
                                    .id("bar-play-pause")
                                    .w(px(38.0))
                                    .h(px(38.0))
                                    .rounded(px(19.0))
                                    .bg(theme.accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.on_accent)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::PlayPause, cx);
                                    }))
                                    .child(if is_playing { "||" } else { ">" }),
                            )
                            .child(
                                div()
                                    .id("bar-next")
                                    .px(px(10.0))
                                    .h(px(30.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_primary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::NextTrack, cx);
                                    }))
                                    .child(">|"),
                            )
                            .child(
                                div()
                                    .id("bar-repeat")
                                    .px(px(8.0))
                                    .h(px(28.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if self.queue.repeat_all || self.queue.repeat_one {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    })
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::CycleRepeat, cx);
                                    }))
                                    .child(if self.queue.repeat_one {
                                        "RPT 1"
                                    } else if self.queue.repeat_all {
                                        "RPT ALL"
                                    } else {
                                        "RPT"
                                    }),
                            ),
                    )
                    // Scrubber row
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .w(px(38.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(format_time_ms(pos_ms)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(14.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .children((0usize..20).map(|step| {
                                        let frac = (step as f32 + 0.5) / 20.0;
                                        let filled = frac <= progress;
                                        let seek_target =
                                            ((step as f64 / 20.0) * dur_ms as f64) as u64;
                                        div()
                                            .id(("bar-seek-seg", step))
                                            .flex_1()
                                            .h(px(4.0))
                                            .rounded(px(2.0))
                                            .cursor_pointer()
                                            .bg(if filled { theme.accent } else { theme.border })
                                            .hover(move |s| s.h(px(6.0)).bg(theme.accent))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if dur_ms > 0 {
                                                    this.send(
                                                        BackendCommand::SeekMs(seek_target),
                                                        cx,
                                                    );
                                                }
                                            }))
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(38.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(format_time_ms(dur_ms)),
                            ),
                    ),
            )
            // Right: Lyrics jump + Mute + Volume steps
            .child(
                div()
                    .w(px(240.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("bar-lyrics-btn")
                            .h(px(28.0))
                            .px(px(9.0))
                            .rounded(px(6.0))
                            .bg(if self.route == Route::NowPlaying {
                                theme.accent_soft
                            } else {
                                theme.surface_elevated
                            })
                            .border_1()
                            .border_color(if self.route == Route::NowPlaying {
                                theme.accent
                            } else {
                                theme.border
                            })
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if self.route == Route::NowPlaying {
                                theme.accent
                            } else {
                                theme.text_primary
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.route == Route::NowPlaying {
                                    this.route = this.previous_route.clone();
                                } else {
                                    this.navigate(Route::NowPlaying, cx);
                                    return;
                                }
                                cx.notify();
                            }))
                            .child("Lyrics"),
                    )
                    .child(
                        div()
                            .id("bar-mute")
                            .h(px(28.0))
                            .px(px(8.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(11.5))
                            .text_color(if self.playback.muted {
                                theme.danger
                            } else {
                                theme.text_secondary
                            })
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send(BackendCommand::ToggleMute, cx);
                            }))
                            .child(if self.playback.muted { "MUTE" } else { "VOL" }),
                    )
                    .child(
                        div()
                            .id("bar-vol-down")
                            .w(px(24.0))
                            .h(px(28.0))
                            .rounded(px(5.0))
                            .bg(theme.surface_elevated)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(theme.text_primary)
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = (this.playback.volume - 0.1).clamp(0.0, 1.0);
                                this.send(BackendCommand::SetVolume(v), cx);
                            }))
                            .child("-"),
                    )
                    .child(
                        div()
                            .w(px(44.0))
                            .h(px(4.0))
                            .rounded(px(2.0))
                            .bg(theme.border)
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(vol_pct as f32 / 100.0))
                                    .bg(theme.text_primary),
                            ),
                    )
                    .child(
                        div()
                            .id("bar-vol-up")
                            .w(px(24.0))
                            .h(px(28.0))
                            .rounded(px(5.0))
                            .bg(theme.surface_elevated)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(theme.text_primary)
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let v = (this.playback.volume + 0.1).clamp(0.0, 1.0);
                                this.send(BackendCommand::SetVolume(v), cx);
                            }))
                            .child("+"),
                    ),
            )
    }
}
