//! Compact Linux desktop top bar with Wayland CSD drag + min/max/close controls
//! when `window.window_decorations()` reports `Decorations::Client` (`gpui-linux.md` §5.1).

use gpui::prelude::*;
use gpui::{Context, Focusable, FontWeight, MouseButton, div, px};

use crate::actions::Route;
use crate::app::BitChordApp;

impl BitChordApp {
    pub fn render_titlebar(&self, is_csd: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let route_title = self.route.title().to_owned();
        let in_party_code = self.party.code.clone();
        let sleep_rem = self.playback.sleep_timer_remaining_ms;
        let sleep_after = self.playback.sleep_after_track;

        div()
            .w_full()
            .h(px(44.0))
            .flex_shrink_0()
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .px(px(14.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            // Left: current route title + draggable Wayland CSD spacer
            .child(
                div()
                    .id("titlebar-drag-area")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .when(is_csd, |el| {
                        el.on_mouse_down(MouseButton::Left, |_ev, window, _cx| {
                            window.start_window_move();
                        })
                    })
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_primary)
                            .child(route_title),
                    )
                    .when_some(in_party_code, |el, code| {
                        el.child(
                            div()
                                .id("party-live-pill")
                                .px(px(8.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .bg(theme.accent_soft)
                                .border_1()
                                .border_color(theme.accent)
                                .cursor_pointer()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.accent)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(Route::ListenTogether, cx);
                                }))
                                .child(format!("Party {code}")),
                        )
                    })
                    .when(sleep_rem.is_some() || sleep_after, |el| {
                        let label = if let Some(ms) = sleep_rem {
                            format!("Sleep {}m", (ms / 60_000).max(1))
                        } else {
                            "Sleep: end of track".to_owned()
                        };
                        el.child(
                            div()
                                .id("sleep-pill")
                                .px(px(8.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .bg(theme.surface_elevated)
                                .border_1()
                                .border_color(theme.border)
                                .cursor_pointer()
                                .text_size(px(11.5))
                                .text_color(theme.text_secondary)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(Route::Settings, cx);
                                }))
                                .child(label),
                        )
                    }),
            )
            // Right: quick actions + Wayland CSD window buttons
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("tb-quick-search")
                            .h(px(28.0))
                            .px(px(10.0))
                            .rounded(px(6.0))
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.navigate(Route::Search, cx);
                                let handle = this.search_input.read(cx).focus_handle(cx);
                                handle.focus(window, cx);
                            }))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.text_secondary)
                                    .child("Search"),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(theme.text_muted)
                                    .child("Ctrl+F"),
                            ),
                    )
                    .child(
                        div()
                            .id("tb-queue-toggle")
                            .h(px(28.0))
                            .px(px(10.0))
                            .rounded(px(6.0))
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
                            .child(format!("Queue ({})", self.queue.items.len())),
                    )
                    .when(is_csd, |el| {
                        el.child(
                            div()
                                .ml(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .id("win-min")
                                        .w(px(28.0))
                                        .h(px(28.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(theme.text_secondary)
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(|_, window, _| window.minimize_window())
                                        .child("_"),
                                )
                                .child(
                                    div()
                                        .id("win-max")
                                        .w(px(28.0))
                                        .h(px(28.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(theme.text_secondary)
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(|_, window, _| window.zoom_window())
                                        .child("[]"),
                                )
                                .child(
                                    div()
                                        .id("win-close")
                                        .w(px(28.0))
                                        .h(px(28.0))
                                        .rounded(px(6.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(theme.text_secondary)
                                        .hover(move |s| {
                                            s.bg(theme.accent).text_color(theme.on_accent)
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            if this.close_to_tray {
                                                window.remove_window();
                                            } else {
                                                this.send(
                                                    crate::backend::BackendCommand::Shutdown,
                                                    cx,
                                                );
                                                cx.quit();
                                            }
                                        }))
                                        .child("x"),
                                ),
                        )
                    }),
            )
    }
}
