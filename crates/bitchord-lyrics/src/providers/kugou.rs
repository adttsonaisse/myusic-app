use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;
use serde::Deserialize;

use crate::http::{LyricsHttp, build_url};
use crate::{LyricLine, parse_lrc};

const SEARCH_SONG_URL: &str = "https://mobileservice.kugou.com/api/v3/search/song";
const SEARCH_LYRICS_URL: &str = "https://lyrics.kugou.com/search";
const DOWNLOAD_URL: &str = "https://lyrics.kugou.com/download";
const DURATION_TOLERANCE_SECONDS: i32 = 8;

static PARENTHETICAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[(（].*?[)）]")
        // INFALLIBLE: static regex pattern.
        .expect("valid PARENTHETICAL regex")
});

static STAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\d{2}:\d{2}\.\d{2,3}\]")
        // INFALLIBLE: static regex pattern.
        .expect("valid STAMP regex")
});

static INSTRUMENTAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"纯音乐.*欣赏")
        // INFALLIBLE: static regex pattern.
        .expect("valid INSTRUMENTAL regex")
});

static STAMPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[\d{2}:\d{2}\.\d{2,3}\].*$")
        // INFALLIBLE: static regex pattern.
        .expect("valid STAMPED regex")
});

static CREDIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^.+][^\[]+[:：].+$")
        // INFALLIBLE: static regex pattern.
        .expect("valid CREDIT regex")
});

#[derive(Deserialize)]
struct SearchSongResponse {
    data: Option<SearchSongData>,
}

#[derive(Deserialize)]
struct SearchSongData {
    #[serde(default)]
    info: Vec<SearchSongInfo>,
}

#[derive(Deserialize)]
struct SearchSongInfo {
    hash: String,
    #[serde(default = "default_neg_one")]
    duration: i32,
}

fn default_neg_one() -> i32 {
    -1
}

#[derive(Deserialize)]
struct SearchLyricsResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    id: String,
    accesskey: String,
}

#[derive(Deserialize)]
struct DownloadResponse {
    #[serde(default)]
    content: String,
}

/// Line-synced lyrics from KuGou's public mobile/lyrics endpoints.
pub struct KuGou;

impl KuGou {
    pub async fn lyrics(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        Self::lyrics_at(
            http,
            SEARCH_SONG_URL,
            SEARCH_LYRICS_URL,
            DOWNLOAD_URL,
            title,
            artist,
            duration_ms,
            album,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn lyrics_at(
        http: &LyricsHttp,
        search_song_url: &str,
        search_lyrics_url: &str,
        download_url: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let kw = keyword(title, artist, album);
        let seconds = (duration_ms / 1_000) as i32;

        let mut candidate: Option<Candidate> = None;
        if let Some(hashes) = Self::search_songs(http, search_song_url, &kw, seconds).await {
            for hash in hashes {
                if let Some(found) =
                    Self::search_lyrics(http, search_lyrics_url, Some(&hash), None, -1)
                        .await
                        .and_then(|c| c.into_iter().next())
                {
                    candidate = Some(found);
                    break;
                }
            }
        }
        let candidate = match candidate {
            Some(c) => c,
            None => Self::search_lyrics(http, search_lyrics_url, None, Some(&kw), seconds)
                .await?
                .into_iter()
                .next()?,
        };

        let lrc = Self::download(http, download_url, &candidate.id, &candidate.accesskey).await?;
        let lines = parse_lrc(&lrc);
        if lines.is_empty() { None } else { Some(lines) }
    }

    async fn search_songs(
        http: &LyricsHttp,
        endpoint: &str,
        keyword: &str,
        seconds: i32,
    ) -> Option<Vec<String>> {
        let url = build_url(
            endpoint,
            &[
                ("version", "9108"),
                ("plat", "0"),
                ("pagesize", "8"),
                ("showtype", "0"),
                ("keyword", keyword),
            ],
        )?;
        let body = http.get(&url).await?;
        let resp: SearchSongResponse = serde_json::from_str(&body).ok()?;
        let mut infos: Vec<SearchSongInfo> = resp
            .data
            .map(|d| d.info)
            .unwrap_or_default()
            .into_iter()
            .filter(|info| {
                seconds <= 0 || (info.duration - seconds).abs() <= DURATION_TOLERANCE_SECONDS
            })
            .collect();
        infos.sort_by_key(|info| (info.duration - seconds).abs());
        Some(infos.into_iter().map(|info| info.hash).collect())
    }

    async fn search_lyrics(
        http: &LyricsHttp,
        endpoint: &str,
        hash: Option<&str>,
        keyword: Option<&str>,
        seconds: i32,
    ) -> Option<Vec<Candidate>> {
        let mut params: Vec<(&str, String)> = vec![
            ("ver", "1".to_owned()),
            ("man", "yes".to_owned()),
            ("client", "pc".to_owned()),
        ];
        if let Some(h) = hash {
            params.push(("hash", h.to_owned()));
        } else {
            let kw = keyword?;
            params.push(("keyword", kw.to_owned()));
            if seconds > 0 {
                params.push(("duration", (i64::from(seconds) * 1_000).to_string()));
            }
        }
        let borrowed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = build_url(endpoint, &borrowed)?;
        let body = http.get(&url).await?;
        let resp: SearchLyricsResponse = serde_json::from_str(&body).ok()?;
        Some(resp.candidates)
    }

    async fn download(
        http: &LyricsHttp,
        endpoint: &str,
        id: &str,
        access_key: &str,
    ) -> Option<String> {
        let url = build_url(
            endpoint,
            &[
                ("fmt", "lrc"),
                ("charset", "utf8"),
                ("client", "pc"),
                ("ver", "1"),
                ("id", id),
                ("accesskey", access_key),
            ],
        )?;
        let body = http.get(&url).await?;
        let resp: DownloadResponse = serde_json::from_str(&body).ok()?;
        let compact: String = resp
            .content
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let decoded_bytes = STANDARD.decode(compact).ok()?;
        let decoded = String::from_utf8(decoded_bytes).ok()?;
        let stripped = Self::strip_credits(&decoded);
        if Self::is_instrumental_notice(&stripped) {
            None
        } else {
            Some(stripped)
        }
    }

    /// Strips KuGou's unstamped metadata preamble and stamped leading/trailing
    /// credit lines (`label: value` / `label：value`) within the first and last
    /// 30 stamped lines.
    pub fn strip_credits(text: &str) -> String {
        let lines: Vec<&str> = text.lines().filter(|line| STAMPED.is_match(line)).collect();
        if lines.is_empty() {
            return String::new();
        }
        let head_limit = 30.min(lines.len() - 1);
        let head_cut = (0..=head_limit)
            .rev()
            .find(|&idx| CREDIT.is_match(lines[idx]))
            .map_or(0, |idx| idx + 1);
        let body = &lines[head_cut..];
        if body.is_empty() {
            return String::new();
        }
        let tail_limit = 30.min(body.len() - 1);
        let tail_cut = (0..=tail_limit)
            .find(|&offset| CREDIT.is_match(body[body.len() - 1 - offset]))
            .map_or(0, |offset| offset + 1);
        let end = body.len().saturating_sub(tail_cut);
        body[..end].join("\n")
    }

    /// Returns `true` when every non-empty stamped line is KuGou's pure-music
    /// placeholder (`纯音乐，请欣赏` and variants).
    pub fn is_instrumental_notice(text: &str) -> bool {
        let lines: Vec<String> = text
            .lines()
            .map(|line| STAMP.replace_all(line, "").trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        !lines.is_empty() && lines.iter().all(|line| INSTRUMENTAL.is_match(line))
    }
}

fn keyword(title: &str, artist: &str, album: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(&strip_parenthetical(title));
    out.push_str(" - ");
    out.push_str(&strip_parenthetical(artist));
    if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
        out.push(' ');
        out.push_str(alb);
    }
    out
}

fn strip_parenthetical(value: &str) -> String {
    let stripped = PARENTHETICAL.replace_all(value, "");
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        value.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
[id:$00000000]
[ti:Heartbreak Anniversary]
[ar:Giveon]
[al:Heartbreak Anniversary]
[by:]
[00:00.00]Heartbreak Anniversary - GIVĒON
[00:04.12]Lyrics by：Giveon Evans/Varren Wade/Sevn Thomas
[00:08.24]Composed by：Giveon Evans/Varren Wade/Sevn Thomas/Maneesh
[00:12.36]Ooh
[00:15.90]Balloons are deflated
[00:19.73]Guess they look lifeless like me
[03:04.12]'Cause I think of you think of you
[03:07.90]Ooh ooh ooh-ooh";

    #[test]
    fn drops_unstamped_header_tags_entirely() {
        let stripped = KuGou::strip_credits(FIXTURE);
        assert!(!stripped.contains("[id:"));
        assert!(!stripped.contains("[ti:"));
        assert!(!stripped.contains("[by:]"));
    }

    #[test]
    fn cuts_everything_through_last_credit_line() {
        let stripped = KuGou::strip_credits(FIXTURE);
        assert!(!stripped.contains("GIVĒON"));
        assert!(!stripped.contains("Lyrics by"));
        assert!(!stripped.contains("Composed by"));
        assert!(stripped.starts_with("[00:12.36]Ooh"));
    }

    #[test]
    fn real_sung_lines_after_header_survive_untouched() {
        let stripped = KuGou::strip_credits(FIXTURE);
        assert!(stripped.contains("[00:15.90]Balloons are deflated"));
        assert!(stripped.contains("[00:19.73]Guess they look lifeless like me"));
    }

    #[test]
    fn outro_is_not_mistaken_for_a_credit() {
        let stripped = KuGou::strip_credits(FIXTURE);
        assert!(stripped.contains("[03:07.90]Ooh ooh ooh-ooh"));
    }

    #[test]
    fn lyric_with_no_credit_block_is_left_whole() {
        let clean = "[00:01.00]first line\n[00:05.00]second line";
        assert_eq!(KuGou::strip_credits(clean), clean);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(KuGou::strip_credits(""), "");
    }

    #[test]
    fn pure_music_notice_is_recognised_as_no_lyrics() {
        assert!(KuGou::is_instrumental_notice("[00:00.00]纯音乐，请欣赏"));
        assert!(KuGou::is_instrumental_notice("[00:00.00]纯音乐,请欣赏"));
        assert!(KuGou::is_instrumental_notice(
            "[00:00.00]此歌曲为没有填词的纯音乐，请您欣赏"
        ));
    }

    #[test]
    fn real_lyrics_are_not_mistaken_for_instrumental_notice() {
        assert!(!KuGou::is_instrumental_notice(&KuGou::strip_credits(
            FIXTURE
        )));
        assert!(!KuGou::is_instrumental_notice(
            "[00:00.00]纯音乐，请欣赏\n[00:05.00]first sung line"
        ));
        assert!(!KuGou::is_instrumental_notice(""));
    }
}
