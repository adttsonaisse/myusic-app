use serde::{Deserialize, Serialize};

use crate::http::{LyricsHttp, build_url};
use crate::{EnhancedLrc, LyricLine, TtmlLyrics, parse_lrc};

const BASE: &str = "https://unison.boidu.dev/lyrics";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnisonResponse {
    pub success: Option<bool>,
    pub data: Option<UnisonEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnisonEntry {
    pub song: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub lyrics: Option<String>,
    /// `"ttml"` or `"lrc"`.
    pub format: Option<String>,
    /// `"wordsync"`, `"linesync"`, or `"plain"`.
    pub sync_type: Option<String>,
    pub confidence: Option<String>,
    pub vote_count: Option<i32>,
}

/// Community-submitted lyrics database (`unison.boidu.dev`).
pub struct Unison;

impl Unison {
    pub async fn lyrics(
        http: &LyricsHttp,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        Self::lyrics_at(http, BASE, title, artist, duration_ms, album).await
    }

    pub(crate) async fn lyrics_at(
        http: &LyricsHttp,
        base: &str,
        title: &str,
        artist: &str,
        duration_ms: i64,
        album: Option<&str>,
    ) -> Option<Vec<LyricLine>> {
        let mut params: Vec<(&str, String)> =
            vec![("song", title.to_owned()), ("artist", artist.to_owned())];
        if let Some(alb) = album.filter(|s| !s.trim().is_empty()) {
            params.push(("album", alb.to_owned()));
        }
        let seconds = duration_ms / 1_000;
        if seconds > 0 {
            params.push(("duration", seconds.to_string()));
        }
        let borrowed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = build_url(base, &borrowed)?;
        let body = http.get(&url).await?;
        let resp: UnisonResponse = serde_json::from_str(&body).ok()?;
        if resp.success != Some(true) {
            return None;
        }
        Self::lines_of(&resp.data?)
    }

    /// Parses one Unison entry according to its declared `format` and `sync_type`.
    pub fn lines_of(data: &UnisonEntry) -> Option<Vec<LyricLine>> {
        let text = data.lyrics.as_deref().filter(|s| !s.trim().is_empty())?;
        let lines = if data
            .format
            .as_deref()
            .is_some_and(|f| f.eq_ignore_ascii_case("ttml"))
        {
            TtmlLyrics::parse(text)
        } else if data
            .sync_type
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("plain"))
        {
            plain(text)
        } else {
            let enhanced = EnhancedLrc::parse(text);
            if enhanced.is_empty() {
                parse_lrc(text)
            } else {
                enhanced
            }
        };
        if lines.is_empty() { None } else { Some(lines) }
    }
}

fn plain(text: &str) -> Vec<LyricLine> {
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| LyricLine::new(0, s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNISON_LRC: &str = concat!(
        r#"{"success":true,"data":{"id":4883,"videoId":"uV_5eEvamoQ","#,
        r#""song":"golden hour","artist":"JVKE","album":"this is what ____ feels like","#,
        r#""lyrics":"[00:15.46]It was just two lovers\n[00:17.30]Sittin' in the car","#,
        r#""format":"lrc","language":"en","syncType":"linesync","score":0,"#,
        r#""effectiveScore":0,"voteCount":0,"confidence":"low","hidden":false,"#,
        r#""submitter":{"keyId":"dea04ab0","reputation":2,"displayName":"Prime","#,
        r#""tier":null,"level":1,"badgeCount":2,"#,
        r#""topBadge":{"key":"early-adopter","name":"Early Adopter"}},"userVote":null}}"#,
    );

    #[test]
    fn reads_a_line_synced_unison_entry() {
        let resp: UnisonResponse = serde_json::from_str(UNISON_LRC).expect("valid");
        assert_eq!(resp.success, Some(true));
        let lines: Vec<LyricLine> = Unison::lines_of(resp.data.as_ref().expect("data"))
            .expect("lines")
            .into_iter()
            .filter(|l| !l.is_gap())
            .collect();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["It was just two lovers", "Sittin' in the car"]);
        assert_eq!(lines[0].time_ms, 15_460);
        assert_eq!(lines[1].time_ms, 17_300);
        assert!(lines.iter().all(|l| !l.is_word_synced()));
    }

    #[test]
    fn reads_a_word_synced_unison_entry_as_ttml() {
        let entry = UnisonEntry {
            format: Some("ttml".to_owned()),
            sync_type: Some("wordsync".to_owned()),
            lyrics: Some(
                r#"<tt><body><div><p begin="1.0" end="2.0"><span begin="1.0" end="1.4">hold</span> <span begin="1.4" end="2.0">on</span></p></div></body></tt>"#
                    .to_owned(),
            ),
            ..Default::default()
        };
        let lines = Unison::lines_of(&entry).expect("lines");
        let sung: Vec<&LyricLine> = lines.iter().filter(|l| !l.is_gap()).collect();
        assert_eq!(sung.len(), 1);
        assert_eq!(sung[0].text, "hold on");
        assert!(sung[0].is_word_synced());
        assert_eq!(sung[0].words[0].end_ms, 1_400);
    }

    #[test]
    fn reads_an_unsynced_unison_entry_as_plain_lines() {
        let entry = UnisonEntry {
            format: Some("lrc".to_owned()),
            sync_type: Some("plain".to_owned()),
            lyrics: Some("  first line \n\n second line \n".to_owned()),
            ..Default::default()
        };
        let lines = Unison::lines_of(&entry).expect("lines");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["first line", "second line"]);
        assert!(lines.iter().all(|l| l.time_ms == 0));
    }

    #[test]
    fn entry_with_no_lyrics_is_not_a_match() {
        assert!(
            Unison::lines_of(&UnisonEntry {
                format: Some("lrc".to_owned()),
                sync_type: Some("linesync".to_owned()),
                ..Default::default()
            })
            .is_none()
        );
        assert!(
            Unison::lines_of(&UnisonEntry {
                format: Some("lrc".to_owned()),
                sync_type: Some("linesync".to_owned()),
                lyrics: Some("  ".to_owned()),
                ..Default::default()
            })
            .is_none()
        );
    }
}
