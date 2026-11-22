//! Listen Together REST + WebSocket client and state machine compatible with the
//! BitChord Go party server (`backend/`).
//!
//! Ported from upstream `ListenTogether.kt` (`app/src/main/java/.../listentogether/ListenTogether.kt`)
//! and `ListenTogetherIdleHealthTest.kt`.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use bitchord_jam::{
    ApiError, CODE_LENGTH, JoinRequest, PartyActivity, PartyMember, PartyMembership, PartyPlayback,
    PartyPreview, PartyQueue, PartySnapshot, PartyTrack, ServerClock, party_position_ms,
};
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{AUTHORIZATION, HeaderValue};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use url::Url;

/// Default custom-server probe timeout (6 s).
pub const CUSTOM_SERVER_TIMEOUT: Duration = Duration::from_secs(6);
/// Default built-in server probe timeout (30 s, accounting for free-tier cold start).
pub const DEFAULT_SERVER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a local kick block lasts (24 h).
pub const KICK_BLOCK_MS: i64 = 24 * 60 * 60 * 1000;
/// Redacted placeholder for server URLs in logs and user-facing errors.
pub const SERVER_PLACEHOLDER: &str = "<party server>";
/// Maximum retained activity log entries for the current session.
pub const MAX_ACTIVITY_ENTRIES: usize = 100;

// INFALLIBLE: static compile-time regex patterns.
static LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?$").expect("valid label"));
static ABSOLUTE_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:https?|wss?)://[^\s,;)\]}'"]+"#).expect("valid url redaction regex")
});

/// Validation errors returned by [`parse_and_normalize_server_url`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ServerUrlError {
    #[error("server URL must not contain whitespace")]
    Whitespace,
    #[error("server URL must use http or https")]
    InvalidScheme,
    #[error("server URL has an invalid host")]
    InvalidHost,
    #[error("server URL port must be in 1..=65535")]
    InvalidPort,
    #[error("server URL path contains invalid segments")]
    InvalidPath,
    #[error("server URL must not include a path")]
    HasPath,
    #[error("server URL must not include a query string")]
    HasQuery,
    #[error("server URL must not include a fragment")]
    HasFragment,
    #[error("server URL is malformed")]
    Malformed,
}

/// Server reachability state shown in Listen Together UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Health {
    #[default]
    Unknown,
    Checking,
    Online,
    Offline,
}

/// WebSocket lifecycle state for an active party session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Offline,
    Connecting,
    Live,
}

/// Result of probing `/healthz` on a party server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeResult {
    pub is_online: bool,
    pub latency_ms: i64,
}

/// Combined connection status distinguishing custom server vs default fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerConnectionState {
    Checking,
    DefaultOnline { latency_ms: i64 },
    CustomOnline { latency_ms: i64 },
    CustomFallback { latency_ms: i64 },
    Offline,
}

impl ServerConnectionState {
    pub fn latency_ms(self) -> Option<i64> {
        match self {
            Self::DefaultOnline { latency_ms }
            | Self::CustomOnline { latency_ms }
            | Self::CustomFallback { latency_ms } => Some(latency_ms),
            Self::Checking | Self::Offline => None,
        }
    }

    pub fn is_fallback(self) -> bool {
        matches!(self, Self::CustomFallback { .. })
    }

    pub fn health(self) -> Health {
        match self {
            Self::Checking => Health::Checking,
            Self::DefaultOnline { .. }
            | Self::CustomOnline { .. }
            | Self::CustomFallback { .. } => Health::Online,
            Self::Offline => Health::Offline,
        }
    }
}

/// Snapshot of client-side Listen Together party state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTogetherState {
    pub code: Option<String>,
    pub you: Option<PartyMember>,
    pub members: Vec<PartyMember>,
    pub max_members: i32,
    pub host_only_control: bool,
    pub playback: PartyPlayback,
    pub queue: PartyQueue,
    pub connection: Connection,
    pub clock_synced: bool,
    pub round_trip_ms: i64,
    pub error: Option<String>,
}

impl Default for ListenTogetherState {
    fn default() -> Self {
        Self {
            code: None,
            you: None,
            members: Vec::new(),
            max_members: 5,
            host_only_control: false,
            playback: PartyPlayback::default(),
            queue: PartyQueue::default(),
            connection: Connection::Offline,
            clock_synced: false,
            round_trip_ms: 0,
            error: None,
        }
    }
}

impl ListenTogetherState {
    pub fn in_party(&self) -> bool {
        self.code.is_some()
    }

    pub fn is_full(&self) -> bool {
        self.members.len() as i32 >= self.max_members
    }

    /// True only for a non-host listener in a party where the host enabled `host_only_control`.
    pub fn controls_locked(&self) -> bool {
        self.in_party() && self.host_only_control && !self.you.as_ref().is_some_and(|m| m.is_host)
    }
}

/// Errors produced by Listen Together REST and WebSocket operations.
#[derive(Debug, Error)]
pub enum PartyError {
    #[error("party server error ({code}): {message}")]
    Server {
        code: String,
        message: String,
        status_code: Option<u16>,
    },
    #[error("invalid server URL: {0}")]
    InvalidServerUrl(#[from] ServerUrlError),
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WebSocket transport error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

impl PartyError {
    /// Whether this failure qualifies to fall back from a custom server to the default server.
    pub fn is_eligible_for_fallback(&self) -> bool {
        match self {
            Self::Server { status_code, .. } => {
                status_code.is_some_and(|s| (500..=599).contains(&s))
            }
            Self::Http(err) => err.is_connect() || err.is_timeout() || err.is_request(),
            Self::WebSocket(_) => true,
            Self::InvalidServerUrl(_) | Self::Json(_) => false,
        }
    }
}

/// Validates and normalizes a user-configured Listen Together server URL.
///
/// Empty/blank input normalizes to `Ok("")` ("use built-in server"). Bare hosts
/// default to `https://`. Rejects whitespace, non-HTTP(S) schemes, queries,
/// fragments, `.`/`..` path segments, out-of-range ports, and invalid domain labels.
pub fn parse_and_normalize_server_url(raw: &str) -> Result<String, ServerUrlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(ServerUrlError::Whitespace);
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };

    // Inspect raw scheme before `Url::parse` so non-http schemes return InvalidScheme cleanly.
    if let Some((scheme_raw, rest)) = candidate.split_once("://") {
        let lower_scheme = scheme_raw.to_ascii_lowercase();
        if lower_scheme != "http" && lower_scheme != "https" {
            return Err(ServerUrlError::InvalidScheme);
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            return Err(ServerUrlError::InvalidHost);
        }
        if let Some(port_str) = extract_authority_port(authority)
            && !port_str.is_empty()
            && port_str.chars().all(|c| c.is_ascii_digit())
        {
            match port_str.parse::<u32>() {
                Ok(1..=65535) => {}
                _ => return Err(ServerUrlError::InvalidPort),
            }
        }
    } else {
        return Err(ServerUrlError::InvalidScheme);
    }

    let parsed = Url::parse(&candidate).map_err(|_| ServerUrlError::InvalidHost)?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(ServerUrlError::InvalidScheme);
    }

    if parsed.query().is_some() || candidate.contains('?') {
        return Err(ServerUrlError::HasQuery);
    }
    if parsed.fragment().is_some() || candidate.contains('#') {
        return Err(ServerUrlError::HasFragment);
    }

    // Check raw path segments from candidate (before URL dot-segment normalization).
    let raw_after_scheme = candidate.split_once("://").map(|(_, r)| r).unwrap_or("");
    let raw_path = raw_after_scheme
        .find('/')
        .map(|i| &raw_after_scheme[i..])
        .unwrap_or("");
    let mut segments = Vec::new();
    for seg in raw_path.split('/').filter(|s| !s.is_empty()) {
        if seg == "." || seg == ".." {
            return Err(ServerUrlError::InvalidPath);
        }
        segments.push(seg);
    }
    let canonical_path = if segments.is_empty() {
        String::new()
    } else {
        format!("/{}", segments.join("/"))
    };

    let raw_host = parsed.host_str().ok_or(ServerUrlError::InvalidHost)?;
    if raw_host.is_empty() {
        return Err(ServerUrlError::InvalidHost);
    }

    let canonical_host = if raw_host.starts_with('[') && raw_host.ends_with(']') {
        let inner = &raw_host[1..raw_host.len() - 1];
        if !inner.contains(':') {
            return Err(ServerUrlError::InvalidHost);
        }
        format!("[{}]", inner.to_ascii_lowercase())
    } else if raw_host.contains(':') {
        format!("[{}]", raw_host.to_ascii_lowercase())
    } else if raw_host.eq_ignore_ascii_case("localhost") {
        "localhost".to_owned()
    } else {
        if raw_host.len() > 253 || raw_host.starts_with('.') || raw_host.ends_with('.') {
            return Err(ServerUrlError::InvalidHost);
        }
        let labels: Vec<&str> = raw_host.split('.').collect();
        if labels.len() < 2 {
            return Err(ServerUrlError::InvalidHost);
        }
        for label in labels {
            if label.is_empty() || label.len() > 63 || !LABEL_RE.is_match(label) {
                return Err(ServerUrlError::InvalidHost);
            }
        }
        raw_host.to_ascii_lowercase()
    };

    // Preserve explicit port if specified in the input (even if it equals the scheme default).
    let explicit_port = raw_after_scheme
        .split(['/', '?', '#'])
        .next()
        .and_then(extract_authority_port)
        .and_then(|p| p.parse::<u16>().ok());
    let port_suffix = match explicit_port.or(parsed.port()) {
        Some(0) => return Err(ServerUrlError::InvalidPort),
        Some(p) => format!(":{p}"),
        None => String::new(),
    };

    Ok(format!(
        "{scheme}://{canonical_host}{port_suffix}{canonical_path}"
    ))
}

fn extract_authority_port(authority: &str) -> Option<&str> {
    if let Some(after_bracket) = authority.strip_prefix('[') {
        let (_, tail) = after_bracket.split_once(']')?;
        tail.strip_prefix(':')
    } else {
        authority.rsplit_once(':').map(|(_, p)| p)
    }
}

/// Decides whether an idle `/healthz` poll should be skipped.
///
/// Exact port of `ListenTogether.shouldSkipIdleProbe` (verified against
/// `ListenTogetherIdleHealthTest.kt`).
pub fn should_skip_idle_probe(
    screen_open: bool,
    socket_live: bool,
    in_party: bool,
    is_metered: Option<bool>,
) -> bool {
    if !screen_open || socket_live {
        return true;
    }
    !in_party && is_metered == Some(true)
}

/// Resolves the target server URL when switching parties while already in one.
pub fn resolve_switch_target<'a>(
    invite_server: Option<&'a str>,
    custom_server: &'a str,
    idle_server: &'a str,
) -> &'a str {
    if let Some(invite) = invite_server {
        return invite;
    }
    if !custom_server.trim().is_empty() {
        custom_server
    } else {
        idle_server
    }
}

/// Strips configured server bases and arbitrary HTTP(S)/WS(S) URLs from diagnostic messages.
pub fn redact_server_urls(text: &str, known_servers: &[&str]) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut needles: Vec<String> = known_servers
        .iter()
        .copied()
        .filter(|s| !s.trim().is_empty())
        .flat_map(|s| {
            let mut items = vec![s.to_owned()];
            if let Some((_, host_part)) = s.split_once("://") {
                items.push(host_part.to_owned());
            }
            items
        })
        .collect();
    needles.sort_by_key(|b| std::cmp::Reverse(b.len()));

    let mut out = text.to_owned();
    for needle in needles {
        out = replace_ignore_ascii_case(&out, &needle, SERVER_PLACEHOLDER);
    }
    ABSOLUTE_URL_RE
        .replace_all(&out, SERVER_PLACEHOLDER)
        .into_owned()
}

fn replace_ignore_ascii_case(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_owned();
    }
    let hay_lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut result = String::with_capacity(haystack.len());
    let mut pos = 0;
    while let Some(idx) = hay_lower[pos..].find(&needle_lower) {
        let abs = pos + idx;
        result.push_str(&haystack[pos..abs]);
        result.push_str(replacement);
        pos = abs + needle.len();
    }
    result.push_str(&haystack[pos..]);
    result
}

/// Computes the stable 32-hex-char SHA-256 `userId` sent to the Listen Together server.
pub fn derive_user_id(account_id: &str, profile_id: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(account_id.as_bytes());
    hasher.update(b":");
    if let Some(pid) = profile_id {
        hasher.update(pid.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Normalizes a user-typed 6-character party code (strips non-alphanumeric, uppercases).
pub fn clean_party_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Control actions sent inside `{"type":"control", ...}` frames to the Go backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ControlAction {
    Play {
        #[serde(rename = "positionMs", skip_serializing_if = "Option::is_none")]
        position_ms: Option<i64>,
    },
    Pause {
        #[serde(rename = "positionMs", skip_serializing_if = "Option::is_none")]
        position_ms: Option<i64>,
    },
    Seek {
        #[serde(rename = "positionMs")]
        position_ms: i64,
    },
    SetTrack {
        track: PartyTrack,
        #[serde(rename = "positionMs")]
        position_ms: i64,
        #[serde(rename = "isPlaying")]
        is_playing: bool,
    },
    SetQueue {
        queue: Vec<PartyTrack>,
        #[serde(rename = "queueIndex")]
        queue_index: i32,
    },
    QueueAdd {
        tracks: Vec<PartyTrack>,
        #[serde(rename = "playNext")]
        play_next: bool,
    },
    QueueRemove {
        #[serde(rename = "videoId")]
        video_id: String,
    },
    QueueClear,
    QueueMove {
        #[serde(rename = "fromIndex")]
        from_index: i32,
        #[serde(rename = "toIndex")]
        to_index: i32,
        #[serde(rename = "videoId", skip_serializing_if = "Option::is_none")]
        video_id: Option<String>,
    },
    Next,
    Previous,
    SetMaxMembers {
        #[serde(rename = "maxMembers")]
        max_members: i32,
    },
    Kick {
        #[serde(rename = "memberId")]
        member_id: String,
    },
    SetHostOnlyControl {
        enabled: bool,
    },
    SetAutoplay {
        enabled: bool,
    },
}

/// Outbound WebSocket frames (`client -> Go backend`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientFrame {
    Ping {
        #[serde(rename = "clientMs")]
        client_ms: i64,
    },
    Sync,
    SyncQueue,
    Report {
        #[serde(rename = "positionMs")]
        position_ms: i64,
        #[serde(rename = "isPlaying")]
        is_playing: bool,
    },
    Control(ControlAction),
}

/// Inbound WebSocket frames (`Go backend -> client`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerFrame {
    Welcome {
        you: Option<PartyMember>,
        party: Box<PartySnapshot>,
        #[serde(rename = "serverMs", default)]
        server_ms: i64,
    },
    Pong {
        #[serde(rename = "clientMs")]
        client_ms: i64,
        #[serde(rename = "serverMs")]
        server_ms: i64,
    },
    State {
        playback: PartyPlayback,
        #[serde(rename = "serverMs", default)]
        server_ms: i64,
    },
    Queue {
        queue: PartyQueue,
        #[serde(rename = "serverMs", default)]
        server_ms: i64,
    },
    Members {
        members: Vec<PartyMember>,
        #[serde(rename = "maxMembers", default)]
        max_members: Option<i32>,
        #[serde(rename = "hostOnlyControl", default)]
        host_only_control: Option<bool>,
        #[serde(rename = "serverMs", default)]
        server_ms: i64,
    },
    Activity {
        action: String,
        by: String,
        #[serde(rename = "atMs", default)]
        at_ms: Option<i64>,
        #[serde(default)]
        detail: String,
    },
    Error {
        #[serde(rename = "error", default)]
        error: String,
        #[serde(default)]
        message: String,
    },
    Bye {
        #[serde(default)]
        reason: String,
    },
}

/// Follow-up actions produced when applying a [`ServerFrame`] to [`ListenTogetherSession`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameEffects {
    /// True when `playback.queue_seq != queue.seq`, instructing the caller to send `syncQueue`.
    pub send_sync_queue: bool,
    /// True when the server terminated membership (`bye`, `bad_token`, or `no_such_party`).
    pub leave_party: bool,
    /// Non-empty party code if the user was kicked and should record a 24h local kick block.
    pub record_kick_code: Option<String>,
}

/// Pure, testable Listen Together session state machine wrapping [`ServerClock`].
pub struct ListenTogetherSession {
    pub state: ListenTogetherState,
    pub activity: Vec<PartyActivity>,
    pub clock: ServerClock,
}

impl Default for ListenTogetherSession {
    fn default() -> Self {
        Self::new(ServerClock::monotonic())
    }
}

impl ListenTogetherSession {
    pub fn new(clock: ServerClock) -> Self {
        Self {
            state: ListenTogetherState::default(),
            activity: Vec::new(),
            clock,
        }
    }

    /// Computes where the party playhead should be right now on this device's clock,
    /// returning `None` if not in a party or before the first `pong` syncs the clock.
    pub fn party_position_ms(&self) -> Option<i64> {
        if !self.state.in_party() {
            return None;
        }
        let server_now = self.clock.server_now_ms()?;
        Some(party_position_ms(&self.state.playback, server_now))
    }

    /// Resets the clock across a socket drop without discarding party membership.
    pub fn on_socket_dropped(&mut self) {
        self.clock.reset();
        self.state.clock_synced = false;
        self.state.connection = Connection::Connecting;
    }

    /// Applies a single [`ServerFrame`] arriving at monotonic local time `received_ms`.
    pub fn apply_server_frame(&mut self, frame: ServerFrame, received_ms: i64) -> FrameEffects {
        let mut effects = FrameEffects::default();
        match frame {
            ServerFrame::Welcome { you, party, .. } => {
                self.state.code = Some(party.code);
                if let Some(member) = you {
                    self.state.you = Some(member);
                }
                self.state.members = party.members;
                self.state.max_members = party.max_members;
                self.state.host_only_control = party.host_only_control;
                self.state.playback = party.playback;
                self.state.queue = party.queue;
                self.state.connection = Connection::Live;
                self.state.error = None;
            }
            ServerFrame::Pong {
                client_ms,
                server_ms,
            } => {
                self.clock.record(client_ms, server_ms, received_ms);
                self.state.clock_synced = self.clock.synced();
                self.state.round_trip_ms = self.clock.round_trip_ms();
            }
            ServerFrame::State { playback, .. } => {
                if playback.seq < self.state.playback.seq {
                    return effects;
                }
                let queue_seq = playback.queue_seq;
                self.state.playback = playback;
                if queue_seq != self.state.queue.seq {
                    effects.send_sync_queue = true;
                }
            }
            ServerFrame::Queue { queue, .. } => {
                if queue.seq < self.state.queue.seq {
                    return effects;
                }
                self.state.queue = queue;
            }
            ServerFrame::Members {
                members,
                max_members,
                host_only_control,
                ..
            } => {
                if let Some(max) = max_members {
                    self.state.max_members = max;
                }
                if let Some(locked) = host_only_control {
                    self.state.host_only_control = locked;
                }
                if let Some(mine) = &self.state.you
                    && let Some(updated) = members.iter().find(|m| m.member_id == mine.member_id)
                {
                    self.state.you = Some(updated.clone());
                }
                self.state.members = members;
            }
            ServerFrame::Activity {
                action,
                by,
                at_ms,
                detail,
            } => {
                if !by.trim().is_empty() {
                    self.activity.insert(
                        0,
                        PartyActivity {
                            action,
                            by,
                            at_ms: at_ms.unwrap_or(received_ms),
                            detail,
                        },
                    );
                    self.activity.truncate(MAX_ACTIVITY_ENTRIES);
                }
            }
            ServerFrame::Error { error, message } => {
                if !message.is_empty() {
                    self.state.error = Some(message);
                }
                if error == "bad_token" || error == "no_such_party" {
                    effects.leave_party = true;
                }
            }
            ServerFrame::Bye { reason } => {
                if reason == "kicked" {
                    effects.record_kick_code = self.state.code.clone();
                }
                effects.leave_party = true;
            }
        }
        effects
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerTimeResponse {
    server_ms: i64,
}

/// REST + WebSocket client for a single Listen Together Go backend base URL.
#[derive(Clone)]
pub struct ListenTogetherClient {
    http: reqwest::Client,
    base_url: String,
}

impl ListenTogetherClient {
    /// Creates a client after validating and normalizing `base_url`.
    pub fn new(http: reqwest::Client, base_url: &str) -> Result<Self, PartyError> {
        let normalized = parse_and_normalize_server_url(base_url)?;
        if normalized.is_empty() {
            return Err(PartyError::Server {
                code: "no_server".to_owned(),
                message: "Set the party server address first.".to_owned(),
                status_code: None,
            });
        }
        Ok(Self {
            http,
            base_url: normalized,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Computes the WebSocket endpoint URL for `code` (`ws://` or `wss://`).
    pub fn ws_url(&self, code: &str) -> String {
        let ws_base = if let Some(rest) = self.base_url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base_url.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            format!("wss://{}", self.base_url)
        };
        format!("{ws_base}/ws/parties/{}", clean_party_code(code))
    }

    /// Probes `GET /healthz` and measures round-trip latency.
    pub async fn healthz(&self, timeout: Duration) -> ProbeResult {
        let started = Instant::now();
        let resp = self
            .http
            .get(format!("{}/healthz", self.base_url))
            .timeout(timeout)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => ProbeResult {
                is_online: true,
                latency_ms: started.elapsed().as_millis() as i64,
            },
            _ => ProbeResult {
                is_online: false,
                latency_ms: 0,
            },
        }
    }

    /// Fetches `GET /api/time` for an initial clock estimate before opening the WebSocket.
    pub async fn server_time(&self) -> Result<i64, PartyError> {
        let resp = self
            .http
            .get(format!("{}/api/time", self.base_url))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(response_to_party_error(resp).await);
        }
        let body: ServerTimeResponse = resp.json().await?;
        Ok(body.server_ms)
    }

    /// Previews party members (`GET /api/parties/{code}/preview`), returning a minimal
    /// preview for older servers whose router returns plain-text 404.
    pub async fn preview_party(&self, code: &str) -> Result<PartyPreview, PartyError> {
        let cleaned = clean_party_code(code);
        if cleaned.len() != CODE_LENGTH {
            return Err(PartyError::Server {
                code: "bad_code".to_owned(),
                message: "A party code is six letters or digits.".to_owned(),
                status_code: None,
            });
        }
        let resp = self
            .http
            .get(format!("{}/api/parties/{cleaned}/preview", self.base_url))
            .send()
            .await?;
        if !resp.status().is_success() {
            let err = response_to_party_error(resp).await;
            if let PartyError::Server {
                code,
                status_code: Some(404),
                ..
            } = &err
                && code == "http_404"
            {
                return Ok(PartyPreview {
                    code: cleaned,
                    host_name: String::new(),
                    member_count: 0,
                    max_members: 5,
                    is_full: false,
                    members: Vec::new(),
                });
            }
            return Err(err);
        }
        Ok(resp.json().await?)
    }

    /// Creates a party and joins it as host (`POST /api/parties`).
    pub async fn create_party(&self, request: &JoinRequest) -> Result<PartyMembership, PartyError> {
        let resp = self
            .http
            .post(format!("{}/api/parties", self.base_url))
            .json(request)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(response_to_party_error(resp).await);
        }
        Ok(resp.json().await?)
    }

    /// Joins an existing party by 6-character code (`POST /api/parties/{code}/join`).
    pub async fn join_party(
        &self,
        code: &str,
        request: &JoinRequest,
    ) -> Result<PartyMembership, PartyError> {
        let cleaned = clean_party_code(code);
        if cleaned.len() != CODE_LENGTH {
            return Err(PartyError::Server {
                code: "bad_code".to_owned(),
                message: "A party code is six letters or digits.".to_owned(),
                status_code: None,
            });
        }
        let resp = self
            .http
            .post(format!("{}/api/parties/{cleaned}/join", self.base_url))
            .json(request)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(response_to_party_error(resp).await);
        }
        Ok(resp.json().await?)
    }

    /// Fetches a full authenticated snapshot (`GET /api/parties/{code}`).
    pub async fn get_party(&self, code: &str, token: &str) -> Result<PartySnapshot, PartyError> {
        let cleaned = clean_party_code(code);
        let resp = self
            .http
            .get(format!("{}/api/parties/{cleaned}", self.base_url))
            .bearer_auth(token)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(response_to_party_error(resp).await);
        }
        Ok(resp.json().await?)
    }

    /// Releases this device's slot (`POST /api/parties/{code}/leave`).
    pub async fn leave_party(&self, code: &str, token: &str) -> Result<(), PartyError> {
        let cleaned = clean_party_code(code);
        let resp = self
            .http
            .post(format!("{}/api/parties/{cleaned}/leave", self.base_url))
            .bearer_auth(token)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(response_to_party_error(resp).await);
        }
        Ok(())
    }

    /// Opens an authenticated native WebSocket to `/ws/parties/{code}` using
    /// `Authorization: Bearer <token>` and no `Origin` header (matching `backend/README.md`).
    pub async fn connect_ws(
        &self,
        code: &str,
        token: &str,
    ) -> Result<ListenTogetherWs, PartyError> {
        let url = self.ws_url(code);
        let mut req = url.into_client_request()?;
        let auth_val =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| PartyError::Server {
                code: "bad_token".to_owned(),
                message: "Party token contains invalid header characters.".to_owned(),
                status_code: None,
            })?;
        req.headers_mut().insert(AUTHORIZATION, auth_val);

        let (stream, _) = connect_async(req).await?;
        Ok(ListenTogetherWs { stream })
    }
}

/// Active WebSocket transport handle speaking typed [`ClientFrame`] / [`ServerFrame`].
pub struct ListenTogetherWs {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl ListenTogetherWs {
    /// Serializes and sends a [`ClientFrame`] as a WebSocket text message.
    pub async fn send_frame(&mut self, frame: &ClientFrame) -> Result<(), PartyError> {
        let text = serde_json::to_string(frame)?;
        self.stream.send(Message::Text(text.into())).await?;
        Ok(())
    }

    /// Receives the next parsed [`ServerFrame`], or `Ok(None)` when the socket closes cleanly.
    pub async fn next_frame(&mut self) -> Result<Option<ServerFrame>, PartyError> {
        while let Some(msg) = self.stream.next().await {
            match msg? {
                Message::Text(text) => {
                    let frame: ServerFrame = serde_json::from_str(&text)?;
                    return Ok(Some(frame));
                }
                Message::Ping(payload) => {
                    let _ = self.stream.send(Message::Pong(payload)).await;
                }
                Message::Close(_) => return Ok(None),
                Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        Ok(None)
    }

    /// Sends a WebSocket close frame.
    pub async fn close(&mut self) -> Result<(), PartyError> {
        self.stream.close(None).await?;
        Ok(())
    }
}

async fn response_to_party_error(resp: reqwest::Response) -> PartyError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<ApiError>(&body).ok();
    let code = parsed
        .as_ref()
        .map(|p| p.code.trim())
        .filter(|c| !c.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("http_{status}"));
    let message = parsed
        .as_ref()
        .map(|p| p.message.trim())
        .filter(|m| !m.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if status == 422 {
                "This account can't be used to jam.".to_owned()
            } else {
                format!("The party server said {status}.")
            }
        });
    PartyError::Server {
        code,
        message,
        status_code: Some(status),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    use super::*;

    // --- Replicated from ListenTogetherIdleHealthTest.kt (all 5 tests) ---

    #[test]
    fn screen_closed_never_probes() {
        assert!(should_skip_idle_probe(false, false, false, None));
        assert!(should_skip_idle_probe(false, false, true, Some(false)));
        assert!(should_skip_idle_probe(false, true, true, Some(false)));
    }

    #[test]
    fn live_socket_never_probes_idle_server() {
        assert!(should_skip_idle_probe(true, true, true, Some(false)));
        assert!(should_skip_idle_probe(true, true, false, None));
    }

    #[test]
    fn metered_with_no_party_skips() {
        assert!(should_skip_idle_probe(true, false, false, Some(true)));
    }

    #[test]
    fn open_screen_probes_on_wifi_or_unknown_network() {
        assert!(!should_skip_idle_probe(true, false, false, Some(false)));
        assert!(!should_skip_idle_probe(true, false, false, None));
    }

    #[test]
    fn in_party_probes_even_on_metered_when_socket_down() {
        assert!(!should_skip_idle_probe(true, false, true, Some(true)));
    }

    // --- URL normalization & redaction ---

    #[test]
    fn parse_and_normalize_server_url_handles_valid_and_invalid_cases() {
        assert_eq!(parse_and_normalize_server_url("   "), Ok(String::new()));
        assert_eq!(
            parse_and_normalize_server_url("API.Example.com/"),
            Ok("https://api.example.com".to_owned())
        );
        assert_eq!(
            parse_and_normalize_server_url("http://LOCALHOST:8000/jam/"),
            Ok("http://localhost:8000/jam".to_owned())
        );
        assert_eq!(
            parse_and_normalize_server_url("http://[::1]:8000"),
            Ok("http://[::1]:8000".to_owned())
        );
        assert_eq!(
            parse_and_normalize_server_url("https://api.example.com /bad"),
            Err(ServerUrlError::Whitespace)
        );
        assert_eq!(
            parse_and_normalize_server_url("ftp://api.example.com"),
            Err(ServerUrlError::InvalidScheme)
        );
        assert_eq!(
            parse_and_normalize_server_url("https://api.example.com?foo=bar"),
            Err(ServerUrlError::HasQuery)
        );
        assert_eq!(
            parse_and_normalize_server_url("https://api.example.com#frag"),
            Err(ServerUrlError::HasFragment)
        );
        assert_eq!(
            parse_and_normalize_server_url("https://api.example.com/a/../b"),
            Err(ServerUrlError::InvalidPath)
        );
        assert_eq!(
            parse_and_normalize_server_url("https://api.example.com:70000"),
            Err(ServerUrlError::InvalidPort)
        );
        assert_eq!(
            parse_and_normalize_server_url("https://singlelabel"),
            Err(ServerUrlError::InvalidHost)
        );
    }

    #[test]
    fn redact_server_urls_strips_known_and_arbitrary_urls() {
        let msg = "failed connecting to https://api.bitchord.test/ws and wss://other.host/p";
        let redacted = redact_server_urls(msg, &["https://api.bitchord.test"]);
        assert!(!redacted.contains("api.bitchord.test"));
        assert!(!redacted.contains("other.host"));
        assert!(redacted.contains(SERVER_PLACEHOLDER));
    }

    // --- State machine: monotonic seq, syncQueue self-healing, host promotion ---

    #[test]
    fn session_enforces_monotonic_seq_and_heals_stale_queue() {
        let now = Arc::new(AtomicI64::new(1_000));
        let reader = Arc::clone(&now);
        let clock: ServerClock =
            ServerClock::with_source(Box::new(move || reader.load(Ordering::Relaxed)));
        let mut session = ListenTogetherSession::new(clock);

        let welcome = ServerFrame::Welcome {
            you: Some(PartyMember {
                member_id: "m2".to_owned(),
                display_name: "Bob".to_owned(),
                is_host: false,
                ..PartyMember::default()
            }),
            party: Box::new(PartySnapshot {
                code: "ABC123".to_owned(),
                host_only_control: true,
                members: vec![
                    PartyMember {
                        member_id: "m1".to_owned(),
                        display_name: "Alice".to_owned(),
                        is_host: true,
                        ..PartyMember::default()
                    },
                    PartyMember {
                        member_id: "m2".to_owned(),
                        display_name: "Bob".to_owned(),
                        is_host: false,
                        ..PartyMember::default()
                    },
                ],
                playback: PartyPlayback {
                    seq: 5,
                    queue_seq: 1,
                    is_playing: true,
                    position_ms: 10_000,
                    anchor_ms: 50_000,
                    ..PartyPlayback::default()
                },
                queue: PartyQueue {
                    seq: 1,
                    index: 0,
                    items: vec![PartyTrack::with_id("v1")],
                },
                ..PartySnapshot::default()
            }),
            server_ms: 50_000,
        };

        let fx = session.apply_server_frame(welcome, 1_000);
        assert!(!fx.send_sync_queue);
        assert!(session.state.controls_locked());

        // Older state frame is dropped unread.
        let stale_state = ServerFrame::State {
            playback: PartyPlayback {
                seq: 4,
                queue_seq: 99,
                position_ms: 1_000,
                ..PartyPlayback::default()
            },
            server_ms: 50_100,
        };
        let fx = session.apply_server_frame(stale_state, 1_100);
        assert!(!fx.send_sync_queue);
        assert_eq!(session.state.playback.seq, 5);

        // Newer state frame with mismatched queue_seq triggers syncQueue.
        let newer_state = ServerFrame::State {
            playback: PartyPlayback {
                seq: 6,
                queue_seq: 2,
                position_ms: 12_000,
                anchor_ms: 52_000,
                is_playing: true,
                ..PartyPlayback::default()
            },
            server_ms: 52_000,
        };
        let fx = session.apply_server_frame(newer_state, 1_200);
        assert!(fx.send_sync_queue);
        assert_eq!(session.state.playback.seq, 6);

        // Host promotion via Members frame unlocks controls for Bob.
        let promoted = ServerFrame::Members {
            members: vec![PartyMember {
                member_id: "m2".to_owned(),
                display_name: "Bob".to_owned(),
                is_host: true,
                ..PartyMember::default()
            }],
            max_members: Some(5),
            host_only_control: None, // absent preserves existing true
            server_ms: 52_500,
        };
        session.apply_server_frame(promoted, 1_300);
        assert!(session.state.host_only_control);
        assert!(!session.state.controls_locked());

        // Kick Bye records the party code and signals leave.
        let fx = session.apply_server_frame(
            ServerFrame::Bye {
                reason: "kicked".to_owned(),
            },
            1_400,
        );
        assert!(fx.leave_party);
        assert_eq!(fx.record_kick_code.as_deref(), Some("ABC123"));
    }

    // --- Live local WebSocket integration test against mock Go protocol ---

    #[tokio::test]
    async fn live_ws_sends_bearer_without_origin_and_exchanges_frames() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut saw_auth = None::<String>;
            let mut saw_origin = false;
            #[allow(clippy::result_large_err)]
            let mut ws = accept_hdr_async(tcp, |req: &Request, resp: Response| {
                saw_auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(ToOwned::to_owned);
                saw_origin = req.headers().contains_key("origin");
                Ok(resp)
            })
            .await
            .expect("ws handshake");

            assert_eq!(saw_auth.as_deref(), Some("Bearer secret-party-token"));
            assert!(!saw_origin, "native client must not send browser Origin");

            // Send welcome snapshot.
            let welcome = r#"{"type":"welcome","you":{"memberId":"m1","displayName":"Ada","isHost":true},"party":{"code":"ABC123","maxMembers":5,"members":[{"memberId":"m1","displayName":"Ada","isHost":true}],"playback":{"seq":1,"queueSeq":1,"isPlaying":true,"positionMs":42000,"anchorMs":50000},"queue":{"seq":1,"index":0,"items":[{"videoId":"v1"}]}},"serverMs":50000}"#;
            ws.send(Message::Text(welcome.into()))
                .await
                .expect("send welcome");

            // Expect ping and reply with pong.
            if let Some(Ok(Message::Text(txt))) = ws.next().await {
                let parsed: ClientFrame = serde_json::from_str(&txt).expect("parse client ping");
                match parsed {
                    ClientFrame::Ping { client_ms } => {
                        let pong =
                            format!(r#"{{"type":"pong","clientMs":{client_ms},"serverMs":50050}}"#);
                        ws.send(Message::Text(pong.into()))
                            .await
                            .expect("send pong");
                    }
                    other => panic!("expected Ping, got {other:?}"),
                }
            }

            // Expect a control frame and verify camelCase wire JSON.
            if let Some(Ok(Message::Text(txt))) = ws.next().await {
                let val: serde_json::Value = serde_json::from_str(&txt).expect("valid json");
                assert_eq!(val["type"], "control");
                assert_eq!(val["action"], "setHostOnlyControl");
                assert_eq!(val["enabled"], true);
            }

            let _ = ws.close(None).await;
        });

        let client = ListenTogetherClient::new(
            reqwest::Client::new(),
            &format!("http://127.0.0.1:{}", addr.port()),
        )
        .expect("valid loopback client");
        let mut ws = client
            .connect_ws("abc123", "secret-party-token")
            .await
            .expect("connect ws");

        let mut session = ListenTogetherSession::default();
        let welcome = ws
            .next_frame()
            .await
            .expect("read welcome")
            .expect("frame present");
        let now_ms = session.clock.now_ms();
        session.apply_server_frame(welcome, now_ms);
        assert_eq!(session.state.code.as_deref(), Some("ABC123"));

        let sent = session.clock.now_ms();
        ws.send_frame(&ClientFrame::Ping { client_ms: sent })
            .await
            .expect("send ping");
        let pong = ws
            .next_frame()
            .await
            .expect("read pong")
            .expect("pong present");
        let recv = session.clock.now_ms();
        session.apply_server_frame(pong, recv);
        assert!(session.state.clock_synced);
        assert!(session.party_position_ms().is_some());

        ws.send_frame(&ClientFrame::Control(ControlAction::SetHostOnlyControl {
            enabled: true,
        }))
        .await
        .expect("send control");

        server.await.expect("server task finished cleanly");
    }
}
