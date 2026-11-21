use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Official BitChord live-install presence endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://api.bitchord.kushagrasingh.in/api/presence";

const DEFAULT_INTERVAL_SEC: u64 = 300;
const MIN_INTERVAL_SEC: u64 = 60;
const MAX_INTERVAL_SEC: u64 = 3600;
const CLOSE_DEBOUNCE_MS: u64 = 2_000;
const DEFAULT_PING_TIMEOUT_SECS: u64 = 15;
const CLOSE_BLOCKING_TIMEOUT_SECS: u64 = 2;

/// Platform identifier sent on the presence wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PresencePlatform {
    Android,
    Pc,
}

impl PresencePlatform {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Android => "android",
            Self::Pc => "pc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresenceCommand {
    Closed,
    Open,
    Shutdown,
}

#[derive(Serialize)]
struct PresencePayload<'a> {
    id: &'a str,
    platform: &'static str,
    open: bool,
}

struct PresenceInner {
    platform: PresencePlatform,
    install_id: String,
    endpoint: String,
    client: reqwest::Client,
    announced: AtomicBool,
    interval_sec: AtomicU64,
}

impl PresenceInner {
    fn next_delay(&self) -> Duration {
        let clamped = self
            .interval_sec
            .load(Ordering::Relaxed)
            .clamp(MIN_INTERVAL_SEC, MAX_INTERVAL_SEC);
        let base_ms = clamped.saturating_mul(1_000);
        let spread = base_ms / 10;
        let jitter_range = spread.saturating_mul(2).saturating_add(1);
        let rand_val = (Uuid::new_v4().as_u128() as u64) % jitter_range;
        let delay_ms = base_ms.saturating_sub(spread).saturating_add(rand_val);
        Duration::from_millis(delay_ms)
    }

    async fn send(&self, open: bool, timeout: Duration) {
        // Set before sending: a ping that times out may still have been counted
        // by the server, so it still needs a close ping.
        if open {
            self.announced.store(true, Ordering::Release);
        }

        let payload = PresencePayload {
            id: &self.install_id,
            platform: self.platform.as_wire(),
            open,
        };

        let response = self
            .client
            .post(&self.endpoint)
            .timeout(timeout)
            .json(&payload)
            .send()
            .await;

        let Ok(resp) = response else {
            return;
        };

        if !open {
            self.announced.store(false, Ordering::Release);
        }

        if !resp.status().is_success() {
            return;
        }

        if let Ok(text) = resp.text().await
            && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text)
            && let Some(interval) = map.get("intervalSec").and_then(Value::as_u64)
        {
            self.interval_sec.store(interval, Ordering::Relaxed);
        }
    }
}

/// Reports whether this BitChord install is currently open so the backend can
/// publish live install counts (`GET /api/stats/live`).
///
/// Only a random per-install UUID and the platform are sent — never anything
/// about the listener or what they play. Requests deliberately omit `Origin`.
pub struct Presence {
    inner: Arc<PresenceInner>,
    tx: watch::Sender<PresenceCommand>,
    handle: Handle,
    _task: JoinHandle<()>,
}

impl Presence {
    /// Start a presence reporter targeting [`DEFAULT_ENDPOINT`] on the current
    /// Tokio runtime.
    pub fn new(platform: PresencePlatform, install_id: impl Into<String>) -> Self {
        Self::with_endpoint(platform, install_id, DEFAULT_ENDPOINT)
    }

    /// Start a presence reporter targeting a custom `endpoint` on the current
    /// Tokio runtime.
    pub fn with_endpoint(
        platform: PresencePlatform,
        install_id: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self::with_client(
            platform,
            install_id,
            endpoint,
            reqwest::Client::new(),
            Handle::current(),
        )
    }

    /// Start a presence reporter with an explicit HTTP client and Tokio runtime
    /// handle.
    pub fn with_client(
        platform: PresencePlatform,
        install_id: impl Into<String>,
        endpoint: impl Into<String>,
        client: reqwest::Client,
        handle: Handle,
    ) -> Self {
        let inner = Arc::new(PresenceInner {
            platform,
            install_id: install_id.into(),
            endpoint: endpoint.into(),
            client,
            announced: AtomicBool::new(false),
            interval_sec: AtomicU64::new(DEFAULT_INTERVAL_SEC),
        });
        let (tx, rx) = watch::channel(PresenceCommand::Closed);
        let task = handle.spawn(run_loop(Arc::clone(&inner), rx));
        Self {
            inner,
            tx,
            handle,
            _task: task,
        }
    }

    /// Update whether the app window/session is currently open.
    ///
    /// Opening triggers an immediate ping followed by periodic heartbeats.
    /// Closing waits [`CLOSE_DEBOUNCE_MS`] (2 s) before sending `open: false`,
    /// and a quick reopen cancels the pending close.
    pub fn set_open(&self, is_open: bool) {
        let target = if is_open {
            PresenceCommand::Open
        } else {
            PresenceCommand::Closed
        };
        self.tx.send_if_modified(|current| {
            if *current == target || *current == PresenceCommand::Shutdown {
                false
            } else {
                *current = target;
                true
            }
        });
    }

    /// Async shutdown that sends `open: false` immediately (2 s timeout) if an
    /// open ping was previously announced.
    pub async fn close_now(&self) {
        let _ = self.tx.send(PresenceCommand::Shutdown);
        if self.inner.announced.load(Ordering::Acquire) {
            self.inner
                .send(false, Duration::from_secs(CLOSE_BLOCKING_TIMEOUT_SECS))
                .await;
        }
    }

    /// Synchronous shutdown for process exit: sends `open: false` on the
    /// calling thread (bounded by a 2 s timeout) before returning.
    pub fn close_blocking(&self) {
        let _ = self.tx.send(PresenceCommand::Shutdown);
        if !self.inner.announced.load(Ordering::Acquire) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let handle = self.handle.clone();
        // Run block_on on a short-lived helper thread so this never panics
        // even if invoked from inside a Tokio worker thread.
        let join = std::thread::spawn(move || {
            handle.block_on(inner.send(false, Duration::from_secs(CLOSE_BLOCKING_TIMEOUT_SECS)));
        });
        let _ = join.join();
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        let _ = self.tx.send(PresenceCommand::Shutdown);
    }
}

async fn run_loop(inner: Arc<PresenceInner>, mut rx: watch::Receiver<PresenceCommand>) {
    let mut state = *rx.borrow_and_update();
    loop {
        match state {
            PresenceCommand::Shutdown => break,
            PresenceCommand::Open => {
                tokio::select! {
                    biased;
                    changed = rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        state = *rx.borrow_and_update();
                    }
                    () = async {
                        loop {
                            inner.send(true, Duration::from_secs(DEFAULT_PING_TIMEOUT_SECS)).await;
                            tokio::time::sleep(inner.next_delay()).await;
                        }
                    } => {}
                }
            }
            PresenceCommand::Closed => {
                if inner.announced.load(Ordering::Acquire) {
                    tokio::select! {
                        biased;
                        changed = rx.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            state = *rx.borrow_and_update();
                        }
                        () = async {
                            tokio::time::sleep(Duration::from_millis(CLOSE_DEBOUNCE_MS)).await;
                            inner.send(false, Duration::from_secs(DEFAULT_PING_TIMEOUT_SECS)).await;
                        } => {
                            if rx.changed().await.is_err() {
                                break;
                            }
                            state = *rx.borrow_and_update();
                        }
                    }
                } else {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    state = *rx.borrow_and_update();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    const TEST_ID: &str = "3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b";

    struct RecordedRequest {
        method: String,
        path: String,
        has_origin: bool,
        body: Value,
    }

    async fn spawn_mock_server() -> (String, mpsc::UnboundedReceiver<RecordedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos;
                        }
                    };

                    let headers_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let mut lines = headers_str.lines();
                    let request_line = lines.next().unwrap_or_default();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_string();
                    let path = parts.next().unwrap_or_default().to_string();

                    let mut content_len: usize = 0;
                    let mut has_origin = false;
                    for line in lines {
                        if let Some((name, val)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                content_len = val.trim().parse().unwrap_or(0);
                            } else if name.eq_ignore_ascii_case("origin") {
                                has_origin = true;
                            }
                        }
                    }

                    let body_start = header_end + 4;
                    while buf.len() < body_start + content_len {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                    }

                    let body_bytes = &buf[body_start..(body_start + content_len).min(buf.len())];
                    let body: Value = serde_json::from_slice(body_bytes).unwrap_or(Value::Null);

                    let _ = tx.send(RecordedRequest {
                        method,
                        path,
                        has_origin,
                        body,
                    });

                    let resp_body = r#"{"intervalSec":300}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        (format!("http://{addr}/api/presence"), rx)
    }

    async fn next_ping(
        rx: &mut mpsc::UnboundedReceiver<RecordedRequest>,
        timeout: Duration,
    ) -> Option<Value> {
        let req = tokio::time::timeout(timeout, rx.recv()).await.ok()??;
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/presence");
        assert!(
            !req.has_origin,
            "the server refuses pings that carry an Origin"
        );
        Some(req.body)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opens_then_closes_after_the_debounce() {
        let (url, mut rx) = spawn_mock_server().await;
        let presence = Presence::with_endpoint(PresencePlatform::Pc, TEST_ID, url);

        presence.set_open(true);
        let open = next_ping(&mut rx, Duration::from_secs(5))
            .await
            .expect("open ping");
        assert_eq!(open["id"], TEST_ID);
        assert_eq!(open["platform"], "pc");
        assert_eq!(open["open"], true);

        presence.set_open(false);
        assert!(
            next_ping(&mut rx, Duration::from_secs(1)).await.is_none(),
            "close should wait out the debounce"
        );
        let close = next_ping(&mut rx, Duration::from_secs(5))
            .await
            .expect("close ping");
        assert_eq!(close["open"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quick_reopen_is_not_a_close() {
        let (url, mut rx) = spawn_mock_server().await;
        let presence = Presence::with_endpoint(PresencePlatform::Android, TEST_ID, url);

        presence.set_open(true);
        let open = next_ping(&mut rx, Duration::from_secs(5))
            .await
            .expect("initial open ping");
        assert_eq!(open["platform"], "android");

        presence.set_open(false);
        tokio::time::sleep(Duration::from_millis(300)).await;
        presence.set_open(true);

        let reopen = next_ping(&mut rx, Duration::from_secs(5))
            .await
            .expect("reopen ping");
        assert_eq!(reopen["open"], true);
        assert!(
            next_ping(&mut rx, Duration::from_secs(3)).await.is_none(),
            "replaced close must never go out"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn never_opened_sends_nothing() {
        let (url, mut rx) = spawn_mock_server().await;
        let presence = Presence::with_endpoint(PresencePlatform::Pc, TEST_ID, url);

        presence.set_open(false);
        presence.close_blocking();
        assert!(next_ping(&mut rx, Duration::from_secs(2)).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_blocking_sends_on_the_calling_thread() {
        let (url, mut rx) = spawn_mock_server().await;
        let presence = Presence::with_endpoint(PresencePlatform::Pc, TEST_ID, url);

        presence.set_open(true);
        next_ping(&mut rx, Duration::from_secs(5))
            .await
            .expect("open ping");

        presence.close_blocking();
        // Already sent by the time close_blocking returns: no waiting.
        let req = rx.try_recv().expect("close ping already received");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/presence");
        assert!(!req.has_origin);
        assert_eq!(req.body["open"], false);
    }
}
