#![forbid(unsafe_code)]

//! BitChord Linux Desktop (`gpui-ce 0.3.x`, `wgpu`, Wayland/X11, PipeWire,
//! MPRIS2, System Tray with `QuitMode::Explicit`).

mod actions;
mod app;
mod artwork;
mod backend;
mod icons;
mod pill_slider;
mod text_input;
mod theme;
mod views;

use gpui::{
    App, AppContext, Bounds, Global, MenuItem, QuitMode, Size, TitlebarOptions, WindowBounds,
    WindowHandle, WindowOptions, px, size,
};
use gpui_tray::Tray;

use crate::actions::{
    NextTrack, PlayPause, PrevTrack, QuitApp, ShowWindow, build_tray_icon, default_keybindings,
};
use crate::app::BitChordApp;
use crate::backend::{AppBackend, BackendCommand};
use crate::theme::{ActiveTheme, ThemeMode};

struct ActiveWindowHandle(Option<WindowHandle<BitChordApp>>);
impl Global for ActiveWindowHandle {}

struct AppTray(Tray);
impl Global for AppTray {}

fn main() {
    gpui_ce_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx: &mut App| {
            // Bounded 2-thread Tokio runtime per ADR-009 (GPUI already uses background workers).
            gpui_ce_tokio::init(cx);
            let rt_handle = gpui_ce_tokio::Tokio::handle(cx);

            let backend = AppBackend::start(rt_handle);
            let initial_theme = match backend.initial_settings.theme_mode {
                bitchord_storage::ThemeMode::Light => ThemeMode::Light,
                _ => ThemeMode::Dark,
            };
            cx.set_global(ActiveTheme(initial_theme));
            cx.set_global(backend);
            cx.set_global(ActiveWindowHandle(None));

            cx.bind_keys(default_keybindings());

            // Global action handlers reachable from both the window AND the Linux system tray.
            cx.on_action(|_: &ShowWindow, cx| {
                open_or_focus_main_window(cx);
            });
            cx.on_action(|_: &PlayPause, cx| {
                cx.global::<AppBackend>().send(BackendCommand::PlayPause);
            });
            cx.on_action(|_: &NextTrack, cx| {
                cx.global::<AppBackend>().send(BackendCommand::NextTrack);
            });
            cx.on_action(|_: &PrevTrack, cx| {
                cx.global::<AppBackend>().send(BackendCommand::PrevTrack);
            });
            cx.on_action(|_: &QuitApp, cx| {
                cx.global::<AppBackend>().send(BackendCommand::Shutdown);
                if let Some(tray) = cx.try_global::<AppTray>().map(|t| t.0.clone()) {
                    let _ = tray.close(cx);
                }
                cx.quit();
            });

            // Linux StatusNotifierItem system tray (pure Rust zbus, zero GTK3).
            if let Ok(icon) = build_tray_icon()
                && let Ok(tray) = Tray::builder()
                    .icon(icon)
                    .title("Myusic")
                    .tooltip("Myusic")
                    .menu(|_cx| {
                        vec![
                            MenuItem::action("Show Myusic", ShowWindow),
                            MenuItem::separator(),
                            MenuItem::action("Play / Pause", PlayPause),
                            MenuItem::action("Next Track", NextTrack),
                            MenuItem::action("Previous Track", PrevTrack),
                            MenuItem::separator(),
                            MenuItem::action("Quit Myusic", QuitApp),
                        ]
                    })
                    .build(cx)
            {
                cx.set_global(AppTray(tray));
            }

            open_or_focus_main_window(cx);
            cx.activate(true);
        });
}

fn open_or_focus_main_window(cx: &mut App) {
    if let Some(existing) = cx.global::<ActiveWindowHandle>().0 {
        let activated = existing.update(cx, |_view, window, cx| {
            window.activate();
            cx.notify();
        });
        if activated.is_ok() {
            return;
        }
    }

    let bounds = Bounds::centered(None, size(px(1200.0), px(780.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(Size {
            width: px(860.0),
            height: px(560.0),
        }),
        titlebar: Some(TitlebarOptions {
            title: Some("Myusic".into()),
            ..TitlebarOptions::default()
        }),
        app_id: Some("com.music.myusic".to_owned()),
        ..WindowOptions::default()
    };

    if let Ok(handle) = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| BitChordApp::new(window, cx));
        window.on_window_should_close(cx, {
            let view = view.clone();
            move |_window, cx| {
                let close_to_tray = view.read(cx).close_to_tray;
                if close_to_tray {
                    cx.global_mut::<ActiveWindowHandle>().0 = None;
                    // Allow window to close while `QuitMode::Explicit` keeps playback alive in tray!
                    true
                } else {
                    cx.global::<AppBackend>().send(BackendCommand::Shutdown);
                    cx.quit();
                    true
                }
            }
        });
        view
    }) {
        cx.global_mut::<ActiveWindowHandle>().0 = Some(handle);
    }
}
