use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::SystemTime;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use unicode_script::{Script, UnicodeScript};

use crate::http::LyricsHttp;
use crate::{LyricLine, LyricWord, is_genius_section_header};

const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";
const INPUT_TOOLS_ENDPOINT: &str = "https://inputtools.google.com/request";
const TRANSLATE_USER_AGENT: &str = "BitChord/1.7.1";
const CACHE_VERSION: u32 = 3;
const CACHE_DIRECTORY: &str = "lyrics_translation_v3";
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MEMORY_ENTRIES: usize = 12;
const MAX_BATCH_CHARS: usize = 3_500;
const MAX_PARALLEL_REQUESTS: usize = 2;

static MARKER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\u{E000}[^\u{E001}]*\u{E001}")
        // INFALLIBLE: static regex pattern.
        .expect("valid MARKER_REGEX")
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+")
        // INFALLIBLE: static regex pattern.
        .expect("valid WHITESPACE regex")
});

/// Result of [`LyricsTranslator::translate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationResult {
    Translated {
        lines: Vec<LyricLine>,
        source_language: String,
        from_cache: bool,
    },
    SameLanguage {
        language: String,
    },
    Unavailable,
}

/// Result of [`LyricsTranslator::romanize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RomanizationResult {
    Romanized {
        lines: Vec<LyricLine>,
        source_language: String,
        from_cache: bool,
    },
    AlreadyRomanized,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedTranslation {
    #[serde(default = "default_cache_version")]
    version: u32,
    source_language: String,
    target_language: String,
    texts: Vec<String>,
}

fn default_cache_version() -> u32 {
    CACHE_VERSION
}

#[derive(Debug, Clone)]
struct TextSlot {
    line_index: usize,
    background: bool,
    text: String,
    section_header: bool,
}

#[derive(Debug, Clone)]
struct Batch {
    slots: Vec<TextSlot>,
    payload: String,
}

#[derive(Debug, Clone)]
struct BatchAnswer {
    translations: Vec<String>,
    source_language: String,
    source_weight: usize,
}

#[derive(Debug, Default)]
struct MemoryLru {
    entries: HashMap<String, CachedTranslation>,
    order: VecDeque<String>,
}

impl MemoryLru {
    fn get(&mut self, key: &str) -> Option<CachedTranslation> {
        let val = self.entries.get(key)?.clone();
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_owned());
        Some(val)
    }

    fn insert(&mut self, key: String, value: CachedTranslation) {
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        } else if self.entries.len() >= MAX_MEMORY_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }
}

/// Lightweight, on-demand lyric translation and romanization with bounded
/// in-memory and gzipped disk caching.
pub struct LyricsTranslator {
    http: LyricsHttp,
    cache_dir: Option<PathBuf>,
    translate_endpoint: String,
    input_tools_endpoint: String,
    memory: StdMutex<MemoryLru>,
    disk_mutex: Mutex<()>,
}

impl Default for LyricsTranslator {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LyricsTranslator {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            http: LyricsHttp::new(),
            cache_dir,
            translate_endpoint: ENDPOINT.to_owned(),
            input_tools_endpoint: INPUT_TOOLS_ENDPOINT.to_owned(),
            memory: StdMutex::new(MemoryLru::default()),
            disk_mutex: Mutex::new(()),
        }
    }

    pub fn with_http(mut self, http: LyricsHttp) -> Self {
        self.http = http;
        self
    }

    #[cfg(test)]
    fn with_endpoints(mut self, translate: String, input_tools: String) -> Self {
        self.translate_endpoint = translate;
        self.input_tools_endpoint = input_tools;
        self
    }

    pub async fn translate(
        &self,
        track_id: &str,
        lines: &[LyricLine],
        target_language_tag: &str,
    ) -> TranslationResult {
        let target = target_language_tag.trim();
        if target.is_empty() || lines.is_empty() {
            return TranslationResult::Unavailable;
        }

        let slots = flatten(lines);
        if slots.is_empty() {
            return TranslationResult::Unavailable;
        }

        let key = cache_key(track_id, target, &slots);
        if let Some(cached) = self.load_cached(&key).await
            && cached.version == CACHE_VERSION
            && cached.texts.len() == slots.len()
        {
            return if same_language(&cached.source_language, target) {
                TranslationResult::SameLanguage {
                    language: cached.source_language,
                }
            } else {
                TranslationResult::Translated {
                    lines: rebuild(lines, &slots, &cached.texts),
                    source_language: cached.source_language,
                    from_cache: true,
                }
            };
        }

        let batches = make_batches(&slots);
        let Some(complete) = self.run_translate_batches(&batches, target, "auto").await else {
            return TranslationResult::Unavailable;
        };

        let source = dominant_source_language(&complete);
        if source.trim().is_empty() {
            return TranslationResult::Unavailable;
        }

        let mut translated: Vec<String> =
            complete.into_iter().flat_map(|b| b.translations).collect();
        if translated.len() != slots.len() {
            return TranslationResult::Unavailable;
        }

        let total_chars: usize = slots.iter().map(|s| s.text.chars().count()).sum();
        if !same_language(&source, target)
            && predominantly_latin(&slots)
            && unchanged_weight(&slots, &translated).saturating_mul(3) >= total_chars
            && let Some(retried) = self
                .retry_romanized_translation(&batches, &source, target)
                .await
            && retried.len() == slots.len()
            && unchanged_weight(&slots, &retried) < unchanged_weight(&slots, &translated)
        {
            translated = retried;
        }

        let entry = CachedTranslation {
            version: CACHE_VERSION,
            source_language: source.clone(),
            target_language: target.to_owned(),
            texts: translated.clone(),
        };
        self.store_cached(key, entry).await;

        if same_language(&source, target) {
            TranslationResult::SameLanguage { language: source }
        } else {
            TranslationResult::Translated {
                lines: rebuild(lines, &slots, &translated),
                source_language: source,
                from_cache: false,
            }
        }
    }

    pub async fn romanize(
        &self,
        track_id: &str,
        lines: &[LyricLine],
        target_language_tag: &str,
    ) -> RomanizationResult {
        if lines.is_empty() {
            return RomanizationResult::Unavailable;
        }
        let slots = flatten(lines);
        if slots.is_empty() {
            return RomanizationResult::Unavailable;
        }
        if !has_non_latin_letters(&slots) {
            return RomanizationResult::AlreadyRomanized;
        }

        let key = cache_key(track_id, "romanize", &slots);
        if let Some(cached) = self.load_cached(&key).await
            && cached.version == CACHE_VERSION
            && cached.texts.len() == slots.len()
        {
            return RomanizationResult::Romanized {
                lines: rebuild(lines, &slots, &cached.texts),
                source_language: cached.source_language,
                from_cache: true,
            };
        }

        let target = {
            let t = target_language_tag.trim();
            if t.is_empty() { "en" } else { t }
        };
        let batches = make_batches(&slots);
        let Some(complete) = self.run_romanization_batches(&batches, target).await else {
            return RomanizationResult::Unavailable;
        };

        let source = dominant_source_language(&complete);
        let romanized: Vec<String> = complete.into_iter().flat_map(|b| b.translations).collect();
        if source.trim().is_empty() || romanized.len() != slots.len() {
            return RomanizationResult::Unavailable;
        }
        let total_chars: usize = slots.iter().map(|s| s.text.chars().count()).sum();
        if unchanged_weight(&slots, &romanized) == total_chars {
            return RomanizationResult::Unavailable;
        }

        let entry = CachedTranslation {
            version: CACHE_VERSION,
            source_language: source.clone(),
            target_language: "Latn".to_owned(),
            texts: romanized.clone(),
        };
        self.store_cached(key, entry).await;

        RomanizationResult::Romanized {
            lines: rebuild(lines, &slots, &romanized),
            source_language: source,
            from_cache: false,
        }
    }

    async fn run_translate_batches(
        &self,
        batches: &[Batch],
        target: &str,
        source_language: &str,
    ) -> Option<Vec<BatchAnswer>> {
        let mut results = Vec::with_capacity(batches.len());
        for chunk in batches.chunks(MAX_PARALLEL_REQUESTS) {
            match chunk {
                [b0] => {
                    results.push(self.request_batch(b0, target, source_language).await?);
                }
                [b0, b1] => {
                    let (r0, r1) = tokio::join!(
                        self.request_batch(b0, target, source_language),
                        self.request_batch(b1, target, source_language),
                    );
                    results.push(r0?);
                    results.push(r1?);
                }
                _ => {
                    for b in chunk {
                        results.push(self.request_batch(b, target, source_language).await?);
                    }
                }
            }
        }
        Some(results)
    }

    async fn run_romanization_batches(
        &self,
        batches: &[Batch],
        target: &str,
    ) -> Option<Vec<BatchAnswer>> {
        let mut results = Vec::with_capacity(batches.len());
        for chunk in batches.chunks(MAX_PARALLEL_REQUESTS) {
            match chunk {
                [b0] => {
                    results.push(self.request_romanization_batch(b0, target).await?);
                }
                [b0, b1] => {
                    let (r0, r1) = tokio::join!(
                        self.request_romanization_batch(b0, target),
                        self.request_romanization_batch(b1, target),
                    );
                    results.push(r0?);
                    results.push(r1?);
                }
                _ => {
                    for b in chunk {
                        results.push(self.request_romanization_batch(b, target).await?);
                    }
                }
            }
        }
        Some(results)
    }

    async fn retry_romanized_translation(
        &self,
        batches: &[Batch],
        source_language: &str,
        target: &str,
    ) -> Option<Vec<String>> {
        let source = canonical_language(source_language);
        if source.is_empty() || source == "en" {
            return None;
        }
        let mut out = Vec::new();
        for chunk in batches.chunks(MAX_PARALLEL_REQUESTS) {
            match chunk {
                [b0] => {
                    let native = self.request_native_script(&b0.payload, &source).await?;
                    let retried_batch = Batch {
                        slots: b0.slots.clone(),
                        payload: native,
                    };
                    let ans = self.request_batch(&retried_batch, target, &source).await?;
                    out.extend(ans.translations);
                }
                [b0, b1] => {
                    let f0 = async {
                        let native = self.request_native_script(&b0.payload, &source).await?;
                        let batch = Batch {
                            slots: b0.slots.clone(),
                            payload: native,
                        };
                        self.request_batch(&batch, target, &source).await
                    };
                    let f1 = async {
                        let native = self.request_native_script(&b1.payload, &source).await?;
                        let batch = Batch {
                            slots: b1.slots.clone(),
                            payload: native,
                        };
                        self.request_batch(&batch, target, &source).await
                    };
                    let (r0, r1) = tokio::join!(f0, f1);
                    out.extend(r0?.translations);
                    out.extend(r1?.translations);
                }
                _ => {
                    for b in chunk {
                        let native = self.request_native_script(&b.payload, &source).await?;
                        let batch = Batch {
                            slots: b.slots.clone(),
                            payload: native,
                        };
                        out.extend(
                            self.request_batch(&batch, target, &source)
                                .await?
                                .translations,
                        );
                    }
                }
            }
        }
        Some(out)
    }

    async fn request_batch(
        &self,
        batch: &Batch,
        target: &str,
        source_language: &str,
    ) -> Option<BatchAnswer> {
        let response = self
            .http
            .post_form(
                &self.translate_endpoint,
                TRANSLATE_USER_AGENT,
                &[
                    ("client", "dict-chrome-ex"),
                    ("sl", source_language),
                    ("tl", target),
                    ("dt", "t"),
                    ("q", &batch.payload),
                ],
            )
            .await?;

        let root = serde_json::from_str::<Value>(&response).ok()?;
        let arr = root.as_array()?;
        let segments = arr.first()?.as_array()?;
        let mut translated_body = String::new();
        for seg in segments {
            if let Some(piece) = seg
                .as_array()
                .and_then(|s| s.first())
                .and_then(Value::as_str)
            {
                translated_body.push_str(piece);
            }
        }
        let source = arr
            .get(2)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let parts: Vec<String> = MARKER_REGEX
            .split(&translated_body)
            .map(|s| s.trim().to_owned())
            .collect();
        if parts.len() != batch.slots.len() || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        Some(BatchAnswer {
            translations: parts,
            source_language: source,
            source_weight: batch.payload.chars().count(),
        })
    }

    async fn request_romanization_batch(&self, batch: &Batch, target: &str) -> Option<BatchAnswer> {
        let response = self
            .http
            .post_form(
                &self.translate_endpoint,
                TRANSLATE_USER_AGENT,
                &[
                    ("client", "dict-chrome-ex"),
                    ("sl", "auto"),
                    ("tl", target),
                    ("dt", "rm"),
                    ("q", &batch.payload),
                ],
            )
            .await?;

        let root = serde_json::from_str::<Value>(&response).ok()?;
        let arr = root.as_array()?;
        let segments = arr.first()?.as_array()?;
        let mut romanized_body = String::new();
        for seg in segments {
            if let Some(piece) = seg
                .as_array()
                .and_then(|s| s.get(3))
                .and_then(Value::as_str)
            {
                romanized_body.push_str(piece);
            }
        }
        let source = arr
            .get(2)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let parts: Vec<String> = MARKER_REGEX
            .split(&romanized_body)
            .map(|s| s.trim().to_owned())
            .collect();
        if source.trim().is_empty()
            || parts.len() != batch.slots.len()
            || parts.iter().any(|p| p.is_empty())
        {
            return None;
        }
        Some(BatchAnswer {
            translations: parts,
            source_language: source,
            source_weight: batch.payload.chars().count(),
        })
    }

    async fn request_native_script(&self, text: &str, source_language: &str) -> Option<String> {
        let itc = format!("{source_language}-t-i0-und");
        let response = self
            .http
            .post_form(
                &self.input_tools_endpoint,
                TRANSLATE_USER_AGENT,
                &[
                    ("text", text),
                    ("itc", &itc),
                    ("num", "1"),
                    ("cp", "0"),
                    ("cs", "1"),
                    ("ie", "utf-8"),
                    ("oe", "utf-8"),
                ],
            )
            .await?;

        let root = serde_json::from_str::<Value>(&response).ok()?;
        let arr = root.as_array()?;
        if arr.first().and_then(Value::as_str) != Some("SUCCESS") {
            return None;
        }
        arr.get(1)?
            .as_array()?
            .first()?
            .as_array()?
            .get(1)?
            .as_array()?
            .first()?
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
    }

    async fn load_cached(&self, key: &str) -> Option<CachedTranslation> {
        if let Ok(mut mem) = self.memory.lock()
            && let Some(hit) = mem.get(key)
        {
            return Some(hit);
        }
        let disk = self.read_disk_cache(key).await?;
        if let Ok(mut mem) = self.memory.lock() {
            mem.insert(key.to_owned(), disk.clone());
        }
        Some(disk)
    }

    async fn store_cached(&self, key: String, value: CachedTranslation) {
        if let Ok(mut mem) = self.memory.lock() {
            mem.insert(key.clone(), value.clone());
        }
        self.write_disk_cache(key, value).await;
    }

    async fn read_disk_cache(&self, key: &str) -> Option<CachedTranslation> {
        let root = self.cache_dir.clone()?;
        let key = key.to_owned();
        let _guard = self.disk_mutex.lock().await;
        tokio::task::spawn_blocking(move || read_disk_cache_sync(&root, &key))
            .await
            .ok()
            .flatten()
    }

    async fn write_disk_cache(&self, key: String, value: CachedTranslation) {
        let Some(root) = self.cache_dir.clone() else {
            return;
        };
        let _guard = self.disk_mutex.lock().await;
        let _ =
            tokio::task::spawn_blocking(move || write_disk_cache_sync(&root, &key, &value)).await;
    }
}

fn read_disk_cache_sync(root: &Path, key: &str) -> Option<CachedTranslation> {
    let file_path = root.join(CACHE_DIRECTORY).join(format!("{key}.json.gz"));
    if !file_path.is_file() {
        return None;
    }
    let file = File::open(&file_path).ok()?;
    let mut decoder = GzDecoder::new(file);
    let mut raw = String::new();
    decoder.read_to_string(&mut raw).ok()?;
    let parsed: CachedTranslation = serde_json::from_str(&raw).ok()?;
    (parsed.version == CACHE_VERSION).then_some(parsed)
}

fn write_disk_cache_sync(root: &Path, key: &str, value: &CachedTranslation) {
    let dir = root.join(CACHE_DIRECTORY);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let dest = dir.join(format!("{key}.json.gz"));
    let tmp = dir.join(format!("{key}.tmp"));
    let write_res = (|| -> std::io::Result<()> {
        let encoded = serde_json::to_vec(value).map_err(std::io::Error::other)?;
        let file = File::create(&tmp)?;
        let mut gz = GzEncoder::new(file, Compression::default());
        gz.write_all(&encoded)?;
        gz.finish()?;
        fs::rename(&tmp, &dest)?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = fs::remove_file(&tmp);
    } else {
        trim_cache(&dir);
    }
}

fn trim_cache(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, SystemTime)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("gz") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Some((path, meta.len(), modified))
        })
        .collect();
    files.sort_by_key(|entry| std::cmp::Reverse(entry.2));
    let mut kept = 0u64;
    for (path, len, _) in files {
        kept = kept.saturating_add(len);
        if kept > MAX_CACHE_BYTES {
            let _ = fs::remove_file(path);
        }
    }
}

fn flatten(lines: &[LyricLine]) -> Vec<TextSlot> {
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !line.text.trim().is_empty() {
            let header = is_genius_section_header(&line.text);
            let text = if header {
                let s = line.text.strip_prefix('[').unwrap_or(&line.text);
                s.strip_suffix(']').unwrap_or(s).trim().to_owned()
            } else {
                line.text.clone()
            };
            out.push(TextSlot {
                line_index: index,
                background: false,
                text,
                section_header: header,
            });
        }
        if let Some(bg) = line.background.as_deref()
            && !bg.text.trim().is_empty()
        {
            out.push(TextSlot {
                line_index: index,
                background: true,
                text: bg.text.clone(),
                section_header: false,
            });
        }
    }
    out
}

fn make_batches(slots: &[TextSlot]) -> Vec<Batch> {
    let mut result = Vec::new();
    let mut current: Vec<TextSlot> = Vec::new();
    let mut length = 0usize;

    for slot in slots {
        let added = slot.text.chars().count() + if current.is_empty() { 0 } else { 8 };
        if !current.is_empty() && length + added > MAX_BATCH_CHARS {
            let payload = format_payload(&current);
            result.push(Batch {
                slots: std::mem::take(&mut current),
                payload,
            });
            length = 0;
        }
        current.push(slot.clone());
        length += added;
    }
    if !current.is_empty() {
        let payload = format_payload(&current);
        result.push(Batch {
            slots: current,
            payload,
        });
    }
    result
}

fn format_payload(slots: &[TextSlot]) -> String {
    let mut out = String::new();
    for (index, slot) in slots.iter().enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str(&format!("\u{E000}{index:04}\u{E001}"));
            out.push('\n');
        }
        out.push_str(&slot.text);
    }
    out
}

fn dominant_source_language(answers: &[BatchAnswer]) -> String {
    let mut weights: HashMap<String, usize> = HashMap::new();
    for ans in answers {
        let lang = canonical_language(&ans.source_language);
        *weights.entry(lang).or_insert(0) += ans.source_weight;
    }
    weights
        .into_iter()
        .max_by_key(|(_, w)| *w)
        .map(|(lang, _)| lang)
        .unwrap_or_default()
}

fn predominantly_latin(slots: &[TextSlot]) -> bool {
    let mut latin = 0usize;
    let mut other = 0usize;
    for slot in slots {
        for ch in slot.text.chars() {
            if ch.is_alphabetic() {
                if ch.script() == Script::Latin {
                    latin += 1;
                } else {
                    other += 1;
                }
            }
        }
    }
    latin > 0 && latin >= other.saturating_mul(4)
}

fn has_non_latin_letters(slots: &[TextSlot]) -> bool {
    slots.iter().any(|slot| {
        slot.text
            .chars()
            .any(|ch| ch.is_alphabetic() && ch.script() != Script::Latin)
    })
}

fn unchanged_weight(slots: &[TextSlot], transformed: &[String]) -> usize {
    slots
        .iter()
        .zip(transformed.iter())
        .map(|(slot, text)| {
            if comparable(&slot.text) == comparable(text) {
                slot.text.chars().count()
            } else {
                0
            }
        })
        .sum()
}

fn comparable(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    WHITESPACE.replace_all(&lowered, " ").into_owned()
}

fn rebuild(original: &[LyricLine], slots: &[TextSlot], translated: &[String]) -> Vec<LyricLine> {
    let mut by_line: HashMap<usize, Vec<(&TextSlot, &String)>> = HashMap::new();
    for (slot, text) in slots.iter().zip(translated.iter()) {
        by_line
            .entry(slot.line_index)
            .or_default()
            .push((slot, text));
    }

    original
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let entries = by_line.get(&index).map(Vec::as_slice).unwrap_or_default();
            let lead = entries.iter().find(|(s, _)| !s.background);
            let backing = entries.iter().find(|(s, _)| s.background);

            let lead_text = match lead {
                Some((slot, text)) if slot.section_header => format!("[{text}]"),
                Some((_, text)) => (*text).clone(),
                None => line.text.clone(),
            };

            let background = line.background.as_deref().map(|source| {
                let bg_text = backing
                    .map(|(_, t)| (*t).clone())
                    .unwrap_or_else(|| source.text.clone());
                Box::new(LyricLine {
                    words: retime_words(source, &bg_text),
                    timing_source: source.is_word_synced().then(|| Box::new(source.clone())),
                    text: bg_text,
                    ..source.clone()
                })
            });

            LyricLine {
                words: retime_words(line, &lead_text),
                timing_source: line.is_word_synced().then(|| Box::new(line.clone())),
                text: lead_text,
                background,
                ..line.clone()
            }
        })
        .collect()
}

fn retime_words(source: &LyricLine, translated: &str) -> Vec<LyricWord> {
    if source.words.is_empty() || translated.is_empty() {
        return Vec::new();
    }
    let start = source.words[0].start_ms;
    let end = source.words[source.words.len() - 1].end_ms;
    vec![LyricWord::new(start, end, translated)]
}

/// Normalizes a BCP-47 / legacy language tag down to its lowercase ISO-639
/// primary language subtag (`iw -> he`, `in -> id`, `ji -> yi`).
pub fn canonical_language(tag: &str) -> String {
    let primary = tag
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match primary.as_str() {
        "iw" => "he".to_owned(),
        "in" => "id".to_owned(),
        "ji" => "yi".to_owned(),
        _ => primary,
    }
}

fn same_language(first: &str, second: &str) -> bool {
    canonical_language(first) == canonical_language(second)
}

fn cache_key(track_id: &str, target: &str, slots: &[TextSlot]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(track_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(target.as_bytes());
    for slot in slots {
        hasher.update([0u8]);
        hasher.update(slot.text.as_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_mock_endpoint(
        responder: impl Fn(String) -> String + Send + Sync + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let responder = std::sync::Arc::new(responder);

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let responder = std::sync::Arc::clone(&responder);
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
                    let headers = String::from_utf8_lossy(&buf[..header_end]);
                    let content_len: usize = headers
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    while buf.len() < body_start + content_len {
                        let n = match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(
                        &buf[body_start..(body_start + content_len).min(buf.len())],
                    )
                    .into_owned();
                    let resp_body = responder(body);
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

        format!("http://{addr}/endpoint")
    }

    #[test]
    fn canonical_language_handles_legacy_codes_and_subtags() {
        assert_eq!(canonical_language("iw-IL"), "he");
        assert_eq!(canonical_language("in_ID"), "id");
        assert_eq!(canonical_language("ji"), "yi");
        assert_eq!(canonical_language("zh-TW"), "zh");
        assert!(same_language("zh-CN", "zh-TW"));
    }

    #[tokio::test]
    async fn romanize_fast_paths_already_latin_lines() {
        let translator = LyricsTranslator::new(None);
        let lines = vec![LyricLine::new(1_000, "Hello world")];
        assert_eq!(
            translator.romanize("id1", &lines, "en").await,
            RomanizationResult::AlreadyRomanized
        );
    }

    #[tokio::test]
    async fn translate_preserves_timing_source_section_brackets_and_caches_to_disk() {
        let translate_url = spawn_mock_endpoint(|_body| {
            serde_json::json!([
                [[
                    "Estribillo\n\u{E000}0001\u{E001}\nHola mundo",
                    "orig",
                    null,
                    null
                ]],
                null,
                "en"
            ])
            .to_string()
        })
        .await;

        let tmp = std::env::temp_dir().join(format!("bitchord-tr-test-{}", uuid::Uuid::new_v4()));
        let lines = vec![
            LyricLine::new(0, "[Chorus]"),
            LyricLine {
                time_ms: 1_000,
                text: "Hello world".to_owned(),
                words: vec![
                    LyricWord::new(1_000, 1_500, "Hello"),
                    LyricWord::new(1_500, 2_200, "world"),
                ],
                ..Default::default()
            },
        ];

        let translator =
            LyricsTranslator::new(Some(tmp.clone())).with_endpoints(translate_url, String::new());
        let first = translator.translate("vid1", &lines, "es").await;
        match first {
            TranslationResult::Translated {
                lines: out,
                source_language,
                from_cache,
            } => {
                assert!(!from_cache);
                assert_eq!(source_language, "en");
                assert_eq!(out[0].text, "[Estribillo]");
                assert_eq!(out[1].text, "Hola mundo");
                assert!(out[1].timing_source.is_some());
                assert_eq!(out[1].words.len(), 1);
                assert_eq!(out[1].words[0].start_ms, 1_000);
                assert_eq!(out[1].words[0].end_ms, 2_200);
            }
            other => panic!("expected Translated, got {other:?}"),
        }

        // Fresh translator instance with empty memory cache must hit gzipped disk cache.
        let translator2 = LyricsTranslator::new(Some(tmp.clone()));
        let second = translator2.translate("vid1", &lines, "es").await;
        match second {
            TranslationResult::Translated {
                lines: out,
                source_language,
                from_cache,
            } => {
                assert!(from_cache);
                assert_eq!(source_language, "en");
                assert_eq!(out[1].text, "Hola mundo");
            }
            other => panic!("expected cached Translated, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn translate_retries_latin_script_lyrics_via_input_tools() {
        let translate_url = spawn_mock_endpoint(|body| {
            if body.contains("sl=hi") {
                serde_json::json!([[["I am becoming yours", "orig", null, null]], null, "hi"])
                    .to_string()
            } else {
                serde_json::json!([[["tera hone laga hoon", "orig", null, null]], null, "hi"])
                    .to_string()
            }
        })
        .await;
        let input_tools_url = spawn_mock_endpoint(|_body| {
            serde_json::json!(["SUCCESS", [["orig", ["तेरा होने लगा हूँ"]]]]).to_string()
        })
        .await;

        let translator = LyricsTranslator::new(None).with_endpoints(translate_url, input_tools_url);
        let lines = vec![LyricLine::new(1_000, "tera hone laga hoon")];
        let res = translator.translate("vid2", &lines, "en").await;
        match res {
            TranslationResult::Translated {
                lines: out,
                source_language,
                from_cache,
            } => {
                assert!(!from_cache);
                assert_eq!(source_language, "hi");
                assert_eq!(out[0].text, "I am becoming yours");
            }
            other => panic!("expected retried translation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn romanize_decodes_segment_3_and_caches() {
        let translate_url = spawn_mock_endpoint(|_body| {
            serde_json::json!([[[null, null, null, "Konnichiwa"]], null, "ja"]).to_string()
        })
        .await;
        let translator = LyricsTranslator::new(None).with_endpoints(translate_url, String::new());
        let lines = vec![LyricLine::new(1_000, "こんにちは")];
        match translator.romanize("vid3", &lines, "en").await {
            RomanizationResult::Romanized {
                lines: out,
                source_language,
                from_cache,
            } => {
                assert!(!from_cache);
                assert_eq!(source_language, "ja");
                assert_eq!(out[0].text, "Konnichiwa");
            }
            other => panic!("expected Romanized, got {other:?}"),
        }
    }
}
