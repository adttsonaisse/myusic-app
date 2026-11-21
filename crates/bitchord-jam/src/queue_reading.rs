use crate::protocol::{PartyPlayback, PartyQueue, PartyTrack};

/// Locates where `video_id` stands in a party queue that preserves played history.
///
/// Trusts `playback.queue_index` when `playback.queue_seq == queue.seq` (otherwise `queue.index`),
/// then picks the copy nearest that hint, preferring slots ahead of the needle over behind it.
pub fn party_queue_index_of(
    queue: &PartyQueue,
    playback: &PartyPlayback,
    video_id: Option<&str>,
) -> Option<usize> {
    let video_id = video_id?;
    let items = &queue.items;
    let hint = if playback.queue_seq == queue.seq {
        playback.queue_index
    } else {
        queue.index
    };
    if let Ok(hint_idx) = usize::try_from(hint)
        && items
            .get(hint_idx)
            .is_some_and(|item| item.video_id == video_id)
    {
        return Some(hint_idx);
    }

    let mut best: Option<usize> = None;
    let mut best_distance = i64::MAX;
    let hint_i64 = i64::from(hint);
    for (i, item) in items.iter().enumerate() {
        if item.video_id != video_id {
            continue;
        }
        let idx = i as i64;
        let distance = if idx >= hint_i64 {
            idx - hint_i64
        } else {
            (hint_i64 - idx) * 2 + 1
        };
        if distance < best_distance {
            best = Some(i);
            best_distance = distance;
        }
    }
    best
}

/// Returns the tracks queued after `video_id`, or an empty slice if `video_id` is absent.
pub fn party_upcoming_after<'a>(
    queue: &'a PartyQueue,
    playback: &PartyPlayback,
    video_id: Option<&str>,
) -> &'a [PartyTrack] {
    match party_queue_index_of(queue, playback, video_id) {
        Some(at) => &queue.items[(at + 1)..],
        None => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(ids: &[&str], seq: i64, index: i32) -> PartyQueue {
        PartyQueue {
            seq,
            index,
            items: ids.iter().map(|id| PartyTrack::with_id(*id)).collect(),
        }
    }

    #[test]
    fn state_frames_index_wins_when_it_names_the_track() {
        let q = queue(&["x", "a", "x", "b"], 4, 0);
        let playback = PartyPlayback {
            queue_seq: 4,
            queue_index: 2,
            ..PartyPlayback::default()
        };
        assert_eq!(Some(2), party_queue_index_of(&q, &playback, Some("x")));
        let upcoming: Vec<&str> = party_upcoming_after(&q, &playback, Some("x"))
            .iter()
            .map(|t| t.video_id.as_str())
            .collect();
        assert_eq!(vec!["b"], upcoming);
    }

    #[test]
    fn stale_queue_index_is_not_taken_from_an_older_queue() {
        let q = queue(&["x", "a", "x", "b"], 3, 2);
        let playback = PartyPlayback {
            queue_seq: 4,
            queue_index: 0,
            ..PartyPlayback::default()
        };
        assert_eq!(Some(2), party_queue_index_of(&q, &playback, Some("x")));
    }

    #[test]
    fn copy_ahead_of_the_needle_beats_the_one_behind_it() {
        let q = queue(&["b", "a", "x", "b"], 4, 2);
        let playback = PartyPlayback {
            queue_seq: 3,
            queue_index: 2,
            ..PartyPlayback::default()
        };
        assert_eq!(Some(3), party_queue_index_of(&q, &playback, Some("b")));
    }

    #[test]
    fn missing_track_has_nothing_after_it() {
        let q = queue(&["a", "b"], 4, -1);
        let pb = PartyPlayback::default();
        assert_eq!(None, party_queue_index_of(&q, &pb, Some("z")));
        assert!(party_upcoming_after(&q, &pb, Some("z")).is_empty());
        assert_eq!(None, party_queue_index_of(&q, &pb, None));
    }
}
