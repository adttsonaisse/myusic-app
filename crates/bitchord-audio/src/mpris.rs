//! Linux MPRIS2 D-Bus server (`org.mpris.MediaPlayer2.bitchord`) backed by
//! `mpris-server 0.11` (unified on `zbus 6.5`).
//!
//! Enforces MPRIS2 specification invariants (`linux-audio-engine.md` §5):
//! - Encodes YouTube video IDs (which contain `-`) as hex into valid D-Bus
//!   `ObjectPath` strings (`/com/music/bitchord/track/<hex>`).
//! - Uses local `file://` artwork URIs when a cached cover file is available.
//! - Never spams `PropertiesChanged` for `Position`; emits `Signal::Seeked`
//!   explicitly on seeks/discontinuities and lets clients extrapolate via `Rate`.

use std::path::Path;
use std::sync::{Arc, RwLock};

use bitchord_model::Song;
pub use mpris_server::{LoopStatus, PlaybackStatus};
use mpris_server::{
    Metadata, PlaybackRate, PlayerInterface, Property, RootInterface, Server, Signal, Time,
    TrackId, Volume,
    zbus::{Result as ZbusResult, fdo},
};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::time_stretch::{MAX_SPEED, MIN_SPEED};

/// Primary MPRIS2 bus name suffix (`org.mpris.MediaPlayer2.bitchord`).
pub const MPRIS_BUS_SUFFIX: &str = "bitchord";
/// Desktop entry basename (`com.music.myusic.desktop`).
pub const DESKTOP_ENTRY: &str = "com.music.myusic";
/// Human-readable application name shown in Linux media widgets.
pub const IDENTITY: &str = "Myusic";

/// Inbound control command sent by Linux desktop media keys, KDE/GNOME/Cutefish
/// media widgets, or `playerctl`.
#[derive(Debug, Clone, PartialEq)]
pub enum MprisCommand {
    Raise,
    Quit,
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    SeekRelativeUs(i64),
    SetPositionUs { track_id: String, position_us: i64 },
    SetVolume(f64),
    SetRate(f64),
    SetShuffle(bool),
    SetLoopStatus(LoopStatus),
}

/// Shared state snapshot exposed over D-Bus properties.
#[derive(Debug, Clone)]
pub struct MprisState {
    pub playback_status: PlaybackStatus,
    pub loop_status: LoopStatus,
    pub rate: f64,
    pub shuffle: bool,
    pub volume: f64,
    pub position_us: i64,
    pub metadata: Metadata,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub can_raise: bool,
}

impl Default for MprisState {
    fn default() -> Self {
        Self {
            playback_status: PlaybackStatus::Stopped,
            loop_status: LoopStatus::None,
            rate: 1.0,
            shuffle: false,
            volume: 1.0,
            position_us: 0,
            metadata: Metadata::new(),
            can_go_next: false,
            can_go_previous: false,
            can_seek: false,
            can_raise: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum MprisError {
    #[error("D-Bus / MPRIS2 error: {0}")]
    Dbus(#[from] mpris_server::zbus::Error),
    #[error("MPRIS2 FDO error: {0}")]
    Fdo(#[from] fdo::Error),
}

/// Encodes an arbitrary `video_id` (which may contain `-`, Unicode, or spaces) into a
/// spec-compliant D-Bus `ObjectPath` (`/com/music/bitchord/track/<lowercase-hex>`).
pub fn encode_mpris_track_id(video_id: &str) -> String {
    if video_id.is_empty() {
        return "/com/music/bitchord/track/00".to_owned();
    }
    let mut hex = String::with_capacity(video_id.len() * 2);
    for b in video_id.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    format!("/com/music/bitchord/track/{hex}")
}

/// Builds an MPRIS2 [`Metadata`] map from a [`Song`], duration in ms, and optional local cover art path.
pub fn build_mpris_metadata(
    song: &Song,
    duration_ms: u64,
    local_art_path: Option<&Path>,
) -> Metadata {
    let mut meta = Metadata::new();
    let encoded_id = encode_mpris_track_id(&song.video_id);
    if let Ok(track_id) = TrackId::try_from(encoded_id) {
        meta.set_trackid(Some(track_id));
    }
    meta.set_title(Some(song.title.clone()));
    if !song.artist.trim().is_empty() {
        meta.set_artist(Some(vec![song.artist.clone()]));
    }
    if let Some(ref album) = song.album_name
        && !album.trim().is_empty()
    {
        meta.set_album(Some(album.clone()));
    }
    if duration_ms > 0 {
        meta.set_length(Some(Time::from_millis(duration_ms as i64)));
    }
    if let Some(path) = local_art_path {
        meta.set_art_url(Some(format!("file://{}", path.display())));
    } else if let Some(ref url) = song.thumbnail_url {
        meta.set_art_url(Some(url.clone()));
    }
    meta
}

/// Internal D-Bus object implementing `RootInterface` and `PlayerInterface`.
pub struct BitChordMprisPlayer {
    state: Arc<RwLock<MprisState>>,
    cmd_tx: mpsc::UnboundedSender<MprisCommand>,
}

impl BitChordMprisPlayer {
    fn send(&self, cmd: MprisCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn snapshot(&self) -> MprisState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl RootInterface for BitChordMprisPlayer {
    async fn raise(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Raise);
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Quit);
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _fullscreen: bool) -> ZbusResult<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_raise)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok(IDENTITY.to_owned())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok(DESKTOP_ENTRY.to_owned())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec!["file".to_owned(), "https".to_owned()])
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![
            "audio/flac".to_owned(),
            "audio/mp4".to_owned(),
            "audio/mpeg".to_owned(),
            "audio/ogg".to_owned(),
            "audio/opus".to_owned(),
            "audio/webm".to_owned(),
            "audio/x-wav".to_owned(),
        ])
    }
}

impl PlayerInterface for BitChordMprisPlayer {
    async fn next(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Next);
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Previous);
        Ok(())
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Pause);
        Ok(())
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.send(MprisCommand::PlayPause);
        Ok(())
    }

    async fn stop(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Stop);
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Play);
        Ok(())
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        self.send(MprisCommand::SeekRelativeUs(offset.as_micros()));
        Ok(())
    }

    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        self.send(MprisCommand::SetPositionUs {
            track_id: track_id.as_str().to_owned(),
            position_us: position.as_micros(),
        });
        Ok(())
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(self.snapshot().playback_status)
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(self.snapshot().loop_status)
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> ZbusResult<()> {
        self.send(MprisCommand::SetLoopStatus(loop_status));
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(self.snapshot().rate)
    }

    async fn set_rate(&self, rate: PlaybackRate) -> ZbusResult<()> {
        let clamped = rate.clamp(f64::from(MIN_SPEED), f64::from(MAX_SPEED));
        self.send(MprisCommand::SetRate(clamped));
        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().shuffle)
    }

    async fn set_shuffle(&self, shuffle: bool) -> ZbusResult<()> {
        self.send(MprisCommand::SetShuffle(shuffle));
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(self.snapshot().metadata)
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self.snapshot().volume)
    }

    async fn set_volume(&self, volume: Volume) -> ZbusResult<()> {
        self.send(MprisCommand::SetVolume(volume.clamp(0.0, 1.0)));
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_micros(self.snapshot().position_us))
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(f64::from(MIN_SPEED))
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(f64::from(MAX_SPEED))
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_go_next)
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_go_previous)
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_seek)
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

/// Live MPRIS2 D-Bus server handle for BitChord Linux Desktop.
pub struct MprisService {
    server: Server<BitChordMprisPlayer>,
    state: Arc<RwLock<MprisState>>,
}

impl MprisService {
    /// Connects to the session D-Bus and exports `org.mpris.MediaPlayer2.bitchord`
    /// (falling back to `.instance<pid>` if another BitChord instance already holds the name).
    pub async fn start() -> Result<(Self, mpsc::UnboundedReceiver<MprisCommand>), MprisError> {
        let state = Arc::new(RwLock::new(MprisState::default()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let make_player = || BitChordMprisPlayer {
            state: Arc::clone(&state),
            cmd_tx: cmd_tx.clone(),
        };

        let server = match Server::new(MPRIS_BUS_SUFFIX, make_player()).await {
            Ok(s) => s,
            Err(_) => {
                let fallback = format!("{MPRIS_BUS_SUFFIX}.instance{}", std::process::id());
                Server::new(&fallback, make_player()).await?
            }
        };

        Ok((Self { server, state }, cmd_rx))
    }

    /// Updates cached `position_us` silently (NEVER emits `PropertiesChanged` for `Position`).
    pub fn set_position_ms(&self, position_ms: u64) {
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .position_us = (position_ms as i64).saturating_mul(1_000);
    }

    /// Emits an explicit MPRIS2 `Seeked` signal after a seek or playhead discontinuity.
    pub async fn notify_seeked(&self, position_ms: u64) -> Result<(), MprisError> {
        self.set_position_ms(position_ms);
        self.server
            .emit(Signal::Seeked {
                position: Time::from_millis(position_ms as i64),
            })
            .await?;
        Ok(())
    }

    /// Updates exported properties and emits `PropertiesChanged` only for changed fields.
    pub async fn publish_state(&self, next: MprisState) -> Result<(), MprisError> {
        let mut changed = Vec::new();
        {
            let mut guard = self.state.write().unwrap_or_else(|e| e.into_inner());
            if guard.playback_status != next.playback_status {
                guard.playback_status = next.playback_status;
                changed.push(Property::PlaybackStatus(next.playback_status));
            }
            if guard.loop_status != next.loop_status {
                guard.loop_status = next.loop_status;
                changed.push(Property::LoopStatus(next.loop_status));
            }
            if (guard.rate - next.rate).abs() > 1e-4 {
                guard.rate = next.rate;
                changed.push(Property::Rate(next.rate));
            }
            if guard.shuffle != next.shuffle {
                guard.shuffle = next.shuffle;
                changed.push(Property::Shuffle(next.shuffle));
            }
            if (guard.volume - next.volume).abs() > 1e-4 {
                guard.volume = next.volume;
                changed.push(Property::Volume(next.volume));
            }
            if guard.can_go_next != next.can_go_next {
                guard.can_go_next = next.can_go_next;
                changed.push(Property::CanGoNext(next.can_go_next));
            }
            if guard.can_go_previous != next.can_go_previous {
                guard.can_go_previous = next.can_go_previous;
                changed.push(Property::CanGoPrevious(next.can_go_previous));
            }
            if guard.can_seek != next.can_seek {
                guard.can_seek = next.can_seek;
                changed.push(Property::CanSeek(next.can_seek));
            }
            if guard.metadata != next.metadata {
                guard.metadata = next.metadata.clone();
                changed.push(Property::Metadata(next.metadata));
            }
            guard.position_us = next.position_us;
        }
        if !changed.is_empty() {
            self.server.properties_changed(changed).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_hyphenated_youtube_ids_into_valid_dbus_object_paths() {
        let id = encode_mpris_track_id("-Abc_123-xyz");
        assert!(id.starts_with("/com/music/bitchord/track/"));
        assert!(!id.contains('-'));
        assert!(TrackId::try_from(id).is_ok());

        assert!(TrackId::try_from(encode_mpris_track_id("")).is_ok());
    }

    #[test]
    fn builds_metadata_with_local_file_art_url_and_duration() {
        let mut song = Song::new(
            "-aBc_123",
            "Nightcall",
            "Kavinsky",
            Some("https://remote".into()),
        );
        song.album_name = Some("OutRun".into());

        let meta = build_mpris_metadata(&song, 258_000, Some(Path::new("/tmp/cover.jpg")));
        assert_eq!(meta.title(), Some("Nightcall"));
        assert_eq!(meta.album(), Some("OutRun"));
        assert_eq!(meta.art_url().as_deref(), Some("file:///tmp/cover.jpg"));
        assert_eq!(meta.length(), Some(Time::from_millis(258_000)));
        assert!(meta.trackid().is_some());
    }
}
