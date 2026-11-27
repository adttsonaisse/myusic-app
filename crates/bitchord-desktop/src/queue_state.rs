//! Bounds-safe playback queue state machine with reversible shuffle/unshuffle,
//! drag-and-drop reordering, autoplay/radio section boundary tracking,
//! session persistence snapshots, and standby-deck preload invalidation tracking.

use std::collections::HashSet;

use bitchord_model::{QueueTier, Song};
use bitchord_queue::{QueueSource, QueueTimeline};
use bitchord_storage::PlaybackSessionState;

use crate::backend::QueueViewSnapshot;

/// Result of inserting a track via `PlayNext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayNextOutcome {
    /// Queue was empty; caller should start playing this track immediately.
    StartedImmediately(Box<Song>),
    /// Track was inserted after the active item; caller should invalidate standby preload.
    InsertedNext,
}

/// Result of removing an item from the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueRemoveOutcome {
    OutOfBounds,
    /// Queue became empty; caller should stop playback.
    Emptied,
    /// The currently active item was removed; caller should start loading the replacement.
    ActiveReplaced(Box<Song>),
    /// A non-active item was removed; caller should refresh standby preload if needed.
    Updated,
}

/// Pure, bounds-safe queue controller separating playback order from audio I/O.
#[derive(Debug, Clone, Default)]
pub struct QueueController {
    items: Vec<Song>,
    original_items: Option<Vec<Song>>,
    current_index: Option<usize>,
    shuffle: bool,
    repeat_all: bool,
    repeat_one: bool,
    autoplay_start_index: Option<usize>,
}

impl QueueController {
    pub fn snapshot(&self) -> QueueViewSnapshot {
        QueueViewSnapshot {
            items: self.items.clone(),
            current_index: self.current_index,
            shuffle: self.shuffle,
            repeat_all: self.repeat_all,
            repeat_one: self.repeat_one,
            autoplay_start_index: self.autoplay_start_index,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[Song] {
        &self.items
    }

    #[allow(dead_code)]
    pub fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    #[allow(dead_code)]
    pub fn autoplay_start_index(&self) -> Option<usize> {
        self.autoplay_start_index
    }

    pub fn current_song(&self) -> Option<&Song> {
        self.current_index.and_then(|idx| self.items.get(idx))
    }

    pub fn song_at(&self, idx: usize) -> Option<&Song> {
        self.items.get(idx)
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat_all(&self) -> bool {
        self.repeat_all
    }

    pub fn repeat_one(&self) -> bool {
        self.repeat_one
    }

    pub fn repeat_mode_str(&self) -> &'static str {
        if self.repeat_one {
            "one"
        } else if self.repeat_all {
            "all"
        } else {
            "off"
        }
    }

    /// Serializes the queue + transport flags into a SQLite [`PlaybackSessionState`].
    pub fn to_session_state(&self, position_ms: u64, updated_at_ms: i64) -> PlaybackSessionState {
        PlaybackSessionState {
            queue: self.items.clone(),
            original_order: self.original_items.clone(),
            current_index: self.current_index,
            position_ms,
            shuffle: self.shuffle,
            repeat_mode: self.repeat_mode_str().to_owned(),
            autoplay_start_index: self.autoplay_start_index,
            updated_at_ms,
        }
    }

    /// Restores queue items, shuffle/repeat flags, active index, and autoplay boundary
    /// from a persisted [`PlaybackSessionState`], returning the restored active track.
    pub fn restore_session(&mut self, state: PlaybackSessionState) -> Option<Song> {
        self.items = state.queue;
        self.original_items = state.original_order;
        self.shuffle = state.shuffle;
        match state.repeat_mode.trim().to_ascii_lowercase().as_str() {
            "one" | "track" => {
                self.repeat_all = false;
                self.repeat_one = true;
            }
            "all" | "playlist" => {
                self.repeat_all = true;
                self.repeat_one = false;
            }
            _ => {
                self.repeat_all = false;
                self.repeat_one = false;
            }
        }
        if self.items.is_empty() {
            self.current_index = None;
            self.original_items = None;
            self.autoplay_start_index = None;
        } else {
            self.current_index = Some(
                state
                    .current_index
                    .unwrap_or(0)
                    .min(self.items.len() - 1),
            );
            self.autoplay_start_index = state
                .autoplay_start_index
                .filter(|&idx| idx < self.items.len())
                .or_else(|| self.find_first_autoplay_index());
        }
        self.current_song().cloned()
    }

    fn find_first_autoplay_index(&self) -> Option<usize> {
        self.items
            .iter()
            .position(|s| s.queue_tier == QueueTier::Autoplay)
    }

    fn normalize_autoplay_start(&mut self) {
        if let Some(ap) = self.autoplay_start_index
            && ap >= self.items.len()
        {
            self.autoplay_start_index = None;
        }
    }

    /// Computes the next queue index respecting `repeat_one`, `repeat_all`, and bounds.
    pub fn next_index(&self, natural_eof: bool) -> Option<usize> {
        compute_next_index(
            self.current_index,
            self.items.len(),
            self.repeat_all,
            self.repeat_one,
            natural_eof,
        )
    }

    /// Returns the song that should be preloaded onto the standby deck (never the same
    /// index as the currently playing song when `repeat_one` is active).
    pub fn preload_candidate(&self) -> Option<Song> {
        let next_idx = self.next_index(true)?;
        if Some(next_idx) == self.current_index {
            return None;
        }
        self.items.get(next_idx).cloned()
    }

    /// Computes the previous queue index safely without out-of-bounds panics.
    pub fn prev_index(&self) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let cur = self.current_index?;
        if cur > 0 {
            Some(cur - 1)
        } else if self.repeat_all {
            Some(self.items.len() - 1)
        } else {
            Some(0)
        }
    }

    pub fn play_context(
        &mut self,
        songs: &[Song],
        start_index: usize,
        source: &QueueSource,
        shuffle_seed: u64,
    ) -> Option<Song> {
        if songs.is_empty() {
            return None;
        }
        let clamped_start = start_index.min(songs.len() - 1);
        let ctx = QueueTimeline::build_context_queue(
            &self.items,
            self.current_index.map_or(-1, |i| i as isize),
            songs,
            clamped_start,
            source,
        );
        self.items = ctx.timeline;
        self.current_index = if self.items.is_empty() {
            None
        } else {
            Some(ctx.start_index.min(self.items.len() - 1))
        };
        self.original_items = None;
        self.autoplay_start_index = None;

        if self.shuffle && self.items.len() > 2 {
            let cur = self.current_index.unwrap_or(0);
            self.original_items = Some(self.items.clone());
            self.items = QueueTimeline::shuffled_starting_order(&self.items, cur, shuffle_seed);
            self.current_index = Some(0);
        }

        self.current_song().cloned()
    }

    /// Starts a fresh Radio queue anchored at `seed` (index 0, `Context`) followed by
    /// deduplicated `recommendations` tagged as `QueueTier::Autoplay`.
    pub fn start_radio(
        &mut self,
        seed: Song,
        recommendations: Vec<Song>,
        shuffle_seed: u64,
    ) -> Option<Song> {
        let seed_entry = QueueTimeline::as_queue_entry(seed, QueueTier::Context);
        let mut seen = HashSet::new();
        seen.insert(seed_entry.video_id.clone());

        let mut next_items = Vec::with_capacity(1 + recommendations.len());
        next_items.push(seed_entry);
        for rec in recommendations {
            if seen.insert(rec.video_id.clone()) {
                next_items.push(QueueTimeline::as_queue_entry(rec, QueueTier::Autoplay));
            }
        }

        self.items = next_items;
        self.current_index = Some(0);
        self.original_items = None;
        self.autoplay_start_index = (self.items.len() > 1).then_some(1);

        if self.shuffle && self.items.len() > 2 {
            self.original_items = Some(self.items.clone());
            self.items = QueueTimeline::shuffled_starting_order(&self.items, 0, shuffle_seed);
            self.current_index = Some(0);
            self.autoplay_start_index = Some(1);
        }

        self.current_song().cloned()
    }

    /// Appends deduplicated Autoplay/Radio recommendation tracks to the tail of the queue,
    /// marking each with `QueueTier::Autoplay` and establishing `autoplay_start_index` when unset.
    /// Returns the number of newly appended tracks.
    pub fn append_autoplay(&mut self, songs: Vec<Song>) -> usize {
        if songs.is_empty() {
            return 0;
        }
        let mut seen: HashSet<String> = self.items.iter().map(|s| s.video_id.clone()).collect();
        let mut to_append = Vec::new();
        for song in songs {
            if seen.insert(song.video_id.clone()) {
                to_append.push(QueueTimeline::as_queue_entry(song, QueueTier::Autoplay));
            }
        }
        if to_append.is_empty() {
            return 0;
        }
        let start_idx = self.items.len();
        if self.autoplay_start_index.is_none() {
            self.autoplay_start_index = Some(start_idx);
        }
        let added = to_append.len();
        if let Some(ref mut orig) = self.original_items {
            orig.extend(to_append.iter().cloned());
        }
        self.items.extend(to_append);
        added
    }

    /// Returns a suitable YouTube radio seed track when autoplay continuation should fetch
    /// more recommendations (i.e. autoplay is enabled, repeat is off, and at most
    /// `threshold_remaining` tracks remain after `current_index`).
    pub fn needs_autoplay_continuation(
        &self,
        autoplay_enabled: bool,
        threshold_remaining: usize,
    ) -> Option<Song> {
        if !autoplay_enabled || self.repeat_all || self.repeat_one || self.items.is_empty() {
            return None;
        }
        let cur = self.current_index?;
        let remaining = self.items.len().saturating_sub(cur + 1);
        if remaining > threshold_remaining {
            return None;
        }
        // Prefer the tail YouTube track for smooth continuous radio flow, falling back to any
        // earlier YouTube track up to the active song.
        self.items
            .iter()
            .rev()
            .find(|s| is_youtube_radio_seed(&s.video_id))
            .cloned()
    }

    /// Moves the item at `from_idx` to `to_idx`, keeping `current_index` and
    /// `autoplay_start_index` accurate. Promoting an Autoplay track above the
    /// autoplay boundary converts it into a `QueueTier::UserQueue` entry.
    pub fn reorder(&mut self, from_idx: usize, to_idx: usize) -> bool {
        let len = self.items.len();
        if from_idx >= len || to_idx >= len || from_idx == to_idx {
            return false;
        }

        let mut moved = self.items.remove(from_idx);
        let prev_ap = self.autoplay_start_index;
        if let Some(ap) = prev_ap
            && from_idx >= ap
            && to_idx < ap
        {
            moved.queue_tier = QueueTier::UserQueue;
        }
        self.items.insert(to_idx, moved);

        if let Some(cur) = self.current_index {
            self.current_index = Some(if cur == from_idx {
                to_idx
            } else if from_idx < cur && cur <= to_idx {
                cur - 1
            } else if to_idx <= cur && cur < from_idx {
                cur + 1
            } else {
                cur
            });
        }

        if let Some(ap) = prev_ap {
            self.autoplay_start_index = if from_idx < ap && to_idx >= ap {
                Some(ap.saturating_sub(1))
            } else if from_idx >= ap && to_idx < ap {
                let next_ap = ap + 1;
                (next_ap < self.items.len()).then_some(next_ap)
            } else {
                Some(ap)
            };
        }
        self.normalize_autoplay_start();
        true
    }

    pub fn play_next(&mut self, song: Song) -> PlayNextOutcome {
        let entry = QueueTimeline::as_queue_entry(song, QueueTier::UserQueue);
        if self.items.is_empty() {
            self.items.push(entry.clone());
            self.current_index = Some(0);
            if let Some(ref mut orig) = self.original_items {
                orig.push(entry.clone());
            }
            return PlayNextOutcome::StartedImmediately(Box::new(entry));
        }

        let insert_at = QueueTimeline::find_user_queue_insertion_index(
            &self.items,
            self.current_index.map_or(-1, |i| i as isize),
            true,
        )
        .min(self.items.len());
        self.items.insert(insert_at, entry.clone());
        if let Some(ap) = self.autoplay_start_index
            && insert_at <= ap
        {
            self.autoplay_start_index = Some(ap + 1);
        }

        if let Some(ref mut orig) = self.original_items {
            let active_vid = self
                .current_index
                .and_then(|i| self.items.get(i))
                .map(|s| s.video_id.as_str());
            let orig_cur = active_vid
                .and_then(|vid| orig.iter().position(|s| s.video_id == vid))
                .map_or(-1, |i| i as isize);
            let orig_insert =
                QueueTimeline::find_user_queue_insertion_index(orig, orig_cur, true)
                    .min(orig.len());
            orig.insert(orig_insert, entry);
        }

        PlayNextOutcome::InsertedNext
    }

    pub fn jump(&mut self, idx: usize) -> Option<Song> {
        let song = self.items.get(idx)?.clone();
        self.current_index = Some(idx);
        Some(song)
    }

    pub fn remove(&mut self, idx: usize) -> QueueRemoveOutcome {
        if idx >= self.items.len() {
            return QueueRemoveOutcome::OutOfBounds;
        }
        let removed = self.items.remove(idx);
        if let Some(ap) = self.autoplay_start_index {
            if idx < ap {
                self.autoplay_start_index = Some(ap - 1);
            }
            self.normalize_autoplay_start();
        }
        if let Some(ref mut orig) = self.original_items
            && let Some(pos) = orig.iter().position(|s| s.video_id == removed.video_id)
        {
            orig.remove(pos);
        }

        let Some(cur) = self.current_index else {
            return if self.items.is_empty() {
                self.original_items = None;
                self.autoplay_start_index = None;
                QueueRemoveOutcome::Emptied
            } else {
                QueueRemoveOutcome::Updated
            };
        };

        if self.items.is_empty() {
            self.current_index = None;
            self.original_items = None;
            self.autoplay_start_index = None;
            QueueRemoveOutcome::Emptied
        } else if idx < cur {
            self.current_index = Some(cur - 1);
            QueueRemoveOutcome::Updated
        } else if idx == cur {
            let next_i = cur.min(self.items.len() - 1);
            self.current_index = Some(next_i);
            match self.items.get(next_i).cloned() {
                Some(song) => QueueRemoveOutcome::ActiveReplaced(Box::new(song)),
                None => QueueRemoveOutcome::Emptied,
            }
        } else {
            QueueRemoveOutcome::Updated
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.original_items = None;
        self.current_index = None;
        self.autoplay_start_index = None;
    }

    /// Toggles shuffle on/off. Preserves original queue order when turning on and
    /// restores original order anchored to the active track when turning off.
    pub fn toggle_shuffle(&mut self, seed: u64) -> bool {
        self.shuffle = !self.shuffle;
        if self.shuffle {
            if self.items.len() > 2 {
                let cur = self.current_index.unwrap_or(0).min(self.items.len() - 1);
                self.original_items = Some(self.items.clone());
                self.items = QueueTimeline::shuffled_starting_order(&self.items, cur, seed);
                self.current_index = Some(0);
                self.autoplay_start_index = self.find_first_autoplay_index();
            } else {
                self.original_items = Some(self.items.clone());
            }
        } else if let Some(orig) = self.original_items.take() {
            let active_vid = self.current_song().map(|s| s.video_id.clone());
            self.items = orig;
            if self.items.is_empty() {
                self.current_index = None;
                self.autoplay_start_index = None;
            } else if let Some(vid) = active_vid
                && let Some(pos) = self.items.iter().position(|s| s.video_id == vid)
            {
                self.current_index = Some(pos);
                self.autoplay_start_index = self.find_first_autoplay_index();
            } else {
                self.current_index = self.current_index.map(|i| i.min(self.items.len() - 1));
                self.autoplay_start_index = self.find_first_autoplay_index();
            }
        }
        self.shuffle
    }

    pub fn cycle_repeat(&mut self) {
        match (self.repeat_all, self.repeat_one) {
            (false, false) => {
                self.repeat_all = true;
                self.repeat_one = false;
            }
            (true, false) => {
                self.repeat_all = false;
                self.repeat_one = true;
            }
            _ => {
                self.repeat_all = false;
                self.repeat_one = false;
            }
        }
    }

    /// Advances `current_index` to the next song when a gapless/crossfade handoff
    /// already switched decks to `active_video_id`. Returns `true` if index moved.
    pub fn advance_to_matching_next(&mut self, active_video_id: &str) -> bool {
        if let Some(next_idx) = self.next_index(true)
            && self
                .items
                .get(next_idx)
                .is_some_and(|s| s.video_id == active_video_id)
        {
            self.current_index = Some(next_idx);
            return true;
        }
        false
    }

    /// Enriches `duration_text` on matching queue entries when newly discovered from
    /// stream metadata or decoder open. Returns `true` if any item changed.
    pub fn update_duration_text(&mut self, video_id: &str, duration_text: &str) -> bool {
        if duration_text.trim().is_empty() {
            return false;
        }
        let mut changed = false;
        for song in &mut self.items {
            if song.video_id == video_id
                && song.duration_text.as_deref().is_none_or(str::is_empty)
            {
                song.duration_text = Some(duration_text.to_owned());
                changed = true;
            }
        }
        if let Some(ref mut orig) = self.original_items {
            for song in orig {
                if song.video_id == video_id
                    && song.duration_text.as_deref().is_none_or(str::is_empty)
                {
                    song.duration_text = Some(duration_text.to_owned());
                }
            }
        }
        changed
    }
}

/// Returns `true` when `video_id` is a remote YouTube Music track suitable as an
/// InnerTube Radio/Autoplay continuation seed (excludes local/NAS/placeholder IDs).
pub fn is_youtube_radio_seed(video_id: &str) -> bool {
    let trimmed = video_id.trim();
    !trimmed.is_empty()
        && !trimmed.starts_with("local:")
        && !trimmed.starts_with("webdav:")
        && !trimmed.starts_with("smb:")
        && !trimmed.starts_with("sp:")
        && !trimmed.contains('/')
}

pub fn compute_next_index(
    current: Option<usize>,
    len: usize,
    repeat_all: bool,
    repeat_one: bool,
    natural_eof: bool,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cur = current?;
    if natural_eof && repeat_one {
        return Some(cur.min(len - 1));
    }
    if cur + 1 < len {
        Some(cur + 1)
    } else if repeat_all {
        Some(0)
    } else {
        None
    }
}
