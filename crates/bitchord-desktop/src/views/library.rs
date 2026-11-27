//! Daily-driver Library & Offline Downloads screens backed by SQLite v5
//! `LibraryDb`, FTS5 instant search, Liked Songs, Local Playlists CRUD/reorder,
//! `lofty` local tag scanning, and live `DownloadStore` progress tracking.

use std::collections::HashMap;

use bitchord_model::{PlaybackSourceType, Song};
use bitchord_storage::SavedSongMetadata;
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, div, px, relative, uniform_list};

use crate::app::{BitChordApp, LibrarySourceFilter, LibraryTab, LoadState};
use crate::artwork::song_thumbnail_url;
use crate::backend::{BackendCommand, DownloadProgress, DownloadStatus};
use crate::icons;
use crate::views::track_actions::format_download_progress_label;

/// Deterministically orders active download tasks (`Downloading` -> `Queued` -> `Failed` -> `Completed`,
/// then alphabetically by title & video_id) so UI rows never jitter on progress ticks.
pub fn sort_active_downloads(map: &HashMap<String, DownloadProgress>) -> Vec<DownloadProgress> {
    let mut list: Vec<DownloadProgress> = map.values().cloned().collect();
    list.sort_by(|a, b| {
        let rank = |st: &DownloadStatus| match st {
            DownloadStatus::Downloading => 0u8,
            DownloadStatus::Queued => 1u8,
            DownloadStatus::Failed(_) => 2u8,
            DownloadStatus::Completed => 3u8,
        };
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| a.video_id.cmp(&b.video_id))
    });
    list
}

/// Converts a persisted offline [`SavedSongMetadata`] record into a playable [`Song`].
/// Keeps `local_path` as `None` so playback does not pollute `songs.local_path` in SQLite;
/// `prepare_track_decoder` automatically prefers the offline file from `downloads` before
/// touching the network.
pub fn download_record_to_song(rec: &SavedSongMetadata) -> Song {
    let mut song = Song::new(
        rec.video_id.clone(),
        rec.title.clone(),
        rec.artist.clone(),
        rec.thumbnail_url.clone(),
    );
    song.album_name = rec.album_name.clone();
    song.duration_text = rec.duration_text.clone();
    song
}

/// Validates and computes a playlist track reorder target index (`delta = -1` for up, `+1` for down).
pub fn playlist_reorder_target(index: usize, len: usize, delta: isize) -> Option<usize> {
    if len < 2 || index >= len {
        return None;
    }
    let next = index as isize + delta;
    if next < 0 || (next as usize) >= len {
        None
    } else {
        Some(next as usize)
    }
}

impl BitChordApp {
    pub fn render_library(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let stats = self.library.stats;
        let active_tab = self.library_tab;
        let active_dl_count = self.active_downloads.len();

        div()
            .size_full()
            .px(px(28.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            // Header + real SQLite counts
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.0))
                                    .child(icons::icon(icons::LIBRARY, 22.0, theme.accent))
                                    .child(
                                        div()
                                            .text_size(px(24.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child("Library"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(theme.text_muted)
                                    .child(format!(
                                        "{} songs · {} liked · {} playlists · {} offline",
                                        stats.songs,
                                        self.library.liked_songs.len(),
                                        self.library.playlist_summaries.len(),
                                        self.library.downloads.len()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("lib-refresh")
                            .h(px(34.0))
                            .px(px(13.0))
                            .rounded(px(8.0))
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(theme.text_primary)
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.send(BackendCommand::RefreshLibrary, cx);
                            }))
                            .child(icons::icon(icons::REFRESH, 14.0, theme.text_secondary))
                            .child("Refresh"),
                    ),
            )
            // Primary Library section tabs: Songs / Liked Songs / Playlists / Downloads
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .children(LibraryTab::ALL.into_iter().map(|tab| {
                        let is_sel = tab == active_tab;
                        let count = match tab {
                            LibraryTab::AllSongs => self.library.songs.len(),
                            LibraryTab::Liked => self.library.liked_songs.len(),
                            LibraryTab::Playlists => self.library.playlist_summaries.len(),
                            LibraryTab::Downloads => self.library.downloads.len(),
                        };
                        let icon_bytes = match tab {
                            LibraryTab::AllSongs => icons::MUSIC_NOTE,
                            LibraryTab::Liked => icons::HEART_FILLED,
                            LibraryTab::Playlists => icons::DISC,
                            LibraryTab::Downloads => icons::DOWNLOAD,
                        };
                        let show_active_dl = tab == LibraryTab::Downloads && active_dl_count > 0;

                        div()
                            .id(("lib-tab", tab as usize))
                            .h(px(34.0))
                            .px(px(13.0))
                            .rounded(px(9.0))
                            .bg(if is_sel {
                                theme.accent_soft
                            } else {
                                theme.surface_elevated
                            })
                            .border_1()
                            .border_color(if is_sel { theme.accent } else { theme.border })
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .font_weight(if is_sel {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .text_color(if is_sel {
                                theme.accent
                            } else {
                                theme.text_secondary
                            })
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.library_tab = tab;
                                cx.notify();
                            }))
                            .child(icons::icon(
                                icon_bytes,
                                13.5,
                                if is_sel {
                                    theme.accent
                                } else {
                                    theme.text_muted
                                },
                            ))
                            .child(format!("{} ({count})", tab.label()))
                            .when(show_active_dl, |el| {
                                el.child(
                                    div()
                                        .px(px(5.0))
                                        .py(px(1.0))
                                        .rounded_full()
                                        .bg(theme.accent)
                                        .text_size(px(10.0))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme.on_accent)
                                        .child(format!("{active_dl_count} active")),
                                )
                            })
                    })),
            )
            // Active tab body
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(match active_tab {
                        LibraryTab::AllSongs => self.render_library_songs_tab(cx).into_any_element(),
                        LibraryTab::Liked => self.render_library_liked_tab(cx).into_any_element(),
                        LibraryTab::Playlists => {
                            self.render_library_playlists_tab(cx).into_any_element()
                        }
                        LibraryTab::Downloads => {
                            self.render_downloads_body(cx).into_any_element()
                        }
                    }),
            )
    }

    fn render_library_songs_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let active_filter = self.library_source_filter;
        let visible_songs = self.visible_library_songs();
        let song_count = visible_songs.len();
        let search_active = !self.library_search_query.is_empty();
        let base_source = self
            .library_search_results
            .as_deref()
            .unwrap_or(&self.library.songs);

        div()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            // Instant FTS5 search bar + Local folder scanner side-by-side / stacked cleanly
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(self.library_search_input.clone()),
                    )
                    .when(search_active, |el| {
                        el.child(
                            div()
                                .id("lib-search-clear")
                                .h(px(38.0))
                                .px(px(11.0))
                                .rounded(px(10.0))
                                .bg(theme.surface_elevated)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .gap(px(5.0))
                                .cursor_pointer()
                                .text_size(px(12.0))
                                .text_color(theme.text_secondary)
                                .hover(move |s| s.bg(theme.surface_hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.clear_library_search(cx);
                                }))
                                .child(icons::icon(icons::CLOSE, 13.5, theme.text_muted))
                                .child("Clear"),
                        )
                    }),
            )
            // Compact local folder scanner bar
            .child(
                div()
                    .p(px(10.0))
                    .rounded(px(11.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(icons::icon(icons::FOLDER, 15.0, theme.text_secondary))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child("Local folder:"),
                    )
                    .child(div().flex_1().min_w(px(0.0)).child(self.folder_input.clone()))
                    .child(
                        div()
                            .id("lib-scan-btn")
                            .h(px(36.0))
                            .px(px(13.0))
                            .rounded(px(9.0))
                            .bg(theme.accent)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.on_accent)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let path = this.folder_input.read(cx).text().to_owned();
                                this.scan_folder(path, cx);
                            }))
                            .child(icons::icon(icons::FOLDER, 14.0, theme.on_accent))
                            .child("Scan Audio"),
                    ),
            )
            // Source filter chips + FTS5 match indicator
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(7.0))
                            .children(LibrarySourceFilter::ALL.into_iter().map(|flt| {
                                let is_sel = flt == active_filter;
                                let count = base_source.iter().filter(|s| flt.matches(s)).count();
                                div()
                                    .id(("lib-filter", flt as usize))
                                    .h(px(28.0))
                                    .px(px(11.0))
                                    .rounded(px(999.0))
                                    .bg(if is_sel {
                                        theme.accent_soft
                                    } else {
                                        theme.surface_elevated
                                    })
                                    .border_1()
                                    .border_color(if is_sel { theme.accent } else { theme.border })
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .font_weight(if is_sel {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(if is_sel {
                                        theme.accent
                                    } else {
                                        theme.text_secondary
                                    })
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.library_source_filter = flt;
                                        cx.notify();
                                    }))
                                    .child(format!("{} ({count})", flt.label()))
                            })),
                    )
                    .when(search_active, |el| {
                        el.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(format!(
                                    "{song_count} match(es) for \"{}\"",
                                    self.library_search_query
                                )),
                        )
                    }),
            )
            // Virtualized track list or honest empty state
            .child(if song_count == 0 {
                div()
                    .p(px(24.0))
                    .rounded(px(12.0))
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
                            .child(if search_active {
                                format!("No library tracks match \"{}\"", self.library_search_query)
                            } else {
                                "Your local library is empty".to_owned()
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child(if search_active {
                                "Try another keyword or clear the search filter."
                            } else {
                                "Enter a Linux directory path above (FLAC, WAV, Opus, M4A, MP3, OGG) and click Scan Audio, or play any YouTube Music track to cache it here."
                            }),
                    )
                    .into_any_element()
            } else {
                uniform_list(
                    "library-songs",
                    song_count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        let t = this.theme();
                        let all_songs = this.visible_library_songs();
                        let active_vid = this
                            .playback
                            .current_song
                            .as_ref()
                            .map(|s| s.video_id.as_str());

                        range
                            .filter_map(|idx| all_songs.get(idx).map(|s| (idx, s.clone())))
                            .map(|(idx, song)| {
                                let is_cur = active_vid == Some(song.video_id.as_str());
                                let source_badge = if song.video_id.starts_with("webdav:") {
                                    "WEBDAV"
                                } else if song.video_id.starts_with("smb:") {
                                    "SMB"
                                } else if song.local_path.is_some() {
                                    "LOCAL"
                                } else {
                                    "YTM"
                                };
                                let songs_c = all_songs.clone();
                                let art = song_thumbnail_url(&song);

                                div()
                                    .id(("lib-row", idx))
                                    .w_full()
                                    .h(px(58.0))
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
                                    .gap(px(10.0))
                                    .cursor_pointer()
                                    .hover(move |st| st.bg(t.surface_hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.play_songs(
                                            songs_c.clone(),
                                            idx,
                                            "Library",
                                            PlaybackSourceType::History,
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
                                                42.0,
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
                                                            .child(
                                                                if let Some(ref al) =
                                                                    song.album_name
                                                                {
                                                                    format!("{} · {al}", song.artist)
                                                                } else {
                                                                    song.artist.clone()
                                                                },
                                                            ),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .flex_shrink_0()
                                            .child(
                                                div()
                                                    .px(px(6.0))
                                                    .py(px(2.0))
                                                    .rounded(px(5.0))
                                                    .bg(t.surface_elevated)
                                                    .text_size(px(10.0))
                                                    .text_color(t.text_muted)
                                                    .child(source_badge),
                                            )
                                            .child(this.render_track_action_buttons(
                                                "lib", idx, &song, true, t, cx,
                                            )),
                                    )
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .w_full()
                .into_any_element()
            })
    }

    fn render_library_liked_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let liked = self.library.liked_songs.clone();
        let count = liked.len();

        div()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .p(px(14.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .w(px(44.0))
                                    .h(px(44.0))
                                    .rounded(px(10.0))
                                    .bg(theme.accent_soft)
                                    .border_1()
                                    .border_color(theme.accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icons::icon(icons::HEART_FILLED, 20.0, theme.accent)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child("Liked Songs"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.0))
                                            .text_color(theme.text_muted)
                                            .child(format!("{count} favorite track(s)")),
                                    ),
                            ),
                    )
                    .when(count > 0, |el| {
                        let play_songs = liked.clone();
                        let shuf_songs = liked.clone();
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .id("liked-play-all")
                                        .h(px(34.0))
                                        .px(px(14.0))
                                        .rounded(px(8.0))
                                        .bg(theme.accent)
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .cursor_pointer()
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.on_accent)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.play_songs(
                                                play_songs.clone(),
                                                0,
                                                "Liked Songs",
                                                PlaybackSourceType::History,
                                                None,
                                                cx,
                                            );
                                        }))
                                        .child(icons::icon(icons::PLAY, 13.0, theme.on_accent))
                                        .child("Play All"),
                                )
                                .child(
                                    div()
                                        .id("liked-shuffle")
                                        .h(px(34.0))
                                        .px(px(12.0))
                                        .rounded(px(8.0))
                                        .bg(theme.surface_elevated)
                                        .border_1()
                                        .border_color(theme.border)
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .cursor_pointer()
                                        .text_size(px(12.5))
                                        .text_color(theme.text_primary)
                                        .hover(move |s| s.bg(theme.surface_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.play_songs(
                                                shuf_songs.clone(),
                                                0,
                                                "Liked Songs",
                                                PlaybackSourceType::History,
                                                None,
                                                cx,
                                            );
                                            this.send(BackendCommand::ToggleShuffle, cx);
                                        }))
                                        .child(icons::icon(
                                            icons::SHUFFLE,
                                            13.5,
                                            theme.text_secondary,
                                        ))
                                        .child("Shuffle"),
                                ),
                        )
                    }),
            )
            .child(if count == 0 {
                div()
                    .p(px(24.0))
                    .rounded(px(12.0))
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
                            .child("No liked songs yet"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Tap the heart icon on any track or in the player bar to save favorites here."),
                    )
                    .into_any_element()
            } else {
                uniform_list(
                    "library-liked-list",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        let t = this.theme();
                        let all_liked = this.library.liked_songs.clone();
                        let active_vid = this
                            .playback
                            .current_song
                            .as_ref()
                            .map(|s| s.video_id.as_str());

                        range
                            .filter_map(|idx| all_liked.get(idx).map(|s| (idx, s.clone())))
                            .map(|(idx, song)| {
                                let is_cur = active_vid == Some(song.video_id.as_str());
                                let songs_c = all_liked.clone();
                                let art = song_thumbnail_url(&song);

                                div()
                                    .id(("liked-row", idx))
                                    .w_full()
                                    .h(px(58.0))
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
                                    .gap(px(10.0))
                                    .cursor_pointer()
                                    .hover(move |st| st.bg(t.surface_hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.play_songs(
                                            songs_c.clone(),
                                            idx,
                                            "Liked Songs",
                                            PlaybackSourceType::History,
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
                                                42.0,
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
                                                            .child(
                                                                if let Some(ref al) =
                                                                    song.album_name
                                                                {
                                                                    format!("{} · {al}", song.artist)
                                                                } else {
                                                                    song.artist.clone()
                                                                },
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
                                                song.duration_text
                                                    .clone()
                                                    .filter(|d| !d.is_empty()),
                                                |d_el, dur| {
                                                    d_el.child(
                                                        div()
                                                            .text_size(px(11.5))
                                                            .text_color(t.text_muted)
                                                            .child(dur),
                                                    )
                                                },
                                            )
                                            .child(this.render_track_action_buttons(
                                                "liked", idx, &song, true, t, cx,
                                            )),
                                    )
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .w_full()
                .into_any_element()
            })
    }

    fn render_library_playlists_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ref selected_id) = self.selected_playlist_id {
            return self
                .render_local_playlist_detail(selected_id.clone(), cx)
                .into_any_element();
        }

        let theme = self.theme();
        let summaries = self.library.playlist_summaries.clone();

        div()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            // Create new playlist bar
            .child(
                div()
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(icons::icon(icons::DISC, 16.0, theme.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(self.new_playlist_input.clone()),
                    )
                    .child(
                        div()
                            .id("pl-create-btn")
                            .h(px(36.0))
                            .px(px(14.0))
                            .rounded(px(9.0))
                            .bg(theme.accent)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.on_accent)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let name = this.new_playlist_input.read(cx).text().to_owned();
                                this.submit_create_playlist(name, cx);
                            }))
                            .child(icons::icon(icons::PLUS, 14.0, theme.on_accent))
                            .child("New Playlist"),
                    ),
            )
            // Playlists list
            .child(if summaries.is_empty() {
                div()
                    .p(px(24.0))
                    .rounded(px(12.0))
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
                            .child("No playlists yet"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Create a local playlist above, click + on any song, or save your active queue from the Queue drawer."),
                    )
                    .into_any_element()
            } else {
                div()
                    .id("playlists-list-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .children(summaries.into_iter().enumerate().map(|(idx, (pl, count))| {
                        let open_id = pl.id.clone();
                        let rename_id = pl.id.clone();
                        let rename_title = pl.title.clone();
                        let delete_id = pl.id.clone();

                        div()
                            .id(("pl-card", idx))
                            .w_full()
                            .p(px(12.0))
                            .rounded(px(11.0))
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
                                this.open_local_playlist(open_id.clone(), cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.0))
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.render_art_box(
                                        pl.thumbnail_url.as_deref(),
                                        48.0,
                                        8.0,
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
                                                    .text_size(px(14.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text_primary)
                                                    .truncate()
                                                    .child(pl.title),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.text_muted)
                                                    .truncate()
                                                    .child(format!(
                                                        "{count} track(s) · {}",
                                                        pl.source
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .flex_shrink_0()
                                    .child(
                                        div()
                                            .id(("pl-card-rename", idx))
                                            .w(px(30.0))
                                            .h(px(30.0))
                                            .rounded(px(7.0))
                                            .bg(theme.surface_elevated)
                                            .border_1()
                                            .border_color(theme.border)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(theme.surface_active))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_local_playlist(rename_id.clone(), cx);
                                                this.start_rename_playlist(
                                                    rename_id.clone(),
                                                    rename_title.clone(),
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                            }))
                                            .child(icons::icon(
                                                icons::PENCIL,
                                                13.5,
                                                theme.text_secondary,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id(("pl-card-delete", idx))
                                            .w(px(30.0))
                                            .h(px(30.0))
                                            .rounded(px(7.0))
                                            .bg(theme.surface_elevated)
                                            .border_1()
                                            .border_color(theme.border)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(theme.surface_active))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_playlist(delete_id.clone(), cx);
                                                cx.stop_propagation();
                                            }))
                                            .child(icons::icon(
                                                icons::TRASH,
                                                13.5,
                                                theme.text_muted,
                                            )),
                                    ),
                            )
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_local_playlist_detail(
        &self,
        playlist_id: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme();
        let meta = self
            .library
            .playlist_summaries
            .iter()
            .find(|(pl, _)| pl.id == playlist_id)
            .cloned();
        let title = meta
            .as_ref()
            .map(|(pl, _)| pl.title.clone())
            .unwrap_or_else(|| "Playlist".to_owned());
        let thumb = meta.as_ref().and_then(|(pl, _)| pl.thumbnail_url.clone());
        let is_renaming = self.playlist_rename_id.as_deref() == Some(playlist_id.as_str());
        let songs_ready = match &self.selected_playlist_songs {
            LoadState::Ready(s) => Some(s.clone()),
            _ => None,
        };
        let track_count = songs_ready
            .as_ref()
            .map(Vec::len)
            .or_else(|| meta.as_ref().map(|(_, c)| *c))
            .unwrap_or(0);

        let pid_rename = playlist_id.clone();
        let title_rename = title.clone();
        let pid_delete = playlist_id.clone();

        div()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            // Header bar with Back, Title / Rename inline form, Play All, Shuffle, Rename, Delete
            .child(
                div()
                    .p(px(14.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .id("pl-detail-back")
                                    .h(px(30.0))
                                    .px(px(10.0))
                                    .rounded(px(7.0))
                                    .bg(theme.surface_elevated)
                                    .border_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .cursor_pointer()
                                    .text_size(px(12.0))
                                    .text_color(theme.text_secondary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_local_playlist(cx);
                                    }))
                                    .child(icons::icon(
                                        icons::CHEVRON_LEFT,
                                        14.0,
                                        theme.text_secondary,
                                    ))
                                    .child("All Playlists"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .id("pl-detail-rename-toggle")
                                            .h(px(30.0))
                                            .px(px(10.0))
                                            .rounded(px(7.0))
                                            .bg(theme.surface_elevated)
                                            .border_1()
                                            .border_color(theme.border)
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .cursor_pointer()
                                            .text_size(px(12.0))
                                            .text_color(theme.text_secondary)
                                            .hover(move |s| s.bg(theme.surface_hover))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.start_rename_playlist(
                                                    pid_rename.clone(),
                                                    title_rename.clone(),
                                                    cx,
                                                );
                                            }))
                                            .child(icons::icon(
                                                icons::PENCIL,
                                                12.5,
                                                theme.text_secondary,
                                            ))
                                            .child("Rename"),
                                    )
                                    .child(
                                        div()
                                            .id("pl-detail-delete")
                                            .h(px(30.0))
                                            .px(px(10.0))
                                            .rounded(px(7.0))
                                            .bg(theme.surface_elevated)
                                            .border_1()
                                            .border_color(theme.border)
                                            .flex()
                                            .items_center()
                                            .gap(px(5.0))
                                            .cursor_pointer()
                                            .text_size(px(12.0))
                                            .text_color(theme.danger)
                                            .hover(move |s| s.bg(theme.surface_hover))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_playlist(pid_delete.clone(), cx);
                                            }))
                                            .child(icons::icon(icons::TRASH, 12.5, theme.danger))
                                            .child("Delete"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.0))
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.render_art_box(
                                        thumb.as_deref(),
                                        56.0,
                                        10.0,
                                        icons::DISC,
                                        theme,
                                    ))
                                    .child(if is_renaming {
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .flex()
                                            .items_center()
                                            .gap(px(8.0))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .child(self.rename_playlist_input.clone()),
                                            )
                                            .child(
                                                div()
                                                    .id("pl-rename-save")
                                                    .h(px(36.0))
                                                    .px(px(12.0))
                                                    .rounded(px(8.0))
                                                    .bg(theme.accent)
                                                    .flex()
                                                    .items_center()
                                                    .cursor_pointer()
                                                    .text_size(px(12.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.on_accent)
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        let val = this
                                                            .rename_playlist_input
                                                            .read(cx)
                                                            .text()
                                                            .to_owned();
                                                        this.submit_rename_playlist(val, cx);
                                                    }))
                                                    .child("Save"),
                                            )
                                            .child(
                                                div()
                                                    .id("pl-rename-cancel")
                                                    .h(px(36.0))
                                                    .px(px(10.0))
                                                    .rounded(px(8.0))
                                                    .bg(theme.surface_elevated)
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .flex()
                                                    .items_center()
                                                    .cursor_pointer()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.text_secondary)
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.cancel_rename_playlist(cx);
                                                    }))
                                                    .child("Cancel"),
                                            )
                                            .into_any_element()
                                    } else {
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.0))
                                            .child(
                                                div()
                                                    .text_size(px(18.0))
                                                    .font_weight(FontWeight::BOLD)
                                                    .text_color(theme.text_primary)
                                                    .truncate()
                                                    .child(title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.text_muted)
                                                    .child(format!("{track_count} track(s)")),
                                            )
                                            .into_any_element()
                                    }),
                            )
                            .when_some(
                                songs_ready.filter(|s| !s.is_empty()),
                                |el, songs_for_play| {
                                    let shuf_songs = songs_for_play.clone();
                                    let title_play = title.clone();
                                    let title_shuf = title.clone();
                                    let pid_play = playlist_id.clone();
                                    let pid_shuf = playlist_id.clone();
                                    el.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.0))
                                            .flex_shrink_0()
                                            .child(
                                                div()
                                                    .id("pl-play-all")
                                                    .h(px(34.0))
                                                    .px(px(14.0))
                                                    .rounded(px(8.0))
                                                    .bg(theme.accent)
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(6.0))
                                                    .cursor_pointer()
                                                    .text_size(px(12.5))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.on_accent)
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.play_songs(
                                                            songs_for_play.clone(),
                                                            0,
                                                            title_play.clone(),
                                                            PlaybackSourceType::Browse,
                                                            Some(pid_play.clone()),
                                                            cx,
                                                        );
                                                    }))
                                                    .child(icons::icon(
                                                        icons::PLAY,
                                                        13.0,
                                                        theme.on_accent,
                                                    ))
                                                    .child("Play All"),
                                            )
                                            .child(
                                                div()
                                                    .id("pl-shuffle")
                                                    .h(px(34.0))
                                                    .px(px(12.0))
                                                    .rounded(px(8.0))
                                                    .bg(theme.surface_elevated)
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(6.0))
                                                    .cursor_pointer()
                                                    .text_size(px(12.5))
                                                    .text_color(theme.text_primary)
                                                    .hover(move |s| s.bg(theme.surface_hover))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.play_songs(
                                                            shuf_songs.clone(),
                                                            0,
                                                            title_shuf.clone(),
                                                            PlaybackSourceType::Browse,
                                                            Some(pid_shuf.clone()),
                                                            cx,
                                                        );
                                                        this.send(
                                                            BackendCommand::ToggleShuffle,
                                                            cx,
                                                        );
                                                    }))
                                                    .child(icons::icon(
                                                        icons::SHUFFLE,
                                                        13.5,
                                                        theme.text_secondary,
                                                    ))
                                                    .child("Shuffle"),
                                            ),
                                    )
                                },
                            ),
                    ),
            )
            // Tracks in playlist (with reorder up/down + remove)
            .child(match &self.selected_playlist_songs {
                LoadState::Idle | LoadState::Loading => div()
                    .p(px(20.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(13.0))
                    .text_color(theme.text_secondary)
                    .child("Loading playlist tracks...")
                    .into_any_element(),
                LoadState::Error(err) => div()
                    .p(px(20.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.danger)
                    .text_size(px(13.0))
                    .text_color(theme.danger)
                    .child(err.clone())
                    .into_any_element(),
                LoadState::Ready(songs) if songs.is_empty() => div()
                    .p(px(24.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(14.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_primary)
                            .child("This playlist has no tracks yet"),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme.text_muted)
                            .child("Click the + button on any track in Search, Detail, Library, or Now Playing to add songs here."),
                    )
                    .into_any_element(),
                LoadState::Ready(songs) => {
                    let total = songs.len();
                    let pl_id_for_list = playlist_id.clone();
                    let pl_title_for_list = title.clone();
                    uniform_list(
                        "local-playlist-tracks",
                        total,
                        cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                            let t = this.theme();
                            let LoadState::Ready(ref all_songs) = this.selected_playlist_songs
                            else {
                                return Vec::new();
                            };
                            let list_len = all_songs.len();
                            let active_vid = this
                                .playback
                                .current_song
                                .as_ref()
                                .map(|s| s.video_id.as_str());

                            range
                                .filter_map(|idx| all_songs.get(idx).map(|s| (idx, s.clone())))
                                .map(|(idx, song)| {
                                    let is_cur = active_vid == Some(song.video_id.as_str());
                                    let songs_c = all_songs.clone();
                                    let art = song_thumbnail_url(&song);
                                    let pid_up = pl_id_for_list.clone();
                                    let pid_dn = pl_id_for_list.clone();
                                    let pid_rm = pl_id_for_list.clone();
                                    let pid_play = pl_id_for_list.clone();
                                    let ptitle = pl_title_for_list.clone();
                                    let rm_vid = song.video_id.clone();
                                    let up_target = playlist_reorder_target(idx, list_len, -1);
                                    let dn_target = playlist_reorder_target(idx, list_len, 1);

                                    div()
                                        .id(("pl-track-row", idx))
                                        .w_full()
                                        .h(px(58.0))
                                        .px(px(10.0))
                                        .rounded(px(10.0))
                                        .bg(if is_cur {
                                            t.surface_elevated
                                        } else {
                                            gpui::transparent_black()
                                        })
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(10.0))
                                        .cursor_pointer()
                                        .hover(move |st| st.bg(t.surface_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.play_songs(
                                                songs_c.clone(),
                                                idx,
                                                ptitle.clone(),
                                                PlaybackSourceType::Browse,
                                                Some(pid_play.clone()),
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
                                                .child(
                                                    div()
                                                        .w(px(22.0))
                                                        .text_size(px(12.0))
                                                        .text_color(if is_cur {
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
                                                                .child(song.artist.clone()),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .flex_shrink_0()
                                                // Reorder Up
                                                .child(
                                                    div()
                                                        .id(("pl-up", idx))
                                                        .w(px(26.0))
                                                        .h(px(26.0))
                                                        .rounded(px(6.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when_some(up_target, |el, to_idx| {
                                                            el.cursor_pointer()
                                                                .hover(move |s| {
                                                                    s.bg(t.surface_active)
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.reorder_playlist_track(
                                                                            pid_up.clone(),
                                                                            idx,
                                                                            to_idx,
                                                                            cx,
                                                                        );
                                                                        cx.stop_propagation();
                                                                    },
                                                                ))
                                                        })
                                                        .when(up_target.is_none(), |el| {
                                                            el.opacity(0.3)
                                                        })
                                                        .child(icons::icon(
                                                            icons::CHEVRON_UP,
                                                            13.0,
                                                            t.text_secondary,
                                                        )),
                                                )
                                                // Reorder Down
                                                .child(
                                                    div()
                                                        .id(("pl-dn", idx))
                                                        .w(px(26.0))
                                                        .h(px(26.0))
                                                        .rounded(px(6.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when_some(dn_target, |el, to_idx| {
                                                            el.cursor_pointer()
                                                                .hover(move |s| {
                                                                    s.bg(t.surface_active)
                                                                })
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.reorder_playlist_track(
                                                                            pid_dn.clone(),
                                                                            idx,
                                                                            to_idx,
                                                                            cx,
                                                                        );
                                                                        cx.stop_propagation();
                                                                    },
                                                                ))
                                                        })
                                                        .when(dn_target.is_none(), |el| {
                                                            el.opacity(0.3)
                                                        })
                                                        .child(icons::icon(
                                                            icons::CHEVRON_DOWN,
                                                            13.0,
                                                            t.text_secondary,
                                                        )),
                                                )
                                                .child(this.render_track_action_buttons(
                                                    "pl-trk", idx, &song, false, t, cx,
                                                ))
                                                // Remove from playlist
                                                .child(
                                                    div()
                                                        .id(("pl-rm", idx))
                                                        .w(px(26.0))
                                                        .h(px(26.0))
                                                        .rounded(px(6.0))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .cursor_pointer()
                                                        .hover(move |s| s.bg(t.surface_active))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.remove_song_from_playlist(
                                                                    pid_rm.clone(),
                                                                    rm_vid.clone(),
                                                                    cx,
                                                                );
                                                                cx.stop_propagation();
                                                            },
                                                        ))
                                                        .child(icons::icon(
                                                            icons::CLOSE,
                                                            13.0,
                                                            t.text_muted,
                                                        )),
                                                ),
                                        )
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

    pub fn render_downloads(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let count = self.library.downloads.len();
        let active_count = self.active_downloads.len();

        div()
            .size_full()
            .px(px(28.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(icons::icon(icons::DOWNLOAD, 22.0, theme.accent))
                            .child(
                                div()
                                    .text_size(px(24.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.text_primary)
                                    .child("Offline Downloads"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme.text_muted)
                            .child(if active_count > 0 {
                                format!(
                                    "{count} offline track(s) · {active_count} download(s) in progress"
                                )
                            } else {
                                format!("{count} offline track(s) saved for zero-buffer playback")
                            }),
                    ),
            )
            .child(self.render_downloads_body(cx))
    }

    fn render_downloads_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let active = sort_active_downloads(&self.active_downloads);
        let downloads = self.library.downloads.clone();
        let all_offline_songs: Vec<Song> = downloads.iter().map(download_record_to_song).collect();
        let has_any = !active.is_empty() || !downloads.is_empty();

        if !has_any {
            return div()
                .p(px(24.0))
                .rounded(px(12.0))
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
                        .child("No offline downloads yet"),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child("Click the download icon on any song in Search, Detail, Library, or Now Playing to save it for offline listening."),
                )
                .into_any_element();
        }

        div()
            .id("downloads-scroll")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.0))
            // Active / In-Progress Downloads with live progress bar & cancel
            .when(!active.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text_muted)
                                .child(format!("ACTIVE DOWNLOADS ({})", active.len())),
                        )
                        .children(active.into_iter().enumerate().map(|(idx, prog)| {
                            let status_text = format_download_progress_label(&prog);
                            let is_failed = matches!(prog.status, DownloadStatus::Failed(_));
                            let frac = prog.progress.clamp(0.0, 1.0);
                            let cancel_vid = prog.video_id.clone();

                            div()
                                .id(("dl-active-row", idx))
                                .w_full()
                                .p(px(12.0))
                                .rounded(px(10.0))
                                .bg(theme.surface)
                                .border_1()
                                .border_color(if is_failed {
                                    theme.danger
                                } else {
                                    theme.border
                                })
                                .flex()
                                .flex_col()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(10.0))
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
                                                        .truncate()
                                                        .child(prog.title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(11.5))
                                                        .text_color(if is_failed {
                                                            theme.danger
                                                        } else {
                                                            theme.text_muted
                                                        })
                                                        .truncate()
                                                        .child(format!(
                                                            "{} · {status_text}",
                                                            prog.artist
                                                        )),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id(("dl-active-cancel", idx))
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
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.cancel_download(cancel_vid.clone(), cx);
                                                }))
                                                .child(icons::icon(
                                                    icons::CLOSE,
                                                    12.0,
                                                    theme.text_muted,
                                                ))
                                                .child(if is_failed { "Dismiss" } else { "Cancel" }),
                                        ),
                                )
                                // Live progress track
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(5.0))
                                        .rounded_full()
                                        .bg(theme.surface_elevated)
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .h_full()
                                                .w(relative(frac))
                                                .rounded_full()
                                                .bg(if is_failed {
                                                    theme.danger
                                                } else {
                                                    theme.accent
                                                }),
                                        ),
                                )
                        })),
                )
            })
            // Completed Offline Tracks
            .when(!downloads.is_empty(), |el| {
                let play_all = all_offline_songs.clone();
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text_muted)
                                        .child(format!("SAVED OFFLINE ({})", downloads.len())),
                                )
                                .child(
                                    div()
                                        .id("dl-play-all")
                                        .h(px(30.0))
                                        .px(px(12.0))
                                        .rounded(px(7.0))
                                        .bg(theme.accent)
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .cursor_pointer()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.on_accent)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.play_songs(
                                                play_all.clone(),
                                                0,
                                                "Offline Downloads",
                                                PlaybackSourceType::History,
                                                None,
                                                cx,
                                            );
                                        }))
                                        .child(icons::icon(icons::PLAY, 12.0, theme.on_accent))
                                        .child("Play All"),
                                ),
                        )
                        .children(downloads.into_iter().enumerate().map(|(idx, rec)| {
                            let song = download_record_to_song(&rec);
                            let art = song_thumbnail_url(&song);
                            let fmt_label = rec
                                .download_format
                                .clone()
                                .unwrap_or_else(|| "Offline".to_owned());
                            let list_for_play = all_offline_songs.clone();
                            let del_vid = rec.video_id.clone();

                            div()
                                .id(("dl-row", idx))
                                .w_full()
                                .h(px(62.0))
                                .px(px(12.0))
                                .rounded(px(10.0))
                                .bg(theme.surface)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(10.0))
                                .cursor_pointer()
                                .hover(move |st| st.bg(theme.surface_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.play_songs(
                                        list_for_play.clone(),
                                        idx,
                                        "Offline Downloads",
                                        PlaybackSourceType::History,
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
                                                        .text_size(px(13.5))
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme.text_primary)
                                                        .truncate()
                                                        .child(rec.title),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(12.0))
                                                        .text_color(theme.text_muted)
                                                        .truncate()
                                                        .child(rec.artist),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .flex_shrink_0()
                                        .child(
                                            div()
                                                .px(px(7.0))
                                                .py(px(3.0))
                                                .rounded(px(6.0))
                                                .bg(theme.accent_soft)
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .text_size(px(10.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.accent)
                                                .child(icons::icon(icons::CHECK, 11.0, theme.accent))
                                                .child(fmt_label),
                                        )
                                        .child(self.render_track_action_buttons(
                                            "dl", idx, &song, true, theme, cx,
                                        ))
                                        .child(
                                            div()
                                                .id(("dl-delete", idx))
                                                .w(px(28.0))
                                                .h(px(28.0))
                                                .rounded(px(7.0))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(theme.surface_active))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.delete_download(del_vid.clone(), cx);
                                                    cx.stop_propagation();
                                                }))
                                                .child(icons::icon(
                                                    icons::TRASH,
                                                    13.5,
                                                    theme.text_muted,
                                                )),
                                        ),
                                )
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_downloads_sort_downloading_then_queued_then_failed() {
        let mut map = HashMap::new();
        map.insert(
            "v_fail".to_owned(),
            DownloadProgress {
                video_id: "v_fail".to_owned(),
                title: "Alpha".to_owned(),
                artist: "A".to_owned(),
                status: DownloadStatus::Failed("err".to_owned()),
                bytes_downloaded: 0,
                total_bytes: None,
                progress: 0.0,
            },
        );
        map.insert(
            "v_q".to_owned(),
            DownloadProgress {
                video_id: "v_q".to_owned(),
                title: "Beta".to_owned(),
                artist: "B".to_owned(),
                status: DownloadStatus::Queued,
                bytes_downloaded: 0,
                total_bytes: None,
                progress: 0.0,
            },
        );
        map.insert(
            "v_dl".to_owned(),
            DownloadProgress {
                video_id: "v_dl".to_owned(),
                title: "Gamma".to_owned(),
                artist: "C".to_owned(),
                status: DownloadStatus::Downloading,
                bytes_downloaded: 500,
                total_bytes: Some(1000),
                progress: 0.5,
            },
        );

        let sorted = sort_active_downloads(&map);
        let ids: Vec<&str> = sorted.iter().map(|p| p.video_id.as_str()).collect();
        assert_eq!(ids, vec!["v_dl", "v_q", "v_fail"]);
    }

    #[test]
    fn playlist_reorder_target_enforces_bounds() {
        assert_eq!(playlist_reorder_target(0, 0, 1), None);
        assert_eq!(playlist_reorder_target(0, 1, 1), None);
        assert_eq!(playlist_reorder_target(0, 3, -1), None);
        assert_eq!(playlist_reorder_target(0, 3, 1), Some(1));
        assert_eq!(playlist_reorder_target(1, 3, -1), Some(0));
        assert_eq!(playlist_reorder_target(1, 3, 1), Some(2));
        assert_eq!(playlist_reorder_target(2, 3, 1), None);
        assert_eq!(playlist_reorder_target(2, 3, -1), Some(1));
    }

    #[test]
    fn download_record_to_song_preserves_remote_identity_and_duration() {
        let rec = SavedSongMetadata {
            video_id: "yt_offline_1".to_owned(),
            title: "Starboy".to_owned(),
            artist: "The Weeknd".to_owned(),
            thumbnail_url: Some("https://img.example/starboy.jpg".to_owned()),
            duration_text: Some("3:50".to_owned()),
            album_name: Some("Starboy".to_owned()),
            uri: "file:///tmp/bitchord-downloads/starboy.webm".to_owned(),
            download_format: Some("OPUS · 160 kbps".to_owned()),
            date_added_seconds: Some(1_700_000_000),
        };

        let song = download_record_to_song(&rec);
        assert_eq!(song.video_id, "yt_offline_1");
        assert_eq!(song.local_path, None);
        assert_eq!(song.duration_text.as_deref(), Some("3:50"));
        assert_eq!(song.album_name.as_deref(), Some("Starboy"));

        // After an offline download is deleted, the song remains re-downloadable and retryable.
        assert_eq!(
            crate::views::track_actions::classify_download_action(
                song.local_path.is_some(),
                false,
                None,
            ),
            Some(crate::views::track_actions::DownloadActionVisual::Available)
        );
        assert!(crate::backend::is_retryable_remote_song(&song));
    }
}
