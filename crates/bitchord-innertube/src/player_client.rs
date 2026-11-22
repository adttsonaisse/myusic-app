use std::collections::BTreeMap;

use url::Url;

use crate::auth::{MUSIC_ORIGIN, YOUTUBE_ORIGIN};

pub const WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const VISIONOS_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

const RANGE_BYTES: u64 = 1024 * 1024;
const NARROW_RANGE_BYTES: u64 = 512 * 1024;

/// The client identity that minted a `googlevideo.com` URL and the headers the
/// subsequent media fetch must repeat to avoid throttling or HTTP 403.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerClient {
    pub client_name: String,
    pub client_id: u32,
    pub client_version: String,
    pub user_agent: String,
    /// Browser origin (`https://music.youtube.com` or `https://www.youtube.com`).
    /// Must be `None` for native mobile/XR clients.
    pub origin: Option<String>,
    pub supports_cookies: bool,
    pub requires_js_player: bool,
}

impl PlayerClient {
    pub fn referer(&self) -> Option<String> {
        self.origin.as_deref().map(|o| format!("{o}/"))
    }

    /// Exact HTTP headers that media fetches for a URL minted by this client
    /// must carry.
    pub fn media_headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        headers.insert("User-Agent".to_owned(), self.user_agent.clone());
        if let Some(ref origin) = self.origin {
            headers.insert("Origin".to_owned(), origin.clone());
            headers.insert("Referer".to_owned(), format!("{origin}/"));
        }
        headers
    }

    pub fn vision_os(version: Option<&str>) -> Self {
        Self {
            client_name: "VISIONOS".to_owned(),
            client_id: 101,
            client_version: version.unwrap_or("1.02").to_owned(),
            user_agent: VISIONOS_USER_AGENT.to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    pub fn tv_samsung() -> Self {
        Self {
            client_name: "TVHTML5".to_owned(),
            client_id: 7,
            client_version: "5.20260707".to_owned(),
            user_agent: "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 \
                (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1"
                .to_owned(),
            origin: Some(YOUTUBE_ORIGIN.to_owned()),
            supports_cookies: true,
            requires_js_player: true,
        }
    }

    pub fn tvhtml5() -> Self {
        Self {
            client_name: "TVHTML5".to_owned(),
            client_id: 7,
            client_version: "7.20260707.07.00".to_owned(),
            user_agent: "Mozilla/5.0(SMART-TV; Linux; Tizen 4.0.0.2) AppleWebkit/605.1.15 \
                (KHTML, like Gecko) SamsungBrowser/9.2 TV Safari/605.1.15"
                .to_owned(),
            origin: Some(YOUTUBE_ORIGIN.to_owned()),
            supports_cookies: true,
            requires_js_player: true,
        }
    }

    pub fn web_remix(version: Option<&str>) -> Self {
        Self {
            client_name: "WEB_REMIX".to_owned(),
            client_id: 67,
            client_version: version.unwrap_or("1.20260707.12.00").to_owned(),
            user_agent: WEB_USER_AGENT.to_owned(),
            origin: Some(MUSIC_ORIGIN.to_owned()),
            supports_cookies: true,
            requires_js_player: true,
        }
    }

    pub fn web() -> Self {
        Self {
            client_name: "WEB".to_owned(),
            client_id: 1,
            client_version: "2.20260708.00.00".to_owned(),
            user_agent: WEB_USER_AGENT.to_owned(),
            origin: Some(YOUTUBE_ORIGIN.to_owned()),
            supports_cookies: true,
            requires_js_player: true,
        }
    }

    pub fn ios() -> Self {
        Self {
            client_name: "IOS".to_owned(),
            client_id: 5,
            client_version: "21.26.4".to_owned(),
            user_agent:
                "com.google.ios.youtube/21.26.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)"
                    .to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    pub fn ios_recent() -> Self {
        Self {
            client_name: "IOS".to_owned(),
            client_id: 5,
            client_version: "21.29.1".to_owned(),
            user_agent:
                "com.google.ios.youtube/21.29.1 (iPhone16,2; U; CPU iOS 18_5 like Mac OS X;)"
                    .to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    pub fn android() -> Self {
        Self {
            client_name: "ANDROID".to_owned(),
            client_id: 3,
            client_version: "21.26.364".to_owned(),
            user_agent: "com.google.android.youtube/21.26.364 \
                (Linux; U; Android 15; en_US; Pixel 9 Pro; Build/AP4A.250205.002; Cronet/132.0.6834.79) gzip"
                .to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    pub fn android_music() -> Self {
        Self {
            client_name: "ANDROID_MUSIC".to_owned(),
            client_id: 21,
            client_version: "8.39.42".to_owned(),
            user_agent: "com.google.android.apps.youtube.music/8.39.42 \
                (Linux; U; Android 15; en_US; Pixel 9 Pro; Build/AP4A.250205.002) gzip"
                .to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    pub fn android_vr() -> Self {
        Self {
            client_name: "ANDROID_VR".to_owned(),
            client_id: 28,
            client_version: "1.65.10".to_owned(),
            user_agent: "com.google.android.apps.youtube.vr.oculus/1.65.10 \
                (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip"
                .to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    pub fn android_vr_legacy() -> Self {
        Self {
            client_name: "ANDROID_VR".to_owned(),
            client_id: 28,
            client_version: "1.43.32".to_owned(),
            user_agent: "com.google.android.apps.youtube.vr.oculus/1.43.32 \
                (Linux; U; Android 12; en_US; Quest 3; Build/SQ3A.220605.009.A1; Cronet/107.0.5284.2)"
                .to_owned(),
            origin: None,
            supports_cookies: false,
            requires_js_player: false,
        }
    }

    /// Infers the minting client from a `googlevideo.com` URL's `c`/`cver`
    /// query parameters (defaulting to [`Self::ios`] if unmodelled).
    pub fn for_stream_url(url: &str) -> Self {
        let Ok(parsed) = Url::parse(url) else {
            return Self::ios();
        };
        let mut client_name: Option<String> = None;
        let mut client_version: Option<String> = None;
        for (k, v) in parsed.query_pairs() {
            if k == "c" {
                client_name = Some(v.to_ascii_uppercase());
            } else if k == "cver" {
                client_version = Some(v.into_owned());
            }
        }
        let Some(name) = client_name else {
            return Self::ios();
        };
        let ver = client_version.as_deref();
        if name.starts_with("IOS") {
            if ver == Some("21.29.1") {
                Self::ios_recent()
            } else {
                Self::ios()
            }
        } else if name == "ANDROID_VR" {
            if ver == Some("1.43.32") {
                Self::android_vr_legacy()
            } else {
                Self::android_vr()
            }
        } else if name == "ANDROID_MUSIC" {
            Self::android_music()
        } else if name.starts_with("ANDROID") {
            Self::android()
        } else if name.starts_with("TVHTML5") {
            Self::tvhtml5()
        } else if name == "WEB_REMIX" {
            Self::web_remix(ver)
        } else if name.starts_with("WEB") || name == "MWEB" {
            Self::web()
        } else if name == "VISIONOS" {
            Self::vision_os(ver)
        } else {
            Self::ios()
        }
    }

    /// Maximum single HTTP `Range` request size `googlevideo.com` reliably
    /// serves for `url` without throttling to 15.5 kB/s. Returns `None` for
    /// non-`googlevideo.com` hosts.
    pub fn range_bytes_for(url: &str) -> Option<u64> {
        let parsed = Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        if !host.ends_with("googlevideo.com") {
            return None;
        }
        let c = parsed
            .query_pairs()
            .find_map(|(k, v)| (k == "c").then(|| v.to_ascii_uppercase()));
        match c.as_deref() {
            Some("ANDROID_VR") => Some(NARROW_RANGE_BYTES),
            Some(name) if name.starts_with("TVHTML5_SIMPLY") => Some(NARROW_RANGE_BYTES),
            _ => Some(RANGE_BYTES),
        }
    }

    /// Reads the `clen` query parameter from a `googlevideo.com` URL when present.
    pub fn content_length_from_url(url: &str) -> Option<u64> {
        let parsed = Url::parse(url).ok()?;
        parsed
            .query_pairs()
            .find_map(|(k, v)| (k == "clen").then(|| v.parse::<u64>().ok())?)
    }

    /// Reads the `dur` (seconds, e.g. `256.881`) query parameter from a `googlevideo.com`
    /// URL and returns the exact duration in milliseconds when positive and finite.
    pub fn duration_ms_from_url(url: &str) -> Option<u64> {
        let parsed = Url::parse(url).ok()?;
        let secs = parsed
            .query_pairs()
            .find_map(|(k, v)| (k == "dur").then(|| v.parse::<f64>().ok())?)?;
        if secs.is_finite() && secs > 0.0 {
            Some((secs * 1_000.0).round() as u64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_clients_omit_origin_and_web_clients_include_it() {
        let vision = PlayerClient::vision_os(None).media_headers();
        assert!(vision.contains_key("User-Agent"));
        assert!(!vision.contains_key("Origin"));
        assert!(!vision.contains_key("Referer"));

        let remix = PlayerClient::web_remix(None).media_headers();
        assert_eq!(remix.get("Origin").map(String::as_str), Some(MUSIC_ORIGIN));
        assert_eq!(
            remix.get("Referer").map(String::as_str),
            Some("https://music.youtube.com/")
        );
    }

    #[test]
    fn range_bytes_caps_googlevideo_by_client() {
        assert_eq!(
            PlayerClient::range_bytes_for("https://rr1.googlevideo.com/videoplayback?c=VISIONOS"),
            Some(1024 * 1024)
        );
        assert_eq!(
            PlayerClient::range_bytes_for("https://rr1.googlevideo.com/videoplayback?c=ANDROID_VR"),
            Some(512 * 1024)
        );
        assert_eq!(
            PlayerClient::range_bytes_for(
                "https://rr1.googlevideo.com/videoplayback?c=TVHTML5_SIMPLY"
            ),
            Some(512 * 1024)
        );
        assert_eq!(
            PlayerClient::range_bytes_for("https://example.com/audio.opus"),
            None
        );
        assert_eq!(
            PlayerClient::duration_ms_from_url(
                "https://rr1.googlevideo.com/videoplayback?clen=4739610&dur=256.881&c=VISIONOS"
            ),
            Some(256_881)
        );
        assert_eq!(
            PlayerClient::duration_ms_from_url("https://example.com/audio.opus"),
            None
        );
    }
}
