//! Real-time audio output backend (`PipeWire` native F32LE stream + deterministic
//! `NullAudioSink`) backed by lock-free `rtrb` SPSC ring buffers.
//!
//! Strictly adheres to real-time audio safety (`linux-audio-engine.md` §3):
//! - **Zero** heap allocations, deallocations, mutex locks, or syscalls inside
//!   the PipeWire `process` callback.
//! - **Zero** `unsafe` code (`#![forbid(unsafe_code)]`).
//! - Per-deck `DspChain` (`Spatial -> Equalizer -> TransitionFilter -> Loudness`)
//!   is pre-allocated off-thread before entering the audio thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};

use libspa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use libspa::pod::Pod;
use pipewire as pw;
use pw::properties::properties;
use rtrb::{Consumer, Producer, RingBuffer};
use thiserror::Error;

use crate::crossfade::DeckId;
use crate::dsp::{DspChain, EqTuningSnapshot};

/// Canonical PipeWire output sample rate (48 kHz).
pub const OUTPUT_SAMPLE_RATE: u32 = 48_000;
/// Canonical stereo channel count.
pub const OUTPUT_CHANNELS: usize = 2;
/// Per-deck ring buffer duration (3 seconds of stereo f32 PCM).
pub const RING_DURATION_SECS: usize = 3;
/// Per-deck `rtrb` ring capacity in interleaved `f32` samples (~1.15 MiB per deck).
pub const RING_CAPACITY_SAMPLES: usize =
    OUTPUT_SAMPLE_RATE as usize * OUTPUT_CHANNELS * RING_DURATION_SECS;
/// Capacity of the lock-free control command ring from engine thread to RT thread.
pub const CMD_RING_CAPACITY: usize = 256;
/// Fixed per-chunk frame limit inside the RT callback so scratch buffers never resize.
const MAX_RT_CHUNK_FRAMES: usize = 4_096;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("PipeWire initialization/connection failed: {0}")]
    PipeWire(String),
    #[error("failed to spawn PipeWire audio thread: {0}")]
    ThreadSpawn(#[from] std::io::Error),
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
    FlushDeck(DeckId),
    ResetPlayedFrames(DeckId),
}

/// Lock-free telemetry published by the RT callback and read from any thread.
#[derive(Debug, Default)]
pub struct RtMetrics {
    played_frames_a: AtomicU64,
    played_frames_b: AtomicU64,
    underrun_count: AtomicU64,
    peak_level_bits: AtomicU32,
    paused: AtomicBool,
}

impl RtMetrics {
    pub fn played_frames(&self, deck: DeckId) -> u64 {
        match deck {
            DeckId::A => self.played_frames_a.load(Ordering::Acquire),
            DeckId::B => self.played_frames_b.load(Ordering::Acquire),
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
    metrics: Arc<RtMetrics>,
}

impl RtMixerState {
    fn new_pair(sample_rate: u32) -> (Self, SinkProducers) {
        let (tx_a, rx_a) = RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);
        let (tx_b, rx_b) = RingBuffer::<f32>::new(RING_CAPACITY_SAMPLES);
        let (cmd_tx, cmd_rx) = RingBuffer::<RtCommand>::new(CMD_RING_CAPACITY);
        let metrics = Arc::new(RtMetrics {
            paused: AtomicBool::new(true),
            ..RtMetrics::default()
        });

        let mut dsp_a = DspChain::new();
        dsp_a.configure(sample_rate, OUTPUT_CHANNELS);
        let mut dsp_b = DspChain::new();
        dsp_b.configure(sample_rate, OUTPUT_CHANNELS);

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
                RtCommand::FlushDeck(deck) => {
                    match deck {
                        DeckId::A => {
                            drain_ring(&mut self.rx_a);
                            self.dsp_a.flush();
                        }
                        DeckId::B => {
                            drain_ring(&mut self.rx_b);
                            self.dsp_b.flush();
                        }
                    }
                    self.metrics.reset_played_frames(deck);
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
            self.metrics
                .peak_level_bits
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
            return;
        }

        let mut peak = 0.0_f32;
        let master = if self.muted { 0.0 } else { self.volume };
        let use_a = self.gain_a > 1e-6;
        let use_b = self.gain_b > 1e-6;

        let mut offset_frames = 0usize;
        while offset_frames < total_frames {
            let chunk_frames = (total_frames - offset_frames).min(MAX_RT_CHUNK_FRAMES);
            let chunk_samples = chunk_frames * OUTPUT_CHANNELS;
            let dst = &mut out
                [offset_frames * OUTPUT_CHANNELS..(offset_frames + chunk_frames) * OUTPUT_CHANNELS];

            if use_a {
                let got = pop_samples(&mut self.rx_a, &mut self.buf_a[..chunk_samples]);
                if got < chunk_samples {
                    self.buf_a[got..chunk_samples].fill(0.0);
                    self.metrics.underrun_count.fetch_add(1, Ordering::Relaxed);
                }
                if got > 0 {
                    self.metrics
                        .add_played_frames(DeckId::A, (got / OUTPUT_CHANNELS) as u64);
                }
                self.dsp_a
                    .process_interleaved(&mut self.buf_a[..chunk_samples]);
            }

            if use_b {
                let got = pop_samples(&mut self.rx_b, &mut self.buf_b[..chunk_samples]);
                if got < chunk_samples {
                    self.buf_b[got..chunk_samples].fill(0.0);
                    self.metrics.underrun_count.fetch_add(1, Ordering::Relaxed);
                }
                if got > 0 {
                    self.metrics
                        .add_played_frames(DeckId::B, (got / OUTPUT_CHANNELS) as u64);
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
fn drain_ring(rx: &mut Consumer<f32>) {
    let slots = rx.slots();
    if slots > 0
        && let Ok(chunk) = rx.read_chunk(slots)
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
    ctrl_tx: pw::channel::Sender<PwControl>,
    thread: Option<JoinHandle<()>>,
}

impl PipeWireSink {
    /// Spawns a PipeWire playback stream at `sample_rate` (default `48_000 Hz`).
    pub fn spawn(sample_rate: u32) -> Result<(Self, SinkProducers), OutputError> {
        let (rt_state, producers) = RtMixerState::new_pair(sample_rate);
        let (ctrl_tx, ctrl_rx) = pw::channel::channel::<PwControl>();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<(), String>>(1);

        let thread = thread::Builder::new()
            .name("bitchord-pw-audio".to_owned())
            .spawn(move || {
                run_pipewire_loop(sample_rate, rt_state, ctrl_rx, ready_tx);
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    sample_rate,
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

    let stream = match pw::stream::StreamBox::new(
        &core,
        "bitchord-playback",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::APP_NAME => "BitChord",
            *pw::keys::AUDIO_CHANNELS => "2",
        },
    ) {
        Ok(s) => s,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("StreamBox::new: {err}")));
            return;
        }
    };

    let _listener = match stream
        .add_local_listener_with_user_data(rt_state)
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let stride = std::mem::size_of::<f32>() * OUTPUT_CHANNELS;
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let n_frames = if let Some(slice) = data.data() {
                state.render_f32le_bytes(slice)
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
    Null(Box<NullAudioSink>),
}

impl AudioBackend {
    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::PipeWire(pw) => pw.sample_rate(),
            Self::Null(null) => null.sample_rate(),
        }
    }

    pub fn as_null_mut(&mut self) -> Option<&mut NullAudioSink> {
        match self {
            Self::Null(null) => Some(null.as_mut()),
            Self::PipeWire(_) => None,
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

        // FlushDeck resets played frames and drains cleanly.
        prod.commands.push(RtCommand::FlushDeck(DeckId::A)).unwrap();
        let _ = sink.render_frames(16);
        assert_eq!(prod.metrics.played_frames(DeckId::A), 0);
    }
}
