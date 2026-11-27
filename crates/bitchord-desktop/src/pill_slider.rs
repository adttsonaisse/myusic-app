//! Single continuous interactive pill slider for playback progress scrubbing
//! and volume control (`notes.md` #4 & §2 #4).
//!
//! Features:
//! - Always renders as **one continuous smooth rounded pill** (never segmented dots).
//! - Window-level mouse drag capture via `window.on_mouse_event` so dragging
//!   keeps tracking smoothly even when the pointer drifts above or below the bar,
//!   and commits the exact release position on `MouseUp`.
//! - Live hover preview highlight + optional floating timestamp pill (`m:ss`)
//!   aligned above the hovered/dragged point on playback progress bars.

use std::cell::Cell;
use std::rc::Rc;

use bitchord_storage::format_duration_ms;
use gpui::prelude::*;
use gpui::{
    Bounds, DispatchPhase, ElementId, FontWeight, Hsla, MouseButton, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, canvas, div, px, relative,
};

use crate::theme::rgba_hex;

/// Persistent per-slider interaction state stored on the parent view.
#[derive(Clone, Default)]
pub struct PillSliderState {
    bounds: Rc<Cell<Bounds<Pixels>>>,
    dragging: Rc<Cell<bool>>,
    drag_fraction: Rc<Cell<Option<f32>>>,
    hover_fraction: Rc<Cell<Option<f32>>>,
}

impl PillSliderState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging.get()
    }

    pub fn active_fraction(&self, external_fraction: f32) -> f32 {
        if self.dragging.get()
            && let Some(drag) = self.drag_fraction.get()
        {
            return drag.clamp(0.0, 1.0);
        }
        external_fraction.clamp(0.0, 1.0)
    }

    pub fn hover_fraction(&self) -> Option<f32> {
        self.hover_fraction.get()
    }
}

/// Computes the clamped `[0.0, 1.0]` horizontal fraction for `pos` inside `bounds`.
pub fn fraction_for_point(bounds: Bounds<Pixels>, pos: Point<Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 1.0 {
        return 0.0;
    }
    let dx = f32::from(pos.x - bounds.origin.x);
    (dx / width).clamp(0.0, 1.0)
}

/// Computes the preview timestamp (in ms) for a hovered/dragged slider fraction.
pub fn preview_time_ms(fraction: f32, duration_ms: u64) -> Option<u64> {
    if duration_ms == 0 {
        return None;
    }
    let clamped = fraction.clamp(0.0, 1.0);
    Some((f64::from(clamped) * duration_ms as f64).round() as u64)
}

/// Computes the visual progress fraction `[0.0, 1.0]` safely, avoiding NaN/overflow.
pub fn progress_fraction(position_ms: u64, duration_ms: u64) -> f32 {
    if duration_ms == 0 {
        return 0.0;
    }
    (position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0) as f32
}

/// Configuration for [`pill_slider`].
pub struct PillSliderStyle {
    pub track_height_px: f32,
    pub hit_height_px: f32,
    pub track_color: Hsla,
    pub fill_color: Hsla,
    pub hover_fill_color: Hsla,
    pub tooltip_bg: Hsla,
    pub tooltip_text: Hsla,
    pub preview_duration_ms: Option<u64>,
}

/// Renders a single smooth continuous pill bar with window-wide drag capture
/// and live hover preview.
#[allow(clippy::too_many_arguments)]
pub fn pill_slider(
    id: impl Into<ElementId>,
    fraction: f32,
    style: PillSliderStyle,
    state: PillSliderState,
    on_press: impl Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static,
    on_drag: impl Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static,
    on_release: impl Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let is_dragging = state.is_dragging();
    let hover_frac = state.hover_fraction();
    let is_active = is_dragging || hover_frac.is_some();
    let clamped = state.active_fraction(fraction);
    let active_track_h = if is_active {
        style.track_height_px + 2.0
    } else {
        style.track_height_px
    };

    let preview_frac = if is_dragging {
        Some(clamped)
    } else {
        hover_frac
    };
    let tooltip_data = preview_frac.and_then(|frac| {
        let dur_ms = style.preview_duration_ms?;
        let ms = preview_time_ms(frac, dur_ms)?;
        let width_px = f32::from(state.bounds.get().size.width);
        let tip_w = 46.0_f32;
        let left_px = if width_px > tip_w {
            (frac * width_px - tip_w * 0.5).clamp(0.0, width_px - tip_w)
        } else {
            0.0
        };
        Some((format_duration_ms(ms), left_px))
    });

    let down_state = state.clone();
    let canvas_state = state.clone();
    let on_drag_rc = Rc::new(on_drag);
    let on_release_rc = Rc::new(on_release);

    div()
        .id(id)
        .relative()
        .w_full()
        .h(px(style.hit_height_px))
        .flex()
        .items_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
            let frac = fraction_for_point(down_state.bounds.get(), ev.position);
            down_state.dragging.set(true);
            down_state.drag_fraction.set(Some(frac));
            down_state.hover_fraction.set(Some(frac));
            on_press(&frac, window, cx);
            window.refresh();
        })
        // Invisible full-width bounds tracker + window-level drag/hover capture.
        .child(
            div().absolute().inset_0().child(
                canvas(
                    {
                        let bounds_slot = canvas_state.bounds.clone();
                        move |bounds, _window, _cx| {
                            bounds_slot.set(bounds);
                        }
                    },
                    move |bounds, _, window, _cx| {
                        let move_state = canvas_state.clone();
                        let move_drag_cb = Rc::clone(&on_drag_rc);
                        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            if move_state.dragging.get() {
                                if ev.pressed_button == Some(MouseButton::Left) {
                                    let frac = fraction_for_point(bounds, ev.position);
                                    move_state.drag_fraction.set(Some(frac));
                                    move_state.hover_fraction.set(Some(frac));
                                    move_drag_cb(&frac, window, cx);
                                    window.refresh();
                                } else {
                                    // Mouse button released outside window edge.
                                    move_state.dragging.set(false);
                                    move_state.drag_fraction.set(None);
                                    window.refresh();
                                }
                            } else if bounds.contains(&ev.position) {
                                let frac = fraction_for_point(bounds, ev.position);
                                let changed = move_state
                                    .hover_fraction
                                    .get()
                                    .is_none_or(|prev| (prev - frac).abs() > 0.004);
                                if changed {
                                    move_state.hover_fraction.set(Some(frac));
                                    window.refresh();
                                }
                            } else if move_state.hover_fraction.get().is_some() {
                                move_state.hover_fraction.set(None);
                                window.refresh();
                            }
                        });

                        let up_state = canvas_state.clone();
                        let up_release_cb = Rc::clone(&on_release_rc);
                        window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                                return;
                            }
                            if up_state.dragging.get() {
                                up_state.dragging.set(false);
                                let frac = fraction_for_point(bounds, ev.position);
                                up_state.drag_fraction.set(None);
                                if !bounds.contains(&ev.position) {
                                    up_state.hover_fraction.set(None);
                                }
                                up_release_cb(&frac, window, cx);
                                window.refresh();
                            }
                        });
                    },
                )
                .size_full(),
            ),
        )
        // Floating hover/drag time preview badge (only on playback bars with known duration).
        .when_some(tooltip_data, |el, (label, left_px)| {
            el.child(
                div()
                    .absolute()
                    .left(px(left_px))
                    .top(px(-22.0))
                    .w(px(46.0))
                    .h(px(20.0))
                    .rounded(px(6.0))
                    .bg(style.tooltip_bg)
                    .border_1()
                    .border_color(rgba_hex(0xFFFF_FF24))
                    .shadow_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(style.tooltip_text)
                    .child(label),
            )
        })
        // Single continuous rounded pill track + subtle hover preview + smooth solid filled pill
        .child(
            div()
                .relative()
                .w_full()
                .h(px(active_track_h))
                .rounded_full()
                .bg(style.track_color)
                .overflow_hidden()
                .when_some(
                    hover_frac.filter(|&hf| !is_dragging && hf > clamped),
                    |el, hf| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left_0()
                                .w(relative(hf.clamp(0.0, 1.0)))
                                .rounded_full()
                                .bg(style.hover_fill_color),
                        )
                    },
                )
                .child(
                    div()
                        .relative()
                        .h_full()
                        .w(relative(clamped))
                        .rounded_full()
                        .bg(style.fill_color),
                ),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{point, size};

    use super::*;

    #[test]
    fn fraction_for_point_clamps_smoothly_across_bounds() {
        let b = Bounds::new(point(px(100.0), px(20.0)), size(px(200.0), px(12.0)));
        assert_eq!(fraction_for_point(b, point(px(50.0), px(25.0))), 0.0);
        assert_eq!(fraction_for_point(b, point(px(100.0), px(25.0))), 0.0);
        assert!((fraction_for_point(b, point(px(150.0), px(25.0))) - 0.25).abs() < 1e-4);
        assert!((fraction_for_point(b, point(px(200.0), px(25.0))) - 0.5).abs() < 1e-4);
        assert_eq!(fraction_for_point(b, point(px(300.0), px(25.0))), 1.0);
        assert_eq!(fraction_for_point(b, point(px(450.0), px(25.0))), 1.0);
    }

    #[test]
    fn preview_time_and_progress_fraction_handle_zero_and_bounds() {
        assert_eq!(preview_time_ms(0.5, 0), None);
        assert_eq!(preview_time_ms(0.5, 240_000), Some(120_000));
        assert_eq!(preview_time_ms(1.5, 240_000), Some(240_000));
        assert_eq!(preview_time_ms(-0.2, 240_000), Some(0));

        assert_eq!(progress_fraction(50_000, 0), 0.0);
        assert!((progress_fraction(60_000, 240_000) - 0.25).abs() < 1e-4);
        assert_eq!(progress_fraction(300_000, 240_000), 1.0);
    }
}
