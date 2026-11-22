//! Compact Linux desktop top bar with crisp vector SVG icons and Wayland CSD
//! drag + min/max/close controls when `window.window_decorations()` reports
//! `Decorations::Client` (`gpui-linux.md` §5.1).

use gpui::prelude::*;
use gpui::{Context, Focusable, FontWeight, MouseButton, div, px};

use crate::actions::Route;
use crate::app::BitChordApp;
use crate::icons;

impl BitChordApp {
    pub fn render_titlebar(&self, is_csd: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let route_title = self.route.title().to_owned();
        let in_party_code = self.party.code.clone();
        let sleep_rem = self.playback.sleep_timer_remaining_ms;
        let sleep_after = self.playback.sleep_after_track;

        div()
            .w_full()
            .h(px(46.0))
            .flex_shrink_0()
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            // Left: current route title + status pills + draggable Wayland CSD spacer
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
                                .px(px(9.0))
                                .py(px(4.0))
                                .rounded_full()
                                .bg(theme.accent_soft)
                                .border_1()
                                .border_color(theme.accent)
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .cursor_pointer()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.accent)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(Route::ListenTogether, cx);
                                }))
                                .child(icons::icon(icons::USERS, 13.0, theme.accent))
                                .child(format!("Party {code}")),
                        )
                    })
                    .when(sleep_rem.is_some() || sleep_after, |el| {
                        let label = if let Some(ms) = sleep_rem {
                            format!("{}m", (ms / 60_000).max(1))
                        } else {
                            "End of track".to_owned()
                        };
                        el.child(
                            div()
                                .id("sleep-pill")
                                .px(px(9.0))
                                .py(px(4.0))
                                .rounded_full()
                                .bg(theme.surface_elevated)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .cursor_pointer()
                                .text_size(px(11.5))
                                .text_color(theme.text_secondary)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(Route::Settings, cx);
                                }))
                                .child(icons::icon(icons::TIMER, 13.0, theme.text_secondary))
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
                            .h(px(30.0))
                            .px(px(11.0))
                            .rounded(px(8.0))
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.navigate(Route::Search, cx);
                                let handle = this.search_input.read(cx).focus_handle(cx);
                                handle.focus(window, cx);
                            }))
                            .child(icons::icon(icons::SEARCH, 13.5, theme.text_muted))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.text_secondary)
                                    .child("Search"),
                            )
                            .child(
                                div()
                                    .px(px(5.0))
                                    .py(px(1.0))
                                    .rounded(px(4.0))
                                    .bg(theme.surface)
                                    .text_size(px(10.0))
                                    .text_color(theme.text_muted)
                                    .child("Ctrl+F"),
                            ),
                    )
                    .child(
                        div()
                            .id("tb-queue-toggle")
                            .h(px(30.0))
                            .px(px(11.0))
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
                                14.0,
                                if self.queue_open {
                                    theme.accent
                                } else {
                                    theme.text_secondary
                                },
                            ))
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
                                        .w(px(30.0))
                                        .h(px(30.0))
                                        .rounded(px(7.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(|_, window, _| window.minimize_window())
                                        .child(icons::icon(
                                            icons::WIN_MIN,
                                            14.0,
                                            theme.text_secondary,
                                        )),
                                )
                                .child(
                                    div()
                                        .id("win-max")
                                        .w(px(30.0))
                                        .h(px(30.0))
                                        .rounded(px(7.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(|_, window, _| window.zoom_window())
                                        .child(icons::icon(
                                            icons::WIN_MAX,
                                            13.0,
                                            theme.text_secondary,
                                        )),
                                )
                                .child(
                                    div()
                                        .id("win-close")
                                        .w(px(30.0))
                                        .h(px(30.0))
                                        .rounded(px(7.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(theme.accent))
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
                                        .child(icons::icon(
                                            icons::CLOSE,
                                            14.0,
                                            theme.text_secondary,
                                        )),
                                ),
                        )
                    }),
            )
    }
}
