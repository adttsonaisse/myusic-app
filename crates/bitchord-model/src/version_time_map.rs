use std::fmt;

use serde::{Deserialize, Serialize};

use crate::models::Song;

/// A stretch both cuts share, `duration_ms` long in each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub from_start_ms: i64,
    pub to_start_ms: i64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeMapError {
    EmptySegments,
}

impl fmt::Display for TimeMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySegments => write!(f, "a time map needs at least one segment"),
        }
    }
}

impl std::error::Error for TimeMapError {}

/// Maps timestamps between a music video and its paired audio track using YouTube's `segmentMap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionTimeMap {
    segments: Vec<Segment>,
}

impl VersionTimeMap {
    pub fn new(mut segments: Vec<Segment>) -> Result<Self, TimeMapError> {
        if segments.is_empty() {
            return Err(TimeMapError::EmptySegments);
        }
        segments.sort_by_key(|s| s.from_start_ms);
        Ok(Self { segments })
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Maps `position_ms` in the playing cut to the corresponding instant in the paired cut.
    pub fn map(&self, position_ms: i64) -> i64 {
        for segment in &self.segments {
            if position_ms < segment.from_start_ms {
                return segment.to_start_ms;
            }
            let into = position_ms - segment.from_start_ms;
            if into < segment.duration_ms {
                return segment.to_start_ms + into;
            }
        }
        // INFALLIBLE: constructor guarantees `self.segments` is non-empty.
        let last = &self.segments[self.segments.len() - 1];
        last.to_start_ms + last.duration_ms
    }

    /// Returns the same pairing read from the opposite direction.
    pub fn inverted(&self) -> Self {
        let mut flipped: Vec<Segment> = self
            .segments
            .iter()
            .map(|s| Segment {
                from_start_ms: s.to_start_ms,
                to_start_ms: s.from_start_ms,
                duration_ms: s.duration_ms,
            })
            .collect();
        flipped.sort_by_key(|s| s.from_start_ms);
        Self { segments: flipped }
    }
}

/// The paired counterpart (video or audio) of a track, with optional timestamp alignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionPair {
    pub song: Song,
    pub time_map: Option<VersionTimeMap>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_segments() {
        assert_eq!(
            Err(TimeMapError::EmptySegments),
            VersionTimeMap::new(vec![])
        );
    }

    #[test]
    fn maps_before_inside_between_and_after_segments() {
        let map = VersionTimeMap::new(vec![
            Segment {
                from_start_ms: 40_000,
                to_start_ms: 20_000,
                duration_ms: 30_000,
            },
            Segment {
                from_start_ms: 10_000,
                to_start_ms: 0,
                duration_ms: 20_000,
            },
        ])
        .expect("non-empty segments");

        // Spoken intro before first stretch lands at start of audio.
        assert_eq!(0, map.map(5_000));
        // Inside first stretch is exact.
        assert_eq!(5_000, map.map(15_000));
        // Skit between stretches lands where the music resumes.
        assert_eq!(20_000, map.map(35_000));
        // Inside second stretch is exact.
        assert_eq!(30_000, map.map(50_000));
        // Past last stretch clamps to its end.
        assert_eq!(50_000, map.map(99_000));

        let inv = map.inverted();
        assert_eq!(15_000, inv.map(5_000));
        assert_eq!(50_000, inv.map(30_000));
    }
}
