//! Smart Automix, LoopMix, and optional CPU-throttled ONNX musical analysis for Myusic.
//!
//! Ported from BitChord's `playback/smart/` and `native/{analyzer,loopmix}/` modules into
//! 100% safe pure Rust (`#![forbid(unsafe_code)]`), with optional `automix-onnx` runtime
//! acceleration (`beat_this_final0_q8.onnx` & `vocals_umxhq_int8.onnx`).

pub mod analysis;
pub mod dsp_analyzer;
pub mod loop_mix;
pub mod onnx_backend;
pub mod planner;
pub mod policy;
pub mod spectrogram;

pub use analysis::{
    BeatSide, BeatSource, CurveWindow, EnergySample, MixCandidate, MixOutAnchor,
    RELIABLE_BEAT_CONFIDENCE, RELIABLE_KEY_CONFIDENCE, RankedMixCandidate, TrackAnalysis,
    VocalSource, level_from_rms, rms_from_level, sample_curve_window,
};
pub use dsp_analyzer::{
    ANALYSIS_SAMPLE_RATE, StreamingAnalyzer, analyze_mono_11025, analyze_stereo_pcm,
    rebuild_derived_structure, synth_beat_track_stereo,
};
pub use loop_mix::{
    LoopMixBiquad, LoopMixClock, LoopMixPlan, SungSpan, plan_loop_mix, render_loop_mix_bridge,
    sung_spans,
};
pub use onnx_backend::{
    BEAT_THIS_MODEL_NAME, MAX_ONNX_INTRA_THREADS, OnnxAutomixStatus, OnnxError, OnnxModelPaths,
    VOCALS_UMXHQ_MODEL_NAME, analyze_stereo_with_optional_onnx,
};
pub use planner::{
    AutomixMode, FilterTransitionVariant, MAX_MIX_WINDOW_SEC, MIN_MIX_WINDOW_SEC, TransitionPlan,
    TransitionStyle, plan_transition,
};
pub use policy::{
    MAX_INCOMING_RATE, MAX_SIMULTANEOUS_VOCAL_FRACTION, MIN_BEATMATCH_CONFIDENCE,
    MIN_DJ_CONFIDENCE, MIN_INCOMING_RATE, MIN_PHRASE_BEAT_CONFIDENCE, MIN_PHRASE_BEATS,
    TransitionTier, VOCAL_ACTIVE_THRESHOLD, align_tempo_octave, assess_transition_tier,
    audible_seconds_between, harmonic_key_score, incoming_rate_between, rank_mix_in_candidates,
    rank_mix_out_candidates, resolve_mix_out_anchor, simultaneous_vocal_fraction,
    stretch_reachable, vocal_activity_between,
};
pub use spectrogram::{
    BEAT_FFT_SIZE, BEAT_FMAX_HZ, BEAT_FMIN_HZ, BEAT_FPS, BEAT_HOP_SIZE, BEAT_N_MELS,
    BEAT_SAMPLE_RATE, VOCAL_BINS, VOCAL_FFT_SIZE, VOCAL_FIXED_FRAMES, VOCAL_HIGH_HZ,
    VOCAL_HOP_SIZE, VOCAL_LOW_HZ, VOCAL_SAMPLE_RATE, compute_beat_mel_spectrogram,
    compute_vocal_stft_tensor, reduce_vocal_band_curve, resample_stereo_to_mono_22050,
};
