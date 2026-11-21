use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::queue_timeline::SplitMix64;

pub const WINDOW_AHEAD: usize = 25;
pub const WINDOW_LOW_WATER: usize = 12;
pub const LOW_WATER: usize = WINDOW_LOW_WATER;
pub const DEFAULT_SHUFFLE_SEED: i64 = 0x4D59_5351_5545_5545;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueWindowEntry {
    pub canonical_index: usize,
    pub cycle_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueWindowCycleSnapshot {
    pub cycle_id: u64,
    pub order: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueWindowSnapshot {
    pub canonical_size: usize,
    pub order: Vec<usize>,
    pub cursor: Option<usize>,
    pub current_canonical_index: Option<usize>,
    pub shuffle_enabled: bool,
    pub shuffle_seed: i64,
    pub cycle_id: u64,
    pub consumed_this_cycle: Vec<usize>,
    pub last_cycle_final_canonical_index: Option<usize>,
    #[serde(default)]
    pub planned_cycles: Vec<QueueWindowCycleSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueWindowError {
    IndexOutOfRange { index: usize, size: usize },
    CannotShrink { current: usize, requested: usize },
    NotStarted,
    InvalidSnapshot(&'static str),
}

impl fmt::Display for QueueWindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IndexOutOfRange { index, size } => {
                write!(f, "index {index} is outside 0..{size}")
            }
            Self::CannotShrink { current, requested } => {
                write!(f, "cannot shrink QueueWindow from {current} to {requested}")
            }
            Self::NotStarted => write!(f, "start_at must be called before jump_to"),
            Self::InvalidSnapshot(msg) => write!(f, "invalid QueueWindowSnapshot: {msg}"),
        }
    }
}

impl std::error::Error for QueueWindowError {}

/// Pure bounded logical state for the CONTEXT portion of a playback queue, supporting
/// deterministic seeded shuffle, continuation growth, and non-mutating repeat-all look-ahead.
pub struct QueueWindow {
    canonical_count: usize,
    seed: i64,
    logical_order: Vec<usize>,
    inverse_positions: Vec<Option<usize>>,
    logical_cursor: Option<usize>,
    current_index: Option<usize>,
    shuffled: bool,
    active_cycle_id: u64,
    consumed_order: Vec<usize>,
    consumed_set: HashSet<usize>,
    last_cycle_final: Option<usize>,
    planned_cycles: BTreeMap<u64, Vec<usize>>,
}

impl QueueWindow {
    pub fn new(canonical_size: usize, shuffle_seed: i64) -> Self {
        let logical_order: Vec<usize> = (0..canonical_size).collect();
        let inverse_positions: Vec<Option<usize>> = (0..canonical_size).map(Some).collect();
        Self {
            canonical_count: canonical_size,
            seed: shuffle_seed,
            logical_order,
            inverse_positions,
            logical_cursor: None,
            current_index: None,
            shuffled: false,
            active_cycle_id: 0,
            consumed_order: Vec::new(),
            consumed_set: HashSet::new(),
            last_cycle_final: None,
            planned_cycles: BTreeMap::new(),
        }
    }

    pub fn canonical_size(&self) -> usize {
        self.canonical_count
    }

    pub fn order(&self) -> &[usize] {
        &self.logical_order
    }

    pub fn pos_of(&self) -> &[Option<usize>] {
        &self.inverse_positions
    }

    pub fn cursor(&self) -> Option<usize> {
        self.logical_cursor
    }

    pub fn current_canonical_index(&self) -> Option<usize> {
        self.current_index
    }

    pub fn cycle_id(&self) -> u64 {
        self.active_cycle_id
    }

    pub fn shuffle_seed(&self) -> i64 {
        self.seed
    }

    pub fn is_shuffle_enabled(&self) -> bool {
        self.shuffled
    }

    pub fn current(&self) -> Option<QueueWindowEntry> {
        self.current_index.map(|canonical_index| QueueWindowEntry {
            canonical_index,
            cycle_id: self.active_cycle_id,
        })
    }

    pub fn start_at(
        &mut self,
        canonical_index: usize,
        shuffle: bool,
    ) -> Result<&mut Self, QueueWindowError> {
        if canonical_index >= self.canonical_count {
            return Err(QueueWindowError::IndexOutOfRange {
                index: canonical_index,
                size: self.canonical_count,
            });
        }
        self.shuffled = shuffle;
        self.active_cycle_id = 0;
        self.current_index = Some(canonical_index);
        self.consumed_order.clear();
        self.consumed_set.clear();
        self.last_cycle_final = None;
        self.planned_cycles.clear();
        self.logical_order = if shuffle {
            self.shuffled_order_anchored_at(canonical_index, self.active_cycle_id)
        } else {
            (0..self.canonical_count).collect()
        };
        self.rebuild_inverse();
        self.logical_cursor = self.inverse_positions[canonical_index];
        self.mark_consumed(canonical_index);
        Ok(self)
    }

    pub fn restore(snapshot: &QueueWindowSnapshot) -> Result<Self, QueueWindowError> {
        if snapshot.order.len() != snapshot.canonical_size {
            return Err(QueueWindowError::InvalidSnapshot("order size mismatch"));
        }
        validate_permutation(&snapshot.order, snapshot.canonical_size)?;
        if snapshot.cursor.is_some() != snapshot.current_canonical_index.is_some() {
            return Err(QueueWindowError::InvalidSnapshot(
                "cursor and current_canonical_index disagree",
            ));
        }

        let mut restored = Self::new(snapshot.canonical_size, snapshot.shuffle_seed);
        restored.logical_order = snapshot.order.clone();
        restored.rebuild_inverse();
        restored.logical_cursor = snapshot.cursor;
        restored.current_index = snapshot.current_canonical_index;
        restored.shuffled = snapshot.shuffle_enabled;
        restored.active_cycle_id = snapshot.cycle_id;
        restored.last_cycle_final = snapshot.last_cycle_final_canonical_index;

        if let (Some(cur_pos), Some(cur_idx)) = (restored.logical_cursor, restored.current_index) {
            if cur_pos >= restored.logical_order.len() || restored.logical_order[cur_pos] != cur_idx
            {
                return Err(QueueWindowError::InvalidSnapshot(
                    "cursor does not point at current_canonical_index",
                ));
            }
        }
        if let Some(last) = restored.last_cycle_final
            && last >= snapshot.canonical_size
        {
            return Err(QueueWindowError::InvalidSnapshot(
                "last_cycle_final out of range",
            ));
        }

        for &idx in &snapshot.consumed_this_cycle {
            if idx >= snapshot.canonical_size {
                return Err(QueueWindowError::InvalidSnapshot(
                    "consumed index out of range",
                ));
            }
            if !restored.consumed_set.insert(idx) {
                return Err(QueueWindowError::InvalidSnapshot(
                    "duplicate consumed index",
                ));
            }
            restored.consumed_order.push(idx);
        }

        if let Some(cur) = restored.current_index {
            if !restored.consumed_set.contains(&cur) {
                return Err(QueueWindowError::InvalidSnapshot(
                    "current item must be consumed",
                ));
            }
        } else if !restored.consumed_set.is_empty() {
            return Err(QueueWindowError::InvalidSnapshot(
                "unstarted snapshot cannot have consumed items",
            ));
        }

        if !restored.shuffled
            && restored
                .logical_order
                .iter()
                .enumerate()
                .any(|(pos, &v)| pos != v)
        {
            return Err(QueueWindowError::InvalidSnapshot(
                "shuffle-off snapshot must use canonical order",
            ));
        }
        if restored.shuffled
            && let Some(cur_pos) = restored.logical_cursor
            && (0..cur_pos).any(|p| !restored.consumed_set.contains(&restored.logical_order[p]))
        {
            return Err(QueueWindowError::InvalidSnapshot(
                "shuffled snapshot left unconsumed tracks behind cursor",
            ));
        }

        let mut expected_cycle = snapshot.cycle_id;
        for planned in &snapshot.planned_cycles {
            if expected_cycle == u64::MAX || planned.cycle_id != expected_cycle + 1 {
                return Err(QueueWindowError::InvalidSnapshot(
                    "planned cycles must be consecutive",
                ));
            }
            validate_permutation(&planned.order, snapshot.canonical_size)?;
            if !restored.shuffled && planned.order.iter().enumerate().any(|(pos, &v)| pos != v) {
                return Err(QueueWindowError::InvalidSnapshot(
                    "shuffle-off planned cycle must use canonical order",
                ));
            }
            restored
                .planned_cycles
                .insert(planned.cycle_id, planned.order.clone());
            expected_cycle = planned.cycle_id;
        }
        Ok(restored)
    }

    pub fn extend_to(&mut self, new_canonical_size: usize) -> Result<&mut Self, QueueWindowError> {
        if new_canonical_size < self.canonical_count {
            return Err(QueueWindowError::CannotShrink {
                current: self.canonical_count,
                requested: new_canonical_size,
            });
        }
        if new_canonical_size == self.canonical_count {
            return Ok(self);
        }
        let first_new = self.canonical_count;
        self.canonical_count = new_canonical_size;
        let fresh_active = self.fresh_order(first_new, self.active_cycle_id);
        self.logical_order.extend(fresh_active);

        let planned_keys: Vec<u64> = self.planned_cycles.keys().copied().collect();
        for cycle in planned_keys {
            let fresh = self.fresh_order(first_new, cycle);
            if let Some(order) = self.planned_cycles.get_mut(&cycle) {
                order.extend(fresh);
            }
        }
        self.rebuild_inverse();
        Ok(self)
    }

    pub fn slice(
        &mut self,
        from_pos: usize,
        count: usize,
        repeat_all: bool,
    ) -> Result<Vec<QueueWindowEntry>, QueueWindowError> {
        if from_pos > self.logical_order.len() {
            return Err(QueueWindowError::IndexOutOfRange {
                index: from_pos,
                size: self.logical_order.len() + 1,
            });
        }
        if count == 0 || self.logical_order.is_empty() {
            return Ok(Vec::new());
        }

        let mut result = Vec::with_capacity(count);
        let mut cycle = self.active_cycle_id;
        let mut position = from_pos;

        while result.len() < count {
            let len = if cycle == self.active_cycle_id {
                self.logical_order.len()
            } else {
                self.planned_order(cycle).len()
            };
            if len == 0 {
                break;
            }
            if position == len {
                if !repeat_all {
                    break;
                }
                cycle = cycle.saturating_add(1);
                let _ = self.planned_order(cycle);
                position = 0;
                continue;
            }
            let canonical_index = if cycle == self.active_cycle_id {
                self.logical_order[position]
            } else {
                self.planned_order(cycle)[position]
            };
            result.push(QueueWindowEntry {
                canonical_index,
                cycle_id: cycle,
            });
            position += 1;
        }
        Ok(result)
    }

    pub fn upcoming(&mut self, count: usize, repeat_all: bool) -> Vec<QueueWindowEntry> {
        let bounded = count.min(WINDOW_AHEAD);
        let Some(cur_pos) = self.logical_cursor else {
            return Vec::new();
        };
        if bounded == 0 || self.logical_order.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(bounded);
        let mut cycle = self.active_cycle_id;
        let mut position = cur_pos + 1;

        while result.len() < bounded {
            let order_len = if cycle == self.active_cycle_id {
                self.logical_order.len()
            } else {
                self.planned_order(cycle).len()
            };
            if order_len == 0 {
                break;
            }
            while position < order_len && result.len() < bounded {
                let canonical = if cycle == self.active_cycle_id {
                    self.logical_order[position]
                } else {
                    self.planned_order(cycle)[position]
                };
                if cycle != self.active_cycle_id
                    || !self.shuffled
                    || !self.consumed_set.contains(&canonical)
                {
                    result.push(QueueWindowEntry {
                        canonical_index: canonical,
                        cycle_id: cycle,
                    });
                }
                position += 1;
            }
            if result.len() >= bounded || !repeat_all {
                break;
            }
            cycle = cycle.saturating_add(1);
            let _ = self.planned_order(cycle);
            position = 0;
        }
        result
    }

    pub fn advance_automatically(&mut self, repeat_all: bool) -> Option<QueueWindowEntry> {
        let cur_pos = self.logical_cursor?;
        if self.logical_order.is_empty() {
            return None;
        }
        if self.shuffled {
            for pos in (cur_pos + 1)..self.logical_order.len() {
                let candidate = self.logical_order[pos];
                if !self.consumed_set.contains(&candidate) {
                    return Some(self.move_within_cycle(pos));
                }
            }
        } else if cur_pos + 1 < self.logical_order.len() {
            return Some(self.move_within_cycle(cur_pos + 1));
        }

        if !repeat_all {
            return None;
        }
        Some(self.transition_to_next_cycle())
    }

    pub fn next(&mut self, repeat_all: bool) -> Option<QueueWindowEntry> {
        let cur_pos = self.logical_cursor?;
        if self.logical_order.is_empty() {
            return None;
        }
        if cur_pos + 1 < self.logical_order.len() {
            return Some(self.move_within_cycle(cur_pos + 1));
        }
        if !repeat_all {
            return None;
        }
        Some(self.transition_to_next_cycle())
    }

    pub fn previous(&mut self) -> Option<QueueWindowEntry> {
        let cur_pos = self.logical_cursor?;
        if cur_pos == 0 {
            return None;
        }
        Some(self.move_within_cycle(cur_pos - 1))
    }

    pub fn jump_to(
        &mut self,
        canonical_index: usize,
    ) -> Result<QueueWindowEntry, QueueWindowError> {
        if canonical_index >= self.canonical_count {
            return Err(QueueWindowError::IndexOutOfRange {
                index: canonical_index,
                size: self.canonical_count,
            });
        }
        if self.current_index.is_none() {
            return Err(QueueWindowError::NotStarted);
        }
        let pos = self.inverse_positions[canonical_index].ok_or(
            QueueWindowError::InvalidSnapshot("canonical index absent from order"),
        )?;
        Ok(self.move_within_cycle(pos))
    }

    pub fn set_shuffle(&mut self, enabled: bool, shuffle_seed: Option<i64>) -> &mut Self {
        if enabled == self.shuffled {
            return self;
        }
        self.shuffled = enabled;
        if enabled && let Some(s) = shuffle_seed {
            self.seed = s;
        }
        self.planned_cycles.clear();
        let Some(current) = self.current_index else {
            self.logical_order = (0..self.canonical_count).collect();
            self.rebuild_inverse();
            return self;
        };
        self.logical_order = if enabled {
            self.shuffled_order_anchored_at(current, self.active_cycle_id)
        } else {
            (0..self.canonical_count).collect()
        };
        self.rebuild_inverse();
        self.logical_cursor = self.inverse_positions[current];
        self.mark_consumed(current);
        self
    }

    pub fn commit(&mut self, entry: QueueWindowEntry) -> bool {
        if self.current_index.is_none() || entry.canonical_index >= self.canonical_count {
            return false;
        }
        if entry.cycle_id == self.active_cycle_id {
            let Some(pos) = self.inverse_positions[entry.canonical_index] else {
                return false;
            };
            self.move_within_cycle(pos);
            return true;
        }
        if self.active_cycle_id == u64::MAX || entry.cycle_id != self.active_cycle_id + 1 {
            return false;
        }
        let next_order = self.planned_order(entry.cycle_id).to_vec();
        let Some(pos) = next_order.iter().position(|&c| c == entry.canonical_index) else {
            return false;
        };
        self.select_planned_cycle(entry.cycle_id, next_order, pos);
        true
    }

    pub fn snapshot(&self) -> QueueWindowSnapshot {
        QueueWindowSnapshot {
            canonical_size: self.canonical_count,
            order: self.logical_order.clone(),
            cursor: self.logical_cursor,
            current_canonical_index: self.current_index,
            shuffle_enabled: self.shuffled,
            shuffle_seed: self.seed,
            cycle_id: self.active_cycle_id,
            consumed_this_cycle: self.consumed_order.clone(),
            last_cycle_final_canonical_index: self.last_cycle_final,
            planned_cycles: self
                .planned_cycles
                .iter()
                .map(|(&cycle_id, order)| QueueWindowCycleSnapshot {
                    cycle_id,
                    order: order.clone(),
                })
                .collect(),
        }
    }

    fn mark_consumed(&mut self, canonical: usize) {
        if self.consumed_set.insert(canonical) {
            self.consumed_order.push(canonical);
        }
    }

    fn move_within_cycle(&mut self, position: usize) -> QueueWindowEntry {
        let prev_boundary = self.boundary_final();
        if self.shuffled
            && let Some(cur_pos) = self.logical_cursor
            && position > cur_pos
        {
            for skipped in (cur_pos + 1)..=position {
                let c = self.logical_order[skipped];
                self.mark_consumed(c);
            }
        }
        self.logical_cursor = Some(position);
        let canonical = self.logical_order[position];
        self.current_index = Some(canonical);
        self.mark_consumed(canonical);
        if self.boundary_final() != prev_boundary {
            self.planned_cycles.clear();
        }
        QueueWindowEntry {
            canonical_index: canonical,
            cycle_id: self.active_cycle_id,
        }
    }

    fn transition_to_next_cycle(&mut self) -> QueueWindowEntry {
        let next_cycle = self.active_cycle_id.saturating_add(1);
        let next_order = self.planned_order(next_cycle).to_vec();
        self.select_planned_cycle(next_cycle, next_order, 0);
        // INFALLIBLE: `select_planned_cycle` sets `current_index` from non-empty `next_order`.
        self.current().expect("non-empty next cycle")
    }

    fn select_planned_cycle(&mut self, cycle: u64, permutation: Vec<usize>, position: usize) {
        self.last_cycle_final = self.current_index;
        self.active_cycle_id = cycle;
        self.logical_order = permutation;
        self.planned_cycles.remove(&cycle);
        self.rebuild_inverse();
        self.logical_cursor = Some(position);
        self.current_index = Some(self.logical_order[position]);
        self.consumed_order.clear();
        self.consumed_set.clear();
        for p in 0..=position {
            let c = self.logical_order[p];
            self.mark_consumed(c);
        }
    }

    fn planned_order(&mut self, cycle: u64) -> &[usize] {
        if !self.planned_cycles.contains_key(&cycle) {
            let prev_cycle = cycle - 1;
            let prev_final = if prev_cycle == self.active_cycle_id {
                self.boundary_final()
            } else {
                self.planned_order(prev_cycle).last().copied()
            };
            let order = self.order_for_cycle(cycle, prev_final);
            self.planned_cycles.insert(cycle, order);
        }
        // INFALLIBLE: inserted immediately above if absent.
        &self.planned_cycles[&cycle]
    }

    fn boundary_final(&self) -> Option<usize> {
        let cur_pos = self.logical_cursor?;
        for pos in ((cur_pos + 1)..self.logical_order.len()).rev() {
            let canonical = self.logical_order[pos];
            if !self.shuffled || !self.consumed_set.contains(&canonical) {
                return Some(canonical);
            }
        }
        self.current_index
    }

    fn fresh_order(&self, first_new: usize, cycle: u64) -> Vec<usize> {
        let mut fresh: Vec<usize> = (first_new..self.canonical_count).collect();
        if self.shuffled && self.current_index.is_some() {
            let mut rng = SplitMix64::new(self.seed_for_extension(cycle, first_new));
            rng.shuffle(&mut fresh);
        }
        fresh
    }

    fn shuffled_order_anchored_at(&self, anchor: usize, cycle: u64) -> Vec<usize> {
        let mut rest: Vec<usize> = (0..self.canonical_count).filter(|&i| i != anchor).collect();
        let mut rng = SplitMix64::new(self.seed_for_cycle(cycle));
        rng.shuffle(&mut rest);
        let mut out = Vec::with_capacity(self.canonical_count);
        out.push(anchor);
        out.extend(rest);
        out
    }

    fn order_for_cycle(&self, cycle: u64, previous_final: Option<usize>) -> Vec<usize> {
        if !self.shuffled {
            return (0..self.canonical_count).collect();
        }
        let mut result: Vec<usize> = (0..self.canonical_count).collect();
        let mut rng = SplitMix64::new(self.seed_for_cycle(cycle));
        rng.shuffle(&mut result);
        if self.canonical_count >= 2
            && let Some(prev) = previous_final
            && result.first().copied() == Some(prev)
            && let Some(swap_with) = result.iter().position(|&x| x != prev)
        {
            result.swap(0, swap_with);
        }
        result
    }

    fn seed_for_cycle(&self, cycle: u64) -> u64 {
        (self.seed as u64) ^ cycle.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    fn seed_for_extension(&self, cycle: u64, first_new: usize) -> u64 {
        (self.seed as u64)
            ^ cycle.wrapping_mul(0xD1B5_4A32_D192_ED03)
            ^ (self.canonical_count as u64)
            ^ ((first_new as u64) << 32)
    }

    fn rebuild_inverse(&mut self) {
        self.inverse_positions = vec![None; self.canonical_count];
        for (pos, &canonical) in self.logical_order.iter().enumerate() {
            self.inverse_positions[canonical] = Some(pos);
        }
    }
}

fn validate_permutation(permutation: &[usize], size: usize) -> Result<(), QueueWindowError> {
    if permutation.len() != size {
        return Err(QueueWindowError::InvalidSnapshot(
            "permutation size mismatch",
        ));
    }
    let mut seen = vec![false; size];
    for &canonical in permutation {
        if canonical >= size {
            return Err(QueueWindowError::InvalidSnapshot(
                "canonical index out of range",
            ));
        }
        if seen[canonical] {
            return Err(QueueWindowError::InvalidSnapshot(
                "duplicate canonical index in permutation",
            ));
        }
        seen[canonical] = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_start_and_deterministic_shuffle() {
        let mut w = QueueWindow::new(6, DEFAULT_SHUFFLE_SEED);
        w.start_at(2, false).unwrap();
        assert_eq!(&[0, 1, 2, 3, 4, 5], w.order());
        assert_eq!(Some(2), w.cursor());
        assert_eq!(
            vec![3, 4, 5],
            w.upcoming(WINDOW_AHEAD, false)
                .iter()
                .map(|e| e.canonical_index)
                .collect::<Vec<_>>()
        );

        let mut a = QueueWindow::new(20, 1234);
        let mut b = QueueWindow::new(20, 1234);
        a.start_at(7, true).unwrap();
        b.start_at(7, true).unwrap();
        assert_eq!(7, a.order()[0]);
        assert_eq!(a.order(), b.order());
    }

    #[test]
    fn shuffle_traversal_repeat_all_boundary_and_planned_commit() {
        let mut w = QueueWindow::new(100, 9);
        w.start_at(37, true).unwrap();
        let mut first_cycle = vec![w.current_canonical_index().unwrap()];
        while let Some(next) = w.advance_automatically(false) {
            first_cycle.push(next.canonical_index);
        }
        assert_eq!(100, first_cycle.len());
        assert_eq!(
            100,
            first_cycle.iter().copied().collect::<HashSet<_>>().len()
        );
        assert_eq!(0, w.cycle_id());

        let final_track = *first_cycle.last().unwrap();
        let next_cycle = w.advance_automatically(true).unwrap();
        assert_eq!(1, next_cycle.cycle_id);
        assert_ne!(final_track, next_cycle.canonical_index);

        // Planned slice does not mutate active cycle until commit.
        let mut w2 = QueueWindow::new(4, 5);
        w2.start_at(0, true).unwrap();
        for _ in 0..3 {
            assert!(w2.advance_automatically(false).is_some());
        }
        let before = w2.snapshot();
        let look = w2.slice(w2.cursor().unwrap() + 1, 3, true).unwrap();
        assert!(look.iter().any(|e| e.cycle_id == 1));
        assert_eq!(before.cycle_id, w2.cycle_id());

        let boundary = *look.iter().find(|e| e.cycle_id == 1).unwrap();
        assert!(w2.commit(boundary));
        assert_eq!(1, w2.cycle_id());
        assert_eq!(Some(boundary.canonical_index), w2.current_canonical_index());
    }

    #[test]
    fn extension_navigation_shuffle_toggle_and_snapshot_restore() {
        let mut w = QueueWindow::new(8, 2);
        w.start_at(3, true).unwrap();
        let old_order = w.order().to_vec();
        let old_cursor = w.cursor();
        w.extend_to(13).unwrap();
        assert_eq!(old_order, w.order()[..8]);
        assert_eq!(old_cursor, w.cursor());

        // Explicit previous/next preserves consumed set.
        let mut nav = QueueWindow::new(6, 1);
        nav.start_at(0, true).unwrap();
        let first = nav.advance_automatically(false).unwrap();
        let second = nav.advance_automatically(false).unwrap();
        nav.previous();
        assert_eq!(Some(first.canonical_index), nav.current_canonical_index());
        let up: Vec<usize> = nav
            .upcoming(25, false)
            .iter()
            .map(|e| e.canonical_index)
            .collect();
        assert!(!up.contains(&first.canonical_index));
        assert!(!up.contains(&second.canonical_index));
        assert_eq!(
            second.canonical_index,
            nav.next(false).unwrap().canonical_index
        );

        // Snapshot round-trip.
        let mut orig = QueueWindow::new(30, 17);
        orig.start_at(11, true).unwrap();
        orig.advance_automatically(false);
        orig.jump_to(20).unwrap();
        let mut restored = QueueWindow::restore(&orig.snapshot()).unwrap();
        assert_eq!(orig.snapshot(), restored.snapshot());
        assert_eq!(orig.upcoming(25, false), restored.upcoming(25, false));
        assert_eq!(
            orig.advance_automatically(false),
            restored.advance_automatically(false)
        );
    }
}
