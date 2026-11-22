//! Offline download store: deterministic filename sanitization, codec/container
//! probing, atomic `.part` file staging, and download metadata records.
//!
//! Ported from upstream `DownloadStore.kt`, `Downloader.kt` (`sniff`/`Container`),
//! `Downloads.kt` (`SavedSongMetadata`, `SavedCollection`, `resolvedDownloadDates`,
//! `youtubeDownloadBadge`, `estimatedKbps`), and `DownloadStoreTest.kt`.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use bitchord_model::Song;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum stem length for downloaded audio filenames (well below 255-byte filesystem ceiling).
pub const MAX_STEM_CHARS: usize = 120;

// INFALLIBLE: static compile-time regex patterns.
static ILLEGAL_FILENAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\\/:*?"<>|\x00-\x1F]"#).expect("valid illegal filename regex"));
static WHITESPACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("valid whitespace regex"));

/// Extension + MIME type pair accepted for a manifest-assembled codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storable {
    pub extension: &'static str,
    pub mime_type: &'static str,
}

/// Container identified by opening magic bytes rather than untrusted HTTP labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Container {
    Flac,
    Wav,
    Mp4,
    Mp3,
    Adts,
    Ogg,
    Webm,
}

impl Container {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Mp4 => "m4a",
            Self::Mp3 => "mp3",
            Self::Adts => "aac",
            Self::Ogg => "ogg",
            Self::Webm => "webm",
        }
    }

    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Flac => "audio/flac",
            Self::Wav => "audio/x-wav",
            Self::Mp4 => "audio/mp4",
            Self::Mp3 => "audio/mpeg",
            Self::Adts => "audio/aac",
            Self::Ogg => "audio/ogg",
            Self::Webm => "audio/webm",
        }
    }

    pub const fn codec(self) -> Option<&'static str> {
        match self {
            Self::Flac => Some("flac"),
            Self::Wav => Some("wav"),
            Self::Mp3 => Some("mp3"),
            Self::Adts => Some("aac"),
            Self::Mp4 | Self::Ogg | Self::Webm => None,
        }
    }
}

/// Result of inspecting the opening bytes and headers of a download URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    File {
        container: Container,
        total_bytes: Option<u64>,
    },
    Manifest {
        dash: bool,
    },
}

/// Sniffs whether `head` is an HLS/DASH manifest or a playable audio container.
///
/// Exact port of `Downloader.sniff` (verified against `DownloadStoreTest.kt`).
pub fn sniff(
    head: &[u8],
    content_type: &str,
    stated_codec: Option<&str>,
    total_bytes: Option<u64>,
) -> Option<Probe> {
    let lower_ct = content_type.to_ascii_lowercase();
    // Strip UTF-8 BOM bytes directly before Latin-1 / ASCII prefix checks.
    let stripped = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    let text: String = stripped.iter().map(|&b| b as char).collect();
    let trimmed = text.trim_start();
    if lower_ct.contains("mpegurl") || trimmed.starts_with("#EXTM3U") {
        return Some(Probe::Manifest { dash: false });
    }
    if lower_ct.contains("dash+xml") || trimmed.contains("<MPD") {
        return Some(Probe::Manifest { dash: true });
    }
    container_of(head, stated_codec).map(|container| Probe::File {
        container,
        total_bytes,
    })
}

fn container_of(head: &[u8], stated_codec: Option<&str>) -> Option<Container> {
    let at = |offset: usize, magic: &[u8]| -> bool {
        head.len() >= offset + magic.len() && &head[offset..offset + magic.len()] == magic
    };

    if at(0, b"fLaC") {
        Some(Container::Flac)
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        Some(Container::Wav)
    } else if at(4, b"ftyp") {
        Some(Container::Mp4)
    } else if at(0, b"OggS") {
        Some(Container::Ogg)
    } else if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        Some(Container::Webm)
    } else if at(0, b"ID3") {
        if stated_codec.is_some_and(|c| c.eq_ignore_ascii_case("aac")) {
            Some(Container::Adts)
        } else {
            Some(Container::Mp3)
        }
    } else if head.len() >= 2 && head[0] == 0xFF {
        let second = head[1];
        if second & 0xF6 == 0xF0 {
            Some(Container::Adts)
        } else if second & 0xE0 == 0xE0 && second & 0x06 != 0 {
            Some(Container::Mp3)
        } else {
            None
        }
    } else {
        None
    }
}

/// Persisted download metadata record (compatible with upstream `SavedSongMetadata`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSongMetadata {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_added_seconds: Option<i64>,
}

/// Persisted album/playlist download record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedCollection {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub playlist: bool,
    #[serde(default)]
    pub video_ids: Vec<String>,
}

/// Resolves `(date_added_seconds, date_modified_seconds)` preserving original creation time.
pub fn resolved_download_dates(
    persisted_added_seconds: Option<i64>,
    file_modified_millis: Option<i64>,
) -> (Option<i64>, Option<i64>) {
    let modified_seconds = file_modified_millis
        .filter(|&ms| ms > 0)
        .map(|ms| ms / 1_000);
    (
        persisted_added_seconds.or(modified_seconds),
        modified_seconds,
    )
}

/// Display badge for YouTube downloads (`"OPUS · 160 kbps"` / `"AAC · 128 kbps"`).
pub fn youtube_download_badge(extension: &str, kbps: i32) -> Option<String> {
    if kbps <= 0 {
        return None;
    }
    let codec = if extension.eq_ignore_ascii_case("m4a") {
        "AAC"
    } else {
        "OPUS"
    };
    Some(format!("{codec} · {kbps} kbps"))
}

/// Estimates bitrate in kbps from byte size and duration in seconds.
pub fn estimated_kbps(bytes: Option<u64>, duration_seconds: Option<u64>) -> Option<u32> {
    let bytes = bytes.filter(|&b| b > 0)?;
    let secs = duration_seconds.filter(|&s| s > 0)?;
    Some(((bytes * 8) / (secs * 1_000)) as u32)
}

#[derive(Debug, Error)]
pub enum DownloadStoreError {
    #[error("download store I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Atomic `.part` download handle; callers must call [`PendingDownload::commit`] or
/// [`PendingDownload::abort`] (dropping without commit removes the `.part` file).
pub struct PendingDownload {
    part_path: PathBuf,
    target_path: PathBuf,
    committed: bool,
}

impl PendingDownload {
    pub fn part_path(&self) -> &Path {
        &self.part_path
    }

    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    pub fn open_writer(&self) -> io::Result<File> {
        File::create(&self.part_path)
    }

    /// Atomically publishes the completed `.part` file at its final target path.
    pub fn commit(mut self) -> Result<PathBuf, DownloadStoreError> {
        fs::rename(&self.part_path, &self.target_path)?;
        self.committed = true;
        Ok(self.target_path.clone())
    }

    /// Deletes the partial file on cancellation or failure.
    pub fn abort(mut self) {
        let _ = fs::remove_file(&self.part_path);
        self.committed = true;
    }
}

impl Drop for PendingDownload {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.part_path);
        }
    }
}

/// Filesystem-backed download destination manager for Linux desktop.
pub struct DownloadStore {
    root: PathBuf,
}

impl DownloadStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Formats `Artist - Title.ext` with FAT32/shell-safe sanitization and 120-char stem cap.
    pub fn file_name_for(song: &Song, extension: &str) -> String {
        let artist = sanitise(&song.artist);
        let title = sanitise(&song.title);
        let stem = match (artist.is_empty(), title.is_empty()) {
            (true, true) => song.video_id.clone(),
            (true, false) => title,
            (false, true) => artist,
            (false, false) => format!("{artist} - {title}"),
        };
        let truncated: String = stem.chars().take(MAX_STEM_CHARS).collect();
        format!("{}.{extension}", truncated.trim_end())
    }

    /// Maps a manifest codec label to a storable `(extension, mime_type)` pair, or `None`.
    pub fn storable(codec: Option<&str>) -> Option<Storable> {
        let normalized = codec?.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "flac" | "x-flac" => Some(Storable {
                extension: "flac",
                mime_type: "audio/flac",
            }),
            "wav" | "x-wav" | "wave" => Some(Storable {
                extension: "wav",
                mime_type: "audio/x-wav",
            }),
            "alac" | "m4a" | "mp4" | "eac3-joc" | "ec3-joc" | "dolby-atmos" => Some(Storable {
                extension: "m4a",
                mime_type: "audio/mp4",
            }),
            _ => None,
        }
    }

    /// Returns the path of an already downloaded file under `name`, if it exists on disk.
    pub fn existing(&self, name: &str) -> Option<PathBuf> {
        let path = self.root.join(name);
        path.is_file().then_some(path)
    }

    /// Checks whether a downloaded file (or offline HLS package playlist) still exists.
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    /// Deletes a downloaded audio file, or removes the whole offline-hls package directory
    /// if `path` points to `offline-hls/<id>/playlist.m3u8`.
    pub fn delete(path: &Path) -> io::Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let is_hls_package = path.file_name().and_then(|n| n.to_str()) == Some("playlist.m3u8")
            && path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|gp| gp.file_name())
                .and_then(|n| n.to_str())
                == Some("offline-hls");
        if is_hls_package && let Some(pkg_dir) = path.parent() {
            fs::remove_dir_all(pkg_dir)?;
            return Ok(true);
        }
        fs::remove_file(path)?;
        Ok(true)
    }

    /// Reserves a hidden `.name.part` staging file inside `root` for atomic commit.
    pub fn begin(&self, name: &str) -> Result<PendingDownload, DownloadStoreError> {
        fs::create_dir_all(&self.root)?;
        let target_path = self.root.join(name);
        let part_path = self.root.join(format!(".{name}.part"));
        let _ = fs::remove_file(&part_path);
        Ok(PendingDownload {
            part_path,
            target_path,
            committed: false,
        })
    }
}

fn sanitise(raw: &str) -> String {
    let cleaned = ILLEGAL_FILENAME_RE.replace_all(raw, " ");
    let collapsed = WHITESPACE_RE.replace_all(&cleaned, " ");
    collapsed.trim().trim_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use uuid::Uuid;

    use super::*;

    fn test_song(title: &str, artist: &str, video_id: &str) -> Song {
        Song::new(video_id, title, artist, None)
    }

    #[test]
    fn old_download_metadata_remains_readable_without_an_added_date() {
        let old: SavedSongMetadata = serde_json::from_str(
            r#"{"videoId":"abc","title":"Song","artist":"Artist","uri":"file:///song.webm"}"#,
        )
        .expect("decode old metadata");
        assert_eq!(old.date_added_seconds, None);
    }

    #[test]
    fn existing_downloads_use_file_time_when_no_creation_time_was_recorded() {
        let (added, modified) = resolved_download_dates(None, Some(1_725_000_123_999));
        assert_eq!(added, Some(1_725_000_123));
        assert_eq!(modified, Some(1_725_000_123));
    }

    #[test]
    fn persisted_creation_time_survives_later_file_modifications() {
        let (added, modified) =
            resolved_download_dates(Some(1_700_000_000), Some(1_725_000_123_999));
        assert_eq!(added, Some(1_700_000_000));
        assert_eq!(modified, Some(1_725_000_123));
    }

    #[test]
    fn flac_wav_and_alac_map_to_expected_extensions_and_mimes() {
        let flac = DownloadStore::storable(Some("flac")).expect("flac");
        assert_eq!(flac.extension, "flac");
        assert_eq!(flac.mime_type, "audio/flac");

        let wav = DownloadStore::storable(Some("wav")).expect("wav");
        assert_eq!(wav.extension, "wav");
        assert_eq!(wav.mime_type, "audio/x-wav");

        let alac = DownloadStore::storable(Some("alac")).expect("alac");
        assert_eq!(alac.extension, "m4a");
        assert_eq!(alac.mime_type, "audio/mp4");

        assert_eq!(
            DownloadStore::storable(Some("FLAC")).map(|s| s.extension),
            Some("flac")
        );
        assert_eq!(
            DownloadStore::storable(Some(" x-flac ")).map(|s| s.extension),
            Some("flac")
        );
    }

    #[test]
    fn unknown_or_absent_codec_is_not_storable() {
        assert_eq!(DownloadStore::storable(None), None);
        assert_eq!(DownloadStore::storable(Some("")), None);
        assert_eq!(DownloadStore::storable(Some("opus")), None);
        assert_eq!(DownloadStore::storable(Some("webm")), None);
        assert_eq!(DownloadStore::storable(Some("dsf")), None);
    }

    #[test]
    fn youtube_downloads_retain_their_codec_and_resolved_bitrate() {
        assert_eq!(
            youtube_download_badge("webm", 160).as_deref(),
            Some("OPUS · 160 kbps")
        );
        assert_eq!(
            youtube_download_badge("m4a", 128).as_deref(),
            Some("AAC · 128 kbps")
        );
        assert_eq!(youtube_download_badge("webm", 0), None);
    }

    #[test]
    fn file_name_sanitizes_and_falls_back_to_video_id() {
        assert_eq!(
            DownloadStore::file_name_for(&test_song("Kesariya", "Arijit Singh", "abc"), "flac"),
            "Arijit Singh - Kesariya.flac"
        );
        assert_eq!(
            DownloadStore::file_name_for(&test_song("A/B: C?", "D|E", "abc"), "m4a"),
            "D E - A B C.m4a"
        );
        assert_eq!(
            DownloadStore::file_name_for(&test_song("", "", "xyz789"), "m4a"),
            "xyz789.m4a"
        );
    }

    #[test]
    fn sniff_identifies_containers_manifests_and_rejects_invalid_bytes() {
        let container_of = |head: &[u8], stated: Option<&str>| match sniff(head, "", stated, None) {
            Some(Probe::File { container, .. }) => Some(container),
            _ => None,
        };

        assert_eq!(container_of(b"fLaC\0\0", None), Some(Container::Flac));
        assert_eq!(container_of(b"\0\0\0 ftypM4A ", None), Some(Container::Mp4));
        assert_eq!(
            container_of(b"RIFF\0\0\0\0WAVEfmt ", None),
            Some(Container::Wav)
        );
        assert_eq!(container_of(b"OggS\0\x02", None), Some(Container::Ogg));
        assert_eq!(
            container_of(&[0x1A, 0x45, 0xDF, 0xA3, 0x01], None),
            Some(Container::Webm)
        );
        assert_eq!(
            container_of(&[0xFF, 0xF1, 0x50, 0x80], None),
            Some(Container::Adts)
        );
        assert_eq!(
            container_of(&[0xFF, 0xFB, 0x90, 0x64], None),
            Some(Container::Mp3)
        );
        assert_eq!(container_of(b"ID3\x04\0", None), Some(Container::Mp3));
        assert_eq!(
            container_of(b"ID3\x04\0", Some("aac")),
            Some(Container::Adts)
        );
        assert_eq!(Container::Mp4.extension(), "m4a");
        assert_eq!(Container::Mp3.mime_type(), "audio/mpeg");

        assert_eq!(
            sniff(b"#EXTM3U\n#EXT-X-VERSION:7\n", "", Some("flac"), None),
            Some(Probe::Manifest { dash: false })
        );
        assert_eq!(
            sniff(b"\xEF\xBB\xBF\n#EXTM3U\n", "", None, None),
            Some(Probe::Manifest { dash: false })
        );
        assert_eq!(
            sniff(
                b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=",
                "",
                None,
                None
            ),
            Some(Probe::Manifest { dash: true })
        );
        assert_eq!(
            sniff(b"garbage", "application/vnd.apple.mpegurl", None, None),
            Some(Probe::Manifest { dash: false })
        );

        assert_eq!(sniff(b"<html><body>Not found", "", None, None), None);
        assert_eq!(sniff(b"", "", None, None), None);
        assert_eq!(sniff(b"FORM\0\0\0\0AIFFCOMM", "", None, None), None);
    }

    #[test]
    fn estimated_kbps_computes_bitrate_from_size_and_duration() {
        assert_eq!(estimated_kbps(Some(9_600_000), Some(240)), Some(320));
        assert_eq!(estimated_kbps(None, Some(240)), None);
        assert_eq!(estimated_kbps(Some(9_600_000), None), None);
        assert_eq!(estimated_kbps(Some(9_600_000), Some(0)), None);
    }

    #[test]
    fn pending_download_commits_atomically_and_cleans_up_on_abort() {
        let dir = std::env::temp_dir().join(format!("bitchord-dl-{}", Uuid::new_v4()));
        let store = DownloadStore::new(&dir);

        let pending = store.begin("Artist - Song.flac").expect("begin");
        {
            let mut f = pending.open_writer().expect("open writer");
            f.write_all(b"fLaC-payload").expect("write");
        }
        assert!(store.existing("Artist - Song.flac").is_none());
        let committed = pending.commit().expect("commit");
        assert_eq!(
            store.existing("Artist - Song.flac"),
            Some(committed.clone())
        );
        assert!(DownloadStore::exists(&committed));

        let aborted = store.begin("Cancelled.m4a").expect("begin abort");
        let part_path = aborted.part_path().to_path_buf();
        aborted.open_writer().expect("create part");
        assert!(part_path.exists());
        aborted.abort();
        assert!(!part_path.exists());
        assert!(store.existing("Cancelled.m4a").is_none());

        assert!(DownloadStore::delete(&committed).expect("delete"));
        assert!(!DownloadStore::exists(&committed));
        let _ = fs::remove_dir_all(&dir);
    }
}
