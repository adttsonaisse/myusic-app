use serde::{Deserialize, Serialize};

const LOSSLESS_CODECS: &[&str] = &["flac", "alac", "wav", "aiff", "ape", "wv", "dsf", "dff"];
const DOLBY_ATMOS_CODECS: &[&str] = &["eac3-joc", "ec3-joc", "dolby-atmos"];

/// What a source declares it is about to serve before the decoder measures the stream.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamFormat {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub kbps: Option<u32>,
    #[serde(default)]
    pub sample_rate_hz: Option<u32>,
    #[serde(default)]
    pub bit_depth: Option<u32>,
    #[serde(default)]
    pub kbps_inferred: bool,
}

impl StreamFormat {
    /// Decided strictly on codec: `Some(true)` for lossless codecs, `Some(false)` for lossy,
    /// `None` when codec is unknown.
    pub fn is_lossless(&self) -> Option<bool> {
        self.codec.as_deref().map(|c| LOSSLESS_CODECS.contains(&c))
    }

    /// Dolby Atmos carried in E-AC-3 JOC (immersive, not lossless).
    pub fn is_dolby_atmos(&self) -> bool {
        self.codec
            .as_deref()
            .is_some_and(|c| DOLBY_ATMOS_CODECS.contains(&c))
    }

    /// Human-readable summary such as `"FLAC · 24-bit · 192 kHz"` or `"MP3 · 44.1 kHz · 320 kbps"`.
    pub fn summary(&self) -> String {
        if self.is_dolby_atmos() {
            return "Dolby Atmos".to_owned();
        }
        let mut parts = Vec::with_capacity(4);
        if let Some(codec) = &self.codec {
            parts.push(codec.to_ascii_uppercase());
        }
        if let Some(depth) = self.bit_depth {
            parts.push(format!("{depth}-bit"));
        }
        if let Some(rate) = self.sample_rate_hz {
            let khz = format!("{:.1}", (rate as f32) / 1000.0);
            let khz = khz.strip_suffix(".0").unwrap_or(&khz);
            parts.push(format!("{khz} kHz"));
        }
        if let Some(kbps) = self.kbps
            && self.is_lossless() != Some(true)
        {
            parts.push(format!("{kbps} kbps"));
        }
        if parts.is_empty() {
            "Unknown format".to_owned()
        } else {
            parts.join(" · ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_and_summary_match_upstream_tests() {
        assert_eq!(
            Some(true),
            StreamFormat {
                codec: Some("flac".to_owned()),
                kbps: Some(900),
                ..StreamFormat::default()
            }
            .is_lossless()
        );
        assert_eq!(
            Some(true),
            StreamFormat {
                codec: Some("alac".to_owned()),
                ..StreamFormat::default()
            }
            .is_lossless()
        );
        assert_eq!(
            Some(false),
            StreamFormat {
                codec: Some("opus".to_owned()),
                sample_rate_hz: Some(192_000),
                ..StreamFormat::default()
            }
            .is_lossless()
        );
        assert_eq!(None, StreamFormat::default().is_lossless());

        let hi_res = StreamFormat {
            codec: Some("flac".to_owned()),
            kbps: Some(4608),
            sample_rate_hz: Some(192_000),
            bit_depth: Some(24),
            kbps_inferred: false,
        };
        assert_eq!("FLAC · 24-bit · 192 kHz", hi_res.summary());

        let lossy = StreamFormat {
            codec: Some("mp3".to_owned()),
            kbps: Some(320),
            sample_rate_hz: Some(44_100),
            ..StreamFormat::default()
        };
        assert_eq!("MP3 · 44.1 kHz · 320 kbps", lossy.summary());
        assert_eq!("Unknown format", StreamFormat::default().summary());

        let atmos = StreamFormat {
            codec: Some("eac3-joc".to_owned()),
            ..StreamFormat::default()
        };
        assert!(atmos.is_dolby_atmos());
        assert_eq!(Some(false), atmos.is_lossless());
        assert_eq!("Dolby Atmos", atmos.summary());
    }
}
