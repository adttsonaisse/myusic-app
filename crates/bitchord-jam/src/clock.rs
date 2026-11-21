use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

const WINDOW: usize = 12;
const SAMPLE_TTL_MS: i64 = 120_000;

#[derive(Debug, Clone, Copy)]
struct Sample {
    offset_ms: i64,
    round_trip_ms: i64,
    taken_at_ms: i64,
}

#[derive(Default)]
struct ClockState {
    samples: VecDeque<Sample>,
    offset_ms: Option<i64>,
    round_trip_ms: i64,
}

/// Monotonic NTP-style offset estimator between this Linux device and the Listen Together server.
/// Keeps a sliding window of 12 samples (120 s TTL) and always selects the lowest-RTT sample.
pub struct ServerClock<F = Box<dyn Fn() -> i64 + Send + Sync>> {
    local_now_ms: F,
    state: Mutex<ClockState>,
}

impl ServerClock<Box<dyn Fn() -> i64 + Send + Sync>> {
    /// Creates a `ServerClock` backed by `std::time::Instant` (strictly monotonic).
    pub fn monotonic() -> Self {
        let origin = Instant::now();
        Self::with_source(Box::new(move || origin.elapsed().as_millis() as i64))
    }
}

impl<F: Fn() -> i64> ServerClock<F> {
    pub fn with_source(local_now_ms: F) -> Self {
        Self {
            local_now_ms,
            state: Mutex::new(ClockState::default()),
        }
    }

    pub fn synced(&self) -> bool {
        self.offset_ms().is_some()
    }

    pub fn offset_ms(&self) -> Option<i64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .offset_ms
    }

    pub fn round_trip_ms(&self) -> i64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .round_trip_ms
    }

    pub fn record(&self, sent_at_local_ms: i64, server_ms: i64, received_at_local_ms: i64) {
        let round_trip = (received_at_local_ms - sent_at_local_ms).max(0);
        let midpoint = sent_at_local_ms + round_trip / 2;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.samples.push_back(Sample {
            offset_ms: server_ms - midpoint,
            round_trip_ms: round_trip,
            taken_at_ms: received_at_local_ms,
        });
        while state.samples.len() > WINDOW {
            state.samples.pop_front();
        }

        let cutoff = received_at_local_ms - SAMPLE_TTL_MS;
        let best = state
            .samples
            .iter()
            .filter(|s| s.taken_at_ms >= cutoff)
            .min_by_key(|s| s.round_trip_ms)
            .or_else(|| state.samples.iter().min_by_key(|s| s.round_trip_ms))
            .copied();

        if let Some(best) = best {
            state.offset_ms = Some(best.offset_ms);
            state.round_trip_ms = best.round_trip_ms;
        }
    }

    /// Current reading of the server clock, or `None` until the first pong lands.
    pub fn server_now_ms(&self) -> Option<i64> {
        self.offset_ms()
            .map(|offset| (self.local_now_ms)() + offset)
    }

    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.samples.clear();
        state.offset_ms = None;
        state.round_trip_ms = 0;
    }

    /// This device's monotonic reading for stamping outgoing pings.
    pub fn now_ms(&self) -> i64 {
        (self.local_now_ms)()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    fn test_clock() -> (Arc<AtomicI64>, ServerClock<impl Fn() -> i64>) {
        let now = Arc::new(AtomicI64::new(1_000));
        let reader = Arc::clone(&now);
        let clock = ServerClock::with_source(move || reader.load(Ordering::Relaxed));
        (now, clock)
    }

    #[test]
    fn knows_nothing_until_first_round_trip() {
        let (_, clock) = test_clock();
        assert!(!clock.synced());
        assert_eq!(None, clock.server_now_ms());
    }

    #[test]
    fn one_exchange_gives_offset_at_midpoint() {
        let (_, clock) = test_clock();
        clock.record(100, 5_150, 200);
        assert!(clock.synced());
        assert_eq!(Some(5_000), clock.offset_ms());
        assert_eq!(100, clock.round_trip_ms());
    }

    #[test]
    fn shortest_round_trip_wins_not_most_recent() {
        let (_, clock) = test_clock();
        clock.record(0, 5_010, 20);
        clock.record(100, 5_400, 700);
        assert_eq!(Some(5_000), clock.offset_ms());
        assert_eq!(20, clock.round_trip_ms());
    }

    #[test]
    fn stale_best_sample_is_dropped() {
        let (_, clock) = test_clock();
        clock.record(0, 5_010, 20);
        clock.record(300_000, 306_200, 300_400);
        assert_eq!(Some(6_000), clock.offset_ms());
        assert_eq!(400, clock.round_trip_ms());
    }

    #[test]
    fn server_reading_is_local_plus_offset() {
        let (now, clock) = test_clock();
        clock.record(100, 5_150, 200);
        now.store(900, Ordering::Relaxed);
        assert_eq!(Some(5_900), clock.server_now_ms());
    }

    #[test]
    fn reset_forgets_everything() {
        let (_, clock) = test_clock();
        clock.record(100, 5_150, 200);
        clock.reset();
        assert!(!clock.synced());
        assert_eq!(0, clock.round_trip_ms());
    }
}
