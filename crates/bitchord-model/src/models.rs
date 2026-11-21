use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Artwork for a list row — shared cache rung across all row heights.
pub const ROW_ART_PX: u32 = 160;

/// Artwork for a shelf card.
pub const CARD_ART_PX: u32 = 480;

/// Artwork for a page header.
pub const HEADER_ART_PX: u32 = 720;

/// Artwork handed to OS media session / MPRIS2 / notifications.
pub const NOTIFICATION_ART_PX: u32 = 544;

/// Artwork for the full player — sleeve and full-bleed banner.
pub const PLAYER_ART_PX: u32 = 1200;

static SIZE_HINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"w\d+-h\d+").expect("valid static regex"));
static PARAM_SIZE_HINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"=s\d+").expect("valid static regex"));

/// Tier of an item in the playback queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QueueTier {
    /// Explicitly queued by user ("Play Next", "Add to Queue", or preserved user queue).
    UserQueue,
    /// Part of the active Album, Playlist, or Artist collection context.
    #[default]
    Context,
    /// Dynamically generated radio recommendations appended when context/user queue ends.
    Autoplay,
}

/// One credited artist, as YouTube Music itself states them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRef {
    pub name: String,
    #[serde(default)]
    pub browse_id: Option<String>,
}

impl ArtistRef {
    pub fn new(name: impl Into<String>, browse_id: Option<String>) -> Self {
        Self {
            name: name.into(),
            browse_id,
        }
    }
}

/// A queue-level origin shown above Now Playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlaybackSourceType {
    Home,
    Search,
    History,
    Replay,
    Explore,
    Browse,
    SharedLink,
    Queue,
}

/// A playable YouTube Music, local, or external-source track.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub duration_text: Option<String>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album_id: Option<String>,
    #[serde(default)]
    pub album_name: Option<String>,
    #[serde(default)]
    pub is_video: bool,
    #[serde(default)]
    pub is_video_origin: bool,
    #[serde(default)]
    pub set_video_id: Option<String>,
    #[serde(default)]
    pub queue_tier: QueueTier,
    #[serde(default)]
    pub queue_entry_id: Option<String>,
    #[serde(default)]
    pub radio_name: Option<String>,
    #[serde(default)]
    pub local_uri: Option<String>,
    #[serde(default)]
    pub download_format: Option<String>,
    #[serde(default)]
    pub local_path: Option<String>,
    #[serde(default)]
    pub local_date_added_seconds: Option<i64>,
    #[serde(default)]
    pub local_date_modified_seconds: Option<i64>,
    #[serde(default)]
    pub track_number: Option<i32>,
    #[serde(default)]
    pub source_quality: Option<String>,
    #[serde(default)]
    pub source_hi_res: bool,
    #[serde(default)]
    pub is_explicit: Option<bool>,
    #[serde(default)]
    pub playback_source: Option<String>,
    #[serde(default)]
    pub playback_source_type: Option<PlaybackSourceType>,
    #[serde(default)]
    pub playback_source_id: Option<String>,
    #[serde(default)]
    pub feedback_token: Option<String>,
}

impl Song {
    /// Minimal constructor matching the required fields of Kotlin's `Song`.
    pub fn new(
        video_id: impl Into<String>,
        title: impl Into<String>,
        artist: impl Into<String>,
        thumbnail_url: Option<String>,
    ) -> Self {
        Self {
            video_id: video_id.into(),
            title: title.into(),
            artist: artist.into(),
            thumbnail_url,
            ..Self::default()
        }
    }

    /// True if and only if `queue_tier` is `QueueTier::Autoplay`.
    pub fn from_autoplay(&self) -> bool {
        self.queue_tier == QueueTier::Autoplay
    }

    /// Artwork URL scaled to `px` pixels if YouTube/Google size hints exist.
    pub fn artwork_at(&self, px: u32) -> Option<String> {
        artwork_at(self.thumbnail_url.as_deref(), px)
    }

    /// Whether a row is the same recording by exact title and artist credit.
    pub fn is_same_track_as(&self, other: Option<&Song>) -> bool {
        match other {
            Some(other) => self.title == other.title && self.artist == other.artist,
            None => false,
        }
    }

    /// Parsed `duration_text` in milliseconds, or `0` when absent/invalid.
    pub fn duration_millis(&self) -> u64 {
        duration_millis(self.duration_text.as_deref())
    }
}

/// Strategy returned by [`playing_row_matcher`] for highlighting rows in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayingRowMatch {
    None,
    ByIdAndSlot {
        video_id: String,
        set_video_id: String,
    },
    ById(String),
    ByExactRow(Box<Song>),
}

impl PlayingRowMatch {
    pub fn matches(&self, row: &Song) -> bool {
        match self {
            Self::None => false,
            Self::ByIdAndSlot {
                video_id,
                set_video_id,
            } => row.video_id == *video_id && row.set_video_id.as_deref() == Some(set_video_id),
            Self::ById(video_id) => row.video_id == *video_id,
            Self::ByExactRow(target) => row == target.as_ref(),
        }
    }
}

/// Decides which row(s) of `rows` receive the now-playing highlight for `current`.
///
/// Identity (`video_id`, plus `set_video_id` when present on both) is tried first across
/// the whole list; only when no row shares `video_id` does cross-catalogue name matching
/// apply, and then only to the first matching row.
pub fn playing_row_matcher(rows: &[Song], current: Option<&Song>) -> PlayingRowMatch {
    let Some(current) = current else {
        return PlayingRowMatch::None;
    };
    let same_id: Vec<&Song> = rows
        .iter()
        .filter(|row| row.video_id == current.video_id)
        .collect();
    if !same_id.is_empty() {
        if let Some(slot) = current.set_video_id.as_deref()
            && same_id
                .iter()
                .any(|row| row.set_video_id.as_deref() == Some(slot))
        {
            return PlayingRowMatch::ByIdAndSlot {
                video_id: current.video_id.clone(),
                set_video_id: slot.to_owned(),
            };
        }
        return PlayingRowMatch::ById(current.video_id.clone());
    }
    match rows.iter().find(|row| row.is_same_track_as(Some(current))) {
        Some(by_name) => PlayingRowMatch::ByExactRow(Box::new(by_name.clone())),
        None => PlayingRowMatch::None,
    }
}

/// Parses an `M:SS` or `H:MM:SS` duration display string to milliseconds, or `0` on invalid input.
pub fn duration_millis(text: Option<&str>) -> u64 {
    let Some(trimmed) = text.map(str::trim).filter(|s| !s.is_empty()) else {
        return 0;
    };
    let mut numbers = Vec::with_capacity(3);
    for part in trimmed.split(':') {
        let Ok(n) = part.trim().parse::<i64>() else {
            return 0;
        };
        numbers.push(n);
    }
    let seconds = match numbers.as_slice() {
        [m, s] => m.saturating_mul(60).saturating_add(*s),
        [h, m, s] => h
            .saturating_mul(3_600)
            .saturating_add(m.saturating_mul(60))
            .saturating_add(*s),
        _ => return 0,
    };
    if seconds <= 0 {
        0
    } else {
        (seconds as u64).saturating_mul(1_000)
    }
}

/// Rewrites YouTube / Google image URL size hints (`w<n>-h<n>` and `=s<n>` on Google image hosts).
pub fn artwork_at(url: Option<&str>, px: u32) -> Option<String> {
    let url = url?;
    let sized = SIZE_HINT.replace_all(url, format!("w{px}-h{px}"));
    if is_google_image_host(url) {
        Some(
            PARAM_SIZE_HINT
                .replace_all(&sized, format!("=s{px}"))
                .into_owned(),
        )
    } else {
        Some(sized.into_owned())
    }
}

fn is_google_image_host(url: &str) -> bool {
    let after_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
        .or_else(|| url.split_once("//").map(|(_, rest)| rest))
        .unwrap_or("");
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "ggpht.com"
        || host.ends_with(".ggpht.com")
        || host == "googleusercontent.com"
        || host.ends_with(".googleusercontent.com")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BrowseType {
    Album,
    Artist,
    Playlist,
    #[default]
    Other,
}

/// A non-track search result: album, artist, or playlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseItem {
    pub browse_id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
    pub r#type: BrowseType,
}

/// Search rows are heterogeneous once filters other than "Songs" are used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SearchResult {
    TopTrack { song: Song },
    Track { song: Song },
    Browse { item: BrowseItem },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Videos => "Videos",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
        }
    }

    pub const fn params(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Songs => Some("EgWKAQIIAWoKEAkQChAFEAMQBA=="),
            Self::Videos => Some("EgWKAQIQAWoKEAkQChAFEAMQBA=="),
            Self::Albums => Some("EgWKAQIYAWoKEAkQChAFEAMQBA=="),
            Self::Artists => Some("EgWKAQIgAWoKEAkQChAFEAMQBA=="),
            Self::Playlists => Some("EgWKAQIoAWoKEAkQChAFEAMQBA=="),
        }
    }
}

/// A card in a home-feed carousel: either a track (`video_id`) or an album/playlist (`browse_id`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShelfItem {
    pub title: String,
    pub subtitle: String,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub video_id: Option<String>,
    #[serde(default)]
    pub browse_id: Option<String>,
    #[serde(default)]
    pub is_video: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ShelfLayout {
    #[default]
    Cards,
    List,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub name: String,
    pub email: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountChannel {
    pub name: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
    pub page_id: Option<String>,
    pub data_sync_id: Option<String>,
    pub active_on_web: bool,
}

impl AccountChannel {
    pub fn key(&self) -> &str {
        self.page_id
            .as_deref()
            .or(self.data_sync_id.as_deref())
            .unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeShelf {
    pub title: String,
    pub items: Vec<ShelfItem>,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub more_browse_id: Option<String>,
    #[serde(default)]
    pub more_params: Option<String>,
    #[serde(default)]
    pub layout: ShelfLayout,
}

impl HomeShelf {
    fn browse_keys(&self) -> HashSet<&str> {
        self.items
            .iter()
            .filter_map(|item| item.browse_id.as_deref())
            .collect()
    }
}

/// Filters out video shelves and video items, dropping any shelf that becomes empty.
pub fn without_videos(shelves: Vec<HomeShelf>) -> Vec<HomeShelf> {
    shelves
        .into_iter()
        .filter_map(|mut shelf| {
            if shelf.layout == ShelfLayout::Video {
                return None;
            }
            shelf.items.retain(|item| !item.is_video);
            if shelf.items.is_empty() {
                None
            } else {
                Some(shelf)
            }
        })
        .collect()
}

/// Decides shelf layout matching YouTube Music / BitChord web rules.
pub fn shelf_layout_for(title: &str, items: &[ShelfItem], from_list: bool) -> ShelfLayout {
    let Some(first) = items.first() else {
        return ShelfLayout::Cards;
    };
    if from_list && first.video_id.is_some() {
        return ShelfLayout::List;
    }
    if title.trim().eq_ignore_ascii_case("Listen again") {
        return ShelfLayout::List;
    }
    let video_count = items.iter().filter(|item| item.is_video).count();
    if video_count * 2 > items.len() {
        return ShelfLayout::Video;
    }
    ShelfLayout::Cards
}

const REPEAT_SHELF_SHARE: f64 = 0.6;

/// Drops browsable shelves that repeat >= 60% of the smaller shelf's items from `existing` or earlier items.
pub fn without_repeats_of(shelves: Vec<HomeShelf>, existing: &[HomeShelf]) -> Vec<HomeShelf> {
    let mut shown: Vec<HomeShelf> = existing.to_vec();
    let mut out = Vec::with_capacity(shelves.len());
    for shelf in shelves {
        let keys = shelf.browse_keys();
        let is_repeat = !keys.is_empty()
            && shown.iter().any(|other| {
                let other_keys = other.browse_keys();
                let shared = other_keys.iter().filter(|k| keys.contains(**k)).count();
                let min_size = shelf.items.len().min(other.items.len());
                shared > 0 && (shared as f64) >= REPEAT_SHELF_SHARE * (min_size as f64)
            });
        if !is_repeat {
            shown.push(shelf.clone());
            out.push(shelf);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeFeed {
    pub shelves: Vec<HomeShelf>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoodGenreSection {
    pub title: String,
    pub items: Vec<MoodGenre>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoodGenre {
    pub title: String,
    pub browse_id: String,
    pub params: Option<String>,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub stripe_color: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPage {
    pub liked_songs: Vec<Song>,
    pub library_songs: Vec<Song>,
    pub shelves: Vec<HomeShelf>,
    #[serde(default)]
    pub liked_continuation: Option<String>,
}

impl LibraryPage {
    pub fn is_empty(&self) -> bool {
        self.liked_songs.is_empty() && self.library_songs.is_empty() && self.shelves.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryState {
    pub playlist_id: String,
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionState {
    pub channel_id: String,
    pub subscribed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum UiState<T> {
    Loading,
    Success { data: T },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailPage {
    pub browse_id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
    pub songs: UiState<Vec<Song>>,
    #[serde(default)]
    pub r#type: BrowseType,
    #[serde(default)]
    pub continuation: Option<String>,
    #[serde(default)]
    pub sections: Vec<HomeShelf>,
    #[serde(default)]
    pub suggested_songs: Vec<Song>,
    #[serde(default)]
    pub library: Option<LibraryState>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subscriber_count_text: Option<String>,
    #[serde(default)]
    pub monthly_listener_count: Option<String>,
    #[serde(default)]
    pub subscription: Option<SubscriptionState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistPage {
    pub songs: Vec<Song>,
    pub more_songs_browse_id: Option<String>,
    pub sections: Vec<HomeShelf>,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subscriber_count_text: Option<String>,
    #[serde(default)]
    pub monthly_listener_count: Option<String>,
    #[serde(default)]
    pub subscription: Option<SubscriptionState>,
    #[serde(default)]
    pub radio: Option<crate::top_picks::WatchTarget>,
    #[serde(default)]
    pub shuffle: Option<crate::top_picks::WatchTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LikeStatus {
    Like,
    Dislike,
    Indifferent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlaylistPrivacy {
    Private,
    Unlisted,
    Public,
}

impl PlaylistPrivacy {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Private => "Private",
            Self::Unlisted => "Unlisted",
            Self::Public => "Public",
        }
    }

    pub const fn api_value(self) -> &'static str {
        match self {
            Self::Private => "PRIVATE",
            Self::Unlisted => "UNLISTED",
            Self::Public => "PUBLIC",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPlaylist {
    pub playlist_id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
}

impl UserPlaylist {
    pub fn browse_id(&self) -> String {
        format!("VL{}", self.playlist_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongMenu {
    pub like_status: Option<LikeStatus>,
    pub in_library: bool,
    pub add_to_library_token: Option<String>,
    pub remove_from_library_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album_item(id: &str) -> ShelfItem {
        ShelfItem {
            title: id.to_owned(),
            browse_id: Some(id.to_owned()),
            ..ShelfItem::default()
        }
    }

    fn song_item(id: &str) -> ShelfItem {
        ShelfItem {
            title: id.to_owned(),
            video_id: Some(id.to_owned()),
            ..ShelfItem::default()
        }
    }

    fn test_song(id: &str, title: &str, artist: &str, slot: Option<&str>) -> Song {
        Song {
            video_id: id.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            set_video_id: slot.map(str::to_owned),
            ..Song::default()
        }
    }

    #[test]
    fn duration_is_shared_across_targets() {
        assert_eq!(225_000, duration_millis(Some("3:45")));
        assert_eq!(3_723_000, duration_millis(Some("1:02:03")));
        assert_eq!(0, duration_millis(Some("not-a-duration")));
        assert_eq!(0, duration_millis(None));
    }

    #[test]
    fn artwork_size_hint_is_shared_across_targets() {
        assert_eq!(
            Some("https://example.test/w720-h720.jpg".to_owned()),
            artwork_at(Some("https://example.test/w120-h120.jpg"), 720),
        );
        assert_eq!(
            Some("https://example.test/cover.jpg".to_owned()),
            artwork_at(Some("https://example.test/cover.jpg"), 720),
        );
        assert_eq!(
            Some("https://yt3.ggpht.com/ytc/AIdro_x123=s512-c-k-c0x00ffffff-no-rj".to_owned()),
            artwork_at(
                Some("https://yt3.ggpht.com/ytc/AIdro_x123=s88-c-k-c0x00ffffff-no-rj"),
                512
            ),
        );
        assert_eq!(
            Some("https://lh3.googleusercontent.com/a/ACg8oc=s512-c".to_owned()),
            artwork_at(
                Some("https://lh3.googleusercontent.com/a/ACg8oc=s96-c"),
                512
            ),
        );
        assert_eq!(
            Some("https://example.test/cover=s88.jpg".to_owned()),
            artwork_at(Some("https://example.test/cover=s88.jpg"), 512),
        );
        assert_eq!(
            Some("https://notggpht.com/avatar=s88.jpg".to_owned()),
            artwork_at(Some("https://notggpht.com/avatar=s88.jpg"), 512),
        );
    }

    #[test]
    fn a_reordered_release_shelf_under_another_title_is_a_repeat() {
        let albums = HomeShelf {
            title: "Albums & singles".to_owned(),
            items: (1..=10).map(|i| album_item(&format!("a{i}"))).collect(),
            ..HomeShelf::default()
        };
        let mut reordered_items: Vec<ShelfItem> = (2..=10)
            .rev()
            .map(|i| album_item(&format!("a{i}")))
            .collect();
        reordered_items.push(album_item("b"));
        let reordered = HomeShelf {
            title: "New albums & singles".to_owned(),
            items: reordered_items,
            ..HomeShelf::default()
        };
        let mut other_items: Vec<ShelfItem> =
            (1..=10).map(|i| album_item(&format!("c{i}"))).collect();
        other_items.push(album_item("a1"));
        let other = HomeShelf {
            title: "Charts".to_owned(),
            items: other_items,
            ..HomeShelf::default()
        };

        assert_eq!(
            vec![other.clone()],
            without_repeats_of(
                vec![reordered.clone(), other],
                std::slice::from_ref(&albums)
            ),
        );
        assert_eq!(
            vec![albums.clone()],
            without_repeats_of(vec![albums, reordered], &[]),
        );
    }

    #[test]
    fn song_shelves_and_a_few_shared_albums_are_kept() {
        let recents = HomeShelf {
            title: "Recents".to_owned(),
            items: (1..=10).map(|i| song_item(&format!("s{i}"))).collect(),
            ..HomeShelf::default()
        };
        let mut listen_items: Vec<ShelfItem> =
            (1..=10).map(|i| song_item(&format!("s{i}"))).collect();
        listen_items.push(album_item("a1"));
        listen_items.push(album_item("a2"));
        let listen_again = HomeShelf {
            title: "Listen again".to_owned(),
            items: listen_items,
            ..HomeShelf::default()
        };
        let albums = HomeShelf {
            title: "Albums & singles".to_owned(),
            items: (1..=10).map(|i| album_item(&format!("a{i}"))).collect(),
            ..HomeShelf::default()
        };

        assert_eq!(
            vec![listen_again.clone()],
            without_repeats_of(vec![listen_again], &[recents, albums]),
        );
    }

    #[test]
    fn playing_row_matcher_replicates_upstream_cases() {
        let track = test_song("track", "Song", "Artist", None);
        let video = test_song("video", "Song", "Artist", None);
        let rows = vec![track.clone(), video.clone()];
        let m_video = playing_row_matcher(&rows, Some(&video));
        assert_eq!(
            vec!["video"],
            rows.iter()
                .filter(|r| m_video.matches(r))
                .map(|r| r.video_id.as_str())
                .collect::<Vec<_>>()
        );

        let stereo = test_song("src:a::tidal:1", "Song", "Artist", None);
        let atmos = test_song("src:a::tidal:2", "Song", "Artist", None);
        let addon_rows = vec![stereo.clone(), atmos];
        let m_stereo = playing_row_matcher(&addon_rows, Some(&stereo));
        assert_eq!(
            vec!["src:a::tidal:1"],
            addon_rows
                .iter()
                .filter(|r| m_stereo.matches(r))
                .map(|r| r.video_id.as_str())
                .collect::<Vec<_>>()
        );

        let playing = test_song("local:42", "Song", "Artist", None);
        let yt_rows = vec![
            test_song("other", "Else", "Artist", None),
            test_song("yt1", "Song", "Artist", None),
            test_song("yt2", "Song", "Artist", None),
        ];
        let m_local = playing_row_matcher(&yt_rows, Some(&playing));
        assert_eq!(
            vec!["yt1"],
            yt_rows
                .iter()
                .filter(|r| m_local.matches(r))
                .map(|r| r.video_id.as_str())
                .collect::<Vec<_>>()
        );

        let first = test_song("x", "Song", "Artist", Some("s1"));
        let second = test_song("x", "Song", "Artist", Some("s2"));
        let dup_rows = vec![first, second.clone()];
        let m_slot = playing_row_matcher(&dup_rows, Some(&second));
        let matched: Vec<&Song> = dup_rows.iter().filter(|r| m_slot.matches(r)).collect();
        assert_eq!(1, matched.len());
        assert_eq!(Some("s2"), matched[0].set_video_id.as_deref());

        let m_none = playing_row_matcher(&[test_song("a", "Song", "Artist", None)], None);
        assert_eq!(PlayingRowMatch::None, m_none);
    }
}
