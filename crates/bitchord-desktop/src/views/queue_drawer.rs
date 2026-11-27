//! Slide-over right Queue drawer (`W4-3`) with selective `backdrop_blur`
//! (glass surface 2 of 2, `R-10`), sectioned `uniform_list` virtualization
//! (Now Playing, Next in Queue, Autoplay / Radio, Played Earlier), track
//! reorder controls (up/down), Autoplay toggle, Save as Playlist form,
//! jump-to-track, remove, and clear.

use bitchord_model::{QueueTier, Song};
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px, uniform_list};

use crate::app::BitChordApp;
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;

/// Logical sections inside the Queue drawer, separated using `autoplay_start_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSectionKind {
    NowPlaying,
    NextInQueue,
    Autoplay,
    PlayedEarlier,
}

impl QueueSectionKind {
    pub const fn title(self) -> &'static str {
        match self {
            Self::NowPlaying => "NOW PLAYING",
            Self::NextInQueue => "NEXT IN QUEUE",
            Self::Autoplay => "AUTOPLAY / RADIO",
            Self::PlayedEarlier => "PLAYED EARLIER",
        }
    }

    pub const fn subtitle(self) -> &'static str {
        match self {
            Self::NowPlaying => "Currently active track",
            Self::NextInQueue => "Queued by you and current context",
            Self::Autoplay => "Similar songs added automatically",
            Self::PlayedEarlier => "Earlier in this listening session",
        }
    }
}

/// Flattened uniform-height entry rendered by `uniform_list` inside the Queue drawer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueDrawerEntry {
    Header {
        kind: QueueSectionKind,
        count: usize,
    },
    Track {
        queue_index: usize,
        section: QueueSectionKind,
    },
}

/// Builds the ordered section headers and track indices for the Queue drawer from
/// `items`, `current_index`, and `autoplay_start_index`.
pub fn build_queue_drawer_entries(
    items: &[Song],
    current_index: Option<usize>,
    autoplay_start_index: Option<usize>,
) -> Vec<QueueDrawerEntry> {
    if items.is_empty() {
        return Vec::new();
    }

    let len = items.len();
    let cur = current_index.filter(|&i| i < len);
    let auto_start = autoplay_start_index.map(|i| i.min(len));
    let mut entries = Vec::with_capacity(len + 4);

    // 1. Now Playing
    if let Some(c) = cur {
        entries.push(QueueDrawerEntry::Header {
            kind: QueueSectionKind::NowPlaying,
            count: 1,
        });
        entries.push(QueueDrawerEntry::Track {
            queue_index: c,
            section: QueueSectionKind::NowPlaying,
        });
    }

    // 2. Next in Queue (user/context tracks after `cur` before `auto_start`)
    let after_cur = cur.map_or(0, |c| c + 1);
    let user_end = auto_start.unwrap_or(len);
    if after_cur < user_end {
        entries.push(QueueDrawerEntry::Header {
            kind: QueueSectionKind::NextInQueue,
            count: user_end - after_cur,
        });
        for idx in after_cur..user_end {
            entries.push(QueueDrawerEntry::Track {
                queue_index: idx,
                section: QueueSectionKind::NextInQueue,
            });
        }
    }

    // 3. Autoplay / Radio continuation tracks
    if let Some(as_idx) = auto_start {
        let radio_start = as_idx.max(after_cur);
        if radio_start < len {
            entries.push(QueueDrawerEntry::Header {
                kind: QueueSectionKind::Autoplay,
                count: len - radio_start,
            });
            for idx in radio_start..len {
                entries.push(QueueDrawerEntry::Track {
                    queue_index: idx,
                    section: QueueSectionKind::Autoplay,
                });
            }
        }
    }

    // 4. Played Earlier (tracks before `cur`, still clickable & reorderable)
    if let Some(c) = cur
        && c > 0
    {
        entries.push(QueueDrawerEntry::Header {
            kind: QueueSectionKind::PlayedEarlier,
            count: c,
        });
        for idx in 0..c {
            entries.push(QueueDrawerEntry::Track {
                queue_index: idx,
                section: QueueSectionKind::PlayedEarlier,
            });
        }
    }

    entries
}

impl BitChordApp {
    pub fn render_queue_drawer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let count = self.queue.items.len();
        let entries = build_queue_drawer_entries(
            &self.queue.items,
            self.queue.current_index,
            self.queue.autoplay_start_index,
        );
        let entry_count = entries.len();
        let autoplay_on = self.autoplay_enabled;

        div()
            .w(px(372.0))
            .h_full()
            .flex_shrink_0()
            .bg(theme.glass_drawer)
            .backdrop_blur(px(20.0))
            .border_l_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            // Top Header
            .child(
                div()
                    .px(px(16.0))
                    .py(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
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
                                            .child(format!("Queue ({count})")),
                                    ),
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
                    )
                    // Action strip: Autoplay toggle + Save as Playlist + Clear
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("queue-autoplay-toggle")
                                    .h(px(28.0))
                                    .px(px(9.0))
                                    .rounded(px(7.0))
                                    .bg(if autoplay_on {
                                        theme.accent_soft
                                    } else {
                                        theme.surface_elevated
                                    })
                                    .border_1()
                                    .border_color(if autoplay_on {
                                        theme.accent
                                    } else {
                                        theme.border
                                    })
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .font_weight(if autoplay_on {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(if autoplay_on {
                                        theme.accent
                                    } else {
                                        theme.text_secondary
                                    })
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_autoplay(cx);
                                    }))
                                    .child(icons::icon(
                                        icons::RADIO,
                                        12.5,
                                        if autoplay_on {
                                            theme.accent
                                        } else {
                                            theme.text_muted
                                        },
                                    ))
                                    .child(if autoplay_on {
                                        "Autoplay: On"
                                    } else {
                                        "Autoplay: Off"
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .when(count > 0, |el| {
                                        el.child(
                                            div()
                                                .id("queue-save-pl-toggle")
                                                .h(px(28.0))
                                                .px(px(9.0))
                                                .rounded(px(7.0))
                                                .bg(if self.queue_save_open {
                                                    theme.accent_soft
                                                } else {
                                                    theme.surface_elevated
                                                })
                                                .border_1()
                                                .border_color(if self.queue_save_open {
                                                    theme.accent
                                                } else {
                                                    theme.border
                                                })
                                                .flex()
                                                .items_center()
                                                .gap(px(5.0))
                                                .cursor_pointer()
                                                .text_size(px(11.5))
                                                .text_color(if self.queue_save_open {
                                                    theme.accent
                                                } else {
                                                    theme.text_secondary
                                                })
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.queue_save_open = !this.queue_save_open;
                                                    cx.notify();
                                                }))
                                                .child(icons::icon(
                                                    icons::SAVE,
                                                    12.0,
                                                    if self.queue_save_open {
                                                        theme.accent
                                                    } else {
                                                        theme.text_muted
                                                    },
                                                ))
                                                .child("Save"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("queue-clear")
                                            .h(px(28.0))
                                            .px(px(9.0))
                                            .rounded(px(7.0))
                                            .bg(theme.surface_elevated)
                                            .border_1()
                                            .border_color(theme.border)
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .cursor_pointer()
                                            .text_size(px(11.5))
                                            .text_color(theme.text_secondary)
                                            .hover(move |s| s.bg(theme.surface_hover))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.queue_save_open = false;
                                                this.send(BackendCommand::QueueClear, cx);
                                            }))
                                            .child(icons::icon(icons::TRASH, 12.0, theme.text_muted))
                                            .child("Clear"),
                                    ),
                            ),
                    )
                    // Inline Save Queue as Playlist quick bar
                    .when(self.queue_save_open && count > 0, |el| {
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .child(self.queue_save_input.clone()),
                                )
                                .child(
                                    div()
                                        .id("queue-save-confirm")
                                        .h(px(38.0))
                                        .px(px(11.0))
                                        .rounded(px(9.0))
                                        .bg(theme.accent)
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.on_accent)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let name =
                                                this.queue_save_input.read(cx).text().to_owned();
                                            this.submit_save_queue_as_playlist(name, cx);
                                        }))
                                        .child("Save"),
                                ),
                        )
                    }),
            )
            // Body: Sectioned virtualized list or empty state
            .child(if count == 0 {
                div()
                    .p(px(20.0))
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .child("Queue is empty. Play a song, click + Next, or start Radio on any track.")
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .child(
                        uniform_list(
                            "queue-virtual-list",
                            entry_count,
                            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                let t = this.theme();
                                let cur = this.queue.current_index;
                                let total_tracks = this.queue.items.len();
                                let flat = build_queue_drawer_entries(
                                    &this.queue.items,
                                    cur,
                                    this.queue.autoplay_start_index,
                                );

                                range
                                    .filter_map(|row_idx| {
                                        flat.get(row_idx).cloned().map(|entry| (row_idx, entry))
                                    })
                                    .filter_map(|(row_idx, entry)| match entry {
                                        QueueDrawerEntry::Header { kind, count: sec_count } => {
                                            let icon_bytes = match kind {
                                                QueueSectionKind::NowPlaying => icons::DISC,
                                                QueueSectionKind::NextInQueue => icons::QUEUE_LIST,
                                                QueueSectionKind::Autoplay => icons::RADIO,
                                                QueueSectionKind::PlayedEarlier => icons::HISTORY,
                                            };
                                            let accent_header = matches!(
                                                kind,
                                                QueueSectionKind::NowPlaying
                                                    | QueueSectionKind::Autoplay
                                            );
                                            Some(
                                                div()
                                                    .id(("queue-sec-hdr", row_idx))
                                                    .w_full()
                                                    .h(px(58.0))
                                                    .px(px(10.0))
                                                    .pt(px(12.0))
                                                    .pb(px(6.0))
                                                    .when(row_idx > 0, |el| {
                                                        el.border_t_1().border_color(t.border)
                                                    })
                                                    .flex()
                                                    .flex_col()
                                                    .justify_end()
                                                    .gap(px(2.0))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap(px(6.0))
                                                                    .child(icons::icon(
                                                                        icon_bytes,
                                                                        12.5,
                                                                        if accent_header {
                                                                            t.accent
                                                                        } else {
                                                                            t.text_muted
                                                                        },
                                                                    ))
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(11.0))
                                                                            .font_weight(
                                                                                FontWeight::BOLD,
                                                                            )
                                                                            .text_color(
                                                                                if accent_header {
                                                                                    t.accent
                                                                                } else {
                                                                                    t.text_secondary
                                                                                },
                                                                            )
                                                                            .child(kind.title()),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .px(px(6.0))
                                                                    .py(px(1.0))
                                                                    .rounded_full()
                                                                    .bg(t.surface_elevated)
                                                                    .text_size(px(10.0))
                                                                    .text_color(t.text_muted)
                                                                    .child(format!("{sec_count}")),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(10.5))
                                                            .text_color(t.text_muted)
                                                            .truncate()
                                                            .child(kind.subtitle()),
                                                    )
                                                    .into_any_element(),
                                            )
                                        }
                                        QueueDrawerEntry::Track {
                                            queue_index: idx,
                                            section,
                                        } => {
                                            let song = this.queue.items.get(idx)?.clone();
                                            let is_cur = cur == Some(idx);
                                            let is_dim =
                                                section == QueueSectionKind::PlayedEarlier;
                                            let tier_label = match song.queue_tier {
                                                QueueTier::UserQueue => "NEXT",
                                                QueueTier::Context => "CTX",
                                                QueueTier::Autoplay => "AUTO",
                                            };
                                            let art = song_thumbnail_url(&song);
                                            let can_up = idx > 0;
                                            let can_dn = idx + 1 < total_tracks;

                                            Some(
                                                div()
                                                    .id(("queue-row", idx))
                                                    .w_full()
                                                    .h(px(58.0))
                                                    .px(px(8.0))
                                                    .py(px(6.0))
                                                    .rounded(px(9.0))
                                                    .bg(if is_cur {
                                                        t.surface_elevated
                                                    } else {
                                                        gpui::transparent_black()
                                                    })
                                                    .when(is_dim, |el| el.opacity(0.68))
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap(px(8.0))
                                                    .cursor_pointer()
                                                    .hover(move |s| s.bg(t.surface_hover))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.send(
                                                            BackendCommand::QueueJump(idx),
                                                            cx,
                                                        );
                                                    }))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(9.0))
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .child(this.render_art_box(
                                                                art.as_deref(),
                                                                38.0,
                                                                7.0,
                                                                icons::MUSIC_NOTE,
                                                                t,
                                                            ))
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w(px(0.0))
                                                                    .flex()
                                                                    .flex_col()
                                                                    .gap(px(2.0))
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(12.5))
                                                                            .font_weight(if is_cur {
                                                                                FontWeight::SEMIBOLD
                                                                            } else {
                                                                                FontWeight::NORMAL
                                                                            })
                                                                            .text_color(if is_cur {
                                                                                t.accent
                                                                            } else {
                                                                                t.text_primary
                                                                            })
                                                                            .truncate()
                                                                            .child(song.title),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(11.0))
                                                                            .text_color(
                                                                                t.text_muted,
                                                                            )
                                                                            .truncate()
                                                                            .child(song.artist),
                                                                    ),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(3.0))
                                                            .flex_shrink_0()
                                                            .child(
                                                                div()
                                                                    .px(px(4.0))
                                                                    .py(px(1.5))
                                                                    .rounded(px(4.0))
                                                                    .bg(t.surface_elevated)
                                                                    .text_size(px(9.0))
                                                                    .text_color(t.text_muted)
                                                                    .child(tier_label),
                                                            )
                                                            // Move Up
                                                            .child(
                                                                div()
                                                                    .id(("queue-up", idx))
                                                                    .w(px(22.0))
                                                                    .h(px(22.0))
                                                                    .rounded(px(5.0))
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .when(can_up, |el| {
                                                                        el.cursor_pointer()
                                                                            .hover(move |s| {
                                                                                s.bg(
                                                                                    t.surface_active,
                                                                                )
                                                                            })
                                                                            .on_click(cx.listener(
                                                                                move |this, _, _, cx| {
                                                                                    this.reorder_queue(
                                                                                        idx,
                                                                                        idx - 1,
                                                                                        cx,
                                                                                    );
                                                                                    cx.stop_propagation();
                                                                                },
                                                                            ))
                                                                    })
                                                                    .when(!can_up, |el| {
                                                                        el.opacity(0.28)
                                                                    })
                                                                    .child(icons::icon(
                                                                        icons::CHEVRON_UP,
                                                                        11.5,
                                                                        t.text_muted,
                                                                    )),
                                                            )
                                                            // Move Down
                                                            .child(
                                                                div()
                                                                    .id(("queue-dn", idx))
                                                                    .w(px(22.0))
                                                                    .h(px(22.0))
                                                                    .rounded(px(5.0))
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .when(can_dn, |el| {
                                                                        el.cursor_pointer()
                                                                            .hover(move |s| {
                                                                                s.bg(
                                                                                    t.surface_active,
                                                                                )
                                                                            })
                                                                            .on_click(cx.listener(
                                                                                move |this, _, _, cx| {
                                                                                    this.reorder_queue(
                                                                                        idx,
                                                                                        idx + 1,
                                                                                        cx,
                                                                                    );
                                                                                    cx.stop_propagation();
                                                                                },
                                                                            ))
                                                                    })
                                                                    .when(!can_dn, |el| {
                                                                        el.opacity(0.28)
                                                                    })
                                                                    .child(icons::icon(
                                                                        icons::CHEVRON_DOWN,
                                                                        11.5,
                                                                        t.text_muted,
                                                                    )),
                                                            )
                                                            // Remove
                                                            .child(
                                                                div()
                                                                    .id(("queue-rm", idx))
                                                                    .w(px(22.0))
                                                                    .h(px(22.0))
                                                                    .rounded(px(5.0))
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .hover(move |s| {
                                                                        s.bg(t.surface_active)
                                                                    })
                                                                    .on_click(cx.listener(
                                                                        move |this, _, _, cx| {
                                                                            this.send(
                                                                                BackendCommand::QueueRemove(
                                                                                    idx,
                                                                                ),
                                                                                cx,
                                                                            );
                                                                            cx.stop_propagation();
                                                                        },
                                                                    ))
                                                                    .child(icons::icon(
                                                                        icons::CLOSE,
                                                                        11.5,
                                                                        t.text_muted,
                                                                    )),
                                                            ),
                                                    )
                                                    .into_any_element(),
                                            )
                                        }
                                    })
                                    .collect()
                            }),
                        )
                        .flex_1()
                        .w_full(),
                    )
                    .into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_songs(n: usize) -> Vec<Song> {
        (0..n)
            .map(|i| Song::new(format!("v{i}"), format!("Track {i}"), "Artist", None))
            .collect()
    }

    #[test]
    fn queue_drawer_sections_split_now_playing_user_queue_autoplay_and_played() {
        let items = mk_songs(5);
        // Current = 1, Autoplay starts at 3 -> Now Playing [1], Next [2], Autoplay [3, 4], Played [0].
        let entries = build_queue_drawer_entries(&items, Some(1), Some(3));
        assert_eq!(
            entries,
            vec![
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::NowPlaying,
                    count: 1
                },
                QueueDrawerEntry::Track {
                    queue_index: 1,
                    section: QueueSectionKind::NowPlaying
                },
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::NextInQueue,
                    count: 1
                },
                QueueDrawerEntry::Track {
                    queue_index: 2,
                    section: QueueSectionKind::NextInQueue
                },
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::Autoplay,
                    count: 2
                },
                QueueDrawerEntry::Track {
                    queue_index: 3,
                    section: QueueSectionKind::Autoplay
                },
                QueueDrawerEntry::Track {
                    queue_index: 4,
                    section: QueueSectionKind::Autoplay
                },
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::PlayedEarlier,
                    count: 1
                },
                QueueDrawerEntry::Track {
                    queue_index: 0,
                    section: QueueSectionKind::PlayedEarlier
                },
            ]
        );
    }

    #[test]
    fn queue_drawer_sections_handle_empty_and_active_track_inside_autoplay() {
        assert!(build_queue_drawer_entries(&[], None, None).is_empty());

        let items = mk_songs(4);
        // Current track is already at index 2 inside the Autoplay region (autoplay_start = 2).
        // Expected: Now Playing [2], Autoplay [3], Played Earlier [0, 1] (no empty NextInQueue header).
        let entries = build_queue_drawer_entries(&items, Some(2), Some(2));
        assert_eq!(
            entries,
            vec![
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::NowPlaying,
                    count: 1
                },
                QueueDrawerEntry::Track {
                    queue_index: 2,
                    section: QueueSectionKind::NowPlaying
                },
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::Autoplay,
                    count: 1
                },
                QueueDrawerEntry::Track {
                    queue_index: 3,
                    section: QueueSectionKind::Autoplay
                },
                QueueDrawerEntry::Header {
                    kind: QueueSectionKind::PlayedEarlier,
                    count: 2
                },
                QueueDrawerEntry::Track {
                    queue_index: 0,
                    section: QueueSectionKind::PlayedEarlier
                },
                QueueDrawerEntry::Track {
                    queue_index: 1,
                    section: QueueSectionKind::PlayedEarlier
                },
            ]
        );
    }
}
