//! Pure-Rust SMB2/3 music library client and seekable chunk-cached byte-range reader.
//!
//! Powered by the pure-Rust `#![forbid(unsafe_code)]` `smb` crate. Credentials are
//! never embedded in `smb://host/share/path` URIs; they are supplied at connection
//! time from Linux Secret Service (`oo7`) / caller config.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use bitchord_model::Song;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use smb::{
    Client, ClientConfig, ConnectionConfig, DirAccessMask, Directory, File, FileAccessMask,
    FileCreateArgs, FileDirectoryInformation, GetLen, ReadAt, Resource, UncPath,
};
use thiserror::Error;
use tokio::runtime::Handle;

use crate::remote_song::{
    MAX_COVER_BYTES, build_remote_song, is_audio_file, is_image_file, pick_artwork,
};

/// Browse identifier for SMB library rows (matches upstream `SmbConfig.BROWSE_ID`).
pub const SMB_BROWSE_ID: &str = "local:smb";
/// Default SMB2/3 TCP port.
pub const SMB_DEFAULT_PORT: u16 = 445;
/// Default recursion limit for SMB directory walks.
pub const SMB_DEFAULT_MAX_FILES: usize = 10_000;
/// Read chunk size for seekable SMB audio streaming (128 KiB).
pub const SMB_CHUNK_BYTES: usize = 128 * 1024;
/// Maximum in-memory cached SMB stream chunks per active reader (16 MiB).
pub const SMB_MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum SmbError {
    #[error("SMB is not configured: {0}")]
    InvalidConfig(String),
    #[error("SMB protocol/transport error: {0}")]
    Protocol(String),
    #[error("remote SMB file exceeds cover limit of {0} bytes")]
    PayloadTooLarge(usize),
    #[error("path is not a file or does not belong to configured share: {0}")]
    InvalidPath(String),
}

impl From<smb::Error> for SmbError {
    fn from(value: smb::Error) -> Self {
        Self::Protocol(value.to_string())
    }
}

/// Connection configuration and pure URI/path helpers for an SMB2/3 share.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmbConfig {
    pub host: String,
    pub share: String,
    #[serde(default)]
    pub base_path: String,
    #[serde(default)]
    pub username: String,
    #[serde(default, skip_serializing)]
    pub password: String,
}

impl SmbConfig {
    pub fn new(
        host: impl Into<String>,
        share: impl Into<String>,
        base_path: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            share: share.into(),
            base_path: base_path.into(),
            username: username.into(),
            password: password.into(),
        }
    }

    /// Parses `host`, `host:port`, `smb://host:port/...`, `\\host\share`, or `[::1]:445`.
    pub fn split_host_port(raw: &str) -> (String, u16) {
        let stripped = raw.trim();
        let stripped = if stripped.len() >= 6 && stripped[..6].eq_ignore_ascii_case("smb://") {
            &stripped[6..]
        } else if let Some(rest) = stripped.strip_prefix(r"\\") {
            rest
        } else {
            stripped
        };
        let authority = stripped.split(['/', '\\']).next().unwrap_or("").trim();

        if let Some(rest) = authority.strip_prefix('[') {
            let (host, after) = rest.split_once(']').unwrap_or((rest, ""));
            let port = after
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|&p| p > 0)
                .unwrap_or(SMB_DEFAULT_PORT);
            return (host.to_ascii_lowercase(), port);
        }

        if let Some((host, port_str)) = authority.rsplit_once(':')
            && let Ok(port) = port_str.parse::<u16>()
            && port > 0
            && !host.contains(':')
        {
            return (host.trim().to_ascii_lowercase(), port);
        }

        (authority.trim().to_ascii_lowercase(), SMB_DEFAULT_PORT)
    }

    pub fn normalize_host(raw: &str) -> String {
        Self::split_host_port(raw).0
    }

    pub fn normalized_share(&self) -> String {
        self.share.trim().trim_matches(['/', '\\']).to_owned()
    }

    pub fn normalized_base_path(&self) -> String {
        self.base_path
            .trim()
            .replace('\\', "/")
            .trim_matches('/')
            .to_owned()
    }

    pub fn is_configured_host_share(host: &str, share: &str) -> bool {
        !Self::normalize_host(host).is_empty() && !share.trim().trim_matches(['/', '\\']).is_empty()
    }

    pub fn is_configured(&self) -> bool {
        Self::is_configured_host_share(&self.host, &self.share)
    }

    /// Stable track id: `smb:<host>/<share>/<path>`.
    pub fn id_for(host: &str, share: &str, path: &str) -> String {
        let h = Self::normalize_host(host);
        let s = share.trim().trim_matches(['/', '\\']);
        let p = path.replace('\\', "/");
        let p = p.trim_matches('/');
        format!("smb:{h}/{s}/{p}").trim_end_matches('/').to_owned()
    }

    /// Display stream URI without credentials: `smb://<host>/<share>/<path>`.
    pub fn stream_url(host: &str, share: &str, path: &str) -> String {
        let h = Self::normalize_host(host);
        let s = share.trim().trim_matches(['/', '\\']);
        let p = path.replace('\\', "/");
        let p = p.trim_matches('/');
        format!("smb://{h}/{s}/{p}")
    }

    pub fn is_smb_id(video_id: &str) -> bool {
        video_id.starts_with("smb:")
    }

    pub fn is_smb_url(url: &str) -> bool {
        url.len() >= 6 && url[..6].eq_ignore_ascii_case("smb://")
    }

    /// Converts `smb://host/share/a/b.flac` back to the share-relative path `a/b.flac`,
    /// ensuring the share segment matches `expected_share` case-insensitively.
    pub fn relative_path(stream_url: &str, expected_share: &str) -> Option<String> {
        let rest = if stream_url.len() >= 6 && stream_url[..6].eq_ignore_ascii_case("smb://") {
            &stream_url[6..]
        } else {
            stream_url.strip_prefix("smb:")?
        };
        let clean = rest
            .split(['?', '#'])
            .next()
            .unwrap_or(rest)
            .replace('\\', "/");
        let segments: Vec<&str> = clean.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 3 {
            return None;
        }
        let target_share = expected_share.trim().trim_matches(['/', '\\']);
        if !segments[1].eq_ignore_ascii_case(target_share) {
            return None;
        }
        let rel = segments[2..].join("/");
        (!rel.is_empty()).then_some(rel)
    }

    /// Converts a display path (`Artist/Album/01.flac`) into SMB wire form (`Artist\Album\01.flac`).
    pub fn smb_wire_path(display_path: &str) -> String {
        display_path
            .replace('/', "\\")
            .trim_matches('\\')
            .to_owned()
    }

    /// Builds a [`Song`] row for an SMB file path relative to `share`.
    pub fn song_for(
        host: &str,
        share: &str,
        path: &str,
        album_name: Option<String>,
        cover_url: Option<String>,
    ) -> Song {
        let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        build_remote_song(
            Self::id_for(host, share, path),
            Self::stream_url(host, share, path),
            file_name,
            album_name,
            cover_url,
            "SMB",
            SMB_BROWSE_ID,
        )
    }
}

/// One entry returned from an SMB share walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmbEntry {
    /// Forward-slash display path relative to the share root.
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
}

/// Combined audio + adjacent cover image discovery result from one SMB traversal pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmbListing {
    pub audio: Vec<SmbEntry>,
    pub images: Vec<SmbEntry>,
}

/// Pure-Rust SMB2/3 client wrapper.
#[derive(Debug, Default, Clone)]
pub struct SmbClient;

impl SmbClient {
    pub fn new() -> Self {
        Self
    }

    async fn connect_share(&self, cfg: &SmbConfig) -> Result<(Client, UncPath), SmbError> {
        if !cfg.is_configured() {
            return Err(SmbError::InvalidConfig(
                "Enter an SMB server host and share name".to_owned(),
            ));
        }
        let (host, port) = SmbConfig::split_host_port(&cfg.host);
        let share = cfg.normalized_share();
        let is_guest = cfg.username.trim().is_empty() && cfg.password.is_empty();

        let client_cfg = ClientConfig {
            dfs: true,
            connection: ConnectionConfig {
                port: Some(port),
                timeout: Some(Duration::from_secs(20)),
                allow_unsigned_guest_access: is_guest,
                disable_notifications: true,
                ..ConnectionConfig::default()
            },
            ..ClientConfig::default()
        };

        let client = Client::new(client_cfg);
        let share_unc = UncPath::new(&host)?.with_share(&share)?;
        let username = if is_guest {
            "guest"
        } else {
            cfg.username.trim()
        };
        client
            .share_connect(&share_unc, username, cfg.password.clone())
            .await?;
        Ok((client, share_unc))
    }

    /// Tests connectivity, authentication, and base folder readability on the SMB share.
    pub async fn test_connection(&self, cfg: &SmbConfig) -> Result<(), SmbError> {
        let (client, share_unc) = self.connect_share(cfg).await?;
        let base = cfg.normalized_base_path();
        let res = self.list_dir(&client, &share_unc, &base).await;
        let _ = client.close().await;
        res.map(|_| ())
    }

    /// Recursively walks the SMB share starting at `cfg.base_path`, returning audio & image files.
    pub async fn list_library(
        &self,
        cfg: &SmbConfig,
        max_files: usize,
    ) -> Result<SmbListing, SmbError> {
        let (client, share_unc) = self.connect_share(cfg).await?;
        let cap = max_files.max(1);
        let mut audio = Vec::new();
        let mut images = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back(cfg.normalized_base_path());

        let result: Result<(), SmbError> = async {
            while let Some(dir) = queue.pop_front() {
                if audio.len() >= cap {
                    break;
                }
                if !visited.insert(dir.to_ascii_lowercase()) {
                    continue;
                }
                for child in self.list_dir(&client, &share_unc, &dir).await? {
                    let child_path = if dir.is_empty() {
                        child.path
                    } else {
                        format!("{dir}/{}", child.path)
                    };
                    if child.is_directory {
                        if !visited.contains(&child_path.to_ascii_lowercase()) {
                            queue.push_back(child_path);
                        }
                    } else {
                        let file_name = child_path.rsplit('/').next().unwrap_or(&child_path);
                        if is_audio_file(file_name) {
                            if audio.len() < cap {
                                audio.push(SmbEntry {
                                    path: child_path,
                                    is_directory: false,
                                    size: child.size,
                                });
                            }
                        } else if is_image_file(file_name) {
                            images.push(SmbEntry {
                                path: child_path,
                                is_directory: false,
                                size: child.size,
                            });
                        }
                    }
                }
            }
            Ok(())
        }
        .await;

        let _ = client.close().await;
        result?;
        Ok(SmbListing { audio, images })
    }

    /// Recursively lists the SMB share and pairs adjacent cover art into [`Song`] rows.
    pub async fn list_songs(
        &self,
        cfg: &SmbConfig,
        max_files: usize,
    ) -> Result<Vec<Song>, SmbError> {
        let listing = self.list_library(cfg, max_files).await?;
        Ok(songs_from_smb_listing(&cfg.host, &cfg.share, &listing))
    }

    /// Reads an entire small SMB file (e.g. folder cover art), capped at `max_bytes`.
    pub async fn read_file_capped(
        &self,
        cfg: &SmbConfig,
        stream_url_or_rel_path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, SmbError> {
        let limit = max_bytes.min(MAX_COVER_BYTES);
        let rel_path = resolve_smb_rel_path(cfg, stream_url_or_rel_path)?;
        let (client, share_unc) = self.connect_share(cfg).await?;

        let read_result: Result<Vec<u8>, SmbError> = async {
            let file_unc = share_unc.with_path(&SmbConfig::smb_wire_path(&rel_path));
            let resource = client
                .create_file(
                    &file_unc,
                    &FileCreateArgs::make_open_existing(
                        FileAccessMask::new().with_generic_read(true),
                    ),
                )
                .await?;
            let Resource::File(file) = resource else {
                return Err(SmbError::InvalidPath(rel_path));
            };
            let total_len = file.get_len().await.unwrap_or(0);
            if total_len > limit as u64 {
                let _ = file.close().await;
                return Err(SmbError::PayloadTooLarge(limit));
            }
            let mut out = Vec::with_capacity((total_len as usize).min(64 * 1024));
            let mut buf = vec![0u8; 64 * 1024];
            let mut offset = 0u64;
            while total_len == 0 || offset < total_len {
                let to_read = if total_len > 0 {
                    buf.len().min((total_len - offset) as usize)
                } else {
                    buf.len()
                };
                if to_read == 0 {
                    break;
                }
                let n = file.read_at(&mut buf[..to_read], offset).await?;
                if n == 0 {
                    break;
                }
                offset = offset.saturating_add(n as u64);
                if out.len().saturating_add(n) > limit {
                    let _ = file.close().await;
                    return Err(SmbError::PayloadTooLarge(limit));
                }
                out.extend_from_slice(&buf[..n]);
            }
            let _ = file.close().await;
            Ok(out)
        }
        .await;

        let _ = client.close().await;
        read_result
    }

    /// Opens a seekable, chunk-cached [`SmbRangeReader`] for streaming audio playback.
    pub async fn open_range_reader(
        &self,
        cfg: &SmbConfig,
        stream_url_or_rel_path: &str,
        rt: Handle,
    ) -> Result<SmbRangeReader, SmbError> {
        let rel_path = resolve_smb_rel_path(cfg, stream_url_or_rel_path)?;
        let (client, share_unc) = self.connect_share(cfg).await?;
        let file_unc = share_unc.with_path(&SmbConfig::smb_wire_path(&rel_path));
        let resource = client
            .create_file(
                &file_unc,
                &FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true)),
            )
            .await?;
        let Resource::File(file) = resource else {
            let _ = client.close().await;
            return Err(SmbError::InvalidPath(rel_path));
        };
        let len = file.get_len().await?;
        let backend = LiveSmbRangeBackend {
            _client: Arc::new(client),
            file: Arc::new(file),
            rt,
            len,
        };
        Ok(SmbRangeReader {
            core: ChunkedReaderCore::new(backend, len, SMB_CHUNK_BYTES, SMB_MAX_CACHE_BYTES),
        })
    }

    async fn list_dir(
        &self,
        client: &Client,
        share_unc: &UncPath,
        display_dir: &str,
    ) -> Result<Vec<SmbEntry>, SmbError> {
        let wire = SmbConfig::smb_wire_path(display_dir);
        let dir_unc = if wire.is_empty() {
            share_unc.clone()
        } else {
            share_unc.clone().with_path(&wire)
        };
        let resource = client
            .create_file(
                &dir_unc,
                &FileCreateArgs::make_open_existing(
                    DirAccessMask::new().with_list_directory(true).into(),
                ),
            )
            .await?;
        let Resource::Directory(dir) = resource else {
            return Err(SmbError::InvalidPath(display_dir.to_owned()));
        };
        let dir = Arc::new(dir);
        let mut entries = Vec::new();
        {
            let mut stream = Directory::query::<FileDirectoryInformation>(&dir, "*").await?;
            while let Some(item) = stream.next().await {
                let info = item?;
                let name = info.file_name.to_string();
                if name == "." || name == ".." || name.is_empty() {
                    continue;
                }
                entries.push(SmbEntry {
                    path: name,
                    is_directory: info.file_attributes.directory(),
                    size: info.end_of_file,
                });
            }
        }
        let _ = dir.close().await;
        Ok(entries)
    }
}

fn resolve_smb_rel_path(cfg: &SmbConfig, stream_url_or_rel_path: &str) -> Result<String, SmbError> {
    if SmbConfig::is_smb_url(stream_url_or_rel_path) || SmbConfig::is_smb_id(stream_url_or_rel_path)
    {
        SmbConfig::relative_path(stream_url_or_rel_path, &cfg.share)
            .ok_or_else(|| SmbError::InvalidPath(stream_url_or_rel_path.to_owned()))
    } else {
        let trimmed = stream_url_or_rel_path
            .replace('\\', "/")
            .trim_matches('/')
            .to_owned();
        if trimmed.is_empty() {
            Err(SmbError::InvalidPath(stream_url_or_rel_path.to_owned()))
        } else {
            Ok(trimmed)
        }
    }
}

/// Converts an [`SmbListing`] into [`Song`] rows paired with sibling folder cover images.
pub fn songs_from_smb_listing(host: &str, share: &str, listing: &SmbListing) -> Vec<Song> {
    let mut art_by_dir: HashMap<String, Vec<String>> = HashMap::new();
    for img in &listing.images {
        art_by_dir
            .entry(smb_dir_key(&img.path))
            .or_default()
            .push(SmbConfig::stream_url(host, share, &img.path));
    }
    listing
        .audio
        .iter()
        .map(|entry| {
            let cover = art_by_dir
                .get(&smb_dir_key(&entry.path))
                .and_then(|siblings| pick_artwork(siblings));
            let album = entry
                .path
                .rsplit_once('/')
                .and_then(|(parent, _)| parent.rsplit('/').next())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned);
            SmbConfig::song_for(host, share, &entry.path, album, cover)
        })
        .collect()
}

fn smb_dir_key(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    trimmed
        .rsplit_once('/')
        .map(|(dir, _)| dir.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Synchronous random-access byte-range backend powering [`ChunkedReaderCore`].
pub trait RangeReadBackend: Send + Sync {
    fn read_range(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

/// Bounded-memory LRU chunked [`Read`] + [`Seek`] core reusable for SMB and deterministic tests.
pub struct ChunkedReaderCore<B: RangeReadBackend> {
    backend: B,
    len: u64,
    chunk_size: usize,
    max_cached_chunks: usize,
    pos: u64,
    chunks: BTreeMap<u64, Vec<u8>>,
    lru: VecDeque<u64>,
}

impl<B: RangeReadBackend> ChunkedReaderCore<B> {
    pub fn new(backend: B, len: u64, chunk_size: usize, max_cache_bytes: usize) -> Self {
        let chunk_size = chunk_size.max(4096);
        let max_cached_chunks = (max_cache_bytes / chunk_size).max(2);
        Self {
            backend,
            len,
            chunk_size,
            max_cached_chunks,
            pos: 0,
            chunks: BTreeMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn byte_len(&self) -> u64 {
        self.len
    }

    fn ensure_chunk(&mut self, chunk_idx: u64) -> io::Result<&[u8]> {
        if !self.chunks.contains_key(&chunk_idx) {
            let offset = chunk_idx.saturating_mul(self.chunk_size as u64);
            if offset >= self.len {
                return Ok(&[]);
            }
            let target = ((self.len - offset) as usize).min(self.chunk_size);
            let mut data = vec![0u8; target];
            let mut filled = 0usize;
            while filled < target {
                let n = self
                    .backend
                    .read_range(offset + filled as u64, &mut data[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            data.truncate(filled);
            self.chunks.insert(chunk_idx, data);
            self.lru.push_back(chunk_idx);
            while self.chunks.len() > self.max_cached_chunks {
                if let Some(evict) = self.lru.pop_front() {
                    self.chunks.remove(&evict);
                }
            }
        }
        Ok(self
            .chunks
            .get(&chunk_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]))
    }
}

impl<B: RangeReadBackend> Read for ChunkedReaderCore<B> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let chunk_idx = self.pos / self.chunk_size as u64;
        let in_chunk = (self.pos % self.chunk_size as u64) as usize;
        let chunk = self.ensure_chunk(chunk_idx)?;
        if in_chunk >= chunk.len() {
            return Ok(0);
        }
        let n = buf.len().min(chunk.len() - in_chunk);
        buf[..n].copy_from_slice(&chunk[in_chunk..in_chunk + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<B: RangeReadBackend> Seek for ChunkedReaderCore<B> {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let next = match style {
            SeekFrom::Start(pos) => pos as i128,
            SeekFrom::End(delta) => self.len as i128 + delta as i128,
            SeekFrom::Current(delta) => self.pos as i128 + delta as i128,
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before byte 0",
            ));
        }
        self.pos = (next as u64).min(self.len);
        Ok(self.pos)
    }
}

struct LiveSmbRangeBackend {
    _client: Arc<Client>,
    file: Arc<File>,
    rt: Handle,
    len: u64,
}

impl RangeReadBackend for LiveSmbRangeBackend {
    fn read_range(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let file = Arc::clone(&self.file);
        self.rt
            .block_on(async move { file.read_at(buf, offset).await })
            .map_err(|e| io::Error::other(format!("SMB read_at failed: {e}")))
    }
}

/// Seekable, chunk-cached SMB2/3 audio stream reader implementing `Read + Seek + Send + Sync`.
pub struct SmbRangeReader {
    core: ChunkedReaderCore<LiveSmbRangeBackend>,
}

impl SmbRangeReader {
    pub fn byte_len(&self) -> u64 {
        self.core.byte_len()
    }
}

impl Read for SmbRangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.core.read(buf)
    }
}

impl Seek for SmbRangeReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.core.seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn splits_ipv4_hostname_unc_and_ipv6_ports() {
        assert_eq!(
            SmbConfig::split_host_port("nas.local"),
            ("nas.local".to_owned(), 445)
        );
        assert_eq!(
            SmbConfig::split_host_port("smb://192.168.1.50:1445/Music"),
            ("192.168.1.50".to_owned(), 1445)
        );
        assert_eq!(
            SmbConfig::split_host_port(r"\\FILESERVER\Audio"),
            ("fileserver".to_owned(), 445)
        );
        assert_eq!(
            SmbConfig::split_host_port("[fe80::1]:4445"),
            ("fe80::1".to_owned(), 4445)
        );
    }

    #[test]
    fn parses_relative_path_and_rejects_mismatched_share() {
        assert_eq!(
            SmbConfig::relative_path("smb://nas.local/Music/Jazz/Miles - So What.flac", "music"),
            Some("Jazz/Miles - So What.flac".to_owned())
        );
        assert_eq!(
            SmbConfig::relative_path("smb://nas.local/OtherShare/a.flac", "Music"),
            None
        );
        assert_eq!(
            SmbConfig::smb_wire_path("Jazz/Kind of Blue/01.flac"),
            r"Jazz\Kind of Blue\01.flac"
        );
    }

    #[test]
    fn pairs_folder_covers_and_builds_smb_songs() {
        let listing = SmbListing {
            audio: vec![SmbEntry {
                path: "Boards of Canada/Geogaddi/Boards of Canada - Music Is Math.flac".to_owned(),
                is_directory: false,
                size: 35_000_000,
            }],
            images: vec![
                SmbEntry {
                    path: "Boards of Canada/Geogaddi/scan_01.jpg".to_owned(),
                    is_directory: false,
                    size: 100_000,
                },
                SmbEntry {
                    path: "Boards of Canada/Geogaddi/folder.jpg".to_owned(),
                    is_directory: false,
                    size: 120_000,
                },
            ],
        };
        let songs = songs_from_smb_listing("nas.local:445", "Audio", &listing);
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].artist, "Boards of Canada");
        assert_eq!(songs[0].title, "Music Is Math");
        assert_eq!(songs[0].album_name.as_deref(), Some("Geogaddi"));
        assert_eq!(
            songs[0].thumbnail_url.as_deref(),
            Some("smb://nas.local/Audio/Boards of Canada/Geogaddi/folder.jpg")
        );
        assert!(SmbConfig::is_smb_id(&songs[0].video_id));
    }

    struct MemBackend {
        data: Vec<u8>,
        reads: Arc<AtomicUsize>,
    }

    impl RangeReadBackend for MemBackend {
        fn read_range(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let start = offset as usize;
            if start >= self.data.len() {
                return Ok(0);
            }
            let n = buf.len().min(self.data.len() - start);
            buf[..n].copy_from_slice(&self.data[start..start + n]);
            Ok(n)
        }
    }

    #[test]
    fn chunked_reader_caches_seeks_and_evicts_lru() {
        let payload: Vec<u8> = (0..20_000usize).map(|i| (i % 251) as u8).collect();
        let reads = Arc::new(AtomicUsize::new(0));
        let mut reader = ChunkedReaderCore::new(
            MemBackend {
                data: payload.clone(),
                reads: Arc::clone(&reads),
            },
            payload.len() as u64,
            4096,
            8192,
        );

        let mut buf = vec![0u8; 5000];
        let mut got = Vec::new();
        while got.len() < buf.len() {
            let n = reader.read(&mut buf[got.len()..]).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[got.len()..got.len() + n]);
        }
        assert_eq!(got, payload[..5000]);
        let reads_after_first = reads.load(Ordering::Relaxed);
        assert_eq!(reads_after_first, 2);

        // Seeking back into cached chunk 0 must NOT hit the backend again.
        reader.seek(SeekFrom::Start(100)).unwrap();
        let mut small = [0u8; 64];
        reader.read_exact(&mut small).unwrap();
        assert_eq!(&small[..], &payload[100..164]);
        assert_eq!(reads.load(Ordering::Relaxed), reads_after_first);

        // Seeking near EOF and reading the tail works accurately.
        reader.seek(SeekFrom::End(-32)).unwrap();
        let mut tail = [0u8; 32];
        reader.read_exact(&mut tail).unwrap();
        assert_eq!(&tail[..], &payload[payload.len() - 32..]);
    }
}
