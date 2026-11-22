//! Dual-deck equal-power crossfade law (`sin² + cos² = 1`), blend headroom trim,
//! and transition lifecycle (`CrossfadeController.kt`).

use std::f32::consts::FRAC_PI_2;

use serde::{Deserialize, Serialize};

/// Fractional progress where metadata, queue index, and active deck role swap (`0.5`).
pub const HANDOFF_AT: f32 = 0.5;
/// Guard window before outgoing EOF guaranteeing handoff fires even on short fades.
pub const HANDOFF_GUARD_MS: u64 = 400;
/// Maximum configurable crossfade duration (seconds).
pub const MAX_CROSSFADE_SECONDS: u32 = 12;
/// Smooth bail ramp step interval when a seek/skip interrupts an active blend.
pub const BAIL_STEP_MS: u64 = 15;
/// Floor for blend headroom trim (`-6 dB`, matching `LoudnessProcessor::MIN_TRIM`).
pub const MIN_BLEND_TRIM: f32 = 0.5;

/// Active peer deck identifier swapped at each crossfade handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeckId {
    #[default]
    A,
    B,
}

impl DeckId {
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Phase of the dual-deck crossfade state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrossfadePhase {
    #[default]
    Idle,
    Arming,
    Fading,
    SleepFade,
    Bailing,
}

/// Instantaneous equal-power gains and headroom trim computed for progress `t ∈ [0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossfadeGains {
    pub outgoing: f32,
    pub incoming: f32,
    pub blend_trim: f32,
}

/// Computes equal-power `cos`/`sin` crossfade gains plus `1 / sqrt(out + in)` blend trim.
///
/// Matches `CrossfadeController.kt`:
/// - `outgoing² + incoming² == 1.0` across the entire fade (no dip in the middle)
/// - `blend_trim = 1 / sqrt(outgoing + incoming)` clamped to `[0.5, 1.0]`
///   (`~0.841` / `-1.5 dB` at midpoint `t = 0.5`, bounding peak sum to `<= 1.19`).
pub fn equal_power_gains(progress: f32) -> CrossfadeGains {
    let t = progress.clamp(0.0, 1.0);
    let angle = t * FRAC_PI_2;
    let outgoing = angle.cos().clamp(0.0, 1.0);
    let incoming = angle.sin().clamp(0.0, 1.0);
    let sum = (outgoing + incoming).max(1.0);
    let blend_trim = (1.0 / sum.sqrt()).clamp(MIN_BLEND_TRIM, 1.0);
    CrossfadeGains {
        outgoing,
        incoming,
        blend_trim,
    }
}

/// Controls a single dual-deck crossfade transition on frame/millisecond ticks.
#[derive(Debug, Clone)]
pub struct CrossfadeState {
    pub phase: CrossfadePhase,
    pub active_deck: DeckId,
    pub crossfade_seconds: u32,
    pub handed_off: bool,
    elapsed_ms: u64,
    duration_ms: u64,
}

impl Default for CrossfadeState {
    fn default() -> Self {
        Self::new(0)
    }
}

impl CrossfadeState {
    pub fn new(crossfade_seconds: u32) -> Self {
        Self {
            phase: CrossfadePhase::Idle,
            active_deck: DeckId::A,
            crossfade_seconds: crossfade_seconds.min(MAX_CROSSFADE_SECONDS),
            handed_off: false,
            elapsed_ms: 0,
            duration_ms: 0,
        }
    }

    pub fn is_transitioning(&self) -> bool {
        !matches!(self.phase, CrossfadePhase::Idle)
    }

    /// Normalized progress `0.0..=1.0` of the currently running transition.
    pub fn progress(&self) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        (self.elapsed_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
    }

    pub fn set_crossfade_seconds(&mut self, secs: u32) {
        self.crossfade_seconds = secs.min(MAX_CROSSFADE_SECONDS);
    }

    /// Whether an active track at `(position_ms, duration_ms)` should begin arming/fading.
    pub fn should_start_crossfade(&self, position_ms: u64, duration_ms: u64) -> bool {
        if self.phase != CrossfadePhase::Idle || self.crossfade_seconds == 0 {
            return false;
        }
        let fade_ms = u64::from(self.crossfade_seconds) * 1_000;
        if duration_ms <= fade_ms + 1_000 {
            return false;
        }
        duration_ms.saturating_sub(position_ms) <= fade_ms
    }

    /// Starts an equal-power blend from `active_deck` to `active_deck.other()`.
    pub fn begin_fade(&mut self, duration_ms: u64) {
        self.phase = CrossfadePhase::Fading;
        self.elapsed_ms = 0;
        self.duration_ms = duration_ms.max(1);
        self.handed_off = false;
    }

    /// Starts a solo sleep-timer fade-out with no incoming track.
    pub fn begin_sleep_fade(&mut self, duration_ms: u64) {
        self.phase = CrossfadePhase::SleepFade;
        self.elapsed_ms = 0;
        self.duration_ms = duration_ms.max(1);
        self.handed_off = false;
    }

    /// Interrupts an in-flight fade with a quick smooth bail-out.
    pub fn bail(&mut self) {
        if self.phase != CrossfadePhase::Idle {
            self.phase = CrossfadePhase::Bailing;
            self.elapsed_ms = 0;
            self.duration_ms = BAIL_STEP_MS * 4;
        }
    }

    /// Advances the fade by `delta_ms`, returning `(gains, just_handed_off, just_finished)`.
    pub fn advance(&mut self, delta_ms: u64) -> (CrossfadeGains, bool, bool) {
        match self.phase {
            CrossfadePhase::Idle | CrossfadePhase::Arming => (
                CrossfadeGains {
                    outgoing: 1.0,
                    incoming: 0.0,
                    blend_trim: 1.0,
                },
                false,
                false,
            ),
            CrossfadePhase::Fading => {
                self.elapsed_ms = self.elapsed_ms.saturating_add(delta_ms);
                let progress = (self.elapsed_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0);
                let gains = equal_power_gains(progress);
                let remaining_ms = self.duration_ms.saturating_sub(self.elapsed_ms);
                let mut just_handed_off = false;
                if !self.handed_off && (progress >= HANDOFF_AT || remaining_ms <= HANDOFF_GUARD_MS)
                {
                    self.handed_off = true;
                    self.active_deck = self.active_deck.other();
                    just_handed_off = true;
                }
                let finished = self.elapsed_ms >= self.duration_ms;
                if finished {
                    self.phase = CrossfadePhase::Idle;
                }
                (gains, just_handed_off, finished)
            }
            CrossfadePhase::SleepFade | CrossfadePhase::Bailing => {
                self.elapsed_ms = self.elapsed_ms.saturating_add(delta_ms);
                let progress = (self.elapsed_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0);
                let outgoing = (progress * FRAC_PI_2).cos().clamp(0.0, 1.0);
                let finished = self.elapsed_ms >= self.duration_ms;
                if finished {
                    self.phase = CrossfadePhase::Idle;
                }
                (
                    CrossfadeGains {
                        outgoing,
                        incoming: 0.0,
                        blend_trim: 1.0,
                    },
                    false,
                    finished,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_power_preserves_constant_power_and_bounds_sum_with_trim() {
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let g = equal_power_gains(t);
            let power = g.outgoing * g.outgoing + g.incoming * g.incoming;
            assert!((power - 1.0).abs() < 1e-5, "power={power} at t={t}");
            let trimmed_sum = (g.outgoing + g.incoming) * g.blend_trim;
            assert!(
                trimmed_sum <= 1.19 + 1e-3,
                "trimmed_sum={trimmed_sum} at t={t}"
            );
        }
        let mid = equal_power_gains(0.5);
        assert!((mid.outgoing - mid.incoming).abs() < 1e-5);
        assert!((mid.blend_trim - (1.0 / (2.0_f32.sqrt()).sqrt())).abs() < 1e-4);
    }

    #[test]
    fn crossfade_swaps_deck_at_halfway_and_settles_idle() {
        let mut cf = CrossfadeState::new(6);
        assert_eq!(cf.active_deck, DeckId::A);
        assert!(!cf.should_start_crossfade(10_000, 180_000));
        assert!(cf.should_start_crossfade(175_000, 180_000));

        cf.begin_fade(6_000);
        let (_, handed, done) = cf.advance(2_000);
        assert!(!handed && !done);
        assert_eq!(cf.active_deck, DeckId::A);

        let (_, handed, done) = cf.advance(1_000);
        assert!(handed && !done);
        assert_eq!(cf.active_deck, DeckId::B);

        let (_, handed, done) = cf.advance(3_000);
        assert!(!handed && done);
        assert_eq!(cf.phase, CrossfadePhase::Idle);
    }
}
