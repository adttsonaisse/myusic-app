use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use bitchord_model::Song;
use regex::Regex;

pub const PER_ARTIST_LIMIT: usize = 2;
pub const SEED_ARTIST_LIMIT: usize = 4;
pub const MIN_VIDEO_STATION: usize = 3;

static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\((?:official|lyric|lyrics|lyrical|audio|video|visuali[sz]er|full song|hd|4k)[^)]*\)|\(from[^)]*\)|\[[^\]]*\]|\b(?:official (?:video|audio|music video)|lyrical video|full video|4k video)\b",
    )
    .expect("valid static regex")
});
static TOPIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*-\s*topic\b").expect("valid static regex"));
static PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("valid static regex"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid static regex"));

/// Builds and de-duplicates AutoPlay / radio station batches with per-artist caps.
pub struct QueueBuilder;

impl QueueBuilder {
    pub fn extend(
        existing: &[Song],
        candidates: &[Song],
        limit: usize,
        prefer_videos: bool,
    ) -> Vec<Song> {
        let mut taken: Vec<Song> = existing.to_vec();
        let mut per_artist: HashMap<String, usize> = HashMap::new();
        let seed_artists = existing
            .last()
            .map(|s| Self::artist_set(&s.artist))
            .unwrap_or_default();
        let mut out = Vec::new();

        let songs_only: Vec<&Song> = candidates.iter().filter(|c| !c.is_video).collect();
        let videos_only: Vec<&Song> = candidates.iter().filter(|c| c.is_video).collect();
        let ordered: Vec<&Song> = if prefer_videos && videos_only.len() >= MIN_VIDEO_STATION {
            videos_only
        } else if prefer_videos || songs_only.is_empty() {
            candidates.iter().collect()
        } else {
            songs_only
        };

        for candidate in ordered {
            if out.len() >= limit {
                break;
            }
            if taken.iter().any(|t| Self::is_same_recording(t, candidate)) {
                continue;
            }
            let artists = Self::artist_set(&candidate.artist);
            if let Some(key) = artists.iter().min().cloned() {
                let cap = if artists.iter().any(|a| seed_artists.contains(a)) {
                    SEED_ARTIST_LIMIT
                } else {
                    PER_ARTIST_LIMIT
                };
                let played = *per_artist.get(&key).unwrap_or(&0);
                if played >= cap {
                    continue;
                }
                per_artist.insert(key, played + 1);
            }
            taken.push(candidate.clone());
            out.push(candidate.clone());
        }
        out
    }

    pub fn is_same_recording(a: &Song, b: &Song) -> bool {
        if a.video_id == b.video_id {
            return true;
        }
        let title = Self::normalised_title(&a.title);
        if title.is_empty() || title != Self::normalised_title(&b.title) {
            return false;
        }
        let left = Self::artist_set(&a.artist);
        let right = Self::artist_set(&b.artist);
        left.is_empty() || right.is_empty() || left.iter().any(|l| right.contains(l))
    }

    pub fn normalised_title(raw: &str) -> String {
        let lower = raw.to_lowercase();
        let before_pipe = lower.split(" | ").next().unwrap_or(&lower);
        let no_noise = NOISE.replace_all(before_pipe, " ");
        let no_punct = PUNCTUATION.replace_all(&no_noise, " ");
        SPACES.replace_all(&no_punct, " ").trim().to_owned()
    }

    pub fn artist_set(raw: &str) -> HashSet<String> {
        let lower = raw.to_lowercase();
        let cleaned = TOPIC.replace_all(&lower, " ");
        split_artist_delimiters(&cleaned)
            .into_iter()
            .map(|part| {
                let no_punct = PUNCTUATION.replace_all(part, " ");
                SPACES.replace_all(&no_punct, " ").trim().to_owned()
            })
            .filter(|s| !s.is_empty())
            .collect()
    }
}

const DELIMITERS: &[&str] = &[
    ",", "&", "·", "•", ";", " feat", " ft.", " ft ", " x ", " with ",
];

fn split_artist_delimiters(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < input.len() {
        let rest = &input[i..];
        if let Some(&delim) = DELIMITERS.iter().find(|&&d| rest.starts_with(d)) {
            parts.push(&input[start..i]);
            i += delim.len();
            start = i;
        } else {
            // INFALLIBLE: `i < input.len()` at a char boundary.
            let ch = rest.chars().next().unwrap_or_default();
            i += ch.len_utf8();
        }
    }
    parts.push(&input[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, title: &str, artist: &str) -> Song {
        Song::new(id, title, artist, None)
    }

    fn video(id: &str, title: &str, artist: &str) -> Song {
        let mut s = song(id, title, artist);
        s.is_video = true;
        s
    }

    #[test]
    fn video_filtering_and_same_recording_deduplication() {
        let extra = QueueBuilder::extend(
            &[],
            &[
                song("aaa", "Dildaara (Stand By Me)", "Vishal-Shekhar"),
                video("bbb", "Lyrical Video: Dildara Song", "Shafqat Amanat Ali"),
                song("ccc", "Sajni", "Arijit Singh"),
            ],
            10,
            false,
        );
        assert_eq!(
            vec!["aaa", "ccc"],
            extra
                .iter()
                .map(|s| s.video_id.as_str())
                .collect::<Vec<_>>()
        );

        let all_videos = QueueBuilder::extend(
            &[],
            &[video("aaa", "One", "A"), video("bbb", "Two", "B")],
            10,
            false,
        );
        assert_eq!(2, all_videos.len());

        assert!(QueueBuilder::is_same_recording(
            &song("aaa", "Kesariya", "Arijit Singh"),
            &song("bbb", "Kesariya (Official Video)", "Arijit Singh"),
        ));
        assert!(QueueBuilder::is_same_recording(
            &song("aaa", "Kesariya", "Arijit Singh"),
            &song("bbb", "Kesariya | Official Video", "Arijit Singh, Pritam"),
        ));
        assert!(!QueueBuilder::is_same_recording(
            &song("aaa", "Kesariya", "Arijit Singh"),
            &song("bbb", "Kesariya (Remix)", "Arijit Singh"),
        ));
        assert!(!QueueBuilder::is_same_recording(
            &song("aaa", "Perfect", "Ed Sheeran"),
            &song("bbb", "Perfect", "One Direction"),
        ));
        assert!(QueueBuilder::is_same_recording(
            &song("aaa", "Kalank (Duet)", "Pritam, Arijit Singh & Shilpa Rao"),
            &song("bbb", "Kalank (Duet)", "Shilpa Rao, Arijit Singh, & Pritam"),
        ));
    }

    #[test]
    fn topic_channels_per_artist_caps_and_seed_room() {
        assert_eq!(
            HashSet::from(["arijit singh".to_owned()]),
            QueueBuilder::artist_set("Arijit Singh - Topic")
        );
        assert_eq!(
            HashSet::from(["pritam".to_owned(), "arijit singh".to_owned()]),
            QueueBuilder::artist_set("Pritam feat. Arijit Singh")
        );

        let seed = song("seed", "Seed", "Diljit Dosanjh");
        let other_candidates: Vec<Song> = (1..=6)
            .map(|i| song(&format!("v{i}"), &format!("Song {i}"), "Badshah"))
            .collect();
        assert_eq!(
            2,
            QueueBuilder::extend(std::slice::from_ref(&seed), &other_candidates, 10, false).len()
        );

        let seed_candidates: Vec<Song> = (1..=8)
            .map(|i| song(&format!("v{i}"), &format!("Song {i}"), "Diljit Dosanjh"))
            .collect();
        assert_eq!(
            4,
            QueueBuilder::extend(std::slice::from_ref(&seed), &seed_candidates, 10, false).len()
        );

        // Reordered credit counts as one artist key.
        let nucleya = song("seed", "Seed", "Nucleya");
        let reordered = vec![
            song("v1", "One", "Pritam, Arijit Singh"),
            song("v2", "Two", "Arijit Singh, Pritam"),
            song("v3", "Three", "Pritam & Arijit Singh"),
        ];
        let capped = QueueBuilder::extend(&[nucleya], &reordered, 10, false);
        assert_eq!(
            vec!["v1", "v2"],
            capped
                .iter()
                .map(|s| s.video_id.as_str())
                .collect::<Vec<_>>()
        );
    }
}
