//! Dual-deck native Linux [`AudioEngine`] uniting Symphonia+libopus decoding,
//! `ChunkedRangeSource` HTTP range streaming, `SkipSilenceProcessor`,
//! `SonicStretcher`, `AudioResampler`, lock-free `rtrb` rings, PipeWire/Null RT
//! DSP (`DspChain`), equal-power crossfade, gapless preload, and [`SleepTimer`].

use std::collections::VecDeque;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bitchord_automix::TransitionPlan;
use bitchord_innertube::{PlayerClient, ResolvedStream};
use bitchord_integrations::NerdStatsSnapshot;
use bitchord_model::{Song, duration_millis};
use reqwest::Client;
use rtrb::Producer;
use serde::{Deserialize, Serialize};
use symphonia::core::formats::probe::Hint;
use thiserror::Error;
use tokio::runtime::Handle;

use crate::alsa_output::AlsaExclusiveSink;
use crate::chunked_source::{ChunkedRangeSource, RefusalCallback, StreamCancelToken};
use crate::crossfade::{CrossfadePhase, CrossfadeState, DeckId};
use crate::decoder::{AudioDecoder, DecodedPacket, DecodedStreamInfo, DecoderError};
use crate::dsp::{EqTuningSnapshot, loudness_db_to_linear_gain};
use crate::output::{
    AudioBackend, AudioOutputMode, NullAudioSink, OUTPUT_CHANNELS, OUTPUT_SAMPLE_RATE, OutputError,
    OutputStreamHealth, PipeWireSink, RING_CAPACITY_SAMPLES, RtCommand, RtMetrics, SinkProducers,
};
use crate::resampler::{AudioResampler, ResamplerError};
use crate::skip_silence::SkipSilenceProcessor;
use crate::sleep_timer::SleepTimer;
use crate::time_stretch::{MAX_SPEED, MIN_SPEED, SonicStretcher};

const SLEEP_FADE_MS: u64 = 2_000;
const WORKER_IDLE_SLEEP: Duration = Duration::from_millis(2);
const SEEK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const FLUSH_ACK_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("audio output error: {0}")]
    Output(#[from] OutputError),
    #[error("decoder error: {0}")]
    Decoder(#[from] DecoderError),
    #[error("resampler error: {0}")]
    Resampler(#[from] ResamplerError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("deck worker channel disconnected before initialization")]
    WorkerUnavailable,
    #[error("operation requires NullAudioSink backend")]
    NotNullBackend,
}

/// High-level playback transport state exposed to UI, CLI, and MPRIS2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackState {
    #[default]
    Stopped,
    Buffering,
    Playing,
    Paused,
    Ended,
    Error,
}

/// Complete observable engine snapshot (`camelCase` JSON for CLI/UI/diagnostics).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSnapshot {
    pub state: PlaybackState,
    pub current_song: Option<Song>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub muted: bool,
    pub speed: f32,
    pub skip_silence: bool,
    pub spatial_audio: bool,
    pub eq_enabled: bool,
    pub crossfade_seconds: u32,
    pub crossfade_phase: CrossfadePhase,
    pub active_deck: DeckId,
    pub nerd_stats: Option<NerdStatsSnapshot>,
    pub sleep_timer_remaining_ms: Option<u64>,
    pub sleep_after_track: bool,
    pub underruns: u64,
    pub peak_level: f32,
    pub skipped_silence_ms: u64,
    pub last_error: Option<String>,
    pub output_mode: AudioOutputMode,
    pub output_device_label: String,
    pub output_format_label: String,
    pub output_sample_rate_hz: u32,
    /// `true` only when hardware clock matches source rate with zero resampling or DSP alteration.
    pub bit_perfect_active: bool,
    /// Active or armed Smart Automix / LoopMix transition summary (if planned).
    pub automix_plan_summary: Option<String>,
    /// Live health of the audio output backend (PipeWire / ALSA).
    pub output_health: OutputStreamHealth,
    /// `true` when PipeWire/ALSA reported a disconnect or fatal error requiring recovery.
    pub needs_output_reconnect: bool,
    /// Latest output backend disconnect / fallback diagnostic message, if any.
    pub output_error: Option<String>,
}

/// Summary produced by headless [`AudioEngine::drain_to_eof_null`] runs (`--no-output`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadlessPlaybackReport {
    pub song: Option<Song>,
    pub output_sample_rate_hz: u32,
    pub rendered_frames: u64,
    pub rendered_duration_ms: u64,
    pub peak_level: f32,
    pub rms_level: f32,
    pub underruns: u64,
    pub skipped_silence_ms: u64,
    pub nerd_stats: Option<NerdStatsSnapshot>,
    pub final_state: PlaybackState,
}

/// Input media source specification for loading a deck.
pub enum AudioSourceSpec {
    LocalFile(PathBuf),
    MemoryBytes {
        bytes: Vec<u8>,
        extension: Option<String>,
    },
    HttpStream {
        http: Client,
        rt: Handle,
        stream: Box<ResolvedStream>,
        on_refused: Option<RefusalCallback>,
    },
    /// Already-opened decoder plus optional opening packets decoded off-thread
    /// so playback starts cleanly from exact sample 0 with prebuffered PCM.
    PreparedDecoder {
        decoder: Box<AudioDecoder>,
        prebuffered: Vec<DecodedPacket>,
    },
}

struct WorkerShared {
    cancel: AtomicBool,
    cancel_token: Mutex<Option<StreamCancelToken>>,
    seek_target_ms: AtomicI64,
    seek_epoch: AtomicU64,
    seek_gate: AtomicBool,
    speed_bits: AtomicU32,
    skip_silence: AtomicBool,
    eof_reached: AtomicBool,
    base_position_ms: AtomicU64,
    skipped_silence_ms: AtomicU64,
    pushed_frames: AtomicU64,
    error_msg: Mutex<Option<String>>,
}

impl WorkerShared {
    fn set_cancel_token(&self, token: Option<StreamCancelToken>) {
        if let Some(ref tok) = token
            && self.cancel.load(Ordering::Acquire)
        {
            tok.cancel();
        }
        *self.cancel_token.lock().unwrap_or_else(|e| e.into_inner()) = token;
    }

    fn trigger_cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.seek_gate.store(false, Ordering::Release);
        let token = self
            .cancel_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(tok) = token {
            tok.cancel();
        }
    }
}

struct DeckWorker {
    shared: Arc<WorkerShared>,
    join: JoinHandle<Producer<f32>>,
}

struct DeckSlot {
    producer: Option<Producer<f32>>,
    worker: Option<DeckWorker>,
    song: Option<Song>,
    stream_info: Option<DecodedStreamInfo>,
    nerd_stats: Option<NerdStatsSnapshot>,
    loudness_gain: f32,
}

impl DeckSlot {
    fn new(producer: Producer<f32>) -> Self {
        Self {
            producer: Some(producer),
            worker: None,
            song: None,
            stream_info: None,
            nerd_stats: None,
            loudness_gain: 1.0,
        }
    }

    fn is_loaded(&self) -> bool {
        self.worker.is_some()
    }

    fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shared.trigger_cancel();
            if let Ok(producer) = worker.join.join() {
                self.producer = Some(producer);
            }
        }
        self.song = None;
        self.stream_info = None;
        self.nerd_stats = None;
        self.loudness_gain = 1.0;
    }

    fn eof_reached(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|w| w.shared.eof_reached.load(Ordering::Acquire))
    }

    fn pushed_frames(&self) -> u64 {
        self.worker
            .as_ref()
            .map_or(0, |w| w.shared.pushed_frames.load(Ordering::Acquire))
    }

    fn base_position_ms(&self) -> u64 {
        self.worker
            .as_ref()
            .map_or(0, |w| w.shared.base_position_ms.load(Ordering::Acquire))
    }

    fn skipped_silence_ms(&self) -> u64 {
        self.worker
            .as_ref()
            .map_or(0, |w| w.shared.skipped_silence_ms.load(Ordering::Relaxed))
    }

    fn last_error(&self) -> Option<String> {
        self.worker.as_ref().and_then(|w| {
            w.shared
                .error_msg
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        })
    }

    fn apply_dynamic_params(&self, speed: f32, skip_silence: bool) {
        if let Some(ref w) = self.worker {
            w.shared
                .speed_bits
                .store(speed.to_bits(), Ordering::Relaxed);
            w.shared.skip_silence.store(skip_silence, Ordering::Relaxed);
        }
    }
}

/// Dual-deck Linux native audio engine.
pub struct AudioEngine {
    backend: AudioBackend,
    cmd_tx: Producer<RtCommand>,
    metrics: Arc<RtMetrics>,
    deck_a: DeckSlot,
    deck_b: DeckSlot,
    crossfade: CrossfadeState,
    crossfade_outgoing: DeckId,
    planned_transition: Option<TransitionPlan>,
    active_transition: Option<TransitionPlan>,
    sleep_timer: SleepTimer,

    state: PlaybackState,
    volume: f32,
    muted: bool,
    speed: f32,
    skip_silence: bool,
    spatial_audio: bool,
    eq_snapshot: EqTuningSnapshot,
    output_mode: AudioOutputMode,
    next_flush_epoch: u64,
    last_output_notice: Option<String>,
}

impl AudioEngine {
    /// Creates an engine connected to the live Linux PipeWire audio server (`48 kHz F32LE`).
    pub fn with_pipewire() -> Result<Self, EngineError> {
        Self::with_output_mode(AudioOutputMode::PipeWireAuto, OUTPUT_SAMPLE_RATE, None)
    }

    /// Creates an engine using the specified [`AudioOutputMode`] (PipeWire Auto,
    /// PipeWire Force-Rate, or direct ALSA Exclusive `hw:X,Y`).
    pub fn with_output_mode(
        mode: AudioOutputMode,
        sample_rate: u32,
        preferred_bits: Option<u32>,
    ) -> Result<Self, EngineError> {
        let rate = sample_rate.clamp(8_000, 384_000);
        let (backend, producers) = spawn_backend_for_mode(&mode, rate, preferred_bits)?;
        Ok(Self::from_parts(backend, producers, mode))
    }

    /// Creates a deterministic headless engine backed by [`NullAudioSink`]
    /// (identical RT DSP/mixer path without requiring a PipeWire daemon).
    pub fn with_null_sink() -> Self {
        Self::with_null_sink_rate(OUTPUT_SAMPLE_RATE)
    }

    /// Creates a headless engine at a custom output sample rate.
    pub fn with_null_sink_rate(sample_rate: u32) -> Self {
        let (sink, producers) = NullAudioSink::new(sample_rate);
        Self::from_parts(
            AudioBackend::Null(Box::new(sink)),
            producers,
            AudioOutputMode::PipeWireAuto,
        )
    }

    fn from_parts(
        backend: AudioBackend,
        producers: SinkProducers,
        output_mode: AudioOutputMode,
    ) -> Self {
        Self {
            backend,
            cmd_tx: producers.commands,
            metrics: producers.metrics,
            deck_a: DeckSlot::new(producers.deck_a),
            deck_b: DeckSlot::new(producers.deck_b),
            crossfade: CrossfadeState::new(0),
            crossfade_outgoing: DeckId::A,
            planned_transition: None,
            active_transition: None,
            sleep_timer: SleepTimer::new(),
            state: PlaybackState::Stopped,
            volume: 1.0,
            muted: false,
            speed: 1.0,
            skip_silence: false,
            spatial_audio: false,
            eq_snapshot: EqTuningSnapshot::off(),
            output_mode,
            next_flush_epoch: 0,
            last_output_notice: None,
        }
    }

    pub fn output_mode(&self) -> &AudioOutputMode {
        &self.output_mode
    }

    pub fn output_health(&self) -> OutputStreamHealth {
        self.metrics.stream_health()
    }

    pub fn needs_output_reconnect(&self) -> bool {
        self.metrics.needs_reconnect()
    }

    pub fn output_error(&self) -> Option<String> {
        self.metrics
            .stream_error()
            .or_else(|| self.last_output_notice.clone())
    }

    /// Re-initializes the active output backend after a PipeWire restart / TWS
    /// disconnect or falls back from a disconnected ALSA USB DAC to PipeWire Auto.
    /// Returns `Ok(true)` when a backend swap occurred.
    pub fn recover_output_backend(&mut self) -> Result<bool, EngineError> {
        if matches!(self.backend, AudioBackend::Null(_)) {
            return Ok(false);
        }
        let reason = self.metrics.stream_error();
        let target_mode = match &self.output_mode {
            AudioOutputMode::AlsaExclusive { .. } => AudioOutputMode::PipeWireAuto,
            other => other.clone(),
        };
        let target_rate = match &target_mode {
            AudioOutputMode::PipeWireAuto => OUTPUT_SAMPLE_RATE,
            _ => self.backend.sample_rate().clamp(8_000, 384_000),
        };
        self.stop();
        let (backend, producers) = spawn_backend_for_mode(&target_mode, target_rate, None)?;
        self.install_backend(backend, producers, target_mode);
        self.last_output_notice = reason;
        Ok(true)
    }

    /// Switches live audio output backend (PipeWire Auto / PipeWire Force-Rate / ALSA Exclusive)
    /// while preserving volume, mute, EQ, spatial, speed, and crossfade settings.
    pub fn set_output_mode(
        &mut self,
        mode: AudioOutputMode,
        sample_rate: Option<u32>,
        preferred_bits: Option<u32>,
    ) -> Result<(), EngineError> {
        self.stop();
        let target_rate = match &mode {
            AudioOutputMode::PipeWireAuto => OUTPUT_SAMPLE_RATE,
            AudioOutputMode::PipeWireForceRate | AudioOutputMode::AlsaExclusive { .. } => {
                sample_rate
                    .or_else(|| {
                        self.deck(self.crossfade.active_deck)
                            .stream_info
                            .as_ref()
                            .map(|i| i.sample_rate_hz)
                    })
                    .unwrap_or(OUTPUT_SAMPLE_RATE)
                    .clamp(8_000, 384_000)
            }
        };
        let (backend, producers) = spawn_backend_for_mode(&mode, target_rate, preferred_bits)?;
        self.install_backend(backend, producers, mode);
        Ok(())
    }

    fn install_backend(
        &mut self,
        backend: AudioBackend,
        producers: SinkProducers,
        mode: AudioOutputMode,
    ) {
        self.deck_a.stop_worker();
        self.deck_b.stop_worker();
        self.backend = backend;
        self.cmd_tx = producers.commands;
        self.metrics = producers.metrics;
        self.deck_a = DeckSlot::new(producers.deck_a);
        self.deck_b = DeckSlot::new(producers.deck_b);
        self.output_mode = mode;
        self.next_flush_epoch = 0;

        // Re-apply user DSP/transport parameters to the newly spawned RT mixer.
        let vol = self.volume;
        let muted = self.muted;
        let spatial = self.spatial_audio;
        let eq = self.eq_snapshot;
        self.send_rt(RtCommand::SetVolume(vol));
        self.send_rt(RtCommand::SetMuted(muted));
        self.send_rt(RtCommand::SetSpatial(spatial));
        self.send_rt(RtCommand::SetEq(eq));
        self.send_active_solo_gains();
        self.sync_null_commands();
    }

    /// Re-opens the hardware output at `target_rate` when operating in native-clock mode
    /// (`PipeWireForceRate` or `AlsaExclusive`). Falls back cleanly to existing sink if
    /// hardware refuses that rate.
    fn maybe_match_hardware_rate(&mut self, target_rate: u32, preferred_bits: Option<u32>) {
        if !self.output_mode.tracks_native_rate() {
            return;
        }
        if matches!(self.backend, AudioBackend::Null(_)) {
            return;
        }
        let target = target_rate.clamp(8_000, 384_000);
        if self.backend.sample_rate() == target {
            return;
        }
        let mode = self.output_mode.clone();
        // Release the current hardware handle before re-opening the same ALSA device at the new rate!
        let was_alsa = matches!(mode, AudioOutputMode::AlsaExclusive { .. });
        if was_alsa {
            let (tmp_null, tmp_prod) = NullAudioSink::new(target);
            self.install_backend(
                AudioBackend::Null(Box::new(tmp_null)),
                tmp_prod,
                mode.clone(),
            );
        }
        if let Ok((backend, producers)) = spawn_backend_for_mode(&mode, target, preferred_bits) {
            self.install_backend(backend, producers, mode);
        } else if was_alsa {
            // Hardware refused the new rate; fall back cleanly to PipeWire 48 kHz so playback continues.
            if let Ok((pw_sink, pw_prod)) = PipeWireSink::spawn(OUTPUT_SAMPLE_RATE) {
                self.install_backend(
                    AudioBackend::PipeWire(pw_sink),
                    pw_prod,
                    AudioOutputMode::PipeWireAuto,
                );
            }
        }
    }

    pub fn output_sample_rate(&self) -> u32 {
        self.backend.sample_rate()
    }

    pub fn active_deck(&self) -> DeckId {
        self.crossfade.active_deck
    }

    pub fn sleep_timer(&self) -> &SleepTimer {
        &self.sleep_timer
    }

    pub fn sleep_timer_mut(&mut self) -> &mut SleepTimer {
        &mut self.sleep_timer
    }

    fn deck(&self, id: DeckId) -> &DeckSlot {
        match id {
            DeckId::A => &self.deck_a,
            DeckId::B => &self.deck_b,
        }
    }

    fn deck_mut(&mut self, id: DeckId) -> &mut DeckSlot {
        match id {
            DeckId::A => &mut self.deck_a,
            DeckId::B => &mut self.deck_b,
        }
    }

    fn send_rt(&mut self, cmd: RtCommand) {
        let _ = self.cmd_tx.push(cmd);
    }

    fn sync_null_commands(&mut self) {
        if let Some(null) = self.backend.as_null_mut() {
            null.drain_commands();
        }
    }

    fn send_flush_async(&mut self, deck_id: DeckId) -> u64 {
        self.next_flush_epoch = self.next_flush_epoch.saturating_add(1);
        let epoch = self.next_flush_epoch;
        self.send_rt(RtCommand::FlushDeck {
            deck: deck_id,
            epoch,
        });
        epoch
    }

    /// Sends a monotonically tagged `FlushDeck` and waits until the RT audio thread
    /// acknowledges completing that exact epoch. If the RT thread is suspended/idle
    /// (e.g. Bluetooth TWS waking up >250 ms), arms a stale-flush guard on [`RtMetrics`]
    /// so the delayed flush can only trim old samples and NEVER wipes the incoming track.
    fn flush_deck_sync(&mut self, deck_id: DeckId) {
        let stale_samples = self
            .deck(deck_id)
            .producer
            .as_ref()
            .map_or(0, |p| RING_CAPACITY_SAMPLES.saturating_sub(p.slots()));
        let target_epoch = self.send_flush_async(deck_id);
        self.sync_null_commands();
        let deadline = Instant::now() + FLUSH_ACK_TIMEOUT;
        while self.metrics.flush_ack(deck_id) < target_epoch {
            if Instant::now() >= deadline {
                // RT thread did not wake in time; advance min_flush_epoch so when the
                // delayed FlushDeck runs later, it only trims `stale_samples` from the
                // old track and leaves any newly pushed audio untouched.
                self.next_flush_epoch = self.next_flush_epoch.saturating_add(1);
                self.metrics
                    .arm_stale_flush_guard(deck_id, self.next_flush_epoch, stale_samples);
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn send_active_solo_gains(&mut self) {
        let (gain_a, gain_b) = match self.crossfade.active_deck {
            DeckId::A => (1.0, 0.0),
            DeckId::B => (0.0, 1.0),
        };
        self.send_rt(RtCommand::SetMixGains { gain_a, gain_b });
        self.send_rt(RtCommand::SetBlendTrim(1.0));
    }

    /// Loads a local file onto the active deck (replacing any current track) and prepares playback.
    pub fn load_file(
        &mut self,
        path: &Path,
        song: Song,
        loudness_db: Option<f64>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let active = self.crossfade.active_deck;
        self.stop();
        let info = self.load_into_deck(
            active,
            AudioSourceSpec::LocalFile(path.to_path_buf()),
            song,
            loudness_db,
            Some("Local"),
        )?;
        self.send_active_solo_gains();
        self.state = PlaybackState::Paused;
        Ok(info)
    }

    /// Loads in-memory encoded audio bytes onto the active deck.
    pub fn load_bytes(
        &mut self,
        bytes: Vec<u8>,
        extension: Option<String>,
        song: Song,
        loudness_db: Option<f64>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let active = self.crossfade.active_deck;
        self.stop();
        let info = self.load_into_deck(
            active,
            AudioSourceSpec::MemoryBytes { bytes, extension },
            song,
            loudness_db,
            Some("Memory"),
        )?;
        self.send_active_solo_gains();
        self.state = PlaybackState::Paused;
        Ok(info)
    }

    /// Loads a resolved HTTP stream onto the active deck via [`ChunkedRangeSource`].
    pub fn load_stream(
        &mut self,
        http: Client,
        rt: Handle,
        stream: ResolvedStream,
        song: Song,
        on_refused: Option<RefusalCallback>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let active = self.crossfade.active_deck;
        let loudness_db = stream.loudness_db;
        let source_label = stream.client_name.clone();
        self.stop();
        let info = self.load_into_deck(
            active,
            AudioSourceSpec::HttpStream {
                http,
                rt,
                stream: Box::new(stream),
                on_refused,
            },
            song,
            loudness_db,
            Some(&source_label),
        )?;
        self.send_active_solo_gains();
        self.state = PlaybackState::Paused;
        Ok(info)
    }

    /// Loads an already-opened [`AudioDecoder`] (plus any opening packets pre-decoded
    /// off-thread) onto the active deck without blocking on HTTP probing.
    pub fn load_prepared_decoder(
        &mut self,
        decoder: AudioDecoder,
        prebuffered: Vec<DecodedPacket>,
        song: Song,
        loudness_db: Option<f64>,
        source_label: Option<&str>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let active = self.crossfade.active_deck;
        self.stop();
        let info = self.load_into_deck(
            active,
            AudioSourceSpec::PreparedDecoder {
                decoder: Box::new(decoder),
                prebuffered,
            },
            song,
            loudness_db,
            source_label,
        )?;
        self.send_active_solo_gains();
        self.state = PlaybackState::Paused;
        Ok(info)
    }

    /// Preloads an already-opened [`AudioDecoder`] (plus optional pre-decoded packets)
    /// onto the standby deck for instant gapless or crossfade transitions.
    pub fn preload_prepared_decoder(
        &mut self,
        decoder: AudioDecoder,
        prebuffered: Vec<DecodedPacket>,
        song: Song,
        loudness_db: Option<f64>,
        source_label: Option<&str>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let standby = self.crossfade.active_deck.other();
        self.load_into_deck(
            standby,
            AudioSourceSpec::PreparedDecoder {
                decoder: Box::new(decoder),
                prebuffered,
            },
            song,
            loudness_db,
            source_label,
        )
    }

    /// Waits briefly until the active deck has decoded at least `min_ms` of PCM
    /// into its `rtrb` ring (or hits EOF / error / `timeout`), preventing startup
    /// underruns when unpausing PipeWire output.
    pub fn wait_prebuffer(&self, min_ms: u64, timeout: Duration) {
        let active = self.crossfade.active_deck;
        let slot = self.deck(active);
        if !slot.is_loaded() {
            return;
        }
        let target_frames = (u64::from(self.output_sample_rate()) * min_ms) / 1_000;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if slot.pushed_frames() >= target_frames
                || slot.eof_reached()
                || slot.last_error().is_some()
            {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Preloads a local file onto the standby deck for gapless or crossfade transition.
    pub fn preload_next_file(
        &mut self,
        path: &Path,
        song: Song,
        loudness_db: Option<f64>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let standby = self.crossfade.active_deck.other();
        self.load_into_deck(
            standby,
            AudioSourceSpec::LocalFile(path.to_path_buf()),
            song,
            loudness_db,
            Some("Local"),
        )
    }

    /// Preloads in-memory encoded audio onto the standby deck for gapless or crossfade transition.
    pub fn preload_next_bytes(
        &mut self,
        bytes: Vec<u8>,
        extension: Option<String>,
        song: Song,
        loudness_db: Option<f64>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let standby = self.crossfade.active_deck.other();
        self.load_into_deck(
            standby,
            AudioSourceSpec::MemoryBytes { bytes, extension },
            song,
            loudness_db,
            Some("Memory"),
        )
    }

    /// Preloads a resolved HTTP stream onto the standby deck.
    pub fn preload_next_stream(
        &mut self,
        http: Client,
        rt: Handle,
        stream: ResolvedStream,
        song: Song,
        on_refused: Option<RefusalCallback>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        let standby = self.crossfade.active_deck.other();
        let loudness_db = stream.loudness_db;
        let source_label = stream.client_name.clone();
        self.load_into_deck(
            standby,
            AudioSourceSpec::HttpStream {
                http,
                rt,
                stream: Box::new(stream),
                on_refused,
            },
            song,
            loudness_db,
            Some(&source_label),
        )
    }

    fn load_into_deck(
        &mut self,
        deck_id: DeckId,
        mut spec: AudioSourceSpec,
        song: Song,
        loudness_db: Option<f64>,
        source_label: Option<&str>,
    ) -> Result<DecodedStreamInfo, EngineError> {
        // When loading the active deck in native-clock mode (PipeWire Force-Rate or ALSA Exclusive),
        // inspect source sample rate up front and switch the hardware clock to match bit-for-bit!
        if deck_id == self.crossfade.active_deck && self.output_mode.tracks_native_rate() {
            if !matches!(spec, AudioSourceSpec::PreparedDecoder { .. }) {
                let (dec, prebuf) = open_decoder_for_spec(spec)?;
                spec = AudioSourceSpec::PreparedDecoder {
                    decoder: Box::new(dec),
                    prebuffered: prebuf.into_iter().collect(),
                };
            }
            if let AudioSourceSpec::PreparedDecoder { ref decoder, .. } = spec {
                let target_rate = decoder.info().sample_rate_hz;
                let preferred_bits = decoder.info().bit_depth;
                self.maybe_match_hardware_rate(target_rate, preferred_bits);
            }
        }

        // Stop any existing worker on this deck and wait until the RT thread has
        // actually flushed its ring buffer so sample 0 of the new track is never dropped.
        self.deck_mut(deck_id).stop_worker();
        self.metrics.set_deck_eof(deck_id, false);
        self.flush_deck_sync(deck_id);

        let output_rate = self.output_sample_rate();
        let speed = self.speed;
        let skip_silence = self.skip_silence;
        let producer = self
            .deck_mut(deck_id)
            .producer
            .take()
            .ok_or(EngineError::WorkerUnavailable)?;

        let initial_cancel_token = match &spec {
            AudioSourceSpec::PreparedDecoder { decoder, .. } => decoder.cancel_token(),
            _ => None,
        };
        let shared = Arc::new(WorkerShared {
            cancel: AtomicBool::new(false),
            cancel_token: Mutex::new(initial_cancel_token),
            seek_target_ms: AtomicI64::new(-1),
            seek_epoch: AtomicU64::new(0),
            seek_gate: AtomicBool::new(false),
            speed_bits: AtomicU32::new(speed.to_bits()),
            skip_silence: AtomicBool::new(skip_silence),
            eof_reached: AtomicBool::new(false),
            base_position_ms: AtomicU64::new(0),
            skipped_silence_ms: AtomicU64::new(0),
            pushed_frames: AtomicU64::new(0),
            error_msg: Mutex::new(None),
        });

        let (ready_tx, ready_rx) =
            std_mpsc::sync_channel::<Result<DecodedStreamInfo, EngineError>>(1);
        let thread_name = match deck_id {
            DeckId::A => "bitchord-decode-a",
            DeckId::B => "bitchord-decode-b",
        };
        let worker_shared = Arc::clone(&shared);
        let worker_metrics = Arc::clone(&self.metrics);
        let join = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                run_decode_worker(
                    spec,
                    deck_id,
                    output_rate,
                    producer,
                    worker_shared,
                    worker_metrics,
                    ready_tx,
                )
            })?;

        let info = match ready_rx.recv() {
            Ok(Ok(info)) => info,
            Ok(Err(err)) => {
                if let Ok(p) = join.join() {
                    self.deck_mut(deck_id).producer = Some(p);
                }
                return Err(err);
            }
            Err(_) => {
                if let Ok(p) = join.join() {
                    self.deck_mut(deck_id).producer = Some(p);
                }
                return Err(EngineError::WorkerUnavailable);
            }
        };

        let gain = loudness_db_to_linear_gain(loudness_db);
        let nerd_stats = info.to_nerd_stats(source_label);

        let slot = self.deck_mut(deck_id);
        slot.worker = Some(DeckWorker { shared, join });
        slot.song = Some(song);
        slot.stream_info = Some(info.clone());
        slot.nerd_stats = Some(nerd_stats);
        slot.loudness_gain = gain;

        self.send_rt(RtCommand::SetLoudnessGain {
            deck: deck_id,
            gain,
        });
        if deck_id != self.crossfade.active_deck {
            self.send_rt(RtCommand::SetNextLoudnessGain {
                deck: self.crossfade.active_deck,
                gain: Some(gain),
            });
        }
        self.sync_null_commands();

        Ok(info)
    }

    pub fn play(&mut self) {
        let active = self.crossfade.active_deck;
        if !self.deck(active).is_loaded() {
            return;
        }
        if self.state == PlaybackState::Error {
            // Attempt clean recovery from the last known position so pressing Play
            // after a transient network glitch resumes instead of bouncing back to Error.
            let _ = self.retry_after_error();
        }
        self.send_rt(RtCommand::SetPaused(false));
        self.sync_null_commands();
        self.state = PlaybackState::Playing;
    }

    /// Clears the active deck's error state and seeks its decode worker back to
    /// the last known playback position so streaming/decoding can resume cleanly.
    pub fn retry_after_error(&mut self) -> Result<u64, EngineError> {
        let active = self.crossfade.active_deck;
        if !self.deck(active).is_loaded() {
            return Ok(0);
        }
        let resume_ms = self.position_ms();
        if let Some(ref w) = self.deck(active).worker {
            *w.shared.error_msg.lock().unwrap_or_else(|e| e.into_inner()) = None;
            w.shared.eof_reached.store(false, Ordering::Release);
        }
        self.metrics.set_deck_eof(active, false);
        let landed = self.seek_deck_ms(active, resume_ms)?;
        if self.state == PlaybackState::Error {
            self.state = PlaybackState::Paused;
        }
        Ok(landed)
    }

    /// Returns `true` if the standby deck already holds a preloaded track matching `video_id`.
    pub fn has_standby_for(&self, video_id: &str) -> bool {
        let standby = self.crossfade.active_deck.other();
        self.deck(standby).is_loaded()
            && self
                .deck(standby)
                .song
                .as_ref()
                .is_some_and(|s| s.video_id == video_id)
    }

    /// Returns the song currently preloaded on the standby deck, if any.
    pub fn standby_song(&self) -> Option<&Song> {
        let standby = self.crossfade.active_deck.other();
        if self.deck(standby).is_loaded() {
            self.deck(standby).song.as_ref()
        } else {
            None
        }
    }

    /// Stops and clears any preloaded standby deck (call whenever queue order,
    /// shuffle, or repeat mode changes so stale preloads never fire on gapless EOF).
    pub fn clear_standby(&mut self) {
        let standby = self.crossfade.active_deck.other();
        self.planned_transition = None;
        if self.crossfade.is_transitioning() {
            self.crossfade.phase = CrossfadePhase::Idle;
            self.active_transition = None;
            self.send_rt(RtCommand::OpenTransition(DeckId::A));
            self.send_rt(RtCommand::OpenTransition(DeckId::B));
            self.send_active_solo_gains();
        }
        self.deck_mut(standby).stop_worker();
        self.metrics.set_deck_eof(standby, false);
        self.send_flush_async(standby);
        self.send_rt(RtCommand::SetNextLoudnessGain {
            deck: self.crossfade.active_deck,
            gain: None,
        });
        self.sync_null_commands();
    }

    /// Instantly promotes the preloaded standby deck to active playback (e.g. on
    /// manual Next Track) without tearing down and re-resolving the stream.
    /// Returns `false` if no standby track is loaded.
    pub fn activate_standby_now(&mut self) -> bool {
        let old_active = self.crossfade.active_deck;
        let standby = old_active.other();
        if !self.deck(standby).is_loaded() {
            return false;
        }
        self.crossfade.phase = CrossfadePhase::Idle;
        self.crossfade.handed_off = false;
        self.planned_transition = None;
        self.active_transition = None;
        self.crossfade.active_deck = standby;

        self.send_rt(RtCommand::OpenTransition(DeckId::A));
        self.send_rt(RtCommand::OpenTransition(DeckId::B));
        self.send_rt(RtCommand::StreamBoundary(standby));
        self.send_active_solo_gains();

        self.deck_mut(old_active).stop_worker();
        self.metrics.set_deck_eof(old_active, false);
        self.send_flush_async(old_active);
        self.deck(standby)
            .apply_dynamic_params(self.speed, self.skip_silence);
        self.send_rt(RtCommand::SetPaused(false));
        self.sync_null_commands();
        self.state = PlaybackState::Playing;
        true
    }

    pub fn pause(&mut self) {
        self.send_rt(RtCommand::SetPaused(true));
        self.sync_null_commands();
        if self.state == PlaybackState::Playing || self.state == PlaybackState::Buffering {
            self.state = PlaybackState::Paused;
        }
    }

    pub fn toggle_play_pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn stop(&mut self) {
        self.send_rt(RtCommand::SetPaused(true));
        self.deck_a.stop_worker();
        self.deck_b.stop_worker();
        self.send_flush_async(DeckId::A);
        self.send_flush_async(DeckId::B);
        self.send_rt(RtCommand::OpenTransition(DeckId::A));
        self.send_rt(RtCommand::OpenTransition(DeckId::B));
        self.crossfade.phase = CrossfadePhase::Idle;
        self.crossfade.handed_off = false;
        self.planned_transition = None;
        self.active_transition = None;
        self.send_active_solo_gains();
        self.sync_null_commands();
        self.state = PlaybackState::Stopped;
    }

    /// Arms or clears an Automix / LoopMix [`TransitionPlan`] for the preloaded standby deck.
    /// When the standby deck is already loaded and the plan specifies a non-zero `incoming_cue_sec`,
    /// seeks the standby deck cleanly to that cue point before the blend triggers.
    pub fn set_transition_plan(&mut self, plan: Option<TransitionPlan>) {
        let valid = plan.filter(TransitionPlan::is_active);
        if let Some(ref p) = valid {
            let standby = self.crossfade.active_deck.other();
            if self.deck(standby).is_loaded() {
                if p.incoming_cue_sec > 0.25 {
                    let cue_ms = (p.incoming_cue_sec * 1_000.0).round() as u64;
                    let _ = self.seek_deck_ms(standby, cue_ms);
                }
                let rate = (self.speed * p.incoming_playback_rate).clamp(MIN_SPEED, MAX_SPEED);
                self.deck(standby)
                    .apply_dynamic_params(rate, self.skip_silence);
            }
        }
        self.planned_transition = valid;
    }

    pub fn transition_plan(&self) -> Option<&TransitionPlan> {
        self.active_transition
            .as_ref()
            .or(self.planned_transition.as_ref())
    }

    fn seek_deck_ms(&mut self, deck: DeckId, target_ms: u64) -> Result<u64, EngineError> {
        let Some(shared) = self
            .deck(deck)
            .worker
            .as_ref()
            .map(|w| Arc::clone(&w.shared))
        else {
            return Ok(0);
        };

        let prev_epoch = shared.seek_epoch.load(Ordering::Acquire);
        shared.seek_gate.store(true, Ordering::Release);
        shared
            .seek_target_ms
            .store(target_ms.min(i64::MAX as u64) as i64, Ordering::Release);

        let deadline = Instant::now() + SEEK_HANDSHAKE_TIMEOUT;
        while shared.seek_epoch.load(Ordering::Acquire) == prev_epoch {
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        self.flush_deck_sync(deck);
        shared.seek_gate.store(false, Ordering::Release);
        Ok(shared.base_position_ms.load(Ordering::Acquire))
    }

    /// Seeks the active deck cleanly to `target_ms`, flushing stale ring buffer frames.
    pub fn seek_ms(&mut self, target_ms: u64) -> Result<u64, EngineError> {
        let active = self.crossfade.active_deck;
        if self.crossfade.is_transitioning() {
            self.crossfade.phase = CrossfadePhase::Idle;
            self.active_transition = None;
            self.send_rt(RtCommand::OpenTransition(DeckId::A));
            self.send_rt(RtCommand::OpenTransition(DeckId::B));
            self.deck_mut(active.other()).stop_worker();
            self.send_flush_async(active.other());
            self.send_active_solo_gains();
        }

        let landed = self.seek_deck_ms(active, target_ms)?;
        if self.state == PlaybackState::Ended {
            self.state = PlaybackState::Paused;
        }
        Ok(landed)
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.send_rt(RtCommand::SetVolume(self.volume));
        self.sync_null_commands();
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.send_rt(RtCommand::SetMuted(muted));
        self.sync_null_commands();
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = if speed.is_finite() {
            speed.clamp(MIN_SPEED, MAX_SPEED)
        } else {
            1.0
        };
        self.deck_a
            .apply_dynamic_params(self.speed, self.skip_silence);
        self.deck_b
            .apply_dynamic_params(self.speed, self.skip_silence);
    }

    pub fn set_skip_silence(&mut self, enabled: bool) {
        self.skip_silence = enabled;
        self.deck_a
            .apply_dynamic_params(self.speed, self.skip_silence);
        self.deck_b
            .apply_dynamic_params(self.speed, self.skip_silence);
    }

    pub fn set_spatial_audio(&mut self, enabled: bool) {
        self.spatial_audio = enabled;
        self.send_rt(RtCommand::SetSpatial(enabled));
        self.sync_null_commands();
    }

    pub fn set_eq(&mut self, snapshot: EqTuningSnapshot) {
        self.eq_snapshot = snapshot;
        self.send_rt(RtCommand::SetEq(snapshot));
        self.sync_null_commands();
    }

    pub fn set_crossfade_seconds(&mut self, seconds: u32) {
        self.crossfade.set_crossfade_seconds(seconds);
    }

    /// Computes current playhead position in milliseconds for the active deck.
    pub fn position_ms(&self) -> u64 {
        let active = self.crossfade.active_deck;
        let slot = self.deck(active);
        let base_ms = slot.base_position_ms();
        let played_frames = self.metrics.played_frames(active);
        let out_rate = u64::from(self.output_sample_rate().max(1));
        let played_out_ms = (played_frames as f64 * 1_000.0) / out_rate as f64;
        let media_delta_ms = (played_out_ms * f64::from(self.speed)).round() as u64;
        let skipped_ms = slot.skipped_silence_ms();
        let raw = base_ms
            .saturating_add(media_delta_ms)
            .saturating_add(skipped_ms);
        let dur = self.duration_ms();
        if dur > 0 { raw.min(dur) } else { raw }
    }

    pub fn duration_ms(&self) -> u64 {
        let active = self.crossfade.active_deck;
        self.deck(active)
            .stream_info
            .as_ref()
            .and_then(|i| i.duration_ms)
            .filter(|&d| d > 0)
            .or_else(|| {
                self.deck(active)
                    .song
                    .as_ref()
                    .map(|s| duration_millis(s.duration_text.as_deref()))
                    .filter(|&d| d > 0)
            })
            .unwrap_or(0)
    }

    /// Advances crossfade transitions, gapless handoffs, output health fallback,
    /// rebuffering state, and the sleep timer by `delta_ms`.
    pub fn tick(&mut self, delta_ms: u64) {
        // 0. If an ALSA exclusive USB DAC was unplugged, fall back cleanly to PipeWire
        //    instead of spinning in a silent progress loop.
        if matches!(self.backend, AudioBackend::AlsaExclusive(_))
            && self.metrics.needs_reconnect()
        {
            let notice = self
                .metrics
                .stream_error()
                .unwrap_or_else(|| "ALSA device disconnected; switched to PipeWire".to_owned());
            if let Ok((pw_sink, pw_prod)) = PipeWireSink::spawn(OUTPUT_SAMPLE_RATE) {
                self.install_backend(
                    AudioBackend::PipeWire(pw_sink),
                    pw_prod,
                    AudioOutputMode::PipeWireAuto,
                );
            } else {
                self.stop();
            }
            self.last_output_notice = Some(notice);
            self.state = PlaybackState::Paused;
            return;
        }

        // 1. Poll monotonic sleep timer deadline.
        if self.sleep_timer.poll_expired(Instant::now()) {
            if self.state == PlaybackState::Playing || self.state == PlaybackState::Buffering {
                self.crossfade_outgoing = self.crossfade.active_deck;
                self.crossfade.begin_sleep_fade(SLEEP_FADE_MS);
            } else {
                self.pause();
            }
        }

        if self.state != PlaybackState::Playing && self.state != PlaybackState::Buffering {
            return;
        }

        // Reflect low-watermark rebuffering transitions cleanly in PlaybackState.
        let is_rebuffering = self.metrics.is_rebuffering();
        if is_rebuffering && self.state == PlaybackState::Playing {
            self.state = PlaybackState::Buffering;
        } else if !is_rebuffering && self.state == PlaybackState::Buffering {
            self.state = PlaybackState::Playing;
        }

        let active = self.crossfade.active_deck;
        let standby = active.other();

        // 2. Arm Automix TransitionPlan (if armed) or standard equal-power crossfade.
        if self.crossfade.phase == CrossfadePhase::Idle && self.deck(standby).is_loaded() {
            let pos = self.position_ms();
            let dur = self.duration_ms();
            if let Some(plan) = self.planned_transition.as_ref()
                && plan.is_active()
                && pos >= (plan.transition_start_sec * 1_000.0).round() as u64
            {
                let plan = self.planned_transition.take().expect("checked above");
                let rate = (self.speed * plan.incoming_playback_rate).clamp(MIN_SPEED, MAX_SPEED);
                self.deck(standby)
                    .apply_dynamic_params(rate, self.skip_silence);
                self.crossfade_outgoing = active;
                let fade_ms = ((plan.duration_sec * 1_000.0).round() as u64).max(500);
                self.crossfade.begin_fade(fade_ms);
                self.active_transition = Some(plan);
            } else if self.planned_transition.is_none()
                && self.crossfade.should_start_crossfade(pos, dur)
            {
                self.crossfade_outgoing = active;
                let fade_ms = u64::from(self.crossfade.crossfade_seconds) * 1_000;
                self.crossfade.begin_fade(fade_ms);
            }
        }

        // 3. Advance active transition (Automix / Crossfade / SleepFade / Bailing).
        if self.crossfade.is_transitioning() {
            let prev_phase = self.crossfade.phase;
            let (gains, _handed_off, finished) = self.crossfade.advance(delta_ms);
            let progress = self.crossfade.progress();
            let out_deck = self.crossfade_outgoing;
            let in_deck = out_deck.other();
            let (gain_a, gain_b) = match out_deck {
                DeckId::A => (gains.outgoing, gains.incoming),
                DeckId::B => (gains.incoming, gains.outgoing),
            };
            self.send_rt(RtCommand::SetMixGains { gain_a, gain_b });
            self.send_rt(RtCommand::SetBlendTrim(gains.blend_trim));

            let plan_step = self.active_transition.as_ref().map(|plan| {
                (
                    plan.filter_cutoffs_at(progress),
                    plan.incoming_playback_rate,
                )
            });
            if let Some(((out_lp, out_hp, in_lp, in_hp), incoming_rate)) = plan_step {
                self.send_rt(RtCommand::SetTransitionCutoffs {
                    deck: out_deck,
                    lowpass_hz: out_lp,
                    highpass_hz: out_hp,
                });
                self.send_rt(RtCommand::SetTransitionCutoffs {
                    deck: in_deck,
                    lowpass_hz: in_lp,
                    highpass_hz: in_hp,
                });
                // Smoothly relax incoming beatmatch stretch back to native speed over final 25%.
                if progress > 0.75 && (incoming_rate - 1.0).abs() > 0.002 {
                    let relax = ((progress - 0.75) / 0.25).clamp(0.0, 1.0);
                    let cur_mult = incoming_rate + (1.0 - incoming_rate) * relax;
                    let rate = (self.speed * cur_mult).clamp(MIN_SPEED, MAX_SPEED);
                    self.deck(in_deck)
                        .apply_dynamic_params(rate, self.skip_silence);
                }
            }

            if finished {
                self.active_transition = None;
                self.send_rt(RtCommand::OpenTransition(DeckId::A));
                self.send_rt(RtCommand::OpenTransition(DeckId::B));
                let cur_active = self.crossfade.active_deck;
                self.deck(cur_active)
                    .apply_dynamic_params(self.speed, self.skip_silence);
                match prev_phase {
                    CrossfadePhase::SleepFade => {
                        self.pause();
                        self.send_active_solo_gains();
                    }
                    CrossfadePhase::Fading => {
                        let outgoing = self.crossfade_outgoing;
                        self.deck_mut(outgoing).stop_worker();
                        self.send_flush_async(outgoing);
                        self.send_active_solo_gains();
                    }
                    _ => {
                        self.send_active_solo_gains();
                    }
                }
            }
            self.sync_null_commands();
            return;
        }

        // 4. Check clean active deck EOF (all pushed frames rendered).
        let active_slot = self.deck(active);
        let eof = active_slot.eof_reached();
        let pushed = active_slot.pushed_frames();
        let played = self.metrics.played_frames(active);
        if eof && played >= pushed {
            if let Some(_err) = active_slot.last_error() {
                self.pause();
                self.state = PlaybackState::Error;
                return;
            }
            if self.sleep_timer.take_after_track() {
                self.pause();
                return;
            }
            if self.deck(standby).is_loaded() {
                // Gapless handoff to preloaded standby deck!
                self.crossfade.active_deck = standby;
                self.send_rt(RtCommand::StreamBoundary(standby));
                self.send_active_solo_gains();
                self.deck_mut(active).stop_worker();
                self.send_flush_async(active);
                self.sync_null_commands();
            } else {
                self.send_rt(RtCommand::SetPaused(true));
                self.sync_null_commands();
                self.state = PlaybackState::Ended;
            }
        }
    }

    /// Captures a complete observable [`PlaybackSnapshot`].
    pub fn snapshot(&self) -> PlaybackSnapshot {
        let active = self.crossfade.active_deck;
        let slot = self.deck(active);
        PlaybackSnapshot {
            state: self.state,
            current_song: slot.song.clone(),
            position_ms: self.position_ms(),
            duration_ms: self.duration_ms(),
            volume: self.volume,
            muted: self.muted,
            speed: self.speed,
            skip_silence: self.skip_silence,
            spatial_audio: self.spatial_audio,
            eq_enabled: self.eq_snapshot.enabled,
            crossfade_seconds: self.crossfade.crossfade_seconds,
            crossfade_phase: self.crossfade.phase,
            active_deck: active,
            nerd_stats: slot.nerd_stats.clone(),
            sleep_timer_remaining_ms: self.sleep_timer.remaining_ms(),
            sleep_after_track: self.sleep_timer.after_track(),
            underruns: self.metrics.underruns(),
            peak_level: self.metrics.peak_level(),
            skipped_silence_ms: slot.skipped_silence_ms(),
            last_error: slot.last_error(),
            output_mode: self.output_mode.clone(),
            output_device_label: self.backend.device_label(),
            output_format_label: self.backend.format_label().to_owned(),
            output_sample_rate_hz: self.backend.sample_rate(),
            bit_perfect_active: self.is_bit_perfect_active(slot),
            automix_plan_summary: self.transition_plan().map(|p| p.summary.clone()),
            output_health: self.output_health(),
            needs_output_reconnect: self.needs_output_reconnect(),
            output_error: self.output_error(),
        }
    }

    /// Honest bit-perfect audit: true ONLY when native-clock output matches source rate
    /// with zero resampling, zero EQ/Spatial/speed/silence processing, and unity gain.
    fn is_bit_perfect_active(&self, slot: &DeckSlot) -> bool {
        if !self.backend.supports_bit_perfect() {
            return false;
        }
        let Some(ref info) = slot.stream_info else {
            return false;
        };
        let same_rate = self.backend.sample_rate() == info.sample_rate_hz;
        let dsp_bypassed = !self.eq_snapshot.enabled
            && !self.spatial_audio
            && !self.skip_silence
            && (self.speed - 1.0).abs() < 1e-4
            && !self.muted
            && (self.volume - 1.0).abs() < 1e-4
            && (slot.loudness_gain - 1.0).abs() < 1e-4
            && self.crossfade.phase == CrossfadePhase::Idle;
        same_rate && dsp_bypassed
    }

    /// Renders `frames` stereo frames synchronously when backed by [`NullAudioSink`].
    pub fn render_null_frames(&mut self, frames: usize) -> Result<Vec<f32>, EngineError> {
        let null = self
            .backend
            .as_null_mut()
            .ok_or(EngineError::NotNullBackend)?;
        Ok(null.render_frames(frames))
    }

    /// Runs a headless [`NullAudioSink`] render loop to EOF (or `max_seconds`), exercising
    /// the full decode -> SkipSilence -> SonicStretcher -> Rubato -> rtrb -> RT DspChain path.
    pub fn drain_to_eof_null(
        &mut self,
        max_seconds: Option<f64>,
    ) -> Result<HeadlessPlaybackReport, EngineError> {
        if self.backend.as_null_mut().is_none() {
            return Err(EngineError::NotNullBackend);
        }
        self.play();
        let out_rate = self.output_sample_rate();
        let max_frames = max_seconds
            .map(|s| (s.max(0.0) * f64::from(out_rate)).round() as u64)
            .unwrap_or(u64::MAX);

        let block_frames = 1_024usize;
        let block_ms = ((block_frames as u64 * 1_000) / u64::from(out_rate.max(1))).max(1);
        let mut rendered_frames = 0u64;
        let mut peak = 0.0_f32;
        let mut sum_sq = 0.0_f64;
        let mut total_samples = 0u64;
        let mut wait_spins = 0usize;

        while rendered_frames < max_frames
            && matches!(self.state, PlaybackState::Playing | PlaybackState::Buffering)
        {
            let active = self.crossfade.active_deck;
            let pushed = self.deck(active).pushed_frames();
            let played = self.metrics.played_frames(active);
            let eof = self.deck(active).eof_reached();
            let buffered = pushed.saturating_sub(played);

            if buffered == 0 && !eof {
                wait_spins += 1;
                if wait_spins > 5_000 {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            wait_spins = 0;

            let to_render = (buffered as usize).min(block_frames);

            if to_render > 0 {
                let remaining_cap = (max_frames - rendered_frames) as usize;
                let step = to_render.min(remaining_cap);
                let pcm = self.render_null_frames(step)?;
                for &s in &pcm {
                    let mag = s.abs();
                    if mag > peak {
                        peak = mag;
                    }
                    sum_sq += f64::from(s) * f64::from(s);
                }
                total_samples += pcm.len() as u64;
                rendered_frames += step as u64;
            }
            self.tick(block_ms);
        }

        let snap = self.snapshot();
        let rms = if total_samples > 0 {
            (sum_sq / total_samples as f64).sqrt() as f32
        } else {
            0.0
        };
        let rendered_duration_ms = (rendered_frames * 1_000) / u64::from(out_rate.max(1));

        Ok(HeadlessPlaybackReport {
            song: snap.current_song,
            output_sample_rate_hz: out_rate,
            rendered_frames,
            rendered_duration_ms,
            peak_level: peak,
            rms_level: rms,
            underruns: snap.underruns,
            skipped_silence_ms: snap.skipped_silence_ms,
            nerd_stats: snap.nerd_stats,
            final_state: snap.state,
        })
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_backend_for_mode(
    mode: &AudioOutputMode,
    sample_rate: u32,
    preferred_bits: Option<u32>,
) -> Result<(AudioBackend, SinkProducers), EngineError> {
    match mode {
        AudioOutputMode::PipeWireAuto => {
            let (sink, producers) = PipeWireSink::spawn_with_force_rate(OUTPUT_SAMPLE_RATE, false)?;
            Ok((AudioBackend::PipeWire(sink), producers))
        }
        AudioOutputMode::PipeWireForceRate => {
            let (sink, producers) = PipeWireSink::spawn_with_force_rate(sample_rate, true)?;
            Ok((AudioBackend::PipeWire(sink), producers))
        }
        AudioOutputMode::AlsaExclusive { device } => {
            let (sink, producers) = AlsaExclusiveSink::spawn(device, sample_rate, preferred_bits)?;
            Ok((AudioBackend::AlsaExclusive(sink), producers))
        }
    }
}

/// Opens a [`ChunkedRangeSource`]-backed [`AudioDecoder`] for a resolved HTTP stream
/// off-thread so the caller can hand the ready decoder to [`AudioEngine::load_prepared_decoder`].
pub fn open_http_decoder(
    http: Client,
    rt: Handle,
    stream: &ResolvedStream,
    on_refused: Option<RefusalCallback>,
) -> Result<AudioDecoder, EngineError> {
    open_http_decoder_with_cancel(http, rt, stream, on_refused, StreamCancelToken::new())
}

/// Opens a [`ChunkedRangeSource`]-backed [`AudioDecoder`] attached to a caller-provided
/// [`StreamCancelToken`] so stalled HTTP fetches can be aborted immediately.
pub fn open_http_decoder_with_cancel(
    http: Client,
    rt: Handle,
    stream: &ResolvedStream,
    on_refused: Option<RefusalCallback>,
    cancel: StreamCancelToken,
) -> Result<AudioDecoder, EngineError> {
    let mut hint = Hint::new();
    if !stream.ext.is_empty() {
        hint.with_extension(&stream.ext);
    } else if let Some(ext) = mime_to_extension(&stream.mime_type) {
        hint.with_extension(ext);
    }
    let source = ChunkedRangeSource::from_resolved_with_cancel(http, rt, stream, on_refused, cancel)?;
    let cancel_token = source.cancel_token();
    let mut decoder = AudioDecoder::open_source(Box::new(source), hint)?;
    decoder.attach_cancel_token(cancel_token);
    let fallback_dur = stream
        .duration_ms
        .filter(|&d| d > 0)
        .or_else(|| PlayerClient::duration_ms_from_url(&stream.url));
    decoder.apply_fallback_duration_ms(fallback_dur);
    decoder.estimate_duration_from_bitrate_kbps(stream.kbps);
    Ok(decoder)
}

fn open_decoder_for_spec(
    spec: AudioSourceSpec,
) -> Result<(AudioDecoder, VecDeque<DecodedPacket>), EngineError> {
    open_decoder_for_spec_with_cancel(spec, None)
}

fn open_decoder_for_spec_with_cancel(
    spec: AudioSourceSpec,
    shared: Option<&WorkerShared>,
) -> Result<(AudioDecoder, VecDeque<DecodedPacket>), EngineError> {
    match spec {
        AudioSourceSpec::LocalFile(path) => Ok((AudioDecoder::open_path(&path)?, VecDeque::new())),
        AudioSourceSpec::MemoryBytes { bytes, extension } => {
            let mut hint = Hint::new();
            if let Some(ext) = extension {
                hint.with_extension(&ext);
            }
            Ok((
                AudioDecoder::open_source(Box::new(Cursor::new(bytes)), hint)?,
                VecDeque::new(),
            ))
        }
        AudioSourceSpec::HttpStream {
            http,
            rt,
            stream,
            on_refused,
        } => {
            let cancel = StreamCancelToken::new();
            if let Some(sh) = shared {
                sh.set_cancel_token(Some(cancel.clone()));
            }
            Ok((
                open_http_decoder_with_cancel(http, rt, &stream, on_refused, cancel)?,
                VecDeque::new(),
            ))
        }
        AudioSourceSpec::PreparedDecoder {
            decoder,
            prebuffered,
        } => {
            if let Some(sh) = shared {
                sh.set_cancel_token(decoder.cancel_token());
            }
            Ok((*decoder, VecDeque::from(prebuffered)))
        }
    }
}

fn mime_to_extension(mime: &str) -> Option<&'static str> {
    let lower = mime.to_ascii_lowercase();
    if lower.contains("webm") || lower.contains("opus") {
        Some("webm")
    } else if lower.contains("mp4") || lower.contains("m4a") || lower.contains("aac") {
        Some("m4a")
    } else if lower.contains("flac") {
        Some("flac")
    } else if lower.contains("ogg") || lower.contains("vorbis") {
        Some("ogg")
    } else if lower.contains("mpeg") || lower.contains("mp3") {
        Some("mp3")
    } else if lower.contains("wav") {
        Some("wav")
    } else if lower.contains("dsf") {
        Some("dsf")
    } else if lower.contains("dff") || lower.contains("dsdiff") {
        Some("dff")
    } else {
        None
    }
}

fn run_decode_worker(
    spec: AudioSourceSpec,
    deck_id: DeckId,
    output_rate: u32,
    mut producer: Producer<f32>,
    shared: Arc<WorkerShared>,
    metrics: Arc<RtMetrics>,
    ready_tx: std_mpsc::SyncSender<Result<DecodedStreamInfo, EngineError>>,
) -> Producer<f32> {
    let (mut decoder, mut prebuffered) = match open_decoder_for_spec_with_cancel(spec, Some(&shared))
    {
        Ok(pair) => pair,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return producer;
        }
    };
    if let Some(tok) = decoder.cancel_token() {
        shared.set_cancel_token(Some(tok));
    }

    let info = decoder.info().clone();
    let mut resampler = match AudioResampler::new(info.sample_rate_hz, output_rate, OUTPUT_CHANNELS)
    {
        Ok(r) => r,
        Err(err) => {
            let _ = ready_tx.send(Err(EngineError::Resampler(err)));
            return producer;
        }
    };

    let mut skip_proc = SkipSilenceProcessor::new();
    skip_proc.configure(info.sample_rate_hz, OUTPUT_CHANNELS);
    skip_proc.enabled = shared.skip_silence.load(Ordering::Relaxed);

    let mut stretcher = SonicStretcher::new();
    stretcher.configure(info.sample_rate_hz, OUTPUT_CHANNELS);
    stretcher.set_speed(f32::from_bits(shared.speed_bits.load(Ordering::Relaxed)));

    if ready_tx.send(Ok(info)).is_err() {
        return producer;
    }

    let mut skip_buf = Vec::with_capacity(8_192);
    let mut stretch_buf = Vec::with_capacity(8_192);
    let mut out_buf = Vec::with_capacity(8_192);
    let mut eof_flushed = false;

    while !shared.cancel.load(Ordering::Acquire) {
        // 1. Handle seek requests immediately, cooperating with seek_gate for clean flush.
        let pending_seek = shared.seek_target_ms.swap(-1, Ordering::AcqRel);
        if pending_seek >= 0 {
            let target = pending_seek as u64;
            prebuffered.clear();
            match decoder.seek_ms(target) {
                Ok(actual_ms) => {
                    skip_proc.reset();
                    stretcher.reset();
                    resampler.reset();
                    *shared.error_msg.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    shared.base_position_ms.store(actual_ms, Ordering::Release);
                    shared.skipped_silence_ms.store(0, Ordering::Relaxed);
                    shared.pushed_frames.store(0, Ordering::Release);
                    shared.eof_reached.store(false, Ordering::Release);
                    metrics.set_deck_eof(deck_id, false);
                    eof_flushed = false;
                }
                Err(err) => {
                    *shared.error_msg.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(err.to_string());
                }
            }
            shared.seek_epoch.fetch_add(1, Ordering::Release);
            while shared.seek_gate.load(Ordering::Acquire) && !shared.cancel.load(Ordering::Acquire)
            {
                thread::sleep(Duration::from_millis(1));
            }
            continue;
        }

        if shared.seek_gate.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        if eof_flushed {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        // 2. Refresh dynamic playback speed & skip-silence flags.
        skip_proc.enabled = shared.skip_silence.load(Ordering::Relaxed);
        stretcher.set_speed(f32::from_bits(shared.speed_bits.load(Ordering::Relaxed)));

        // 3. Drain pre-decoded opening packets first (zero network latency!), then decode live.
        let next_pkt = if let Some(pkt) = prebuffered.pop_front() {
            Ok(Some(pkt))
        } else {
            decoder.next_packet()
        };

        match next_pkt {
            Ok(Some(pkt)) => {
                skip_proc.process(&pkt.samples, &mut skip_buf);
                shared
                    .skipped_silence_ms
                    .store(skip_proc.skipped_duration_ms(), Ordering::Relaxed);
                stretcher.process(&skip_buf, &mut stretch_buf);
                if let Err(err) = resampler.process(&stretch_buf, &mut out_buf) {
                    *shared.error_msg.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(err.to_string());
                    shared.eof_reached.store(true, Ordering::Release);
                    metrics.set_deck_eof(deck_id, true);
                    eof_flushed = true;
                    continue;
                }
                push_samples_interruptible(&mut producer, &out_buf, &shared);
            }
            Ok(None) => {
                stretcher.flush_eof(&mut stretch_buf);
                if !stretch_buf.is_empty() && resampler.process(&stretch_buf, &mut out_buf).is_ok()
                {
                    push_samples_interruptible(&mut producer, &out_buf, &shared);
                }
                if resampler.flush_eof(&mut out_buf).is_ok() && !out_buf.is_empty() {
                    push_samples_interruptible(&mut producer, &out_buf, &shared);
                }
                shared.eof_reached.store(true, Ordering::Release);
                metrics.set_deck_eof(deck_id, true);
                eof_flushed = true;
            }
            Err(err) => {
                *shared.error_msg.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
                shared.eof_reached.store(true, Ordering::Release);
                metrics.set_deck_eof(deck_id, true);
                eof_flushed = true;
            }
        }
    }

    producer
}

fn push_samples_interruptible(
    producer: &mut Producer<f32>,
    samples: &[f32],
    shared: &WorkerShared,
) {
    let mut written = 0usize;
    while written < samples.len() {
        if shared.cancel.load(Ordering::Acquire)
            || shared.seek_target_ms.load(Ordering::Acquire) >= 0
            || shared.seek_gate.load(Ordering::Acquire)
        {
            return;
        }
        let avail = producer.slots();
        // Keep writes aligned to stereo frame pairs so ring never splits L/R.
        let to_write = (avail.min(samples.len() - written) / OUTPUT_CHANNELS) * OUTPUT_CHANNELS;
        if to_write == 0 {
            thread::sleep(WORKER_IDLE_SLEEP);
            continue;
        }
        if let Ok(mut chunk) = producer.write_chunk(to_write) {
            let (first, second) = chunk.as_mut_slices();
            let n1 = first.len();
            first.copy_from_slice(&samples[written..written + n1]);
            let n2 = second.len();
            if n2 > 0 {
                second.copy_from_slice(&samples[written + n1..written + n1 + n2]);
            }
            chunk.commit_all();
            written += n1 + n2;
            shared
                .pushed_frames
                .fetch_add(((n1 + n2) / OUTPUT_CHANNELS) as u64, Ordering::Release);
        } else {
            thread::sleep(WORKER_IDLE_SLEEP);
        }
    }
}

#[cfg(test)]
mod tests {
    use bitchord_dsp_core::{EqCurve, EqLayout};

    use super::*;
    use crate::decoder::build_test_wav_pcm16;

    #[test]
    fn headless_engine_decodes_44100_wav_resamples_to_48k_runs_dsp_and_seeks() {
        let wav = build_test_wav_pcm16(44_100, 440.0, 600);
        let song = Song::new("test_wav_1", "Test Tone", "BitChord", None);

        let mut engine = AudioEngine::with_null_sink();
        let info = engine
            .load_bytes(wav, Some("wav".to_owned()), song, Some(-14.0))
            .expect("load wav");
        assert_eq!(info.sample_rate_hz, 44_100);
        assert_eq!(info.bit_depth, Some(16));

        // Enable EQ + Spatial to exercise full RT DspChain.
        let mut gains = [0.0; EqLayout::SLOTS];
        gains[2] = 3.0;
        engine.set_eq(EqTuningSnapshot::from_curve(
            true,
            &EqCurve {
                gains_db: gains,
                qs: [0.707; EqLayout::SLOTS],
                preamp_db: -1.0,
            },
            0.0,
        ));
        engine.set_spatial_audio(true);

        let report = engine
            .drain_to_eof_null(None)
            .expect("drain headless playback");
        assert_eq!(report.output_sample_rate_hz, 48_000);
        assert_eq!(report.final_state, PlaybackState::Ended);
        assert!(
            report.rendered_duration_ms >= 550 && report.rendered_duration_ms <= 650,
            "duration_ms={}",
            report.rendered_duration_ms
        );
        assert!(report.peak_level > 0.1 && report.peak_level <= 1.0);
        assert!(report.rms_level > 0.05);
        assert_eq!(report.underruns, 0);

        let stats = report.nerd_stats.expect("nerd stats");
        assert_eq!(stats.sample_rate_hz, Some(44_100));
        assert_eq!(stats.bit_depth, Some(16));

        // Seek back to 300 ms and drain second half (~300 ms).
        let actual_seek = engine.seek_ms(300).expect("seek to 300ms");
        assert!(actual_seek <= 320, "actual_seek={actual_seek}");
        let second_half = engine.drain_to_eof_null(None).expect("drain after seek");
        assert!(
            second_half.rendered_duration_ms >= 250 && second_half.rendered_duration_ms <= 350,
            "second_half_ms={}",
            second_half.rendered_duration_ms
        );
    }

    #[test]
    fn headless_engine_performs_gapless_handoff_to_preloaded_standby_deck() {
        let wav_a = build_test_wav_pcm16(48_000, 440.0, 250);
        let wav_b = build_test_wav_pcm16(48_000, 660.0, 250);

        let mut engine = AudioEngine::with_null_sink();
        engine
            .load_bytes(
                wav_a,
                Some("wav".to_owned()),
                Song::new("a", "Track A", "Artist", None),
                None,
            )
            .expect("load A");
        engine
            .preload_next_bytes(
                wav_b,
                Some("wav".to_owned()),
                Song::new("b", "Track B", "Artist", None),
                None,
            )
            .expect("preload B");

        assert_eq!(engine.active_deck(), DeckId::A);
        let report = engine
            .drain_to_eof_null(None)
            .expect("drain A -> B gapless");
        assert_eq!(engine.active_deck(), DeckId::B);
        assert_eq!(report.song.as_ref().map(|s| s.video_id.as_str()), Some("b"));
        assert!(
            report.rendered_duration_ms >= 470 && report.rendered_duration_ms <= 530,
            "total_gapless_ms={}",
            report.rendered_duration_ms
        );
    }

    #[test]
    fn prepared_prebuffered_track_preserves_opening_samples_and_full_duration() {
        let wav_old = build_test_wav_pcm16(48_000, 220.0, 400);
        let wav_new = build_test_wav_pcm16(48_000, 440.0, 800);

        let mut engine = AudioEngine::with_null_sink();
        engine
            .load_bytes(
                wav_old,
                Some("wav".to_owned()),
                Song::new("old", "Old Track", "Artist", None),
                None,
            )
            .expect("load old");
        engine.wait_prebuffer(50, Duration::from_millis(500));
        engine.play();
        let _ = engine.render_null_frames(1_024).expect("render old");

        // Pre-decode 500 ms off-thread just like `bitchord-desktop/src/backend.rs`.
        let mut hint = Hint::new();
        hint.with_extension("wav");
        let mut dec =
            AudioDecoder::open_source(Box::new(Cursor::new(wav_new)), hint).expect("open new wav");
        let prebuf = dec.prebuffer_ms(500, || false).expect("prebuffer 500ms");
        assert!(
            !prebuf.is_empty(),
            "expected prebuffered opening packets before play"
        );
        let first_pkt_peak = prebuf[0]
            .samples
            .iter()
            .fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(first_pkt_peak > 0.2, "expected audible opening packet");

        engine
            .load_prepared_decoder(
                dec,
                prebuf,
                Song::new("new", "New Track", "Artist", None),
                None,
                Some("Test"),
            )
            .expect("load prepared");
        engine.wait_prebuffer(500, Duration::from_millis(500));

        let report = engine
            .drain_to_eof_null(None)
            .expect("drain prepared track");
        assert_eq!(report.underruns, 0);
        assert!(
            report.rendered_duration_ms >= 770 && report.rendered_duration_ms <= 830,
            "opening frames must not be clipped: got {}ms",
            report.rendered_duration_ms
        );
    }

    #[test]
    fn headless_engine_executes_automix_dj_bass_swap_transition_plan() {
        let wav_a = build_test_wav_pcm16(48_000, 220.0, 2_400);
        let wav_b = build_test_wav_pcm16(48_000, 440.0, 2_400);

        let mut engine = AudioEngine::with_null_sink();
        engine
            .load_bytes(
                wav_a,
                Some("wav".to_owned()),
                Song::new("a", "Track A", "DJ A", None),
                None,
            )
            .expect("load A");
        engine
            .preload_next_bytes(
                wav_b,
                Some("wav".to_owned()),
                Song::new("b", "Track B", "DJ B", None),
                None,
            )
            .expect("preload B");

        let plan = TransitionPlan {
            style: bitchord_automix::TransitionStyle::DjBassSwap,
            filter_variant: bitchord_automix::FilterTransitionVariant::None,
            transition_start_sec: 0.4,
            duration_sec: 1.6,
            incoming_cue_sec: 0.0,
            incoming_playback_rate: 1.03,
            bass_swap_fraction: Some(0.50),
            vocal_clash_avoided: false,
            harmonic_match: true,
            beats_matched: true,
            phrase_bars: 8,
            outgoing_bpm: 124.0,
            incoming_bpm: 120.4,
            summary: "DJ Bass Swap · 124->120 BPM (1.03x) · 8 bars · Harmonic".to_owned(),
        };
        engine.set_transition_plan(Some(plan));
        assert!(engine.snapshot().automix_plan_summary.is_some());

        engine.wait_prebuffer(200, Duration::from_millis(500));
        let report = engine
            .drain_to_eof_null(None)
            .expect("drain automix bass swap");
        assert_eq!(engine.active_deck(), DeckId::B);
        assert_eq!(report.song.as_ref().map(|s| s.video_id.as_str()), Some("b"));
        assert!(engine.snapshot().automix_plan_summary.is_none());
        assert!(report.rendered_duration_ms >= 2_500 && report.rendered_duration_ms <= 4_200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn engine_stop_unblocks_stalled_http_chunk_under_200ms() {
        use std::collections::BTreeMap;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // Serve a valid 4-second WAV (~768 KiB = 6 chunks): answer chunk 0 (header)
        // and trailing chunks (so Symphonia's WAV probe metadata scan succeeds instantly),
        // then stall forever on chunk 1 (`bytes=131072-`) right as playback decodes into it.
        let wav = build_test_wav_pcm16(48_000, 440.0, 4_000);
        assert!(wav.len() > 300_000);
        let wav_arc = Arc::new(wav);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let data_srv = Arc::clone(&wav_arc);

        let _server = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let data = Arc::clone(&data_srv);
                tokio::spawn(async move {
                    let mut req_bytes = Vec::with_capacity(512);
                    let mut tmp = [0u8; 512];
                    while !req_bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req_bytes.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let req = String::from_utf8_lossy(&req_bytes).to_ascii_lowercase();
                    let range_line = req
                        .lines()
                        .find(|l| l.starts_with("range:"))
                        .unwrap_or("range: bytes=0-131071");
                    let bounds = range_line.split('=').nth(1).unwrap_or("0-131071").trim();
                    let (s_str, e_str) = bounds.split_once('-').unwrap_or(("0", "131071"));
                    let start: usize = s_str.parse().unwrap_or(0);
                    if start == 131_072 {
                        // Stall chunk 1 indefinitely to simulate a dropped Wi-Fi connection.
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        drop(sock);
                        return;
                    }
                    let end: usize = e_str
                        .parse::<usize>()
                        .unwrap_or(data.len() - 1)
                        .min(data.len() - 1);
                    let slice = &data[start..=end];
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len(),
                        slice.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(slice).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let stream = ResolvedStream {
            video_id: "stalled_track".to_owned(),
            url: format!("http://127.0.0.1:{}/track.wav", addr.port()),
            kbps: 1536,
            mime_type: "audio/wav".to_owned(),
            ext: "wav".to_owned(),
            content_length: Some(wav_arc.len() as u64),
            duration_ms: Some(4_000),
            loudness_db: None,
            is_live_hls: false,
            client_name: "TEST".to_owned(),
            profile_id: "test:stall".to_owned(),
            media_headers: BTreeMap::new(),
            range_chunk_bytes: Some(131_072),
        };
        let rt = Handle::current();
        let elapsed = tokio::task::spawn_blocking(move || {
            let mut engine = AudioEngine::with_null_sink();
            let song = Song::new("stalled_track", "Stalled", "Artist", None);
            engine
                .load_stream(Client::new(), rt, stream, song, None)
                .expect("load stalled stream");
            // Wait briefly so the decode worker consumes chunk 0 and blocks on stalled chunk 1.
            thread::sleep(Duration::from_millis(80));
            let t0 = Instant::now();
            engine.stop();
            t0.elapsed()
        })
        .await
        .expect("join blocking");

        assert!(
            elapsed < Duration::from_millis(200),
            "engine.stop() on stalled HTTP stream must finish <200ms, took {:?}",
            elapsed
        );
    }

    #[test]
    fn play_recovers_cleanly_from_playback_error_and_standby_helpers_work() {
        let wav_a = build_test_wav_pcm16(48_000, 440.0, 400);
        let wav_b = build_test_wav_pcm16(48_000, 660.0, 400);

        let mut engine = AudioEngine::with_null_sink();
        engine
            .load_bytes(
                wav_a,
                Some("wav".to_owned()),
                Song::new("a", "Track A", "Artist", None),
                None,
            )
            .expect("load A");
        engine
            .preload_next_bytes(
                wav_b,
                Some("wav".to_owned()),
                Song::new("b", "Track B", "Artist", None),
                None,
            )
            .expect("preload B");

        assert!(engine.has_standby_for("b"));
        assert!(!engine.has_standby_for("c"));

        // Simulate a transient decode error landing in PlaybackState::Error.
        if let Some(ref w) = engine.deck(DeckId::A).worker {
            *w.shared.error_msg.lock().unwrap() = Some("transient network glitch".to_owned());
            w.shared.eof_reached.store(true, Ordering::Release);
        }
        engine.state = PlaybackState::Error;

        // Calling play() must clear error state and resume cleanly rather than snapping back to Error.
        engine.play();
        assert_eq!(engine.snapshot().state, PlaybackState::Playing);
        assert!(engine.snapshot().last_error.is_none());

        // Instant manual switch to preloaded standby deck.
        assert!(engine.activate_standby_now());
        assert_eq!(engine.active_deck(), DeckId::B);
        assert_eq!(
            engine.snapshot().current_song.as_ref().map(|s| s.video_id.as_str()),
            Some("b")
        );
        engine.clear_standby();
        assert!(engine.standby_song().is_none());
    }
}
