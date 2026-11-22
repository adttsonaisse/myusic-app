//! Beatmatching, vocal-clash guard, Camelot harmonic scoring, and cue-ranking policy
//! ported from upstream `TransitionPolicy.kt`.

use crate::analysis::{
    BeatSide, MixOutAnchor, RankedMixCandidate, TrackAnalysis, sample_curve_window,
};

pub const MIN_BEATMATCH_CONFIDENCE: f32 = 0.45;
pub const MIN_DJ_CONFIDENCE: f32 = 0.55;
pub const MIN_INCOMING_RATE: f32 = 0.90;
pub const MAX_INCOMING_RATE: f32 = 1.25;
pub const VOCAL_ACTIVE_THRESHOLD: f32 = 0.25;
pub const MAX_SIMULTANEOUS_VOCAL_FRACTION: f32 = 0.35;
pub const MIN_PHRASE_BEAT_CONFIDENCE: f32 = 0.55;
pub const MIN_PHRASE_BEATS: usize = 32;

/// Transition capability tier determined from two tracks' analysis metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionTier {
    /// Both tracks have high beat confidence + compatible tempo within `[0.90, 1.25]`
    /// and enough structure for full DJ beatmatched/bass-swap/phrase blending.
    DjBeatmatch,
    /// Moderate beat or phrase structure; smooth structural blend without hard tempo warping.
    PhraseBlend,
    /// Unmetered, spoken word, or incompatible tempos -> clean crossfade / filter exit.
    Crossfade,
}

/// Aligns `source_bpm` by half/double-time octave folds (`0.5x`, `1x`, `2x`) closest to `target_bpm`.
pub fn align_tempo_octave(source_bpm: f32, target_bpm: f32) -> f32 {
    if !source_bpm.is_finite() || source_bpm <= 0.0 || !target_bpm.is_finite() || target_bpm <= 0.0
    {
        return source_bpm;
    }
    let candidates = [source_bpm * 0.5, source_bpm, source_bpm * 2.0];
    candidates
        .into_iter()
        .min_by(|a, b| {
            let da = (a / target_bpm).ln().abs();
            let db = (b / target_bpm).ln().abs();
            da.total_cmp(&db)
        })
        .unwrap_or(source_bpm)
}

/// Playback rate multiplier needed on `incoming_bpm` to match `outgoing_bpm` (after octave fold).
pub fn incoming_rate_between(outgoing_bpm: f32, incoming_bpm: f32) -> Option<f32> {
    if !outgoing_bpm.is_finite()
        || !incoming_bpm.is_finite()
        || outgoing_bpm <= 0.0
        || incoming_bpm <= 0.0
    {
        return None;
    }
    let aligned_in = align_tempo_octave(incoming_bpm, outgoing_bpm);
    let rate = outgoing_bpm / aligned_in;
    rate.is_finite().then_some(rate)
}

/// Whether the incoming track can be cleanly time-stretched to match the outgoing track's BPM.
pub fn stretch_reachable(outgoing_bpm: f32, incoming_bpm: f32) -> bool {
    incoming_rate_between(outgoing_bpm, incoming_bpm)
        .is_some_and(|r| (MIN_INCOMING_RATE..=MAX_INCOMING_RATE).contains(&r))
}

/// Camelot wheel harmonic compatibility score (`0.0..=1.0`):
/// - `1.0` = identical key or relative major/minor (e.g. 8A <-> 8B)
/// - `0.85` = adjacent fifth on the Camelot wheel (±1 step, same mode)
/// - `0.55` = energy boost (+2 steps, same mode)
/// - `0.0` = unknown or clashing keys
pub fn harmonic_key_score(key_a: i32, key_b: i32) -> f32 {
    let Some((num_a, mode_a)) = key_to_camelot(key_a) else {
        return 0.0;
    };
    let Some((num_b, mode_b)) = key_to_camelot(key_b) else {
        return 0.0;
    };
    let diff = (num_a - num_b).rem_euclid(12);
    let circ = diff.min(12 - diff);
    if circ == 0 {
        return 1.0;
    }
    if mode_a == mode_b {
        if circ == 1 {
            return 0.85;
        }
        if diff == 2 {
            return 0.55;
        }
    }
    0.0
}

fn key_to_camelot(key: i32) -> Option<(i32, bool)> {
    // 0..11 = C..B major (mode = true -> B), 12..23 = C..B minor (mode = false -> A)
    const MAJOR_NUM: [i32; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
    const MINOR_NUM: [i32; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];
    if (0..12).contains(&key) {
        Some((MAJOR_NUM[key as usize], true))
    } else if (12..24).contains(&key) {
        Some((MINOR_NUM[(key - 12) as usize], false))
    } else {
        None
    }
}

/// Average vocal activity across `[start, end]`, returning `0.0` when absent.
pub fn vocal_activity_between(analysis: &TrackAnalysis, start: f64, end: f64) -> f32 {
    if !analysis.has_vocal_mask() {
        return 0.0;
    }
    sample_curve_window(&analysis.vocal_activity_mask, start, end).average
}

/// Estimates the fraction of a transition overlap (`0.0..=1.0`) where both tracks have
/// active vocals simultaneously (`energy >= VOCAL_ACTIVE_THRESHOLD`).
pub fn simultaneous_vocal_fraction(
    outgoing: &TrackAnalysis,
    out_start: f64,
    incoming: &TrackAnalysis,
    in_start: f64,
    duration_sec: f64,
) -> f32 {
    if duration_sec <= 0.25 || (!outgoing.has_vocal_mask() && !incoming.has_vocal_mask()) {
        return 0.0;
    }
    let steps = ((duration_sec / 0.25).ceil() as usize).clamp(1, 240);
    let dt = duration_sec / steps as f64;
    let mut both = 0usize;
    for i in 0..steps {
        let offset = (i as f64 + 0.5) * dt;
        let vo = vocal_activity_between(
            outgoing,
            out_start + offset - 0.125,
            out_start + offset + 0.125,
        );
        let vi = vocal_activity_between(
            incoming,
            in_start + offset - 0.125,
            in_start + offset + 0.125,
        );
        if vo >= VOCAL_ACTIVE_THRESHOLD && vi >= VOCAL_ACTIVE_THRESHOLD {
            both += 1;
        }
    }
    both as f32 / steps as f32
}

/// Total audible seconds (`energy >= 0.08`) inside `[start, end]`.
pub fn audible_seconds_between(analysis: &TrackAnalysis, start: f64, end: f64) -> f64 {
    let lo = start.min(end);
    let hi = start.max(end);
    if hi <= lo {
        return 0.0;
    }
    if analysis.energy_curve.is_empty() {
        let eff_lo = lo.max(analysis.audible_start_time);
        let eff_hi = hi.min(analysis.content_end_time);
        return (eff_hi - eff_lo).max(0.0);
    }
    let steps = ((hi - lo) / 0.25).ceil() as usize;
    let dt = (hi - lo) / steps.max(1) as f64;
    let mut audible = 0.0_f64;
    for i in 0..steps.max(1) {
        let t0 = lo + i as f64 * dt;
        let t1 = (t0 + dt).min(hi);
        let avg = sample_curve_window(&analysis.energy_curve, t0, t1).average;
        if avg >= 0.08 {
            audible += t1 - t0;
        }
    }
    audible
}

/// Ranks incoming cue candidates preferring low vocal clash, strong structure, and early entry.
pub fn rank_mix_in_candidates(
    analysis: &TrackAnalysis,
    blend_duration: f64,
) -> Vec<RankedMixCandidate> {
    let mut out: Vec<RankedMixCandidate> = analysis
        .mix_in_candidates
        .iter()
        .filter(|c| c.time >= 0.0 && c.time + blend_duration * 0.5 < analysis.content_end_time)
        .map(|c| {
            let vocal = vocal_activity_between(analysis, c.time, c.time + blend_duration);
            let penalty = if vocal >= VOCAL_ACTIVE_THRESHOLD {
                0.28 * vocal
            } else {
                0.0
            };
            RankedMixCandidate {
                time: c.time,
                score: (c.score - penalty).clamp(0.0, 1.0),
            }
        })
        .collect();
    if out.is_empty() {
        out.push(RankedMixCandidate {
            time: analysis.audible_start_time,
            score: 0.4,
        });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Ranks outgoing cue candidates preferring low-vocal structural outro anchors with enough tail.
pub fn rank_mix_out_candidates(
    analysis: &TrackAnalysis,
    blend_duration: f64,
) -> Vec<RankedMixCandidate> {
    let end_limit = analysis.content_end_time.min(analysis.duration);
    let mut out: Vec<RankedMixCandidate> = analysis
        .mix_out_candidates
        .iter()
        .filter(|c| c.time >= analysis.duration * 0.45 && c.time + 1.5 <= end_limit)
        .map(|c| {
            let vocal =
                vocal_activity_between(analysis, c.time, (c.time + blend_duration).min(end_limit));
            let tail = (end_limit - c.time).max(0.0);
            let tail_ok = if tail >= blend_duration * 0.8 {
                0.1
            } else {
                -0.15
            };
            RankedMixCandidate {
                time: c.time,
                score: (c.score - 0.30 * vocal + tail_ok).clamp(0.0, 1.0),
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Resolves the best outgoing anchor timestamp around `blend_duration` before content end.
pub fn resolve_mix_out_anchor(analysis: &TrackAnalysis, blend_duration: f64) -> MixOutAnchor {
    let ranked = rank_mix_out_candidates(analysis, blend_duration);
    if let Some(top) = ranked.first()
        && top.score >= 0.45
    {
        return MixOutAnchor {
            time: top.time,
            exact: true,
        };
    }
    let end = analysis.content_end_time.min(analysis.duration);
    let raw = (end - blend_duration).max(analysis.audible_start_time);
    if let Some(snapped) = analysis.nearest_downbeat(raw, Some(BeatSide::Before), 2.5) {
        return MixOutAnchor {
            time: snapped,
            exact: true,
        };
    }
    MixOutAnchor {
        time: raw,
        exact: false,
    }
}

/// Determines the highest safe transition tier supported by both tracks' analyses.
pub fn assess_transition_tier(
    outgoing: &TrackAnalysis,
    incoming: &TrackAnalysis,
) -> TransitionTier {
    if outgoing.has_reliable_beat()
        && incoming.has_reliable_beat()
        && outgoing.beat_confidence >= MIN_DJ_CONFIDENCE
        && incoming.beat_confidence >= MIN_DJ_CONFIDENCE
        && stretch_reachable(outgoing.bpm, incoming.bpm)
    {
        return TransitionTier::DjBeatmatch;
    }
    if outgoing.has_reliable_beat() || incoming.has_reliable_beat() {
        return TransitionTier::PhraseBlend;
    }
    TransitionTier::Crossfade
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octave_alignment_and_camelot_harmonic_scoring_match_dj_rules() {
        assert!((align_tempo_octave(64.0, 128.0) - 128.0).abs() < 1e-4);
        assert!((align_tempo_octave(250.0, 125.0) - 125.0).abs() < 1e-4);
        assert!(stretch_reachable(124.0, 128.0));
        assert!(!stretch_reachable(124.0, 175.0));

        // C major (0 = 8B) and A minor (21 = 8A) are relative keys -> score 1.0!
        assert!((harmonic_key_score(0, 21) - 1.0).abs() < 1e-6);
        // C major (0 = 8B) and G major (7 = 9B) are adjacent fifths -> score 0.85!
        assert!((harmonic_key_score(0, 7) - 0.85).abs() < 1e-6);
        // C major (0 = 8B) and F# major (6 = 2B) tritone clash -> 0.0!
        assert_eq!(harmonic_key_score(0, 6), 0.0);
    }
}
