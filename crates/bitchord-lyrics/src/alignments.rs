use std::collections::HashMap;

use crate::line::LyricAlignment;

/// `ttm:agent` types, as both TTML and LyricsPlus name them.
const PERSON: &str = "person";
const GROUP: &str = "group";
const OTHER: &str = "other";

/// Apple's two reserved voices: everyone at once, and the other singer.
const GROUP_AGENT: &str = "v1000";
const OTHER_AGENT: &str = "v2000";

/// Past this share of lines on the right, the whole song is flipped.
/// Not 100%: a chorus or two sung by the group lands on the left and would
/// otherwise be enough to call it a genuine duet.
const MOSTLY_RIGHT: f32 = 0.85;

/// Decides which side of the panel each line is sung from, given the voice
/// identifier for each line and an optional `agent_id -> agent_type` map
/// (`"person"`, `"group"`, or `"other"`).
///
/// Sides alternate every time the voice changes (rather than one fixed side
/// per voice), keeping multi-voice tracks conversational. A `"group"` line
/// stays on [`LyricAlignment::Start`] without disturbing whose turn it is.
/// If 85% or more of placed lines land on [`LyricAlignment::End`], the whole
/// track is flipped so a solo voice declared `"other"` does not hug the right
/// margin.
pub fn line_alignments<S: AsRef<str>>(
    singers: &[Option<S>],
    types: &HashMap<String, String>,
) -> Vec<LyricAlignment> {
    let mut left = true;
    let mut last_voice: Option<&str> = None;
    let mut rightward = 0usize;
    let mut placed = 0usize;

    let mut sides = Vec::with_capacity(singers.len());
    for singer in singers {
        let Some(singer) = singer.as_ref().map(AsRef::as_ref).filter(|s| !s.is_empty()) else {
            sides.push(LyricAlignment::Start);
            continue;
        };
        // Apple's two reserved ids carry no declaration of their own.
        let agent_type = types
            .get(singer)
            .map(String::as_str)
            .unwrap_or(match singer {
                GROUP_AGENT => GROUP,
                OTHER_AGENT => OTHER,
                _ => PERSON,
            });
        placed += 1;
        if agent_type == GROUP {
            sides.push(LyricAlignment::Start);
            continue;
        }

        match last_voice {
            None => left = agent_type != OTHER,
            Some(prev) if prev != singer => left = !left,
            Some(_) => {}
        }
        last_voice = Some(singer);

        if !left {
            rightward += 1;
        }
        sides.push(if left {
            LyricAlignment::Start
        } else {
            LyricAlignment::End
        });
    }

    if placed == 0 || (rightward as f32 / placed as f32) < MOSTLY_RIGHT {
        return sides;
    }
    sides
        .into_iter()
        .map(|side| match side {
            LyricAlignment::Start => LyricAlignment::End,
            LyricAlignment::End => LyricAlignment::Start,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternates_sides_on_voice_change_and_keeps_group_left() {
        let mut types = HashMap::new();
        types.insert("v1".to_owned(), "person".to_owned());
        types.insert("v2".to_owned(), "person".to_owned());
        types.insert("v1000".to_owned(), "group".to_owned());

        let singers = vec![
            Some("v1"),
            Some("v2"),
            Some("v2"),
            Some("v1"),
            Some("v1000"),
        ];
        assert_eq!(
            vec![
                LyricAlignment::Start,
                LyricAlignment::End,
                LyricAlignment::End,
                LyricAlignment::Start,
                LyricAlignment::Start,
            ],
            line_alignments(&singers, &types)
        );
    }

    #[test]
    fn flips_song_when_mostly_right() {
        let mut types = HashMap::new();
        types.insert("v2".to_owned(), "other".to_owned());
        let singers = vec![Some("v2"), Some("v2")];
        assert_eq!(
            vec![LyricAlignment::Start, LyricAlignment::Start],
            line_alignments(&singers, &types)
        );
    }

    #[test]
    fn reserved_apple_agents_work_without_metadata_declarations() {
        let types = HashMap::new();
        let singers = vec![Some("v1"), Some("v2000"), Some("v1000")];
        assert_eq!(
            vec![
                LyricAlignment::Start,
                LyricAlignment::End,
                LyricAlignment::Start,
            ],
            line_alignments(&singers, &types)
        );
    }
}
