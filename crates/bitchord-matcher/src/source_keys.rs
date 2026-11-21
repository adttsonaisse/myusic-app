pub const PREFIX: &str = "src:";
pub const SEPARATOR: &str = "::";

/// Parser and formatter for source-backed track ids (`src:<config_id>::<track_id>`).
pub struct SourceTrackKeys;

impl SourceTrackKeys {
    pub fn format(config_id: &str, track_id: &str) -> String {
        format!("{PREFIX}{config_id}{SEPARATOR}{track_id}")
    }

    /// Extracts `(config_id, track_id)` or returns `None` for ordinary YouTube video ids.
    pub fn parse(key: &str) -> Option<(&str, &str)> {
        let body = key.strip_prefix(PREFIX)?;
        let cut = body.find(SEPARATOR)?;
        if cut == 0 {
            return None;
        }
        Some((&body[..cut], &body[(cut + SEPARATOR.len())..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_key_round_trips_and_survives_colons() {
        let key = SourceTrackKeys::format("cfg-1", "track-42");
        assert_eq!(Some(("cfg-1", "track-42")), SourceTrackKeys::parse(&key));

        let nested = SourceTrackKeys::format("cfg-1", "al::bum::7");
        assert_eq!(
            Some(("cfg-1", "al::bum::7")),
            SourceTrackKeys::parse(&nested)
        );

        assert_eq!(None, SourceTrackKeys::parse("dQw4w9WgXcQ"));
        assert_eq!(None, SourceTrackKeys::parse(""));
        assert_eq!(None, SourceTrackKeys::parse("src:::empty-config"));
    }
}
