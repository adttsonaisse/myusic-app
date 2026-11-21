use serde::{Deserialize, Serialize};

/// CPU budget for Automix's background analysis (v2), not its audible mix algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AutomixPerformanceMode {
    Efficient,
    #[default]
    Balanced,
    Performance,
}

impl AutomixPerformanceMode {
    pub const fn inference_threads(self) -> usize {
        match self {
            Self::Efficient => 1,
            Self::Balanced => 2,
            Self::Performance => 4,
        }
    }

    pub const fn yields_to_playback(self) -> bool {
        matches!(self, Self::Efficient)
    }
}

/// Where one track stands in Automix's analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TrackAnalysisState {
    #[default]
    Waiting,
    Analysing,
    Analysed,
    Refining,
    Failed,
}

/// Both sides of the next transition, for Stats for Nerds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartAnalysis {
    pub current: TrackAnalysisState,
    pub next: TrackAnalysisState,
}

/// Span of the playing track (in fractions `0.0..=1.0`) occupied by the next transition.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionWindow {
    pub start: f32,
    pub end: f32,
}

/// An Automix blend in flight as drawn on the scrubber.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixBlend {
    pub beat_ms: f32,
    pub beat_anchor_nanos: u64,
    pub playing: bool,
}

/// One side of an Automix blend for the mix popup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixDeck {
    pub title: String,
    pub artist: String,
    pub artwork_url: Option<String>,
    pub bpm: f64,
    pub key: String,
    pub duration_ms: i64,
    pub wave_start: f64,
    pub wave_end: f64,
    #[serde(default)]
    pub low: Vec<f32>,
    #[serde(default)]
    pub mid: Vec<f32>,
    #[serde(default)]
    pub high: Vec<f32>,
    #[serde(default)]
    pub analysing: bool,
    #[serde(default)]
    pub decoding: bool,
}

impl MixDeck {
    pub fn with_wave(
        mut self,
        low: Vec<f32>,
        mid: Vec<f32>,
        high: Vec<f32>,
        start: f64,
        end: f64,
    ) -> Self {
        self.low = low;
        self.mid = mid;
        self.high = high;
        self.wave_start = start;
        self.wave_end = end;
        self
    }

    pub fn with_decoding(mut self, decoding: bool) -> Self {
        self.decoding = decoding;
        self
    }
}

/// An Automix blend laid out for the mix popup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MixTransitionView {
    pub outgoing: MixDeck,
    pub incoming: MixDeck,
    pub out_start: f64,
    pub out_end: f64,
    pub in_start: f64,
    pub in_end: f64,
    pub beats: i32,
    pub beat_marks: Vec<f32>,
    pub fade_out: Vec<f32>,
    pub fade_in: Vec<f32>,
    pub bass_swap_at: f32,
}

impl MixTransitionView {
    pub fn with_decks(mut self, outgoing: MixDeck, incoming: MixDeck) -> Self {
        self.outgoing = outgoing;
        self.incoming = incoming;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibrarySort {
    #[default]
    Default,
    TitleAsc,
    TitleDesc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibraryViewType {
    #[default]
    List,
    Grid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AudioQuality {
    Low,
    Medium,
    #[default]
    High,
    Lossless,
}

impl AudioQuality {
    pub const fn max_kbps(self) -> u32 {
        match self {
            Self::Low => 64,
            Self::Medium | Self::High | Self::Lossless => u32::MAX,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Lossless => "Lossless",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Low => "~64 kbps · smallest download",
            Self::Medium => "Best available · ~171 kbps Opus",
            Self::High => "JioSaavn up to 320kbps, YouTube fallback",
            Self::Lossless => "Your addons + JioSaavn, bit-exact where available",
        }
    }

    pub const fn hourly(self) -> &'static str {
        match self {
            Self::Low => "29 MB/hr",
            Self::Medium => "77 MB/hr",
            Self::High => "144 MB/hr",
            Self::Lossless => "300+ MB/hr",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlayerTheme {
    #[default]
    Default,
    Legacy,
    Simple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LastPlayerScreen {
    #[default]
    Main,
    Lyrics,
    Queue,
}
