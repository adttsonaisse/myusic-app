//! Bounded LRU artwork cache + self-healing disk cover-art cache (`gpui-linux.md` §4.3 & §5.4).
//!
//! Capped at `MAX_CACHED_COVERS = 128` GPU textures (downscaled to `<= 256px`
//! to stay well inside Intel UHD shared-VRAM limits on a 7.5 GiB laptop), with
//! bounded concurrent HTTP downloads (`MAX_CONCURRENT_FETCHES = 8`) so scrolling
//! thumbnails never starves audio streaming.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bitchord_model::Song;
use gpui::RenderImage;
use image::imageops::FilterType;
use reqwest::Client;
use tokio::sync::Semaphore;

use crate::theme::{ArtworkPalette, rgba_image_to_render_image};

/// Maximum concurrent decoded album covers kept in GPUI's wgpu texture atlas.
pub const MAX_CACHED_COVERS: usize = 128;
/// Maximum thumbnail edge in pixels (never upload raw 3000x3000 covers to VRAM).
pub const THUMBNAIL_MAX_PX: u32 = 256;
/// Maximum concurrent remote artwork downloads so covers never contend with audio.
const MAX_CONCURRENT_FETCHES: usize = 8;
/// Cooldown before retrying a URL whose download or decode failed.
const RETRY_COOLDOWN: Duration = Duration::from_secs(15);
const ARTWORK_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

fn fetch_semaphore() -> &'static Arc<Semaphore> {
    static SEM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES)))
}

/// Decoded album artwork entry ready for GPUI `img(...)` + dynamic mesh gradient.
#[derive(Clone)]
pub struct CachedArtwork {
    pub image: Arc<RenderImage>,
    pub mesh: Arc<RenderImage>,
}

/// Bounded LRU artwork cache holding at most [`MAX_CACHED_COVERS`] entries.
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

    pub fn get(&self, url: &str) -> Option<&CachedArtwork> {
        let key = normalize_artwork_url(url);
        self.entries.get(&key)
    }

    /// Returns `true` if `url` is cached, currently fetching, or in retry cooldown.
    pub fn is_settled(&self, url: &str) -> bool {
        let key = normalize_artwork_url(url);
        if key.is_empty() || self.entries.contains_key(&key) || self.in_flight.contains(&key) {
            return true;
        }
        self.failed_at
            .get(&key)
            .is_some_and(|at| at.elapsed() < RETRY_COOLDOWN)
    }

    /// Marks `url` as in-flight; returns `Some(normalized_url)` if caller should spawn a fetch.
    pub fn begin_fetch(&mut self, url: &str) -> Option<String> {
        let key = normalize_artwork_url(url);
        if key.is_empty() || self.entries.contains_key(&key) || self.in_flight.contains(&key) {
            return None;
        }
        if let Some(at) = self.failed_at.get(&key)
            && at.elapsed() < RETRY_COOLDOWN
        {
            return None;
        }
        self.failed_at.remove(&key);
        self.in_flight.insert(key.clone());
        Some(key)
    }

    pub fn mark_failed(&mut self, url: &str) {
        let key = normalize_artwork_url(url);
        self.in_flight.remove(&key);
        self.failed_at.insert(key, Instant::now());
    }

    /// Inserts a newly decoded cover, returning any evicted [`CachedArtwork`] so the
    /// caller can call `cx.drop_image(...)` to immediately reclaim `wgpu` atlas tiles.
    pub fn insert(&mut self, url: String, artwork: CachedArtwork) -> Vec<CachedArtwork> {
        let key = normalize_artwork_url(&url);
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

/// Normalizes protocol-relative (`//...`) and whitespace-padded thumbnail URLs.
pub fn normalize_artwork_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.to_owned()
    }
}

/// Resolves the best artwork URL for a [`Song`], falling back to YouTube's canonical
/// `i.ytimg.com/vi/<videoId>/hqdefault.jpg` cover for remote tracks lacking an explicit URL.
pub fn song_thumbnail_url(song: &Song) -> Option<String> {
    if let Some(ref url) = song.thumbnail_url {
        let norm = normalize_artwork_url(url);
        if !norm.is_empty() {
            return Some(norm);
        }
    }
    if song.local_path.is_none()
        && song.video_id.len() == 11
        && song
            .video_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(format!(
            "https://i.ytimg.com/vi/{}/hqdefault.jpg",
            song.video_id
        ));
    }
    None
}

/// Fetches (or loads from self-healing disk cache), downscales to `<= 256px`, computes
/// [`ArtworkPalette`], builds the 64x64 BGRA mesh, and returns a [`CachedArtwork`].
pub async fn fetch_and_decode_artwork(
    http: &Client,
    cache_dir: &Path,
    url: &str,
) -> Result<CachedArtwork, String> {
    let normalized = normalize_artwork_url(url);
    if normalized.is_empty() {
        return Err("empty artwork URL".to_owned());
    }

    let art_dir = cache_dir.join("artwork");
    let _ = std::fs::create_dir_all(&art_dir);
    let key = fnv1a_hex(&normalized);
    let disk_path = art_dir.join(format!("{key}.img"));

    // Fast path: try decoding from local disk cache; self-heal if corrupt.
    if disk_path.exists() {
        if let Ok(cached_bytes) = std::fs::read(&disk_path)
            && let Ok(art) = decode_artwork_bytes(&cached_bytes)
        {
            return Ok(art);
        }
        let _ = std::fs::remove_file(&disk_path);
    }

    let raw_bytes = if normalized.starts_with("http://") || normalized.starts_with("https://") {
        let _permit = fetch_semaphore()
            .acquire()
            .await
            .map_err(|_| "artwork fetch cancelled".to_owned())?;
        let resp = http
            .get(&normalized)
            .timeout(Duration::from_secs(12))
            .header(reqwest::header::USER_AGENT, ARTWORK_USER_AGENT)
            .header(
                reqwest::header::ACCEPT,
                "image/webp,image/png,image/jpeg,image/*;q=0.8",
            )
            .send()
            .await
            .map_err(|e| format!("artwork request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("artwork HTTP {}", resp.status()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("artwork body failed: {e}"))?
            .to_vec();
        let art = decode_artwork_bytes(&bytes)?;
        let _ = std::fs::write(&disk_path, &bytes);
        return Ok(art);
    } else {
        std::fs::read(&normalized).map_err(|e| format!("local artwork read failed: {e}"))?
    };

    decode_artwork_bytes(&raw_bytes)
}

fn decode_artwork_bytes(raw_bytes: &[u8]) -> Result<CachedArtwork, String> {
    let decoded =
        image::load_from_memory(raw_bytes).map_err(|e| format!("artwork decode failed: {e}"))?;
    let resized = if decoded.width() > THUMBNAIL_MAX_PX || decoded.height() > THUMBNAIL_MAX_PX {
        decoded.resize(THUMBNAIL_MAX_PX, THUMBNAIL_MAX_PX, FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = resized.to_rgba8();
    let palette = ArtworkPalette::from_rgba_image(&rgba);
    let mesh = palette.build_mesh_render_image();
    let image = rgba_image_to_render_image(rgba);
    Ok(CachedArtwork { image, mesh })
}

fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgba};

    use super::*;

    fn dummy_art() -> CachedArtwork {
        let img = ImageBuffer::from_pixel(4, 4, Rgba([40, 80, 120, 255]));
        let palette = ArtworkPalette::from_rgba_image(&img);
        CachedArtwork {
            image: rgba_image_to_render_image(img),
            mesh: palette.build_mesh_render_image(),
        }
    }

    #[test]
    fn lru_artwork_cache_enforces_capacity_and_evicts_oldest() {
        let mut cache = ArtworkCache::new(4);
        assert!(cache.begin_fetch("u1").is_some());
        assert!(cache.begin_fetch("u1").is_none()); // deduplicated while in-flight

        for i in 1..=6 {
            let key = format!("u{i}");
            let evicted = cache.insert(key, dummy_art());
            if i <= 4 {
                assert!(evicted.is_empty());
            } else {
                assert_eq!(evicted.len(), 1);
            }
        }
        assert_eq!(cache.len(), 4);
        assert!(cache.get("u1").is_none());
        assert!(cache.get("u2").is_none());
        assert!(cache.get("u3").is_some());
        assert!(cache.get("u6").is_some());
    }

    #[test]
    fn normalizes_protocol_relative_and_synthesizes_youtube_fallback_thumbnail() {
        assert_eq!(
            normalize_artwork_url("//lh3.googleusercontent.com/abc"),
            "https://lh3.googleusercontent.com/abc"
        );
        let yt_song = Song::new("MV_3Dpw-BRY", "Nightcall", "Kavinsky", None);
        assert_eq!(
            song_thumbnail_url(&yt_song).as_deref(),
            Some("https://i.ytimg.com/vi/MV_3Dpw-BRY/hqdefault.jpg")
        );
        let mut local_song = Song::new("local:/tmp/a.flac", "Local", "Artist", None);
        local_song.local_path = Some("/tmp/a.flac".to_owned());
        assert_eq!(song_thumbnail_url(&local_song), None);
    }
}
