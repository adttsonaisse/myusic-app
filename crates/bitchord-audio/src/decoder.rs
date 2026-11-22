//! Audio demuxer and decoder powered by `symphonia 0.6` + `symphonia-adapter-libopus 0.3`.
//!
//! Reads authoritative source codec parameters (`sample_rate`, `bits_per_sample`,
//! `channels`, `codec_mime`) directly from the container/codec header so `NerdStats`
//! never falsely reports `32-bit` just because the DSP pipeline operates in `f32`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::LazyLock;

use bitchord_integrations::{NerdStatsSnapshot, TelemetryProvenance};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3,
    CODEC_ID_OPUS, CODEC_ID_PCM_ALAW, CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F64BE,
    CODEC_ID_PCM_F64LE, CODEC_ID_PCM_MULAW, CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S16LE,
    CODEC_ID_PCM_S24BE, CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S32BE, CODEC_ID_PCM_S32LE,
    CODEC_ID_PCM_U8, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{
    AudioCodecId, AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions,
};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};
use symphonia_adapter_libopus::OpusDecoder;
use thiserror::Error;

use crate::dsd::DsdStreamDecoder;

/// Global codec registry extending Symphonia's built-in codecs with `libopus`.
static CODEC_REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<OpusDecoder>();
    registry
});

#[derive(Debug, Error)]
pub enum DecoderError {
    #[error("I/O error opening audio source: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported or corrupt container/codec: {0}")]
    Symphonia(#[from] SymphoniaError),
    #[error("no playable audio track found in container")]
    NoAudioTrack,
}

/// Authoritative codec parameters extracted from the container/codec header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedStreamInfo {
    pub track_id: u32,
    pub codec_mime: String,
    pub sample_rate_hz: u32,
    /// Native 1-bit DSD clock rate (e.g. `2_822_400` for DSD64) when decoding `.dsf`/`.dff`.
    pub dsd_bit_rate_hz: Option<u32>,
    pub channels: usize,
    /// Authoritative source bit depth (e.g. `Some(16)` or `Some(24)` for FLAC/WAV/ALAC;
    /// `Some(1)` for DSD; `None` for lossy codecs such as Opus/AAC/MP3).
    pub bit_depth: Option<u32>,
    pub duration_ms: Option<u64>,
    pub estimated_bitrate_kbps: Option<u32>,
    pub is_seekable: bool,
}

impl DecodedStreamInfo {
    /// Builds a measured [`NerdStatsSnapshot`] respecting BitChord's provenance contract.
    pub fn to_nerd_stats(&self, source_label: Option<&str>) -> NerdStatsSnapshot {
        let reported_rate = self.dsd_bit_rate_hz.unwrap_or(self.sample_rate_hz);
        let pcm_rate_kbps = self.bit_depth.map(|depth| {
            ((u64::from(reported_rate) * u64::from(depth) * self.channels as u64) / 1_000) as u32
        });
        NerdStatsSnapshot {
            mime_type: Some(self.codec_mime.clone()),
            sample_rate_hz: Some(reported_rate),
            bit_depth: self.bit_depth,
            channels: Some(self.channels as u32),
            bitrate_kbps: self.estimated_bitrate_kbps,
            pcm_data_rate_kbps: pcm_rate_kbps,
            source_name: source_label.map(ToOwned::to_owned),
            bitrate_provenance: TelemetryProvenance::Measured,
            ..NerdStatsSnapshot::default()
        }
    }
}

/// One decoded stereo `f32` audio packet along with its source media timestamp.
#[derive(Debug, Clone)]
pub struct DecodedPacket {
    /// Interleaved stereo (`L, R, L, R, ...`) normalized `f32` samples.
    pub samples: Vec<f32>,
    /// Source media timestamp of the start of this packet (ms).
    pub timestamp_ms: u64,
    /// Number of stereo frames (`samples.len() / 2`).
    pub frames: usize,
}

enum DecoderBackend {
    Symphonia {
        format: Box<dyn FormatReader>,
        decoder: Box<dyn SymphoniaAudioDecoder>,
        time_base: Option<TimeBase>,
        scratch: Vec<f32>,
        decoded_frames: u64,
    },
    Dsd(Box<DsdStreamDecoder>),
}

/// Stateful audio demuxer + decoder outputting stereo interleaved `f32` frames.
pub struct AudioDecoder {
    backend: DecoderBackend,
    info: DecodedStreamInfo,
    byte_len: Option<u64>,
}

/// Adapts any seekable `Read + Seek + Send + Sync` byte source (such as
/// `bitchord_netstorage::SmbRangeReader`) into a Symphonia [`MediaSource`].
pub struct SeekableReadSource<R> {
    inner: R,
    byte_len: Option<u64>,
}

impl<R: Read + Seek + Send + Sync + 'static> SeekableReadSource<R> {
    pub fn new(inner: R, byte_len: Option<u64>) -> Self {
        Self { inner, byte_len }
    }
}

impl<R: Read + Seek + Send + Sync + 'static> Read for SeekableReadSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Read + Seek + Send + Sync + 'static> Seek for SeekableReadSource<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<R: Read + Seek + Send + Sync + 'static> MediaSource for SeekableReadSource<R> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

impl AudioDecoder {
    /// Opens a local audio file on disk.
    pub fn open_path(path: &Path) -> Result<Self, DecoderError> {
        let file = File::open(path)?;
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        Self::open_source(Box::new(file), hint)
    }

    /// Opens any seekable `Read + Seek + Send + Sync` reader (e.g. an SMB2/3 range stream).
    pub fn open_seekable_reader<R>(
        reader: R,
        byte_len: Option<u64>,
        extension: Option<&str>,
    ) -> Result<Self, DecoderError>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mut hint = Hint::new();
        if let Some(ext) = extension
            && !ext.trim().is_empty()
        {
            hint.with_extension(ext.trim().trim_start_matches('.'));
        }
        Self::open_source(Box::new(SeekableReadSource::new(reader, byte_len)), hint)
    }

    /// Opens any `MediaSource` (file, memory cursor, `ChunkedRangeSource`, or SMB stream),
    /// transparently sniffing DSF/DSDIFF (`.dsf`/`.dff`) ahead of Symphonia.
    pub fn open_source(mut source: Box<dyn MediaSource>, hint: Hint) -> Result<Self, DecoderError> {
        let byte_len = source.byte_len();
        let is_seekable = source.is_seekable();

        // Sniff DSF ("DSD ") or DSDIFF ("FRM8") magic before handing to Symphonia.
        if is_seekable {
            let mut magic = [0u8; 4];
            if source.read_exact(&mut magic).is_ok() {
                let _ = source.seek(SeekFrom::Start(0));
                if DsdStreamDecoder::is_dsd_magic(&magic) {
                    let dsd = DsdStreamDecoder::open(source)?;
                    let info = dsd.info().clone();
                    return Ok(Self {
                        backend: DecoderBackend::Dsd(Box::new(dsd)),
                        info,
                        byte_len,
                    });
                }
            } else {
                let _ = source.seek(SeekFrom::Start(0));
            }
        }

        let mss = MediaSourceStream::new(source, Default::default());

        let format = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;

        let track = format
            .default_track(TrackType::Audio)
            .or_else(|| format.first_track_known_codec(TrackType::Audio))
            .ok_or(DecoderError::NoAudioTrack)?;

        let track_id = track.id;
        let time_base = track.time_base;
        let audio_params = track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .ok_or(DecoderError::NoAudioTrack)?
            .clone();

        let sample_rate_hz = audio_params.sample_rate.unwrap_or(48_000);
        let channels = audio_params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(2)
            .max(1);
        let bit_depth = audio_params
            .bits_per_sample
            .or(audio_params.bits_per_coded_sample);
        let duration_ms = track
            .duration
            .and_then(|dur| {
                let ts = Timestamp::from(dur.get().min(i64::MAX as u64) as i64);
                time_base.and_then(|tb| tb.calc_time(ts))
            })
            .map(time_to_ms)
            .or_else(|| {
                track
                    .num_frames
                    .map(|frames| (frames * 1_000) / u64::from(sample_rate_hz.max(1)))
            });

        let estimated_bitrate_kbps = match (byte_len, duration_ms) {
            (Some(bytes), Some(ms)) if bytes > 0 && ms > 0 => Some(((bytes * 8) / ms) as u32),
            _ => None,
        };

        let decoder =
            CODEC_REGISTRY.make_audio_decoder(&audio_params, &AudioDecoderOptions::default())?;

        let info = DecodedStreamInfo {
            track_id,
            codec_mime: codec_id_to_mime(audio_params.codec).to_owned(),
            sample_rate_hz,
            dsd_bit_rate_hz: None,
            channels,
            bit_depth,
            duration_ms,
            estimated_bitrate_kbps,
            is_seekable,
        };

        Ok(Self {
            backend: DecoderBackend::Symphonia {
                format,
                decoder,
                time_base,
                scratch: Vec::with_capacity(8_192),
                decoded_frames: 0,
            },
            info,
            byte_len,
        })
    }

    pub fn info(&self) -> &DecodedStreamInfo {
        &self.info
    }

    /// Populates `duration_ms` (and derived `estimated_bitrate_kbps`) from an
    /// authoritative stream metadata source (e.g. YouTube `approxDurationMs` or
    /// `googlevideo.com` `dur=` parameter) when the container demuxer does not
    /// carry a per-track duration (common on WebM/Matroska Opus streams).
    pub fn apply_fallback_duration_ms(&mut self, duration_ms: Option<u64>) {
        let Some(dur) = duration_ms.filter(|&d| d > 0) else {
            return;
        };
        if self.info.duration_ms.is_none_or(|d| d == 0) {
            self.info.duration_ms = Some(dur);
        }
        if self.info.estimated_bitrate_kbps.is_none()
            && let Some(bytes) = self.byte_len
            && bytes > 0
        {
            self.info.estimated_bitrate_kbps = Some(((bytes * 8) / dur) as u32);
        }
    }

    /// Pre-decodes at least `min_ms` of opening audio packets starting from exact
    /// sample 0 so playback can begin immediately without network stall or clipped intro.
    /// Returns early if `should_cancel()` becomes `true` or EOF is reached.
    pub fn prebuffer_ms(
        &mut self,
        min_ms: u64,
        should_cancel: impl Fn() -> bool,
    ) -> Result<Vec<DecodedPacket>, DecoderError> {
        let target_frames = (u64::from(self.info.sample_rate_hz.max(1)) * min_ms) / 1_000;
        let mut packets = Vec::new();
        let mut frames_acc = 0u64;
        while frames_acc < target_frames {
            if should_cancel() {
                break;
            }
            match self.next_packet()? {
                Some(pkt) => {
                    frames_acc = frames_acc.saturating_add(pkt.frames as u64);
                    packets.push(pkt);
                }
                None => break,
            }
        }
        Ok(packets)
    }

    /// Seeks to `position_ms` (accurate mode when supported) and resets the codec state.
    pub fn seek_ms(&mut self, position_ms: u64) -> Result<u64, DecoderError> {
        match &mut self.backend {
            DecoderBackend::Symphonia {
                format,
                decoder,
                time_base,
                decoded_frames,
                ..
            } => {
                let target = SeekTo::Time {
                    time: Time::from_millis_u64(position_ms),
                    track_id: Some(self.info.track_id),
                };
                let seeked = format.seek(SeekMode::Accurate, target)?;
                decoder.reset();
                let actual_ms = time_base
                    .and_then(|tb| tb.calc_time(seeked.actual_ts))
                    .map(time_to_ms)
                    .unwrap_or(position_ms);
                *decoded_frames = (actual_ms * u64::from(self.info.sample_rate_hz)) / 1_000;
                Ok(actual_ms)
            }
            DecoderBackend::Dsd(dsd) => dsd.seek_ms(position_ms),
        }
    }

    /// Decodes the next stereo `f32` audio packet, or returns `Ok(None)` at clean EOF.
    pub fn next_packet(&mut self) -> Result<Option<DecodedPacket>, DecoderError> {
        match &mut self.backend {
            DecoderBackend::Symphonia {
                format,
                decoder,
                time_base,
                scratch,
                decoded_frames,
            } => loop {
                let Some(packet) = format.next_packet()? else {
                    return Ok(None);
                };
                if packet.track_id != self.info.track_id {
                    continue;
                }
                let pkt_ts = packet.pts;

                match decoder.decode(&packet) {
                    Ok(decoded) => {
                        let frames = decoded.frames();
                        if frames == 0 {
                            continue;
                        }
                        let src_channels = decoded.spec().channels().count().max(1);
                        scratch.clear();
                        decoded.copy_to_vec_interleaved::<f32>(scratch);

                        let stereo = mix_to_stereo(scratch, frames, src_channels);
                        let timestamp_ms = time_base
                            .and_then(|tb| tb.calc_time(pkt_ts))
                            .map(time_to_ms)
                            .unwrap_or_else(|| {
                                (*decoded_frames * 1_000)
                                    / u64::from(self.info.sample_rate_hz.max(1))
                            });
                        *decoded_frames = decoded_frames.saturating_add(frames as u64);

                        return Ok(Some(DecodedPacket {
                            samples: stereo,
                            timestamp_ms,
                            frames,
                        }));
                    }
                    Err(SymphoniaError::DecodeError(_)) => {
                        // Recoverable corrupted packet; skip and continue decoding.
                        continue;
                    }
                    Err(SymphoniaError::IoError(ref io_err))
                        if io_err.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        return Ok(None);
                    }
                    Err(err) => return Err(DecoderError::Symphonia(err)),
                }
            },
            DecoderBackend::Dsd(dsd) => dsd.next_packet(),
        }
    }
}

fn mix_to_stereo(src: &[f32], frames: usize, src_channels: usize) -> Vec<f32> {
    if src_channels == 2 {
        return src[..frames * 2].to_vec();
    }
    let mut stereo = Vec::with_capacity(frames * 2);
    if src_channels == 1 {
        for &s in &src[..frames] {
            stereo.push(s);
            stereo.push(s);
        }
    } else {
        // Multichannel downmix: preserve FL/FR and fold center/surrounds softly.
        for f in 0..frames {
            let base = f * src_channels;
            let fl = src[base];
            let fr = src[base + 1];
            let center = if src_channels >= 3 {
                src[base + 2] * 0.707
            } else {
                0.0
            };
            stereo.push((fl + center).clamp(-1.0, 1.0));
            stereo.push((fr + center).clamp(-1.0, 1.0));
        }
    }
    stereo
}

fn time_to_ms(t: Time) -> u64 {
    t.as_millis().max(0) as u64
}

fn codec_id_to_mime(codec: AudioCodecId) -> &'static str {
    match codec {
        CODEC_ID_FLAC => "audio/flac",
        CODEC_ID_ALAC => "audio/alac",
        CODEC_ID_OPUS => "audio/opus",
        CODEC_ID_VORBIS => "audio/vorbis",
        CODEC_ID_AAC => "audio/mp4a-latm",
        CODEC_ID_MP1 | CODEC_ID_MP2 | CODEC_ID_MP3 => "audio/mpeg",
        CODEC_ID_PCM_U8 | CODEC_ID_PCM_S16LE | CODEC_ID_PCM_S16BE | CODEC_ID_PCM_S24LE
        | CODEC_ID_PCM_S24BE | CODEC_ID_PCM_S32LE | CODEC_ID_PCM_S32BE | CODEC_ID_PCM_F32LE
        | CODEC_ID_PCM_F32BE | CODEC_ID_PCM_F64LE | CODEC_ID_PCM_F64BE | CODEC_ID_PCM_ALAW
        | CODEC_ID_PCM_MULAW => "audio/raw",
        _ => "audio/unknown",
    }
}

/// Builds a valid minimal 16-bit stereo PCM WAV byte buffer for tests and CLI smoke checks.
pub fn build_test_wav_pcm16(sample_rate: u32, freq_hz: f64, duration_ms: u64) -> Vec<u8> {
    use std::f64::consts::PI;
    let frames = ((u64::from(sample_rate) * duration_ms) / 1_000) as usize;
    let data_bytes = (frames * 4) as u32;
    let riff_size = 36 + data_bytes;
    let mut wav = Vec::with_capacity((44 + data_bytes) as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 4).to_le_bytes()); // byte rate
    wav.extend_from_slice(&4u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for f in 0..frames {
        let s = (0.5 * (2.0 * PI * freq_hz * f as f64 / f64::from(sample_rate)).sin() * 32_767.0)
            .round() as i16;
        wav.extend_from_slice(&s.to_le_bytes());
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn decodes_wav_reports_exact_16bit_depth_and_seeks_cleanly() {
        let wav = build_test_wav_pcm16(44_100, 440.0, 500);
        let mut hint = Hint::new();
        hint.with_extension("wav");
        let mut dec =
            AudioDecoder::open_source(Box::new(Cursor::new(wav)), hint).expect("open wav");

        // Must read authoritative 16-bit header, NEVER 32-bit from f32 pipeline.
        assert_eq!(dec.info().sample_rate_hz, 44_100);
        assert_eq!(dec.info().channels, 2);
        assert_eq!(dec.info().bit_depth, Some(16));
        assert_eq!(dec.info().codec_mime, "audio/raw");
        assert!(dec.info().duration_ms.unwrap_or(0) >= 490);

        let stats = dec.info().to_nerd_stats(Some("Local"));
        assert!(stats.is_lossless());
        assert!(!stats.is_hi_res());
        assert_eq!(stats.bit_depth, Some(16));
        assert_eq!(stats.bitrate_provenance, TelemetryProvenance::Measured);

        let mut total_frames = 0usize;
        while let Some(pkt) = dec.next_packet().expect("decode") {
            total_frames += pkt.frames;
        }
        assert_eq!(total_frames, 22_050);

        // Seek back to 250 ms and verify remaining frames (~11_025).
        let actual_ms = dec.seek_ms(250).expect("seek");
        assert!(actual_ms <= 260);
        let mut after_seek = 0usize;
        while let Some(pkt) = dec.next_packet().expect("decode after seek") {
            after_seek += pkt.frames;
        }
        assert!((after_seek as isize - 11_025).abs() < 1_024);
    }
}
