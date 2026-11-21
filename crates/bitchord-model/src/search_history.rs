use serde::{Deserialize, Serialize};

use crate::models::Song;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EntityType {
    Track,
    Album,
    Artist,
    Playlist,
    /// Any other page that opens rather than plays (e.g., podcast show).
    Other,
}

/// An entity the user tapped in search results — a track, album, artist, or playlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHistoryEntity {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub artwork_url: Option<String>,
    pub entity_type: EntityType,
    pub timestamp: i64,
}

impl SearchHistoryEntity {
    /// Repairs legacy history rows that filed `MPSPP*` podcast browse ids as `Track`.
    pub fn with_corrected_type(mut self) -> Self {
        if self.entity_type == EntityType::Track && self.id.starts_with("MPSPP") {
            self.entity_type = EntityType::Other;
        }
        self
    }

    /// Converts playable track entries into a `Song`, excluding legacy `"q:<term>"` query rows.
    pub fn to_song(&self) -> Option<Song> {
        if self.entity_type == EntityType::Track && !self.id.starts_with("q:") {
            Some(Song::new(
                self.id.clone(),
                self.title.clone(),
                self.subtitle.clone(),
                self.artwork_url.clone(),
            ))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrects_podcast_browse_id_and_skips_raw_query_rows() {
        let podcast = SearchHistoryEntity {
            id: "MPSPP12345".to_owned(),
            title: "Podcast".to_owned(),
            subtitle: "Show".to_owned(),
            artwork_url: None,
            entity_type: EntityType::Track,
            timestamp: 1,
        }
        .with_corrected_type();
        assert_eq!(EntityType::Other, podcast.entity_type);
        assert!(podcast.to_song().is_none());

        let query = SearchHistoryEntity {
            id: "q:daft punk".to_owned(),
            title: "daft punk".to_owned(),
            subtitle: "".to_owned(),
            artwork_url: None,
            entity_type: EntityType::Track,
            timestamp: 2,
        };
        assert!(query.to_song().is_none());

        let track = SearchHistoryEntity {
            id: "dQw4w9WgXcQ".to_owned(),
            title: "Song".to_owned(),
            subtitle: "Artist".to_owned(),
            artwork_url: Some("https://img".to_owned()),
            entity_type: EntityType::Track,
            timestamp: 3,
        };
        let song = track.to_song().expect("playable track");
        assert_eq!("dQw4w9WgXcQ", song.video_id);
        assert_eq!("Artist", song.artist);
    }
}
