use std::sync::{LazyLock, RwLock};

use regex::Regex;
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, ProviderLyrics, TimedRun, with_instrumental_gaps, words_from_runs};

const API: &str = "https://api.paxsenix.org";
const PUBLIC_PROXY: &str = "https://lyrics.paxsenix.org";
const APPLE_SEARCH: &str = "https://amp-api.music.apple.com/v1/catalog/us/search";
const MINIMUM_MATCH_SCORE: i32 = 10;

const ID_KEYS: &[&str] = &["id", "trackId", "track_id", "realId"];
const TITLE_KEYS: &[&str] = &["name", "title", "trackName", "track_name"];
const ARTIST_KEYS: &[&str] = &["artistName", "artist_name"];
const DURATION_KEYS: &[&str] = &["durationInMillis", "durationMs", "duration_ms", "duration"];

static APPLE_INDEX_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"/assets/index~[^"]+\.js"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid APPLE_INDEX_SCRIPT regex")
});

static APPLE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
        // INFALLIBLE: static regex pattern.
        .expect("valid APPLE_TOKEN regex")
});

/// Strips an optional case-insensitive `"Bearer "` prefix and surrounding
/// whitespace from a user-supplied PaxSenix API key.
pub fn normalize_paxsenix_api_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
        trimmed[7..].trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Public Apple Music lyrics proxy plus optional authenticated Spotify and
/// Musixmatch routes via PaxSenix.
#[derive(Debug, Default)]
pub struct PaxSenix {
    api_key: RwLock<String>,
    cached_apple_token: RwLock<Option<String>>,
    token_mutex: Mutex<()>,
}

impl PaxSenix {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_api_key(&self, value: &str) {
        if let Ok(mut guard) = self.api_key.write() {
            *guard = normalize_paxsenix_api_key(value);
        }
    }

    pub fn api_key(&self) -> String {
        self.api_key.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Original keyless Apple Music provider (`LyricsSource::PaxSenix`).
    pub async fn lyrics(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        _album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let id = self
            .search_public_apple_track_id(http, title, artist, duration_ms)
            .await?;
        let url = build_url(
            &format!("{PUBLIC_PROXY}/apple-music/lyrics"),
            &[("id", &id), ("ttml", "true")],
        )?;
        let body = http.get(&url).await?;
        Self::parse_response(&body)
    }

    pub async fn spotify_lyrics(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        if let Some(id) = self
            .search_track_id(http, "spotify/search", title, artist, duration_ms)
            .await
            && let Some(url) = build_url(&format!("{API}/lyrics/spotify"), &[("id", &id)])
            && let Some(body) = self.api_body(http, &url).await
            && let Some(lines) = Self::parse_response(&body)
        {
            return Some(lines);
        }
        self.generic_authenticated_lyrics(http, title, artist, duration_ms)
            .await
    }

    pub async fn musixmatch_lyrics(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        let secs = (duration_ms / 1_000).to_string();
        if let Some(url) = build_url(
            &format!("{API}/lyrics/musixmatch"),
            &[("t", title), ("a", artist), ("d", &secs)],
        ) && let Some(body) = self.api_body(http, &url).await
            && let Some(lines) = Self::parse_response(&body)
        {
            return Some(lines);
        }
        self.generic_authenticated_lyrics(http, title, artist, duration_ms)
            .await
    }

    async fn search_public_apple_track_id(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<String> {
        let token = self.apple_token(http).await?;
        let term = format!("{title} {artist}");
        let url = build_url(
            APPLE_SEARCH,
            &[
                ("term", &term),
                ("types", "songs"),
                ("limit", "10"),
                ("l", "en-US"),
            ],
        )?;
        let body = http.get_apple_authorized(&url, &token).await?;
        let root: Value = serde_json::from_str(&body).ok()?;
        best_candidate(&root, title, artist, duration_ms).map(|c| c.id)
    }

    async fn apple_token(&self, http: &LyricsHttp) -> Option<String> {
        if let Ok(guard) = self.cached_apple_token.read()
            && let Some(tok) = guard.as_ref()
        {
            return Some(tok.clone());
        }
        let _lock = self.token_mutex.lock().await;
        if let Ok(guard) = self.cached_apple_token.read()
            && let Some(tok) = guard.as_ref()
        {
            return Some(tok.clone());
        }
        let scraped = scrape_apple_token(http).await?;
        if let Ok(mut guard) = self.cached_apple_token.write() {
            *guard = Some(scraped.clone());
        }
        Some(scraped)
    }

    async fn search_track_id(
        &self,
        http: &LyricsHttp,
        path: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<String> {
        let query = format!("{title} {artist}");
        let url = build_url(&format!("{API}/{path}"), &[("q", &query)])?;
        let body = self.api_body(http, &url).await?;
        let root: Value = serde_json::from_str(&body).ok()?;
        best_candidate(&root, title, artist, duration_ms).map(|c| c.id)
    }

    async fn generic_authenticated_lyrics(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        let query = format!("{title} {artist}");
        let url = build_url(&format!("{API}/lyrics/lrcget"), &[("q", &query)])?;
        let body = self.api_body(http, &url).await?;
        Self::parse_lrc_get(&body, title, artist, duration_ms)
    }

    async fn api_body(&self, http: &LyricsHttp, url: &str) -> Option<String> {
        let key = self.api_key();
        if key.trim().is_empty() {
            return None;
        }
        http.get_bearer(url, &key).await
    }

    fn parse_response(raw: &str) -> Option<Vec<LyricLine>> {
        Self::parse_timed_apple(raw).or_else(|| ProviderLyrics::parse(raw))
    }

    /// Selects a single recording from PaxSenix's general `/lyrics/lrcget`
    /// array so multiple search candidates are never concatenated together.
    pub fn parse_lrc_get(
        raw: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
    ) -> Option<Vec<LyricLine>> {
        let root: Value = serde_json::from_str(raw).ok()?;
        let documents = root
            .as_object()
            .and_then(|o| o.get("lyrics"))
            .and_then(Value::as_array);
        let Some(docs) = documents.filter(|a| !a.is_empty()) else {
            return Self::parse_response(raw);
        };

        type RankKey = (i32, i64, usize, usize);
        let mut best: Option<(Vec<LyricLine>, RankKey)> = None;
        for doc in docs {
            let serialized = doc.to_string();
            let Some(lines) = Self::parse_response(&serialized) else {
                continue;
            };
            let metadata_score = doc
                .as_object()
                .map_or(0, |o| lyric_candidate_score(o, title, artist, duration_ms));
            let dist = duration_distance(&lines, duration_ms);
            let timed_count = lines.iter().filter(|l| l.time_ms > 0).count();
            let word_count = lines.iter().filter(|l| l.is_word_synced()).count();
            let key = (metadata_score, -dist, timed_count, word_count);
            if best.as_ref().is_none_or(|(_, best_key)| key > *best_key) {
                best = Some((lines, key));
            }
        }
        best.map(|(lines, _)| lines)
    }

    /// Preserves word/syllable timestamps when PaxSenix returns its structured
    /// Apple payload (`{"type":"Word"|"Syllable","content":[...]}`).
    pub fn parse_timed_apple(raw: &str) -> Option<Vec<LyricLine>> {
        let root: Value = serde_json::from_str(raw).ok()?;
        let content = find_timed_content(&root)?;
        let rows: Vec<&Map<String, Value>> = content.iter().filter_map(Value::as_object).collect();
        let mut lines = Vec::new();

        for (index, row) in rows.iter().enumerate() {
            let Some(start) = obj_i64(row, "timestamp") else {
                continue;
            };
            let Some(word_rows) = row.get("text").and_then(Value::as_array) else {
                continue;
            };
            let texts: Vec<String> = word_rows
                .iter()
                .filter_map(|v| v.as_object().and_then(|o| obj_str(o, "text")))
                .collect();
            if texts.is_empty() {
                continue;
            }
            let next_line = rows.get(index + 1).and_then(|r| obj_i64(r, "timestamp"));

            let mut runs = Vec::new();
            for (word_index, elem) in word_rows.iter().enumerate() {
                let Some(word) = elem.as_object() else {
                    continue;
                };
                let Some(text) = obj_str(word, "text")
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let Some(word_start) = obj_i64(word, "timestamp") else {
                    continue;
                };
                let word_end = obj_i64(word, "endtime")
                    .or_else(|| {
                        word_rows
                            .get(word_index + 1)
                            .and_then(Value::as_object)
                            .and_then(|o| obj_i64(o, "timestamp"))
                    })
                    .or(next_line)
                    .unwrap_or(word_start + 800);
                let joins_next = word.get("part").and_then(Value::as_bool) == Some(true);
                let run_text = if joins_next { text } else { format!("{text} ") };
                runs.push(TimedRun::new(word_start, word_end, run_text));
            }

            let timed = words_from_runs(&runs, true);
            let untimed = runs.len() != texts.iter().filter(|t| !t.trim().is_empty()).count();
            let line_time = start.min(timed.first().map_or(start, |w| w.start_ms));
            let text = if untimed {
                texts.iter().map(|t| t.trim()).collect::<Vec<_>>().join(" ")
            } else {
                timed
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            lines.push(LyricLine {
                time_ms: line_time,
                text,
                words: if untimed { Vec::new() } else { timed },
                sung_until_ms: next_line,
                ..Default::default()
            });
        }

        let with_gaps = with_instrumental_gaps(lines);
        if with_gaps.iter().any(|l| !l.text.trim().is_empty()) {
            Some(with_gaps)
        } else {
            None
        }
    }
}

async fn scrape_apple_token(http: &LyricsHttp) -> Option<String> {
    let page = http.get("https://music.apple.com/us/new").await?;
    let script_path = APPLE_INDEX_SCRIPT.find(&page)?.as_str();
    let script = http
        .get(&format!("https://music.apple.com{script_path}"))
        .await?;
    APPLE_TOKEN.find(&script).map(|m| m.as_str().to_owned())
}

fn find_timed_content(val: &Value) -> Option<&Vec<Value>> {
    match val {
        Value::Object(map) => {
            if let Some(arr) = map.get("content").and_then(Value::as_array)
                && arr
                    .iter()
                    .any(|item| item.as_object().and_then(|o| o.get("timestamp")).is_some())
            {
                return Some(arr);
            }
            for v in map.values() {
                if let Some(found) = find_timed_content(v) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for v in arr {
                if let Some(found) = find_timed_content(v) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

struct Candidate {
    id: String,
    title: String,
    artist: String,
    duration_ms: i64,
}

impl Candidate {
    fn score(&self, wanted_title: &str, wanted_artist: &str, wanted_duration: i64) -> i32 {
        let mut score = text_score(&self.title, wanted_title, 20, 10)
            + text_score(&self.artist, wanted_artist, 15, 5);
        if wanted_duration > 0 && self.duration_ms > 0 {
            let diff = (self.duration_ms - wanted_duration).abs();
            score += if diff < 3_000 {
                10
            } else if diff < 10_000 {
                5
            } else {
                0
            };
        }
        score
    }
}

fn best_candidate(root: &Value, title: &str, artist: &str, duration_ms: i64) -> Option<Candidate> {
    let mut candidates = Vec::new();
    collect_candidates(root, &mut candidates);
    let mut best: Option<(Candidate, i32)> = None;
    for candidate in candidates {
        let s = candidate.score(title, artist, duration_ms);
        if best.as_ref().is_none_or(|(_, bs)| s > *bs) {
            best = Some((candidate, s));
        }
    }
    best.and_then(|(c, s)| {
        if s >= MINIMUM_MATCH_SCORE {
            Some(c)
        } else {
            None
        }
    })
}

fn collect_candidates(val: &Value, into: &mut Vec<Candidate>) {
    match val {
        Value::Array(arr) => {
            for item in arr {
                collect_candidates(item, into);
            }
        }
        Value::Object(map) => {
            if let Some(c) = to_candidate(map) {
                into.push(c);
            }
            for v in map.values() {
                collect_candidates(v, into);
            }
        }
        _ => {}
    }
}

fn to_candidate(map: &Map<String, Value>) -> Option<Candidate> {
    let details = map
        .get("attributes")
        .and_then(Value::as_object)
        .unwrap_or(map);
    let id = first_string(map, ID_KEYS).or_else(|| first_string(details, ID_KEYS))?;
    let title = first_string(details, TITLE_KEYS)?;
    let artist = first_string(details, ARTIST_KEYS)
        .or_else(|| artist_names(details))
        .unwrap_or_default();
    let duration_ms = to_duration_ms(first_i64(details, DURATION_KEYS));
    Some(Candidate {
        id,
        title,
        artist,
        duration_ms,
    })
}

fn lyric_candidate_score(
    map: &Map<String, Value>,
    title: &str,
    artist: &str,
    duration_ms: i64,
) -> i32 {
    let details = map
        .get("attributes")
        .and_then(Value::as_object)
        .unwrap_or(map);
    let candidate = Candidate {
        id: first_string(map, ID_KEYS).unwrap_or_default(),
        title: first_string(details, TITLE_KEYS).unwrap_or_default(),
        artist: first_string(details, ARTIST_KEYS)
            .or_else(|| artist_names(details))
            .unwrap_or_default(),
        duration_ms: to_duration_ms(first_i64(details, DURATION_KEYS)),
    };
    candidate.score(title, artist, duration_ms)
}

fn duration_distance(lines: &[LyricLine], duration_ms: i64) -> i64 {
    if duration_ms <= 0 {
        return 0;
    }
    let Some(last_ts) = lines
        .iter()
        .map(|line| {
            let max_word = line.words.iter().map(|w| w.end_ms).max().unwrap_or(0);
            line.time_ms
                .max(line.sung_until_ms.unwrap_or(0))
                .max(max_word)
        })
        .max()
    else {
        return i64::MAX / 2;
    };
    (last_ts - duration_ms).abs()
}

fn artist_names(map: &Map<String, Value>) -> Option<String> {
    let val = map.get("artists").or_else(|| map.get("artist"))?;
    match val {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => first_string(o, &["name", "artistName", "title"]),
        Value::Array(arr) => {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|item| match item {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => first_string(o, &["name", "artistName", "title"]),
                    _ => None,
                })
                .collect();
            if names.is_empty() {
                None
            } else {
                Some(names.join(", "))
            }
        }
        _ => None,
    }
}

fn first_string(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        obj_str(map, k)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    })
}

fn first_i64(map: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| obj_i64(map, k))
}

fn obj_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    match map.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn obj_i64(map: &Map<String, Value>, key: &str) -> Option<i64> {
    match map.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn to_duration_ms(raw: Option<i64>) -> i64 {
    match raw {
        None => 0,
        Some(v) if v <= 0 => 0,
        Some(v) if v < 10_000 => v * 1_000,
        Some(v) => v,
    }
}

fn text_score(candidate: &str, wanted: &str, exact: i32, partial: i32) -> i32 {
    if candidate.trim().is_empty() || wanted.trim().is_empty() {
        return 0;
    }
    let c = candidate.to_lowercase();
    let w = wanted.to_lowercase();
    if c == w {
        exact
    } else if c.contains(&w) || w.contains(&c) {
        partial
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LyricSyllable;

    #[test]
    fn structured_paxsenix_apple_response_keeps_word_timing() {
        let raw = r#"{"type":"Word","content":[
            {"timestamp":1000,"text":[{"text":"sing","timestamp":1000},{"text":"along","timestamp":1400}]},
            {"timestamp":2200,"text":[{"text":"again","timestamp":2200}]}
        ]}"#;
        let lines: Vec<LyricLine> = PaxSenix::parse_timed_apple(raw)
            .expect("parsed")
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        assert_eq!(lines[0].text, "sing along");
        assert_eq!(lines[0].words[1].start_ms, 1_400);
        assert_eq!(lines[0].words[1].end_ms, 2_200);
    }

    #[test]
    fn paxsenix_accepts_bare_and_bearer_prefixed_api_keys() {
        assert_eq!(normalize_paxsenix_api_key(" secret "), "secret");
        assert_eq!(normalize_paxsenix_api_key("Bearer secret"), "secret");
        assert_eq!(normalize_paxsenix_api_key("bearer   secret "), "secret");
    }

    #[test]
    fn paxsenix_general_fallback_selects_one_candidate() {
        let raw = r#"{"lyrics":[
            {"id":"wrong","trackName":"Out of Touch","artistName":"Other","duration":201,
             "syncedLyrics":"[00:01.00]wrong line\n[00:02.00]wrong again"},
            {"id":"right","trackName":"Out of Time","artistName":"The Weeknd","duration":201,
             "syncedLyrics":"[00:01.00]first line\n[00:02.00]second line"},
            {"id":"duplicate","trackName":"Out of Time (Remix)","artistName":"The Weeknd","duration":240,
             "syncedLyrics":"[00:01.00]first line\n[00:02.00]second line"}
        ]}"#;

        let lines: Vec<LyricLine> =
            PaxSenix::parse_lrc_get(raw, "Out of Time", "The Weeknd", 201_000)
                .expect("lines")
                .into_iter()
                .filter(|l| !l.is_gap())
                .collect();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        let times: Vec<i64> = lines.iter().map(|l| l.time_ms).collect();
        assert_eq!(texts, vec!["first line", "second line"]);
        assert_eq!(times, vec![1_000, 2_000]);
    }

    #[test]
    fn paxsenix_general_fallback_treats_string_array_entries_as_separate_lrc() {
        let raw = r#"{"lyrics":[
            "[00:01.00]wrong first\n[00:40.00]wrong last",
            "[00:01.00]right first\n[03:19.00]right last",
            "[00:01.00]right first\n[03:19.00]right last"
        ]}"#;

        let lines: Vec<LyricLine> = PaxSenix::parse_lrc_get(raw, "Song", "Artist", 200_000)
            .expect("lines")
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        let times: Vec<i64> = lines.iter().map(|l| l.time_ms).collect();
        assert_eq!(texts, vec!["right first", "right last"]);
        assert_eq!(times, vec![1_000, 199_000]);
    }

    #[test]
    fn paxsenix_part_flag_joins_syllables_into_one_word() {
        let raw = r#"{"type":"Syllable","content":[{"timestamp":31498,"endtime":32529,"text":[
            {"text":"long","timestamp":31498,"endtime":31839,"part":false},
            {"text":"e","timestamp":31839,"endtime":31996,"part":true},
            {"text":"nough","timestamp":31996,"endtime":32529,"part":false}
        ]}]}"#;
        let lines: Vec<LyricLine> = PaxSenix::parse_timed_apple(raw)
            .expect("parsed")
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.text, "long enough");
        let words: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(words, vec!["long", "enough"]);
        assert_eq!(line.words[0].end_ms, 31_839);
        assert_eq!(
            line.words[1].syllables,
            vec![
                LyricSyllable::new(31_839, 31_996, 0, 1),
                LyricSyllable::new(31_996, 32_529, 1, 6),
            ]
        );
    }
}
