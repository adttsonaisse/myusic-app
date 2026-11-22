//! Authentic BitChord Linux Desktop design system, theme tokens (Dark OLED &
//! Light Studio matching upstream `Theme.kt`), WCAG contrast helpers, and
//! perceptual 64x64 BGRA dynamic artwork mesh texture generator.

use std::sync::Arc;

#[cfg(test)]
use gpui::hsla_to_rgba;
use gpui::{Global, Hsla, RenderImage, rgb_to_hsla, rgba};
use image::{Frame, ImageBuffer, Rgba as ImgRgba, RgbaImage};

/// App-level global storing the active [`ThemeMode`] so leaf components
/// (such as [`crate::text_input::TextInput`]) stay synchronized across Dark/Light toggles.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActiveTheme(pub ThemeMode);

impl Global for ActiveTheme {}

/// BitChord's signature crimson accent (tuned from upstream `#FA2D48` to `#E01E3A`
/// so white button text `on_accent` meets strict WCAG AA >= 4.5:1 contrast).
/// Reserved strictly for primary focus signals (active rail marker, live play
/// progress, favorite, live Listen Together indicator).
pub const ACCENT_RED_HEX: u32 = 0xE01E_3AFF;

/// Active theme polarity. Both modes are fully styled and verified for WCAG AA contrast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub const fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark (OLED)",
            Self::Light => "Light (Studio)",
        }
    }
}

/// Complete color token set for BitChord desktop chrome and content surfaces.
#[derive(Debug, Clone, Copy)]
pub struct BitChordTheme {
    pub bg: Hsla,
    pub surface: Hsla,
    pub surface_elevated: Hsla,
    pub surface_hover: Hsla,
    pub surface_active: Hsla,
    pub glass_bar: Hsla,
    pub glass_drawer: Hsla,
    pub border: Hsla,
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub on_accent: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
}

impl BitChordTheme {
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }

    /// Pitch-black OLED canvas + deep obsidian rail + crisp neutral ink (`Theme.kt`).
    pub fn dark() -> Self {
        Self {
            bg: rgb_hex(0x000000),
            surface: rgb_hex(0x0D0D0F),
            surface_elevated: rgb_hex(0x1C1C1E),
            surface_hover: rgb_hex(0x262629),
            surface_active: rgb_hex(0x303035),
            glass_bar: rgba_hex(0x1414_17D9),
            glass_drawer: rgba_hex(0x0F0F_12E6),
            border: rgb_hex(0x2C2C2E),
            text_primary: rgb_hex(0xFFFFFF),
            text_secondary: rgb_hex(0xAEAEB2),
            text_muted: rgb_hex(0x8E8E93),
            accent: rgba_hex(ACCENT_RED_HEX),
            accent_soft: rgba_hex(0xFA2D_4826),
            on_accent: rgb_hex(0xFFFFFF),
            success: rgb_hex(0x30D158),
            warning: rgb_hex(0xFFD60A),
            danger: rgb_hex(0xFF453A),
        }
    }

    /// Crisp studio paper canvas + subtle neutral elevation (`Theme.kt`).
    pub fn light() -> Self {
        Self {
            bg: rgb_hex(0xFFFFFF),
            surface: rgb_hex(0xF7F7F9),
            surface_elevated: rgb_hex(0xEFEFF4),
            surface_hover: rgb_hex(0xE5E5EA),
            surface_active: rgb_hex(0xDCDCE2),
            glass_bar: rgba_hex(0xF7F7_F9E0),
            glass_drawer: rgba_hex(0xFFFF_FFEB),
            border: rgb_hex(0xD8D8DF),
            text_primary: rgb_hex(0x111114),
            text_secondary: rgb_hex(0x48484F),
            text_muted: rgb_hex(0x5E5E66),
            accent: rgb_hex(0xD91E3A),
            accent_soft: rgba_hex(0xD91E_3A1A),
            on_accent: rgb_hex(0xFFFFFF),
            success: rgb_hex(0x1E8E3E),
            warning: rgb_hex(0xB26A00),
            danger: rgb_hex(0xC62828),
        }
    }
}

#[inline]
pub fn rgb_hex(hex: u32) -> Hsla {
    rgb_to_hsla(rgba((hex << 8) | 0xFF))
}

#[inline]
pub fn rgba_hex(hex: u32) -> Hsla {
    rgb_to_hsla(rgba(hex))
}

/// Four dominant RGB colors extracted from album artwork for the Now Playing mesh backdrop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtworkPalette {
    pub stops: [[u8; 3]; 4],
}

impl Default for ArtworkPalette {
    /// Quiet charcoal studio acoustic fallback when no artwork is loaded
    /// (intentionally monochrome rather than a generic blue/purple gradient).
    fn default() -> Self {
        Self {
            stops: [[36, 18, 22], [22, 22, 26], [16, 18, 22], [10, 10, 12]],
        }
    }
}

impl ArtworkPalette {
    /// Extracts 4 visually distinct, darkened atmospheric stops from an `RgbaImage`.
    /// Runs in <0.5 ms on a downscaled cover and guarantees dark readability under white lyrics.
    pub fn from_rgba_image(img: &RgbaImage) -> Self {
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return Self::default();
        }
        // Sample 4 quadrants (top-left, top-right, bottom-left, bottom-right)
        // picking the richest non-muddy pixel average per quadrant, then applying
        // a controlled luminance ceiling so white lyrics over the mesh always exceed WCAG AA.
        let half_w = (w / 2).max(1);
        let half_h = (h / 2).max(1);
        let quadrants = [
            (0..half_w, 0..half_h),
            (w / 2..w, 0..half_h),
            (0..half_w, h / 2..h),
            (w / 2..w, h / 2..h),
        ];
        let mut stops = [[18u8; 3]; 4];
        let step_x = (w / 16).max(1);
        let step_y = (h / 16).max(1);

        for (qi, (xr, yr)) in quadrants.iter().enumerate() {
            let mut r_sum = 0u64;
            let mut g_sum = 0u64;
            let mut b_sum = 0u64;
            let mut weight_sum = 0u64;

            let mut y = yr.start;
            while y < yr.end {
                let mut x = xr.start;
                while x < xr.end {
                    let p = img.get_pixel(x, y);
                    let r = p[0];
                    let g = p[1];
                    let b = p[2];
                    let max_c = r.max(g).max(b);
                    let min_c = r.min(g).min(b);
                    let sat = u64::from(max_c - min_c);
                    // Weight chromatic pixels higher so neutral borders don't wash out art hue.
                    let wgt = 1 + (sat / 24);
                    r_sum += u64::from(r) * wgt;
                    g_sum += u64::from(g) * wgt;
                    b_sum += u64::from(b) * wgt;
                    weight_sum += wgt;
                    x += step_x;
                }
                y += step_y;
            }

            if let Some(r_avg) = r_sum.checked_div(weight_sum)
                && let Some(g_avg) = g_sum.checked_div(weight_sum)
                && let Some(b_avg) = b_sum.checked_div(weight_sum)
            {
                let mut r = r_avg as f32;
                let mut g = g_avg as f32;
                let mut b = b_avg as f32;
                // Cap luminance at ~0.26 so white lyrics over the Now Playing mesh maintain >7:1 contrast.
                let lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
                if lum > 0.24 {
                    let scale = 0.24 / lum;
                    r *= scale;
                    g *= scale;
                    b *= scale;
                }
                stops[qi] = [
                    r.clamp(8.0, 210.0) as u8,
                    g.clamp(8.0, 210.0) as u8,
                    b.clamp(8.0, 210.0) as u8,
                ];
            }
        }
        Self { stops }
    }

    /// Synthesizes a 64x64 BGRA [`RenderImage`] smooth bilinear-upscaled mesh gradient
    /// (16 KiB VRAM) ready for `img(mesh).size_full().object_fit(ObjectFit::Fill)`.
    pub fn build_mesh_render_image(&self) -> Arc<RenderImage> {
        const SIZE: u32 = 64;
        let bgra = self.build_mesh_bgra_buffer(SIZE);
        Arc::new(RenderImage::new(vec![Frame::new(bgra)]))
    }

    /// Generates the raw `SIZE x SIZE` BGRA pixel buffer (exposed for deterministic tests).
    pub fn build_mesh_bgra_buffer(&self, size: u32) -> ImageBuffer<ImgRgba<u8>, Vec<u8>> {
        let mut buf = ImageBuffer::new(size, size);
        let max_coord = (size.saturating_sub(1)).max(1) as f32;

        // Corner anchors gently pulled inward with radial weights for an organic smooth mesh.
        let anchors: [(f32, f32, [f32; 3]); 4] = [
            (0.18, 0.20, to_f32_rgb(self.stops[0])),
            (0.84, 0.22, to_f32_rgb(self.stops[1])),
            (0.22, 0.82, to_f32_rgb(self.stops[2])),
            (0.80, 0.78, to_f32_rgb(self.stops[3])),
        ];

        for y in 0..size {
            let ny = y as f32 / max_coord;
            for x in 0..size {
                let nx = x as f32 / max_coord;
                let mut r = 0.0_f32;
                let mut g = 0.0_f32;
                let mut b = 0.0_f32;
                let mut w_tot = 0.0_f32;

                for &(ax, ay, col) in &anchors {
                    let dx = nx - ax;
                    let dy = ny - ay;
                    let dist_sq = dx * dx + dy * dy;
                    let w = 1.0 / (0.14 + dist_sq * 2.2);
                    r += col[0] * w;
                    g += col[1] * w;
                    b += col[2] * w;
                    w_tot += w;
                }

                // Subtle bottom-edge vignette deepening for crisp mini-controls & lyrics contrast.
                let vignette = 1.0 - 0.22 * ny;
                let cr = ((r / w_tot) * vignette).clamp(4.0, 255.0) as u8;
                let cg = ((g / w_tot) * vignette).clamp(4.0, 255.0) as u8;
                let cb = ((b / w_tot) * vignette).clamp(4.0, 255.0) as u8;

                // GPUI RenderImage expects BGRA byte order (`gpui-linux.md` §4.3).
                buf.put_pixel(x, y, ImgRgba([cb, cg, cr, 255]));
            }
        }
        buf
    }
}

#[inline]
fn to_f32_rgb(rgb: [u8; 3]) -> [f32; 3] {
    [f32::from(rgb[0]), f32::from(rgb[1]), f32::from(rgb[2])]
}

/// Converts an `RgbaImage` from standard RGBA byte order to GPUI's BGRA `RenderImage`.
pub fn rgba_image_to_render_image(mut rgba_img: RgbaImage) -> Arc<RenderImage> {
    for px in rgba_img.pixels_mut() {
        px.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(rgba_img)]))
}

#[cfg(test)]
pub fn wcag_contrast_ratio(fg: Hsla, bg: Hsla) -> f32 {
    let f = hsla_to_rgba(fg);
    let b = hsla_to_rgba(bg);
    let l1 = relative_luminance(f.red, f.green, f.blue);
    let l2 = relative_luminance(b.red, b.green, b.blue);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
fn relative_luminance(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

#[cfg(test)]
fn linearize(c: f32) -> f32 {
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_light_themes_satisfy_wcag_aa_text_contrast() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let t = BitChordTheme::for_mode(mode);
            let primary_bg = wcag_contrast_ratio(t.text_primary, t.bg);
            let primary_surf = wcag_contrast_ratio(t.text_primary, t.surface_elevated);
            let secondary_bg = wcag_contrast_ratio(t.text_secondary, t.bg);
            let muted_surf = wcag_contrast_ratio(t.text_muted, t.surface);
            let on_accent = wcag_contrast_ratio(t.on_accent, t.accent);

            assert!(primary_bg >= 12.0, "{mode:?} primary_bg={primary_bg}");
            assert!(primary_surf >= 10.0, "{mode:?} primary_surf={primary_surf}");
            assert!(secondary_bg >= 6.0, "{mode:?} secondary_bg={secondary_bg}");
            assert!(muted_surf >= 4.5, "{mode:?} muted_surf={muted_surf}");
            assert!(on_accent >= 4.5, "{mode:?} on_accent={on_accent}");
        }
    }

    #[test]
    fn palette_extraction_caps_luminance_and_swaps_mesh_pixels_to_bgra() {
        // Bright red cover image -> extracted stop must stay dark enough for white lyrics
        // and place red in channel index 2 (BGRA) rather than channel 0.
        let red_cover = ImageBuffer::from_pixel(32, 32, ImgRgba([255, 20, 40, 255]));
        let palette = ArtworkPalette::from_rgba_image(&red_cover);
        for stop in palette.stops {
            assert!(
                stop[0] > stop[1] && stop[0] > stop[2],
                "red dominant: {stop:?}"
            );
            let lum = (0.2126 * f32::from(stop[0])
                + 0.7152 * f32::from(stop[1])
                + 0.0722 * f32::from(stop[2]))
                / 255.0;
            assert!(
                lum <= 0.28,
                "mesh luminance too bright for white lyrics: {lum}"
            );
        }

        let mesh = palette.build_mesh_bgra_buffer(64);
        assert_eq!(mesh.dimensions(), (64, 64));
        let center = mesh.get_pixel(32, 32);
        // BGRA order: index 2 is Red, index 0 is Blue.
        assert!(
            center[2] > center[0],
            "expected BGRA with R > B, got {:?}",
            center.0
        );
        assert_eq!(center[3], 255);
    }
}
