//! Home feed screen (`W4-2`) showing live YouTube Music InnerTube shelves
//! plus recent local library tracks with vector SVG icons, automatic lazy
//! cover-art loading, and distinct loading, empty, and error states.

use bitchord_model::{BrowseType, PlaybackSourceType, Song};
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};

use crate::actions::Route;
use crate::app::{BitChordApp, LoadState};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;

impl BitChordApp {
    pub fn render_home(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();

        div()
            .id("home-scroll")
            .size_full()
            .overflow_y_scroll()
            .px(px(28.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(24.0))
            // Header row with explicit refresh & local library actions
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(26.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.text_primary)
                                    .child("Listen Now"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.text_muted)
                                    .child("YouTube Music feed and local library"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("home-refresh")
                                    .h(px(36.0))
                                    .px(px(14.0))
                                    .rounded(px(9.0))
                                    .bg(theme.surface_elevated)
                                    .border_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .cursor_pointer()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text_primary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::LoadHome, cx);
                                    }))
                                    .child(icons::icon(icons::REFRESH, 14.5, theme.text_secondary))
                                    .child("Refresh Feed"),
                            )
                            .child(
                                div()
                                    .id("home-open-library")
                                    .h(px(36.0))
                                    .px(px(14.0))
                                    .rounded(px(9.0))
                                    .bg(theme.accent)
                                    .flex()
                                    .items_center()
                                    .gap(px(7.0))
                                    .cursor_pointer()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.on_accent)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.navigate(Route::Library, cx);
                                    }))
                                    .child(icons::icon(icons::LIBRARY, 14.5, theme.on_accent))
                                    .child("Local Music"),
                            ),
                    ),
            )
            // Recent Local / Cached Tracks strip (only shown when real tracks exist)
            .when(!self.library.songs.is_empty(), |el| {
                let songs = self.library.songs.clone();
                let preview: Vec<_> = songs.iter().take(6).cloned().collect();
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(px(17.5))
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.text_primary)
                                .child("Recent & Local Library"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(10.0))
                                .children(preview.into_iter().enumerate().map(|(idx, song)| {
                                    let all = songs.clone();
                                    let title = song.title.clone();
                                    let artist = song.artist.clone();
                                    let art_url = song_thumbnail_url(&song);
                                    div()
                                        .id(("home-local-card", idx))
                                        .w(px(286.0))
                                        .p(px(10.0))
                                        .rounded(px(12.0))
                                        .bg(theme.surface)
                                        .border_1()
                                        .border_color(theme.border)
                                        .flex()
                                        .items_center()
                                        .gap(px(12.0))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.play_songs(
                                                all.clone(),
                                                idx,
                                                "Library",
                                                PlaybackSourceType::History,
                                                None,
                                                cx,
                                            );
                                        }))
                                        .child(self.render_art_box(
                                            art_url.as_deref(),
                                            48.0,
                                            8.0,
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
                                                        .text_size(px(13.5))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(theme.text_primary)
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .child(title),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(12.0))
                                                        .text_color(theme.text_muted)
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .child(artist),
                                                ),
                                        )
                                        .child(icons::icon(icons::PLAY, 13.0, theme.text_muted))
                                })),
                        ),
                )
            })
            // Remote YouTube Music Home Shelves (Loading / Error / Empty / Populated)
            .child(match &self.home {
                LoadState::Idle | LoadState::Loading => div()
                    .p(px(24.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(13.5))
                    .text_color(theme.text_secondary)
                    .child("Loading YouTube Music home shelves...")
                    .into_any_element(),
                LoadState::Error(err) => div()
                    .p(px(20.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.danger)
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.danger)
                            .child("Could not reach YouTube Music Home"),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme.text_secondary)
                            .child(err.clone()),
                    )
                    .child(
                        div()
                            .id("home-retry")
                            .w(px(110.0))
                            .h(px(34.0))
                            .rounded(px(8.0))
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(theme.text_primary)
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send(BackendCommand::LoadHome, cx);
                            }))
                            .child(icons::icon(icons::REFRESH, 13.5, theme.text_primary))
                            .child("Retry"),
                    )
                    .into_any_element(),
                LoadState::Ready(feed) if feed.shelves.is_empty() => div()
                    .p(px(24.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_primary)
                            .child("Home feed returned no shelves"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Search YouTube Music directly or scan a local music folder in Library."),
                    )
                    .into_any_element(),
                LoadState::Ready(feed) => div()
                    .flex()
                    .flex_col()
                    .gap(px(26.0))
                    .children(feed.shelves.iter().enumerate().map(|(s_idx, shelf)| {
                        let shelf_title = shelf.title.clone();
                        let shelf_songs: Vec<Song> = shelf
                            .items
                            .iter()
                            .filter_map(|it| {
                                it.video_id.as_ref().map(|vid| {
                                    Song::new(
                                        vid.clone(),
                                        it.title.clone(),
                                        it.subtitle.clone(),
                                        it.thumbnail_url.clone(),
                                    )
                                })
                            })
                            .collect();

                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.text_primary)
                                    .child(shelf_title.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(px(14.0))
                                    .children(shelf.items.iter().take(10).enumerate().map(
                                        |(i_idx, item)| {
                                            if let Some(ref vid) = item.video_id {
                                                let s = Song::new(
                                                    vid.clone(),
                                                    item.title.clone(),
                                                    item.subtitle.clone(),
                                                    item.thumbnail_url.clone(),
                                                );
                                                let art = song_thumbnail_url(&s);
                                                let list_for_play = if shelf_songs.is_empty() {
                                                    vec![s.clone()]
                                                } else {
                                                    shelf_songs.clone()
                                                };
                                                let start_i = list_for_play
                                                    .iter()
                                                    .position(|x| x.video_id == s.video_id)
                                                    .unwrap_or(0);
                                                let src_title = shelf_title.clone();
                                                div()
                                                    .id(("home-track", s_idx * 100 + i_idx))
                                                    .w(px(166.0))
                                                    .p(px(11.0))
                                                    .rounded(px(12.0))
                                                    .bg(theme.surface)
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(8.0))
                                                    .cursor_pointer()
                                                    .hover(move |st| st.bg(theme.surface_hover))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.play_songs(
                                                            list_for_play.clone(),
                                                            start_i,
                                                            src_title.clone(),
                                                            PlaybackSourceType::Home,
                                                            None,
                                                            cx,
                                                        );
                                                    }))
                                                    .child(self.render_art_box(
                                                        art.as_deref(),
                                                        142.0,
                                                        9.0,
                                                        icons::MUSIC_NOTE,
                                                        theme,
                                                    ))
                                                    .child(
                                                        div()
                                                            .text_size(px(13.0))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(theme.text_primary)
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .child(s.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(11.5))
                                                            .text_color(theme.text_muted)
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .child(s.artist.clone()),
                                                    )
                                                    .into_any_element()
                                            } else {
                                                let bid = item.browse_id.clone().unwrap_or_default();
                                                let title = item.title.clone();
                                                let subtitle = item.subtitle.clone();
                                                let kind = if bid.starts_with("UC") {
                                                    BrowseType::Artist
                                                } else if bid.starts_with("MPREb") {
                                                    BrowseType::Album
                                                } else {
                                                    BrowseType::Playlist
                                                };
                                                div()
                                                    .id(("home-browse", s_idx * 100 + i_idx))
                                                    .w(px(166.0))
                                                    .p(px(11.0))
                                                    .rounded(px(12.0))
                                                    .bg(theme.surface)
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(8.0))
                                                    .cursor_pointer()
                                                    .hover(move |st| st.bg(theme.surface_hover))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if !bid.is_empty() {
                                                            this.open_detail(
                                                                bid.clone(),
                                                                title.clone(),
                                                                subtitle.clone(),
                                                                kind,
                                                                cx,
                                                            );
                                                        }
                                                    }))
                                                    .child(self.render_art_box(
                                                        item.thumbnail_url.as_deref(),
                                                        142.0,
                                                        9.0,
                                                        icons::DISC,
                                                        theme,
                                                    ))
                                                    .child(
                                                        div()
                                                            .text_size(px(13.0))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(theme.text_primary)
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .child(item.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(11.5))
                                                            .text_color(theme.text_muted)
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .child(item.subtitle.clone()),
                                                    )
                                                    .into_any_element()
                                            }
                                        },
                                    )),
                            )
                    }))
                    .into_any_element(),
            })
    }
}
