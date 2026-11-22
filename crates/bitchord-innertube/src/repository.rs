use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};

use bitchord_model::{
    Account, AccountChannel, ArtistPage, HomeFeed, HomeShelf, LibraryState, LikeStatus,
    MoodGenreSection, PlaylistMove, PlaylistPrivacy, SearchFilter, Song, SongMenu, UserPlaylist,
    VersionPair, VersionTimeMap,
};

use crate::client::InnertubeClient;
use crate::error::InnertubeError;
use crate::parser::{
    BrowseHeader, InnertubeParser, LibraryItemPage, PlaylistShelfPage, SearchPage,
};

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
