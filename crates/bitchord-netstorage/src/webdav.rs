//! Pure-Rust WebDAV music library client (`PROPFIND` Depth 0/1, `MKCOL`, `HEAD`, `PUT`).
//!
//! Uses `Depth: 1` with manual BFS recursion rather than `Depth: infinity` (which
//! Nextcloud and many RFC-4918 servers reject). Streaming audio reuses
//! `bitchord-audio`'s `ChunkedRangeSource` with `Authorization: Basic ...`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bitchord_model::Song;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::remote_song::{
    MAX_COVER_BYTES, build_remote_song, is_audio_file, is_image_file, pick_artwork,
    split_artist_title,
};

/// Browse identifier used for WebDAV library tracks (matches upstream `WebDavConfig.BROWSE_ID`).
pub const WEBDAV_BROWSE_ID: &str = "local:webdav";
/// Default recursion cap to prevent pathological symlink/collection loops.
pub const DEFAULT_MAX_FILES: usize = 10_000;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <d:getcontenttype/>
    <d:getcontentlength/>
  </d:prop>
</d:propfind>"#;

#[derive(Debug, Error)]
pub enum WebDavError {
    #[error("WebDAV is not configured: {0}")]
    InvalidConfig(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("WebDAV server answered HTTP {status}: {context}")]
    ServerStatus { status: u16, context: String },
    #[error("remote payload exceeds limit of {0} bytes")]
    PayloadTooLarge(usize),
}

/// Connection configuration and helpers for a WebDAV music server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConfig {
    pub url: String,
    pub username: String,
    #[serde(default, skip_serializing)]
    pub password: String,
}

impl WebDavConfig {
    pub fn new(
        url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            username: username.into(),
            password: password.into(),
        }
    }

    /// Normalizes a server URL, prepending `https://` when no scheme is provided.
    pub fn normalize_url(raw: &str) -> String {
        let trimmed = raw.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return String::new();
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            trimmed.to_owned()
        } else {
            format!("https://{trimmed}")
        }
    }

    /// Extracts the lowercased host of a WebDAV URL if parseable.
    pub fn host_of(url: &str) -> Option<String> {
        let normalized = Self::normalize_url(url);
        let parsed = Url::parse(&normalized).ok()?;
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        parsed
            .host_str()
            .filter(|h| !h.is_empty())
            .map(|h| h.to_ascii_lowercase())
    }

    /// Returns `true` when `url` normalizes to a valid `http(s)` URL with a host.
    pub fn is_configured_url(url: &str) -> bool {
        Self::host_of(url).is_some()
    }

    pub fn is_configured(&self) -> bool {
        Self::is_configured_url(&self.url)
    }

    /// Encodes `Authorization: Basic ...` header value, or `None` for anonymous access.
    pub fn basic_auth_header(username: &str, password: &str) -> Option<String> {
        if username.trim().is_empty() && password.is_empty() {
            return None;
        }
        let raw = format!("{username}:{password}");
        Some(format!("Basic {}", BASE64.encode(raw.as_bytes())))
    }

    pub fn auth_header(&self) -> Option<String> {
        Self::basic_auth_header(&self.username, &self.password)
    }

    /// Returns `Authorization: Basic ...` only if `target_url`'s host matches this server's host.
    pub fn auth_header_for_url(&self, target_url: &str) -> Option<String> {
        let expected_host = Self::host_of(&self.url)?;
        let actual_host = Self::host_of(target_url)?;
        if expected_host == actual_host {
            self.auth_header()
        } else {
            None
        }
    }

    /// Stable track id (`webdav:<fileUrl>`) never colliding with YouTube, local, or SMB ids.
    pub fn id_for(file_url: &str) -> String {
        format!("webdav:{file_url}")
    }

    pub fn is_webdav_id(video_id: &str) -> bool {
        video_id.starts_with("webdav:")
    }

    pub fn file_url_of(video_id: &str) -> Option<&str> {
        let url = video_id.strip_prefix("webdav:")?;
        let lower = url.to_ascii_lowercase();
        (lower.starts_with("http://") || lower.starts_with("https://")).then_some(url)
    }

    /// Joins `root/<percent-encoded segment>` using `%20` for spaces (RFC 3986 path segment).
    pub fn join_url(root: &str, segment: &str) -> String {
        let encoded = utf8_percent_encode(segment, NON_ALPHANUMERIC).to_string();
        format!("{}/{}", root.trim_end_matches('/'), encoded)
    }

    /// Sanitizes an upload filename (`Artist - Title.ext`), stripping path separators & controls.
    pub fn upload_file_name(title: &str, artist: &str, extension: &str) -> String {
        let raw_base = if !artist.trim().is_empty() && artist.trim() != "Unknown Artist" {
            format!("{} - {}", artist.trim(), title.trim())
        } else {
            title.trim().to_owned()
        };
        let clean: String = raw_base
            .chars()
            .map(|c| {
                if c == '/' || c == '\\' || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let trimmed = clean.trim().trim_matches('.');
        let bounded: String = trimmed.chars().take(120).collect();
        let stem = if bounded.is_empty() {
            "track".to_owned()
        } else {
            bounded
        };
        let ext = extension.trim_start_matches('.').to_ascii_lowercase();
        let ext = if ext.is_empty() { "mp3" } else { ext.as_str() };
        format!("{stem}.{ext}")
    }

    /// Finds the first unused filename among `base.ext`, `base (1).ext`, `base (2).ext`, ...
    pub fn resolve_numbered_name(base: &str, extension: &str, taken: &HashSet<String>) -> String {
        let lowered: HashSet<String> = taken.iter().map(|s| s.to_ascii_lowercase()).collect();
        let mut candidate = format!("{base}.{extension}");
        let mut n = 1u32;
        while lowered.contains(&candidate.to_ascii_lowercase()) && n < 1000 {
            candidate = format!("{base} ({n}).{extension}");
            n += 1;
        }
        candidate
    }

    /// Constructs a [`Song`] row from a WebDAV file URL and optional album/cover metadata.
    pub fn song_for(file_url: &str, album_name: Option<String>, cover_url: Option<String>) -> Song {
        let decoded = decoded_last_segment(file_url);
        build_remote_song(
            Self::id_for(file_url),
            file_url.to_owned(),
            &decoded,
            album_name,
            cover_url,
            "WebDAV",
            WEBDAV_BROWSE_ID,
        )
    }
}

/// One resource row returned by a WebDAV `PROPFIND` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavEntry {
    pub url: String,
    pub display_name: String,
    pub is_collection: bool,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
}

/// Combined audio + adjacent cover image discovery result from one traversal pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavListing {
    pub audio: Vec<WebDavEntry>,
    pub images: Vec<WebDavEntry>,
}

/// Outcome of a `PUT` upload request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebDavPutResult {
    Uploaded,
    /// Server rejected a non-overwriting upload with HTTP 412 Precondition Failed.
    AlreadyExists,
}

/// Async HTTP WebDAV client built on `reqwest` + `quick-xml`.
#[derive(Clone)]
pub struct WebDavClient {
    http: Client,
}

impl Default for WebDavClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WebDavClient {
    pub fn new() -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(12))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { http }
    }

    pub fn with_http(http: Client) -> Self {
        Self { http }
    }

    /// Probes the server root with `PROPFIND` (`Depth: 0`) to verify reachability and credentials.
    pub async fn test_connection(&self, cfg: &WebDavConfig) -> Result<(), WebDavError> {
        let target = WebDavConfig::normalize_url(&cfg.url);
        if !WebDavConfig::is_configured_url(&target) {
            return Err(WebDavError::InvalidConfig(
                "Enter a valid http:// or https:// server URL".to_owned(),
            ));
        }
        let resp = self.send_propfind(&target, "0", cfg).await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::MULTI_STATUS {
            Ok(())
        } else {
            Err(WebDavError::ServerStatus {
                status: status.as_u16(),
                context: "PROPFIND connection test failed".to_owned(),
            })
        }
    }

    /// Lists direct children of `dir_url` via `PROPFIND` (`Depth: 1`).
    pub async fn propfind(
        &self,
        dir_url: &str,
        cfg: &WebDavConfig,
    ) -> Result<Vec<WebDavEntry>, WebDavError> {
        let resp = self.send_propfind(dir_url, "1", cfg).await?;
        let status = resp.status();
        if !status.is_success() && status != StatusCode::MULTI_STATUS {
            return Err(WebDavError::ServerStatus {
                status: status.as_u16(),
                context: format!("PROPFIND {dir_url} failed"),
            });
        }
        let body = resp.text().await?;
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(parse_multistatus(&body, dir_url))
    }

    /// Recursively discovers all audio files and sibling cover images via BFS (`Depth: 1`).
    pub async fn list_library(
        &self,
        cfg: &WebDavConfig,
        max_files: usize,
    ) -> Result<WebDavListing, WebDavError> {
        let root = WebDavConfig::normalize_url(&cfg.url);
        if !WebDavConfig::is_configured_url(&root) {
            return Err(WebDavError::InvalidConfig(
                "WebDAV URL is empty or invalid".to_owned(),
            ));
        }
        let cap = max_files.max(1);
        let mut found_audio: Vec<WebDavEntry> = Vec::new();
        let mut seen_audio: HashSet<String> = HashSet::new();
        let mut found_images: Vec<WebDavEntry> = Vec::new();
        let mut seen_images: HashSet<String> = HashSet::new();

        let mut dirs: VecDeque<String> = VecDeque::new();
        dirs.push_back(root);
        let mut visited: HashSet<String> = HashSet::new();

        while let Some(dir) = dirs.pop_front() {
            if found_audio.len() >= cap {
                break;
            }
            let dir_norm = dir.trim_end_matches('/').to_ascii_lowercase();
            if !visited.insert(dir_norm) {
                continue;
            }
            let entries = self.propfind(&dir, cfg).await?;
            for entry in entries {
                if entry.is_collection {
                    let next_norm = entry.url.trim_end_matches('/').to_ascii_lowercase();
                    if !visited.contains(&next_norm) {
                        dirs.push_back(entry.url);
                    }
                } else {
                    let probe_name = if entry.display_name.trim().is_empty() {
                        entry.url.as_str()
                    } else {
                        entry.display_name.as_str()
                    };
                    if is_audio_file(probe_name) {
                        if found_audio.len() < cap && seen_audio.insert(entry.url.clone()) {
                            found_audio.push(entry);
                        }
                    } else if is_image_file(probe_name) && seen_images.insert(entry.url.clone()) {
                        found_images.push(entry);
                    }
                }
            }
        }

        Ok(WebDavListing {
            audio: found_audio,
            images: found_images,
        })
    }

    /// Recursively lists the WebDAV library and converts entries into paired [`Song`] rows.
    pub async fn list_songs(
        &self,
        cfg: &WebDavConfig,
        max_files: usize,
    ) -> Result<Vec<Song>, WebDavError> {
        let listing = self.list_library(cfg, max_files).await?;
        Ok(songs_from_listing(&listing))
    }

    /// Checks whether `file_url` already exists via HTTP `HEAD`.
    pub async fn exists(&self, file_url: &str, cfg: &WebDavConfig) -> Result<bool, WebDavError> {
        let mut req = self.http.head(file_url);
        if let Some(auth) = cfg.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await?;
        match resp.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            other => Err(WebDavError::ServerStatus {
                status: other.as_u16(),
                context: "HEAD existence check failed".to_owned(),
            }),
        }
    }

    /// Creates a collection (`MKCOL`), accepting `2xx` or `405 Method Not Allowed` (already exists).
    pub async fn ensure_collection(
        &self,
        dir_url: &str,
        cfg: &WebDavConfig,
    ) -> Result<(), WebDavError> {
        let method = Method::from_bytes(b"MKCOL").expect("valid MKCOL method");
        let mut req = self.http.request(method, dir_url);
        if let Some(auth) = cfg.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED {
            Ok(())
        } else {
            Err(WebDavError::ServerStatus {
                status: status.as_u16(),
                context: format!("MKCOL {dir_url} failed"),
            })
        }
    }

    /// Uploads a file payload via `PUT`, enforcing atomic non-overwrite with `If-None-Match: *`.
    pub async fn put_bytes(
        &self,
        file_url: &str,
        bytes: Vec<u8>,
        mime_type: &str,
        cfg: &WebDavConfig,
        overwrite: bool,
    ) -> Result<WebDavPutResult, WebDavError> {
        let mut req = self
            .http
            .put(file_url)
            .header("Content-Type", mime_type)
            .body(bytes);
        if !overwrite {
            req = req.header("If-None-Match", "*");
        }
        if let Some(auth) = cfg.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await?;
        match resp.status() {
            s if s.is_success() => Ok(WebDavPutResult::Uploaded),
            StatusCode::PRECONDITION_FAILED => Ok(WebDavPutResult::AlreadyExists),
            other => Err(WebDavError::ServerStatus {
                status: other.as_u16(),
                context: format!("PUT {file_url} failed"),
            }),
        }
    }

    /// Downloads a small remote resource (e.g. album cover art) capped at `max_bytes`.
    pub async fn fetch_bytes_capped(
        &self,
        url: &str,
        cfg: &WebDavConfig,
        max_bytes: usize,
    ) -> Result<Vec<u8>, WebDavError> {
        let limit = max_bytes.min(MAX_COVER_BYTES);
        let mut req = self.http.get(url);
        if let Some(auth) = cfg.auth_header_for_url(url) {
            req = req.header("Authorization", auth);
        }
        let mut resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(WebDavError::ServerStatus {
                status: status.as_u16(),
                context: format!("GET {url} failed"),
            });
        }
        if let Some(len) = resp.content_length()
            && len > limit as u64
        {
            return Err(WebDavError::PayloadTooLarge(limit));
        }
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            if out.len().saturating_add(chunk.len()) > limit {
                return Err(WebDavError::PayloadTooLarge(limit));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    async fn send_propfind(
        &self,
        url: &str,
        depth: &str,
        cfg: &WebDavConfig,
    ) -> Result<reqwest::Response, WebDavError> {
        let method = Method::from_bytes(b"PROPFIND").expect("valid PROPFIND method");
        let mut req = self
            .http
            .request(method, url)
            .header("Depth", depth)
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(PROPFIND_BODY);
        if let Some(auth) = cfg.auth_header() {
            req = req.header("Authorization", auth);
        }
        Ok(req.send().await?)
    }
}

/// Converts a [`WebDavListing`] into [`Song`] rows with adjacent cover-art pairing.
pub fn songs_from_listing(listing: &WebDavListing) -> Vec<Song> {
    let mut art_by_dir: HashMap<String, Vec<String>> = HashMap::new();
    for img in &listing.images {
        art_by_dir
            .entry(dir_key(&img.url))
            .or_default()
            .push(img.url.clone());
    }

    listing
        .audio
        .iter()
        .map(|entry| {
            let cover = art_by_dir
                .get(&dir_key(&entry.url))
                .and_then(|siblings| pick_artwork(siblings));
            let album = parent_folder_name(&entry.url);
            let mut song = WebDavConfig::song_for(&entry.url, album, cover);
            // Prefer server's displayname over URL-decoded segment when present.
            if !entry.display_name.trim().is_empty() {
                let stem = entry
                    .display_name
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(&entry.display_name);
                let (artist, title) = split_artist_title(stem);
                if let Some(t) = title {
                    song.title = t;
                }
                if let Some(a) = artist {
                    song.artist = a;
                }
            }
            song
        })
        .collect()
}

/// Pure namespace-agnostic RFC 4918 `<multistatus>` XML parser.
///
/// Drops the echoed self-entry for `dir_url`.
pub fn parse_multistatus(xml: &str, dir_url: &str) -> Vec<WebDavEntry> {
    if xml.trim().is_empty() {
        return Vec::new();
    }

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut in_response = false;
    let mut in_resourcetype = false;
    let mut active_field: Option<&'static str> = None;

    let mut cur_href = String::new();
    let mut cur_display_name = String::new();
    let mut cur_is_collection = false;
    let mut cur_content_type = String::new();
    let mut cur_content_len = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = local_name_lower(e.name().as_ref());
                match local.as_str() {
                    "response" => {
                        in_response = true;
                        in_resourcetype = false;
                        active_field = None;
                        cur_href.clear();
                        cur_display_name.clear();
                        cur_is_collection = false;
                        cur_content_type.clear();
                        cur_content_len.clear();
                    }
                    "resourcetype" if in_response => {
                        in_resourcetype = true;
                    }
                    "collection" if in_response && in_resourcetype => {
                        cur_is_collection = true;
                    }
                    "href" if in_response => active_field = Some("href"),
                    "displayname" if in_response => active_field = Some("displayname"),
                    "getcontenttype" if in_response => active_field = Some("getcontenttype"),
                    "getcontentlength" if in_response => active_field = Some("getcontentlength"),
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = local_name_lower(e.name().as_ref());
                if in_response && in_resourcetype && local == "collection" {
                    cur_is_collection = true;
                }
            }
            Ok(Event::Text(ref e)) => {
                if let Some(field) = active_field {
                    let raw: &str = e.as_ref();
                    let text = quick_xml::escape::unescape(raw)
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| raw.to_owned());
                    match field {
                        "href" => cur_href.push_str(&text),
                        "displayname" => cur_display_name.push_str(&text),
                        "getcontenttype" => cur_content_type.push_str(&text),
                        "getcontentlength" => cur_content_len.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(ref e)) => {
                if let Some(field) = active_field {
                    let text: &str = e.as_ref();
                    match field {
                        "href" => cur_href.push_str(text),
                        "displayname" => cur_display_name.push_str(text),
                        "getcontenttype" => cur_content_type.push_str(text),
                        "getcontentlength" => cur_content_len.push_str(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name_lower(e.name().as_ref());
                match local.as_str() {
                    "response" if in_response => {
                        let href = cur_href.trim();
                        if !href.is_empty() {
                            let url = resolve_href(dir_url, href);
                            if !is_same_url(&url, dir_url) {
                                let display_name = if cur_display_name.trim().is_empty() {
                                    decoded_last_segment(&url)
                                } else {
                                    cur_display_name.trim().to_owned()
                                };
                                let content_type = cur_content_type.trim();
                                let content_type =
                                    (!content_type.is_empty()).then(|| content_type.to_owned());
                                let content_length = cur_content_len.trim().parse::<u64>().ok();
                                out.push(WebDavEntry {
                                    url,
                                    display_name,
                                    is_collection: cur_is_collection,
                                    content_type,
                                    content_length,
                                });
                            }
                        }
                        in_response = false;
                        in_resourcetype = false;
                        active_field = None;
                    }
                    "resourcetype" => in_resourcetype = false,
                    "href" | "displayname" | "getcontenttype" | "getcontentlength" => {
                        active_field = None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    out
}

/// Resolves a WebDAV `<href>` (absolute URL, root-relative path, or relative segment) against `base_url`.
pub fn resolve_href(base_url: &str, href: &str) -> String {
    let trimmed = href.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return trimmed.to_owned();
    }
    let base = WebDavConfig::normalize_url(base_url);
    let Some(scheme_end) = base.find("://") else {
        return trimmed.to_owned();
    };
    let origin = match base[scheme_end + 3..].find('/') {
        Some(rel) => &base[..scheme_end + 3 + rel],
        None => base.as_str(),
    };
    if trimmed.starts_with('/') {
        format!("{origin}{trimmed}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), trimmed)
    }
}

fn local_name_lower(name: &str) -> String {
    let local = name.rsplit_once(':').map(|(_, l)| l).unwrap_or(name);
    local.to_ascii_lowercase()
}

fn is_same_url(a: &str, b: &str) -> bool {
    a.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

fn decoded_last_segment(url: &str) -> String {
    let seg = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
    percent_decode_str(seg)
        .decode_utf8()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| seg.to_owned())
}

fn dir_key(file_url: &str) -> String {
    if let Ok(parsed) = Url::parse(file_url) {
        let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
        let path = parsed.path().trim_end_matches('/');
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        format!("{host}{parent}").to_ascii_lowercase()
    } else {
        file_url
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap_or(file_url)
            .to_ascii_lowercase()
    }
}

fn parent_folder_name(file_url: &str) -> Option<String> {
    let parsed = Url::parse(file_url).ok()?;
    let path = parsed.path().trim_end_matches('/');
    let parent_path = path.rsplit_once('/')?.0;
    let folder = parent_path.rsplit('/').next()?.trim();
    if folder.is_empty() {
        return None;
    }
    let decoded = percent_decode_str(folder)
        .decode_utf8()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| folder.to_owned());
    (!decoded.trim().is_empty()).then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn normalizes_urls_and_computes_basic_auth() {
        assert_eq!(
            WebDavConfig::normalize_url("cloud.example.com/remote.php/dav/files/me/"),
            "https://cloud.example.com/remote.php/dav/files/me"
        );
        assert!(WebDavConfig::is_configured_url("http://127.0.0.1:8080/dav"));
        assert!(!WebDavConfig::is_configured_url("not a url"));
        assert_eq!(
            WebDavConfig::basic_auth_header("alice", "secret").as_deref(),
            Some("Basic YWxpY2U6c2VjcmV0")
        );
        assert_eq!(WebDavConfig::basic_auth_header("", ""), None);
    }

    #[test]
    fn parses_nextcloud_and_apache_multistatus_xml() {
        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
          <d:response>
            <d:href>/dav/Music/</d:href>
            <d:propstat><d:prop>
              <d:resourcetype><d:collection/></d:resourcetype>
            </d:prop></d:propstat>
          </d:response>
          <d:response>
            <d:href>/dav/Music/Discovery/</d:href>
            <d:propstat><d:prop>
              <d:displayname>Discovery</d:displayname>
              <d:resourcetype><d:collection/></d:resourcetype>
            </d:prop></d:propstat>
          </d:response>
          <d:response>
            <d:href>/dav/Music/Daft%20Punk%20-%20One%20More%20Time.flac</d:href>
            <d:propstat><d:prop>
              <d:getcontenttype>audio/flac</d:getcontenttype>
              <d:getcontentlength>42000000</d:getcontentlength>
              <d:resourcetype/>
            </d:prop></d:propstat>
          </d:response>
        </d:multistatus>"#;

        let entries = parse_multistatus(xml, "https://nas.local/dav/Music");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_collection);
        assert_eq!(entries[0].url, "https://nas.local/dav/Music/Discovery/");
        assert!(!entries[1].is_collection);
        assert_eq!(entries[1].display_name, "Daft Punk - One More Time.flac");
        assert_eq!(entries[1].content_length, Some(42_000_000));
    }

    #[test]
    fn sanitizes_upload_names_and_numbers_collisions() {
        assert_eq!(
            WebDavConfig::upload_file_name("A/B\\Track", "Daft Punk", ".FLAC"),
            "Daft Punk - A_B_Track.flac"
        );
        let taken: HashSet<String> = ["song.flac".to_owned(), "Song (1).flac".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            WebDavConfig::resolve_numbered_name("Song", "flac", &taken),
            "Song (2).flac"
        );
    }

    #[tokio::test]
    async fn end_to_end_mock_webdav_server_propfind_auth_and_cover_pairing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let expected_auth = WebDavConfig::basic_auth_header("music", "pass123").unwrap();

        let server = tokio::spawn(async move {
            for _ in 0..8 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let mut parts = first_line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("/");

                let authorized = req
                    .lines()
                    .any(|l| l.eq_ignore_ascii_case(&format!("Authorization: {expected_auth}")));
                if !authorized {
                    let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
                    let _ = sock.write_all(resp.as_bytes()).await;
                    continue;
                }

                let (status, body) = match (method, path) {
                    ("PROPFIND", "/dav/Music") | ("PROPFIND", "/dav/Music/") => {
                        let depth_0 = req.lines().any(|l| l.eq_ignore_ascii_case("Depth: 0"));
                        if depth_0 {
                            (
                                "207 Multi-Status",
                                r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/Music/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response></d:multistatus>"#,
                            )
                        } else {
                            (
                                "207 Multi-Status",
                                r#"<d:multistatus xmlns:d="DAV:">
                                  <d:response><d:href>/dav/Music/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
                                  <d:response><d:href>/dav/Music/Random%20Access/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
                                </d:multistatus>"#,
                            )
                        }
                    }
                    ("PROPFIND", "/dav/Music/Random%20Access/") => (
                        "207 Multi-Status",
                        r#"<d:multistatus xmlns:d="DAV:">
                          <d:response><d:href>/dav/Music/Random%20Access/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
                          <d:response><d:href>/dav/Music/Random%20Access/cover.jpg</d:href><d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat></d:response>
                          <d:response><d:href>/dav/Music/Random%20Access/Daft%20Punk%20-%20Giorgio.flac</d:href><d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat></d:response>
                        </d:multistatus>"#,
                    ),
                    ("MKCOL", _) => ("201 Created", ""),
                    ("HEAD", "/dav/Music/existing.flac") => ("200 OK", ""),
                    ("PUT", "/dav/Music/existing.flac") => {
                        if req
                            .lines()
                            .any(|l| l.eq_ignore_ascii_case("If-None-Match: *"))
                        {
                            ("412 Precondition Failed", "")
                        } else {
                            ("204 No Content", "")
                        }
                    }
                    _ => ("404 Not Found", ""),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });

        let client = WebDavClient::new();
        let bad_cfg = WebDavConfig::new(format!("http://{addr}/dav/Music"), "music", "wrong");
        assert!(client.test_connection(&bad_cfg).await.is_err());

        let cfg = WebDavConfig::new(format!("http://{addr}/dav/Music"), "music", "pass123");
        client.test_connection(&cfg).await.expect("test ok");

        let songs = client
            .list_songs(&cfg, DEFAULT_MAX_FILES)
            .await
            .expect("list songs");
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].artist, "Daft Punk");
        assert_eq!(songs[0].title, "Giorgio");
        assert_eq!(songs[0].album_name.as_deref(), Some("Random Access"));
        assert_eq!(
            songs[0].thumbnail_url.as_deref(),
            Some(format!("http://{addr}/dav/Music/Random%20Access/cover.jpg").as_str())
        );
        assert!(WebDavConfig::is_webdav_id(&songs[0].video_id));

        client
            .ensure_collection(&format!("http://{addr}/dav/Music/New"), &cfg)
            .await
            .expect("mkcol");
        assert!(
            client
                .exists(&format!("http://{addr}/dav/Music/existing.flac"), &cfg)
                .await
                .expect("head")
        );
        let put = client
            .put_bytes(
                &format!("http://{addr}/dav/Music/existing.flac"),
                vec![1, 2, 3],
                "audio/flac",
                &cfg,
                false,
            )
            .await
            .expect("put");
        assert_eq!(put, WebDavPutResult::AlreadyExists);

        server.abort();
    }
}
