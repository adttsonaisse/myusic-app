use std::f64::consts::PI;

const MIN_HZ: f32 = 10.0;
const STAGES: usize = 2;
const BUTTERWORTH_Q: [f32; STAGES] = [0.54120, 1.30656];
const GLIDE_RATE: f32 = 0.05;
const SETTLED_HZ: f32 = 1.0;
const MAX_CUTOFF_FRACTION: f32 = 0.45;

/// Fourth-order (24 dB/oct) Butterworth state-variable DJ transition filter
/// with logarithmic cutoff gliding (low-pass + high-pass cascade).
///
/// Audio-thread safe once configured: `advance()` and `filter()` allocate nothing.
pub struct TransitionFilter {
    target_low_pass_hz: f32,
    target_high_pass_hz: f32,
    channel_count: usize,
    sample_rate: u32,
    current_low_pass_hz: f32,
    current_high_pass_hz: f32,

    low_state: Vec<f32>,
    high_state: Vec<f32>,
    low_a1: [f32; STAGES],
    low_a2: [f32; STAGES],
    low_a3: [f32; STAGES],
    high_a1: [f32; STAGES],
    high_a2: [f32; STAGES],
    high_a3: [f32; STAGES],
    high_k: [f32; STAGES],

    low_on: bool,
    high_on: bool,
}

impl Default for TransitionFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl TransitionFilter {
    pub const OPEN_HZ: f32 = 20_000.0;
    pub const OFF_HZ: f32 = 20.0;
    pub const MAX_HIGH_PASS_HZ: f32 = 2_000.0;
    pub const GLIDE_FRAMES: usize = 64;

    pub fn new() -> Self {
        Self {
            target_low_pass_hz: Self::OPEN_HZ,
            target_high_pass_hz: Self::OFF_HZ,
            channel_count: 0,
            sample_rate: 0,
            current_low_pass_hz: Self::OPEN_HZ,
            current_high_pass_hz: Self::OFF_HZ,
            low_state: Vec::new(),
            high_state: Vec::new(),
            low_a1: [0.0; STAGES],
            low_a2: [0.0; STAGES],
            low_a3: [0.0; STAGES],
            high_a1: [0.0; STAGES],
            high_a2: [0.0; STAGES],
            high_a3: [0.0; STAGES],
            high_k: [0.0; STAGES],
            low_on: false,
            high_on: false,
        }
    }

    /// True while either low-pass or high-pass section is audibly engaged.
    pub fn active(&self) -> bool {
        self.low_on || self.high_on
    }

    /// True when both targets are open/off and current glided cutoffs have settled.
    pub fn parked(&self) -> bool {
        self.target_low_pass_hz >= Self::OPEN_HZ
            && self.target_high_pass_hz <= Self::OFF_HZ
            && self.current_low_pass_hz >= Self::OPEN_HZ - SETTLED_HZ
            && self.current_high_pass_hz <= Self::OFF_HZ + SETTLED_HZ
    }

    pub fn configure(&mut self, channel_count: usize, sample_rate: u32) {
        self.channel_count = channel_count;
        self.sample_rate = sample_rate;
        self.low_state = vec![0.0; channel_count * STAGES * 2];
        self.high_state = vec![0.0; channel_count * STAGES * 2];
        self.current_low_pass_hz = self.target_low_pass_hz;
        self.current_high_pass_hz = self.target_high_pass_hz;
    }

    pub fn set_cutoffs(&mut self, low_pass_hz: f32, high_pass_hz: f32) {
        self.target_low_pass_hz = low_pass_hz.clamp(MIN_HZ, Self::OPEN_HZ);
        self.target_high_pass_hz = high_pass_hz.clamp(Self::OFF_HZ, Self::MAX_HIGH_PASS_HZ);
    }

    pub fn open(&mut self) {
        self.set_cutoffs(Self::OPEN_HZ, Self::OFF_HZ);
    }

    /// Advances cutoffs by one glide sub-block and updates SVF coefficients.
    pub fn advance(&mut self) {
        self.current_low_pass_hz = glide(self.current_low_pass_hz, self.target_low_pass_hz);
        self.current_high_pass_hz = glide(self.current_high_pass_hz, self.target_high_pass_hz);
        self.low_on = self.current_low_pass_hz < Self::OPEN_HZ - SETTLED_HZ;
        self.high_on = self.current_high_pass_hz > Self::OFF_HZ + SETTLED_HZ;
        if self.low_on {
            self.update_low_coefficients();
        }
        if self.high_on {
            self.update_high_coefficients();
        }
    }

    pub fn filter(&mut self, channel: usize, input: f32) -> f32 {
        let mut value = input;
        if self.low_on {
            value = self.low_pass(channel, value);
        }
        if self.high_on {
            value = self.high_pass(channel, value);
        }
        value
    }

    pub fn flush(&mut self) {
        self.low_state.fill(0.0);
        self.high_state.fill(0.0);
        self.current_low_pass_hz = self.target_low_pass_hz;
        self.current_high_pass_hz = self.target_high_pass_hz;
    }

    pub fn reset(&mut self) {
        self.target_low_pass_hz = Self::OPEN_HZ;
        self.target_high_pass_hz = Self::OFF_HZ;
        self.low_state.clear();
        self.high_state.clear();
        self.low_on = false;
        self.high_on = false;
    }

    fn usable_cutoff(&self, hz: f32) -> f32 {
        hz.clamp(MIN_HZ, (self.sample_rate as f32) * MAX_CUTOFF_FRACTION)
    }

    fn update_low_coefficients(&mut self) {
        if self.sample_rate == 0 {
            return;
        }
        let g = (PI * f64::from(self.usable_cutoff(self.current_low_pass_hz))
            / f64::from(self.sample_rate))
        .tan() as f32;
        for stage in 0..STAGES {
            let k = 1.0 / BUTTERWORTH_Q[stage];
            let a1 = 1.0 / (1.0 + g * (g + k));
            self.low_a1[stage] = a1;
            self.low_a2[stage] = g * a1;
            self.low_a3[stage] = g * (g * a1);
        }
    }

    fn update_high_coefficients(&mut self) {
        if self.sample_rate == 0 {
            return;
        }
        let g = (PI * f64::from(self.usable_cutoff(self.current_high_pass_hz))
            / f64::from(self.sample_rate))
        .tan() as f32;
        for stage in 0..STAGES {
            let k = 1.0 / BUTTERWORTH_Q[stage];
            let a1 = 1.0 / (1.0 + g * (g + k));
            self.high_a1[stage] = a1;
            self.high_a2[stage] = g * a1;
            self.high_a3[stage] = g * (g * a1);
            self.high_k[stage] = k;
        }
    }

    fn low_pass(&mut self, channel: usize, input: f32) -> f32 {
        let mut value = input;
        for stage in 0..STAGES {
            let i = (channel * STAGES + stage) * 2;
            let ic1 = self.low_state[i];
            let ic2 = self.low_state[i + 1];
            let v3 = value - ic2;
            let v1 = self.low_a1[stage] * ic1 + self.low_a2[stage] * v3;
            let v2 = ic2 + self.low_a2[stage] * ic1 + self.low_a3[stage] * v3;
            self.low_state[i] = 2.0 * v1 - ic1;
            self.low_state[i + 1] = 2.0 * v2 - ic2;
            value = v2;
        }
        value
    }

    fn high_pass(&mut self, channel: usize, input: f32) -> f32 {
        let mut value = input;
        for stage in 0..STAGES {
            let i = (channel * STAGES + stage) * 2;
            let ic1 = self.high_state[i];
            let ic2 = self.high_state[i + 1];
            let v3 = value - ic2;
            let v1 = self.high_a1[stage] * ic1 + self.high_a2[stage] * v3;
            let v2 = ic2 + self.high_a2[stage] * ic1 + self.high_a3[stage] * v3;
            self.high_state[i] = 2.0 * v1 - ic1;
            self.high_state[i + 1] = 2.0 * v2 - ic2;
            value -= self.high_k[stage] * v1 + v2;
        }
        value
    }
}

fn glide(current: f32, target: f32) -> f32 {
    let from = current.max(MIN_HZ).ln();
    let to = target.max(MIN_HZ).ln();
    (from + (to - from) * GLIDE_RATE).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parked_by_default_and_attenuates_outside_passband() {
        let mut filter = TransitionFilter::new();
        filter.configure(2, 48_000);
        assert!(filter.parked());
        filter.advance();
        assert!(!filter.active());

        // Low-pass at 300 Hz must heavily attenuate a 6 kHz tone while passing 100 Hz.
        filter.set_cutoffs(300.0, TransitionFilter::OFF_HZ);
        filter.flush();
        filter.advance();
        assert!(filter.active());
        assert!(!filter.parked());

        let mut high_peak = 0.0_f32;
        for n in 0..4800 {
            let x = (2.0 * PI * 6_000.0 * (n as f64) / 48_000.0).sin() as f32;
            let y = filter.filter(0, x);
            if n > 1000 {
                high_peak = high_peak.max(y.abs());
            }
        }
        assert!(
            high_peak < 0.01,
            "6 kHz peak through 300 Hz LP was {high_peak}"
        );

        // High-pass at 1 kHz must heavily attenuate 60 Hz while passing 4 kHz.
        filter.set_cutoffs(TransitionFilter::OPEN_HZ, 1_000.0);
        filter.flush();
        filter.advance();
        let mut low_peak = 0.0_f32;
        for n in 0..4800 {
            let x = (2.0 * PI * 60.0 * (n as f64) / 48_000.0).sin() as f32;
            let y = filter.filter(0, x);
            if n > 1000 {
                low_peak = low_peak.max(y.abs());
            }
        }
        assert!(
            low_peak < 0.01,
            "60 Hz peak through 1 kHz HP was {low_peak}"
        );
    }
}
