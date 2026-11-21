use serde::{Deserialize, Serialize};

fn default_queue_index() -> i32 {
    -1
}

fn default_max_members() -> i32 {
    5
}

/// A song as the Listen Together party server knows it (`backend/README.md`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyTrack {
    pub video_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub from_autoplay: bool,
}

impl PartyTrack {
    pub fn with_id(video_id: impl Into<String>) -> Self {
        Self {
            video_id: video_id.into(),
            ..Self::default()
        }
    }
}

/// One signed-in device in the party.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyMember {
    pub member_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub is_host: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub joined_at_ms: i64,
    #[serde(default)]
    pub last_seen_ms: i64,
}

/// Where the party is as of a server timestamp (`anchor_ms`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyPlayback {
    #[serde(default)]
    pub seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<PartyTrack>,
    #[serde(default)]
    pub queue_seq: i64,
    #[serde(default)]
    pub queue_length: i32,
    #[serde(default = "default_queue_index")]
    pub queue_index: i32,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub position_ms: i64,
    #[serde(default)]
    pub anchor_ms: i64,
    #[serde(default)]
    pub effective_position_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by_name: Option<String>,
    #[serde(default)]
    pub autoplay_enabled: bool,
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl Default for PartyPlayback {
    fn default() -> Self {
        Self {
            seq: 0,
            track: None,
            queue_seq: 0,
            queue_length: 0,
            queue_index: -1,
            is_playing: false,
            position_ms: 0,
            anchor_ms: 0,
            effective_position_ms: 0,
            updated_by: None,
            started_by: None,
            started_by_name: None,
            autoplay_enabled: false,
            updated_at_ms: 0,
        }
    }
}

/// Computes the party's playhead at `server_now_ms` according to `backend/README.md`:
/// - playing: `position_ms + max(0, server_now_ms - anchor_ms)` (never runs backwards during lead-in)
/// - paused: `position_ms`
pub fn party_position_ms(playback: &PartyPlayback, server_now_ms: i64) -> i64 {
    if playback.is_playing {
        let elapsed = (server_now_ms - playback.anchor_ms).max(0);
        playback.position_ms.saturating_add(elapsed)
    } else {
        playback.position_ms
    }
}

/// The party's running order, transmitted separately from heartbeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyQueue {
    #[serde(default)]
    pub seq: i64,
    #[serde(default = "default_queue_index")]
    pub index: i32,
    #[serde(default)]
    pub items: Vec<PartyTrack>,
}

impl Default for PartyQueue {
    fn default() -> Self {
        Self {
            seq: 0,
            index: -1,
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartySnapshot {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default = "default_max_members")]
    pub max_members: i32,
    #[serde(default)]
    pub host_only_control: bool,
    #[serde(default)]
    pub members: Vec<PartyMember>,
    #[serde(default)]
    pub playback: PartyPlayback,
    #[serde(default)]
    pub queue: PartyQueue,
    #[serde(default)]
    pub server_ms: i64,
}

impl Default for PartySnapshot {
    fn default() -> Self {
        Self {
            code: String::new(),
            created_at_ms: 0,
            max_members: 5,
            host_only_control: false,
            members: Vec::new(),
            playback: PartyPlayback::default(),
            queue: PartyQueue::default(),
            server_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyPreview {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub host_name: String,
    #[serde(default)]
    pub member_count: i32,
    #[serde(default = "default_max_members")]
    pub max_members: i32,
    #[serde(default)]
    pub is_full: bool,
    #[serde(default)]
    pub members: Vec<PartyPreviewMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyPreviewMember {
    #[serde(default)]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub is_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyMembership {
    pub code: String,
    pub token: String,
    pub you: PartyMember,
    pub party: PartySnapshot,
    #[serde(default)]
    pub server_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyActivity {
    pub action: String,
    pub by: String,
    pub at_ms: i64,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub user_id: String,
    pub device_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_members: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoplay_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    #[serde(rename = "error", default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_matches_go_backend_camel_case_contract() {
        let raw = r#"{
            "code": "ABC123",
            "createdAtMs": 1757630000000,
            "maxMembers": 5,
            "members": [
                {"memberId": "m1", "userId": "u1", "displayName": "Ada", "isHost": true, "connected": true}
            ],
            "playback": {
                "seq": 7,
                "track": {"videoId": "v1", "title": "Song", "artist": "Artist", "fromAutoplay": true},
                "queueSeq": 3,
                "queueLength": 2,
                "queueIndex": 1,
                "isPlaying": true,
                "positionMs": 42000,
                "anchorMs": 1757630001234
            },
            "queue": {
                "seq": 3,
                "index": 1,
                "items": [{"videoId": "v0"}, {"videoId": "v1", "fromAutoplay": true}]
            },
            "serverMs": 1757630002000
        }"#;

        let snap: PartySnapshot = serde_json::from_str(raw).expect("valid Go server snapshot");
        assert_eq!("ABC123", snap.code);
        assert!(!snap.host_only_control); // absent defaults false
        assert_eq!(1, snap.playback.queue_index);
        assert_eq!(42_766, party_position_ms(&snap.playback, 1_757_630_002_000));
        // Scheduled lead-in before anchor never runs backwards.
        assert_eq!(42_000, party_position_ms(&snap.playback, 1_757_630_001_000));

        let err: ApiError = serde_json::from_str(r#"{"error":"party_full","message":"full"}"#)
            .expect("valid ApiError");
        assert_eq!("party_full", err.code);
        assert_eq!("full", err.message);
    }
}
