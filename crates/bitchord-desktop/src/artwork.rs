//! Bounded LRU artwork cache + self-healing disk cover-art cache (`gpui-linux.md` §4.3 & §5.4).
//!
//! Features:
//! - Two resolution tiers (`256px` list/card thumbnails & `512px` Now Playing hero covers)
//!   with separate cache keys to stay well inside shared-VRAM limits (`MAX_CACHED_COVERS = 128`).
//! - Anti-blank thumbnail pipeline (`notes.md` #6): explicit non-AVIF `Accept` header,
//!   Google/YouTube CDN URL normalization (`lh3.googleusercontent.com`, `yt3.ggpht.com`,
//!   `i.ytimg.com/vi_webp`), and multi-step `i.ytimg.com` fallback chain on HTTP or decode error.
//! - Embedded cover-art extraction for local audio files via `lofty` (+ folder sidecar fallback).
//! - Persistent SQLite v4 `artwork_index` LRU tracking & disk eviction via [`DbWorker`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bitchord_model::Song;
use bitchord_netstorage::{MAX_COVER_BYTES, SmbClient, SmbConfig, WebDavConfig};
use bitchord_storage::ArtworkIndexEntry;
use gpui::RenderImage;
use image::imageops::FilterType;
use lofty::config::ParseOptions;
use lofty::picture::PictureType;
use lofty::prelude::TaggedFileExt;
use lofty::probe::Probe as LoftyProbe;
use reqwest::Client;
use tokio::sync::Semaphore;

use crate::db_worker::DbWorker;
use crate::theme::{ArtworkPalette, rgba_image_to_render_image};

/// Shared live network storage credentials for authenticated WebDAV/SMB artwork fetches.
#[derive(Debug, Clone, Default)]
pub struct RemoteArtworkAuth {
    pub webdav: WebDavConfig,
    pub smb: SmbConfig,
}

/// URI prefix for local audio files whose cover art is extracted via `lofty` (or folder sidecar).
pub const LOCAL_ARTWORK_SCHEME: &str = "local-art://";
/// Maximum concurrent decoded album covers kept in GPUI's wgpu texture atlas.
pub const MAX_CACHED_COVERS: usize = 128;
/// Maximum edge in pixels for list/card/player-bar thumbnails.
pub const THUMBNAIL_MAX_PX: u32 = 256;
/// Maximum edge in pixels for Now Playing hero / album detail artwork.
pub const HERO_MAX_PX: u32 = 512;
/// Display size threshold (in logical px) above which [`ArtworkTier::Hero`] is requested.
pub const HERO_DISPLAY_THRESHOLD_PX: f32 = 160.0;
/// Explicit `Accept` header avoiding AVIF (unsupported by the `image` crate features in use).
pub const ARTWORK_ACCEPT_HEADER: &str = "image/jpeg,image/png,image/webp;q=0.9,*/*;q=0.1";
/// Maximum concurrent remote artwork downloads so covers never contend with audio.
const MAX_CONCURRENT_FETCHES: usize = 8;
/// Cooldown before retrying a URL whose download or decode failed.
const RETRY_COOLDOWN: Duration = Duration::from_secs(15);
const ARTWORK_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const LOCAL_SIDECAR_NAMES: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.png",
    "Cover.jpg",
    "Folder.jpg",
];

fn fetch_semaphore() -> &'static Arc<Semaphore> {
    static SEM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES)))
}

/// Resolution tier separating compact list thumbnails (`256px`) from large hero covers (`512px`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ArtworkTier {
    #[default]
    Thumbnail,
    Hero,
}

impl ArtworkTier {
    pub const fn max_px(self) -> u32 {
        match self {
            Self::Thumbnail => THUMBNAIL_MAX_PX,
            Self::Hero => HERO_MAX_PX,
        }
    }

    pub const fn as_i32(self) -> i32 {
        self.max_px() as i32
    }

    /// Selects [`Self::Hero`] for large display slots (e.g. Now Playing) and [`Self::Thumbnail`]
    /// for lists, search results, cards, and the bottom player bar.
    pub fn for_display_size(size_px: f32) -> Self {
        if size_px > HERO_DISPLAY_THRESHOLD_PX {
            Self::Hero
        } else {
            Self::Thumbnail
        }
    }
}

/// Decoded album artwork entry ready for GPUI `img(...)` + dynamic mesh gradient.
#[derive(Clone)]
pub struct CachedArtwork {
    pub image: Arc<RenderImage>,
    pub mesh: Arc<RenderImage>,
}

/// Bounded LRU artwork cache holding at most [`MAX_CACHED_COVERS`] tier-keyed entries.
pub struct ArtworkCache {
    capacity: usize,
    entries: HashMap<String, CachedArtwork>,
    order: VecDeque<String>,
    in_flight: HashSet<String>,
    failed_at: HashMap<String, Instant>,
}

impl Default for ArtworkCache {
    fn default() -> Self {
        Self::new(MAX_CACHED_COVERS)
    }
}

impl ArtworkCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(4),
            entries: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashSet::new(),
            failed_at: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn get(&self, url: &str) -> Option<&CachedArtwork> {
        self.get_tier(url, ArtworkTier::Thumbnail)
    }

    pub fn get_tier(&self, url: &str, tier: ArtworkTier) -> Option<&CachedArtwork> {
        let key = memory_cache_key(url, tier);
        if key.is_empty() {
            return None;
        }
        self.entries.get(&key)
    }

    /// Returns the requested tier if cached, falling back to the alternate tier if already
    /// decoded so large hero surfaces never flash blank while upgrading from `256px` -> `512px`.
    pub fn get_best_for_tier(&self, url: &str, tier: ArtworkTier) -> Option<&CachedArtwork> {
        self.get_tier(url, tier).or_else(|| {
            let fallback = match tier {
                ArtworkTier::Hero => ArtworkTier::Thumbnail,
                ArtworkTier::Thumbnail => ArtworkTier::Hero,
            };
            self.get_tier(url, fallback)
        })
    }

    /// Returns `true` if `url` is cached, currently fetching, or in retry cooldown at Thumbnail tier.
    #[allow(dead_code)]
    pub fn is_settled(&self, url: &str) -> bool {
        self.is_settled_tier(url, ArtworkTier::Thumbnail)
    }

    /// Returns `true` if `(url, tier)` is cached, currently fetching, or in retry cooldown.
    pub fn is_settled_tier(&self, url: &str, tier: ArtworkTier) -> bool {
        let key = memory_cache_key(url, tier);
        if key.is_empty() || self.entries.contains_key(&key) || self.in_flight.contains(&key) {
            return true;
        }
        self.failed_at
            .get(&key)
            .is_some_and(|at| at.elapsed() < RETRY_COOLDOWN)
    }

    /// Marks `url` as in-flight at Thumbnail tier; returns `Some(normalized_url)` if caller should fetch.
    #[allow(dead_code)]
    pub fn begin_fetch(&mut self, url: &str) -> Option<String> {
        self.begin_fetch_tier(url, ArtworkTier::Thumbnail)
            .map(|(_, normalized)| normalized)
    }

    /// Marks `(url, tier)` as in-flight; returns `Some((cache_key, normalized_url))` if caller should fetch.
    pub fn begin_fetch_tier(&mut self, url: &str, tier: ArtworkTier) -> Option<(String, String)> {
        let normalized = normalize_artwork_url(url);
        if normalized.is_empty() {
            return None;
        }
        let key = memory_cache_key(&normalized, tier);
        if self.entries.contains_key(&key) || self.in_flight.contains(&key) {
            return None;
        }
        if let Some(at) = self.failed_at.get(&key)
            && at.elapsed() < RETRY_COOLDOWN
        {
            return None;
        }
        self.failed_at.remove(&key);
        self.in_flight.insert(key.clone());
        Some((key, normalized))
    }

    #[allow(dead_code)]
    pub fn mark_failed(&mut self, url: &str) {
        self.mark_failed_tier(url, ArtworkTier::Thumbnail);
    }

    pub fn mark_failed_tier(&mut self, url: &str, tier: ArtworkTier) {
        let key = memory_cache_key(url, tier);
        if key.is_empty() {
            return;
        }
        self.in_flight.remove(&key);
        self.failed_at.insert(key, Instant::now());
    }

    /// Inserts a newly decoded Thumbnail cover, returning any evicted [`CachedArtwork`] so the
    /// caller can call `cx.drop_image(...)` to immediately reclaim `wgpu` atlas tiles.
    #[allow(dead_code)]
    pub fn insert(&mut self, url: String, artwork: CachedArtwork) -> Vec<CachedArtwork> {
        self.insert_tier(&url, ArtworkTier::Thumbnail, artwork)
    }

    /// Inserts a newly decoded cover at `tier`, returning any evicted [`CachedArtwork`].
    pub fn insert_tier(
        &mut self,
        url: &str,
        tier: ArtworkTier,
        artwork: CachedArtwork,
    ) -> Vec<CachedArtwork> {
        let key = memory_cache_key(url, tier);
        if key.is_empty() {
            return Vec::new();
        }
        self.in_flight.remove(&key);
        self.failed_at.remove(&key);
        let mut evicted = Vec::new();
        if let Some(prev) = self.entries.insert(key.clone(), artwork) {
            evicted.push(prev);
        }
        if let Some(pos) = self.order.iter().position(|k| k == &key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);

        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old_art) = self.entries.remove(&oldest) {
                evicted.push(old_art);
            }
        }
        evicted
    }
}

/// Builds the canonical `local-art://` URI for extracting embedded cover art from a local file.
pub fn local_artwork_url(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with(LOCAL_ARTWORK_SCHEME) {
        trimmed.to_owned()
    } else {
        format!("{LOCAL_ARTWORK_SCHEME}{trimmed}")
    }
}

/// Normalizes protocol-relative (`//...`), insecure YouTube/Google CDN `http://` URLs,
/// `i.ytimg.com/vi_webp/...` URLs, and strips unsupported Google CDN format flags (`-rw`).
pub fn normalize_artwork_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with(LOCAL_ARTWORK_SCHEME) || SmbConfig::is_smb_url(trimmed) {
        return trimmed.to_owned();
    }

    let mut url = if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        if is_google_or_yt_cdn_host(rest) {
            format!("https://{rest}")
        } else {
            trimmed.to_owned()
        }
    } else {
        trimmed.to_owned()
    };

    // Convert `i.ytimg.com/vi_webp/<vid>/<variant>.webp` to reliable JPEG `/vi/<vid>/<variant>.jpg`.
    if let Some(converted) = convert_ytimg_vi_webp(&url) {
        url = converted;
    }

    // Sanitize Google CDN flags (e.g. `-rw` / `-rwa` which can return AVIF or animated WebP).
    if is_google_cdn_url(&url) {
        url = sanitize_google_cdn_url(&url, None);
    }

    url
}

/// Tier-aware URL normalization (scales Google CDN `=wN-hN`/`=sN` parameters to match `tier`).
pub fn normalize_artwork_url_for_tier(raw: &str, tier: ArtworkTier) -> String {
    let base = normalize_artwork_url(raw);
    if base.is_empty() || !is_google_cdn_url(&base) {
        return base;
    }
    let target_px = match tier {
        ArtworkTier::Thumbnail => 256,
        ArtworkTier::Hero => 544,
    };
    sanitize_google_cdn_url(&base, Some(target_px))
}

/// Computes the tier-qualified in-memory cache key.
pub fn memory_cache_key(url: &str, tier: ArtworkTier) -> String {
    let norm = normalize_artwork_url(url);
    if norm.is_empty() {
        String::new()
    } else {
        format!("{}:{norm}", tier.as_i32())
    }
}

/// Computes the deterministic SQLite / disk cache key (`t256_<hash>` or `t512_<hash>`).
pub fn disk_cache_key(url: &str, tier: ArtworkTier) -> String {
    let norm = normalize_artwork_url(url);
    format!("t{}_{}", tier.as_i32(), fnv1a_hex(&norm))
}

/// Resolves the best artwork URL for a [`Song`], supporting:
/// 1. Explicit `song.thumbnail_url` (normalized)
/// 2. Local audio file embedded/sidecar cover via `local-art://<local_path>`
/// 3. Canonical YouTube `i.ytimg.com/vi/<videoId>/hqdefault.jpg` fallback
pub fn song_thumbnail_url(song: &Song) -> Option<String> {
    if let Some(ref url) = song.thumbnail_url {
        let norm = normalize_artwork_url(url);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    if let Some(ref local) = song.local_path {
        let uri = local_artwork_url(local);
        if !uri.is_empty() {
            return Some(uri);
        }
    }
    if is_valid_youtube_video_id(&song.video_id) {
        return Some(format!(
            "https://i.ytimg.com/vi/{}/hqdefault.jpg",
            song.video_id
        ));
    }
    None
}

/// Builds the ordered list of HTTP candidate URLs for `url` at `tier`, including a clean
/// fallback chain (`sddefault` -> `hqdefault` -> `mqdefault` -> `default`) for YouTube covers.
pub fn artwork_candidate_urls(raw_url: &str, tier: ArtworkTier) -> Vec<String> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut push_unique = |candidate: String| {
        if !candidate.is_empty() && !out.contains(&candidate) {
            out.push(candidate);
        }
    };

    let primary = normalize_artwork_url_for_tier(trimmed, tier);
    push_unique(primary);

    // Keep the minimally-normalized original URL as fallback if Google CDN rejects rewritten sizing.
    let raw_https = if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.to_owned()
    };
    if is_google_cdn_url(&raw_https) {
        push_unique(normalize_artwork_url(trimmed));
        push_unique(raw_https);
    }

    if let Some(vid) = extract_youtube_video_id(trimmed) {
        let variants: &[&str] = match tier {
            ArtworkTier::Hero => &["sddefault.jpg", "hqdefault.jpg", "mqdefault.jpg", "default.jpg"],
            ArtworkTier::Thumbnail => &["hqdefault.jpg", "mqdefault.jpg", "default.jpg"],
        };
        for variant in variants {
            push_unique(format!("https://i.ytimg.com/vi/{vid}/{variant}"));
        }
    }

    out
}

/// Fetches (or loads from self-healing disk cache), downscales to `<= 256px`, computes
/// [`ArtworkPalette`], builds the 64x64 BGRA mesh, and returns a [`CachedArtwork`].
#[allow(dead_code)]
pub async fn fetch_and_decode_artwork(
    http: &Client,
    cache_dir: &Path,
    url: &str,
    remote_auth: Option<&RemoteArtworkAuth>,
) -> Result<CachedArtwork, String> {
    fetch_and_decode_artwork_with_tier(
        http,
        cache_dir,
        url,
        ArtworkTier::Thumbnail,
        remote_auth,
        None,
    )
    .await
}

/// Tier-aware artwork fetcher with self-healing disk cache, `lofty` local cover extraction,
/// YouTube fallback chain, anti-AVIF headers, and SQLite v4 LRU index updates via [`DbWorker`].
pub async fn fetch_and_decode_artwork_with_tier(
    http: &Client,
    cache_dir: &Path,
    url: &str,
    tier: ArtworkTier,
    remote_auth: Option<&RemoteArtworkAuth>,
    db_worker: Option<&DbWorker>,
) -> Result<CachedArtwork, String> {
    let normalized = normalize_artwork_url(url);
    if normalized.is_empty() {
        return Err("empty artwork URL".to_owned());
    }

    let art_dir = cache_dir.join("artwork");
    let cache_key = disk_cache_key(&normalized, tier);
    let disk_path = art_dir.join(format!("{cache_key}.img"));
    let max_px = tier.max_px();

    // Fast path: check, read, and decode local disk cache on Tokio's blocking pool
    // so image decode & filesystem I/O never stall async audio chunk fetches.
    let fast_disk_path = disk_path.clone();
    let fast_art_dir = art_dir.clone();
    let cached_probe = tokio::task::spawn_blocking(move || {
        let _ = std::fs::create_dir_all(&fast_art_dir);
        if fast_disk_path.exists() {
            if let Ok(cached_bytes) = std::fs::read(&fast_disk_path)
                && let Ok(art) = decode_artwork_bytes(&cached_bytes, max_px)
            {
                return DiskProbe::Hit(art);
            }
            let _ = std::fs::remove_file(&fast_disk_path);
            return DiskProbe::CorruptRemoved;
        }
        DiskProbe::Miss
    })
    .await
    .map_err(|e| format!("artwork cache task join error: {e}"))?;

    match cached_probe {
        DiskProbe::Hit(art) => {
            if let Some(db) = db_worker {
                db.touch_artwork_entry(cache_key);
            }
            return Ok(art);
        }
        DiskProbe::CorruptRemoved => {
            if let Some(db) = db_worker {
                db.delete_artwork_entry(cache_key.clone());
            }
        }
        DiskProbe::Miss => {}
    }

    // Local embedded cover art via `lofty` (+ folder sidecar image fallback).
    if let Some(local_audio_path) = normalized.strip_prefix(LOCAL_ARTWORK_SCHEME) {
        let audio_path = PathBuf::from(local_audio_path);
        let bytes = tokio::task::spawn_blocking(move || extract_local_cover_bytes(&audio_path))
            .await
            .map_err(|e| format!("local cover task join error: {e}"))??;
        return decode_and_persist_blocking(bytes, disk_path, cache_key, tier, db_worker.cloned())
            .await;
    }

    if SmbConfig::is_smb_url(&normalized) {
        let Some(auth) = remote_auth.filter(|a| a.smb.is_configured()) else {
            return Err("SMB artwork requires configured SMB share".to_owned());
        };
        let _permit = fetch_semaphore()
            .acquire()
            .await
            .map_err(|_| "artwork fetch cancelled".to_owned())?;
        let bytes = SmbClient::new()
            .read_file_capped(&auth.smb, &normalized, MAX_COVER_BYTES)
            .await
            .map_err(|e| format!("SMB artwork read failed: {e}"))?;
        return decode_and_persist_blocking(bytes, disk_path, cache_key, tier, db_worker.cloned())
            .await;
    }

    if normalized.starts_with("http://") || normalized.starts_with("https://") {
        let _permit = fetch_semaphore()
            .acquire()
            .await
            .map_err(|_| "artwork fetch cancelled".to_owned())?;

        let candidates = artwork_candidate_urls(url, tier);
        let mut last_err = "no artwork candidate URLs".to_owned();

        for candidate in candidates {
            let mut req = http
                .get(&candidate)
                .timeout(Duration::from_secs(12))
                .header(reqwest::header::USER_AGENT, ARTWORK_USER_AGENT)
                .header(reqwest::header::ACCEPT, ARTWORK_ACCEPT_HEADER);
            if let Some(auth_hdr) =
                remote_auth.and_then(|a| a.webdav.auth_header_for_url(&candidate))
            {
                req = req.header(reqwest::header::AUTHORIZATION, auth_hdr);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("artwork request failed ({candidate}): {e}");
                    continue;
                }
            };
            if !resp.status().is_success() {
                last_err = format!("artwork HTTP {} ({candidate})", resp.status());
                continue;
            }
            let bytes = match resp.bytes().await {
                Ok(b) => b.to_vec(),
                Err(e) => {
                    last_err = format!("artwork body failed ({candidate}): {e}");
                    continue;
                }
            };
            match decode_and_persist_blocking(
                bytes,
                disk_path.clone(),
                cache_key.clone(),
                tier,
                db_worker.cloned(),
            )
            .await
            {
                Ok(art) => return Ok(art),
                Err(e) => {
                    last_err = format!("artwork decode failed ({candidate}): {e}");
                }
            }
        }
        return Err(last_err);
    }

    // Plain local filesystem path (image file or direct audio file path).
    let path_buf = PathBuf::from(&normalized);
    let raw_bytes = tokio::task::spawn_blocking(move || {
        if is_audio_file_extension(&path_buf) {
            extract_local_cover_bytes(&path_buf)
        } else {
            std::fs::read(&path_buf).map_err(|e| format!("local artwork read failed: {e}"))
        }
    })
    .await
    .map_err(|e| format!("local artwork task join error: {e}"))??;
    decode_and_persist_blocking(raw_bytes, disk_path, cache_key, tier, db_worker.cloned()).await
}

enum DiskProbe {
    Hit(CachedArtwork),
    CorruptRemoved,
    Miss,
}

/// Extracts embedded cover art bytes from a local audio file via `lofty`, falling back to
/// common folder sidecar covers (`cover.jpg`, `folder.jpg`, etc.) in the same directory.
pub fn extract_local_cover_bytes(audio_path: &Path) -> Result<Vec<u8>, String> {
    let parse_opts = ParseOptions::new().read_properties(false);
    let tagged_opt = LoftyProbe::open(audio_path)
        .ok()
        .and_then(|p| p.options(parse_opts).guess_file_type().ok())
        .and_then(|p| p.read().ok());
    if let Some(tagged) = tagged_opt {
        let mut fallback_pic: Option<&[u8]> = None;
        let tags = tagged.primary_tag().into_iter().chain(tagged.tags().iter());
        for tag in tags {
            for pic in tag.pictures() {
                if pic.data().is_empty() {
                    continue;
                }
                if pic.pic_type() == PictureType::CoverFront {
                    return Ok(pic.data().to_vec());
                }
                if fallback_pic.is_none() {
                    fallback_pic = Some(pic.data());
                }
            }
        }
        if let Some(bytes) = fallback_pic {
            return Ok(bytes.to_vec());
        }
    }

    if let Some(parent) = audio_path.parent() {
        for name in LOCAL_SIDECAR_NAMES {
            let candidate = parent.join(name);
            if candidate.is_file()
                && let Ok(meta) = std::fs::metadata(&candidate)
                && meta.len() > 0
                && meta.len() <= MAX_COVER_BYTES as u64
                && let Ok(bytes) = std::fs::read(&candidate)
            {
                return Ok(bytes);
            }
        }
    }

    Err(format!(
        "no embedded or sidecar artwork found for {}",
        audio_path.display()
    ))
}

async fn decode_and_persist_blocking(
    bytes: Vec<u8>,
    disk_path: PathBuf,
    cache_key: String,
    tier: ArtworkTier,
    db_worker: Option<DbWorker>,
) -> Result<CachedArtwork, String> {
    let max_px = tier.max_px();
    let (art, written_size) = tokio::task::spawn_blocking(move || {
        let art = decode_artwork_bytes(&bytes, max_px)?;
        if let Some(parent) = disk_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp_path = disk_path.with_extension("img.tmp");
        let persisted = if std::fs::write(&tmp_path, &bytes).is_ok()
            && std::fs::rename(&tmp_path, &disk_path).is_ok()
        {
            Some((disk_path.to_string_lossy().into_owned(), bytes.len() as i64))
        } else {
            let _ = std::fs::remove_file(&tmp_path);
            None
        };
        Ok::<_, String>((art, persisted))
    })
    .await
    .map_err(|e| format!("artwork decode task join error: {e}"))??;

    if let (Some(db), Some((file_path, byte_size))) = (db_worker, written_size) {
        let now_ms = unix_now_ms();
        db.upsert_artwork_entry(ArtworkIndexEntry {
            cache_key,
            tier: tier.as_i32(),
            file_path,
            byte_size,
            last_accessed_ms: now_ms,
            created_at_ms: now_ms,
        });
    }

    Ok(art)
}

fn decode_artwork_bytes(raw_bytes: &[u8], max_px: u32) -> Result<CachedArtwork, String> {
    let decoded =
        image::load_from_memory(raw_bytes).map_err(|e| format!("artwork decode failed: {e}"))?;
    let resized = if decoded.width() > max_px || decoded.height() > max_px {
        decoded.resize(max_px, max_px, FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = resized.to_rgba8();
    let palette = ArtworkPalette::from_rgba_image(&rgba);
    let mesh = palette.build_mesh_render_image();
    let image = rgba_image_to_render_image(rgba);
    Ok(CachedArtwork { image, mesh })
}

fn is_audio_file_extension(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "flac" | "wav" | "mp3" | "opus" | "ogg" | "m4a" | "aac" | "aiff" | "dsf" | "dff"
    )
}

fn is_valid_youtube_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_google_or_yt_cdn_host(rest_after_scheme: &str) -> bool {
    let host = rest_after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host.ends_with("googleusercontent.com")
        || host.ends_with("ggpht.com")
        || host.ends_with("ytimg.com")
        || host == "img.youtube.com"
}

fn is_google_cdn_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host.ends_with("googleusercontent.com") || host.ends_with("ggpht.com")
}

/// Converts `https://i.ytimg.com/vi_webp/<vid>/<name>.webp[?sqp=...]` into
/// `https://i.ytimg.com/vi/<vid>/<name>.jpg`.
fn convert_ytimg_vi_webp(url: &str) -> Option<String> {
    let marker = "ytimg.com/vi_webp/";
    let idx = url.find(marker)?;
    let prefix = &url[..idx];
    let after = &url[idx + marker.len()..];
    let path_only = after.split(['?', '#']).next().unwrap_or(after);
    let mut parts = path_only.split('/');
    let vid = parts.next()?;
    let file = parts.next()?;
    if !is_valid_youtube_video_id(vid) {
        return None;
    }
    let stem = file.strip_suffix(".webp").unwrap_or(file);
    let jpg_file = if stem.ends_with(".jpg") {
        stem.to_owned()
    } else {
        format!("{stem}.jpg")
    };
    Some(format!("{prefix}ytimg.com/vi/{vid}/{jpg_file}"))
}

/// Extracts an 11-character YouTube video ID from `i.ytimg.com` or `img.youtube.com` URLs.
pub fn extract_youtube_video_id(url: &str) -> Option<String> {
    for marker in [
        "ytimg.com/vi/",
        "ytimg.com/vi_webp/",
        "img.youtube.com/vi/",
        "img.youtube.com/vi_webp/",
    ] {
        if let Some(idx) = url.find(marker) {
            let after = &url[idx + marker.len()..];
            let candidate = after.split(['/', '?', '#']).next().unwrap_or("");
            if is_valid_youtube_video_id(candidate) {
                return Some(candidate.to_owned());
            }
        }
    }
    None
}

/// Cleans Google CDN `=...` image transform directives so servers return standard JPEG/PNG
/// (never AVIF/animated WebP via `-rw`/`-rwa`) and optionally adjusts `wN-hN` / `sN` sizing
/// to match the requested [`ArtworkTier`].
fn sanitize_google_cdn_url(url: &str, target_px: Option<u32>) -> String {
    // Only inspect `=` inside the final path segment (not query parameters).
    let (before_query, query_suffix) = match url.find('?') {
        Some(q) => (&url[..q], &url[q..]),
        None => (url, ""),
    };
    let Some(last_slash) = before_query.rfind('/') else {
        return url.to_owned();
    };
    let last_seg = &before_query[last_slash + 1..];
    let Some(eq_rel) = last_seg.find('=') else {
        return url.to_owned();
    };
    let base = &before_query[..last_slash + 1 + eq_rel];
    let params = &last_seg[eq_rel + 1..];
    if params.is_empty() {
        return url.to_owned();
    }

    let mut tokens: Vec<String> = Vec::new();
    let mut had_size = false;
    let mut had_jpeg = false;

    for tok in params.split('-') {
        if tok.is_empty() {
            continue;
        }
        // Drop WebP/AVIF forcing flags (`-rw`, `-rwa`, `-fv`).
        if matches!(tok, "rw" | "rwa" | "fv") {
            continue;
        }
        if tok == "rj" {
            had_jpeg = true;
            tokens.push(tok.to_owned());
            continue;
        }
        if let Some(px) = target_px {
            if let Some(num) = tok.strip_prefix('w')
                && !num.is_empty()
                && num.chars().all(|c| c.is_ascii_digit())
            {
                had_size = true;
                tokens.push(format!("w{px}"));
                continue;
            }
            if let Some(num) = tok.strip_prefix('h')
                && !num.is_empty()
                && num.chars().all(|c| c.is_ascii_digit())
            {
                had_size = true;
                tokens.push(format!("h{px}"));
                continue;
            }
            if let Some(num) = tok.strip_prefix('s')
                && !num.is_empty()
                && num.chars().all(|c| c.is_ascii_digit())
            {
                had_size = true;
                tokens.push(format!("s{px}"));
                continue;
            }
        }
        tokens.push(tok.to_owned());
    }

    if !had_jpeg && (had_size || params.contains("rw")) {
        tokens.push("rj".to_owned());
    }

    if tokens.is_empty() {
        format!("{base}{query_suffix}")
    } else {
        format!("{base}={}{query_suffix}", tokens.join("-"))
    }
}

fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{ImageBuffer, ImageFormat, Rgba};
    use lofty::picture::{MimeType, Picture};
    use lofty::prelude::TagExt;
    use lofty::tag::{Tag, TagType};

    use super::*;

    fn dummy_art() -> CachedArtwork {
        let img = ImageBuffer::from_pixel(4, 4, Rgba([40, 80, 120, 255]));
        let palette = ArtworkPalette::from_rgba_image(&img);
        CachedArtwork {
            image: rgba_image_to_render_image(img),
            mesh: palette.build_mesh_render_image(),
        }
    }

    fn sample_png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = ImageBuffer::from_pixel(width, height, Rgba([220, 40, 80, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .expect("encode png");
        out
    }

    /// Creates a minimal valid MP3 file (4 silent MPEG-1 Layer III frames) with an embedded ID3v2
    /// Front Cover picture using `lofty`.
    fn write_mp3_with_embedded_cover(path: &Path, cover_png: &[u8]) {
        // Minimal valid MPEG1 Layer3 128kbps 44.1kHz mono frame (417 bytes: 0xFF 0xFB 0x90 0xC4 + zeros)
        let mut frame = vec![0u8; 417];
        frame[0] = 0xFF;
        frame[1] = 0xFB;
        frame[2] = 0x90;
        frame[3] = 0xC4;
        let mut mp3_bytes = Vec::with_capacity(417 * 4);
        for _ in 0..4 {
            mp3_bytes.extend_from_slice(&frame);
        }
        std::fs::write(path, &mp3_bytes).expect("write dummy mp3");

        let mut tag = Tag::new(TagType::Id3v2);
        let pic = Picture::unchecked(cover_png.to_vec())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build();
        tag.push_picture(pic);
        tag.save_to_path(path, lofty::config::WriteOptions::default())
            .expect("save id3v2 cover");
    }

    #[test]
    fn lru_artwork_cache_enforces_capacity_and_separates_tiers() {
        let mut cache = ArtworkCache::new(4);
        assert!(cache.begin_fetch_tier("https://example.com/a.jpg", ArtworkTier::Thumbnail).is_some());
        assert!(cache.begin_fetch_tier("https://example.com/a.jpg", ArtworkTier::Thumbnail).is_none());
        // Hero tier has its own cache key and can fetch concurrently with Thumbnail.
        assert!(cache.begin_fetch_tier("https://example.com/a.jpg", ArtworkTier::Hero).is_some());

        cache.insert_tier("https://example.com/a.jpg", ArtworkTier::Thumbnail, dummy_art());
        assert!(cache.get_tier("https://example.com/a.jpg", ArtworkTier::Thumbnail).is_some());
        assert!(cache.get_tier("https://example.com/a.jpg", ArtworkTier::Hero).is_none());
        // `get_best_for_tier` smoothly falls back to Thumbnail while Hero is still loading.
        assert!(cache.get_best_for_tier("https://example.com/a.jpg", ArtworkTier::Hero).is_some());

        let mut cap_cache = ArtworkCache::new(4);
        for i in 1..=6 {
            let key = format!("u{i}");
            let evicted = cap_cache.insert(key, dummy_art());
            if i <= 4 {
                assert!(evicted.is_empty());
            } else {
                assert_eq!(evicted.len(), 1);
            }
        }
        assert_eq!(cap_cache.len(), 4);
        assert!(cap_cache.get("u1").is_none());
        assert!(cap_cache.get("u2").is_none());
        assert!(cap_cache.get("u3").is_some());
        assert!(cap_cache.get("u6").is_some());
    }

    #[test]
    fn normalizes_google_cdn_vi_webp_and_builds_youtube_fallback_chain() {
        assert_eq!(
            normalize_artwork_url("//lh3.googleusercontent.com/abc"),
            "https://lh3.googleusercontent.com/abc"
        );
        // Strip `-rw` (WebP/AVIF trigger) and enforce `-rj` (JPEG) while preserving size / scaling by tier.
        assert_eq!(
            normalize_artwork_url("https://lh3.googleusercontent.com/xyz=w60-h60-l90-rw"),
            "https://lh3.googleusercontent.com/xyz=w60-h60-l90-rj"
        );
        assert_eq!(
            normalize_artwork_url_for_tier(
                "https://lh3.googleusercontent.com/xyz=w60-h60-l90-rw",
                ArtworkTier::Hero
            ),
            "https://lh3.googleusercontent.com/xyz=w544-h544-l90-rj"
        );
        assert_eq!(
            normalize_artwork_url_for_tier(
                "https://yt3.ggpht.com/avatar=s68-c-k-c0x00ffffff-no-rj-rw",
                ArtworkTier::Thumbnail
            ),
            "https://yt3.ggpht.com/avatar=s256-c-k-c0x00ffffff-no-rj"
        );

        // `vi_webp` -> JPEG `/vi/.../hqdefault.jpg`
        assert_eq!(
            normalize_artwork_url("https://i.ytimg.com/vi_webp/MV_3Dpw-BRY/hqdefault.webp?sqp=123"),
            "https://i.ytimg.com/vi/MV_3Dpw-BRY/hqdefault.jpg"
        );

        let hero_candidates = artwork_candidate_urls(
            "https://i.ytimg.com/vi/MV_3Dpw-BRY/maxresdefault.jpg",
            ArtworkTier::Hero,
        );
        assert_eq!(
            hero_candidates,
            vec![
                "https://i.ytimg.com/vi/MV_3Dpw-BRY/maxresdefault.jpg",
                "https://i.ytimg.com/vi/MV_3Dpw-BRY/sddefault.jpg",
                "https://i.ytimg.com/vi/MV_3Dpw-BRY/hqdefault.jpg",
                "https://i.ytimg.com/vi/MV_3Dpw-BRY/mqdefault.jpg",
                "https://i.ytimg.com/vi/MV_3Dpw-BRY/default.jpg",
            ]
        );

        // `song_thumbnail_url` synthesizes YouTube fallback for remote tracks and `local-art://` for local files.
        let yt_song = Song::new("MV_3Dpw-BRY", "Nightcall", "Kavinsky", None);
        assert_eq!(
            song_thumbnail_url(&yt_song).as_deref(),
            Some("https://i.ytimg.com/vi/MV_3Dpw-BRY/hqdefault.jpg")
        );
        let mut local_song = Song::new("local:/tmp/a.flac", "Local", "Artist", None);
        local_song.local_path = Some("/tmp/a.flac".to_owned());
        assert_eq!(
            song_thumbnail_url(&local_song).as_deref(),
            Some("local-art:///tmp/a.flac")
        );
        assert!(!ARTWORK_ACCEPT_HEADER.contains("avif"));
    }

    #[test]
    fn extracts_local_embedded_cover_via_lofty_and_sidecar_and_persists_lru_tiers() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");

        let tmp_dir = std::env::temp_dir().join(format!(
            "myusic_artwork_test_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        let music_dir = tmp_dir.join("music");
        let cache_dir = tmp_dir.join("cache");
        std::fs::create_dir_all(&music_dir).expect("mkdir music");

        // 1. Embedded ID3v2 cover in local MP3
        let mp3_path = music_dir.join("embedded.mp3");
        let cover_png = sample_png_bytes(600, 600);
        write_mp3_with_embedded_cover(&mp3_path, &cover_png);

        let extracted = extract_local_cover_bytes(&mp3_path).expect("extract embedded cover");
        assert_eq!(extracted, cover_png);

        // 2. Folder sidecar fallback when file has no embedded tag
        let sidecar_dir = tmp_dir.join("sidecar");
        std::fs::create_dir_all(&sidecar_dir).expect("mkdir sidecar");
        let bare_audio = sidecar_dir.join("track.flac");
        std::fs::write(&bare_audio, b"not-a-tagged-file").expect("write bare audio");
        let sidecar_png = sample_png_bytes(120, 120);
        std::fs::write(sidecar_dir.join("cover.png"), &sidecar_png).expect("write sidecar");
        assert_eq!(
            extract_local_cover_bytes(&bare_audio).expect("sidecar cover"),
            sidecar_png
        );

        // 3. Full async decode at both Thumbnail (256px) and Hero (512px) tiers + DbWorker LRU prune
        let db = DbWorker::start(tmp_dir.join("library.db"));
        let http = Client::new();
        let uri = local_artwork_url(&mp3_path.to_string_lossy());

        rt.block_on(async {
            let thumb = fetch_and_decode_artwork_with_tier(
                &http,
                &cache_dir,
                &uri,
                ArtworkTier::Thumbnail,
                None,
                Some(&db),
            )
            .await
            .expect("decode 256px tier");
            let hero = fetch_and_decode_artwork_with_tier(
                &http,
                &cache_dir,
                &uri,
                ArtworkTier::Hero,
                None,
                Some(&db),
            )
            .await
            .expect("decode 512px tier");

            assert_eq!(thumb.image.size(0).width.0, 256);
            assert_eq!(hero.image.size(0).width.0, 512);

            let thumb_file = cache_dir
                .join("artwork")
                .join(format!("{}.img", disk_cache_key(&uri, ArtworkTier::Thumbnail)));
            let hero_file = cache_dir
                .join("artwork")
                .join(format!("{}.img", disk_cache_key(&uri, ArtworkTier::Hero)));
            assert!(thumb_file.exists());
            assert!(hero_file.exists());

            // Prune down to 1 byte -> both files get evicted from SQLite and unlinked from disk.
            let evicted = db.prune_artwork_lru(1).await;
            assert_eq!(evicted.len(), 2);
            assert!(!thumb_file.exists());
            assert!(!hero_file.exists());
        });

        db.shutdown_blocking();
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn http_fetch_sends_non_avif_accept_header_and_persists_to_disk_cache() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test tokio runtime");

        let tmp_dir = std::env::temp_dir().join(format!(
            "myusic_artwork_http_test_{}_{}",
            std::process::id(),
            unix_now_ms()
        ));
        let cache_dir = tmp_dir.join("cache");
        let png = sample_png_bytes(400, 400);

        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("addr");
            let png_clone = png.clone();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.expect("read req");
                let req_text = String::from_utf8_lossy(&buf[..n]).to_string();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    png_clone.len()
                );
                socket.write_all(header.as_bytes()).await.expect("write hdr");
                socket.write_all(&png_clone).await.expect("write body");
                let _ = socket.shutdown().await;
                req_text
            });

            let http = Client::new();
            let url = format!("http://{addr}/cover.png");
            let art = fetch_and_decode_artwork_with_tier(
                &http,
                &cache_dir,
                &url,
                ArtworkTier::Thumbnail,
                None,
                None,
            )
            .await
            .expect("http artwork fetch");
            assert_eq!(art.image.size(0).width.0, 256);

            let req_text = server.await.expect("server join");
            let accept_line = req_text
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("accept:"))
                .expect("accept header present");
            assert!(
                !accept_line.to_ascii_lowercase().contains("avif"),
                "Accept header must not request avif: {accept_line}"
            );
            assert!(accept_line.contains("image/jpeg"));
        });

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
