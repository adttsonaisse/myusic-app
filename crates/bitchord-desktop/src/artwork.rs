//! Bounded LRU artwork cache + disk cover-art cache (`gpui-linux.md` §4.3 & §5.4).
//!
//! Capped strictly at `MAX_CACHED_COVERS = 64` GPU textures to stay comfortably
//! within Intel UHD shared-VRAM limits on a 7.5 GiB laptop. Saves downloaded
//! covers to `cache_dir/artwork/<hash>.png` so MPRIS2 can serve local `file://` URIs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;
use image::imageops::FilterType;
use reqwest::Client;

use crate::theme::{ArtworkPalette, rgba_image_to_render_image};

/// Maximum concurrent decoded album covers kept in GPUI's wgpu texture atlas.
pub const MAX_CACHED_COVERS: usize = 64;
/// Maximum thumbnail edge in pixels (never upload raw 3000x3000 covers to VRAM).
pub const THUMBNAIL_MAX_PX: u32 = 256;

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
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, url: &str) -> Option<&CachedArtwork> {
        self.entries.get(url)
    }

    /// Marks `url` as in-flight; returns `true` if the caller should spawn a fetch.
    pub fn begin_fetch(&mut self, url: &str) -> bool {
        if url.is_empty() || self.entries.contains_key(url) || self.in_flight.contains(url) {
            return false;
        }
        self.in_flight.insert(url.to_owned());
        true
    }

    pub fn mark_failed(&mut self, url: &str) {
        self.in_flight.remove(url);
    }

    /// Inserts a newly decoded cover, returning any evicted [`CachedArtwork`] so the
    /// caller can call `cx.drop_image(...)` to immediately reclaim `wgpu` atlas tiles.
    pub fn insert(&mut self, url: String, artwork: CachedArtwork) -> Vec<CachedArtwork> {
        self.in_flight.remove(&url);
        let mut evicted = Vec::new();
        if let Some(prev) = self.entries.insert(url.clone(), artwork) {
            evicted.push(prev);
        }
        if let Some(pos) = self.order.iter().position(|k| k == &url) {
            self.order.remove(pos);
        }
        self.order.push_back(url);

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

/// Fetches (or loads from disk cache), downscales to `<= 256px`, computes [`ArtworkPalette`],
/// builds the 64x64 BGRA mesh, and returns a [`CachedArtwork`].
pub async fn fetch_and_decode_artwork(
    http: &Client,
    cache_dir: &Path,
    url: &str,
) -> Result<CachedArtwork, String> {
    let art_dir = cache_dir.join("artwork");
    let _ = std::fs::create_dir_all(&art_dir);
    let key = fnv1a_hex(url);
    let disk_path = art_dir.join(format!("{key}.img"));

    let raw_bytes = if disk_path.exists() {
        std::fs::read(&disk_path).map_err(|e| e.to_string())?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let resp = http
            .get(url)
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
        let _ = std::fs::write(&disk_path, &bytes);
        bytes
    } else {
        std::fs::read(url).map_err(|e| format!("local artwork read failed: {e}"))?
    };

    let decoded =
        image::load_from_memory(&raw_bytes).map_err(|e| format!("artwork decode failed: {e}"))?;
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
        assert!(cache.begin_fetch("u1"));
        assert!(!cache.begin_fetch("u1")); // deduplicated while in-flight

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
}
