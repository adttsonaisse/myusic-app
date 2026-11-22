use thiserror::Error;

/// Errors returned by [`crate::InnertubeClient`] and [`crate::SongInfoParser`].
#[derive(Debug, Error)]
pub enum InnertubeError {
    #[error("Sign in to YouTube Music to do that")]
    NotSignedIn,
    #[error("InnerTube HTTP {status}: {detail}")]
    HttpStatus { status: u16, detail: String },
    #[error("InnerTube transport error: {0}")]
    Transport(String),
    #[error("YouTube Music refused the request: {0}")]
    ApiRefused(String),
    #[error("invalid JSON response: {0}")]
    Json(String),
    #[error("{0}")]
    InvalidState(String),
}

impl InnertubeError {
    pub fn is_auth_rejection(&self) -> bool {
        matches!(
            self,
            Self::HttpStatus {
                status: 401 | 403,
                ..
            }
        )
    }
}

/// Terminal, non-retryable reasons why YouTube refuses to serve a track.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PermanentReason {
    #[error("This track is age-restricted. Sign in to YouTube to play it.")]
    AgeRestrictedSignedOut,
    #[error("YouTube will not serve this age-restricted track to this account.")]
    AgeRestrictedSignedIn,
    #[error("This track isn't available in your country")]
    GeoRestricted,
    #[error("This track is paid content")]
    PaidContent,
    #[error("This track needs YouTube Music Premium")]
    PremiumRequired,
    #[error("This track is private")]
    Private,
    #[error("The channel behind this track was terminated")]
    AccountTerminated,
    #[error("This track was removed from YouTube")]
    Removed,
}

/// Errors produced by [`crate::StreamResolver`] and [`crate::StreamBackend`].
#[derive(Debug, Clone, Error)]
pub enum StreamError {
    #[error("{0}")]
    Permanent(#[from] PermanentReason),
    #[error("stream refused by upstream ({status}): {detail}")]
    Refused {
        status: u16,
        profile_id: String,
        detail: String,
    },
    #[error("temporary extraction failure: {0}")]
    Transient(String),
    #[error("rate limited / anti-bot challenge")]
    RateLimited,
}
