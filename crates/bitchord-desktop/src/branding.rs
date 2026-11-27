//! Centralized user-facing branding constants for the desktop shell, tray menu,
//! window metadata, Discord presence, and Listen Together defaults (`notes.md` §2 #2).
//!
//! Keep protocol/storage identifiers (`~/.config/bitchord`, `org.mpris.MediaPlayer2.bitchord`)
//! decoupled from these display constants so existing user libraries and D-Bus bindings
//! continue working seamlessly.

/// Primary display name shown in the window titlebar, sidebar, tray, and presence.
pub const APP_NAME: &str = "Myusic";

/// Wayland `app_id` / freedesktop desktop entry stem (`com.music.myusic.desktop`).
pub const APP_ID: &str = "com.music.myusic";

/// System tray icon title & tooltip.
pub const TRAY_TITLE: &str = APP_NAME;
pub const TRAY_TOOLTIP: &str = APP_NAME;

/// System tray menu labels.
pub const TRAY_SHOW_LABEL: &str = "Show Myusic";
pub const TRAY_QUIT_LABEL: &str = "Quit Myusic";

/// Default display nickname for Listen Together sessions when the user leaves it blank.
pub const DEFAULT_PARTY_NICKNAME: &str = "Myusic Linux";

/// Display subtitle/asset label used for Discord Rich Presence.
pub const DISCORD_APP_LABEL: &str = APP_NAME;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branding_constants_use_myusic_identity() {
        assert_eq!(APP_NAME, "Myusic");
        assert_eq!(APP_ID, "com.music.myusic");
        assert!(TRAY_SHOW_LABEL.contains(APP_NAME));
        assert!(TRAY_QUIT_LABEL.contains(APP_NAME));
        assert!(DEFAULT_PARTY_NICKNAME.contains(APP_NAME));
    }
}
