use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// One step of a playlist reorder: put `set_video_id` directly before `before`,
/// or at the end of the playlist when `before` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistMove {
    pub set_video_id: String,
    pub before: Option<String>,
}

/// Computes the minimal sequence of `ACTION_MOVE_VIDEO_BEFORE` moves to turn
/// `current` into `target` (both lists of unique `set_video_id`s).
///
/// Returns an empty vector when unchanged or when the two slices do not hold
/// the same set of unique entries.
pub fn playlist_moves(current: &[String], target: &[String]) -> Vec<PlaylistMove> {
    if current == target || current.len() != target.len() {
        return Vec::new();
    }
    let current_set: HashSet<&str> = current.iter().map(String::as_str).collect();
    if current_set.len() != current.len() {
        return Vec::new();
    }
    let target_set: HashSet<&str> = target.iter().map(String::as_str).collect();
    if current_set != target_set {
        return Vec::new();
    }

    let position: HashMap<&str, usize> = current
        .iter()
        .enumerate()
        .map(|(idx, id)| (id.as_str(), idx))
        .collect();
    let mapped: Vec<usize> = target.iter().map(|id| position[id.as_str()]).collect();
    let stay: HashSet<&str> = longest_increasing_run(&mapped)
        .into_iter()
        .map(|idx| target[idx].as_str())
        .collect();

    let mut moves = Vec::new();
    for index in (0..target.len()).rev() {
        let id = &target[index];
        if stay.contains(id.as_str()) {
            continue;
        }
        moves.push(PlaylistMove {
            set_video_id: id.clone(),
            before: target.get(index + 1).cloned(),
        });
    }
    moves
}

/// Indices into `values` of one longest strictly increasing subsequence.
fn longest_increasing_run(values: &[usize]) -> Vec<usize> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut tails = vec![0usize; values.len()];
    let mut previous = vec![None::<usize>; values.len()];
    let mut length = 0usize;

    for (i, &val) in values.iter().enumerate() {
        let mut low = 0usize;
        let mut high = length;
        while low < high {
            let mid = low + (high - low) / 2;
            if values[tails[mid]] < val {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if low > 0 {
            previous[i] = Some(tails[low - 1]);
        }
        tails[low] = i;
        if low == length {
            length += 1;
        }
    }

    let mut run = Vec::with_capacity(length);
    let mut at = Some(tails[length - 1]);
    while let Some(idx) = at {
        run.push(idx);
        at = previous[idx];
    }
    run.reverse();
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).to_owned()).collect()
    }

    fn apply(start: &[String], moves: &[PlaylistMove]) -> Vec<String> {
        let mut list = start.to_vec();
        for mv in moves {
            if let Some(pos) = list.iter().position(|x| x == &mv.set_video_id) {
                list.remove(pos);
            }
            let at = mv
                .before
                .as_deref()
                .and_then(|b| list.iter().position(|x| x == b))
                .unwrap_or(list.len());
            list.insert(at, mv.set_video_id.clone());
        }
        list
    }

    #[test]
    fn unchanged_order_sends_nothing() {
        let items = s(&["a", "b", "c"]);
        assert!(playlist_moves(&items, &items).is_empty());
    }

    #[test]
    fn one_track_dragged_up_is_one_move() {
        let current = s(&["a", "b", "c", "d"]);
        let target = s(&["d", "a", "b", "c"]);
        let moves = playlist_moves(&current, &target);
        assert_eq!(
            vec![PlaylistMove {
                set_video_id: "d".to_owned(),
                before: Some("a".to_owned()),
            }],
            moves
        );
        assert_eq!(target, apply(&current, &moves));
    }

    #[test]
    fn one_track_dragged_to_the_end_moves_to_the_end() {
        let current = s(&["a", "b", "c", "d"]);
        let target = s(&["b", "c", "d", "a"]);
        let moves = playlist_moves(&current, &target);
        assert_eq!(
            vec![PlaylistMove {
                set_video_id: "a".to_owned(),
                before: None,
            }],
            moves
        );
        assert_eq!(target, apply(&current, &moves));
    }

    #[test]
    fn any_rearrangement_replays_to_the_target() {
        let current: Vec<String> = (1..=30).map(|i| format!("e{i}")).collect();
        // Deterministic LCG shuffle (no external rand dep needed).
        let mut state: u64 = 7;
        for _ in 0..50 {
            let mut target = current.clone();
            for i in (1..target.len()).rev() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = ((state >> 33) as usize) % (i + 1);
                target.swap(i, j);
            }
            let moves = playlist_moves(&current, &target);
            assert_eq!(target, apply(&current, &moves));
        }
    }

    #[test]
    fn reversing_moves_all_but_one() {
        let current = s(&["a", "b", "c", "d", "e"]);
        let mut target = current.clone();
        target.reverse();
        let moves = playlist_moves(&current, &target);
        assert_eq!(current.len() - 1, moves.len());
        assert_eq!(target, apply(&current, &moves));
    }

    #[test]
    fn lists_that_no_longer_match_are_left_alone() {
        assert!(playlist_moves(&s(&["a", "b"]), &s(&["b", "a", "c"])).is_empty());
        assert!(playlist_moves(&s(&["a", "b"]), &s(&["b", "c"])).is_empty());
        assert!(playlist_moves(&s(&["a", "a"]), &s(&["a", "a"])).is_empty());
    }
}
