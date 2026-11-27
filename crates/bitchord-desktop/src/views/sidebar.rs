//! Collapsible grouped navigation rail (`W4-1`) with crisp vector SVG icons
//! (`notes.md` #5) and a persistent active shape + crimson edge marker that
//! survives collapsed icon-rail mode (`R-24`, `R-25`).

use gpui::prelude::*;
use gpui::{Context, FontWeight, div, px};

use crate::actions::Route;
use crate::app::BitChordApp;
use crate::branding;
use crate::icons;
use crate::theme::{BitChordTheme, ThemeMode};

impl BitChordApp {
    pub fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let collapsed = self.sidebar_collapsed;
        let rail_width = if collapsed { px(72.0) } else { px(228.0) };

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
                    .gap(px(18.0))
                    // Brand header + rail collapse toggle
                    .child(
                        div()
                            .px(px(4.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(if collapsed {
                                div()
                                    .w(px(30.0))
                                    .h(px(30.0))
                                    .rounded(px(8.0))
                                    .bg(theme.accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icons::icon(icons::BRAND_WAVE, 17.0, theme.on_accent))
                                    .into_any_element()
                            } else {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.0))
                                    .child(
                                        div()
                                            .w(px(28.0))
                                            .h(px(28.0))
                                            .rounded(px(8.0))
                                            .bg(theme.accent)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(icons::icon(
                                                icons::BRAND_WAVE,
                                                16.0,
                                                theme.on_accent,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(16.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child(branding::APP_NAME),
                                    )
                                    .into_any_element()
                            })
                            .child(
                                div()
                                    .id("sidebar-collapse")
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .rounded(px(7.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_collapsed = !this.sidebar_collapsed;
                                        cx.notify();
                                    }))
                                    .child(icons::icon(
                                        if collapsed {
                                            icons::SIDEBAR_EXPAND
                                        } else {
                                            icons::SIDEBAR_COLLAPSE
                                        },
                                        16.0,
                                        theme.text_muted,
                                    )),
                            ),
                    )
                    // Group 1: Discover
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .when(!collapsed, |el| el.child(section_label("DISCOVER", theme)))
                            .child(self.nav_row(
                                "nav-home",
                                icons::HOME,
                                "Home",
                                Route::Home,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-search",
                                icons::SEARCH,
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
                                icons::LIBRARY,
                                "Library",
                                Route::Library,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-downloads",
                                icons::DOWNLOAD,
                                "Downloads",
                                Route::Downloads,
                                theme,
                                cx,
                            )),
                    )
                    // Group 3: Stage & Settings
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .when(!collapsed, |el| el.child(section_label("STAGE", theme)))
                            .child(self.nav_row(
                                "nav-now-playing",
                                icons::DISC,
                                "Now Playing",
                                Route::NowPlaying,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-party",
                                icons::USERS,
                                "Listen Together",
                                Route::ListenTogether,
                                theme,
                                cx,
                            ))
                            .child(self.nav_row(
                                "nav-settings",
                                icons::SETTINGS,
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
                            .h(px(38.0))
                            .px(px(10.0))
                            .rounded(px(9.0))
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
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(icons::icon(
                                        if self.theme_mode == ThemeMode::Dark {
                                            icons::MOON
                                        } else {
                                            icons::SUN
                                        },
                                        15.0,
                                        theme.text_primary,
                                    ))
                                    .when(!collapsed, |el| {
                                        el.child(
                                            div()
                                                .text_size(px(12.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.text_primary)
                                                .child(self.theme_mode.label()),
                                        )
                                    }),
                            )
                            .when(!collapsed, |el| {
                                el.child(
                                    div()
                                        .text_size(px(10.5))
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
        icon_bytes: &'static [u8],
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
        let icon_col = if active {
            theme.accent
        } else {
            theme.text_muted
        };

        div()
            .id(id)
            .relative()
            .w_full()
            .h(px(38.0))
            .px(px(10.0))
            .rounded(px(9.0))
            .bg(bg)
            .flex()
            .items_center()
            .when(collapsed, |el| el.justify_center())
            .gap(px(11.0))
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
            // Left crimson active indicator bar
            .when(active, |el| {
                el.child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(9.0))
                        .w(px(3.0))
                        .h(px(20.0))
                        .rounded_r(px(3.0))
                        .bg(theme.accent),
                )
            })
            .child(icons::icon(icon_bytes, 17.0, icon_col))
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
        .pb(px(3.0))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(text)
}
