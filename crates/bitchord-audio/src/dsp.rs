//! Canonical per-deck Float32 DSP pipeline for BitChord Linux Desktop:
//!
//! `SpatialAudioProcessor -> EqualizerProcessor -> TransitionFilter -> LoudnessProcessor`
//!
//! Ported directly from upstream `SpatialAudioProcessor.kt`, `LoudnessProcessor.kt`,
//! `DspChain.kt`, `LoudnessProcessorTest.kt`, and `FloatDspChainTest.kt`.
//!
//! Once configured off-thread, `process_interleaved` performs **zero heap allocations**,
//! **zero locks**, and **self-bypasses bit-exactly** whenever every stage is idle/settled.

use bitchord_dsp_core::{EqCurve, EqLayout, EqualizerProcessor, TransitionFilter};

/// Copyable EQ tuning snapshot safe to send across an `rtrb` SPSC queue to the RT thread
/// without any heap allocations or drops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqTuningSnapshot {
    pub enabled: bool,
    pub gains_db: [f32; EqLayout::SLOTS],
    pub qs: [f32; EqLayout::SLOTS],
    pub preamp_db: f32,
    pub balance: f32,
}

impl Default for EqTuningSnapshot {
    fn default() -> Self {
        Self::off()
    }
}

impl EqTuningSnapshot {
    pub const fn off() -> Self {
        Self {
            enabled: false,
            gains_db: [0.0; EqLayout::SLOTS],
            qs: [0.707; EqLayout::SLOTS],
            preamp_db: 0.0,
            balance: 0.0,
        }
    }

    pub fn from_curve(enabled: bool, curve: &EqCurve, balance: f32) -> Self {
        Self {
            enabled,
            gains_db: curve.gains_db,
            qs: curve.qs,
            preamp_db: curve.preamp_db,
            balance: balance.clamp(-1.0, 1.0),
        }
    }

    pub fn to_curve(self) -> EqCurve {
        EqCurve {
            gains_db: self.gains_db,
            qs: self.qs,
            preamp_db: self.preamp_db,
        }
    }
}

/// Sanitizes non-finite float samples (`NaN -> 0.0`, `±Inf -> ±1.0`) without clamping
/// finite headroom above `±1.0`.
#[inline]
pub fn sanitize_samples(samples: &mut [f32]) {
    for sample in samples.iter_mut() {
        if !sample.is_finite() {
            *sample = if sample.is_nan() {
                0.0
            } else if sample.is_sign_positive() {
                1.0
            } else {
                -1.0
            };
        }
    }
}

/// Mid/side stereo widener + 15 ms low-passed crossfeed (`SpatialAudioProcessor.kt`).
///
/// Stereo-only: mono or multichannel blocks pass through untouched.
/// Bit-exact bypass when `enabled == false`.
pub struct SpatialAudioProcessor {
    pub enabled: bool,
    sample_rate: u32,
    channel_count: usize,
    delay_left: Vec<f32>,
    delay_right: Vec<f32>,
    delay_index: usize,
    lowpass_left: f32,
    lowpass_right: f32,
}

impl Default for SpatialAudioProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl SpatialAudioProcessor {
    const WIDTH_GAIN: f32 = 2.5;
    const OUTPUT_GAIN: f32 = 0.82;
    const CROSSFEED_GAIN: f32 = 0.2;
    const LOWPASS_COEFF: f32 = 0.3;
    const DELAY_MS: f32 = 15.0;

    pub fn new() -> Self {
        Self {
            enabled: false,
            sample_rate: 0,
            channel_count: 0,
            delay_left: Vec::new(),
            delay_right: Vec::new(),
            delay_index: 0,
            lowpass_left: 0.0,
            lowpass_right: 0.0,
        }
    }

    pub fn configure(&mut self, sample_rate: u32, channel_count: usize) {
        self.sample_rate = sample_rate;
        self.channel_count = channel_count;
        if channel_count != 2 || sample_rate == 0 {
            self.delay_left.clear();
            self.delay_right.clear();
            self.delay_index = 0;
            self.lowpass_left = 0.0;
            self.lowpass_right = 0.0;
            return;
        }
        let delay_samples =
            ((sample_rate as f32 * Self::DELAY_MS / 1000.0).round() as usize).max(1);
        if self.delay_left.len() != delay_samples {
            self.delay_left = vec![0.0; delay_samples];
            self.delay_right = vec![0.0; delay_samples];
        }
        self.flush();
    }

    pub fn flush(&mut self) {
        self.delay_left.fill(0.0);
        self.delay_right.fill(0.0);
        self.delay_index = 0;
        self.lowpass_left = 0.0;
        self.lowpass_right = 0.0;
    }

    pub fn reset(&mut self) {
        self.delay_left.clear();
        self.delay_right.clear();
        self.delay_index = 0;
        self.lowpass_left = 0.0;
        self.lowpass_right = 0.0;
        self.channel_count = 0;
        self.sample_rate = 0;
    }

    /// Processes interleaved stereo `f32` audio in-place, preserving headroom above `±1.0`.
    pub fn process_interleaved(&mut self, samples: &mut [f32]) {
        if !self.enabled || self.channel_count != 2 || self.delay_left.is_empty() {
            return;
        }
        let delay_size = self.delay_left.len();
        let total_samples = (samples.len() / 2) * 2;
        if total_samples == 0 {
            return;
        }

        let mut d_idx = self.delay_index;
        let mut lp_l = self.lowpass_left;
        let mut lp_r = self.lowpass_right;

        let mut idx = 0;
        while idx < total_samples {
            let left = samples[idx];
            let right = samples[idx + 1];

            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5 * Self::WIDTH_GAIN;
            let mut widened_left = mid + side;
            let mut widened_right = mid - side;

            let delayed_right = self.delay_right[d_idx];
            let delayed_left = self.delay_left[d_idx];
            lp_l += Self::LOWPASS_COEFF * (delayed_right - lp_l);
            lp_r += Self::LOWPASS_COEFF * (delayed_left - lp_r);
            widened_left += lp_l * Self::CROSSFEED_GAIN;
            widened_right += lp_r * Self::CROSSFEED_GAIN;

            self.delay_left[d_idx] = left;
            self.delay_right[d_idx] = right;
            d_idx += 1;
            if d_idx >= delay_size {
                d_idx = 0;
            }

            samples[idx] = widened_left * Self::OUTPUT_GAIN;
            samples[idx + 1] = widened_right * Self::OUTPUT_GAIN;
            idx += 2;
        }

        self.delay_index = d_idx;
        self.lowpass_left = lp_l;
        self.lowpass_right = lp_r;
    }
}

/// Per-deck loudness normalization + crossfade blend headroom trim + instant-attack peak limiter
/// (`LoudnessProcessor.kt`).
///
/// Audio-thread safe: all targets are plain `f32` scalars with zero heap allocation or locking.
pub struct LoudnessProcessor {
    pub hold_limiter: bool,
    target_gain: f32,
    next_target_gain: Option<f32>,
    trim_target: f32,

    sample_rate: u32,
    channel_count: usize,
    current_gain: f32,
    current_trim: f32,
    reduction: f32,
    glide_coef: f32,
    release_coef: f32,
    snap: bool,
}

impl Default for LoudnessProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl LoudnessProcessor {
    /// Ceiling matching `PeakLimiter::CEILING` (`0.985`).
    pub const LIMIT: f32 = 0.985;
    /// Floor for blend headroom trim (`-6 dB`).
    pub const MIN_TRIM: f32 = 0.5;
    const GLIDE_SECONDS: f64 = 0.04;
    const RELEASE_SECONDS: f64 = 0.08;
    const SETTLED: f32 = 1e-4;
    const PARK_REDUCTION: f32 = 0.97;

    pub fn new() -> Self {
        Self {
            hold_limiter: false,
            target_gain: 1.0,
            next_target_gain: None,
            trim_target: 1.0,
            sample_rate: 0,
            channel_count: 0,
            current_gain: 1.0,
            current_trim: 1.0,
            reduction: 1.0,
            glide_coef: 1.0,
            release_coef: 1.0,
            snap: true,
        }
    }

    /// Sets linear loudness gain for the active track (glides smoothly unless snapped).
    pub fn set_target_gain(&mut self, gain: f32) {
        self.target_gain = if gain.is_finite() && gain > 0.0 {
            gain
        } else {
            1.0
        };
    }

    /// Arms the linear loudness gain for the next gapless track (`on_stream_boundary` promotes it).
    pub fn set_next_target_gain(&mut self, gain: Option<f32>) {
        self.next_target_gain = gain.filter(|g| g.is_finite() && *g > 0.0);
    }

    /// Sets smooth crossfade headroom trim (`1 / sqrt(inGain + outGain)` clamped to `[0.5, 1.0]`).
    pub fn set_blend_trim(&mut self, value: f32) {
        self.trim_target = if value.is_finite() {
            value.clamp(Self::MIN_TRIM, 1.0)
        } else {
            1.0
        };
    }

    /// Promotes `next_target_gain` at an exact gapless sample boundary and snaps immediately.
    pub fn on_stream_boundary(&mut self) {
        if let Some(next) = self.next_target_gain.take() {
            self.target_gain = next;
        }
        self.snap = true;
    }

    pub fn configure(&mut self, sample_rate: u32, channel_count: usize) {
        self.sample_rate = sample_rate;
        self.channel_count = channel_count;
        self.glide_coef = Self::coefficient(Self::GLIDE_SECONDS, sample_rate);
        self.release_coef = Self::coefficient(Self::RELEASE_SECONDS, sample_rate);
        self.snap = true;
    }

    pub fn flush(&mut self) {
        self.reduction = 1.0;
        self.snap = true;
    }

    pub fn reset(&mut self) {
        self.sample_rate = 0;
        self.channel_count = 0;
        self.reduction = 1.0;
        self.snap = true;
    }

    pub fn process_interleaved(&mut self, samples: &mut [f32]) {
        let channels = self.channel_count;
        if channels == 0 || self.sample_rate == 0 {
            return;
        }
        let frames = samples.len() / channels;
        if frames == 0 {
            return;
        }

        let target_gain = self.target_gain;
        let target_trim = self.trim_target;
        if self.snap {
            self.current_gain = target_gain;
            self.current_trim = target_trim;
            self.snap = false;
        }

        // Bit-exact park when unity gain, unity trim, limiter released, and EQ not shaping.
        if !self.hold_limiter
            && target_gain == 1.0
            && target_trim >= 1.0
            && (self.current_gain - 1.0).abs() < Self::SETTLED
            && self.current_trim >= 1.0 - Self::SETTLED
            && self.reduction >= Self::PARK_REDUCTION
        {
            self.current_gain = 1.0;
            self.current_trim = 1.0;
            self.reduction = 1.0;
            return;
        }

        let mut gain = self.current_gain;
        let mut trim = self.current_trim;
        let mut gr = self.reduction;
        let glide_coef = self.glide_coef;
        let release_coef = self.release_coef;

        let mut index = 0;
        for _ in 0..frames {
            gain += (target_gain - gain) * glide_coef;
            trim += (target_trim - trim) * glide_coef;
            let scale = gain * trim;

            let mut peak = 0.0_f32;
            for ch in 0..channels {
                let mag = samples[index + ch].abs();
                if mag > peak {
                    peak = mag;
                }
            }
            peak *= scale;

            let wanted = if peak > Self::LIMIT {
                Self::LIMIT / peak
            } else {
                1.0
            };
            gr = if wanted < gr {
                wanted
            } else {
                gr + (wanted - gr) * release_coef
            };

            let applied = scale * gr;
            for ch in 0..channels {
                samples[index + ch] *= applied;
            }
            index += channels;
        }

        self.current_gain = gain;
        self.current_trim = trim;
        self.reduction = gr;
    }

    fn coefficient(seconds: f64, rate: u32) -> f32 {
        if rate == 0 {
            1.0
        } else {
            (1.0 - (-1.0 / (seconds * f64::from(rate))).exp()) as f32
        }
    }
}

/// Composite per-deck Float32 DSP pipeline:
/// `Spatial -> Equalizer -> TransitionFilter -> Loudness/PeakLimiter`.
pub struct DspChain {
    pub spatial: SpatialAudioProcessor,
    pub equalizer: EqualizerProcessor,
    pub transition: TransitionFilter,
    pub loudness: LoudnessProcessor,
    channel_count: usize,
    sample_rate: u32,
}

impl Default for DspChain {
    fn default() -> Self {
        Self::new()
    }
}

impl DspChain {
    pub fn new() -> Self {
        Self {
            spatial: SpatialAudioProcessor::new(),
            equalizer: EqualizerProcessor::new(),
            transition: TransitionFilter::new(),
            loudness: LoudnessProcessor::new(),
            channel_count: 0,
            sample_rate: 0,
        }
    }

    pub fn configure(&mut self, sample_rate: u32, channel_count: usize) {
        self.sample_rate = sample_rate;
        self.channel_count = channel_count;
        self.spatial.configure(sample_rate, channel_count);
        self.equalizer.configure(sample_rate, channel_count);
        self.transition.configure(channel_count, sample_rate);
        self.loudness.configure(sample_rate, channel_count);
    }

    pub fn apply_eq_snapshot(&mut self, snap: EqTuningSnapshot) {
        self.equalizer
            .set_tuning(snap.enabled, snap.to_curve(), snap.balance);
    }

    pub fn on_stream_boundary(&mut self) {
        self.loudness.on_stream_boundary();
    }

    pub fn flush(&mut self) {
        self.spatial.flush();
        self.equalizer.flush();
        self.transition.flush();
        self.loudness.flush();
    }

    pub fn reset(&mut self) {
        self.spatial.reset();
        self.transition.reset();
        self.loudness.reset();
        self.channel_count = 0;
        self.sample_rate = 0;
    }

    /// Processes interleaved `f32` audio in-place with zero heap allocations.
    pub fn process_interleaved(&mut self, samples: &mut [f32]) {
        if samples.is_empty() || self.channel_count == 0 || self.sample_rate == 0 {
            return;
        }
        let channels = self.channel_count;
        let frames = samples.len() / channels;
        if frames == 0 {
            return;
        }
        let active_slice = &mut samples[..frames * channels];

        sanitize_samples(active_slice);
        self.spatial.process_interleaved(active_slice);
        self.equalizer.process_f32_interleaved(active_slice);
        self.process_transition_interleaved(active_slice, frames, channels);
        self.loudness.hold_limiter = self.equalizer.is_shaping();
        self.loudness.process_interleaved(active_slice);
    }

    fn process_transition_interleaved(
        &mut self,
        samples: &mut [f32],
        frames: usize,
        channels: usize,
    ) {
        if self.transition.parked() {
            return;
        }
        let mut remaining = frames;
        let mut frame_offset = 0usize;
        while remaining > 0 {
            let sub = remaining.min(TransitionFilter::GLIDE_FRAMES);
            self.transition.advance();
            if self.transition.active() {
                for f in 0..sub {
                    let base = (frame_offset + f) * channels;
                    for ch in 0..channels {
                        samples[base + ch] = self.transition.filter(ch, samples[base + ch]);
                    }
                }
            }
            frame_offset += sub;
            remaining -= sub;
        }
    }
}

/// Converts YouTube `loudnessDb` (typically negative when loud, positive when quiet) into
/// a linear gain factor matching BitChord's target loudness curve.
pub fn loudness_db_to_linear_gain(loudness_db: Option<f64>) -> f32 {
    let Some(db) = loudness_db.filter(|v| v.is_finite()) else {
        return 1.0;
    };
    // Match standard -14 LUFS normalization capped to a conservative [-12, +6] dB window.
    let gain_db = (-db).clamp(-12.0, 6.0);
    10.0_f64.powf(gain_db / 20.0) as f32
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use bitchord_dsp_core::PeakLimiter;

    use super::*;

    const SR: u32 = 44_100;

    fn sine_stereo(amp: f32, freq_hz: f64, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0_f32; frames * 2];
        for f in 0..frames {
            let s = (f64::from(amp) * (2.0 * PI * freq_hz * f as f64 / f64::from(SR)).sin()) as f32;
            out[f * 2] = s;
            out[f * 2 + 1] = s;
        }
        out
    }

    fn peak_from(samples: &[f32], from_frame: usize) -> f32 {
        samples[from_frame * 2..]
            .iter()
            .fold(0.0_f32, |acc, &s| acc.max(s.abs()))
    }

    // ---- LoudnessProcessorTest.kt (all 6 tests) -----------------------------

    #[test]
    fn unity_gain_leaves_samples_bit_exact() {
        let mut buf = sine_stereo(0.9, 440.0, 4096);
        let before = buf.clone();
        let mut proc = LoudnessProcessor::new();
        proc.configure(SR, 2);
        proc.process_interleaved(&mut buf);
        assert_eq!(before, buf);
    }

    #[test]
    fn attenuation_is_applied_from_the_first_sample_of_a_track() {
        let mut buf = sine_stereo(0.8, 440.0, 4096);
        let mut proc = LoudnessProcessor::new();
        proc.configure(SR, 2);
        proc.set_target_gain(0.5);
        proc.process_interleaved(&mut buf);
        assert!((peak_from(&buf, 0) - 0.4).abs() < 0.01);
    }

    #[test]
    fn boost_never_passes_full_scale() {
        let mut buf = sine_stereo(0.95, 440.0, 4096);
        let mut proc = LoudnessProcessor::new();
        proc.configure(SR, 2);
        proc.set_target_gain(1.41);
        proc.process_interleaved(&mut buf);
        assert!(peak_from(&buf, 0) <= 1.0);
    }

    #[test]
    fn blend_trim_is_a_plain_gain_that_never_reshapes_loud_tracks() {
        let trim = 1.0_f32 / std::f32::consts::SQRT_2.sqrt();
        let mut proc = LoudnessProcessor::new();
        proc.configure(SR, 2);
        proc.set_blend_trim(trim);
        let mut warmup = sine_stereo(0.98, 440.0, 4096);
        proc.process_interleaved(&mut warmup);

        let mut buf = sine_stereo(0.98, 440.0, 4096);
        let before = buf.clone();
        proc.process_interleaved(&mut buf);
        for (a, b) in before.iter().zip(buf.iter()) {
            assert!((a * trim - b).abs() < 1e-4);
        }
    }

    #[test]
    fn gapless_boundary_switches_to_the_next_tracks_gain() {
        let mut proc = LoudnessProcessor::new();
        proc.configure(SR, 2);
        proc.set_target_gain(0.5);
        proc.set_next_target_gain(Some(0.25));
        let mut first = sine_stereo(0.8, 440.0, 4096);
        proc.process_interleaved(&mut first);

        proc.on_stream_boundary();
        let mut second = sine_stereo(0.8, 440.0, 4096);
        proc.process_interleaved(&mut second);
        assert!((peak_from(&second, 0) - 0.2).abs() < 0.01);
    }

    #[test]
    fn late_figure_glides_in_rather_than_stepping() {
        let mut proc = LoudnessProcessor::new();
        proc.configure(SR, 2);
        let mut first = sine_stereo(0.8, 440.0, 4096);
        proc.process_interleaved(&mut first);

        proc.set_target_gain(0.5);
        let mut mid = sine_stereo(0.8, 440.0, 4096);
        proc.process_interleaved(&mut mid);
        assert!(peak_from(&mid, 0) > 0.7);

        let mut settled = sine_stereo(0.8, 440.0, 4096);
        proc.process_interleaved(&mut settled);
        assert!((peak_from(&settled, 3000) - 0.4).abs() < 0.01);
    }

    // ---- FloatDspChainTest.kt (Spatial, Transition, DspChain) ---------------

    #[test]
    fn spatial_crossfeed_leaks_delayed_signal_and_bypasses_when_off() {
        let mut spatial = SpatialAudioProcessor::new();
        spatial.configure(SR, 2);
        spatial.enabled = false;
        let mut bypass: Vec<f32> = (1..=128).map(|i| i as f32 * 0.01).collect();
        let before = bypass.clone();
        spatial.process_interleaved(&mut bypass);
        assert_eq!(before, bypass);

        spatial.enabled = true;
        let frames = 1000;
        let mut buf = vec![0.0_f32; frames * 2];
        for f in 0..frames {
            buf[f * 2] = (0.5 * (2.0 * PI * 500.0 * f as f64 / f64::from(SR)).sin()) as f32;
        }
        spatial.process_interleaved(&mut buf);
        let right_energy: f64 = (700..frames).map(|f| f64::from(buf[f * 2 + 1].abs())).sum();
        assert!(right_energy > 1.0, "right_energy={right_energy}");
    }

    #[test]
    fn dsp_chain_preserves_sub_lsb_precision_and_limits_eq_overs_at_end() {
        let mut chain = DspChain::new();
        chain.configure(SR, 2);
        chain.spatial.enabled = true;
        chain.equalizer.set_tuning(true, EqCurve::flat(), 0.0);
        chain.transition.open();
        chain.flush();

        let mut tiny = vec![1.0e-7_f32; 256];
        chain.process_interleaved(&mut tiny);
        assert!(tiny.iter().any(|&v| v != 0.0));

        // +3 dB preamp pushes 0.9 sine past 1.0; chain limits cleanly under ceiling.
        let mut chain2 = DspChain::new();
        chain2.configure(SR, 2);
        chain2.spatial.enabled = false;
        chain2.equalizer.set_tuning(
            true,
            EqCurve {
                gains_db: [0.0; EqLayout::SLOTS],
                qs: [0.707; EqLayout::SLOTS],
                preamp_db: 3.0,
            },
            0.0,
        );
        chain2.transition.open();
        chain2.flush();

        let mut loud = sine_stereo(0.9, 100.0, 512);
        chain2.process_interleaved(&mut loud);
        let pk = peak_from(&loud, 0);
        assert!(pk <= PeakLimiter::CEILING + 1e-5, "pk={pk}");
        assert!(pk > 0.9, "pk={pk}");
    }

    #[test]
    fn dsp_chain_returns_to_bit_exact_once_eq_is_switched_off() {
        let mut chain = DspChain::new();
        chain.configure(SR, 2);
        let mut gains = [0.0; EqLayout::SLOTS];
        gains[0] = 6.0;
        chain.equalizer.set_tuning(
            true,
            EqCurve {
                gains_db: gains,
                qs: [0.707; EqLayout::SLOTS],
                preamp_db: -3.0,
            },
            0.0,
        );
        chain.flush();

        let source = sine_stereo(0.99, 60.0, 1024);
        let mut buf = vec![0.0_f32; source.len()];
        for _ in 0..10 {
            buf.copy_from_slice(&source);
            chain.process_interleaved(&mut buf);
        }
        chain.equalizer.set_tuning(false, EqCurve::flat(), 0.0);
        for _ in 0..80 {
            buf.copy_from_slice(&source);
            chain.process_interleaved(&mut buf);
        }
        buf.copy_from_slice(&source);
        chain.process_interleaved(&mut buf);
        assert_eq!(source, buf);
    }

    #[test]
    fn dsp_chain_transition_filters_and_stability_under_repeated_blocks() {
        let mut chain = DspChain::new();
        chain.configure(SR, 2);
        chain.spatial.enabled = true;
        let mut gains = [0.0; EqLayout::SLOTS];
        gains[2] = 4.0;
        chain.equalizer.set_tuning(
            true,
            EqCurve {
                gains_db: gains,
                qs: [0.707; EqLayout::SLOTS],
                preamp_db: 0.0,
            },
            0.0,
        );
        chain.transition.set_cutoffs(8_000.0, 100.0);

        let block_frames = 512;
        let mut buf = vec![0.0_f32; block_frames * 2];
        for iter in 0..50 {
            for f in 0..block_frames {
                let total_f = iter * block_frames + f;
                let s = (2.0 * PI * 440.0 * total_f as f64 / f64::from(SR)).sin() as f32;
                buf[f * 2] = s;
                buf[f * 2 + 1] = s;
            }
            chain.process_interleaved(&mut buf);
            assert!(buf.iter().all(|v| v.is_finite()));
        }
    }
}
