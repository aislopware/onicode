//! Settings → General → Permissions: the way back to the permissions checklist
//! (`crate::onboarding::permissions`), which onboarding opened once.

use gpui::prelude::*;
use gpui::{Context, Entity, div, px};

use crate::settings::search::{SearchAnchor, SettingId};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct PermissionsCard {
    state: Entity<AppState>,
}

impl PermissionsCard {
    pub fn new(state: Entity<AppState>, _cx: &mut Context<Self>) -> Self {
        Self { state }
    }
}

impl Render for PermissionsCard {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let row = widgets::card_row(&theme, true)
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.0))
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(&theme, "Permissions"))
                    .child(widgets::meta_line(
                        &theme,
                        vec![
                            div()
                                .child(
                                    "Screen Recording, Full Disk Access and the rest, in one list.",
                                )
                                .into_any_element(),
                        ],
                    )),
            )
            .child(
                widgets::text_action(&theme, widgets::ActionTone::Filled, "Open")
                    .id("settings-permissions-open")
                    .debug_selector(|| "settings-permissions-open".to_owned())
                    .tab_index(0)
                    .role(gpui::Role::Button)
                    .aria_label("Open the permissions checklist")
                    .focus_visible(|s| s.border_2().border_color(theme.accent))
                    .on_click(cx.listener(|card, _, _, cx| {
                        crate::onboarding::permissions::open_window(Some(card.state.clone()), cx);
                    })),
            )
            .search_anchor(SettingId::Permissions);
        widgets::section(
            &theme,
            "Permissions",
            widgets::section_card(&theme).mt_0().child(row),
        )
    }
}
