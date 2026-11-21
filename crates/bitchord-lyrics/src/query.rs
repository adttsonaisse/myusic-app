use std::sync::LazyLock;

use regex::Regex;

static WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("valid static regex"));

static CREDITS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        // Bracketed credits: (feat. X), [ft. X], (with X).
        Regex::new(r"(?i)\s*[(\[]\s*(feat|ft|featuring|with)\b[^)\]]*[)\]]")
            .expect("valid static regex"),
        // The same, unbracketed and running to the end of the title.
        Regex::new(r"(?i)\s+(feat|ft|featuring)\.?\s+.*$").expect("valid static regex"),
        // How the upload was labelled, not what was recorded.
        Regex::new(
            r"(?i)\s*[(\[]\s*(official\s*)?(music\s*)?(video|audio|visuali[sz]er|lyrics?\s*video|lyrics?|m/?v|hd|hq|4k|full\s*song)\s*[)\]]",
        )
        .expect("valid static regex"),
        Regex::new(r"(?i)\s*[(\[]\s*official\s*[)\]]").expect("valid static regex"),
    ]
});

/// Strips YouTube credit and upload-packaging annotations from `title` so
/// lyrics databases can match the canonical track name.
///
/// Only credits (`(feat. X)`, `[ft. X]`, `(with X)`, `feat. X`) and packaging
/// (`(Official Video)`, `[Lyrics]`, `(Visualizer)`, `(4K)`) are removed.
/// Brackets naming a different recording — `(Remix)`, `(Acoustic)`, `(Live)`,
/// `(Remastered 2011)`, `(Sped Up)` — are deliberately preserved.
pub fn for_lyrics_search(title: &str) -> String {
    let mut name = title.to_owned();
    for pattern in CREDITS.iter() {
        name = pattern.replace_all(&name, " ").into_owned();
    }
    let collapsed = WHITESPACE.replace_all(&name, " ");
    let cleaned = collapsed
        .trim()
        .trim_end_matches([',', '-', '\u{2013}', '\u{2014}'])
        .trim();
    if cleaned.is_empty() {
        title.trim().to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Trims `" - Topic"` off an auto-generated YouTube artist channel name.
pub fn artist_for_lyrics_search(artist: &str) -> String {
    let stripped = artist.strip_suffix(" - Topic").unwrap_or(artist).trim();
    if stripped.is_empty() {
        artist.trim().to_owned()
    } else {
        stripped.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_credits_catalogues_do_not_file_under() {
        assert_eq!("Dracula", for_lyrics_search("Dracula (feat. JENNIE)"));
        assert_eq!("Dracula", for_lyrics_search("Dracula (with JENNIE)"));
        assert_eq!("Dracula", for_lyrics_search("Dracula [ft. JENNIE]"));
        assert_eq!("Dracula", for_lyrics_search("Dracula feat. JENNIE"));
        assert_eq!("Dracula", for_lyrics_search("Dracula featuring JENNIE"));
    }

    #[test]
    fn strips_upload_packaging_labels() {
        assert_eq!(
            "Blinding Lights",
            for_lyrics_search("Blinding Lights (Official Video)")
        );
        assert_eq!(
            "Blinding Lights",
            for_lyrics_search("Blinding Lights (Official Music Video)")
        );
        assert_eq!(
            "Blinding Lights",
            for_lyrics_search("Blinding Lights [Lyrics]")
        );
        assert_eq!(
            "Blinding Lights",
            for_lyrics_search("Blinding Lights (Visualizer)")
        );
        assert_eq!("Blinding Lights", for_lyrics_search("Blinding Lights (4K)"));
    }

    #[test]
    fn leaves_different_recording_brackets_alone() {
        for title in [
            "Dracula (JENNIE Remix)",
            "Everlong (Acoustic)",
            "Song 2 (Live)",
            "Bohemian Rhapsody (Remastered 2011)",
            "Nights (Sped Up)",
        ] {
            assert_eq!(title, for_lyrics_search(title));
        }
    }

    #[test]
    fn handles_both_at_once_and_leaves_ordinary_or_packaging_only_titles_usable() {
        assert_eq!(
            "Levitating",
            for_lyrics_search("Levitating (feat. DaBaby) [Official Video]")
        );
        assert_eq!("golden hour", for_lyrics_search("golden hour"));
        assert_eq!("(Official Video)", for_lyrics_search("(Official Video)"));
    }

    #[test]
    fn drops_topic_suffix_from_auto_generated_channel() {
        assert_eq!(
            "Tame Impala",
            artist_for_lyrics_search("Tame Impala - Topic")
        );
        assert_eq!("Tame Impala", artist_for_lyrics_search("Tame Impala"));
    }
}
