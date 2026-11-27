//! Pure-Rust DSD64–DSD1024 (`.dsf` / `.dff`) demuxer, ISO/IEC 14496-3 Subpart 10
//! DST lossless decoder, Blackman-windowed sinc nibble-LUT DSD→176.4 kHz PCM
//! decimator, and DoP (DSD-over-PCM v1.1) framer (`Wave 5C`).
//!
//! 100% safe Rust (`#![forbid(unsafe_code)]`). Mirrors upstream `DsdFormat.kt`,
//! `DsdExtractor.kt`, `dsd_decimator.cpp`, and `dst/decoder.c` so `.dsf` and
//! `.dff` (plain DSD and DST-compressed SACD rips) stream seamlessly from local
//! disk, WebDAV HTTP Range, and SMB2/3 shares into PipeWire or bit-perfect
//! ALSA Exclusive DACs.

use std::f64::consts::PI;
use std::io::{self, Read, Seek, SeekFrom};

use symphonia::core::io::MediaSource;

use crate::decoder::{DecodedPacket, DecodedStreamInfo, DecoderError};

/// Target PCM sample rate every DSD rate decimates to (176.4 kHz).
pub const DSD_PCM_RATE_HZ: u32 = 176_400;
/// Low-pass corner frequency eliminating ultrasonic DSD sigma-delta noise (48 kHz).
pub const DSD_FILTER_CUTOFF_HZ: f64 = 48_000.0;
/// Supported DSD 1-bit sample rates (DSD64 through DSD1024).
pub const SUPPORTED_DSD_RATES: &[u32] = &[
    2_822_400,  // DSD64
    5_644_800,  // DSD128
    11_289_600, // DSD256
    22_579_200, // DSD512
    45_158_400, // DSD1024
];
/// Standard DSF per-channel block size in bytes.
pub const DSF_BLOCK_BYTES: usize = 4_096;
/// One DST frame represents 1/75 s of DSD64 per channel = 4,704 bytes = 37,632 1-bit samples.
pub const DST_FRAME_BYTES_PER_CH: usize = 4_704;
pub const DST_FRAME_SAMPLES: u64 = 37_632;

/// Container flavor for a DSD file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdContainer {
    Dsf,
    Dff,
}

/// Parsed container and stream parameters for a DSD/DST track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdFormat {
    pub container: DsdContainer,
    pub bit_rate_hz: u32,
    pub channels: usize,
    pub sample_count: u64,
    pub data_offset: u64,
    pub data_bytes: u64,
    pub lsb_first: bool,
    pub block_bytes: usize,
    pub is_dst: bool,
}

impl DsdFormat {
    pub fn supports_bit_rate(rate: u32) -> bool {
        SUPPORTED_DSD_RATES.contains(&rate)
    }

    pub fn decimation(&self) -> usize {
        (self.bit_rate_hz / DSD_PCM_RATE_HZ) as usize
    }

    pub fn pcm_frames(&self) -> u64 {
        let dec = self.decimation() as u64;
        self.sample_count.saturating_add(dec - 1) / dec
    }

    pub fn duration_ms(&self) -> u64 {
        (self.sample_count.saturating_mul(1_000)) / u64::from(self.bit_rate_hz.max(1))
    }

    pub fn bytes_per_channel(&self) -> u64 {
        self.sample_count.saturating_add(7) / 8
    }

    pub fn filter_taps(&self) -> usize {
        512 * (self.bit_rate_hz / 2_822_400) as usize
    }

    pub fn dsd_multiplier(&self) -> u32 {
        self.bit_rate_hz / 44_100
    }

    pub fn codec_mime(&self) -> String {
        format!("audio/dsd{}", self.dsd_multiplier())
    }

    /// Required DoP (DSD-over-PCM v1.1) PCM carrier rate (`bit_rate_hz / 16`).
    pub fn dop_pcm_rate_hz(&self) -> u32 {
        self.bit_rate_hz / 16
    }

    pub fn seek_frame(&self, time_ms: u64) -> u64 {
        if time_ms >= self.duration_ms() {
            return self.pcm_frames();
        }
        ((time_ms.saturating_mul(u64::from(DSD_PCM_RATE_HZ))) / 1_000).min(self.pcm_frames())
    }

    /// First per-channel DSD byte needed to prime the FIR filter before `frame`.
    pub fn preroll_byte(&self, frame: u64) -> u64 {
        let dec = self.decimation() as u64;
        let half_taps = (self.filter_taps() / 2) as u64;
        let bit_pos = frame.saturating_mul(dec).saturating_sub(half_taps);
        let first = (bit_pos / 8).min(self.bytes_per_channel());
        if self.container == DsdContainer::Dsf {
            let blk = self.block_bytes as u64;
            (first / blk) * blk
        } else {
            first
        }
    }

    /// Byte offset inside the container for per-channel DSD byte index `byte_per_channel`.
    pub fn file_position(&self, byte_per_channel: u64) -> u64 {
        let clamped = byte_per_channel.min(self.bytes_per_channel());
        let ch = self.channels as u64;
        if self.container == DsdContainer::Dsf {
            let blk = self.block_bytes as u64;
            self.data_offset + (clamped / blk) * blk * ch
        } else {
            self.data_offset + clamped * ch
        }
    }
}

/// Computes unity-normalized Blackman-windowed sinc FIR taps for `bit_rate_hz` (`DsdFilter.kt`).
pub fn build_dsd_fir_coefficients(bit_rate_hz: u32) -> Vec<f64> {
    let taps = 512 * (bit_rate_hz / 2_822_400).max(1) as usize;
    let cutoff = DSD_FILTER_CUTOFF_HZ / f64::from(bit_rate_hz);
    let half = (taps - 1) as f64 / 2.0;
    let mut coeffs = Vec::with_capacity(taps);
    let mut sum = 0.0_f64;
    for i in 0..taps {
        let x = i as f64 - half;
        let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (taps - 1) as f64).cos()
            + 0.08 * (4.0 * PI * i as f64 / (taps - 1) as f64).cos();
        let sinc = if x.abs() < 1e-12 {
            2.0 * cutoff
        } else {
            (2.0 * PI * cutoff * x).sin() / (PI * x)
        };
        let v = sinc * w;
        sum += v;
        coeffs.push(v);
    }
    if sum.abs() > 1e-15 {
        for c in &mut coeffs {
            *c /= sum;
        }
    }
    coeffs
}

/// Fast nibble-lookup-table 1-bit DSD to `176.4 kHz` `f32` PCM FIR decimator
/// (100% safe Rust port of `dsd_decimator.cpp`).
pub struct DsdBlockDecimator {
    channels: usize,
    decimation: usize,
    groups: usize,
    position: usize,
    full: usize,
    total_samples: u64,
    total_frames: u64,
    source_byte: u64,
    output_frame: u64,
    coefficients: Vec<f64>,
    lookup: Vec<f32>,
    history: Vec<u16>,
}

impl DsdBlockDecimator {
    pub fn new(format: &DsdFormat) -> Self {
        let coefficients = build_dsd_fir_coefficients(format.bit_rate_hz);
        let groups = coefficients.len() / 8;
        let mut lookup = vec![0.0_f32; groups * 32];
        for g in 0..groups {
            for n in 0..2 {
                for v in 0..16usize {
                    let mut sum = 0.0_f64;
                    for b in 0..4 {
                        // MSB-first bit ordering: bit 7 is earliest sample in byte -> coeff index 0..7.
                        // Match dsd_decimator.cpp exact bit mapping: bit `n*4 + b` of byte.
                        let bit_val = if (v & (1 << b)) != 0 { 1.0 } else { -1.0 };
                        sum += coefficients[g * 8 + n * 4 + b] * bit_val;
                    }
                    lookup[g * 32 + n * 16 + v] = sum as f32;
                }
            }
        }
        Self {
            channels: format.channels,
            decimation: format.decimation(),
            groups,
            position: 0,
            full: 0,
            total_samples: format.sample_count,
            total_frames: format.pcm_frames(),
            source_byte: 0,
            output_frame: 0,
            coefficients,
            lookup,
            history: vec![0u16; groups * format.channels],
        }
    }

    pub fn ended(&self) -> bool {
        self.output_frame >= self.total_frames
    }

    pub fn output_frame(&self) -> u64 {
        self.output_frame
    }

    pub fn reset(&mut self, first_byte: u64, first_frame: u64) {
        self.source_byte = first_byte;
        self.output_frame = first_frame;
        self.position = 0;
        self.full = 0;
        self.history.fill(0);
    }

    #[inline]
    fn group_value(&self, group: usize, byte: u16) -> f64 {
        let lo = (byte & 0x0F) as usize;
        let hi = ((byte >> 4) & 0x0F) as usize;
        f64::from(self.lookup[group * 32 + lo]) + f64::from(self.lookup[group * 32 + 16 + hi])
    }

    fn push_byte(&mut self, input: Option<&[u8]>, valid_bits: usize, out_stereo: &mut Vec<f32>) {
        if (self.history[self.position] >> 8) == 8 {
            self.full = self.full.saturating_sub(1);
        }
        if valid_bits == 8 {
            self.full += 1;
        }
        for c in 0..self.channels {
            let byte_val = if valid_bits > 0 {
                u16::from(input.map(|s| s[c]).unwrap_or(0))
            } else {
                0
            };
            self.history[c * self.groups + self.position] = ((valid_bits as u16) << 8) | byte_val;
        }
        self.source_byte += 1;
        let ready = self.source_byte * 8
            >= self.output_frame * self.decimation as u64 + (self.groups as u64) * 4;
        if ready && self.output_frame < self.total_frames {
            let mask = self.groups - 1;
            let mut ring = self.position;
            if self.full == self.groups {
                let mut left = 0.0_f64;
                let mut right = 0.0_f64;
                for g in 0..self.groups {
                    left += self.group_value(g, self.history[ring]);
                    if self.channels == 2 {
                        right += self.group_value(g, self.history[self.groups + ring]);
                    }
                    ring = ring.wrapping_sub(1) & mask;
                }
                let l = left as f32;
                let r = if self.channels == 2 { right as f32 } else { l };
                out_stereo.push(l);
                out_stereo.push(r);
            } else {
                let mut ch_out = [0.0_f32; 2];
                for (c, slot) in ch_out.iter_mut().enumerate().take(self.channels) {
                    let mut sum = 0.0_f64;
                    let mut r_idx = self.position;
                    for g in 0..self.groups {
                        let packed = self.history[c * self.groups + r_idx];
                        let count = (packed >> 8) as usize;
                        if count == 8 {
                            sum += self.group_value(g, packed);
                        } else {
                            for b in (8 - count)..8 {
                                let bit_val = if (packed & (1 << b)) != 0 { 1.0 } else { -1.0 };
                                sum += self.coefficients[g * 8 + b] * bit_val;
                            }
                        }
                        r_idx = r_idx.wrapping_sub(1) & mask;
                    }
                    *slot = sum as f32;
                }
                let l = ch_out[0];
                let r = if self.channels == 2 { ch_out[1] } else { l };
                out_stereo.push(l);
                out_stereo.push(r);
            }
            self.output_frame += 1;
        }
        self.position = (self.position + 1) & (self.groups - 1);
    }

    /// Decodes interleaved MSB-first DSD bytes (or flushes filter tail when `bytes` is `None`),
    /// appending interleaved stereo `f32` PCM samples to `out_stereo` and returning frames produced.
    pub fn decode_block(&mut self, bytes: Option<&[u8]>, out_stereo: &mut Vec<f32>) -> usize {
        if self.ended() {
            return 0;
        }
        let start_frame = self.output_frame;
        if let Some(data) = bytes {
            let count = data.len() / self.channels;
            for i in 0..count {
                if self.output_frame >= self.total_frames {
                    break;
                }
                let rem_bits = self.total_samples.saturating_sub(self.source_byte * 8);
                let valid = rem_bits.min(8) as usize;
                let slice = &data[i * self.channels..(i + 1) * self.channels];
                self.push_byte(Some(slice), valid, out_stereo);
            }
        } else {
            let end_source =
                (self.total_frames * self.decimation as u64 + (self.groups as u64) * 4).div_ceil(8);
            while self.source_byte < end_source && self.output_frame < self.total_frames {
                self.push_byte(None, 0, out_stereo);
            }
        }
        (self.output_frame - start_frame) as usize
    }
}

/// DSD-over-PCM (DoP v1.1) framer packing stereo MSB-first DSD bytes into 32-bit
/// `S32_LE` (24-bit left-justified) samples with alternating `0x05` / `0xFA` markers.
///
/// For USB DACs supporting DoP over bit-perfect ALSA Exclusive (`176.4 kHz` for DSD64,
/// `352.8 kHz` for DSD128).
#[derive(Debug, Default, Clone)]
pub struct DopPacker {
    toggle: bool,
}

impl DopPacker {
    pub fn new() -> Self {
        Self { toggle: false }
    }

    pub fn reset(&mut self) {
        self.toggle = false;
    }

    /// Packs `dsd_interleaved_msb` (`[L0, R0, L1, R1, ...]` where every 4 bytes = 1 stereo DoP frame)
    /// into `S32_LE` stereo samples (`[L_dop, R_dop, ...]`) with alternating `0x05`/`0xFA` markers.
    pub fn pack_stereo_s32le(&mut self, dsd_interleaved_msb: &[u8], out_s32: &mut Vec<i32>) {
        for chunk in dsd_interleaved_msb.as_chunks::<4>().0 {
            let marker: u32 = if self.toggle { 0xFA } else { 0x05 };
            self.toggle = !self.toggle;
            // MSB-first: earlier byte goes into bits 15..8, next byte into bits 7..0.
            let left_24 = (marker << 16) | (u32::from(chunk[0]) << 8) | u32::from(chunk[2]);
            let right_24 = (marker << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[3]);
            // Left-justify 24-bit DoP word inside S32_LE (`bits 31..8`).
            out_s32.push((left_24 << 8) as i32);
            out_s32.push((right_24 << 8) as i32);
        }
    }
}

// ============================================================================
// Pure-Rust ISO/IEC 14496-3 Subpart 10 DST Lossless Decoder (ported from decoder.c)
// ============================================================================

const DST_MAX_CHANNELS: usize = 6;
const DST_MAX_ELEMENTS: usize = 2 * DST_MAX_CHANNELS;

const FSETS_CODE_PRED_COEFF: [[i32; 3]; 3] = [[-8, 0, 0], [-16, 8, 0], [-9, -5, 6]];
const PROBS_CODE_PRED_COEFF: [[i32; 3]; 3] = [[-8, 0, 0], [-16, 8, 0], [-24, 24, -8]];

struct BitReader<'a> {
    data: &'a [u8],
    bits: usize,
    at: usize,
    error: bool,
    padding: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bits: data.len() * 8,
            at: 0,
            error: false,
            padding: 0,
        }
    }

    fn bits_left(&self) -> usize {
        self.bits.saturating_sub(self.at)
    }

    fn read_bits(&mut self, count: usize) -> u32 {
        if count > 24 || count > self.bits_left() {
            self.error = true;
            return 0;
        }
        let mut val = 0u32;
        for _ in 0..count {
            let byte = self.data[self.at / 8];
            let bit = u32::from((byte >> (7 - (self.at % 8))) & 1);
            val = (val << 1) | bit;
            self.at += 1;
        }
        val
    }

    fn read_bit(&mut self) -> u32 {
        self.read_bits(1)
    }

    fn read_sbits(&mut self, count: usize) -> i32 {
        if count == 0 {
            return 0;
        }
        let val = self.read_bits(count);
        let sign = 1u32 << (count - 1);
        (val ^ sign) as i32 - sign as i32
    }

    fn read_ur_golomb(&mut self, k: usize) -> i32 {
        let mut zeros = 0u32;
        let mut terminated = false;
        while self.bits_left() > 0 {
            if self.read_bit() != 0 {
                terminated = true;
                break;
            }
            zeros += 1;
            if zeros > 65_535 {
                self.error = true;
                return 0;
            }
        }
        if !terminated || self.bits_left() < k || self.error {
            self.error = true;
            return 0;
        }
        ((zeros << k) | self.read_bits(k)) as i32
    }

    fn read_sr_golomb_dst(&mut self, k: usize) -> i32 {
        let mut v = self.read_ur_golomb(k);
        if v != 0 && self.read_bit() != 0 {
            v = -v;
        }
        v
    }
}

#[derive(Default)]
struct DstArithCoder {
    a: u32,
    c: u32,
}

impl DstArithCoder {
    fn init(&mut self, gb: &mut BitReader<'_>) {
        self.a = 4095;
        self.c = gb.read_bits(12);
    }

    #[inline]
    fn get(&mut self, gb: &mut BitReader<'_>, p: i32) -> i32 {
        let k = (self.a >> 8) | ((self.a >> 7) & 1);
        let q = k.wrapping_mul(p as u32);
        let a_q = self.a.wrapping_sub(q);

        let e = if self.c < a_q {
            self.a = a_q;
            1
        } else {
            self.a = q;
            self.c = self.c.wrapping_sub(a_q);
            0
        };

        if self.a < 2048 {
            let log2_a = if self.a == 0 {
                0
            } else {
                31 - self.a.leading_zeros() as usize
            };
            let n = 11usize.saturating_sub(log2_a);
            let left = gb.bits_left();
            self.a <<= n;
            if left >= n {
                self.c = (self.c << n) | gb.read_bits(n);
            } else {
                gb.padding += n - left;
                if gb.padding > 32 {
                    gb.error = true;
                }
                self.c <<= n;
                if left > 0 {
                    self.c |= gb.read_bits(left) << (n - left);
                }
            }
        }
        e
    }
}

struct DstTable {
    elements: usize,
    length: [usize; DST_MAX_ELEMENTS],
    coeff: [[i32; 128]; DST_MAX_ELEMENTS],
}

impl Default for DstTable {
    fn default() -> Self {
        Self {
            elements: 0,
            length: [0; DST_MAX_ELEMENTS],
            coeff: [[0; 128]; DST_MAX_ELEMENTS],
        }
    }
}

/// Pure-Rust ISO/IEC 14496-3 Subpart 10 DST lossless frame decoder (DSD64 mono/stereo).
pub struct DstDecoder {
    channels: usize,
    fsets: DstTable,
    probs: DstTable,
    filter: Vec<[[i16; 256]; 16]>,
}

impl DstDecoder {
    pub fn new(channels: usize) -> Result<Self, DecoderError> {
        if !(1..=2).contains(&channels) {
            return Err(DecoderError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "DST decoder supports mono or stereo only",
            )));
        }
        Ok(Self {
            channels,
            fsets: DstTable::default(),
            probs: DstTable::default(),
            filter: vec![[[0i16; 256]; 16]; DST_MAX_ELEMENTS],
        })
    }

    pub fn frame_output_bytes(&self) -> usize {
        DST_FRAME_BYTES_PER_CH * self.channels
    }

    /// Decodes one compressed (or bypass) DST frame into `output` (exact `4704 * channels` bytes).
    pub fn decode_frame(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), DecoderError> {
        let expected_out = self.frame_output_bytes();
        if input.len() < 2 || input.len() > 1_048_576 || output.len() < expected_out {
            return Err(dsd_err("invalid DST frame size"));
        }
        let samples_per_frame = DST_FRAME_SAMPLES as usize;
        let nb_bytes_per_ch = samples_per_frame / 8;

        let mut gb = BitReader::new(input);

        // Uncompressed (bypass) DST frame (`bit0 == 0`).
        if gb.read_bit() == 0 {
            let _ = gb.read_bit();
            if gb.read_bits(6) != 0 {
                return Err(dsd_err("invalid uncompressed DST frame header"));
            }
            if input.len() - 1 != nb_bytes_per_ch * self.channels {
                return Err(dsd_err("uncompressed DST frame length mismatch"));
            }
            output[..expected_out].copy_from_slice(&input[1..1 + expected_out]);
            return Ok(());
        }

        // Segmentation flags (10.4, 10.5, 10.6): single segment across all channels.
        if gb.read_bit() == 0 || gb.read_bit() == 0 || gb.read_bit() == 0 {
            return Err(dsd_err("unsupported multi-segment DST frame"));
        }

        // Channel-to-element mappings (10.7..10.9).
        let same_map = gb.read_bit() != 0;
        let mut map_felem = [0usize; DST_MAX_CHANNELS];
        let mut map_pelem = [0usize; DST_MAX_CHANNELS];
        read_dst_map(&mut gb, &mut self.fsets, &mut map_felem, self.channels)?;
        if same_map {
            self.probs.elements = self.fsets.elements;
            map_pelem = map_felem;
        } else {
            read_dst_map(&mut gb, &mut self.probs, &mut map_pelem, self.channels)?;
        }

        // Half-probability flags (10.10).
        let mut half_prob = [false; DST_MAX_CHANNELS];
        for slot in half_prob.iter_mut().take(self.channels) {
            *slot = gb.read_bit() != 0;
        }

        // Filter coefficient sets (10.12) and probability tables (10.13).
        read_dst_table(
            &mut gb,
            &mut self.fsets,
            &FSETS_CODE_PRED_COEFF,
            7,
            9,
            true,
            0,
        )?;
        read_dst_table(
            &mut gb,
            &mut self.probs,
            &PROBS_CODE_PRED_COEFF,
            6,
            7,
            false,
            1,
        )?;
        if gb.error || gb.read_bit() != 0 {
            return Err(dsd_err("corrupt DST header tables"));
        }

        let mut ac = DstArithCoder::default();
        ac.init(&mut gb);
        if gb.error {
            return Err(dsd_err("truncated DST arithmetic header"));
        }
        build_dst_filter(&mut self.filter, &self.fsets)?;

        let mut status = [u128::from_le_bytes([0xAA; 16]); DST_MAX_CHANNELS];
        output[..expected_out].fill(0);

        let x_prob = (((self.fsets.coeff[0][0] & 127) as u8).reverse_bits() >> 1) + 1;
        let _dst_x_bit = ac.get(&mut gb, i32::from(x_prob));

        for i in 0..samples_per_frame {
            if gb.error {
                return Err(dsd_err("corrupt DST arithmetic stream"));
            }
            for ch in 0..self.channels {
                let felem = map_felem[ch];
                let filt = &self.filter[felem];
                let st_bytes = status[ch].to_le_bytes();
                let mut predict: i32 = 0;
                for j in 0..16 {
                    predict += i32::from(filt[j][usize::from(st_bytes[j])]);
                }
                let predict = predict as i16;

                let prob = if !half_prob[ch] || i >= self.fsets.length[felem] {
                    let pelem = map_pelem[ch];
                    let index = (i32::from(predict).unsigned_abs() >> 3) as usize;
                    let max_idx = self.probs.length[pelem].saturating_sub(1);
                    self.probs.coeff[pelem][index.min(max_idx)]
                } else {
                    128
                };

                let residual = ac.get(&mut gb, prob);
                let v = (((i32::from(predict) >> 15) ^ residual) & 1) as u8;
                output[(i >> 3) * self.channels + ch] |= v << (7 - (i & 7));
                status[ch] = (status[ch] << 1) | u128::from(v);
            }
        }
        if gb.error {
            return Err(dsd_err("truncated DST frame payload"));
        }
        Ok(())
    }
}

fn read_dst_map(
    gb: &mut BitReader<'_>,
    table: &mut DstTable,
    map: &mut [usize; DST_MAX_CHANNELS],
    channels: usize,
) -> Result<(), DecoderError> {
    table.elements = 1;
    map.fill(0);
    if gb.read_bit() == 0 {
        for slot in map.iter_mut().take(channels).skip(1) {
            let bits = (31 - (table.elements as u32).leading_zeros()) as usize + 1;
            let idx = gb.read_bits(bits) as usize;
            *slot = idx;
            if idx == table.elements {
                table.elements += 1;
                if table.elements >= DST_MAX_ELEMENTS {
                    return Err(dsd_err("DST channel map overflow"));
                }
            } else if idx > table.elements {
                return Err(dsd_err("invalid DST channel map index"));
            }
        }
    }
    Ok(())
}

fn read_dst_table(
    gb: &mut BitReader<'_>,
    table: &mut DstTable,
    pred_coeff: &[[i32; 3]; 3],
    length_bits: usize,
    coeff_bits: usize,
    is_signed: bool,
    offset: i32,
) -> Result<(), DecoderError> {
    for i in 0..table.elements {
        let len = gb.read_bits(length_bits) as usize + 1;
        if len > 128 {
            return Err(dsd_err("DST table length exceeds 128"));
        }
        table.length[i] = len;
        if gb.read_bit() == 0 {
            for slot in table.coeff[i].iter_mut().take(len) {
                let raw = if is_signed {
                    gb.read_sbits(coeff_bits)
                } else {
                    gb.read_bits(coeff_bits) as i32
                };
                *slot = raw + offset;
            }
        } else {
            let method = gb.read_bits(2) as usize;
            if method >= 3 || method + 1 > len {
                return Err(dsd_err("invalid DST coefficient prediction method"));
            }
            for slot in table.coeff[i].iter_mut().take(method + 1) {
                let raw = if is_signed {
                    gb.read_sbits(coeff_bits)
                } else {
                    gb.read_bits(coeff_bits) as i32
                };
                *slot = raw + offset;
            }
            let lsb_size = gb.read_bits(3) as usize;
            for j in (method + 1)..len {
                let mut x: i64 = 0;
                for k in 0..=method {
                    x += i64::from(pred_coeff[method][k]) * i64::from(table.coeff[i][j - k - 1]);
                }
                let mut c = i64::from(gb.read_sr_golomb_dst(lsb_size));
                if x >= 0 {
                    c -= (x + 4) / 8;
                } else {
                    c += (-x + 3) / 8;
                }
                if !is_signed
                    && (c < i64::from(offset) || c >= i64::from(offset + (1 << coeff_bits)))
                {
                    return Err(dsd_err("DST unsigned table coefficient out of range"));
                }
                if gb.error || !(-32_768..=32_767).contains(&c) {
                    return Err(dsd_err("DST coefficient out of i16 range"));
                }
                table.coeff[i][j] = c as i32;
            }
        }
    }
    Ok(())
}

fn build_dst_filter(filter: &mut [[[i16; 256]; 16]], fsets: &DstTable) -> Result<(), DecoderError> {
    for i in 0..fsets.elements {
        let len = fsets.length[i] as isize;
        for j in 0..16usize {
            let total = (len - (j as isize) * 8).clamp(0, 8) as usize;
            for (k, slot) in filter[i][j].iter_mut().enumerate() {
                let mut v: i64 = 0;
                for l in 0..total {
                    let sign = if ((k >> l) & 1) != 0 { 1 } else { -1 };
                    v += sign * i64::from(fsets.coeff[i][j * 8 + l]);
                }
                if i16::try_from(v).is_err() {
                    return Err(dsd_err("DST filter LUT overflow"));
                }
                *slot = v as i16;
            }
        }
    }
    Ok(())
}

/// Computes the SACD/DSDIFF DST frame CRC-32 (`0x8000_0011`, matching `DstFrameCrc.kt`).
pub fn dst_frame_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bytes {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if (crc & 0x8000_0000) != 0 {
                (crc << 1) ^ 0x8000_0011
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ============================================================================
// Unified DSF / DSDIFF (plain + DST) Seekable Stream Decoder
// ============================================================================

#[derive(Debug, Clone)]
struct DstFrameEntry {
    offset: u64,
    size: usize,
    crc: Option<u32>,
}

/// Stateful `.dsf` / `.dff` (plain DSD or DST-compressed) demuxer and 176.4 kHz `f32` decoder.
pub struct DsdStreamDecoder {
    source: Box<dyn MediaSource>,
    format: DsdFormat,
    info: DecodedStreamInfo,
    decimator: DsdBlockDecimator,
    byte_position: u64,
    raw_buf: Vec<u8>,
    norm_buf: Vec<u8>,
    dst_frames: Vec<DstFrameEntry>,
    dst_decoder: Option<DstDecoder>,
    dst_frame_idx: usize,
    dst_byte_cursor: usize,
    dst_loaded: bool,
    dst_decoded: Vec<u8>,
}

impl DsdStreamDecoder {
    /// Checks whether the first 4 bytes of a stream match DSF (`"DSD "`) or DSDIFF (`"FRM8"`).
    pub fn is_dsd_magic(magic: &[u8; 4]) -> bool {
        magic == b"DSD " || magic == b"FRM8"
    }

    /// Opens a DSF or DSDIFF/DFF stream positioned at byte 0.
    pub fn open(mut source: Box<dyn MediaSource>) -> Result<Self, DecoderError> {
        let is_seekable = source.is_seekable();
        let file_len = source.byte_len();
        source.seek(SeekFrom::Start(0))?;

        let mut head = [0u8; 12];
        source.read_exact(&mut head)?;
        let (format, dst_frames) = if &head[0..4] == b"DSD " {
            parse_dsf_header(&mut *source, &head, file_len)?
        } else if &head[0..4] == b"FRM8" {
            parse_dff_header(&mut *source, &head, file_len)?
        } else {
            return Err(dsd_err("not a DSF or DFF container"));
        };

        let decimator = DsdBlockDecimator::new(&format);
        let dst_decoder = if format.is_dst {
            Some(DstDecoder::new(format.channels)?)
        } else {
            None
        };
        let dst_decoded = if format.is_dst {
            vec![0u8; DST_FRAME_BYTES_PER_CH * format.channels]
        } else {
            Vec::new()
        };

        let info = DecodedStreamInfo {
            track_id: 0,
            codec_mime: format.codec_mime(),
            // Decoded PCM output rate handed to resampler / ALSA exclusive sink.
            sample_rate_hz: DSD_PCM_RATE_HZ,
            dsd_bit_rate_hz: Some(format.bit_rate_hz),
            channels: format.channels,
            bit_depth: Some(1),
            duration_ms: Some(format.duration_ms()),
            estimated_bitrate_kbps: Some(
                ((u64::from(format.bit_rate_hz) * format.channels as u64) / 1_000) as u32,
            ),
            is_seekable,
        };

        // Position source at start of raw DSD audio (or first DST frame).
        if let Some(first) = dst_frames.first() {
            source.seek(SeekFrom::Start(first.offset))?;
        } else {
            source.seek(SeekFrom::Start(format.data_offset))?;
        }

        let buf_cap = (format.block_bytes * format.channels).max(8_192);
        Ok(Self {
            source,
            format,
            info,
            decimator,
            byte_position: 0,
            raw_buf: vec![0u8; buf_cap],
            norm_buf: vec![0u8; buf_cap],
            dst_frames,
            dst_decoder,
            dst_frame_idx: 0,
            dst_byte_cursor: 0,
            dst_loaded: false,
            dst_decoded,
        })
    }

    pub fn info(&self) -> &DecodedStreamInfo {
        &self.info
    }

    pub fn dsd_format(&self) -> &DsdFormat {
        &self.format
    }

    pub fn seek_ms(&mut self, position_ms: u64) -> Result<u64, DecoderError> {
        let frame = self.format.seek_frame(position_ms);
        let req_byte = self.format.preroll_byte(frame);
        let source_byte = if !self.dst_frames.is_empty() {
            let step = DST_FRAME_BYTES_PER_CH as u64;
            (req_byte / step) * step
        } else {
            req_byte
        };

        self.decimator.reset(source_byte, frame);
        self.byte_position = source_byte;

        if !self.dst_frames.is_empty() {
            self.dst_frame_idx = (source_byte / DST_FRAME_BYTES_PER_CH as u64) as usize;
            self.dst_byte_cursor = 0;
            self.dst_loaded = false;
            if let Some(entry) = self.dst_frames.get(self.dst_frame_idx) {
                self.source.seek(SeekFrom::Start(entry.offset))?;
            }
        } else {
            let file_pos = self.format.file_position(source_byte);
            self.source.seek(SeekFrom::Start(file_pos))?;
        }

        Ok((frame * 1_000) / u64::from(DSD_PCM_RATE_HZ))
    }

    pub fn next_packet(&mut self) -> Result<Option<DecodedPacket>, DecoderError> {
        if self.decimator.ended() {
            return Ok(None);
        }
        let mut samples = Vec::with_capacity(4_096);
        let mut first_frame = self.decimator.output_frame();

        while samples.is_empty() && !self.decimator.ended() {
            first_frame = self.decimator.output_frame();
            if !self.dst_frames.is_empty() {
                if self.dst_frame_idx < self.dst_frames.len() {
                    if !self.dst_loaded {
                        let entry = self.dst_frames[self.dst_frame_idx].clone();
                        self.source.seek(SeekFrom::Start(entry.offset))?;
                        let mut encoded = vec![0u8; entry.size];
                        self.source.read_exact(&mut encoded)?;
                        let dec = self
                            .dst_decoder
                            .as_mut()
                            .ok_or_else(|| dsd_err("missing DST decoder"))?;
                        dec.decode_frame(&encoded, &mut self.dst_decoded)?;
                        if let Some(expected_crc) = entry.crc {
                            let actual_crc = dst_frame_crc32(&self.dst_decoded);
                            if actual_crc != expected_crc {
                                return Err(dsd_err("DST frame CRC-32 mismatch"));
                            }
                        }
                        self.dst_loaded = true;
                        self.dst_byte_cursor = 0;
                    }
                    let ch = self.format.channels;
                    let max_chunk = 8_192 - (8_192 % ch);
                    let rem = self.dst_decoded.len() - self.dst_byte_cursor;
                    let take = rem.min(max_chunk);
                    let slice =
                        &self.dst_decoded[self.dst_byte_cursor..self.dst_byte_cursor + take];
                    self.decimator.decode_block(Some(slice), &mut samples);
                    self.dst_byte_cursor += take;
                    if self.dst_byte_cursor >= self.dst_decoded.len() {
                        self.dst_loaded = false;
                        self.dst_frame_idx += 1;
                    }
                } else {
                    self.decimator.decode_block(None, &mut samples);
                }
            } else if self.byte_position < self.format.bytes_per_channel() {
                let ch = self.format.channels;
                let read_size = if self.format.container == DsdContainer::Dsf {
                    self.format.block_bytes * ch
                } else {
                    let per_ch = ((8_192 / ch) as u64)
                        .min(self.format.bytes_per_channel() - self.byte_position)
                        as usize;
                    per_ch * ch
                };
                self.source.read_exact(&mut self.raw_buf[..read_size])?;

                let count = ((read_size / ch) as u64)
                    .min(self.format.bytes_per_channel() - self.byte_position)
                    as usize;
                let norm_len = count * ch;
                for frame in 0..count {
                    for c in 0..ch {
                        let src_idx = if self.format.container == DsdContainer::Dsf {
                            c * self.format.block_bytes + frame
                        } else {
                            frame * ch + c
                        };
                        let raw_b = self.raw_buf[src_idx];
                        self.norm_buf[frame * ch + c] = if self.format.lsb_first {
                            raw_b.reverse_bits()
                        } else {
                            raw_b
                        };
                    }
                }
                self.byte_position += count as u64;
                self.decimator
                    .decode_block(Some(&self.norm_buf[..norm_len]), &mut samples);
            } else {
                // Flush FIR filter tail.
                self.decimator.decode_block(None, &mut samples);
            }
        }

        if samples.is_empty() {
            return Ok(None);
        }
        let frames = samples.len() / 2;
        let timestamp_ms = (first_frame * 1_000) / u64::from(DSD_PCM_RATE_HZ);
        Ok(Some(DecodedPacket {
            samples,
            timestamp_ms,
            frames,
        }))
    }
}

fn parse_dsf_header(
    src: &mut dyn MediaSource,
    first12: &[u8; 12],
    _file_len: Option<u64>,
) -> Result<(DsdFormat, Vec<DstFrameEntry>), DecoderError> {
    let dsd_chunk_size = u64::from_le_bytes(first12[4..12].try_into().unwrap());
    if dsd_chunk_size != 28 {
        return Err(dsd_err("invalid DSF DSD chunk size"));
    }
    let mut rest = [0u8; 16];
    src.read_exact(&mut rest)?;
    let _total_file_size = u64::from_le_bytes(rest[0..8].try_into().unwrap());
    let _metadata_offset = u64::from_le_bytes(rest[8..16].try_into().unwrap());

    let mut fmt_hdr = [0u8; 12];
    src.read_exact(&mut fmt_hdr)?;
    if &fmt_hdr[0..4] != b"fmt " {
        return Err(dsd_err("missing DSF fmt chunk"));
    }
    let fmt_size = u64::from_le_bytes(fmt_hdr[4..12].try_into().unwrap());
    if !(52..=4096).contains(&fmt_size) {
        return Err(dsd_err("invalid DSF fmt chunk size"));
    }
    let mut fmt_body = vec![0u8; (fmt_size - 12) as usize];
    src.read_exact(&mut fmt_body)?;

    let format_version = u32::from_le_bytes(fmt_body[0..4].try_into().unwrap());
    let format_id = u32::from_le_bytes(fmt_body[4..8].try_into().unwrap());
    let _channel_type = u32::from_le_bytes(fmt_body[8..12].try_into().unwrap());
    let channels = u32::from_le_bytes(fmt_body[12..16].try_into().unwrap()) as usize;
    let bit_rate_hz = u32::from_le_bytes(fmt_body[16..20].try_into().unwrap());
    let bits_per_sample = u32::from_le_bytes(fmt_body[20..24].try_into().unwrap());
    let sample_count = u64::from_le_bytes(fmt_body[24..32].try_into().unwrap());
    let block_bytes = u32::from_le_bytes(fmt_body[32..36].try_into().unwrap()) as usize;

    if format_version != 1 || format_id != 0 {
        return Err(dsd_err("unsupported DSF format version/id"));
    }
    if !(1..=2).contains(&channels) {
        return Err(dsd_err("DSF supports mono or stereo only"));
    }
    if !DsdFormat::supports_bit_rate(bit_rate_hz) {
        return Err(dsd_err("unsupported DSD sample rate"));
    }
    if bits_per_sample != 1 && bits_per_sample != 8 {
        return Err(dsd_err("invalid DSF bit order"));
    }
    if block_bytes != DSF_BLOCK_BYTES {
        return Err(dsd_err("unsupported DSF block size"));
    }

    let mut data_hdr = [0u8; 12];
    src.read_exact(&mut data_hdr)?;
    if &data_hdr[0..4] != b"data" {
        return Err(dsd_err("missing DSF data chunk"));
    }
    let data_chunk_size = u64::from_le_bytes(data_hdr[4..12].try_into().unwrap());
    if data_chunk_size < 12 {
        return Err(dsd_err("invalid DSF data chunk size"));
    }
    let data_bytes = data_chunk_size - 12;
    let data_offset = src.stream_position()?;

    let format = DsdFormat {
        container: DsdContainer::Dsf,
        bit_rate_hz,
        channels,
        sample_count,
        data_offset,
        data_bytes,
        lsb_first: bits_per_sample == 1,
        block_bytes,
        is_dst: false,
    };
    Ok((format, Vec::new()))
}

fn parse_dff_header(
    src: &mut dyn MediaSource,
    first12: &[u8; 12],
    _file_len: Option<u64>,
) -> Result<(DsdFormat, Vec<DstFrameEntry>), DecoderError> {
    let form_size = u64::from_be_bytes(first12[4..12].try_into().unwrap());
    if form_size < 4 {
        return Err(dsd_err("invalid DFF FRM8 size"));
    }
    let file_end = form_size.saturating_add(12);
    let mut form_type = [0u8; 4];
    src.read_exact(&mut form_type)?;
    if &form_type != b"DSD " {
        return Err(dsd_err("FRM8 container is not DSDIFF"));
    }

    let mut rate = 0u32;
    let mut channels = 0usize;
    let mut is_dst = false;
    let mut data_offset = None;
    let mut data_bytes = 0u64;
    let mut dst_frames = Vec::new();
    let mut dst_declared_count = 0usize;

    let mut scan = src.stream_position()?;
    let mut chunks = 0usize;
    while scan + 12 <= file_end {
        chunks += 1;
        if chunks > 4_096 {
            return Err(dsd_err("too many DFF chunks"));
        }
        src.seek(SeekFrom::Start(scan))?;
        let mut hdr = [0u8; 12];
        src.read_exact(&mut hdr)?;
        let id = &hdr[0..4];
        let size = u64::from_be_bytes(hdr[4..12].try_into().unwrap());
        let payload_start = scan + 12;
        let payload_end = payload_start.saturating_add(size).min(file_end);
        let padded_next = payload_end.saturating_add(size & 1);

        match id {
            b"PROP" if (4..=65_536).contains(&size) => {
                let mut payload = vec![0u8; size as usize];
                src.read_exact(&mut payload)?;
                let (r, ch, dst) = parse_dff_prop(&payload)?;
                rate = r;
                channels = ch;
                is_dst = dst;
            }
            b"DSD " => {
                data_offset = Some(payload_start);
                data_bytes = size;
            }
            b"DST " => {
                data_offset = Some(payload_start);
                data_bytes = size;
                let mut sub = payload_start;
                while sub + 12 <= payload_end {
                    src.seek(SeekFrom::Start(sub))?;
                    let mut sh = [0u8; 12];
                    src.read_exact(&mut sh)?;
                    let sid = &sh[0..4];
                    let ssize = u64::from_be_bytes(sh[4..12].try_into().unwrap());
                    let s_start = sub + 12;
                    let s_end = s_start.saturating_add(ssize).min(payload_end);
                    match sid {
                        b"FRTE" if ssize == 6 => {
                            let mut info = [0u8; 6];
                            src.read_exact(&mut info)?;
                            dst_declared_count =
                                u32::from_be_bytes(info[0..4].try_into().unwrap()) as usize;
                        }
                        b"DSTF" if (2..=1_048_576).contains(&ssize) => {
                            dst_frames.push(DstFrameEntry {
                                offset: s_start,
                                size: ssize as usize,
                                crc: None,
                            });
                        }
                        b"DSTC" if ssize == 4 => {
                            let mut c = [0u8; 4];
                            src.read_exact(&mut c)?;
                            if let Some(last) = dst_frames.last_mut() {
                                last.crc = Some(u32::from_be_bytes(c));
                            }
                        }
                        _ => {}
                    }
                    sub = s_end.saturating_add(ssize & 1);
                }
            }
            _ => {}
        }
        scan = padded_next;
    }

    let offset = data_offset.ok_or_else(|| dsd_err("missing DFF DSD/DST sound data"))?;
    if !DsdFormat::supports_bit_rate(rate) || !(1..=2).contains(&channels) {
        return Err(dsd_err("missing or unsupported DFF PROP metadata"));
    }
    if is_dst {
        if dst_frames.is_empty()
            || (dst_declared_count > 0 && dst_frames.len() != dst_declared_count)
        {
            return Err(dsd_err("invalid DST frame count"));
        }
        if rate != 2_822_400 {
            return Err(dsd_err("DST decoding requires DSD64 (2.8224 MHz)"));
        }
    }
    let sample_count = if is_dst {
        dst_frames.len() as u64 * DST_FRAME_SAMPLES
    } else {
        (data_bytes / channels as u64) * 8
    };

    let format = DsdFormat {
        container: DsdContainer::Dff,
        bit_rate_hz: rate,
        channels,
        sample_count,
        data_offset: offset,
        data_bytes,
        lsb_first: false,
        block_bytes: DSF_BLOCK_BYTES,
        is_dst,
    };
    Ok((format, dst_frames))
}

fn parse_dff_prop(payload: &[u8]) -> Result<(u32, usize, bool), DecoderError> {
    if payload.len() < 4 || &payload[0..4] != b"SND " {
        return Err(dsd_err("DFF PROP chunk is not SND "));
    }
    let mut pos = 4usize;
    let mut rate = 0u32;
    let mut channels = 0usize;
    let mut is_dst = false;
    while let Some(header_end) = pos.checked_add(12) {
        if header_end > payload.len() {
            break;
        }
        let id = &payload[pos..pos + 4];
        let size_u64 = u64::from_be_bytes(payload[pos + 4..header_end].try_into().unwrap());
        let Ok(size) = usize::try_from(size_u64) else {
            break;
        };
        pos = header_end;
        let Some(chunk_end) = pos.checked_add(size) else {
            break;
        };
        if chunk_end > payload.len() {
            break;
        }
        match id {
            b"FS  " if size == 4 => {
                rate = u32::from_be_bytes(payload[pos..pos + 4].try_into().unwrap());
            }
            b"CHNL" if size >= 2 => {
                channels = u16::from_be_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
            }
            b"CMPR" if size >= 4 => {
                let cmpr = &payload[pos..pos + 4];
                if cmpr == b"DST " {
                    is_dst = true;
                } else if cmpr != b"DSD " {
                    return Err(dsd_err("unsupported DFF compression type"));
                }
            }
            _ => {}
        }
        let Some(next_pos) = chunk_end.checked_add(size & 1) else {
            break;
        };
        pos = next_pos;
    }
    Ok((rate, channels, is_dst))
}

fn dsd_err(msg: &str) -> DecoderError {
    DecoderError::Io(io::Error::new(io::ErrorKind::InvalidData, msg))
}

/// Generates a deterministic 1st-order sigma-delta modulated DSD64 stereo `.dsf` byte buffer
/// carrying a sine wave at `freq_hz` (ideal for unit tests and CLI diagnostics).
pub fn build_test_dsf_dsd64(freq_hz: f64, duration_ms: u64) -> Vec<u8> {
    let rate = 2_822_400u32;
    let sample_count = (u64::from(rate) * duration_ms) / 1_000;
    let bytes_per_ch = sample_count.div_ceil(8) as usize;
    let blocks = bytes_per_ch.div_ceil(DSF_BLOCK_BYTES);
    let padded_per_ch = blocks * DSF_BLOCK_BYTES;

    let mut ch_bytes = vec![0u8; padded_per_ch];
    let mut integrator = 0.0_f64;
    for s in 0..sample_count as usize {
        let target = 0.45 * (2.0 * PI * freq_hz * s as f64 / f64::from(rate)).sin();
        let bit = if integrator >= 0.0 { 1u8 } else { 0u8 };
        let q = if bit != 0 { 1.0 } else { -1.0 };
        integrator += target - q;
        // DSF bits_per_sample = 1 stores LSB-first.
        if bit != 0 {
            ch_bytes[s / 8] |= 1 << (s % 8);
        }
    }

    let data_bytes = (blocks * DSF_BLOCK_BYTES * 2) as u64;
    let total_file_size = 28 + 52 + 12 + data_bytes;
    let mut out = Vec::with_capacity(total_file_size as usize);
    // "DSD " chunk (28 bytes)
    out.extend_from_slice(b"DSD ");
    out.extend_from_slice(&28u64.to_le_bytes());
    out.extend_from_slice(&total_file_size.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // no ID3v2 tail
    // "fmt " chunk (52 bytes)
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&52u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // format version
    out.extend_from_slice(&0u32.to_le_bytes()); // format id (DSD raw)
    out.extend_from_slice(&2u32.to_le_bytes()); // stereo channel type
    out.extend_from_slice(&2u32.to_le_bytes()); // channel count
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // LSB-first
    out.extend_from_slice(&sample_count.to_le_bytes());
    out.extend_from_slice(&(DSF_BLOCK_BYTES as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    // "data" chunk
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(12 + data_bytes).to_le_bytes());
    for blk in 0..blocks {
        let start = blk * DSF_BLOCK_BYTES;
        let slice = &ch_bytes[start..start + DSF_BLOCK_BYTES];
        out.extend_from_slice(slice); // L block
        out.extend_from_slice(slice); // R block
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::decoder::AudioDecoder;
    use symphonia::core::formats::probe::Hint;

    fn build_test_dff_dsd64(freq_hz: f64, duration_ms: u64) -> Vec<u8> {
        let rate = 2_822_400u32;
        let sample_count = (u64::from(rate) * duration_ms) / 1_000;
        let bytes_per_ch = sample_count.div_ceil(8) as usize;
        let mut interleaved = vec![0u8; bytes_per_ch * 2];
        let mut integrator = 0.0_f64;
        for s in 0..sample_count as usize {
            let target = 0.45 * (2.0 * PI * freq_hz * s as f64 / f64::from(rate)).sin();
            let bit = if integrator >= 0.0 { 1u8 } else { 0u8 };
            let q = if bit != 0 { 1.0 } else { -1.0 };
            integrator += target - q;
            if bit != 0 {
                // DFF stores MSB-first, byte-interleaved stereo.
                let mask = 1u8 << (7 - (s % 8));
                interleaved[(s / 8) * 2] |= mask;
                interleaved[(s / 8) * 2 + 1] |= mask;
            }
        }
        let mut prop = Vec::new();
        prop.extend_from_slice(b"SND ");
        prop.extend_from_slice(b"FS  ");
        prop.extend_from_slice(&4u64.to_be_bytes());
        prop.extend_from_slice(&rate.to_be_bytes());
        prop.extend_from_slice(b"CHNL");
        prop.extend_from_slice(&10u64.to_be_bytes());
        prop.extend_from_slice(&2u16.to_be_bytes());
        prop.extend_from_slice(b"SLFTSRGT");
        prop.extend_from_slice(b"CMPR");
        prop.extend_from_slice(&10u64.to_be_bytes());
        prop.extend_from_slice(b"DSD ");
        prop.push(4);
        prop.extend_from_slice(b"none\0");

        let mut body = Vec::new();
        body.extend_from_slice(b"DSD ");
        body.extend_from_slice(b"FVER");
        body.extend_from_slice(&4u64.to_be_bytes());
        body.extend_from_slice(&0x0105_0000u32.to_be_bytes());
        body.extend_from_slice(b"PROP");
        body.extend_from_slice(&(prop.len() as u64).to_be_bytes());
        body.extend_from_slice(&prop);
        body.extend_from_slice(b"DSD ");
        body.extend_from_slice(&(interleaved.len() as u64).to_be_bytes());
        body.extend_from_slice(&interleaved);

        let mut out = Vec::with_capacity(12 + body.len());
        out.extend_from_slice(b"FRM8");
        out.extend_from_slice(&(body.len() as u64).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn decodes_dsf_dsd64_sine_wave_preserves_frequency_and_seeks() {
        let dsf = build_test_dsf_dsd64(1_000.0, 40);
        let mut dec =
            AudioDecoder::open_source(Box::new(Cursor::new(dsf)), Hint::new()).expect("open DSF");
        assert_eq!(dec.info().codec_mime, "audio/dsd64");
        assert_eq!(dec.info().sample_rate_hz, DSD_PCM_RATE_HZ);
        assert_eq!(dec.info().dsd_bit_rate_hz, Some(2_822_400));
        assert_eq!(dec.info().bit_depth, Some(1));

        let stats = dec.info().to_nerd_stats(Some("Local"));
        assert!(stats.is_lossless());
        assert!(stats.is_hi_res());
        assert_eq!(stats.sample_rate_hz, Some(2_822_400));
        assert_eq!(stats.bit_depth, Some(1));

        let mut left = Vec::new();
        while let Some(pkt) = dec.next_packet().expect("decode DSF") {
            for frame in pkt.samples.as_chunks::<2>().0 {
                left.push(frame[0]);
            }
        }
        // 40 ms @ 176.4 kHz = 7,056 stereo frames.
        assert!((left.len() as isize - 7_056).abs() <= 64);

        // Verify dominant 1 kHz sine tone via DFT correlation and clean hysteresis crossings.
        let mid = &left[1_500..5_028];
        let mag_at = |hz: f64| -> f64 {
            let mut re = 0.0_f64;
            let mut im = 0.0_f64;
            for (i, &x) in mid.iter().enumerate() {
                let phase = 2.0 * PI * hz * i as f64 / f64::from(DSD_PCM_RATE_HZ);
                re += f64::from(x) * phase.cos();
                im += f64::from(x) * phase.sin();
            }
            2.0 * (re * re + im * im).sqrt() / mid.len() as f64
        };
        let mag_1k = mag_at(1_000.0);
        let mag_2k = mag_at(2_000.0);
        let mag_500 = mag_at(500.0);
        assert!(
            (mag_1k - 0.45).abs() < 0.05,
            "expected ~0.45 amplitude at 1 kHz, got mag_1k={mag_1k:.4}, mag_2k={mag_2k:.4}, mag_500={mag_500:.4}"
        );
        assert!(
            mag_1k > mag_2k * 20.0 && mag_1k > mag_500 * 20.0,
            "1 kHz fundamental should dominate: 1k={mag_1k:.4}, 2k={mag_2k:.4}, 500={mag_500:.4}"
        );

        // Hysteresis positive crossings (±0.10 threshold rejects 1st-order SDM zero-crossing limit-cycle ripple).
        let mut armed = false;
        let mut crossings = 0usize;
        for &s in mid {
            if s < -0.10 {
                armed = true;
            } else if armed && s > 0.10 {
                crossings += 1;
                armed = false;
            }
        }
        assert!(
            (19..=21).contains(&crossings),
            "expected ~20 1kHz cycles in 20ms, got {crossings}"
        );

        // Seek to 20 ms and verify remaining frames (~3,528).
        let actual_ms = dec.seek_ms(20).expect("seek DSF");
        assert!((18..=22).contains(&actual_ms));
        let mut after_seek = 0usize;
        while let Some(pkt) = dec.next_packet().expect("decode after seek") {
            after_seek += pkt.frames;
        }
        assert!((after_seek as isize - 3_528).abs() <= 128);
    }

    #[test]
    fn decodes_dff_dsd64_and_dst_uncompressed_frame_with_crc() {
        let dff = build_test_dff_dsd64(1_000.0, 30);
        let mut dec =
            AudioDecoder::open_source(Box::new(Cursor::new(dff)), Hint::new()).expect("open DFF");
        assert_eq!(dec.info().codec_mime, "audio/dsd64");
        let mut frames = 0usize;
        while let Some(pkt) = dec.next_packet().expect("decode DFF") {
            frames += pkt.frames;
        }
        assert!((frames as isize - 5_292).abs() <= 64);

        // Verify DST decoder bypass path + CRC-32 consistency.
        let mut dst = DstDecoder::new(2).expect("dst init");
        let raw_dsd = vec![0x69u8; dst.frame_output_bytes()];
        let mut framed = Vec::with_capacity(1 + raw_dsd.len());
        framed.push(0x00); // bit0 = 0 (uncompressed DST frame), bits 1..7 = 0
        framed.extend_from_slice(&raw_dsd);
        let mut decoded = vec![0u8; dst.frame_output_bytes()];
        dst.decode_frame(&framed, &mut decoded).expect("dst decode");
        assert_eq!(decoded, raw_dsd);
        assert_eq!(dst_frame_crc32(&decoded), dst_frame_crc32(&raw_dsd));
    }

    #[test]
    fn dop_packer_alternates_0x05_and_0xfa_markers_across_stereo_frames() {
        let mut packer = DopPacker::new();
        let dsd = [0x11, 0x22, 0x33, 0x44, 0xAA, 0xBB, 0xCC, 0xDD];
        let mut out = Vec::new();
        packer.pack_stereo_s32le(&dsd, &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0] as u32, 0x0511_3300);
        assert_eq!(out[1] as u32, 0x0522_4400);
        assert_eq!(out[2] as u32, 0xFAAA_CC00);
        assert_eq!(out[3] as u32, 0xFABB_DD00);
    }

    #[test]
    fn parse_dff_prop_handles_corrupt_huge_chunk_size_without_overflow_panic() {
        // Build a malicious PROP payload whose sub-chunk declares u64::MAX bytes.
        let mut payload = Vec::new();
        payload.extend_from_slice(b"SND ");
        payload.extend_from_slice(b"FS  ");
        payload.extend_from_slice(&4u64.to_be_bytes());
        payload.extend_from_slice(&2_822_400u32.to_be_bytes());
        payload.extend_from_slice(b"JUNK");
        payload.extend_from_slice(&u64::MAX.to_be_bytes());
        payload.extend_from_slice(&[0u8; 8]);

        let (rate, channels, is_dst) =
            parse_dff_prop(&payload).expect("huge chunk size must not panic");
        assert_eq!(rate, 2_822_400);
        assert_eq!(channels, 0);
        assert!(!is_dst);

        // Also test near-usize::MAX size that would overflow `pos + size` or `size + (size & 1)`.
        let mut payload2 = Vec::new();
        payload2.extend_from_slice(b"SND ");
        payload2.extend_from_slice(b"JUNK");
        payload2.extend_from_slice(&((usize::MAX - 2) as u64).to_be_bytes());
        let (rate2, _, _) =
            parse_dff_prop(&payload2).expect("near-usize::MAX must not overflow");
        assert_eq!(rate2, 0);
    }
}
