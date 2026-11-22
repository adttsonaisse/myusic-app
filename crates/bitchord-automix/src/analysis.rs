//! Core musical analysis model (`TrackAnalysis`) and curve query helpers
//! ported from upstream `TrackAnalysis.kt`.

use serde::{Deserialize, Serialize};

/// Minimum beat confidence required for beatmatching or phrase alignment.
pub const RELIABLE_BEAT_CONFIDENCE: f32 = 0.45;
/// Minimum Krumhansl-Schmuckler key correlation considered reliable.
pub const RELIABLE_KEY_CONFIDENCE: f32 = 0.30;

/// Provenance of beat/downbeat timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BeatSource {
    #[default]
    Dsp,
    OnnxBeatThis,
}

/// Provenance of the vocal-activity mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VocalSource {
    #[default]
    Dsp,
    OnnxUmxHq,
}

/// Time-indexed normalized scalar energy/activity sample (`0.0..=1.0`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnergySample {
    pub time: f64,
    pub energy: f32,
}

/// Candidate mix-in or mix-out cue point with an analyzer suitability score (`0.0..=1.0`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixCandidate {
    pub time: f64,
    pub score: f32,
}

/// Policy-ranked mix cue candidate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedMixCandidate {
    pub time: f64,
    pub score: f32,
}

/// Resolved outgoing mix-out point and whether it landed directly on a structural cue.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixOutAnchor {
    pub time: f64,
    pub exact: bool,
}

/// Directional beat lookup constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatSide {
    Before,
    After,
}

/// Aggregated statistics across a curve time window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CurveWindow {
    pub samples: usize,
    pub average: f32,
    pub peak: f32,
}

/// Musical structure, rhythm, tonal key, energy, and vocal activity analysis for a track.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAnalysis {
    pub bpm: f32,
    pub beat_interval: f64,
    pub beat_confidence: f32,
    #[serde(default)]
    pub beats: Vec<f64>,
    #[serde(default)]
    pub downbeats: Vec<f64>,
    #[serde(default)]
    pub phrase_boundaries: Vec<f64>,
    /// Musical key (`-1` = unknown, `0..=11` = C..B major, `12..=23` = C..B minor).
    pub key: i32,
    pub key_confidence: f32,
    pub duration: f64,
    pub audible_start_time: f64,
    pub content_end_time: f64,
    #[serde(default)]
    pub mix_in_candidates: Vec<MixCandidate>,
    #[serde(default)]
    pub mix_out_candidates: Vec<MixCandidate>,
    #[serde(default)]
    pub energy_curve: Vec<EnergySample>,
    #[serde(default)]
    pub low_energy_curve: Vec<EnergySample>,
    #[serde(default)]
    pub vocal_activity_mask: Vec<EnergySample>,
    pub vocal_probability: f32,
    #[serde(default)]
    pub beat_source: BeatSource,
    #[serde(default)]
    pub vocal_source: VocalSource,
}

const PITCH_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

impl TrackAnalysis {
    pub fn has_reliable_beat(&self) -> bool {
        self.bpm.is_finite()
            && (40.0..=240.0).contains(&self.bpm)
            && self.beat_interval.is_finite()
            && self.beat_interval > 0.15
            && self.beat_confidence >= RELIABLE_BEAT_CONFIDENCE
            && self.beats.len() >= 8
    }

    pub fn has_reliable_key(&self) -> bool {
        (0..24).contains(&self.key) && self.key_confidence >= RELIABLE_KEY_CONFIDENCE
    }

    /// Human-readable musical key label (e.g. `"A minor"`, `"C major"`, `"Unknown"`).
    pub fn key_label(&self) -> String {
        if !(0..24).contains(&self.key) {
            return "Unknown".to_owned();
        }
        let pitch = PITCH_NAMES[(self.key % 12) as usize];
        let mode = if self.key < 12 { "major" } else { "minor" };
        format!("{pitch} {mode}")
    }

    /// Camelot wheel notation (e.g. `"8A"`, `"8B"`) if key is known.
    pub fn camelot_code(&self) -> Option<&'static str> {
        // Index 0..11 = C..B major (B wheel), 12..23 = C..B minor (A wheel).
        const CAMELOT: [&str; 24] = [
            "8B", "3B", "10B", "5B", "12B", "7B", "2B", "9B", "4B", "11B", "6B", "1B", "5A", "12A",
            "7A", "2A", "9A", "4A", "11A", "6A", "1A", "8A", "3A", "10A",
        ];
        usize::try_from(self.key)
            .ok()
            .and_then(|i| CAMELOT.get(i).copied())
    }

    pub fn has_vocal_mask(&self) -> bool {
        !self.vocal_activity_mask.is_empty()
    }

    /// Nearest beat timestamp within `max_delta_sec` (optionally constrained to before/after).
    pub fn nearest_beat(
        &self,
        time: f64,
        side: Option<BeatSide>,
        max_delta_sec: f64,
    ) -> Option<f64> {
        nearest_timestamp(&self.beats, time, side, max_delta_sec)
    }

    /// Nearest downbeat timestamp within `max_delta_sec`.
    pub fn nearest_downbeat(
        &self,
        time: f64,
        side: Option<BeatSide>,
        max_delta_sec: f64,
    ) -> Option<f64> {
        nearest_timestamp(&self.downbeats, time, side, max_delta_sec)
    }

    /// Nearest 8-bar phrase boundary timestamp within `max_delta_sec`.
    pub fn nearest_phrase_boundary(
        &self,
        time: f64,
        side: Option<BeatSide>,
        max_delta_sec: f64,
    ) -> Option<f64> {
        nearest_timestamp(&self.phrase_boundaries, time, side, max_delta_sec)
    }

    pub fn average_energy_between(&self, start: f64, end: f64) -> Option<f32> {
        let w = sample_curve_window(&self.energy_curve, start, end);
        (w.samples > 0).then_some(w.average)
    }

    pub fn average_low_energy_between(&self, start: f64, end: f64) -> Option<f32> {
        let w = sample_curve_window(&self.low_energy_curve, start, end);
        (w.samples > 0).then_some(w.average)
    }

    pub fn peak_low_energy_between(&self, start: f64, end: f64) -> Option<f32> {
        let w = sample_curve_window(&self.low_energy_curve, start, end);
        (w.samples > 0).then_some(w.peak)
    }
}

/// Samples a sorted [`EnergySample`] curve across `[start, end]`, falling back to
/// the single nearest point if the window falls between two grid timestamps.
pub fn sample_curve_window(curve: &[EnergySample], start: f64, end: f64) -> CurveWindow {
    if curve.is_empty() || !start.is_finite() || !end.is_finite() {
        return CurveWindow::default();
    }
    let lo = start.min(end);
    let hi = start.max(end);
    let first = curve.partition_point(|s| s.time < lo);
    let mut sum = 0.0_f64;
    let mut peak = 0.0_f32;
    let mut count = 0usize;
    for sample in &curve[first..] {
        if sample.time > hi {
            break;
        }
        let e = sample.energy.clamp(0.0, 1.0);
        sum += f64::from(e);
        peak = peak.max(e);
        count += 1;
    }
    if count > 0 {
        return CurveWindow {
            samples: count,
            average: (sum / count as f64) as f32,
            peak,
        };
    }
    // Narrow window between grid steps: evaluate nearest neighbor to midpoint.
    let mid = 0.5 * (lo + hi);
    let idx = curve.partition_point(|s| s.time < mid);
    let pick = match (
        idx.checked_sub(1).and_then(|i| curve.get(i)),
        curve.get(idx),
    ) {
        (Some(a), Some(b)) => {
            if (mid - a.time).abs() <= (b.time - mid).abs() {
                a
            } else {
                b
            }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return CurveWindow::default(),
    };
    let e = pick.energy.clamp(0.0, 1.0);
    CurveWindow {
        samples: 1,
        average: e,
        peak: e,
    }
}

pub fn nearest_timestamp(
    points: &[f64],
    time: f64,
    side: Option<BeatSide>,
    max_delta_sec: f64,
) -> Option<f64> {
    if points.is_empty() || !time.is_finite() {
        return None;
    }
    let idx = points.partition_point(|&t| t < time);
    let mut best: Option<(f64, f64)> = None;
    let check = |cand: f64, best: &mut Option<(f64, f64)>| {
        let valid_side = match side {
            None => true,
            Some(BeatSide::Before) => cand <= time + 1e-6,
            Some(BeatSide::After) => cand >= time - 1e-6,
        };
        if !valid_side {
            return;
        }
        let dist = (cand - time).abs();
        if dist <= max_delta_sec && best.is_none_or(|(_, d)| dist < d) {
            *best = Some((cand, dist));
        }
    };
    if idx > 0 {
        check(points[idx - 1], &mut best);
    }
    if idx < points.len() {
        check(points[idx], &mut best);
    }
    best.map(|(t, _)| t)
}

/// Converts linear RMS amplitude (`0..=1`) to perceptually compressed level (`0..=1`)
/// matching `TrackAnalysis.levelFromRms` (`-60 dB .. 0 dB`).
pub fn level_from_rms(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 1e-3 {
        return 0.0;
    }
    let db = 20.0 * rms.min(1.0).log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Inverse of [`level_from_rms`], mapping normalized level (`0..=1`) back to linear RMS.
pub fn rms_from_level(level: f32) -> f32 {
    let l = level.clamp(0.0, 1.0);
    if l <= 0.0 {
        return 0.0;
    }
    10.0_f32.powf((l * 60.0 - 60.0) / 20.0)
}
