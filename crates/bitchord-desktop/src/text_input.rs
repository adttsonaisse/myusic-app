//! Keyboard-accessible single-line text input entity for GPUI-CE.
//!
//! Uses `.key_context("TextInput")` so global single-key player shortcuts
//! (`space`, `m`, `q`, `l`) never hijack typing, handles Wayland/X11 `key_char`
//! commits, Ctrl+V paste, Ctrl+U clear, Backspace, Escape blur, Enter submit,
//! and live `Changed(String)` emissions for debounced search suggestions.

use gpui::prelude::*;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, Render,
    SharedString, Window, div, px,
};

use crate::theme::{ActiveTheme, BitChordTheme, ThemeMode};

/// Events emitted by [`TextInput`] to its parent view.
#[derive(Debug, Clone)]
pub enum TextInputEvent {
    Changed(String),
    Submitted(String),
}

/// Reusable single-line text input supporting masked secrets and clear focus indicators.
pub struct TextInput {
    focus_handle: FocusHandle,
    value: String,
    placeholder: SharedString,
    masked: bool,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            value: String::new(),
            placeholder: placeholder.into(),
            masked: false,
        }
    }

    pub fn with_masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    pub fn with_initial(mut self, initial: impl Into<String>) -> Self {
        self.value = initial.into();
        self
    }

    pub fn text(&self) -> &str {
        &self.value
    }

    pub fn set_text(&mut self, next: impl Into<String>, cx: &mut Context<Self>) {
        self.value = next.into();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.key == "enter" {
            cx.emit(TextInputEvent::Submitted(self.value.clone()));
            cx.stop_propagation();
            return;
        }
        if ks.key == "escape" {
            window.blur(cx);
            cx.stop_propagation();
            return;
        }
        if ks.key == "backspace" {
            if self.value.pop().is_some() {
                cx.emit(TextInputEvent::Changed(self.value.clone()));
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if ks.modifiers.control {
            match ks.key.as_str() {
                "u" => {
                    if !self.value.is_empty() {
                        self.value.clear();
                        cx.emit(TextInputEvent::Changed(self.value.clone()));
                        cx.notify();
                    }
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    if let Some(item) = cx.read_from_clipboard()
                        && let Some(text) = item.text()
                    {
                        let single_line = text.replace(['\r', '\n'], " ");
                        self.value.push_str(&single_line);
                        cx.emit(TextInputEvent::Changed(self.value.clone()));
                        cx.notify();
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            }
        }
        if ks.modifiers.alt || ks.modifiers.platform {
            return;
        }
        if let Some(ref ch) = ks.key_char
            && !ch.is_empty()
            && !ch.chars().all(char::is_control)
        {
            self.value.push_str(ch);
            cx.emit(TextInputEvent::Changed(self.value.clone()));
            cx.notify();
            cx.stop_propagation();
        }
    }

    /// Renders the input styled against the active [`BitChordTheme`].
    pub fn render_themed(
        &self,
        theme: BitChordTheme,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let is_empty = self.value.is_empty();
        let display: SharedString = if is_empty {
            self.placeholder.clone()
        } else if self.masked {
            "*".repeat(self.value.chars().count().min(32)).into()
        } else {
            self.value.clone().into()
        };

        let border_col = if focused { theme.accent } else { theme.border };
        let text_col = if is_empty {
            theme.text_muted
        } else {
            theme.text_primary
        };

        div()
            .id(("text-input", cx.entity_id()))
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            )
            .w_full()
            .h(px(38.0))
            .px(px(13.0))
            .flex()
            .items_center()
            .rounded(px(10.0))
            .bg(theme.surface_elevated)
            .border_1()
            .border_color(border_col)
            .cursor_text()
            .overflow_hidden()
            .text_size(px(13.5))
            .text_color(text_col)
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(display)
                    .when(focused, |el| {
                        el.child(div().ml(px(1.5)).w(px(1.5)).h(px(15.0)).bg(theme.accent))
                    }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = cx
            .try_global::<ActiveTheme>()
            .map_or(ThemeMode::Dark, |g| g.0);
        self.render_themed(BitChordTheme::for_mode(mode), window, cx)
    }
}
