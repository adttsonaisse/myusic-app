//! Reusable daily-driver track actions (Like, Add to Playlist, Offline Download
//! with live progress, Start Radio, Play Next), download progress formatting
//! helpers, and the floating Add-to-Playlist picker modal.

use bitchord_model::Song;
use gpui::prelude::*;
use gpui::{Context, FontWeight, MouseButton, SharedString, deferred, div, px};

use crate::app::BitChordApp;
use crate::artwork::song_thumbnail_url;
use crate::backend::{BackendCommand, DownloadProgress, DownloadStatus};
use crate::icons;
use crate::theme::{BitChordTheme, rgba_hex};

/// Compact visual state for a track's offline download button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadActionVisual {
    Available,
    Queued,
    Downloading(u8),
    Downloaded,
    Failed,
}

/// Classifies what download button/badge should appear on a track row.
/// Returns `None` for already-local filesystem tracks that are not in the offline store.
pub fn classify_download_action(
    is_local: bool,
    is_downloaded: bool,
    progress: Option<&DownloadProgress>,
) -> Option<DownloadActionVisual> {
    if is_downloaded {
        return Some(DownloadActionVisual::Downloaded);
    }
    if let Some(p) = progress {
        return Some(match p.status {
            DownloadStatus::Queued => DownloadActionVisual::Queued,
            DownloadStatus::Downloading => {
                let pct = (p.progress.clamp(0.0, 1.0) * 100.0).round() as u8;
                DownloadActionVisual::Downloading(pct.min(100))
            }
            DownloadStatus::Completed => DownloadActionVisual::Downloaded,
            DownloadStatus::Failed(_) => DownloadActionVisual::Failed,
        });
    }
    if is_local {
        None
    } else {
        Some(DownloadActionVisual::Available)
    }
}

/// Formats byte counts cleanly for live download progress rows.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Human-readable live status string for an entry in `active_downloads`.
pub fn format_download_progress_label(progress: &DownloadProgress) -> String {
    match &progress.status {
        DownloadStatus::Queued => "Queued for download...".to_owned(),
        DownloadStatus::Downloading => {
            let pct = (progress.progress.clamp(0.0, 1.0) * 100.0).round() as u8;
            match progress.total_bytes {
                Some(total) if total > 0 => format!(
                    "Downloading {pct}% · {} / {}",
                    format_bytes(progress.bytes_downloaded),
                    format_bytes(total)
                ),
                _ if progress.bytes_downloaded > 0 => format!(
                    "Downloading {pct}% · {}",
                    format_bytes(progress.bytes_downloaded)
                ),
                _ => format!("Downloading {pct}%"),
            }
        }
        DownloadStatus::Completed => "Saved for offline playback".to_owned(),
        DownloadStatus::Failed(err) => format!("Failed: {err}"),
    }
}

/// Builds a sensible default playlist name when saving the active queue without typing one.
pub fn default_queue_playlist_name(current_song: Option<&Song>, count: usize) -> String {
    if let Some(song) = current_song {
        let title = song.title.trim();
        if !title.is_empty() {
            let short: String = title.chars().take(32).collect();
            return format!("{short} Mix");
        }
    }
    format!("Queue ({count} tracks)")
}

impl BitChordApp {
    /// Renders compact daily-driver track controls (Like, Add to Playlist, Download, Start Radio,
    /// and optionally Play Next) with `stop_propagation` so row click-to-play stays clean.
    pub fn render_track_action_buttons(
        &self,
        prefix: &'static str,
        idx: usize,
        song: &Song,
        include_play_next: bool,
        theme: BitChordTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_liked = self.is_song_liked(&song.video_id);
        let is_downloaded = self.is_song_downloaded(&song.video_id);
        let dl_state = classify_download_action(
            song.local_path.is_some(),
            is_downloaded,
            self.download_progress(&song.video_id),
        );

        let like_song = song.clone();
        let pl_song = song.clone();
        let radio_song = song.clone();
        let next_song = song.clone();
        let dl_song = song.clone();
        let dl_vid = song.video_id.clone();

        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .flex_shrink_0()
            // 1. Like / Heart
            .child(
                div()
                    .id(SharedString::from(format!("{prefix}-like-{idx}")))
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.surface_active))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_like_song(like_song.clone(), cx);
                        cx.stop_propagation();
                    }))
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
            // 2. Add to Playlist picker
            .child(
                div()
                    .id(SharedString::from(format!("{prefix}-add-pl-{idx}")))
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.surface_active))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_playlist_picker(pl_song.clone(), cx);
                        cx.stop_propagation();
                    }))
                    .child(icons::icon(icons::PLUS, 14.0, theme.text_muted)),
            )
            // 3. Offline Download / Live Progress / Downloaded check
            .when_some(dl_state, |el, state| match state {
                DownloadActionVisual::Available => el.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-dl-{idx}")))
                        .w(px(28.0))
                        .h(px(28.0))
                        .rounded(px(7.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(move |s| s.bg(theme.surface_active))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.download_song(dl_song.clone(), cx);
                            cx.stop_propagation();
                        }))
                        .child(icons::icon(icons::DOWNLOAD, 13.5, theme.text_muted)),
                ),
                DownloadActionVisual::Queued => el.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-dl-q-{idx}")))
                        .h(px(26.0))
                        .px(px(6.0))
                        .rounded(px(6.0))
                        .bg(theme.surface_elevated)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .cursor_pointer()
                        .text_size(px(10.0))
                        .text_color(theme.text_secondary)
                        .hover(move |s| s.bg(theme.surface_active))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cancel_download(dl_vid.clone(), cx);
                            cx.stop_propagation();
                        }))
                        .child("...")
                        .child(icons::icon(icons::CLOSE, 10.0, theme.text_muted)),
                ),
                DownloadActionVisual::Downloading(pct) => el.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-dl-pct-{idx}")))
                        .h(px(26.0))
                        .px(px(6.0))
                        .rounded(px(6.0))
                        .bg(theme.accent_soft)
                        .border_1()
                        .border_color(theme.accent)
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .cursor_pointer()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.accent)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cancel_download(dl_vid.clone(), cx);
                            cx.stop_propagation();
                        }))
                        .child(format!("{pct}%"))
                        .child(icons::icon(icons::CLOSE, 10.0, theme.accent)),
                ),
                DownloadActionVisual::Downloaded => el.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-dl-ok-{idx}")))
                        .w(px(26.0))
                        .h(px(26.0))
                        .rounded(px(7.0))
                        .bg(theme.accent_soft)
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(icons::icon(icons::CHECK, 13.0, theme.accent)),
                ),
                DownloadActionVisual::Failed => el.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-dl-retry-{idx}")))
                        .w(px(28.0))
                        .h(px(28.0))
                        .rounded(px(7.0))
                        .bg(theme.surface_elevated)
                        .border_1()
                        .border_color(theme.danger)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(move |s| s.bg(theme.surface_active))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.download_song(dl_song.clone(), cx);
                            cx.stop_propagation();
                        }))
                        .child(icons::icon(icons::DOWNLOAD, 13.5, theme.danger)),
                ),
            })
            // 4. Start Radio
            .child(
                div()
                    .id(SharedString::from(format!("{prefix}-radio-{idx}")))
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(px(7.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.surface_active))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_radio(radio_song.clone(), cx);
                        cx.stop_propagation();
                    }))
                    .child(icons::icon(icons::RADIO, 14.0, theme.text_muted)),
            )
            // 5. Optional Play Next chip
            .when(include_play_next, |el| {
                el.child(
                    div()
                        .id(SharedString::from(format!("{prefix}-next-{idx}")))
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(7.0))
                        .bg(theme.surface_elevated)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .cursor_pointer()
                        .text_size(px(11.0))
                        .text_color(theme.text_secondary)
                        .hover(move |s| s.bg(theme.surface_active).text_color(theme.text_primary))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(BackendCommand::PlayNext(Box::new(next_song.clone())), cx);
                            cx.stop_propagation();
                        }))
                        .child(icons::icon(icons::LIST_PLUS, 12.5, theme.text_secondary))
                        .child("Next"),
                )
            })
    }

    /// Floating centered modal overlay to add `self.playlist_picker_song` to an existing local
    /// playlist or create a brand-new playlist seeded with that song.
    pub fn render_playlist_picker_overlay(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let song = self.playlist_picker_song.as_ref()?.clone();
        let theme = self.theme();
        let art_url = song_thumbnail_url(&song);
        let playlists = self.library.playlist_summaries.clone();

        let modal = div()
            .id("playlist-picker-backdrop")
            .absolute()
            .inset_0()
            .bg(rgba_hex(0x0000_0094))
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_playlist_picker(cx);
                }),
            )
            .child(
                div()
                    .id("playlist-picker-card")
                    .w(px(400.0))
                    .max_h(px(480.0))
                    .rounded(px(14.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_xl()
                    .p(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Header
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(icons::icon(icons::LIST_PLUS, 17.0, theme.accent))
                                    .child(
                                        div()
                                            .text_size(px(15.5))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child("Add to Playlist"),
                                    ),
                            )
                            .child(
                                div()
                                    .id("picker-close")
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_playlist_picker(cx);
                                    }))
                                    .child(icons::icon(icons::CLOSE, 14.0, theme.text_muted)),
                            ),
                    )
                    // Selected track preview
                    .child(
                        div()
                            .p(px(10.0))
                            .rounded(px(10.0))
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(self.render_art_box(
                                art_url.as_deref(),
                                40.0,
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
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme.text_primary)
                                            .truncate()
                                            .child(song.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(theme.text_muted)
                                            .truncate()
                                            .child(song.artist.clone()),
                                    ),
                            ),
                    )
                    // Quick create new playlist + add song
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(self.picker_new_playlist_input.clone()),
                            )
                            .child(
                                div()
                                    .id("picker-create-add")
                                    .h(px(38.0))
                                    .px(px(12.0))
                                    .rounded(px(10.0))
                                    .bg(theme.accent)
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .cursor_pointer()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.on_accent)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let name = this
                                            .picker_new_playlist_input
                                            .read(cx)
                                            .text()
                                            .to_owned();
                                        this.submit_picker_create_playlist(name, cx);
                                    }))
                                    .child(icons::icon(icons::PLUS, 13.0, theme.on_accent))
                                    .child("Create"),
                            ),
                    )
                    // Existing local playlists list
                    .child(if playlists.is_empty() {
                        div()
                            .p(px(14.0))
                            .rounded(px(9.0))
                            .bg(theme.surface_elevated)
                            .text_size(px(12.5))
                            .text_color(theme.text_muted)
                            .child("No local playlists yet. Type a name above to create your first one.")
                            .into_any_element()
                    } else {
                        div()
                            .id("picker-playlists-scroll")
                            .flex_1()
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .children(playlists.into_iter().enumerate().map(|(idx, (pl, count))| {
                                let pl_id = pl.id.clone();
                                let s_add = song.clone();
                                div()
                                    .id(("picker-pl-row", idx))
                                    .w_full()
                                    .px(px(10.0))
                                    .py(px(8.0))
                                    .rounded(px(9.0))
                                    .bg(theme.surface_elevated)
                                    .border_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(10.0))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.add_song_to_playlist(pl_id.clone(), s_add.clone(), cx);
                                        this.close_playlist_picker(cx);
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(10.0))
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .child(self.render_art_box(
                                                pl.thumbnail_url.as_deref(),
                                                34.0,
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
                                                    .gap(px(1.0))
                                                    .child(
                                                        div()
                                                            .text_size(px(13.0))
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .text_color(theme.text_primary)
                                                            .truncate()
                                                            .child(pl.title),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(11.0))
                                                            .text_color(theme.text_muted)
                                                            .child(format!("{count} tracks")),
                                                    ),
                                            ),
                                    )
                                    .child(icons::icon(icons::PLUS, 14.0, theme.accent))
                            }))
                            .into_any_element()
                    }),
            );

        Some(deferred(modal).priority(20).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_action_classification_and_progress_formatting() {
        // Remote song not yet downloaded.
        assert_eq!(
            classify_download_action(false, false, None),
            Some(DownloadActionVisual::Available)
        );
        // Local file without download record hides YouTube download icon.
        assert_eq!(classify_download_action(true, false, None), None);
        // Already downloaded always shows Downloaded check.
        assert_eq!(
            classify_download_action(false, true, None),
            Some(DownloadActionVisual::Downloaded)
        );

        let queued = DownloadProgress {
            video_id: "v1".to_owned(),
            title: "Song".to_owned(),
            artist: "Artist".to_owned(),
            status: DownloadStatus::Queued,
            bytes_downloaded: 0,
            total_bytes: None,
            progress: 0.0,
        };
        assert_eq!(
            classify_download_action(false, false, Some(&queued)),
            Some(DownloadActionVisual::Queued)
        );
        assert_eq!(
            format_download_progress_label(&queued),
            "Queued for download..."
        );

        let active = DownloadProgress {
            video_id: "v1".to_owned(),
            title: "Song".to_owned(),
            artist: "Artist".to_owned(),
            status: DownloadStatus::Downloading,
            bytes_downloaded: 2 * 1024 * 1024,
            total_bytes: Some(5 * 1024 * 1024),
            progress: 0.424,
        };
        assert_eq!(
            classify_download_action(false, false, Some(&active)),
            Some(DownloadActionVisual::Downloading(42))
        );
        assert_eq!(
            format_download_progress_label(&active),
            "Downloading 42% · 2.0 MB / 5.0 MB"
        );

        let failed = DownloadProgress {
            status: DownloadStatus::Failed("network timeout".to_owned()),
            ..active
        };
        assert_eq!(
            classify_download_action(false, false, Some(&failed)),
            Some(DownloadActionVisual::Failed)
        );
        assert_eq!(
            format_download_progress_label(&failed),
            "Failed: network timeout"
        );

        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
    }

    #[test]
    fn default_queue_playlist_name_uses_current_song_or_track_count() {
        let song = Song::new("v1", "Midnight City", "M83", None);
        assert_eq!(
            default_queue_playlist_name(Some(&song), 8),
            "Midnight City Mix"
        );
        assert_eq!(default_queue_playlist_name(None, 5), "Queue (5 tracks)");
    }
}
