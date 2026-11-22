//! Pure-Rust pitch-preserving WSOLA time-stretcher (`0.5x..=2.0x`) for playback speed.
//!
//! Runs on the decode thread strictly separate from `rubato` (which resamples sample rates
//! and would shift pitch if abused for playback speed). Passthrough is zero-cost at `1.0x`.

/// Minimum supported playback speed.
pub const MIN_SPEED: f32 = 0.5;
/// Maximum supported playback speed.
pub const MAX_SPEED: f32 = 2.0;

/// Pitch-preserving waveform similarity overlap-add (WSOLA) time-stretcher.
pub struct SonicStretcher {
    sample_rate: u32,
    channels: usize,
    speed: f32,
    window_frames: usize,
    search_frames: usize,
    input_buf: Vec<f32>,
    prev_tail: Vec<f32>,
    read_pos_f: f64,
}

impl Default for SonicStretcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SonicStretcher {
    pub fn new() -> Self {
        let mut s = Self {
            sample_rate: 48_000,
            channels: 2,
            speed: 1.0,
            window_frames: 0,
            search_frames: 0,
            input_buf: Vec::new(),
            prev_tail: Vec::new(),
            read_pos_f: 0.0,
        };
        s.configure(48_000, 2);
        s
    }

    pub fn configure(&mut self, sample_rate: u32, channels: usize) {
        self.sample_rate = sample_rate.max(8_000);
        self.channels = channels.max(1);
        // 30 ms analysis window (15 ms overlap-add hop) with 8 ms correlation search window.
        let win = ((self.sample_rate as usize * 30) / 1_000).max(256);
        self.window_frames = win + (win % 2); // even so half_win is exact
        self.search_frames = ((self.sample_rate as usize * 8) / 1_000).max(32);
        self.reset();
    }

    pub fn set_speed(&mut self, speed: f32) {
        let clamped = if speed.is_finite() {
            speed.clamp(MIN_SPEED, MAX_SPEED)
        } else {
            1.0
        };
        if (clamped - 1.0).abs() < 1e-3 && (self.speed - 1.0).abs() >= 1e-3 {
            self.reset();
        }
        self.speed = clamped;
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    pub fn is_passthrough(&self) -> bool {
        (self.speed - 1.0).abs() < 1e-3
    }

    /// Approximate algorithmic latency in output frames when stretching is active.
    pub fn latency_frames(&self) -> usize {
        if self.is_passthrough() {
            0
        } else {
            self.window_frames / 2
        }
    }

    pub fn reset(&mut self) {
        self.input_buf.clear();
        self.prev_tail.clear();
        self.read_pos_f = 0.0;
    }

    /// Appends interleaved `f32` frames and writes time-stretched output into `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        if !input.is_empty() {
            self.input_buf.extend_from_slice(input);
        }

        let ch = self.channels;
        let half = self.window_frames / 2;
        let hop_out = half;
        let hop_in = f64::from(hop_out as u32) * f64::from(self.speed);
        let available_frames = self.input_buf.len() / ch;
        let needed_ahead = self.window_frames + self.search_frames;

        while (self.read_pos_f.floor() as usize) + needed_ahead <= available_frames {
            let nominal = self.read_pos_f.round() as usize;
            let best_offset = if self.prev_tail.len() == half * ch {
                self.find_best_offset(nominal, half)
            } else {
                nominal
            };

            let start_sample = best_offset * ch;
            if self.prev_tail.len() == half * ch {
                // Crossfade first half of new window with stored second half of previous window.
                for f in 0..half {
                    let w = f as f32 / half as f32;
                    let inv_w = 1.0 - w;
                    for c in 0..ch {
                        let a = self.prev_tail[f * ch + c];
                        let b = self.input_buf[start_sample + f * ch + c];
                        out.push(a * inv_w + b * w);
                    }
                }
            } else {
                out.extend_from_slice(&self.input_buf[start_sample..start_sample + half * ch]);
            }

            // Store second half of chosen window as next overlap tail.
            let tail_start = start_sample + half * ch;
            self.prev_tail.clear();
            self.prev_tail
                .extend_from_slice(&self.input_buf[tail_start..tail_start + half * ch]);

            self.read_pos_f += hop_in;
        }

        // Discard already-consumed prefix of `input_buf` to keep memory strictly bounded.
        let safe_drop_frames =
            (self.read_pos_f.floor() as usize).saturating_sub(self.search_frames);
        if safe_drop_frames > 0 {
            let drop_samples = (safe_drop_frames * ch).min(self.input_buf.len());
            self.input_buf.drain(..drop_samples);
            self.read_pos_f -= (drop_samples / ch) as f64;
        }
    }

    /// Flushes remaining buffered audio at EOF.
    pub fn flush_eof(&mut self, out: &mut Vec<f32>) {
        out.clear();
        if self.is_passthrough() {
            return;
        }
        if !self.prev_tail.is_empty() {
            out.extend_from_slice(&self.prev_tail);
            self.prev_tail.clear();
        }
        self.input_buf.clear();
        self.read_pos_f = 0.0;
    }

    fn find_best_offset(&self, nominal: usize, half: usize) -> usize {
        let ch = self.channels;
        let total_frames = self.input_buf.len() / ch;
        let min_off = nominal.saturating_sub(self.search_frames / 2);
        let max_off =
            (nominal + self.search_frames / 2).min(total_frames.saturating_sub(self.window_frames));
        let mut best_off = nominal.clamp(min_off, max_off);
        let mut best_score = f32::NEG_INFINITY;

        // Subsample correlation by 2 frames for fast O(N) decode-thread CPU budget.
        let cmp_len = half.min(256);
        for cand in min_off..=max_off {
            let base = cand * ch;
            let mut dot = 0.0_f32;
            let mut energy_b = 1e-9_f32;
            let mut f = 0;
            while f < cmp_len {
                let a = self.prev_tail[f * ch];
                let b = self.input_buf[base + f * ch];
                dot += a * b;
                energy_b += b * b;
                f += 2;
            }
            let score = dot / energy_b.sqrt();
            if score > best_score {
                best_score = score;
                best_off = cand;
            }
        }
        best_off
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn estimate_frequency_hz(samples: &[f32], channels: usize, sr: u32) -> f64 {
        let frames = samples.len() / channels;
        let mut crossings = 0usize;
        for f in 1..frames {
            let prev = samples[(f - 1) * channels];
            let curr = samples[f * channels];
            if prev <= 0.0 && curr > 0.0 {
                crossings += 1;
            }
        }
        crossings as f64 * f64::from(sr) / frames as f64
    }

    #[test]
    fn stretches_duration_at_1_5x_while_preserving_440hz_pitch() {
        let sr = 48_000;
        let frames = sr as usize; // 1.0 s of 440 Hz stereo sine
        let mut input = vec![0.0_f32; frames * 2];
        for f in 0..frames {
            let s = (0.5 * (2.0 * PI * 440.0 * f as f64 / f64::from(sr)).sin()) as f32;
            input[f * 2] = s;
            input[f * 2 + 1] = s;
        }

        let mut stretcher = SonicStretcher::new();
        stretcher.configure(sr, 2);
        stretcher.set_speed(1.5);

        let mut out = Vec::new();
        stretcher.process(&input, &mut out);
        let mut tail = Vec::new();
        stretcher.flush_eof(&mut tail);
        out.extend_from_slice(&tail);

        let out_frames = out.len() / 2;
        let expected_frames = (frames as f64 / 1.5) as usize;
        assert!(
            (out_frames as isize - expected_frames as isize).abs() < 4_000,
            "out_frames={out_frames}, expected~{expected_frames}"
        );

        let measured_hz = estimate_frequency_hz(&out, 2, sr);
        assert!(
            (measured_hz - 440.0).abs() < 20.0,
            "pitch drifted: measured {measured_hz} Hz (expected ~440 Hz)"
        );
    }

    #[test]
    fn unity_speed_is_bit_exact_passthrough() {
        let mut stretcher = SonicStretcher::new();
        stretcher.configure(48_000, 2);
        stretcher.set_speed(1.0);
        assert!(stretcher.is_passthrough());
        let input = vec![0.1, -0.2, 0.3, -0.4];
        let mut out = Vec::new();
        stretcher.process(&input, &mut out);
        assert_eq!(out, input);
    }
}
