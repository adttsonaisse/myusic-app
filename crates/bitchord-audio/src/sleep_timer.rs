//! Monotonic sleep timer (`SleepTimer.kt` + `SleepTimerTest.kt`).
//!
//! Holds a monotonic `Instant` deadline (immune to NTP wall-clock jumps) or an
//! `after_track` latch that pauses on the next track-end event.

use std::time::{Duration, Instant};

/// Standard duration presets offered in the Sleep Timer UI (minutes).
pub const PRESETS: [u32; 4] = [15, 30, 45, 60];
/// Step size of the custom slider (minutes).
pub const STEP_MINUTES: u32 = 5;
/// Maximum custom sleep duration (12 hours = 720 minutes).
pub const MAX_MINUTES: u32 = 720;

/// Monotonic sleep timer state.
#[derive(Debug, Clone, Default)]
pub struct SleepTimer {
    deadline: Option<Instant>,
    minutes: Option<u32>,
    after_track: bool,
}

impl SleepTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts a normalized slider position `fraction ∈ [0.0, 1.0]` into stepped minutes.
    pub fn minutes_at(fraction: f32) -> u32 {
        let clamped = fraction.clamp(0.0, 1.0);
        let raw = STEP_MINUTES as f32 + clamped * (MAX_MINUTES - STEP_MINUTES) as f32;
        ((raw / STEP_MINUTES as f32).round() as u32) * STEP_MINUTES
    }

    /// Inverse of [`SleepTimer::minutes_at`].
    pub fn fraction_of(minutes: u32) -> f32 {
        (minutes.saturating_sub(STEP_MINUTES)) as f32 / (MAX_MINUTES - STEP_MINUTES) as f32
    }

    pub fn is_running(&self) -> bool {
        self.deadline.is_some() || self.after_track
    }

    pub fn minutes(&self) -> Option<u32> {
        self.minutes
    }

    pub fn after_track(&self) -> bool {
        self.after_track
    }

    pub fn start(&mut self, minutes: u32) {
        self.start_at(minutes, Instant::now());
    }

    pub fn start_at(&mut self, minutes: u32, now: Instant) {
        let mins = minutes.max(1);
        self.after_track = false;
        self.minutes = Some(mins);
        self.deadline = Some(now + Duration::from_secs(u64::from(mins) * 60));
    }

    /// Arm timer to pause once the current track ends.
    pub fn start_after_track(&mut self) {
        self.minutes = None;
        self.deadline = None;
        self.after_track = true;
    }

    pub fn cancel(&mut self) {
        self.minutes = None;
        self.deadline = None;
        self.after_track = false;
    }

    /// Milliseconds remaining until deadline, or `None` when not running a timed countdown.
    pub fn remaining_ms(&self) -> Option<u64> {
        self.remaining_ms_at(Instant::now())
    }

    pub fn remaining_ms_at(&self, now: Instant) -> Option<u64> {
        self.deadline
            .map(|d| d.saturating_duration_since(now).as_millis() as u64)
    }

    /// Returns `true` and clears the timer once the monotonic deadline has elapsed.
    pub fn poll_expired(&mut self, now: Instant) -> bool {
        if let Some(deadline) = self.deadline
            && now >= deadline
        {
            self.cancel();
            return true;
        }
        false
    }

    /// Consumes the `after_track` latch when the current track finishes.
    pub fn take_after_track(&mut self) -> bool {
        if self.after_track {
            self.after_track = false;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Replicated from SleepTimerTest.kt (all 3 tests) --------------------

    #[test]
    fn custom_slider_spans_one_step_to_twelve_hours() {
        assert_eq!(STEP_MINUTES, SleepTimer::minutes_at(0.0));
        assert_eq!(MAX_MINUTES, SleepTimer::minutes_at(1.0));
        assert_eq!(STEP_MINUTES, SleepTimer::minutes_at(-0.5));
        assert_eq!(MAX_MINUTES, SleepTimer::minutes_at(1.5));
    }

    #[test]
    fn custom_slider_lands_on_whole_steps() {
        for i in 0..=100 {
            assert_eq!(0, SleepTimer::minutes_at(i as f32 / 100.0) % STEP_MINUTES);
        }
    }

    #[test]
    fn fraction_of_is_the_inverse_of_minutes_at() {
        for minutes in (STEP_MINUTES..=MAX_MINUTES).step_by(STEP_MINUTES as usize) {
            assert_eq!(
                minutes,
                SleepTimer::minutes_at(SleepTimer::fraction_of(minutes))
            );
        }
    }

    #[test]
    fn monotonic_deadline_and_after_track_lifecycle() {
        let mut timer = SleepTimer::new();
        let t0 = Instant::now();
        timer.start_at(15, t0);
        assert!(timer.is_running());
        assert_eq!(timer.remaining_ms_at(t0), Some(15 * 60_000));
        assert!(!timer.poll_expired(t0 + Duration::from_secs(14 * 60)));
        assert!(timer.poll_expired(t0 + Duration::from_secs(15 * 60)));
        assert!(!timer.is_running());

        timer.start_after_track();
        assert!(timer.is_running());
        assert!(timer.after_track());
        assert!(timer.take_after_track());
        assert!(!timer.is_running());
    }
}
