//! Live YouTube Music search screen (`W4-2`) with floating keyboard-navigable
//! query + song/artist/album suggestion dropdown (`notes.md` #7), rich Recent
//! Searches cards with cover thumbnails, album & duration metadata (separated
//! cleanly from recent query chips), Like toggles, filter tabs, and `uniform_list`
//! virtualized results.

use bitchord_model::{BrowseType, EntityType, PlaybackSourceType, SearchFilter, SearchResult};
use bitchord_storage::SearchHistoryRecord;
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, deferred, div, px, uniform_list};

use crate::app::{BitChordApp, LoadState, format_time_ms};
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

/// Formats secondary metadata line (`subtitle · album`) and optional duration badge
/// for a rich [`SearchHistoryRecord`] card (`notes.md` #7).
pub fn format_recent_metadata(entry: &SearchHistoryRecord) -> (String, Option<String>) {
    let badge = match entry.entity_type {
        EntityType::Track => "Song",
        EntityType::Album => "Album",
        EntityType::Artist => "Artist",
        EntityType::Playlist => "Playlist",
        EntityType::Other => "Search",
    };

    let mut parts: Vec<&str> = Vec::new();
    let sub = entry.subtitle.trim();
    if !sub.is_empty() {
        parts.push(sub);
    }
    if let Some(ref album) = entry.album {
        let a = album.trim();
        if !a.is_empty() && !parts.iter().any(|p| p.eq_ignore_ascii_case(a)) {
            parts.push(a);
        }
    }
    let meta_line = if parts.is_empty() {
        badge.to_owned()
    } else {
        parts.join(" · ")
    };

    let duration_label = entry
        .duration_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| entry.duration_ms.filter(|&ms| ms > 0).map(format_time_ms));

    (meta_line, duration_label)
}

impl BitChordApp {
    /// Total selectable entries currently shown in the floating search suggestion dropdown.
    pub fn search_suggestion_total_count(&self) -> usize {
        self.search_suggestions.queries.len().min(6) + self.search_suggestions.items.len()
    }

    /// Moves keyboard highlight up/down inside the floating search suggestion dropdown.
    pub fn move_search_suggestion_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let total = self.search_suggestion_total_count();
        if !self.search_suggestions_open || total == 0 {
            self.search_suggest_selected_idx = None;
            return;
        }
        let next = match self.search_suggest_selected_idx {
            None if delta > 0 => 0,
            None => total - 1,
            Some(cur) => {
                let signed = cur as isize + delta;
                if signed < 0 {
                    total - 1
                } else if (signed as usize) >= total {
                    0
                } else {
                    signed as usize
                }
            }
        };
        self.search_suggest_selected_idx = Some(next);
        cx.notify();
    }

    /// Activates the currently highlighted floating search suggestion (if any).
    /// Returns `true` if a suggestion was activated, `false` to fall back to regular search submit.
    pub fn activate_selected_search_suggestion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(sel) = self.search_suggest_selected_idx else {
            return false;
        };
        if !self.search_suggestions_open {
            return false;
        }
        let query_count = self.search_suggestions.queries.len().min(6);
        if sel < query_count {
            if let Some(q) = self.search_suggestions.queries.get(sel).cloned() {
                self.search_input.update(cx, |inp, cx| inp.set_text(q.clone(), cx));
                self.submit_search(q, cx);
                return true;
            }
            return false;
        }
        let item_idx = sel - query_count;
        let Some(row) = self.search_suggestions.items.get(item_idx).cloned() else {
            return false;
        };
        self.search_suggestions_open = false;
        self.search_suggest_selected_idx = None;
        match row {
            SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                let suggested_songs: Vec<_> = self
                    .search_suggestions
                    .items
                    .iter()
                    .filter_map(|r| match r {
                        SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                            Some(song.clone())
                        }
                        SearchResult::Browse { .. } => None,
                    })
                    .collect();
                let songs = if suggested_songs.is_empty() {
                    vec![song.clone()]
                } else {
                    suggested_songs
                };
                let start_i = songs
                    .iter()
                    .position(|s| s.video_id == song.video_id)
                    .unwrap_or(0);
                self.record_song_search_selection(&song, cx);
                self.play_songs(songs, start_i, "Search", PlaybackSourceType::Search, None, cx);
                true
            }
            SearchResult::Browse { item } => {
                self.record_browse_search_selection(&item, cx);
                self.open_detail(
                    item.browse_id,
                    item.title,
                    item.subtitle,
                    item.r#type,
                    cx,
                );
                true
            }
        }
    }

    pub fn render_search(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let trimmed_input = self.search_input_text.trim();
        let has_suggestions = self.search_suggestions_open
            && trimmed_input.len() >= 2
            && (!self.search_suggestions.queries.is_empty()
                || !self.search_suggestions.items.is_empty());
        let loading_suggestions = self.search_suggestions_open
            && trimmed_input.len() >= 2
            && !has_suggestions
            && !self.search_suggestions.query.eq_ignore_ascii_case(trimmed_input);

        let queries = self.search_suggestions.queries.clone();
        let items = self.search_suggestions.items.clone();
        let query_count = queries.len().min(6);
        let selected_idx = self.search_suggest_selected_idx;

        div()
            .relative()
            .size_full()
            .px(px(28.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            // Search input bar + floating deferred suggestion overlay (`notes.md` #7)
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().min_w(px(0.0)).child(self.search_input.clone()))
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
                    )
                    // Floating Suggestion Dropdown (renders cleanly above filter chips & results)
                    .when(has_suggestions || loading_suggestions, |el| {
                        let suggested_songs: Vec<_> = items
                            .iter()
                            .filter_map(|r| match r {
                                SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                                    Some(song.clone())
                                }
                                SearchResult::Browse { .. } => None,
                            })
                            .collect();

                        let dropdown = div()
                            .id("search-floating-dropdown")
                            .absolute()
                            .top(px(46.0))
                            .left_0()
                            .right_0()
                            .max_h(px(440.0))
                            .overflow_y_scroll()
                            .p(px(12.0))
                            .rounded(px(12.0))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .shadow_xl()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .when(loading_suggestions, |box_el| {
                                box_el.child(
                                    div()
                                        .px(px(6.0))
                                        .py(px(8.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .text_size(px(12.5))
                                        .text_color(theme.text_muted)
                                        .child(icons::icon(icons::SEARCH, 14.0, theme.text_muted))
                                        .child("Finding matching songs and queries..."),
                                )
                            })
                            .when(!queries.is_empty(), |box_el| {
                                box_el.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.0))
                                        .child(
                                            div()
                                                .px(px(6.0))
                                                .pb(px(2.0))
                                                .text_size(px(10.5))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.text_muted)
                                                .child("SUGGESTED QUERIES"),
                                        )
                                        .children(queries.into_iter().take(6).enumerate().map(
                                            |(idx, suggestion)| {
                                                let s_click = suggestion.clone();
                                                let s_fill = suggestion.clone();
                                                let is_sel = selected_idx == Some(idx);
                                                div()
                                                    .id(("suggest-query", idx))
                                                    .w_full()
                                                    .px(px(8.0))
                                                    .py(px(6.0))
                                                    .rounded(px(8.0))
                                                    .bg(if is_sel {
                                                        theme.surface_hover
                                                    } else {
                                                        gpui::transparent_black()
                                                    })
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap(px(8.0))
                                                    .cursor_pointer()
                                                    .hover(move |s| s.bg(theme.surface_hover))
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.search_input.update(
                                                                cx,
                                                                |inp, cx| {
                                                                    inp.set_text(
                                                                        s_click.clone(),
                                                                        cx,
                                                                    );
                                                                },
                                                            );
                                                            this.submit_search(s_click.clone(), cx);
                                                        },
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(9.0))
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .child(icons::icon(
                                                                icons::SEARCH,
                                                                13.5,
                                                                if is_sel {
                                                                    theme.accent
                                                                } else {
                                                                    theme.text_muted
                                                                },
                                                            ))
                                                            .child(
                                                                div()
                                                                    .text_size(px(13.0))
                                                                    .text_color(theme.text_primary)
                                                                    .truncate()
                                                                    .child(suggestion),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(("suggest-fill", idx))
                                                            .w(px(24.0))
                                                            .h(px(24.0))
                                                            .rounded(px(6.0))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .hover(move |s| {
                                                                s.bg(theme.surface_active)
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.search_input.update(
                                                                        cx,
                                                                        |inp, cx| {
                                                                            inp.set_text(
                                                                                s_fill.clone(),
                                                                                cx,
                                                                            );
                                                                        },
                                                                    );
                                                                    this.on_search_query_changed(
                                                                        s_fill.clone(),
                                                                        cx,
                                                                    );
                                                                    cx.stop_propagation();
                                                                },
                                                            ))
                                                            .child(icons::icon(
                                                                icons::ARROW_UP_LEFT,
                                                                13.0,
                                                                theme.text_muted,
                                                            )),
                                                    )
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
                                                .px(px(6.0))
                                                .pb(px(2.0))
                                                .text_size(px(10.5))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.text_muted)
                                                .child("SONGS, ARTISTS & ALBUMS"),
                                        )
                                        .children(items.into_iter().enumerate().map(
                                            |(idx, row)| {
                                                let flat_idx = query_count + idx;
                                                let is_sel = selected_idx == Some(flat_idx);
                                                match row {
                                                    SearchResult::TopTrack { song }
                                                    | SearchResult::Track { song } => {
                                                        let art = song_thumbnail_url(&song);
                                                        let songs_c = if suggested_songs.is_empty()
                                                        {
                                                            vec![song.clone()]
                                                        } else {
                                                            suggested_songs.clone()
                                                        };
                                                        let start_i = songs_c
                                                            .iter()
                                                            .position(|s| {
                                                                s.video_id == song.video_id
                                                            })
                                                            .unwrap_or(0);
                                                        let rec_song = song.clone();
                                                        let subtitle = {
                                                            let mut parts = vec![
                                                                "Song".to_owned(),
                                                                song.artist.clone(),
                                                            ];
                                                            if let Some(ref album) = song.album_name
                                                                && !album.trim().is_empty()
                                                            {
                                                                parts.push(album.clone());
                                                            }
                                                            parts.join(" · ")
                                                        };
                                                        let dur_text = song.duration_text.clone();
                                                        div()
                                                            .id(("suggest-track", idx))
                                                            .w_full()
                                                            .px(px(8.0))
                                                            .py(px(6.0))
                                                            .rounded(px(8.0))
                                                            .bg(if is_sel {
                                                                theme.surface_hover
                                                            } else {
                                                                gpui::transparent_black()
                                                            })
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap(px(10.0))
                                                            .cursor_pointer()
                                                            .hover(move |s| {
                                                                s.bg(theme.surface_hover)
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.search_suggestions_open =
                                                                        false;
                                                                    this.search_suggest_selected_idx =
                                                                        None;
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
                                                                },
                                                            ))
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap(px(10.0))
                                                                    .flex_1()
                                                                    .min_w(px(0.0))
                                                                    .child(self.render_art_box(
                                                                        art.as_deref(),
                                                                        42.0,
                                                                        7.0,
                                                                        icons::MUSIC_NOTE,
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
                                                                                    .text_size(px(
                                                                                        13.0,
                                                                                    ))
                                                                                    .font_weight(
                                                                                        FontWeight::MEDIUM,
                                                                                    )
                                                                                    .text_color(
                                                                                        theme.text_primary,
                                                                                    )
                                                                                    .truncate()
                                                                                    .child(
                                                                                        song.title,
                                                                                    ),
                                                                            )
                                                                            .child(
                                                                                div()
                                                                                    .text_size(px(
                                                                                        11.5,
                                                                                    ))
                                                                                    .text_color(
                                                                                        theme.text_muted,
                                                                                    )
                                                                                    .truncate()
                                                                                    .child(
                                                                                        subtitle,
                                                                                    ),
                                                                            ),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap(px(8.0))
                                                                    .flex_shrink_0()
                                                                    .when_some(
                                                                        dur_text.filter(|d| {
                                                                            !d.is_empty()
                                                                        }),
                                                                        |d_el, d| {
                                                                            d_el.child(
                                                                                div()
                                                                                    .text_size(px(
                                                                                        11.0,
                                                                                    ))
                                                                                    .text_color(
                                                                                        theme.text_muted,
                                                                                    )
                                                                                    .child(d),
                                                                            )
                                                                        },
                                                                    )
                                                                    .child(icons::icon(
                                                                        icons::PLAY,
                                                                        13.0,
                                                                        if is_sel {
                                                                            theme.accent
                                                                        } else {
                                                                            theme.text_muted
                                                                        },
                                                                    )),
                                                            )
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
                                                            .bg(if is_sel {
                                                                theme.surface_hover
                                                            } else {
                                                                gpui::transparent_black()
                                                            })
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap(px(10.0))
                                                            .cursor_pointer()
                                                            .hover(move |s| {
                                                                s.bg(theme.surface_hover)
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.search_suggestions_open =
                                                                        false;
                                                                    this.search_suggest_selected_idx =
                                                                        None;
                                                                    this.record_browse_search_selection(
                                                                        &b, cx,
                                                                    );
                                                                    this.open_detail(
                                                                        b.browse_id.clone(),
                                                                        b.title.clone(),
                                                                        b.subtitle.clone(),
                                                                        b.r#type,
                                                                        cx,
                                                                    );
                                                                },
                                                            ))
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap(px(10.0))
                                                                    .flex_1()
                                                                    .min_w(px(0.0))
                                                                    .child(self.render_art_box(
                                                                        item.thumbnail_url
                                                                            .as_deref(),
                                                                        42.0,
                                                                        7.0,
                                                                        icons::DISC,
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
                                                                                    .text_size(px(
                                                                                        13.0,
                                                                                    ))
                                                                                    .font_weight(
                                                                                        FontWeight::MEDIUM,
                                                                                    )
                                                                                    .text_color(
                                                                                        theme.text_primary,
                                                                                    )
                                                                                    .truncate()
                                                                                    .child(
                                                                                        item.title,
                                                                                    ),
                                                                            )
                                                                            .child(
                                                                                div()
                                                                                    .text_size(px(
                                                                                        11.5,
                                                                                    ))
                                                                                    .text_color(
                                                                                        theme.text_muted,
                                                                                    )
                                                                                    .truncate()
                                                                                    .child(
                                                                                        item.subtitle,
                                                                                    ),
                                                                            ),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .px(px(7.0))
                                                                    .py(px(2.0))
                                                                    .rounded(px(5.0))
                                                                    .bg(theme.surface_elevated)
                                                                    .text_size(px(10.5))
                                                                    .text_color(
                                                                        theme.text_secondary,
                                                                    )
                                                                    .child(format!(
                                                                        "{:?}",
                                                                        item.r#type
                                                                    )),
                                                            )
                                                            .into_any_element()
                                                    }
                                                }
                                            },
                                        )),
                                )
                            });

                        el.child(deferred(dropdown).priority(10))
                    }),
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
                    let history: Vec<SearchHistoryRecord> = self
                        .library
                        .search_history
                        .iter()
                        .cloned()
                        .map(SearchHistoryRecord::with_corrected_type)
                        .collect();
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
                                        "Start typing to see instant song & artist suggestions, use ↑/↓ to navigate, or press Enter for full results.",
                                    ),
                            )
                            .into_any_element()
                    } else {
                        let recent_queries: Vec<SearchHistoryRecord> =
                            history.iter().filter(|e| e.is_query()).cloned().collect();
                        let recent_entities: Vec<SearchHistoryRecord> =
                            history.iter().filter(|e| !e.is_query()).cloned().collect();

                        div()
                            .id("recent-searches-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(14.0))
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
                            // Compact chips for raw text query history
                            .when(!recent_queries.is_empty(), |el| {
                                el.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap(px(8.0))
                                        .children(recent_queries.into_iter().enumerate().map(
                                            |(idx, q_entry)| {
                                                let q_click = q_entry.title.clone();
                                                let rm_id = q_entry.id.clone();
                                                div()
                                                    .id(("recent-query-chip", idx))
                                                    .h(px(32.0))
                                                    .pl(px(10.0))
                                                    .pr(px(6.0))
                                                    .rounded_full()
                                                    .bg(theme.surface_elevated)
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(7.0))
                                                    .cursor_pointer()
                                                    .text_size(px(12.5))
                                                    .text_color(theme.text_primary)
                                                    .hover(move |s| s.bg(theme.surface_hover))
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.search_input.update(
                                                                cx,
                                                                |inp, cx| {
                                                                    inp.set_text(
                                                                        q_click.clone(),
                                                                        cx,
                                                                    );
                                                                },
                                                            );
                                                            this.submit_search(q_click.clone(), cx);
                                                        },
                                                    ))
                                                    .child(icons::icon(
                                                        icons::HISTORY,
                                                        13.0,
                                                        theme.text_muted,
                                                    ))
                                                    .child(q_entry.title)
                                                    .child(
                                                        div()
                                                            .id(("recent-q-rm", idx))
                                                            .w(px(20.0))
                                                            .h(px(20.0))
                                                            .rounded_full()
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
                                                                11.0,
                                                                theme.text_muted,
                                                            )),
                                                    )
                                            },
                                        )),
                                )
                            })
                            // Rich media cards for played songs & opened albums/artists/playlists
                            .when(!recent_entities.is_empty(), |el| {
                                el.child(div().flex().flex_col().gap(px(6.0)).children(
                                    recent_entities.into_iter().enumerate().map(|(idx, entry)| {
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
                                        let maybe_song = entry.to_song();
                                        let is_liked = maybe_song
                                            .as_ref()
                                            .is_some_and(|s| self.is_song_liked(&s.video_id));
                                        let art_url = entry.artwork_url.clone().or_else(|| {
                                            maybe_song.as_ref().and_then(song_thumbnail_url)
                                        });
                                        let (meta_line, duration_label) =
                                            format_recent_metadata(&entry);
                                        let click_entry = entry.clone();
                                        let like_song = maybe_song.clone();

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
                                                ) {
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
                                                            .gap(px(3.0))
                                                            .child(
                                                                div()
                                                                    .text_size(px(13.5))
                                                                    .font_weight(
                                                                        FontWeight::SEMIBOLD,
                                                                    )
                                                                    .text_color(theme.text_primary)
                                                                    .truncate()
                                                                    .child(entry.title),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(12.0))
                                                                    .text_color(theme.text_muted)
                                                                    .truncate()
                                                                    .child(meta_line),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(8.0))
                                                    .flex_shrink_0()
                                                    .when_some(duration_label, |d_el, dur| {
                                                        d_el.child(
                                                            div()
                                                                .flex()
                                                                .items_center()
                                                                .gap(px(4.0))
                                                                .text_size(px(11.5))
                                                                .text_color(theme.text_muted)
                                                                .child(icons::icon(
                                                                    icons::CLOCK,
                                                                    12.0,
                                                                    theme.text_muted,
                                                                ))
                                                                .child(dur),
                                                        )
                                                    })
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
                                                    .when_some(like_song, |l_el, song_to_like| {
                                                        l_el.child(
                                                            div()
                                                                .id(("recent-like", idx))
                                                                .w(px(28.0))
                                                                .h(px(28.0))
                                                                .rounded(px(6.0))
                                                                .flex()
                                                                .items_center()
                                                                .justify_center()
                                                                .hover(move |s| {
                                                                    s.bg(theme.surface_active)
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.toggle_like_song(
                                                                            song_to_like.clone(),
                                                                            cx,
                                                                        );
                                                                        cx.stop_propagation();
                                                                    },
                                                                ))
                                                                .child(icons::icon(
                                                                    if is_liked {
                                                                        icons::HEART_FILLED
                                                                    } else {
                                                                        icons::HEART
                                                                    },
                                                                    14.0,
                                                                    if is_liked {
                                                                        theme.accent
                                                                    } else {
                                                                        theme.text_muted
                                                                    },
                                                                )),
                                                        )
                                                    })
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
                            })
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
                                        let is_liked = this.is_song_liked(&song.video_id);
                                        let songs_c = playable_songs.clone();
                                        let start_idx = songs_c
                                            .iter()
                                            .position(|s| s.video_id == song.video_id)
                                            .unwrap_or(0);
                                        let next_s = song.clone();
                                        let like_s = song.clone();
                                        let rec_song = song.clone();
                                        let art = song_thumbnail_url(&song);
                                        let sub_text = match &song.album_name {
                                            Some(al) if !al.trim().is_empty() => {
                                                format!("{} · {al}", song.artist)
                                            }
                                            _ => song.artist.clone(),
                                        };

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
                                                                    .truncate()
                                                                    .child(song.title.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(12.0))
                                                                    .text_color(t.text_muted)
                                                                    .truncate()
                                                                    .child(sub_text),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(8.0))
                                                    .flex_shrink_0()
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
                                                            .id(("search-like", idx))
                                                            .w(px(28.0))
                                                            .h(px(28.0))
                                                            .rounded(px(7.0))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .hover(move |s| s.bg(t.surface_active))
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.toggle_like_song(
                                                                        like_s.clone(),
                                                                        cx,
                                                                    );
                                                                    cx.stop_propagation();
                                                                },
                                                            ))
                                                            .child(icons::icon(
                                                                if is_liked {
                                                                    icons::HEART_FILLED
                                                                } else {
                                                                    icons::HEART
                                                                },
                                                                14.0,
                                                                if is_liked {
                                                                    t.accent
                                                                } else {
                                                                    t.text_muted
                                                                },
                                                            )),
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
                                                                icons::LIST_PLUS,
                                                                12.5,
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
                                                                    .truncate()
                                                                    .child(item.title),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(12.0))
                                                                    .text_color(t.text_muted)
                                                                    .truncate()
                                                                    .child(item.subtitle),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_shrink_0()
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

#[cfg(test)]
mod tests {
    use bitchord_model::Song;

    use super::*;

    #[test]
    fn recent_metadata_joins_artist_album_and_formats_duration() {
        let mut song = Song::new("abc123", "Midnight City", "M83", None);
        song.album_name = Some("Hurry Up, We're Dreaming".to_owned());
        song.duration_text = Some("4:03".to_owned());

        let record = SearchHistoryRecord::from_song(&song, 100);
        let (meta, dur) = format_recent_metadata(&record);
        assert_eq!(meta, "M83 · Hurry Up, We're Dreaming");
        assert_eq!(dur.as_deref(), Some("4:03"));

        // Falls back to duration_ms when duration_text is missing.
        let rec2 = SearchHistoryRecord {
            id: "xyz".to_owned(),
            title: "Track".to_owned(),
            subtitle: "Artist".to_owned(),
            artwork_url: None,
            entity_type: EntityType::Track,
            timestamp: 1,
            album: Some("Artist".to_owned()), // duplicate of subtitle should not repeat
            duration_text: None,
            duration_ms: Some(185_000),
        };
        let (meta2, dur2) = format_recent_metadata(&rec2);
        assert_eq!(meta2, "Artist");
        assert_eq!(dur2.as_deref(), Some("3:05"));
    }
}
