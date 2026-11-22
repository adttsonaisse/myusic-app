//! LoopMix planner, analytical tempo-ramp beat clock, and pure-Rust DJ loop-out renderer
//! ported from upstream `LoopMix.kt` and `native/loopmix/loop_mix_renderer.cpp`.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use crate::analysis::{BeatSide, TrackAnalysis};
use crate::policy::{VOCAL_ACTIVE_THRESHOLD, align_tempo_octave, rank_mix_in_candidates};

/// A contiguous sung vocal interval `[start, end]` in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SungSpan {
    pub start: f64,
    pub end: f64,
}

/// Complete LoopMix transition specification (beat-locked loop on outgoing + smooth tempo ramp
/// to incoming track's native BPM + DJ high-pass exit sweep).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopMixPlan {
    pub loop_start_sec: f64,
    pub loop_end_sec: f64,
    pub loop_length_sec: f64,
    pub loop_bars: u32,
    pub outgoing_bpm: f32,
    pub incoming_bpm: f32,
    pub start_rate: f32,
    pub end_rate: f32,
    pub tempo_ramp_sec: f64,
    pub incoming_cue_sec: f64,
    pub fade_start_sec: f64,
    pub fade_end_sec: f64,
}

/// Analytical clock for a looping deck whose playback rate ramps linearly from `start_rate`
/// to `end_rate` over `ramp_duration_sec`, holding `end_rate` afterward (`LoopMix.Clock`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopMixClock {
    pub loop_start_sec: f64,
    pub loop_length_sec: f64,
    pub beat_interval_sec: f64,
    pub start_rate: f64,
    pub end_rate: f64,
    pub ramp_duration_sec: f64,
}

impl LoopMixClock {
    /// Instantaneous playback rate at `elapsed_sec` since loop engagement.
    pub fn rate_at(&self, elapsed_sec: f64) -> f64 {
        let t = elapsed_sec.max(0.0);
        if self.ramp_duration_sec <= 1e-6 {
            return self.end_rate;
        }
        let u = (t / self.ramp_duration_sec).clamp(0.0, 1.0);
        self.start_rate + (self.end_rate - self.start_rate) * u
    }

    /// Total source audio seconds traversed since loop engagement (exact integral of `rate_at`).
    pub fn traversed_sec(&self, elapsed_sec: f64) -> f64 {
        let t = elapsed_sec.max(0.0);
        if self.ramp_duration_sec <= 1e-6 {
            return t * self.end_rate;
        }
        if t <= self.ramp_duration_sec {
            let slope = (self.end_rate - self.start_rate) / self.ramp_duration_sec;
            self.start_rate * t + 0.5 * slope * t * t
        } else {
            let ramp_dist = 0.5 * (self.start_rate + self.end_rate) * self.ramp_duration_sec;
            ramp_dist + (t - self.ramp_duration_sec) * self.end_rate
        }
    }

    /// Source track playback position (seconds) wrapped cleanly inside `[loop_start, loop_end)`.
    pub fn position_at(&self, elapsed_sec: f64) -> f64 {
        if self.loop_length_sec <= 1e-6 {
            return self.loop_start_sec;
        }
        let d = self
            .traversed_sec(elapsed_sec)
            .rem_euclid(self.loop_length_sec);
        self.loop_start_sec + d
    }

    /// Beat phase `[0.0, 1.0)` at `elapsed_sec`.
    pub fn beat_phase_at(&self, elapsed_sec: f64) -> f64 {
        if self.beat_interval_sec <= 1e-6 {
            return 0.0;
        }
        (self.traversed_sec(elapsed_sec) / self.beat_interval_sec).rem_euclid(1.0)
    }
}

/// Extracts contiguous sung vocal spans (`energy >= VOCAL_ACTIVE_THRESHOLD`) from `analysis`.
pub fn sung_spans(analysis: &TrackAnalysis) -> Vec<SungSpan> {
    let mut spans = Vec::new();
    let mut open: Option<f64> = None;
    let mut last_t = 0.0_f64;
    for s in &analysis.vocal_activity_mask {
        if s.energy >= VOCAL_ACTIVE_THRESHOLD {
            if open.is_none() {
                open = Some(s.time);
            }
            last_t = s.time;
        } else if let Some(start) = open.take() {
            spans.push(SungSpan {
                start,
                end: (last_t + 0.25).min(analysis.duration),
            });
        }
    }
    if let Some(start) = open {
        spans.push(SungSpan {
            start,
            end: (last_t + 0.25).min(analysis.duration),
        });
    }
    spans
}

/// Plans a LoopMix beat-locked looping transition if both tracks have reliable beat grids
/// and an instrumental (low-vocal) bar-aligned loop window exists near the outgoing track's outro.
pub fn plan_loop_mix(outgoing: &TrackAnalysis, incoming: &TrackAnalysis) -> Option<LoopMixPlan> {
    if !outgoing.has_reliable_beat() || !incoming.has_reliable_beat() {
        return None;
    }
    if outgoing.downbeats.len() < 4 || outgoing.duration < 16.0 || incoming.duration < 12.0 {
        return None;
    }

    // Target tempo ratio after metrical octave folding; allow wider range than static beatmatch
    // because LoopMix ramps smoothly across bars (0.72x .. 1.40x).
    let aligned_in_bpm = align_tempo_octave(incoming.bpm, outgoing.bpm);
    let end_rate = (aligned_in_bpm / outgoing.bpm).clamp(0.72, 1.40);
    if !end_rate.is_finite() {
        return None;
    }

    let end_limit = outgoing.content_end_time.min(outgoing.duration);
    let search_start = (end_limit - (outgoing.duration * 0.35).clamp(16.0, 48.0))
        .max(outgoing.audible_start_time + 4.0);
    let bar_sec = 4.0 * outgoing.beat_interval;
    let spans = sung_spans(outgoing);

    // Search candidate 4-bar or 2-bar downbeat-aligned instrumental loops in the outro window.
    let mut best_loop: Option<(f64, f64, u32, f32)> = None;
    for bars in [4u32, 2u32] {
        let len_sec = f64::from(bars) * bar_sec;
        for &start in &outgoing.downbeats {
            if start < search_start {
                continue;
            }
            let end = start + len_sec;
            if end > end_limit - 0.5 {
                break;
            }
            let vocal_overlap = spans
                .iter()
                .map(|sp| (sp.end.min(end) - sp.start.max(start)).max(0.0))
                .sum::<f64>()
                / len_sec.max(1e-6);
            let energy = outgoing.average_energy_between(start, end).unwrap_or(0.5);
            if energy < 0.14 {
                continue;
            }
            let score = (1.0 - vocal_overlap as f32) * 0.65
                + energy.clamp(0.2, 0.85) * 0.25
                + if bars == 4 { 0.10 } else { 0.0 };
            if best_loop.is_none_or(|(_, _, _, s)| score > s) {
                best_loop = Some((start, end, bars, score));
            }
        }
        if best_loop.is_some_and(|(_, _, _, s)| s >= 0.72) {
            break;
        }
    }

    let (loop_start_sec, loop_end_sec, loop_bars, _) = best_loop?;
    let loop_length_sec = (loop_end_sec - loop_start_sec).max(0.5);
    let tempo_ramp_sec = (loop_length_sec * 2.0).clamp(4.0, 16.0);

    let in_ranked = rank_mix_in_candidates(incoming, tempo_ramp_sec);
    let mut incoming_cue_sec = in_ranked
        .first()
        .map(|c| c.time)
        .unwrap_or(incoming.audible_start_time);
    if let Some(snapped) = incoming.nearest_downbeat(incoming_cue_sec, Some(BeatSide::After), 2.0) {
        incoming_cue_sec = snapped;
    }

    let fade_start_sec = tempo_ramp_sec * 0.25;
    let fade_end_sec = (tempo_ramp_sec * 1.15).min(24.0);

    Some(LoopMixPlan {
        loop_start_sec,
        loop_end_sec,
        loop_length_sec,
        loop_bars,
        outgoing_bpm: outgoing.bpm,
        incoming_bpm: aligned_in_bpm,
        start_rate: 1.0,
        end_rate,
        tempo_ramp_sec,
        incoming_cue_sec,
        fade_start_sec,
        fade_end_sec,
    })
}

/// Stereo biquad filter used by [`LoopMixRenderer`] for DJ low-shelf cut (`250 Hz`)
/// and progressive high-pass sweep (`20 Hz -> 1500 Hz`), matching `loop_mix_renderer.cpp`.
#[derive(Debug, Clone)]
pub struct LoopMixBiquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: [f64; 2],
    z2: [f64; 2],
}

impl Default for LoopMixBiquad {
    fn default() -> Self {
        Self::bypass()
    }
}

impl LoopMixBiquad {
    pub fn bypass() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: [0.0; 2],
            z2: [0.0; 2],
        }
    }

    pub fn set_highpass(&mut self, cutoff_hz: f64, sample_rate_hz: f64, q: f64) {
        let fc = cutoff_hz.clamp(20.0, sample_rate_hz * 0.45);
        let w0 = 2.0 * PI * fc / sample_rate_hz;
        let cos_w = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.25));
        let a0 = 1.0 + alpha;
        self.b0 = ((1.0 + cos_w) * 0.5) / a0;
        self.b1 = -(1.0 + cos_w) / a0;
        self.b2 = ((1.0 + cos_w) * 0.5) / a0;
        self.a1 = (-2.0 * cos_w) / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    pub fn set_low_shelf(&mut self, freq_hz: f64, gain_db: f64, sample_rate_hz: f64) {
        let a = 10.0_f64.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq_hz.clamp(40.0, sample_rate_hz * 0.4) / sample_rate_hz;
        let cos_w = w0.cos();
        let alpha = w0.sin() / 2.0 * (2.0_f64).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha;
        self.b0 = (a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha)) / a0;
        self.b1 = (2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w)) / a0;
        self.b2 = (a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha)) / a0;
        self.a1 = (-2.0 * ((a - 1.0) + (a + 1.0) * cos_w)) / a0;
        self.a2 = ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha) / a0;
    }

    #[inline]
    pub fn process_frame(&mut self, left: f32, right: f32) -> (f32, f32) {
        let l = self.process_ch(0, f64::from(left)) as f32;
        let r = self.process_ch(1, f64::from(right)) as f32;
        (l, r)
    }

    #[inline]
    fn process_ch(&mut self, ch: usize, x: f64) -> f64 {
        let y = self.b0 * x + self.z1[ch];
        self.z1[ch] = self.b1 * x - self.a1 * y + self.z2[ch];
        self.z2[ch] = self.b2 * x - self.a2 * y;
        y
    }
}

/// Offline/pre-render pure-Rust LoopMix blender producing a seamless stereo PCM bridge
/// between a looped, tempo-ramped outgoing segment and the incoming track's opening phrase.
pub fn render_loop_mix_bridge(
    outgoing_loop_stereo: &[f32],
    incoming_intro_stereo: &[f32],
    plan: &LoopMixPlan,
    sample_rate_hz: u32,
) -> Vec<f32> {
    let loop_frames = outgoing_loop_stereo.len() / 2;
    let in_frames = incoming_intro_stereo.len() / 2;
    if loop_frames < 64 || in_frames < 64 {
        return Vec::new();
    }

    let sr = f64::from(sample_rate_hz.max(8_000));
    let total_frames = ((plan.fade_end_sec * sr).round() as usize).min(in_frames);
    let mut out = Vec::with_capacity(total_frames * 2);

    let clock = LoopMixClock {
        loop_start_sec: 0.0,
        loop_length_sec: loop_frames as f64 / sr,
        beat_interval_sec: 60.0 / f64::from(plan.outgoing_bpm.max(40.0)),
        start_rate: f64::from(plan.start_rate),
        end_rate: f64::from(plan.end_rate),
        ramp_duration_sec: plan.tempo_ramp_sec,
    };

    let mut hp = LoopMixBiquad::bypass();
    let mut shelf = LoopMixBiquad::bypass();
    let xfade_samples = (sr * 0.012).round() as usize; // 12 ms wrap crossfade for click-free loop seam

    for i in 0..total_frames {
        let t = i as f64 / sr;
        let progress = (t / plan.fade_end_sec.max(1e-3)).clamp(0.0, 1.0);

        // Update filter coefficients every 64 frames (~1.3 ms) for smooth, click-free sweeps.
        if i % 64 == 0 {
            let hp_hz = 20.0 * (1_450.0_f64 / 20.0).powf(progress * progress);
            hp.set_highpass(hp_hz, sr, 0.707);
            let shelf_db = -12.0 * progress;
            shelf.set_low_shelf(250.0, shelf_db, sr);
        }

        // Linear-interpolated read from looped outgoing buffer with wrap seam crossfade.
        let src_pos = (clock.position_at(t) * sr).rem_euclid(loop_frames as f64);
        let idx0 = src_pos as usize % loop_frames;
        let idx1 = (idx0 + 1) % loop_frames;
        let frac = (src_pos - idx0 as f64) as f32;
        let mut ol =
            outgoing_loop_stereo[idx0 * 2] * (1.0 - frac) + outgoing_loop_stereo[idx1 * 2] * frac;
        let mut or = outgoing_loop_stereo[idx0 * 2 + 1] * (1.0 - frac)
            + outgoing_loop_stereo[idx1 * 2 + 1] * frac;

        if idx0 + xfade_samples >= loop_frames && xfade_samples > 0 {
            let wrap_idx = idx0 + xfade_samples - loop_frames;
            let w = (wrap_idx as f32 / xfade_samples as f32).clamp(0.0, 1.0);
            let wl = outgoing_loop_stereo[(wrap_idx % loop_frames) * 2];
            let wr = outgoing_loop_stereo[(wrap_idx % loop_frames) * 2 + 1];
            ol = ol * (1.0 - w) + wl * w;
            or = or * (1.0 - w) + wr * w;
        }

        let (ol, or) = shelf.process_frame(ol, or);
        let (ol, or) = hp.process_frame(ol, or);

        // Equal-power fade between fade_start_sec and fade_end_sec.
        let fade_u = if t <= plan.fade_start_sec {
            0.0
        } else {
            ((t - plan.fade_start_sec) / (plan.fade_end_sec - plan.fade_start_sec).max(1e-3))
                .clamp(0.0, 1.0)
        };
        let angle = fade_u * std::f64::consts::FRAC_PI_2;
        let out_gain = angle.cos() as f32;
        let in_gain = angle.sin() as f32;
        let trim = 1.0 / (out_gain + in_gain).max(1.0).sqrt();

        let il = incoming_intro_stereo[i * 2];
        let ir = incoming_intro_stereo[i * 2 + 1];
        out.push(((ol * out_gain + il * in_gain) * trim).clamp(-1.0, 1.0));
        out.push(((or * out_gain + ir * in_gain) * trim).clamp(-1.0, 1.0));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp_analyzer::{analyze_stereo_pcm, synth_beat_track_stereo};

    #[test]
    fn loop_mix_clock_integrates_linear_tempo_ramp_and_renders_bridge() {
        let clock = LoopMixClock {
            loop_start_sec: 10.0,
            loop_length_sec: 4.0,
            beat_interval_sec: 0.5,
            start_rate: 1.0,
            end_rate: 1.2,
            ramp_duration_sec: 4.0,
        };
        // Integral of linear ramp from 1.0 -> 1.2 over 4s = avg 1.1 * 4s = 4.4s -> wraps to 10.4s.
        assert!((clock.traversed_sec(4.0) - 4.4).abs() < 1e-6);
        assert!((clock.position_at(4.0) - 10.4).abs() < 1e-6);
        // After ramp (+2s at 1.2x = +2.4s -> total 6.8s -> wraps to 12.8s).
        assert!((clock.position_at(6.0) - 12.8).abs() < 1e-6);

        let a_pcm = synth_beat_track_stereo(120.0, 32.0, 44_100);
        let b_pcm = synth_beat_track_stereo(128.0, 32.0, 44_100);
        let a = analyze_stereo_pcm(&a_pcm, 44_100);
        let b = analyze_stereo_pcm(&b_pcm, 44_100);

        let plan = plan_loop_mix(&a, &b).expect("viable loop mix plan");
        assert!(plan.end_rate > 1.03 && plan.end_rate < 1.12);

        let loop_slice = &a_pcm[0..44_100 * 4 * 2];
        let intro_slice = &b_pcm[0..44_100 * 12 * 2];
        let bridge = render_loop_mix_bridge(loop_slice, intro_slice, &plan, 44_100);
        assert!(!bridge.is_empty());
        assert!(bridge.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    }
}
