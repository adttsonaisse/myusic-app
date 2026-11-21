use std::time::Duration;

use reqwest::Client;
use url::Url;

/// Default User-Agent sent by BitChord lyrics providers.
pub const LYRICS_AGENT: &str = "BitChord (https://github.com/bitchord)";

const LYRICS_TIMEOUT_SECS: u64 = 6;
const LYRICS_CONNECT_TIMEOUT_SECS: u64 = 3;
const AUTHENTICATED_TIMEOUT_SECS: u64 = 15;
const AUTHENTICATED_CONNECT_TIMEOUT_SECS: u64 = 10;
const SLOW_TIMEOUT_SECS: u64 = 8;
const SLOW_CONNECT_TIMEOUT_SECS: u64 = 4;
const TRANSLATE_TIMEOUT_SECS: u64 = 12;
const TRANSLATE_CONNECT_TIMEOUT_SECS: u64 = 5;

/// Shared HTTP clients with upstream-matched timeout profiles for lyrics
/// providers and translation.
///
/// Cloning [`LyricsHttp`] is cheap (`reqwest::Client` is internally `Arc`).
#[derive(Debug, Clone)]
pub struct LyricsHttp {
    fast: Client,
    authenticated: Client,
    slow: Client,
    translate: Client,
}

impl Default for LyricsHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsHttp {
    pub fn new() -> Self {
        let fast = Client::builder()
            .timeout(Duration::from_secs(LYRICS_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(LYRICS_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| Client::new());

        let authenticated = Client::builder()
            .timeout(Duration::from_secs(AUTHENTICATED_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(AUTHENTICATED_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| Client::new());

        let slow = Client::builder()
            .timeout(Duration::from_secs(SLOW_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(SLOW_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| Client::new());

        let translate = Client::builder()
            .timeout(Duration::from_secs(TRANSLATE_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(TRANSLATE_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            fast,
            authenticated,
            slow,
            translate,
        }
    }

    /// Body of a successful GET using the fast 6s provider deadline, or `None`
    /// on any network/HTTP failure.
    pub async fn get(&self, url: &str) -> Option<String> {
        let resp = self
            .fast
            .get(url)
            .header(reqwest::header::USER_AGENT, LYRICS_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .ok()?;
        if resp.status().is_success() {
            resp.text().await.ok()
        } else {
            None
        }
    }

    /// Body of a successful GET using the 8s client with default `LYRICS_AGENT`
    /// (used by LRCLIB).
    pub async fn get_slow(&self, url: &str) -> Option<String> {
        let resp = self
            .slow
            .get(url)
            .header(reqwest::header::USER_AGENT, LYRICS_AGENT)
            .send()
            .await
            .ok()?;
        if resp.status().is_success() {
            resp.text().await.ok()
        } else {
            None
        }
    }

    /// Body of an authenticated provider GET (15s timeout, Bearer header).
    pub async fn get_bearer(&self, url: &str, bearer: &str) -> Option<String> {
        if bearer.trim().is_empty() {
            return None;
        }
        let resp = self
            .authenticated
            .get(url)
            .header(reqwest::header::USER_AGENT, LYRICS_AGENT)
            .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"))
            .send()
            .await
            .ok()?;
        if resp.status().is_success() {
            resp.text().await.ok()
        } else {
            None
        }
    }

    /// Apple Music catalog API GET with Bearer token and `Origin`/`Referer`
    /// headers required by `amp-api.music.apple.com`.
    pub async fn get_apple_authorized(&self, url: &str, bearer: &str) -> Option<String> {
        if bearer.trim().is_empty() {
            return None;
        }
        let resp = self
            .fast
            .get(url)
            .header(reqwest::header::USER_AGENT, LYRICS_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(reqwest::header::ORIGIN, "https://music.apple.com")
            .header(reqwest::header::REFERER, "https://music.apple.com/")
            .send()
            .await
            .ok()?;
        if resp.status().is_success() {
            resp.text().await.ok()
        } else {
            None
        }
    }

    /// GET with explicit `User-Agent`, `Accept`, `Accept-Language`, and
    /// optional `Cookie` headers (used by Musixmatch and Genius).
    pub async fn get_custom(
        &self,
        url: &str,
        user_agent: &str,
        accept: &str,
        cookie: Option<&str>,
    ) -> Option<String> {
        let mut req = self
            .slow
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .header(reqwest::header::ACCEPT, accept)
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9");
        if let Some(c) = cookie {
            req = req.header(reqwest::header::COOKIE, c);
        }
        let resp = req.send().await.ok()?;
        if resp.status().is_success() {
            resp.text().await.ok()
        } else {
            None
        }
    }

    /// Form-encoded POST for translation / romanization endpoints (12s timeout).
    pub async fn post_form(
        &self,
        url: &str,
        user_agent: &str,
        params: &[(&str, &str)],
    ) -> Option<String> {
        let resp = self
            .translate
            .post(url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(params)
            .send()
            .await
            .ok()?;
        if resp.status().is_success() {
            resp.text().await.ok()
        } else {
            None
        }
    }
}

/// Build a URL with query parameters, returning `None` if `base` is not a
/// valid URL.
pub(crate) fn build_url(base: &str, params: &[(&str, &str)]) -> Option<String> {
    let mut url = Url::parse(base).ok()?;
    {
        let mut pairs = url.query_pairs_mut();
        for &(k, v) in params {
            pairs.append_pair(k, v);
        }
    }
    Some(url.into())
}
