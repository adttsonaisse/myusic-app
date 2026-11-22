use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use bitchord_model::{
    Account, AccountChannel, ArtistNameIndex, ArtistPage, ArtistRef, BrowseItem, BrowseType,
    HomeShelf, LibraryState, LikeStatus, MoodGenre, MoodGenreSection, SearchResult, Segment,
    ShelfItem, Song, SongMenu, SubscriptionState, UserPlaylist, VersionPair, VersionTimeMap,
    WatchTarget, complete_artist_credits, shelf_layout_for,
};
use percent_encoding::percent_decode_str;
use regex::Regex;
use serde_json::{Map, Value};

use crate::auth::normalize_data_sync_id;
use crate::uploaded::UploadedTracks;

const MUSIC_VIDEO_TYPE_ATV: &str = "MUSIC_VIDEO_TYPE_ATV";
const HEADER_RENDERERS: &[&str] = &["musicResponsiveHeaderRenderer", "musicDetailHeaderRenderer"];
const HEADER_CREDIT_LINES: &[&str] = &["straplineTextOne", "subtitle"];
const RELEASE_WORDS: &[&str] = &["album", "single", "ep"];
const TYPE_WORDS: &[&str] = &[
    "song", "video", "album", "single", "ep", "artist", "playlist", "podcast", "episode",
];
const LIBRARY_ICONS: &[&str] = &["LIBRARY_ADD", "LIBRARY_REMOVE", "LIBRARY_SAVED"];
const OWNER_ICONS: &[&str] = &["DELETE", "EDIT"];
const NOT_EDITABLE: &[&str] = &["LM", "SE", "RD", "OLAK", "MPRE"];

static DURATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d+:\d{2}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid DURATION_RE")
});

static RUNTIME_PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\d+)\s*(hours?|hr|min|sec)")
        // INFALLIBLE: static regex pattern.
        .expect("valid RUNTIME_PART")
});

static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{4}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid YEAR_RE")
});

static TALLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^[\d.,]+\s*[KMB]?\s+(plays|views|likes|songs|tracks|subscribers|hours?|minutes?|seconds?)\b.*$",
    )
    // INFALLIBLE: static regex pattern.
    .expect("valid TALLY_RE")
});

static VIDEO_WORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bvideos?\b")
        // INFALLIBLE: static regex pattern.
        .expect("valid VIDEO_WORD")
});

/// One page of search rows, plus continuation token if available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    pub rows: Vec<SearchResult>,
    pub continuation: Option<String>,
}

/// A playlist page's own tracks, YouTube's suggested additions, and continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistShelfPage {
    pub songs: Vec<Song>,
    pub suggested: Vec<Song>,
    pub continuation: Option<String>,
}

/// One page of saved library items plus continuation token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryItemPage {
    pub items: Vec<ShelfItem>,
    pub continuation: Option<String>,
}

/// Title, subtitle, and cover off an album/playlist/podcast browse page header.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseHeader {
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Credits {
    artist_id: Option<String>,
    artist_name: Option<String>,
    artists: Vec<ArtistRef>,
    album_id: Option<String>,
    album_name: Option<String>,
}

/// Parser for YouTube Music InnerTube JSON payloads.
///
/// Owns shared [`ArtistNameIndex`] and [`UploadedTracks`] registries so parsed
/// artist links and private-upload markers flow seamlessly into downstream
/// credit completion and stream resolution without global mutable state.
#[derive(Default)]
pub struct InnertubeParser {
    artist_index: Arc<ArtistNameIndex>,
    uploaded: Arc<UploadedTracks>,
}

impl InnertubeParser {
    pub fn new(artist_index: Arc<ArtistNameIndex>, uploaded: Arc<UploadedTracks>) -> Self {
        Self {
            artist_index,
            uploaded,
        }
    }

    pub fn artist_index(&self) -> &Arc<ArtistNameIndex> {
        &self.artist_index
    }

    pub fn uploaded_tracks(&self) -> &Arc<UploadedTracks> {
        &self.uploaded
    }

    // ---- Search -------------------------------------------------------------

    pub fn parse_search_songs(&self, response: &Value) -> Vec<Song> {
        self.parse_search(response)
            .into_iter()
            .filter_map(|r| match r {
                SearchResult::Track { song } => Some(song),
                _ => None,
            })
            .collect()
    }

    pub fn parse_search(&self, response: &Value) -> Vec<SearchResult> {
        self.parse_search_page(response, false).rows
    }

    pub fn parse_search_page(&self, response: &Value, include_videos: bool) -> SearchPage {
        let top_results: Vec<SearchResult> = if include_videos {
            Vec::new()
        } else {
            collect_renderers(response, "musicCardShelfRenderer")
                .into_iter()
                .filter_map(|card| {
                    if let Some(song) = self.parse_card_shelf_song(card) {
                        Some(SearchResult::TopTrack { song })
                    } else {
                        parse_card_shelf_browse(card).map(|item| SearchResult::Browse { item })
                    }
                })
                .collect()
        };

        let rows = collect_renderers(response, "musicResponsiveListItemRenderer");

        let card_credits: Vec<(*const Map<String, Value>, Credits)> = if include_videos {
            Vec::new()
        } else {
            collect_renderers(response, "musicCardShelfRenderer")
                .into_iter()
                .flat_map(|card| {
                    let Some(credit) = card_shelf_credit(card) else {
                        return Vec::new();
                    };
                    collect_renderers_map(card, "musicResponsiveListItemRenderer")
                        .into_iter()
                        .map(|r| (std::ptr::from_ref(r), credit.clone()))
                        .collect()
                })
                .collect()
        };

        let mut seen = HashSet::new();
        let mut parsed = Vec::new();

        for result in top_results {
            match result {
                SearchResult::TopTrack { song } => {
                    if !song.is_video && seen.insert(format!("v:{}", song.video_id)) {
                        parsed.push(SearchResult::TopTrack { song });
                    }
                }
                SearchResult::Browse { item } => {
                    if seen.insert(format!("b:{}", item.browse_id)) {
                        parsed.push(SearchResult::Browse { item });
                    }
                }
                SearchResult::Track { .. } => {}
            }
        }

        let default_credit = Credits::default();
        for renderer in rows {
            if let Some(browse) = parse_browse_item(renderer) {
                if seen.insert(format!("b:{}", browse.browse_id)) {
                    parsed.push(SearchResult::Browse { item: browse });
                }
            } else {
                let ptr = std::ptr::from_ref(renderer);
                let fallback = card_credits
                    .iter()
                    .find(|(p, _)| *p == ptr)
                    .map_or(&default_credit, |(_, c)| c);
                if let Some(song) = self.parse_responsive_list_item(Some(renderer), fallback)
                    && song.is_video == include_videos
                    && seen.insert(format!("v:{}", song.video_id))
                {
                    parsed.push(SearchResult::Track { song });
                }
            }
        }

        SearchPage {
            rows: parsed,
            continuation: self.continuation_token(response),
        }
    }

    pub fn parse_search_suggestions(&self, response: &Value) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for renderer in collect_renderers(response, "searchSuggestionRenderer") {
            let query = obj_val(renderer, "navigationEndpoint")
                .and_then(|o| obj_val(o, "searchEndpoint"))
                .and_then(|o| str_val(o, "query"))
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| runs_text(renderer.get("suggestion")));
            if !query.trim().is_empty() && seen.insert(query.clone()) {
                out.push(query);
            }
        }
        out
    }

    // ---- Home & Explore -----------------------------------------------------

    pub fn parse_home(&self, response: &Value) -> Vec<HomeShelf> {
        let sections = response
            .get("contents")
            .and_then(|v| v.get("singleColumnBrowseResultsRenderer"))
            .and_then(|v| v.get("tabs"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|v| v.get("tabRenderer"))
            .and_then(|v| v.get("content"))
            .and_then(|v| v.get("sectionListRenderer"))
            .and_then(|v| v.get("contents"))
            .and_then(Value::as_array);

        let Some(sections) = sections else {
            return Vec::new();
        };

        sections
            .iter()
            .filter_map(|section| {
                section
                    .get("musicCarouselShelfRenderer")
                    .and_then(Value::as_object)
                    .and_then(|c| self.carousel_shelf(c))
                    .or_else(|| {
                        section
                            .get("musicShelfRenderer")
                            .and_then(Value::as_object)
                            .and_then(|s| self.plain_shelf(s))
                    })
            })
            .collect()
    }

    pub fn parse_mood_and_genres(&self, response: &Value) -> Vec<MoodGenreSection> {
        let sections = response
            .get("contents")
            .and_then(|v| v.get("singleColumnBrowseResultsRenderer"))
            .and_then(|v| v.get("tabs"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|v| v.get("tabRenderer"))
            .and_then(|v| v.get("content"))
            .and_then(|v| v.get("sectionListRenderer"))
            .and_then(|v| v.get("contents"))
            .and_then(Value::as_array);

        let Some(sections) = sections else {
            return Vec::new();
        };

        let parsed: Vec<MoodGenreSection> = sections
            .iter()
            .filter_map(|section| {
                let grid = section.get("gridRenderer")?.as_object()?;
                let title = runs_text(
                    grid.get("header")
                        .and_then(|h| h.get("gridHeaderRenderer"))
                        .and_then(|h| h.get("title")),
                );
                let items: Vec<MoodGenre> = grid
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| {
                        let button = item.get("musicNavigationButtonRenderer")?.as_object()?;
                        let endpoint = button
                            .get("clickCommand")
                            .and_then(|c| c.get("browseEndpoint"))
                            .or_else(|| {
                                button
                                    .get("navigationEndpoint")
                                    .and_then(|n| n.get("browseEndpoint"))
                            })
                            .and_then(Value::as_object)?;
                        let browse_id = str_val(endpoint, "browseId")?.to_owned();
                        let label = runs_text(button.get("buttonText"));
                        if label.trim().is_empty() {
                            return None;
                        }
                        let stripe_color = button
                            .get("solid")
                            .and_then(Value::as_object)
                            .and_then(|s| s.get("leftStripeColor"))
                            .and_then(value_as_u64);
                        Some(MoodGenre {
                            title: label,
                            browse_id,
                            params: str_val(endpoint, "params").map(ToOwned::to_owned),
                            thumbnail_url: None,
                            stripe_color,
                        })
                    })
                    .collect();
                if title.trim().is_empty() || items.is_empty() {
                    None
                } else {
                    Some(MoodGenreSection { title, items })
                }
            })
            .collect();

        parsed
            .iter()
            .enumerate()
            .filter(|(idx, section)| {
                let later: HashSet<(&str, Option<&str>)> = parsed[idx + 1..]
                    .iter()
                    .flat_map(|s| {
                        s.items
                            .iter()
                            .map(|item| (item.browse_id.as_str(), item.params.as_deref()))
                    })
                    .collect();
                let is_shortcuts = section
                    .items
                    .iter()
                    .any(|item| later.contains(&(item.browse_id.as_str(), item.params.as_deref())));
                !is_shortcuts && !section.title.eq_ignore_ascii_case("For you")
            })
            .map(|(_, s)| s.clone())
            .collect()
    }

    pub fn parse_home_continuation(&self, root: &Value) -> Vec<HomeShelf> {
        let mut out = Vec::new();
        self.walk_home_continuation(root, &mut out);
        out
    }

    fn walk_home_continuation(&self, node: &Value, out: &mut Vec<HomeShelf>) {
        match node {
            Value::Object(map) => {
                if let Some(carousel) = map
                    .get("musicCarouselShelfRenderer")
                    .and_then(Value::as_object)
                    && let Some(shelf) = self.carousel_shelf(carousel)
                {
                    out.push(shelf);
                }
                if let Some(plain) = map.get("musicShelfRenderer").and_then(Value::as_object)
                    && let Some(shelf) = self.plain_shelf(plain)
                {
                    out.push(shelf);
                }
                for v in map.values() {
                    self.walk_home_continuation(v, out);
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    self.walk_home_continuation(v, out);
                }
            }
            _ => {}
        }
    }

    fn carousel_shelf(&self, carousel: &Map<String, Value>) -> Option<HomeShelf> {
        let header = carousel
            .get("header")
            .and_then(|h| h.get("musicCarouselShelfBasicHeaderRenderer"))
            .and_then(Value::as_object);
        let title = runs_text(header.and_then(|h| h.get("title")));
        let strapline = runs_text(header.and_then(|h| h.get("strapline")));
        let contents = carousel
            .get("contents")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        let default_credit = Credits::default();
        let parsed: Vec<ShelfItem> = contents
            .iter()
            .filter_map(|item| {
                parse_two_row_item(
                    item.get("musicTwoRowItemRenderer")
                        .and_then(Value::as_object),
                )
                .or_else(|| {
                    self.parse_responsive_list_item(
                        item.get("musicResponsiveListItemRenderer")
                            .and_then(Value::as_object),
                        &default_credit,
                    )
                    .map(|song| ShelfItem {
                        title: song.title,
                        subtitle: song.artist,
                        thumbnail_url: song.thumbnail_url,
                        video_id: Some(song.video_id),
                        browse_id: None,
                        is_video: song.is_video,
                    })
                })
                .or_else(|| {
                    parse_artist_row(
                        item.get("musicResponsiveListItemRenderer")
                            .and_then(Value::as_object),
                    )
                })
            })
            .collect();

        let items: Vec<ShelfItem> = if VIDEO_WORD.is_match(&title) {
            parsed
                .into_iter()
                .filter(|i| i.video_id.is_some())
                .collect()
        } else {
            parsed
        };
        if items.is_empty() {
            return None;
        }
        let shelf_title = if title.trim().is_empty() {
            "For you".to_owned()
        } else {
            title
        };
        let from_list = contents
            .first()
            .and_then(|v| v.get("musicResponsiveListItemRenderer"))
            .and_then(Value::as_object)
            .is_some();
        let layout = shelf_layout_for(&shelf_title, &items, from_list);
        Some(HomeShelf {
            title: shelf_title,
            items,
            subtitle: strapline,
            more_browse_id: None,
            more_params: None,
            layout,
        })
    }

    fn plain_shelf(&self, shelf: &Map<String, Value>) -> Option<HomeShelf> {
        let title = runs_text(shelf.get("title"));
        let default_credit = Credits::default();
        let items: Vec<ShelfItem> = shelf
            .get("contents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                self.parse_responsive_list_item(
                    item.get("musicResponsiveListItemRenderer")
                        .and_then(Value::as_object),
                    &default_credit,
                )
            })
            .map(|song| ShelfItem {
                title: song.title,
                subtitle: song.artist,
                thumbnail_url: song.thumbnail_url,
                video_id: Some(song.video_id),
                browse_id: None,
                is_video: song.is_video,
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        let shelf_title = if title.trim().is_empty() {
            "For you".to_owned()
        } else {
            title
        };
        let layout = shelf_layout_for(&shelf_title, &items, true);
        Some(HomeShelf {
            title: shelf_title,
            items,
            subtitle: String::new(),
            more_browse_id: None,
            more_params: None,
            layout,
        })
    }

    // ---- Artist Page --------------------------------------------------------

    pub fn parse_artist_page(&self, response: &Value) -> ArtistPage {
        let sections = response
            .get("contents")
            .and_then(|v| v.get("singleColumnBrowseResultsRenderer"))
            .and_then(|v| v.get("tabs"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|v| v.get("tabRenderer"))
            .and_then(|v| v.get("content"))
            .and_then(|v| v.get("sectionListRenderer"))
            .and_then(|v| v.get("contents"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        let header = response.get("header");
        let name = artist_name(header);
        let credit = Credits {
            artist_name: name.clone(),
            ..Default::default()
        };

        let mut songs = Vec::new();
        let mut more_songs: Option<String> = None;
        let mut shelves = Vec::new();

        for section in sections {
            if let Some(shelf) = section.get("musicShelfRenderer").and_then(Value::as_object) {
                if let Some(contents) = shelf.get("contents").and_then(Value::as_array) {
                    for row in contents {
                        if let Some(song) = self.parse_responsive_list_item(
                            row.get("musicResponsiveListItemRenderer")
                                .and_then(Value::as_object),
                            &credit,
                        ) {
                            songs.push(song);
                        }
                    }
                }
                if more_songs.is_none() {
                    more_songs = shelf
                        .get("title")
                        .and_then(|t| t.get("runs"))
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|r| r.get("navigationEndpoint"))
                        .and_then(|n| n.get("browseEndpoint"))
                        .and_then(Value::as_object)
                        .and_then(|b| str_val(b, "browseId"))
                        .map(ToOwned::to_owned);
                }
            }
            if let Some(carousel) = section
                .get("musicCarouselShelfRenderer")
                .and_then(Value::as_object)
            {
                let ch = carousel
                    .get("header")
                    .and_then(|h| h.get("musicCarouselShelfBasicHeaderRenderer"))
                    .and_then(Value::as_object);
                let title = runs_text(ch.and_then(|h| h.get("title")));
                let items: Vec<ShelfItem> = carousel
                    .get("contents")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| {
                        parse_two_row_item(
                            item.get("musicTwoRowItemRenderer")
                                .and_then(Value::as_object),
                        )
                    })
                    .filter(|i| i.browse_id.is_some() || i.is_video)
                    .collect();

                let more_endpoint = ch.and_then(extract_carousel_more_endpoint);
                let more_browse_id = more_endpoint
                    .and_then(|e| str_val(e, "browseId"))
                    .map(ToOwned::to_owned);
                let more_params = more_endpoint
                    .and_then(|e| str_val(e, "params"))
                    .map(ToOwned::to_owned);

                if !title.trim().is_empty() && !items.is_empty() {
                    let layout = shelf_layout_for(&title, &items, false);
                    shelves.push(HomeShelf {
                        title,
                        items,
                        subtitle: String::new(),
                        more_browse_id,
                        more_params,
                        layout,
                    });
                }
            }
        }

        ArtistPage {
            songs,
            more_songs_browse_id: more_songs,
            sections: shelves,
            thumbnail_url: artist_thumbnail(header),
            name,
            description: self.parse_description(response),
            subscriber_count_text: subscriber_count(header),
            monthly_listener_count: monthly_listeners(header),
            subscription: subscription(header),
            radio: header_watch_target(header, "startRadioButton"),
            shuffle: header_watch_target(header, "playButton"),
        }
    }

    pub fn parse_two_row_items_deep(&self, root: &Value) -> Vec<ShelfItem> {
        collect_renderers(root, "musicTwoRowItemRenderer")
            .into_iter()
            .filter_map(|r| parse_two_row_item(Some(r)))
            .collect()
    }

    // ---- Generic / Deep Walk ------------------------------------------------

    pub fn collect_songs_deep(&self, root: &Value, show: Option<&str>) -> Vec<Song> {
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Song> = HashMap::new();
        let page_credit = self.page_credit(root);
        self.walk_songs_deep(root, &page_credit, show, &mut order, &mut map);
        order.into_iter().filter_map(|id| map.remove(&id)).collect()
    }

    fn walk_songs_deep(
        &self,
        node: &Value,
        page_credit: &Credits,
        show: Option<&str>,
        order: &mut Vec<String>,
        map: &mut HashMap<String, Song>,
    ) {
        match node {
            Value::Object(obj) => {
                if let Some(renderer) = obj
                    .get("musicResponsiveListItemRenderer")
                    .and_then(Value::as_object)
                    && let Some(song) = self.parse_responsive_list_item(Some(renderer), page_credit)
                {
                    if !map.contains_key(&song.video_id) {
                        order.push(song.video_id.clone());
                    }
                    map.insert(song.video_id.clone(), song);
                }
                if let Some(renderer) = obj
                    .get("musicMultiRowListItemRenderer")
                    .and_then(Value::as_object)
                    && let Some(song) = parse_multi_row_list_item(Some(renderer), show)
                {
                    if !map.contains_key(&song.video_id) {
                        order.push(song.video_id.clone());
                    }
                    map.insert(song.video_id.clone(), song);
                }
                for v in obj.values() {
                    self.walk_songs_deep(v, page_credit, show, order, map);
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    self.walk_songs_deep(v, page_credit, show, order, map);
                }
            }
            _ => {}
        }
    }

    pub fn podcast_show(&self, root: &Value) -> Option<String> {
        if collect_renderers(root, "musicMultiRowListItemRenderer").is_empty() {
            return None;
        }
        self.parse_browse_header(root).map(|h| h.title)
    }

    pub fn parse_playlist_shelf(&self, root: &Value) -> Option<PlaylistShelfPage> {
        let scope = root.get("continuationContents").or_else(|| {
            root.get("contents")
                .and_then(|c| c.get("twoColumnBrowseResultsRenderer"))
                .and_then(|c| c.get("secondaryContents"))
        })?;
        let playlist_scope = scope
            .get("musicPlaylistShelfContinuation")
            .and_then(Value::as_object)
            .or_else(|| {
                collect_renderers(scope, "musicPlaylistShelfRenderer")
                    .into_iter()
                    .next()
            });
        let suggestion_shelves: Vec<&Map<String, Value>> =
            collect_renderers(scope, "musicShelfRenderer")
                .into_iter()
                .filter(|s| runs_text(s.get("title")) == "Suggestions")
                .collect();
        if playlist_scope.is_none() && suggestion_shelves.is_empty() {
            return None;
        }

        let page_credit = self.page_credit(root);
        let mut seen_songs = HashSet::new();
        let songs: Vec<Song> = playlist_scope
            .map(|pl| {
                collect_renderers_map(pl, "musicResponsiveListItemRenderer")
                    .into_iter()
                    .filter_map(|r| self.parse_responsive_list_item(Some(r), &page_credit))
                    .filter(|s| seen_songs.insert(s.video_id.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut seen_suggested = HashSet::new();
        let suggested: Vec<Song> = suggestion_shelves
            .into_iter()
            .flat_map(|s| collect_renderers_map(s, "musicResponsiveListItemRenderer"))
            .filter_map(|r| self.parse_responsive_list_item(Some(r), &page_credit))
            .filter(|s| {
                !seen_songs.contains(&s.video_id) && seen_suggested.insert(s.video_id.clone())
            })
            .collect();

        let continuation = playlist_scope.and_then(continuation_token_map);
        Some(PlaylistShelfPage {
            songs,
            suggested,
            continuation,
        })
    }

    pub fn parse_library_items(&self, root: &Value) -> Vec<ShelfItem> {
        self.parse_library_item_page(root).items
    }

    pub fn parse_library_item_page(&self, root: &Value) -> LibraryItemPage {
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, ShelfItem> = HashMap::new();

        for renderer in collect_renderers(root, "musicTwoRowItemRenderer") {
            if let Some(item) = parse_two_row_item(Some(renderer))
                && let Some(ref bid) = item.browse_id
                && !map.contains_key(bid)
            {
                order.push(bid.clone());
                map.insert(bid.clone(), item);
            }
        }
        for renderer in collect_renderers(root, "musicResponsiveListItemRenderer") {
            if let Some(item) = parse_browse_item(renderer)
                && !map.contains_key(&item.browse_id)
            {
                order.push(item.browse_id.clone());
                map.insert(
                    item.browse_id.clone(),
                    ShelfItem {
                        title: item.title,
                        subtitle: item.subtitle,
                        thumbnail_url: item.thumbnail_url,
                        video_id: None,
                        browse_id: Some(item.browse_id),
                        is_video: false,
                    },
                );
            }
        }

        let items = order.into_iter().filter_map(|k| map.remove(&k)).collect();
        LibraryItemPage {
            items,
            continuation: self.continuation_token(root),
        }
    }

    pub fn continuation_token(&self, root: &Value) -> Option<String> {
        if let Some(endpoint) = collect_renderers(root, "continuationItemRenderer")
            .into_iter()
            .next()
            .and_then(|r| obj_val(r, "continuationEndpoint"))
        {
            let cmd = obj_val(endpoint, "continuationCommand").or_else(|| {
                obj_val(endpoint, "commandExecutorCommand")
                    .and_then(|c| c.get("commands"))
                    .and_then(Value::as_array)
                    .and_then(|cmds| {
                        cmds.iter()
                            .find_map(|c| c.get("continuationCommand").and_then(Value::as_object))
                    })
            });
            if let Some(tok) = cmd.and_then(|c| str_val(c, "token")) {
                return Some(tok.to_owned());
            }
        }
        collect_renderers(root, "nextContinuationData")
            .into_iter()
            .next()
            .and_then(|n| str_val(n, "continuation"))
            .map(ToOwned::to_owned)
    }

    // ---- Headers, Account, Watch Queue, Menus -------------------------------

    pub fn parse_browse_header(&self, root: &Value) -> Option<BrowseHeader> {
        let header = HEADER_RENDERERS
            .iter()
            .find_map(|name| collect_renderers(root, name).into_iter().next())?;
        let title = runs_text(header.get("title"));
        if title.trim().is_empty() {
            return None;
        }
        let mut seen = HashSet::new();
        let subtitle_parts: Vec<String> = HEADER_CREDIT_LINES
            .iter()
            .map(|line| runs_text(header.get(*line)))
            .filter(|s| !s.trim().is_empty() && seen.insert(s.clone()))
            .collect();
        let thumbnail_url = collect_renderers_map(header, "musicThumbnailRenderer")
            .into_iter()
            .next()
            .and_then(|t| obj_val(t, "thumbnail"))
            .and_then(|t| t.get("thumbnails"))
            .and_then(Value::as_array)
            .and_then(|a| best_thumbnail(a));
        Some(BrowseHeader {
            title,
            subtitle: subtitle_parts.join(" • "),
            thumbnail_url,
        })
    }

    pub fn parse_album_playlist_id(&self, root: &Value) -> Option<String> {
        if let Some(header) = HEADER_RENDERERS
            .iter()
            .find_map(|name| collect_renderers(root, name).into_iter().next())
        {
            for btn in collect_renderers_map(header, "musicPlayButtonRenderer") {
                if let Some(id) = playlist_id_from_play_action(btn) {
                    return Some(id);
                }
            }
            for btn in collect_renderers_map(header, "buttonRenderer") {
                if let Some(id) = playlist_id_from_play_action(btn) {
                    return Some(id);
                }
            }
        }
        let canonical = root
            .get("microformat")
            .and_then(|m| m.get("microformatDataRenderer"))
            .and_then(Value::as_object)
            .and_then(|m| str_val(m, "urlCanonical"))?;
        let (_, after) = canonical.split_once("list=")?;
        let id = after.split('&').next().unwrap_or_default();
        if id.trim().is_empty() {
            None
        } else {
            Some(id.to_owned())
        }
    }

    pub fn parse_description(&self, root: &Value) -> Option<String> {
        if let Some(shelf) = collect_renderers(root, "musicDescriptionShelfRenderer")
            .into_iter()
            .next()
        {
            let desc = runs_text(shelf.get("description"));
            if !desc.trim().is_empty() {
                return Some(desc);
            }
        }
        for name in HEADER_RENDERERS
            .iter()
            .copied()
            .chain(std::iter::once("musicImmersiveHeaderRenderer"))
        {
            if let Some(hdr) = collect_renderers(root, name).into_iter().next() {
                let desc = runs_text(hdr.get("description"));
                if !desc.trim().is_empty() {
                    return Some(desc);
                }
            }
        }
        None
    }

    pub fn parse_account(&self, response: &Value) -> Option<Account> {
        let header = collect_renderers(response, "activeAccountHeaderRenderer")
            .into_iter()
            .next()?;
        let name = runs_text(header.get("accountName"));
        if name.trim().is_empty() {
            return None;
        }
        let email = {
            let r = runs_text(header.get("email"));
            if !r.trim().is_empty() {
                r
            } else if let Some(s) = header
                .get("email")
                .and_then(Value::as_object)
                .and_then(|o| str_val(o, "simpleText"))
                .filter(|s| !s.trim().is_empty())
            {
                s.to_owned()
            } else {
                runs_text(header.get("channelHandle"))
            }
        };
        let thumbnail_url = header
            .get("accountPhoto")
            .and_then(|p| p.get("thumbnails"))
            .and_then(Value::as_array)
            .and_then(|a| best_thumbnail(a));
        Some(Account {
            name,
            email,
            thumbnail_url,
        })
    }

    pub fn parse_account_channels(&self, root: &Value) -> Vec<AccountChannel> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for item in collect_renderers(root, "accountItem") {
            let name = text_or_simple(item.get("accountName"));
            if name.trim().is_empty() {
                continue;
            }
            let page_id = find_string_map(item, "pageId");
            let data_sync_id =
                normalize_data_sync_id(find_string_map(item, "datasyncIdToken").as_deref());
            if page_id.is_none() && data_sync_id.is_none() {
                continue;
            }
            let subtitle = {
                let h = text_or_simple(item.get("channelHandle"));
                if !h.trim().is_empty() {
                    h
                } else {
                    text_or_simple(item.get("accountByline"))
                }
            };
            let thumbnail_url = item
                .get("accountPhoto")
                .and_then(|p| p.get("thumbnails"))
                .and_then(Value::as_array)
                .and_then(|a| best_thumbnail(a));
            let active_on_web = match item.get("isSelected") {
                Some(Value::Bool(b)) => *b,
                Some(Value::String(s)) => s == "true",
                _ => false,
            };
            let channel = AccountChannel {
                name,
                subtitle,
                thumbnail_url,
                page_id,
                data_sync_id,
                active_on_web,
            };
            if seen.insert(channel.key().to_owned()) {
                out.push(channel);
            }
        }
        out
    }

    pub fn watch_queue_pairs(&self, root: &Value) -> (usize, usize) {
        let wrappers = collect_renderers(root, "playlistPanelVideoWrapperRenderer");
        let audio_count = wrappers
            .iter()
            .filter(|w| wrapper_renderers(w).iter().any(|r| is_audio_track(r)))
            .count();
        (wrappers.len(), audio_count)
    }

    pub fn parse_watch_queue(
        &self,
        root: &Value,
        prefer_audio: bool,
        prefer_video: bool,
    ) -> Vec<Song> {
        let mut replacements: HashMap<String, &Map<String, Value>> = HashMap::new();
        let mut dropped: HashSet<String> = HashSet::new();

        if prefer_audio || prefer_video {
            for wrapper in collect_renderers(root, "playlistPanelVideoWrapperRenderer") {
                let halves = wrapper_renderers(wrapper);
                let Some(&primary) = halves.first() else {
                    continue;
                };
                let Some(&chosen) = halves.iter().find(|r| {
                    if prefer_video {
                        !is_audio_track(r)
                    } else {
                        is_audio_track(r)
                    }
                }) else {
                    continue;
                };
                let Some(primary_id) = str_val(primary, "videoId") else {
                    continue;
                };
                replacements.insert(primary_id.to_owned(), chosen);
                for &half in &halves {
                    if !std::ptr::eq(half, primary)
                        && let Some(id) = str_val(half, "videoId")
                    {
                        dropped.insert(id.to_owned());
                    }
                }
            }
        }

        let mut order: Vec<String> = Vec::new();
        let mut out: HashMap<String, Song> = HashMap::new();
        for found in collect_renderers(root, "playlistPanelVideoRenderer") {
            let Some(found_id) = str_val(found, "videoId") else {
                continue;
            };
            if dropped.contains(found_id) {
                continue;
            }
            let target = replacements.get(found_id).copied().unwrap_or(found);
            if let Some(song) = self.panel_song(target) {
                if !out.contains_key(&song.video_id) {
                    order.push(song.video_id.clone());
                }
                out.insert(song.video_id.clone(), song);
            }
        }
        order.into_iter().filter_map(|id| out.remove(&id)).collect()
    }

    pub fn parse_version_pair(
        &self,
        root: &Value,
        video_id: &str,
        want_audio: bool,
    ) -> Option<VersionPair> {
        let fits = |r: &Map<String, Value>| -> bool {
            match music_video_type(r) {
                Some(kind) => (kind == MUSIC_VIDEO_TYPE_ATV) == want_audio,
                None => true,
            }
        };

        for wrapper in collect_renderers(root, "playlistPanelVideoWrapperRenderer") {
            let Some(primary) = obj_val(wrapper, "primaryRenderer")
                .and_then(|o| obj_val(o, "playlistPanelVideoRenderer"))
            else {
                continue;
            };
            let counterparts: Vec<&Map<String, Value>> = wrapper
                .get("counterpart")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
                .collect();

            if str_val(primary, "videoId") == Some(video_id) {
                let Some(&matched) = counterparts
                    .iter()
                    .find(|cp| counterpart_renderer(cp).is_some_and(fits))
                else {
                    continue;
                };
                let Some(renderer) = counterpart_renderer(matched) else {
                    continue;
                };
                let Some(mut song) = self.panel_song(renderer) else {
                    continue;
                };
                song.is_video = !want_audio;
                return Some(VersionPair {
                    song,
                    time_map: time_map_of(matched),
                });
            }

            let Some(&as_counterpart) = counterparts.iter().find(|cp| {
                counterpart_renderer(cp).and_then(|r| str_val(r, "videoId")) == Some(video_id)
            }) else {
                continue;
            };
            if !fits(primary) {
                continue;
            }
            let Some(mut song) = self.panel_song(primary) else {
                continue;
            };
            song.is_video = !want_audio;
            return Some(VersionPair {
                song,
                time_map: time_map_of(as_counterpart).map(|m| m.inverted()),
            });
        }
        None
    }

    pub fn parse_song_menu(&self, root: &Value, video_id: &str) -> Option<SongMenu> {
        let row = collect_renderers(root, "playlistPanelVideoRenderer")
            .into_iter()
            .find(|r| str_val(r, "videoId") == Some(video_id))?;

        let like_status = collect_renderers_map(row, "likeButtonRenderer")
            .into_iter()
            .next()
            .and_then(|b| str_val(b, "likeStatus"))
            .and_then(|s| match s {
                "LIKE" => Some(LikeStatus::Like),
                "DISLIKE" => Some(LikeStatus::Dislike),
                "INDIFFERENT" => Some(LikeStatus::Indifferent),
                _ => None,
            });

        let toggle = collect_renderers_map(row, "toggleMenuServiceItemRenderer")
            .into_iter()
            .find(|t| {
                feedback_token_of(t, "defaultServiceEndpoint").is_some() && is_library_toggle(t)
            });

        let default_adds = toggle
            .and_then(|t| obj_val(t, "defaultIcon"))
            .and_then(|i| str_val(i, "iconType"))
            == Some("LIBRARY_ADD");
        let default_token = toggle.and_then(|t| feedback_token_of(t, "defaultServiceEndpoint"));
        let toggled_token = toggle.and_then(|t| feedback_token_of(t, "toggledServiceEndpoint"));

        let (add_token, remove_token) = if default_adds {
            (default_token, toggled_token)
        } else {
            (toggled_token, default_token)
        };

        Some(SongMenu {
            like_status,
            in_library: toggle.is_some() && !default_adds,
            add_to_library_token: add_token,
            remove_from_library_token: remove_token,
        })
    }

    pub fn parse_library_state(&self, root: &Value) -> Option<LibraryState> {
        let buttons = collect_renderers(root, "musicResponsiveHeaderRenderer")
            .into_iter()
            .next()?
            .get("buttons")
            .and_then(Value::as_array)?;

        let save = buttons.iter().find_map(|b| {
            let toggle = b.get("toggleButtonRenderer")?.as_object()?;
            is_save_toggle(toggle).then_some(toggle)
        })?;
        if value_is_true(save.get("isDisabled")) {
            return None;
        }
        let play = buttons.iter().find_map(|b| {
            b.get("musicPlayButtonRenderer")?
                .get("playNavigationEndpoint")?
                .as_object()
        })?;
        let playlist_id = obj_val(play, "watchPlaylistEndpoint")
            .and_then(|e| str_val(e, "playlistId"))
            .or_else(|| obj_val(play, "watchEndpoint").and_then(|e| str_val(e, "playlistId")))?
            .to_owned();
        Some(LibraryState {
            playlist_id,
            saved: value_is_true(save.get("isToggled")),
        })
    }

    pub fn parse_playlist_owned(&self, root: &Value) -> Option<bool> {
        if !collect_renderers(root, "musicEditablePlaylistDetailHeaderRenderer").is_empty() {
            return Some(true);
        }
        let header = collect_renderers(root, "musicResponsiveHeaderRenderer")
            .into_iter()
            .next()?;
        if collect_renderers_map(header, "menuNavigationItemRenderer")
            .into_iter()
            .any(|item| {
                obj_val(item, "icon")
                    .and_then(|i| str_val(i, "iconType"))
                    .is_some_and(|icon| OWNER_ICONS.contains(&icon))
            })
        {
            return Some(true);
        }
        let has_save_toggle = header
            .get("buttons")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|b| {
                b.get("toggleButtonRenderer")
                    .and_then(Value::as_object)
                    .is_some_and(is_save_toggle)
            });
        Some(!has_save_toggle)
    }

    pub fn parse_user_playlists(&self, root: &Value) -> Vec<UserPlaylist> {
        Self::user_playlists_from_items(&self.parse_library_items(root))
    }

    pub fn user_playlists_from_items(items: &[ShelfItem]) -> Vec<UserPlaylist> {
        items
            .iter()
            .filter_map(|item| {
                let bid = item.browse_id.as_deref()?;
                let raw = bid.strip_prefix("VL")?;
                if NOT_EDITABLE.iter().any(|p| raw.starts_with(p)) {
                    return None;
                }
                Some(UserPlaylist {
                    playlist_id: raw.to_owned(),
                    title: item.title.clone(),
                    subtitle: item.subtitle.clone(),
                    thumbnail_url: item.thumbnail_url.clone(),
                })
            })
            .collect()
    }

    pub fn artist_from_subtitle(subtitle: &str) -> String {
        subtitle
            .split(" • ")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .find(|p| {
                let lower = p.to_lowercase();
                !TYPE_WORDS.contains(&lower.as_str())
                    && !TALLY_RE.is_match(p)
                    && !DURATION_RE.is_match(p)
            })
            .unwrap_or(subtitle)
            .to_owned()
    }

    // ---- Internal Item Parsers ----------------------------------------------

    fn parse_responsive_list_item(
        &self,
        renderer: Option<&Map<String, Value>>,
        fallback: &Credits,
    ) -> Option<Song> {
        let renderer = renderer?;
        let video_id = obj_val(renderer, "playlistItemData")
            .and_then(|d| str_val(d, "videoId"))
            .or_else(|| {
                obj_val(renderer, "overlay")
                    .and_then(|o| obj_val(o, "musicItemThumbnailOverlayRenderer"))
                    .and_then(|o| obj_val(o, "content"))
                    .and_then(|o| obj_val(o, "musicPlayButtonRenderer"))
                    .and_then(|o| obj_val(o, "playNavigationEndpoint"))
                    .and_then(|o| obj_val(o, "watchEndpoint"))
                    .and_then(|o| str_val(o, "videoId"))
            })?;

        if is_private_upload(renderer) {
            self.uploaded.mark(video_id);
        }

        let columns = renderer
            .get("flexColumns")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let title = runs_text(
            columns
                .first()
                .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
                .and_then(|c| c.get("text")),
        );
        if title.trim().is_empty() {
            return None;
        }

        let subtitle_runs = columns
            .get(1)
            .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.get("runs"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let subtitle: String = subtitle_runs
            .iter()
            .filter_map(|r| r.as_object().and_then(|o| str_val(o, "text")))
            .collect();
        let parts: Vec<&str> = subtitle
            .split(" • ")
            .filter(|s| !s.trim().is_empty())
            .collect();

        let duration = parts
            .last()
            .copied()
            .filter(|p| DURATION_RE.is_match(p))
            .map(ToOwned::to_owned)
            .or_else(|| {
                renderer
                    .get("fixedColumns")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find_map(|col| {
                        let txt = runs_text(
                            col.get("musicResponsiveListItemFixedColumnRenderer")
                                .and_then(|c| c.get("text")),
                        );
                        DURATION_RE.is_match(&txt).then_some(txt)
                    })
            });

        let row_type = parts
            .iter()
            .map(|p| p.to_lowercase())
            .find(|p| TYPE_WORDS.contains(&p.as_str()));

        let artist_part = parts.iter().copied().find(|p| {
            let lower = p.to_lowercase();
            !DURATION_RE.is_match(p)
                && !TYPE_WORDS.contains(&lower.as_str())
                && !TALLY_RE.is_match(p)
        });

        let all_runs: Vec<&Value> = columns
            .iter()
            .filter_map(|c| {
                c.get("musicResponsiveListItemFlexColumnRenderer")?
                    .get("text")?
                    .get("runs")?
                    .as_array()
            })
            .flatten()
            .collect();
        let credits = self.credits_of(&all_runs);
        let credited_artists = artist_names_from_runs(subtitle_runs);

        let episode = row_type.as_deref() == Some("episode") || is_podcast_episode(renderer);
        let show = if episode {
            show_name_from_runs(subtitle_runs)
        } else {
            None
        };

        let thumbnails = obj_val(renderer, "thumbnail")
            .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
            .and_then(|t| obj_val(t, "thumbnail"))
            .and_then(|t| t.get("thumbnails"))
            .and_then(Value::as_array);

        let artist_display = credited_artists
            .or_else(|| {
                credits
                    .artist_name
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(ToOwned::to_owned)
            })
            .or(show)
            .or_else(|| artist_part.map(ToOwned::to_owned))
            .or_else(|| fallback.artist_name.clone())
            .unwrap_or_else(|| "Unknown artist".to_owned());

        let stated_artists = if credits.artists.is_empty() {
            &fallback.artists
        } else {
            &credits.artists
        };

        Some(Song {
            video_id: video_id.to_owned(),
            title,
            artist: artist_display,
            thumbnail_url: thumbnails.and_then(|a| best_thumbnail(a)),
            duration_text: duration,
            artist_id: credits.artist_id.or_else(|| fallback.artist_id.clone()),
            artists: complete_artist_credits(
                &self.artist_index,
                artist_part.unwrap_or_default(),
                stated_artists,
            ),
            album_id: credits.album_id.or_else(|| fallback.album_id.clone()),
            album_name: credits.album_name.or_else(|| fallback.album_name.clone()),
            set_video_id: obj_val(renderer, "playlistItemData")
                .and_then(|d| str_val(d, "playlistSetVideoId"))
                .map(ToOwned::to_owned),
            is_video: !episode
                && (row_type.as_deref() == Some("video")
                    || thumbnails.is_some_and(|a| is_not_square(a))),
            is_explicit: has_explicit_badge_map(renderer),
            feedback_token: extract_feedback_token(renderer),
            ..Song::default()
        })
    }

    fn parse_card_shelf_song(&self, renderer: &Map<String, Value>) -> Option<Song> {
        let video_id = obj_val(renderer, "onTap")
            .and_then(|t| obj_val(t, "watchEndpoint"))
            .and_then(|w| str_val(w, "videoId"))?;
        let title = runs_text(renderer.get("title"));
        if title.trim().is_empty() {
            return None;
        }
        let subtitle_runs = renderer
            .get("subtitle")
            .and_then(|s| s.get("runs"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let subtitle: String = subtitle_runs
            .iter()
            .filter_map(|r| r.as_object().and_then(|o| str_val(o, "text")))
            .collect();
        let parts: Vec<&str> = subtitle
            .split(" • ")
            .filter(|s| !s.trim().is_empty())
            .collect();
        let duration = parts
            .last()
            .copied()
            .filter(|p| DURATION_RE.is_match(p))
            .map(ToOwned::to_owned);
        let row_type = parts
            .iter()
            .map(|p| p.to_lowercase())
            .find(|p| TYPE_WORDS.contains(&p.as_str()));
        let run_refs: Vec<&Value> = subtitle_runs.iter().collect();
        let credits = self.credits_of(&run_refs);
        let credited_artists = artist_names_from_runs(subtitle_runs);
        let artist_part = parts.iter().copied().find(|p| {
            let lower = p.to_lowercase();
            !DURATION_RE.is_match(p)
                && !TYPE_WORDS.contains(&lower.as_str())
                && !TALLY_RE.is_match(p)
        });
        let thumbnails = obj_val(renderer, "thumbnail")
            .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
            .and_then(|t| obj_val(t, "thumbnail"))
            .and_then(|t| t.get("thumbnails"))
            .and_then(Value::as_array);

        let artist_display = credited_artists
            .or_else(|| {
                credits
                    .artist_name
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(ToOwned::to_owned)
            })
            .or_else(|| artist_part.map(ToOwned::to_owned))
            .unwrap_or_else(|| "Unknown artist".to_owned());

        Some(Song {
            video_id: video_id.to_owned(),
            title,
            artist: artist_display,
            thumbnail_url: thumbnails.and_then(|a| best_thumbnail(a)),
            duration_text: duration,
            artist_id: credits.artist_id,
            artists: complete_artist_credits(
                &self.artist_index,
                artist_part.unwrap_or_default(),
                &credits.artists,
            ),
            album_id: credits.album_id,
            album_name: credits.album_name,
            is_video: row_type.as_deref() == Some("video")
                || thumbnails.is_some_and(|a| is_not_square(a)),
            is_explicit: renderer.get("subtitleBadges").and_then(has_explicit_badge),
            ..Song::default()
        })
    }

    fn panel_song(&self, renderer: &Map<String, Value>) -> Option<Song> {
        let video_id = str_val(renderer, "videoId")?;
        let title = runs_text(renderer.get("title"));
        if title.trim().is_empty() {
            return None;
        }
        let byline_runs = renderer
            .get("longBylineText")
            .and_then(|l| l.get("runs"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let byline: Vec<&str> = byline_runs
            .iter()
            .filter_map(|r| r.as_object().and_then(|o| str_val(o, "text")))
            .collect();
        let artist: String = byline
            .iter()
            .take_while(|t| !t.contains('•'))
            .copied()
            .collect::<String>()
            .trim()
            .to_owned();
        let run_refs: Vec<&Value> = byline_runs.iter().collect();
        let credits = self.credits_of(&run_refs);
        let duration = {
            let txt = runs_text(renderer.get("lengthText"));
            if txt.trim().is_empty() {
                None
            } else {
                Some(txt)
            }
        };
        let is_video = byline
            .iter()
            .any(|t| t.to_ascii_lowercase().contains("views"));
        let completed_artists =
            complete_artist_credits(&self.artist_index, &artist, &credits.artists);
        Some(Song {
            video_id: video_id.to_owned(),
            title,
            artist,
            thumbnail_url: renderer
                .get("thumbnail")
                .and_then(|t| t.get("thumbnails"))
                .and_then(Value::as_array)
                .and_then(|a| best_thumbnail(a)),
            duration_text: duration,
            artist_id: credits.artist_id,
            artists: completed_artists,
            album_id: credits.album_id,
            album_name: credits.album_name,
            is_video,
            is_explicit: has_explicit_badge_map(renderer),
            ..Song::default()
        })
    }

    fn credits_of(&self, runs: &[&Value]) -> Credits {
        let mut credits = Credits::default();
        let mut artists = Vec::new();
        for run in runs {
            let Some(obj) = run.as_object() else {
                continue;
            };
            let Some(browse) =
                obj_val(obj, "navigationEndpoint").and_then(|n| obj_val(n, "browseEndpoint"))
            else {
                continue;
            };
            let Some(id) = str_val(browse, "browseId") else {
                continue;
            };
            let page_type = obj_val(browse, "browseEndpointContextSupportedConfigs")
                .and_then(|c| obj_val(c, "browseEndpointContextMusicConfig"))
                .and_then(|c| str_val(c, "pageType"))
                .unwrap_or_default();
            let is_artist = page_type.contains("ARTIST");
            let is_album = page_type.contains("ALBUM");
            if !is_artist && !is_album {
                continue;
            }
            let name = str_val(obj, "text").unwrap_or_default();
            if is_artist {
                if !name.trim().is_empty() {
                    artists.push(ArtistRef::new(name, Some(id.to_owned())));
                }
                if credits.artist_id.is_none() {
                    credits.artist_id = Some(id.to_owned());
                    credits.artist_name = Some(name.to_owned());
                }
            } else if credits.album_id.is_none() {
                credits.album_id = Some(id.to_owned());
                credits.album_name = Some(name.to_owned());
            }
        }
        self.artist_index.record(&artists);
        credits.artists = artists;
        credits
    }

    fn page_credit(&self, root: &Value) -> Credits {
        let Some(header) = HEADER_RENDERERS
            .iter()
            .find_map(|name| collect_renderers(root, name).into_iter().next())
        else {
            return Credits::default();
        };
        let lines: Vec<&[Value]> = HEADER_CREDIT_LINES
            .iter()
            .map(|key| {
                header
                    .get(*key)
                    .and_then(|v| v.get("runs"))
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            })
            .collect();

        let parts: Vec<String> = lines
            .iter()
            .flat_map(|line| {
                let joined: String = line
                    .iter()
                    .filter_map(|r| r.as_object().and_then(|o| str_val(o, "text")))
                    .collect();
                joined
                    .split(" • ")
                    .flat_map(|s| s.split(" · "))
                    .map(|s| s.trim().to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();

        if !parts
            .iter()
            .any(|p| RELEASE_WORDS.contains(&p.to_lowercase().as_str()))
        {
            return Credits::default();
        }

        let flat_runs: Vec<&Value> = lines.iter().flat_map(|l| l.iter()).collect();
        let mut credits = self.credits_of(&flat_runs);
        if let Some(credited) = lines.iter().find_map(|l| artist_names_from_runs(l)) {
            credits.artist_name = Some(credited);
            return credits;
        }
        let name = parts.into_iter().find(|p| {
            let lower = p.to_lowercase();
            !p.is_empty()
                && !TYPE_WORDS.contains(&lower.as_str())
                && !TALLY_RE.is_match(p)
                && !YEAR_RE.is_match(p)
                && !DURATION_RE.is_match(p)
        });
        credits.artist_name = name;
        credits
    }
}

// ---- Free Helper Functions --------------------------------------------------

fn parse_multi_row_list_item(
    renderer: Option<&Map<String, Value>>,
    show: Option<&str>,
) -> Option<Song> {
    let renderer = renderer?;
    let video_id = obj_val(renderer, "onTap")
        .and_then(|t| obj_val(t, "watchEndpoint"))
        .and_then(|w| str_val(w, "videoId"))
        .or_else(|| {
            obj_val(renderer, "overlay")
                .and_then(|o| obj_val(o, "musicItemThumbnailOverlayRenderer"))
                .and_then(|o| obj_val(o, "content"))
                .and_then(|o| obj_val(o, "musicPlayButtonRenderer"))
                .and_then(|o| obj_val(o, "playNavigationEndpoint"))
                .and_then(|o| obj_val(o, "watchEndpoint"))
                .and_then(|o| str_val(o, "videoId"))
        })?;
    let title = runs_text(renderer.get("title"));
    if title.trim().is_empty() {
        return None;
    }
    let second_title = runs_text(renderer.get("secondTitle"));
    let artist = if !second_title.trim().is_empty() {
        second_title
    } else if let Some(s) = show {
        s.to_owned()
    } else {
        "Unknown artist".to_owned()
    };
    let duration_raw = runs_text(
        renderer
            .get("playbackProgress")
            .and_then(|p| p.get("musicPlaybackProgressRenderer"))
            .and_then(|p| p.get("durationText")),
    );
    let thumbnail_url = obj_val(renderer, "thumbnail")
        .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
        .and_then(|t| obj_val(t, "thumbnail"))
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array)
        .and_then(|a| best_thumbnail(a));
    Some(Song {
        video_id: video_id.to_owned(),
        title,
        artist,
        thumbnail_url,
        duration_text: episode_runtime(&duration_raw),
        is_video: false,
        is_explicit: has_explicit_badge_map(renderer),
        ..Song::default()
    })
}

fn episode_runtime(text: &str) -> Option<String> {
    let mut seconds: u64 = 0;
    for caps in RUNTIME_PART.captures_iter(text) {
        let count: u64 = caps.get(1)?.as_str().parse().ok()?;
        let unit = caps.get(2)?.as_str().to_ascii_lowercase();
        if unit.starts_with('h') {
            seconds += count * 3_600;
        } else if unit.starts_with('m') {
            seconds += count * 60;
        } else {
            seconds += count;
        }
    }
    if seconds == 0 {
        return None;
    }
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        Some(format!("{hours}:{minutes:02}:{secs:02}"))
    } else {
        Some(format!("{minutes}:{secs:02}"))
    }
}

fn parse_browse_item(renderer: &Map<String, Value>) -> Option<BrowseItem> {
    let endpoint =
        obj_val(renderer, "navigationEndpoint").and_then(|n| obj_val(n, "browseEndpoint"))?;
    let browse_id = str_val(endpoint, "browseId")?;
    let page_type = obj_val(endpoint, "browseEndpointContextSupportedConfigs")
        .and_then(|c| obj_val(c, "browseEndpointContextMusicConfig"))
        .and_then(|c| str_val(c, "pageType"))
        .unwrap_or_default();
    let columns = renderer
        .get("flexColumns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let title = runs_text(
        columns
            .first()
            .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
            .and_then(|c| c.get("text")),
    );
    if title.trim().is_empty() {
        return None;
    }
    let subtitle = runs_text(
        columns
            .get(1)
            .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
            .and_then(|c| c.get("text")),
    );
    if VIDEO_WORD.is_match(&title) || VIDEO_WORD.is_match(&subtitle) {
        return None;
    }
    let thumbnail_url = obj_val(renderer, "thumbnail")
        .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
        .and_then(|t| obj_val(t, "thumbnail"))
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array)
        .and_then(|a| best_thumbnail(a));
    Some(BrowseItem {
        browse_id: browse_id.to_owned(),
        title,
        subtitle,
        thumbnail_url,
        r#type: browse_type_of(page_type),
    })
}

fn parse_card_shelf_browse(renderer: &Map<String, Value>) -> Option<BrowseItem> {
    let endpoint = obj_val(renderer, "onTap").and_then(|t| obj_val(t, "browseEndpoint"))?;
    let browse_id = str_val(endpoint, "browseId")?;
    let page_type = obj_val(endpoint, "browseEndpointContextSupportedConfigs")
        .and_then(|c| obj_val(c, "browseEndpointContextMusicConfig"))
        .and_then(|c| str_val(c, "pageType"))
        .unwrap_or_default();
    if page_type.contains("NON_MUSIC_AUDIO_TRACK") {
        return None;
    }
    let title = runs_text(renderer.get("title"));
    if title.trim().is_empty() {
        return None;
    }
    let subtitle = runs_text(renderer.get("subtitle"));
    if VIDEO_WORD.is_match(&title) || VIDEO_WORD.is_match(&subtitle) {
        return None;
    }
    let thumbnail_url = obj_val(renderer, "thumbnail")
        .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
        .and_then(|t| obj_val(t, "thumbnail"))
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array)
        .and_then(|a| best_thumbnail(a));
    Some(BrowseItem {
        browse_id: browse_id.to_owned(),
        title,
        subtitle,
        thumbnail_url,
        r#type: browse_type_of(page_type),
    })
}

fn card_shelf_credit(card: &Map<String, Value>) -> Option<Credits> {
    let endpoint = obj_val(card, "onTap").and_then(|t| obj_val(t, "browseEndpoint"))?;
    let page_type = obj_val(endpoint, "browseEndpointContextSupportedConfigs")
        .and_then(|c| obj_val(c, "browseEndpointContextMusicConfig"))
        .and_then(|c| str_val(c, "pageType"))
        .unwrap_or_default();
    if !page_type.contains("ARTIST") {
        return None;
    }
    let name = runs_text(card.get("title"));
    if name.trim().is_empty() {
        return None;
    }
    Some(Credits {
        artist_id: str_val(endpoint, "browseId").map(ToOwned::to_owned),
        artist_name: Some(name),
        ..Default::default()
    })
}

fn parse_artist_row(renderer: Option<&Map<String, Value>>) -> Option<ShelfItem> {
    let renderer = renderer?;
    let endpoint =
        obj_val(renderer, "navigationEndpoint").and_then(|n| obj_val(n, "browseEndpoint"))?;
    let page_type = obj_val(endpoint, "browseEndpointContextSupportedConfigs")
        .and_then(|c| obj_val(c, "browseEndpointContextMusicConfig"))
        .and_then(|c| str_val(c, "pageType"))
        .unwrap_or_default();
    if !page_type.contains("ARTIST") {
        return None;
    }
    let browse_id = str_val(endpoint, "browseId")?;
    let columns = renderer
        .get("flexColumns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let title = runs_text(
        columns
            .first()
            .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
            .and_then(|c| c.get("text")),
    );
    if title.trim().is_empty() {
        return None;
    }
    let subtitle = runs_text(
        columns
            .get(1)
            .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
            .and_then(|c| c.get("text")),
    );
    let thumbnail_url = obj_val(renderer, "thumbnail")
        .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
        .and_then(|t| obj_val(t, "thumbnail"))
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array)
        .and_then(|a| best_thumbnail(a));
    Some(ShelfItem {
        title,
        subtitle,
        thumbnail_url,
        video_id: None,
        browse_id: Some(browse_id.to_owned()),
        is_video: false,
    })
}

fn parse_two_row_item(renderer: Option<&Map<String, Value>>) -> Option<ShelfItem> {
    let renderer = renderer?;
    let title = runs_text(renderer.get("title"));
    if title.trim().is_empty() {
        return None;
    }
    let endpoint = obj_val(renderer, "navigationEndpoint");
    let browse_id = endpoint
        .and_then(|e| obj_val(e, "browseEndpoint"))
        .and_then(|b| str_val(b, "browseId"));
    let video_id = endpoint
        .and_then(|e| obj_val(e, "watchEndpoint"))
        .and_then(|w| str_val(w, "videoId"))
        .or_else(|| browse_id.and_then(|b| b.strip_prefix("MPED")))
        .map(ToOwned::to_owned);
    let resolved_browse_id = browse_id
        .filter(|b| !b.starts_with("MPED"))
        .map(ToOwned::to_owned);
    let thumbnails = obj_val(renderer, "thumbnailRenderer")
        .and_then(|t| obj_val(t, "musicThumbnailRenderer"))
        .and_then(|t| obj_val(t, "thumbnail"))
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array);
    let subtitle = runs_text(renderer.get("subtitle"));
    let is_video = resolved_browse_id.is_none()
        && video_id.is_some()
        && (thumbnails.is_some_and(|a| is_not_square(a))
            || str_val(renderer, "aspectRatio").is_some_and(|a| a.contains("16_9")));

    if let Some(ref bid) = resolved_browse_id
        && !bid.starts_with("VL")
        && !bid.starts_with("PL")
        && !bid.starts_with("MPRE")
        && !bid.starts_with("UC")
        && (VIDEO_WORD.is_match(&title) || VIDEO_WORD.is_match(&subtitle))
    {
        return None;
    }
    Some(ShelfItem {
        title,
        subtitle,
        thumbnail_url: thumbnails.and_then(|a| best_thumbnail(a)),
        video_id,
        browse_id: resolved_browse_id,
        is_video,
    })
}

fn browse_type_of(page_type: &str) -> BrowseType {
    if page_type.contains("ALBUM") {
        BrowseType::Album
    } else if page_type.contains("ARTIST") {
        BrowseType::Artist
    } else if page_type.contains("PLAYLIST") {
        BrowseType::Playlist
    } else {
        BrowseType::Other
    }
}

fn extract_carousel_more_endpoint(header: &Map<String, Value>) -> Option<&Map<String, Value>> {
    header
        .get("title")
        .and_then(|t| t.get("runs"))
        .and_then(Value::as_array)
        .and_then(|runs| {
            runs.iter().find_map(|r| {
                r.get("navigationEndpoint")?
                    .get("browseEndpoint")?
                    .as_object()
            })
        })
        .or_else(|| {
            header
                .get("moreContentButton")?
                .get("buttonRenderer")?
                .get("navigationEndpoint")?
                .get("browseEndpoint")?
                .as_object()
        })
        .or_else(|| {
            header
                .get("moreContentButton")?
                .get("musicMoreContentButtonRenderer")?
                .get("navigationEndpoint")?
                .get("browseEndpoint")?
                .as_object()
        })
        .or_else(|| {
            header.get("endIcons")?.as_array()?.iter().find_map(|icon| {
                let btn = icon.get("musicNavigationButtonRenderer")?;
                btn.get("clickCommand")
                    .and_then(|c| c.get("browseEndpoint"))
                    .or_else(|| {
                        btn.get("navigationEndpoint")
                            .and_then(|n| n.get("browseEndpoint"))
                    })
                    .and_then(Value::as_object)
            })
        })
        .or_else(|| {
            header
                .get("navigationEndpoint")?
                .get("browseEndpoint")?
                .as_object()
        })
}

fn header_watch_target(header: Option<&Value>, button: &str) -> Option<WatchTarget> {
    let endpoint = header?
        .get("musicImmersiveHeaderRenderer")?
        .get(button)?
        .get("buttonRenderer")?
        .get("navigationEndpoint")?
        .get("watchEndpoint")?
        .as_object()?;
    let playlist_id = str_val(endpoint, "playlistId")?.to_owned();
    let params = str_val(endpoint, "params").map(|p| {
        percent_decode_str(p)
            .decode_utf8()
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| p.to_owned())
    });
    Some(WatchTarget {
        playlist_id,
        video_id: str_val(endpoint, "videoId").map(ToOwned::to_owned),
        params,
    })
}

fn subscription(header: Option<&Value>) -> Option<SubscriptionState> {
    let immersive = header?.get("musicImmersiveHeaderRenderer")?.as_object()?;
    let candidates = [
        immersive
            .get("subscriptionButton2")
            .and_then(|b| b.get("subscribeButtonRenderer"))
            .and_then(Value::as_object),
        immersive
            .get("subscriptionButton")
            .and_then(|b| b.get("subscribeButtonRenderer"))
            .and_then(Value::as_object),
    ];
    for button in candidates.into_iter().flatten() {
        let Some(subscribed) = button.get("subscribed").and_then(Value::as_bool) else {
            continue;
        };
        let channel_id = str_val(button, "channelId")
            .map(ToOwned::to_owned)
            .or_else(|| {
                button
                    .get("serviceEndpoints")?
                    .as_array()?
                    .iter()
                    .find_map(|ep| {
                        ep.get("subscribeEndpoint")?
                            .get("channelIds")?
                            .as_array()?
                            .first()?
                            .as_str()
                            .map(ToOwned::to_owned)
                    })
            });
        if let Some(channel_id) = channel_id {
            return Some(SubscriptionState {
                channel_id,
                subscribed,
            });
        }
    }
    None
}

fn subscriber_count(header: Option<&Value>) -> Option<String> {
    let immersive = header?.get("musicImmersiveHeaderRenderer")?.as_object()?;
    let button2 = immersive
        .get("subscriptionButton2")
        .and_then(|b| b.get("subscribeButtonRenderer"));
    let button1 = immersive
        .get("subscriptionButton")
        .and_then(|b| b.get("subscribeButtonRenderer"));
    first_run_text(button2.and_then(|b| b.get("subscriberCountWithSubscribeText")))
        .or_else(|| first_run_text(button1.and_then(|b| b.get("longSubscriberCountText"))))
        .or_else(|| first_run_text(button1.and_then(|b| b.get("shortSubscriberCountText"))))
}

fn monthly_listeners(header: Option<&Value>) -> Option<String> {
    first_run_text(
        header?
            .get("musicImmersiveHeaderRenderer")?
            .get("monthlyListenerCount"),
    )
}

fn artist_name(header: Option<&Value>) -> Option<String> {
    let h = header?;
    let renderer = h
        .get("musicImmersiveHeaderRenderer")
        .or_else(|| h.get("musicVisualHeaderRenderer"))?;
    let title = runs_text(renderer.get("title"));
    if title.trim().is_empty() {
        None
    } else {
        Some(title)
    }
}

fn artist_thumbnail(header: Option<&Value>) -> Option<String> {
    let h = header?;
    let immersive = h.get("musicImmersiveHeaderRenderer");
    let visual = h.get("musicVisualHeaderRenderer");
    let renderer = immersive
        .and_then(|i| i.get("thumbnail"))
        .or_else(|| visual.and_then(|v| v.get("foregroundThumbnail")))
        .or_else(|| visual.and_then(|v| v.get("thumbnail")))
        .and_then(|t| t.get("musicThumbnailRenderer"))
        .and_then(Value::as_object)
        .or_else(|| {
            collect_renderers(h, "musicThumbnailRenderer")
                .into_iter()
                .next()
        })?;
    renderer
        .get("thumbnail")
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array)
        .and_then(|a| best_thumbnail(a))
}

fn artist_names_from_runs(runs: &[Value]) -> Option<String> {
    let mut groups: Vec<Vec<&Value>> = vec![Vec::new()];
    for run in runs {
        if run
            .as_object()
            .and_then(|o| str_val(o, "text"))
            .is_some_and(|t| t.trim() == "•")
        {
            groups.push(Vec::new());
        } else if let Some(last) = groups.last_mut() {
            last.push(run);
        }
    }
    let artist_group = groups.into_iter().find(|group| {
        group.iter().any(|run| {
            run.get("navigationEndpoint")
                .and_then(|n| n.get("browseEndpoint"))
                .and_then(|b| b.get("browseEndpointContextSupportedConfigs"))
                .and_then(|c| c.get("browseEndpointContextMusicConfig"))
                .and_then(Value::as_object)
                .and_then(|c| str_val(c, "pageType"))
                .is_some_and(|pt| pt.contains("ARTIST"))
        })
    })?;
    let mut seen = HashSet::new();
    let names: Vec<String> = artist_group
        .into_iter()
        .enumerate()
        .filter(|(idx, _)| idx % 2 == 0)
        .filter_map(|(_, run)| {
            let txt = run.as_object().and_then(|o| str_val(o, "text"))?.trim();
            if txt.is_empty() || !seen.insert(txt.to_owned()) {
                None
            } else {
                Some(txt.to_owned())
            }
        })
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn show_name_from_runs(runs: &[Value]) -> Option<String> {
    runs.iter().find_map(|run| {
        let pt = run
            .get("navigationEndpoint")?
            .get("browseEndpoint")?
            .get("browseEndpointContextSupportedConfigs")?
            .get("browseEndpointContextMusicConfig")?
            .as_object()
            .and_then(|c| str_val(c, "pageType"))?;
        if !pt.contains("PODCAST_SHOW") {
            return None;
        }
        let txt = run.as_object().and_then(|o| str_val(o, "text"))?.trim();
        (!txt.is_empty()).then(|| txt.to_owned())
    })
}

fn playlist_id_from_play_action(btn: &Map<String, Value>) -> Option<String> {
    let ep = obj_val(btn, "playNavigationEndpoint")
        .or_else(|| obj_val(btn, "navigationEndpoint"))
        .unwrap_or(btn);
    obj_val(ep, "watchPlaylistEndpoint")
        .and_then(|w| str_val(w, "playlistId"))
        .or_else(|| obj_val(ep, "watchEndpoint").and_then(|w| str_val(w, "playlistId")))
        .map(ToOwned::to_owned)
}

fn wrapper_renderers(wrapper: &Map<String, Value>) -> Vec<&Map<String, Value>> {
    let mut out = Vec::new();
    if let Some(p) =
        obj_val(wrapper, "primaryRenderer").and_then(|o| obj_val(o, "playlistPanelVideoRenderer"))
    {
        out.push(p);
    }
    if let Some(cps) = wrapper.get("counterpart").and_then(Value::as_array) {
        for cp in cps {
            if let Some(r) = cp.as_object().and_then(counterpart_renderer) {
                out.push(r);
            }
        }
    }
    out
}

fn counterpart_renderer(cp: &Map<String, Value>) -> Option<&Map<String, Value>> {
    obj_val(cp, "counterpartRenderer").and_then(|o| obj_val(o, "playlistPanelVideoRenderer"))
}

fn is_audio_track(renderer: &Map<String, Value>) -> bool {
    music_video_type(renderer) == Some(MUSIC_VIDEO_TYPE_ATV)
}

fn music_video_type(renderer: &Map<String, Value>) -> Option<&str> {
    obj_val(renderer, "navigationEndpoint")
        .and_then(|n| obj_val(n, "watchEndpoint"))
        .and_then(|w| obj_val(w, "watchEndpointMusicSupportedConfigs"))
        .and_then(|c| obj_val(c, "watchEndpointMusicConfig"))
        .and_then(|c| str_val(c, "musicVideoType"))
}

fn time_map_of(counterpart: &Map<String, Value>) -> Option<VersionTimeMap> {
    let segments: Vec<Segment> = obj_val(counterpart, "segmentMap")?
        .get("segment")?
        .as_array()?
        .iter()
        .filter_map(|seg| {
            let obj = seg.as_object()?;
            let from = value_as_i64(obj.get("primaryVideoStartTimeMilliseconds")?)?;
            let to = value_as_i64(obj.get("counterpartVideoStartTimeMilliseconds")?)?;
            let dur = value_as_i64(obj.get("durationMilliseconds")?)?;
            if dur <= 0 {
                return None;
            }
            Some(Segment {
                from_start_ms: from,
                to_start_ms: to,
                duration_ms: dur,
            })
        })
        .collect();
    VersionTimeMap::new(segments).ok()
}

fn extract_feedback_token(renderer: &Map<String, Value>) -> Option<String> {
    obj_val(renderer, "menu")?
        .get("menuRenderer")?
        .get("items")?
        .as_array()?
        .iter()
        .find_map(|item| {
            let service = item.get("menuServiceItemRenderer")?.as_object()?;
            if obj_val(service, "icon").and_then(|i| str_val(i, "iconType")) != Some("DELETE") {
                return None;
            }
            obj_val(service, "serviceEndpoint")?
                .get("feedbackEndpoint")?
                .as_object()
                .and_then(|f| str_val(f, "feedbackToken"))
                .map(ToOwned::to_owned)
        })
}

fn feedback_token_of(toggle: &Map<String, Value>, endpoint: &str) -> Option<String> {
    obj_val(toggle, endpoint)?
        .get("feedbackEndpoint")?
        .as_object()
        .and_then(|f| str_val(f, "feedbackToken"))
        .map(ToOwned::to_owned)
}

fn is_library_toggle(toggle: &Map<String, Value>) -> bool {
    let default_icon = obj_val(toggle, "defaultIcon").and_then(|i| str_val(i, "iconType"));
    let toggled_icon = obj_val(toggle, "toggledIcon").and_then(|i| str_val(i, "iconType"));
    LIBRARY_ICONS
        .iter()
        .any(|&icon| default_icon == Some(icon) || toggled_icon == Some(icon))
}

fn is_save_toggle(toggle: &Map<String, Value>) -> bool {
    obj_val(toggle, "defaultIcon").and_then(|i| str_val(i, "iconType")) == Some("BOOKMARK_BORDER")
        || obj_val(toggle, "toggledIcon").and_then(|i| str_val(i, "iconType")) == Some("BOOKMARK")
}

fn continuation_token_map(root: &Map<String, Value>) -> Option<String> {
    if let Some(endpoint) = collect_renderers_map(root, "continuationItemRenderer")
        .into_iter()
        .next()
        .and_then(|r| obj_val(r, "continuationEndpoint"))
    {
        let cmd = obj_val(endpoint, "continuationCommand").or_else(|| {
            obj_val(endpoint, "commandExecutorCommand")
                .and_then(|c| c.get("commands"))
                .and_then(Value::as_array)
                .and_then(|cmds| {
                    cmds.iter()
                        .find_map(|c| c.get("continuationCommand").and_then(Value::as_object))
                })
        });
        if let Some(tok) = cmd.and_then(|c| str_val(c, "token")) {
            return Some(tok.to_owned());
        }
    }
    collect_renderers_map(root, "nextContinuationData")
        .into_iter()
        .next()
        .and_then(|n| str_val(n, "continuation"))
        .map(ToOwned::to_owned)
}

// ---- Tiny null-safe JSON traversal helpers ----------------------------------

fn obj_val<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    map.get(key)?.as_object()
}

fn str_val<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key)?.as_str()
}

fn value_as_i64(val: &Value) -> Option<i64> {
    match val {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn value_as_u64(val: &Value) -> Option<u64> {
    match val {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn value_as_f64(val: &Value) -> Option<f64> {
    match val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn value_is_true(val: Option<&Value>) -> bool {
    match val {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

fn runs_text(val: Option<&Value>) -> String {
    val.and_then(|v| v.get("runs"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.as_object().and_then(|o| str_val(o, "text")))
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn text_or_simple(val: Option<&Value>) -> String {
    let r = runs_text(val);
    if !r.trim().is_empty() {
        return r;
    }
    val.and_then(Value::as_object)
        .and_then(|o| str_val(o, "simpleText"))
        .unwrap_or_default()
        .to_owned()
}

fn first_run_text(val: Option<&Value>) -> Option<String> {
    val?.get("runs")?
        .as_array()?
        .first()?
        .as_object()
        .and_then(|o| str_val(o, "text"))
        .map(ToOwned::to_owned)
}

fn best_thumbnail(thumbnails: &[Value]) -> Option<String> {
    thumbnails
        .last()?
        .as_object()
        .and_then(|o| str_val(o, "url"))
        .map(ToOwned::to_owned)
}

fn is_not_square(thumbnails: &[Value]) -> bool {
    let Some(last) = thumbnails.last().and_then(Value::as_object) else {
        return false;
    };
    let (Some(w), Some(h)) = (
        last.get("width").and_then(value_as_f64),
        last.get("height").and_then(value_as_f64),
    ) else {
        return false;
    };
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    let ratio = w / h;
    !(0.85..=1.15).contains(&ratio)
}

fn collect_renderers<'a>(root: &'a Value, name: &str) -> Vec<&'a Map<String, Value>> {
    let mut out = Vec::new();
    walk_renderers(root, name, &mut out);
    out
}

fn collect_renderers_map<'a>(
    map: &'a Map<String, Value>,
    name: &str,
) -> Vec<&'a Map<String, Value>> {
    let mut out = Vec::new();
    if let Some(obj) = map.get(name).and_then(Value::as_object) {
        out.push(obj);
    }
    for v in map.values() {
        walk_renderers(v, name, &mut out);
    }
    out
}

fn walk_renderers<'a>(node: &'a Value, name: &str, out: &mut Vec<&'a Map<String, Value>>) {
    match node {
        Value::Object(map) => {
            if let Some(obj) = map.get(name).and_then(Value::as_object) {
                out.push(obj);
            }
            for v in map.values() {
                walk_renderers(v, name, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_renderers(v, name, out);
            }
        }
        _ => {}
    }
}

fn find_string_map(map: &Map<String, Value>, key: &str) -> Option<String> {
    if let Some(s) = map.get(key).and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    map.values().find_map(|v| find_string_val(v, key))
}

fn find_string_val(node: &Value, key: &str) -> Option<String> {
    match node {
        Value::Object(map) => find_string_map(map, key),
        Value::Array(arr) => arr.iter().find_map(|v| find_string_val(v, key)),
        _ => None,
    }
}

fn has_key_map(map: &Map<String, Value>, key: &str) -> bool {
    map.contains_key(key) || map.values().any(|v| has_key_val(v, key))
}

fn has_key_val(node: &Value, key: &str) -> bool {
    match node {
        Value::Object(map) => has_key_map(map, key),
        Value::Array(arr) => arr.iter().any(|v| has_key_val(v, key)),
        _ => false,
    }
}

fn is_podcast_episode(renderer: &Map<String, Value>) -> bool {
    find_string_map(renderer, "musicVideoType").as_deref()
        == Some("MUSIC_VIDEO_TYPE_PODCAST_EPISODE")
}

fn is_private_upload(renderer: &Map<String, Value>) -> bool {
    has_key_map(renderer, "musicDeletePrivatelyOwnedEntityCommand")
        || find_string_map(renderer, "musicVideoType")
            .is_some_and(|t| t.to_ascii_uppercase().contains("PRIVATELY_OWNED"))
}

fn has_explicit_badge(node: &Value) -> Option<bool> {
    let found = match node {
        Value::String(s) => s == "MUSIC_EXPLICIT_BADGE",
        Value::Array(arr) => arr.iter().any(|v| has_explicit_badge(v) == Some(true)),
        Value::Object(map) => has_explicit_badge_map(map) == Some(true),
        _ => false,
    };
    found.then_some(true)
}

fn has_explicit_badge_map(map: &Map<String, Value>) -> Option<bool> {
    map.values()
        .any(|v| has_explicit_badge(v) == Some(true))
        .then_some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> InnertubeParser {
        InnertubeParser::default()
    }

    // ---- PodcastTest --------------------------------------------------------

    fn episode_json(video_id: &str, title: &str, runtime: &str, extra: &str) -> String {
        format!(
            r#"{{
              "musicMultiRowListItemRenderer": {{
                "thumbnail": {{ "musicThumbnailRenderer": {{ "thumbnail": {{ "thumbnails": [
                  {{ "url": "https://i.ytimg.com/vi/{video_id}/hq720.jpg", "width": 853, "height": 479 }}
                ] }} }} }},
                "onTap": {{ "watchEndpoint": {{
                  "videoId": "{video_id}",
                  "watchEndpointMusicSupportedConfigs": {{
                    "watchEndpointMusicConfig": {{ "musicVideoType": "MUSIC_VIDEO_TYPE_PODCAST_EPISODE" }}
                  }}
                }} }},
                "subtitle": {{ "runs": [ {{ "text": "128K views" }}, {{ "text": " • " }}, {{ "text": "2d ago" }} ] }},
                "playbackProgress": {{ "musicPlaybackProgressRenderer": {{
                  "durationText": {{ "runs": [ {{ "text": " • " }}, {{ "text": "{runtime}" }} ] }}
                }} }},
                "title": {{ "runs": [ {{ "text": "{title}" }} ] }}{extra}
              }}
            }}"#
        )
    }

    #[test]
    fn podcast_show_page_lists_episodes_as_playable_tracks() {
        let p = parser();
        let show_page: Value = serde_json::from_str(&format!(
            r#"{{
              "contents": {{ "twoColumnBrowseResultsRenderer": {{
                "tabs": [ {{ "tabRenderer": {{ "content": {{ "sectionListRenderer": {{ "contents": [
                  {{ "musicResponsiveHeaderRenderer": {{
                    "title": {{ "runs": [ {{ "text": "Huberman Lab" }} ] }},
                    "subtitle": {{}},
                    "straplineTextOne": {{ "runs": [ {{ "text": "Andrew Huberman" }} ] }}
                  }} }}
                ] }} }} }} }} ],
                "secondaryContents": {{ "sectionListRenderer": {{ "contents": [
                  {{ "musicShelfRenderer": {{
                    "contents": [
                      {},
                      {}
                    ],
                    "continuations": [ {{ "nextContinuationData": {{ "continuation": "PAGE2" }} }} ]
                  }} }}
                ] }} }}
              }} }}
            }}"#,
            episode_json("NlVu0P1RdHs", "Ross Edgley", "2 hr 43 min", ""),
            episode_json("eYSOabR3YP0", "Essentials: Supplementation", "34 min", ""),
        ))
        .expect("valid json");

        let show = p.podcast_show(&show_page);
        assert_eq!(show.as_deref(), Some("Huberman Lab"));
        let songs = p.collect_songs_deep(&show_page, show.as_deref());
        let ids: Vec<&str> = songs.iter().map(|s| s.video_id.as_str()).collect();
        let artists: Vec<&str> = songs.iter().map(|s| s.artist.as_str()).collect();
        let durs: Vec<Option<&str>> = songs.iter().map(|s| s.duration_text.as_deref()).collect();
        assert_eq!(ids, vec!["NlVu0P1RdHs", "eYSOabR3YP0"]);
        assert_eq!(artists, vec!["Huberman Lab", "Huberman Lab"]);
        assert_eq!(durs, vec![Some("2:43:00"), Some("34:00")]);
        assert!(songs.iter().all(|s| !s.is_video));
        assert_eq!(p.continuation_token(&show_page).as_deref(), Some("PAGE2"));
    }

    #[test]
    fn podcast_continuation_carries_show_and_second_title_overrides() {
        let p = parser();
        let page: Value = serde_json::from_str(&format!(
            r#"{{ "continuationContents": {{ "musicShelfContinuation": {{ "contents": [ {} ] }} }} }}"#,
            episode_json("P-h5WSQG1Sw", "Emotions", "2 hr 9 min", ""),
        ))
        .expect("json");
        assert!(p.podcast_show(&page).is_none());
        assert!(p.parse_playlist_shelf(&page).is_none());
        let songs = p.collect_songs_deep(&page, Some("Huberman Lab"));
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].artist, "Huberman Lab");
        assert_eq!(songs[0].duration_text.as_deref(), Some("2:09:00"));

        let shelf: Value = serde_json::from_str(&format!(
            r#"{{ "contents": [ {} ] }}"#,
            episode_json(
                "TPXdcOJRmRc",
                "World News Tonight",
                "20 min",
                r#", "secondTitle": { "runs": [ { "text": "ABC World News Tonight" } ] }"#
            ),
        ))
        .expect("json");
        let away = p.collect_songs_deep(&shelf, Some("Ignored"));
        assert_eq!(away[0].artist, "ABC World News Tonight");
    }

    #[test]
    fn search_episode_is_credited_to_its_show_and_kept_on_all_tab() {
        let p = parser();
        let resp: Value = serde_json::from_str(
            r#"{ "contents": [ { "musicResponsiveListItemRenderer": {
              "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                { "url": "https://i.ytimg.com/vi/Y566_T-YlNQ/hqdefault.jpg", "width": 400, "height": 225 }
              ] } } },
              "overlay": { "musicItemThumbnailOverlayRenderer": { "content": { "musicPlayButtonRenderer": {
                "playNavigationEndpoint": { "watchEndpoint": {
                  "videoId": "Y566_T-YlNQ",
                  "watchEndpointMusicSupportedConfigs": {
                    "watchEndpointMusicConfig": { "musicVideoType": "MUSIC_VIDEO_TYPE_PODCAST_EPISODE" }
                  }
                } }
              } } } },
              "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [ { "text": "Become Mentally Dangerous" } ] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                  { "text": "Episode" }, { "text": " • " }, { "text": "Sep 1" }, { "text": " • " },
                  { "text": "Figuring Out With Raj Shamani", "navigationEndpoint": { "browseEndpoint": {
                    "browseId": "MPSPPLE0Jo6NF_JYO5-phess8GKafKMtPv3tfZ",
                    "browseEndpointContextSupportedConfigs": { "browseEndpointContextMusicConfig": {
                      "pageType": "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE"
                    } }
                  } } }
                ] } } }
              ]
            } } ] }"#,
        )
        .expect("json");

        let songs = p.parse_search_songs(&resp);
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].video_id, "Y566_T-YlNQ");
        assert_eq!(songs[0].artist, "Figuring Out With Raj Shamani");
        assert!(!songs[0].is_video);
    }

    // ---- UploadedTracksTest -------------------------------------------------

    fn upload_row(video_id: &str, extra: &str, mv_type: Option<&str>) -> String {
        let cfg = mv_type
            .map(|t| {
                format!(
                    r#", "watchEndpointMusicSupportedConfigs": {{ "watchEndpointMusicConfig": {{ "musicVideoType": "{t}" }} }}"#
                )
            })
            .unwrap_or_default();
        format!(
            r#"{{
              "musicResponsiveListItemRenderer": {{
                "playlistItemData": {{ "videoId": "{video_id}" }},
                "overlay": {{ "musicItemThumbnailOverlayRenderer": {{ "content": {{ "musicPlayButtonRenderer": {{
                  "playNavigationEndpoint": {{ "watchEndpoint": {{ "videoId": "{video_id}"{cfg} }} }}
                }} }} }} }},
                "flexColumns": [
                  {{ "musicResponsiveListItemFlexColumnRenderer": {{ "text": {{ "runs": [ {{ "text": "A title" }} ] }} }} }},
                  {{ "musicResponsiveListItemFlexColumnRenderer": {{ "text": {{ "runs": [ {{ "text": "An artist" }} ] }} }} }}
                ]{extra}
              }}
            }}"#
        )
    }

    #[test]
    fn marks_private_uploads_from_delete_menu_and_video_type() {
        let p = parser();
        let menu = r#", "menu": { "menuRenderer": { "items": [ { "menuNavigationItemRenderer": {
            "navigationEndpoint": { "confirmDialogEndpoint": { "content": { "confirmDialogRenderer": {
              "confirmButton": { "buttonRenderer": { "command": {
                "musicDeletePrivatelyOwnedEntityCommand": { "entityId": "e1" } } } } } } } } } } ] } }"#;
        let payload: Value = serde_json::from_str(&format!(
            r#"{{ "contents": [ {}, {}, {}, {} ] }}"#,
            upload_row("up1", menu, None),
            upload_row("pub1", "", None),
            upload_row("up2", "", Some("MUSIC_VIDEO_TYPE_PRIVATELY_OWNED_TRACK")),
            upload_row("pub2", "", Some("MUSIC_VIDEO_TYPE_ATV")),
        ))
        .expect("json");

        let songs = p.collect_songs_deep(&payload, None);
        assert_eq!(songs.len(), 4);
        assert!(p.uploaded_tracks().contains("up1"));
        assert!(!p.uploaded_tracks().contains("pub1"));
        assert!(p.uploaded_tracks().contains("up2"));
        assert!(!p.uploaded_tracks().contains("pub2"));
    }

    // ---- VersionPairTest ----------------------------------------------------

    fn panel_json(video_id: &str, title: &str, mv_type: &str, byline: &str) -> String {
        format!(
            r#"{{
              "playlistPanelVideoRenderer": {{
                "videoId": "{video_id}",
                "title": {{ "runs": [ {{ "text": "{title}" }} ] }},
                "longBylineText": {{ "runs": [ {{ "text": "{byline}" }} ] }},
                "navigationEndpoint": {{ "watchEndpoint": {{
                  "videoId": "{video_id}",
                  "watchEndpointMusicSupportedConfigs": {{ "watchEndpointMusicConfig": {{ "musicVideoType": "{mv_type}" }} }}
                }} }}
              }}
            }}"#
        )
    }

    #[test]
    fn parses_version_pairs_in_both_directions_and_without_segments() {
        let p = parser();
        let video = panel_json(
            "omv1",
            "Song (Official Video)",
            "MUSIC_VIDEO_TYPE_OMV",
            "Artist • 10M views",
        );
        let audio = panel_json(
            "atv1",
            "Song",
            "MUSIC_VIDEO_TYPE_ATV",
            "Artist • Album • 2024",
        );
        let segments = r#""segmentMap": { "segment": [
            { "primaryVideoStartTimeMilliseconds": "12000",
              "counterpartVideoStartTimeMilliseconds": "0",
              "durationMilliseconds": "180000" }
        ] }"#;

        let wrapped: Value = serde_json::from_str(&format!(
            r#"{{ "contents": [ {{ "playlistPanelVideoWrapperRenderer": {{
                "primaryRenderer": {video},
                "counterpart": [ {{ "counterpartRenderer": {audio}, {segments} }} ]
            }} }} ] }}"#
        ))
        .expect("json");

        let pair = p
            .parse_version_pair(&wrapped, "omv1", true)
            .expect("audio pair");
        assert_eq!(pair.song.video_id, "atv1");
        assert!(!pair.song.is_video);
        assert_eq!(pair.time_map.as_ref().expect("map").map(42_000), 30_000);

        let rev_pair = p
            .parse_version_pair(&wrapped, "atv1", false)
            .expect("video pair");
        assert_eq!(rev_pair.song.video_id, "omv1");
        assert!(rev_pair.song.is_video);
        assert_eq!(rev_pair.time_map.as_ref().expect("map").map(30_000), 42_000);

        let song_first: Value = serde_json::from_str(&format!(
            r#"{{ "contents": [ {{ "playlistPanelVideoWrapperRenderer": {{
                "primaryRenderer": {audio},
                "counterpart": [ {{ "counterpartRenderer": {video}, "segmentMap": {{ "segment": [
                    {{ "primaryVideoStartTimeMilliseconds": 0,
                       "counterpartVideoStartTimeMilliseconds": 12000,
                       "durationMilliseconds": 180000 }} ] }} }} ]
            }} }} ] }}"#
        ))
        .expect("json");
        let back = p
            .parse_version_pair(&song_first, "omv1", true)
            .expect("backwards pair");
        assert_eq!(back.song.video_id, "atv1");
        assert_eq!(back.time_map.as_ref().expect("map").map(42_000), 30_000);

        assert!(p.parse_version_pair(&song_first, "atv1", true).is_none());

        let no_seg: Value = serde_json::from_str(&format!(
            r#"{{ "contents": [ {{ "playlistPanelVideoWrapperRenderer": {{
                "primaryRenderer": {video},
                "counterpart": [ {{ "counterpartRenderer": {audio} }} ]
            }} }} ] }}"#
        ))
        .expect("json");
        let paired_no_seg = p
            .parse_version_pair(&no_seg, "omv1", true)
            .expect("pair without segments");
        assert_eq!(paired_no_seg.song.video_id, "atv1");
        assert!(paired_no_seg.time_map.is_none());

        let plain: Value =
            serde_json::from_str(&format!(r#"{{ "contents": [ {video} ] }}"#)).expect("json");
        assert!(p.parse_version_pair(&plain, "omv1", true).is_none());
    }
}
