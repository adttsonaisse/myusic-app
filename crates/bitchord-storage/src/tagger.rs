//! Container metadata taggers (`FlacTagger`, `Mp4Tagger`, `WebmTagger`),
//! embedded & sidecar lyrics reader (`EmbeddedLyrics`), and general local audio
//! tag scanning via `lofty`.
//!
//! Pure-Rust, `#![forbid(unsafe_code)]` ports of upstream `FlacTagger.kt`,
//! `Mp4Tagger.kt`, `WebmTagger.kt`, `EmbeddedLyrics.kt`, `MediaTaggerTest.kt`,
//! and `EmbeddedLyricsTest.kt`.

use std::fs;
use std::path::Path;

use bitchord_lyrics::{LyricLine, WORD_LYRICS_FIELD, parse_lrc, with_background_vocals};
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use serde::{Deserialize, Serialize};

/// Extensions for which BitChord writes embedded metadata tags directly.
pub fn carries_tags(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "m4a" | "webm" | "flac"
    )
}

// ============================================================================
// FlacTagger
// ============================================================================

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const FLAC_BLOCK_HEADER: usize = 4;
const FLAC_TYPE_STREAMINFO: u8 = 0;
const FLAC_TYPE_PADDING: u8 = 1;
const FLAC_TYPE_VORBIS_COMMENT: u8 = 4;
const FLAC_TYPE_PICTURE: u8 = 6;
const FLAC_MAX_BLOCK_BYTES: usize = (1 << 24) - 1;
const FLAC_PICTURE_FRONT_COVER: u32 = 3;
const FLAC_VENDOR: &str = "BitChord";

/// Replacement metadata region + frame boundary offset for streaming FLAC rewrites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlacHeaderRewrite {
    pub metadata: Vec<u8>,
    pub audio_start: usize,
}

/// Pure-byte FLAC metadata block rewriter (streaming-capable).
pub struct FlacTagger;

impl FlacTagger {
    /// Returns a freshly tagged FLAC byte buffer, or `None` if `bytes` is unrecognized
    /// or there are no metadata fields to write (signaling "leave file untouched").
    #[allow(clippy::too_many_arguments)]
    pub fn tag(
        bytes: &[u8],
        title: &str,
        artist: &str,
        album: Option<&str>,
        lyrics: Option<&str>,
        cover: Option<&[u8]>,
        cover_mime: &str,
        word_lyrics: Option<&str>,
    ) -> Option<Vec<u8>> {
        let done = Self::header(
            bytes,
            title,
            artist,
            album,
            lyrics,
            cover,
            cover_mime,
            word_lyrics,
        )?;
        let mut out = Vec::with_capacity(done.metadata.len() + (bytes.len() - done.audio_start));
        out.extend_from_slice(&done.metadata);
        out.extend_from_slice(&bytes[done.audio_start..]);
        Some(out)
    }

    /// Rewrites only the leading metadata block chain and reports where audio frames start.
    #[allow(clippy::too_many_arguments)]
    pub fn header(
        head: &[u8],
        title: &str,
        artist: &str,
        album: Option<&str>,
        lyrics: Option<&str>,
        cover: Option<&[u8]>,
        cover_mime: &str,
        word_lyrics: Option<&str>,
    ) -> Option<FlacHeaderRewrite> {
        if !head.starts_with(FLAC_MAGIC) {
            return None;
        }

        let mut blocks: Vec<(u8, usize, usize)> = Vec::new();
        let mut offset = FLAC_MAGIC.len();
        loop {
            if offset + FLAC_BLOCK_HEADER > head.len() {
                return None;
            }
            let flags = head[offset];
            let length = ((head[offset + 1] as usize) << 16)
                | ((head[offset + 2] as usize) << 8)
                | (head[offset + 3] as usize);
            let start = offset + FLAC_BLOCK_HEADER;
            if start.checked_add(length)? > head.len() {
                return None;
            }
            blocks.push((flags & 0x7F, start, length));
            offset = start + length;
            if flags & 0x80 != 0 {
                break;
            }
        }

        if blocks.first().map(|b| b.0) != Some(FLAC_TYPE_STREAMINFO) {
            return None;
        }

        let mut additions: Vec<(u8, Vec<u8>)> = Vec::new();
        if let Some(vc) = Self::vorbis_comment(title, artist, album, lyrics, word_lyrics)
            && vc.len() <= FLAC_MAX_BLOCK_BYTES
        {
            additions.push((FLAC_TYPE_VORBIS_COMMENT, vc));
        }
        if let Some(cover_bytes) = cover.filter(|c| !c.is_empty()) {
            let pic = Self::picture(cover_bytes, cover_mime);
            if pic.len() <= FLAC_MAX_BLOCK_BYTES {
                additions.push((FLAC_TYPE_PICTURE, pic));
            }
        }
        if additions.is_empty() {
            return None;
        }

        let mut chain: Vec<(u8, Vec<u8>)> = blocks
            .into_iter()
            .filter(|(ty, _, _)| {
                !matches!(
                    *ty,
                    FLAC_TYPE_PADDING | FLAC_TYPE_VORBIS_COMMENT | FLAC_TYPE_PICTURE
                )
            })
            .map(|(ty, start, len)| (ty, head[start..start + len].to_vec()))
            .collect();
        chain.extend(additions);

        let total_add: usize = chain.iter().map(|(_, p)| 4 + p.len()).sum();
        let mut out = Vec::with_capacity(FLAC_MAGIC.len() + total_add);
        out.extend_from_slice(FLAC_MAGIC);
        let last_idx = chain.len() - 1;
        for (idx, (ty, payload)) in chain.into_iter().enumerate() {
            let len = payload.len();
            out.push(if idx == last_idx { ty | 0x80 } else { ty });
            out.push(((len >> 16) & 0xFF) as u8);
            out.push(((len >> 8) & 0xFF) as u8);
            out.push((len & 0xFF) as u8);
            out.extend_from_slice(&payload);
        }

        Some(FlacHeaderRewrite {
            metadata: out,
            audio_start: offset,
        })
    }

    fn vorbis_comment(
        title: &str,
        artist: &str,
        album: Option<&str>,
        lyrics: Option<&str>,
        word_lyrics: Option<&str>,
    ) -> Option<Vec<u8>> {
        let mut fields = Vec::new();
        if !title.trim().is_empty() {
            fields.push(format!("TITLE={title}"));
        }
        if !artist.trim().is_empty() {
            fields.push(format!("ARTIST={artist}"));
        }
        if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
            fields.push(format!("ALBUM={alb}"));
        }
        if let Some(lyr) = lyrics.filter(|s| !s.trim().is_empty()) {
            fields.push(format!("LYRICS={lyr}"));
        }
        if let Some(wl) = word_lyrics.filter(|s| !s.trim().is_empty()) {
            fields.push(format!("{WORD_LYRICS_FIELD}={wl}"));
        }
        if fields.is_empty() {
            return None;
        }

        let mut out = Vec::new();
        let vendor = FLAC_VENDOR.as_bytes();
        out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        out.extend_from_slice(vendor);
        out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        for field in fields {
            let b = field.as_bytes();
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        Some(out)
    }

    fn picture(cover: &[u8], mime: &str) -> Vec<u8> {
        let mime_bytes = mime.as_bytes();
        let mut out = Vec::with_capacity(cover.len() + mime_bytes.len() + 32);
        out.extend_from_slice(&FLAC_PICTURE_FRONT_COVER.to_be_bytes());
        out.extend_from_slice(&(mime_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(mime_bytes);
        // description length, width, height, color depth, colors used
        for _ in 0..5 {
            out.extend_from_slice(&0u32.to_be_bytes());
        }
        out.extend_from_slice(&(cover.len() as u32).to_be_bytes());
        out.extend_from_slice(cover);
        out
    }
}

// ============================================================================
// Mp4Tagger
// ============================================================================

const MP4_MEAN: &str = "com.music.bitchord";

#[derive(Debug, Clone, Copy)]
struct BoxRef {
    offset: usize,
    header_len: usize,
    size: usize,
    raw_size32: u32,
    box_type: [u8; 4],
}

impl BoxRef {
    fn content_offset(self) -> usize {
        self.offset + self.header_len
    }

    fn end(self) -> usize {
        self.offset + self.size
    }
}

/// Pure-byte MP4/M4A `moov/udta/meta/ilst` tagger with `stco`/`co64` offset patching.
pub struct Mp4Tagger;

impl Mp4Tagger {
    /// Returns a freshly tagged MP4/M4A byte buffer, or `None` if `bytes` has no
    /// recognizable `moov` box or nothing worth writing.
    #[allow(clippy::too_many_arguments)]
    pub fn tag(
        bytes: &[u8],
        title: &str,
        artist: &str,
        album: Option<&str>,
        lyrics: Option<&str>,
        cover: Option<&[u8]>,
        cover_is_png: bool,
        word_lyrics: Option<&str>,
    ) -> Option<Vec<u8>> {
        let mut items = Vec::new();
        if !title.trim().is_empty() {
            items.push(Self::text_item([0xA9, b'n', b'a', b'm'], title));
        }
        if !artist.trim().is_empty() {
            items.push(Self::text_item([0xA9, b'A', b'R', b'T'], artist));
        }
        if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
            items.push(Self::text_item([0xA9, b'a', b'l', b'b'], alb));
        }
        if let Some(lyr) = lyrics.filter(|s| !s.trim().is_empty()) {
            items.push(Self::text_item([0xA9, b'l', b'y', b'r'], lyr));
        }
        if let Some(wl) = word_lyrics.filter(|s| !s.trim().is_empty()) {
            items.push(Self::freeform_item(WORD_LYRICS_FIELD, wl));
        }
        if let Some(img) = cover.filter(|c| !c.is_empty()) {
            items.push(Self::cover_item(img, cover_is_png));
        }
        if items.is_empty() {
            return None;
        }

        let moov = Self::parse_boxes(bytes, 0, bytes.len())
            .into_iter()
            .find(|b| &b.box_type == b"moov")?;

        let udta = Self::udta_atom(&Self::meta_atom(&Self::ilst_atom(&items)));
        Self::insert(bytes, moov, &udta)
    }

    fn insert(bytes: &[u8], moov: BoxRef, udta: &[u8]) -> Option<Vec<u8>> {
        let insert_at = moov.end();
        let delta = udta.len();
        let mut prefix = bytes[..insert_at].to_vec();

        if moov.raw_size32 != 0 {
            let new_size = (moov.size as u64).checked_add(delta as u64)?;
            if moov.header_len == 16 {
                prefix[moov.offset + 8..moov.offset + 16].copy_from_slice(&new_size.to_be_bytes());
            } else {
                let sz32 = u32::try_from(new_size).ok()?;
                prefix[moov.offset..moov.offset + 4].copy_from_slice(&sz32.to_be_bytes());
            }
        }

        let mut offset_boxes = Vec::new();
        Self::collect_offset_boxes(bytes, moov, &mut offset_boxes);
        for b in offset_boxes {
            if &b.box_type == b"stco" {
                Self::patch_stco(&mut prefix, b, insert_at, delta)?;
            } else if &b.box_type == b"co64" {
                Self::patch_co64(&mut prefix, b, insert_at, delta)?;
            }
        }

        let mut out = Vec::with_capacity(bytes.len() + delta);
        out.extend_from_slice(&prefix);
        out.extend_from_slice(udta);
        out.extend_from_slice(&bytes[insert_at..]);
        Some(out)
    }

    fn collect_offset_boxes(bytes: &[u8], b: BoxRef, out: &mut Vec<BoxRef>) {
        if &b.box_type == b"stco" || &b.box_type == b"co64" {
            out.push(b);
            return;
        }
        if matches!(&b.box_type, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl") {
            for child in Self::parse_boxes(bytes, b.content_offset(), b.end()) {
                Self::collect_offset_boxes(bytes, child, out);
            }
        }
    }

    fn patch_stco(bytes: &mut [u8], b: BoxRef, insert_at: usize, delta: usize) -> Option<()> {
        let base = b.content_offset().checked_add(4)?;
        if base + 4 > bytes.len() {
            return None;
        }
        let count = read_be_u32(bytes, base)? as usize;
        let mut p = base + 4;
        for _ in 0..count {
            let off = read_be_u32(bytes, p)? as usize;
            if off >= insert_at {
                let updated = u32::try_from(off.checked_add(delta)?).ok()?;
                bytes[p..p + 4].copy_from_slice(&updated.to_be_bytes());
            }
            p += 4;
        }
        Some(())
    }

    fn patch_co64(bytes: &mut [u8], b: BoxRef, insert_at: usize, delta: usize) -> Option<()> {
        let base = b.content_offset().checked_add(4)?;
        if base + 4 > bytes.len() {
            return None;
        }
        let count = read_be_u32(bytes, base)? as usize;
        let mut p = base + 4;
        for _ in 0..count {
            let off = read_be_u64(bytes, p)?;
            if off >= insert_at as u64 {
                let updated = off.checked_add(delta as u64)?;
                bytes[p..p + 8].copy_from_slice(&updated.to_be_bytes());
            }
            p += 8;
        }
        Some(())
    }

    fn parse_boxes(bytes: &[u8], start: usize, end: usize) -> Vec<BoxRef> {
        let mut out = Vec::new();
        let mut pos = start;
        while pos + 8 <= end {
            let Some(size32) = read_be_u32(bytes, pos) else {
                break;
            };
            let box_type = [
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ];
            let (header_len, size64) = if size32 == 1 {
                if pos + 16 > end {
                    break;
                }
                let Some(s) = read_be_u64(bytes, pos + 8) else {
                    break;
                };
                (16usize, s)
            } else if size32 == 0 {
                (8usize, (end - pos) as u64)
            } else {
                (8usize, size32 as u64)
            };
            let Ok(size) = usize::try_from(size64) else {
                break;
            };
            if size < header_len || pos.saturating_add(size) > end {
                break;
            }
            out.push(BoxRef {
                offset: pos,
                header_len,
                size,
                raw_size32: size32,
                box_type,
            });
            pos += size;
        }
        out
    }

    fn make_box(box_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let total = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(&box_type);
        out.extend_from_slice(payload);
        out
    }

    fn data_atom(type_indicator: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(8 + payload.len());
        body.extend_from_slice(&type_indicator.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(payload);
        Self::make_box(*b"data", &body)
    }

    fn text_item(four_cc: [u8; 4], text: &str) -> Vec<u8> {
        Self::make_box(four_cc, &Self::data_atom(1, text.as_bytes()))
    }

    fn cover_item(image: &[u8], is_png: bool) -> Vec<u8> {
        Self::make_box(
            *b"covr",
            &Self::data_atom(if is_png { 14 } else { 13 }, image),
        )
    }

    fn freeform_item(name: &str, text: &str) -> Vec<u8> {
        let mut mean_body = vec![0u8; 4];
        mean_body.extend_from_slice(MP4_MEAN.as_bytes());
        let mean = Self::make_box(*b"mean", &mean_body);

        let mut name_body = vec![0u8; 4];
        name_body.extend_from_slice(name.as_bytes());
        let label = Self::make_box(*b"name", &name_body);

        let data = Self::data_atom(1, text.as_bytes());
        let mut payload = Vec::with_capacity(mean.len() + label.len() + data.len());
        payload.extend_from_slice(&mean);
        payload.extend_from_slice(&label);
        payload.extend_from_slice(&data);
        Self::make_box(*b"----", &payload)
    }

    fn hdlr_atom() -> Vec<u8> {
        let mut body = vec![0u8; 25];
        body[8..12].copy_from_slice(b"mdir");
        Self::make_box(*b"hdlr", &body)
    }

    fn ilst_atom(items: &[Vec<u8>]) -> Vec<u8> {
        let total: usize = items.iter().map(Vec::len).sum();
        let mut payload = Vec::with_capacity(total);
        for item in items {
            payload.extend_from_slice(item);
        }
        Self::make_box(*b"ilst", &payload)
    }

    fn meta_atom(ilst: &[u8]) -> Vec<u8> {
        let hdlr = Self::hdlr_atom();
        let mut payload = Vec::with_capacity(4 + hdlr.len() + ilst.len());
        payload.extend_from_slice(&[0u8; 4]);
        payload.extend_from_slice(&hdlr);
        payload.extend_from_slice(ilst);
        Self::make_box(*b"meta", &payload)
    }

    fn udta_atom(meta: &[u8]) -> Vec<u8> {
        Self::make_box(*b"udta", meta)
    }
}

// ============================================================================
// WebmTagger
// ============================================================================

const EBML_HEADER_ID: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
const SEGMENT_ID: &[u8] = &[0x18, 0x53, 0x80, 0x67];
const ID_TAGS: &[u8] = &[0x12, 0x54, 0xC3, 0x67];
const ID_TAG: &[u8] = &[0x73, 0x73];
const ID_TARGETS: &[u8] = &[0x63, 0xC0];
const ID_SIMPLETAG: &[u8] = &[0x67, 0xC8];
const ID_TAGNAME: &[u8] = &[0x45, 0xA3];
const ID_TAGSTRING: &[u8] = &[0x44, 0x87];
const ID_ATTACHMENTS: &[u8] = &[0x19, 0x41, 0xA4, 0x69];
const ID_ATTACHEDFILE: &[u8] = &[0x61, 0xA7];
const ID_FILENAME: &[u8] = &[0x46, 0x6E];
const ID_FILEDESCRIPTION: &[u8] = &[0x46, 0x7E];
const ID_FILEMIMETYPE: &[u8] = &[0x46, 0x60];
const ID_FILEDATA: &[u8] = &[0x46, 0x5C];
const ID_FILEUID: &[u8] = &[0x46, 0xAE];

/// Pure-byte WebM/Matroska `Tags` and `Attachments` appender.
pub struct WebmTagger;

impl WebmTagger {
    /// Appends Matroska `Tags`/`Attachments` to a WebM buffer, or returns `None` if
    /// `bytes` is unrecognized or nothing worth writing.
    #[allow(clippy::too_many_arguments)]
    pub fn tag(
        bytes: &[u8],
        title: &str,
        artist: &str,
        album: Option<&str>,
        lyrics: Option<&str>,
        cover: Option<&[u8]>,
        cover_mime: &str,
        word_lyrics: Option<&str>,
    ) -> Option<Vec<u8>> {
        let tail = Self::build_tail(title, artist, album, lyrics, cover, cover_mime, word_lyrics);
        if tail.is_empty() {
            return None;
        }
        Self::insert(bytes, &tail)
    }

    fn insert(bytes: &[u8], tail: &[u8]) -> Option<Vec<u8>> {
        if bytes.len() < 16 || !bytes.starts_with(EBML_HEADER_ID) {
            return None;
        }
        let header_size = read_ebml_size(bytes, EBML_HEADER_ID.len())?;
        let segment_id_offset = EBML_HEADER_ID
            .len()
            .checked_add(header_size.width)?
            .checked_add(usize::try_from(header_size.value).ok()?)?;
        if segment_id_offset + SEGMENT_ID.len() > bytes.len()
            || &bytes[segment_id_offset..segment_id_offset + SEGMENT_ID.len()] != SEGMENT_ID
        {
            return None;
        }
        let segment_size_offset = segment_id_offset + SEGMENT_ID.len();
        let segment_size = read_ebml_size(bytes, segment_size_offset)?;
        let segment_content_start = segment_size_offset + segment_size.width;

        if segment_size.is_unknown {
            let mut out = Vec::with_capacity(bytes.len() + tail.len());
            out.extend_from_slice(bytes);
            out.extend_from_slice(tail);
            return Some(out);
        }

        let declared_end = (segment_content_start as u64).checked_add(segment_size.value)?;
        if declared_end != bytes.len() as u64 {
            return None;
        }
        let new_size = segment_size.value.checked_add(tail.len() as u64)?;
        let max_for_width = (1u64 << (7 * segment_size.width)) - 2;
        if new_size > max_for_width {
            return None;
        }

        let mut out = Vec::with_capacity(bytes.len() + tail.len());
        out.extend_from_slice(bytes);
        out.extend_from_slice(tail);
        write_ebml_vint(&mut out, segment_size_offset, new_size, segment_size.width);
        Some(out)
    }

    fn build_tail(
        title: &str,
        artist: &str,
        album: Option<&str>,
        lyrics: Option<&str>,
        cover: Option<&[u8]>,
        cover_mime: &str,
        word_lyrics: Option<&str>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut simple = Vec::new();
        if !title.trim().is_empty() {
            simple.push(Self::simple_tag("TITLE", title));
        }
        if !artist.trim().is_empty() {
            simple.push(Self::simple_tag("ARTIST", artist));
        }
        if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
            simple.push(Self::simple_tag("ALBUM", alb));
        }
        if let Some(lyr) = lyrics.filter(|s| !s.trim().is_empty()) {
            simple.push(Self::simple_tag("LYRICS", lyr));
        }
        if let Some(wl) = word_lyrics.filter(|s| !s.trim().is_empty()) {
            simple.push(Self::simple_tag(WORD_LYRICS_FIELD, wl));
        }
        if !simple.is_empty() {
            let mut tag_payload = Self::elem(ID_TARGETS, &[]);
            for s in simple {
                tag_payload.extend_from_slice(&s);
            }
            let tag = Self::elem(ID_TAG, &tag_payload);
            out.extend_from_slice(&Self::elem(ID_TAGS, &tag));
        }

        if let Some(img) = cover.filter(|c| !c.is_empty()) {
            let mut attached = Vec::new();
            attached.extend_from_slice(&Self::elem(ID_FILENAME, b"cover.jpg"));
            attached.extend_from_slice(&Self::elem(ID_FILEDESCRIPTION, b"Cover (front)"));
            attached.extend_from_slice(&Self::elem(ID_FILEMIMETYPE, cover_mime.as_bytes()));
            attached.extend_from_slice(&Self::elem(ID_FILEUID, &[0, 0, 0, 0, 0, 0, 0, 1]));
            attached.extend_from_slice(&Self::elem(ID_FILEDATA, img));
            let file_elem = Self::elem(ID_ATTACHEDFILE, &attached);
            out.extend_from_slice(&Self::elem(ID_ATTACHMENTS, &file_elem));
        }

        out
    }

    fn simple_tag(name: &str, value: &str) -> Vec<u8> {
        let mut body = Self::elem(ID_TAGNAME, name.as_bytes());
        body.extend_from_slice(&Self::elem(ID_TAGSTRING, value.as_bytes()));
        Self::elem(ID_SIMPLETAG, &body)
    }

    fn elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let vint = encode_ebml_vint(payload.len() as u64);
        let mut out = Vec::with_capacity(id.len() + vint.len() + payload.len());
        out.extend_from_slice(id);
        out.extend_from_slice(&vint);
        out.extend_from_slice(payload);
        out
    }
}

struct EbmlSize {
    value: u64,
    width: usize,
    is_unknown: bool,
}

fn read_ebml_size(bytes: &[u8], offset: usize) -> Option<EbmlSize> {
    let first = *bytes.get(offset)?;
    if first == 0 {
        return None;
    }
    let mut width = 1usize;
    let mut mask = 0x80u8;
    while mask != 0 && (first & mask) == 0 {
        mask >>= 1;
        width += 1;
    }
    if mask == 0 || offset + width > bytes.len() {
        return None;
    }
    let mut value = (first & (mask - 1)) as u64;
    for i in 1..width {
        value = (value << 8) | (bytes[offset + i] as u64);
    }
    let max_val = (1u64 << (7 * width)) - 1;
    Some(EbmlSize {
        value,
        width,
        is_unknown: value == max_val,
    })
}

fn encode_ebml_vint(value: u64) -> Vec<u8> {
    let mut width = 1usize;
    while width < 8 && value > (1u64 << (7 * width)) - 2 {
        width += 1;
    }
    let mut buf = vec![0u8; width];
    write_ebml_vint(&mut buf, 0, value, width);
    buf
}

fn write_ebml_vint(bytes: &mut [u8], offset: usize, mut value: u64, width: usize) {
    for i in (0..width).rev() {
        bytes[offset + i] = (value & 0xFF) as u8;
        value >>= 8;
    }
    bytes[offset] |= 0x80u8 >> (width - 1);
}

// ============================================================================
// EmbeddedLyrics
// ============================================================================

const MAX_TAG_BYTES: usize = 8 * 1024 * 1024;
const LYR_ATOM: &[u8] = &[0xA9, b'l', b'y', b'r'];

/// Reader for offline lyrics embedded in FLAC/MP4/WebM files or `lyrics.lrc`
/// sidecars next to offline HLS/DASH `.m3u8` packages.
pub struct EmbeddedLyrics;

impl EmbeddedLyrics {
    /// Reads and parses embedded or sidecar lyrics for `path` into [`LyricLine`]s
    /// (with background vocals split identically to network sources).
    pub fn for_path(path: &Path) -> Option<Vec<LyricLine>> {
        let raw = Self::sidecar(Some(path)).or_else(|| {
            let bytes = read_at_most(path, MAX_TAG_BYTES).ok()?;
            Self::from_bytes(&bytes)
        })?;
        let lines = parse_lrc(&raw);
        if !lines.iter().any(|l| !l.text.trim().is_empty()) {
            return None;
        }
        Some(with_background_vocals(lines))
    }

    /// Reads the `lyrics.lrc` sidecar sitting next to an offline `.m3u8` playlist.
    pub fn sidecar(path: Option<&Path>) -> Option<String> {
        let playlist = path?;
        let name = playlist.file_name()?.to_str()?;
        if !name.to_ascii_lowercase().ends_with(".m3u8") {
            return None;
        }
        let sidecar = playlist.parent()?.join("lyrics.lrc");
        let meta = fs::metadata(&sidecar).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_TAG_BYTES as u64 {
            return None;
        }
        let text = fs::read_to_string(&sidecar).ok()?;
        (!text.trim().is_empty()).then_some(text)
    }

    /// Extracts raw LRC text from FLAC, Matroska/WebM, or MP4/M4A bytes, preferring
    /// `BITCHORD_LYRICS` (word-synced A2) over portable `LYRICS`/`©lyr`.
    pub fn from_bytes(head: &[u8]) -> Option<String> {
        let found = if head.starts_with(FLAC_MAGIC) {
            Self::flac(head)
        } else if head.starts_with(EBML_HEADER_ID) {
            Self::matroska(head)
        } else if is_mp4(head) {
            Self::mp4(head)
        } else {
            None
        };
        found.filter(|s| !s.trim().is_empty())
    }

    fn mp4(bytes: &[u8]) -> Option<String> {
        let (start, end) = top_level_mp4_box(bytes, b"moov")?;
        Self::ilst_text(bytes, start, end, true)
            .or_else(|| Self::ilst_text(bytes, start, end, false))
    }

    fn ilst_text(bytes: &[u8], from: usize, end: usize, freeform: bool) -> Option<String> {
        let marker = if freeform {
            WORD_LYRICS_FIELD.as_bytes()
        } else {
            LYR_ATOM
        };
        let mut at = from;
        while let Some(found) = index_of_slice(bytes, marker, at, end) {
            let data = index_of_slice(bytes, b"data", found, end)?;
            if let Some(text) = Self::data_text(bytes, data, end) {
                return Some(text);
            }
            at = found + marker.len();
        }
        None
    }

    fn data_text(bytes: &[u8], data_at: usize, end: usize) -> Option<String> {
        let start = data_at.checked_sub(4)?;
        if data_at + 12 > end {
            return None;
        }
        let size = read_be_u32(bytes, start)? as usize;
        if size <= 16 || start.checked_add(size)? > end {
            return None;
        }
        if read_be_u32(bytes, data_at + 4)? != 1 {
            return None;
        }
        String::from_utf8(bytes[data_at + 12..start + size].to_vec()).ok()
    }

    fn flac(bytes: &[u8]) -> Option<String> {
        let mut pos = FLAC_MAGIC.len();
        while pos + 4 <= bytes.len() {
            let flags = bytes[pos];
            let length = ((bytes[pos + 1] as usize) << 16)
                | ((bytes[pos + 2] as usize) << 8)
                | (bytes[pos + 3] as usize);
            let start = pos + 4;
            if start.checked_add(length)? > bytes.len() {
                return None;
            }
            if flags & 0x7F == FLAC_TYPE_VORBIS_COMMENT {
                return Self::vorbis_comment(bytes, start, start + length);
            }
            if flags & 0x80 != 0 {
                return None;
            }
            pos = start + length;
        }
        None
    }

    fn vorbis_comment(bytes: &[u8], start: usize, end: usize) -> Option<String> {
        let mut pos = start;
        let vendor = read_le_u32(bytes, &mut pos, end)? as usize;
        pos = pos.checked_add(vendor)?;
        if pos > end {
            return None;
        }
        let count = (read_le_u32(bytes, &mut pos, end)? as usize).min(4_096);
        let mut plain: Option<String> = None;
        for _ in 0..count {
            let Some(len) = read_le_u32(bytes, &mut pos, end).map(|v| v as usize) else {
                return plain;
            };
            if pos.checked_add(len).is_none_or(|e| e > end) {
                return plain;
            }
            let entry = String::from_utf8_lossy(&bytes[pos..pos + len]);
            pos += len;
            if let Some((raw_name, value)) = entry.split_once('=') {
                if raw_name.eq_ignore_ascii_case(WORD_LYRICS_FIELD) && !value.trim().is_empty() {
                    return Some(value.to_owned());
                }
                if raw_name.eq_ignore_ascii_case("LYRICS")
                    && plain.is_none()
                    && !value.trim().is_empty()
                {
                    plain = Some(value.to_owned());
                }
            }
        }
        plain
    }

    fn matroska(bytes: &[u8]) -> Option<String> {
        let mut plain: Option<String> = None;
        for name in [WORD_LYRICS_FIELD, "LYRICS"] {
            let needle = name.as_bytes();
            let mut from = 0usize;
            while let Some(at) = index_of_slice(bytes, needle, from, bytes.len()) {
                from = at + needle.len();
                if at < 3 || bytes[at - 3] != ID_TAGNAME[0] || bytes[at - 2] != ID_TAGNAME[1] {
                    continue;
                }
                if (bytes[at - 1] & 0x7F) as usize != needle.len() {
                    continue;
                }
                let Some(string_pos) = index_of_slice(bytes, ID_TAGSTRING, from, bytes.len())
                else {
                    continue;
                };
                let Some(size) = read_ebml_size(bytes, string_pos + 2) else {
                    continue;
                };
                let value_at = string_pos + 2 + size.width;
                let Ok(len) = usize::try_from(size.value) else {
                    continue;
                };
                if len == 0 || value_at.checked_add(len).is_none_or(|e| e > bytes.len()) {
                    continue;
                }
                let Ok(value) = String::from_utf8(bytes[value_at..value_at + len].to_vec()) else {
                    continue;
                };
                if value.trim().is_empty() {
                    continue;
                }
                if name == WORD_LYRICS_FIELD {
                    return Some(value);
                }
                if plain.is_none() {
                    plain = Some(value);
                }
            }
        }
        plain
    }
}

// ============================================================================
// LocalAudioTags (lofty)
// ============================================================================

/// Audio metadata and stream properties read from an arbitrary local file via `lofty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAudioTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub audio_bitrate_kbps: Option<u32>,
}

/// Reads standard metadata tags and audio properties from a local audio file using `lofty`.
pub fn read_local_audio_tags(path: &Path) -> Option<LocalAudioTags> {
    let tagged = lofty::read_from_path(path).ok()?;
    let props = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    Some(LocalAudioTags {
        title: tag.and_then(|t| t.title().map(|s| s.into_owned())),
        artist: tag.and_then(|t| t.artist().map(|s| s.into_owned())),
        album: tag.and_then(|t| t.album().map(|s| s.into_owned())),
        duration_ms: props.duration().as_millis() as u64,
        sample_rate_hz: props.sample_rate(),
        bit_depth: props.bit_depth(),
        channels: props.channels(),
        audio_bitrate_kbps: props.audio_bitrate(),
    })
}

// ============================================================================
// Byte helpers
// ============================================================================

fn read_at_most(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut out = Vec::new();
    file.take(max as u64).read_to_end(&mut out)?;
    Ok(out)
}

fn is_mp4(bytes: &[u8]) -> bool {
    bytes.len() > 12 && &bytes[4..8] == b"ftyp"
}

fn top_level_mp4_box(bytes: &[u8], target: &[u8; 4]) -> Option<(usize, usize)> {
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let declared = read_be_u32(bytes, pos)?;
        let (header_len, size64) = if declared == 1 {
            if pos + 16 > bytes.len() {
                return None;
            }
            (16usize, read_be_u64(bytes, pos + 8)?)
        } else if declared == 0 {
            (8usize, (bytes.len() - pos) as u64)
        } else {
            (8usize, declared as u64)
        };
        let size = usize::try_from(size64).ok()?;
        if size < header_len {
            return None;
        }
        let end = pos.saturating_add(size).min(bytes.len());
        if &bytes[pos + 4..pos + 8] == target {
            return Some((pos, end));
        }
        pos = pos.checked_add(size)?;
    }
    None
}

fn index_of_slice(haystack: &[u8], needle: &[u8], from: usize, until: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let bound = until.min(haystack.len());
    if from >= bound || bound - from < needle.len() {
        return None;
    }
    haystack[from..bound]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|rel| from + rel)
}

fn read_be_u32(b: &[u8], off: usize) -> Option<u32> {
    let slice: [u8; 4] = b.get(off..off + 4)?.try_into().ok()?;
    Some(u32::from_be_bytes(slice))
}

fn read_be_u64(b: &[u8], off: usize) -> Option<u64> {
    let slice: [u8; 8] = b.get(off..off + 8)?.try_into().ok()?;
    Some(u64::from_be_bytes(slice))
}

fn read_le_u32(b: &[u8], pos: &mut usize, end: usize) -> Option<u32> {
    if *pos + 4 > end {
        return None;
    }
    let slice: [u8; 4] = b[*pos..*pos + 4].try_into().ok()?;
    *pos += 4;
    Some(u32::from_le_bytes(slice))
}

#[cfg(test)]
mod tests {
    use bitchord_lyrics::{LyricWord, to_enhanced_lrc, to_lrc};
    use uuid::Uuid;

    use super::*;

    const SAMPLE_LRC: &str = "[00:01.20]first line here\n[00:04.50]second line here";

    fn mp4_box(ty: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&ty);
        out.extend_from_slice(payload);
        out
    }

    fn build_fake_mp4(mdat_payload: &[u8]) -> (Vec<u8>, usize) {
        let ftyp = mp4_box(*b"ftyp", &[0u8; 8]);
        let build_moov = |offset: u32| {
            let mut stco_payload = vec![0u8; 12];
            stco_payload[7] = 1;
            stco_payload[8..12].copy_from_slice(&offset.to_be_bytes());
            let stco = mp4_box(*b"stco", &stco_payload);
            let stbl = mp4_box(*b"stbl", &stco);
            let minf = mp4_box(*b"minf", &stbl);
            let mdia = mp4_box(*b"mdia", &minf);
            let trak = mp4_box(*b"trak", &mdia);
            mp4_box(*b"moov", &trak)
        };
        let placeholder = build_moov(0);
        let mdat_payload_offset = ftyp.len() + placeholder.len() + 8;
        let moov = build_moov(mdat_payload_offset as u32);
        let mdat = mp4_box(*b"mdat", mdat_payload);
        let mut all = Vec::new();
        all.extend_from_slice(&ftyp);
        all.extend_from_slice(&moov);
        all.extend_from_slice(&mdat);
        (all, mdat_payload_offset)
    }

    fn minimal_mp4() -> Vec<u8> {
        let mut out = mp4_box(*b"ftyp", b"isom");
        out.extend_from_slice(&mp4_box(*b"moov", &[]));
        out
    }

    fn flac_block(ty: u8, payload: &[u8], last: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.push(if last { ty | 0x80 } else { ty });
        out.push(((payload.len() >> 16) & 0xFF) as u8);
        out.push(((payload.len() >> 8) & 0xFF) as u8);
        out.push((payload.len() & 0xFF) as u8);
        out.extend_from_slice(payload);
        out
    }

    fn minimal_flac() -> Vec<u8> {
        let mut out = FLAC_MAGIC.to_vec();
        out.extend_from_slice(&flac_block(FLAC_TYPE_STREAMINFO, &[0u8; 34], true));
        out.push(0xFF);
        out
    }

    fn flac_chain(bytes: &[u8]) -> (Vec<(u8, Vec<u8>)>, usize) {
        assert_eq!(&bytes[0..4], FLAC_MAGIC);
        let mut blocks = Vec::new();
        let mut offset = 4usize;
        loop {
            let flags = bytes[offset];
            let length = ((bytes[offset + 1] as usize) << 16)
                | ((bytes[offset + 2] as usize) << 8)
                | (bytes[offset + 3] as usize);
            blocks.push((
                flags & 0x7F,
                bytes[offset + 4..offset + 4 + length].to_vec(),
            ));
            offset += 4 + length;
            if flags & 0x80 != 0 {
                return (blocks, offset);
            }
        }
    }

    fn build_fake_webm(segment_body: &[u8], segment_size: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(EBML_HEADER_ID);
        out.push(0x84);
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(SEGMENT_ID);
        out.extend_from_slice(segment_size);
        out.extend_from_slice(segment_body);
        out
    }

    fn minimal_webm() -> Vec<u8> {
        let body = [0u8; 8];
        build_fake_webm(&body, &[0x01, 0, 0, 0, 0, 0, 0, body.len() as u8])
    }

    // ---- MediaTaggerTest.kt ports -------------------------------------------

    #[test]
    fn mp4_tagging_preserves_mdat_and_repoints_stco() {
        let mdat_payload: Vec<u8> = (1u8..=24).collect();
        let (original, mdat_offset) = build_fake_mp4(&mdat_payload);
        let cover = [9u8, 8, 7, 6, 5];

        let tagged = Mp4Tagger::tag(
            &original,
            "My Title",
            "My Artist",
            Some("My Album"),
            None,
            Some(&cover),
            false,
            None,
        )
        .expect("tagged mp4");
        let delta = tagged.len() - original.len();
        assert!(delta > 0);
        let new_offset = mdat_offset + delta;
        assert_eq!(
            &tagged[new_offset..new_offset + mdat_payload.len()],
            &mdat_payload
        );

        let stco_pos = index_of_slice(&tagged, b"stco", 0, tagged.len()).expect("stco");
        assert_eq!(read_be_u32(&tagged, stco_pos + 12), Some(new_offset as u32));
        assert!(index_of_slice(&tagged, b"My Title", 0, tagged.len()).is_some());
        assert!(index_of_slice(&tagged, b"My Artist", 0, tagged.len()).is_some());
        assert!(index_of_slice(&tagged, b"My Album", 0, tagged.len()).is_some());
        assert!(index_of_slice(&tagged, &cover, 0, tagged.len()).is_some());
    }

    #[test]
    fn mp4_tagging_noops_and_lyrics_only() {
        let mut no_moov = mp4_box(*b"ftyp", &[0u8; 8]);
        no_moov.extend_from_slice(&mp4_box(*b"mdat", &[0u8; 16]));
        assert!(
            Mp4Tagger::tag(&no_moov, "Title", "Artist", None, None, None, false, None).is_none()
        );

        let (original, _) = build_fake_mp4(&[0u8; 4]);
        assert!(Mp4Tagger::tag(&original, "", "", None, None, None, false, None).is_none());

        let lyrics_only =
            Mp4Tagger::tag(&original, "", "", None, Some(SAMPLE_LRC), None, false, None)
                .expect("lyrics only");
        assert!(index_of_slice(&lyrics_only, LYR_ATOM, 0, lyrics_only.len()).is_some());
        assert!(
            index_of_slice(&lyrics_only, SAMPLE_LRC.as_bytes(), 0, lyrics_only.len()).is_some()
        );
    }

    #[test]
    fn webm_tagging_handles_unknown_and_definite_segment_sizes() {
        let unknown = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let body: Vec<u8> = (1u8..=10).collect();
        let original = build_fake_webm(&body, &unknown);
        let cover = [3u8, 1, 4, 1, 5];
        let tagged = WebmTagger::tag(
            &original,
            "T",
            "A",
            Some("Al"),
            None,
            Some(&cover),
            "image/jpeg",
            None,
        )
        .expect("tagged webm");
        assert_eq!(&tagged[..original.len()], &original);
        assert!(index_of_slice(&tagged, b"Cover (front)", 0, tagged.len()).is_some());

        // Definite 2-byte vint widens in place.
        let body2: Vec<u8> = (0u8..20).collect();
        let size_field = [0x40u8, body2.len() as u8];
        let original2 = build_fake_webm(&body2, &size_field);
        let tagged2 = WebmTagger::tag(&original2, "T", "", None, None, None, "image/jpeg", None)
            .expect("widened webm");
        let size_off = EBML_HEADER_ID.len() + 1 + 4 + SEGMENT_ID.len();
        let decoded =
            (((tagged2[size_off] as usize) & 0x3F) << 8) | (tagged2[size_off + 1] as usize);
        assert_eq!(decoded, tagged2.len() - (size_off + 2));

        // Unrecognized EBML header returns None.
        assert!(
            WebmTagger::tag(&[0u8; 20], "T", "A", None, None, None, "image/jpeg", None).is_none()
        );

        // Lyrics alone appends SimpleTag.
        let lyr = WebmTagger::tag(
            &original,
            "",
            "",
            None,
            Some(SAMPLE_LRC),
            None,
            "image/jpeg",
            None,
        )
        .expect("lyrics webm");
        assert!(index_of_slice(&lyr, b"LYRICS", 0, lyr.len()).is_some());
    }

    #[test]
    fn flac_tagging_streaming_header_matches_whole_and_replaces_old_comments() {
        let stream_info: Vec<u8> = (0u8..34).collect();
        let seek_table: Vec<u8> = (100u8..118).collect();
        let frames: Vec<u8> = (1u8..=64).collect();
        let mut original = FLAC_MAGIC.to_vec();
        original.extend_from_slice(&flac_block(FLAC_TYPE_STREAMINFO, &stream_info, false));
        original.extend_from_slice(&flac_block(3, &seek_table, false));
        original.extend_from_slice(&flac_block(FLAC_TYPE_PADDING, &[0u8; 200], true));
        original.extend_from_slice(&frames);
        let cover = [9u8, 8, 7, 6, 5];

        let whole = FlacTagger::tag(
            &original,
            "My Title",
            "My Artist",
            Some("My Album"),
            Some(SAMPLE_LRC),
            Some(&cover),
            "image/jpeg",
            None,
        )
        .expect("whole flac");
        let rewrite = FlacTagger::header(
            &original,
            "My Title",
            "My Artist",
            Some("My Album"),
            Some(SAMPLE_LRC),
            Some(&cover),
            "image/jpeg",
            None,
        )
        .expect("header rewrite");

        let mut streamed = rewrite.metadata.clone();
        streamed.extend_from_slice(&original[rewrite.audio_start..]);
        assert_eq!(whole, streamed);
        assert_eq!(&original[rewrite.audio_start..], &frames);

        let (blocks, frames_at) = flac_chain(&whole);
        assert_eq!(
            blocks.iter().map(|b| b.0).collect::<Vec<_>>(),
            vec![
                FLAC_TYPE_STREAMINFO,
                3,
                FLAC_TYPE_VORBIS_COMMENT,
                FLAC_TYPE_PICTURE
            ]
        );
        assert_eq!(&whole[frames_at..], &frames);

        // Short prefix cut mid-block returns None.
        assert!(
            FlacTagger::header(
                &original[..50],
                "T",
                "A",
                None,
                None,
                None,
                "image/jpeg",
                None
            )
            .is_none()
        );
        // No fLaC magic returns None.
        assert!(
            FlacTagger::tag(&[0u8; 64], "T", "A", None, None, None, "image/jpeg", None).is_none()
        );
        // Nothing to write returns None.
        assert!(
            FlacTagger::tag(
                &minimal_flac(),
                "  ",
                "",
                None,
                None,
                None,
                "image/jpeg",
                None
            )
            .is_none()
        );
    }

    // ---- EmbeddedLyricsTest.kt ports ----------------------------------------

    #[test]
    fn embedded_lyrics_round_trips_plain_and_word_timings_in_all_three_containers() {
        let lrc = "[00:01.00]first line\n[00:05.00]second line";

        let m4a = Mp4Tagger::tag(
            &minimal_mp4(),
            "t",
            "a",
            None,
            Some(lrc),
            Some(&[0x7F; 64]),
            false,
            None,
        )
        .expect("m4a");
        assert_eq!(EmbeddedLyrics::from_bytes(&m4a).as_deref(), Some(lrc));

        let flac = FlacTagger::tag(
            &minimal_flac(),
            "t",
            "a",
            None,
            Some(lrc),
            None,
            "image/jpeg",
            None,
        )
        .expect("flac");
        assert_eq!(EmbeddedLyrics::from_bytes(&flac).as_deref(), Some(lrc));

        let webm = WebmTagger::tag(
            &minimal_webm(),
            "t",
            "a",
            None,
            Some(lrc),
            None,
            "image/jpeg",
            None,
        )
        .expect("webm");
        assert_eq!(EmbeddedLyrics::from_bytes(&webm).as_deref(), Some(lrc));

        // Word-timed A2 lyrics survive and beat plain lyrics in all three containers.
        let words = vec![LyricLine {
            time_ms: 1_000,
            text: "two words".to_owned(),
            words: vec![
                LyricWord::new(1_000, 1_400, "two"),
                LyricWord::new(1_400, 2_000, "words"),
            ],
            ..LyricLine::default()
        }];
        let plain = to_lrc(&words);
        let enhanced = to_enhanced_lrc(&words);
        assert_eq!(plain, "[00:01.00]two words");

        let containers = [
            Mp4Tagger::tag(
                &minimal_mp4(),
                "t",
                "a",
                None,
                Some(&plain),
                None,
                false,
                Some(&enhanced),
            )
            .unwrap(),
            FlacTagger::tag(
                &minimal_flac(),
                "t",
                "a",
                None,
                Some(&plain),
                None,
                "image/jpeg",
                Some(&enhanced),
            )
            .unwrap(),
            WebmTagger::tag(
                &minimal_webm(),
                "t",
                "a",
                None,
                Some(&plain),
                None,
                "image/jpeg",
                Some(&enhanced),
            )
            .unwrap(),
        ];
        for bytes in containers {
            let raw = EmbeddedLyrics::from_bytes(&bytes).expect("embedded word lyrics");
            let parsed = parse_lrc(&raw);
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].text, "two words");
            assert_eq!(
                parsed[0]
                    .words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["two", "words"]
            );
            assert_eq!(
                parsed[0]
                    .words
                    .iter()
                    .map(|w| w.start_ms)
                    .collect::<Vec<_>>(),
                vec![1_000, 1_400]
            );
            assert_eq!(parsed[0].end_ms(), 2_000);
        }

        // No lyrics -> None; random bytes -> None.
        let no_lyr =
            Mp4Tagger::tag(&minimal_mp4(), "t", "a", None, None, None, false, None).unwrap();
        assert_eq!(EmbeddedLyrics::from_bytes(&no_lyr), None);
        assert_eq!(EmbeddedLyrics::from_bytes(&[42u8; 512]), None);
    }

    #[test]
    fn embedded_lyrics_reads_m3u8_sidecar_and_ignores_blank_or_ordinary_files() {
        let dir = std::env::temp_dir().join(format!("bitchord-hls-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("mkdir");
        let playlist = dir.join("playlist.m3u8");
        fs::write(&playlist, "#EXTM3U\n#EXT-X-ENDLIST\n").expect("write m3u8");
        assert_eq!(EmbeddedLyrics::sidecar(Some(&playlist)), None);

        let sidecar = dir.join("lyrics.lrc");
        fs::write(&sidecar, "  \n").expect("write blank");
        assert_eq!(EmbeddedLyrics::sidecar(Some(&playlist)), None);

        fs::write(&sidecar, SAMPLE_LRC).expect("write lrc");
        assert_eq!(
            EmbeddedLyrics::sidecar(Some(&playlist)).as_deref(),
            Some(SAMPLE_LRC)
        );
        assert_eq!(EmbeddedLyrics::sidecar(Some(&dir.join("song.flac"))), None);
        assert_eq!(EmbeddedLyrics::sidecar(None), None);

        let _ = fs::remove_dir_all(&dir);
    }
}
