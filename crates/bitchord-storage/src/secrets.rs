//! Secret storage backed by Linux Secret Service (`oo7`) with an in-memory
//! implementation for headless environments and unit tests.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

use thiserror::Error;

const APP_ATTRIBUTE: &str = "bitchord";

/// Named credential slots persisted outside plaintext `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKey {
    YoutubeCookie,
    YoutubeDataSyncId,
    PaxSenixKey,
    LastFmSessionKey,
    ListenBrainzToken,
    SpotifyBearerToken,
    SpotifyClientToken,
    ListenTogetherToken,
    WebDavPassword,
    SmbPassword,
}

impl SecretKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::YoutubeCookie => "youtube_cookie",
            Self::YoutubeDataSyncId => "youtube_data_sync_id",
            Self::PaxSenixKey => "paxsenix_key",
            Self::LastFmSessionKey => "lastfm_session_key",
            Self::ListenBrainzToken => "listenbrainz_token",
            Self::SpotifyBearerToken => "spotify_bearer_token",
            Self::SpotifyClientToken => "spotify_client_token",
            Self::ListenTogetherToken => "listen_together_token",
            Self::WebDavPassword => "webdav_password",
            Self::SmbPassword => "smb_password",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::YoutubeCookie => "Myusic YouTube Music Cookie",
            Self::YoutubeDataSyncId => "Myusic YouTube Data Sync ID",
            Self::PaxSenixKey => "Myusic PaxSenix API Key",
            Self::LastFmSessionKey => "Myusic Last.fm Session Key",
            Self::ListenBrainzToken => "Myusic ListenBrainz Token",
            Self::SpotifyBearerToken => "Myusic Spotify Bearer Token",
            Self::SpotifyClientToken => "Myusic Spotify Client Token",
            Self::ListenTogetherToken => "Myusic Listen Together Token",
            Self::WebDavPassword => "Myusic WebDAV Password",
            Self::SmbPassword => "Myusic SMB Password",
        }
    }
}

/// Wrapper around a secret string that never leaks its contents via `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("Linux Secret Service error: {0}")]
    Keyring(String),
    #[error("stored secret was not valid UTF-8")]
    InvalidUtf8,
}

/// Boxed future alias so [`SecretStore`] is object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Asynchronous credential store abstraction.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: SecretKey) -> BoxFuture<'_, Result<Option<SecretValue>, SecretError>>;
    fn set(&self, key: SecretKey, value: &str) -> BoxFuture<'_, Result<(), SecretError>>;
    fn delete(&self, key: SecretKey) -> BoxFuture<'_, Result<(), SecretError>>;
}

/// Thread-safe in-memory [`SecretStore`] for tests and headless fallback.
#[derive(Default)]
pub struct InMemorySecretStore {
    map: RwLock<HashMap<SecretKey, SecretValue>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: SecretKey) -> BoxFuture<'_, Result<Option<SecretValue>, SecretError>> {
        let result = self
            .map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        Box::pin(async move { Ok(result) })
    }

    fn set(&self, key: SecretKey, value: &str) -> BoxFuture<'_, Result<(), SecretError>> {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, SecretValue::new(value));
        Box::pin(async move { Ok(()) })
    }

    fn delete(&self, key: SecretKey) -> BoxFuture<'_, Result<(), SecretError>> {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        Box::pin(async move { Ok(()) })
    }
}

/// Linux Secret Service / XDG Portal credential store powered by `oo7`.
pub struct KeyringSecretStore {
    keyring: oo7::Keyring,
}

impl KeyringSecretStore {
    /// Connects to the user's Linux Secret Service (or XDG Secret portal when sandboxed).
    pub async fn open() -> Result<Self, SecretError> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        Ok(Self { keyring })
    }

    fn attributes(key: SecretKey) -> [(&'static str, &'static str); 2] {
        [("app", APP_ATTRIBUTE), ("key", key.as_str())]
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: SecretKey) -> BoxFuture<'_, Result<Option<SecretValue>, SecretError>> {
        Box::pin(async move {
            let items = self
                .keyring
                .search_items(&Self::attributes(key))
                .await
                .map_err(|e| SecretError::Keyring(e.to_string()))?;
            let Some(item) = items.into_iter().next() else {
                return Ok(None);
            };
            let secret = item
                .secret()
                .await
                .map_err(|e| SecretError::Keyring(e.to_string()))?;
            let text = std::str::from_utf8(secret.as_bytes())
                .map_err(|_| SecretError::InvalidUtf8)?
                .to_owned();
            Ok(Some(SecretValue::new(text)))
        })
    }

    fn set(&self, key: SecretKey, value: &str) -> BoxFuture<'_, Result<(), SecretError>> {
        let value = value.to_owned();
        Box::pin(async move {
            self.keyring
                .create_item(
                    key.label(),
                    &Self::attributes(key),
                    oo7::Secret::text(value),
                    true,
                )
                .await
                .map_err(|e| SecretError::Keyring(e.to_string()))?;
            Ok(())
        })
    }

    fn delete(&self, key: SecretKey) -> BoxFuture<'_, Result<(), SecretError>> {
        Box::pin(async move {
            self.keyring
                .delete(&Self::attributes(key))
                .await
                .map_err(|e| SecretError::Keyring(e.to_string()))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_secret_store_round_trips_and_redacts_debug() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get(SecretKey::YoutubeCookie).await.unwrap(), None);

        store
            .set(SecretKey::YoutubeCookie, "SAPISID=super-secret")
            .await
            .unwrap();
        let val = store
            .get(SecretKey::YoutubeCookie)
            .await
            .unwrap()
            .expect("present");
        assert_eq!(val.expose(), "SAPISID=super-secret");
        assert_eq!(format!("{val:?}"), "SecretValue(<redacted>)");

        store.delete(SecretKey::YoutubeCookie).await.unwrap();
        assert_eq!(store.get(SecretKey::YoutubeCookie).await.unwrap(), None);
    }
}
