//! Persistent bottom Now Playing bar (`W4-1`) with selective `backdrop_blur`
//! (glass surface 1 of 2, `R-10`), Lucide SVG transport controls (`notes.md` #5),
//! Like toggle, single continuous interactive pill progress scrubber with hover
//! preview & duration fallback fix (`notes.md` #4 & §2 #4), side-by-side Lyrics
//! & Queue controls with distinct icons (`notes.md` §2 #3), lossless quality
//! badge (from measured `NerdStats` only), and continuous pill volume bar.

use bitchord_audio::PlaybackState;
use bitchord_model::Song;
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};

use crate::actions::Route;
use crate::app::{BitChordApp, format_time_ms};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;
use crate::pill_slider::{PillSliderStyle, pill_slider, progress_fraction};

/// Resolves the best known track duration in milliseconds, falling back from the
/// audio engine snapshot (`duration_ms`) to `current_song.duration_millis()`, then
/// matching entries in the active queue or local library (`notes.md` §2 #4).
pub fn resolve_effective_duration_ms(
    snapshot_duration_ms: u64,
    current_song: Option<&Song>,
    queue_items: &[Song],
    library_songs: &[Song],
) -> u64 {
    if snapshot_duration_ms > 0 {
        return snapshot_duration_ms;
    }
    let Some(song) = current_song else {
        return 0;
    };
    let song_ms = song.duration_millis();
    if song_ms > 0 {
        return song_ms;
    }
    if let Some(found) = queue_items
        .iter()
        .find(|s| s.video_id == song.video_id && s.duration_millis() > 0)
    {
        return found.duration_millis();
    }
    if let Some(found) = library_songs
        .iter()
        .find(|s| s.video_id == song.video_id && s.duration_millis() > 0)
    {
        return found.duration_millis();
    }
    0
}

/// Formats the right-side total duration label so unknown live/unparsed streams
/// show `--:--` instead of a misleading `0:00` while a track is loaded.
pub fn format_duration_label(duration_ms: u64, has_song: bool) -> String {
    if duration_ms > 0 {
        format_time_ms(duration_ms)
    } else if has_song {
        "--:--".to_owned()
    } else {
        "0:00".to_owned()
    }
}

impl BitChordApp {
    pub fn render_player_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let song = self.playback.current_song.as_ref();
        let art_url = song.and_then(song_thumbnail_url);
        let is_playing = self.playback.state == PlaybackState::Playing;
        let is_buffering = self.playback.state == PlaybackState::Buffering;
        let is_liked = self.is_current_song_liked();
        let pos_ms = self.live_position_ms();
        let dur_ms = self.effective_duration_ms();
        let progress = progress_fraction(pos_ms, dur_ms);
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
            .h(px(86.0))
            .flex_shrink_0()
            .bg(theme.glass_bar)
            .backdrop_blur(px(20.0))
            .border_t_1()
            .border_color(theme.border)
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(14.0))
            // Left: Artwork + Title/Artist (opens Now Playing) + Heart Like button
            .child(
                div()
                    .w(px(272.0))
                    .min_w(px(196.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("bar-open-now-playing")
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .items_center()
                            .gap(px(11.0))
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
                                                    .truncate()
                                                    .child(
                                                        song.map(|s| s.title.clone())
                                                            .unwrap_or_else(|| {
                                                                "Nothing playing".to_owned()
                                                            }),
                                                    ),
                                            )
                                            .when_some(quality_label, |el, badge| {
                                                el.child(
                                                    div()
                                                        .flex_shrink_0()
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
                                            .truncate()
                                            .child(if is_buffering {
                                                format!(
                                                    "Loading · {}",
                                                    song.map(|s| s.artist.as_str()).unwrap_or("")
                                                )
                                            } else {
                                                song.map(|s| s.artist.clone()).unwrap_or_else(
                                                    || "Pick a track to start listening".to_owned(),
                                                )
                                            }),
                                    ),
                            ),
                    )
                    .when_some(song.cloned(), |el, cur_song| {
                        el.child(
                            div()
                                .id("bar-like-btn")
                                .w(px(30.0))
                                .h(px(30.0))
                                .flex_shrink_0()
                                .rounded(px(8.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(move |s| s.bg(theme.surface_hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_like_current_song(cx);
                                }))
                                .child(icons::icon(
                                    if is_liked {
                                        icons::HEART_FILLED
                                    } else {
                                        icons::HEART
                                    },
                                    16.0,
                                    if is_liked {
                                        theme.accent
                                    } else {
                                        theme.text_muted
                                    },
                                )),
                        )
                        .child(
                            div()
                                .id("bar-add-pl-btn")
                                .w(px(28.0))
                                .h(px(28.0))
                                .flex_shrink_0()
                                .rounded(px(8.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(move |s| s.bg(theme.surface_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_playlist_picker(cur_song.clone(), cx);
                                }))
                                .child(icons::icon(icons::PLUS, 14.5, theme.text_muted)),
                        )
                    }),
            )
            // Center: Lucide transport buttons + single continuous pill progress scrubber
            .child(
                div()
                    .flex_1()
                    .min_w(px(240.0))
                    .max_w(px(540.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(5.0))
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
                    // Single continuous smooth pill progress scrubber with hover preview
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .child(
                                div()
                                    .w(px(38.0))
                                    .flex_shrink_0()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(format_time_ms(pos_ms)),
                            )
                            .child(div().flex_1().min_w(px(80.0)).child(pill_slider(
                                "bar-seek-pill",
                                progress,
                                PillSliderStyle {
                                    track_height_px: 5.5,
                                    hit_height_px: 18.0,
                                    track_color: theme.border,
                                    fill_color: theme.accent,
                                    hover_fill_color: theme.accent_soft,
                                    tooltip_bg: theme.surface_elevated,
                                    tooltip_text: theme.text_primary,
                                    preview_duration_ms: (dur_ms > 0).then_some(dur_ms),
                                },
                                self.bar_seek_slider.clone(),
                                cx.listener(|this, &frac: &f32, _w, cx| {
                                    this.seek_fraction(frac, false, cx);
                                }),
                                cx.listener(|this, &frac: &f32, _w, cx| {
                                    this.seek_fraction(frac, true, cx);
                                }),
                                cx.listener(|this, &frac: &f32, _w, cx| {
                                    this.seek_fraction(frac, false, cx);
                                }),
                            )))
                            .child(
                                div()
                                    .w(px(38.0))
                                    .flex_shrink_0()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_muted)
                                    .child(format_duration_label(dur_ms, song.is_some())),
                            ),
                    ),
            )
            // Right: Lyrics (mic) + Queue (list-music) side-by-side + Mute + continuous pill volume bar
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(7.0))
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
                                15.0,
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
                            .id("bar-queue-btn")
                            .h(px(32.0))
                            .px(px(10.0))
                            .rounded(px(8.0))
                            .bg(if self.queue_open {
                                theme.accent_soft
                            } else {
                                theme.surface_elevated
                            })
                            .border_1()
                            .border_color(if self.queue_open {
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
                            .text_color(if self.queue_open {
                                theme.accent
                            } else {
                                theme.text_primary
                            })
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queue_open = !this.queue_open;
                                cx.notify();
                            }))
                            .child(icons::icon(
                                icons::QUEUE_LIST,
                                15.0,
                                if self.queue_open {
                                    theme.accent
                                } else {
                                    theme.text_secondary
                                },
                            ))
                            .child("Queue")
                            .when(!self.queue.items.is_empty(), |el| {
                                el.child(
                                    div()
                                        .px(px(5.0))
                                        .py(px(1.0))
                                        .rounded_full()
                                        .bg(if self.queue_open {
                                            theme.accent
                                        } else {
                                            theme.surface
                                        })
                                        .text_size(px(10.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(if self.queue_open {
                                            theme.on_accent
                                        } else {
                                            theme.text_muted
                                        })
                                        .child(format!("{}", self.queue.items.len())),
                                )
                            }),
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
                    .child(div().w(px(76.0)).child(pill_slider(
                        "bar-vol-pill",
                        vol_frac,
                        PillSliderStyle {
                            track_height_px: 5.0,
                            hit_height_px: 18.0,
                            track_color: theme.border,
                            fill_color: theme.text_primary,
                            hover_fill_color: theme.surface_active,
                            tooltip_bg: theme.surface_elevated,
                            tooltip_text: theme.text_primary,
                            preview_duration_ms: None,
                        },
                        self.vol_slider.clone(),
                        cx.listener(|this, &frac: &f32, _w, cx| {
                            this.send(BackendCommand::SetVolume(frac), cx);
                        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_duration_falls_back_from_zero_snapshot_to_song_then_queue_then_library() {
        let mut song_with_dur = Song::new("vid1", "Nightcall", "Kavinsky", None);
        song_with_dur.duration_text = Some("4:18".to_owned());

        // 1. Non-zero snapshot wins.
        assert_eq!(
            resolve_effective_duration_ms(195_000, Some(&song_with_dur), &[], &[]),
            195_000
        );

        // 2. Zero snapshot falls back to current song duration_text (`4:18` = 258_000 ms).
        assert_eq!(
            resolve_effective_duration_ms(0, Some(&song_with_dur), &[], &[]),
            258_000
        );

        // 3. Song without duration falls back to matching queue item.
        let bare_song = Song::new("vid1", "Nightcall", "Kavinsky", None);
        assert_eq!(
            resolve_effective_duration_ms(0, Some(&bare_song), &[song_with_dur.clone()], &[]),
            258_000
        );

        // 4. Falls back to matching library item if not in queue.
        assert_eq!(
            resolve_effective_duration_ms(0, Some(&bare_song), &[], &[song_with_dur]),
            258_000
        );

        // 5. Unknown duration returns 0 and renders `--:--` when a song is loaded.
        assert_eq!(resolve_effective_duration_ms(0, Some(&bare_song), &[], &[]), 0);
        assert_eq!(format_duration_label(0, true), "--:--");
        assert_eq!(format_duration_label(0, false), "0:00");
        assert_eq!(format_duration_label(258_000, true), "4:18");
    }
}
