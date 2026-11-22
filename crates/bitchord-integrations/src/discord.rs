use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use uuid::Uuid;

use crate::nerd_stats::{NerdStatsSnapshot, codec_label, megahertz_label};

/// BitChord's registered Discord application ID (shared with upstream).
pub const DISCORD_APPLICATION_ID: &str = "1543679166967193620";

/// Discord caps every activity text field at 128 characters.
const DISCORD_TEXT_LIMIT: usize = 128;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;

#[derive(Debug, Error)]
pub enum DiscordError {
    #[error("no local Discord IPC socket found")]
    NotRunning,
    #[error("Discord IPC I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Discord IPC protocol error: {0}")]
    Protocol(String),
}

/// Formats the optional premium audio quality line shown on BitChord's Discord
/// activity card from measured [`NerdStatsSnapshot`] figures.
pub fn discord_audio_quality_line(stats: Option<&NerdStatsSnapshot>) -> Option<String> {
    let stats = stats?;
    let quality = if stats.is_dolby_atmos() {
        "◗◖ Dolby Atmos"
    } else if stats.is_hi_res() {
        "Hi-Res Lossless"
    } else if stats.is_lossless() {
        "Lossless"
    } else {
        return None;
    };

    let mut parts: Vec<String> = vec![quality.to_owned()];
    if let Some(codec) = codec_label(stats.mime_type.as_deref()) {
        parts.push(codec);
    }
    if let Some(kbps) = stats.bitrate_kbps.filter(|&k| k > 0) {
        parts.push(format!("{kbps} kbps"));
    }
    if let Some(depth) = stats.bit_depth.filter(|&d| d > 0) {
        parts.push(format!("{depth}-bit"));
    }
    if let Some(hz) = stats.sample_rate_hz.filter(|&r| r > 0) {
        if let Some(mhz) = megahertz_label(hz) {
            parts.push(mhz);
        } else {
            let formatted = format!("{:.1}", hz as f64 / 1000.0);
            let khz = formatted.strip_suffix(".0").unwrap_or(&formatted);
            parts.push(format!("{khz} kHz"));
        }
    }
    if let Some(ch) = stats.channels.filter(|&c| c > 0) {
        let label = match ch {
            1 => "Mono".to_owned(),
            2 => "Stereo".to_owned(),
            n => format!("{n} ch"),
        };
        parts.push(label);
    }

    let joined = parts.join(" · ");
    let truncated: String = joined.chars().take(DISCORD_TEXT_LIMIT).collect();
    Some(truncated.trim_end().to_owned())
}

/// Activity timestamps (`start`/`end` in UNIX milliseconds for Discord IPC/Gateway).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordTimestamps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiscordAssets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordButton {
    pub label: String,
    pub url: String,
}

/// Listening activity payload compatible with both Linux local Discord IPC and
/// Discord Gateway Rich Presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordActivity {
    /// `2` = Listening (Discord ActivityType.Listening).
    #[serde(rename = "type")]
    pub activity_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<DiscordTimestamps>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<DiscordAssets>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<DiscordButton>,
}

impl DiscordActivity {
    pub fn listening(title: impl Into<String>, artist: impl Into<String>) -> Self {
        Self {
            activity_type: 2,
            details: Some(truncate_discord(&title.into())),
            state: Some(truncate_discord(&artist.into())),
            timestamps: None,
            assets: None,
            buttons: Vec::new(),
        }
    }
}

fn truncate_discord(s: &str) -> String {
    s.chars().take(DISCORD_TEXT_LIMIT).collect()
}

/// Candidate Linux Unix socket paths checked for a running Discord desktop
/// client (native, Flatpak, Snap, `/tmp`).
pub fn candidate_ipc_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        dirs.push(runtime.clone());
        dirs.push(runtime.join("app").join("com.discordapp.Discord"));
        dirs.push(runtime.join("snap.discord"));
    }
    if let Some(tmp) = std::env::var_os("TMPDIR").map(PathBuf::from) {
        dirs.push(tmp);
    }
    dirs.push(PathBuf::from("/tmp"));

    let mut paths = Vec::new();
    for dir in dirs {
        for idx in 0..10 {
            paths.push(dir.join(format!("discord-ipc-{idx}")));
        }
    }
    paths
}

/// Native Linux Discord Rich Presence IPC client over `UnixStream`.
pub struct DiscordIpcClient {
    stream: UnixStream,
    client_id: String,
}

impl DiscordIpcClient {
    /// Connects to the first responsive local Discord IPC socket and completes
    /// the v1 handshake.
    pub async fn connect_default() -> Result<Self, DiscordError> {
        Self::connect_with_app_id(DISCORD_APPLICATION_ID).await
    }

    pub async fn connect_with_app_id(client_id: impl Into<String>) -> Result<Self, DiscordError> {
        let client_id = client_id.into();
        for path in candidate_ipc_paths() {
            if let Ok(client) = Self::connect_path(&path, client_id.clone()).await {
                return Ok(client);
            }
        }
        Err(DiscordError::NotRunning)
    }

    pub async fn connect_path(
        path: &Path,
        client_id: impl Into<String>,
    ) -> Result<Self, DiscordError> {
        let stream = UnixStream::connect(path).await?;
        let mut client = Self {
            stream,
            client_id: client_id.into(),
        };
        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<Value, DiscordError> {
        let payload = json!({
            "v": 1,
            "client_id": self.client_id,
        });
        self.write_frame(OP_HANDSHAKE, &payload).await?;
        let (op, reply) = self.read_frame().await?;
        if op == OP_CLOSE {
            return Err(DiscordError::Protocol(format!("handshake closed: {reply}")));
        }
        Ok(reply)
    }

    /// Publishes a Rich Presence activity to the local Discord client.
    pub async fn set_activity(
        &mut self,
        activity: &DiscordActivity,
    ) -> Result<Value, DiscordError> {
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "args": {
                "pid": std::process::id(),
                "activity": activity,
            },
            "nonce": Uuid::new_v4().to_string(),
        });
        self.write_frame(OP_FRAME, &payload).await?;
        let (_, reply) = self.read_frame().await?;
        Ok(reply)
    }

    /// Clears the current Discord activity.
    pub async fn clear_activity(&mut self) -> Result<Value, DiscordError> {
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "args": {
                "pid": std::process::id(),
                "activity": Value::Null,
            },
            "nonce": Uuid::new_v4().to_string(),
        });
        self.write_frame(OP_FRAME, &payload).await?;
        let (_, reply) = self.read_frame().await?;
        Ok(reply)
    }

    /// Cleanly closes the IPC connection.
    pub async fn close(mut self) -> Result<(), DiscordError> {
        let _ = self.write_frame(OP_CLOSE, &json!({})).await;
        let _ = self.stream.shutdown().await;
        Ok(())
    }

    async fn write_frame(&mut self, opcode: u32, payload: &Value) -> Result<(), DiscordError> {
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| DiscordError::Protocol(format!("serialize ipc frame: {e}")))?;
        let mut header = [0u8; 8];
        header[..4].copy_from_slice(&opcode.to_le_bytes());
        header[4..8].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        self.stream.write_all(&header).await?;
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<(u32, Value), DiscordError> {
        let mut header = [0u8; 8];
        self.stream.read_exact(&mut header).await?;
        let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if len > 1024 * 1024 {
            return Err(DiscordError::Protocol(format!(
                "IPC frame too large: {len}"
            )));
        }
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body).await?;
        let value = serde_json::from_slice(&body)
            .map_err(|e| DiscordError::Protocol(format!("invalid ipc json: {e}")))?;
        Ok((opcode, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[test]
    fn hi_res_line_contains_measured_specs_separated_by_center_dots() {
        let stats = NerdStatsSnapshot {
            mime_type: Some("audio/flac".to_owned()),
            bitrate_kbps: Some(4608),
            sample_rate_hz: Some(96_000),
            channels: Some(2),
            bit_depth: Some(24),
            ..Default::default()
        };
        assert_eq!(
            discord_audio_quality_line(Some(&stats)).as_deref(),
            Some("Hi-Res Lossless · FLAC · 4608 kbps · 24-bit · 96 kHz · Stereo")
        );
    }

    #[test]
    fn cd_quality_lossless_keeps_known_specs_only() {
        let stats = NerdStatsSnapshot {
            mime_type: Some("audio/alac".to_owned()),
            bitrate_kbps: Some(1411),
            sample_rate_hz: Some(44_100),
            channels: None,
            bit_depth: Some(16),
            ..Default::default()
        };
        assert_eq!(
            discord_audio_quality_line(Some(&stats)).as_deref(),
            Some("Lossless · ALAC · 1411 kbps · 16-bit · 44.1 kHz")
        );
    }

    #[test]
    fn dolby_line_includes_immersive_codec_and_channels() {
        let stats = NerdStatsSnapshot {
            mime_type: Some("audio/eac3-joc".to_owned()),
            bitrate_kbps: Some(768),
            sample_rate_hz: Some(48_000),
            channels: Some(6),
            ..Default::default()
        };
        assert_eq!(
            discord_audio_quality_line(Some(&stats)).as_deref(),
            Some("◗◖ Dolby Atmos · E-AC-3 JOC · 768 kbps · 48 kHz · 6 ch")
        );
    }

    #[test]
    fn ordinary_lossy_audio_has_no_quality_line() {
        let stats = NerdStatsSnapshot {
            mime_type: Some("audio/opus".to_owned()),
            bitrate_kbps: Some(160),
            sample_rate_hz: Some(48_000),
            channels: Some(2),
            ..Default::default()
        };
        assert!(discord_audio_quality_line(Some(&stats)).is_none());
        assert!(discord_audio_quality_line(None).is_none());
    }

    #[test]
    fn dsd_line_names_rate_in_megahertz() {
        let stats = NerdStatsSnapshot {
            mime_type: Some("audio/dsd64".to_owned()),
            bitrate_kbps: Some(5644),
            sample_rate_hz: Some(2_822_400),
            channels: Some(2),
            ..Default::default()
        };
        assert_eq!(
            discord_audio_quality_line(Some(&stats)).as_deref(),
            Some("Hi-Res Lossless · DSD64 · 5644 kbps · 2.8224 MHz · Stereo")
        );
        assert_eq!(megahertz_label(45_158_400).as_deref(), Some("45.1584 MHz"));
        assert!(megahertz_label(192_000).is_none());
    }

    #[tokio::test]
    async fn local_unix_ipc_handshake_and_set_activity_round_trip() {
        let sock_path =
            std::env::temp_dir().join(format!("bitchord-discord-ipc-test-{}.sock", Uuid::new_v4()));
        let listener = UnixListener::bind(&sock_path).expect("bind unix socket");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            for expected_op in [OP_HANDSHAKE, OP_FRAME, OP_FRAME] {
                let mut hdr = [0u8; 8];
                stream.read_exact(&mut hdr).await.expect("read hdr");
                let op = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
                assert_eq!(op, expected_op);
                let mut body = vec![0u8; len];
                stream.read_exact(&mut body).await.expect("read body");
                let val: Value = serde_json::from_slice(&body).expect("json");
                if op == OP_HANDSHAKE {
                    assert_eq!(val["client_id"], DISCORD_APPLICATION_ID);
                } else {
                    assert_eq!(val["cmd"], "SET_ACTIVITY");
                }
                let reply = serde_json::to_vec(&json!({"cmd": "DISPATCH", "evt": "READY"}))
                    .expect("serialize");
                let mut out_hdr = [0u8; 8];
                out_hdr[..4].copy_from_slice(&OP_FRAME.to_le_bytes());
                out_hdr[4..8].copy_from_slice(&(reply.len() as u32).to_le_bytes());
                stream.write_all(&out_hdr).await.expect("write hdr");
                stream.write_all(&reply).await.expect("write reply");
            }
        });

        let mut client = DiscordIpcClient::connect_path(&sock_path, DISCORD_APPLICATION_ID)
            .await
            .expect("connect");
        let activity = DiscordActivity::listening("Bohemian Rhapsody", "Queen");
        let resp = client.set_activity(&activity).await.expect("set activity");
        assert_eq!(resp["evt"], "READY");
        let _ = client.clear_activity().await.expect("clear activity");
        client.close().await.expect("close");
        server.await.expect("server done");
        let _ = std::fs::remove_file(&sock_path);
    }
}
