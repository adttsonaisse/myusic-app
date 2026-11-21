use crate::models::Song;

pub const SPOTIFY_PENDING_PREFIX: &str = "sp:";
pub const SPOTIFY_MISSING_PREFIX: &str = "sp-miss:";

impl Song {
    /// Still looking for the YouTube Music version of this Spotify track.
    pub fn is_match_pending(&self) -> bool {
        self.video_id.starts_with(SPOTIFY_PENDING_PREFIX)
    }

    /// Looked, and there is no YouTube Music version to play.
    pub fn is_match_missing(&self) -> bool {
        self.video_id.starts_with(SPOTIFY_MISSING_PREFIX)
    }

    /// True if this row is a Spotify track that cannot be played yet.
    pub fn is_unresolved_spotify(&self) -> bool {
        self.is_match_pending() || self.is_match_missing()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_prefix_flags() {
        let pending = Song::new("sp:123", "Title", "Artist", None);
        assert!(pending.is_match_pending());
        assert!(!pending.is_match_missing());
        assert!(pending.is_unresolved_spotify());

        let missing = Song::new("sp-miss:123", "Title", "Artist", None);
        assert!(!missing.is_match_pending());
        assert!(missing.is_match_missing());
        assert!(missing.is_unresolved_spotify());

        let resolved = Song::new("dQw4w9WgXcQ", "Title", "Artist", None);
        assert!(!resolved.is_unresolved_spotify());
    }
}
