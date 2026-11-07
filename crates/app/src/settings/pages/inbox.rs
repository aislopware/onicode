use gpui::prelude::*;
use gpui::{AnyElement, Context, Window, div, px};

use crate::settings::widgets::secondary_button;
use crate::settings::{SettingsEvent, SettingsView};
use crate::theme::*;
impl SettingsView {
    pub(super) fn inbox_page(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let now = crate::inbox::RelativeNow::read(window, cx);
        let mut page = vec![self.project_notification_settings(&now, cx)];
        let (connected, connection_label) = self
            .ctx
            .inbox_connection
            .clone()
            .unwrap_or_else(|| (false, "Checking connection…".into()));
        let status = div()
            .text_size(px(12.))
            .text_color(content(if connected { 0.7 } else { 0.5 }))
            .child(connection_label)
            .into_any_element();
        let controls = vec![
            status,
            secondary_button(
                "github-installation-guide",
                "Installation guide",
                false,
                false,
            )
            .child("Installation guide")
            .on_click(|_, _, cx| cx.open_url("https://cli.github.com/"))
            .into_any_element(),
            secondary_button("github-check-again", "Check again", false, false)
                .child("Check again")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::ManageInboxConnection)))
                .into_any_element(),
        ];
        let row = self.row(
            None,
            false,
            "Connection",
            Some(
                "Install GitHub CLI from cli.github.com, run gh auth login in a terminal, then \
                 check again."
                    .into(),
            ),
            controls,
        );
        page.push(
            self.group(
                Some("github"),
                false,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon("inbox-github", 16.))
                    .child("GitHub"),
                Some("Pull requests, reviews, and issues, read through the GitHub CLI.".into()),
                None,
                vec![row],
            ),
        );
        page
    }
}
