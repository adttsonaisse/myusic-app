#![forbid(unsafe_code)]

//! Native Linux dual-deck audio engine, Float32 DSP chain, bounded HTTP range
//! streaming, Listen Together sync coordinator, monotonic sleep timer, and
//! MPRIS2 D-Bus server for BitChord Linux Desktop (`Wave 3`).

pub mod alsa_output;
pub mod chunked_source;
pub mod crossfade;
pub mod decoder;
pub mod dsp;
pub mod engine;
pub mod mpris;
pub mod output;
pub mod party_sync;
pub mod resampler;
pub mod skip_silence;
pub mod sleep_timer;
pub mod time_stretch;

pub use alsa_output::{
    AlsaDeviceInfo, AlsaExclusiveSink, AlsaSampleFormat, PROBE_SAMPLE_RATES, convert_f32_to_s16le,
    convert_f32_to_s24_3le, convert_f32_to_s32le, enumerate_alsa_devices, parse_proc_asound_cards,
    parse_proc_asound_pcm,
};

pub use chunked_source::{
    ChunkedRangeSource, DEFAULT_CHUNK_BYTES, MAX_CACHE_BYTES, MAX_EMPTY_RANGES,
    PREFETCH_CHUNKS_AHEAD, RefusalCallback,
};
pub use crossfade::{
    BAIL_STEP_MS, CrossfadeGains, CrossfadePhase, CrossfadeState, DeckId, HANDOFF_AT,
    HANDOFF_GUARD_MS, MAX_CROSSFADE_SECONDS, MIN_BLEND_TRIM, equal_power_gains,
};
pub use decoder::{
    AudioDecoder, DecodedPacket, DecodedStreamInfo, DecoderError, SeekableReadSource,
    build_test_wav_pcm16,
};
pub use dsp::{
    DspChain, EqTuningSnapshot, LoudnessProcessor, SpatialAudioProcessor,
    loudness_db_to_linear_gain, sanitize_samples,
};
pub use engine::{
    AudioEngine, AudioSourceSpec, EngineError, HeadlessPlaybackReport, PlaybackSnapshot,
    PlaybackState, open_http_decoder,
};
pub use mpris::{
    BitChordMprisPlayer, DESKTOP_ENTRY, IDENTITY, LoopStatus, MPRIS_BUS_SUFFIX, MprisCommand,
    MprisError, MprisService, MprisState, PlaybackStatus, build_mpris_metadata,
    encode_mpris_track_id,
};
pub use output::{
    AudioBackend, AudioOutputMode, CMD_RING_CAPACITY, NullAudioSink, OUTPUT_CHANNELS,
    OUTPUT_SAMPLE_RATE, OutputError, PipeWireSink, RING_CAPACITY_SAMPLES, RtCommand, RtMetrics,
    RtMixerState, SinkProducers,
};
pub use party_sync::{LocalPlaybackView, PartySyncCoordinator, ReconcileAction};
pub use resampler::{AudioResampler, ResamplerError};
pub use skip_silence::{DEFAULT_THRESHOLD, FADE_MS, MIN_SILENCE_MS, SkipSilenceProcessor};
pub use sleep_timer::{MAX_MINUTES, PRESETS, STEP_MINUTES, SleepTimer};
pub use time_stretch::{MAX_SPEED, MIN_SPEED, SonicStretcher};
