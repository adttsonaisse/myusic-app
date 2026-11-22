//! Shared filename parsing, cover-art selection, and [`Song`] row construction
//! for remote file libraries (WebDAV and SMB2/3).

use bitchord_model::{PlaybackSourceType, Song};
use percent_encoding::percent_decode_str;

/// Maximum cover image payload accepted from remote storage (16 MiB).
pub const MAX_COVER_BYTES: usize = 16 * 1024 * 1024;

/// Audio extensions indexed from remote WebDAV/SMB shares (includes DSD `.dsf`/`.dff`).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "flac", "ogg", "opus", "aac", "wav", "wma", "aif", "aiff", "webm", "dsf", "dff",
];

/// Image extensions considered as folder cover art beside audio tracks.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// Preferred cover-art filename stems in priority order (upstream `RemoteArtwork.kt`).
const PREFERRED_COVER_STEMS: &[&str] =
    &["cover", "folder", "front", "albumart", "album", "artwork"];

/// Returns `true` if `name` ends with a supported audio extension.
pub fn is_audio_file(name: &str) -> bool {
    let ext = extension_lower(name);
    AUDIO_EXTENSIONS.contains(&ext.as_str())
}

/// Returns `true` if `name` ends with a supported image extension.
pub fn is_image_file(name: &str) -> bool {
    let ext = extension_lower(name);
    IMAGE_EXTENSIONS.contains(&ext.as_str())
}

/// Lowercased file extension without leading dot, or empty string if absent.
pub fn extension_lower(name: &str) -> String {
    let trimmed = name
        .split('?')
        .next()
        .unwrap_or(name)
        .split('#')
        .next()
        .unwrap_or(name)
        .trim_end_matches('/');
    let file = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    file.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Splits `"Artist - Title"` on the first `" - "`, matching upstream `RemoteSong.splitArtistTitle`.
pub fn split_artist_title(base: &str) -> (Option<String>, Option<String>) {
    if let Some((artist, title)) = base.split_once(" - ") {
        let artist = artist.trim();
        let title = title.trim();
        (
            (!artist.is_empty()).then(|| artist.to_owned()),
            (!title.is_empty()).then(|| title.to_owned()),
        )
    } else {
        let trimmed = base.trim();
        (None, (!trimmed.is_empty()).then(|| trimmed.to_owned()))
    }
}

/// Picks the best cover image from sibling image URLs/paths in the same folder.
///
/// Preferred stems (`cover`, `folder`, `front`, `albumart`, `album`, `artwork`) win
/// over stray photos (`IMG_1234.jpg`); ties break alphabetically by decoded stem.
pub fn pick_artwork(sibling_urls: &[String]) -> Option<String> {
    if sibling_urls.is_empty() {
        return None;
    }
    sibling_urls
        .iter()
        .min_by(|a, b| {
            let sa = artwork_stem(a);
            let sb = artwork_stem(b);
            let pa = PREFERRED_COVER_STEMS
                .iter()
                .position(|prefix| sa.starts_with(prefix))
                .unwrap_or(usize::MAX);
            let pb = PREFERRED_COVER_STEMS
                .iter()
                .position(|prefix| sb.starts_with(prefix))
                .unwrap_or(usize::MAX);
            (pa, &sa).cmp(&(pb, &sb))
        })
        .cloned()
}

fn artwork_stem(url: &str) -> String {
    let last = url
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(url);
    let decoded = percent_decode_str(last)
        .decode_utf8()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| last.to_owned());
    decoded
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&decoded)
        .to_ascii_lowercase()
}

/// Builds a uniform [`Song`] row for a remote audio track.
pub fn build_remote_song(
    video_id: String,
    stream_url: String,
    file_name: &str,
    album_name: Option<String>,
    cover_url: Option<String>,
    source: &str,
    browse_id: &str,
) -> Song {
    let base = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or(file_name);
    let (artist, title) = split_artist_title(base);
    let mut song = Song::new(
        video_id,
        title.unwrap_or_else(|| base.to_owned()),
        artist.unwrap_or_else(|| "Unknown Artist".to_owned()),
        cover_url,
    );
    song.album_name = album_name.filter(|s| !s.trim().is_empty());
    song.local_uri = Some(stream_url);
    song.playback_source = Some(source.to_owned());
    song.playback_source_type = Some(PlaybackSourceType::Browse);
    song.playback_source_id = Some(browse_id.to_owned());
    song
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_artist_and_title_matching_upstream() {
        assert_eq!(
            split_artist_title("Daft Punk - Instant Crush"),
            (
                Some("Daft Punk".to_owned()),
                Some("Instant Crush".to_owned())
            )
        );
        assert_eq!(
            split_artist_title("01 Track Without Artist"),
            (None, Some("01 Track Without Artist".to_owned()))
        );
    }

    #[test]
    fn picks_preferred_cover_stem_over_stray_images() {
        let candidates = vec![
            "https://nas.local/music/Album/IMG_1234.jpg".to_owned(),
            "https://nas.local/music/Album/folder.png".to_owned(),
            "https://nas.local/music/Album/Cover%20Art.jpg".to_owned(),
            "https://nas.local/music/Album/artwork.webp".to_owned(),
        ];
        assert_eq!(
            pick_artwork(&candidates).as_deref(),
            Some("https://nas.local/music/Album/Cover%20Art.jpg")
        );
    }

    #[test]
    fn recognizes_audio_including_dsd_and_images() {
        assert!(is_audio_file("01.flac"));
        assert!(is_audio_file("song.OPUS"));
        assert!(is_audio_file("master.dsf"));
        assert!(is_audio_file("sacd.dff"));
        assert!(!is_audio_file("cover.jpg"));
        assert!(is_image_file("cover.JPG"));
        assert!(is_image_file("front.webp"));
        assert!(!is_image_file("notes.txt"));
    }
}
