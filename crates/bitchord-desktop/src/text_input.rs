//! Keyboard-accessible single-line text input entity for GPUI-CE.
//!
//! Uses `.key_context("TextInput")` so global single-key player shortcuts
//! (`space`, `m`, `q`, `l`) never hijack typing, handles Wayland/X11 `key_char`
//! commits, cursor navigation (Left/Right/Home/End), Ctrl+A select-all,
//! Ctrl+C copy, Ctrl+V paste, Ctrl+U clear, Backspace/Delete, Escape blur,
//! Enter submit, optional leading icon, and ArrowUp/ArrowDown events for
//! keyboard navigation inside floating search suggestion dropdowns.

use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    Render, SharedString, Window, div, px,
};

use crate::icons;
use crate::theme::{ActiveTheme, BitChordTheme, ThemeMode};

/// Events emitted by [`TextInput`] to its parent view.
#[derive(Debug, Clone)]
pub enum TextInputEvent {
    Changed(String),
    Submitted(String),
    ArrowDown,
    ArrowUp,
    Escape,
    Focused,
}

/// Reusable single-line text input supporting masked secrets, cursor editing,
/// select-all, leading icon, and dropdown keyboard events.
pub struct TextInput {
    focus_handle: FocusHandle,
    value: String,
    cursor_char: usize,
    all_selected: bool,
    placeholder: SharedString,
    masked: bool,
    leading_icon: Option<&'static [u8]>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            value: String::new(),
            cursor_char: 0,
            all_selected: false,
            placeholder: placeholder.into(),
            masked: false,
            leading_icon: None,
        }
    }

    pub fn with_masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    pub fn with_leading_icon(mut self, icon_bytes: &'static [u8]) -> Self {
        self.leading_icon = Some(icon_bytes);
        self
    }

    pub fn with_initial(mut self, initial: impl Into<String>) -> Self {
        let s = initial.into();
        self.cursor_char = s.chars().count();
        self.value = s;
        self.all_selected = false;
        self
    }

    pub fn text(&self) -> &str {
        &self.value
    }

    pub fn set_text(&mut self, next: impl Into<String>, cx: &mut Context<Self>) {
        let s = next.into();
        self.cursor_char = s.chars().count();
        self.value = s;
        self.all_selected = false;
        cx.notify();
    }

    fn char_to_byte(text: &str, char_idx: usize) -> usize {
        text.char_indices()
            .nth(char_idx)
            .map_or(text.len(), |(b, _)| b)
    }

    fn insert_text(&mut self, snippet: &str) -> bool {
        if snippet.is_empty() {
            return false;
        }
        if self.all_selected {
            self.value.clear();
            self.cursor_char = 0;
            self.all_selected = false;
        }
        let byte_idx = Self::char_to_byte(&self.value, self.cursor_char);
        self.value.insert_str(byte_idx, snippet);
        self.cursor_char += snippet.chars().count();
        true
    }

    fn backspace_once(&mut self) -> bool {
        if self.all_selected {
            self.all_selected = false;
            if self.value.is_empty() {
                return false;
            }
            self.value.clear();
            self.cursor_char = 0;
            return true;
        }
        if self.cursor_char == 0 {
            return false;
        }
        let start = Self::char_to_byte(&self.value, self.cursor_char - 1);
        let end = Self::char_to_byte(&self.value, self.cursor_char);
        self.value.replace_range(start..end, "");
        self.cursor_char -= 1;
        true
    }

    fn delete_forward_once(&mut self) -> bool {
        if self.all_selected {
            self.all_selected = false;
            if self.value.is_empty() {
                return false;
            }
            self.value.clear();
            self.cursor_char = 0;
            return true;
        }
        let len = self.value.chars().count();
        if self.cursor_char >= len {
            return false;
        }
        let start = Self::char_to_byte(&self.value, self.cursor_char);
        let end = Self::char_to_byte(&self.value, self.cursor_char + 1);
        self.value.replace_range(start..end, "");
        true
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.key == "enter" {
            self.all_selected = false;
            cx.emit(TextInputEvent::Submitted(self.value.clone()));
            cx.stop_propagation();
            return;
        }
        if ks.key == "escape" {
            self.all_selected = false;
            cx.emit(TextInputEvent::Escape);
            window.blur(cx);
            cx.stop_propagation();
            return;
        }
        if ks.key == "down" {
            cx.emit(TextInputEvent::ArrowDown);
            cx.stop_propagation();
            return;
        }
        if ks.key == "up" {
            cx.emit(TextInputEvent::ArrowUp);
            cx.stop_propagation();
            return;
        }
        if ks.key == "left" {
            self.all_selected = false;
            self.cursor_char = self.cursor_char.saturating_sub(1);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if ks.key == "right" {
            self.all_selected = false;
            let len = self.value.chars().count();
            self.cursor_char = (self.cursor_char + 1).min(len);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if ks.key == "home" {
            self.all_selected = false;
            self.cursor_char = 0;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if ks.key == "end" {
            self.all_selected = false;
            self.cursor_char = self.value.chars().count();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if ks.key == "backspace" {
            if self.backspace_once() {
                cx.emit(TextInputEvent::Changed(self.value.clone()));
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if ks.key == "delete" {
            if self.delete_forward_once() {
                cx.emit(TextInputEvent::Changed(self.value.clone()));
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if ks.modifiers.control {
            match ks.key.as_str() {
                "a" => {
                    if !self.value.is_empty() {
                        self.all_selected = true;
                        self.cursor_char = self.value.chars().count();
                        cx.notify();
                    }
                    cx.stop_propagation();
                    return;
                }
                "c" => {
                    if !self.value.is_empty() && !self.masked {
                        cx.write_to_clipboard(ClipboardItem::new_string(self.value.clone()));
                    }
                    cx.stop_propagation();
                    return;
                }
                "u" => {
                    if !self.value.is_empty() {
                        self.value.clear();
                        self.cursor_char = 0;
                        self.all_selected = false;
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
                        if self.insert_text(&single_line) {
                            cx.emit(TextInputEvent::Changed(self.value.clone()));
                            cx.notify();
                        }
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
            if self.insert_text(ch) {
                cx.emit(TextInputEvent::Changed(self.value.clone()));
                cx.notify();
            }
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
        let display_full: String = if is_empty {
            self.placeholder.to_string()
        } else if self.masked {
            "•".repeat(self.value.chars().count().min(32))
        } else {
            self.value.clone()
        };

        let border_col = if focused { theme.accent } else { theme.border };
        let text_col = if is_empty {
            theme.text_muted
        } else {
            theme.text_primary
        };

        let (before_cursor, after_cursor) = if is_empty || self.masked {
            (display_full.clone(), String::new())
        } else {
            let split = Self::char_to_byte(&self.value, self.cursor_char);
            (
                self.value[..split].to_owned(),
                self.value[split..].to_owned(),
            )
        };

        div()
            .id(("text-input", cx.entity_id()))
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.all_selected = false;
                    this.cursor_char = this.value.chars().count();
                    this.focus_handle.focus(window, cx);
                    cx.emit(TextInputEvent::Focused);
                    cx.notify();
                }),
            )
            .w_full()
            .h(px(38.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(10.0))
            .bg(theme.surface_elevated)
            .border_1()
            .border_color(border_col)
            .cursor_text()
            .overflow_hidden()
            .text_size(px(13.5))
            .text_color(text_col)
            .when_some(self.leading_icon, |el, ic| {
                el.child(icons::icon(
                    ic,
                    15.0,
                    if focused {
                        theme.accent
                    } else {
                        theme.text_muted
                    },
                ))
            })
            .child(if is_empty {
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(focused, |el| {
                        el.child(div().mr(px(1.5)).w(px(1.5)).h(px(15.0)).bg(theme.accent))
                    })
                    .child(SharedString::from(display_full))
                    .into_any_element()
            } else if self.all_selected && focused {
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .px(px(2.0))
                            .rounded(px(3.0))
                            .bg(theme.accent_soft)
                            .child(SharedString::from(display_full)),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(SharedString::from(before_cursor))
                    .when(focused, |el| {
                        el.child(div().mx(px(0.5)).w(px(1.5)).h(px(15.0)).bg(theme.accent))
                    })
                    .when(!after_cursor.is_empty(), |el| {
                        el.child(SharedString::from(after_cursor))
                    })
                    .into_any_element()
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_to_byte_handles_utf8_boundaries() {
        let s = "a♪é";
        assert_eq!(TextInput::char_to_byte(s, 0), 0);
        assert_eq!(TextInput::char_to_byte(s, 1), 1);
        assert_eq!(TextInput::char_to_byte(s, 2), 4);
        assert_eq!(TextInput::char_to_byte(s, 3), s.len());
        assert_eq!(TextInput::char_to_byte(s, 99), s.len());
    }
}
