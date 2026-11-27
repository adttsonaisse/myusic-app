//! Dedicated 60fps smooth auto-scrolling Lyrics stage entity (`notes.md` #3 & §2 #1).
//!
//! Isolated as its own GPUI `Entity<LyricsView>` so verse transitions and smooth
//! exponential glide scroll frames re-render cleanly without red highlight artifacts:
//! - Pure-white neutral contrast hierarchy (zero harsh red tint on lyrics).
//! - Smooth exponential ease-out auto-scroll centering the active verse around 30%
//!   from the top of the viewport, with top/bottom breathing room so first and last
//!   verses settle comfortably.
//! - Manual wheel scroll gracefully pauses auto-scroll and surfaces a crisp floating
//!   **Sync** button to snap smoothly back to the active line.
//! - Instrumental intros and interludes render as `. . .` scaled to the active lyric
//!   font size with progressive pure-white illumination.

use std::time::{Duration, Instant};

use bitchord_lyrics::LyricLine;
use gpui::prelude::*;
use gpui::{
    Context, EventEmitter, FontWeight, ListAlignment, ListOffset, ListState, Render, Rgba, Window,
    div, list, px, rgba,
};

use crate::backend::LyricsPayload;
use crate::icons;
use crate::theme::rgba_hex;

/// Events emitted by [`LyricsView`] back to [`crate::app::BitChordApp`].
#[derive(Debug, Clone)]
pub enum LyricsViewEvent {
    SeekMs(u64),
    ToggleTranslate,
}

/// High-level state of the lyrics panel.
#[derive(Debug, Clone, Default)]
pub enum LyricsViewState {
    #[default]
    Idle,
    Loading,
    Error(String),
    Ready(Box<LyricsPayload>),
}

/// Lightweight playback timing snapshot fed to [`LyricsView`].
#[derive(Debug, Clone, Copy)]
pub struct LyricsClock {
    pub position_ms: u64,
    pub duration_ms: u64,
    pub speed: f32,
    pub playing: bool,
    pub synced_at: Instant,
}

impl Default for LyricsClock {
    fn default() -> Self {
        Self {
            position_ms: 0,
            duration_ms: 0,
            speed: 1.0,
            playing: false,
            synced_at: Instant::now(),
        }
    }
}

impl LyricsClock {
    pub fn live_position_ms(&self) -> u64 {
        if !self.playing {
            return self.position_ms;
        }
        let elapsed =
            (self.synced_at.elapsed().as_millis() as f64 * f64::from(self.speed)).round() as u64;
        let est = self.position_ms.saturating_add(elapsed);
        if self.duration_ms > 0 {
            est.min(self.duration_ms)
        } else {
            est
        }
    }
}

/// Pure helper: smooth exponential ease-out scroll delta for a single animation frame.
/// Returns `0.0` once within the settle threshold (`0.75px`) so 60fps stops cleanly.
pub(crate) fn smooth_scroll_step(delta_px: f32, dt_secs: f32) -> f32 {
    if delta_px.abs() <= 0.75 {
        return 0.0;
    }
    if delta_px.abs() < 1.6 {
        return delta_px;
    }
    let dt = dt_secs.clamp(0.001, 0.05);
    let alpha = 1.0 - (-9.5 * dt).exp();
    delta_px * alpha
}

/// Pure helper: cubic ease-out curve in `[0.0, 1.0]`.
#[inline]
pub(crate) fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// Pure helper: returns `true` when a lyric entry represents an instrumental intro
/// or interlude (blank gap markers emitted by `with_instrumental_gaps`, musical note
/// symbols, ellipsis, or explicit `[Instrumental]` tags).
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
    trimmed.chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '♪' | '♫' | '♩' | '♬' | '🎵' | '🎶' | '•' | '·' | '.' | '…' | '-' | '~'
            )
    })
}

/// Pure helper: computes the display text size (in px) for a lyric line, used for
/// both regular lyrics and instrumental `. . .` dots so their font sizes match (`notes.md` §2 #1).
pub(crate) fn lyric_font_size_px(
    is_active: bool,
    was_prev_active: bool,
    transition_t: f32,
) -> f32 {
    let t = transition_t.clamp(0.0, 1.0);
    if is_active {
        21.0 + 4.0 * t
    } else if was_prev_active {
        21.0 + 4.0 * (1.0 - t)
    } else {
        20.5
    }
}

/// Pure helper: finds the active lyric index for `pos_ms`.
pub(crate) fn find_active_lyric_index(lines: &[LyricLine], pos_ms: i64) -> Option<usize> {
    lines.iter().rposition(|l| pos_ms >= l.time_ms)
}

#[inline]
fn white_alpha(alpha: f32) -> Rgba {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    rgba(0xFFFF_FF00 | a)
}

/// Self-contained 60fps smooth auto-scrolling Lyrics view entity.
pub struct LyricsView {
    state: LyricsViewState,
    clock: LyricsClock,
    list_state: ListState,
    show_translated: bool,
    active_idx: Option<usize>,
    prev_idx: Option<usize>,
    transition_at: Instant,
    last_frame_at: Instant,
    user_scrolled: bool,
    last_item_y_before_step: Option<(usize, f32)>,
}

impl LyricsView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let now = Instant::now();
        Self {
            state: LyricsViewState::Idle,
            clock: LyricsClock::default(),
            list_state: ListState::new(0, ListAlignment::Top, px(260.0)),
            show_translated: false,
            active_idx: None,
            prev_idx: None,
            transition_at: now,
            last_frame_at: now,
            user_scrolled: false,
            last_item_y_before_step: None,
        }
    }

    pub fn set_clock(&mut self, clock: LyricsClock, cx: &mut Context<Self>) {
        self.clock = clock;
        cx.notify();
    }

    pub fn set_show_translated(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_translated != show {
            self.show_translated = show;
            cx.notify();
        }
    }

    pub fn set_state(&mut self, next: LyricsViewState, cx: &mut Context<Self>) {
        let item_count = match &next {
            LyricsViewState::Ready(payload) if !payload.lines.is_empty() => {
                // +2 for top and bottom breathing-room spacers so first & last verses center smoothly.
                payload.lines.len() + 2
            }
            _ => 0,
        };
        self.state = next;
        self.active_idx = None;
        self.prev_idx = None;
        self.user_scrolled = false;
        self.last_item_y_before_step = None;
        self.transition_at = Instant::now();
        self.list_state = ListState::new(item_count, ListAlignment::Top, px(260.0));
        cx.notify();
    }

    /// Re-enables auto-scroll and smoothly glides back to the currently active verse.
    pub fn resync(&mut self, cx: &mut Context<Self>) {
        self.user_scrolled = false;
        self.last_item_y_before_step = None;
        self.last_frame_at = Instant::now();
        cx.notify();
    }

    /// Advances smooth auto-scroll by one animation frame; returns `true` while
    /// scroll or verse-transition animation is still converging.
    fn step_auto_scroll(&mut self) -> bool {
        let LyricsViewState::Ready(ref payload) = self.state else {
            return false;
        };
        if payload.lines.is_empty() {
            return false;
        }

        let pos_ms = self.clock.live_position_ms() as i64;
        let active_idx = find_active_lyric_index(&payload.lines, pos_ms);

        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame_at)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_frame_at = now;

        if active_idx != self.active_idx {
            self.prev_idx = self.active_idx;
            self.active_idx = active_idx;
            self.transition_at = now;
            self.last_item_y_before_step = None;
        }

        let mut animating = self.transition_at.elapsed() < Duration::from_millis(480);

        if !self.user_scrolled
            && let Some(idx) = active_idx
        {
            // List row index is `idx + 1` because row 0 is the top breathing spacer.
            let list_idx = idx + 1;
            let vp = self.list_state.viewport_bounds();
            let vp_h = f32::from(vp.size.height);
            if vp_h > 40.0 {
                let target_y = vp.origin.y + px(vp_h * 0.28);
                if let Some(item_b) = self.list_state.bounds_for_item(list_idx) {
                    let cur_y = f32::from(item_b.origin.y);
                    let delta = cur_y - f32::from(target_y);
                    let step = smooth_scroll_step(delta, dt);
                    if step != 0.0 {
                        // Guard against spinning at top/bottom scroll clamp where `scroll_by`
                        // can no longer move the item further.
                        let stuck = self
                            .last_item_y_before_step
                            .is_some_and(|(prev_i, prev_y)| {
                                prev_i == list_idx && (prev_y - cur_y).abs() < 0.05
                            });
                        if !stuck {
                            self.last_item_y_before_step = Some((list_idx, cur_y));
                            self.list_state.scroll_by(px(step));
                            animating = true;
                        }
                    } else {
                        self.last_item_y_before_step = None;
                    }
                } else {
                    // Active line is off-screen (seek jump or opened mid-song): jump nearby
                    // so the next frames glide smoothly into the focal zone.
                    self.last_item_y_before_step = None;
                    self.list_state.scroll_to(ListOffset {
                        item_ix: list_idx.saturating_sub(2),
                        offset_in_item: px(0.0),
                    });
                    animating = true;
                }
            }
        }

        // Keep word-by-word / instrumental dot animation flowing smoothly while playing.
        if self.clock.playing
            && let Some(idx) = active_idx
            && let Some(line) = payload.lines.get(idx)
            && (!line.words.is_empty() || is_instrumental_line(line))
        {
            animating = true;
        }

        animating
    }
}

impl EventEmitter<LyricsViewEvent> for LyricsView {}

impl Render for LyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let animating = self.step_auto_scroll();
        if self.clock.playing || animating {
            window.request_animation_frame();
        }

        let ink_primary = rgba_hex(0xFFFF_FFFF);
        let ink_secondary = rgba_hex(0xFFFF_FFB8);
        let surface_chip = rgba_hex(0xFFFF_FF18);
        let surface_chip_hover = rgba_hex(0xFFFF_FF2C);
        let chip_border = rgba_hex(0xFFFF_FF28);

        let source_badge = match &self.state {
            LyricsViewState::Ready(p) => Some(p.source.label().to_owned()),
            _ => None,
        };
        let translated_label = if self.show_translated {
            let lang = match &self.state {
                LyricsViewState::Ready(p) => p.translated_lang.as_deref().unwrap_or("en"),
                _ => "en",
            };
            format!("Translation ({})", lang.to_uppercase())
        } else {
            "Translate (EN)".to_owned()
        };
        let show_sync_btn = self.user_scrolled && self.active_idx.is_some();

        div()
            .id("lyrics-stage")
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .on_scroll_wheel(cx.listener(|this, _ev, _w, cx| {
                if !this.user_scrolled {
                    this.user_scrolled = true;
                    this.last_item_y_before_step = None;
                    cx.notify();
                }
            }))
            // Header bar (provider provenance + Sync button when manually scrolled + translation toggle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
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
                            .when_some(source_badge, |el, src| {
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
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .when(show_sync_btn, |el| {
                                el.child(
                                    div()
                                        .id("lyrics-sync-btn")
                                        .px(px(11.0))
                                        .h(px(30.0))
                                        .rounded_full()
                                        .bg(rgba_hex(0xFFFF_FF30))
                                        .border_1()
                                        .border_color(rgba_hex(0xFFFF_FF80))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .cursor_pointer()
                                        .text_size(px(11.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(ink_primary)
                                        .hover(move |s| s.bg(rgba_hex(0xFFFF_FF44)))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.resync(cx);
                                        }))
                                        .child(icons::icon(icons::SYNC_TARGET, 13.5, ink_primary))
                                        .child("Sync"),
                                )
                            })
                            .child(
                                div()
                                    .id("lyrics-translate-en")
                                    .px(px(11.0))
                                    .h(px(30.0))
                                    .rounded(px(8.0))
                                    .bg(if self.show_translated {
                                        rgba_hex(0xFFFF_FF30)
                                    } else {
                                        surface_chip
                                    })
                                    .border_1()
                                    .border_color(if self.show_translated {
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
                                    .on_click(cx.listener(|_this, _, _, cx| {
                                        cx.emit(LyricsViewEvent::ToggleTranslate);
                                    }))
                                    .child(icons::icon(icons::TRANSLATE, 14.0, ink_primary))
                                    .child(translated_label),
                            ),
                    ),
            )
            // Lyrics list body
            .child(match &self.state {
                LyricsViewState::Idle => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .text_size(px(15.0))
                    .text_color(ink_secondary)
                    .child("Play a track to fetch synchronized lyrics across 17 providers.")
                    .into_any_element(),
                LyricsViewState::Loading => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .text_size(px(15.0))
                    .text_color(ink_secondary)
                    .child("Racing synced lyrics providers...")
                    .into_any_element(),
                LyricsViewState::Error(msg) => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .text_size(px(14.5))
                    .text_color(ink_secondary)
                    .child(msg.clone())
                    .into_any_element(),
                LyricsViewState::Ready(payload) if payload.lines.is_empty() => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .text_size(px(14.5))
                    .text_color(ink_secondary)
                    .child("No lyrics lines returned for this recording.")
                    .into_any_element(),
                LyricsViewState::Ready(_) => list(
                    self.list_state.clone(),
                    cx.processor(|this, list_idx: usize, _window, cx| {
                        let LyricsViewState::Ready(ref payload) = this.state else {
                            return div().into_any_element();
                        };
                        let total_lines = payload.lines.len();
                        // Row 0: top breathing spacer; Last row: bottom spacer for smooth tail centering.
                        if list_idx == 0 {
                            return div().h(px(36.0)).w_full().into_any_element();
                        }
                        if list_idx > total_lines {
                            return div().h(px(260.0)).w_full().into_any_element();
                        }
                        let idx = list_idx - 1;
                        let Some(line) = payload.lines.get(idx) else {
                            return div().into_any_element();
                        };
                        let pos_ms = this.clock.live_position_ms() as i64;
                        let is_active = this.active_idx == Some(idx);
                        let was_prev_active = this.prev_idx == Some(idx);
                        let is_past = this.active_idx.is_some_and(|act| idx < act);
                        let transition_t =
                            ease_out_cubic(this.transition_at.elapsed().as_secs_f32() / 0.42);
                        let translated_text = if this.show_translated {
                            payload
                                .translated_lines
                                .as_ref()
                                .and_then(|tl| tl.get(idx))
                                .map(|l| l.text.clone())
                        } else {
                            None
                        };
                        let next_time_ms = payload.lines.get(idx + 1).map(|l| l.time_ms);

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
            })
            // Subtle floating Sync pill at bottom-right when user scrolls far away
            .when(show_sync_btn, |el| {
                el.child(
                    div()
                        .id("lyrics-sync-floating")
                        .absolute()
                        .right(px(16.0))
                        .bottom(px(16.0))
                        .px(px(14.0))
                        .h(px(34.0))
                        .rounded_full()
                        .bg(rgba_hex(0x1414_18D9))
                        .border_1()
                        .border_color(rgba_hex(0xFFFF_FF48))
                        .shadow_lg()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ink_primary)
                        .hover(|s| s.bg(rgba_hex(0x2424_2CE6)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.resync(cx);
                        }))
                        .child(icons::icon(icons::SYNC_TARGET, 14.0, ink_primary))
                        .child("Sync to Song"),
                )
            })
    }
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
    cx: &mut Context<LyricsView>,
) -> impl IntoElement {
    let seek_ms = line.time_ms.max(0) as u64;

    // Pure-white neutral opacity & subtle horizontal focus glide (zero red color!).
    let line_alpha = if is_active {
        0.50 + 0.50 * transition_t
    } else if was_prev_active {
        1.0 - 0.64 * transition_t
    } else if is_past {
        0.34
    } else {
        0.46
    };
    let left_pad = if is_active {
        14.0 + 6.0 * transition_t
    } else if was_prev_active {
        14.0 + 6.0 * (1.0 - transition_t)
    } else {
        14.0
    };
    let focus_bg_alpha = if is_active {
        0.08 * transition_t
    } else if was_prev_active {
        0.08 * (1.0 - transition_t)
    } else {
        0.0
    };
    let lyric_size = lyric_font_size_px(is_active, was_prev_active, transition_t);
    let instrumental = is_instrumental_line(line);

    let content = if instrumental {
        // Instrumental interlude indicator: `. . .` sized directly to `lyric_size` (`notes.md` §2 #1)
        // with smooth progressive pure-white illumination across the break duration.
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
            .flex()
            .items_center()
            .gap(px(lyric_size * 0.34))
            .children((0usize..3).map(|dot_i| {
                let alpha = if is_active {
                    let dot_t = ease_out_cubic((gap_frac * 3.0 - dot_i as f32).clamp(0.0, 1.0));
                    0.34 + 0.66 * dot_t
                } else {
                    line_alpha
                };
                div()
                    .text_size(px(lyric_size))
                    .font_weight(FontWeight::BOLD)
                    .text_color(white_alpha(alpha))
                    .child(".")
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
                    .text_size(px(lyric_size))
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
            this.user_scrolled = false;
            this.last_item_y_before_step = None;
            cx.emit(LyricsViewEvent::SeekMs(seek_ms));
            cx.notify();
        }))
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(content)
        .when(!instrumental, |el| {
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
        assert!(is_instrumental_line(&LyricLine::new(19_000, ". . .")));
        assert!(!is_instrumental_line(&LyricLine::new(
            20_000,
            "I'm giving you a night call"
        )));
    }

    #[test]
    fn smooth_scroll_step_converges_and_settles_cleanly() {
        assert_eq!(smooth_scroll_step(0.5, 0.016), 0.0);
        assert_eq!(smooth_scroll_step(-0.4, 0.016), 0.0);
        assert!((smooth_scroll_step(1.2, 0.016) - 1.2).abs() < 1e-4);
        let step = smooth_scroll_step(100.0, 0.016);
        assert!(step > 10.0 && step < 25.0, "unexpected step={step}");
        let neg_step = smooth_scroll_step(-100.0, 0.016);
        assert!(neg_step < -10.0 && neg_step > -25.0);
    }

    #[test]
    fn lyric_font_size_scales_active_and_inactive_smoothly() {
        assert!((lyric_font_size_px(true, false, 1.0) - 25.0).abs() < 1e-4);
        assert!((lyric_font_size_px(true, false, 0.0) - 21.0).abs() < 1e-4);
        assert!((lyric_font_size_px(false, true, 0.5) - 23.0).abs() < 1e-4);
        assert!((lyric_font_size_px(false, false, 1.0) - 20.5).abs() < 1e-4);
    }

    #[test]
    fn finds_active_lyric_index_accurately() {
        let lines = vec![
            LyricLine::new(0, ""),
            LyricLine::new(5_000, "First line"),
            LyricLine::new(12_000, "Second line"),
        ];
        assert_eq!(find_active_lyric_index(&lines, 2_000), Some(0));
        assert_eq!(find_active_lyric_index(&lines, 5_000), Some(1));
        assert_eq!(find_active_lyric_index(&lines, 15_000), Some(2));
    }
}
