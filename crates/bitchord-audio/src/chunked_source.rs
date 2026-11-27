//! Progressive-streaming, read-ahead bounded HTTP `Range` [`MediaSource`]
//! defeating `googlevideo.com`'s ~15.5 kB/s unbounded stream throttling while
//! eliminating startup stalls and chunk-boundary audio dropouts.
//!
//! Architecture:
//! - Uses bounded `128 KiB` ranges (capped by `PlayerClient::range_bytes_for(url)`
//!   and `clen`), completing rapidly even on 200 KB/s links and keeping EOF Cues/moov
//!   seeks small.
//! - Streams each chunk progressively via `resp.chunk().await` into a shared
//!   `Mutex + Condvar` buffer so `symphonia` parses headers and starts decoding
//!   at first-byte TTFB (~150 ms) instead of blocking on a full 1 MiB download.
//! - Proactively prefetches the next `PREFETCH_CHUNKS_AHEAD = 2` chunks
//!   (`256 KiB`, ~14 seconds of Opus audio) asynchronously on Tokio so crossing
//!   chunk boundaries never stalls the decode thread.
//! - Cancels all in-flight Tokio fetch tasks immediately on `Drop`, caches recent
//!   chunks up to `MAX_CACHE_BYTES = 32 MiB` for instant backward seeks, retries
//!   truncated ranges up to `MAX_EMPTY_RANGES = 3`, and reports HTTP `403`/`404`/`410`
//!   refusals back to `StreamResolver`.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use bitchord_innertube::{PlayerClient, ResolvedStream};
use reqwest::Client;
use symphonia::core::io::MediaSource;
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Default bounded range request size (128 KiB — fast TTFB + small EOF cue seeks).
pub const DEFAULT_CHUNK_BYTES: u64 = 131_072;
/// Number of upcoming sequential chunks prefetched asynchronously ahead of read cursor.
pub const PREFETCH_CHUNKS_AHEAD: u64 = 4;
/// Maximum consecutive empty/truncated range attempts before signaling EOF.
pub const MAX_EMPTY_RANGES: usize = 3;
/// Maximum transient HTTP/network retries per chunk before propagating error.
pub const MAX_TRANSIENT_RETRIES: usize = 3;
/// Maximum in-memory cached chunk bytes per active stream (32 MiB).
pub const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const RETRY_BASE_BACKOFF_MS: u64 = 150;

/// Refusal callback invoked when an HTTP range fetch receives 403/404/410.
pub type RefusalCallback = Arc<dyn Fn(&str, u16) + Send + Sync>;

/// Shared cancellation token that immediately aborts in-flight HTTP range
/// requests and wakes any blocked [`ChunkedRangeSource::read`] caller so
/// track switches/stops never hang waiting on a stalled socket.
#[derive(Clone, Default)]
pub struct StreamCancelToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
    slots: Arc<Mutex<Vec<Weak<ChunkSlot>>>>,
}

impl std::fmt::Debug for StreamCancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamCancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl StreamCancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Cancels all active HTTP range fetches and immediately wakes any thread
    /// blocked inside `ChunkedRangeSource::read()` or initialization.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
        let live_slots: Vec<Arc<ChunkSlot>> = {
            let mut guard = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            let mut live = Vec::with_capacity(guard.len());
            guard.retain(|weak| {
                if let Some(arc) = weak.upgrade() {
                    live.push(arc);
                    true
                } else {
                    false
                }
            });
            live
        };
        for slot in live_slots {
            slot.finish_cancelled();
        }
    }

    fn register_slot(&self, slot: &Arc<ChunkSlot>) {
        let mut guard = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|w| w.strong_count() > 0);
        guard.push(Arc::downgrade(slot));
        if self.is_cancelled() {
            slot.finish_cancelled();
        }
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

struct ChunkBuffer {
    bytes: Vec<u8>,
    done: bool,
    header_ready: bool,
    discovered_total: Option<u64>,
    error: Option<(io::ErrorKind, String)>,
}

struct ChunkSlot {
    state: Mutex<ChunkBuffer>,
    ready: Condvar,
}

impl ChunkSlot {
    fn new(expected_capacity: usize) -> Self {
        Self {
            state: Mutex::new(ChunkBuffer {
                bytes: Vec::with_capacity(expected_capacity.min(262_144)),
                done: false,
                header_ready: false,
                discovered_total: None,
                error: None,
            }),
            ready: Condvar::new(),
        }
    }

    fn finish_with_error(&self, kind: io::ErrorKind, msg: String) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.error = Some((kind, msg));
        guard.header_ready = true;
        guard.done = true;
        self.ready.notify_all();
    }

    fn finish_cancelled(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.done {
            guard.error = Some((
                io::ErrorKind::ConnectionAborted,
                "HTTP range stream cancelled".to_owned(),
            ));
            guard.header_ready = true;
            guard.done = true;
        }
        self.ready.notify_all();
    }

    fn finish_ok(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.header_ready = true;
        guard.done = true;
        self.ready.notify_all();
    }
}

/// Ensures any waiting reader wakes up even if the Tokio task is aborted/cancelled.
struct SlotCompletionGuard(Arc<ChunkSlot>);

impl Drop for SlotCompletionGuard {
    fn drop(&mut self) {
        let mut guard = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.done {
            guard.header_ready = true;
            guard.done = true;
            self.0.ready.notify_all();
        }
    }
}

struct ActiveChunk {
    slot: Arc<ChunkSlot>,
    task: JoinHandle<()>,
}

/// Seekable, progressive-streaming, read-ahead HTTP range [`MediaSource`] for `symphonia`.
pub struct ChunkedRangeSource {
    http: Client,
    rt: Handle,
    url: String,
    headers: Arc<Vec<(String, String)>>,
    total_len: Option<u64>,
    chunk_bytes: u64,
    pos: u64,
    chunks: BTreeMap<u64, ActiveChunk>,
    cache_order: VecDeque<u64>,
    on_refused: Option<RefusalCallback>,
    cancel: StreamCancelToken,
}

impl ChunkedRangeSource {
    /// Builds a `ChunkedRangeSource` from a [`ResolvedStream`] and Tokio runtime handle.
    pub fn from_resolved(
        http: Client,
        rt: Handle,
        stream: &ResolvedStream,
        on_refused: Option<RefusalCallback>,
    ) -> Result<Self, io::Error> {
        Self::from_resolved_with_cancel(http, rt, stream, on_refused, StreamCancelToken::new())
    }

    /// Builds a `ChunkedRangeSource` with a caller-owned [`StreamCancelToken`].
    pub fn from_resolved_with_cancel(
        http: Client,
        rt: Handle,
        stream: &ResolvedStream,
        on_refused: Option<RefusalCallback>,
        cancel: StreamCancelToken,
    ) -> Result<Self, io::Error> {
        let headers: Vec<(String, String)> = stream
            .media_headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let total_len = stream
            .content_length
            .or_else(|| PlayerClient::content_length_from_url(&stream.url));
        let client_cap = stream
            .range_chunk_bytes
            .or_else(|| PlayerClient::range_bytes_for(&stream.url))
            .unwrap_or(DEFAULT_CHUNK_BYTES);
        let chunk_bytes = DEFAULT_CHUNK_BYTES.min(client_cap).max(65_536);

        Self::new_with_cancel(
            http,
            rt,
            stream.url.clone(),
            headers,
            total_len,
            chunk_bytes,
            on_refused,
            cancel,
        )
    }

    /// Creates a `ChunkedRangeSource` with explicit parameters, starting chunk 0
    /// immediately when `total_len` is unknown so total byte length is discovered
    /// without wasting a second request.
    pub fn new(
        http: Client,
        rt: Handle,
        url: String,
        headers: Vec<(String, String)>,
        total_len: Option<u64>,
        chunk_bytes: u64,
        on_refused: Option<RefusalCallback>,
    ) -> Result<Self, io::Error> {
        Self::new_with_cancel(
            http,
            rt,
            url,
            headers,
            total_len,
            chunk_bytes,
            on_refused,
            StreamCancelToken::new(),
        )
    }

    /// Creates a `ChunkedRangeSource` attached to an external [`StreamCancelToken`].
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_cancel(
        http: Client,
        rt: Handle,
        url: String,
        headers: Vec<(String, String)>,
        total_len: Option<u64>,
        chunk_bytes: u64,
        on_refused: Option<RefusalCallback>,
        cancel: StreamCancelToken,
    ) -> Result<Self, io::Error> {
        let chunk_bytes = chunk_bytes.max(4_096);
        let mut src = Self {
            http,
            rt,
            url,
            headers: Arc::new(headers),
            total_len,
            chunk_bytes,
            pos: 0,
            chunks: BTreeMap::new(),
            cache_order: VecDeque::new(),
            on_refused,
            cancel,
        };

        if src.total_len.is_none() {
            // Prime chunk 0 and wait only for its HTTP response headers (or error)
            // to learn total_len from Content-Range / Content-Length.
            if let Some(slot) = src.ensure_chunk_started(0, true) {
                let mut guard = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                while !guard.header_ready && !guard.done && !src.cancel.is_cancelled() {
                    guard = slot.ready.wait(guard).unwrap_or_else(|e| e.into_inner());
                }
                if src.cancel.is_cancelled() {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "HTTP range stream cancelled",
                    ));
                }
                if let Some((kind, ref msg)) = guard.error {
                    return Err(io::Error::new(kind, msg.clone()));
                }
                if let Some(discovered) = guard.discovered_total {
                    src.total_len = Some(discovered);
                } else {
                    while !guard.done && !src.cancel.is_cancelled() {
                        guard = slot.ready.wait(guard).unwrap_or_else(|e| e.into_inner());
                    }
                    if src.cancel.is_cancelled() {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "HTTP range stream cancelled",
                        ));
                    }
                    if let Some((kind, ref msg)) = guard.error {
                        return Err(io::Error::new(kind, msg.clone()));
                    }
                    src.total_len = guard.discovered_total.or(Some(guard.bytes.len() as u64));
                }
            }
        }
        Ok(src)
    }

    /// Returns a cloneable [`StreamCancelToken`] bound to this HTTP range source.
    pub fn cancel_token(&self) -> StreamCancelToken {
        self.cancel.clone()
    }

    fn ensure_chunk_started(&mut self, chunk_idx: u64, retry_failed: bool) -> Option<Arc<ChunkSlot>> {
        if let Some(existing) = self.chunks.get(&chunk_idx) {
            let should_retry = retry_failed
                && !self.cancel.is_cancelled()
                && {
                    let guard = existing.slot.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.done && is_retryable_slot_error(guard.error.as_ref())
                };
            if !should_retry {
                return Some(Arc::clone(&existing.slot));
            }
            if let Some(removed) = self.chunks.remove(&chunk_idx) {
                removed.task.abort();
            }
            if let Some(pos) = self.cache_order.iter().position(|&id| id == chunk_idx) {
                self.cache_order.remove(pos);
            }
        }
        let start = chunk_idx.saturating_mul(self.chunk_bytes);
        let max_len = match self.total_len {
            Some(total) if start >= total => return None,
            Some(total) => self.chunk_bytes.min(total - start),
            None => self.chunk_bytes,
        };

        let slot = Arc::new(ChunkSlot::new(max_len as usize));
        self.cancel.register_slot(&slot);
        let slot_task = Arc::clone(&slot);
        let http = self.http.clone();
        let url = self.url.clone();
        let headers = Arc::clone(&self.headers);
        let on_refused = self.on_refused.clone();
        let cancel = self.cancel.clone();
        let known_total = self.total_len;

        let task = self.rt.spawn(async move {
            let _guard = SlotCompletionGuard(Arc::clone(&slot_task));
            fetch_chunk_streaming(
                http,
                url,
                headers,
                start,
                max_len,
                known_total,
                on_refused,
                cancel,
                slot_task,
            )
            .await;
        });

        self.chunks.insert(
            chunk_idx,
            ActiveChunk {
                slot: Arc::clone(&slot),
                task,
            },
        );
        self.cache_order.push_back(chunk_idx);
        self.evict_if_needed(chunk_idx);
        Some(slot)
    }

    fn prefetch_ahead(&mut self, current_chunk: u64) {
        let Some(total) = self.total_len else {
            return;
        };
        for step in 1..=PREFETCH_CHUNKS_AHEAD {
            let next_idx = current_chunk.saturating_add(step);
            if next_idx.saturating_mul(self.chunk_bytes) >= total {
                break;
            }
            let _ = self.ensure_chunk_started(next_idx, false);
        }
    }

    fn evict_if_needed(&mut self, protected_chunk: u64) {
        let mut total_cached: usize = self
            .chunks
            .values()
            .map(|c| {
                c.slot
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .bytes
                    .len()
            })
            .sum();

        if total_cached <= MAX_CACHE_BYTES {
            return;
        }

        let protected_end = protected_chunk.saturating_add(PREFETCH_CHUNKS_AHEAD);
        let mut kept = VecDeque::with_capacity(self.cache_order.len());
        while let Some(idx) = self.cache_order.pop_front() {
            if total_cached <= MAX_CACHE_BYTES || (idx >= protected_chunk && idx <= protected_end) {
                kept.push_back(idx);
                continue;
            }
            let can_evict = self
                .chunks
                .get(&idx)
                .is_some_and(|c| c.slot.state.lock().unwrap_or_else(|e| e.into_inner()).done);
            if can_evict && let Some(removed) = self.chunks.remove(&idx) {
                let bytes_len = removed
                    .slot
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .bytes
                    .len();
                total_cached = total_cached.saturating_sub(bytes_len);
            } else {
                kept.push_back(idx);
            }
        }
        self.cache_order = kept;
    }
}

impl Drop for ChunkedRangeSource {
    fn drop(&mut self) {
        self.cancel.cancel();
        for chunk in self.chunks.values() {
            chunk.task.abort();
        }
    }
}

/// Returns `true` when a finished chunk slot failed due to a transient/network
/// error and should be evicted & refetched on a subsequent read/retry, while
/// keeping permanent refusals (403/404/410), protocol errors, and cancellations
/// sticky so they are never spammed.
fn is_retryable_slot_error(error: Option<&(io::ErrorKind, String)>) -> bool {
    match error {
        None => false,
        Some((
            io::ErrorKind::ConnectionAborted
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::InvalidData,
            _,
        )) => false,
        Some(_) => true,
    }
}

async fn sleep_or_cancel(delay: Duration, cancel: &StreamCancelToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => true,
        () = tokio::time::sleep(delay) => cancel.is_cancelled(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_chunk_streaming(
    http: Client,
    url: String,
    headers: Arc<Vec<(String, String)>>,
    start: u64,
    max_len: u64,
    mut known_total: Option<u64>,
    on_refused: Option<RefusalCallback>,
    cancel: StreamCancelToken,
    slot: Arc<ChunkSlot>,
) {
    let mut cursor = start;
    let mut remaining = max_len;
    let mut empty_attempts = 0usize;
    let mut transient_retries = 0usize;

    while remaining > 0 && !cancel.is_cancelled() {
        let end_inclusive = cursor.saturating_add(remaining.saturating_sub(1));
        let range_header = format!("bytes={cursor}-{end_inclusive}");

        let mut req = http
            .get(&url)
            .timeout(REQUEST_TIMEOUT)
            .header(reqwest::header::RANGE, range_header);
        for (k, v) in headers.iter() {
            req = req.header(k.as_str(), v.as_str());
        }

        let send_outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                slot.finish_cancelled();
                return;
            }
            res = req.send() => res,
        };

        let mut resp = match send_outcome {
            Ok(r) => r,
            Err(e) => {
                if cancel.is_cancelled() {
                    slot.finish_cancelled();
                    return;
                }
                if transient_retries < MAX_TRANSIENT_RETRIES {
                    transient_retries += 1;
                    let backoff = Duration::from_millis(
                        RETRY_BASE_BACKOFF_MS.saturating_mul(1u64 << (transient_retries - 1)),
                    );
                    if sleep_or_cancel(backoff, &cancel).await {
                        slot.finish_cancelled();
                        return;
                    }
                    continue;
                }
                slot.finish_with_error(io::ErrorKind::Other, format!("HTTP range error: {e}"));
                return;
            }
        };

        let status = resp.status().as_u16();
        if matches!(status, 403 | 404 | 410) {
            if let Some(cb) = &on_refused {
                cb(&url, status);
            }
            slot.finish_with_error(
                io::ErrorKind::PermissionDenied,
                format!("stream range refused with HTTP {status}"),
            );
            return;
        }
        if status == 416 {
            slot.finish_ok();
            return;
        }
        if status == 200 && cursor > 0 {
            slot.finish_with_error(
                io::ErrorKind::InvalidData,
                "server answered 200 OK instead of 206 Partial Content for ranged seek".to_owned(),
            );
            return;
        }
        if (status == 429 || (500..=599).contains(&status))
            && transient_retries < MAX_TRANSIENT_RETRIES
        {
            transient_retries += 1;
            let backoff = Duration::from_millis(
                RETRY_BASE_BACKOFF_MS.saturating_mul(1u64 << (transient_retries - 1)),
            );
            if sleep_or_cancel(backoff, &cancel).await {
                slot.finish_cancelled();
                return;
            }
            continue;
        }
        if status != 206 && status != 200 {
            slot.finish_with_error(
                io::ErrorKind::Other,
                format!("unexpected HTTP status {status}"),
            );
            return;
        }

        let discovered_total = if status == 206 {
            resp.headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.rsplit_once('/'))
                .and_then(|(_, tot)| tot.parse::<u64>().ok())
        } else {
            resp.content_length()
        };

        if known_total.is_none()
            && let Some(tot) = discovered_total
        {
            known_total = Some(tot);
        }

        {
            let mut guard = slot.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.discovered_total.is_none() {
                guard.discovered_total = known_total;
            }
            guard.header_ready = true;
            slot.ready.notify_all();
        }

        let mut got_this_req = 0u64;
        let mut body_retry = false;
        loop {
            if cancel.is_cancelled() {
                slot.finish_cancelled();
                return;
            }
            let next_piece = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    slot.finish_cancelled();
                    return;
                }
                chunk = resp.chunk() => chunk,
            };
            match next_piece {
                Ok(Some(piece)) => {
                    if piece.is_empty() {
                        continue;
                    }
                    let take = (piece.len() as u64).min(remaining) as usize;
                    if take > 0 {
                        let mut guard = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.bytes.extend_from_slice(&piece[..take]);
                        slot.ready.notify_all();
                        got_this_req = got_this_req.saturating_add(take as u64);
                        cursor = cursor.saturating_add(take as u64);
                        remaining = remaining.saturating_sub(take as u64);
                    }
                    if remaining == 0 {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    if cancel.is_cancelled() {
                        slot.finish_cancelled();
                        return;
                    }
                    if transient_retries < MAX_TRANSIENT_RETRIES {
                        transient_retries += 1;
                        let backoff = Duration::from_millis(
                            RETRY_BASE_BACKOFF_MS.saturating_mul(1u64 << (transient_retries - 1)),
                        );
                        if sleep_or_cancel(backoff, &cancel).await {
                            slot.finish_cancelled();
                            return;
                        }
                        body_retry = true;
                        break;
                    }
                    slot.finish_with_error(
                        io::ErrorKind::Other,
                        format!("HTTP range body error: {e}"),
                    );
                    return;
                }
            }
        }

        if body_retry {
            continue;
        }

        if got_this_req == 0 {
            empty_attempts += 1;
            if empty_attempts >= MAX_EMPTY_RANGES {
                break;
            }
            continue;
        }
        empty_attempts = 0;
        transient_retries = 0;

        if let Some(total) = known_total
            && cursor >= total
        {
            break;
        }
    }

    if cancel.is_cancelled() {
        slot.finish_cancelled();
    } else {
        slot.finish_ok();
    }
}

impl Read for ChunkedRangeSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "HTTP range stream cancelled",
            ));
        }
        if let Some(total) = self.total_len
            && self.pos >= total
        {
            return Ok(0);
        }

        let chunk_idx = self.pos / self.chunk_bytes;
        let offset_in_chunk = (self.pos % self.chunk_bytes) as usize;

        let Some(slot) = self.ensure_chunk_started(chunk_idx, true) else {
            return Ok(0);
        };
        // Trigger non-blocking background prefetch of upcoming sequential chunks.
        self.prefetch_ahead(chunk_idx);

        let mut guard = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.cancel.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "HTTP range stream cancelled",
                ));
            }
            if self.total_len.is_none()
                && let Some(tot) = guard.discovered_total
            {
                self.total_len = Some(tot);
            }
            if offset_in_chunk < guard.bytes.len() {
                let avail = &guard.bytes[offset_in_chunk..];
                let to_copy = buf.len().min(avail.len());
                buf[..to_copy].copy_from_slice(&avail[..to_copy]);
                drop(guard);
                self.pos = self.pos.saturating_add(to_copy as u64);
                return Ok(to_copy);
            }
            if let Some((kind, ref msg)) = guard.error {
                return Err(io::Error::new(kind, msg.clone()));
            }
            if guard.done {
                return Ok(0);
            }
            guard = slot.ready.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Seek for ChunkedRangeSource {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let target: i128 = match style {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(delta) => i128::from(self.pos) + i128::from(delta),
            SeekFrom::End(delta) => {
                let total = self.total_len.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "unknown stream byte length")
                })?;
                i128::from(total) + i128::from(delta)
            }
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before byte 0",
            ));
        }
        let clamped = match self.total_len {
            Some(total) => (target as u64).min(total),
            None => target as u64,
        };
        self.pos = clamped;
        Ok(self.pos)
    }
}

impl MediaSource for ChunkedRangeSource {
    fn is_seekable(&self) -> bool {
        self.total_len.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.total_len
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
    use std::time::Instant;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn fetches_bounded_206_chunks_caches_backward_seeks_and_reports_403() {
        let payload: Vec<u8> = (0..12_000usize).map(|i| (i % 251) as u8).collect();
        let payload_arc = Arc::new(payload.clone());
        let requests = Arc::new(AtomicUsize::new(0));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let req_counter = Arc::clone(&requests);
        let data = Arc::clone(&payload_arc);

        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let req_counter = Arc::clone(&req_counter);
                let data = Arc::clone(&data);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let req_text = String::from_utf8_lossy(&buf[..n]);
                    req_counter.fetch_add(1, Ordering::Relaxed);

                    if req_text.contains("/forbidden") {
                        let _ = sock
                            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        return;
                    }

                    let range_line = req_text
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .unwrap_or("Range: bytes=0-4095");
                    let bounds = range_line.split('=').nth(1).unwrap_or("0-4095").trim();
                    let (s_str, e_str) = bounds.split_once('-').unwrap_or(("0", "4095"));
                    let start: usize = s_str.parse().unwrap_or(0);
                    let end: usize = e_str
                        .parse::<usize>()
                        .unwrap_or(data.len() - 1)
                        .min(data.len() - 1);
                    let slice = &data[start..=end];
                    let header = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len(),
                        slice.len()
                    );
                    let _ = sock.write_all(header.as_bytes()).await;
                    let _ = sock.write_all(slice).await;
                });
            }
        });

        let handle = Handle::current();
        let url = format!("http://127.0.0.1:{}/audio.bin", addr.port());
        let client = Client::new();

        // Perform blocking MediaSource reads on spawn_blocking (just like the decode OS thread).
        let read_res = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new(
                client.clone(),
                handle.clone(),
                url,
                Vec::new(),
                Some(12_000),
                4_096,
                None,
            )
            .expect("create range source");

            let mut all = Vec::new();
            src.read_to_end(&mut all).expect("read all");
            assert_eq!(all.len(), 12_000);

            // Backward seek to byte 100 must hit memory cache without extra HTTP call.
            src.seek(SeekFrom::Start(100)).expect("seek back");
            let mut slice = [0u8; 32];
            src.read_exact(&mut slice).expect("read cached");
            (all, slice)
        })
        .await
        .expect("blocking join");

        assert_eq!(read_res.0, payload);
        assert_eq!(&read_res.1[..], &payload[100..132]);
        assert_eq!(requests.load(Ordering::Relaxed), 3); // 3 chunks of 4096 B, 0 on cached seek!

        // Verify 403 triggers refusal callback.
        let refused_code = Arc::new(AtomicU16::new(0));
        let cb_code = Arc::clone(&refused_code);
        let bad_url = format!("http://127.0.0.1:{}/forbidden", addr.port());
        let handle2 = Handle::current();
        let err = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new(
                Client::new(),
                handle2,
                bad_url,
                Vec::new(),
                Some(4_096),
                4_096,
                Some(Arc::new(move |_u, code| {
                    cb_code.store(code, Ordering::Relaxed);
                })),
            )
            .expect("init forbidden source");
            let mut b = [0u8; 16];
            src.read(&mut b)
        })
        .await
        .expect("join forbidden");
        assert!(err.is_err());
        assert_eq!(refused_code.load(Ordering::Relaxed), 403);

        let _ = server.await;
    }

    #[tokio::test]
    async fn streams_first_bytes_before_chunk_finishes_downloading() {
        // Simulates a throttled server delivering the first 512 B immediately,
        // pausing 250 ms, then sending the remaining 3584 B.
        // Progressive read() MUST return the first bytes immediately (<100 ms),
        // NOT wait for the full chunk to finish!
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let head = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-4095/4096\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&[0xABu8; 512]).await;
                let _ = sock.flush().await;
                tokio::time::sleep(Duration::from_millis(250)).await;
                let _ = sock.write_all(&[0xCDu8; 3584]).await;
            }
        });

        let handle = Handle::current();
        let url = format!("http://127.0.0.1:{}/slow.bin", addr.port());
        let elapsed_first_read = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new(
                Client::new(),
                handle,
                url,
                Vec::new(),
                Some(4_096),
                4_096,
                None,
            )
            .expect("create");
            let t0 = Instant::now();
            let mut first = [0u8; 64];
            src.read_exact(&mut first).expect("first read");
            assert_eq!(first, [0xABu8; 64]);
            t0.elapsed()
        })
        .await
        .expect("join");

        assert!(
            elapsed_first_read < Duration::from_millis(180),
            "expected progressive first read before 250ms stall, took {:?}",
            elapsed_first_read
        );
        let _ = server.await;
    }

    #[tokio::test]
    async fn cancel_unblocks_stalled_http_chunk_under_200ms() {
        // Server accepts TCP connection and never responds (simulating dead Wi-Fi / stall).
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let _server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });

        let cancel = StreamCancelToken::new();
        let cancel_trigger = cancel.clone();
        let handle = Handle::current();
        let url = format!("http://127.0.0.1:{}/stalled.bin", addr.port());

        let reader_task = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new_with_cancel(
                Client::new(),
                handle,
                url,
                Vec::new(),
                Some(16_384),
                4_096,
                None,
                cancel,
            )
            .expect("create stalled source");
            let mut buf = [0u8; 64];
            src.read(&mut buf)
        });

        // Give the reader a moment to block waiting on the stalled server, then cancel.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let t0 = Instant::now();
        cancel_trigger.cancel();
        let res = reader_task.await.expect("join reader");
        let elapsed = t0.elapsed();

        assert!(res.is_err(), "cancelled stalled read must return error");
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        assert!(
            elapsed < Duration::from_millis(200),
            "cancelled read must return in <200ms, took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn retries_transient_503_and_truncated_body_resuming_from_cursor() {
        let payload: Vec<u8> = (0..4_096usize).map(|i| (i % 199) as u8).collect();
        let payload_arc = Arc::new(payload.clone());
        let attempt = Arc::new(AtomicUsize::new(0));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let att_srv = Arc::clone(&attempt);
        let data_srv = Arc::clone(&payload_arc);

        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let n_try = att_srv.fetch_add(1, Ordering::SeqCst);
                let data = Arc::clone(&data_srv);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    if n_try == 0 {
                        // First attempt: transient 503.
                        let _ = sock
                            .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    } else if n_try == 1 {
                        // Second attempt: send half the chunk and drop connection mid-body.
                        let head = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-4095/4096\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(&data[..1_024]).await;
                    } else {
                        // Third attempt: resume from byte 1024 to end!
                        let slice = &data[1_024..];
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 1024-4095/4096\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            slice.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(slice).await;
                    }
                });
            }
        });

        let handle = Handle::current();
        let url = format!("http://127.0.0.1:{}/flaky.bin", addr.port());
        let got = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new(
                Client::new(),
                handle,
                url,
                Vec::new(),
                Some(4_096),
                4_096,
                None,
            )
            .expect("create flaky");
            let mut all = Vec::new();
            src.read_to_end(&mut all).expect("read after retries");
            all
        })
        .await
        .expect("join");

        assert_eq!(got, payload);
        assert_eq!(attempt.load(Ordering::SeqCst), 3);
        let _ = server.await;
    }

    #[tokio::test]
    async fn evicts_and_refetches_transient_failed_chunk_on_retry() {
        use std::sync::atomic::AtomicBool;

        let payload: Vec<u8> = (0..4_096usize).map(|i| ((i * 7) % 251) as u8).collect();
        let payload_arc = Arc::new(payload.clone());
        let fail_requests = Arc::new(AtomicBool::new(true));
        let hits = Arc::new(AtomicUsize::new(0));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let fail_srv = Arc::clone(&fail_requests);
        let hits_srv = Arc::clone(&hits);
        let data_srv = Arc::clone(&payload_arc);

        let _server = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let fail = Arc::clone(&fail_srv);
                let hits = Arc::clone(&hits_srv);
                let data = Arc::clone(&data_srv);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    hits.fetch_add(1, Ordering::SeqCst);
                    if fail.load(Ordering::SeqCst) {
                        let _ = sock
                            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    } else {
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-4095/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            data.len(),
                            data.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(&data).await;
                    }
                });
            }
        });

        let handle = Handle::current();
        let url = format!("http://127.0.0.1:{}/recover.bin", addr.port());
        let fail_flip = Arc::clone(&fail_requests);
        let got = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new(
                Client::new(),
                handle,
                url,
                Vec::new(),
                Some(4_096),
                4_096,
                None,
            )
            .expect("create source");

            // First read exhausts internal transient retries and lands in a failed slot.
            let mut first_buf = [0u8; 64];
            let first_err = src.read(&mut first_buf).expect_err("expected initial 502 error");
            assert_eq!(first_err.kind(), io::ErrorKind::Other);

            // Network comes back; subsequent read (e.g. play -> retry_after_error -> seek)
            // must evict the failed transient slot and fetch fresh bytes cleanly.
            fail_flip.store(false, Ordering::SeqCst);
            src.seek(SeekFrom::Start(0)).expect("seek back to 0");
            let mut recovered = Vec::new();
            src.read_to_end(&mut recovered)
                .expect("retry read must succeed after evicting failed slot");
            recovered
        })
        .await
        .expect("join blocking");

        assert_eq!(got, payload);
        assert!(
            hits.load(Ordering::SeqCst) >= MAX_TRANSIENT_RETRIES + 2,
            "expected initial failed attempts plus fresh retry fetch"
        );
    }

    #[tokio::test]
    async fn keeps_403_and_cancelled_chunk_errors_sticky_without_refetch() {
        let hits = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits_srv = Arc::clone(&hits);

        let _server = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let hits = Arc::clone(&hits_srv);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    hits.fetch_add(1, Ordering::SeqCst);
                    let _ = sock
                        .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                });
            }
        });

        let handle = Handle::current();
        let url = format!("http://127.0.0.1:{}/refused.bin", addr.port());
        let (err1_kind, err2_kind) = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new(
                Client::new(),
                handle,
                url,
                Vec::new(),
                Some(4_096),
                4_096,
                None,
            )
            .expect("create 403 source");
            let mut buf = [0u8; 32];
            let e1 = src.read(&mut buf).expect_err("first 403").kind();
            let e2 = src.read(&mut buf).expect_err("second 403 must stay sticky").kind();
            (e1, e2)
        })
        .await
        .expect("join 403");

        assert_eq!(err1_kind, io::ErrorKind::PermissionDenied);
        assert_eq!(err2_kind, io::ErrorKind::PermissionDenied);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "403 refusal must stay sticky and never spam refetch"
        );

        // Cancelled token must also never refetch on subsequent reads.
        let cancel = StreamCancelToken::new();
        cancel.cancel();
        let handle2 = Handle::current();
        let url2 = format!("http://127.0.0.1:{}/cancelled.bin", addr.port());
        let cancel_kind = tokio::task::spawn_blocking(move || {
            let mut src = ChunkedRangeSource::new_with_cancel(
                Client::new(),
                handle2,
                url2,
                Vec::new(),
                Some(4_096),
                4_096,
                None,
                cancel,
            )
            .expect("create cancelled source");
            let mut buf = [0u8; 16];
            src.read(&mut buf).expect_err("cancelled read").kind()
        })
        .await
        .expect("join cancel");

        assert_eq!(cancel_kind, io::ErrorKind::ConnectionAborted);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "cancelled source must not issue HTTP calls");
    }
}
