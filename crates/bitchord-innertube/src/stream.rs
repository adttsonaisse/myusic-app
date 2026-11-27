use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use crate::auth::{MUSIC_ORIGIN, YOUTUBE_ORIGIN, sapisid_from_cookie, sapisid_hash};
use crate::cipher::CipherSandbox;
use crate::client::InnertubeClient;
use crate::error::{PermanentReason, StreamError};
use crate::player_client::{PlayerClient, WEB_USER_AGENT};
use crate::uploaded::UploadedTracks;

/// Past YouTube's 1 MiB unauthenticated prefix wall (`StreamResolver.kt`).
pub const AUTH_BOUNDARY_BYTES: u64 = 1_048_576;
/// Minimum audio bytes read during URL probing to catch header-then-stall throttling.
pub const PROBE_READ_BYTES: usize = 16 * 1024;

const RECENT_TTL: Duration = Duration::from_secs(20 * 60);
const RECENT_MAX_ENTRIES: usize = 32;
const UNPLAYABLE_TTL: Duration = Duration::from_secs(10 * 60);
const EXCLUDE_TTL: Duration = Duration::from_secs(10 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_RESOLVE_ATTEMPTS: usize = 3;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Audio container preference for playback vs private/exported downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContainerPreference {
    /// Playback: prefer Opus/WebM, accept M4A/AAC.
    #[default]
    Any,
    /// Private download: Opus in WebM.
    WebmOpus,
    /// Exported download: AAC in MP4/M4A.
    M4aAac,
}

#[derive(Debug, Clone)]
pub struct ResolveRequest {
    pub video_id: String,
    pub max_kbps: u32,
    pub container: ContainerPreference,
    pub uploaded_hint: bool,
    pub skip_profiles: Vec<String>,
}

impl ResolveRequest {
    pub fn for_playback(video_id: impl Into<String>, max_kbps: u32) -> Self {
        Self {
            video_id: video_id.into(),
            max_kbps,
            container: ContainerPreference::Any,
            uploaded_hint: false,
            skip_profiles: Vec::new(),
        }
    }
}

/// Probe-verified direct audio stream ready for Wave 3 `bitchord-audio` or download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedStream {
    pub video_id: String,
    pub url: String,
    pub kbps: u32,
    pub mime_type: String,
    pub ext: String,
    pub content_length: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    pub loudness_db: Option<f64>,
    pub is_live_hls: bool,
    pub client_name: String,
    pub profile_id: String,
    pub media_headers: BTreeMap<String, String>,
    pub range_chunk_bytes: Option<u64>,
}

/// Pluggable stream extraction backend (`NativeStreamBackend`, `YtDlpStreamBackend`,
/// or `FallbackStreamBackend`).
pub trait StreamBackend: Send + Sync {
    fn resolve_uncached<'a>(
        &'a self,
        req: &'a ResolveRequest,
    ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>>;

    fn on_refused(&self, _video_id: &str, _profile_id: &str, _url: &str) {}
    fn on_session_changed(&self) {}
}

/// Optional BotGuard PO-token provider (`NoopPoTokenProvider` or `BgUtilHttpProvider`).
pub trait PoTokenProvider: Send + Sync {
    fn supports_web_botguard(&self) -> bool;
    fn mint<'a>(
        &'a self,
        video_id: &'a str,
        visitor_data: &'a str,
    ) -> BoxFuture<'a, Result<Option<PoTokenPair>, StreamError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoTokenPair {
    pub player_request_token: String,
    pub streaming_data_token: String,
}

/// Default no-op provider when no external BotGuard daemon is running.
#[derive(Debug, Default)]
pub struct NoopPoTokenProvider;

impl PoTokenProvider for NoopPoTokenProvider {
    fn supports_web_botguard(&self) -> bool {
        false
    }

    fn mint<'a>(
        &'a self,
        _video_id: &'a str,
        _visitor_data: &'a str,
    ) -> BoxFuture<'a, Result<Option<PoTokenPair>, StreamError>> {
        Box::pin(async { Ok(None) })
    }
}

/// Consumes an external `bgutil-ytdlp-pot-provider` HTTP daemon (`127.0.0.1:4416`)
/// without linking V8/Deno into BitChord.
#[derive(Debug, Clone)]
pub struct BgUtilHttpProvider {
    http: Arc<Client>,
    base_url: String,
}

impl BgUtilHttpProvider {
    pub fn new(http: Arc<Client>, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }

    pub fn localhost(http: Arc<Client>) -> Self {
        Self::new(http, "http://127.0.0.1:4416")
    }

    pub async fn is_alive(&self) -> bool {
        self.http
            .get(format!("{}/ping", self.base_url))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    async fn fetch_pot(&self, content_binding: &str) -> Result<Option<String>, StreamError> {
        let resp = self
            .http
            .post(format!("{}/get_pot", self.base_url))
            .timeout(Duration::from_secs(8))
            .json(&json!({
                "content_binding": content_binding,
                "bypass_cache": false,
            }))
            .send()
            .await
            .map_err(|e| StreamError::Transient(format!("bgutil get_pot failed: {e}")))?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| StreamError::Transient(format!("bgutil invalid json: {e}")))?;
        Ok(body
            .get("poToken")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned))
    }
}

impl PoTokenProvider for BgUtilHttpProvider {
    fn supports_web_botguard(&self) -> bool {
        true
    }

    fn mint<'a>(
        &'a self,
        video_id: &'a str,
        visitor_data: &'a str,
    ) -> BoxFuture<'a, Result<Option<PoTokenPair>, StreamError>> {
        Box::pin(async move {
            if !self.is_alive().await {
                return Ok(None);
            }
            let (player_pot, stream_pot) =
                tokio::join!(self.fetch_pot(visitor_data), self.fetch_pot(video_id));
            match (player_pot?, stream_pot?) {
                (Some(player_request_token), Some(streaming_data_token)) => Ok(Some(PoTokenPair {
                    player_request_token,
                    streaming_data_token,
                })),
                _ => Ok(None),
            }
        })
    }
}

// ---- Native InnerTube `/player` Stream Backend ------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignatureCipherParts {
    pub base_url: String,
    pub sig: String,
    pub sp: String,
}

/// Parses YouTube's URL-encoded `signatureCipher` / `cipher` string into
/// `(base_url, s, sp)`.
pub(crate) fn parse_signature_cipher(raw: &str) -> Option<SignatureCipherParts> {
    let mut base_url: Option<String> = None;
    let mut sig: Option<String> = None;
    let mut sp: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
        match k.as_ref() {
            "url" => base_url = Some(v.into_owned()),
            "s" => sig = Some(v.into_owned()),
            "sp" => sp = Some(v.into_owned()),
            _ => {}
        }
    }
    let base_url = base_url.filter(|s| !s.is_empty())?;
    let sig = sig.filter(|s| !s.is_empty())?;
    let sp = sp.filter(|s| !s.is_empty()).unwrap_or_else(|| "sig".to_owned());
    Some(SignatureCipherParts { base_url, sig, sp })
}

/// Extracts a self-contained `(s) => ...` JS decipher function from a YouTube
/// `base.js` player bundle when present.
pub(crate) fn extract_player_sig_transform(player_js: &str) -> Option<String> {
    // Match the classic entry function calling `.split("")` and `.join("")` on its input.
    let fn_re = regex::Regex::new(
        r#"(?:function\s+[A-Za-z0-9_$]+|(?:var|let|const)\s+[A-Za-z0-9_$]+\s*=\s*function|[A-Za-z0-9_$]+\s*=\s*function)\s*\(([A-Za-z0-9_$]+)\)\s*\{\s*([A-Za-z0-9_$]+)=([A-Za-z0-9_$]+)\.split\(""\);([^}]{1,600})return\s+[A-Za-z0-9_$]+\.join\(""\)\s*\}"#,
    )
    .ok()?;
    let caps = fn_re.captures(player_js)?;
    let arg = caps.get(1)?.as_str();
    let body = caps.get(4)?.as_str();

    // Find helper object name referenced inside body (e.g. `ab.cd(a,3)`).
    let helper_ref_re = regex::Regex::new(r"([A-Za-z0-9_$]+)\.[A-Za-z0-9_$]+\(").ok()?;
    let helper_decl = if let Some(hcap) = helper_ref_re.captures(body) {
        let helper_name = hcap.get(1)?.as_str();
        let escaped = regex::escape(helper_name);
        let obj_pat = format!(r"var\s+{escaped}\s*=\s*\{{[\s\S]{{1,1200}}?\}};");
        regex::Regex::new(&obj_pat)
            .ok()
            .and_then(|re| re.find(player_js))
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };

    Some(format!(
        "({arg}) => {{ {helper_decl} {arg} = {arg}.split(\"\"); {body} return {arg}.join(\"\"); }}"
    ))
}

/// Native-first YouTube stream resolver walking direct InnerTube `/player`
/// client profiles (`VISIONOS` / `IOS` JS-less direct Opus first for low-latency
/// public playback; `WEB_REMIX` + `TV_SAMSUNG` with QuickJS [`CipherSandbox`]
/// fallback for signed-in / private uploads / age-restricted tracks).
pub struct NativeStreamBackend {
    innertube: Arc<InnertubeClient>,
    po_token: Arc<dyn PoTokenProvider>,
    cipher: Arc<CipherSandbox>,
    player_endpoint: String,
    player_cache: Arc<Mutex<Option<String>>>,
    sig_transform_override: Option<String>,
}

impl NativeStreamBackend {
    pub fn new(innertube: Arc<InnertubeClient>) -> Self {
        Self::with_po_token(innertube, Arc::new(NoopPoTokenProvider))
    }

    pub fn with_po_token(
        innertube: Arc<InnertubeClient>,
        po_token: Arc<dyn PoTokenProvider>,
    ) -> Self {
        Self {
            innertube,
            po_token,
            cipher: Arc::new(CipherSandbox::default()),
            player_endpoint: "https://www.youtube.com/youtubei/v1/player".to_owned(),
            player_cache: Arc::new(Mutex::new(None)),
            sig_transform_override: None,
        }
    }

    /// Attaches a custom [`CipherSandbox`] (e.g. with a persistent player-JS cache dir).
    pub fn with_cipher_sandbox(mut self, cipher: Arc<CipherSandbox>) -> Self {
        self.cipher = cipher;
        self
    }

    #[cfg(test)]
    fn with_endpoint(innertube: Arc<InnertubeClient>, player_endpoint: String) -> Self {
        Self {
            innertube,
            po_token: Arc::new(NoopPoTokenProvider),
            cipher: Arc::new(CipherSandbox::default()),
            player_endpoint,
            player_cache: Arc::new(Mutex::new(None)),
            sig_transform_override: None,
        }
    }

    #[cfg(test)]
    fn with_sig_transform_override(mut self, fn_source: impl Into<String>) -> Self {
        self.sig_transform_override = Some(fn_source.into());
        self
    }

    fn profiles_for(&self, signed_in: bool, uploaded_hint: bool) -> Vec<PlayerClient> {
        if uploaded_hint {
            vec![
                PlayerClient::web_remix(Some(&self.innertube.web_remix_version())),
                PlayerClient::tv_samsung(),
                PlayerClient::vision_os(None),
            ]
        } else if signed_in {
            // Fast JS-less direct clients first; authenticated profiles follow for
            // age-restricted / premium / private tracks.
            vec![
                PlayerClient::vision_os(None),
                PlayerClient::ios(),
                PlayerClient::web_remix(Some(&self.innertube.web_remix_version())),
                PlayerClient::tv_samsung(),
            ]
        } else {
            vec![
                PlayerClient::vision_os(None),
                PlayerClient::ios(),
                PlayerClient::web_remix(Some(&self.innertube.web_remix_version())),
            ]
        }
    }

    async fn resolve_player_sig_transform(&self) -> Result<String, StreamError> {
        if let Some(ref ov) = self.sig_transform_override {
            return Ok(ov.clone());
        }
        {
            let guard = self.player_cache.lock().await;
            if let Some(ref cached) = *guard {
                return Ok(cached.clone());
            }
        }

        // Fetch lightweight `/iframe_api` to discover current player hash and base.js URL.
        let api_text = self
            .innertube
            .http_client()
            .get(format!("{YOUTUBE_ORIGIN}/iframe_api"))
            .timeout(Duration::from_secs(6))
            .header(reqwest::header::USER_AGENT, WEB_USER_AGENT)
            .send()
            .await
            .map_err(|e| StreamError::Transient(format!("iframe_api fetch failed: {e}")))?
            .text()
            .await
            .map_err(|e| StreamError::Transient(format!("iframe_api read failed: {e}")))?;

        let hash_re = regex::Regex::new(r"player\\?/([A-Za-z0-9_-]{6,32})\\?/").ok();
        let player_hash = hash_re
            .and_then(|re| re.captures(&api_text))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
            .ok_or_else(|| {
                StreamError::Transient("unable to locate YouTube player hash".to_owned())
            })?;

        if let Some(saved_fn) = self.cipher.load_preprocessed_player(&player_hash) {
            let mut guard = self.player_cache.lock().await;
            *guard = Some(saved_fn.clone());
            return Ok(saved_fn);
        }

        let base_js_url =
            format!("{YOUTUBE_ORIGIN}/s/player/{player_hash}/player_ias.vflset/en_US/base.js");
        let player_js = self
            .innertube
            .http_client()
            .get(&base_js_url)
            .timeout(Duration::from_secs(10))
            .header(reqwest::header::USER_AGENT, WEB_USER_AGENT)
            .send()
            .await
            .map_err(|e| StreamError::Transient(format!("base.js fetch failed: {e}")))?
            .text()
            .await
            .map_err(|e| StreamError::Transient(format!("base.js read failed: {e}")))?;

        let fn_source = extract_player_sig_transform(&player_js).ok_or_else(|| {
            StreamError::Transient("unable to extract signature transform from base.js".to_owned())
        })?;
        let _ = self
            .cipher
            .save_preprocessed_player(&player_hash, &fn_source);

        let mut guard = self.player_cache.lock().await;
        *guard = Some(fn_source.clone());
        Ok(fn_source)
    }

    async fn solve_signature_cipher(
        &self,
        parts: &SignatureCipherParts,
    ) -> Result<String, StreamError> {
        let fn_source = self.resolve_player_sig_transform().await?;
        let arg_json = serde_json::to_string(&parts.sig)
            .map_err(|e| StreamError::Transient(format!("encode cipher arg: {e}")))?;
        let script = format!("String(({fn_source})({arg_json}))");
        let decrypted = self.cipher.eval_to_string_async(script).await?;
        if decrypted.is_empty() {
            return Err(StreamError::Transient(
                "QuickJS signature decipher returned empty string".to_owned(),
            ));
        }
        let mut parsed = url::Url::parse(&parts.base_url)
            .map_err(|e| StreamError::Transient(format!("invalid cipher base url: {e}")))?;
        parsed.query_pairs_mut().append_pair(&parts.sp, &decrypted);
        Ok(parsed.into())
    }

    async fn resolve_with_client(
        &self,
        client: &PlayerClient,
        req: &ResolveRequest,
        visitor_data: Option<&str>,
    ) -> Result<ResolvedStream, StreamError> {
        let cookie = if client.supports_cookies {
            self.innertube.cookie()
        } else {
            None
        };
        let signed_in = cookie.is_some();

        let pot = if client.requires_js_player && self.po_token.supports_web_botguard() {
            if let Some(vd) = visitor_data {
                self.po_token.mint(&req.video_id, vd).await.ok().flatten()
            } else {
                None
            }
        } else {
            None
        };

        let mut client_ctx = serde_json::Map::new();
        client_ctx.insert(
            "clientName".to_owned(),
            Value::String(client.client_name.clone()),
        );
        client_ctx.insert(
            "clientVersion".to_owned(),
            Value::String(client.client_version.clone()),
        );
        client_ctx.insert(
            "hl".to_owned(),
            Value::String(self.innertube.current_language()),
        );
        client_ctx.insert(
            "gl".to_owned(),
            Value::String(self.innertube.current_region().to_owned()),
        );
        if let Some(vd) = visitor_data {
            client_ctx.insert("visitorData".to_owned(), Value::String(vd.to_owned()));
        }

        let mut body = json!({
            "videoId": req.video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
            "context": {
                "client": Value::Object(client_ctx),
                "user": { "lockedSafetyMode": false },
            }
        });
        if let Some(ref pair) = pot {
            body["serviceIntegrityDimensions"] = json!({ "poToken": pair.player_request_token });
        }

        let endpoint =
            if client.client_name == "WEB_REMIX" && self.player_endpoint.contains("youtube.com") {
                "https://music.youtube.com/youtubei/v1/player"
            } else {
                &self.player_endpoint
            };

        let mut http_req = self
            .innertube
            .http_client()
            .post(format!("{endpoint}?prettyPrint=false"))
            .timeout(Duration::from_secs(12))
            .header(reqwest::header::USER_AGENT, &client.user_agent)
            .header("X-YouTube-Client-Name", client.client_id.to_string())
            .header("X-YouTube-Client-Version", &client.client_version);

        if let Some(vd) = visitor_data {
            http_req = http_req.header("X-Goog-Visitor-Id", vd);
        }
        if let Some(ref origin) = client.origin {
            http_req = http_req
                .header(reqwest::header::ORIGIN, origin)
                .header("X-Origin", origin)
                .header(reqwest::header::REFERER, format!("{origin}/"));
        }
        if let Some(ref c) = cookie {
            let origin = client
                .origin
                .as_deref()
                .unwrap_or(if endpoint.contains("music.") {
                    MUSIC_ORIGIN
                } else {
                    YOUTUBE_ORIGIN
                });
            http_req = http_req.header(reqwest::header::COOKIE, c);
            if let Some(sapisid) = sapisid_from_cookie(c) {
                http_req = http_req.header(
                    reqwest::header::AUTHORIZATION,
                    sapisid_hash(&sapisid, origin),
                );
            }
        }

        let resp = http_req
            .json(&body)
            .send()
            .await
            .map_err(|e| StreamError::Transient(format!("player request failed: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| StreamError::Transient(format!("player read failed: {e}")))?;
        if !status.is_success() {
            return Err(StreamError::Refused {
                status: status.as_u16(),
                profile_id: client.client_name.clone(),
                detail: text.chars().take(160).collect(),
            });
        }
        let payload: Value = serde_json::from_str(&text)
            .map_err(|e| StreamError::Transient(format!("invalid player json: {e}")))?;

        check_playability(&payload, signed_in)?;

        let loudness_db = payload
            .get("playerConfig")
            .and_then(|p| p.get("audioConfig"))
            .and_then(|a| a.get("loudnessDb"))
            .and_then(Value::as_f64);

        let streaming = payload
            .get("streamingData")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                StreamError::Transient(format!(
                    "{} player response had no streamingData",
                    client.client_name
                ))
            })?;

        // Check live HLS manifest first if the broadcast is live.
        let is_live = payload
            .get("videoDetails")
            .and_then(|v| v.get("isLive"))
            .and_then(Value::as_bool)
            == Some(true);
        if is_live && let Some(hls_url) = streaming.get("hlsManifestUrl").and_then(Value::as_str) {
            return Ok(ResolvedStream {
                video_id: req.video_id.clone(),
                url: hls_url.to_owned(),
                kbps: 128,
                mime_type: "application/vnd.apple.mpegurl".to_owned(),
                ext: "m3u8".to_owned(),
                content_length: None,
                duration_ms: None,
                loudness_db,
                is_live_hls: true,
                client_name: client.client_name.clone(),
                profile_id: format!("native:{}", client.client_name),
                media_headers: client.media_headers(),
                range_chunk_bytes: None,
            });
        }

        let details_duration_ms = payload
            .get("videoDetails")
            .and_then(|v| v.get("lengthSeconds"))
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse::<u64>().ok()))
            .filter(|&s| s > 0)
            .map(|s| s.saturating_mul(1_000));

        let formats = streaming
            .get("adaptiveFormats")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                StreamError::Transient(format!(
                    "{} response contained no adaptiveFormats",
                    client.client_name
                ))
            })?;

        let chosen =
            select_best_audio_format(formats, req.max_kbps, req.container).ok_or_else(|| {
                StreamError::Transient(format!(
                    "{} offered no playable audio format",
                    client.client_name
                ))
            })?;

        let mut final_url = match chosen.source {
            FormatUrlSource::Direct(url) => url,
            FormatUrlSource::SignatureCipher(parts) => self.solve_signature_cipher(&parts).await?,
        };
        if let Some(ref pair) = pot
            && !final_url.contains("pot=")
            && let Ok(mut parsed) = url::Url::parse(&final_url)
        {
            parsed
                .query_pairs_mut()
                .append_pair("pot", &pair.streaming_data_token);
            final_url = parsed.into();
        }

        let range_chunk_bytes = PlayerClient::range_bytes_for(&final_url);
        let content_length = chosen
            .content_length
            .or_else(|| PlayerClient::content_length_from_url(&final_url));
        let duration_ms = chosen
            .approx_duration_ms
            .filter(|&d| d > 0)
            .or_else(|| PlayerClient::duration_ms_from_url(&final_url))
            .or(details_duration_ms);

        Ok(ResolvedStream {
            video_id: req.video_id.clone(),
            url: final_url,
            kbps: chosen.kbps,
            mime_type: chosen.mime_type,
            ext: chosen.ext,
            content_length,
            duration_ms,
            loudness_db,
            is_live_hls: false,
            client_name: client.client_name.clone(),
            profile_id: format!("native:{}", client.client_name),
            media_headers: client.media_headers(),
            range_chunk_bytes,
        })
    }
}

impl StreamBackend for NativeStreamBackend {
    fn resolve_uncached<'a>(
        &'a self,
        req: &'a ResolveRequest,
    ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>> {
        Box::pin(async move {
            let visitor_data = self.innertube.ensure_visitor_data(false).await;
            let signed_in = self.innertube.is_signed_in();
            let profiles = self.profiles_for(signed_in, req.uploaded_hint);

            let mut last_err =
                StreamError::Transient("no native player client succeeded".to_owned());
            for client in profiles {
                let profile_id = format!("native:{}", client.client_name);
                if req.skip_profiles.iter().any(|p| p == &profile_id) {
                    continue;
                }
                match self
                    .resolve_with_client(&client, req, visitor_data.as_deref())
                    .await
                {
                    Ok(stream) => return Ok(stream),
                    Err(StreamError::Permanent(reason)) => {
                        // When signed in, an anonymous fast profile (VISIONOS/IOS) may hit
                        // an auth-gated reason that WEB_REMIX/TV_SAMSUNG can satisfy.
                        let retry_with_auth = signed_in
                            && !client.supports_cookies
                            && matches!(
                                reason,
                                PermanentReason::AgeRestrictedSignedOut
                                    | PermanentReason::PremiumRequired
                                    | PermanentReason::Private
                            );
                        if retry_with_auth {
                            last_err = StreamError::Permanent(reason);
                            continue;
                        }
                        return Err(StreamError::Permanent(reason));
                    }
                    Err(err) => last_err = err,
                }
            }
            Err(last_err)
        })
    }
}

enum FormatUrlSource {
    Direct(String),
    SignatureCipher(SignatureCipherParts),
}

struct SelectedFormat {
    source: FormatUrlSource,
    kbps: u32,
    mime_type: String,
    ext: String,
    content_length: Option<u64>,
    approx_duration_ms: Option<u64>,
}

fn select_best_audio_format(
    formats: &[Value],
    max_kbps: u32,
    pref: ContainerPreference,
) -> Option<SelectedFormat> {
    let mut candidates: Vec<(SelectedFormat, bool, bool, bool)> = Vec::new();
    for fmt in formats {
        let Some(obj) = fmt.as_object() else {
            continue;
        };
        let Some(mime) = obj.get("mimeType").and_then(Value::as_str) else {
            continue;
        };
        if !mime.starts_with("audio/") {
            continue;
        }
        let (source, is_direct) = if let Some(url) = obj.get("url").and_then(Value::as_str) {
            (FormatUrlSource::Direct(url.to_owned()), true)
        } else if let Some(raw_cipher) = obj
            .get("signatureCipher")
            .or_else(|| obj.get("cipher"))
            .and_then(Value::as_str)
            && let Some(parts) = parse_signature_cipher(raw_cipher)
        {
            (FormatUrlSource::SignatureCipher(parts), false)
        } else {
            continue;
        };
        let is_webm_opus = mime.contains("webm") && mime.contains("opus");
        let is_m4a = mime.contains("mp4");
        let ext = if is_webm_opus {
            "webm"
        } else if is_m4a {
            "m4a"
        } else {
            continue;
        };
        match pref {
            ContainerPreference::WebmOpus if !is_webm_opus => continue,
            ContainerPreference::M4aAac if !is_m4a => continue,
            _ => {}
        }
        let bitrate = obj
            .get("averageBitrate")
            .or_else(|| obj.get("bitrate"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let kbps = ((bitrate / 1_000) as u32).max(1);
        let content_length = obj
            .get("contentLength")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
        let approx_duration_ms = obj
            .get("approxDurationMs")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
        let within_cap = max_kbps == 0 || kbps <= max_kbps;
        candidates.push((
            SelectedFormat {
                source,
                kbps,
                mime_type: mime.to_owned(),
                ext: ext.to_owned(),
                content_length,
                approx_duration_ms,
            },
            within_cap,
            is_direct,
            is_webm_opus,
        ));
    }

    if candidates.is_empty() {
        return None;
    }

    // Prefer formats within max_kbps (direct URL > cipher, highest kbps, Opus > M4A);
    // if every format exceeds the cap, fall back to the lowest bitrate so playback still works.
    let has_within_cap = candidates.iter().any(|(_, ok, _, _)| *ok);
    if has_within_cap {
        candidates
            .into_iter()
            .filter(|(_, ok, _, _)| *ok)
            .max_by_key(|(f, _, is_direct, is_opus)| (*is_direct, f.kbps, *is_opus))
            .map(|(f, _, _, _)| f)
    } else {
        candidates
            .into_iter()
            .min_by_key(|(f, _, is_direct, _)| (!*is_direct, f.kbps))
            .map(|(f, _, _, _)| f)
    }
}

fn check_playability(payload: &Value, signed_in: bool) -> Result<(), StreamError> {
    let Some(ps) = payload.get("playabilityStatus").and_then(Value::as_object) else {
        return Ok(());
    };
    let status = ps.get("status").and_then(Value::as_str).unwrap_or("OK");
    if status == "OK" {
        return Ok(());
    }
    let reason = ps.get("reason").and_then(Value::as_str).unwrap_or_default();
    if let Some(perm) = classify_permanent_reason(reason, signed_in) {
        return Err(StreamError::Permanent(perm));
    }
    if reason
        .to_ascii_lowercase()
        .contains("confirm you're not a bot")
        || reason.to_ascii_lowercase().contains("not a bot")
    {
        return Err(StreamError::RateLimited);
    }
    Err(StreamError::Transient(format!(
        "playabilityStatus={status}: {reason}"
    )))
}

/// Classifies YouTube/yt-dlp error messages into terminal [`PermanentReason`]s.
pub fn classify_permanent_reason(reason: &str, signed_in: bool) -> Option<PermanentReason> {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("confirm your age") || lower.contains("age-restricted") {
        return Some(if signed_in {
            PermanentReason::AgeRestrictedSignedIn
        } else {
            PermanentReason::AgeRestrictedSignedOut
        });
    }
    if lower.contains("in your country") || lower.contains("geo-restricted") {
        return Some(PermanentReason::GeoRestricted);
    }
    if lower.contains("music premium") || lower.contains("members-only") {
        return Some(PermanentReason::PremiumRequired);
    }
    if lower.contains("requires payment") || lower.contains("paid content") {
        return Some(PermanentReason::PaidContent);
    }
    if lower.contains("private video") || lower.contains("this video is private") {
        return Some(PermanentReason::Private);
    }
    if lower.contains("account associated with this video has been terminated") {
        return Some(PermanentReason::AccountTerminated);
    }
    if lower.contains("video has been removed") || lower.contains("copyright claim") {
        return Some(PermanentReason::Removed);
    }
    None
}

// ---- Feature-Gated `yt-dlp` Dev Bridge (ADR-004) ----------------------------

#[cfg(feature = "ytdlp-bridge")]
pub mod ytdlp {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use serde::Deserialize;
    use tokio::process::Command;

    use super::{
        BoxFuture, ContainerPreference, ResolveRequest, ResolvedStream, StreamBackend,
        classify_permanent_reason,
    };
    use crate::error::StreamError;
    use crate::player_client::PlayerClient;

    const YTDLP_TIMEOUT: Duration = Duration::from_secs(15);

    #[derive(Debug, Deserialize)]
    pub(crate) struct YtDlpSingleJson {
        pub id: String,
        pub url: Option<String>,
        pub http_headers: Option<BTreeMap<String, String>>,
        pub format_id: Option<String>,
        pub ext: Option<String>,
        pub acodec: Option<String>,
        pub abr: Option<f64>,
        pub tbr: Option<f64>,
        pub filesize: Option<u64>,
        pub filesize_approx: Option<u64>,
        pub duration: Option<f64>,
        pub protocol: Option<String>,
        pub is_live: Option<bool>,
    }

    /// Feature-gated (`ytdlp-bridge`) subprocess bridge using local `yt-dlp`
    /// with low-latency audio-only flags (`rust/research/youtube-extraction.md` §3.3).
    #[derive(Debug, Clone)]
    pub struct YtDlpStreamBackend {
        binary: String,
        cache_dir: Option<PathBuf>,
        js_runtime: Option<String>,
    }

    impl Default for YtDlpStreamBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    impl YtDlpStreamBackend {
        pub fn new() -> Self {
            let cache_dir = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .map(|base| base.join("bitchord").join("yt-dlp"));
            Self {
                binary: "yt-dlp".to_owned(),
                cache_dir,
                js_runtime: Some("node".to_owned()),
            }
        }

        pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
            self.binary = binary.into();
            self
        }

        pub(crate) fn format_selector(max_kbps: u32, pref: ContainerPreference) -> String {
            let cap = if max_kbps > 0 && max_kbps < u32::MAX {
                format!("[abr<={max_kbps}]")
            } else {
                String::new()
            };
            match pref {
                ContainerPreference::WebmOpus => {
                    format!(
                        "bestaudio[ext=webm][acodec=opus]{cap}/bestaudio[ext=webm][acodec=opus]/bestaudio"
                    )
                }
                ContainerPreference::M4aAac => {
                    format!("bestaudio[ext=m4a]{cap}/bestaudio[ext=m4a]/bestaudio")
                }
                ContainerPreference::Any => {
                    format!(
                        "bestaudio[ext=webm][acodec=opus]{cap}/bestaudio[ext=m4a]{cap}/bestaudio/best"
                    )
                }
            }
        }

        pub(crate) fn parse_single_json(
            raw: &[u8],
            req: &ResolveRequest,
        ) -> Result<ResolvedStream, StreamError> {
            let info: YtDlpSingleJson = serde_json::from_slice(raw)
                .map_err(|e| StreamError::Transient(format!("yt-dlp invalid json: {e}")))?;
            let url = info.url.ok_or_else(|| {
                StreamError::Transient("yt-dlp returned no direct stream url".to_owned())
            })?;
            let ext = info.ext.unwrap_or_else(|| "webm".to_owned());
            let acodec = info.acodec.unwrap_or_default();
            let mime_type = if ext == "m4a" || acodec.contains("mp4a") {
                r#"audio/mp4; codecs="mp4a.40.2""#.to_owned()
            } else if ext == "m3u8" || info.protocol.as_deref() == Some("m3u8_native") {
                "application/vnd.apple.mpegurl".to_owned()
            } else {
                r#"audio/webm; codecs="opus""#.to_owned()
            };
            let kbps = info
                .abr
                .or(info.tbr)
                .map(|v| v.round().max(1.0) as u32)
                .unwrap_or(128);
            let content_length = info
                .filesize
                .or(info.filesize_approx)
                .or_else(|| PlayerClient::content_length_from_url(&url));
            let duration_ms = info
                .duration
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(|d| (d * 1_000.0).round() as u64)
                .or_else(|| PlayerClient::duration_ms_from_url(&url));
            let inferred_client = PlayerClient::for_stream_url(&url);
            let media_headers = info
                .http_headers
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| inferred_client.media_headers());
            let range_chunk_bytes = PlayerClient::range_bytes_for(&url);
            let profile_id = format!(
                "ytdlp:{}:{}",
                inferred_client.client_name,
                info.format_id.as_deref().unwrap_or("auto")
            );
            Ok(ResolvedStream {
                video_id: if info.id.is_empty() {
                    req.video_id.clone()
                } else {
                    info.id
                },
                url,
                kbps,
                mime_type,
                ext,
                content_length,
                duration_ms,
                // yt-dlp does not extract playerConfig.audioConfig.loudnessDb
                loudness_db: None,
                is_live_hls: info.is_live == Some(true)
                    || info.protocol.as_deref() == Some("m3u8_native"),
                client_name: inferred_client.client_name,
                profile_id,
                media_headers,
                range_chunk_bytes,
            })
        }
    }

    impl StreamBackend for YtDlpStreamBackend {
        fn resolve_uncached<'a>(
            &'a self,
            req: &'a ResolveRequest,
        ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>> {
            Box::pin(async move {
                let mut cmd = Command::new(&self.binary);
                cmd.kill_on_drop(true)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .arg("--dump-single-json")
                    .arg("--no-playlist")
                    .arg("--no-warnings")
                    .arg("--skip-download")
                    .arg("-f")
                    .arg(Self::format_selector(req.max_kbps, req.container))
                    .arg("--extractor-args")
                    .arg(
                        "youtube:player_client=visionos,tv_samsung,web;player_skip=webpage,configs,initial_data;skip=hls,dash,translated_subs",
                    );
                if let Some(ref cache) = self.cache_dir {
                    cmd.arg("--cache-dir").arg(cache);
                }
                if let Some(ref rt) = self.js_runtime {
                    cmd.arg("--js-runtimes").arg(rt);
                }
                cmd.arg("--")
                    .arg(format!("https://www.youtube.com/watch?v={}", req.video_id));

                let output = tokio::time::timeout(YTDLP_TIMEOUT, cmd.output())
                    .await
                    .map_err(|_| StreamError::Transient("yt-dlp timed out after 15s".to_owned()))?
                    .map_err(|e| StreamError::Transient(format!("failed to spawn yt-dlp: {e}")))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if let Some(perm) = classify_permanent_reason(&stderr, false) {
                        return Err(StreamError::Permanent(perm));
                    }
                    if stderr.to_ascii_lowercase().contains("not a bot") {
                        return Err(StreamError::RateLimited);
                    }
                    return Err(StreamError::Transient(format!(
                        "yt-dlp exited {}: {}",
                        output.status,
                        stderr.trim().chars().take(240).collect::<String>()
                    )));
                }

                Self::parse_single_json(&output.stdout, req)
            })
        }
    }
}

#[cfg(feature = "ytdlp-bridge")]
pub use ytdlp::YtDlpStreamBackend;

/// Chains a primary backend (native InnerTube) with a secondary fallback
/// backend (e.g. `yt-dlp` dev bridge), short-circuiting on permanent errors.
pub struct FallbackStreamBackend<P, S> {
    primary: P,
    secondary: S,
}

impl<P, S> FallbackStreamBackend<P, S> {
    pub fn new(primary: P, secondary: S) -> Self {
        Self { primary, secondary }
    }
}

impl<P: StreamBackend, S: StreamBackend> StreamBackend for FallbackStreamBackend<P, S> {
    fn resolve_uncached<'a>(
        &'a self,
        req: &'a ResolveRequest,
    ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>> {
        Box::pin(async move {
            match self.primary.resolve_uncached(req).await {
                Ok(stream) => Ok(stream),
                Err(StreamError::Permanent(reason)) => Err(StreamError::Permanent(reason)),
                Err(_) => self.secondary.resolve_uncached(req).await,
            }
        })
    }

    fn on_refused(&self, video_id: &str, profile_id: &str, url: &str) {
        self.primary.on_refused(video_id, profile_id, url);
        self.secondary.on_refused(video_id, profile_id, url);
    }

    fn on_session_changed(&self) {
        self.primary.on_session_changed();
        self.secondary.on_session_changed();
    }
}

// ---- StreamResolver Orchestrator (Caching, Coalescing, Probing) -------------

#[derive(Debug, Clone)]
struct CachedStream {
    stream: ResolvedStream,
    inserted_at: Instant,
}

#[derive(Debug, Default)]
struct RecentCache {
    entries: HashMap<String, CachedStream>,
    order: VecDeque<String>,
}

impl RecentCache {
    fn get(&mut self, key: &str) -> Option<ResolvedStream> {
        let now = Instant::now();
        let valid = self
            .entries
            .get(key)
            .is_some_and(|e| now.duration_since(e.inserted_at) < RECENT_TTL);
        if !valid {
            self.entries.remove(key);
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                self.order.remove(pos);
            }
            return None;
        }
        let stream = self.entries.get(key)?.stream.clone();
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_owned());
        Some(stream)
    }

    fn insert(&mut self, key: String, stream: ResolvedStream) {
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        } else if self.entries.len() >= RECENT_MAX_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CachedStream {
                stream,
                inserted_at: Instant::now(),
            },
        );
    }

    fn remove(&mut self, key: &str) {
        self.entries.remove(key);
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

/// Controls whether [`StreamResolver`] performs a blocking 1 MiB range probe
/// before returning a freshly resolved stream URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProbePolicy {
    /// Always run a blocking 1 MiB range probe before returning uncached URLs.
    Always,
    /// Skip the extra blocking probe round-trip for known-healthy fast profiles
    /// (`native:VISIONOS`, `native:IOS`) unless that profile recently suffered a
    /// refusal; still probe fallback/unproven profiles synchronously.
    #[default]
    Optimistic,
    /// Disable network probing completely.
    Disabled,
}

fn is_optimistic_healthy_profile(profile_id: &str) -> bool {
    matches!(profile_id, "native:VISIONOS" | "native:IOS")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeOutcome {
    Ok,
    Refused(u16, String),
    Unreachable(String),
}

type InFlightValue = Option<Result<ResolvedStream, StreamError>>;

/// Layered, single-flight coalesced, probe-verified stream resolver facade.
pub struct StreamResolver {
    http: Arc<Client>,
    backend: Arc<dyn StreamBackend>,
    uploads: Arc<UploadedTracks>,
    recent: Arc<RwLock<RecentCache>>,
    unplayable: Arc<RwLock<HashMap<String, (PermanentReason, Instant)>>>,
    excluded: Arc<RwLock<HashMap<(String, String), Instant>>>,
    degraded_profiles: Arc<RwLock<HashMap<String, Instant>>>,
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<InFlightValue>>>>,
    probe_policy: ProbePolicy,
}

impl StreamResolver {
    pub fn new(
        http: Arc<Client>,
        backend: Arc<dyn StreamBackend>,
        uploads: Arc<UploadedTracks>,
    ) -> Self {
        Self {
            http,
            backend,
            uploads,
            recent: Arc::new(RwLock::new(RecentCache::default())),
            unplayable: Arc::new(RwLock::new(HashMap::new())),
            excluded: Arc::new(RwLock::new(HashMap::new())),
            degraded_profiles: Arc::new(RwLock::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            probe_policy: ProbePolicy::Optimistic,
        }
    }

    /// Configures how aggressively [`StreamResolver`] probes candidate URLs.
    pub fn with_probe_policy(mut self, policy: ProbePolicy) -> Self {
        self.probe_policy = policy;
        self
    }

    /// Enables (`Always`) or disables (`Disabled`) network probing.
    pub fn with_probing(mut self, enabled: bool) -> Self {
        self.probe_policy = if enabled {
            ProbePolicy::Always
        } else {
            ProbePolicy::Disabled
        };
        self
    }

    /// Canonical cache key shared with persistent storage caches (`stream_cache`).
    pub fn cache_key_for(req: &ResolveRequest) -> String {
        format!("{}:{}:{:?}", req.video_id, req.max_kbps, req.container)
    }

    /// Seeds the in-memory recent cache (e.g. from persistent SQLite `stream_cache`).
    pub fn seed_cached_stream(&self, req: &ResolveRequest, stream: ResolvedStream) {
        let key = Self::cache_key_for(req);
        self.recent
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, stream);
    }

    /// Resolves a playable audio URL for `req`, serving from the 20m cache,
    /// coalescing concurrent identical lookups, and probing according to [`ProbePolicy`].
    pub async fn resolve(&self, req: ResolveRequest) -> Result<ResolvedStream, StreamError> {
        let cache_key = Self::cache_key_for(&req);

        // 1. Check 20-minute latency cache.
        if let Some(hit) = self
            .recent
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cache_key)
        {
            return Ok(hit);
        }

        // 2. Check 10-minute permanent unplayable verdict cache.
        {
            let mut guard = self.unplayable.write().unwrap_or_else(|e| e.into_inner());
            if let Some((reason, at)) = guard.get(&req.video_id) {
                if at.elapsed() < UNPLAYABLE_TTL {
                    return Err(StreamError::Permanent(reason.clone()));
                }
                guard.remove(&req.video_id);
            }
        }

        // 3. Single-flight coalescing: join an existing task or spawn a detached
        //    task that removes itself from `in_flight` on completion (never on caller drop).
        let mut rx = {
            let mut map = self.in_flight.lock().await;
            if let Some(existing) = map.get(&cache_key) {
                existing.clone()
            } else {
                let (tx, rx) = watch::channel(None);
                map.insert(cache_key.clone(), rx.clone());

                let http = Arc::clone(&self.http);
                let backend = Arc::clone(&self.backend);
                let uploads = Arc::clone(&self.uploads);
                let recent = Arc::clone(&self.recent);
                let unplayable = Arc::clone(&self.unplayable);
                let excluded = Arc::clone(&self.excluded);
                let degraded_profiles = Arc::clone(&self.degraded_profiles);
                let in_flight = Arc::clone(&self.in_flight);
                let probe_policy = self.probe_policy;
                let key = cache_key.clone();

                tokio::spawn(async move {
                    let result = resolve_and_probe(
                        &http,
                        backend.as_ref(),
                        &uploads,
                        &excluded,
                        &degraded_profiles,
                        req,
                        probe_policy,
                    )
                    .await;
                    match &result {
                        Ok(stream) => {
                            recent
                                .write()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(key.clone(), stream.clone());
                        }
                        Err(StreamError::Permanent(reason)) => {
                            unplayable
                                .write()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(
                                    key.split(':').next().unwrap_or_default().to_owned(),
                                    (reason.clone(), Instant::now()),
                                );
                        }
                        Err(_) => {}
                    }
                    let _ = tx.send(Some(result));
                    in_flight.lock().await.remove(&key);
                });
                rx
            }
        };

        loop {
            if let Some(res) = rx.borrow().clone() {
                return res;
            }
            if rx.changed().await.is_err() {
                return Err(StreamError::Transient(
                    "in-flight stream resolution terminated unexpectedly".to_owned(),
                ));
            }
        }
    }

    /// Reports a mid-playback 403/404/410 refusal: evicts cached URLs for
    /// `video_id`, marks `profile_id` as degraded (re-enabling synchronous probing
    /// under [`ProbePolicy::Optimistic`]), and benches `(video_id, profile_id)` for 10 minutes.
    pub fn on_playback_refused(&self, video_id: &str, profile_id: &str, url: &str) {
        let now = Instant::now();
        self.excluded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((video_id.to_owned(), profile_id.to_owned()), now);
        self.degraded_profiles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(profile_id.to_owned(), now);
        {
            let mut recent = self.recent.write().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<String> = recent
                .entries
                .keys()
                .filter(|k| k.starts_with(&format!("{video_id}:")))
                .cloned()
                .collect();
            for k in keys {
                recent.remove(&k);
            }
        }
        self.backend.on_refused(video_id, profile_id, url);
    }

    /// Invalidates every cache upon sign-in, sign-out, or channel switch.
    pub fn on_session_changed(&self) {
        self.recent
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.unplayable
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.excluded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.degraded_profiles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.uploads.clear();
        self.backend.on_session_changed();
    }
}

async fn resolve_and_probe(
    http: &Client,
    backend: &dyn StreamBackend,
    uploads: &UploadedTracks,
    excluded: &RwLock<HashMap<(String, String), Instant>>,
    degraded_profiles: &RwLock<HashMap<String, Instant>>,
    mut req: ResolveRequest,
    probe_policy: ProbePolicy,
) -> Result<ResolvedStream, StreamError> {
    if uploads.contains(&req.video_id) {
        req.uploaded_hint = true;
    }

    // Seed skip_profiles with non-expired exclusions.
    {
        let mut guard = excluded.write().unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, at| at.elapsed() < EXCLUDE_TTL);
        for (vid, profile) in guard.keys() {
            if vid == &req.video_id && !req.skip_profiles.contains(profile) {
                req.skip_profiles.push(profile.clone());
            }
        }
    }

    let mut last_err = StreamError::Transient("stream resolution failed".to_owned());
    for _attempt in 0..MAX_RESOLVE_ATTEMPTS {
        let stream = match backend.resolve_uncached(&req).await {
            Ok(s) => s,
            Err(StreamError::Permanent(reason)) => return Err(StreamError::Permanent(reason)),
            Err(err) => {
                last_err = err;
                continue;
            }
        };

        if stream.is_live_hls || probe_policy == ProbePolicy::Disabled {
            return Ok(stream);
        }

        if probe_policy == ProbePolicy::Optimistic
            && is_optimistic_healthy_profile(&stream.profile_id)
        {
            let is_degraded = {
                let mut guard = degraded_profiles
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                guard.retain(|_, at| at.elapsed() < EXCLUDE_TTL);
                guard.contains_key(&stream.profile_id)
            };
            if !is_degraded {
                return Ok(stream);
            }
        }

        match probe_stream_url(http, &stream).await {
            ProbeOutcome::Ok => return Ok(stream),
            ProbeOutcome::Refused(status, detail) => {
                let now = Instant::now();
                excluded
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert((req.video_id.clone(), stream.profile_id.clone()), now);
                degraded_profiles
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(stream.profile_id.clone(), now);
                if !req.skip_profiles.contains(&stream.profile_id) {
                    req.skip_profiles.push(stream.profile_id.clone());
                }
                backend.on_refused(&req.video_id, &stream.profile_id, &stream.url);
                last_err = StreamError::Refused {
                    status,
                    profile_id: stream.profile_id,
                    detail,
                };
            }
            ProbeOutcome::Unreachable(detail) => {
                last_err = StreamError::Transient(detail);
            }
        }
    }
    Err(last_err)
}

/// Probes a candidate stream URL past YouTube's 1 MiB auth wall, requiring an
/// `audio/*` Content-Type and 16 KiB of readable audio bytes within 6 seconds.
async fn probe_stream_url(http: &Client, stream: &ResolvedStream) -> ProbeOutcome {
    let clen = stream
        .content_length
        .or_else(|| PlayerClient::content_length_from_url(&stream.url));
    let start: u64 = match clen {
        Some(len) if len > AUTH_BOUNDARY_BYTES + PROBE_READ_BYTES as u64 => AUTH_BOUNDARY_BYTES,
        _ => 0,
    };
    let max_span = stream
        .range_chunk_bytes
        .unwrap_or(AUTH_BOUNDARY_BYTES)
        .min(PROBE_READ_BYTES as u64);
    let mut end = start.saturating_add(max_span.saturating_sub(1));
    if let Some(len) = clen
        && len > 0
    {
        end = end.min(len.saturating_sub(1));
    }

    let mut req = http
        .get(&stream.url)
        .timeout(PROBE_TIMEOUT)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"));
    for (k, v) in &stream.media_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let mut resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return ProbeOutcome::Unreachable(format!("probe transport failed: {e}")),
    };

    let code = resp.status().as_u16();
    if matches!(code, 403 | 404 | 410) {
        return ProbeOutcome::Refused(code, format!("probe HTTP {code}"));
    }
    if !resp.status().is_success() && code != 416 {
        return ProbeOutcome::Unreachable(format!("probe HTTP {code}"));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("audio/") {
        return ProbeOutcome::Refused(code, format!("unexpected Content-Type: {content_type}"));
    }

    let expected_read = match clen {
        Some(len) => (len.saturating_sub(start) as usize).min(PROBE_READ_BYTES),
        None => PROBE_READ_BYTES,
    };
    let mut read = 0usize;
    while read < expected_read {
        match resp.chunk().await {
            Ok(Some(chunk)) => read += chunk.len(),
            Ok(None) => break,
            Err(e) => return ProbeOutcome::Unreachable(format!("probe read stalled: {e}")),
        }
    }
    if read < expected_read && expected_read > 0 {
        return ProbeOutcome::Unreachable(format!(
            "probe truncated at {read}/{expected_read} bytes"
        ));
    }
    ProbeOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::build_shared_http_client;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_media_server(
        handler: impl Fn(String) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handler = Arc::new(handler);

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req_str = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (code, ct, body) = handler(req_str);
                    let reason = if code == 206 {
                        "Partial Content"
                    } else if code == 200 {
                        "OK"
                    } else {
                        "Forbidden"
                    };
                    let head = format!(
                        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    struct SwitchingBackend {
        bad_url: String,
        good_url: String,
        calls: Arc<AtomicUsize>,
    }

    impl StreamBackend for SwitchingBackend {
        fn resolve_uncached<'a>(
            &'a self,
            req: &'a ResolveRequest,
        ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let use_good = req.skip_profiles.iter().any(|p| p == "native:BAD");
                Ok(ResolvedStream {
                    video_id: req.video_id.clone(),
                    url: if use_good {
                        self.good_url.clone()
                    } else {
                        self.bad_url.clone()
                    },
                    kbps: 128,
                    mime_type: r#"audio/webm; codecs="opus""#.to_owned(),
                    ext: "webm".to_owned(),
                    content_length: Some(2_000_000),
                    duration_ms: Some(180_000),
                    loudness_db: Some(-4.5),
                    is_live_hls: false,
                    client_name: if use_good { "VISIONOS" } else { "BAD" }.to_owned(),
                    profile_id: if use_good {
                        "native:VISIONOS"
                    } else {
                        "native:BAD"
                    }
                    .to_owned(),
                    media_headers: BTreeMap::new(),
                    range_chunk_bytes: Some(1024 * 1024),
                })
            })
        }
    }

    #[tokio::test]
    async fn probe_rejects_html_200_and_benches_profile_until_good_audio_past_1mib() {
        let base = spawn_media_server(|req| {
            // Verify probing starts at AUTH_BOUNDARY_BYTES (1 MiB) for clen=2_000_000.
            assert!(
                req.to_ascii_lowercase().contains("range: bytes=1048576-"),
                "expected probe past 1 MiB, got: {req}"
            );
            if req.starts_with("GET /bad") {
                (200, "text/html", b"<html>Consent</html>".to_vec())
            } else {
                (206, "audio/webm", vec![0xAA; PROBE_READ_BYTES])
            }
        })
        .await;

        let calls = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(SwitchingBackend {
            bad_url: format!("{base}/bad"),
            good_url: format!("{base}/good"),
            calls: Arc::clone(&calls),
        });
        let resolver = StreamResolver::new(
            Arc::new(build_shared_http_client()),
            backend,
            Arc::new(UploadedTracks::new()),
        );

        let stream = resolver
            .resolve(ResolveRequest::for_playback("dQw4w9WgXcQ", 256))
            .await
            .expect("should recover on second profile");
        assert_eq!(stream.profile_id, "native:VISIONOS");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // Cached hit should not invoke backend again.
        let cached = resolver
            .resolve(ResolveRequest::for_playback("dQw4w9WgXcQ", 256))
            .await
            .expect("cached");
        assert_eq!(cached.url, stream.url);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn single_flight_coalesces_concurrent_resolutions() {
        struct SlowBackend {
            calls: Arc<AtomicUsize>,
        }
        impl StreamBackend for SlowBackend {
            fn resolve_uncached<'a>(
                &'a self,
                req: &'a ResolveRequest,
            ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>> {
                Box::pin(async move {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    Ok(ResolvedStream {
                        video_id: req.video_id.clone(),
                        url: "https://example.test/a.webm".to_owned(),
                        kbps: 160,
                        mime_type: r#"audio/webm; codecs="opus""#.to_owned(),
                        ext: "webm".to_owned(),
                        content_length: None,
                        duration_ms: None,
                        loudness_db: None,
                        is_live_hls: false,
                        client_name: "VISIONOS".to_owned(),
                        profile_id: "native:VISIONOS".to_owned(),
                        media_headers: BTreeMap::new(),
                        range_chunk_bytes: None,
                    })
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Arc::new(
            StreamResolver::new(
                Arc::new(build_shared_http_client()),
                Arc::new(SlowBackend {
                    calls: Arc::clone(&calls),
                }),
                Arc::new(UploadedTracks::new()),
            )
            .with_probing(false),
        );

        let (r1, r2) = tokio::join!(
            resolver.resolve(ResolveRequest::for_playback("same_id_123", 256)),
            resolver.resolve(ResolveRequest::for_playback("same_id_123", 256)),
        );
        assert_eq!(r1.expect("r1").kbps, 160);
        assert_eq!(r2.expect("r2").kbps, 160);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn native_backend_selects_opus_extracts_loudness_and_detects_permanent_errors() {
        let base = spawn_media_server(|req| {
            let body = if req.contains("AGE_ID") {
                json!({
                    "playabilityStatus": {
                        "status": "LOGIN_REQUIRED",
                        "reason": "Sign in to confirm your age"
                    }
                })
            } else {
                json!({
                    "playabilityStatus": { "status": "OK" },
                    "playerConfig": { "audioConfig": { "loudnessDb": -5.25 } },
                    "streamingData": {
                        "adaptiveFormats": [
                            { "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "averageBitrate": 128000, "url": "https://rr1.googlevideo.com/videoplayback?c=VISIONOS&clen=4000000" },
                            { "mimeType": "audio/webm; codecs=\"opus\"", "averageBitrate": 160000, "contentLength": "4200000", "url": "https://rr1.googlevideo.com/videoplayback?c=VISIONOS" }
                        ]
                    }
                })
            };
            (200, "application/json", body.to_string().into_bytes())
        })
        .await;

        let innertube = Arc::new(InnertubeClient::with_endpoints(
            Arc::new(build_shared_http_client()),
            format!("{base}/youtubei/v1"),
            base.clone(),
            base.clone(),
        ));
        let backend = NativeStreamBackend::with_endpoint(innertube, format!("{base}/player"));

        let ok = backend
            .resolve_uncached(&ResolveRequest::for_playback("GOOD_ID_123", 256))
            .await
            .expect("should pick opus");
        assert_eq!(ok.ext, "webm");
        assert_eq!(ok.kbps, 160);
        assert_eq!(ok.loudness_db, Some(-5.25));
        assert_eq!(ok.content_length, Some(4_200_000));
        assert_eq!(ok.range_chunk_bytes, Some(1024 * 1024));

        // Direct test of permanent reason classifier.
        assert_eq!(
            classify_permanent_reason("Sign in to confirm your age", false),
            Some(PermanentReason::AgeRestrictedSignedOut)
        );
        assert_eq!(
            classify_permanent_reason(
                "This video is only available to Music Premium members",
                false
            ),
            Some(PermanentReason::PremiumRequired)
        );
    }

    #[cfg(feature = "ytdlp-bridge")]
    #[test]
    fn ytdlp_single_json_parses_into_resolved_stream() {
        let sample = br#"{
            "id": "dQw4w9WgXcQ",
            "url": "https://rr2.googlevideo.com/videoplayback?c=VISIONOS&clen=3500000",
            "format_id": "251",
            "ext": "webm",
            "acodec": "opus",
            "abr": 142.4,
            "filesize": 3500000,
            "http_headers": { "User-Agent": "TestUA" }
        }"#;
        let req = ResolveRequest::for_playback("dQw4w9WgXcQ", 256);
        let parsed = YtDlpStreamBackend::parse_single_json(sample, &req).expect("parsed");
        assert_eq!(parsed.video_id, "dQw4w9WgXcQ");
        assert_eq!(parsed.kbps, 142);
        assert_eq!(parsed.ext, "webm");
        assert_eq!(parsed.content_length, Some(3_500_000));
        assert_eq!(parsed.loudness_db, None);
        assert_eq!(parsed.range_chunk_bytes, Some(1024 * 1024));
        assert_eq!(
            parsed.media_headers.get("User-Agent").map(String::as_str),
            Some("TestUA")
        );
    }

    #[tokio::test]
    async fn native_backend_solves_signature_cipher_and_orders_fast_profiles_first() {
        let base = spawn_media_server(|req| {
            let body = if req.contains("CIPHER_ID") {
                json!({
                    "playabilityStatus": { "status": "OK" },
                    "streamingData": {
                        "adaptiveFormats": [{
                            "mimeType": "audio/webm; codecs=\"opus\"",
                            "averageBitrate": 160000,
                            "contentLength": "3200000",
                            "approxDurationMs": "185000",
                            "signatureCipher": "s=abcdef&sp=sig&url=https%3A%2F%2Frr1.googlevideo.com%2Fvideoplayback%3Fc%3DWEB_REMIX"
                        }]
                    }
                })
            } else {
                json!({
                    "playabilityStatus": { "status": "OK" },
                    "streamingData": { "adaptiveFormats": [] }
                })
            };
            (200, "application/json", body.to_string().into_bytes())
        })
        .await;

        let innertube = Arc::new(InnertubeClient::with_endpoints(
            Arc::new(build_shared_http_client()),
            format!("{base}/youtubei/v1"),
            base.clone(),
            base.clone(),
        ));
        innertube.set_cookie(Some("SAPISID=test; SID=test".to_owned()));

        let backend = NativeStreamBackend::with_endpoint(innertube, format!("{base}/player"))
            .with_sig_transform_override("(s) => s.split('').reverse().join('')");

        // Signed-in public track prefers fast VISIONOS/IOS before WEB_REMIX.
        let public_order: Vec<String> = backend
            .profiles_for(true, false)
            .into_iter()
            .map(|c| c.client_name)
            .collect();
        assert_eq!(public_order[0], "VISIONOS");
        assert_eq!(public_order[1], "IOS");

        // Uploaded track still puts WEB_REMIX first.
        let upload_order: Vec<String> = backend
            .profiles_for(true, true)
            .into_iter()
            .map(|c| c.client_name)
            .collect();
        assert_eq!(upload_order[0], "WEB_REMIX");

        let resolved = backend
            .resolve_uncached(&ResolveRequest::for_playback("CIPHER_ID", 256))
            .await
            .expect("cipher resolved via QuickJS sandbox");
        assert!(
            resolved.url.contains("sig=fedcba"),
            "expected decrypted sig in url, got: {}",
            resolved.url
        );
        assert_eq!(resolved.duration_ms, Some(185_000));
    }

    #[tokio::test]
    async fn optimistic_probe_skips_healthy_profile_until_refused() {
        let probe_hits = Arc::new(AtomicUsize::new(0));
        let hits_for_srv = Arc::clone(&probe_hits);
        let base = spawn_media_server(move |_req| {
            hits_for_srv.fetch_add(1, Ordering::SeqCst);
            (206, "audio/webm", vec![0xAA; PROBE_READ_BYTES])
        })
        .await;

        struct VisionBackend {
            url: String,
        }
        impl StreamBackend for VisionBackend {
            fn resolve_uncached<'a>(
                &'a self,
                req: &'a ResolveRequest,
            ) -> BoxFuture<'a, Result<ResolvedStream, StreamError>> {
                Box::pin(async move {
                    Ok(ResolvedStream {
                        video_id: req.video_id.clone(),
                        url: self.url.clone(),
                        kbps: 160,
                        mime_type: r#"audio/webm; codecs="opus""#.to_owned(),
                        ext: "webm".to_owned(),
                        content_length: Some(2_000_000),
                        duration_ms: Some(120_000),
                        loudness_db: None,
                        is_live_hls: false,
                        client_name: "VISIONOS".to_owned(),
                        profile_id: "native:VISIONOS".to_owned(),
                        media_headers: BTreeMap::new(),
                        range_chunk_bytes: Some(1024 * 1024),
                    })
                })
            }
        }

        let resolver = StreamResolver::new(
            Arc::new(build_shared_http_client()),
            Arc::new(VisionBackend {
                url: format!("{base}/stream"),
            }),
            Arc::new(UploadedTracks::new()),
        );

        let s1 = resolver
            .resolve(ResolveRequest::for_playback("vid_fast_1", 256))
            .await
            .expect("optimistic resolve");
        assert_eq!(s1.profile_id, "native:VISIONOS");
        assert_eq!(
            probe_hits.load(Ordering::SeqCst),
            0,
            "healthy VISIONOS profile should skip blocking probe under Optimistic policy"
        );

        // Once a refusal is reported for VISIONOS, subsequent tracks re-enable blocking probe.
        resolver.on_playback_refused("vid_other", "native:VISIONOS", &s1.url);
        let _ = resolver
            .resolve(ResolveRequest::for_playback("vid_fast_2", 256))
            .await
            .expect("probed resolve after degradation");
        assert_eq!(
            probe_hits.load(Ordering::SeqCst),
            1,
            "degraded VISIONOS profile must perform blocking probe"
        );
    }
}
