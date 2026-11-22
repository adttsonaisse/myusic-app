//! Bounded HTTP `Range` [`MediaSource`] defeating `googlevideo.com`'s ~15.5 kB/s
//! unbounded stream throttling (`ChunkedDataSource.kt`).
//!
//! Fetches bounded `1 MiB` ranges (capped by `PlayerClient::range_bytes_for(url)`
//! and `clen`), verifies `206 Partial Content`, caches recent chunks in memory for
//! instant backward seeks, retries truncated ranges up to `MAX_EMPTY_RANGES = 3`,
//! and reports HTTP `403`/`404`/`410` refusals back to `StreamResolver`.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use bitchord_innertube::{PlayerClient, ResolvedStream};
use reqwest::Client;
use symphonia::core::io::MediaSource;
use tokio::runtime::Handle;

/// Default bounded range request size (1 MiB, matching `linux-audio-engine.md` §4).
pub const DEFAULT_CHUNK_BYTES: u64 = 1_048_576;
/// Maximum consecutive empty/truncated range attempts before signaling EOF.
pub const MAX_EMPTY_RANGES: usize = 3;
/// Maximum in-memory cached chunk bytes per active stream (32 MiB).
pub const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Refusal callback invoked when an HTTP range fetch receives 403/404/410.
pub type RefusalCallback = Arc<dyn Fn(&str, u16) + Send + Sync>;

/// Seekable HTTP range [`MediaSource`] for `symphonia` on the decode thread.
pub struct ChunkedRangeSource {
    http: Client,
    rt: Handle,
    url: String,
    headers: Vec<(String, String)>,
    total_len: Option<u64>,
    chunk_bytes: u64,
    pos: u64,
    cache: BTreeMap<u64, Vec<u8>>,
    cache_order: VecDeque<u64>,
    cached_bytes: usize,
    on_refused: Option<RefusalCallback>,
}

impl ChunkedRangeSource {
    /// Builds a `ChunkedRangeSource` from a [`ResolvedStream`] and Tokio runtime handle.
    pub fn from_resolved(
        http: Client,
        rt: Handle,
        stream: &ResolvedStream,
        on_refused: Option<RefusalCallback>,
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

        Self::new(
            http,
            rt,
            stream.url.clone(),
            headers,
            total_len,
            chunk_bytes,
            on_refused,
        )
    }

    /// Creates a `ChunkedRangeSource` with explicit parameters, probing total byte length
    /// via a single bounded range if `total_len` is `None`.
    pub fn new(
        http: Client,
        rt: Handle,
        url: String,
        headers: Vec<(String, String)>,
        mut total_len: Option<u64>,
        chunk_bytes: u64,
        on_refused: Option<RefusalCallback>,
    ) -> Result<Self, io::Error> {
        let chunk_bytes = chunk_bytes.max(4_096);
        let mut src = Self {
            http,
            rt,
            url,
            headers,
            total_len,
            chunk_bytes,
            pos: 0,
            cache: BTreeMap::new(),
            cache_order: VecDeque::new(),
            cached_bytes: 0,
            on_refused,
        };

        if total_len.is_none() {
            // Prime chunk 0 and learn total_len from Content-Range / Content-Length.
            let (data, discovered_total) = src.fetch_range(0, chunk_bytes.min(65_536))?;
            total_len = discovered_total.or(Some(data.len() as u64));
            src.total_len = total_len;
            if !data.is_empty()
                && data.len() as u64 == chunk_bytes.min(total_len.unwrap_or(u64::MAX))
            {
                src.insert_cache(0, data);
            }
        }
        Ok(src)
    }

    fn insert_cache(&mut self, chunk_idx: u64, data: Vec<u8>) {
        if let Some(old) = self.cache.remove(&chunk_idx) {
            self.cached_bytes = self.cached_bytes.saturating_sub(old.len());
            if let Some(pos) = self.cache_order.iter().position(|&k| k == chunk_idx) {
                self.cache_order.remove(pos);
            }
        }
        while self.cached_bytes.saturating_add(data.len()) > MAX_CACHE_BYTES {
            let Some(evict) = self.cache_order.pop_front() else {
                break;
            };
            if let Some(removed) = self.cache.remove(&evict) {
                self.cached_bytes = self.cached_bytes.saturating_sub(removed.len());
            }
        }
        self.cached_bytes = self.cached_bytes.saturating_add(data.len());
        self.cache.insert(chunk_idx, data);
        self.cache_order.push_back(chunk_idx);
    }

    fn fetch_chunk(&mut self, chunk_idx: u64) -> io::Result<()> {
        if self.cache.contains_key(&chunk_idx) {
            return Ok(());
        }
        let start = chunk_idx.saturating_mul(self.chunk_bytes);
        let max_len = match self.total_len {
            Some(total) if start >= total => return Ok(()),
            Some(total) => self.chunk_bytes.min(total - start),
            None => self.chunk_bytes,
        };

        let mut acc = Vec::with_capacity(max_len as usize);
        let mut cursor = start;
        let mut remaining = max_len;
        let mut empty_attempts = 0usize;

        while remaining > 0 {
            let (part, discovered_total) = self.fetch_range(cursor, remaining)?;
            if self.total_len.is_none()
                && let Some(total) = discovered_total
            {
                self.total_len = Some(total);
            }
            if part.is_empty() {
                empty_attempts += 1;
                if empty_attempts >= MAX_EMPTY_RANGES {
                    break;
                }
                continue;
            }
            empty_attempts = 0;
            let got = part.len() as u64;
            acc.extend_from_slice(&part);
            cursor = cursor.saturating_add(got);
            remaining = remaining.saturating_sub(got);
            if let Some(total) = self.total_len
                && cursor >= total
            {
                break;
            }
        }

        if !acc.is_empty() {
            self.insert_cache(chunk_idx, acc);
        }
        Ok(())
    }

    fn fetch_range(&self, start: u64, len: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
        let end_inclusive = start.saturating_add(len.saturating_sub(1));
        let range_header = format!("bytes={start}-{end_inclusive}");
        let http = self.http.clone();
        let url = self.url.clone();
        let headers = self.headers.clone();
        let on_refused = self.on_refused.clone();

        self.rt.block_on(async move {
            let mut req = http
                .get(&url)
                .timeout(REQUEST_TIMEOUT)
                .header(reqwest::header::RANGE, range_header);
            for (k, v) in &headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req
                .send()
                .await
                .map_err(|e| io::Error::other(format!("HTTP range error: {e}")))?;

            let status = resp.status().as_u16();
            if matches!(status, 403 | 404 | 410) {
                if let Some(cb) = &on_refused {
                    cb(&url, status);
                }
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("stream range refused with HTTP {status}"),
                ));
            }
            if status == 416 {
                return Ok((Vec::new(), None));
            }
            if status == 200 && start > 0 {
                // Server ignored Range and returned offset 0 — never splice into middle!
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "server answered 200 OK instead of 206 Partial Content for ranged seek",
                ));
            }
            if status != 206 && status != 200 {
                return Err(io::Error::other(format!("unexpected HTTP status {status}")));
            }

            let total = if status == 206 {
                resp.headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.rsplit_once('/'))
                    .and_then(|(_, tot)| tot.parse::<u64>().ok())
            } else {
                resp.content_length()
            };

            let bytes = resp
                .bytes()
                .await
                .map_err(|e| io::Error::other(format!("HTTP range body error: {e}")))?;
            Ok((bytes.to_vec(), total))
        })
    }
}

impl Read for ChunkedRangeSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(total) = self.total_len
            && self.pos >= total
        {
            return Ok(0);
        }

        let chunk_idx = self.pos / self.chunk_bytes;
        let offset_in_chunk = (self.pos % self.chunk_bytes) as usize;
        self.fetch_chunk(chunk_idx)?;

        let Some(chunk) = self.cache.get(&chunk_idx) else {
            return Ok(0);
        };
        if offset_in_chunk >= chunk.len() {
            return Ok(0);
        }
        let avail = &chunk[offset_in_chunk..];
        let to_copy = buf.len().min(avail.len());
        buf[..to_copy].copy_from_slice(&avail[..to_copy]);
        self.pos = self.pos.saturating_add(to_copy as u64);
        Ok(to_copy)
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
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let req_text = String::from_utf8_lossy(&buf[..n]);
                req_counter.fetch_add(1, Ordering::Relaxed);

                if req_text.contains("/forbidden") {
                    let _ = sock
                        .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    continue;
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
}
