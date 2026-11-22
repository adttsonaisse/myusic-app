use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use bitchord_matcher::{SourceTrackKeys, StreamFormat};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

pub const LOW_QUALITY_MAX_KBPS: u32 = 80;
pub const HI_QUALITY_KBPS: u32 = 256;
const MAX_REMEMBERED: usize = 64;
const LOSSLESS_CODEC_SUFFIXES: &[&str] = &["flac", "alac", "raw"];

/// Provenance of reported audio bitrate telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TelemetryProvenance {
    Authoritative,
    Measured,
    Derived,
    #[default]
    Unknown,
}

/// Measured audio pipeline telemetry for the currently active track.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NerdStatsSnapshot {
    pub mime_type: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
    pub bit_depth: Option<u32>,
    pub claimed: Option<StreamFormat>,
    pub source_name: Option<String>,
    pub container: Option<String>,
    pub bitrate_provenance: TelemetryProvenance,
    pub pcm_data_rate_kbps: Option<u32>,
    pub media_id: Option<String>,
}

impl NerdStatsSnapshot {
    /// Whether the decoded audio stream is measurably worse than what the
    /// source advertised.
    pub fn downgraded(&self) -> bool {
        let Some(ref claimed) = self.claimed else {
            return false;
        };
        let rate_drop = claimed
            .sample_rate_hz
            .zip(self.sample_rate_hz)
            .is_some_and(|(wanted, actual)| actual < wanted);
        let depth_drop = claimed
            .bit_depth
            .zip(self.bit_depth)
            .is_some_and(|(wanted, actual)| actual < wanted);
        rate_drop || depth_drop
    }

    pub fn is_lossless(&self) -> bool {
        is_lossless_mime(self.mime_type.as_deref())
    }

    pub fn is_dolby_atmos(&self) -> bool {
        is_dolby_atmos_mime(self.mime_type.as_deref())
    }

    pub fn is_hi_res(&self) -> bool {
        self.is_lossless()
            && (self.bit_depth.unwrap_or(0) > 16 || self.sample_rate_hz.unwrap_or(0) > 48_000)
    }

    pub fn is_hi_quality(&self) -> bool {
        !self.is_lossless() && self.bitrate_kbps.unwrap_or(0) >= HI_QUALITY_KBPS
    }

    pub fn is_medium_quality(&self) -> bool {
        let kbps = self.bitrate_kbps.unwrap_or(0);
        !self.is_lossless() && kbps > LOW_QUALITY_MAX_KBPS && kbps < HI_QUALITY_KBPS
    }

    pub fn is_low_quality(&self) -> bool {
        let kbps = self.bitrate_kbps.unwrap_or(0);
        !self.is_lossless() && (1..=LOW_QUALITY_MAX_KBPS).contains(&kbps)
    }
}

pub fn is_lossless_mime(mime_type: Option<&str>) -> bool {
    let Some(mime) = mime_type else {
        return false;
    };
    LOSSLESS_CODEC_SUFFIXES.iter().any(|s| mime.ends_with(s)) || mime.starts_with("audio/dsd")
}

pub fn is_dolby_atmos_mime(mime_type: Option<&str>) -> bool {
    mime_type.is_some_and(|m| m.ends_with("eac3-joc") || m.ends_with("eac3"))
}

pub fn is_raw_pcm(mime_type: Option<&str>) -> bool {
    mime_type.is_some_and(|m| {
        m == "audio/raw"
            || m.eq_ignore_ascii_case("audio/x-wav")
            || m.to_ascii_lowercase().ends_with("wav")
    })
}

/// Formats rates ≥ 1 MHz (DSD) with exact decimal digits and no trailing zeros
/// (`2_822_400 -> "2.8224 MHz"`, `45_158_400 -> "45.1584 MHz"`). Returns `None`
/// below 1 MHz.
pub fn megahertz_label(hz: u32) -> Option<String> {
    if hz < 1_000_000 {
        return None;
    }
    let whole = hz / 1_000_000;
    let frac = hz % 1_000_000;
    if frac == 0 {
        return Some(format!("{whole} MHz"));
    }
    let frac_str = format!("{frac:06}");
    let trimmed = frac_str.trim_end_matches('0');
    Some(format!("{whole}.{trimmed} MHz"))
}

pub fn codec_label(mime_type: Option<&str>) -> Option<String> {
    let mime = mime_type?;
    let lower = mime.to_ascii_lowercase();
    let label = if lower.ends_with("flac") {
        "FLAC".to_owned()
    } else if lower.ends_with("opus") {
        "Opus".to_owned()
    } else if lower.ends_with("mp4a-latm") || lower.ends_with("aac") {
        "AAC".to_owned()
    } else if lower.ends_with("mpeg") || lower.ends_with("mp3") {
        "MP3".to_owned()
    } else if lower.ends_with("alac") {
        "ALAC".to_owned()
    } else if lower.ends_with("raw") {
        "PCM".to_owned()
    } else if lower.starts_with("audio/dsd") {
        mime.split_once('/')
            .map_or(mime, |(_, sub)| sub)
            .to_ascii_uppercase()
    } else if lower.ends_with("vorbis") {
        "Vorbis".to_owned()
    } else if lower.ends_with("eac3-joc") {
        "E-AC-3 JOC".to_owned()
    } else if lower.ends_with("eac3") {
        "E-AC-3".to_owned()
    } else if let Some((_, sub)) = mime.rsplit_once('/') {
        sub.to_ascii_uppercase()
    } else {
        mime.to_ascii_uppercase()
    };
    Some(label)
}

/// Process-scoped or player-scoped live telemetry store with generation gating
/// so stale crossfade/read-ahead snapshots never overwrite the active track.
pub struct NerdStats {
    generation: AtomicU64,
    active_media_id: RwLock<Option<String>>,
    current_tx: watch::Sender<Option<NerdStatsSnapshot>>,
    racing_lossless: RwLock<HashSet<String>>,
    picked: RwLock<HashMap<String, u32>>,
    declared: RwLock<HashMap<String, StreamFormat>>,
    sources: RwLock<HashMap<String, String>>,
}

impl Default for NerdStats {
    fn default() -> Self {
        Self::new()
    }
}

impl NerdStats {
    pub fn new() -> Self {
        let (current_tx, _) = watch::channel(None);
        Self {
            generation: AtomicU64::new(1),
            active_media_id: RwLock::new(None),
            current_tx,
            racing_lossless: RwLock::new(HashSet::new()),
            picked: RwLock::new(HashMap::new()),
            declared: RwLock::new(HashMap::new()),
            sources: RwLock::new(HashMap::new()),
        }
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn current(&self) -> Option<NerdStatsSnapshot> {
        self.current_tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<NerdStatsSnapshot>> {
        self.current_tx.subscribe()
    }

    pub fn publish(&self, snapshot: NerdStatsSnapshot, expected_generation: u64) -> bool {
        if expected_generation != self.current_generation() {
            return false;
        }
        let active = self
            .active_media_id
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let (Some(active), Some(snap_id)) = (active.as_deref(), snapshot.media_id.as_deref())
            && snap_id != active
        {
            return false;
        }
        let _ = self.current_tx.send(Some(snapshot));
        true
    }

    pub fn on_lossless_race_start(&self, video_id: &str) {
        self.racing_lossless
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(video_id.to_owned());
    }

    pub fn on_lossless_race_end(&self, video_id: &str) {
        self.racing_lossless
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(video_id);
    }

    pub fn on_stream_picked(&self, video_id: &str, kbps: u32, source: Option<&str>) {
        if kbps > 0 {
            let mut picked = self.picked.write().unwrap_or_else(|e| e.into_inner());
            if picked.len() >= MAX_REMEMBERED {
                picked.clear();
            }
            picked.insert(video_id.to_owned(), kbps);
        }
        if let Some(src) = source.filter(|s| !s.trim().is_empty()) {
            let mut sources = self.sources.write().unwrap_or_else(|e| e.into_inner());
            if sources.len() >= MAX_REMEMBERED {
                sources.clear();
            }
            sources.insert(video_id.to_owned(), src.to_owned());
        }
    }

    pub fn on_source_stream(&self, track_id: &str, format: StreamFormat, source: Option<&str>) {
        if track_id.trim().is_empty() {
            return;
        }
        {
            let mut declared = self.declared.write().unwrap_or_else(|e| e.into_inner());
            if declared.len() >= MAX_REMEMBERED {
                declared.clear();
            }
            declared.insert(track_id.to_owned(), format);
        }
        if let Some(src) = source.filter(|s| !s.trim().is_empty()) {
            let mut sources = self.sources.write().unwrap_or_else(|e| e.into_inner());
            if sources.len() >= MAX_REMEMBERED {
                sources.clear();
            }
            sources.insert(track_id.to_owned(), src.to_owned());
        }
    }

    pub fn declared_format(&self, media_id: Option<&str>) -> Option<StreamFormat> {
        let key = media_id?;
        let declared = self.declared.read().unwrap_or_else(|e| e.into_inner());
        if let Some(fmt) = declared.get(key) {
            return Some(fmt.clone());
        }
        SourceTrackKeys::parse(key).and_then(|(_, inner)| declared.get(inner).cloned())
    }

    pub fn source_for(&self, media_id: Option<&str>) -> Option<String> {
        let key = media_id?;
        let sources = self.sources.read().unwrap_or_else(|e| e.into_inner());
        if let Some(src) = sources.get(key) {
            return Some(src.clone());
        }
        SourceTrackKeys::parse(key).and_then(|(_, inner)| sources.get(inner).cloned())
    }

    pub fn clear_declared(&self, track_id: &str) {
        self.declared
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(track_id);
        self.sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(track_id);
    }

    pub fn on_track_transition(&self, media_id: Option<String>) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self
            .active_media_id
            .write()
            .unwrap_or_else(|e| e.into_inner()) = media_id;
        let _ = self.current_tx.send(None);
    }

    pub fn forget_last_session(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self
            .active_media_id
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let _ = self.current_tx.send(None);
        self.racing_lossless
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.picked
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.declared
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
