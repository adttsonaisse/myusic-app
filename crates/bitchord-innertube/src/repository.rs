use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, RwLock};

use bitchord_model::{
    Account, AccountChannel, ArtistPage, HomeFeed, HomeShelf, LibraryState, LikeStatus,
    MoodGenreSection, PlaylistMove, PlaylistPrivacy, SearchFilter, SearchResult, Song, SongMenu,
    UserPlaylist, VersionPair, VersionTimeMap,
};

use crate::client::InnertubeClient;
use crate::error::InnertubeError;
use crate::parser::{
    BrowseHeader, InnertubeParser, LibraryItemPage, PlaylistShelfPage, SearchPage,
    SearchSuggestionsResult, ensure_song_thumbnail,
};

const TYPEAHEAD_FALLBACK_MAX_ITEMS: usize = 6;

/// Standard YouTube Music browse IDs used across Home, Explore, and Library.
pub mod browse_ids {
    pub const HOME: &str = "FEmusic_home";
    pub const NEW_RELEASES: &str = "FEmusic_new_releases";
    pub const EXPLORE: &str = "FEmusic_explore";
    pub const MOODS_AND_GENRES: &str = "FEmusic_moods_and_genres";
    pub const MIXED_FOR_YOU: &str = "FEmusic_mixed_for_you";
    pub const HISTORY: &str = "FEmusic_history";
    pub const LIKED_VIDEOS: &str = "FEmusic_liked_videos";
    pub const LIKED_PLAYLISTS: &str = "FEmusic_liked_playlists";
    pub const LIKED_ALBUMS: &str = "FEmusic_liked_albums";
    pub const LIBRARY_ARTISTS: &str = "FEmusic_library_corpus_track_artists";
    pub const SUBSCRIPTIONS: &str = "FEmusic_library_corpus_artists";
    pub const PODCASTS: &str = "FEmusic_library_non_music_audio_list";
}

/// High-level YouTube Music repository over [`InnertubeClient`] and
/// [`InnertubeParser`], matching `YtMusicRepository.kt`'s single-retry session
/// scope refresh on HTTP 401/403 and version-pair time-map caching.
pub struct YtMusicRepository {
    client: Arc<InnertubeClient>,
    parser: Arc<InnertubeParser>,
    version_time_maps: RwLock<HashMap<(String, String), VersionTimeMap>>,
}

impl YtMusicRepository {
    pub fn new(client: Arc<InnertubeClient>, parser: Arc<InnertubeParser>) -> Self {
        Self {
            client,
            parser,
            version_time_maps: RwLock::new(HashMap::new()),
        }
    }

    pub fn client(&self) -> &Arc<InnertubeClient> {
        &self.client
    }

    pub fn parser(&self) -> &Arc<InnertubeParser> {
        &self.parser
    }

    /// Runs an InnerTube operation, refreshing `SessionScope` once and retrying
    /// if the signed-in request was rejected with HTTP 401 or 403.
    async fn call<F, Fut, T>(&self, mut op: F) -> Result<T, InnertubeError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, InnertubeError>>,
    {
        match op().await {
            Ok(val) => Ok(val),
            Err(err) if err.is_auth_rejection() && self.client.is_signed_in() => {
                self.client.refresh_session_scope().await;
                op().await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn home(&self) -> Result<HomeFeed, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_ids::HOME, None).await?;
            Ok(HomeFeed {
                shelves: self.parser.parse_home(&raw),
                continuation: self.parser.continuation_token(&raw),
            })
        })
        .await
    }

    pub async fn more_home(&self, token: &str) -> Result<HomeFeed, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse_continuation(token).await?;
            Ok(HomeFeed {
                shelves: self.parser.parse_home_continuation(&raw),
                continuation: self.parser.continuation_token(&raw),
            })
        })
        .await
    }

    pub async fn home_supplement(&self, browse_id: &str) -> Result<Vec<HomeShelf>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_id, None).await?;
            Ok(self.parser.parse_home(&raw))
        })
        .await
    }

    pub async fn moods_and_genres(&self) -> Result<Vec<MoodGenreSection>, InnertubeError> {
        self.call(|| async {
            let raw = self
                .client
                .browse(browse_ids::MOODS_AND_GENRES, None)
                .await?;
            Ok(self.parser.parse_mood_and_genres(&raw))
        })
        .await
    }

    pub async fn search(
        &self,
        query: &str,
        filter: SearchFilter,
        anonymous: bool,
    ) -> Result<SearchPage, InnertubeError> {
        self.call(|| async {
            let raw = self
                .client
                .search(query, filter.params(), anonymous)
                .await?;
            Ok(self
                .parser
                .parse_search_page(&raw, filter == SearchFilter::Videos))
        })
        .await
    }

    pub async fn search_more(
        &self,
        token: &str,
        include_videos: bool,
    ) -> Result<SearchPage, InnertubeError> {
        self.call(|| async {
            let raw = self.client.search_continuation(token).await?;
            Ok(self.parser.parse_search_page(&raw, include_videos))
        })
        .await
    }

    pub async fn search_suggestions(&self, input: &str) -> Result<Vec<String>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.search_suggestions(input).await?;
            Ok(self.parser.parse_search_suggestions(&raw))
        })
        .await
    }

    /// Fetches structured search suggestions (query completions + direct song,
    /// artist, album, and playlist matches) in a single `music/get_search_suggestions`
    /// request, falling back thinly to `search_typeahead` (max 6 items, error ignored)
    /// only when the suggestion endpoint returns no direct items.
    pub async fn search_suggestions_structured(
        &self,
        input: &str,
    ) -> Result<SearchSuggestionsResult, InnertubeError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(SearchSuggestionsResult::default());
        }
        let mut result = self
            .call(|| async {
                let raw = self.client.search_suggestions(trimmed).await?;
                Ok(self.parser.parse_search_suggestions_structured(&raw))
            })
            .await?;

        if result.items.is_empty()
            && let Ok(page) = self.search_typeahead(trimmed).await
        {
            let mut seen = HashSet::new();
            for mut row in page.rows {
                let key = match &mut row {
                    SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                        ensure_song_thumbnail(song);
                        format!("v:{}", song.video_id)
                    }
                    SearchResult::Browse { item } => format!("b:{}", item.browse_id),
                };
                if seen.insert(key) {
                    result.items.push(row);
                    if result.items.len() >= TYPEAHEAD_FALLBACK_MAX_ITEMS {
                        break;
                    }
                }
            }
        }

        Ok(result)
    }

    pub async fn search_typeahead(&self, query: &str) -> Result<SearchPage, InnertubeError> {
        let raw = self.client.search_typeahead(query).await?;
        Ok(self.parser.parse_search_page(&raw, false))
    }

    pub async fn artist_page(&self, browse_id: &str) -> Result<ArtistPage, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_id, None).await?;
            Ok(self.parser.parse_artist_page(&raw))
        })
        .await
    }

    pub async fn browse_songs(
        &self,
        browse_id: &str,
        params: Option<&str>,
    ) -> Result<(Option<BrowseHeader>, Vec<Song>, Option<String>), InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_id, params).await?;
            let header = self.parser.parse_browse_header(&raw);
            let show = self.parser.podcast_show(&raw);
            if let Some(pl) = self.parser.parse_playlist_shelf(&raw) {
                return Ok((header, pl.songs, pl.continuation));
            }
            let songs = self.parser.collect_songs_deep(&raw, show.as_deref());
            let cont = self.parser.continuation_token(&raw);
            Ok((header, songs, cont))
        })
        .await
    }

    pub async fn playlist_shelf(
        &self,
        browse_id: &str,
    ) -> Result<Option<PlaylistShelfPage>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_id, None).await?;
            Ok(self.parser.parse_playlist_shelf(&raw))
        })
        .await
    }

    pub async fn library_items(&self, browse_id: &str) -> Result<LibraryItemPage, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_id, None).await?;
            Ok(self.parser.parse_library_item_page(&raw))
        })
        .await
    }

    pub async fn history(&self) -> Result<Vec<Song>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_ids::HISTORY, None).await?;
            Ok(self.parser.collect_songs_deep(&raw, None))
        })
        .await
    }

    pub async fn radio_queue(
        &self,
        video_id: &str,
        prefer_audio: bool,
        prefer_video: bool,
    ) -> Result<Vec<Song>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.next(video_id).await?;
            Ok(self
                .parser
                .parse_watch_queue(&raw, prefer_audio, prefer_video))
        })
        .await
    }

    pub async fn playlist_queue(
        &self,
        playlist_id: &str,
        video_id: Option<&str>,
        params: Option<&str>,
        prefer_audio: bool,
        prefer_video: bool,
    ) -> Result<Vec<Song>, InnertubeError> {
        self.call(|| async {
            let raw = self
                .client
                .next_playlist(playlist_id, video_id, params)
                .await?;
            Ok(self
                .parser
                .parse_watch_queue(&raw, prefer_audio, prefer_video))
        })
        .await
    }

    pub async fn version_pair(
        &self,
        video_id: &str,
        want_audio: bool,
    ) -> Result<Option<VersionPair>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.next(video_id).await?;
            let pair = self.parser.parse_version_pair(&raw, video_id, want_audio);
            if let Some(ref p) = pair
                && let Some(ref map) = p.time_map
            {
                let mut cache = self
                    .version_time_maps
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                cache.insert((video_id.to_owned(), p.song.video_id.clone()), map.clone());
                cache.insert(
                    (p.song.video_id.clone(), video_id.to_owned()),
                    map.inverted(),
                );
            }
            Ok(pair)
        })
        .await
    }

    pub fn cached_version_time_map(&self, from_id: &str, to_id: &str) -> Option<VersionTimeMap> {
        self.version_time_maps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(from_id.to_owned(), to_id.to_owned()))
            .cloned()
    }

    pub async fn song_menu(&self, video_id: &str) -> Result<Option<SongMenu>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.next(video_id).await?;
            Ok(self.parser.parse_song_menu(&raw, video_id))
        })
        .await
    }

    pub async fn library_state(
        &self,
        browse_id: &str,
    ) -> Result<Option<LibraryState>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.browse(browse_id, None).await?;
            Ok(self.parser.parse_library_state(&raw))
        })
        .await
    }

    pub async fn account(&self) -> Result<Option<Account>, InnertubeError> {
        self.call(|| async {
            let raw = self.client.account_menu().await?;
            Ok(self.parser.parse_account(&raw))
        })
        .await
    }

    pub async fn account_channels(&self) -> Result<Vec<AccountChannel>, InnertubeError> {
        let from_list = self
            .call(|| async { self.client.accounts_list().await })
            .await
            .map(|raw| self.parser.parse_account_channels(&raw))
            .unwrap_or_default();
        if !from_list.is_empty() {
            return Ok(from_list);
        }
        let raw = self.client.account_switcher().await?;
        Ok(self.parser.parse_account_channels(&raw))
    }

    pub async fn user_playlists(&self) -> Result<Vec<UserPlaylist>, InnertubeError> {
        self.call(|| async {
            let raw = self
                .client
                .browse(browse_ids::LIKED_PLAYLISTS, None)
                .await?;
            Ok(self.parser.parse_user_playlists(&raw))
        })
        .await
    }

    pub async fn rate(&self, video_id: &str, status: LikeStatus) -> Result<(), InnertubeError> {
        self.call(|| async { self.client.rate(video_id, status).await })
            .await
    }

    pub async fn rate_playlist(
        &self,
        playlist_id: &str,
        saved: bool,
    ) -> Result<(), InnertubeError> {
        self.call(|| async { self.client.rate_playlist(playlist_id, saved).await })
            .await
    }

    pub async fn create_playlist(
        &self,
        title: &str,
        privacy: PlaylistPrivacy,
        description: Option<&str>,
        video_ids: &[String],
    ) -> Result<String, InnertubeError> {
        self.call(|| async {
            self.client
                .create_playlist(title, privacy, description, video_ids)
                .await
        })
        .await
    }

    pub async fn move_playlist_items(
        &self,
        playlist_id: &str,
        moves: &[PlaylistMove],
    ) -> Result<(), InnertubeError> {
        self.call(|| async { self.client.move_playlist_items(playlist_id, moves).await })
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::client::build_shared_http_client;

    async fn spawn_mock_repo(
        responder: impl Fn(String, String) -> (u16, String) + Send + Sync + 'static,
    ) -> YtMusicRepository {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let responder = Arc::new(responder);

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let responder = Arc::clone(&responder);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos;
                        }
                    };
                    let headers_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let req_line = headers_str.lines().next().unwrap_or_default().to_owned();
                    let content_len: usize = headers_str
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    while buf.len() < body_start + content_len {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(
                        &buf[body_start..(body_start + content_len).min(buf.len())],
                    )
                    .into_owned();
                    let (code, resp_body) = responder(req_line, body);
                    let reason = if code == 200 { "OK" } else { "ERR" };
                    let resp = format!(
                        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        let base = format!("http://{addr}");
        let client = Arc::new(InnertubeClient::with_endpoints(
            Arc::new(build_shared_http_client()),
            format!("{base}/youtubei/v1"),
            base.clone(),
            base,
        ));
        YtMusicRepository::new(client, Arc::new(InnertubeParser::default()))
    }

    #[tokio::test]
    async fn search_suggestions_structured_uses_single_request_when_items_present() {
        let suggest_hits = Arc::new(AtomicUsize::new(0));
        let search_hits = Arc::new(AtomicUsize::new(0));
        let s_hits = Arc::clone(&suggest_hits);
        let t_hits = Arc::clone(&search_hits);

        let repo = spawn_mock_repo(move |req_line, _body| {
            if req_line.contains("/music/get_search_suggestions") {
                s_hits.fetch_add(1, Ordering::SeqCst);
                return (
                    200,
                    r#"{
                      "contents": [
                        { "searchSuggestionsSectionRenderer": { "contents": [
                          { "searchSuggestionRenderer": {
                            "navigationEndpoint": { "searchEndpoint": { "query": "starboy" } }
                          } }
                        ] } },
                        { "searchSuggestionsSectionRenderer": { "contents": [
                          { "musicResponsiveListItemRenderer": {
                            "navigationEndpoint": { "watchEndpoint": { "videoId": "34Na4j8AVgA" } },
                            "flexColumns": [
                              { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [ { "text": "Starboy" } ] } } },
                              { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [ { "text": "Song • The Weeknd • 3:50" } ] } } }
                            ]
                          } }
                        ] } }
                      ]
                    }"#
                    .to_owned(),
                );
            }
            if req_line.contains("/search") {
                t_hits.fetch_add(1, Ordering::SeqCst);
            }
            (500, "{}".to_owned())
        })
        .await;

        let res = repo
            .search_suggestions_structured("starb")
            .await
            .expect("structured suggestions ok");
        assert_eq!(suggest_hits.load(Ordering::SeqCst), 1);
        assert_eq!(search_hits.load(Ordering::SeqCst), 0);
        assert_eq!(res.queries, vec!["starboy"]);
        assert_eq!(res.items.len(), 1);
        match &res.items[0] {
            SearchResult::Track { song } => {
                assert_eq!(song.video_id, "34Na4j8AVgA");
                assert_eq!(
                    song.thumbnail_url.as_deref(),
                    Some("https://i.ytimg.com/vi/34Na4j8AVgA/hqdefault.jpg")
                );
            }
            other => panic!("expected Track, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_suggestions_structured_falls_back_to_typeahead_when_items_empty() {
        let suggest_hits = Arc::new(AtomicUsize::new(0));
        let search_hits = Arc::new(AtomicUsize::new(0));
        let s_hits = Arc::clone(&suggest_hits);
        let t_hits = Arc::clone(&search_hits);

        let rows_json: String = (0..8)
            .map(|i| {
                format!(
                    r#"{{ "musicResponsiveListItemRenderer": {{
                        "playlistItemData": {{ "videoId": "vid0000000{i}" }},
                        "flexColumns": [
                            {{ "musicResponsiveListItemFlexColumnRenderer": {{ "text": {{ "runs": [ {{ "text": "Track {i}" }} ] }} }} }},
                            {{ "musicResponsiveListItemFlexColumnRenderer": {{ "text": {{ "runs": [ {{ "text": "Song • Artist {i} • 3:1{i}" }} ] }} }} }}
                        ]
                    }} }}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let search_payload = format!(r#"{{ "contents": [ {rows_json} ] }}"#);

        let repo = spawn_mock_repo(move |req_line, _body| {
            if req_line.contains("/music/get_search_suggestions") {
                s_hits.fetch_add(1, Ordering::SeqCst);
                return (
                    200,
                    r#"{
                      "contents": [
                        { "searchSuggestionsSectionRenderer": { "contents": [
                          { "searchSuggestionRenderer": {
                            "navigationEndpoint": { "searchEndpoint": { "query": "niche track" } }
                          } }
                        ] } }
                      ]
                    }"#
                    .to_owned(),
                );
            }
            if req_line.contains("/search") {
                t_hits.fetch_add(1, Ordering::SeqCst);
                return (200, search_payload.clone());
            }
            (404, "{}".to_owned())
        })
        .await;

        let res = repo
            .search_suggestions_structured("niche")
            .await
            .expect("fallback ok");
        assert_eq!(suggest_hits.load(Ordering::SeqCst), 1);
        assert_eq!(search_hits.load(Ordering::SeqCst), 1);
        assert_eq!(res.queries, vec!["niche track"]);
        assert_eq!(res.items.len(), TYPEAHEAD_FALLBACK_MAX_ITEMS);
        match &res.items[0] {
            SearchResult::Track { song } => {
                assert_eq!(song.video_id, "vid00000000");
                assert_eq!(
                    song.thumbnail_url.as_deref(),
                    Some("https://i.ytimg.com/vi/vid00000000/hqdefault.jpg")
                );
            }
            other => panic!("expected fallback Track, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_suggestions_structured_keeps_queries_on_fallback_error_and_skips_blank() {
        let suggest_hits = Arc::new(AtomicUsize::new(0));
        let search_hits = Arc::new(AtomicUsize::new(0));
        let s_hits = Arc::clone(&suggest_hits);
        let t_hits = Arc::clone(&search_hits);

        let repo = spawn_mock_repo(move |req_line, _body| {
            if req_line.contains("/music/get_search_suggestions") {
                s_hits.fetch_add(1, Ordering::SeqCst);
                return (
                    200,
                    r#"{
                      "contents": [
                        { "searchSuggestionsSectionRenderer": { "contents": [
                          { "historySuggestionRenderer": {
                            "navigationEndpoint": { "searchEndpoint": { "query": "lofi beats" } }
                          } }
                        ] } }
                      ]
                    }"#
                    .to_owned(),
                );
            }
            if req_line.contains("/search") {
                t_hits.fetch_add(1, Ordering::SeqCst);
                return (503, r#"{"error":"unavailable"}"#.to_owned());
            }
            (404, "{}".to_owned())
        })
        .await;

        let empty = repo
            .search_suggestions_structured("   ")
            .await
            .expect("blank ok");
        assert!(empty.queries.is_empty() && empty.items.is_empty());
        assert_eq!(suggest_hits.load(Ordering::SeqCst), 0);
        assert_eq!(search_hits.load(Ordering::SeqCst), 0);

        let res = repo
            .search_suggestions_structured("lofi")
            .await
            .expect("queries preserved on fallback failure");
        assert_eq!(suggest_hits.load(Ordering::SeqCst), 1);
        assert_eq!(search_hits.load(Ordering::SeqCst), 1);
        assert_eq!(res.queries, vec!["lofi beats"]);
        assert!(res.items.is_empty());
    }
}
