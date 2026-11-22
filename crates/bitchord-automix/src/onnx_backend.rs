//! Optional CPU-throttled ONNX Runtime inference backend for:
//! - `beat_this_final0_q8.onnx` (beat & downbeat tracker, 50 fps Slaney mel)
//! - `vocals_umxhq_int8.onnx` (UMX-HQ vocal activity mask, 44.1 kHz STFT)
//!
//! Always enforces laptop-safe CPU/RAM limits (`intra_threads <= 2`, `inter_threads = 1`,
//! arena allocator disabled) and degrades cleanly to the pure-Rust DSP analyzer whenever
//! model weights, `libonnxruntime.so`, or the `automix-onnx` feature are unavailable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::analysis::TrackAnalysis;
use crate::dsp_analyzer::analyze_stereo_pcm;

/// Expected ONNX filename for beat/downbeat detection (`BeatTracker.kt`).
pub const BEAT_THIS_MODEL_NAME: &str = "beat_this_final0_q8.onnx";
/// Expected ONNX filename for vocal separation masking (`VocalTracker.kt`).
pub const VOCALS_UMXHQ_MODEL_NAME: &str = "vocals_umxhq_int8.onnx";

/// Hard ceiling on ONNX Runtime CPU threads for laptop thermal safety (`ADR-007`).
pub const MAX_ONNX_INTRA_THREADS: usize = 2;

#[derive(Debug, Error)]
pub enum OnnxError {
    #[error("ONNX backend feature `automix-onnx` is disabled in this build")]
    FeatureDisabled,
    #[error("ONNX model file not found: {0}")]
    ModelNotFound(PathBuf),
    #[error("libonnxruntime.so not found (set ORT_DYLIB_PATH or install onnxruntime)")]
    RuntimeNotFound,
    #[error("ONNX runtime error: {0}")]
    Runtime(String),
}

/// Resolved filesystem paths for optional Automix ONNX model assets.
#[derive(Debug, Clone, Default)]
pub struct OnnxModelPaths {
    pub models_dir: PathBuf,
    pub beat_model: Option<PathBuf>,
    pub vocal_model: Option<PathBuf>,
    pub runtime_dylib: Option<PathBuf>,
}

/// User-facing capability snapshot displayed in Settings UI and `bitchord-cli automix status`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnnxAutomixStatus {
    pub feature_compiled: bool,
    pub runtime_available: bool,
    pub beat_model_present: bool,
    pub vocal_model_present: bool,
    pub models_dir: String,
    pub active_backend_label: String,
}

impl OnnxModelPaths {
    /// Discovers ONNX model files inside `BITCHORD_MODELS_DIR` (if set) or `default_models_dir`,
    /// and probes standard Linux paths for `libonnxruntime.so`.
    pub fn discover(default_models_dir: &Path) -> Self {
        let models_dir = std::env::var_os("BITCHORD_MODELS_DIR")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| default_models_dir.to_path_buf());

        let beat_cand = models_dir.join(BEAT_THIS_MODEL_NAME);
        let vocal_cand = models_dir.join(VOCALS_UMXHQ_MODEL_NAME);

        let beat_model = beat_cand.is_file().then_some(beat_cand);
        let vocal_model = vocal_cand.is_file().then_some(vocal_cand);
        let runtime_dylib = find_onnxruntime_dylib(&models_dir);

        Self {
            models_dir,
            beat_model,
            vocal_model,
            runtime_dylib,
        }
    }

    pub fn status(&self) -> OnnxAutomixStatus {
        let feature_compiled = cfg!(feature = "automix-onnx");
        let runtime_available = self.runtime_dylib.is_some();
        let beat_model_present = self.beat_model.is_some();
        let vocal_model_present = self.vocal_model.is_some();

        let active_backend_label =
            if feature_compiled && runtime_available && (beat_model_present || vocal_model_present)
            {
                match (beat_model_present, vocal_model_present) {
                    (true, true) => "ONNX Runtime (Beat-This + UMX-HQ, <=2 threads)".to_owned(),
                    (true, false) => "ONNX Beat-This + Pure-Rust Vocal DSP".to_owned(),
                    (false, true) => "Pure-Rust Beat DSP + ONNX UMX-HQ Vocals".to_owned(),
                    (false, false) => "Pure-Rust DSP Analyzer".to_owned(),
                }
            } else {
                "Pure-Rust DSP Analyzer (ONNX optional)".to_owned()
            };

        OnnxAutomixStatus {
            feature_compiled,
            runtime_available,
            beat_model_present,
            vocal_model_present,
            models_dir: self.models_dir.display().to_string(),
            active_backend_label,
        }
    }
}

fn find_onnxruntime_dylib(models_dir: &Path) -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("ORT_DYLIB_PATH") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates = [
        models_dir.join("libonnxruntime.so"),
        PathBuf::from("/usr/lib/libonnxruntime.so"),
        PathBuf::from("/usr/lib64/libonnxruntime.so"),
        PathBuf::from("/usr/local/lib/libonnxruntime.so"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu/libonnxruntime.so"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Runs pure-Rust DSP analysis and, when `automix-onnx` is enabled and local ONNX models +
/// `libonnxruntime.so` are available, refines beats/downbeats (`beat_this_final0_q8.onnx`)
/// and/or the vocal mask (`vocals_umxhq_int8.onnx`).
pub fn analyze_stereo_with_optional_onnx(
    interleaved_stereo: &[f32],
    sample_rate_hz: u32,
    paths: Option<&OnnxModelPaths>,
) -> TrackAnalysis {
    #[allow(unused_mut)]
    let mut analysis = analyze_stereo_pcm(interleaved_stereo, sample_rate_hz);
    #[cfg(feature = "automix-onnx")]
    if let Some(p) = paths {
        let _ = refine_with_onnx(interleaved_stereo, sample_rate_hz, p, &mut analysis);
    }
    #[cfg(not(feature = "automix-onnx"))]
    let _ = paths;
    analysis
}

#[cfg(feature = "automix-onnx")]
fn refine_with_onnx(
    interleaved_stereo: &[f32],
    sample_rate_hz: u32,
    paths: &OnnxModelPaths,
    analysis: &mut TrackAnalysis,
) -> Result<(), OnnxError> {
    use ort::session::{Session, builder::GraphOptimizationLevel};
    use ort::value::Tensor;

    use crate::analysis::{BeatSource, EnergySample, VocalSource};
    use crate::dsp_analyzer::rebuild_derived_structure;
    use crate::spectrogram::{
        BEAT_FPS, BEAT_N_MELS, VOCAL_BINS, VOCAL_FIXED_FRAMES, VOCAL_HOP_SIZE, VOCAL_SAMPLE_RATE,
        compute_beat_mel_spectrogram, compute_vocal_stft_tensor, reduce_vocal_band_curve,
        resample_stereo_to_mono_22050,
    };

    let dylib = paths
        .runtime_dylib
        .as_ref()
        .ok_or(OnnxError::RuntimeNotFound)?;
    let env = ort::init_from(dylib)
        .map_err(|e| OnnxError::Runtime(e.to_string()))?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .build()
        .map_err(|e| OnnxError::Runtime(e.to_string()))?;

    let mut updated = false;

    // 1. Beat-This neural beat + downbeat tracking (`CHUNK_FRAMES = 1500`, `BORDER_FRAMES = 6`).
    if let Some(beat_path) = paths.beat_model.as_ref() {
        let mono_22k = resample_stereo_to_mono_22050(interleaved_stereo, sample_rate_hz);
        let (mel, total_frames) = compute_beat_mel_spectrogram(&mono_22k);
        if total_frames >= 32 {
            let mut session = Session::builder(&env)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_intra_threads(MAX_ONNX_INTRA_THREADS)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_inter_threads(1)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_memory_pattern(false)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .commit_from_file(beat_path)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?;

            const CHUNK_FRAMES: usize = 1_500;
            const BORDER_FRAMES: usize = 6;
            let mut beat_logits = vec![f32::NEG_INFINITY; total_frames];
            let mut down_logits = vec![f32::NEG_INFINITY; total_frames];

            let stride = CHUNK_FRAMES - 2 * BORDER_FRAMES;
            let mut start = 0usize;
            while start < total_frames {
                let end = (start + CHUNK_FRAMES).min(total_frames);
                let chunk_len = end - start;
                let slice = mel[start * BEAT_N_MELS..end * BEAT_N_MELS].to_vec();
                let tensor = Tensor::from_array(([1usize, chunk_len, BEAT_N_MELS], slice))
                    .map_err(|e| OnnxError::Runtime(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs![tensor])
                    .map_err(|e| OnnxError::Runtime(e.to_string()))?;

                let (_, b_slice) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| OnnxError::Runtime(e.to_string()))?;
                let (_, d_slice) = outputs[1]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| OnnxError::Runtime(e.to_string()))?;

                let keep_lo = if start == 0 { 0 } else { BORDER_FRAMES };
                let keep_hi = if end == total_frames {
                    chunk_len
                } else {
                    chunk_len.saturating_sub(BORDER_FRAMES)
                };
                for i in keep_lo..keep_hi {
                    if i < b_slice.len() && start + i < total_frames {
                        beat_logits[start + i] = b_slice[i];
                    }
                    if i < d_slice.len() && start + i < total_frames {
                        down_logits[start + i] = d_slice[i];
                    }
                }
                if end == total_frames {
                    break;
                }
                start += stride;
            }

            let beats = pick_logit_peaks(&beat_logits, 7, BEAT_FPS);
            let downbeats = pick_logit_peaks(&down_logits, 7, BEAT_FPS);
            if beats.len() >= 8 {
                let mut intervals: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
                intervals.sort_by(|a, b| a.total_cmp(b));
                let median_interval = intervals[intervals.len() / 2];
                if (0.25..=1.2).contains(&median_interval) {
                    analysis.beat_interval = median_interval;
                    analysis.bpm = (60.0 / median_interval) as f32;
                    analysis.beat_confidence = 0.92;
                    analysis.beats = beats;
                    if !downbeats.is_empty() {
                        analysis.downbeats = downbeats;
                    }
                    analysis.beat_source = BeatSource::OnnxBeatThis;
                    updated = true;
                }
            }
        }
    }

    // 2. UMX-HQ vocal separation mask (`FIXED_FRAMES = 960`, ~22.8s window).
    if let Some(vocal_path) = paths.vocal_model.as_ref()
        && sample_rate_hz == VOCAL_SAMPLE_RATE
    {
        let (mix, usable) = compute_vocal_stft_tensor(interleaved_stereo);
        if usable > 4 {
            let mut session = Session::builder(&env)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_intra_threads(MAX_ONNX_INTRA_THREADS)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_inter_threads(1)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .with_memory_pattern(false)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?
                .commit_from_file(vocal_path)
                .map_err(|e| OnnxError::Runtime(e.to_string()))?;

            let tensor =
                Tensor::from_array(([1usize, 2, VOCAL_BINS, VOCAL_FIXED_FRAMES], mix.clone()))
                    .map_err(|e| OnnxError::Runtime(e.to_string()))?;
            let outputs = session
                .run(ort::inputs![tensor])
                .map_err(|e| OnnxError::Runtime(e.to_string()))?;
            let (_, target_slice) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| OnnxError::Runtime(e.to_string()))?;
            let curve = reduce_vocal_band_curve(&mix, target_slice, usable);
            if !curve.is_empty() {
                let frame_sec = VOCAL_HOP_SIZE as f64 / f64::from(VOCAL_SAMPLE_RATE);
                analysis.vocal_activity_mask = curve
                    .into_iter()
                    .enumerate()
                    .map(|(i, e)| EnergySample {
                        time: i as f64 * frame_sec,
                        energy: e,
                    })
                    .collect();
                analysis.vocal_source = VocalSource::OnnxUmxHq;
                updated = true;
            }
        }
    }

    if updated {
        rebuild_derived_structure(analysis);
    }
    Ok(())
}

#[cfg(feature = "automix-onnx")]
fn pick_logit_peaks(logits: &[f32], window: usize, fps: f64) -> Vec<f64> {
    let half = window / 2;
    let mut peaks = Vec::new();
    for (i, &val) in logits.iter().enumerate() {
        if !val.is_finite() || val <= 0.0 {
            continue;
        }
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(logits.len());
        if logits[lo..hi].iter().all(|&other| val >= other) {
            peaks.push(i as f64 / fps);
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_discovery_degrades_gracefully_when_assets_are_absent() {
        let dir = std::env::temp_dir().join(format!(
            "bitchord_automix_empty_models_{}",
            std::process::id()
        ));
        let paths = OnnxModelPaths::discover(&dir);
        let status = paths.status();
        assert!(!status.beat_model_present);
        assert!(!status.vocal_model_present);
        assert!(status.active_backend_label.contains("Pure-Rust"));
    }
}
