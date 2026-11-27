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
        NavigateBack,
        NavigateForward,
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

/// Bounded browser-style Back/Forward navigation history stack for top-level [`Route`]s.
#[derive(Debug, Clone, Default)]
pub struct NavigationHistory {
    back_stack: Vec<Route>,
    forward_stack: Vec<Route>,
}

impl NavigationHistory {
    pub const MAX_ENTRIES: usize = 64;

    pub fn new() -> Self {
        Self::default()
    }

    /// Records navigating from `current` to `next`.
    /// Returns `false` (no-op) if `current == *next`.
    pub fn push(&mut self, current: Route, next: &Route) -> bool {
        if &current == next {
            return false;
        }
        self.back_stack.push(current);
        if self.back_stack.len() > Self::MAX_ENTRIES {
            let excess = self.back_stack.len() - Self::MAX_ENTRIES;
            self.back_stack.drain(0..excess);
        }
        self.forward_stack.clear();
        true
    }

    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    /// Pops the previous route and pushes `current` onto the forward stack.
    pub fn go_back(&mut self, current: Route) -> Option<Route> {
        let prev = self.back_stack.pop()?;
        self.forward_stack.push(current);
        if self.forward_stack.len() > Self::MAX_ENTRIES {
            let excess = self.forward_stack.len() - Self::MAX_ENTRIES;
            self.forward_stack.drain(0..excess);
        }
        Some(prev)
    }

    /// Pops the next forward route and pushes `current` onto the back stack.
    pub fn go_forward(&mut self, current: Route) -> Option<Route> {
        let next = self.forward_stack.pop()?;
        self.back_stack.push(current);
        if self.back_stack.len() > Self::MAX_ENTRIES {
            let excess = self.back_stack.len() - Self::MAX_ENTRIES;
            self.back_stack.drain(0..excess);
        }
        Some(next)
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.back_stack.clear();
        self.forward_stack.clear();
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
        KeyBinding::new("alt-left", NavigateBack, None),
        KeyBinding::new("alt-right", NavigateForward, None),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_history_tracks_back_forward_clears_forward_on_branch_and_caps_size() {
        let mut hist = NavigationHistory::new();
        assert!(!hist.can_go_back());
        assert!(!hist.can_go_forward());

        // Same-route push is a no-op.
        assert!(!hist.push(Route::Home, &Route::Home));
        assert!(!hist.can_go_back());

        // Home -> Search -> Library -> Detail
        let detail = Route::Detail {
            browse_id: "MPREb_123".to_owned(),
            title: "OutRun".to_owned(),
            subtitle: "Album".to_owned(),
            kind: BrowseType::Album,
        };
        assert!(hist.push(Route::Home, &Route::Search));
        assert!(hist.push(Route::Search, &Route::Library));
        assert!(hist.push(Route::Library, &detail));
        assert!(hist.can_go_back());
        assert!(!hist.can_go_forward());

        // Go back twice: Detail -> Library -> Search
        assert_eq!(hist.go_back(detail.clone()), Some(Route::Library));
        assert!(hist.can_go_forward());
        assert_eq!(hist.go_back(Route::Library), Some(Route::Search));

        // Go forward once: Search -> Library
        assert_eq!(hist.go_forward(Route::Search), Some(Route::Library));

        // Navigating to a new destination clears the forward stack.
        assert!(hist.push(Route::Library, &Route::Settings));
        assert!(!hist.can_go_forward());
        assert_eq!(hist.go_back(Route::Settings), Some(Route::Library));

        // Verify bounded capacity.
        for i in 0..(NavigationHistory::MAX_ENTRIES + 20) {
            let next = Route::Detail {
                browse_id: format!("id_{i}"),
                title: format!("Item {i}"),
                subtitle: String::new(),
                kind: BrowseType::Playlist,
            };
            hist.push(Route::Home, &next);
        }
        let mut count = 0;
        let mut cur = Route::Settings;
        while let Some(prev) = hist.go_back(cur) {
            cur = prev;
            count += 1;
        }
        assert_eq!(count, NavigationHistory::MAX_ENTRIES);
    }
}
