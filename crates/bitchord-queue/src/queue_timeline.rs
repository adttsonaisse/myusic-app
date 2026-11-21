use bitchord_model::{PlaybackSourceType, QueueTier, Song};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Information about where a queue or track was started from in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSource {
    pub title: String,
    pub r#type: PlaybackSourceType,
    #[serde(default)]
    pub id: Option<String>,
}

/// Result of constructing a context queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextQueueResult {
    pub timeline: Vec<Song>,
    pub start_index: usize,
}

/// Index permutation and target plan for jumping to a track in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpOrderPlan {
    pub is_pure_seek: bool,
    pub new_order_indices: Vec<usize>,
    pub new_target_index: usize,
    pub promote_target_to_context: bool,
}

/// Pure three-tier queue rules (`USER_QUEUE`, `CONTEXT`, `AUTOPLAY`).
pub struct QueueTimeline;

impl QueueTimeline {
    /// Marks a song with `tier` and assigns an immutable UUID `queue_entry_id` once.
    pub fn as_queue_entry(mut song: Song, tier: QueueTier) -> Song {
        song.queue_tier = tier;
        if song.queue_entry_id.is_none() {
            song.queue_entry_id = Some(Uuid::new_v4().to_string());
        }
        song
    }

    fn upcoming_user_queue(timeline: &[Song], current_index: isize) -> Vec<Song> {
        let Ok(cur) = usize::try_from(current_index) else {
            return Vec::new();
        };
        if cur >= timeline.len() {
            return Vec::new();
        }
        timeline[(cur + 1)..]
            .iter()
            .filter(|s| s.queue_tier == QueueTier::UserQueue)
            .cloned()
            .collect()
    }

    fn apply_source(mut song: Song, source: &QueueSource) -> Song {
        song.playback_source = Some(source.title.clone());
        song.playback_source_type = Some(source.r#type);
        song.playback_source_id = source.id.clone();
        song
    }

    /// `[Preceding Context] + [Selected] + [Preserved USER_QUEUE] + [Following Context]`.
    pub fn build_context_queue(
        current_timeline: &[Song],
        current_index: isize,
        new_context_songs: &[Song],
        selected_index: usize,
        context_source: &QueueSource,
    ) -> ContextQueueResult {
        if new_context_songs.is_empty() {
            return ContextQueueResult {
                timeline: Vec::new(),
                start_index: 0,
            };
        }
        let upcoming_uq = Self::upcoming_user_queue(current_timeline, current_index);
        let context_entries: Vec<Song> = new_context_songs
            .iter()
            .cloned()
            .map(|s| {
                Self::as_queue_entry(Self::apply_source(s, context_source), QueueTier::Context)
            })
            .collect();
        let safe_index = selected_index.min(context_entries.len() - 1);

        let mut timeline = Vec::with_capacity(context_entries.len() + upcoming_uq.len());
        timeline.extend_from_slice(&context_entries[..safe_index]);
        timeline.push(context_entries[safe_index].clone());
        timeline.extend(upcoming_uq);
        timeline.extend_from_slice(&context_entries[(safe_index + 1)..]);

        ContextQueueResult {
            timeline,
            start_index: safe_index,
        }
    }

    /// `[Tapped Track (CONTEXT)] + [Preserved USER_QUEUE]`.
    pub fn build_one_off_queue(
        current_timeline: &[Song],
        current_index: isize,
        tapped_song: Song,
        source: &QueueSource,
    ) -> Vec<Song> {
        let upcoming_uq = Self::upcoming_user_queue(current_timeline, current_index);
        let mut out = Vec::with_capacity(1 + upcoming_uq.len());
        out.push(Self::as_queue_entry(
            Self::apply_source(tapped_song, source),
            QueueTier::Context,
        ));
        out.extend(upcoming_uq);
        out
    }

    /// `[Tapped Track (CONTEXT)] + [Upcoming non-AutoPlay party tracks (USER_QUEUE)]`.
    pub fn build_party_playback_queue(
        tapped_song: Song,
        source: &QueueSource,
        upcoming_party_songs: &[Song],
    ) -> Vec<Song> {
        let mut out = Vec::with_capacity(1 + upcoming_party_songs.len());
        out.push(Self::as_queue_entry(
            Self::apply_source(tapped_song, source),
            QueueTier::Context,
        ));
        for song in upcoming_party_songs {
            if !song.from_autoplay() {
                out.push(Self::as_queue_entry(song.clone(), QueueTier::UserQueue));
            }
        }
        out
    }

    /// Insertion index for "Play Next" (`is_next = true`) or "Add to Queue" (`is_next = false`).
    pub fn find_user_queue_insertion_index(
        timeline: &[Song],
        current_index: isize,
        is_next: bool,
    ) -> usize {
        if timeline.is_empty() {
            return 0;
        }
        let start = (current_index + 1).clamp(0, timeline.len() as isize) as usize;
        if is_next {
            return start;
        }
        for (i, song) in timeline.iter().enumerate().skip(start) {
            if song.queue_tier != QueueTier::UserQueue {
                return i;
            }
        }
        timeline.len()
    }

    /// Authoritative jump permutation plan for tapping `target_index` while at `current_index`.
    pub fn compute_jump_order(
        tiers: &[QueueTier],
        current_index: usize,
        target_index: usize,
    ) -> Option<JumpOrderPlan> {
        if current_index >= tiers.len() || target_index >= tiers.len() {
            return None;
        }
        if target_index <= current_index {
            return Some(JumpOrderPlan {
                is_pure_seek: true,
                new_order_indices: (0..tiers.len()).collect(),
                new_target_index: target_index,
                promote_target_to_context: false,
            });
        }

        match tiers[target_index] {
            QueueTier::Context => {
                let crossed_uq: Vec<usize> = ((current_index + 1)..target_index)
                    .filter(|&i| tiers[i] == QueueTier::UserQueue)
                    .collect();
                if crossed_uq.is_empty() {
                    Some(JumpOrderPlan {
                        is_pure_seek: true,
                        new_order_indices: (0..tiers.len()).collect(),
                        new_target_index: target_index,
                        promote_target_to_context: false,
                    })
                } else {
                    let preceding_history: Vec<usize> = (0..=current_index).collect();
                    let bypassed_context: Vec<usize> = ((current_index + 1)..target_index)
                        .filter(|&i| tiers[i] == QueueTier::Context)
                        .collect();
                    let following_uq: Vec<usize> = ((target_index + 1)..tiers.len())
                        .filter(|&i| tiers[i] == QueueTier::UserQueue)
                        .collect();
                    let following_ctx: Vec<usize> = ((target_index + 1)..tiers.len())
                        .filter(|&i| tiers[i] == QueueTier::Context)
                        .collect();
                    let following_auto: Vec<usize> = ((target_index + 1)..tiers.len())
                        .filter(|&i| tiers[i] == QueueTier::Autoplay)
                        .collect();

                    let new_target = preceding_history.len() + bypassed_context.len();
                    let mut order = Vec::with_capacity(tiers.len());
                    order.extend(preceding_history);
                    order.extend(bypassed_context);
                    order.push(target_index);
                    order.extend(crossed_uq);
                    order.extend(following_uq);
                    order.extend(following_ctx);
                    order.extend(following_auto);
                    Some(JumpOrderPlan {
                        is_pure_seek: false,
                        new_order_indices: order,
                        new_target_index: new_target,
                        promote_target_to_context: false,
                    })
                }
            }
            QueueTier::UserQueue => {
                let history: Vec<usize> = (0..=current_index).collect();
                let new_target = history.len();
                let mut order = history;
                order.push(target_index);
                order.extend(
                    ((target_index + 1)..tiers.len()).filter(|&i| tiers[i] == QueueTier::UserQueue),
                );
                order.extend(
                    ((current_index + 1)..tiers.len()).filter(|&i| tiers[i] == QueueTier::Context),
                );
                order.extend(
                    ((current_index + 1)..tiers.len()).filter(|&i| tiers[i] == QueueTier::Autoplay),
                );
                Some(JumpOrderPlan {
                    is_pure_seek: false,
                    new_order_indices: order,
                    new_target_index: new_target,
                    promote_target_to_context: false,
                })
            }
            QueueTier::Autoplay => {
                let history: Vec<usize> = (0..=current_index).collect();
                let new_target = history.len();
                let mut order = history;
                order.push(target_index);
                order.extend(
                    ((current_index + 1)..tiers.len())
                        .filter(|&i| tiers[i] == QueueTier::UserQueue),
                );
                order.extend(
                    ((target_index + 1)..tiers.len()).filter(|&i| tiers[i] == QueueTier::Autoplay),
                );
                Some(JumpOrderPlan {
                    is_pure_seek: false,
                    new_order_indices: order,
                    new_target_index: new_target,
                    promote_target_to_context: true,
                })
            }
        }
    }

    /// Reconstructs the upcoming queue from `target_index` onward using [`Self::compute_jump_order`].
    pub fn build_jump_queue(
        current_timeline: &[Song],
        current_index: usize,
        target_index: usize,
    ) -> Option<Vec<Song>> {
        let tiers: Vec<QueueTier> = current_timeline.iter().map(|s| s.queue_tier).collect();
        let plan = Self::compute_jump_order(&tiers, current_index, target_index)?;
        if plan.is_pure_seek && target_index <= current_index {
            return None;
        }
        let target_song = &current_timeline[target_index];
        Some(
            plan.new_order_indices
                .into_iter()
                .skip(plan.new_target_index)
                .map(|idx| {
                    let mut song = current_timeline[idx].clone();
                    if idx == target_index && plan.promote_target_to_context {
                        let source_title = target_song
                            .playback_source
                            .as_deref()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(&target_song.title)
                            .to_owned();
                        song.playback_source = Some(source_title);
                        song.playback_source_type = Some(
                            target_song
                                .playback_source_type
                                .unwrap_or(PlaybackSourceType::Queue),
                        );
                        Self::as_queue_entry(song, QueueTier::Context)
                    } else {
                        song
                    }
                })
                .collect(),
        )
    }

    pub fn upcoming_user_queue_indices(tiers: &[QueueTier], current_index: isize) -> Vec<usize> {
        let start = (current_index + 1).max(0) as usize;
        (start..tiers.len())
            .filter(|&i| tiers[i] == QueueTier::UserQueue)
            .collect()
    }

    pub fn played_user_queue_indices(tiers: &[QueueTier], current_index: usize) -> Vec<usize> {
        if current_index == 0 || current_index >= tiers.len() {
            return Vec::new();
        }
        if tiers[current_index] != QueueTier::Context {
            return Vec::new();
        }
        (0..current_index)
            .filter(|&i| tiers[i] == QueueTier::UserQueue)
            .collect()
    }

    /// Reorders starting songs when shuffle is on: selected track first, USER_QUEUE pinned,
    /// CONTEXT and AUTOPLAY each shuffled within their own tier using `seed`.
    pub fn shuffled_starting_order(songs: &[Song], start_index: usize, seed: u64) -> Vec<Song> {
        if songs.is_empty() {
            return Vec::new();
        }
        let at = start_index.min(songs.len() - 1);
        let mut user = Vec::new();
        let mut ctx = Vec::new();
        let mut auto = Vec::new();
        for (i, s) in songs.iter().enumerate() {
            if i == at {
                continue;
            }
            match s.queue_tier {
                QueueTier::UserQueue => user.push(s.clone()),
                QueueTier::Context => ctx.push(s.clone()),
                QueueTier::Autoplay => auto.push(s.clone()),
            }
        }
        let mut rng = SplitMix64::new(seed);
        rng.shuffle(&mut ctx);
        rng.shuffle(&mut auto);

        let mut out = Vec::with_capacity(songs.len());
        out.push(songs[at].clone());
        out.extend(user);
        out.extend(ctx);
        out.extend(auto);
        out
    }

    /// Shuffles upcoming indices within CONTEXT and AUTOPLAY while leaving USER_QUEUE untouched.
    pub fn shuffled_upcoming_order(upcoming: &[QueueTier], seed: u64) -> Vec<usize> {
        let user: Vec<usize> = (0..upcoming.len())
            .filter(|&i| upcoming[i] == QueueTier::UserQueue)
            .collect();
        let ctx: Vec<usize> = (0..upcoming.len())
            .filter(|&i| upcoming[i] == QueueTier::Context)
            .collect();
        let auto: Vec<usize> = (0..upcoming.len())
            .filter(|&i| upcoming[i] == QueueTier::Autoplay)
            .collect();

        let mut rng = SplitMix64::new(seed);
        let mut ctx_shuf = ctx.clone();
        rng.shuffle(&mut ctx_shuf);
        let mut auto_shuf = auto.clone();
        rng.shuffle(&mut auto_shuf);

        let mut out = Vec::with_capacity(upcoming.len());
        out.extend(user);
        out.extend(Self::avoid_identity_shuffle(&ctx, ctx_shuf));
        out.extend(Self::avoid_identity_shuffle(&auto, auto_shuf));
        out
    }

    pub fn sectioned_order(restored: &[usize], upcoming: &[QueueTier]) -> Vec<usize> {
        let mut out = Vec::with_capacity(restored.len());
        out.extend(
            restored
                .iter()
                .copied()
                .filter(|&i| upcoming[i] == QueueTier::UserQueue),
        );
        out.extend(
            restored
                .iter()
                .copied()
                .filter(|&i| upcoming[i] == QueueTier::Context),
        );
        out.extend(
            restored
                .iter()
                .copied()
                .filter(|&i| upcoming[i] == QueueTier::Autoplay),
        );
        out
    }

    pub fn avoid_identity_shuffle(original: &[usize], mut shuffled: Vec<usize>) -> Vec<usize> {
        if original.len() <= 1 || shuffled.as_slice() != original {
            return shuffled;
        }
        shuffled.rotate_left(1);
        shuffled
    }
}

pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(crate) fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = (self.next_u64() as usize) % (i + 1);
            slice.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, tier: QueueTier) -> Song {
        let mut s = Song::new(id, format!("Title {id}"), "Artist", None);
        s.queue_tier = tier;
        s.queue_entry_id = Some(format!("entry-{id}"));
        s
    }

    #[test]
    fn context_queue_preserves_upcoming_user_queue_after_selected() {
        let current = vec![
            song("old", QueueTier::Context),
            song("u1", QueueTier::UserQueue),
            song("u2", QueueTier::UserQueue),
        ];
        let album = vec![
            song("a0", QueueTier::Context),
            song("a1", QueueTier::Context),
            song("a2", QueueTier::Context),
        ];
        let src = QueueSource {
            title: "Album".into(),
            r#type: PlaybackSourceType::Browse,
            id: Some("MPREb1".into()),
        };
        let res = QueueTimeline::build_context_queue(&current, 0, &album, 1, &src);
        assert_eq!(1, res.start_index);
        let ids: Vec<&str> = res.timeline.iter().map(|s| s.video_id.as_str()).collect();
        assert_eq!(vec!["a0", "a1", "u1", "u2", "a2"], ids);
    }

    #[test]
    fn jump_across_user_queue_pins_user_queue_and_autoplay_jump_promotes() {
        // [C0(cur), U1, C2(target), C3, A4] -> jumping to C2 pins U1 right after C2!
        let tiers = vec![
            QueueTier::Context,
            QueueTier::UserQueue,
            QueueTier::Context,
            QueueTier::Context,
            QueueTier::Autoplay,
        ];
        let plan = QueueTimeline::compute_jump_order(&tiers, 0, 2).unwrap();
        assert!(!plan.is_pure_seek);
        assert_eq!(vec![0, 2, 1, 3, 4], plan.new_order_indices);
        assert_eq!(1, plan.new_target_index);

        // Jumping to Autoplay promotes target and keeps user queue while dropping old context.
        let auto_plan = QueueTimeline::compute_jump_order(&tiers, 0, 4).unwrap();
        assert!(auto_plan.promote_target_to_context);
        assert_eq!(vec![0, 4, 1], auto_plan.new_order_indices);
        assert_eq!(1, auto_plan.new_target_index);
    }

    #[test]
    fn shuffle_keeps_user_queue_pinned_and_avoids_identity() {
        let items = vec![
            song("sel", QueueTier::Context),
            song("u1", QueueTier::UserQueue),
            song("u2", QueueTier::UserQueue),
            song("c1", QueueTier::Context),
            song("c2", QueueTier::Context),
            song("a1", QueueTier::Autoplay),
        ];
        let ordered = QueueTimeline::shuffled_starting_order(&items, 0, 42);
        assert_eq!("sel", ordered[0].video_id);
        assert_eq!("u1", ordered[1].video_id);
        assert_eq!("u2", ordered[2].video_id);
        assert_eq!("a1", ordered[5].video_id);

        assert_eq!(
            vec![1, 0],
            QueueTimeline::avoid_identity_shuffle(&[0, 1], vec![0, 1])
        );
    }
}
