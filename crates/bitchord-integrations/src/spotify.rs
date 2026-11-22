//! Spotify playlist import (public embed + Pathfinder v1 paging) and signed-in
//! Spotify library browsing (Pathfinder v2 GraphQL), plus YouTube Music track
//! matching via [`bitchord_matcher::TrackMatcher`].
//!
//! Ported from upstream `SpotifyImporter.kt`, `SpotifyLibrary.kt`, and
//! `SpotifyMatch.kt`.

use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use bitchord_innertube::YtMusicRepository;
use bitchord_matcher::{Target, TrackMatcher};
use bitchord_model::{SearchFilter, SearchResult, Song};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;
use url::Url;

/// A Spotify playlist's page id prefix (`spotify:playlist:<id>` or `spotify:playlist:liked`).
pub const SPOTIFY_PAGE_PREFIX: &str = "spotify:playlist:";

/// Video ID prefix for a Spotify track still awaiting YouTube Music resolution.
pub const SPOTIFY_PENDING_PREFIX: &str = "sp:";

/// Video ID prefix for a Spotify track with no YouTube Music counterpart.
pub const SPOTIFY_MISSING_PREFIX: &str = "sp-miss:";

/// Pseudo-playlist ID for Spotify's Liked Songs collection.
pub const LIKED_ID: &str = "liked";

/// Official Spotify Liked Songs cover artwork.
pub const LIKED_COVER: &str = "https://misc.scdn.co/liked-songs/liked-songs-640.png";

const EMBED_TRACK_LIMIT: usize = 100;
const PAGE_SIZE: usize = 100;
const PATHFINDER_V1_URL: &str = "https://api-partner.spotify.com/pathfinder/v1/query";
const PATHFINDER_V2_URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const FETCH_PLAYLIST_V1_HASH: &str =
    "19ff1327c29e99c208c86d7a9d8f1929cfdf3d3202a0ff4253c821f1901aa94d";
const LIBRARY_V3_HASH: &str = "973e511ca44261fda7eebac8b653155e7caee3675abb4fb110cc1b8c78b091c3";
const FETCH_PLAYLIST_V2_HASH: &str =
    "346811f856fb0b7e4f6c59f8ebea78dd081c6e2fb01b77c954b26259d5fc6763";
const FETCH_LIKED_V2_HASH: &str =
    "087278b20b743578a6262c2b0b4bcd20d879c503cc359a2285baf083ef944240";

const IMPORTER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const LIBRARY_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36";

const KNOWN_LANGUAGES: &[&str] = &[
    "Tamil",
    "Telugu",
    "Kannada",
    "Malayalam",
    "Hindi",
    "English",
    "Punjabi",
    "Marathi",
    "Bengali",
    "Bhojpuri",
    "Gujarati",
];

// INFALLIBLE: static compile-time regex patterns.
static PLAYLIST_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{22}$").expect("valid playlist id regex"));
static NEXT_DATA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<script id="__NEXT_DATA__" type="application/json">(.*?)</script>"#)
        .expect("valid __NEXT_DATA__ regex")
});

/// Errors returned by Spotify public playlist scraping or Pathfinder GraphQL queries.
#[derive(Debug, Error)]
pub enum SpotifyError {
    #[error("Spotify sign-in expired or missing")]
    Unauthorized,
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Spotify API returned HTTP {status} for {operation}")]
    HttpStatus { operation: String, status: u16 },
    #[error("Spotify response parse error: {0}")]
    Parse(String),
    #[error("Spotify GraphQL error: {0}")]
    GraphQl(String),
}

/// Single track metadata extracted from a public Spotify playlist embed or Pathfinder v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyImportTrack {
    pub title: String,
    pub artist: String,
}

/// Summary of importing and resolving a Spotify playlist to YouTube Music tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyImportResult {
    pub title: String,
    pub description: String,
    pub thumbnail_url: Option<String>,
    pub total_tracks: usize,
    pub resolved_video_ids: Vec<String>,
    pub unmatched_tracks: Vec<SpotifyImportTrack>,
}

/// Playlist entry from a signed-in Spotify library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyPlaylist {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub image_url: Option<String>,
}

/// Track entry from a signed-in Spotify playlist or Liked Songs collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: i32,
    pub image_url: Option<String>,
}

/// Web player bearer token and optional client token used for Pathfinder v2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyTokens {
    pub bearer: String,
    pub client_token: Option<String>,
}

/// Whether `song` is still awaiting YouTube Music resolution for a Spotify row.
pub fn is_match_pending(song: &Song) -> bool {
    song.video_id.starts_with(SPOTIFY_PENDING_PREFIX)
}

/// Whether `song` has no YouTube Music counterpart.
pub fn is_match_missing(song: &Song) -> bool {
    song.video_id.starts_with(SPOTIFY_MISSING_PREFIX)
}

/// Whether `song` is an unresolved Spotify placeholder row.
pub fn is_unresolved_spotify(song: &Song) -> bool {
    is_match_pending(song) || is_match_missing(song)
}

fn is_spotify_host(host: &str) -> bool {
    host == "spotify.com" || host.ends_with(".spotify.com")
}

/// Public Spotify playlist parser and YouTube Music resolver.
pub struct SpotifyImporter;

impl SpotifyImporter {
    /// Parses a Spotify playlist URI (`spotify:playlist:<id>`) or `http(s)://*.spotify.com/.../playlist/<id>` URL.
    pub fn extract_playlist_id(input: &str) -> Option<String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(rest) = trimmed.strip_prefix(SPOTIFY_PAGE_PREFIX) {
            let id = rest.split(['?', '/']).next().unwrap_or_default();
            return PLAYLIST_ID_RE.is_match(id).then(|| id.to_owned());
        }

        let url = Url::parse(trimmed).ok()?;
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        if !is_spotify_host(&host) {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let playlist_idx = segments.iter().position(|s| *s == "playlist")?;
        let candidate = segments.get(playlist_idx + 1)?;
        PLAYLIST_ID_RE
            .is_match(candidate)
            .then(|| (*candidate).to_owned())
    }

    /// Parses Spotify's embed HTML `<script id="__NEXT_DATA__">` payload into
    /// `(playlist_title, tracks, optional_anonymous_access_token)`.
    pub fn parse_embed_html(
        html: &str,
    ) -> Result<(String, Vec<SpotifyImportTrack>, Option<String>), SpotifyError> {
        let caps = NEXT_DATA_RE.captures(html).ok_or_else(|| {
            SpotifyError::Parse(
                "Could not parse Spotify playlist metadata. Make sure the playlist is public."
                    .to_owned(),
            )
        })?;
        let json_str = caps
            .get(1)
            .map(|m| m.as_str())
            .ok_or_else(|| SpotifyError::Parse("Empty __NEXT_DATA__ script".to_owned()))?;
        let root: Value = serde_json::from_str(json_str)
            .map_err(|e| SpotifyError::Parse(format!("invalid __NEXT_DATA__ JSON: {e}")))?;

        let page_props = root.get("props").and_then(|p| p.get("pageProps"));
        let state_data = page_props
            .and_then(|p| p.get("state"))
            .and_then(|s| s.get("data"))
            .and_then(|d| d.get("entity"))
            .or_else(|| page_props.and_then(|p| p.get("entity")))
            .ok_or_else(|| {
                SpotifyError::Parse("Spotify playlist entity missing or private".to_owned())
            })?;

        let playlist_title = state_data
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| state_data.get("title").and_then(Value::as_str))
            .unwrap_or("Imported Spotify Playlist")
            .to_owned();

        let track_list = state_data
            .get("trackList")
            .and_then(Value::as_array)
            .or_else(|| state_data.get("tracks").and_then(Value::as_array));

        let mut tracks = Vec::new();
        if let Some(items) = track_list {
            for elem in items {
                let Some(title) = elem
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| elem.get("name").and_then(Value::as_str))
                else {
                    continue;
                };
                let subtitle = elem
                    .get("subtitle")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        elem.get("artists").and_then(Value::as_array).map(|arr| {
                            arr.iter()
                                .filter_map(|a| a.get("name").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                    })
                    .unwrap_or_default();
                let title = title.trim();
                if !title.is_empty() {
                    tracks.push(SpotifyImportTrack {
                        title: title.to_owned(),
                        artist: subtitle.trim().to_owned(),
                    });
                }
            }
        }

        if tracks.is_empty() {
            return Err(SpotifyError::Parse(
                "No tracks found in public Spotify playlist.".to_owned(),
            ));
        }

        let token = page_props
            .and_then(|p| p.get("state"))
            .and_then(|s| s.get("settings"))
            .and_then(|s| s.get("session"))
            .and_then(|s| s.get("accessToken"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        Ok((playlist_title, tracks, token))
    }

    /// Fetches public Spotify playlist title and tracks from Spotify's embed page,
    /// paging through Pathfinder v1 if the playlist exceeds the 100-track embed cutoff.
    pub async fn fetch_playlist_tracks(
        http: &reqwest::Client,
        playlist_id: &str,
    ) -> Result<(String, Vec<SpotifyImportTrack>), SpotifyError> {
        let url = format!("https://open.spotify.com/embed/playlist/{playlist_id}");
        let resp = http
            .get(&url)
            .header(reqwest::header::USER_AGENT, IMPORTER_USER_AGENT)
            .send()
            .await?;
        let status = resp.status();
        let html = resp.text().await?;
        if !status.is_success() || html.trim().is_empty() {
            return Err(SpotifyError::HttpStatus {
                operation: "embed/playlist".to_owned(),
                status: status.as_u16(),
            });
        }

        let (title, mut tracks, token) = Self::parse_embed_html(&html)?;

        if tracks.len() >= EMBED_TRACK_LIMIT
            && let Some(token) = token
            && let Ok(extra) =
                Self::fetch_remaining_tracks(http, playlist_id, &token, tracks.len()).await
        {
            tracks.extend(extra);
        }

        Ok((title, tracks))
    }

    async fn fetch_remaining_tracks(
        http: &reqwest::Client,
        playlist_id: &str,
        token: &str,
        start: usize,
    ) -> Result<Vec<SpotifyImportTrack>, SpotifyError> {
        let mut out = Vec::new();
        let mut offset = start;
        loop {
            let variables = json!({
                "uri": format!("spotify:playlist:{playlist_id}"),
                "offset": offset,
                "limit": PAGE_SIZE,
                "enableWatchFeedEntrypoint": false,
            })
            .to_string();
            let extensions = json!({
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": FETCH_PLAYLIST_V1_HASH,
                }
            })
            .to_string();

            let resp = http
                .get(PATHFINDER_V1_URL)
                .query(&[
                    ("operationName", "fetchPlaylist"),
                    ("variables", variables.as_str()),
                    ("extensions", extensions.as_str()),
                ])
                .header(reqwest::header::USER_AGENT, IMPORTER_USER_AGENT)
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
                .header("App-platform", "WebPlayer")
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await?;
            let status = resp.status();
            if !status.is_success() {
                return Err(SpotifyError::HttpStatus {
                    operation: "fetchPlaylist".to_owned(),
                    status: status.as_u16(),
                });
            }
            let body: Value = resp
                .json()
                .await
                .map_err(|e| SpotifyError::Parse(e.to_string()))?;
            let Some(content) = body
                .get("data")
                .and_then(|d| d.get("playlistV2"))
                .and_then(|p| p.get("content"))
            else {
                break;
            };
            let Some(items) = content.get("items").and_then(Value::as_array) else {
                break;
            };
            for item in items {
                let Some(data) = item.get("itemV2").and_then(|i| i.get("data")) else {
                    continue;
                };
                let title = data
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if title.is_empty() {
                    continue;
                }
                let artist = data
                    .get("artists")
                    .and_then(|a| a.get("items"))
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|a| {
                                a.get("profile")
                                    .and_then(|p| p.get("name"))
                                    .and_then(Value::as_str)
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                out.push(SpotifyImportTrack {
                    title: title.to_owned(),
                    artist,
                });
            }
            let Some(total) = content
                .get("totalCount")
                .and_then(Value::as_u64)
                .map(|v| v as usize)
            else {
                break;
            };
            offset += items.len();
            if items.is_empty() || offset >= total {
                break;
            }
        }
        Ok(out)
    }

    /// Resolves a single [`SpotifyTrack`] against YouTube Music using [`TrackMatcher`].
    pub async fn match_track(repo: &YtMusicRepository, track: &SpotifyTrack) -> Option<Song> {
        let query = [track.title.as_str(), track.artist.as_str()]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let page = repo.search(&query, SearchFilter::Songs, false).await.ok()?;
        let candidates: Vec<Song> = page
            .rows
            .into_iter()
            .filter_map(|item| match item {
                SearchResult::Track { song } => Some(song),
                _ => None,
            })
            .collect();

        let target = Target {
            title: track.title.clone(),
            artist: track.artist.clone(),
            duration_sec: (track.duration_ms > 0).then_some(track.duration_ms / 1000),
            album: track.album.clone(),
            is_explicit: None,
            is_video: false,
        };
        TrackMatcher::best(&candidates, &target)
            .cloned()
            .or_else(|| candidates.into_iter().next())
    }

    /// Resolves a list of [`SpotifyImportTrack`] items to YouTube Music [`Song`]s
    /// with bounded concurrency (4 in-flight searches) and optional language preference filtering.
    pub async fn resolve_to_songs<F>(
        repo: Arc<YtMusicRepository>,
        tracks: &[SpotifyImportTrack],
        language: Option<&str>,
        on_progress: F,
    ) -> (Vec<Song>, Vec<SpotifyImportTrack>)
    where
        F: Fn(usize, usize) + Send + Sync,
    {
        let total = tracks.len();
        let completed = Arc::new(AtomicUsize::new(0));
        let semaphore = Arc::new(Semaphore::new(4));
        let clean_lang: Option<String> = language
            .map(str::trim)
            .filter(|l| {
                !l.is_empty()
                    && !l.eq_ignore_ascii_case("No Preference")
                    && !l.eq_ignore_ascii_case("Multilingual")
            })
            .map(ToOwned::to_owned);

        let futs = tracks.iter().map(|track| {
            let repo = Arc::clone(&repo);
            let semaphore = Arc::clone(&semaphore);
            let completed = Arc::clone(&completed);
            let clean_lang = clean_lang.clone();
            let track = track.clone();
            let on_progress = &on_progress;

            async move {
                let _permit = semaphore.acquire().await.ok()?;
                let query = match &clean_lang {
                    Some(lang) => format!("{} {} {lang}", track.title, track.artist),
                    None => format!("{} {}", track.title, track.artist),
                };
                let query = query.trim().to_owned();

                let candidates: Vec<Song> = repo
                    .search(&query, SearchFilter::Songs, false)
                    .await
                    .ok()
                    .map(|page| {
                        page.rows
                            .into_iter()
                            .filter_map(|item| match item {
                                SearchResult::Track { song } => Some(song),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let target = Target::new(&track.title, &track.artist);
                let best_match = TrackMatcher::best(&candidates, &target)
                    .cloned()
                    .or_else(|| candidates.first().cloned());

                let matched = if let Some(lang) = &clean_lang {
                    if !candidates.is_empty() {
                        let conflicting: Vec<String> = KNOWN_LANGUAGES
                            .iter()
                            .filter(|l| !l.eq_ignore_ascii_case(lang))
                            .map(|l| l.to_ascii_lowercase())
                            .collect();
                        let non_conflicting: Vec<Song> = candidates
                            .into_iter()
                            .filter(|candidate| {
                                let meta = format!(
                                    "{} {} {}",
                                    candidate.title,
                                    candidate.artist,
                                    candidate.album_name.as_deref().unwrap_or_default()
                                )
                                .to_ascii_lowercase();
                                !conflicting.iter().any(|c| {
                                    meta.contains(&format!("({c})"))
                                        || meta.contains(&format!("{c} version"))
                                        || meta.contains(&format!("{c} song"))
                                })
                            })
                            .collect();
                        TrackMatcher::best(&non_conflicting, &target)
                            .cloned()
                            .or_else(|| non_conflicting.into_iter().next())
                            .or(best_match)
                    } else {
                        best_match
                    }
                } else {
                    best_match
                };

                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(done, total);

                Some((matched, track))
            }
        });

        let results = futures_util::future::join_all(futs).await;
        let mut songs = Vec::new();
        let mut unmatched = Vec::new();
        for item in results.into_iter().flatten() {
            match item.0 {
                Some(song) => songs.push(song),
                None => unmatched.push(item.1),
            }
        }
        (songs, unmatched)
    }
}

/// Signed-in Spotify library client speaking Pathfinder v2 GraphQL.
pub struct SpotifyLibrary<'a> {
    http: &'a reqwest::Client,
    tokens: SpotifyTokens,
}

impl<'a> SpotifyLibrary<'a> {
    pub fn new(http: &'a reqwest::Client, tokens: SpotifyTokens) -> Self {
        Self { http, tokens }
    }

    /// Fetches every playlist in the user's Spotify library, prepending the synthetic
    /// `"liked"` (Liked Songs) entry.
    pub async fn playlists(&self) -> Result<Vec<SpotifyPlaylist>, SpotifyError> {
        let mut collected = Vec::new();
        let mut offset = 0usize;
        loop {
            let variables = json!({
                "filters": ["Playlists"],
                "order": Value::Null,
                "textFilter": "",
                "features": ["LIKED_SONGS", "YOUR_EPISODES_V2", "PRERELEASES", "EVENTS"],
                "limit": 50,
                "offset": offset,
                "flatten": true,
                "expandedFolders": [],
                "folderUri": Value::Null,
                "includeFoldersWhenFlattening": false,
            });
            let root = self.gql("libraryV3", LIBRARY_V3_HASH, variables).await?;
            let (items, total, raw) = parse_playlist_page(&root)?;
            collected.extend(items);
            offset += raw;
            if raw == 0 || offset >= total {
                break;
            }
        }

        let mut all = Vec::with_capacity(collected.len() + 1);
        all.push(SpotifyPlaylist {
            id: LIKED_ID.to_owned(),
            name: "Liked Songs".to_owned(),
            owner: None,
            image_url: Some(LIKED_COVER.to_owned()),
        });
        all.extend(collected);
        Ok(all)
    }

    /// Fetches all tracks of `playlist_id` (or [`LIKED_ID`]), invoking `on_page`
    /// with the accumulated tracks after each page.
    pub async fn tracks<F>(
        &self,
        playlist_id: &str,
        mut on_page: F,
    ) -> Result<Vec<SpotifyTrack>, SpotifyError>
    where
        F: FnMut(&[SpotifyTrack]),
    {
        let mut collected = Vec::new();
        let mut offset = 0usize;
        loop {
            let (items, total, raw) = if playlist_id == LIKED_ID {
                let variables = json!({ "offset": offset, "limit": 100 });
                let root = self
                    .gql("fetchLibraryTracks", FETCH_LIKED_V2_HASH, variables)
                    .await?;
                parse_liked_page(&root)?
            } else {
                let variables = json!({
                    "uri": format!("spotify:playlist:{playlist_id}"),
                    "offset": offset,
                    "limit": 100,
                    "enableWatchFeedEntrypoint": false,
                });
                let root = self
                    .gql("fetchPlaylist", FETCH_PLAYLIST_V2_HASH, variables)
                    .await?;
                parse_track_page(&root)?
            };
            collected.extend(items);
            offset += raw;
            on_page(&collected);
            if raw == 0 || offset >= total {
                break;
            }
        }
        Ok(collected)
    }

    /// Fetches the largest available cover image URL for `playlist_id`.
    pub async fn cover(&self, playlist_id: &str) -> Result<Option<String>, SpotifyError> {
        if playlist_id == LIKED_ID {
            return Ok(Some(LIKED_COVER.to_owned()));
        }
        let variables = json!({
            "uri": format!("spotify:playlist:{playlist_id}"),
            "offset": 0,
            "limit": 1,
            "enableWatchFeedEntrypoint": false,
        });
        let root = self
            .gql("fetchPlaylist", FETCH_PLAYLIST_V2_HASH, variables)
            .await?;
        let images = root
            .get("data")
            .and_then(|d| d.get("playlistV2"))
            .and_then(|p| p.get("images"));
        Ok(largest_source(images))
    }

    async fn gql(
        &self,
        operation: &str,
        hash: &str,
        variables: Value,
    ) -> Result<Value, SpotifyError> {
        let payload = json!({
            "variables": variables,
            "operationName": operation,
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": hash,
                }
            }
        });
        let mut req = self
            .http
            .post(PATHFINDER_V2_URL)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.tokens.bearer),
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .header("app-platform", "WebPlayer")
            .header(reqwest::header::ORIGIN, "https://open.spotify.com")
            .header(reqwest::header::REFERER, "https://open.spotify.com/")
            .header(reqwest::header::USER_AGENT, LIBRARY_USER_AGENT)
            .json(&payload);
        if let Some(ct) = &self.tokens.client_token {
            req = req.header("Client-Token", ct);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if status.as_u16() == 401 {
            return Err(SpotifyError::Unauthorized);
        }
        if !status.is_success() {
            return Err(SpotifyError::HttpStatus {
                operation: operation.to_owned(),
                status: status.as_u16(),
            });
        }
        let root: Value =
            serde_json::from_str(&text).map_err(|e| SpotifyError::Parse(e.to_string()))?;
        if let Some(msg) = root
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|err| err.get("message"))
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
        {
            return Err(SpotifyError::GraphQl(msg.to_owned()));
        }
        Ok(root)
    }
}

/// Parses a `libraryV3` GraphQL page into `(playlists, total_count, raw_item_count)`.
pub fn parse_playlist_page(
    root: &Value,
) -> Result<(Vec<SpotifyPlaylist>, usize, usize), SpotifyError> {
    let library = root
        .get("data")
        .and_then(|d| d.get("me"))
        .and_then(|m| m.get("libraryV3"))
        .ok_or_else(|| SpotifyError::Parse("Spotify library response was empty".to_owned()))?;
    let raw_items = library
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut items = Vec::new();
    for elem in raw_items {
        let Some(wrapper) = elem.get("item") else {
            continue;
        };
        let type_name = wrapper
            .get("__typename")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !type_name.to_ascii_lowercase().contains("playlist") {
            continue;
        }
        let Some(data) = wrapper.get("data") else {
            continue;
        };
        if data.get("__typename").and_then(Value::as_str) != Some("Playlist") {
            continue;
        }
        let Some(uri) = wrapper.get("_uri").and_then(Value::as_str) else {
            continue;
        };
        let name = data.get("name").and_then(Value::as_str).unwrap_or_default();
        if name.trim().is_empty() {
            continue;
        }
        let id = uri.rsplit(':').next().unwrap_or(uri).to_owned();
        let owner = data
            .get("ownerV2")
            .and_then(|o| o.get("data"))
            .and_then(|d| d.get("name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        items.push(SpotifyPlaylist {
            id,
            name: name.to_owned(),
            owner,
            image_url: largest_source(data.get("images")),
        });
    }

    let total = library
        .get("totalCount")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(raw_items.len());
    Ok((items, total, raw_items.len()))
}

/// Parses a `fetchPlaylist` Pathfinder v2 page into `(tracks, total_count, raw_item_count)`.
pub fn parse_track_page(root: &Value) -> Result<(Vec<SpotifyTrack>, usize, usize), SpotifyError> {
    let content = root
        .get("data")
        .and_then(|d| d.get("playlistV2"))
        .and_then(|p| p.get("content"))
        .ok_or_else(|| SpotifyError::Parse("Spotify playlist response was empty".to_owned()))?;
    let raw_items = content
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut items = Vec::new();
    for elem in raw_items {
        let Some(data) = elem.get("itemV2").and_then(|i| i.get("data")) else {
            continue;
        };
        let title = data.get("name").and_then(Value::as_str).unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let Some(uri) = data
            .get("uri")
            .and_then(Value::as_str)
            .or_else(|| data.get("_uri").and_then(Value::as_str))
        else {
            continue;
        };
        let artist = extract_artists(data);
        let album = data.get("albumOfTrack");
        let duration_ms = data
            .get("duration")
            .and_then(|d| d.get("totalMilliseconds"))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        items.push(SpotifyTrack {
            id: uri.rsplit(':').next().unwrap_or(uri).to_owned(),
            title: title.to_owned(),
            artist,
            album: album
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            duration_ms,
            image_url: last_cover_url(album),
        });
    }

    let total = content
        .get("totalCount")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(raw_items.len());
    Ok((items, total, raw_items.len()))
}

/// Parses a `fetchLibraryTracks` (Liked Songs) page into `(tracks, total_count, raw_item_count)`.
pub fn parse_liked_page(root: &Value) -> Result<(Vec<SpotifyTrack>, usize, usize), SpotifyError> {
    let tracks = root
        .get("data")
        .and_then(|d| d.get("me"))
        .and_then(|m| m.get("library"))
        .and_then(|l| l.get("tracks"))
        .ok_or_else(|| SpotifyError::Parse("Spotify liked songs response was empty".to_owned()))?;
    let raw_items = tracks
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut items = Vec::new();
    for elem in raw_items {
        let Some(wrapper) = elem.get("track") else {
            continue;
        };
        let Some(data) = wrapper.get("data") else {
            continue;
        };
        let title = data.get("name").and_then(Value::as_str).unwrap_or_default();
        if title.trim().is_empty() {
            continue;
        }
        let Some(uri) = wrapper
            .get("_uri")
            .and_then(Value::as_str)
            .or_else(|| data.get("uri").and_then(Value::as_str))
        else {
            continue;
        };
        let album = data.get("albumOfTrack");
        let duration_ms = data
            .get("trackDuration")
            .or_else(|| data.get("duration"))
            .and_then(|d| d.get("totalMilliseconds"))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        items.push(SpotifyTrack {
            id: uri.rsplit(':').next().unwrap_or(uri).to_owned(),
            title: title.to_owned(),
            artist: extract_artists(data),
            album: album
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            duration_ms,
            image_url: last_cover_url(album),
        });
    }

    let total = tracks
        .get("totalCount")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(raw_items.len());
    Ok((items, total, raw_items.len()))
}

fn extract_artists(data: &Value) -> String {
    data.get("artists")
        .and_then(|a| a.get("items"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    a.get("profile")
                        .and_then(|p| p.get("name"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn last_cover_url(album: Option<&Value>) -> Option<String> {
    album?
        .get("coverArt")?
        .get("sources")?
        .as_array()?
        .iter()
        .filter_map(|s| s.get("url").and_then(Value::as_str))
        .next_back()
        .map(ToOwned::to_owned)
}

/// Selects the widest image source by `width` rather than list position.
pub fn largest_source(images: Option<&Value>) -> Option<String> {
    let sources = images?
        .get("items")?
        .as_array()?
        .first()?
        .get("sources")?
        .as_array()?;
    sources
        .iter()
        .filter_map(|src| {
            let url = src.get("url").and_then(Value::as_str)?;
            let width = src.get("width").and_then(Value::as_i64).unwrap_or(0);
            Some((url, width))
        })
        .max_by_key(|(_, width)| *width)
        .map(|(url, _)| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlists_include_folder_leaves() {
        let root: Value = serde_json::from_str(
            r#"
            {"data":{"me":{"libraryV3":{"totalCount":1,"items":[
              {"item":{"__typename":"PlaylistResponseWrapper","_uri":"spotify:playlist:abc","data":{
                "__typename":"Playlist","name":"Road","ownerV2":{"data":{"name":"Amit"}},
                "images":{"items":[{"sources":[{"url":"https://i.scdn.co/x"}]}]}
              }}}
            ]}}}}
            "#,
        )
        .expect("valid json");
        let (items, total, raw) = parse_playlist_page(&root).expect("parse playlist page");
        assert_eq!(total, 1);
        assert_eq!(raw, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "abc");
        assert_eq!(items[0].name, "Road");
        assert_eq!(items[0].owner.as_deref(), Some("Amit"));
        assert_eq!(items[0].image_url.as_deref(), Some("https://i.scdn.co/x"));
    }

    #[test]
    fn tracks_read_title_artist_and_duration() {
        let root: Value = serde_json::from_str(
            r#"
            {"data":{"playlistV2":{"content":{"totalCount":1,"items":[
              {"itemV2":{"data":{
                "name":"Song","uri":"spotify:track:t1",
                "artists":{"items":[{"uri":"spotify:artist:a","profile":{"name":"Artist"}}]},
                "albumOfTrack":{"name":"Album","coverArt":{"sources":[{"url":"https://img"}]}},
                "duration":{"totalMilliseconds":180000}
              }}}
            ]}}}}
            "#,
        )
        .expect("valid json");
        let (tracks, total, _) = parse_track_page(&root).expect("parse track page");
        assert_eq!(total, 1);
        assert_eq!(tracks.len(), 1);
        let track = &tracks[0];
        assert_eq!(track.id, "t1");
        assert_eq!(track.title, "Song");
        assert_eq!(track.artist, "Artist");
        assert_eq!(track.album.as_deref(), Some("Album"));
        assert_eq!(track.duration_ms, 180_000);
        assert_eq!(track.image_url.as_deref(), Some("https://img"));
    }

    #[test]
    fn playlist_ids_come_from_links_and_uris() {
        let id = "37i9dQZF1DXcBWIGoYBM5M";
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!(
                "https://open.spotify.com/playlist/{id}"
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!(
                "  https://open.spotify.com/playlist/{id}?si=abc123  "
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!(
                "https://open.spotify.com/intl-id/playlist/{id}"
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!("spotify:playlist:{id}")).as_deref(),
            Some(id)
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!("https://open.spotify.com/album/{id}")),
            None
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!("https://notspotify.com/playlist/{id}")),
            None
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id(&format!("ftp://open.spotify.com/playlist/{id}")),
            None
        );
        assert_eq!(
            SpotifyImporter::extract_playlist_id("https://open.spotify.com/playlist/short"),
            None
        );
        assert_eq!(SpotifyImporter::extract_playlist_id(""), None);
    }

    #[test]
    fn largest_source_picks_widest_not_last() {
        let images = json!({
            "items": [{
                "sources": [
                    {"url": "https://i.scdn.co/640", "width": 640},
                    {"url": "https://i.scdn.co/60", "width": 60}
                ]
            }]
        });
        assert_eq!(
            largest_source(Some(&images)).as_deref(),
            Some("https://i.scdn.co/640")
        );
    }

    #[test]
    fn parse_embed_html_extracts_title_tracks_and_token() {
        let html = r#"
        <html><body>
        <script id="__NEXT_DATA__" type="application/json">{
            "props": {
                "pageProps": {
                    "state": {
                        "data": {
                            "entity": {
                                "name": "Late Night Drive",
                                "trackList": [
                                    {"title": "Nightcall", "subtitle": "Kavinsky"},
                                    {"name": "A Real Hero", "artists": [{"name": "College"}, {"name": "Electric Youth"}]}
                                ]
                            }
                        },
                        "settings": {"session": {"accessToken": "anon-tok"}}
                    }
                }
            }
        }</script>
        </body></html>"#;

        let (title, tracks, token) = SpotifyImporter::parse_embed_html(html).expect("embed parse");
        assert_eq!(title, "Late Night Drive");
        assert_eq!(token.as_deref(), Some("anon-tok"));
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title, "Nightcall");
        assert_eq!(tracks[0].artist, "Kavinsky");
        assert_eq!(tracks[1].title, "A Real Hero");
        assert_eq!(tracks[1].artist, "College, Electric Youth");
    }

    #[test]
    fn parse_liked_page_reads_track_duration_and_uri() {
        let root = json!({
            "data": {
                "me": {
                    "library": {
                        "tracks": {
                            "totalCount": 1,
                            "items": [{
                                "track": {
                                    "_uri": "spotify:track:liked1",
                                    "data": {
                                        "name": "Loved",
                                        "artists": {"items": [{"profile": {"name": "Singer"}}]},
                                        "albumOfTrack": {"name": "LP", "coverArt": {"sources": [{"url": "https://art"}]}},
                                        "trackDuration": {"totalMilliseconds": 215000}
                                    }
                                }
                            }]
                        }
                    }
                }
            }
        });
        let (tracks, total, raw) = parse_liked_page(&root).expect("parse liked page");
        assert_eq!((total, raw), (1, 1));
        assert_eq!(tracks[0].id, "liked1");
        assert_eq!(tracks[0].duration_ms, 215_000);
        assert_eq!(tracks[0].image_url.as_deref(), Some("https://art"));
    }

    #[test]
    fn match_prefix_helpers_distinguish_pending_and_missing() {
        let mut song = Song::new("sp:abc", "Track", "Artist", None);
        assert!(is_match_pending(&song));
        assert!(!is_match_missing(&song));
        assert!(is_unresolved_spotify(&song));

        song.video_id = "sp-miss:abc".to_owned();
        assert!(!is_match_pending(&song));
        assert!(is_match_missing(&song));
        assert!(is_unresolved_spotify(&song));

        song.video_id = "dQw4w9WgXcQ".to_owned();
        assert!(!is_unresolved_spotify(&song));
    }
}
