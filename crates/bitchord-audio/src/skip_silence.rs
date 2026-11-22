//! Silence skipper honoring BitChord's 1.0-second minimum silence threshold
//! (`MIN_SILENCE_US = 1_000_000`) and tracking exact skipped source frames so
//! synced lyrics never drift from vocals.

/// Minimum contiguous silence before skipping begins (1.0 second, matching upstream).
pub const MIN_SILENCE_MS: u64 = 1_000;
/// Short crossfade ramp (20 ms) applied at silence entry/exit to eliminate clicks.
pub const FADE_MS: u64 = 20;
/// Default RMS/peak amplitude threshold (~ -48 dBFS).
pub const DEFAULT_THRESHOLD: f32 = 0.004;

/// Runs on the decode thread before resampling/stretching.
pub struct SkipSilenceProcessor {
    pub enabled: bool,
    sample_rate: u32,
    channels: usize,
    threshold: f32,
    min_silence_frames: usize,
    fade_frames: usize,

    consecutive_silent_frames: usize,
    skipping: bool,
    fade_in_remaining: usize,
    skipped_source_frames: u64,
}

impl Default for SkipSilenceProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl SkipSilenceProcessor {
    pub fn new() -> Self {
        Self {
            enabled: false,
            sample_rate: 48_000,
            channels: 2,
            threshold: DEFAULT_THRESHOLD,
            min_silence_frames: 48_000,
            fade_frames: 960,
            consecutive_silent_frames: 0,
            skipping: false,
            fade_in_remaining: 0,
            skipped_source_frames: 0,
        }
    }

    pub fn configure(&mut self, sample_rate: u32, channels: usize) {
        self.sample_rate = sample_rate.max(1);
        self.channels = channels.max(1);
        self.min_silence_frames = ((u64::from(self.sample_rate) * MIN_SILENCE_MS) / 1_000) as usize;
        self.fade_frames = (((u64::from(self.sample_rate) * FADE_MS) / 1_000) as usize).max(1);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.consecutive_silent_frames = 0;
        self.skipping = false;
        self.fade_in_remaining = 0;
        self.skipped_source_frames = 0;
    }

    /// Total source frames dropped so far on the current track (used to keep synced lyrics aligned).
    pub fn skipped_source_frames(&self) -> u64 {
        self.skipped_source_frames
    }

    pub fn skipped_duration_ms(&self) -> u64 {
        (self.skipped_source_frames * 1_000) / u64::from(self.sample_rate.max(1))
    }

    /// Filters interleaved `f32` frames into `out`, skipping dead air past `MIN_SILENCE_MS`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        if !self.enabled {
            out.extend_from_slice(input);
            return;
        }
        let ch = self.channels;
        let frames = input.len() / ch;
        out.reserve(input.len());

        for f in 0..frames {
            let base = f * ch;
            let frame_slice = &input[base..base + ch];
            let peak = frame_slice.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));

            if peak < self.threshold {
                self.consecutive_silent_frames += 1;
                if self.skipping {
                    self.skipped_source_frames += 1;
                    continue;
                }
                if self.consecutive_silent_frames >= self.min_silence_frames {
                    self.skipping = true;
                    self.skipped_source_frames += 1;
                    continue;
                }
                out.extend_from_slice(frame_slice);
            } else {
                if self.skipping {
                    self.skipping = false;
                    self.fade_in_remaining = self.fade_frames;
                }
                self.consecutive_silent_frames = 0;

                if self.fade_in_remaining > 0 {
                    let progress = 1.0 - (self.fade_in_remaining as f32 / self.fade_frames as f32);
                    for &s in frame_slice {
                        out.push(s * progress);
                    }
                    self.fade_in_remaining -= 1;
                } else {
                    out.extend_from_slice(frame_slice);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_short_pauses_under_one_second_and_collapses_long_dead_air() {
        let sr = 10_000;
        let mut proc = SkipSilenceProcessor::new();
        proc.configure(sr, 2);
        proc.enabled = true;

        // 0.5 s audio + 0.6 s pause + 0.5 s audio -> nothing skipped (< 1.0 s).
        let mut short_pause = vec![0.5_f32; 5_000 * 2];
        short_pause.extend(std::iter::repeat_n(0.0_f32, 6_000 * 2));
        short_pause.extend(std::iter::repeat_n(0.5_f32, 5_000 * 2));
        let mut out = Vec::new();
        proc.process(&short_pause, &mut out);
        assert_eq!(out.len(), short_pause.len());
        assert_eq!(proc.skipped_source_frames(), 0);

        // 2.5 s silence -> keeps first 1.0 s threshold, skips ~1.5 s.
        proc.reset();
        let mut long_silence = vec![0.0_f32; 25_000 * 2];
        long_silence.extend(std::iter::repeat_n(0.5_f32, 2_000 * 2));
        proc.process(&long_silence, &mut out);
        assert!(proc.skipped_duration_ms() >= 1_450);
        assert!(out.len() < long_silence.len());
    }
}
