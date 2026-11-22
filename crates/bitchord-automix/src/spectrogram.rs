//! Exact pure-Rust Mel & STFT feature extractors matching upstream ONNX model contracts:
//! - [`BeatSpectrogram`]: `beat_this_final0_q8.onnx` (22,050 Hz mono, FFT 1024, hop 441 = 50 fps,
//!   128 Slaney mels 30..11,000 Hz, `log1p(1000 * mag)`), ported from `mel_spectrogram.cpp`.
//! - [`VocalSpectrogram`]: `vocals_umxhq_int8.onnx` (44,100 Hz planar stereo, periodic Hann
//!   FFT 4096, hop 1024, 2049 bins, `FIXED_FRAMES = 960`), ported from `vocal_spectrogram.cpp`.

use std::f64::consts::PI;

use crate::dsp_analyzer::fft_in_place;

/// Parameters for `beat_this_final0_q8.onnx` mel spectrogram (`BeatTracker.kt`).
pub const BEAT_SAMPLE_RATE: u32 = 22_050;
pub const BEAT_FFT_SIZE: usize = 1024;
pub const BEAT_HOP_SIZE: usize = 441;
pub const BEAT_N_MELS: usize = 128;
pub const BEAT_FMIN_HZ: f64 = 30.0;
pub const BEAT_FMAX_HZ: f64 = 11_000.0;
pub const BEAT_FPS: f64 = 50.0;

/// Parameters for `vocals_umxhq_int8.onnx` stereo magnitude STFT (`VocalTracker.kt`).
pub const VOCAL_SAMPLE_RATE: u32 = 44_100;
pub const VOCAL_FFT_SIZE: usize = 4096;
pub const VOCAL_HOP_SIZE: usize = 1024;
pub const VOCAL_BINS: usize = VOCAL_FFT_SIZE / 2 + 1;
pub const VOCAL_FIXED_FRAMES: usize = 960;
pub const VOCAL_LOW_HZ: f64 = 200.0;
pub const VOCAL_HIGH_HZ: f64 = 4_000.0;

/// Slaney mel scale conversion matching torchaudio/librosa (`mel_scale="slaney"`).
fn hz_to_mel_slaney(hz: f64) -> f64 {
    const F_MIN: f64 = 0.0;
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1_000.0;
    const MIN_LOG_MEL: f64 = (MIN_LOG_HZ - F_MIN) / F_SP;
    let logstep = (6.4_f64).ln() / 27.0;
    if hz >= MIN_LOG_HZ {
        MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() / logstep
    } else {
        (hz - F_MIN) / F_SP
    }
}

fn mel_to_hz_slaney(mel: f64) -> f64 {
    const F_MIN: f64 = 0.0;
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1_000.0;
    const MIN_LOG_MEL: f64 = (MIN_LOG_HZ - F_MIN) / F_SP;
    let logstep = (6.4_f64).ln() / 27.0;
    if mel >= MIN_LOG_MEL {
        MIN_LOG_HZ * (logstep * (mel - MIN_LOG_MEL)).exp()
    } else {
        F_MIN + F_SP * mel
    }
}

/// Computes the `[frames, 128]` row-major Slaney log-mel spectrogram for `beat_this_final0_q8.onnx`
/// from 22,050 Hz mono PCM (`log1p(1000 * mel_mag)`).
pub fn compute_beat_mel_spectrogram(mono_22050: &[f32]) -> (Vec<f32>, usize) {
    if mono_22050.is_empty() {
        return (Vec::new(), 0);
    }

    // Center-pad with reflect/zero padding (`BEAT_FFT_SIZE / 2`) matching torchaudio center=True.
    let pad = BEAT_FFT_SIZE / 2;
    let frames = mono_22050.len() / BEAT_HOP_SIZE + 1;
    let bins = BEAT_FFT_SIZE / 2 + 1;

    let mut hann = [0.0_f32; BEAT_FFT_SIZE];
    for (i, slot) in hann.iter_mut().enumerate() {
        *slot = (0.5 * (1.0 - (2.0 * PI * i as f64 / BEAT_FFT_SIZE as f64).cos())) as f32;
    }

    // Build sparse triangular Slaney mel filterbank (`norm=None`).
    let mel_lo = hz_to_mel_slaney(BEAT_FMIN_HZ);
    let mel_hi = hz_to_mel_slaney(BEAT_FMAX_HZ);
    let mut mel_pts = [0.0_f64; BEAT_N_MELS + 2];
    for (i, slot) in mel_pts.iter_mut().enumerate() {
        let m = mel_lo + (mel_hi - mel_lo) * (i as f64 / (BEAT_N_MELS + 1) as f64);
        *slot = mel_to_hz_slaney(m);
    }

    let bin_hz = f64::from(BEAT_SAMPLE_RATE) / BEAT_FFT_SIZE as f64;
    let mut filter_weights: Vec<Vec<(usize, f32)>> = Vec::with_capacity(BEAT_N_MELS);
    for m in 0..BEAT_N_MELS {
        let f_left = mel_pts[m];
        let f_center = mel_pts[m + 1];
        let f_right = mel_pts[m + 2];
        let mut row = Vec::new();
        for b in 0..bins {
            let hz = b as f64 * bin_hz;
            let w = if hz > f_left && hz <= f_center {
                (hz - f_left) / (f_center - f_left).max(1e-9)
            } else if hz > f_center && hz < f_right {
                (f_right - hz) / (f_right - f_center).max(1e-9)
            } else {
                0.0
            };
            if w > 0.0 {
                row.push((b, w as f32));
            }
        }
        filter_weights.push(row);
    }

    let mut out = vec![0.0_f32; frames * BEAT_N_MELS];
    let mut re = [0.0_f32; BEAT_FFT_SIZE];
    let mut im = [0.0_f32; BEAT_FFT_SIZE];
    let mut mags = [0.0_f32; BEAT_FFT_SIZE / 2 + 1];

    for f in 0..frames {
        let center = f as isize * BEAT_HOP_SIZE as isize;
        for i in 0..BEAT_FFT_SIZE {
            let idx = center + i as isize - pad as isize;
            let sample = if idx >= 0 && (idx as usize) < mono_22050.len() {
                mono_22050[idx as usize]
            } else {
                0.0
            };
            re[i] = sample * hann[i];
            im[i] = 0.0;
        }
        fft_in_place(&mut re, &mut im);
        for b in 0..bins {
            mags[b] = (re[b] * re[b] + im[b] * im[b]).sqrt();
        }
        let row_out = &mut out[f * BEAT_N_MELS..(f + 1) * BEAT_N_MELS];
        for (m, weights) in filter_weights.iter().enumerate() {
            let mut sum = 0.0_f32;
            for &(b, w) in weights {
                sum += mags[b] * w;
            }
            row_out[m] = (1_000.0 * sum).ln_1p();
        }
    }
    (out, frames)
}

/// Computes the stereo `[2, 2049, FIXED_FRAMES]` magnitude STFT tensor for `vocals_umxhq_int8.onnx`
/// from 44,100 Hz interleaved stereo PCM, returning `(tensor, usable_frames)`.
pub fn compute_vocal_stft_tensor(stereo_44100: &[f32]) -> (Vec<f32>, usize) {
    let total_samples = stereo_44100.len() / 2;
    let usable_frames = if total_samples == 0 {
        0
    } else {
        (total_samples / VOCAL_HOP_SIZE + 1).min(VOCAL_FIXED_FRAMES)
    };
    let mut out = vec![0.0_f32; 2 * VOCAL_BINS * VOCAL_FIXED_FRAMES];
    if usable_frames == 0 {
        return (out, 0);
    }

    let pad = VOCAL_FFT_SIZE / 2;
    let mut hann = vec![0.0_f32; VOCAL_FFT_SIZE];
    for (i, slot) in hann.iter_mut().enumerate() {
        *slot = (0.5 * (1.0 - (2.0 * PI * i as f64 / VOCAL_FFT_SIZE as f64).cos())) as f32;
    }

    let mut re = vec![0.0_f32; VOCAL_FFT_SIZE];
    let mut im = vec![0.0_f32; VOCAL_FFT_SIZE];

    for ch in 0..2usize {
        for f in 0..usable_frames {
            let center = f as isize * VOCAL_HOP_SIZE as isize;
            for i in 0..VOCAL_FFT_SIZE {
                let idx = center + i as isize - pad as isize;
                let s = if idx >= 0 && (idx as usize) < total_samples {
                    stereo_44100[idx as usize * 2 + ch]
                } else {
                    0.0
                };
                re[i] = s * hann[i];
                im[i] = 0.0;
            }
            fft_in_place(&mut re, &mut im);
            for b in 0..VOCAL_BINS {
                let mag = (re[b] * re[b] + im[b] * im[b]).sqrt();
                out[(ch * VOCAL_BINS + b) * VOCAL_FIXED_FRAMES + f] = mag;
            }
        }
    }

    (out, usable_frames)
}

/// Reduces a `[2, 2049, FIXED_FRAMES]` mixture + target vocal magnitude tensor pair into a
/// per-frame vocal activity ratio (`0.0..=1.0`) across `200..=4000 Hz`, matching `VocalTracker.kt`.
pub fn reduce_vocal_band_curve(mix: &[f32], target: &[f32], usable_frames: usize) -> Vec<f32> {
    let expected = 2 * VOCAL_BINS * VOCAL_FIXED_FRAMES;
    if mix.len() < expected || target.len() < expected || usable_frames == 0 {
        return Vec::new();
    }
    let frames = usable_frames.min(VOCAL_FIXED_FRAMES);
    let low_bin = ((VOCAL_LOW_HZ * VOCAL_FFT_SIZE as f64 / f64::from(VOCAL_SAMPLE_RATE)).floor()
        as usize)
        .min(VOCAL_BINS - 1);
    let high_bin = ((VOCAL_HIGH_HZ * VOCAL_FFT_SIZE as f64 / f64::from(VOCAL_SAMPLE_RATE)).ceil()
        as usize)
        .min(VOCAL_BINS - 1);
    if high_bin <= low_bin {
        return Vec::new();
    }

    let mut curve = vec![0.0_f32; frames];
    for (frame, slot) in curve.iter_mut().enumerate() {
        let mut sum = 0.0_f64;
        let mut count = 0usize;
        for ch in 0..2 {
            for b in low_bin..=high_bin {
                let idx = (ch * VOCAL_BINS + b) * VOCAL_FIXED_FRAMES + frame;
                let mv = mix[idx];
                if mv <= 1e-6 {
                    continue;
                }
                sum += f64::from((target[idx] / mv).clamp(0.0, 1.0));
                count += 1;
            }
        }
        *slot = if count > 0 {
            (sum / count as f64) as f32
        } else {
            0.0
        };
    }
    curve
}

/// Resamples interleaved stereo PCM to 22,050 Hz mono for `BeatSpectrogram`.
pub fn resample_stereo_to_mono_22050(stereo: &[f32], input_rate_hz: u32) -> Vec<f32> {
    let frames = stereo.len() / 2;
    if frames == 0 {
        return Vec::new();
    }
    let step = f64::from(input_rate_hz.max(8_000)) / f64::from(BEAT_SAMPLE_RATE);
    let out_len = ((frames as f64) / step).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    let mut pos = 0.0_f64;
    while (pos as usize) < frames {
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let m0 = 0.5 * (stereo[idx * 2] + stereo[idx * 2 + 1]);
        let m1 = if idx + 1 < frames {
            0.5 * (stereo[(idx + 1) * 2] + stereo[(idx + 1) * 2 + 1])
        } else {
            m0
        };
        out.push(m0 + (m1 - m0) * frac);
        pos += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_and_vocal_spectrograms_produce_expected_tensor_shapes_and_finite_values() {
        // 1 second of 440 Hz tone at 22,050 Hz -> 51 frames x 128 mels.
        let mono: Vec<f32> = (0..BEAT_SAMPLE_RATE as usize)
            .map(|i| (2.0 * PI * 440.0 * i as f64 / f64::from(BEAT_SAMPLE_RATE)).sin() as f32)
            .collect();
        let (mel, frames) = compute_beat_mel_spectrogram(&mono);
        assert_eq!(frames, 51);
        assert_eq!(mel.len(), 51 * BEAT_N_MELS);
        assert!(mel.iter().all(|v| v.is_finite() && *v >= 0.0));
        assert!(mel.iter().copied().fold(0.0_f32, f32::max) > 1.0);

        // 0.25 second of stereo 440 Hz tone at 44,100 Hz -> 11 frames in [2, 2049, 960].
        let mut stereo = Vec::with_capacity(11_025 * 2);
        for i in 0..11_025usize {
            let s = (2.0 * PI * 440.0 * i as f64 / f64::from(VOCAL_SAMPLE_RATE)).sin() as f32;
            stereo.push(s);
            stereo.push(s);
        }
        let (mix, usable) = compute_vocal_stft_tensor(&stereo);
        assert_eq!(usable, 11);
        assert_eq!(mix.len(), 2 * VOCAL_BINS * VOCAL_FIXED_FRAMES);
        let curve = reduce_vocal_band_curve(&mix, &mix, usable);
        assert_eq!(curve.len(), 11);
        // Identical target & mix -> ratio ~1.0 across active frames.
        assert!(curve[5] > 0.95);
    }
}
