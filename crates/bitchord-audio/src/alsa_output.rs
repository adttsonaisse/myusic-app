//! Direct bit-perfect ALSA `hw:card,dev` exclusive output backend and Linux
//! USB DAC / sound-card enumerator (`Wave 5B`).
//!
//! Opens the hardware device directly (`hw:X,Y`, bypassing `dmix`/OS resamplers)
//! at the track's exact native sample rate (`44.1 kHz` through `384 kHz`) and
//! highest bit-exact PCM format (`S32_LE`, `S24_3LE`, `FLOAT_LE`, or `S16_LE`).
//! If the device refuses the exact rate or is currently held (`EBUSY`), returns
//! an explicit [`OutputError::Alsa`] so the caller never advertises a false
//! bit-perfect badge.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use serde::{Deserialize, Serialize};

use crate::output::{OUTPUT_CHANNELS, OutputError, RtMixerState, SinkProducers};

/// Standard hi-res PCM sample rates probed on ALSA hardware devices.
pub const PROBE_SAMPLE_RATES: &[u32] = &[
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

/// Period target for low-jitter ALSA exclusive writes (20 ms).
const PERIOD_TARGET_MS: u32 = 20;
/// Buffer target (~4 periods = 80 ms) balancing responsiveness and xrun safety.
const BUFFER_TARGET_MS: u32 = 80;

/// Hardware PCM sample format negotiated with an ALSA `hw:` device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AlsaSampleFormat {
    S32Le,
    S243Le,
    FloatLe,
    S16Le,
}

impl AlsaSampleFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::S32Le => "S32_LE",
            Self::S243Le => "S24_3LE",
            Self::FloatLe => "FLOAT_LE",
            Self::S16Le => "S16_LE",
        }
    }

    pub const fn bits_per_sample(self) -> u32 {
        match self {
            Self::S32Le | Self::FloatLe => 32,
            Self::S243Le => 24,
            Self::S16Le => 16,
        }
    }

    pub const fn to_alsa(self) -> Format {
        match self {
            Self::S32Le => Format::S32LE,
            Self::S243Le => Format::S243LE,
            Self::FloatLe => Format::FloatLE,
            Self::S16Le => Format::S16LE,
        }
    }

    /// Format negotiation priority given a source track's preferred bit depth.
    pub fn preference_order(preferred_bits: Option<u32>) -> [Self; 4] {
        match preferred_bits {
            Some(16) => [Self::S16Le, Self::S32Le, Self::S243Le, Self::FloatLe],
            Some(24) => [Self::S32Le, Self::S243Le, Self::FloatLe, Self::S16Le],
            _ => [Self::S32Le, Self::FloatLe, Self::S243Le, Self::S16Le],
        }
    }
}

/// Discovered Linux ALSA playback hardware endpoint (`hw:card,dev`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlsaDeviceInfo {
    /// ALSA hardware identifier (e.g. `"hw:0,0"`, `"hw:1,0"`).
    pub hw_id: String,
    pub card: u32,
    pub device: u32,
    pub card_name: String,
    pub device_name: String,
    pub display_name: String,
    /// `true` when sysfs/proc reports the sound card lives on the USB bus (external DAC/dongle).
    pub is_usb_dac: bool,
    /// Optional USB `VID:PID` read from `/proc/asound/cardN/usbid`.
    pub usb_id: Option<String>,
    /// Verified hardware sample rates (empty when device was busy during probe).
    pub supported_rates: Vec<u32>,
    /// Verified hardware PCM formats (empty when device was busy during probe).
    pub supported_formats: Vec<AlsaSampleFormat>,
    /// `true` if another daemon (such as PipeWire/PulseAudio) currently holds the device.
    pub busy: bool,
}

/// Enumerates all Linux ALSA playback hardware devices (`hw:card,dev`), detecting
/// USB Audio DACs via sysfs and probing supported rates/formats non-blockingly.
pub fn enumerate_alsa_devices() -> Vec<AlsaDeviceInfo> {
    let cards_text = fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    let pcm_text = fs::read_to_string("/proc/asound/pcm").unwrap_or_default();
    let card_names = parse_proc_asound_cards(&cards_text);
    let mut devices = parse_proc_asound_pcm(&pcm_text, &card_names);

    for dev in &mut devices {
        let (is_usb, usb_id) = detect_usb_sound_card(dev.card);
        dev.is_usb_dac = is_usb;
        dev.usb_id = usb_id;
        if is_usb {
            dev.display_name = format!("{} (USB DAC · {})", dev.display_name, dev.hw_id);
        } else {
            dev.display_name = format!("{} ({})", dev.display_name, dev.hw_id);
        }
        probe_device_capabilities(dev);
    }

    // Surface external USB DACs first, then analog/internal cards in numeric order.
    devices.sort_by_key(|d| (!d.is_usb_dac, d.card, d.device));
    devices
}

/// Parses `/proc/asound/cards` into `card_index -> friendly_card_name`.
pub fn parse_proc_asound_cards(raw: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let Some((num_part, rest)) = trimmed.split_once('[') else {
            continue;
        };
        let Ok(card_idx) = num_part.trim().parse::<u32>() else {
            continue;
        };
        let name = rest
            .split_once("]:")
            .and_then(|(_, after)| {
                after
                    .split_once(" - ")
                    .map(|(_, long)| long)
                    .or(Some(after))
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| rest.split(']').next().unwrap_or("ALSA Card").trim());
        map.insert(card_idx, name.to_owned());
    }
    map
}

/// Parses `/proc/asound/pcm` playback rows into [`AlsaDeviceInfo`] descriptors.
pub fn parse_proc_asound_pcm(raw: &str, card_names: &HashMap<u32, String>) -> Vec<AlsaDeviceInfo> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((id_part, rest)) = trimmed.split_once(':') else {
            continue;
        };
        let Some((card_str, dev_str)) = id_part.trim().split_once('-') else {
            continue;
        };
        let (Ok(card), Ok(device)) = (card_str.parse::<u32>(), dev_str.parse::<u32>()) else {
            continue;
        };
        // Only include devices that expose at least one playback subdevice.
        let lower_rest = rest.to_ascii_lowercase();
        if !lower_rest.contains("playback") {
            continue;
        }
        let device_name = rest
            .split(':')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("PCM")
            .to_owned();
        let card_name = card_names
            .get(&card)
            .cloned()
            .unwrap_or_else(|| format!("Card {card}"));
        let display_name = if device_name.eq_ignore_ascii_case(&card_name) {
            card_name.clone()
        } else {
            format!("{card_name} - {device_name}")
        };
        out.push(AlsaDeviceInfo {
            hw_id: format!("hw:{card},{device}"),
            card,
            device,
            card_name,
            device_name,
            display_name,
            is_usb_dac: false,
            usb_id: None,
            supported_rates: Vec::new(),
            supported_formats: Vec::new(),
            busy: false,
        });
    }
    out
}

fn detect_usb_sound_card(card: u32) -> (bool, Option<String>) {
    let usbid_path = format!("/proc/asound/card{card}/usbid");
    let usb_id = fs::read_to_string(&usbid_path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    let subsystem_path = format!("/sys/class/sound/card{card}/device/subsystem");
    let sysfs_usb = fs::read_link(Path::new(&subsystem_path))
        .ok()
        .is_some_and(|target| {
            target
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("usb")
        });

    (usb_id.is_some() || sysfs_usb, usb_id)
}

fn probe_device_capabilities(dev: &mut AlsaDeviceInfo) {
    let pcm = match PCM::new(&dev.hw_id, Direction::Playback, true) {
        Ok(p) => p,
        Err(err) => {
            let msg = err.to_string().to_ascii_lowercase();
            if msg.contains("busy") || msg.contains("ebusy") || err.errno() == 16 {
                dev.busy = true;
            }
            return;
        }
    };
    let Ok(hwp) = HwParams::any(&pcm) else {
        return;
    };
    for &rate in PROBE_SAMPLE_RATES {
        if hwp.test_rate(rate).is_ok() {
            dev.supported_rates.push(rate);
        }
    }
    for fmt in [
        AlsaSampleFormat::S32Le,
        AlsaSampleFormat::S243Le,
        AlsaSampleFormat::FloatLe,
        AlsaSampleFormat::S16Le,
    ] {
        if hwp.test_format(fmt.to_alsa()).is_ok() {
            dev.supported_formats.push(fmt);
        }
    }
}

/// Exclusive direct-hardware ALSA `hw:card,dev` playback sink running on its own RT OS thread.
pub struct AlsaExclusiveSink {
    device: String,
    sample_rate: u32,
    format: AlsaSampleFormat,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AlsaExclusiveSink {
    /// Opens `device` (e.g. `"hw:1,0"`) exclusively at exact `sample_rate`, bypassing OS mixing.
    pub fn spawn(
        device: &str,
        sample_rate: u32,
        preferred_bit_depth: Option<u32>,
    ) -> Result<(Self, SinkProducers), OutputError> {
        let trimmed = device.trim();
        if trimmed.is_empty() {
            return Err(OutputError::Alsa(
                "ALSA exclusive device identifier is empty (expected e.g. hw:0,0)".to_owned(),
            ));
        }

        let (rt_state, producers) = RtMixerState::new_pair(sample_rate);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let dev_owned = trimmed.to_owned();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<AlsaSampleFormat, String>>(1);

        let thread = thread::Builder::new()
            .name("bitchord-alsa-rt".to_owned())
            .spawn(move || {
                run_alsa_loop(
                    &dev_owned,
                    sample_rate,
                    preferred_bit_depth,
                    rt_state,
                    stop_thread,
                    ready_tx,
                );
            })?;

        match ready_rx.recv() {
            Ok(Ok(format)) => Ok((
                Self {
                    device: trimmed.to_owned(),
                    sample_rate,
                    format,
                    stop,
                    thread: Some(thread),
                },
                producers,
            )),
            Ok(Err(msg)) => {
                let _ = thread.join();
                Err(OutputError::Alsa(msg))
            }
            Err(_) => {
                let _ = thread.join();
                Err(OutputError::Alsa(
                    "ALSA audio thread exited before completing initialization".to_owned(),
                ))
            }
        }
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn format(&self) -> AlsaSampleFormat {
        self.format
    }
}

impl Drop for AlsaExclusiveSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn configure_alsa_pcm(
    pcm: &PCM,
    sample_rate: u32,
    preferred_bit_depth: Option<u32>,
) -> Result<(AlsaSampleFormat, usize), String> {
    let mut last_err = String::new();
    for fmt in AlsaSampleFormat::preference_order(preferred_bit_depth) {
        let hwp = match HwParams::any(pcm) {
            Ok(h) => h,
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        if let Err(e) = hwp.set_access(Access::RWInterleaved) {
            last_err = e.to_string();
            continue;
        }
        if let Err(e) = hwp.set_format(fmt.to_alsa()) {
            last_err = e.to_string();
            continue;
        }
        if let Err(e) = hwp.set_channels(OUTPUT_CHANNELS as u32) {
            last_err = e.to_string();
            continue;
        }
        if let Err(e) = hwp.set_rate(sample_rate, ValueOr::Nearest) {
            last_err = e.to_string();
            continue;
        }
        let actual_rate = hwp.get_rate().unwrap_or(0);
        if actual_rate != sample_rate {
            last_err =
                format!("hardware rejected exact rate {sample_rate} Hz (offered {actual_rate} Hz)");
            continue;
        }
        let target_period = ((u64::from(sample_rate) * u64::from(PERIOD_TARGET_MS)) / 1_000)
            .clamp(128, 4_096) as i64;
        let target_buffer = ((u64::from(sample_rate) * u64::from(BUFFER_TARGET_MS)) / 1_000)
            .clamp(target_period as u64 * 2, 32_768);
        let _ = hwp.set_period_size_near(target_period, ValueOr::Nearest);
        let _ = hwp.set_buffer_size_near(target_buffer as alsa::pcm::Frames);

        if let Err(e) = pcm.hw_params(&hwp) {
            last_err = e.to_string();
            continue;
        }
        let period_frames = hwp
            .get_period_size()
            .map(|f| (f as usize).clamp(64, 4_096))
            .unwrap_or(target_period as usize);
        if let Err(e) = pcm.prepare() {
            last_err = e.to_string();
            continue;
        }
        return Ok((fmt, period_frames));
    }
    Err(format!(
        "failed to configure ALSA exclusive stream at {sample_rate} Hz: {last_err}"
    ))
}

fn run_alsa_loop(
    device: &str,
    sample_rate: u32,
    preferred_bit_depth: Option<u32>,
    mut rt_state: RtMixerState,
    stop: Arc<AtomicBool>,
    ready_tx: std_mpsc::SyncSender<Result<AlsaSampleFormat, String>>,
) {
    let pcm = match PCM::new(device, Direction::Playback, false) {
        Ok(p) => p,
        Err(err) => {
            let detail = if err.errno() == 16 {
                format!(
                    "ALSA device {device} is busy (another audio server such as PipeWire holds it)"
                )
            } else {
                format!("cannot open ALSA device {device}: {err}")
            };
            let _ = ready_tx.send(Err(detail));
            return;
        }
    };

    let (fmt, period_frames) = match configure_alsa_pcm(&pcm, sample_rate, preferred_bit_depth) {
        Ok(pair) => pair,
        Err(msg) => {
            let _ = ready_tx.send(Err(msg));
            return;
        }
    };

    if ready_tx.send(Ok(fmt)).is_err() {
        return;
    }

    let sample_count = period_frames * OUTPUT_CHANNELS;
    let mut f32_buf = vec![0.0_f32; sample_count];
    let mut i32_buf = vec![0_i32; sample_count];
    let mut i16_buf = vec![0_i16; sample_count];
    let mut s24_buf = vec![0_u8; sample_count * 3];

    while !stop.load(Ordering::Acquire) {
        rt_state.render_interleaved_f32(&mut f32_buf);

        let write_res = match fmt {
            AlsaSampleFormat::S32Le => {
                convert_f32_to_s32le(&f32_buf, &mut i32_buf);
                pcm.io_i32().and_then(|io| io.writei(&i32_buf))
            }
            AlsaSampleFormat::S16Le => {
                convert_f32_to_s16le(&f32_buf, &mut i16_buf);
                pcm.io_i16().and_then(|io| io.writei(&i16_buf))
            }
            AlsaSampleFormat::FloatLe => pcm.io_f32().and_then(|io| io.writei(&f32_buf)),
            AlsaSampleFormat::S243Le => {
                convert_f32_to_s24_3le(&f32_buf, &mut s24_buf);
                pcm.io_bytes().writei(&s24_buf)
            }
        };

        if let Err(err) = write_res
            && pcm.try_recover(err, true).is_err()
        {
            // Hardware unplugged or fatally stalled; avoid hot spinning.
            thread::sleep(Duration::from_millis(20));
        }
    }

    let _ = pcm.drop();
}

/// Bit-exact `f32` (`[-1.0, 1.0]`) to signed 32-bit PCM (`S32_LE`) conversion.
///
/// Exact for every 16-bit and 24-bit integer sample represented in `f32` mantissa (24 bits).
pub fn convert_f32_to_s32le(src: &[f32], dst: &mut [i32]) {
    const SCALE: f64 = 2_147_483_648.0; // 2^31
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        let clamped = if s.is_finite() {
            f64::from(*s).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let scaled = (clamped * SCALE).round();
        *d = scaled.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    }
}

/// Bit-exact `f32` (`[-1.0, 1.0]`) to packed 24-bit signed little-endian PCM (`S24_3LE`).
pub fn convert_f32_to_s24_3le(src: &[f32], dst: &mut [u8]) {
    const SCALE: f64 = 8_388_608.0; // 2^23
    for (idx, s) in src.iter().enumerate() {
        let clamped = if s.is_finite() {
            f64::from(*s).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let v = (clamped * SCALE).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
        let base = idx * 3;
        if base + 2 < dst.len() {
            dst[base] = (v & 0xFF) as u8;
            dst[base + 1] = ((v >> 8) & 0xFF) as u8;
            dst[base + 2] = ((v >> 16) & 0xFF) as u8;
        }
    }
}

/// Bit-exact `f32` (`[-1.0, 1.0]`) to signed 16-bit PCM (`S16_LE`) conversion.
pub fn convert_f32_to_s16le(src: &[f32], dst: &mut [i16]) {
    const SCALE: f32 = 32_768.0;
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        let clamped = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        *d = (clamped * SCALE)
            .round()
            .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_asound_cards_and_pcm_playback_devices() {
        let cards_raw = r#"
 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                      HDA Intel PCH at 0x6001150000 irq 163
 1 [E30II          ]: USB-Audio - Topping E30 II
                      Topping E30 II at usb-0000:00:14.0-2, high speed
"#;
        let pcm_raw = r#"
00-00: SN6140 Analog : SN6140 Analog : playback 1 : capture 1
00-03: HDMI 0 : HDMI 0 : playback 1
01-00: USB Audio : USB Audio : playback 1
01-01: Mic In : Mic In : capture 1
"#;
        let cards = parse_proc_asound_cards(cards_raw);
        assert_eq!(cards.get(&0).map(String::as_str), Some("HDA Intel PCH"));
        assert_eq!(cards.get(&1).map(String::as_str), Some("Topping E30 II"));

        let devs = parse_proc_asound_pcm(pcm_raw, &cards);
        assert_eq!(devs.len(), 3); // capture-only 01-01 is excluded!
        assert_eq!(devs[0].hw_id, "hw:0,0");
        assert_eq!(devs[1].hw_id, "hw:0,3");
        assert_eq!(devs[2].hw_id, "hw:1,0");
        assert_eq!(devs[2].card_name, "Topping E30 II");
    }

    #[test]
    fn bit_exact_pcm_conversions_preserve_16bit_and_24bit_integer_values() {
        // Exact 16-bit round-trip: i16 -> f32 -> S16_LE
        let samples_i16 = [i16::MIN, -16384, -1, 0, 1, 16384, i16::MAX];
        let as_f32: Vec<f32> = samples_i16
            .iter()
            .map(|&x| f32::from(x) / 32768.0)
            .collect();
        let mut out_i16 = vec![0i16; samples_i16.len()];
        convert_f32_to_s16le(&as_f32, &mut out_i16);
        assert_eq!(out_i16, samples_i16);

        // Exact 24-bit round-trip via S24_3LE and S32_LE
        let samples_i24 = [-8_388_608i32, -123_456, 0, 123_456, 8_388_607];
        let as_f32_24: Vec<f32> = samples_i24
            .iter()
            .map(|&x| (x as f64 / 8_388_608.0) as f32)
            .collect();
        let mut out_s24 = vec![0u8; samples_i24.len() * 3];
        convert_f32_to_s24_3le(&as_f32_24, &mut out_s24);
        for (i, &expected) in samples_i24.iter().enumerate() {
            let b0 = i32::from(out_s24[i * 3]);
            let b1 = i32::from(out_s24[i * 3 + 1]) << 8;
            let b2 = i32::from(out_s24[i * 3 + 2]) << 16;
            let raw = b0 | b1 | b2;
            let signed = if (raw & 0x80_0000) != 0 {
                raw | !0xFF_FFFF
            } else {
                raw
            };
            assert_eq!(signed, expected);
        }

        let mut out_s32 = vec![0i32; samples_i24.len()];
        convert_f32_to_s32le(&as_f32_24, &mut out_s32);
        for (i, &expected) in samples_i24.iter().enumerate() {
            assert_eq!(out_s32[i] >> 8, expected);
        }
    }
}
