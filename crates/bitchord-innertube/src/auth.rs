use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use sha1::{Digest, Sha1};

/// Default origin used when signing YouTube Music API requests.
pub const MUSIC_ORIGIN: &str = "https://music.youtube.com";
/// Origin used when signing www.youtube.com endpoints (e.g. account switcher).
pub const YOUTUBE_ORIGIN: &str = "https://www.youtube.com";

/// Cookie names checked in exact priority order for `SAPISIDHASH` signing.
const SAPISID_NAMES: &[&str] = &["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"];

static CONFIG_LOGGED_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""LOGGED_IN"\s*:\s*(true|false)"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid CONFIG_LOGGED_IN regex")
});
static CONFIG_DATASYNC_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""DATASYNC_ID"\s*:\s*"([^"]+)""#)
        // INFALLIBLE: static regex pattern.
        .expect("valid CONFIG_DATASYNC_ID regex")
});
static CONFIG_PAGE_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""DELEGATED_SESSION_ID"\s*:\s*"([^"]+)""#)
        // INFALLIBLE: static regex pattern.
        .expect("valid CONFIG_PAGE_ID regex")
});
static CONFIG_SESSION_INDEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""SESSION_INDEX"\s*:\s*"?(\d+)"#)
        // INFALLIBLE: static regex pattern.
        .expect("valid CONFIG_SESSION_INDEX regex")
});
static CONFIG_VISITOR_DATA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""VISITOR_DATA"\s*:\s*"([^"]+)""#)
        // INFALLIBLE: static regex pattern.
        .expect("valid CONFIG_VISITOR_DATA regex")
});
static CONFIG_CLIENT_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""INNERTUBE_CLIENT_VERSION"\s*:\s*"([^"]+)""#)
        // INFALLIBLE: static regex pattern.
        .expect("valid CONFIG_CLIENT_VERSION regex")
});

/// Active session identity extracted from the `music.youtube.com` shell `ytcfg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScope {
    pub data_sync_id: Option<String>,
    pub page_id: Option<String>,
    pub auth_user: String,
    pub client_version: Option<String>,
    pub visitor_data: Option<String>,
    pub logged_in: bool,
}

/// Explicit channel selected by the listener, overriding the shell's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSelection {
    pub page_id: Option<String>,
    pub data_sync_id: Option<String>,
    pub auth_user: Option<String>,
}

/// Normalizes YouTube's `DATASYNC_ID` (`"account||delegated"`) into the value
/// accepted by InnerTube as `context.user.onBehalfOfUser`.
pub fn normalize_data_sync_id(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    if value.is_empty() {
        return None;
    }
    let Some((before, after)) = value.split_once("||") else {
        return Some(value.to_owned());
    };
    if !after.trim().is_empty() {
        Some(after.trim().to_owned())
    } else if !before.trim().is_empty() {
        Some(before.trim().to_owned())
    } else {
        None
    }
}

/// Extracts the API-signing secret from a `Cookie` header by exact cookie name
/// (never substring matching) in priority order:
/// `SAPISID` > `__Secure-3PAPISID` > `__Secure-1PAPISID`.
pub fn sapisid_from_cookie(cookie_header: &str) -> Option<String> {
    let mut jar: HashMap<&str, &str> = HashMap::new();
    for entry in cookie_header.split(';') {
        let Some((raw_name, raw_val)) = entry.split_once('=') else {
            continue;
        };
        let name = raw_name.trim();
        let value = raw_val.trim();
        if !name.is_empty() && !value.is_empty() {
            jar.insert(name, value);
        }
    }
    SAPISID_NAMES
        .iter()
        .find_map(|name| jar.get(name).map(|v| (*v).to_owned()))
}

/// Returns `true` iff `cookie_header` carries a usable SAPISID signing cookie.
pub fn has_auth_cookie(cookie_header: &str) -> bool {
    sapisid_from_cookie(cookie_header).is_some()
}

/// Computes `SAPISIDHASH <unix_sec>_<sha1_hex>` for the given `sapisid` and
/// `origin` using the current wall clock.
pub fn sapisid_hash(sapisid: &str, origin: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    sapisid_hash_at(sapisid, origin, ts)
}

/// Deterministic `SAPISIDHASH` at a specific UNIX timestamp in seconds.
pub fn sapisid_hash_at(sapisid: &str, origin: &str, unix_secs: u64) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("{unix_secs} {sapisid} {origin}").as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(40);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("SAPISIDHASH {unix_secs}_{hex}")
}

/// Parses `ytcfg` identity fields from the `music.youtube.com/` HTML shell.
pub fn parse_session_scope_html(html: &str) -> Option<SessionScope> {
    let logged_in = CONFIG_LOGGED_IN
        .captures(html)
        .and_then(|c| c.get(1))
        .is_some_and(|m| m.as_str() == "true");
    let client_version = CONFIG_CLIENT_VERSION
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned());

    if !logged_in {
        return client_version.map(|ver| SessionScope {
            data_sync_id: None,
            page_id: None,
            auth_user: "0".to_owned(),
            client_version: Some(ver),
            visitor_data: None,
            logged_in: false,
        });
    }

    let page_id = CONFIG_PAGE_ID
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned);
    let raw_datasync = CONFIG_DATASYNC_ID
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str());
    let data_sync_id = page_id
        .clone()
        .or_else(|| normalize_data_sync_id(raw_datasync));
    let auth_user = CONFIG_SESSION_INDEX
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("0")
        .to_owned();
    let visitor_data = CONFIG_VISITOR_DATA
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned);

    Some(SessionScope {
        data_sync_id,
        page_id,
        auth_user,
        client_version,
        visitor_data,
        logged_in: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_data_sync_id_handles_delegated_plain_and_blank() {
        assert_eq!(
            normalize_data_sync_id(Some("account||delegated")).as_deref(),
            Some("delegated")
        );
        assert_eq!(
            normalize_data_sync_id(Some("account||")).as_deref(),
            Some("account")
        );
        assert_eq!(
            normalize_data_sync_id(Some("plain123")).as_deref(),
            Some("plain123")
        );
        assert!(normalize_data_sync_id(Some("   ")).is_none());
        assert!(normalize_data_sync_id(None).is_none());
    }

    #[test]
    fn sapisid_extraction_respects_exact_names_and_priority() {
        assert_eq!(
            sapisid_from_cookie("__Secure-1PAPISID=one; __Secure-3PAPISID=three; SAPISID=plain")
                .as_deref(),
            Some("plain")
        );
        assert_eq!(
            sapisid_from_cookie("__Secure-1PAPISID=one; __Secure-3PAPISID=three").as_deref(),
            Some("three")
        );
        assert_eq!(
            sapisid_from_cookie("SID=abc; __Secure-1PAPISID=one").as_deref(),
            Some("one")
        );
        // Must NOT match substring-similar fake cookie names or empty values.
        assert!(sapisid_from_cookie("MY_SAPISID=fake; SAPISID=").is_none());
        assert!(!has_auth_cookie("SID=abc; HSID=def"));
    }

    #[test]
    fn sapisid_hash_matches_sha1_rfc_format() {
        let header = sapisid_hash_at("secret123", MUSIC_ORIGIN, 1_700_000_000);
        assert!(header.starts_with("SAPISIDHASH 1700000000_"));
        let hex = header
            .strip_prefix("SAPISIDHASH 1700000000_")
            .expect("prefix");
        assert_eq!(hex.len(), 40);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // Origin changes the hash.
        assert_ne!(
            header,
            sapisid_hash_at("secret123", YOUTUBE_ORIGIN, 1_700_000_000)
        );
    }

    #[test]
    fn parse_session_scope_ignores_datasync_when_signed_out() {
        let signed_out = r#"{"LOGGED_IN":false,"DATASYNC_ID":"stale||id","INNERTUBE_CLIENT_VERSION":"1.20260707.12.00"}"#;
        let scope = parse_session_scope_html(signed_out).expect("version present");
        assert!(!scope.logged_in);
        assert!(scope.data_sync_id.is_none());
        assert_eq!(scope.client_version.as_deref(), Some("1.20260707.12.00"));

        let signed_in = r#"{"LOGGED_IN":true,"DATASYNC_ID":"acct||brand","DELEGATED_SESSION_ID":"102938","SESSION_INDEX":"2","VISITOR_DATA":"Cgtest","INNERTUBE_CLIENT_VERSION":"1.20260707.12.00"}"#;
        let scope = parse_session_scope_html(signed_in).expect("signed in");
        assert!(scope.logged_in);
        assert_eq!(scope.page_id.as_deref(), Some("102938"));
        assert_eq!(scope.data_sync_id.as_deref(), Some("102938"));
        assert_eq!(scope.auth_user, "2");
        assert_eq!(scope.visitor_data.as_deref(), Some("Cgtest"));
    }
}
