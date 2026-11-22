//! JSON-persisted XDG application settings with atomic temp-file writes.
//!
//! Covers desktop-relevant preferences from upstream `AppSettings.kt`, keeping
//! download quality independent from streaming ceilings (`DownloadStoreTest.kt`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use bitchord_lyrics::LyricsSource;
use bitchord_model::{AudioQuality, LastPlayerScreen, LibrarySort, LibraryViewType, PlayerTheme};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Default allowed duration slack when upgrading a stream to another source.
pub const DEFAULT_UPGRADE_LENGTH_SLACK_SECONDS: u32 = 8;

/// PCM format requested from the audio output sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OutputPcmMode {
    #[default]
    Pcm16,
    Float32,
}

impl OutputPcmMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pcm16 => "16-bit PCM",
            Self::Float32 => "32-bit float",
        }
    }
}

/// Download quality tier, kept independent from streaming ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DownloadQuality {
    Standard,
    High,
    #[default]
    Lossless,
}

impl DownloadQuality {
    /// Ceiling for the YouTube Opus ladder (`i32::MAX` means uncapped / best available).
    pub const fn max_kbps(self) -> i32 {
        match self {
            Self::Standard => 128,
            Self::High | Self::Lossless => i32::MAX,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::High => "High",
            Self::Lossless => "Lossless",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Standard => "~128 kbps Opus · fits more on the device",
            Self::High => "Best audio on offer; source quality first",
            Self::Lossless => "Bit-exact if a source has it, best Opus if not",
        }
    }

    pub const fn per_track(self) -> &'static str {
        match self {
            Self::Standard => "~4 MB",
            Self::High => "~8 MB",
            Self::Lossless => "~35 MB",
        }
    }

    pub const fn keeps_lossless(self) -> bool {
        matches!(self, Self::Lossless)
    }

    pub const ALL: [Self; 3] = [Self::Standard, Self::High, Self::Lossless];
}

/// Stream quality request derived from a [`DownloadQuality`] tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRequest {
    Lossless,
    Best,
    Capped(u32),
}

/// Maps a [`DownloadQuality`] tier to its corresponding [`StreamRequest`].
///
/// Mirrors `SourceResolver.requestForDownload` tested in `DownloadStoreTest.kt`.
pub const fn request_for_download(quality: DownloadQuality) -> StreamRequest {
    match quality {
        DownloadQuality::Lossless => StreamRequest::Lossless,
        DownloadQuality::High => StreamRequest::Best,
        DownloadQuality::Standard => StreamRequest::Capped(128),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AppFont {
    #[default]
    Default,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EqualizerMode {
    #[default]
    Dynamic,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LocalMusicSort {
    #[default]
    TitleAsc,
    TitleDesc,
    DateAdded,
    DateModified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SongSort {
    #[default]
    Default,
    TitleAsc,
    TitleDesc,
    DateAddedAsc,
    DateAddedDesc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AudioOutputPreference {
    #[default]
    PipeWireAuto,
    PipeWireForceRate,
    AlsaExclusive,
}

impl AudioOutputPreference {
    pub const ALL: [Self; 3] = [
        Self::PipeWireAuto,
        Self::PipeWireForceRate,
        Self::AlsaExclusive,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::PipeWireAuto => "PipeWire Auto (48 kHz)",
            Self::PipeWireForceRate => "PipeWire Force-Rate",
            Self::AlsaExclusive => "ALSA Exclusive (hw:)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavSettings {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmbSettings {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub share: String,
    #[serde(default)]
    pub base_path: String,
    #[serde(default)]
    pub username: String,
}

fn default_true() -> bool {
    true
}

fn default_volume() -> f32 {
    1.0
}

fn default_upgrade_slack() -> u32 {
    DEFAULT_UPGRADE_LENGTH_SLACK_SECONDS
}

fn default_lyrics_order() -> Vec<LyricsSource> {
    LyricsSource::OFFERED.to_vec()
}

fn default_lastfm_endpoint() -> String {
    "https://ws.audioscrobbler.com/2.0/".to_owned()
}

fn default_scrobble_delay_percent() -> f32 {
    0.5
}

fn default_scrobble_delay_seconds() -> u32 {
    180
}

fn default_scrobble_min_duration() -> u32 {
    30
}

fn default_device_id() -> String {
    Uuid::new_v4().to_string()
}

/// Complete serializable desktop configuration stored at `<config_dir>/settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsData {
    #[serde(default)]
    pub audio_quality_wifi: AudioQuality,
    #[serde(default)]
    pub audio_quality_cellular: AudioQuality,
    #[serde(default = "default_upgrade_slack")]
    pub upgrade_length_slack_seconds: u32,
    #[serde(default)]
    pub download_quality: DownloadQuality,
    #[serde(default = "default_true")]
    pub wifi_only_downloads: bool,
    #[serde(default)]
    pub export_downloads: bool,
    #[serde(default)]
    pub auto_sync_playlists: bool,
    #[serde(default)]
    pub output_pcm_mode: OutputPcmMode,
    #[serde(default)]
    pub audio_output_mode: AudioOutputPreference,
    #[serde(default)]
    pub alsa_exclusive_device: String,

    // Playback & DSP
    #[serde(default = "default_true")]
    pub gapless_playback: bool,
    #[serde(default)]
    pub crossfade_seconds: u32,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub replay_gain_enabled: bool,
    #[serde(default)]
    pub equalizer_mode: EqualizerMode,

    // Appearance & Library
    #[serde(default)]
    pub theme_mode: ThemeMode,
    #[serde(default)]
    pub player_theme: PlayerTheme,
    #[serde(default)]
    pub app_font: AppFont,
    #[serde(default)]
    pub last_player_screen: LastPlayerScreen,
    #[serde(default)]
    pub library_sort: LibrarySort,
    #[serde(default)]
    pub library_view_type: LibraryViewType,
    #[serde(default)]
    pub local_music_sort: LocalMusicSort,
    #[serde(default)]
    pub song_sort: SongSort,
    #[serde(default)]
    pub local_music_folders: Vec<PathBuf>,

    // Lyrics
    #[serde(default = "default_lyrics_order")]
    pub lyrics_source_order: Vec<LyricsSource>,
    #[serde(default)]
    pub lyrics_translation_enabled: bool,
    #[serde(default)]
    pub lyrics_target_language: Option<String>,

    // Listen Together
    #[serde(default)]
    pub listen_together_server: String,
    #[serde(default)]
    pub listen_together_nickname: String,
    #[serde(default = "default_device_id")]
    pub device_id: String,

    // Discord & Scrobbling
    #[serde(default = "default_true")]
    pub discord_rpc_enabled: bool,
    #[serde(default = "default_true")]
    pub discord_show_audio_quality: bool,
    #[serde(default)]
    pub lastfm_enabled: bool,
    #[serde(default = "default_lastfm_endpoint")]
    pub lastfm_endpoint: String,
    #[serde(default)]
    pub lastfm_username: Option<String>,
    #[serde(default)]
    pub listenbrainz_enabled: bool,
    #[serde(default = "default_scrobble_delay_percent")]
    pub scrobble_delay_percent: f32,
    #[serde(default = "default_scrobble_delay_seconds")]
    pub scrobble_delay_seconds: u32,
    #[serde(default = "default_scrobble_min_duration")]
    pub scrobble_min_song_duration_secs: u32,

    // Network Storage (non-secret endpoints; passwords live in Linux Secret Service)
    #[serde(default)]
    pub webdav: WebDavSettings,
    #[serde(default)]
    pub smb: SmbSettings,
}

impl Default for AppSettingsData {
    fn default() -> Self {
        Self {
            audio_quality_wifi: AudioQuality::Lossless,
            audio_quality_cellular: AudioQuality::Lossless,
            upgrade_length_slack_seconds: DEFAULT_UPGRADE_LENGTH_SLACK_SECONDS,
            download_quality: DownloadQuality::Lossless,
            wifi_only_downloads: true,
            export_downloads: false,
            auto_sync_playlists: false,
            output_pcm_mode: OutputPcmMode::Pcm16,
            audio_output_mode: AudioOutputPreference::PipeWireAuto,
            alsa_exclusive_device: String::new(),
            gapless_playback: true,
            crossfade_seconds: 0,
            volume: 1.0,
            muted: false,
            replay_gain_enabled: false,
            equalizer_mode: EqualizerMode::Dynamic,
            theme_mode: ThemeMode::System,
            player_theme: PlayerTheme::Default,
            app_font: AppFont::Default,
            last_player_screen: LastPlayerScreen::Main,
            library_sort: LibrarySort::Default,
            library_view_type: LibraryViewType::List,
            local_music_sort: LocalMusicSort::TitleAsc,
            song_sort: SongSort::Default,
            local_music_folders: Vec::new(),
            lyrics_source_order: default_lyrics_order(),
            lyrics_translation_enabled: false,
            lyrics_target_language: None,
            listen_together_server: String::new(),
            listen_together_nickname: String::new(),
            device_id: default_device_id(),
            discord_rpc_enabled: true,
            discord_show_audio_quality: true,
            lastfm_enabled: false,
            lastfm_endpoint: default_lastfm_endpoint(),
            lastfm_username: None,
            listenbrainz_enabled: false,
            scrobble_delay_percent: 0.5,
            scrobble_delay_seconds: 180,
            scrobble_min_song_duration_secs: 30,
            webdav: WebDavSettings::default(),
            smb: SmbSettings::default(),
        }
    }
}

impl AppSettingsData {
    /// Whether downloads are allowed given the current connection's metered status.
    ///
    /// Matches `AppSettings.downloadsAllowedNow`: `None` (offline) is allowed through
    /// so the network failure reports itself honestly instead of blaming Wi-Fi-only.
    pub const fn downloads_allowed_now(&self, metered: Option<bool>) -> bool {
        !self.wifi_only_downloads || !matches!(metered, Some(true))
    }

    /// Effective streaming quality ceiling given the current connection's metered status.
    pub const fn effective_audio_quality(&self, metered: Option<bool>) -> AudioQuality {
        match metered {
            Some(true) => self.audio_quality_cellular,
            _ => self.audio_quality_wifi,
        }
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("settings JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Atomic file-backed persistence wrapper around [`AppSettingsData`].
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads settings from disk, returning [`AppSettingsData::default()`] if the file
    /// does not yet exist.
    pub fn load(&self) -> Result<AppSettingsData, SettingsError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => Ok(serde_json::from_str(&raw)?),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AppSettingsData::default()),
            Err(err) => Err(SettingsError::Io(err)),
        }
    }

    /// Atomically writes settings via a sibling temp file followed by `fs::rename`.
    pub fn save(&self, data: &AppSettingsData) -> Result<(), SettingsError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_string_pretty(data)?;
        let temp_name = format!(".settings-{}.tmp", Uuid::new_v4());
        let temp_path = self.path.with_file_name(temp_name);
        fs::write(&temp_path, encoded)?;
        fs::rename(&temp_path, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lossless_asks_source_for_bit_exact_file() {
        assert_eq!(
            request_for_download(DownloadQuality::Lossless),
            StreamRequest::Lossless
        );
        assert_eq!(
            request_for_download(DownloadQuality::High),
            StreamRequest::Best
        );
        assert_eq!(
            request_for_download(DownloadQuality::Standard),
            StreamRequest::Capped(128)
        );
    }

    #[test]
    fn high_is_uncapped_and_every_download_rung_is_describable() {
        assert_eq!(DownloadQuality::High.max_kbps(), i32::MAX);
        assert!(!matches!(
            request_for_download(DownloadQuality::High),
            StreamRequest::Capped(_)
        ));
        for quality in DownloadQuality::ALL {
            assert!(!quality.label().trim().is_empty());
            assert!(!quality.detail().trim().is_empty());
            assert!(!quality.per_track().trim().is_empty());
        }
    }

    #[test]
    fn wifi_only_refuses_metered_connections_and_nothing_else() {
        let mut settings = AppSettingsData {
            wifi_only_downloads: true,
            ..AppSettingsData::default()
        };
        assert!(!settings.downloads_allowed_now(Some(true)));
        assert!(settings.downloads_allowed_now(Some(false)));
        assert!(settings.downloads_allowed_now(None));

        settings.wifi_only_downloads = false;
        assert!(settings.downloads_allowed_now(Some(true)));
    }

    #[test]
    fn capped_streaming_ceiling_does_not_downgrade_download_quality() {
        let settings = AppSettingsData {
            audio_quality_cellular: AudioQuality::Low,
            download_quality: DownloadQuality::Lossless,
            ..AppSettingsData::default()
        };
        assert_eq!(
            settings.effective_audio_quality(Some(true)),
            AudioQuality::Low
        );
        assert_eq!(
            request_for_download(settings.download_quality),
            StreamRequest::Lossless
        );
    }

    #[test]
    fn settings_store_round_trips_atomically() {
        let dir = std::env::temp_dir().join(format!("bitchord-settings-{}", Uuid::new_v4()));
        let store = SettingsStore::new(dir.join("settings.json"));
        let mut settings = store.load().expect("default load when missing");
        settings.listen_together_nickname = "Ada".to_owned();
        settings.volume = 0.75;
        store.save(&settings).expect("atomic save");

        let reloaded = store.load().expect("reload");
        assert_eq!(reloaded.listen_together_nickname, "Ada");
        assert_eq!(reloaded.volume, 0.75);
        assert_eq!(reloaded.device_id, settings.device_id);
        let _ = fs::remove_dir_all(&dir);
    }
}
