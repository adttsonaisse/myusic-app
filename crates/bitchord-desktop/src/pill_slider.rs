//! Single continuous interactive pill slider for playback progress scrubbing
//! and volume control (`notes.md` #4).
//!
//! Uses a zero-overhead `canvas` prepaint probe to capture live window-space
//! layout bounds so click and drag positions map cleanly to `[0.0, 1.0]`
//! across any window width without segmented dots.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Bounds, ElementId, Hsla, MouseButton, Pixels, Point, canvas, div, px, relative};

/// Computes the clamped `[0.0, 1.0]` horizontal fraction for `pos` inside `bounds`.
pub fn fraction_for_point(bounds: Bounds<Pixels>, pos: Point<Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 1.0 {
        return 0.0;
    }
    let dx = f32::from(pos.x - bounds.origin.x);
    (dx / width).clamp(0.0, 1.0)
}

/// Renders a single smooth continuous pill bar with click & drag interaction.
#[allow(clippy::too_many_arguments)]
pub fn pill_slider(
    id: impl Into<ElementId>,
    fraction: f32,
    track_height_px: f32,
    hit_height_px: f32,
    track_color: Hsla,
    fill_color: Hsla,
    bounds_slot: Rc<Cell<Bounds<Pixels>>>,
    on_press: impl Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static,
    on_drag: impl Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let clamped = fraction.clamp(0.0, 1.0);
    let prepaint_bounds = Rc::clone(&bounds_slot);
    let down_bounds = Rc::clone(&bounds_slot);
    let move_bounds = Rc::clone(&bounds_slot);

    div()
        .id(id)
        .relative()
        .w_full()
        .h(px(hit_height_px))
        .flex()
        .items_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
            let frac = fraction_for_point(down_bounds.get(), ev.position);
            on_press(&frac, window, cx);
        })
        .on_mouse_move(move |ev, window, cx| {
            if ev.pressed_button == Some(MouseButton::Left) {
                let frac = fraction_for_point(move_bounds.get(), ev.position);
                on_drag(&frac, window, cx);
            }
        })
        // Invisible full-width bounds tracker updated on every layout prepaint.
        .child(
            div().absolute().inset_0().child(
                canvas(
                    move |bounds, _window, _cx| {
                        prepaint_bounds.set(bounds);
                    },
                    |_bounds, _, _window, _cx| {},
                )
                .size_full(),
            ),
        )
        // Continuous rounded pill track + smooth filled pill
        .child(
            div()
                .relative()
                .w_full()
                .h(px(track_height_px))
                .rounded_full()
                .bg(track_color)
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(relative(clamped))
                        .rounded_full()
                        .bg(fill_color),
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
}
