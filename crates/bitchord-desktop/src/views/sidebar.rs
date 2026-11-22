//! Collapsible grouped navigation rail (`W4-1`) with persistent active shape +
//! crimson edge marker that survives collapsed icon-rail mode (`R-24`, `R-25`).

use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};

use crate::actions::Route;
use crate::app::BitChordApp;
use crate::theme::BitChordTheme;

impl BitChordApp {
    pub fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let collapsed = self.sidebar_collapsed;
        let rail_width = if collapsed { px(68.0) } else { px(224.0) };

        div()
            .w(rail_width)
            .h_full()
            .flex_shrink_0()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .justify_between()
            .py(px(14.0))
            .px(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    // Brand header + rail collapse toggle
                    .child(
                        div()
                            .px(px(6.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(if collapsed {
                                div()
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(7.0))
                                    .bg(theme.accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.on_accent)
                                    .child("B")
                                    .into_any_element()
                            } else {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .w(px(24.0))
                                            .h(px(24.0))
                                            .rounded(px(6.0))
                                            .bg(theme.accent)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(12.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.on_accent)
                                            .child("B"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child("BitChord"),
                                    )
                                    .into_any_element()
                            })
                            .child(
                                div()
                                    .id("sidebar-collapse")
                                    .w(px(26.0))
                                    .h(px(26.0))
                                    .rounded(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .text_size(px(12.0))
                                    .text_color(theme.text_muted)
                                    .hover(move |s| {
                                        s.bg(theme.surface_hover).text_color(theme.text_primary)
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_collapsed = !this.sidebar_collapsed;
                                        cx.notify();
                                    }))
                                    .child(if collapsed { ">" } else { "<" }),
                            ),
                    )
                    // Group 1: Discover
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .when(!collapsed, |el| el.child(section_label("DISCOVER", theme)))
                            .child(self.nav_row("nav-home", "HM", "Home", Route::Home, theme, cx))
                            .child(self.nav_row(
                                "nav-search",
                                "SR",
                                "Search",
                                Route::Search,
                                theme,
                                cx,
                            )),
                    )
                    // Group 2: Collection
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .when(!collapsed, |el| {
                                el.child(section_label("COLLECTION", theme))
                            })
                            .child(self.nav_row(
                                "nav-library",
                                "LB",
                                "Library",
                                Route::Library,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-downloads",
                                "DL",
                                "Downloads",
                                Route::Downloads,
                                theme,
                                cx,
                            )),
                    )
                    // Group 3: Social & Audio
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .when(!collapsed, |el| el.child(section_label("STAGE", theme)))
                            .child(self.nav_row(
                                "nav-now-playing",
                                "NP",
                                "Now Playing",
                                Route::NowPlaying,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-party",
                                "LT",
                                "Listen Together",
                                Route::ListenTogether,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-settings",
                                "ST",
                                "Settings",
                                Route::Settings,
                                theme,
                                cx,
                            )),
                    ),
            )
            // Bottom utility: Theme polarity toggle (Dark OLED / Light Studio)
            .child(
                div()
                    .pt(px(10.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("sidebar-theme-toggle")
                            .w_full()
                            .h(px(36.0))
                            .px(px(10.0))
                            .rounded(px(8.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .bg(theme.surface_elevated)
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.theme_mode = this.theme_mode.toggled();
                                cx.set_global(crate::theme::ActiveTheme(this.theme_mode));
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text_primary)
                                    .child(if collapsed {
                                        if self.theme_mode == crate::theme::ThemeMode::Dark {
                                            "DK"
                                        } else {
                                            "LT"
                                        }
                                    } else {
                                        self.theme_mode.label()
                                    }),
                            )
                            .when(!collapsed, |el| {
                                el.child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(theme.text_muted)
                                        .child("Ctrl+T"),
                                )
                            }),
                    ),
            )
    }

    fn nav_row(
        &self,
        id: &'static str,
        short_code: &'static str,
        label: &'static str,
        target: Route,
        theme: BitChordTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.route == target;
        let collapsed = self.sidebar_collapsed;
        let bg = if active {
            theme.surface_elevated
        } else {
            gpui::transparent_black()
        };
        let fg = if active {
            theme.text_primary
        } else {
            theme.text_secondary
        };

        div()
            .id(id)
            .relative()
            .w_full()
            .h(px(38.0))
            .px(px(10.0))
            .rounded(px(8.0))
            .bg(bg)
            .flex()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .hover(move |s| {
                s.bg(if active {
                    theme.surface_active
                } else {
                    theme.surface_hover
                })
                .text_color(theme.text_primary)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.navigate(target.clone(), cx);
            }))
            // Left crimson active indicator bar (real active-route signal)
            .when(active, |el| {
                el.child(
                    div()
                        .w(px(3.0))
                        .h(px(18.0))
                        .rounded(px(2.0))
                        .bg(theme.accent),
                )
            })
            .child(
                div()
                    .w(px(24.0))
                    .text_size(px(11.0))
                    .font_weight(if active {
                        FontWeight::BOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(if active {
                        theme.accent
                    } else {
                        theme.text_muted
                    })
                    .child(short_code),
            )
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(if active {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(fg)
                        .child(label),
                )
            })
    }
}

fn section_label(text: &'static str, theme: BitChordTheme) -> impl IntoElement {
    div()
        .px(px(10.0))
        .pb(px(2.0))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(text)
}
