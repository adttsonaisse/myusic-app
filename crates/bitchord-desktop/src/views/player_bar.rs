//! Persistent bottom Now Playing bar (`W4-1`) with selective `backdrop_blur`
//! (glass surface 1 of 2, `R-10`), crisp vector SVG transport controls, single
//! continuous interactive pill progress scrubber (`notes.md` #4), lossless
//! quality badge (from measured `NerdStats` only), and continuous pill volume bar.

use bitchord_audio::PlaybackState;
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};

use crate::actions::Route;
use crate::app::{BitChordApp, format_time_ms};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;
use crate::pill_slider::pill_slider;

impl BitChordApp {
    pub fn render_player_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let song = self.playback.current_song.as_ref();
        let art_url = song.and_then(song_thumbnail_url);
        let is_playing = self.playback.state == PlaybackState::Playing;
        let is_buffering = self.playback.state == PlaybackState::Buffering;
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
        let vol_frac = if self.playback.muted {
            0.0
        } else {
            self.playback.volume.clamp(0.0, 1.0)
        };
        let vol_icon = if self.playback.muted || vol_frac <= 0.01 {
            icons::VOLUME_MUTE
        } else if vol_frac < 0.5 {
            icons::VOLUME_LOW
        } else {
            icons::VOLUME_HIGH
        };

        div()
            .w_full()
            .h(px(84.0))
            .flex_shrink_0()
            .bg(theme.glass_bar)
            .backdrop_blur(px(20.0))
            .border_t_1()
            .border_color(theme.border)
            .px(px(18.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(18.0))
            // Left: Artwork + Title/Artist (clicking opens full Now Playing stage)
            .child(
                div()
                    .id("bar-open-now-playing")
                    .w(px(286.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .rounded(px(10.0))
                    .p(px(5.0))
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
                        art_url.as_deref(),
                        50.0,
                        8.0,
                        icons::MUSIC_NOTE,
                        theme,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
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
                                    .text_color(if is_buffering {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    })
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(if is_buffering {
                                        format!(
                                            "Loading · {}",
                                            song.map(|s| s.artist.as_str()).unwrap_or("")
                                        )
                                    } else {
                                        song.map(|s| s.artist.clone()).unwrap_or_else(|| {
                                            "Pick a track from Home, Search, or Library".to_owned()
                                        })
                                    }),
                            ),
                    ),
            )
            // Center: Vector transport buttons + single continuous interactive pill scrubber
            .child(
                div()
                    .flex_1()
                    .max_w(px(540.0))
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
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .rounded(px(8.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::ToggleShuffle, cx);
                                    }))
                                    .child(icons::icon(
                                        icons::SHUFFLE,
                                        15.5,
                                        if self.queue.shuffle {
                                            theme.accent
                                        } else {
                                            theme.text_muted
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .id("bar-prev")
                                    .w(px(34.0))
                                    .h(px(34.0))
                                    .rounded(px(8.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::PrevTrack, cx);
                                    }))
                                    .child(icons::icon(icons::SKIP_PREV, 17.0, theme.text_primary)),
                            )
                            .child(
                                div()
                                    .id("bar-play-pause")
                                    .w(px(38.0))
                                    .h(px(38.0))
                                    .rounded_full()
                                    .bg(theme.accent)
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
                                        17.0,
                                        theme.on_accent,
                                    )),
                            )
                            .child(
                                div()
                                    .id("bar-next")
                                    .w(px(34.0))
                                    .h(px(34.0))
                                    .rounded(px(8.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::NextTrack, cx);
                                    }))
                                    .child(icons::icon(icons::SKIP_NEXT, 17.0, theme.text_primary)),
                            )
                            .child(
                                div()
                                    .id("bar-repeat")
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .rounded(px(8.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::CycleRepeat, cx);
                                    }))
                                    .child(icons::icon(
                                        if self.queue.repeat_one {
                                            icons::REPEAT_ONE
                                        } else {
                                            icons::REPEAT
                                        },
                                        15.5,
                                        if self.queue.repeat_all || self.queue.repeat_one {
                                            theme.accent
                                        } else {
                                            theme.text_muted
                                        },
                                    )),
                            ),
                    )
                    // Single continuous smooth pill progress scrubber (`notes.md` #4)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .w(px(38.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(format_time_ms(pos_ms)),
                            )
                            .child(div().flex_1().child(pill_slider(
                                "bar-seek-pill",
                                progress,
                                5.5,
                                18.0,
                                theme.border,
                                theme.accent,
                                self.bar_seek_bounds.clone(),
                                cx.listener(|this, &frac: &f32, _w, cx| {
                                    this.seek_fraction(frac, false, cx);
                                }),
                                cx.listener(|this, &frac: &f32, _w, cx| {
                                    this.seek_fraction(frac, true, cx);
                                }),
                            )))
                            .child(
                                div()
                                    .w(px(38.0))
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(format_time_ms(dur_ms)),
                            ),
                    ),
            )
            // Right: Lyrics jump + Mute icon + continuous pill volume bar
            .child(
                div()
                    .w(px(236.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(10.0))
                    .child(
                        div()
                            .id("bar-lyrics-btn")
                            .h(px(32.0))
                            .px(px(10.0))
                            .rounded(px(8.0))
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
                            .gap(px(6.0))
                            .cursor_pointer()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if self.route == Route::NowPlaying {
                                theme.accent
                            } else {
                                theme.text_primary
                            })
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
                            .child(icons::icon(
                                icons::LYRICS,
                                14.5,
                                if self.route == Route::NowPlaying {
                                    theme.accent
                                } else {
                                    theme.text_secondary
                                },
                            ))
                            .child("Lyrics"),
                    )
                    .child(
                        div()
                            .id("bar-mute")
                            .w(px(32.0))
                            .h(px(32.0))
                            .rounded(px(8.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send(BackendCommand::ToggleMute, cx);
                            }))
                            .child(icons::icon(
                                vol_icon,
                                16.5,
                                if self.playback.muted {
                                    theme.danger
                                } else {
                                    theme.text_secondary
                                },
                            )),
                    )
                    .child(div().w(px(86.0)).child(pill_slider(
                        "bar-vol-pill",
                        vol_frac,
                        5.0,
                        18.0,
                        theme.border,
                        theme.text_primary,
                        self.vol_slider_bounds.clone(),
                        cx.listener(|this, &frac: &f32, _w, cx| {
                            this.send(BackendCommand::SetVolume(frac), cx);
                        }),
                        cx.listener(|this, &frac: &f32, _w, cx| {
                            this.send(BackendCommand::SetVolume(frac), cx);
                        }),
                    ))),
            )
    }
}
