//! Pure-Rust DSP musical structure analyzer ported from upstream
//! `native/analyzer/audio_analysis.cpp` and `tempo_analysis.cpp`.
//!
//! Runs completely offline on 11,025 Hz mono PCM with zero native dependencies:
//! - 250 ms RMS energy envelope + audible start / content end boundaries
//! - Spectral flux onset envelope (FFT 512, hop 128)
//! - Autocorrelation BPM estimator (70..=200 BPM) with metrical octave resolver
//! - Two-pass PLL beat tracker + bass-weighted 4/4 downbeat & 8-bar phrase detector
//! - Krumhansl-Schmuckler 24-key chromagram classifier
//! - Low-frequency (`20..250 Hz`) and vocal-band (`300..3000 Hz`) energy curves
//! - Structural mix-in and mix-out cue candidate scorer

use std::f64::consts::PI;

use crate::analysis::{BeatSource, EnergySample, MixCandidate, TrackAnalysis, VocalSource};

/// Canonical analysis sample rate (11,025 Hz mono, matching `audio_analysis.cpp`).
pub const ANALYSIS_SAMPLE_RATE: u32 = 11_025;
/// Cap stored mono analysis samples at 15 minutes (~39 MiB max RAM for long DJ sets).
const MAX_ANALYSIS_SAMPLES: usize = ANALYSIS_SAMPLE_RATE as usize * 60 * 15;
const FFT_SIZE: usize = 512;
const HOP_SIZE: usize = 128;
const CURVE_STEP_SEC: f64 = 0.25;

// Krumhansl-Schmuckler tonal key profiles (major & minor).
const MAJOR_PROFILE: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Iterative in-place radix-2 Cooley-Tukey FFT (`len` must be a power of 2).
pub(crate) fn fft_in_place(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    if n <= 1 {
        return;
    }

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while (j & bit) != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2usize;
    while len <= n {
        let half = len >> 1;
        let angle = -2.0 * PI / len as f64;
        let w_re = angle.cos() as f32;
        let w_im = angle.sin() as f32;
        let mut start = 0usize;
        while start < n {
            let mut cur_re = 1.0_f32;
            let mut cur_im = 0.0_f32;
            for k in 0..half {
                let u_idx = start + k;
                let v_idx = u_idx + half;
                let t_re = cur_re * re[v_idx] - cur_im * im[v_idx];
                let t_im = cur_re * im[v_idx] + cur_im * re[v_idx];
                let u_re = re[u_idx];
                let u_im = im[u_idx];
                re[u_idx] = u_re + t_re;
                im[u_idx] = u_im + t_im;
                re[v_idx] = u_re - t_re;
                im[v_idx] = u_im - t_im;
                let next_re = cur_re * w_re - cur_im * w_im;
                let next_im = cur_re * w_im + cur_im * w_re;
                cur_re = next_re;
                cur_im = next_im;
            }
            start += len;
        }
        len <<= 1;
    }
}

/// Incremental stereo-to-11,025 Hz mono decimating collector so audio decoders can stream
/// packets straight into the analyzer without buffering full-rate PCM in RAM.
#[derive(Debug, Clone)]
pub struct StreamingAnalyzer {
    input_rate_hz: u32,
    phase: f64,
    mono: Vec<f32>,
    total_input_frames: u64,
}

impl StreamingAnalyzer {
    pub fn new(input_rate_hz: u32) -> Self {
        Self {
            input_rate_hz: input_rate_hz.max(8_000),
            phase: 0.0,
            mono: Vec::with_capacity(ANALYSIS_SAMPLE_RATE as usize * 60),
            total_input_frames: 0,
        }
    }

    /// Feeds interleaved stereo `f32` PCM frames (`[L, R, L, R, ...]`).
    pub fn push_stereo(&mut self, interleaved_stereo: &[f32]) {
        let step = f64::from(self.input_rate_hz) / f64::from(ANALYSIS_SAMPLE_RATE);
        let frames = interleaved_stereo.len() / 2;
        if frames == 0 {
            return;
        }
        self.total_input_frames += frames as u64;
        if self.mono.len() >= MAX_ANALYSIS_SAMPLES {
            return;
        }

        let mut pos = self.phase;
        while (pos as usize) < frames && self.mono.len() < MAX_ANALYSIS_SAMPLES {
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let m0 = 0.5 * (interleaved_stereo[idx * 2] + interleaved_stereo[idx * 2 + 1]);
            let m1 = if idx + 1 < frames {
                0.5 * (interleaved_stereo[(idx + 1) * 2] + interleaved_stereo[(idx + 1) * 2 + 1])
            } else {
                m0
            };
            self.mono.push(m0 + (m1 - m0) * frac);
            pos += step;
        }
        self.phase = pos - frames as f64;
    }

    pub fn mono_samples(&self) -> &[f32] {
        &self.mono
    }

    pub fn duration_sec(&self) -> f64 {
        self.total_input_frames as f64 / f64::from(self.input_rate_hz)
    }

    pub fn finish(self) -> TrackAnalysis {
        let dur = self.duration_sec();
        analyze_mono_11025(&self.mono, dur)
    }
}

/// Analyzes an interleaved stereo `f32` buffer at `sample_rate_hz`.
pub fn analyze_stereo_pcm(interleaved_stereo: &[f32], sample_rate_hz: u32) -> TrackAnalysis {
    let mut stream = StreamingAnalyzer::new(sample_rate_hz);
    stream.push_stereo(interleaved_stereo);
    stream.finish()
}

/// Runs the complete pure-Rust DSP musical analyzer on 11,025 Hz mono PCM.
pub fn analyze_mono_11025(mono: &[f32], duration_sec: f64) -> TrackAnalysis {
    let effective_duration = if duration_sec.is_finite() && duration_sec > 0.0 {
        duration_sec
    } else {
        mono.len() as f64 / f64::from(ANALYSIS_SAMPLE_RATE)
    };
    if mono.len() < FFT_SIZE * 4 || effective_duration < 2.0 {
        return TrackAnalysis {
            key: -1,
            duration: effective_duration.max(0.0),
            audible_start_time: 0.0,
            content_end_time: effective_duration.max(0.0),
            ..TrackAnalysis::default()
        };
    }

    // 1. 250 ms RMS energy envelope + audible start / content end detection.
    let (energy_curve, audible_start_time, content_end_time) =
        compute_energy_envelope(mono, effective_duration);

    // 2. Short-time FFT pass: spectral flux onset envelope, bass energy, vocal formant ratio,
    //    and 12-bin chromagram.
    let stft = compute_stft_features(mono);

    // 3. Tempo estimation + two-pass PLL beat tracking + 4/4 downbeats + 8-bar phrases.
    let rhythm = estimate_rhythm(&stft, effective_duration);

    // 4. Krumhansl-Schmuckler 24-key detection.
    let (key, key_confidence) = estimate_key(&stft.chroma);

    // 5. Downsample STFT band energies onto 250 ms curves for low-end and vocal activity.
    let low_energy_curve = downsample_curve(&stft.bass_frames, stft.frame_rate, effective_duration);
    let vocal_activity_mask =
        downsample_curve(&stft.vocal_frames, stft.frame_rate, effective_duration);
    let vocal_probability = if vocal_activity_mask.is_empty() {
        0.0
    } else {
        let active = vocal_activity_mask
            .iter()
            .filter(|s| s.energy >= 0.25)
            .count();
        (active as f32 / vocal_activity_mask.len() as f32).clamp(0.0, 1.0)
    };

    let mut analysis = TrackAnalysis {
        bpm: rhythm.bpm,
        beat_interval: rhythm.beat_interval,
        beat_confidence: rhythm.confidence,
        beats: rhythm.beats,
        downbeats: rhythm.downbeats,
        phrase_boundaries: rhythm.phrase_boundaries,
        key,
        key_confidence,
        duration: effective_duration,
        audible_start_time,
        content_end_time,
        mix_in_candidates: Vec::new(),
        mix_out_candidates: Vec::new(),
        energy_curve,
        low_energy_curve,
        vocal_activity_mask,
        vocal_probability,
        beat_source: BeatSource::Dsp,
        vocal_source: VocalSource::Dsp,
    };

    // 6. Score structural mix-in and mix-out cue points.
    analysis.mix_in_candidates = score_mix_in_candidates(&analysis);
    analysis.mix_out_candidates = score_mix_out_candidates(&analysis);
    analysis
}

/// Recomputes phrase boundaries and mix-in/mix-out candidates after an external model
/// (such as ONNX Beat-This or UMX-HQ vocal separation) upgrades `beats`/`downbeats`
/// or `vocal_activity_mask`.
pub fn rebuild_derived_structure(analysis: &mut TrackAnalysis) {
    analysis.phrase_boundaries = build_phrase_boundaries(&analysis.downbeats);
    if !analysis.vocal_activity_mask.is_empty() {
        let active = analysis
            .vocal_activity_mask
            .iter()
            .filter(|s| s.energy >= 0.25)
            .count();
        analysis.vocal_probability =
            (active as f32 / analysis.vocal_activity_mask.len() as f32).clamp(0.0, 1.0);
    }
    analysis.mix_in_candidates = score_mix_in_candidates(analysis);
    analysis.mix_out_candidates = score_mix_out_candidates(analysis);
}

fn compute_energy_envelope(mono: &[f32], duration: f64) -> (Vec<EnergySample>, f64, f64) {
    let step = ((f64::from(ANALYSIS_SAMPLE_RATE) * CURVE_STEP_SEC).round() as usize).max(1);
    let mut rms_vals = Vec::with_capacity(mono.len() / step + 1);
    let mut times = Vec::with_capacity(mono.len() / step + 1);

    let mut pos = 0usize;
    while pos < mono.len() {
        let end = (pos + step).min(mono.len());
        let slice = &mono[pos..end];
        let mut sum_sq = 0.0_f64;
        for &s in slice {
            sum_sq += f64::from(s) * f64::from(s);
        }
        let rms = (sum_sq / slice.len().max(1) as f64).sqrt() as f32;
        rms_vals.push(rms);
        times.push((pos as f64 / f64::from(ANALYSIS_SAMPLE_RATE)).min(duration));
        pos += step;
    }

    let ref_rms = percentile(&rms_vals, 0.95).max(1e-4);
    let curve: Vec<EnergySample> = times
        .iter()
        .zip(rms_vals.iter())
        .map(|(&time, &r)| EnergySample {
            time,
            energy: (r / ref_rms).clamp(0.0, 1.0),
        })
        .collect();

    let start_thresh = 0.08_f32;
    let end_thresh = 0.08_f32;
    let audible_start = curve
        .iter()
        .find(|s| s.energy >= start_thresh)
        .map(|s| s.time)
        .unwrap_or(0.0);
    let content_end = curve
        .iter()
        .rev()
        .find(|s| s.energy >= end_thresh)
        .map(|s| (s.time + CURVE_STEP_SEC).min(duration))
        .unwrap_or(duration);

    (
        curve,
        audible_start.min(content_end),
        content_end.max(audible_start),
    )
}

struct StftFeatures {
    frame_rate: f64,
    onset: Vec<f32>,
    bass_frames: Vec<f32>,
    vocal_frames: Vec<f32>,
    chroma: [f64; 12],
}

fn compute_stft_features(mono: &[f32]) -> StftFeatures {
    let frame_rate = f64::from(ANALYSIS_SAMPLE_RATE) / HOP_SIZE as f64;
    let num_frames = if mono.len() >= FFT_SIZE {
        (mono.len() - FFT_SIZE) / HOP_SIZE + 1
    } else {
        0
    };
    let bins = FFT_SIZE / 2 + 1;
    let bin_hz = f64::from(ANALYSIS_SAMPLE_RATE) / FFT_SIZE as f64;

    let mut hann = [0.0_f32; FFT_SIZE];
    for (i, slot) in hann.iter_mut().enumerate() {
        *slot = (0.5 * (1.0 - (2.0 * PI * i as f64 / FFT_SIZE as f64).cos())) as f32;
    }

    // Precompute pitch-class mapping for tonal harmonic range 110..=2000 Hz
    // (excludes sub-bass kick drum fundamentals so percussion doesn't skew key detection).
    let mut bin_pc = vec![None::<usize>; bins];
    for (b, slot) in bin_pc.iter_mut().enumerate().skip(1) {
        let hz = b as f64 * bin_hz;
        if (110.0..=2_000.0).contains(&hz) {
            let midi = 69.0 + 12.0 * (hz / 440.0).log2();
            let pc = (midi.round() as i32).rem_euclid(12) as usize;
            *slot = Some(pc);
        }
    }

    let bass_hi = ((250.0 / bin_hz).ceil() as usize).min(bins - 1);
    let vocal_lo = ((300.0 / bin_hz).floor() as usize).max(1);
    let vocal_hi = ((3_000.0 / bin_hz).ceil() as usize).min(bins - 1);

    let mut re = [0.0_f32; FFT_SIZE];
    let mut im = [0.0_f32; FFT_SIZE];
    let mut prev_mag = vec![0.0_f32; bins];
    let mut onset = Vec::with_capacity(num_frames);
    let mut bass_raw = Vec::with_capacity(num_frames);
    let mut vocal_raw = Vec::with_capacity(num_frames);
    let mut chroma = [0.0_f64; 12];

    for f in 0..num_frames {
        let base = f * HOP_SIZE;
        for i in 0..FFT_SIZE {
            re[i] = mono[base + i] * hann[i];
            im[i] = 0.0;
        }
        fft_in_place(&mut re, &mut im);

        let mut flux = 0.0_f32;
        let mut bass_e = 0.0_f32;
        let mut vocal_e = 0.0_f32;
        let mut total_e = 1e-9_f32;

        for b in 1..bins {
            let mag = (re[b] * re[b] + im[b] * im[b]).sqrt();
            let log_mag = (1.0 + 10.0 * mag).ln();
            let diff = log_mag - prev_mag[b];
            if diff > 0.0 {
                // Slightly emphasize low/mid percussive transients for stable beat tracking.
                let w = if b <= bass_hi { 1.4 } else { 1.0 };
                flux += diff * w;
            }
            prev_mag[b] = log_mag;

            let pwr = mag * mag;
            total_e += pwr;
            if b <= bass_hi {
                bass_e += pwr;
            }
            if b >= vocal_lo && b <= vocal_hi {
                vocal_e += pwr;
            }
            if let Some(pc) = bin_pc[b] {
                chroma[pc] += f64::from(mag);
            }
        }
        onset.push(flux);
        bass_raw.push(bass_e.sqrt());
        // Heuristic vocal likelihood: mid-band share weighted by frame loudness.
        let ratio = (vocal_e / total_e).clamp(0.0, 1.0);
        let loudness = (total_e.sqrt() / 12.0).clamp(0.0, 1.0);
        let vocal_score = ((ratio - 0.30) / 0.45).clamp(0.0, 1.0) * loudness;
        vocal_raw.push(vocal_score);
    }

    // Local mean subtraction on onset envelope so stationary noise doesn't bias autocorrelation.
    let half_win = 8usize;
    let mut onset_norm = vec![0.0_f32; onset.len()];
    for i in 0..onset.len() {
        let lo = i.saturating_sub(half_win);
        let hi = (i + half_win + 1).min(onset.len());
        let mean = onset[lo..hi].iter().sum::<f32>() / (hi - lo).max(1) as f32;
        onset_norm[i] = (onset[i] - mean).max(0.0);
    }

    let bass_ref = percentile(&bass_raw, 0.95).max(1e-4);
    let bass_frames = bass_raw
        .into_iter()
        .map(|v| (v / bass_ref).clamp(0.0, 1.0))
        .collect();

    StftFeatures {
        frame_rate,
        onset: onset_norm,
        bass_frames,
        vocal_frames: vocal_raw,
        chroma,
    }
}

struct RhythmResult {
    bpm: f32,
    beat_interval: f64,
    confidence: f32,
    beats: Vec<f64>,
    downbeats: Vec<f64>,
    phrase_boundaries: Vec<f64>,
}

fn estimate_rhythm(stft: &StftFeatures, duration: f64) -> RhythmResult {
    let n = stft.onset.len();
    if n < 64 {
        return RhythmResult {
            bpm: 0.0,
            beat_interval: 0.0,
            confidence: 0.0,
            beats: Vec::new(),
            downbeats: Vec::new(),
            phrase_boundaries: Vec::new(),
        };
    }

    // Autocorrelation over BPM 65..=200.
    let min_lag = ((stft.frame_rate * 60.0 / 200.0).floor() as usize).max(8);
    let max_lag = ((stft.frame_rate * 60.0 / 65.0).ceil() as usize).min(n / 2);
    if min_lag >= max_lag {
        return RhythmResult {
            bpm: 0.0,
            beat_interval: 0.0,
            confidence: 0.0,
            beats: Vec::new(),
            downbeats: Vec::new(),
            phrase_boundaries: Vec::new(),
        };
    }

    let mut energy = 0.0_f64;
    for &x in &stft.onset {
        energy += f64::from(x) * f64::from(x);
    }
    if energy <= 1e-6 {
        return RhythmResult {
            bpm: 0.0,
            beat_interval: 0.0,
            confidence: 0.0,
            beats: Vec::new(),
            downbeats: Vec::new(),
            phrase_boundaries: Vec::new(),
        };
    }

    // Compute normalized autocorrelation + Gaussian perceptual tempo prior centered at 120 BPM.
    let mut raw_ac = vec![0.0_f64; max_lag + 2];
    let mut weighted_ac = vec![0.0_f64; max_lag + 2];
    for lag in min_lag..=max_lag {
        let mut dot = 0.0_f64;
        let len = n - lag;
        for i in 0..len {
            dot += f64::from(stft.onset[i]) * f64::from(stft.onset[i + lag]);
        }
        let norm = (dot / (energy * (len as f64 / n as f64)).max(1e-6)).max(0.0);
        raw_ac[lag] = norm;
        let cand_bpm = 60.0 * stft.frame_rate / lag as f64;
        let oct_dist = (cand_bpm / 120.0).log2();
        let prior = (-0.5 * (oct_dist / 0.55).powi(2)).exp();
        // Harmonic reinforcement from 2x lag (2-beat / bar periodicity).
        let harm = if lag * 2 <= max_lag {
            let mut hdot = 0.0_f64;
            let hlen = n - lag * 2;
            for i in 0..hlen {
                hdot += f64::from(stft.onset[i]) * f64::from(stft.onset[i + lag * 2]);
            }
            (hdot / (energy * (hlen as f64 / n as f64)).max(1e-6)).max(0.0)
        } else {
            0.0
        };
        weighted_ac[lag] = (norm + 0.35 * harm) * prior;
    }

    let mut best_lag = min_lag;
    let mut best_score = -1.0_f64;
    for (lag, &score) in weighted_ac
        .iter()
        .enumerate()
        .take(max_lag + 1)
        .skip(min_lag)
    {
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }

    // Metrical octave resolver: compare best_lag vs half_lag (2x BPM) and double_lag (0.5x BPM).
    let half_lag = best_lag / 2;
    if half_lag >= min_lag {
        let half_bpm = 60.0 * stft.frame_rate / half_lag as f64;
        if (92.0..=165.0).contains(&half_bpm) && raw_ac[half_lag] >= raw_ac[best_lag] * 0.85 {
            best_lag = half_lag;
        }
    }
    let double_lag = best_lag * 2;
    if double_lag <= max_lag {
        let cur_bpm = 60.0 * stft.frame_rate / best_lag as f64;
        if cur_bpm > 165.0 && raw_ac[double_lag] >= raw_ac[best_lag] * 0.78 {
            best_lag = double_lag;
        }
    }

    // Parabolic sub-frame interpolation around peak lag.
    let refined_lag = if best_lag > min_lag && best_lag < max_lag {
        let y0 = raw_ac[best_lag - 1];
        let y1 = raw_ac[best_lag];
        let y2 = raw_ac[best_lag + 1];
        let denom = y0 - 2.0 * y1 + y2;
        if denom.abs() > 1e-9 {
            best_lag as f64 + (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
        } else {
            best_lag as f64
        }
    } else {
        best_lag as f64
    };

    let period_frames = refined_lag.max(8.0);
    let beat_interval = period_frames / stft.frame_rate;
    let bpm = (60.0 / beat_interval) as f32;

    // Best global phase alignment across [0, period_frames).
    let steps = (period_frames.round() as usize).clamp(8, 256);
    let mut best_phase = 0.0_f64;
    let mut best_phase_sum = -1.0_f64;
    for s in 0..steps {
        let phase = period_frames * (s as f64 / steps as f64);
        let mut sum = 0.0_f64;
        let mut p = phase;
        while (p as usize) < n {
            sum += f64::from(stft.onset[p as usize]);
            p += period_frames;
        }
        if sum > best_phase_sum {
            best_phase_sum = sum;
            best_phase = phase;
        }
    }

    // Two-pass PLL beat tracker: snaps each beat locally within ±14% of period to strongest onset.
    let search_rad = ((period_frames * 0.14).round() as isize).max(2);
    let mut beats = Vec::new();
    let mut beat_onset_sum = 0.0_f64;
    let mut cur = best_phase;
    while (cur as usize) < n {
        let center = cur.round() as isize;
        let mut best_idx = center.clamp(0, (n - 1) as isize) as usize;
        let mut best_val = -1.0_f32;
        for d in -search_rad..=search_rad {
            let idx = (center + d).clamp(0, (n - 1) as isize) as usize;
            let prox = 1.0 - 0.15 * (d.abs() as f32 / (search_rad as f32 + 1.0));
            let val = stft.onset[idx] * prox;
            if val > best_val {
                best_val = val;
                best_idx = idx;
            }
        }
        let t = best_idx as f64 / stft.frame_rate;
        if t <= duration {
            beats.push(t);
            beat_onset_sum += f64::from(stft.onset[best_idx]);
        }
        // Soft phase-locked correction (30% pull toward snapped onset).
        cur = (cur + 0.30 * (best_idx as f64 - cur)) + period_frames;
    }

    // Beat confidence combines autocorrelation sharpness and beat-grid onset concentration.
    let mean_onset = stft.onset.iter().map(|&v| f64::from(v)).sum::<f64>() / n as f64;
    let beat_mean = if beats.is_empty() {
        0.0
    } else {
        beat_onset_sum / beats.len() as f64
    };
    let grid_ratio = if mean_onset > 1e-6 {
        ((beat_mean / mean_onset - 1.0) / 3.5).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let ac_peak = raw_ac[best_lag].clamp(0.0, 1.0);
    let confidence = ((0.55 * ac_peak.sqrt() + 0.45 * grid_ratio) as f32).clamp(0.0, 1.0);

    // 4/4 Downbeat voting using bass-band energy + onset strength at mod-4 offsets.
    let mut downbeats = Vec::new();
    if beats.len() >= 4 {
        let mut phase_votes = [0.0_f64; 4];
        for (i, &t) in beats.iter().enumerate() {
            let f_idx = ((t * stft.frame_rate).round() as usize).min(n - 1);
            let bass = f64::from(stft.bass_frames.get(f_idx).copied().unwrap_or(0.0));
            let ons = f64::from(stft.onset[f_idx]);
            phase_votes[i % 4] += 0.7 * bass + 0.3 * ons;
        }
        let best_mod = phase_votes
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap_or(0);
        for (i, &t) in beats.iter().enumerate() {
            if i % 4 == best_mod {
                downbeats.push(t);
            }
        }
    }

    let phrase_boundaries = build_phrase_boundaries(&downbeats);
    RhythmResult {
        bpm,
        beat_interval,
        confidence,
        beats,
        downbeats,
        phrase_boundaries,
    }
}

fn build_phrase_boundaries(downbeats: &[f64]) -> Vec<f64> {
    if downbeats.len() < 4 {
        return Vec::new();
    }
    let step = if downbeats.len() >= 16 { 8 } else { 4 };
    downbeats.iter().step_by(step).copied().collect()
}

fn estimate_key(chroma: &[f64; 12]) -> (i32, f32) {
    let sum: f64 = chroma.iter().sum();
    if sum <= 1e-6 {
        return (-1, 0.0);
    }
    let mut norm = [0.0_f64; 12];
    for (i, &c) in chroma.iter().enumerate() {
        norm[i] = c / sum;
    }

    let mut best_key = -1i32;
    let mut best_corr = -2.0_f64;
    let mut second_corr = -2.0_f64;

    for tonic in 0..12usize {
        for (is_minor, profile) in [(false, &MAJOR_PROFILE), (true, &MINOR_PROFILE)] {
            let mut rotated = [0.0_f64; 12];
            for i in 0..12 {
                rotated[i] = norm[(tonic + i) % 12];
            }
            let r = pearson_corr(&rotated, profile);
            let key_id = if is_minor {
                12 + tonic as i32
            } else {
                tonic as i32
            };
            if r > best_corr {
                second_corr = best_corr;
                best_corr = r;
                best_key = key_id;
            } else if r > second_corr {
                second_corr = r;
            }
        }
    }

    if best_corr <= 0.0 {
        return (-1, 0.0);
    }
    let margin = (best_corr - second_corr).max(0.0);
    let conf = ((0.75 * best_corr + 1.25 * margin) as f32).clamp(0.0, 1.0);
    (best_key, conf)
}

fn pearson_corr(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..12 {
        let xa = a[i] - ma;
        let xb = b[i] - mb;
        num += xa * xb;
        da += xa * xa;
        db += xb * xb;
    }
    let denom = (da * db).sqrt();
    if denom <= 1e-12 { 0.0 } else { num / denom }
}

fn downsample_curve(frames: &[f32], frame_rate: f64, duration: f64) -> Vec<EnergySample> {
    if frames.is_empty() || frame_rate <= 0.0 {
        return Vec::new();
    }
    let step_frames = ((frame_rate * CURVE_STEP_SEC).round() as usize).max(1);
    let mut out = Vec::with_capacity(frames.len() / step_frames + 1);
    let mut idx = 0usize;
    while idx < frames.len() {
        let end = (idx + step_frames).min(frames.len());
        let avg = frames[idx..end].iter().sum::<f32>() / (end - idx).max(1) as f32;
        out.push(EnergySample {
            time: (idx as f64 / frame_rate).min(duration),
            energy: avg.clamp(0.0, 1.0),
        });
        idx += step_frames;
    }
    out
}

fn score_mix_in_candidates(analysis: &TrackAnalysis) -> Vec<MixCandidate> {
    let dur = analysis.duration;
    if dur < 8.0 {
        return vec![MixCandidate {
            time: analysis.audible_start_time,
            score: 0.5,
        }];
    }
    let window_end = (analysis.audible_start_time + (dur * 0.25).clamp(12.0, 45.0)).min(dur * 0.45);
    let mut raw_points: Vec<f64> = Vec::new();
    for &t in &analysis.phrase_boundaries {
        if t >= analysis.audible_start_time.saturating_sub_f64(0.25) && t <= window_end {
            raw_points.push(t);
        }
    }
    for &t in &analysis.downbeats {
        if t >= analysis.audible_start_time.saturating_sub_f64(0.25) && t <= window_end {
            raw_points.push(t);
        }
    }
    raw_points.push(analysis.audible_start_time);
    dedup_times(&mut raw_points, 0.5);

    let mut scored: Vec<MixCandidate> = raw_points
        .into_iter()
        .map(|t| {
            let energy = analysis.average_energy_between(t, t + 4.0).unwrap_or(0.5);
            let vocal =
                crate::analysis::sample_curve_window(&analysis.vocal_activity_mask, t, t + 6.0)
                    .average;
            let on_phrase = analysis.nearest_phrase_boundary(t, None, 0.20).is_some();
            let on_downbeat = analysis.nearest_downbeat(t, None, 0.20).is_some();
            let early_bias = (1.0
                - ((t - analysis.audible_start_time) / window_end.max(1.0)) as f32)
                .clamp(0.0, 1.0);
            let score = (0.30 * (1.0 - vocal)
                + 0.25 * energy.clamp(0.15, 0.85)
                + 0.20 * early_bias
                + if on_phrase {
                    0.15
                } else if on_downbeat {
                    0.10
                } else {
                    0.0
                })
            .clamp(0.0, 1.0);
            MixCandidate { time: t, score }
        })
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(8);
    scored
}

fn score_mix_out_candidates(analysis: &TrackAnalysis) -> Vec<MixCandidate> {
    let dur = analysis.content_end_time.min(analysis.duration);
    if dur < 10.0 {
        return vec![MixCandidate {
            time: (dur - 2.0).max(0.0),
            score: 0.5,
        }];
    }
    let window_start = (dur - (dur * 0.30).clamp(16.0, 50.0)).max(dur * 0.50);
    let window_end = (dur - 2.0).max(window_start);
    let mut raw_points: Vec<f64> = Vec::new();
    for &t in &analysis.phrase_boundaries {
        if t >= window_start && t <= window_end {
            raw_points.push(t);
        }
    }
    for &t in &analysis.downbeats {
        if t >= window_start && t <= window_end {
            raw_points.push(t);
        }
    }
    raw_points.push((dur - 12.0).max(window_start));
    dedup_times(&mut raw_points, 0.5);

    let mut scored: Vec<MixCandidate> = raw_points
        .into_iter()
        .map(|t| {
            let vocal = crate::analysis::sample_curve_window(
                &analysis.vocal_activity_mask,
                t,
                (t + 8.0).min(dur),
            )
            .average;
            let energy = analysis
                .average_energy_between(t, (t + 6.0).min(dur))
                .unwrap_or(0.5);
            let on_phrase = analysis.nearest_phrase_boundary(t, None, 0.20).is_some();
            let on_downbeat = analysis.nearest_downbeat(t, None, 0.20).is_some();
            // Prefer cues ~8-20s before content end.
            let tail = (dur - t).max(0.0);
            let tail_fit = (1.0 - ((tail - 12.0).abs() / 16.0) as f32).clamp(0.0, 1.0);
            let score = (0.35 * (1.0 - vocal)
                + 0.25 * tail_fit
                + 0.15 * energy.clamp(0.15, 0.90)
                + if on_phrase {
                    0.18
                } else if on_downbeat {
                    0.10
                } else {
                    0.0
                })
            .clamp(0.0, 1.0);
            MixCandidate { time: t, score }
        })
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(8);
    scored
}

fn dedup_times(points: &mut Vec<f64>, min_gap: f64) {
    points.sort_by(|a, b| a.total_cmp(b));
    let mut out = Vec::with_capacity(points.len());
    for t in points.drain(..) {
        if out
            .last()
            .is_none_or(|&prev: &f64| (t - prev).abs() >= min_gap)
        {
            out.push(t);
        }
    }
    *points = out;
}

fn percentile(vals: &[f32], q: f32) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = ((sorted.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    sorted[idx]
}

trait SaturatingSubF64 {
    fn saturating_sub_f64(self, rhs: f64) -> f64;
}

impl SaturatingSubF64 for f64 {
    fn saturating_sub_f64(self, rhs: f64) -> f64 {
        (self - rhs).max(0.0)
    }
}

/// Synthesizes a clean deterministic 4/4 dance track (kick on every beat, accent on bar 1,
/// harmonic A-minor triad pad) at `bpm` for tests and CLI diagnostics.
pub fn synth_beat_track_stereo(bpm: f64, duration_sec: f64, sample_rate_hz: u32) -> Vec<f32> {
    let total_frames = (duration_sec * f64::from(sample_rate_hz)).round() as usize;
    let beat_period = 60.0 / bpm;
    let mut out = Vec::with_capacity(total_frames * 2);
    // A minor chord (A3=220, C4=261.63, E4=329.63, A4=440)
    let freqs = [220.0_f64, 261.63, 329.63, 440.0];

    for i in 0..total_frames {
        let t = i as f64 / f64::from(sample_rate_hz);
        let beat_num = (t / beat_period).floor() as usize;
        let beat_local = t - beat_num as f64 * beat_period;

        // Punchy synthetic kick drum (exponential pitch drop 140 Hz -> 48 Hz).
        let kick = if beat_local < 0.14 {
            let env = (-beat_local * 28.0).exp();
            let accent = if beat_num.is_multiple_of(4) {
                1.0
            } else {
                0.72
            };
            let phase =
                2.0 * PI * (48.0 * beat_local + (92.0 / 32.0) * (1.0 - (-32.0 * beat_local).exp()));
            (phase.sin() * env * accent * 0.70) as f32
        } else {
            0.0
        };

        // Warm A-minor harmonic pad.
        let mut pad = 0.0_f64;
        for &f in &freqs {
            pad += (2.0 * PI * f * t).sin();
        }
        let sample = (kick + (pad * 0.09) as f32).clamp(-0.98, 0.98);
        out.push(sample);
        out.push(sample);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_synthesized_124_bpm_four_on_floor_track_accurately() {
        let pcm = synth_beat_track_stereo(124.0, 24.0, 44_100);
        let analysis = analyze_stereo_pcm(&pcm, 44_100);

        assert!(
            (analysis.bpm - 124.0).abs() <= 2.0,
            "expected ~124 BPM, got {:.2}",
            analysis.bpm
        );
        assert!(
            analysis.beat_confidence >= 0.45,
            "expected reliable beat confidence >= 0.45, got {:.3}",
            analysis.beat_confidence
        );
        assert!(analysis.has_reliable_beat());
        assert!(analysis.beats.len() >= 40);
        assert!(analysis.downbeats.len() >= 10);
        assert!(!analysis.mix_in_candidates.is_empty());
        assert!(!analysis.mix_out_candidates.is_empty());
        assert!(
            analysis.has_reliable_key(),
            "key={} conf={}",
            analysis.key,
            analysis.key_confidence
        );
        // A minor (21) or its relative C major (0) are both harmonically consistent for A-C-E triad.
        assert!(
            analysis.key == 21 || analysis.key == 0,
            "expected A minor (21) or C major (0), got {} ({})",
            analysis.key,
            analysis.key_label()
        );
    }
}
