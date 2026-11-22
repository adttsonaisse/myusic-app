//! Full-bleed Now Playing stage (`W4-3`) over the 64x64 BGRA dynamic artwork
//! mesh backdrop, combining large cover art, continuous pill scrubber (`notes.md` #4),
//! vector icon transport/DSP controls (`notes.md` #5), collapsible measured
//! `NerdStats` telemetry, and smooth auto-scrolling pure-white lyrics (`notes.md` #3).

use std::time::{Duration, Instant};

use bitchord_audio::PlaybackState;
use bitchord_lyrics::LyricLine;
use gpui::prelude::*;
use gpui::{Context, FontWeight, ObjectFit, Rgba, div, img, list, px, rgba};

use crate::app::{BitChordApp, LoadState, format_time_ms};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;
use crate::pill_slider::pill_slider;
use crate::theme::rgba_hex;

const SPEEDS: [f32; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

#[inline]
fn white_alpha(alpha: f32) -> Rgba {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    rgba(0xFFFF_FF00 | a)
}

#[inline]
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

impl BitChordApp {
    pub fn render_now_playing(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let mesh = self.active_mesh_image();
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

        // Text on the darkened Now Playing mesh backdrop stays crisp white (>7:1 contrast).
        let ink_primary = rgba_hex(0xFFFF_FFFF);
        let ink_secondary = rgba_hex(0xFFFF_FFB8);
        let ink_dim = rgba_hex(0xFFFF_FF70);
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
            // Layer 1: 2-column desktop stage (Artwork + Controls | Auto-Scrolling Lyrics)
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
                    // Left column: Album art + metadata + continuous pill scrubber + DSP controls
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
                            // Artwork
                            .child(self.render_art_box(
                                art_url.as_deref(),
                                320.0,
                                16.0,
                                icons::DISC,
                                theme,
                            ))
                            // Title + Artist + Album
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_size(px(22.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(ink_primary)
                                            .child(
                                                song.map(|s| s.title.clone()).unwrap_or_else(|| {
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
                                                    song.map(|s| s.artist.as_str()).unwrap_or("")
                                                )
                                            } else {
                                                song.map(|s| s.artist.clone()).unwrap_or_else(
                                                    || {
                                                        "Select any song from Home, Search, or Library"
                                                            .to_owned()
                                                    },
                                                )
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
                            // Single continuous smooth pill scrubber (`notes.md` #4)
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(pill_slider(
                                        "np-seek-pill",
                                        progress,
                                        6.5,
                                        20.0,
                                        rgba_hex(0xFFFF_FF30),
                                        ink_primary,
                                        self.np_seek_bounds.clone(),
                                        cx.listener(|this, &frac: &f32, _w, cx| {
                                            this.seek_fraction(frac, false, cx);
                                        }),
                                        cx.listener(|this, &frac: &f32, _w, cx| {
                                            this.seek_fraction(frac, true, cx);
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .text_size(px(11.5))
                                            .text_color(ink_secondary)
                                            .child(format_time_ms(pos_ms))
                                            .child(format_time_ms(dur_ms)),
                                    ),
                            )
                            // Vector icon transport row
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
                                            "Sample rate: {} Hz · Bit depth: {}",
                                            ns.and_then(|n| n.sample_rate_hz)
                                                .map(|r| r.to_string())
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
                                        )),
                                )
                            }),
                    )
                    // Right column: Smooth Auto-Scrolling Pure-White Lyrics Stage (`notes.md` #3)
                    .child(
                        div()
                            .id("lyrics-stage")
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
                            // Pause auto-scroll gracefully for 4s when the user manually scrolls lyrics.
                            .on_scroll_wheel(cx.listener(|this, _ev, _w, _cx| {
                                this.lyrics_manual_scroll_until =
                                    Some(Instant::now() + Duration::from_secs(4));
                            }))
                            // Lyrics header bar (provider provenance + translation button)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.0))
                                            .child(icons::icon(icons::LYRICS, 18.0, ink_primary))
                                            .child(
                                                div()
                                                    .text_size(px(16.0))
                                                    .font_weight(FontWeight::BOLD)
                                                    .text_color(ink_primary)
                                                    .child("Lyrics"),
                                            )
                                            .when_some(
                                                match &self.lyrics {
                                                    LoadState::Ready(p) => {
                                                        Some(p.source.label().to_owned())
                                                    }
                                                    _ => None,
                                                },
                                                |el, src| {
                                                    el.child(
                                                        div()
                                                            .px(px(8.0))
                                                            .py(px(2.0))
                                                            .rounded_full()
                                                            .bg(surface_chip)
                                                            .text_size(px(11.0))
                                                            .text_color(ink_secondary)
                                                            .child(src),
                                                    )
                                                },
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("lyrics-translate-en")
                                            .px(px(11.0))
                                            .h(px(30.0))
                                            .rounded(px(8.0))
                                            .bg(if self.show_translated_lyrics {
                                                rgba_hex(0xFFFF_FF30)
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.show_translated_lyrics {
                                                ink_primary
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_translated_lyrics =
                                                    !this.show_translated_lyrics;
                                                if this.show_translated_lyrics {
                                                    this.send(
                                                        BackendCommand::TranslateLyrics {
                                                            target_lang: "en".to_owned(),
                                                        },
                                                        cx,
                                                    );
                                                }
                                                cx.notify();
                                            }))
                                            .child(icons::icon(
                                                icons::TRANSLATE,
                                                14.0,
                                                ink_primary,
                                            ))
                                            .child(if self.show_translated_lyrics {
                                                let lang = match &self.lyrics {
                                                    LoadState::Ready(p) => {
                                                        p.translated_lang.as_deref().unwrap_or("en")
                                                    }
                                                    _ => "en",
                                                };
                                                format!("Translation ({})", lang.to_uppercase())
                                            } else {
                                                "Translate (EN)".to_owned()
                                            }),
                                    ),
                            )
                            // Variable-height virtualized lyrics body
                            .child(match &self.lyrics {
                                LoadState::Idle => div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .text_size(px(15.0))
                                    .text_color(ink_secondary)
                                    .child(
                                        "Play a track to fetch synchronized lyrics across 17 providers.",
                                    )
                                    .into_any_element(),
                                LoadState::Loading => div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .text_size(px(15.0))
                                    .text_color(ink_secondary)
                                    .child("Racing synced lyrics providers...")
                                    .into_any_element(),
                                LoadState::Error(msg) => div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .text_size(px(14.5))
                                    .text_color(ink_secondary)
                                    .child(msg.clone())
                                    .into_any_element(),
                                LoadState::Ready(payload) if payload.lines.is_empty() => div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .text_size(px(14.5))
                                    .text_color(ink_secondary)
                                    .child("No lyrics lines returned for this recording.")
                                    .into_any_element(),
                                LoadState::Ready(_) => list(
                                    self.lyrics_list_state.clone(),
                                    cx.processor(|this, idx: usize, _window, cx| {
                                        let LoadState::Ready(ref payload) = this.lyrics else {
                                            return div().into_any_element();
                                        };
                                        let Some(line) = payload.lines.get(idx) else {
                                            return div().into_any_element();
                                        };
                                        let pos_ms = this.live_position_ms() as i64;
                                        let is_active = this.active_lyric_idx == Some(idx);
                                        let was_prev_active = this.prev_lyric_idx == Some(idx);
                                        let is_past = this
                                            .active_lyric_idx
                                            .is_some_and(|act| idx < act);
                                        let transition_t = ease_out_cubic(
                                            this.lyric_transition_at.elapsed().as_secs_f32()
                                                / 0.42,
                                        );
                                        let translated_text = if this.show_translated_lyrics {
                                            payload
                                                .translated_lines
                                                .as_ref()
                                                .and_then(|tl| tl.get(idx))
                                                .map(|l| l.text.clone())
                                        } else {
                                            None
                                        };

                                        let next_time_ms =
                                            payload.lines.get(idx + 1).map(|l| l.time_ms);

                                        render_lyric_line_row(
                                            idx,
                                            line,
                                            next_time_ms,
                                            pos_ms,
                                            is_active,
                                            was_prev_active,
                                            is_past,
                                            transition_t,
                                            translated_text,
                                            cx,
                                        )
                                        .into_any_element()
                                    }),
                                )
                                .flex_1()
                                .w_full()
                                .into_any_element(),
                            }),
                    ),
            )
    }
}

/// Returns `true` when a lyric entry represents an instrumental intro or interlude
/// (blank gap markers emitted by `with_instrumental_gaps`, musical note symbols,
/// ellipsis, or explicit `[Instrumental]` tags).
pub(crate) fn is_instrumental_line(line: &LyricLine) -> bool {
    if line.is_gap() {
        return true;
    }
    let trimmed = line.text.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "..."
            | "…"
            | ". . ."
            | "• • •"
            | "•••"
            | "instrumental"
            | "[instrumental]"
            | "(instrumental)"
            | "music"
            | "[music]"
            | "(music)"
            | "interlude"
            | "[interlude]"
            | "(interlude)"
    ) {
        return true;
    }
    // Musical note symbols / dots / whitespace only (e.g. "♪", "♫ ♪", "🎵")
    trimmed.chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '♪' | '♫' | '♩' | '♬' | '🎵' | '🎶' | '•' | '·' | '.' | '…' | '-' | '~'
            )
    })
}

#[allow(clippy::too_many_arguments)]
fn render_lyric_line_row(
    idx: usize,
    line: &LyricLine,
    next_time_ms: Option<i64>,
    pos_ms: i64,
    is_active: bool,
    was_prev_active: bool,
    is_past: bool,
    transition_t: f32,
    translated_text: Option<String>,
    cx: &mut Context<BitChordApp>,
) -> impl IntoElement {
    let seek_ms = line.time_ms.max(0) as u64;

    // Smooth pure-white opacity & horizontal focus glide (zero harsh red color jumps!).
    let line_alpha = if is_active {
        0.48 + 0.52 * transition_t
    } else if was_prev_active {
        1.0 - 0.66 * transition_t
    } else if is_past {
        0.34
    } else {
        0.46
    };
    let left_pad = if is_active {
        14.0 + 7.0 * transition_t
    } else if was_prev_active {
        14.0 + 7.0 * (1.0 - transition_t)
    } else {
        14.0
    };
    let focus_bg_alpha = if is_active {
        0.075 * transition_t
    } else if was_prev_active {
        0.075 * (1.0 - transition_t)
    } else {
        0.0
    };
    let lyric_size = if is_active {
        21.0 + 4.0 * transition_t
    } else if was_prev_active {
        21.0 + 4.0 * (1.0 - transition_t)
    } else {
        20.5
    };

    let content = if is_instrumental_line(line) {
        // Instrumental interlude indicator: three dots (. . .) scaling with lyric size
        // with smooth progressive pure-white illumination across the break duration.
        let dot_base = lyric_size * 0.42;
        let dot_gap = lyric_size * 0.38;
        let gap_frac = if is_active {
            let end_ms = next_time_ms
                .filter(|&nt| nt > line.time_ms)
                .unwrap_or(line.time_ms + 4_000);
            ((pos_ms - line.time_ms) as f32 / (end_ms - line.time_ms).max(1) as f32).clamp(0.0, 1.0)
        } else if is_past {
            1.0
        } else {
            0.0
        };

        div()
            .h(px(lyric_size * 1.28))
            .flex()
            .items_center()
            .gap(px(dot_gap))
            .children((0usize..3).map(|dot_i| {
                let (alpha, diameter) = if is_active {
                    let dot_t = ease_out_cubic((gap_frac * 3.0 - dot_i as f32).clamp(0.0, 1.0));
                    let a = 0.34 + 0.66 * dot_t;
                    let d = dot_base * (0.90 + 0.16 * dot_t);
                    (a, d)
                } else {
                    (line_alpha, dot_base * 0.90)
                };
                div()
                    .w(px(diameter))
                    .h(px(diameter))
                    .rounded_full()
                    .bg(white_alpha(alpha))
            }))
            .into_any_element()
    } else if !line.words.is_empty() && is_active {
        // Continuous liquid pure-white luminance sweep across each word as it is sung.
        div()
            .flex()
            .flex_wrap()
            .gap_x(px(7.5))
            .gap_y(px(3.0))
            .children(line.words.iter().map(|word| {
                let total_chars = word.text.chars().count().max(1) as f32;
                let sung = word.chars_sung_at(pos_ms);
                let frac = ease_out_cubic((sung / total_chars).clamp(0.0, 1.0));
                let word_alpha = (0.42 + 0.58 * frac).max(0.45 * transition_t);
                div()
                    .text_size(px(25.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(white_alpha(word_alpha))
                    .child(word.text.clone())
            }))
            .into_any_element()
    } else {
        div()
            .text_size(px(lyric_size))
            .font_weight(if is_active {
                FontWeight::BOLD
            } else {
                FontWeight::SEMIBOLD
            })
            .text_color(white_alpha(line_alpha))
            .child(line.text.clone())
            .into_any_element()
    };

    let show_translation = !is_instrumental_line(line);

    div()
        .id(("lyric-line", idx))
        .w_full()
        .py(px(12.0))
        .pl(px(left_pad))
        .pr(px(14.0))
        .rounded(px(12.0))
        .bg(white_alpha(focus_bg_alpha))
        .cursor_pointer()
        .hover(|s| s.bg(rgba_hex(0xFFFF_FF14)))
        .on_click(cx.listener(move |this, _, _, cx| {
            // Clicking any line immediately resumes auto-scroll and seeks cleanly.
            this.lyrics_manual_scroll_until = None;
            this.send(BackendCommand::SeekMs(seek_ms), cx);
        }))
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(content)
        .when(show_translation, |el| {
            el.when_some(translated_text, |inner, tr| {
                inner.child(
                    div()
                        .text_size(px(14.0))
                        .text_color(white_alpha(if is_active { 0.78 } else { 0.42 }))
                        .child(tr),
                )
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_instrumental_gap_and_placeholder_lines_only() {
        assert!(is_instrumental_line(&LyricLine::new(0, "")));
        assert!(is_instrumental_line(&LyricLine::new(10_000, "   ")));
        assert!(is_instrumental_line(&LyricLine::new(12_000, "♪")));
        assert!(is_instrumental_line(&LyricLine::new(
            15_000,
            "[Instrumental]"
        )));
        assert!(is_instrumental_line(&LyricLine::new(18_000, "...")));
        assert!(!is_instrumental_line(&LyricLine::new(
            20_000,
            "I'm giving you a night call"
        )));
    }
}
