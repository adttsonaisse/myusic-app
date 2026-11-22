//! Smart Automix transition planner ported from upstream `TransitionPlanner.kt`.
//!
//! Combines rhythm/tempo alignment, 8/16-bar phrase matching, Camelot harmonic scoring,
//! bass-swap / filter-sweep selection, and vocal-clash avoidance into a deterministic
//! [`TransitionPlan`] executed by `bitchord-audio`'s lock-free dual-deck mixer.

use serde::{Deserialize, Serialize};

use crate::analysis::{BeatSide, TrackAnalysis};
use crate::policy::{
    MAX_INCOMING_RATE, MAX_SIMULTANEOUS_VOCAL_FRACTION, MIN_INCOMING_RATE,
    MIN_PHRASE_BEAT_CONFIDENCE, MIN_PHRASE_BEATS, TransitionTier, assess_transition_tier,
    harmonic_key_score, incoming_rate_between, rank_mix_in_candidates, resolve_mix_out_anchor,
    simultaneous_vocal_fraction,
};

/// Upper clamp for any single Automix transition overlap (seconds).
pub const MAX_MIX_WINDOW_SEC: f64 = 30.0;
/// Lower clamp for any active Automix transition overlap (seconds).
pub const MIN_MIX_WINDOW_SEC: f64 = 1.5;

/// User-selected Automix mode (persisted in settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AutomixMode {
    #[default]
    Off,
    Standard,
    Smart,
    SmartAdvanced,
    LoopMix,
}

impl AutomixMode {
    pub const ALL: [Self; 5] = [
        Self::Off,
        Self::Standard,
        Self::Smart,
        Self::SmartAdvanced,
        Self::LoopMix,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Standard => "Standard Crossfade",
            Self::Smart => "Smart Automix",
            Self::SmartAdvanced => "Advanced Automix (DJ)",
            Self::LoopMix => "LoopMix (Tempo Ramp)",
        }
    }

    pub const fn is_smart(self) -> bool {
        matches!(self, Self::Smart | Self::SmartAdvanced | Self::LoopMix)
    }
}

/// Chosen DSP/DJ transition strategy executed by the dual-deck engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionStyle {
    #[default]
    None,
    StandardCrossfade,
    SmoothBlend,
    BeatmatchedBlend,
    DjFilterSweep,
    DjBassSwap,
    PhraseMix,
    LoopMix,
}

impl TransitionStyle {
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "Gapless",
            Self::StandardCrossfade => "Crossfade",
            Self::SmoothBlend => "Smooth Blend",
            Self::BeatmatchedBlend => "Beatmatched Blend",
            Self::DjFilterSweep => "DJ Filter Sweep",
            Self::DjBassSwap => "DJ Bass Swap",
            Self::PhraseMix => "Phrase Mix",
            Self::LoopMix => "LoopMix",
        }
    }
}

/// Filter automation contour during a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterTransitionVariant {
    #[default]
    None,
    LowPassExit,
    HighPassEntry,
    DualSweep,
}

/// Complete executable transition plan between an outgoing and incoming track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionPlan {
    pub style: TransitionStyle,
    pub filter_variant: FilterTransitionVariant,
    /// Outgoing track playback position (seconds) where the blend begins.
    pub transition_start_sec: f64,
    /// Total transition overlap duration (seconds).
    pub duration_sec: f64,
    /// Standby incoming deck cue point (seconds) aligned to downbeat / mix-in.
    pub incoming_cue_sec: f64,
    /// Incoming deck playback speed multiplier (`0.90..=1.25`; `1.0` if non-beatmatched).
    pub incoming_playback_rate: f32,
    /// Fractional progress (`0.0..=1.0`) where outgoing bass cuts and incoming bass enters.
    pub bass_swap_fraction: Option<f32>,
    pub vocal_clash_avoided: bool,
    pub harmonic_match: bool,
    pub beats_matched: bool,
    pub phrase_bars: u32,
    pub outgoing_bpm: f32,
    pub incoming_bpm: f32,
    pub summary: String,
}

impl TransitionPlan {
    pub fn none() -> Self {
        Self {
            style: TransitionStyle::None,
            filter_variant: FilterTransitionVariant::None,
            transition_start_sec: 0.0,
            duration_sec: 0.0,
            incoming_cue_sec: 0.0,
            incoming_playback_rate: 1.0,
            bass_swap_fraction: None,
            vocal_clash_avoided: false,
            harmonic_match: false,
            beats_matched: false,
            phrase_bars: 0,
            outgoing_bpm: 0.0,
            incoming_bpm: 0.0,
            summary: "Gapless".to_owned(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.style != TransitionStyle::None && self.duration_sec >= MIN_MIX_WINDOW_SEC
    }

    /// Evaluates per-deck transition filter cutoffs `(out_lp_hz, out_hp_hz, in_lp_hz, in_hp_hz)`
    /// at normalized transition progress `t ∈ [0, 1]`.
    /// Parked values: `LP >= 19_500 Hz` and `HP <= 25 Hz` bypass the biquad cleanly.
    pub fn filter_cutoffs_at(&self, progress: f32) -> (f32, f32, f32, f32) {
        let t = progress.clamp(0.0, 1.0);
        const PARK_LP: f32 = 20_000.0;
        const PARK_HP: f32 = 20.0;

        match self.style {
            TransitionStyle::DjBassSwap | TransitionStyle::PhraseMix => {
                let swap = self.bass_swap_fraction.unwrap_or(0.50).clamp(0.20, 0.80);
                // Smooth 3-stage bass swap: incoming enters HP-filtered at 260 Hz, swaps cleanly at `swap`,
                // then outgoing HP sweeps upward through 320 Hz while incoming opens to full bass.
                let ramp = 0.14_f32;
                let out_hp = if t < swap - ramp {
                    PARK_HP
                } else if t < swap + ramp {
                    let u = (t - (swap - ramp)) / (2.0 * ramp);
                    20.0 + u * 300.0
                } else {
                    320.0 + ((t - (swap + ramp)) / (1.0 - (swap + ramp)).max(0.01)) * 280.0
                };
                let in_hp = if t < swap - ramp {
                    280.0
                } else if t < swap + ramp {
                    let u = (t - (swap - ramp)) / (2.0 * ramp);
                    280.0 - u * 260.0
                } else {
                    PARK_HP
                };
                (PARK_LP, out_hp, PARK_LP, in_hp)
            }
            TransitionStyle::DjFilterSweep => match self.filter_variant {
                FilterTransitionVariant::LowPassExit => {
                    // Exponential low-pass sweep closing outgoing 18 kHz -> 360 Hz.
                    let out_lp = 18_000.0 * (360.0_f32 / 18_000.0).powf(t * t);
                    (out_lp.clamp(280.0, PARK_LP), PARK_HP, PARK_LP, PARK_HP)
                }
                FilterTransitionVariant::HighPassEntry => {
                    let in_hp = 1_400.0 * (20.0_f32 / 1_400.0).powf(t);
                    (PARK_LP, PARK_HP, PARK_LP, in_hp.clamp(PARK_HP, 1_600.0))
                }
                FilterTransitionVariant::DualSweep | FilterTransitionVariant::None => {
                    let out_lp = 18_000.0 * (420.0_f32 / 18_000.0).powf(t);
                    let in_hp = 900.0 * (20.0_f32 / 900.0).powf(t);
                    (
                        out_lp.clamp(300.0, PARK_LP),
                        PARK_HP,
                        PARK_LP,
                        in_hp.clamp(PARK_HP, 1_200.0),
                    )
                }
            },
            TransitionStyle::LoopMix => {
                // Progressive DJ high-pass exit on looping outgoing track (20 Hz -> 1400 Hz).
                let out_hp = 20.0 * (1_400.0_f32 / 20.0).powf(t);
                (PARK_LP, out_hp.clamp(PARK_HP, 1_500.0), PARK_LP, PARK_HP)
            }
            _ => (PARK_LP, PARK_HP, PARK_LP, PARK_HP),
        }
    }
}

/// Computes an optimal [`TransitionPlan`] for the given mode, phrase bar preference (`0 = auto`,
/// `8`, or `16`), fallback `crossfade_seconds`, and track analyses.
pub fn plan_transition(
    mode: AutomixMode,
    requested_phrase_bars: u32,
    crossfade_seconds: u32,
    outgoing: &TrackAnalysis,
    incoming: &TrackAnalysis,
) -> TransitionPlan {
    if mode == AutomixMode::Off || outgoing.duration < 6.0 || incoming.duration < 6.0 {
        return TransitionPlan::none();
    }

    if mode == AutomixMode::Standard {
        let dur = f64::from(crossfade_seconds.clamp(2, 12)).min(outgoing.duration * 0.35);
        let start = (outgoing.content_end_time.min(outgoing.duration) - dur).max(0.0);
        return TransitionPlan {
            style: TransitionStyle::StandardCrossfade,
            filter_variant: FilterTransitionVariant::None,
            transition_start_sec: start,
            duration_sec: dur,
            incoming_cue_sec: incoming.audible_start_time,
            incoming_playback_rate: 1.0,
            bass_swap_fraction: None,
            vocal_clash_avoided: false,
            harmonic_match: false,
            beats_matched: false,
            phrase_bars: 0,
            outgoing_bpm: outgoing.bpm,
            incoming_bpm: incoming.bpm,
            summary: format!("Crossfade · {:.1}s", dur),
        };
    }

    // LoopMix mode delegates to LoopMix structural planning when viable, falling back cleanly
    // to SmartAdvanced if either track lacks beat reliability.
    if mode == AutomixMode::LoopMix
        && let Some(lm) = crate::loop_mix::plan_loop_mix(outgoing, incoming)
    {
        let dur = (lm.fade_end_sec - lm.fade_start_sec)
            .max(lm.tempo_ramp_sec)
            .clamp(4.0, MAX_MIX_WINDOW_SEC);
        return TransitionPlan {
            style: TransitionStyle::LoopMix,
            filter_variant: FilterTransitionVariant::HighPassEntry,
            transition_start_sec: lm.loop_start_sec,
            duration_sec: dur,
            incoming_cue_sec: lm.incoming_cue_sec,
            incoming_playback_rate: 1.0,
            bass_swap_fraction: Some(0.55),
            vocal_clash_avoided: true,
            harmonic_match: harmonic_key_score(outgoing.key, incoming.key) >= 0.80,
            beats_matched: true,
            phrase_bars: 4,
            outgoing_bpm: outgoing.bpm,
            incoming_bpm: incoming.bpm,
            summary: format!(
                "LoopMix · {:.0}->{:.0} BPM · {:.1}s loop ramp",
                outgoing.bpm, incoming.bpm, dur
            ),
        };
    }

    let tier = assess_transition_tier(outgoing, incoming);
    let harmonic_score = if outgoing.has_reliable_key() && incoming.has_reliable_key() {
        harmonic_key_score(outgoing.key, incoming.key)
    } else {
        0.0
    };
    let harmonic_match = harmonic_score >= 0.80;

    // Choose target duration from phrase bars when DJ-capable, or crossfade fallback.
    let mut phrase_bars = 0u32;
    let mut target_dur = f64::from(crossfade_seconds.clamp(4, 12));
    let can_phrase = tier == TransitionTier::DjBeatmatch
        && outgoing.beat_confidence >= MIN_PHRASE_BEAT_CONFIDENCE
        && incoming.beat_confidence >= MIN_PHRASE_BEAT_CONFIDENCE
        && outgoing.beats.len() >= MIN_PHRASE_BEATS
        && incoming.beats.len() >= MIN_PHRASE_BEATS;

    if can_phrase {
        let bars = match requested_phrase_bars {
            16 => 16,
            8 => 8,
            _ => {
                if mode == AutomixMode::SmartAdvanced && harmonic_match {
                    16
                } else {
                    8
                }
            }
        };
        let candidate_dur = f64::from(bars) * 4.0 * outgoing.beat_interval;
        if candidate_dur <= MAX_MIX_WINDOW_SEC
            && candidate_dur < outgoing.duration * 0.35
            && candidate_dur < incoming.duration * 0.35
        {
            phrase_bars = bars;
            target_dur = candidate_dur;
        } else {
            let fallback_8 = 8.0 * 4.0 * outgoing.beat_interval;
            if fallback_8 <= MAX_MIX_WINDOW_SEC && fallback_8 < outgoing.duration * 0.35 {
                phrase_bars = 8;
                target_dur = fallback_8;
            }
        }
    }
    target_dur = target_dur.clamp(
        MIN_MIX_WINDOW_SEC,
        MAX_MIX_WINDOW_SEC
            .min(outgoing.duration * 0.35)
            .max(MIN_MIX_WINDOW_SEC),
    );

    // Resolve outgoing anchor & incoming cue, rerouting around vocal clash when detected.
    let out_anchor = resolve_mix_out_anchor(outgoing, target_dur);
    let in_candidates = rank_mix_in_candidates(incoming, target_dur);
    let mut best_in_cue = in_candidates
        .first()
        .map(|c| c.time)
        .unwrap_or(incoming.audible_start_time);
    let mut vocal_clash_avoided = false;

    let initial_clash =
        simultaneous_vocal_fraction(outgoing, out_anchor.time, incoming, best_in_cue, target_dur);
    if initial_clash > MAX_SIMULTANEOUS_VOCAL_FRACTION {
        for cand in in_candidates.iter().skip(1) {
            let clash = simultaneous_vocal_fraction(
                outgoing,
                out_anchor.time,
                incoming,
                cand.time,
                target_dur,
            );
            if clash < initial_clash - 0.10 {
                best_in_cue = cand.time;
                vocal_clash_avoided = true;
                break;
            }
        }
        // If vocal clash still persists, tighten transition overlap cleanly.
        let remaining_clash = simultaneous_vocal_fraction(
            outgoing,
            out_anchor.time,
            incoming,
            best_in_cue,
            target_dur,
        );
        if remaining_clash > MAX_SIMULTANEOUS_VOCAL_FRACTION {
            target_dur = (target_dur * 0.55).clamp(3.0, 8.0);
            vocal_clash_avoided = true;
        }
    }

    // Snap incoming cue to nearest downbeat when beatmatching.
    if tier == TransitionTier::DjBeatmatch
        && let Some(snapped) = incoming.nearest_downbeat(best_in_cue, Some(BeatSide::After), 2.0)
    {
        best_in_cue = snapped;
    }

    // Clamp transition start so outgoing track never runs past content end.
    let end_limit = outgoing.content_end_time.min(outgoing.duration);
    let transition_start_sec = out_anchor
        .time
        .min((end_limit - target_dur).max(0.0))
        .max(outgoing.audible_start_time);
    let actual_dur = (end_limit - transition_start_sec)
        .min(target_dur)
        .clamp(MIN_MIX_WINDOW_SEC, MAX_MIX_WINDOW_SEC);

    let (beats_matched, incoming_playback_rate) = if tier == TransitionTier::DjBeatmatch {
        let rate = incoming_rate_between(outgoing.bpm, incoming.bpm)
            .unwrap_or(1.0)
            .clamp(MIN_INCOMING_RATE, MAX_INCOMING_RATE);
        (true, rate)
    } else {
        (false, 1.0)
    };

    // Determine DSP style and filter variant based on mode, beatmatch tier, and low-end energy.
    let out_low = outgoing
        .average_low_energy_between(transition_start_sec, transition_start_sec + actual_dur)
        .unwrap_or(0.4);
    let in_low = incoming
        .average_low_energy_between(best_in_cue, best_in_cue + actual_dur)
        .unwrap_or(0.4);

    let (style, filter_variant, bass_swap_fraction) = match (mode, tier) {
        (AutomixMode::SmartAdvanced, TransitionTier::DjBeatmatch) => {
            if phrase_bars >= 8 && harmonic_match {
                (
                    TransitionStyle::PhraseMix,
                    FilterTransitionVariant::None,
                    Some(0.50),
                )
            } else if out_low >= 0.35 && in_low >= 0.35 {
                (
                    TransitionStyle::DjBassSwap,
                    FilterTransitionVariant::None,
                    Some(0.52),
                )
            } else {
                (
                    TransitionStyle::DjFilterSweep,
                    FilterTransitionVariant::DualSweep,
                    None,
                )
            }
        }
        (AutomixMode::Smart, TransitionTier::DjBeatmatch) => {
            if out_low >= 0.45 && in_low >= 0.45 {
                (
                    TransitionStyle::DjBassSwap,
                    FilterTransitionVariant::None,
                    Some(0.50),
                )
            } else {
                (
                    TransitionStyle::BeatmatchedBlend,
                    FilterTransitionVariant::None,
                    None,
                )
            }
        }
        (_, TransitionTier::PhraseBlend) => (
            TransitionStyle::DjFilterSweep,
            FilterTransitionVariant::LowPassExit,
            None,
        ),
        (_, TransitionTier::Crossfade) => (
            TransitionStyle::SmoothBlend,
            FilterTransitionVariant::None,
            None,
        ),
        _ => (
            TransitionStyle::SmoothBlend,
            FilterTransitionVariant::None,
            None,
        ),
    };

    let mut tags = Vec::new();
    tags.push(style.label().to_owned());
    if beats_matched {
        tags.push(format!(
            "{:.0}->{:.0} BPM ({:.2}x)",
            outgoing.bpm, incoming.bpm, incoming_playback_rate
        ));
    }
    if phrase_bars > 0 {
        tags.push(format!("{phrase_bars} bars"));
    } else {
        tags.push(format!("{:.1}s", actual_dur));
    }
    if harmonic_match {
        tags.push("Harmonic".to_owned());
    }
    if vocal_clash_avoided {
        tags.push("Vocal-Safe".to_owned());
    }

    TransitionPlan {
        style,
        filter_variant,
        transition_start_sec,
        duration_sec: actual_dur,
        incoming_cue_sec: best_in_cue,
        incoming_playback_rate,
        bass_swap_fraction,
        vocal_clash_avoided,
        harmonic_match,
        beats_matched,
        phrase_bars,
        outgoing_bpm: outgoing.bpm,
        incoming_bpm: incoming.bpm,
        summary: tags.join(" · "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp_analyzer::{analyze_stereo_pcm, synth_beat_track_stereo};

    #[test]
    fn plans_beatmatched_phrase_or_bass_swap_between_compatible_tracks() {
        let a_pcm = synth_beat_track_stereo(124.0, 40.0, 44_100);
        let b_pcm = synth_beat_track_stereo(126.0, 40.0, 44_100);
        let a = analyze_stereo_pcm(&a_pcm, 44_100);
        let b = analyze_stereo_pcm(&b_pcm, 44_100);

        let plan = plan_transition(AutomixMode::SmartAdvanced, 8, 8, &a, &b);
        assert!(plan.is_active());
        assert!(plan.beats_matched, "expected beatmatch, got {:?}", plan);
        assert!(
            (0.90..=1.25).contains(&plan.incoming_playback_rate),
            "rate out of bounds: {}",
            plan.incoming_playback_rate
        );
        assert!(
            matches!(
                plan.style,
                TransitionStyle::PhraseMix
                    | TransitionStyle::DjBassSwap
                    | TransitionStyle::DjFilterSweep
            ),
            "unexpected style: {:?}",
            plan.style
        );
        // Verify filter cutoffs evolve cleanly across [0, 1].
        let (_, hp_start, _, in_hp_start) = plan.filter_cutoffs_at(0.05);
        let (_, hp_end, _, in_hp_end) = plan.filter_cutoffs_at(0.95);
        assert!(hp_end >= hp_start);
        assert!(in_hp_start >= in_hp_end);
    }
}
