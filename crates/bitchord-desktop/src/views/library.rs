//! Local Music Library + Offline Downloads screens (`W4-2`) backed by SQLite
//! `LibraryDb`, `lofty` local tag scanning, `DownloadStore`, track thumbnails,
//! and vector SVG icons.

use bitchord_model::{PlaybackSourceType, Song};
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, div, px, uniform_list};

use crate::app::{BitChordApp, LibrarySourceFilter};
use crate::artwork::song_thumbnail_url;
use crate::backend::BackendCommand;
use crate::icons;

impl BitChordApp {
    pub fn render_library(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let stats = self.library.stats;
        let active_filter = self.library_source_filter;
        let filtered_songs: Vec<Song> = self
            .library
            .songs
            .iter()
            .filter(|s| active_filter.matches(s))
            .cloned()
            .collect();
        let song_count = filtered_songs.len();

        div()
            .size_full()
            .px(px(28.0))
            .py(px(20.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            // Header + real SQLite counts (R-17: real numbers only)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
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
                                        "{} songs · {} liked · {} playlists · {} plays recorded",
                                        stats.songs,
                                        self.liked_video_ids.len(),
                                        stats.playlists,
                                        stats.listening_history
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
            // Local folder scanner bar
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
                    .child(icons::icon(icons::FOLDER, 16.0, theme.text_secondary))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child("Scan folder:"),
                    )
                    .child(div().flex_1().child(self.folder_input.clone()))
                    .child(
                        div()
                            .id("lib-scan-btn")
                            .h(px(38.0))
                            .px(px(15.0))
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
                                let path = this.folder_input.read(cx).text().to_owned();
                                this.scan_folder(path, cx);
                            }))
                            .child(icons::icon(icons::FOLDER, 14.5, theme.on_accent))
                            .child("Scan Local Audio"),
                    ),
            )
            // Source filter chips (All / Local / WebDAV / SMB / YouTube)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.0))
                    .children(LibrarySourceFilter::ALL.into_iter().map(|flt| {
                        let is_sel = flt == active_filter;
                        let count = self
                            .library
                            .songs
                            .iter()
                            .filter(|s| flt.matches(s))
                            .count();
                        div()
                            .id(("lib-filter", flt as usize))
                            .h(px(30.0))
                            .px(px(12.0))
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
                            .text_size(px(12.0))
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
            // Imported/Local Playlists row (only if playlists exist)
            .when(!self.library.playlists.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.0))
                        .children(self.library.playlists.iter().map(|pl| {
                            div()
                                .px(px(12.0))
                                .py(px(6.0))
                                .rounded(px(8.0))
                                .bg(theme.surface)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(px(12.5))
                                .text_color(theme.text_primary)
                                .child(icons::icon(icons::DISC, 13.5, theme.text_muted))
                                .child(format!("{} ({})", pl.title, pl.source))
                        })),
                )
            })
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
                            .child("Your local library is empty"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Enter a Linux directory path above (FLAC, WAV, Opus, M4A, MP3, OGG) and click Scan Local Audio, or play any YouTube Music track to cache it here."),
                    )
                    .into_any_element()
            } else {
                uniform_list(
                    "library-songs",
                    song_count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        let t = this.theme();
                        let flt = this.library_source_filter;
                        let all_songs: Vec<Song> = this
                            .library
                            .songs
                            .iter()
                            .filter(|s| flt.matches(s))
                            .cloned()
                            .collect();
                        let active_vid = this
                            .playback
                            .current_song
                            .as_ref()
                            .map(|s| s.video_id.as_str());

                        range
                            .filter_map(|idx| all_songs.get(idx).map(|s| (idx, s.clone())))
                            .map(|(idx, song)| {
                                let is_cur = active_vid == Some(song.video_id.as_str());
                                let is_liked = this.is_song_liked(&song.video_id);
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
                                let next_s = song.clone();
                                let like_s = song.clone();
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
                                    .gap(px(12.0))
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
                                            .child(
                                                div()
                                                    .id(("lib-like", idx))
                                                    .w(px(28.0))
                                                    .h(px(28.0))
                                                    .rounded(px(7.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .hover(move |st| st.bg(t.surface_active))
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
                                                    .px(px(7.0))
                                                    .py(px(2.0))
                                                    .rounded(px(5.0))
                                                    .bg(t.surface_elevated)
                                                    .text_size(px(10.5))
                                                    .text_color(t.text_muted)
                                                    .child(source_badge),
                                            )
                                            .child(
                                                div()
                                                    .id(("lib-next", idx))
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
                                                    .hover(move |st| st.bg(t.surface_active))
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.send(
                                                                BackendCommand::PlayNext(Box::new(
                                                                    next_s.clone(),
                                                                )),
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
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .w_full()
                .into_any_element()
            })
    }

    pub fn render_downloads(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let downloads = self.library.downloads.clone();
        let count = downloads.len();

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
                            .child(format!("{count} offline track(s) indexed in SQLite")),
                    ),
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
                            .child("No offline downloads yet"),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Downloaded tracks saved via DownloadStore appear here with format badges and embedded lyrics."),
                    )
                    .into_any_element()
            } else {
                div()
                    .id("downloads-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .children(downloads.into_iter().enumerate().map(|(idx, rec)| {
                        let mut song = Song::new(
                            rec.video_id.clone(),
                            rec.title.clone(),
                            rec.artist.clone(),
                            rec.thumbnail_url.clone(),
                        );
                        song.album_name = rec.album_name.clone();
                        song.local_path = Some(rec.uri.clone());
                        let art = song_thumbnail_url(&song);
                        let fmt_label = rec
                            .download_format
                            .clone()
                            .unwrap_or_else(|| "Offline".to_owned());

                        div()
                            .id(("dl-row", idx))
                            .w_full()
                            .h(px(60.0))
                            .px(px(12.0))
                            .rounded(px(10.0))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.0))
                            .cursor_pointer()
                            .hover(move |st| st.bg(theme.surface_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.play_songs(
                                    vec![song.clone()],
                                    0,
                                    "Downloads",
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
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_size(px(13.5))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.text_primary)
                                                    .child(rec.title),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.text_muted)
                                                    .child(rec.artist),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(3.0))
                                    .rounded(px(6.0))
                                    .bg(theme.surface_elevated)
                                    .text_size(px(11.0))
                                    .text_color(theme.text_secondary)
                                    .child(fmt_label),
                            )
                    }))
                    .into_any_element()
            })
    }
}
