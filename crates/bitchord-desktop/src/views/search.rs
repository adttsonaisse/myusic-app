//! Live YouTube Music search screen (`W4-2`) with debounced live query & track
//! suggestions (`notes.md` #7), rich Recent Searches cards with cover thumbnails
//! and track metadata, filter tabs, and `uniform_list` virtualized results.

use bitchord_model::{BrowseType, EntityType, PlaybackSourceType, SearchFilter, SearchResult};
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, div, px, uniform_list};

use crate::app::{BitChordApp, LoadState};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;

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
        let has_suggestions = self.search_suggestions_open
            && !self.search_input_text.trim().is_empty()
            && (!self.search_suggestions.queries.is_empty()
                || !self.search_suggestions.items.is_empty());

        div()
            .size_full()
            .px(px(28.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            // Search input + clear + Submit button
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().child(self.search_input.clone()))
                    .when(!self.search_input_text.is_empty(), |el| {
                        el.child(
                            div()
                                .id("search-clear-input")
                                .w(px(36.0))
                                .h(px(38.0))
                                .rounded(px(10.0))
                                .bg(theme.surface_elevated)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(move |s| s.bg(theme.surface_hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.search_input.update(cx, |inp, cx| inp.set_text("", cx));
                                    this.on_search_query_changed(String::new(), cx);
                                }))
                                .child(icons::icon(icons::CLOSE, 15.0, theme.text_secondary)),
                        )
                    })
                    .child(
                        div()
                            .id("search-submit")
                            .h(px(38.0))
                            .px(px(16.0))
                            .rounded(px(10.0))
                            .bg(theme.accent)
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .cursor_pointer()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.on_accent)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let q = this.search_input.read(cx).text().to_owned();
                                this.submit_search(q, cx);
                            }))
                            .child(icons::icon(icons::SEARCH, 15.0, theme.on_accent))
                            .child("Search"),
                    ),
            )
            // Live Search Suggestions dropdown (queries + live matching songs/artists/albums)
            .when(has_suggestions, |el| {
                let queries = self.search_suggestions.queries.clone();
                let items = self.search_suggestions.items.clone();
                let suggested_songs: Vec<_> = items
                    .iter()
                    .filter_map(|r| match r {
                        SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                            Some(song.clone())
                        }
                        SearchResult::Browse { .. } => None,
                    })
                    .collect();

                el.child(
                    div()
                        .p(px(12.0))
                        .rounded(px(12.0))
                        .bg(theme.surface)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .when(!queries.is_empty(), |box_el| {
                            box_el.child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(6.0))
                                    .children(queries.into_iter().take(6).enumerate().map(
                                        |(idx, suggestion)| {
                                            let s_click = suggestion.clone();
                                            div()
                                                .id(("suggest-query", idx))
                                                .px(px(10.0))
                                                .py(px(5.0))
                                                .rounded_full()
                                                .bg(theme.surface_elevated)
                                                .border_1()
                                                .border_color(theme.border)
                                                .flex()
                                                .items_center()
                                                .gap(px(6.0))
                                                .cursor_pointer()
                                                .text_size(px(12.0))
                                                .text_color(theme.text_primary)
                                                .hover(move |s| s.bg(theme.surface_hover))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.search_input.update(cx, |inp, cx| {
                                                        inp.set_text(s_click.clone(), cx);
                                                    });
                                                    this.submit_search(s_click.clone(), cx);
                                                }))
                                                .child(icons::icon(
                                                    icons::SEARCH,
                                                    12.0,
                                                    theme.text_muted,
                                                ))
                                                .child(suggestion)
                                        },
                                    )),
                            )
                        })
                        .when(!items.is_empty(), |box_el| {
                            box_el.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .px(px(4.0))
                                            .text_size(px(11.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text_muted)
                                            .child("SUGGESTED SONGS & MATCHES"),
                                    )
                                    .children(items.into_iter().enumerate().map(|(idx, row)| {
                                        match row {
                                            SearchResult::TopTrack { song }
                                            | SearchResult::Track { song } => {
                                                let art = song_thumbnail_url(&song);
                                                let songs_c = if suggested_songs.is_empty() {
                                                    vec![song.clone()]
                                                } else {
                                                    suggested_songs.clone()
                                                };
                                                let start_i = songs_c
                                                    .iter()
                                                    .position(|s| s.video_id == song.video_id)
                                                    .unwrap_or(0);
                                                let rec_song = song.clone();
                                                let subtitle = match &song.duration_text {
                                                    Some(d) if !d.is_empty() => {
                                                        format!("Song · {} · {d}", song.artist)
                                                    }
                                                    _ => format!("Song · {}", song.artist),
                                                };
                                                div()
                                                    .id(("suggest-track", idx))
                                                    .w_full()
                                                    .px(px(8.0))
                                                    .py(px(6.0))
                                                    .rounded(px(8.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap(px(10.0))
                                                    .cursor_pointer()
                                                    .hover(move |s| s.bg(theme.surface_hover))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.search_suggestions_open = false;
                                                        this.record_song_search_selection(
                                                            &rec_song, cx,
                                                        );
                                                        this.play_songs(
                                                            songs_c.clone(),
                                                            start_i,
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
                                                            .gap(px(10.0))
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .child(self.render_art_box(
                                                                art.as_deref(),
                                                                40.0,
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
                                                                            .font_weight(
                                                                                FontWeight::MEDIUM,
                                                                            )
                                                                            .text_color(
                                                                                theme.text_primary,
                                                                            )
                                                                            .overflow_hidden()
                                                                            .whitespace_nowrap()
                                                                            .child(song.title),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(11.5))
                                                                            .text_color(
                                                                                theme.text_muted,
                                                                            )
                                                                            .overflow_hidden()
                                                                            .whitespace_nowrap()
                                                                            .child(subtitle),
                                                                    ),
                                                            ),
                                                    )
                                                    .child(icons::icon(
                                                        icons::PLAY,
                                                        13.0,
                                                        theme.text_muted,
                                                    ))
                                                    .into_any_element()
                                            }
                                            SearchResult::Browse { item } => {
                                                let b = item.clone();
                                                div()
                                                    .id(("suggest-browse", idx))
                                                    .w_full()
                                                    .px(px(8.0))
                                                    .py(px(6.0))
                                                    .rounded(px(8.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap(px(10.0))
                                                    .cursor_pointer()
                                                    .hover(move |s| s.bg(theme.surface_hover))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.search_suggestions_open = false;
                                                        this.record_browse_search_selection(&b, cx);
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
                                                            .gap(px(10.0))
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .child(self.render_art_box(
                                                                item.thumbnail_url.as_deref(),
                                                                40.0,
                                                                6.0,
                                                                icons::DISC,
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
                                                                            .font_weight(
                                                                                FontWeight::MEDIUM,
                                                                            )
                                                                            .text_color(
                                                                                theme.text_primary,
                                                                            )
                                                                            .overflow_hidden()
                                                                            .whitespace_nowrap()
                                                                            .child(item.title),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(11.5))
                                                                            .text_color(
                                                                                theme.text_muted,
                                                                            )
                                                                            .overflow_hidden()
                                                                            .whitespace_nowrap()
                                                                            .child(item.subtitle),
                                                                    ),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .px(px(7.0))
                                                            .py(px(2.0))
                                                            .rounded(px(5.0))
                                                            .bg(theme.surface_elevated)
                                                            .text_size(px(10.5))
                                                            .text_color(theme.text_secondary)
                                                            .child(format!("{:?}", item.r#type)),
                                                    )
                                                    .into_any_element()
                                            }
                                        }
                                    })),
                            )
                        }),
                )
            })
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
                            .rounded_full()
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
            // Results / Rich Recent Searches / Loading / Error states (`R-27`, `notes.md` #7)
            .child(match &self.search_results {
                LoadState::Idle => {
                    let history = self.library.search_history.clone();
                    if history.is_empty() {
                        div()
                            .p(px(28.0))
                            .rounded(px(12.0))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(icons::icon(icons::SEARCH, 18.0, theme.accent))
                                    .child(
                                        div()
                                            .text_size(px(15.5))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text_primary)
                                            .child("Search YouTube Music"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.text_muted)
                                    .child(
                                        "Start typing to see instant song suggestions, or press Enter for full results.",
                                    ),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .id("recent-searches-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
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
                                            .child(icons::icon(
                                                icons::HISTORY,
                                                16.0,
                                                theme.text_secondary,
                                            ))
                                            .child(
                                                div()
                                                    .text_size(px(15.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text_primary)
                                                    .child("Recent Searches"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("recent-clear-all")
                                            .h(px(28.0))
                                            .px(px(10.0))
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
                                                this.send(BackendCommand::ClearSearchHistory, cx);
                                            }))
                                            .child(icons::icon(
                                                icons::TRASH,
                                                12.5,
                                                theme.text_muted,
                                            ))
                                            .child("Clear All"),
                                    ),
                            )
                            .child(div().flex().flex_col().gap(px(6.0)).children(
                                history.into_iter().enumerate().map(|(idx, item)| {
                                    let entry = item.clone().with_corrected_type();
                                    let rm_id = entry.id.clone();
                                    let badge = match entry.entity_type {
                                        EntityType::Track => "Song",
                                        EntityType::Album => "Album",
                                        EntityType::Artist => "Artist",
                                        EntityType::Playlist => "Playlist",
                                        EntityType::Other => "Search",
                                    };
                                    let fallback_icon = match entry.entity_type {
                                        EntityType::Track => icons::MUSIC_NOTE,
                                        EntityType::Album | EntityType::Playlist => icons::DISC,
                                        EntityType::Artist => icons::USERS,
                                        EntityType::Other => icons::HISTORY,
                                    };
                                    let art_url = entry.artwork_url.clone().or_else(|| {
                                        entry.to_song().as_ref().and_then(song_thumbnail_url)
                                    });
                                    let click_entry = entry.clone();

                                    div()
                                        .id(("recent-search-card", idx))
                                        .w_full()
                                        .px(px(12.0))
                                        .py(px(9.0))
                                        .rounded(px(10.0))
                                        .bg(theme.surface)
                                        .border_1()
                                        .border_color(theme.border)
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(12.0))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(song) = click_entry.to_song() {
                                                this.play_songs(
                                                    vec![song],
                                                    0,
                                                    "Recent Search",
                                                    PlaybackSourceType::Search,
                                                    None,
                                                    cx,
                                                );
                                            } else if matches!(
                                                click_entry.entity_type,
                                                EntityType::Album
                                                    | EntityType::Artist
                                                    | EntityType::Playlist
                                            ) && !click_entry.id.starts_with("q:")
                                            {
                                                let kind = match click_entry.entity_type {
                                                    EntityType::Album => BrowseType::Album,
                                                    EntityType::Artist => BrowseType::Artist,
                                                    _ => BrowseType::Playlist,
                                                };
                                                this.open_detail(
                                                    click_entry.id.clone(),
                                                    click_entry.title.clone(),
                                                    click_entry.subtitle.clone(),
                                                    kind,
                                                    cx,
                                                );
                                            } else {
                                                let q = click_entry.title.clone();
                                                this.search_input.update(cx, |inp, cx| {
                                                    inp.set_text(q.clone(), cx);
                                                });
                                                this.submit_search(q, cx);
                                            }
                                        }))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(12.0))
                                                .flex_1()
                                                .min_w(px(0.0))
                                                .child(self.render_art_box(
                                                    art_url.as_deref(),
                                                    48.0,
                                                    8.0,
                                                    fallback_icon,
                                                    theme,
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
                                                                .text_size(px(13.5))
                                                                .font_weight(FontWeight::SEMIBOLD)
                                                                .text_color(theme.text_primary)
                                                                .overflow_hidden()
                                                                .whitespace_nowrap()
                                                                .child(entry.title),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(12.0))
                                                                .text_color(theme.text_muted)
                                                                .overflow_hidden()
                                                                .whitespace_nowrap()
                                                                .child(if entry.subtitle.is_empty() {
                                                                    badge.to_owned()
                                                                } else {
                                                                    entry.subtitle
                                                                }),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(8.0))
                                                .child(
                                                    div()
                                                        .px(px(8.0))
                                                        .py(px(3.0))
                                                        .rounded(px(5.0))
                                                        .bg(theme.surface_elevated)
                                                        .text_size(px(10.5))
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme.text_secondary)
                                                        .child(badge),
                                                )
                                                .child(
                                                    div()
                                                        .id(("recent-rm", idx))
                                                        .w(px(26.0))
                                                        .h(px(26.0))
                                                        .rounded(px(6.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .hover(move |s| {
                                                            s.bg(theme.surface_active)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.send(
                                                                    BackendCommand::RemoveSearchHistory(
                                                                        rm_id.clone(),
                                                                    ),
                                                                    cx,
                                                                );
                                                                cx.stop_propagation();
                                                            },
                                                        ))
                                                        .child(icons::icon(
                                                            icons::CLOSE,
                                                            13.0,
                                                            theme.text_muted,
                                                        )),
                                                ),
                                        )
                                }),
                            ))
                            .into_any_element()
                    }
                }
                LoadState::Loading => div()
                    .p(px(24.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(13.5))
                    .text_color(theme.text_secondary)
                    .child(format!("Searching for \"{}\"...", self.last_search_query))
                    .into_any_element(),
                LoadState::Error(err) => div()
                    .p(px(20.0))
                    .rounded(px(12.0))
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
                    .rounded(px(12.0))
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
                                        let rec_song = song.clone();
                                        let art = song_thumbnail_url(&song);

                                        div()
                                            .id(("search-track", idx))
                                            .w_full()
                                            .h(px(62.0))
                                            .px(px(12.0))
                                            .rounded(px(10.0))
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
                                                this.record_song_search_selection(&rec_song, cx);
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
                                                        art.as_deref(),
                                                        44.0,
                                                        8.0,
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
                                                            .px(px(9.0))
                                                            .py(px(5.0))
                                                            .rounded(px(7.0))
                                                            .bg(t.surface_elevated)
                                                            .border_1()
                                                            .border_color(t.border)
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(4.0))
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
                                                            .child(icons::icon(
                                                                icons::PLUS,
                                                                12.0,
                                                                t.text_secondary,
                                                            ))
                                                            .child("Next"),
                                                    ),
                                            )
                                            .into_any_element()
                                    }
                                    SearchResult::Browse { item } => {
                                        let b = item.clone();
                                        div()
                                            .id(("search-browse", idx))
                                            .w_full()
                                            .h(px(62.0))
                                            .px(px(12.0))
                                            .rounded(px(10.0))
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(12.0))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(t.surface_hover))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.record_browse_search_selection(&b, cx);
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
                                                        44.0,
                                                        8.0,
                                                        icons::DISC,
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
                                                    .rounded(px(6.0))
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
