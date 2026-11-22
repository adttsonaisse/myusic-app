//! Full-bleed Now Playing stage (`W4-3`) over the 64x64 BGRA dynamic artwork
//! mesh backdrop, combining large cover art, real-time transport/DSP controls,
//! collapsible measured `NerdStats` telemetry, and Apple-like 60fps word-synced
//! lyrics via GPUI's variable-height `list()`.

use bitchord_audio::PlaybackState;
use bitchord_lyrics::LyricLine;
use gpui::prelude::*;
use gpui::{Context, FontWeight, ObjectFit, div, img, list, px, rgba};

use crate::app::{BitChordApp, LoadState, format_time_ms};
use crate::backend::BackendCommand;
use crate::theme::rgba_hex;

const SPEEDS: [f32; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

impl BitChordApp {
    pub fn render_now_playing(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let mesh = self.active_mesh_image();
        let song = self.playback.current_song.as_ref();
        let is_playing = self.playback.state == PlaybackState::Playing;
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
        let ink_dim = rgba_hex(0xFFFF_FF66);
        let surface_chip = rgba_hex(0xFFFF_FF18);
        let surface_chip_hover = rgba_hex(0xFFFF_FF29);
        let chip_border = rgba_hex(0xFFFF_FF24);

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            // Layer 0: 64x64 Oklab bilinear-upscaled dynamic artwork mesh backdrop
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        img(mesh)
                            .size_full()
                            .object_fit(ObjectFit::Fill),
                    ),
            )
            // Layer 1: 2-column desktop stage (Artwork + Controls | Word-Synced Lyrics)
            .child(
                div()
                    .relative()
                    .size_full()
                    .px(px(36.0))
                    .py(px(28.0))
                    .flex()
                    .flex_row()
                    .gap(px(40.0))
                    .overflow_hidden()
                    // Left column: Album art + metadata + scrubber + DSP toggles + Nerd Stats
                    .child(
                        div()
                            .id("np-left-scroll")
                            .w(px(380.0))
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
                                    .w(px(124.0))
                                    .h(px(30.0))
                                    .px(px(10.0))
                                    .rounded(px(6.0))
                                    .bg(surface_chip)
                                    .border_1()
                                    .border_color(chip_border)
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(12.0))
                                    .text_color(ink_primary)
                                    .hover(move |s| s.bg(surface_chip_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.route = this.previous_route.clone();
                                        cx.notify();
                                    }))
                                    .child("< Back to Browse"),
                            )
                            // Artwork
                            .child(self.render_art_box(
                                song.and_then(|s| s.thumbnail_url.as_deref()),
                                320.0,
                                14.0,
                                "BC",
                                theme,
                            ))
                            // Title + Artist + Album
                            .child(
                                div()
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
                                            .child(song.map(|s| s.artist.clone()).unwrap_or_else(
                                                || "Select any song from Home, Search, or Library".to_owned(),
                                            )),
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
                            // Interactive scrubber
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .w_full()
                                            .h(px(14.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(2.0))
                                            .children((0usize..32).map(|step| {
                                                let frac = (step as f32 + 0.5) / 32.0;
                                                let filled = frac <= progress;
                                                let target =
                                                    ((step as f64 / 32.0) * dur_ms as f64) as u64;
                                                div()
                                                    .id(("np-seek-seg", step))
                                                    .flex_1()
                                                    .h(px(5.0))
                                                    .rounded(px(2.5))
                                                    .cursor_pointer()
                                                    .bg(if filled {
                                                        ink_primary
                                                    } else {
                                                        rgba_hex(0xFFFF_FF30)
                                                    })
                                                    .hover(move |s| s.bg(theme.accent))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if dur_ms > 0 {
                                                            this.send(
                                                                BackendCommand::SeekMs(target),
                                                                cx,
                                                            );
                                                        }
                                                    }))
                                            })),
                                    )
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
                            // Transport row
                            .child(
                                div()
                                    .w(px(320.0))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .id("np-prev")
                                            .px(px(14.0))
                                            .h(px(38.0))
                                            .rounded(px(8.0))
                                            .bg(surface_chip)
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::PrevTrack, cx);
                                            }))
                                            .child("Prev"),
                                    )
                                    .child(
                                        div()
                                            .id("np-play-pause")
                                            .px(px(26.0))
                                            .h(px(42.0))
                                            .rounded(px(21.0))
                                            .bg(theme.accent)
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.on_accent)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::PlayPause, cx);
                                            }))
                                            .child(if is_playing { "Pause" } else { "Play" }),
                                    )
                                    .child(
                                        div()
                                            .id("np-next")
                                            .px(px(14.0))
                                            .h(px(38.0))
                                            .rounded(px(8.0))
                                            .bg(surface_chip)
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_color(ink_primary)
                                            .hover(move |s| s.bg(surface_chip_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(BackendCommand::NextTrack, cx);
                                            }))
                                            .child("Next"),
                                    ),
                            )
                            // Quick DSP chips: Speed, Skip Silence, Spatial, Nerd Stats
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
                                            .h(px(28.0))
                                            .rounded(px(6.0))
                                            .bg(surface_chip)
                                            .border_1()
                                            .border_color(chip_border)
                                            .flex()
                                            .items_center()
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
                                            .child(format!("{:.2}x Speed", self.playback.speed)),
                                    )
                                    .child(
                                        div()
                                            .id("np-skip-silence")
                                            .px(px(10.0))
                                            .h(px(28.0))
                                            .rounded(px(6.0))
                                            .bg(if self.playback.skip_silence {
                                                theme.accent_soft
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.playback.skip_silence {
                                                theme.accent
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
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
                                            .h(px(28.0))
                                            .rounded(px(6.0))
                                            .bg(if self.playback.spatial_audio {
                                                theme.accent_soft
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.playback.spatial_audio {
                                                theme.accent
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.send(
                                                    BackendCommand::SetSpatial(
                                                        !this.playback.spatial_audio,
                                                    ),
                                                    cx,
                                                );
                                            }))
                                            .child("Spatial"),
                                    )
                                    .child(
                                        div()
                                            .id("np-nerd-toggle")
                                            .px(px(10.0))
                                            .h(px(28.0))
                                            .rounded(px(6.0))
                                            .bg(if self.nerd_stats_open {
                                                theme.accent_soft
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.nerd_stats_open {
                                                theme.accent
                                            } else {
                                                chip_border
                                            })
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(ink_primary)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.nerd_stats_open = !this.nerd_stats_open;
                                                cx.notify();
                                            }))
                                            .child("Stats for Nerds"),
                                    ),
                            )
                            // Collapsible measured NerdStats panel
                            .when(self.nerd_stats_open, |el| {
                                let ns = self.playback.nerd_stats.as_ref();
                                el.child(
                                    div()
                                        .w(px(320.0))
                                        .p(px(12.0))
                                        .rounded(px(8.0))
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
                                            "Pipeline: PipeWire 48 kHz F32LE · Underruns: {} · Skipped: {}ms",
                                            self.playback.underruns,
                                            self.playback.skipped_silence_ms,
                                        )),
                                )
                            }),
                    )
                    // Right column: Apple-like Word-Synced Lyrics Stage
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
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
                                                            .rounded(px(5.0))
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
                                            .px(px(10.0))
                                            .h(px(28.0))
                                            .rounded(px(6.0))
                                            .bg(if self.show_translated_lyrics {
                                                theme.accent_soft
                                            } else {
                                                surface_chip
                                            })
                                            .border_1()
                                            .border_color(if self.show_translated_lyrics {
                                                theme.accent
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
                                    .child("Play a track to fetch synchronized lyrics across 17 providers.")
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
                                        let next_time = payload
                                            .lines
                                            .get(idx + 1)
                                            .map(|l| l.time_ms)
                                            .unwrap_or(i64::MAX);
                                        let is_active =
                                            pos_ms >= line.time_ms && pos_ms < next_time;
                                        let is_past = pos_ms >= next_time;
                                        let translated_text = if this.show_translated_lyrics {
                                            payload
                                                .translated_lines
                                                .as_ref()
                                                .and_then(|tl| tl.get(idx))
                                                .map(|l| l.text.clone())
                                        } else {
                                            None
                                        };

                                        render_lyric_line_row(
                                            idx,
                                            line,
                                            pos_ms,
                                            is_active,
                                            is_past,
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

fn render_lyric_line_row(
    idx: usize,
    line: &LyricLine,
    pos_ms: i64,
    is_active: bool,
    is_past: bool,
    translated_text: Option<String>,
    cx: &mut Context<BitChordApp>,
) -> impl IntoElement {
    let seek_ms = line.time_ms.max(0) as u64;
    let base_color = if is_active {
        rgba(0xFFFF_FFFF)
    } else if is_past {
        rgba(0xFFFF_FF66)
    } else {
        rgba(0xFFFF_FF8C)
    };

    let content = if !line.words.is_empty() && is_active {
        // Word-by-word & syllable-accurate illumination using `LyricWord::chars_sung_at`
        div()
            .flex()
            .flex_wrap()
            .gap_x(px(7.0))
            .gap_y(px(2.0))
            .children(line.words.iter().map(|word| {
                let total_chars = word.text.chars().count().max(1) as f32;
                let sung = word.chars_sung_at(pos_ms);
                let frac = (sung / total_chars).clamp(0.0, 1.0);
                let word_color = if frac >= 0.85 {
                    rgba(0xFFFF_FFFF)
                } else if frac > 0.05 {
                    rgba(0xFA2D_48FF) // live active word sweep in BitChord crimson
                } else {
                    rgba(0xFFFF_FF75)
                };
                div()
                    .text_size(px(24.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(word_color)
                    .child(word.text.clone())
            }))
            .into_any_element()
    } else {
        div()
            .text_size(if is_active { px(24.0) } else { px(20.0) })
            .font_weight(if is_active {
                FontWeight::BOLD
            } else {
                FontWeight::SEMIBOLD
            })
            .text_color(base_color)
            .child(line.text.clone())
            .into_any_element()
    };

    div()
        .id(("lyric-line", idx))
        .w_full()
        .py(px(10.0))
        .px(px(12.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgba_hex(0xFFFF_FF10)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.send(BackendCommand::SeekMs(seek_ms), cx);
        }))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(content)
        .when_some(translated_text, |el, tr| {
            el.child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgba(0xFFFF_FFA8))
                    .child(tr),
            )
        })
}
