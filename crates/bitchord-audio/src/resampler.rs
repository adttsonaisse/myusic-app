//! Decode-thread sample-rate converter backed by `rubato 5.1` (`Fft` synchronous
//! resampler) so mixed-rate tracks (e.g. `44.1 kHz` FLAC -> `48 kHz` PipeWire)
//! mix seamlessly with zero pitch shift.
//!
//! When `input_rate == output_rate`, processing is a zero-cost slice copy.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler, WindowFunction};
use thiserror::Error;

const DEFAULT_CHUNK_FRAMES: usize = 1_024;

#[derive(Debug, Error)]
pub enum ResamplerError {
    #[error("failed to construct resampler ({in_rate} Hz -> {out_rate} Hz): {detail}")]
    Construction {
        in_rate: u32,
        out_rate: u32,
        detail: String,
    },
    #[error("resampler processing failed: {0}")]
    Process(String),
}

/// Interleaved `f32` sample-rate converter for the decode worker thread.
pub struct AudioResampler {
    in_rate: u32,
    out_rate: u32,
    channels: usize,
    inner: Option<Fft<f32>>,
    pending_in: Vec<f32>,
    scratch_out: Vec<f32>,
}

impl AudioResampler {
    /// Creates a resampler from `in_rate` to `out_rate` for `channels` interleaved channels.
    ///
    /// If `in_rate == out_rate`, enters zero-cost passthrough mode.
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Result<Self, ResamplerError> {
        let channels = channels.max(1);
        if in_rate == out_rate || in_rate == 0 || out_rate == 0 {
            return Ok(Self {
                in_rate,
                out_rate,
                channels,
                inner: None,
                pending_in: Vec::new(),
                scratch_out: Vec::new(),
            });
        }

        let fft = Fft::<f32>::new_custom(
            in_rate as usize,
            out_rate as usize,
            DEFAULT_CHUNK_FRAMES,
            2,
            channels,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|e| ResamplerError::Construction {
            in_rate,
            out_rate,
            detail: e.to_string(),
        })?;

        let out_max = fft.output_frames_max();
        Ok(Self {
            in_rate,
            out_rate,
            channels,
            inner: Some(fft),
            pending_in: Vec::with_capacity(DEFAULT_CHUNK_FRAMES * channels * 2),
            scratch_out: vec![0.0_f32; out_max * channels],
        })
    }

    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    pub fn input_rate(&self) -> u32 {
        self.in_rate
    }

    pub fn output_rate(&self) -> u32 {
        self.out_rate
    }

    /// Resampler algorithmic delay in output frames.
    pub fn output_delay_frames(&self) -> usize {
        self.inner.as_ref().map_or(0, Resampler::output_delay)
    }

    pub fn reset(&mut self) {
        self.pending_in.clear();
        if let Some(ref mut fft) = self.inner {
            fft.reset();
        }
    }

    /// Feeds interleaved `f32` samples and writes resampled interleaved `f32` samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), ResamplerError> {
        out.clear();
        let Some(ref mut fft) = self.inner else {
            out.extend_from_slice(input);
            return Ok(());
        };
        if !input.is_empty() {
            self.pending_in.extend_from_slice(input);
        }

        let ch = self.channels;
        loop {
            let needed = fft.input_frames_next();
            let available = self.pending_in.len() / ch;
            if available < needed {
                break;
            }
            let out_max = fft.output_frames_max();
            if self.scratch_out.len() < out_max * ch {
                self.scratch_out.resize(out_max * ch, 0.0);
            }

            let in_adapter = InterleavedSlice::new(&self.pending_in[..needed * ch], ch, needed)
                .map_err(|e| ResamplerError::Process(e.to_string()))?;
            let mut out_adapter =
                InterleavedSlice::new_mut(&mut self.scratch_out[..out_max * ch], ch, out_max)
                    .map_err(|e| ResamplerError::Process(e.to_string()))?;

            let (consumed, produced) = fft
                .process_into_buffer(&in_adapter, &mut out_adapter, None)
                .map_err(|e| ResamplerError::Process(e.to_string()))?;

            out.extend_from_slice(&self.scratch_out[..produced * ch]);
            self.pending_in.drain(..consumed * ch);
        }
        Ok(())
    }

    /// Flushes remaining partial frames at track EOF.
    pub fn flush_eof(&mut self, out: &mut Vec<f32>) -> Result<(), ResamplerError> {
        out.clear();
        let Some(ref mut fft) = self.inner else {
            return Ok(());
        };
        let ch = self.channels;
        let remaining_frames = self.pending_in.len() / ch;
        if remaining_frames == 0 {
            return Ok(());
        }

        let needed = fft.input_frames_next();
        self.pending_in.resize(needed * ch, 0.0);
        let out_max = fft.output_frames_max();
        if self.scratch_out.len() < out_max * ch {
            self.scratch_out.resize(out_max * ch, 0.0);
        }

        let in_adapter = InterleavedSlice::new(&self.pending_in[..needed * ch], ch, needed)
            .map_err(|e| ResamplerError::Process(e.to_string()))?;
        let mut out_adapter =
            InterleavedSlice::new_mut(&mut self.scratch_out[..out_max * ch], ch, out_max)
                .map_err(|e| ResamplerError::Process(e.to_string()))?;

        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: Some(remaining_frames),
            active_channels_mask: None,
        };
        let (_, produced) = fft
            .process_into_buffer(&in_adapter, &mut out_adapter, Some(&indexing))
            .map_err(|e| ResamplerError::Process(e.to_string()))?;

        let expected = ((remaining_frames as u64 * u64::from(self.out_rate))
            / u64::from(self.in_rate.max(1))) as usize;
        let emit = produced.min(expected.max(1));
        out.extend_from_slice(&self.scratch_out[..emit * ch]);
        self.pending_in.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    #[test]
    fn passthrough_when_rates_match_and_resamples_44100_to_48000() {
        let mut pt = AudioResampler::new(48_000, 48_000, 2).expect("passthrough");
        assert!(pt.is_passthrough());
        let src = vec![0.25_f32, -0.25, 0.5, -0.5];
        let mut dst = Vec::new();
        pt.process(&src, &mut dst).expect("process");
        assert_eq!(dst, src);

        let mut rs = AudioResampler::new(44_100, 48_000, 2).expect("44.1->48");
        assert!(!rs.is_passthrough());
        let in_frames = 44_100usize; // 1.0 s
        let mut sine = vec![0.0_f32; in_frames * 2];
        for f in 0..in_frames {
            let s = (0.5 * (2.0 * PI * 440.0 * f as f64 / 44_100.0).sin()) as f32;
            sine[f * 2] = s;
            sine[f * 2 + 1] = s;
        }
        let mut total_out = Vec::new();
        let mut chunk = Vec::new();
        rs.process(&sine, &mut chunk).expect("resample");
        total_out.extend_from_slice(&chunk);
        rs.flush_eof(&mut chunk).expect("flush");
        total_out.extend_from_slice(&chunk);

        let out_frames = total_out.len() / 2;
        assert!(
            (out_frames as isize - 48_000).abs() < 2_500,
            "out_frames={out_frames}"
        );
        assert!(total_out.iter().all(|s| s.is_finite()));
    }
}
