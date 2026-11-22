//! Listen Together playback reconciliation state machine (`PartySync.kt`).
//!
//! Separates local playback intents (outbound) from server party frames (inbound)
//! so seeking/pausing never echoes in an oscillation loop, honors scheduled resume
//! lead-in (`ms_until_start`), and guards against slow-device seek storms with
//! consecutive drift strikes + cooldown.

use bitchord_integrations::ListenTogetherState;
use bitchord_jam::{PartyTrack, party_position_ms};

/// Local intent quiet window suppressing inbound reconcile while our control echoes back.
pub const INTENT_QUIET_MS: i64 = 1_500;
/// Allowed drift before a correcting seek is considered (ms).
pub const DRIFT_LIMIT_MS: i64 = 750;
/// Exact alignment tolerance on a brand-new playback control `seq` (ms).
pub const ALIGN_TOLERANCE_MS: i64 = 120;
/// Tolerance when paused so all devices park on the same position (ms).
pub const PAUSED_TOLERANCE_MS: i64 = 250;
/// Consecutive over-limit ticks required before seeking to fix drift.
pub const DRIFT_STRIKES: u32 = 2;
/// Minimum cooldown between drift-correction seeks (ms).
pub const DRIFT_COOLDOWN_MS: i64 = 5_000;

/// Snapshot of the local audio engine passed into [`PartySyncCoordinator::reconcile`].
#[derive(Debug, Clone, Default)]
pub struct LocalPlaybackView {
    pub current_video_id: Option<String>,
    pub is_device_file: bool,
    pub play_when_ready: bool,
    pub is_ready: bool,
    pub position_ms: i64,
}

/// Concrete imperative action the audio engine should execute to stay synced with the party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    None,
    LoadTrack {
        track: PartyTrack,
        start_ms: i64,
        auto_play: bool,
    },
    Pause {
        seek_ms: Option<i64>,
    },
    DeferStart {
        wait_ms: i64,
    },
    SeekAndPlay {
        position_ms: i64,
    },
    Seek {
        position_ms: i64,
    },
}

/// Pure, deterministic Listen Together playback synchronizer.
#[derive(Debug, Default)]
pub struct PartySyncCoordinator {
    reconcile_quiet_until_ms: i64,
    await_playback_seq: i64,
    await_queue_seq: i64,
    aligned_seq: i64,
    last_anchor_ms: i64,
    last_position_ms: i64,
    last_track_id: Option<String>,
    last_is_playing: bool,
    pub focus_lost: bool,
    pub rejoining: bool,
    pub locally_paused: bool,
    drift_strikes: u32,
    drift_cooldown_until_ms: i64,
}

impl PartySyncCoordinator {
    pub fn new() -> Self {
        Self {
            await_playback_seq: i64::MAX,
            await_queue_seq: i64::MAX,
            aligned_seq: -1,
            ..Self::default()
        }
    }

    /// Records a local user gesture, opening the quiet window until our control echoes back.
    pub fn on_local_intent(&mut self, party: &ListenTogetherState, now_ms: i64) {
        if !party.in_party() || party.controls_locked() {
            return;
        }
        if self.focus_lost {
            self.focus_lost = false;
            self.rejoining = true;
        }
        self.reconcile_quiet_until_ms = now_ms + INTENT_QUIET_MS;
        self.await_playback_seq = i64::MAX;
        self.await_queue_seq = i64::MAX;
    }

    /// Records the sequence numbers we expect back after sending outbound controls.
    pub fn await_seqs(&mut self, playback_seq: i64, queue_seq: i64) {
        self.await_playback_seq = playback_seq;
        self.await_queue_seq = queue_seq;
    }

    /// Handles a local play/pause tap when `party.controls_locked()` is active (radio model).
    pub fn on_locked_transport(&mut self, party: &ListenTogetherState, playing: bool) -> bool {
        if !party.controls_locked() {
            return false;
        }
        self.locally_paused = !playing;
        true
    }

    /// Computes the next playback reconciliation action against `party` at `now_ms`.
    pub fn reconcile(
        &mut self,
        party: &ListenTogetherState,
        server_now_ms: Option<i64>,
        local: &LocalPlaybackView,
        now_ms: i64,
    ) -> ReconcileAction {
        if !party.in_party() {
            self.reset();
            return ReconcileAction::None;
        }

        if self.locally_paused && !party.controls_locked() {
            self.locally_paused = false;
        }

        if party.playback.seq >= self.await_playback_seq && party.queue.seq >= self.await_queue_seq
        {
            self.reconcile_quiet_until_ms = 0;
        }

        if now_ms < self.reconcile_quiet_until_ms || self.focus_lost || local.is_device_file {
            return ReconcileAction::None;
        }

        let target = &party.playback;
        let Some(ref track) = target.track else {
            return ReconcileAction::None;
        };

        let want = server_now_ms.map(|sn| party_position_ms(target, sn));
        let wait_ms = server_now_ms
            .filter(|_| target.is_playing)
            .map(|sn| (target.anchor_ms - sn).max(0))
            .unwrap_or(0);

        if local.current_video_id.as_deref() != Some(track.video_id.as_str()) {
            let start_ms = want.unwrap_or(target.position_ms);
            let auto_play =
                target.is_playing && party.clock_synced && wait_ms == 0 && !self.locally_paused;
            return ReconcileAction::LoadTrack {
                track: track.clone(),
                start_ms,
                auto_play,
            };
        }

        if self.locally_paused {
            return ReconcileAction::None;
        }

        if !target.is_playing {
            let seek_ms = ((local.position_ms - target.position_ms).abs() > PAUSED_TOLERANCE_MS)
                .then_some(target.position_ms);
            if local.play_when_ready || seek_ms.is_some() {
                return ReconcileAction::Pause { seek_ms };
            }
            return ReconcileAction::None;
        }

        if !party.clock_synced {
            return ReconcileAction::None;
        }
        let Some(want_ms) = want else {
            return ReconcileAction::None;
        };

        if !local.play_when_ready {
            if wait_ms > 0 {
                return ReconcileAction::DeferStart { wait_ms };
            }
            self.aligned_seq = target.seq;
            self.record_anchor(target, &track.video_id);
            return ReconcileAction::SeekAndPlay {
                position_ms: want_ms,
            };
        }

        if !local.is_ready {
            self.drift_strikes = 0;
            return ReconcileAction::None;
        }

        let drift = local.position_ms - want_ms;
        if target.seq != self.aligned_seq {
            let anchor_changed = self.last_anchor_ms != target.anchor_ms
                || self.last_position_ms != target.position_ms
                || self.last_track_id.as_deref() != Some(track.video_id.as_str())
                || self.last_is_playing != target.is_playing;

            self.aligned_seq = target.seq;
            self.record_anchor(target, &track.video_id);

            if anchor_changed && drift.abs() > ALIGN_TOLERANCE_MS {
                return ReconcileAction::Seek {
                    position_ms: want_ms,
                };
            }
        }

        if drift.abs() <= DRIFT_LIMIT_MS {
            self.drift_strikes = 0;
            return ReconcileAction::None;
        }

        if now_ms < self.drift_cooldown_until_ms {
            return ReconcileAction::None;
        }
        self.drift_strikes += 1;
        if self.drift_strikes < DRIFT_STRIKES {
            return ReconcileAction::None;
        }

        self.drift_strikes = 0;
        self.drift_cooldown_until_ms = now_ms + DRIFT_COOLDOWN_MS;
        ReconcileAction::Seek {
            position_ms: want_ms,
        }
    }

    fn record_anchor(&mut self, playback: &bitchord_jam::PartyPlayback, video_id: &str) {
        self.last_anchor_ms = playback.anchor_ms;
        self.last_position_ms = playback.position_ms;
        self.last_track_id = Some(video_id.to_owned());
        self.last_is_playing = playback.is_playing;
    }

    pub fn reset(&mut self) {
        self.reconcile_quiet_until_ms = 0;
        self.await_playback_seq = i64::MAX;
        self.await_queue_seq = i64::MAX;
        self.aligned_seq = -1;
        self.last_anchor_ms = 0;
        self.last_position_ms = 0;
        self.last_track_id = None;
        self.last_is_playing = false;
        self.focus_lost = false;
        self.rejoining = false;
        self.locally_paused = false;
        self.drift_strikes = 0;
        self.drift_cooldown_until_ms = 0;
    }
}

#[cfg(test)]
mod tests {
    use bitchord_jam::{PartyPlayback, PartyTrack};

    use super::*;

    fn playing_party(seq: i64, pos: i64, anchor: i64) -> ListenTogetherState {
        ListenTogetherState {
            code: Some("ABC123".to_owned()),
            clock_synced: true,
            playback: PartyPlayback {
                seq,
                track: Some(PartyTrack::with_id("vid1")),
                is_playing: true,
                position_ms: pos,
                anchor_ms: anchor,
                ..PartyPlayback::default()
            },
            ..ListenTogetherState::default()
        }
    }

    #[test]
    fn defers_start_during_lead_in_and_requires_two_strikes_for_drift() {
        let mut sync = PartySyncCoordinator::new();
        let party = playing_party(1, 10_000, 50_350);
        let local = LocalPlaybackView {
            current_video_id: Some("vid1".to_owned()),
            is_device_file: false,
            play_when_ready: false,
            is_ready: true,
            position_ms: 0,
        };

        // 350 ms ahead of anchor -> DeferStart.
        assert_eq!(
            sync.reconcile(&party, Some(50_000), &local, 1_000),
            ReconcileAction::DeferStart { wait_ms: 350 }
        );
        // At anchor -> SeekAndPlay.
        assert_eq!(
            sync.reconcile(&party, Some(50_350), &local, 1_350),
            ReconcileAction::SeekAndPlay {
                position_ms: 10_000
            }
        );

        // Now playing with 1000 ms drift on same seq -> 1st strike ignored, 2nd seeks.
        let drifted = LocalPlaybackView {
            play_when_ready: true,
            position_ms: 11_000,
            ..local
        };
        assert_eq!(
            sync.reconcile(&party, Some(50_350), &drifted, 2_000),
            ReconcileAction::None
        );
        assert_eq!(
            sync.reconcile(&party, Some(50_350), &drifted, 2_700),
            ReconcileAction::Seek {
                position_ms: 10_000
            }
        );
        // Inside cooldown -> no immediate repeat seek.
        assert_eq!(
            sync.reconcile(&party, Some(50_350), &drifted, 3_000),
            ReconcileAction::None
        );
    }

    #[test]
    fn queue_only_seq_bump_does_not_force_audible_seek() {
        let mut sync = PartySyncCoordinator::new();
        let p1 = playing_party(1, 10_000, 50_000);
        let local = LocalPlaybackView {
            current_video_id: Some("vid1".to_owned()),
            is_device_file: false,
            play_when_ready: true,
            is_ready: true,
            position_ms: 10_200, // 200 ms drift (> ALIGN_TOLERANCE_MS, < DRIFT_LIMIT_MS)
        };
        // First seq aligns.
        assert_eq!(
            sync.reconcile(&p1, Some(50_000), &local, 1_000),
            ReconcileAction::Seek {
                position_ms: 10_000
            }
        );
        // Seq bumps to 2 with identical anchor/position/track (e.g. queue add) -> NO seek!
        let p2 = playing_party(2, 10_000, 50_000);
        assert_eq!(
            sync.reconcile(&p2, Some(50_000), &local, 2_000),
            ReconcileAction::None
        );
    }
}
