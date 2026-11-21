use std::f64::consts::PI;

use crate::eq::{EqCurve, EqLayout, FilterKind};
use crate::limiter::PeakLimiter;

const GLIDE_FRAMES: usize = 64;
const GLIDE_RATE: f32 = 0.08;
const SETTLED_DB: f32 = 0.01;
const SETTLED_BALANCE: f32 = 0.0005;
const MIN_Q: f32 = 0.05;
const MIN_HZ: f32 = 10.0;
const MAX_FREQUENCY_FRACTION: f32 = 0.45;
const DENORMAL_FLOOR: f32 = 1e-12;

/// Clamps and quantizes a normalized `f32` PCM sample to signed 16-bit PCM,
/// matching Java's `Math.round` (half toward +∞) as used in BitChord's `PcmBoundary`.
pub fn clamp16_from_float(f: f32) -> i16 {
    if f.is_nan() {
        return 0;
    }
    let scaled = f * 32_768.0;
    if scaled <= -32_768.0 {
        return i16::MIN;
    }
    if scaled >= 32_767.0 {
        return i16::MAX;
    }
    (scaled + 0.5).floor() as i16
}

#[derive(Debug, Clone)]
struct Tuning {
    enabled: bool,
    curve: EqCurve,
    balance: f32,
}

impl Tuning {
    fn off() -> Self {
        Self {
            enabled: false,
            curve: EqCurve::flat(),
            balance: 0.0,
        }
    }
}

/// 10-slot trapezoidal state-variable equalizer with smooth parameter gliding,
/// make-up attenuation, stereo balance trim, and 16-bit peak limiting.
///
/// Audio-thread safe once configured: `process_f32_interleaved` and
/// `process_i16_interleaved` perform zero heap allocations.
pub struct EqualizerProcessor {
    target: Tuning,
    channel_count: usize,
    sample_rate: u32,

    current_gain_db: [f32; EqLayout::SLOTS],
    current_q: [f32; EqLayout::SLOTS],
    current_preamp_db: f32,
    current_balance: f32,

    coeff_a1: [f32; EqLayout::SLOTS],
    coeff_a2: [f32; EqLayout::SLOTS],
    coeff_a3: [f32; EqLayout::SLOTS],
    mix_input: [f32; EqLayout::SLOTS],
    mix_band: [f32; EqLayout::SLOTS],
    mix_low: [f32; EqLayout::SLOTS],

    active_slots: [usize; EqLayout::SLOTS],
    running: [bool; EqLayout::SLOTS],

    state: Vec<f32>,
    channel_gain: Vec<f32>,
    frame: Vec<f32>,
    limiter: PeakLimiter,
    is_shaping: bool,
}

impl Default for EqualizerProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl EqualizerProcessor {
    pub fn new() -> Self {
        Self {
            target: Tuning::off(),
            channel_count: 0,
            sample_rate: 0,
            current_gain_db: [0.0; EqLayout::SLOTS],
            current_q: [0.707; EqLayout::SLOTS],
            current_preamp_db: 0.0,
            current_balance: 0.0,
            coeff_a1: [0.0; EqLayout::SLOTS],
            coeff_a2: [0.0; EqLayout::SLOTS],
            coeff_a3: [0.0; EqLayout::SLOTS],
            mix_input: [0.0; EqLayout::SLOTS],
            mix_band: [0.0; EqLayout::SLOTS],
            mix_low: [0.0; EqLayout::SLOTS],
            active_slots: [0; EqLayout::SLOTS],
            running: [false; EqLayout::SLOTS],
            state: Vec::new(),
            channel_gain: Vec::new(),
            frame: Vec::new(),
            limiter: PeakLimiter::new(),
            is_shaping: false,
        }
    }

    /// Updates target tuning; `enabled = false` glides smoothly back to flat & centered.
    pub fn set_tuning(&mut self, enabled: bool, curve: EqCurve, balance: f32) {
        self.target = if enabled {
            Tuning {
                enabled: true,
                curve,
                balance: balance.clamp(-1.0, 1.0),
            }
        } else {
            Tuning::off()
        };
    }

    pub fn is_enabled(&self) -> bool {
        self.target.enabled
    }

    pub fn is_shaping(&self) -> bool {
        self.is_shaping
    }

    /// Configures internal state buffers for `sample_rate` and `channel_count`.
    pub fn configure(&mut self, sample_rate: u32, channel_count: usize) {
        self.sample_rate = sample_rate;
        self.channel_count = channel_count;
        let required = channel_count * EqLayout::SLOTS * 2;
        if self.state.len() != required {
            self.state.resize(required, 0.0);
        }
        if self.channel_gain.len() != channel_count {
            self.channel_gain.resize(channel_count, 1.0);
        }
        if self.frame.len() != channel_count {
            self.frame.resize(channel_count, 0.0);
        }
        self.running.fill(false);
        self.limiter.configure(sample_rate);
        self.snap_to_target();
    }

    /// Flushes filter integrator states and snaps coefficients immediately to target.
    pub fn flush(&mut self) {
        self.state.fill(0.0);
        self.running.fill(false);
        self.limiter.reset();
        self.snap_to_target();
    }

    /// Processes interleaved `f32` PCM samples in-place, preserving headroom above `±1.0`.
    pub fn process_f32_interleaved(&mut self, samples: &mut [f32]) {
        if self.channel_count == 0 || self.sample_rate == 0 {
            return;
        }
        let frame_count = samples.len() / self.channel_count;
        if frame_count == 0 {
            return;
        }

        if Self::is_flat(&self.target) && self.is_settled(&self.target) {
            self.is_shaping = false;
            return;
        }
        self.is_shaping = true;

        let mut remaining = frame_count;
        let mut frame_offset = 0usize;
        while remaining > 0 {
            let sub_block = remaining.min(GLIDE_FRAMES);
            self.glide_towards_target();
            let active = self.prepare_sections();
            self.prepare_channel_gains();

            for f in 0..sub_block {
                let base_idx = (frame_offset + f) * self.channel_count;
                for channel in 0..self.channel_count {
                    let mut sample = samples[base_idx + channel];
                    for index in 0..active {
                        let slot = self.active_slots[index];
                        sample = self.section(slot, channel, sample);
                    }
                    samples[base_idx + channel] = sample * self.channel_gain[channel];
                }
            }
            self.flush_denormals(active);
            frame_offset += sub_block;
            remaining -= sub_block;
        }
    }

    /// Processes interleaved signed 16-bit PCM samples in-place with integrated [`PeakLimiter`].
    pub fn process_i16_interleaved(&mut self, samples: &mut [i16]) {
        if self.channel_count == 0 || self.sample_rate == 0 {
            return;
        }
        let frame_count = samples.len() / self.channel_count;
        if frame_count == 0 {
            return;
        }

        if Self::is_flat(&self.target) && self.is_settled(&self.target) {
            self.limiter.reset();
            self.is_shaping = false;
            return;
        }
        self.is_shaping = true;

        let inv_scale = 1.0_f32 / 32_768.0;
        let mut remaining = frame_count;
        let mut frame_offset = 0usize;
        while remaining > 0 {
            let sub_block = remaining.min(GLIDE_FRAMES);
            self.glide_towards_target();
            let active = self.prepare_sections();
            self.prepare_channel_gains();

            for f in 0..sub_block {
                let base_idx = (frame_offset + f) * self.channel_count;
                let mut peak = 0.0_f32;
                for channel in 0..self.channel_count {
                    let mut sample = f32::from(samples[base_idx + channel]) * inv_scale;
                    for index in 0..active {
                        let slot = self.active_slots[index];
                        sample = self.section(slot, channel, sample);
                    }
                    sample *= self.channel_gain[channel];
                    self.frame[channel] = sample;
                    let abs_sample = sample.abs();
                    if abs_sample > peak {
                        peak = abs_sample;
                    }
                }
                let gain = self.limiter.gain_for(peak);
                for channel in 0..self.channel_count {
                    samples[base_idx + channel] = clamp16_from_float(self.frame[channel] * gain);
                }
            }
            self.flush_denormals(active);
            frame_offset += sub_block;
            remaining -= sub_block;
        }
    }

    fn snap_to_target(&mut self) {
        self.current_gain_db = self.target.curve.gains_db;
        self.current_q = self.target.curve.qs;
        self.current_preamp_db = self.target.curve.preamp_db;
        self.current_balance = self.target.balance;
    }

    fn glide_towards_target(&mut self) {
        for slot in 0..EqLayout::SLOTS {
            self.current_gain_db[slot] =
                linear_glide(self.current_gain_db[slot], self.target.curve.gains_db[slot]);
            self.current_q[slot] =
                geometric_glide(self.current_q[slot], self.target.curve.qs[slot]);
        }
        self.current_preamp_db = linear_glide(self.current_preamp_db, self.target.curve.preamp_db);
        self.current_balance = linear_glide(self.current_balance, self.target.balance);
    }

    fn is_flat(tuning: &Tuning) -> bool {
        tuning.balance.abs() < SETTLED_BALANCE
            && tuning.curve.preamp_db.abs() < SETTLED_DB
            && tuning.curve.gains_db.iter().all(|g| g.abs() < SETTLED_DB)
    }

    fn is_settled(&self, tuning: &Tuning) -> bool {
        if (self.current_balance - tuning.balance).abs() >= SETTLED_BALANCE {
            return false;
        }
        if (self.current_preamp_db - tuning.curve.preamp_db).abs() >= SETTLED_DB {
            return false;
        }
        for slot in 0..EqLayout::SLOTS {
            if (self.current_gain_db[slot] - tuning.curve.gains_db[slot]).abs() >= SETTLED_DB {
                return false;
            }
        }
        true
    }

    fn prepare_sections(&mut self) -> usize {
        let mut active = 0usize;
        for slot in 0..EqLayout::SLOTS {
            if self.current_gain_db[slot].abs() >= SETTLED_DB {
                self.update_coefficients(slot);
                self.active_slots[active] = slot;
                active += 1;
                self.running[slot] = true;
            } else if self.running[slot] {
                self.clear_state(slot);
                self.running[slot] = false;
            }
        }
        active
    }

    fn update_coefficients(&mut self, slot: usize) {
        if self.sample_rate == 0 {
            return;
        }
        let spec = EqLayout::SLOTS_SPEC[slot];
        let a = 10.0_f32.powf(self.current_gain_db[slot] / 40.0);
        let q = self.current_q[slot].max(MIN_Q);
        let usable_hz = spec
            .frequency_hz
            .clamp(MIN_HZ, (self.sample_rate as f32) * MAX_FREQUENCY_FRACTION);
        let base = (PI * f64::from(usable_hz) / f64::from(self.sample_rate)).tan() as f32;

        let (g, k) = match spec.kind {
            FilterKind::Bell => {
                let k = 1.0 / (q * a);
                self.mix_input[slot] = 1.0;
                self.mix_band[slot] = k * (a * a - 1.0);
                self.mix_low[slot] = 0.0;
                (base, k)
            }
            FilterKind::LowShelf => {
                let k = 1.0 / q;
                self.mix_input[slot] = 1.0;
                self.mix_band[slot] = k * (a - 1.0);
                self.mix_low[slot] = a * a - 1.0;
                (base / a.sqrt(), k)
            }
            FilterKind::HighShelf => {
                let k = 1.0 / q;
                self.mix_input[slot] = a * a;
                self.mix_band[slot] = k * (1.0 - a) * a;
                self.mix_low[slot] = 1.0 - a * a;
                (base * a.sqrt(), k)
            }
        };

        let d = 1.0 / (1.0 + g * (g + k));
        self.coeff_a1[slot] = d;
        self.coeff_a2[slot] = g * d;
        self.coeff_a3[slot] = g * (g * d);
    }

    fn prepare_channel_gains(&mut self) {
        let preamp = 10.0_f32.powf(self.current_preamp_db / 20.0);
        if self.channel_count == 2 {
            self.channel_gain[0] = preamp * (1.0_f32 - self.current_balance).min(1.0);
            self.channel_gain[1] = preamp * (1.0_f32 + self.current_balance).min(1.0);
        } else {
            self.channel_gain.fill(preamp);
        }
    }

    fn section(&mut self, slot: usize, channel: usize, input: f32) -> f32 {
        let i = (channel * EqLayout::SLOTS + slot) * 2;
        let ic1 = self.state[i];
        let ic2 = self.state[i + 1];
        let v3 = input - ic2;
        let v1 = self.coeff_a1[slot] * ic1 + self.coeff_a2[slot] * v3;
        let v2 = ic2 + self.coeff_a2[slot] * ic1 + self.coeff_a3[slot] * v3;
        self.state[i] = 2.0 * v1 - ic1;
        self.state[i + 1] = 2.0 * v2 - ic2;
        self.mix_input[slot] * input + self.mix_band[slot] * v1 + self.mix_low[slot] * v2
    }

    fn clear_state(&mut self, slot: usize) {
        for channel in 0..self.channel_count {
            let i = (channel * EqLayout::SLOTS + slot) * 2;
            self.state[i] = 0.0;
            self.state[i + 1] = 0.0;
        }
    }

    fn flush_denormals(&mut self, active: usize) {
        for index in 0..active {
            let slot = self.active_slots[index];
            for channel in 0..self.channel_count {
                let i = (channel * EqLayout::SLOTS + slot) * 2;
                if self.state[i].abs() < DENORMAL_FLOOR {
                    self.state[i] = 0.0;
                }
                if self.state[i + 1].abs() < DENORMAL_FLOOR {
                    self.state[i + 1] = 0.0;
                }
            }
        }
    }
}

fn linear_glide(current: f32, target: f32) -> f32 {
    current + (target - current) * GLIDE_RATE
}

fn geometric_glide(current: f32, target: f32) -> f32 {
    let from = current.max(MIN_Q).ln();
    let to = target.max(MIN_Q).ln();
    (from + (to - from) * GLIDE_RATE).exp()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;
    use crate::eq::{manual_curve, tone_curve};
    use crate::preset::EqualizerPreset;

    const SAMPLE_RATE: u32 = 48_000;
    const TONE_FRAMES: usize = (SAMPLE_RATE as usize) * 2;
    const SETTLE_FRAMES: usize = (SAMPLE_RATE as usize) / 2;
    const CHUNK_SAMPLES: usize = 4_096 * 2;
    const AMPLITUDE: f64 = 8_000.0;
    const BOOST_DB: f32 = 6.0;
    const TOLERANCE_DB: f64 = 0.4;
    const TILT_TOLERANCE_DB: f64 = 0.8;

    fn stereo_tone(hz: f64, amplitude: f64, frames: usize) -> Vec<i16> {
        let mut samples = vec![0i16; frames * 2];
        for frame in 0..frames {
            let v = (amplitude * (2.0 * PI * hz * (frame as f64) / f64::from(SAMPLE_RATE)).sin())
                as i16;
            samples[frame * 2] = v;
            samples[frame * 2 + 1] = v;
        }
        samples
    }

    fn run_chunks(processor: &mut EqualizerProcessor, input: &[i16]) -> Vec<i16> {
        processor.configure(SAMPLE_RATE, 2);
        processor.flush();
        let mut output = input.to_vec();
        for chunk in output.chunks_mut(CHUNK_SAMPLES) {
            processor.process_i16_interleaved(chunk);
        }
        output
    }

    fn amplitude_at(samples: &[i16], hz: f64) -> f64 {
        let mut real = 0.0_f64;
        let mut imag = 0.0_f64;
        let mut count = 0usize;
        for frame in SETTLE_FRAMES..(samples.len() / 2) {
            let angle = 2.0 * PI * hz * (frame as f64) / f64::from(SAMPLE_RATE);
            let value = f64::from(samples[frame * 2]);
            real += value * angle.cos();
            imag += value * angle.sin();
            count += 1;
        }
        2.0 * (real * real + imag * imag).sqrt() / (count as f64)
    }

    fn measure(processor: &mut EqualizerProcessor, hz: f64) -> f64 {
        let input = stereo_tone(hz, AMPLITUDE, TONE_FRAMES);
        let output = run_chunks(processor, &input);
        20.0 * (amplitude_at(&output, hz) / amplitude_at(&input, hz)).log10()
    }

    fn measure_band(band: usize, gain_db: f32, at_hz: f64) -> f64 {
        let mut bands = [0.0_f32; EqLayout::MANUAL_COUNT];
        bands[band] = gain_db;
        let curve = manual_curve(&bands);
        let preamp = f64::from(curve.preamp_db);
        let mut processor = EqualizerProcessor::new();
        processor.set_tuning(true, curve, 0.0);
        measure(&mut processor, at_hz) - preamp
    }

    #[test]
    fn interior_bands_lift_and_cut_their_centres() {
        for band in 1..(EqLayout::MANUAL_COUNT - 1) {
            let hz = f64::from(EqLayout::MANUAL_BANDS_HZ[band]);
            let lifted = measure_band(band, BOOST_DB, hz);
            assert!(
                (lifted - f64::from(BOOST_DB)).abs() <= TOLERANCE_DB,
                "band {band} boost was {lifted} dB"
            );
            let cut = measure_band(band, -BOOST_DB, hz);
            assert!(
                (cut - f64::from(-BOOST_DB)).abs() <= TOLERANCE_DB,
                "band {band} cut was {cut} dB"
            );
        }
    }

    #[test]
    fn end_bands_are_shelves() {
        let low = f64::from(EqLayout::MANUAL_BANDS_HZ[0]);
        assert!((measure_band(0, BOOST_DB, low) - f64::from(BOOST_DB) / 2.0).abs() <= TOLERANCE_DB);
        assert!((measure_band(0, BOOST_DB, 20.0) - f64::from(BOOST_DB)).abs() <= TOLERANCE_DB);

        let top = EqLayout::MANUAL_COUNT - 1;
        let high = f64::from(EqLayout::MANUAL_BANDS_HZ[top]);
        assert!(
            (measure_band(top, BOOST_DB, high) - f64::from(BOOST_DB) / 2.0).abs() <= TOLERANCE_DB
        );
    }

    #[test]
    fn tone_pad_tilt_contour_and_focus() {
        let bright = tone_curve(EqLayout::TONE_STEPS, 0, false);
        let corner = f64::from(EqLayout::TONE_STEPS as f32 * EqLayout::TONE_DB_PER_STEP);
        let mut proc = EqualizerProcessor::new();
        proc.set_tuning(true, bright.clone(), 0.0);
        let low_end = measure(&mut proc, 60.0) - f64::from(bright.preamp_db);
        let high_end = measure(&mut proc, 12_000.0) - f64::from(bright.preamp_db);
        assert!((low_end - (-corner)).abs() <= TILT_TOLERANCE_DB);
        assert!((high_end - corner).abs() <= TILT_TOLERANCE_DB);

        let forward = tone_curve(0, EqLayout::TONE_STEPS, true);
        let mut proc_mid = EqualizerProcessor::new();
        proc_mid.set_tuning(true, forward.clone(), 0.0);
        let mids = measure(&mut proc_mid, 1_000.0) - f64::from(forward.preamp_db);
        let bass = measure(&mut proc_mid, 60.0) - f64::from(forward.preamp_db);
        assert!((mids - corner).abs() <= TOLERANCE_DB);
        assert!(bass.abs() < 0.5);

        let broad_curve = tone_curve(0, EqLayout::TONE_STEPS, false);
        let focused_curve = tone_curve(0, EqLayout::TONE_STEPS, true);
        let mut broad = EqualizerProcessor::new();
        broad.set_tuning(true, broad_curve.clone(), 0.0);
        let mut focused = EqualizerProcessor::new();
        focused.set_tuning(true, focused_curve.clone(), 0.0);
        let broad_skirt = measure(&mut broad, 500.0) - f64::from(broad_curve.preamp_db);
        let focused_skirt = measure(&mut focused, 500.0) - f64::from(focused_curve.preamp_db);
        assert!(broad_skirt > focused_skirt + 1.5);
    }

    #[test]
    fn balance_limiter_passthrough_and_presets() {
        let mut proc = EqualizerProcessor::new();
        proc.set_tuning(true, manual_curve(EqualizerPreset::Flat.bands_db()), -1.0);
        let out = run_chunks(&mut proc, &stereo_tone(1_000.0, AMPLITUDE, TONE_FRAMES));
        let right_peak = out
            .iter()
            .skip(1)
            .step_by(2)
            .map(|&s| i32::from(s).abs())
            .max()
            .unwrap_or(0);
        let left_peak = out
            .iter()
            .step_by(2)
            .map(|&s| i32::from(s).abs())
            .max()
            .unwrap_or(0);
        assert_eq!(0, right_peak);
        assert!(left_peak > 7_000);

        // Boosting every band does not clip past PeakLimiter::CEILING.
        let mut max_proc = EqualizerProcessor::new();
        max_proc.set_tuning(
            true,
            manual_curve(&[EqLayout::MANUAL_RANGE_DB; EqLayout::MANUAL_COUNT]),
            0.0,
        );
        let clipped_test = run_chunks(&mut max_proc, &stereo_tone(400.0, 24_000.0, TONE_FRAMES));
        let peak = clipped_test
            .iter()
            .map(|&s| i32::from(s).abs())
            .max()
            .unwrap_or(0) as f32;
        assert!(peak <= PeakLimiter::CEILING * 32_768.0 + 1.0);

        // Make-up takes back half of a lone +12 dB band.
        let mut one_band = [0.0_f32; EqLayout::MANUAL_COUNT];
        one_band[3] = EqLayout::MANUAL_RANGE_DB;
        assert!(
            (manual_curve(&one_band).preamp_db - (-EqLayout::MANUAL_RANGE_DB / 2.0)).abs() <= 0.1
        );

        // Switched-off EQ is bit-exact passthrough, and gliding down settles to bit-exact passthrough.
        let mut off_proc = EqualizerProcessor::new();
        off_proc.set_tuning(false, manual_curve(EqualizerPreset::Rock.bands_db()), 0.5);
        let tone = stereo_tone(1_000.0, AMPLITUDE, 4_096);
        assert_eq!(tone, run_chunks(&mut off_proc, &tone));

        let mut glide_proc = EqualizerProcessor::new();
        glide_proc.set_tuning(
            true,
            manual_curve(EqualizerPreset::BassBoost.bands_db()),
            0.0,
        );
        glide_proc.configure(SAMPLE_RATE, 2);
        glide_proc.flush();
        let mut scratch = tone.clone();
        glide_proc.process_i16_interleaved(&mut scratch);
        glide_proc.set_tuning(false, manual_curve(EqualizerPreset::Flat.bands_db()), 0.0);
        for _ in 0..12 {
            let mut buf = tone.clone();
            glide_proc.process_i16_interleaved(&mut buf);
        }
        let mut final_buf = tone.clone();
        glide_proc.process_i16_interleaved(&mut final_buf);
        assert_eq!(tone, final_buf);

        // Preset coverage & matching + non-positive preamp invariant.
        for preset in EqualizerPreset::ALL {
            if preset == EqualizerPreset::Custom {
                continue;
            }
            assert_eq!(EqLayout::MANUAL_COUNT, preset.bands_db().len());
            assert!(manual_curve(preset.bands_db()).preamp_db <= 0.001);
        }
        assert_eq!(
            EqualizerPreset::Rock,
            EqualizerPreset::matching(EqualizerPreset::Rock.bands_db())
        );
        assert_eq!(
            EqualizerPreset::Custom,
            EqualizerPreset::matching(&[7.0; EqLayout::MANUAL_COUNT])
        );
    }
}
