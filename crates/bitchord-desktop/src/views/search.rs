//! Live YouTube Music search screen (`W4-2`) with filter tabs, SQLite recent
//! search history, and `uniform_list` virtualized results.

use bitchord_model::{PlaybackSourceType, SearchFilter, SearchResult};
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, div, px, uniform_list};

use crate::app::{BitChordApp, LoadState};
use crate::backend::BackendCommand;

const FILTERS: [(SearchFilter, &str); 6] = [
    (SearchFilter::All, "All"),
    (SearchFilter::Songs, "Songs"),
    (SearchFilter::Videos, "Videos"),
    (SearchFilter::Albums, "Albums"),
    (SearchFilter::Artists, "Artists"),
    (SearchFilter::Playlists, "Playlists"),
];

impl BitChordApp {
    pub fn render_search(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();

        div()
            .size_full()
            .px(px(28.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            // Search input + Submit button
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().child(self.search_input.clone()))
                    .child(
                        div()
                            .id("search-submit")
                            .h(px(36.0))
                            .px(px(16.0))
                            .rounded(px(8.0))
                            .bg(theme.accent)
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.on_accent)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let q = this.search_input.read(cx).text().to_owned();
                                this.submit_search(q, cx);
                            }))
                            .child("Search"),
                    ),
            )
            // Filter chips row
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(8.0))
                    .children(FILTERS.into_iter().enumerate().map(|(idx, (filter, label))| {
                        let active = self.search_filter == filter;
                        div()
                            .id(("search-filter", idx))
                            .h(px(30.0))
                            .px(px(12.0))
                            .rounded(px(6.0))
                            .bg(if active {
                                theme.accent_soft
                            } else {
                                theme.surface_elevated
                            })
                            .border_1()
                            .border_color(if active { theme.accent } else { theme.border })
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .font_weight(if active {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if active {
                                theme.accent
                            } else {
                                theme.text_secondary
                            })
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.search_filter = filter;
                                let q = this.search_input.read(cx).text().to_owned();
                                if !q.trim().is_empty() {
                                    this.submit_search(q, cx);
                                } else {
                                    cx.notify();
                                }
                            }))
                            .child(label)
                    })),
            )
            // Results / Recent History / Loading / Error states (`R-27`)
            .child(match &self.search_results {
                LoadState::Idle => {
                    let history = self.library.search_history.clone();
                    if history.is_empty() {
                        div()
                            .p(px(24.0))
                            .rounded(px(10.0))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_primary)
                                    .child("Search YouTube Music"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.text_muted)
                                    .child("Type a song, album, artist, or playlist above and press Enter."),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text_secondary)
                                    .child("Recent Searches"),
                            )
                            .child(div().flex().flex_wrap().gap(px(8.0)).children(
                                history.into_iter().enumerate().map(|(idx, item)| {
                                    let q = item.title.clone();
                                    div()
                                        .id(("recent-search", idx))
                                        .px(px(12.0))
                                        .py(px(6.0))
                                        .rounded(px(6.0))
                                        .bg(theme.surface)
                                        .border_1()
                                        .border_color(theme.border)
                                        .cursor_pointer()
                                        .text_size(px(12.5))
                                        .text_color(theme.text_primary)
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.search_input.update(cx, |inp, cx| {
                                                inp.set_text(q.clone(), cx);
                                            });
                                            this.submit_search(q.clone(), cx);
                                        }))
                                        .child(item.title)
                                }),
                            ))
                            .into_any_element()
                    }
                }
                LoadState::Loading => div()
                    .p(px(24.0))
                    .rounded(px(10.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(13.5))
                    .text_color(theme.text_secondary)
                    .child(format!("Searching for \"{}\"...", self.last_search_query))
                    .into_any_element(),
                LoadState::Error(err) => div()
                    .p(px(20.0))
                    .rounded(px(10.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.danger)
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.danger)
                            .child("Search request failed"),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme.text_secondary)
                            .child(err.clone()),
                    )
                    .into_any_element(),
                LoadState::Ready(rows) if rows.is_empty() => div()
                    .p(px(24.0))
                    .rounded(px(10.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(13.5))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "No matches found for \"{}\". Try switching the filter chip to All.",
                        self.last_search_query
                    ))
                    .into_any_element(),
                LoadState::Ready(rows) => {
                    let count = rows.len();
                    uniform_list(
                        "search-results-list",
                        count,
                        cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                            let t = this.theme();
                            let LoadState::Ready(ref all_rows) = this.search_results else {
                                return Vec::new();
                            };
                            let playable_songs: Vec<_> = all_rows
                                .iter()
                                .filter_map(|r| match r {
                                    SearchResult::TopTrack { song }
                                    | SearchResult::Track { song } => Some(song.clone()),
                                    SearchResult::Browse { .. } => None,
                                })
                                .collect();
                            let active_vid = this
                                .playback
                                .current_song
                                .as_ref()
                                .map(|s| s.video_id.as_str());

                            range
                                .filter_map(|idx| all_rows.get(idx).map(|r| (idx, r.clone())))
                                .map(|(idx, row)| match row {
                                    SearchResult::TopTrack { song }
                                    | SearchResult::Track { song } => {
                                        let is_cur = active_vid == Some(song.video_id.as_str());
                                        let songs_c = playable_songs.clone();
                                        let start_idx = songs_c
                                            .iter()
                                            .position(|s| s.video_id == song.video_id)
                                            .unwrap_or(0);
                                        let next_s = song.clone();

                                        div()
                                            .id(("search-track", idx))
                                            .w_full()
                                            .h(px(58.0))
                                            .px(px(12.0))
                                            .rounded(px(8.0))
                                            .bg(if is_cur {
                                                t.surface_elevated
                                            } else {
                                                gpui::transparent_black()
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(12.0))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(t.surface_hover))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.play_songs(
                                                    songs_c.clone(),
                                                    start_idx,
                                                    "Search",
                                                    PlaybackSourceType::Search,
                                                    None,
                                                    cx,
                                                );
                                            }))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(12.0))
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .child(this.render_art_box(
                                                        song.thumbnail_url.as_deref(),
                                                        42.0,
                                                        6.0,
                                                        "TR",
                                                        t,
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .flex()
                                                            .flex_col()
                                                            .child(
                                                                div()
                                                                    .text_size(px(13.5))
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .text_color(if is_cur {
                                                                        t.accent
                                                                    } else {
                                                                        t.text_primary
                                                                    })
                                                                    .overflow_hidden()
                                                                    .whitespace_nowrap()
                                                                    .child(song.title.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(12.0))
                                                                    .text_color(t.text_muted)
                                                                    .overflow_hidden()
                                                                    .whitespace_nowrap()
                                                                    .child(song.artist.clone()),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(10.0))
                                                    .child(
                                                        div()
                                                            .text_size(px(12.0))
                                                            .text_color(t.text_muted)
                                                            .child(
                                                                song.duration_text
                                                                    .clone()
                                                                    .unwrap_or_default(),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(("search-next", idx))
                                                            .px(px(8.0))
                                                            .py(px(4.0))
                                                            .rounded(px(5.0))
                                                            .bg(t.surface_elevated)
                                                            .border_1()
                                                            .border_color(t.border)
                                                            .text_size(px(11.0))
                                                            .text_color(t.text_secondary)
                                                            .hover(move |s| s.bg(t.surface_active))
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.send(
                                                                        BackendCommand::PlayNext(
                                                                            Box::new(
                                                                                next_s.clone(),
                                                                            ),
                                                                        ),
                                                                        cx,
                                                                    );
                                                                    cx.stop_propagation();
                                                                },
                                                            ))
                                                            .child("+ Next"),
                                                    ),
                                            )
                                            .into_any_element()
                                    }
                                    SearchResult::Browse { item } => {
                                        let b = item.clone();
                                        div()
                                            .id(("search-browse", idx))
                                            .w_full()
                                            .h(px(58.0))
                                            .px(px(12.0))
                                            .rounded(px(8.0))
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(12.0))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(t.surface_hover))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_detail(
                                                    b.browse_id.clone(),
                                                    b.title.clone(),
                                                    b.subtitle.clone(),
                                                    b.r#type,
                                                    cx,
                                                );
                                            }))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(12.0))
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .child(this.render_art_box(
                                                        item.thumbnail_url.as_deref(),
                                                        42.0,
                                                        6.0,
                                                        "PL",
                                                        t,
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .flex()
                                                            .flex_col()
                                                            .child(
                                                                div()
                                                                    .text_size(px(13.5))
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .text_color(t.text_primary)
                                                                    .overflow_hidden()
                                                                    .whitespace_nowrap()
                                                                    .child(item.title),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(12.0))
                                                                    .text_color(t.text_muted)
                                                                    .overflow_hidden()
                                                                    .whitespace_nowrap()
                                                                    .child(item.subtitle),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .px(px(8.0))
                                                    .py(px(3.0))
                                                    .rounded(px(5.0))
                                                    .bg(t.surface_elevated)
                                                    .text_size(px(11.0))
                                                    .text_color(t.text_secondary)
                                                    .child(format!("{:?}", item.r#type)),
                                            )
                                            .into_any_element()
                                    }
                                })
                                .collect()
                        }),
                    )
                    .flex_1()
                    .w_full()
                    .into_any_element()
                }
            })
    }
}
