//! GPUI action declarations, Linux keyboard bindings, top-level navigation routes,
//! and pure-Rust RGBA tray icon generation.

use bitchord_model::BrowseType;
use gpui::{KeyBinding, actions};
use gpui_tray::Icon;

actions!(
    bitchord,
    [
        PlayPause,
        NextTrack,
        PrevTrack,
        SeekForward,
        SeekBack,
        VolumeUp,
        VolumeDown,
        ToggleMute,
        ToggleNowPlaying,
        ToggleQueue,
        ToggleLyrics,
        ToggleTheme,
        FocusSearch,
        CloseWindowAction,
        ShowWindow,
        QuitApp,
    ]
);

/// Active top-level destination inside the BitChord desktop shell.
/// Every route corresponds to a real, interactive screen (`R-24`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    #[default]
    Home,
    Search,
    Detail {
        browse_id: String,
        title: String,
        subtitle: String,
        kind: BrowseType,
    },
    Library,
    Downloads,
    ListenTogether,
    Settings,
    NowPlaying,
}

impl Route {
    pub fn title(&self) -> &str {
        match self {
            Self::Home => "Home",
            Self::Search => "Search",
            Self::Detail { title, .. } => title.as_str(),
            Self::Library => "Library",
            Self::Downloads => "Downloads",
            Self::ListenTogether => "Listen Together",
            Self::Settings => "Settings",
            Self::NowPlaying => "Now Playing",
        }
    }
}

/// Linux-native keyboard bindings scoped so typing inside `TextInput` is never hijacked.
pub fn default_keybindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("space", PlayPause, Some("Player && !TextInput")),
        KeyBinding::new("n", NextTrack, Some("Player && !TextInput")),
        KeyBinding::new("p", PrevTrack, Some("Player && !TextInput")),
        KeyBinding::new("m", ToggleMute, Some("Player && !TextInput")),
        KeyBinding::new("q", ToggleQueue, Some("Player && !TextInput")),
        KeyBinding::new("l", ToggleLyrics, Some("Player && !TextInput")),
        KeyBinding::new("ctrl-right", SeekForward, Some("Player")),
        KeyBinding::new("ctrl-left", SeekBack, Some("Player")),
        KeyBinding::new("ctrl-up", VolumeUp, Some("Player")),
        KeyBinding::new("ctrl-down", VolumeDown, Some("Player")),
        KeyBinding::new("ctrl-f", FocusSearch, None),
        KeyBinding::new("ctrl-t", ToggleTheme, None),
        KeyBinding::new("escape", CloseWindowAction, Some("Player")),
        KeyBinding::new("ctrl-q", QuitApp, None),
    ]
}

/// Builds a crisp 32x32 RGBA system tray icon (BitChord crimson mark on rounded obsidian badge)
/// without depending on external PNG assets at runtime.
pub fn build_tray_icon() -> gpui_tray::Result<Icon> {
    const SIZE: u32 = 32;
    let mut rgba = vec![0_u8; (SIZE * SIZE * 4) as usize];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * SIZE + x) * 4) as usize;

            if dist <= 14.5 {
                // Dark obsidian disc background
                let mut col = [14u8, 14, 18, 255];
                // Crimson ring accent
                if dist >= 12.2 {
                    col = [250, 45, 72, 255];
                } else {
                    // Geometric play triangle inside the disc
                    let px = x as f32 - 12.0;
                    let py = (y as f32 - 15.5).abs();
                    if (0.0..=10.0).contains(&px) && py <= (10.0 - px) * 0.62 {
                        col = [250, 45, 72, 255];
                    }
                }
                rgba[idx..idx + 4].copy_from_slice(&col);
            }
        }
    }

    Icon::from_rgba(rgba, SIZE, SIZE).build()
}
