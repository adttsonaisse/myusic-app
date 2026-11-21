use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::http::{LyricsHttp, build_url};
use crate::{
    LyricAlignment, LyricLine, LyricWord, TimedRun, line_alignments, with_instrumental_gaps,
    words_from_runs,
};

const MIRRORS: &[&str] = &[
    "https://lyricsplus.prjktla.my.id",
    "https://lyricsplus.atomix.one",
    "https://lyricsplus.binimum.org",
    "https://lyricsplus.prjktla.workers.dev",
    "https://lyricsplus-seven.vercel.app",
    "https://lyrics-plus-backend.vercel.app",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsPlusResponse {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub lyrics: Option<Vec<LyricsPlusLine>>,
    pub metadata: Option<LyricsPlusMetadata>,
}

impl LyricsPlusResponse {
    /// Declared voices keyed the way lines refer to them (by alias if present).
    pub fn agent_types(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        if let Some(agents) = self.metadata.as_ref().and_then(|m| m.agents.as_ref()) {
            for (id, agent) in agents {
                if let Some(kind) = agent.kind.as_ref() {
                    let key = agent.alias.as_deref().unwrap_or(id.as_str());
                    out.insert(key.to_owned(), kind.clone());
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsPlusMetadata {
    pub agents: Option<HashMap<String, LyricsPlusAgent>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsPlusAgent {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsPlusLine {
    pub time: Option<i64>,
    pub duration: Option<i64>,
    pub text: Option<String>,
    pub syllabus: Option<Vec<LyricsPlusSyllable>>,
    /// Raw JSON: object carrying `singer` on v2, or array of tags on older shape.
    pub element: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsPlusSyllable {
    pub time: Option<i64>,
    pub duration: Option<i64>,
    pub text: Option<String>,
}

/// Syllable-timed lyrics from LyricsPlus (YouLy+ backend) across volunteer
/// mirrors raced concurrently, remembering the last responsive mirror.
#[derive(Debug, Default)]
pub struct LyricsPlus {
    last_good: RwLock<Option<String>>,
}

impl LyricsPlus {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn lyrics(
        &self,
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
        isrc: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let mirrors: Vec<String> = MIRRORS.iter().map(|s| (*s).to_owned()).collect();
        self.lyrics_with_mirrors(http, &mirrors, title, artist, duration_ms, album, isrc)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn lyrics_with_mirrors(
        &self,
        http: &LyricsHttp,
        mirrors: &[String],
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
        isrc: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let mut hosts: Vec<String> = Vec::with_capacity(mirrors.len());
        if let Ok(guard) = self.last_good.read()
            && let Some(prev) = guard.as_ref()
            && mirrors.iter().any(|m| m == prev)
        {
            hosts.push(prev.clone());
        }
        for m in mirrors {
            if !hosts.contains(m) {
                hosts.push(m.clone());
            }
        }

        let mut set = JoinSet::new();
        for host in hosts {
            let http = http.clone();
            let title = title.to_owned();
            let artist = artist.to_owned();
            let album = album.map(ToOwned::to_owned);
            let isrc = isrc.map(ToOwned::to_owned);
            set.spawn(async move {
                let res = Self::fetch(
                    &http,
                    &host,
                    &title,
                    &artist,
                    duration_ms,
                    album.as_deref(),
                    isrc.as_deref(),
                )
                .await;
                (host, res)
            });
        }

        while let Some(joined) = set.join_next().await {
            if let Ok((host, Some(lines))) = joined
                && !lines.is_empty()
            {
                if let Ok(mut guard) = self.last_good.write() {
                    *guard = Some(host);
                }
                set.abort_all();
                return Some(lines);
            }
        }
        None
    }

    async fn fetch(
        http: &LyricsHttp,
        host: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
        isrc: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let mut params: Vec<(&str, String)> =
            vec![("title", title.to_owned()), ("artist", artist.to_owned())];
        let seconds = duration_ms / 1_000;
        if seconds > 0 {
            params.push(("duration", seconds.to_string()));
        }
        if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
            params.push(("album", alb.to_owned()));
        }
        if let Some(code) = isrc.filter(|s| !s.trim().is_empty()) {
            params.push(("isrc", code.to_owned()));
        }
        let borrowed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = build_url(&format!("{host}/v2/lyrics/get"), &borrowed)?;
        let body = http.get(&url).await?;
        let response: LyricsPlusResponse = serde_json::from_str(&body).ok()?;
        let lines = Self::parse(&response);
        if lines.is_empty() { None } else { Some(lines) }
    }

    pub fn parse(response: &LyricsPlusResponse) -> Vec<LyricLine> {
        let mut sung: Vec<(LyricLine, Option<&Value>)> = response
            .lyrics
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|line| {
                let start = line.time?;
                let words = merge_syllables(line.syllabus.as_deref().unwrap_or_default());
                let built = if !words.is_empty() {
                    let first_start = words[0].start_ms;
                    let joined = words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    LyricLine {
                        time_ms: start.min(first_start),
                        text: joined,
                        words,
                        ..Default::default()
                    }
                } else {
                    let text = line.text.as_deref().filter(|s| !s.trim().is_empty())?;
                    LyricLine {
                        time_ms: start,
                        text: text.trim().to_owned(),
                        sung_until_ms: line.duration.filter(|&d| d > 0).map(|d| start + d),
                        ..Default::default()
                    }
                };
                Some((built, line.element.as_ref()))
            })
            .collect();

        sung.sort_by_key(|(line, _)| line.time_ms);

        let singers: Vec<Option<String>> = sung
            .iter()
            .map(|(_, elem)| singer_of(*elem).map(ToOwned::to_owned))
            .collect();
        let agent_types = response.agent_types();
        let sides = line_alignments(&singers, &agent_types);

        let aligned: Vec<LyricLine> = sung
            .into_iter()
            .enumerate()
            .map(|(idx, (mut line, elem))| {
                let side = if sides.get(idx) == Some(&LyricAlignment::End) || says_opposite(elem) {
                    LyricAlignment::End
                } else {
                    LyricAlignment::Start
                };
                line.alignment = side;
                line
            })
            .collect();

        with_instrumental_gaps(aligned)
    }
}

fn singer_of(element: Option<&Value>) -> Option<&str> {
    element?.as_object()?.get("singer")?.as_str()
}

fn says_opposite(element: Option<&Value>) -> bool {
    let Some(arr) = element.and_then(Value::as_array) else {
        return false;
    };
    arr.iter()
        .filter_map(Value::as_str)
        .any(|tag| tag == "opposite" || tag == "right")
}

fn merge_syllables(syllables: &[LyricsPlusSyllable]) -> Vec<LyricWord> {
    let runs: Vec<TimedRun> = syllables
        .iter()
        .filter_map(|s| {
            let text = s.text.as_ref()?;
            let time = s.time?;
            Some(TimedRun::new(
                time,
                time + s.duration.unwrap_or(0),
                text.clone(),
            ))
        })
        .collect();
    words_from_runs(&runs, true)
}
