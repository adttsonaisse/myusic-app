use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use bitchord_model::Song;
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

pub const DURATION_LIMIT_SEC: i32 = 30;

const BASE: i32 = 100;
const ARTIST_EXACT: i32 = 25;
const ARTIST_SHARED: i32 = 10;
const CREDITS_DISAGREE: i32 = -30;
const CREDIT_OVERRIDE_SEC: i32 = 2;
const DURATION_TIGHT: i32 = 40;
const DURATION_LOOSE: i32 = 15;
const ALBUM_EXACT: i32 = 35;
const EXPLICIT_EXACT: i32 = 20;
const CONTEXT_SHARED: i32 = 20;
const DURATION_TIGHT_SEC: i32 = 3;
const VIDEO_DURATION_LIMIT_SEC: i32 = 90;
const BRACKET_PASSES: usize = 3;
const DASH_PASSES: usize = 3;

static BRACKETED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[(\[]([^()\[\]]*)[)\]]").expect("valid static regex"));
static DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[-–—|]+\s+").expect("valid static regex"));
static FEATURING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:feat|ft|featuring|with)\b.*").expect("valid static regex"));
static WORD_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s.·~〜]+").expect("valid static regex"));
static NON_ALNUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]").expect("valid static regex"));
static CJK_STRIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s\p{P}・～〜ー-]+").expect("valid static regex"));
static LATIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z0-9]").expect("valid static regex"));
static CJK_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[「『【〈《〔［｛]").expect("valid static regex"));
static CJK_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[」』】〉》〕］｝]").expect("valid static regex"));
static ARTIST_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\s*(?:[,&/;·|、，×]|\band\b|\bx\b|\bvs\.?\b|\bfeat\.?\b|\bft\.?\b|\bfeaturing\b|\bwith\b)\s*",
    )
    .expect("valid static regex")
});

const VERSION_WORDS: &[&str] = &[
    "remix",
    "remixes",
    "rmx",
    "refix",
    "flip",
    "bootleg",
    "mashup",
    "medley",
    "live",
    "concert",
    "unplugged",
    "acoustic",
    "instrumental",
    "karaoke",
    "vocals",
    "vocal",
    "acapella",
    "acappella",
    "backing",
    "stems",
    "stem",
    "cover",
    "demo",
    "reprise",
    "remake",
    "rework",
    "extended",
    "edit",
    "version",
    "mix",
    "dub",
    "vip",
    "session",
    "sessions",
    "sped",
    "slowed",
    "reverb",
    "nightcore",
    "lofi",
    "orchestral",
    "symphonic",
    "part",
    "pt",
    "chapter",
    "カバー",
    "カラオケ",
    "リミックス",
    "ライブ",
    "アコースティック",
    "インスト",
    "バージョン",
    "歌ってみた",
    "踊ってみた",
    "弾いてみた",
    "翻唱",
    "现场",
    "混音",
    "커버",
    "라이브",
    "리믹스",
];

const TRAILING_TAKES: &[&str] = &[
    "instrumental",
    "karaoke",
    "acapella",
    "acappella",
    "remix",
    "rmx",
    "nightcore",
];

const NOVOCAL_TAKES: &[&str] = &[
    "instrumental",
    "karaoke",
    "acapella",
    "acappella",
    "backing",
    "stems",
    "stem",
];

const NEUTRAL_SEGMENTS: &[&str] = &[
    "albumversion",
    "originalversion",
    "originalmix",
    "singleversion",
    "radioversion",
    "radioedit",
    "stereoversion",
    "monoversion",
    "studioversion",
    "fullversion",
    "standardversion",
    "explicitversion",
    "deluxeversion",
    "originaltrack",
];

const YOUTUBE_TYPE_LABELS: &[&str] = &[
    "música", "musica", "vídeo", "video", "song", "album", "álbum", "artista", "artist",
];

const NOISE_WORDS: &[&str] = &[
    "official",
    "video",
    "audio",
    "lyrics",
    "lyric",
    "lyrical",
    "visualizer",
    "song",
    "songs",
    "full",
    "music",
    "the",
    "and",
    "from",
    "feat",
    "ft",
    "featuring",
    "with",
    "new",
    "latest",
    "free",
    "download",
    "remaster",
    "remastered",
    "explicit",
    "clean",
    "bonus",
    "track",
    "deluxe",
    "original",
    "album",
    "single",
    "hd",
    "hq",
    "4k",
    "mp3",
];

const ALBUM_NOISE_WORDS: &[&str] = &[
    "album",
    "deluxe",
    "edition",
    "expanded",
    "remaster",
    "remastered",
    "version",
    "explicit",
    "clean",
    "bonus",
    "anniversary",
];

const TRAILING_NOISE: &[&str] = &[
    "song",
    "songs",
    "video",
    "audio",
    "lyrics",
    "lyric",
    "lyrical",
    "official",
    "full",
    "hd",
    "hq",
    "4k",
    "mp3",
    "ost",
    "soundtrack",
];

const JOINING_WORDS: &[&str] = &["and"];

/// The recording being looked for across external catalogues.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Target {
    pub title: String,
    pub artist: String,
    pub duration_sec: Option<i32>,
    pub album: Option<String>,
    pub is_explicit: Option<bool>,
    pub is_video: bool,
}

impl Target {
    pub fn new(title: impl Into<String>, artist: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            ..Self::default()
        }
    }

    pub fn of_song(song: &Song) -> Self {
        Self {
            title: song.title.clone(),
            artist: song.artist.clone(),
            duration_sec: seconds_of(song.duration_text.as_deref()),
            album: song.album_name.clone(),
            is_explicit: song.is_explicit,
            is_video: song.is_video,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TitleParts {
    words: Vec<String>,
    core: String,
    versions: BTreeSet<String>,
    /// Preserves insertion order (matching Kotlin's `LinkedHashSet`) so `context.concat()`
    /// matches whole multi-word bracketed translations deterministically.
    context: Vec<String>,
}

/// Cross-catalogue recording matcher & query builder.
pub struct TrackMatcher;

impl TrackMatcher {
    /// Builds deduplicated search queries (best first) with packaging stripped and version markers kept.
    pub fn queries(target: &Target) -> Vec<String> {
        let title = searchable_title(&target.title, &target.artist);
        if title.trim().is_empty() {
            return Vec::new();
        }
        let artist = primary_artist(&target.artist);
        let mut out = if artist.trim().is_empty() {
            vec![title.clone()]
        } else {
            vec![format!("{title} {artist}"), title.clone()]
        };

        let latin = romaji_of(&title);
        let latin = latin.trim();
        let other_script = if !latin.is_empty()
            && latin != title
            && latin
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == ' ')
        {
            Some(latin.to_owned())
        } else {
            script_mates(&target.title)
                .into_iter()
                .find(|s| !s.trim().is_empty() && s != &title)
        };

        if let Some(other) = other_script {
            out.push(other);
        }

        let mut seen = HashSet::new();
        out.retain(|q| seen.insert(q.clone()));
        out
    }

    /// Whether both artist credit strings share at least one whole artist name.
    pub fn shares_artist(wanted: &str, got: &str) -> bool {
        let want = artist_names(wanted);
        let have = artist_names(got);
        !want.is_empty()
            && !have.is_empty()
            && want.iter().any(|w| have.iter().any(|h| same_artist(w, h)))
    }

    /// Best matching candidate for `target`, or `None`.
    pub fn best<'a>(candidates: &'a [Song], target: &Target) -> Option<&'a Song> {
        Self::ranked(candidates, target).into_iter().next()
    }

    /// Official catalogue audio counterpart for a manually switched music video.
    pub fn best_official_audio_for_video<'a>(
        candidates: &'a [Song],
        target: &Target,
    ) -> Option<&'a Song> {
        let wanted = parse_title(&target.title, &target.artist);
        if wanted.core.is_empty() {
            return None;
        }
        let mut scored: Vec<(&'a Song, i32)> = candidates
            .iter()
            .filter_map(|candidate| {
                let got = parse_title(&candidate.title, &candidate.artist);
                if wanted.core != got.core || wanted.versions != got.versions {
                    return None;
                }
                let artist = artist_score(&target.artist, &candidate.artist)?;
                let duration = match target.duration_sec {
                    Some(expected) => match seconds_of(candidate.duration_text.as_deref()) {
                        Some(actual) => -(expected - actual).abs(),
                        None => -120,
                    },
                    None => 0,
                };
                Some((candidate, artist * 1_000 + duration))
            })
            .collect();
        scored.sort_by_key(|a| std::cmp::Reverse(a.1));
        scored.into_iter().next().map(|(song, _)| song)
    }

    /// Every acceptable candidate for `target`, ordered most confident first.
    pub fn ranked<'a>(candidates: &'a [Song], target: &Target) -> Vec<&'a Song> {
        let scored: Vec<(&'a Song, i32)> = candidates
            .iter()
            .filter_map(|c| Self::score(c, target).map(|s| (c, s)))
            .collect();
        let credited: Vec<(&'a Song, i32)> = scored
            .iter()
            .copied()
            .filter(|(c, _)| artist_score(&target.artist, &c.artist).is_some())
            .collect();
        let mut chosen = if credited.is_empty() {
            scored
        } else {
            credited
        };
        chosen.sort_by_key(|a| std::cmp::Reverse(a.1));
        chosen.into_iter().map(|(song, _)| song).collect()
    }

    /// Confidence score for `candidate` against `target`, or `None` if rejected.
    pub fn score(candidate: &Song, target: &Target) -> Option<i32> {
        let wanted = parse_title(&target.title, &target.artist);
        let got = parse_title(&candidate.title, &candidate.artist);
        if wanted.core.trim().is_empty() || got.core.trim().is_empty() {
            return None;
        }

        let is_title_match = if wanted.core == got.core
            || romaji_of(&wanted.core) == romaji_of(&got.core)
            || is_translation_match(&wanted, &got)
        {
            true
        } else if wanted.core.encode_utf16().count() <= 3 || got.core.encode_utf16().count() <= 3 {
            false
        } else {
            wanted.core.contains(&got.core) || got.core.contains(&wanted.core)
        };
        if !is_title_match {
            return None;
        }

        let album_marks = album_version_markers(candidate.album_name.as_deref());
        let mut effective_versions = got.versions.clone();
        for mark in &album_marks {
            if NOVOCAL_TAKES.contains(&mark.as_str()) || wanted.versions.contains(mark) {
                effective_versions.insert(mark.clone());
            }
        }
        if wanted.versions != effective_versions {
            return None;
        }

        let credited_artist = artist_score(&target.artist, &candidate.artist);
        let duration = duration_score(
            target.duration_sec,
            seconds_of(candidate.duration_text.as_deref()),
            credited_artist.is_some(),
        )?;
        let artist = match credited_artist {
            Some(score) => score,
            None if !target.is_video
                && Self::within_seconds(candidate, target, CREDIT_OVERRIDE_SEC) =>
            {
                CREDITS_DISAGREE
            }
            None => return None,
        };
        let explicit = explicit_score(target.is_explicit, candidate.is_explicit)?;
        Some(
            BASE + artist
                + duration
                + album_score(target.album.as_deref(), candidate.album_name.as_deref())
                + explicit
                + context_score(&wanted, &got),
        )
    }

    /// True if valid candidates disagree on album release while `target` names no album.
    pub fn has_conflicting_albums(candidates: &[Song], target: &Target) -> bool {
        if target
            .album
            .as_deref()
            .is_some_and(|a| !a.trim().is_empty())
        {
            return false;
        }
        let comparable: Vec<&Song> = if target.duration_sec.is_some() {
            candidates
                .iter()
                .filter(|c| Self::within_seconds(c, target, DURATION_LIMIT_SEC))
                .collect()
        } else {
            candidates.iter().collect()
        };
        if target.duration_sec.is_some() && comparable.is_empty() {
            return false;
        }
        let distinct: HashSet<String> = comparable
            .into_iter()
            .filter_map(|c| album_key(c.album_name.as_deref()))
            .collect();
        distinct.len() > 1
    }

    /// Resolves a release collision only when one close-duration candidate uniquely carries the
    /// fullest ensemble artist credit (>= 2 credits).
    pub fn uniquely_most_credited_close_match<'a>(
        candidates: &'a [Song],
        target: &Target,
    ) -> Option<&'a Song> {
        let close: Vec<&'a Song> = candidates
            .iter()
            .filter(|c| Self::within_seconds(c, target, DURATION_LIMIT_SEC))
            .collect();
        if close.len() < 2 {
            return None;
        }
        let ranked: Vec<(&'a Song, usize)> = close
            .into_iter()
            .map(|c| (c, artist_names(&c.artist).len()))
            .collect();
        let top_credits = ranked.iter().map(|(_, n)| *n).max()?;
        if top_credits < 2 {
            return None;
        }
        let mut winners = ranked
            .into_iter()
            .filter(|(_, n)| *n == top_credits)
            .map(|(c, _)| c);
        let first = winners.next()?;
        if winners.next().is_some() {
            None
        } else {
            Some(first)
        }
    }

    /// True iff both `target` and `candidate` state runtimes within `seconds` of each other.
    pub fn within_seconds(candidate: &Song, target: &Target, seconds: i32) -> bool {
        let Some(wanted) = target.duration_sec else {
            return false;
        };
        let Some(got) = seconds_of(candidate.duration_text.as_deref()) else {
            return false;
        };
        (wanted - got).abs() <= seconds
    }

    /// Simple boolean match helper.
    pub fn matches(candidate: &Song, title: &str, artist: &str, duration_sec: Option<i32>) -> bool {
        Self::score(
            candidate,
            &Target {
                title: title.to_owned(),
                artist: artist.to_owned(),
                duration_sec,
                ..Target::default()
            },
        )
        .is_some()
    }

    /// True when two known runtimes differ by more than `DURATION_LIMIT_SEC` (30 s).
    pub fn is_severe_mismatch(expected_sec: Option<i32>, actual_sec: Option<i32>) -> bool {
        match (expected_sec, actual_sec) {
            (Some(exp), Some(act)) => (act - exp).abs() > DURATION_LIMIT_SEC,
            _ => false,
        }
    }
}

/// Parses `"3:45"` or `"1:02:03"` as whole seconds, returning `None` for 0 or invalid strings.
pub fn seconds_of(text: Option<&str>) -> Option<i32> {
    let parts: Vec<&str> = text?.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut total: i32 = 0;
    for part in parts {
        let n = part.trim().parse::<i32>().ok()?;
        total = total.checked_mul(60)?.checked_add(n)?;
    }
    (total > 0).then_some(total)
}

fn searchable_title(title: &str, artist: &str) -> String {
    let parts = parse_title(title, artist);
    let mut combined = parts.words;
    combined.extend(parts.versions);
    combined.join(" ")
}

fn primary_artist(artist: &str) -> String {
    let norm = normalize(artist);
    let first = ARTIST_SEPARATORS.split(&norm).next().unwrap_or("").trim();
    if YOUTUBE_TYPE_LABELS.contains(&first) {
        String::new()
    } else {
        first.to_owned()
    }
}

fn parse_title(raw: &str, artist: &str) -> TitleParts {
    let mut versions = BTreeSet::new();
    let mut context = Vec::new();
    let mut text = strip_pt_br_featuring(&normalize(raw));

    for _ in 0..BRACKET_PASSES {
        if !BRACKETED.is_match(&text) {
            break;
        }
        let mut next = String::with_capacity(text.len());
        let mut last = 0usize;
        for caps in BRACKETED.captures_iter(&text) {
            // INFALLIBLE: capture 0 is always present.
            let full = caps.get(0).expect("group 0");
            let inner = caps.get(1).map_or("", |m| m.as_str());
            next.push_str(&text[last..full.start()]);
            classify(inner, &mut versions, &mut context);
            next.push(' ');
            last = full.end();
        }
        next.push_str(&text[last..]);
        text = next;
    }

    if let Some(open) = text.find(['(', '[']) {
        classify(&text[open..], &mut versions, &mut context);
        text.truncate(open);
    }

    for _ in 0..DASH_PASSES {
        let Some(m) = DASH.find(&text) else {
            break;
        };
        let head = text[..m.start()].to_owned();
        let tail = text[m.end()..].to_owned();
        let prefer_tail = is_artist_name(&head, artist)
            || (!has_filing_latin(&head)
                && has_filing_latin(&tail)
                && !is_artist_name(&tail, artist));
        text = if prefer_tail {
            classify(&head, &mut versions, &mut context);
            tail
        } else {
            classify(&tail, &mut versions, &mut context);
            head
        };
    }

    text = FEATURING.replace_all(&text, " ").into_owned();
    let contains_cjk = contains_cjk_char(&text);

    let mut words: Vec<String> = WORD_SPLIT
        .split(&text)
        .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
        .filter(|w| !w.is_empty() && !JOINING_WORDS.contains(&w.as_str()))
        .collect();

    while words.len() > 1 {
        let last = words[words.len() - 1].as_str();
        if TRAILING_NOISE.contains(&last) {
            words.pop();
        } else if TRAILING_TAKES.contains(&last) {
            let take = words.pop().expect("len > 1");
            versions.insert(take);
        } else {
            break;
        }
    }

    let core = if contains_cjk {
        CJK_STRIP.replace_all(&text, "").into_owned()
    } else {
        words.concat()
    };

    TitleParts {
        words,
        core,
        versions,
        context,
    }
}

/// Pure-Rust equivalent of Kotlin's `PT_BR_FEATURING` negative-lookahead regex:
/// `\bpart\.\s+(?!(?:\d|[ivx]+\b))[^)\]]*[)\]]?|\bpart\s+(?:esp\.?|especial)\b[^)\]]*[)\]]?`
/// (input is already lowercased by `normalize`).
fn strip_pt_br_featuring(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut idx = 0usize;

    while idx < text.len() {
        let prev_is_word = idx > 0
            && text[..idx]
                .chars()
                .next_back()
                .is_some_and(is_regex_word_char);

        if !prev_is_word && text[idx..].starts_with("part") {
            let after_part = &text[(idx + 4)..];
            let matched_pt_br = if let Some(after_dot) = after_part.strip_prefix('.') {
                let ws_len = after_dot
                    .char_indices()
                    .find(|(_, c)| !c.is_whitespace())
                    .map_or(after_dot.len(), |(pos, _)| pos);
                if ws_len > 0 {
                    let rest = &after_dot[ws_len..];
                    !starts_with_digit_or_roman_word(rest)
                } else {
                    false
                }
            } else {
                let ws_len = after_part
                    .char_indices()
                    .find(|(_, c)| !c.is_whitespace())
                    .map_or(after_part.len(), |(pos, _)| pos);
                if ws_len > 0 {
                    let rest = &after_part[ws_len..];
                    is_word_prefix(rest, "especial")
                        || is_word_prefix(rest, "esp")
                        || rest.starts_with("esp.")
                } else {
                    false
                }
            };

            if matched_pt_br {
                let rest = &text[idx..];
                let end_rel = rest.find([')', ']']).map_or(rest.len(), |p| p + 1);
                out.push(' ');
                idx += end_rel;
                continue;
            }
        }

        // INFALLIBLE: `idx < text.len()` at a char boundary.
        let ch = text[idx..].chars().next().unwrap_or_default();
        out.push(ch);
        idx += ch.len_utf8();
    }

    out
}

fn is_regex_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word_prefix(s: &str, word: &str) -> bool {
    let Some(rest) = s.strip_prefix(word) else {
        return false;
    };
    rest.chars().next().is_none_or(|c| !is_regex_word_char(c))
}

fn starts_with_digit_or_roman_word(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if first.is_ascii_digit() {
        return true;
    }
    let word_len = s
        .char_indices()
        .find(|(_, c)| !is_regex_word_char(*c))
        .map_or(s.len(), |(pos, _)| pos);
    word_len > 0 && s[..word_len].chars().all(|c| matches!(c, 'i' | 'v' | 'x'))
}

fn classify(segment: &str, versions: &mut BTreeSet<String>, context: &mut Vec<String>) {
    let words: Vec<String> = WORD_SPLIT
        .split(segment)
        .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return;
    }
    if NEUTRAL_SEGMENTS.contains(&words.concat().as_str()) {
        return;
    }
    let marks: Vec<String> = words
        .iter()
        .filter(|w| VERSION_WORDS.contains(&w.as_str()))
        .cloned()
        .collect();
    if !marks.is_empty() {
        versions.extend(marks);
        return;
    }
    for word in words {
        if (word.encode_utf16().count() > 2 || contains_cjk_char(&word))
            && !NOISE_WORDS.contains(&word.as_str())
            && !context.contains(&word)
        {
            context.push(word);
        }
    }
}

fn is_translation_match(wanted: &TitleParts, got: &TitleParts) -> bool {
    let wanted_cjk = contains_cjk_char(&wanted.core) && !has_filing_latin(&wanted.core);
    let got_cjk = contains_cjk_char(&got.core) && !has_filing_latin(&got.core);
    let wanted_latin = has_filing_latin(&wanted.core);
    let got_latin = has_filing_latin(&got.core);

    if (wanted_cjk && got_latin) || (got_cjk && wanted_latin) {
        if wanted.context.concat() == got.core {
            return true;
        }
        if got.context.concat() == wanted.core {
            return true;
        }
    }
    false
}

fn is_cjk_block(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul Syllables
    )
}

fn contains_cjk_char(text: &str) -> bool {
    text.chars().any(is_cjk_block)
}

fn is_artist_name(text: &str, artist: &str) -> bool {
    if artist.trim().is_empty() {
        return false;
    }
    let words: Vec<String> = WORD_SPLIT
        .split(text)
        .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return false;
    }
    let credited: HashSet<String> = WORD_SPLIT
        .split(&normalize(artist))
        .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().all(|w| credited.contains(w))
}

fn normalize(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let lower = nfkc.to_lowercase().replace('&', " and ");
    let open = CJK_OPEN.replace_all(&lower, "(");
    CJK_CLOSE.replace_all(&open, ")").into_owned()
}

fn has_filing_latin(text: &str) -> bool {
    WORD_SPLIT
        .split(text)
        .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
        .filter(|w| !w.is_empty())
        .any(|w| {
            LATIN.is_match(&w)
                && !VERSION_WORDS.contains(&w.as_str())
                && !NOISE_WORDS.contains(&w.as_str())
                && !TRAILING_NOISE.contains(&w.as_str())
                && !JOINING_WORDS.contains(&w.as_str())
        })
}

fn script_mates(raw: &str) -> Vec<String> {
    let text = normalize(raw);
    let Some(dash) = DASH.find(&text) else {
        return Vec::new();
    };
    let head = searchable_title(&text[..dash.start()], "");
    let tail = searchable_title(&text[dash.end()..], "");
    if head.trim().is_empty() || tail.trim().is_empty() || head == tail {
        return Vec::new();
    }
    if romaji_of(&head) == tail || romaji_of(&tail) == head {
        vec![head, tail]
    } else {
        Vec::new()
    }
}

/// Hepburn transliteration for Hiragana/Katakana; kanji, Latin, and digits pass through unchanged.
pub fn romaji_of(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut geminate = false;
    let mut last_vowel: Option<char> = None;

    let emit =
        |roma: &str, out: &mut String, geminate: &mut bool, last_vowel: &mut Option<char>| {
            let owned;
            let r = if *geminate {
                *geminate = false;
                if roma.starts_with("ch") || roma.starts_with("ts") {
                    owned = format!("t{roma}");
                    owned.as_str()
                } else if roma.starts_with("sh") {
                    owned = format!("s{roma}");
                    owned.as_str()
                } else if let Some(first) = roma.chars().next() {
                    owned = format!("{first}{roma}");
                    owned.as_str()
                } else {
                    roma
                }
            } else {
                roma
            };
            out.push_str(r);
            *last_vowel = r
                .chars()
                .rev()
                .find(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'));
        };

    while i < chars.len() {
        let c = fold_katakana(chars[i]);
        match c {
            'っ' => {
                geminate = true;
                i += 1;
            }
            'ー' => {
                if let Some(v) = last_vowel {
                    out.push(v);
                }
                i += 1;
            }
            'ん' => {
                let peek = chars
                    .get(i + 1)
                    .copied()
                    .and_then(|nc| kana_romaji(fold_katakana(nc)))
                    .and_then(|r| r.chars().next());
                let syllabic = if matches!(peek, Some('b' | 'm' | 'p')) {
                    "m"
                } else {
                    "n"
                };
                emit(syllabic, &mut out, &mut geminate, &mut last_vowel);
                i += 1;
            }
            _ => match kana_romaji(c) {
                None => {
                    out.push(c);
                    last_vowel = None;
                    geminate = false;
                    i += 1;
                }
                Some(base) => {
                    let small = chars
                        .get(i + 1)
                        .copied()
                        .and_then(|nc| small_kana(fold_katakana(nc)));
                    if let Some(small) = small {
                        let cons = cons_override(base)
                            .unwrap_or_else(|| &base[..base.len().saturating_sub(1)]);
                        let glide = if matches!(cons, "sh" | "ch" | "j") {
                            small.strip_prefix('y').unwrap_or(small)
                        } else {
                            small
                        };
                        let combined = format!("{cons}{glide}");
                        emit(&combined, &mut out, &mut geminate, &mut last_vowel);
                        i += 2;
                    } else {
                        emit(base, &mut out, &mut geminate, &mut last_vowel);
                        i += 1;
                    }
                }
            },
        }
    }
    out
}

fn fold_katakana(c: char) -> char {
    if ('ァ'..='ヶ').contains(&c) {
        char::from_u32((c as u32) - 0x60).unwrap_or(c)
    } else {
        c
    }
}

fn cons_override(base: &str) -> Option<&'static str> {
    if base.starts_with("shi") {
        Some("sh")
    } else if base.starts_with("chi") {
        Some("ch")
    } else if base.starts_with("tsu") {
        Some("ts")
    } else if base.starts_with("fu") {
        Some("f")
    } else if base.starts_with("ji") {
        Some("j")
    } else if base.starts_with("zu") {
        Some("z")
    } else {
        None
    }
}

fn small_kana(c: char) -> Option<&'static str> {
    match c {
        'ゃ' => Some("ya"),
        'ゅ' => Some("yu"),
        'ょ' => Some("yo"),
        'ぁ' => Some("a"),
        'ぃ' => Some("i"),
        'ぅ' => Some("u"),
        'ぇ' => Some("e"),
        'ぉ' => Some("o"),
        _ => None,
    }
}

fn kana_romaji(c: char) -> Option<&'static str> {
    Some(match c {
        'あ' | 'ぁ' => "a",
        'い' | 'ぃ' => "i",
        'う' | 'ぅ' => "u",
        'え' | 'ぇ' => "e",
        'お' | 'ぉ' => "o",
        'か' | 'ゕ' => "ka",
        'き' => "ki",
        'く' => "ku",
        'け' | 'ゖ' => "ke",
        'こ' => "ko",
        'が' => "ga",
        'ぎ' => "gi",
        'ぐ' => "gu",
        'げ' => "ge",
        'ご' => "go",
        'さ' => "sa",
        'し' => "shi",
        'す' => "su",
        'せ' => "se",
        'そ' => "so",
        'ざ' => "za",
        'じ' | 'ぢ' => "ji",
        'ず' | 'づ' => "zu",
        'ぜ' => "ze",
        'ぞ' => "zo",
        'た' => "ta",
        'ち' => "chi",
        'つ' => "tsu",
        'て' => "te",
        'と' => "to",
        'だ' => "da",
        'で' => "de",
        'ど' => "do",
        'な' => "na",
        'に' => "ni",
        'ぬ' => "nu",
        'ね' => "ne",
        'の' => "no",
        'は' => "ha",
        'ひ' => "hi",
        'ふ' => "fu",
        'へ' => "he",
        'ほ' => "ho",
        'ば' => "ba",
        'び' => "bi",
        'ぶ' => "bu",
        'べ' => "be",
        'ぼ' => "bo",
        'ぱ' => "pa",
        'ぴ' => "pi",
        'ぷ' => "pu",
        'ぺ' => "pe",
        'ぽ' => "po",
        'ま' => "ma",
        'み' => "mi",
        'む' => "mu",
        'め' => "me",
        'も' => "mo",
        'や' | 'ゃ' => "ya",
        'ゆ' | 'ゅ' => "yu",
        'よ' | 'ょ' => "yo",
        'ら' => "ra",
        'り' => "ri",
        'る' => "ru",
        'れ' => "re",
        'ろ' => "ro",
        'わ' | 'ゎ' => "wa",
        'を' => "wo",
        'ん' => "n",
        'ゔ' => "vu",
        _ => return None,
    })
}

fn artist_score(wanted: &str, got: &str) -> Option<i32> {
    let want = artist_names(wanted);
    let have = artist_names(got);
    if want.is_empty() || have.is_empty() {
        return Some(0);
    }
    if !TrackMatcher::shares_artist(wanted, got) {
        return None;
    }
    Some(if want == have {
        ARTIST_EXACT
    } else {
        ARTIST_SHARED
    })
}

fn artist_names(value: &str) -> HashSet<Vec<String>> {
    let norm = normalize(value);
    ARTIST_SEPARATORS
        .split(&norm)
        .map(|name| {
            WORD_SPLIT
                .split(name)
                .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
                .filter(|w| w.encode_utf16().count() > 1)
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

fn same_artist(a: &[String], b: &[String]) -> bool {
    run_of(a, b) || run_of(b, a)
}

fn run_of(outer: &[String], inner: &[String]) -> bool {
    if inner.is_empty() || inner.len() > outer.len() {
        return false;
    }
    outer.windows(inner.len()).any(|w| w == inner)
}

fn duration_score(wanted: Option<i32>, got: Option<i32>, allow_video_drift: bool) -> Option<i32> {
    let (Some(wanted), Some(got)) = (wanted, got) else {
        return Some(0);
    };
    let drift = (wanted - got).abs();
    if drift > DURATION_LIMIT_SEC {
        if allow_video_drift && drift <= VIDEO_DURATION_LIMIT_SEC {
            Some(0)
        } else {
            None
        }
    } else if drift <= DURATION_TIGHT_SEC {
        Some(DURATION_TIGHT)
    } else {
        Some(DURATION_LOOSE)
    }
}

fn album_score(wanted: Option<&str>, got: Option<&str>) -> i32 {
    let (Some(want), Some(have)) = (album_key(wanted), album_key(got)) else {
        return 0;
    };
    if want == have { ALBUM_EXACT } else { 0 }
}

fn explicit_score(wanted: Option<bool>, got: Option<bool>) -> Option<i32> {
    match (wanted, got) {
        (None, _) | (_, None) => Some(0),
        (Some(w), Some(g)) if w != g => None,
        _ => Some(EXPLICIT_EXACT),
    }
}

fn album_version_markers(album: Option<&str>) -> BTreeSet<String> {
    match album.filter(|a| !a.trim().is_empty()) {
        Some(a) => parse_title(a, "").versions,
        None => BTreeSet::new(),
    }
}

fn album_key(value: Option<&str>) -> Option<String> {
    let mut text = normalize(value.unwrap_or("")).trim().to_owned();
    if text.is_empty() {
        return None;
    }
    for _ in 0..BRACKET_PASSES {
        text = BRACKETED.replace_all(&text, " ").into_owned();
    }
    let joined: String = WORD_SPLIT
        .split(&text)
        .map(|w| NON_ALNUM.replace_all(w, "").into_owned())
        .filter(|w| !w.is_empty() && !ALBUM_NOISE_WORDS.contains(&w.as_str()))
        .collect();
    (!joined.is_empty()).then_some(joined)
}

fn context_score(wanted: &TitleParts, got: &TitleParts) -> i32 {
    if wanted.context.iter().any(|w| got.context.contains(w)) {
        CONTEXT_SHARED
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str, artist: &str, duration: Option<&str>, album: Option<&str>) -> Song {
        Song {
            video_id: format!("id:{title}:{artist}"),
            title: title.to_owned(),
            artist: artist.to_owned(),
            duration_text: duration.map(str::to_owned),
            album_name: album.map(str::to_owned),
            ..Song::default()
        }
    }

    #[test]
    fn matches_across_packaging_cjk_and_film_credits() {
        assert!(TrackMatcher::matches(
            &song("Bohemian Rhapsody (Remastered 2011)", "Queen", None, None),
            "Bohemian Rhapsody",
            "Queen",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Sunflower", "Post Malone, Swae Lee", None, None),
            "Sunflower (feat. Swae Lee)",
            "Post Malone",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Don't Stop Me Now", "QUEEN", None, None),
            "Dont Stop Me Now",
            "Queen",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("夜に駆ける", "YOASOBI", Some("4:20"), None),
            "夜 に 駆ける！",
            "YOASOBI",
            Some(261)
        ));
        assert!(TrackMatcher::matches(
            &song("좋은 날", "IU", None, None),
            "좋은날",
            "IU",
            None
        ));
        // Short Unicode titles stay strict.
        assert!(!TrackMatcher::matches(
            &song("愛情", "Artist", None, None),
            "愛",
            "Artist",
            None
        ));
        assert!(!TrackMatcher::matches(
            &song("봄날", "Artist", None, None),
            "봄",
            "Artist",
            None
        ));

        // Film credits & upload labelling.
        assert!(TrackMatcher::matches(
            &song("Paniyon Sa", "Atif Aslam, Tulsi Kumar", Some("4:07"), None),
            "Paniyon Sa (From \"Satyameva Jayate\")",
            "Atif Aslam",
            Some(247)
        ));
        assert!(TrackMatcher::matches(
            &song("Tum Hi Ho", "Arijit Singh", None, None),
            "Tum Hi Ho Full Song",
            "Arijit Singh",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Kesariya", "Arijit Singh", None, None),
            "Kesariya - Brahmastra | Official Video",
            "Arijit Singh",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Believer", "Imagine Dragons", None, None),
            "Imagine Dragons - Believer",
            "Imagine Dragons",
            None
        ));
    }

    #[test]
    fn rejects_covers_wrong_takes_and_extreme_durations() {
        assert!(!TrackMatcher::matches(
            &song("The Show Must Go On", "Queen", None, None),
            "Bohemian Rhapsody",
            "Queen",
            None
        ));
        assert!(!TrackMatcher::matches(
            &song("Hurt", "Johnny Cash", None, None),
            "Hurt",
            "Nine Inch Nails",
            None
        ));
        assert!(!TrackMatcher::matches(
            &song("No One Knows", "Queens of the Stone Age", None, None),
            "No One Knows",
            "Queen",
            None
        ));
        assert!(!TrackMatcher::matches(
            &song("Shape of You (Acoustic)", "Ed Sheeran", None, None),
            "Shape of You",
            "Ed Sheeran",
            None
        ));
        assert!(!TrackMatcher::matches(
            &song("Shape of You", "Ed Sheeran", None, None),
            "Shape of You (Acoustic)",
            "Ed Sheeran",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Creep (Live)", "Radiohead", None, None),
            "Creep [Live]",
            "Radiohead",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Africa", "Toto", None, None),
            "Africa (Album Version)",
            "Toto",
            None
        ));
        assert!(TrackMatcher::matches(
            &song("Clocks", "Coldplay", None, None),
            "Clocks (Radio Edit)",
            "Coldplay",
            None
        ));
        assert!(!TrackMatcher::matches(
            &song("Levitating", "Dua Lipa", Some("1:00:12"), None),
            "Levitating",
            "Dua Lipa",
            Some(203)
        ));
        assert!(TrackMatcher::matches(
            &song("Levitating", "Dua Lipa", Some("3:25"), None),
            "Levitating",
            "Dua Lipa",
            Some(203)
        ));
        assert!(!TrackMatcher::matches(
            &song("Let Me Live (Live)", "Queen", Some("4:45"), None),
            "Let Me Live",
            "Queen",
            Some(285)
        ));
        assert!(TrackMatcher::matches(
            &song("Let Me Live", "Queen", Some("4:45"), None),
            "Let Me Live",
            "Queen",
            Some(285)
        ));
    }

    #[test]
    fn credit_override_explicit_flag_and_video_switch() {
        // Exact runtime bridges composer vs singer credit.
        assert!(TrackMatcher::matches(
            &song("Jhak Maar Ke", "Neeraj Shridhar", Some("3:53"), None),
            "Jhak Maar Ke",
            "Pritam",
            Some(233)
        ));
        assert!(!TrackMatcher::matches(
            &song("Jhak Maar Ke", "Leo Lz Mix", Some("4:01"), None),
            "Jhak Maar Ke",
            "Pritam",
            Some(233)
        ));
        assert!(!TrackMatcher::matches(
            &song("Jhak Maar Ke", "Neeraj Shridhar", None, None),
            "Jhak Maar Ke",
            "Pritam",
            None
        ));

        // Explicit flag mismatch vetoes clean edition.
        let mut clean = song("Starboy", "The Weeknd", Some("3:50"), Some("Starboy"));
        clean.is_explicit = Some(false);
        let mut uncensored = clean.clone();
        uncensored.video_id = "uncensored".to_owned();
        uncensored.is_explicit = Some(true);
        let explicit_target = Target {
            title: "Starboy".to_owned(),
            artist: "The Weeknd".to_owned(),
            duration_sec: Some(230),
            album: Some("Starboy".to_owned()),
            is_explicit: Some(true),
            is_video: false,
        };
        assert_eq!(None, TrackMatcher::score(&clean, &explicit_target));
        assert_eq!(
            Some(&uncensored),
            TrackMatcher::best(&[clean, uncensored.clone()], &explicit_target)
        );

        // Brown Rang regression: credited audio beats unrelated exact-runtime row.
        let brown_target = Target {
            title: "Brown Rang".to_owned(),
            artist: "Yo Yo Honey Singh".to_owned(),
            duration_sec: Some(211),
            ..Target::default()
        };
        let wanted_audio = song(
            "Brown Rang",
            "Yo Yo Honey Singh",
            Some("2:59"),
            Some("International Villager"),
        );
        let wrong_exact = song(
            "Brown Rang",
            "Lovely, Jais Rikhi, Love Sagar",
            Some("3:30"),
            Some("Brown Rang"),
        );
        assert_eq!(
            vec![&wanted_audio],
            TrackMatcher::ranked(&[wrong_exact, wanted_audio.clone()], &brown_target)
        );

        // Manual video-to-audio switch accepts official song despite multi-minute visual intro.
        let video_target = Target {
            title: "Big Dawgs".to_owned(),
            artist: "Hanumankind, Kalmi".to_owned(),
            duration_sec: Some(391),
            is_video: true,
            ..Target::default()
        };
        let official = song("Big Dawgs", "Hanumankind, Kalmi", Some("3:11"), None);
        let cover = song("Big Dawgs", "Unrelated Cover Artist", Some("6:31"), None);
        assert_eq!(
            None,
            TrackMatcher::best(&[official.clone(), cover.clone()], &video_target)
        );
        assert_eq!(
            Some(&official),
            TrackMatcher::best_official_audio_for_video(&[cover, official.clone()], &video_target)
        );
    }

    #[test]
    fn album_collisions_and_album_version_markers() {
        let wanted = song(
            "Brown Rang",
            "Yo Yo Honey Singh",
            Some("2:59"),
            Some("International Villager"),
        );
        let closer = song(
            "Brown Rang",
            "Yo Yo Honey Singh",
            Some("2:54"),
            Some("Chaar Ikke"),
        );
        assert!(TrackMatcher::has_conflicting_albums(
            &[closer.clone(), wanted.clone()],
            &Target {
                title: "Brown Rang".to_owned(),
                artist: "Yo Yo Honey Singh".to_owned(),
                duration_sec: Some(175),
                ..Target::default()
            }
        ));

        let full = song(
            "Ek Dil Ek Jaan",
            "Shivam Pathak, Shashwat Singh",
            Some("3:40"),
            Some("Padmaavat"),
        );
        let short = song(
            "Ek Dil Ek Jaan",
            "Shivam Pathak",
            Some("3:41"),
            Some("Romance"),
        );
        assert_eq!(
            Some(&full),
            TrackMatcher::uniquely_most_credited_close_match(
                &[short, full.clone()],
                &Target {
                    title: "Ek Dil Ek Jaan".to_owned(),
                    artist: "Shivam Pathak".to_owned(),
                    duration_sec: Some(220),
                    ..Target::default()
                }
            )
        );

        // Album-level remix rescues remix request, stays ignored on plain request.
        let kalyani_remix = song(
            "KALYANI",
            "ARJN, KDS, FIFTY4, Shreya Ghoshal",
            Some("4:29"),
            Some("KALYANI (Remix)"),
        );
        assert!(TrackMatcher::matches(
            &kalyani_remix,
            "KALYANI (Remix)",
            "ARJN, KDS, FIFTY4 & Shreya Ghoshal",
            Some(270)
        ));
        assert!(TrackMatcher::matches(
            &kalyani_remix,
            "KALYANI",
            "ARJN, KDS, FIFTY4 & Shreya Ghoshal",
            Some(269)
        ));

        // Album-level instrumental is bidirectional (rejects vocal, accepts instrumental).
        let inst_twin = song(
            "Am I Dreaming",
            "Metro Boomin",
            Some("4:16"),
            Some("METRO BOOMIN PRESENTS SPIDER-MAN (METROVERSE INSTRUMENTAL EDITION)"),
        );
        assert!(!TrackMatcher::matches(
            &inst_twin,
            "Am I Dreaming",
            "Metro Boomin, A$AP Rocky & Roisee",
            Some(257)
        ));
        assert!(TrackMatcher::matches(
            &inst_twin,
            "Am I Dreaming (Instrumental)",
            "Metro Boomin",
            Some(257)
        ));
    }

    #[test]
    fn portuguese_guest_queries_kana_romaji_and_cjk_translations() {
        assert_eq!(
            vec!["o mundo é mal froid", "o mundo é mal"],
            TrackMatcher::queries(&Target::new("O Mundo É Mal (part. Clara Lima)", "Froid"))
        );
        assert_eq!(
            vec!["song part"],
            TrackMatcher::queries(&Target::new("Song (Part 1)", ""))
        );
        assert_eq!(
            vec!["part of me"],
            TrackMatcher::queries(&Target::new("Part of Me", ""))
        );
        assert_eq!(
            vec!["lud session 2"],
            TrackMatcher::queries(&Target::new("Lud Session 2", "Música"))
        );
        assert_eq!(
            vec!["dai zero kan 10-feet", "dai zero kan"],
            TrackMatcher::queries(&Target::new("第ゼロ感 - Dai Zero Kan", "10-FEET"))
        );
        assert_eq!(
            vec!["紅蓮華 lisa", "紅蓮華"],
            TrackMatcher::queries(&Target::new("紅蓮華 - LiSA", "LiSA"))
        );
        assert_eq!(
            vec!["真夜中のドア stay miki matsubara", "真夜中のドア stay"],
            TrackMatcher::queries(&Target::new(
                "真夜中のドア〜stay with me - Mayonaka no Door~stay with me",
                "Miki Matsubara"
            ))
        );
        assert_eq!(
            vec!["koikogare mugi", "koikogare", "コイコガレ"],
            TrackMatcher::queries(&Target::new("コイコガレ - koikogare", "MUGI"))
        );
        assert_eq!(
            vec!["コイコガレ mugi", "コイコガレ", "koikogare"],
            TrackMatcher::queries(&Target::new("コイコガレ", "MUGI"))
        );

        // Hepburn romaji checks.
        assert_eq!("koikogare", romaji_of("コイコガレ"));
        assert_eq!("abunaikioku", romaji_of("アブナイキオク"));
        assert_eq!("kiseki", romaji_of("きせき"));
        assert_eq!("gakkou", romaji_of("がっこう"));
        assert_eq!("shimbun", romaji_of("しんぶん"));
        assert_eq!("kyou", romaji_of("きょう"));
        assert_eq!("第zero感", romaji_of("第ゼロ感"));
        assert_eq!("doa", romaji_of("ドア"));
        assert_eq!("sharuru", romaji_of("シャルル"));
        assert_eq!("janki", romaji_of("ジャンキ"));
        assert_eq!("chotto", romaji_of("ちょっと"));
        assert_eq!("jetto", romaji_of("ジェット"));
        assert_eq!("renai", romaji_of("レンアイ"));

        // Bracketed CJK translation vs Latin filing.
        assert!(TrackMatcher::matches(
            &song("Love", "Singer", Some("2:53"), None),
            "愛(Love)",
            "Singer",
            Some(174)
        ));
        assert!(TrackMatcher::matches(
            &song("Love (愛)", "Singer", Some("2:53"), None),
            "愛",
            "Singer",
            Some(174)
        ));
        assert!(!TrackMatcher::matches(
            &song("Film Name", "Some Artist", Some("4:07"), None),
            "Song Name (From \"Film Name\")",
            "Singer",
            Some(247)
        ));
        assert!(!TrackMatcher::matches(
            &song("Night", "Singer", Some("4:20"), None),
            "夜に駆ける (Racing Into The Night)",
            "Singer",
            Some(260)
        ));

        // Katakana middle dot joins one artist name.
        assert!(!TrackMatcher::shares_artist(
            "ジョン・レノン",
            "ジョン・ウィリアムズ"
        ));
        assert!(TrackMatcher::shares_artist(
            "ジョン・レノン",
            "ジョン・レノン、オノ・ヨーコ"
        ));

        // Severe mismatch helper (São Paulo regression).
        assert!(TrackMatcher::is_severe_mismatch(Some(302), Some(209)));
        assert!(!TrackMatcher::is_severe_mismatch(Some(302), Some(288)));
        assert!(!TrackMatcher::is_severe_mismatch(None, Some(209)));
    }
}
