//! Real-time audio output backend (`PipeWire` native F32LE stream + deterministic
//! `NullAudioSink`) backed by lock-free `rtrb` SPSC ring buffers.
//!
//! Strictly adheres to real-time audio safety (`linux-audio-engine.md` §3):
//! - **Zero** heap allocations, deallocations, mutex locks, or syscalls inside
//!   the PipeWire `process` callback.
//! - **Zero** `unsafe` code (`#![forbid(unsafe_code)]`).
//! - Per-deck `DspChain` (`Spatial -> Equalizer -> TransitionFilter -> Loudness`)
//!   is pre-allocated off-thread before entering the audio thread.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread::{self, JoinHandle};

use libspa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use libspa::pod::Pod;
use pipewire as pw;
use pw::properties::properties;
use rtrb::{Consumer, Producer, RingBuffer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::alsa_output::AlsaExclusiveSink;
use crate::crossfade::DeckId;
use crate::dsp::{DspChain, EqTuningSnapshot};

/// Canonical PipeWire output sample rate (48 kHz).
pub const OUTPUT_SAMPLE_RATE: u32 = 48_000;
/// Canonical stereo channel count.
pub const OUTPUT_CHANNELS: usize = 2;
/// Per-deck ring buffer duration (8 seconds of stereo f32 PCM for deep network jitter immunity).
pub const RING_DURATION_SECS: usize = 8;
/// Per-deck `rtrb` ring capacity in interleaved `f32` samples (~3.07 MiB per deck).
pub const RING_CAPACITY_SAMPLES: usize =
    OUTPUT_SAMPLE_RATE as usize * OUTPUT_CHANNELS * RING_DURATION_SECS;
/// Tiny click-free fade ramp length (16 frames = 0.33 ms at 48 kHz) applied on rare underruns.
const UNDERRUN_RAMP_FRAMES: usize = 16;
/// Capacity of the lock-free control command ring from engine thread to RT thread.
pub const CMD_RING_CAPACITY: usize = 256;
/// Fixed per-chunk frame limit inside the RT callback so scratch buffers never resize.
const MAX_RT_CHUNK_FRAMES: usize = 4_096;
/// Default low-watermark re-buffering threshold (350 ms) after a mid-track underrun
/// so playback pauses briefly to accumulate audio instead of stuttering every 20 ms.
pub const DEFAULT_REBUFFER_MS: usize = 350;

/// Observable health of the live audio output backend (PipeWire or ALSA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputStreamHealth {
    #[default]
    Connecting,
    Streaming,
    Paused,
    Disconnected,
    Error,
}

impl OutputStreamHealth {
    const fn to_u8(self) -> u8 {
        match self {
            Self::Connecting => 0,
            Self::Streaming => 1,
            Self::Paused => 2,
            Self::Disconnected => 3,
            Self::Error => 4,
        }
    }

    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Streaming,
            2 => Self::Paused,
            3 => Self::Disconnected,
            4 => Self::Error,
            _ => Self::Connecting,
        }
    }
}

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("PipeWire initialization/connection failed: {0}")]
    PipeWire(String),
    #[error("ALSA exclusive output failed: {0}")]
    Alsa(String),
    #[error("failed to spawn audio output thread: {0}")]
    ThreadSpawn(#[from] std::io::Error),
}

/// Selected Linux audio output mode (`Wave 5B`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AudioOutputMode {
    /// Standard PipeWire shared stream at 48 kHz F32LE (always mixes smoothly with desktop audio).
    #[default]
    PipeWireAuto,
    /// PipeWire Pro Audio / dynamic clock switching requesting the track's exact native sample rate.
    PipeWireForceRate,
    /// Direct hardware ALSA `hw:card,dev` exclusive output bypassing OS mixing and resampling.
    AlsaExclusive { device: String },
}

impl AudioOutputMode {
    /// Whether this mode switches output hardware clock rate to match each track's native rate.
    pub fn tracks_native_rate(&self) -> bool {
        matches!(self, Self::PipeWireForceRate | Self::AlsaExclusive { .. })
    }

    pub fn label(&self) -> String {
        match self {
            Self::PipeWireAuto => "PipeWire Auto (48 kHz)".to_owned(),
            Self::PipeWireForceRate => "PipeWire Force-Rate (Native Clock)".to_owned(),
            Self::AlsaExclusive { device } => format!("ALSA Exclusive ({device})"),
        }
    }
}

/// Lock-free, heap-free (`Copy`) command sent from the engine/UI thread to the RT mixer.
#[derive(Debug, Clone, Copy)]
pub enum RtCommand {
    SetEq(EqTuningSnapshot),
    SetSpatial(bool),
    SetLoudnessGain {
        deck: DeckId,
        gain: f32,
    },
    SetNextLoudnessGain {
        deck: DeckId,
        gain: Option<f32>,
    },
    SetBlendTrim(f32),
    SetTransitionCutoffs {
        deck: DeckId,
        lowpass_hz: f32,
        highpass_hz: f32,
    },
    OpenTransition(DeckId),
    SetMixGains {
        gain_a: f32,
        gain_b: f32,
    },
    SetVolume(f32),
    SetMuted(bool),
    SetPaused(bool),
    StreamBoundary(DeckId),
    FlushDeck {
        deck: DeckId,
        epoch: u64,
    },
    ResetPlayedFrames(DeckId),
}

/// Lock-free telemetry published by the RT callback and read from any thread.
#[derive(Debug, Default)]
pub struct RtMetrics {
    played_frames_a: AtomicU64,
    played_frames_b: AtomicU64,
    flush_ack_a: AtomicU64,
    flush_ack_b: AtomicU64,
    min_flush_epoch_a: AtomicU64,
    min_flush_epoch_b: AtomicU64,
    stale_drain_a: AtomicUsize,
    stale_drain_b: AtomicUsize,
    deck_eof_a: AtomicBool,
    deck_eof_b: AtomicBool,
    underrun_count: AtomicU64,
    peak_level_bits: AtomicU32,
    paused: AtomicBool,
    rebuffering: AtomicBool,
    stream_health: AtomicU8,
    needs_reconnect: AtomicBool,
    stream_error: Mutex<Option<String>>,
}

impl RtMetrics {
    pub fn played_frames(&self, deck: DeckId) -> u64 {
        match deck {
            DeckId::A => self.played_frames_a.load(Ordering::Acquire),
            DeckId::B => self.played_frames_b.load(Ordering::Acquire),
        }
    }

    pub fn flush_ack(&self, deck: DeckId) -> u64 {
        match deck {
            DeckId::A => self.flush_ack_a.load(Ordering::Acquire),
            DeckId::B => self.flush_ack_b.load(Ordering::Acquire),
        }
    }

    pub fn set_deck_eof(&self, deck: DeckId, eof: bool) {
        match deck {
            DeckId::A => self.deck_eof_a.store(eof, Ordering::Release),
            DeckId::B => self.deck_eof_b.store(eof, Ordering::Release),
        }
    }

    pub fn deck_eof(&self, deck: DeckId) -> bool {
        match deck {
            DeckId::A => self.deck_eof_a.load(Ordering::Acquire),
            DeckId::B => self.deck_eof_b.load(Ordering::Acquire),
        }
    }

    /// Marks `min_epoch` as the earliest flush epoch still allowed to drain the
    /// entire ring for `deck`. Any older flush that arrives late after a timeout
    /// will only trim `stale_samples` old samples from the front, preserving newly
    /// pushed audio from the incoming track.
    pub(crate) fn arm_stale_flush_guard(&self, deck: DeckId, min_epoch: u64, stale_samples: usize) {
        match deck {
            DeckId::A => {
                self.stale_drain_a.store(stale_samples, Ordering::Release);
                self.min_flush_epoch_a.store(min_epoch, Ordering::Release);
            }
            DeckId::B => {
                self.stale_drain_b.store(stale_samples, Ordering::Release);
                self.min_flush_epoch_b.store(min_epoch, Ordering::Release);
            }
        }
    }

    fn min_flush_epoch(&self, deck: DeckId) -> u64 {
        match deck {
            DeckId::A => self.min_flush_epoch_a.load(Ordering::Acquire),
            DeckId::B => self.min_flush_epoch_b.load(Ordering::Acquire),
        }
    }

    fn take_stale_drain(&self, deck: DeckId) -> usize {
        match deck {
            DeckId::A => self.stale_drain_a.swap(0, Ordering::AcqRel),
            DeckId::B => self.stale_drain_b.swap(0, Ordering::AcqRel),
        }
    }

    pub fn underruns(&self) -> u64 {
        self.underrun_count.load(Ordering::Relaxed)
    }

    pub fn peak_level(&self) -> f32 {
        f32::from_bits(self.peak_level_bits.load(Ordering::Relaxed))
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn is_rebuffering(&self) -> bool {
        self.rebuffering.load(Ordering::Acquire)
    }

    pub fn stream_health(&self) -> OutputStreamHealth {
        OutputStreamHealth::from_u8(self.stream_health.load(Ordering::Acquire))
    }

    pub fn needs_reconnect(&self) -> bool {
        self.needs_reconnect.load(Ordering::Acquire)
    }

    pub fn stream_error(&self) -> Option<String> {
        self.stream_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_stream_health(&self, health: OutputStreamHealth) {
        self.stream_health.store(health.to_u8(), Ordering::Release);
    }

    pub(crate) fn mark_disconnected(&self, reason: Option<String>, is_error: bool) {
        let health = if is_error {
            OutputStreamHealth::Error
        } else {
            OutputStreamHealth::Disconnected
        };
        self.stream_health.store(health.to_u8(), Ordering::Release);
        self.needs_reconnect.store(true, Ordering::Release);
        if let Some(msg) = reason {
            *self.stream_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
        }
    }

    pub(crate) fn clear_reconnect_flag(&self) {
        self.needs_reconnect.store(false, Ordering::Release);
        *self.stream_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn add_played_frames(&self, deck: DeckId, frames: u64) {
        match deck {
            DeckId::A => {
                self.played_frames_a.fetch_add(frames, Ordering::Release);
            }
            DeckId::B => {
                self.played_frames_b.fetch_add(frames, Ordering::Release);
            }
        }
    }

    fn reset_played_frames(&self, deck: DeckId) {
        match deck {
            DeckId::A => self.played_frames_a.store(0, Ordering::Release),
            DeckId::B => self.played_frames_b.store(0, Ordering::Release),
        }
    }

    fn set_flush_ack(&self, deck: DeckId, epoch: u64) {
        match deck {
            DeckId::A => {
                self.flush_ack_a.store(epoch, Ordering::Release);
            }
            DeckId::B => {
                self.flush_ack_b.store(epoch, Ordering::Release);
            }
        }
    }
}

/// Producer endpoints handed to the `AudioEngine` and its two decode worker threads.
pub struct SinkProducers {
    pub deck_a: Producer<f32>,
    pub deck_b: Producer<f32>,
    pub commands: Producer<RtCommand>,
    pub metrics: Arc<RtMetrics>,
    pub sample_rate: u32,
}

/// Real-time dual-deck mixer + DSP consumer running inside the PipeWire callback
/// (or driven synchronously by [`NullAudioSink`]).
pub struct RtMixerState {
    rx_a: Consumer<f32>,
    rx_b: Consumer<f32>,
    cmd_rx: Consumer<RtCommand>,
    dsp_a: DspChain,
    dsp_b: DspChain,
    buf_a: Vec<f32>,
    buf_b: Vec<f32>,
    mix_scratch: Vec<f32>,
    gain_a: f32,
    gain_b: f32,
    volume: f32,
    muted: bool,
    paused: bool,
    underrun_a: bool,
    underrun_b: bool,
    rebuffer_a: bool,
    rebuffer_b: bool,
    rebuffer_threshold_samples: usize,
    metrics: Arc<RtMetrics>,
}

impl RtMixerState {
    pub(crate) fn new_pair(sample_rate: u32) -> (Self, SinkProducers) {
        let (tx_a, rx_a) = RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);
        let (tx_b, rx_b) = RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);
        let (cmd_tx, cmd_rx) = RingBuffer::<RtCommand>::new(CMD_RING_CAPACITY);
        let metrics = Arc::new(RtMetrics {
            paused: AtomicBool::new(true),
            stream_health: AtomicU8::new(OutputStreamHealth::Streaming.to_u8()),
            ..RtMetrics::default()
        });

        let mut dsp_a = DspChain::new();
        dsp_a.configure(sample_rate, OUTPUT_CHANNELS);
        let mut dsp_b = DspChain::new();
        dsp_b.configure(sample_rate, OUTPUT_CHANNELS);

        let rebuffer_threshold_samples =
            ((sample_rate as usize * DEFAULT_REBUFFER_MS) / 1_000) * OUTPUT_CHANNELS;

        let scratch_samples = MAX_RT_CHUNK_FRAMES * OUTPUT_CHANNELS;
        let state = Self {
            rx_a,
            rx_b,
            cmd_rx,
            dsp_a,
            dsp_b,
            buf_a: vec![0.0; scratch_samples],
            buf_b: vec![0.0; scratch_samples],
            mix_scratch: vec![0.0; scratch_samples],
            gain_a: 1.0,
            gain_b: 0.0,
            volume: 1.0,
            muted: false,
            paused: true,
            underrun_a: false,
            underrun_b: false,
            rebuffer_a: false,
            rebuffer_b: false,
            rebuffer_threshold_samples,
            metrics: Arc::clone(&metrics),
        };

        let producers = SinkProducers {
            deck_a: tx_a,
            deck_b: tx_b,
            commands: cmd_tx,
            metrics,
            sample_rate,
        };
        (state, producers)
    }

    pub(crate) fn metrics(&self) -> &Arc<RtMetrics> {
        &self.metrics
    }

    pub(crate) fn mark_fatal_output_error(&self, reason: String) {
        self.metrics.mark_disconnected(Some(reason), true);
    }

    fn drain_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.pop() {
            match cmd {
                RtCommand::SetEq(snap) => {
                    self.dsp_a.apply_eq_snapshot(snap);
                    self.dsp_b.apply_eq_snapshot(snap);
                }
                RtCommand::SetSpatial(enabled) => {
                    self.dsp_a.spatial.enabled = enabled;
                    self.dsp_b.spatial.enabled = enabled;
                }
                RtCommand::SetLoudnessGain { deck, gain } => match deck {
                    DeckId::A => self.dsp_a.loudness.set_target_gain(gain),
                    DeckId::B => self.dsp_b.loudness.set_target_gain(gain),
                },
                RtCommand::SetNextLoudnessGain { deck, gain } => match deck {
                    DeckId::A => self.dsp_a.loudness.set_next_target_gain(gain),
                    DeckId::B => self.dsp_b.loudness.set_next_target_gain(gain),
                },
                RtCommand::SetBlendTrim(trim) => {
                    self.dsp_a.loudness.set_blend_trim(trim);
                    self.dsp_b.loudness.set_blend_trim(trim);
                }
                RtCommand::SetTransitionCutoffs {
                    deck,
                    lowpass_hz,
                    highpass_hz,
                } => match deck {
                    DeckId::A => self.dsp_a.transition.set_cutoffs(lowpass_hz, highpass_hz),
                    DeckId::B => self.dsp_b.transition.set_cutoffs(lowpass_hz, highpass_hz),
                },
                RtCommand::OpenTransition(deck) => match deck {
                    DeckId::A => self.dsp_a.transition.open(),
                    DeckId::B => self.dsp_b.transition.open(),
                },
                RtCommand::SetMixGains { gain_a, gain_b } => {
                    self.gain_a = gain_a.clamp(0.0, 1.0);
                    self.gain_b = gain_b.clamp(0.0, 1.0);
                }
                RtCommand::SetVolume(vol) => {
                    self.volume = if vol.is_finite() {
                        vol.clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                }
                RtCommand::SetMuted(muted) => {
                    self.muted = muted;
                }
                RtCommand::SetPaused(paused) => {
                    self.paused = paused;
                    self.metrics.paused.store(paused, Ordering::Release);
                }
                RtCommand::StreamBoundary(deck) => match deck {
                    DeckId::A => self.dsp_a.on_stream_boundary(),
                    DeckId::B => self.dsp_b.on_stream_boundary(),
                },
                RtCommand::FlushDeck { deck, epoch } => {
                    let min_epoch = self.metrics.min_flush_epoch(deck);
                    let is_stale = epoch < min_epoch;
                    let stale_limit = if is_stale {
                        self.metrics.take_stale_drain(deck)
                    } else {
                        usize::MAX
                    };
                    match deck {
                        DeckId::A => {
                            if is_stale {
                                drain_ring_at_most(&mut self.rx_a, stale_limit);
                            } else {
                                drain_ring(&mut self.rx_a);
                            }
                            self.dsp_a.flush();
                            self.underrun_a = false;
                            self.rebuffer_a = false;
                        }
                        DeckId::B => {
                            if is_stale {
                                drain_ring_at_most(&mut self.rx_b, stale_limit);
                            } else {
                                drain_ring(&mut self.rx_b);
                            }
                            self.dsp_b.flush();
                            self.underrun_b = false;
                            self.rebuffer_b = false;
                        }
                    }
                    if !is_stale {
                        self.metrics.reset_played_frames(deck);
                    }
                    self.metrics.rebuffering.store(false, Ordering::Release);
                    self.metrics.set_flush_ack(deck, epoch);
                }
                RtCommand::ResetPlayedFrames(deck) => {
                    self.metrics.reset_played_frames(deck);
                }
            }
        }
    }

    /// Renders `frames` stereo interleaved `f32` frames into `out` (`out.len() >= frames * 2`).
    ///
    /// Strictly RT-safe: zero allocations, zero locks, zero syscalls, zero panics.
    pub fn render_interleaved_f32(&mut self, out: &mut [f32]) {
        self.drain_commands();

        let total_frames = out.len() / OUTPUT_CHANNELS;
        if total_frames == 0 {
            return;
        }
        let total_samples = total_frames * OUTPUT_CHANNELS;

        if self.paused {
            out[..total_samples].fill(0.0);
            self.metrics.rebuffering.store(false, Ordering::Release);
            self.metrics
                .peak_level_bits
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
            return;
        }

        let mut peak = 0.0_f32;
        let master = if self.muted { 0.0 } else { self.volume };
        let use_a = self.gain_a > 1e-6;
        let use_b = self.gain_b > 1e-6;
        let eof_a = self.metrics.deck_eof(DeckId::A);
        let eof_b = self.metrics.deck_eof(DeckId::B);
        let mut any_rebuffering = false;

        let mut offset_frames = 0usize;
        while offset_frames < total_frames {
            let chunk_frames = (total_frames - offset_frames).min(MAX_RT_CHUNK_FRAMES);
            let chunk_samples = chunk_frames * OUTPUT_CHANNELS;
            let dst = &mut out
                [offset_frames * OUTPUT_CHANNELS..(offset_frames + chunk_frames) * OUTPUT_CHANNELS];

            if use_a {
                if self.rebuffer_a {
                    if self.rx_a.slots() >= self.rebuffer_threshold_samples || eof_a {
                        self.rebuffer_a = false;
                    } else {
                        self.buf_a[..chunk_samples].fill(0.0);
                        any_rebuffering = true;
                    }
                }
                if !self.rebuffer_a {
                    let got = pop_samples(&mut self.rx_a, &mut self.buf_a[..chunk_samples]);
                    if self.underrun_a && got > 0 {
                        apply_fade_in(&mut self.buf_a[..got]);
                        self.underrun_a = false;
                    }
                    if got < chunk_samples {
                        if got > 0 {
                            apply_fade_out(&mut self.buf_a[..got]);
                        }
                        self.buf_a[got..chunk_samples].fill(0.0);
                        self.underrun_a = true;
                        self.metrics.underrun_count.fetch_add(1, Ordering::Relaxed);
                        if !eof_a && self.rebuffer_threshold_samples > 0 {
                            self.rebuffer_a = true;
                            any_rebuffering = true;
                        }
                    }
                    if got > 0 {
                        self.metrics
                            .add_played_frames(DeckId::A, (got / OUTPUT_CHANNELS) as u64);
                    }
                }
                self.dsp_a
                    .process_interleaved(&mut self.buf_a[..chunk_samples]);
            }

            if use_b {
                if self.rebuffer_b {
                    if self.rx_b.slots() >= self.rebuffer_threshold_samples || eof_b {
                        self.rebuffer_b = false;
                    } else {
                        self.buf_b[..chunk_samples].fill(0.0);
                        any_rebuffering = true;
                    }
                }
                if !self.rebuffer_b {
                    let got = pop_samples(&mut self.rx_b, &mut self.buf_b[..chunk_samples]);
                    if self.underrun_b && got > 0 {
                        apply_fade_in(&mut self.buf_b[..got]);
                        self.underrun_b = false;
                    }
                    if got < chunk_samples {
                        if got > 0 {
                            apply_fade_out(&mut self.buf_b[..got]);
                        }
                        self.buf_b[got..chunk_samples].fill(0.0);
                        self.underrun_b = true;
                        self.metrics.underrun_count.fetch_add(1, Ordering::Relaxed);
                        if !eof_b && self.rebuffer_threshold_samples > 0 {
                            self.rebuffer_b = true;
                            any_rebuffering = true;
                        }
                    }
                    if got > 0 {
                        self.metrics
                            .add_played_frames(DeckId::B, (got / OUTPUT_CHANNELS) as u64);
                    }
                }
                self.dsp_b
                    .process_interleaved(&mut self.buf_b[..chunk_samples]);
            }

            for i in 0..chunk_samples {
                let mut mixed = 0.0_f32;
                if use_a {
                    mixed += self.buf_a[i] * self.gain_a;
                }
                if use_b {
                    mixed += self.buf_b[i] * self.gain_b;
                }
                let scaled = mixed * master;
                let clean = if scaled.is_finite() { scaled } else { 0.0 };
                dst[i] = clean;
                let mag = clean.abs();
                if mag > peak {
                    peak = mag;
                }
            }

            offset_frames += chunk_frames;
        }

        self.metrics
            .rebuffering
            .store(any_rebuffering, Ordering::Release);
        self.metrics
            .peak_level_bits
            .store(peak.to_bits(), Ordering::Relaxed);
    }

    /// Renders stereo `f32` audio directly into a PipeWire `F32LE` byte slice without allocating.
    fn render_f32le_bytes(&mut self, byte_slice: &mut [u8]) -> usize {
        let stride = std::mem::size_of::<f32>() * OUTPUT_CHANNELS;
        let total_frames = byte_slice.len() / stride;
        if total_frames == 0 {
            return 0;
        }
        let mut frame_offset = 0usize;
        while frame_offset < total_frames {
            let chunk_frames = (total_frames - frame_offset).min(MAX_RT_CHUNK_FRAMES);
            let chunk_samples = chunk_frames * OUTPUT_CHANNELS;
            // Temporarily borrow mix_scratch by splitting render via local call:
            let mut scratch = std::mem::take(&mut self.mix_scratch);
            self.render_interleaved_f32(&mut scratch[..chunk_samples]);
            let base_byte = frame_offset * stride;
            for (idx, &sample) in scratch[..chunk_samples].iter().enumerate() {
                let start = base_byte + idx * 4;
                byte_slice[start..start + 4].copy_from_slice(&sample.to_le_bytes());
            }
            self.mix_scratch = scratch;
            frame_offset += chunk_frames;
        }
        total_frames
    }
}

#[inline]
fn apply_fade_out(samples: &mut [f32]) {
    let frames = samples.len() / OUTPUT_CHANNELS;
    let ramp = frames.min(UNDERRUN_RAMP_FRAMES);
    if ramp == 0 {
        return;
    }
    let start_frame = frames - ramp;
    for f in 0..ramp {
        let gain = 1.0 - (f + 1) as f32 / (ramp + 1) as f32;
        let base = (start_frame + f) * OUTPUT_CHANNELS;
        samples[base] *= gain;
        samples[base + 1] *= gain;
    }
}

#[inline]
fn apply_fade_in(samples: &mut [f32]) {
    let frames = samples.len() / OUTPUT_CHANNELS;
    let ramp = frames.min(UNDERRUN_RAMP_FRAMES);
    if ramp == 0 {
        return;
    }
    for f in 0..ramp {
        let gain = (f + 1) as f32 / (ramp + 1) as f32;
        let base = f * OUTPUT_CHANNELS;
        samples[base] *= gain;
        samples[base + 1] *= gain;
    }
}

#[inline]
fn drain_ring(rx: &mut Consumer<f32>) {
    let slots = rx.slots();
    if slots > 0
        && let Ok(chunk) = rx.read_chunk(slots)
    {
        chunk.commit_all();
    }
}

#[inline]
fn drain_ring_at_most(rx: &mut Consumer<f32>, max_samples: usize) {
    let to_drain = rx.slots().min(max_samples);
    if to_drain > 0
        && let Ok(chunk) = rx.read_chunk(to_drain)
    {
        chunk.commit_all();
    }
}

#[inline]
fn pop_samples(rx: &mut Consumer<f32>, dst: &mut [f32]) -> usize {
    let available = rx.slots().min(dst.len());
    if available == 0 {
        return 0;
    }
    let Ok(chunk) = rx.read_chunk(available) else {
        return 0;
    };
    let (first, second) = chunk.as_slices();
    let n1 = first.len();
    dst[..n1].copy_from_slice(first);
    let n2 = second.len();
    if n2 > 0 {
        dst[n1..n1 + n2].copy_from_slice(second);
    }
    chunk.commit_all();
    n1 + n2
}

enum PwControl {
    Quit,
}

/// Live PipeWire native stereo `F32LE` output stream running on a dedicated OS thread.
pub struct PipeWireSink {
    sample_rate: u32,
    force_rate: bool,
    ctrl_tx: pw::channel::Sender<PwControl>,
    thread: Option<JoinHandle<()>>,
}

impl PipeWireSink {
    /// Spawns a PipeWire playback stream at `sample_rate` (default `48_000 Hz`).
    pub fn spawn(sample_rate: u32) -> Result<(Self, SinkProducers), OutputError> {
        Self::spawn_with_force_rate(sample_rate, false)
    }

    /// Spawns a PipeWire stream, optionally hinting `node.force-rate` / `node.rate`
    /// so PipeWire switches compatible DACs to the track's native sample rate.
    pub fn spawn_with_force_rate(
        sample_rate: u32,
        force_rate: bool,
    ) -> Result<(Self, SinkProducers), OutputError> {
        let (rt_state, producers) = RtMixerState::new_pair(sample_rate);
        let (ctrl_tx, ctrl_rx) = pw::channel::channel::<PwControl>();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<(), String>>(1);

        let thread = thread::Builder::new()
            .name("bitchord-pw-audio".to_owned())
            .spawn(move || {
                run_pipewire_loop(sample_rate, force_rate, rt_state, ctrl_rx, ready_tx);
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    sample_rate,
                    force_rate,
                    ctrl_tx,
                    thread: Some(thread),
                },
                producers,
            )),
            Ok(Err(msg)) => {
                let _ = thread.join();
                Err(OutputError::PipeWire(msg))
            }
            Err(_) => {
                let _ = thread.join();
                Err(OutputError::PipeWire(
                    "PipeWire thread exited before completing initialization".to_owned(),
                ))
            }
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn force_rate(&self) -> bool {
        self.force_rate
    }
}

impl Drop for PipeWireSink {
    fn drop(&mut self) {
        let _ = self.ctrl_tx.send(PwControl::Quit);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_pipewire_loop(
    sample_rate: u32,
    force_rate: bool,
    rt_state: RtMixerState,
    ctrl_rx: pw::channel::Receiver<PwControl>,
    ready_tx: std_mpsc::SyncSender<Result<(), String>>,
) {
    pw::init();

    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(m) => m,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("MainLoopRc::new: {err}")));
            return;
        }
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("ContextRc::new: {err}")));
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("ContextRc::connect_rc: {err}")));
            return;
        }
    };

    let _ctrl_attached = ctrl_rx.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |msg| match msg {
            PwControl::Quit => mainloop.quit(),
        }
    });

    let metrics_for_core = Arc::clone(rt_state.metrics());
    let mainloop_for_core = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, res, message| {
            // id == 0 is the PipeWire core object itself (e.g. pipewire daemon restart).
            if id == 0 {
                metrics_for_core.mark_disconnected(
                    Some(format!("PipeWire core error ({res}): {message}")),
                    true,
                );
                mainloop_for_core.quit();
            }
        })
        .register();

    // Target ~42.7 ms quantum (2048/48000, scaled proportionally at higher rates) so
    // Bluetooth TWS (A2DP SBC/AAC/LDAC) and laptop speakers never xrun on tiny quanta.
    let latency_frames = ((u64::from(sample_rate) * 2_048) / 48_000).clamp(1_024, 8_192);
    let latency_frac = format!("{latency_frames}/{sample_rate}");

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::APP_NAME => "Myusic",
        *pw::keys::AUDIO_CHANNELS => "2",
        *pw::keys::NODE_LATENCY => latency_frac,
    };
    if force_rate {
        let rate_frac = format!("1/{sample_rate}");
        let rate_str = sample_rate.to_string();
        props.insert("node.rate", rate_frac);
        props.insert("node.force-rate", rate_str);
    }

    rt_state
        .metrics()
        .set_stream_health(OutputStreamHealth::Connecting);

    let stream = match pw::stream::StreamBox::new(&core, "bitchord-playback", props) {
        Ok(s) => s,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("StreamBox::new: {err}")));
            return;
        }
    };

    let _listener = match stream
        .add_local_listener_with_user_data(rt_state)
        .state_changed(|_stream, state, old, new| match new {
            pw::stream::StreamState::Streaming => {
                state.metrics().set_stream_health(OutputStreamHealth::Streaming);
                state.metrics().clear_reconnect_flag();
            }
            pw::stream::StreamState::Paused => {
                state.metrics().set_stream_health(OutputStreamHealth::Paused);
            }
            pw::stream::StreamState::Connecting => {
                state.metrics().set_stream_health(OutputStreamHealth::Connecting);
            }
            pw::stream::StreamState::Unconnected => {
                if matches!(
                    old,
                    pw::stream::StreamState::Streaming | pw::stream::StreamState::Paused
                ) {
                    state.metrics().mark_disconnected(
                        Some("PipeWire output stream disconnected".to_owned()),
                        false,
                    );
                } else {
                    state
                        .metrics()
                        .set_stream_health(OutputStreamHealth::Disconnected);
                }
            }
            pw::stream::StreamState::Error(msg) => {
                state.metrics().mark_disconnected(Some(msg), true);
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested_frames = buffer.requested() as usize;
            let stride = std::mem::size_of::<f32>() * OUTPUT_CHANNELS;
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let n_frames = if let Some(slice) = data.data() {
                let max_frames = slice.len() / stride;
                let target_frames = if requested_frames > 0 {
                    requested_frames.min(max_frames)
                } else {
                    max_frames
                };
                let target_bytes = target_frames * stride;
                state.render_f32le_bytes(&mut slice[..target_bytes])
            } else {
                0
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * n_frames) as _;
        })
        .register()
    {
        Ok(l) => l,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("stream listener register: {err}")));
            return;
        }
    };

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(sample_rate);
    audio_info.set_channels(OUTPUT_CHANNELS as u32);
    let mut position = [0u32; MAX_CHANNELS];
    position[0] = libspa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = libspa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);

    let values: Vec<u8> = match pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: libspa::sys::SPA_TYPE_OBJECT_Format,
            id: libspa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    ) {
        Ok((cursor, _)) => cursor.into_inner(),
        Err(err) => {
            let _ = ready_tx.send(Err(format!("PodSerializer::serialize: {err:?}")));
            return;
        }
    };

    let Some(pod) = Pod::from_bytes(&values) else {
        let _ = ready_tx.send(Err("Pod::from_bytes failed for F32LE format".to_owned()));
        return;
    };
    let mut params = [pod];

    if let Err(err) = stream.connect(
        pw::spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    ) {
        let _ = ready_tx.send(Err(format!("stream.connect: {err}")));
        return;
    }

    let _ = ready_tx.send(Ok(()));
    mainloop.run();
}

/// Deterministic headless sink driving the exact same [`RtMixerState`] DSP/mixer
/// without touching PipeWire (used by unit tests and `bitchord-cli play --no-output`).
pub struct NullAudioSink {
    sample_rate: u32,
    state: RtMixerState,
}

impl NullAudioSink {
    pub fn new(sample_rate: u32) -> (Self, SinkProducers) {
        let (state, producers) = RtMixerState::new_pair(sample_rate);
        (Self { sample_rate, state }, producers)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Renders `frames` stereo interleaved `f32` frames through the full RT DSP + mixer.
    pub fn render_frames(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0_f32; frames * OUTPUT_CHANNELS];
        self.state.render_interleaved_f32(&mut out);
        out
    }

    /// Renders into a caller-supplied stereo interleaved `f32` buffer.
    pub fn render_into(&mut self, out: &mut [f32]) {
        self.state.render_interleaved_f32(out);
    }

    /// Processes any queued [`RtCommand`] entries without advancing audio frames.
    pub fn drain_commands(&mut self) {
        self.state.drain_commands();
    }
}

/// Concrete output backend held by [`crate::engine::AudioEngine`].
pub enum AudioBackend {
    PipeWire(PipeWireSink),
    AlsaExclusive(AlsaExclusiveSink),
    Null(Box<NullAudioSink>),
}

impl AudioBackend {
    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::PipeWire(pw) => pw.sample_rate(),
            Self::AlsaExclusive(alsa) => alsa.sample_rate(),
            Self::Null(null) => null.sample_rate(),
        }
    }

    pub fn device_label(&self) -> String {
        match self {
            Self::PipeWire(pw) if pw.force_rate() => "PipeWire (Force-Rate)".to_owned(),
            Self::PipeWire(_) => "PipeWire".to_owned(),
            Self::AlsaExclusive(alsa) => format!("ALSA Exclusive ({})", alsa.device()),
            Self::Null(_) => "Headless NullSink".to_owned(),
        }
    }

    pub fn format_label(&self) -> &'static str {
        match self {
            Self::PipeWire(_) | Self::Null(_) => "F32LE",
            Self::AlsaExclusive(alsa) => alsa.format().label(),
        }
    }

    /// `true` when this backend bypasses fixed-rate OS resampling (ALSA Exclusive or PipeWire Force-Rate).
    pub fn supports_bit_perfect(&self) -> bool {
        match self {
            Self::AlsaExclusive(_) => true,
            Self::PipeWire(pw) => pw.force_rate(),
            Self::Null(_) => false,
        }
    }

    pub fn as_null_mut(&mut self) -> Option<&mut NullAudioSink> {
        match self {
            Self::Null(null) => Some(null.as_mut()),
            Self::PipeWire(_) | Self::AlsaExclusive(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crossfade::equal_power_gains;

    #[test]
    fn null_sink_mixes_dual_decks_equal_power_and_handles_pause_and_underrun() {
        let (mut sink, mut prod) = NullAudioSink::new(OUTPUT_SAMPLE_RATE);

        // Push constant 0.5 on Deck A and -0.25 on Deck B.
        for _ in 0..512 {
            prod.deck_a.push(0.5).unwrap();
            prod.deck_a.push(0.5).unwrap();
            prod.deck_b.push(-0.25).unwrap();
            prod.deck_b.push(-0.25).unwrap();
        }

        // Starts paused -> outputs exact silence.
        let silent = sink.render_frames(128);
        assert!(silent.iter().all(|&s| s == 0.0));
        assert_eq!(prod.metrics.played_frames(DeckId::A), 0);

        // Unpause + midpoint equal-power blend.
        let gains = equal_power_gains(0.5);
        prod.commands.push(RtCommand::SetPaused(false)).unwrap();
        prod.commands
            .push(RtCommand::SetMixGains {
                gain_a: gains.outgoing,
                gain_b: gains.incoming,
            })
            .unwrap();
        prod.commands
            .push(RtCommand::SetBlendTrim(gains.blend_trim))
            .unwrap();

        let mixed = sink.render_frames(256);
        let expected = (0.5 * gains.outgoing - 0.25 * gains.incoming) * gains.blend_trim;
        assert!(
            (mixed[10] - expected).abs() < 1e-3,
            "got {}, expected {expected}",
            mixed[10]
        );
        assert_eq!(prod.metrics.played_frames(DeckId::A), 256);
        assert_eq!(prod.metrics.played_frames(DeckId::B), 256);
        assert_eq!(prod.metrics.underruns(), 0);
        assert!(prod.metrics.peak_level() > 0.1);

        // Drain remaining 256 frames + request 256 more -> counts underrun without panicking.
        let _ = sink.render_frames(512);
        assert!(prod.metrics.underruns() >= 1);

        // FlushDeck resets played frames, acknowledges epoch, and drains cleanly.
        prod.commands
            .push(RtCommand::FlushDeck {
                deck: DeckId::A,
                epoch: 7,
            })
            .unwrap();
        let _ = sink.render_frames(16);
        assert_eq!(prod.metrics.played_frames(DeckId::A), 0);
        assert_eq!(prod.metrics.flush_ack(DeckId::A), 7);
    }

    #[test]
    fn low_watermark_rebuffer_holds_until_threshold_and_releases_cleanly_at_eof() {
        let (mut sink, mut prod) = NullAudioSink::new(OUTPUT_SAMPLE_RATE);
        prod.commands.push(RtCommand::SetPaused(false)).unwrap();

        // Trigger a mid-track underrun (deck_eof == false).
        let _ = sink.render_frames(128);
        assert!(prod.metrics.is_rebuffering());

        // Pushing fewer samples than DEFAULT_REBUFFER_MS (~350ms = 16_800 frames)
        // must keep the mixer rebuffering (outputting silence without consuming them).
        for _ in 0..256 {
            prod.deck_a.push(0.5).unwrap();
            prod.deck_a.push(0.5).unwrap();
        }
        let silent = sink.render_frames(128);
        assert!(silent.iter().all(|&s| s == 0.0));
        assert!(prod.metrics.is_rebuffering());
        assert_eq!(prod.metrics.played_frames(DeckId::A), 0);

        // Once EOF is signaled, rebuffering releases immediately to drain the tail.
        prod.metrics.set_deck_eof(DeckId::A, true);
        let tail = sink.render_frames(256);
        assert!(!prod.metrics.is_rebuffering());
        assert_eq!(prod.metrics.played_frames(DeckId::A), 256);
        assert!(tail.iter().any(|&s| s.abs() > 0.1));
    }

    #[test]
    fn stale_flush_after_timeout_only_trims_old_samples_and_preserves_new_track() {
        let (mut sink, mut prod) = NullAudioSink::new(OUTPUT_SAMPLE_RATE);

        // Old track left 4 stereo frames (8 samples) of -0.5 in the ring.
        for _ in 0..8 {
            prod.deck_a.push(-0.5).unwrap();
        }
        // Engine queues FlushDeck epoch 1, times out, arms guard (min_epoch=2, stale=8).
        prod.commands
            .push(RtCommand::FlushDeck {
                deck: DeckId::A,
                epoch: 1,
            })
            .unwrap();
        prod.metrics.arm_stale_flush_guard(DeckId::A, 2, 8);

        // New track immediately starts pushing 128 frames of +0.75 before RT wakes up.
        for _ in 0..256 {
            prod.deck_a.push(0.75).unwrap();
        }

        // Now RT wakes up, unpauses, drains commands, and renders.
        prod.metrics.set_deck_eof(DeckId::A, true);
        prod.commands.push(RtCommand::SetPaused(false)).unwrap();
        let out = sink.render_frames(128);

        // Old -0.5 samples were trimmed; all 128 frames of new +0.75 survived intact!
        assert_eq!(prod.metrics.flush_ack(DeckId::A), 1);
        assert_eq!(prod.metrics.played_frames(DeckId::A), 128);
        assert!(
            out[0] > 0.5 && out[100] > 0.5,
            "new track opening samples must survive stale flush: out[0]={}, out[100]={}",
            out[0],
            out[100]
        );
    }
}
