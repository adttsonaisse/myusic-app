//! Slide-over right Queue drawer (`W4-3`) with selective `backdrop_blur`
//! (glass surface 2 of 2, `R-10`), track thumbnails, vector SVG icons,
//! `QueueTier` badges, jump-to-track, remove, and clear.

use bitchord_model::QueueTier;
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};

use crate::app::BitChordApp;
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;

impl BitChordApp {
    pub fn render_queue_drawer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let items = self.queue.items.clone();
        let cur = self.queue.current_index;

        div()
            .w(px(344.0))
            .h_full()
            .flex_shrink_0()
            .bg(theme.glass_drawer)
            .backdrop_blur(px(20.0))
            .border_l_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(16.0))
                    .py(px(14.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icons::icon(icons::QUEUE_LIST, 16.0, theme.accent))
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.text_primary)
                                    .child(format!("Up Next ({})", items.len())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("queue-clear")
                                    .px(px(9.0))
                                    .py(px(5.0))
                                    .rounded(px(7.0))
                                    .bg(theme.surface_elevated)
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .text_color(theme.text_secondary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::QueueClear, cx);
                                    }))
                                    .child(icons::icon(icons::TRASH, 12.5, theme.text_muted))
                                    .child("Clear"),
                            )
                            .child(
                                div()
                                    .id("queue-close")
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.queue_open = false;
                                        cx.notify();
                                    }))
                                    .child(icons::icon(icons::CLOSE, 14.0, theme.text_muted)),
                            ),
                    ),
            )
            .child(if items.is_empty() {
                div()
                    .p(px(20.0))
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .child("Queue is empty. Play a song or click + Next on any track.")
                    .into_any_element()
            } else {
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(10.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .children(items.into_iter().enumerate().map(|(idx, song)| {
                        let is_cur = cur == Some(idx);
                        let tier_label = match song.queue_tier {
                            QueueTier::UserQueue => "NEXT",
                            QueueTier::Context => "CTX",
                            QueueTier::Autoplay => "AUTO",
                        };
                        let art = song_thumbnail_url(&song);

                        div()
                            .id(("queue-row", idx))
                            .w_full()
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(9.0))
                            .bg(if is_cur {
                                theme.surface_elevated
                            } else {
                                gpui::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(10.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send(BackendCommand::QueueJump(idx), cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.0))
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.render_art_box(
                                        art.as_deref(),
                                        38.0,
                                        6.0,
                                        icons::MUSIC_NOTE,
                                        theme,
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_size(px(13.0))
                                                    .font_weight(if is_cur {
                                                        FontWeight::SEMIBOLD
                                                    } else {
                                                        FontWeight::NORMAL
                                                    })
                                                    .text_color(if is_cur {
                                                        theme.accent
                                                    } else {
                                                        theme.text_primary
                                                    })
                                                    .overflow_hidden()
                                                    .whitespace_nowrap()
                                                    .child(song.title),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.5))
                                                    .text_color(theme.text_muted)
                                                    .overflow_hidden()
                                                    .whitespace_nowrap()
                                                    .child(song.artist),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .px(px(5.0))
                                            .py(px(2.0))
                                            .rounded(px(4.0))
                                            .bg(theme.surface_elevated)
                                            .text_size(px(9.5))
                                            .text_color(theme.text_muted)
                                            .child(tier_label),
                                    )
                                    .child(
                                        div()
                                            .id(("queue-rm", idx))
                                            .w(px(24.0))
                                            .h(px(24.0))
                                            .rounded(px(6.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .hover(move |s| s.bg(theme.surface_active))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.send(BackendCommand::QueueRemove(idx), cx);
                                                cx.stop_propagation();
                                            }))
                                            .child(icons::icon(
                                                icons::CLOSE,
                                                12.5,
                                                theme.text_muted,
                                            )),
                                    ),
                            )
                    }))
                    .into_any_element()
            })
    }
}
