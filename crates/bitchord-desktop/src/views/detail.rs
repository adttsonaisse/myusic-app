//! Album, Playlist, and Artist detail screen (`W4-2`) with `uniform_list`
//! virtualized tracks, thumbnails, Play All, and Shuffle triggers with vector SVG icons.

use bitchord_model::PlaybackSourceType;
use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px, uniform_list};

use crate::app::{BitChordApp, LoadState};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;

impl BitChordApp {
    pub fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();

        match &self.detail {
            LoadState::Idle | LoadState::Loading => div()
                .size_full()
                .p(px(28.0))
                .text_size(px(14.0))
                .text_color(theme.text_secondary)
                .child("Loading collection tracks...")
                .into_any_element(),
            LoadState::Error(err) => div()
                .size_full()
                .p(px(28.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.danger)
                        .child("Could not load details"),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text_secondary)
                        .child(err.clone()),
                )
                .into_any_element(),
            LoadState::Ready(payload) => {
                let title = payload
                    .header
                    .as_ref()
                    .map(|h| h.title.clone())
                    .unwrap_or_else(|| payload.browse_id.clone());
                let subtitle = payload.subtitle.clone();
                let thumb = payload
                    .header
                    .as_ref()
                    .and_then(|h| h.thumbnail_url.clone())
                    .or_else(|| payload.songs.first().and_then(song_thumbnail_url));
                let songs = payload.songs.clone();
                let track_count = songs.len();
                let browse_id = payload.browse_id.clone();

                div()
                    .size_full()
                    .px(px(28.0))
                    .py(px(20.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    // Detail header
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(20.0))
                            .child(self.render_art_box(
                                thumb.as_deref(),
                                136.0,
                                12.0,
                                icons::DISC,
                                theme,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .text_size(px(24.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .text_color(theme.text_secondary)
                                            .child(format!("{subtitle} · {track_count} tracks")),
                                    )
                                    .child(
                                        div()
                                            .pt(px(6.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(10.0))
                                            .child(
                                                div()
                                                    .id("detail-play-all")
                                                    .h(px(36.0))
                                                    .px(px(16.0))
                                                    .rounded(px(9.0))
                                                    .bg(theme.accent)
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(7.0))
                                                    .cursor_pointer()
                                                    .text_size(px(13.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.on_accent)
                                                    .on_click(cx.listener({
                                                        let songs = songs.clone();
                                                        let title = title.clone();
                                                        let bid = browse_id.clone();
                                                        move |this, _, _, cx| {
                                                            this.play_songs(
                                                                songs.clone(),
                                                                0,
                                                                title.clone(),
                                                                PlaybackSourceType::Browse,
                                                                Some(bid.clone()),
                                                                cx,
                                                            );
                                                        }
                                                    }))
                                                    .child(icons::icon(
                                                        icons::PLAY,
                                                        14.0,
                                                        theme.on_accent,
                                                    ))
                                                    .child("Play All"),
                                            )
                                            .child(
                                                div()
                                                    .id("detail-shuffle")
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
                                                    .text_color(theme.text_primary)
                                                    .hover(move |s| s.bg(theme.surface_hover))
                                                    .on_click(cx.listener({
                                                        let songs = songs.clone();
                                                        let title = title.clone();
                                                        let bid = browse_id.clone();
                                                        move |this, _, _, cx| {
                                                            this.play_songs(
                                                                songs.clone(),
                                                                0,
                                                                title.clone(),
                                                                PlaybackSourceType::Browse,
                                                                Some(bid.clone()),
                                                                cx,
                                                            );
                                                            this.send(
                                                                BackendCommand::ToggleShuffle,
                                                                cx,
                                                            );
                                                        }
                                                    }))
                                                    .child(icons::icon(
                                                        icons::SHUFFLE,
                                                        14.5,
                                                        theme.text_secondary,
                                                    ))
                                                    .child("Shuffle"),
                                            ),
                                    ),
                            ),
                    )
                    // Track list (virtualized via uniform_list)
                    .child(if track_count == 0 {
                        div()
                            .p(px(20.0))
                            .rounded(px(10.0))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("No playable tracks were returned for this item.")
                            .into_any_element()
                    } else {
                        uniform_list(
                            "detail-tracks",
                            track_count,
                            cx.processor(
                                move |this, range: std::ops::Range<usize>, _window, cx| {
                                    let t = this.theme();
                                    let LoadState::Ready(ref p) = this.detail else {
                                        return Vec::new();
                                    };
                                    let all_songs = p.songs.clone();
                                    let src_title = p
                                        .header
                                        .as_ref()
                                        .map(|h| h.title.clone())
                                        .unwrap_or_else(|| p.browse_id.clone());
                                    let bid = p.browse_id.clone();
                                    let active_vid = this
                                        .playback
                                        .current_song
                                        .as_ref()
                                        .map(|s| s.video_id.as_str());

                                    range
                                        .filter_map(|idx| {
                                            all_songs.get(idx).map(|s| (idx, s.clone()))
                                        })
                                        .map(|(idx, song)| {
                                            let is_current =
                                                active_vid == Some(song.video_id.as_str());
                                            let play_list = all_songs.clone();
                                            let st = src_title.clone();
                                            let bid_c = bid.clone();
                                            let next_song = song.clone();
                                            let art = song_thumbnail_url(&song);

                                            div()
                                                .id(("detail-row", idx))
                                                .w_full()
                                                .h(px(58.0))
                                                .px(px(12.0))
                                                .rounded(px(10.0))
                                                .bg(if is_current {
                                                    t.surface_elevated
                                                } else {
                                                    gpui::transparent_black()
                                                })
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(12.0))
                                                .cursor_pointer()
                                                .hover(move |st| st.bg(t.surface_hover))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.play_songs(
                                                        play_list.clone(),
                                                        idx,
                                                        st.clone(),
                                                        PlaybackSourceType::Browse,
                                                        Some(bid_c.clone()),
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
                                                        .child(
                                                            div()
                                                                .w(px(22.0))
                                                                .text_size(px(12.0))
                                                                .text_color(if is_current {
                                                                    t.accent
                                                                } else {
                                                                    t.text_muted
                                                                })
                                                                .child(format!("{}", idx + 1)),
                                                        )
                                                        .child(this.render_art_box(
                                                            art.as_deref(),
                                                            40.0,
                                                            6.0,
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
                                                                        .font_weight(
                                                                            if is_current {
                                                                                FontWeight::SEMIBOLD
                                                                            } else {
                                                                                FontWeight::NORMAL
                                                                            },
                                                                        )
                                                                        .text_color(if is_current {
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
                                                                .id(("detail-next", idx))
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
                                                                .hover(move |st| {
                                                                    st.bg(t.surface_active)
                                                                        .text_color(t.text_primary)
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.send(
                                                                            BackendCommand::PlayNext(
                                                                                Box::new(
                                                                                    next_song
                                                                                        .clone(),
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
                                        })
                                        .collect()
                                },
                            ),
                        )
                        .flex_1()
                        .w_full()
                        .into_any_element()
                    })
                    .into_any_element()
            }
        }
    }
}
