//! Listen Together party room screen (`W4-3`) talking natively to the existing
//! BitChord Go backend (`backend/`) over REST + WebSocket (`Authorization: Bearer`).

use bitchord_integrations::Connection;
use gpui::prelude::*;
use gpui::{Context, FontWeight, Window, div, px};

use crate::app::BitChordApp;
use crate::backend::BackendCommand;
use crate::icons;

impl BitChordApp {
    pub fn render_party(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let in_party = self.party.in_party();
        let conn_label = match self.party.connection {
            Connection::Live => "Live WebSocket",
            Connection::Connecting => "Connecting...",
            Connection::Offline => "Disconnected",
        };

        div()
            .id("party-scroll")
            .size_full()
            .overflow_y_scroll()
            .px(px(28.0))
            .py(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .max_w(px(760.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(icons::icon(icons::USERS, 22.0, theme.accent))
                            .child(
                                div()
                                    .text_size(px(24.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme.text_primary)
                                    .child("Listen Together"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child("Synchronized group playback compatible with BitChord Go party servers."),
                    ),
            )
            // Server URL & Nickname configuration
            .child(
                div()
                    .p(px(16.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_primary)
                            .child("Party Server & Identity"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(div().flex_1().child(self.party_server_input.clone()))
                            .child(
                                div()
                                    .id("party-probe-btn")
                                    .h(px(38.0))
                                    .px(px(14.0))
                                    .rounded(px(10.0))
                                    .bg(theme.surface_elevated)
                                    .border_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .text_color(theme.text_primary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let url =
                                            this.party_server_input.read(cx).text().to_owned();
                                        this.send(BackendCommand::PartyProbeServer(url), cx);
                                    }))
                                    .child(icons::icon(icons::REFRESH, 14.0, theme.text_secondary))
                                    .child("Check /healthz"),
                            ),
                    )
                    .when_some(self.party_probe_status.clone(), |el, st| {
                        let ok = st.contains("Online");
                        el.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(if ok {
                                    theme.success
                                } else {
                                    theme.text_secondary
                                })
                                .child(st),
                        )
                    })
                    .child(self.party_nick_input.clone()),
            )
            // Active Party card OR Create / Join controls
            .child(if in_party {
                let code = self.party.code.clone().unwrap_or_default();
                let members = self.party.members.clone();
                let rtt = self.party.round_trip_ms;
                let max_m = self.party.max_members;

                div()
                    .p(px(18.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.accent)
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_size(px(20.0))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(theme.text_primary)
                                            .child(format!("Room {code}")),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.0))
                                            .text_color(theme.text_secondary)
                                            .child(format!(
                                                "{conn_label} · {} / {} members · RTT {rtt} ms",
                                                members.len(),
                                                max_m
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .id("party-leave-btn")
                                    .h(px(36.0))
                                    .px(px(14.0))
                                    .rounded(px(9.0))
                                    .bg(theme.danger)
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.on_accent)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.send(BackendCommand::PartyLeave, cx);
                                    }))
                                    .child(icons::icon(icons::CLOSE, 14.0, theme.on_accent))
                                    .child("Leave Party"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .children(members.into_iter().map(|m| {
                                div()
                                    .px(px(12.0))
                                    .py(px(8.0))
                                    .rounded(px(8.0))
                                    .bg(theme.surface_elevated)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .text_color(theme.text_primary)
                                            .child(m.display_name),
                                    )
                                    .when(m.is_host, |el| {
                                        el.child(
                                            div()
                                                .px(px(7.0))
                                                .py(px(2.0))
                                                .rounded(px(5.0))
                                                .bg(theme.accent_soft)
                                                .text_size(px(10.5))
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(theme.accent)
                                                .child("HOST"),
                                        )
                                    })
                            })),
                    )
                    .into_any_element()
            } else {
                div()
                    .p(px(16.0))
                    .rounded(px(12.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_primary)
                            .child("Start or Join a Room"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(div().flex_1().child(self.party_code_input.clone()))
                            .child(
                                div()
                                    .id("party-join-btn")
                                    .h(px(38.0))
                                    .px(px(16.0))
                                    .rounded(px(10.0))
                                    .bg(theme.accent)
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.on_accent)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let code =
                                            this.party_code_input.read(cx).text().to_owned();
                                        this.join_party(code, cx);
                                    }))
                                    .child(icons::icon(icons::USERS, 14.5, theme.on_accent))
                                    .child("Join Room"),
                            )
                            .child(
                                div()
                                    .id("party-create-btn")
                                    .h(px(38.0))
                                    .px(px(16.0))
                                    .rounded(px(10.0))
                                    .bg(theme.surface_elevated)
                                    .border_1()
                                    .border_color(theme.border)
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .cursor_pointer()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text_primary)
                                    .hover(move |s| s.bg(theme.surface_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let server_url =
                                            this.party_server_input.read(cx).text().to_owned();
                                        let nickname =
                                            this.party_nick_input.read(cx).text().to_owned();
                                        this.send(
                                            BackendCommand::PartyCreate {
                                                server_url,
                                                nickname,
                                            },
                                            cx,
                                        );
                                    }))
                                    .child(icons::icon(icons::PLUS, 14.5, theme.text_secondary))
                                    .child("Create Room"),
                            ),
                    )
                    .into_any_element()
            })
    }
}
