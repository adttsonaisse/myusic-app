use std::sync::LazyLock;
use std::time::Duration;

use bitchord_model::Song;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::client::InnertubeClient;
use crate::error::InnertubeError;
use crate::parser::InnertubeParser;

static VIDEO_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_-]{11}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid VIDEO_ID_RE")
});

static RELEASED_ON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^Released on:\s*(\d{4})-(\d{2})-(\d{2})$")
        // INFALLIBLE: static regex pattern.
        .expect("valid RELEASED_ON_RE")
});

static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:19|20)[0-9]{2}$")
        // INFALLIBLE: static regex pattern.
        .expect("valid YEAR_RE")
});

static SCRIPT_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("script")
        // INFALLIBLE: static CSS selector.
        .expect("valid script selector")
});

const WATCH_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Extended watch-page metadata and engagement counts for a specific upload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongInfo {
    pub album: Option<String>,
    pub release_year: Option<String>,
    pub release_date: Option<String>,
    pub views: Option<String>,
    pub likes: Option<String>,
    pub comments: Option<String>,
    pub failed: bool,
}

/// YouTube-style truncated short form of a raw numeric count (`950 -> "950"`,
/// `1299 -> "1.2K"`, `999999 -> "999K"`, `2345678 -> "2.3M"`). Non-numeric
/// input is returned unchanged.
pub fn compact_count(raw: &str) -> String {
    let Ok(n) = raw.parse::<i64>() else {
        return raw.to_owned();
    };
    if n < 0 {
        return raw.to_owned();
    }
    let (unit, suffix) = if n >= 1_000_000_000 {
        (1_000_000_000i64, "B")
    } else if n >= 1_000_000 {
        (1_000_000i64, "M")
    } else if n >= 1_000 {
        (1_000i64, "K")
    } else {
        return n.to_string();
    };
    let whole = n / unit;
    if whole >= 10 {
        return format!("{whole}{suffix}");
    }
    let tenth = (n % unit) * 10 / unit;
    if tenth == 0 {
        format!("{whole}{suffix}")
    } else {
        format!("{whole}.{tenth}{suffix}")
    }
}

/// Parses `ytInitialPlayerResponse` and `ytInitialData` from a YouTube watch
/// HTML page for a specific `video_id`.
pub struct SongInfoParser;

impl SongInfoParser {
    pub fn parse(html: &str, video_id: &str) -> Result<SongInfo, InnertubeError> {
        let document = Html::parse_document(html);
        let scripts: Vec<String> = document
            .select(&SCRIPT_SELECTOR)
            .map(|el| el.text().collect::<String>())
            .collect();

        let player = scripts
            .iter()
            .find_map(|s| extract_assigned_object(s, "ytInitialPlayerResponse"))
            .ok_or_else(|| InnertubeError::InvalidState("Missing player metadata".to_owned()))?;

        let details = player.get("videoDetails").and_then(Value::as_object);
        let actual_id = details.and_then(|d| obj_str(d, "videoId"));
        if actual_id != Some(video_id) {
            return Err(InnertubeError::InvalidState(
                "Metadata belongs to another track".to_owned(),
            ));
        }

        let data = scripts
            .iter()
            .find_map(|s| extract_assigned_object(s, "ytInitialData"));

        let primary = data.as_ref().and_then(|d| {
            find_objects_map(d, "videoPrimaryInfoRenderer")
                .into_iter()
                .next()
        });

        let old_like = primary.and_then(|p| {
            find_objects_map(p, "toggleButtonRenderer")
                .into_iter()
                .find(|btn| {
                    btn.get("defaultIcon")
                        .and_then(Value::as_object)
                        .and_then(|icon| obj_str(icon, "iconType"))
                        == Some("LIKE")
                })
                .and_then(|btn| btn.get("defaultText"))
                .and_then(extract_text)
        });

        let new_like = primary.and_then(|p| {
            find_objects_map(p, "likeButtonViewModel")
                .into_iter()
                .next()
                .and_then(|vm| find_objects_map(vm, "buttonViewModel").into_iter().next())
                .and_then(|btn| obj_str(btn, "title"))
                .map(ToOwned::to_owned)
        });

        let comments = data.as_ref().and_then(|d| {
            find_objects_map(d, "commentsHeaderRenderer")
                .into_iter()
                .find_map(|r| {
                    r.get("countText")
                        .and_then(extract_text)
                        .and_then(with_count)
                })
                .or_else(|| {
                    find_objects_map(d, "commentsEntryPointHeaderRenderer")
                        .into_iter()
                        .next()
                        .and_then(|r| r.get("commentCount"))
                        .and_then(extract_text)
                        .and_then(with_count)
                })
                .or_else(|| {
                    find_objects_map(d, "engagementPanelSectionListRenderer")
                        .into_iter()
                        .find(|r| {
                            obj_str(r, "targetId") == Some("engagement-panel-comments-section")
                                || obj_str(r, "panelIdentifier")
                                    == Some("engagement-panel-comments-section")
                        })
                        .and_then(|panel| {
                            find_objects_map(panel, "engagementPanelTitleHeaderRenderer")
                                .into_iter()
                                .next()
                        })
                        .and_then(|hdr| hdr.get("contextualInfo"))
                        .and_then(extract_text)
                        .and_then(with_count)
                })
        });

        let views = details
            .and_then(|d| obj_str(d, "viewCount"))
            .filter(|v| v.parse::<i64>().is_ok_and(|n| n >= 0))
            .map(ToOwned::to_owned);

        let release_date = details
            .and_then(|d| obj_str(d, "shortDescription"))
            .and_then(parse_release_date);

        Ok(SongInfo {
            album: None,
            release_year: None,
            release_date,
            views,
            likes: new_like.or(old_like).and_then(with_count),
            comments,
            failed: data.is_none(),
        })
    }
}

/// Fetches watch-page counts + album release year for a single track.
pub struct SongInfoRepository;

impl SongInfoRepository {
    pub async fn load(client: &InnertubeClient, parser: &InnertubeParser, song: &Song) -> SongInfo {
        if song.local_uri.is_some() || !VIDEO_ID_RE.is_match(&song.video_id) {
            return SongInfo {
                album: song.album_name.clone(),
                ..Default::default()
            };
        }

        let watch_fut = async {
            let mut url = url::Url::parse("https://www.youtube.com/watch")
                .map_err(|e| InnertubeError::InvalidState(e.to_string()))?;
            url.query_pairs_mut()
                .append_pair("v", &song.video_id)
                .append_pair("hl", &client.current_language());
            let resp = client
                .http_client()
                .get(url)
                .timeout(Duration::from_secs(15))
                .header(reqwest::header::USER_AGENT, WATCH_USER_AGENT)
                // SOCS=CAI answers EU consent interstitials with "reject all".
                .header(reqwest::header::COOKIE, "SOCS=CAI")
                .send()
                .await
                .map_err(|e| InnertubeError::Transport(e.to_string()))?;
            if resp.status().as_u16() != 200 {
                return Err(InnertubeError::HttpStatus {
                    status: resp.status().as_u16(),
                    detail: "watch page non-200".to_owned(),
                });
            }
            let html = resp
                .text()
                .await
                .map_err(|e| InnertubeError::Transport(e.to_string()))?;
            SongInfoParser::parse(&html, &song.video_id)
        };

        let release_fut = async {
            let mut album = song.album_name.clone();
            let mut year: Option<String> = None;
            let album_id = if let Some(ref id) = song.album_id {
                Some(id.clone())
            } else {
                let next = client.next(&song.video_id).await?;
                let matched = parser
                    .parse_watch_queue(&next, false, false)
                    .into_iter()
                    .find(|s| s.video_id == song.video_id);
                if album.is_none() {
                    album = matched.as_ref().and_then(|s| s.album_name.clone());
                }
                matched.and_then(|s| s.album_id)
            };
            if let Some(id) = album_id {
                let page = client.browse(&id, None).await?;
                if let Some(header) = parser.parse_browse_header(&page) {
                    if album.is_none() {
                        album = Some(header.title);
                    }
                    year = header
                        .subtitle
                        .split('•')
                        .map(str::trim)
                        .find(|part| YEAR_RE.is_match(part))
                        .map(ToOwned::to_owned);
                }
            }
            Ok::<(Option<String>, Option<String>), InnertubeError>((album, year))
        };

        let (stats_res, release_res) = tokio::join!(watch_fut, release_fut);
        let mut info = stats_res.unwrap_or_else(|_| SongInfo {
            failed: true,
            ..Default::default()
        });
        match release_res {
            Ok((album, year)) => {
                info.album = album;
                info.release_year = year;
            }
            Err(_) => {
                info.album = song.album_name.clone();
                info.failed = true;
            }
        }
        info
    }
}

fn parse_release_date(description: &str) -> Option<String> {
    for line in description.lines() {
        let Some(caps) = RELEASED_ON_RE.captures(line.trim()) else {
            continue;
        };
        let year: u32 = caps.get(1)?.as_str().parse().ok()?;
        let month: u32 = caps.get(2)?.as_str().parse().ok()?;
        let day: u32 = caps.get(3)?.as_str().parse().ok()?;
        if is_valid_gregorian_date(year, month, day) {
            return Some(format!("{year:04}-{month:02}-{day:02}"));
        }
        return None;
    }
    None
}

fn is_valid_gregorian_date(year: u32, month: u32, day: u32) -> bool {
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let is_leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    day <= max_day
}

fn extract_assigned_object(script: &str, name: &str) -> Option<Map<String, Value>> {
    let pattern = format!(r"(?:var\s+)?{}\s*=\s*", regex::escape(name));
    let re = Regex::new(&pattern).ok()?;
    let m = re.find(script)?;
    let start = m.end();
    let rest = script.get(start..)?;
    if !rest.starts_with('{') {
        return None;
    }

    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (rel_idx, ch) in rest.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else {
            match ch {
                '"' => quoted = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let slice = &rest[..=rel_idx];
                        return serde_json::from_str::<Value>(slice)
                            .ok()
                            .and_then(|v| match v {
                                Value::Object(map) => Some(map),
                                _ => None,
                            });
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn obj_str<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

fn with_count(text: String) -> Option<String> {
    text.chars().any(|c| c.is_ascii_digit()).then_some(text)
}

fn extract_text(val: &Value) -> Option<String> {
    let obj = val.as_object()?;
    let out = if let Some(s) = obj_str(obj, "simpleText") {
        s.to_owned()
    } else {
        let runs = obj.get("runs").and_then(Value::as_array)?;
        runs.iter()
            .filter_map(|r| r.as_object().and_then(|o| obj_str(o, "text")))
            .collect::<String>()
    };
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn find_objects_map<'a>(map: &'a Map<String, Value>, key: &str) -> Vec<&'a Map<String, Value>> {
    let mut out = Vec::new();
    if let Some(obj) = map.get(key).and_then(Value::as_object) {
        out.push(obj);
    }
    for v in map.values() {
        collect_find_objects(v, key, &mut out);
    }
    out
}

fn collect_find_objects<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            if let Some(obj) = map.get(key).and_then(Value::as_object) {
                out.push(obj);
            }
            for v in map.values() {
                collect_find_objects(v, key, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_find_objects(v, key, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(data: &str, id: &str, views: &str) -> String {
        format!(
            r#"<script>var ytInitialPlayerResponse = {{"videoDetails":{{"videoId":"{id}","viewCount":"{views}","title":"A }} title"}},"microformat":{{"playerMicroformatRenderer":{{"uploadDate":"2024-02-03"}}}}}}; window.after = true;</script>
<script>var ytInitialData = {data};</script>"#
        )
    }

    #[test]
    fn extracts_counts_without_treating_upload_date_as_release_year() {
        let html = page(
            r#"{"contents":{"videoPrimaryInfoRenderer":{"buttons":[{"toggleButtonRenderer":{"defaultIcon":{"iconType":"LIKE"},"defaultText":{"simpleText":"1.2K"}}}]},"commentsEntryPointHeaderRenderer":{"commentCount":{"simpleText":"42"}}}}"#,
            "abcdefghijk",
            "12345",
        );
        let info = SongInfoParser::parse(&html, "abcdefghijk").expect("parsed");
        assert_eq!(info.views.as_deref(), Some("12345"));
        assert_eq!(info.likes.as_deref(), Some("1.2K"));
        assert_eq!(info.comments.as_deref(), Some("42"));
        assert!(info.release_date.is_none());
        assert!(info.release_year.is_none());
    }

    #[test]
    fn uses_explicit_release_date_instead_of_video_upload_date() {
        let base = page("{}", "abcdefghijk", "12345");
        let html = base.replace(
            r#""title":"A } title""#,
            r#""title":"A } title","shortDescription":"Provided to YouTube\nReleased on: 1987-07-27\nAuto-generated by YouTube.""#,
        );
        assert_eq!(
            SongInfoParser::parse(&html, "abcdefghijk")
                .expect("valid")
                .release_date
                .as_deref(),
            Some("1987-07-27")
        );
        let invalid = html.replace("1987-07-27", "1987-02-30");
        assert!(
            SongInfoParser::parse(&invalid, "abcdefghijk")
                .expect("parsed")
                .release_date
                .is_none()
        );
    }

    #[test]
    fn ignores_recommended_video_likes_and_missing_counts() {
        let html = page(
            r#"{"recommendations":{"toggleButtonRenderer":{"defaultIcon":{"iconType":"LIKE"},"defaultText":{"simpleText":"999"}}}}"#,
            "abcdefghijk",
            "-1",
        );
        let info = SongInfoParser::parse(&html, "abcdefghijk").expect("parsed");
        assert!(info.likes.is_none());
        assert!(info.comments.is_none());
        assert!(info.views.is_none());
    }

    #[test]
    fn supports_current_like_button_and_zero_counts() {
        let html = page(
            r#"{"videoPrimaryInfoRenderer":{"likeButtonViewModel":{"toggleButtonViewModel":{"defaultButtonViewModel":{"buttonViewModel":{"title":"0"}}}}}}"#,
            "abcdefghijk",
            "0",
        );
        let info = SongInfoParser::parse(&html, "abcdefghijk").expect("parsed");
        assert_eq!(info.likes.as_deref(), Some("0"));
        assert_eq!(info.views.as_deref(), Some("0"));
    }

    #[test]
    fn rejects_wrong_identity_and_consent_pages() {
        assert!(SongInfoParser::parse(&page("{}", "other_song_", "12345"), "abcdefghijk").is_err());
        assert!(SongInfoParser::parse("<html>Consent</html>", "abcdefghijk").is_err());
    }

    #[test]
    fn reads_comment_panel_count_when_header_has_only_label() {
        let html = page(
            r#"{"commentsHeaderRenderer":{"countText":{"runs":[{"text":"Comments"}]}},"engagementPanels":[{"engagementPanelSectionListRenderer":{"targetId":"engagement-panel-comments-section","header":{"engagementPanelTitleHeaderRenderer":{"contextualInfo":{"runs":[{"text":"2.4M"}]}}}}}]}"#,
            "abcdefghijk",
            "12345",
        );
        let info = SongInfoParser::parse(&html, "abcdefghijk").expect("parsed");
        assert_eq!(info.comments.as_deref(), Some("2.4M"));
    }

    #[test]
    fn compacts_raw_counts() {
        assert_eq!(compact_count("0"), "0");
        assert_eq!(compact_count("999"), "999");
        assert_eq!(compact_count("1000"), "1K");
        assert_eq!(compact_count("1299"), "1.2K");
        assert_eq!(compact_count("100456"), "100K");
        assert_eq!(compact_count("999999"), "999K");
        assert_eq!(compact_count("2345678"), "2.3M");
        assert_eq!(compact_count("1500000000"), "1.5B");
        assert_eq!(compact_count("1.2K"), "1.2K");
    }
}
