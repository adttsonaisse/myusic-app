use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::LyricLine;

/// Boxed `Send` future alias used by [`YouTubeLyricsBackend`] without needing
/// `async-trait`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

static YOUTUBE_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_-]{11}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid YOUTUBE_ID regex")
});

/// Trait implemented by `bitchord-innertube` (Wave 2) to supply InnerTube JSON
/// responses to [`YouTubeMusicLyrics`] and [`YouTubeTranscriptLyrics`] without
/// creating a circular dependency.
pub trait YouTubeLyricsBackend: Send + Sync {
    fn next<'a>(&'a self, video_id: &'a str) -> BoxFuture<'a, Option<Value>>;
    fn browse<'a>(
        &'a self,
        browse_id: &'a str,
        params: Option<&'a str>,
    ) -> BoxFuture<'a, Option<Value>>;
    fn transcript<'a>(&'a self, video_id: &'a str) -> BoxFuture<'a, Option<Value>>;
}

/// Lyrics exposed by YouTube Music's Lyrics tab for the exact playing video.
pub struct YouTubeMusicLyrics;

impl YouTubeMusicLyrics {
    pub async fn lyrics(
        backend: &dyn YouTubeLyricsBackend,
        video_id: &str,
    ) -> Option<Vec<LyricLine>> {
        if !YOUTUBE_ID.is_match(video_id) {
            return None;
        }
        let next = backend.next(video_id).await?;
        let tabs = objects_named(&next, "tabRenderer");
        let endpoint = tabs
            .iter()
            .find(|tab| {
                youtube_strings_map(tab)
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case("Lyrics"))
            })
            .and_then(|tab| objects_named_map(tab, "browseEndpoint").into_iter().next())
            .or_else(|| {
                tabs.iter()
                    .skip(1)
                    .find_map(|tab| objects_named_map(tab, "browseEndpoint").into_iter().next())
            })?;

        let browse_id = endpoint.get("browseId")?.as_str()?;
        let params = endpoint.get("params").and_then(Value::as_str);
        let page = backend.browse(browse_id, params).await?;
        let shelf = objects_named(&page, "musicDescriptionShelfRenderer")
            .into_iter()
            .next()?;
        let text = shelf
            .get("description")
            .map(youtube_strings)
            .unwrap_or_default()
            .join("");
        let lines: Vec<LyricLine> = text
            .trim()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| LyricLine::new(0, s))
            .collect();
        if lines.is_empty() { None } else { Some(lines) }
    }
}

/// Timed YouTube transcript/captions for the exact playing video.
pub struct YouTubeTranscriptLyrics;

impl YouTubeTranscriptLyrics {
    pub async fn lyrics(
        backend: &dyn YouTubeLyricsBackend,
        video_id: &str,
    ) -> Option<Vec<LyricLine>> {
        if !YOUTUBE_ID.is_match(video_id) {
            return None;
        }
        let response = backend.transcript(video_id).await?;
        let mut lines: Vec<LyricLine> = objects_named(&response, "transcriptCueRenderer")
            .into_iter()
            .filter_map(|cue| {
                let start = match cue.get("startOffsetMs")? {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.trim().parse::<i64>().ok(),
                    _ => None,
                }?;
                let joined = cue
                    .get("cue")
                    .map(youtube_strings)
                    .unwrap_or_default()
                    .join("");
                let trimmed = joined.trim_matches(|c| matches!(c, ' ' | '\n' | '♪'));
                if trimmed.is_empty() {
                    None
                } else {
                    Some(LyricLine::new(start, trimmed))
                }
            })
            .collect();
        lines.sort_by_key(|l| l.time_ms);
        if lines.is_empty() { None } else { Some(lines) }
    }
}

/// Recursively collects YouTube text strings from `text`/`simpleText` leaves
/// without crashing when `"text"` is itself a nested runs object.
pub fn youtube_strings(value: &Value) -> Vec<String> {
    match value {
        Value::Null => Vec::new(),
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Bool(b) => vec![b.to_string()],
        Value::Array(arr) => arr.iter().flat_map(youtube_strings).collect(),
        Value::Object(map) => youtube_strings_map(map),
    }
}

fn youtube_strings_map(map: &Map<String, Value>) -> Vec<String> {
    if let Some(direct) =
        primitive_text(map.get("text")).or_else(|| primitive_text(map.get("simpleText")))
    {
        return vec![direct];
    }
    map.values().flat_map(youtube_strings).collect()
}

fn primitive_text(val: Option<&Value>) -> Option<String> {
    match val? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn objects_named<'a>(value: &'a Value, name: &str) -> Vec<&'a Map<String, Value>> {
    let mut out = Vec::new();
    collect_objects_named(value, name, &mut out);
    out
}

fn objects_named_map<'a>(map: &'a Map<String, Value>, name: &str) -> Vec<&'a Map<String, Value>> {
    let mut out = Vec::new();
    for (k, v) in map {
        if k == name
            && let Some(obj) = v.as_object()
        {
            out.push(obj);
        }
        collect_objects_named(v, name, &mut out);
    }
    out
}

fn collect_objects_named<'a>(value: &'a Value, name: &str, out: &mut Vec<&'a Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == name
                    && let Some(obj) = v.as_object()
                {
                    out.push(obj);
                }
                collect_objects_named(v, name, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_objects_named(v, name, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FakeBackend;

    impl YouTubeLyricsBackend for FakeBackend {
        fn next<'a>(&'a self, _video_id: &'a str) -> BoxFuture<'a, Option<Value>> {
            Box::pin(async {
                Some(json!({
                    "tabs": [
                        { "tabRenderer": { "title": "Up next" } },
                        {
                            "tabRenderer": {
                                "title": "Lyrics",
                                "endpoint": { "browseEndpoint": { "browseId": "MPLYt_abc" } }
                            }
                        }
                    ]
                }))
            })
        }

        fn browse<'a>(
            &'a self,
            browse_id: &'a str,
            _params: Option<&'a str>,
        ) -> BoxFuture<'a, Option<Value>> {
            let matches = browse_id == "MPLYt_abc";
            Box::pin(async move {
                matches.then(|| {
                    json!({
                        "contents": {
                            "musicDescriptionShelfRenderer": {
                                "description": { "runs": [{ "text": "Line one\n\nLine two" }] }
                            }
                        }
                    })
                })
            })
        }

        fn transcript<'a>(&'a self, _video_id: &'a str) -> BoxFuture<'a, Option<Value>> {
            Box::pin(async {
                Some(json!({
                    "cues": [
                        { "transcriptCueRenderer": { "startOffsetMs": "5000", "cue": { "simpleText": "♪ second ♪" } } },
                        { "transcriptCueRenderer": { "startOffsetMs": "1200", "cue": { "simpleText": "first" } } }
                    ]
                }))
            })
        }
    }

    #[test]
    fn youtube_nested_text_objects_are_traversed_without_crashing() {
        let response: Value =
            serde_json::from_str(r#"{"text":{"runs":[{"text":"Lyrics"}]}}"#).expect("valid");
        assert_eq!(youtube_strings(&response), vec!["Lyrics"]);
    }

    #[tokio::test]
    async fn youtube_music_and_transcript_lyrics_extract_from_backend() {
        let backend = FakeBackend;
        let music = YouTubeMusicLyrics::lyrics(&backend, "dQw4w9WgXcQ")
            .await
            .expect("music lyrics");
        let music_texts: Vec<&str> = music.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(music_texts, vec!["Line one", "Line two"]);

        let captions = YouTubeTranscriptLyrics::lyrics(&backend, "dQw4w9WgXcQ")
            .await
            .expect("transcript");
        assert_eq!(captions.len(), 2);
        assert_eq!(captions[0].time_ms, 1_200);
        assert_eq!(captions[0].text, "first");
        assert_eq!(captions[1].time_ms, 5_000);
        assert_eq!(captions[1].text, "second");

        assert!(YouTubeMusicLyrics::lyrics(&backend, "bad").await.is_none());
    }
}
