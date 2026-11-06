//! Settings → Notifications: independently configurable session chimes plus
//! desktop banners on the same status transitions (`shell::on_state_changed`),
//! and whether this Mac stays awake while its agents run.
//!
//! The ShortcutsPage arrangement: the page holds a working copy, every flip
//! emits [`NotificationsEvent::Changed`], and the shell persists it. Nothing
//! here talks RPC — all preferences are device-local UI settings.

use gpui::prelude::*;
use gpui::{Context, EventEmitter, SharedString, Window, div, px};

use crate::settings::search::{SearchAnchor, SettingId};
use crate::settings::widgets;
use crate::theme::Theme;
use crate::{icons, popover};

/// The page's whole preference set, which the shell persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationPrefs {
    pub sound: bool,
    pub completion_sound: bool,
    pub input_sound: bool,
    pub attention_sound: bool,
    pub desktop: bool,
    pub background_only: bool,
    pub agent_updates: bool,
    pub keep_awake: bool,
}

#[derive(Debug, Clone)]
pub enum NotificationsEvent {
    /// A toggle flipped — persist the complete notification preference set.
    Changed(NotificationPrefs),
}

pub struct NotificationsPage {
    scroll: widgets::PageScroll,
    prefs: NotificationPrefs,
}

impl EventEmitter<NotificationsEvent> for NotificationsPage {}

#[derive(Clone, Copy)]
enum NotificationPreference {
    Sound,
    CompletionSound,
    InputSound,
    AttentionSound,
    Desktop,
    BackgroundOnly,
    AgentUpdates,
    KeepAwake,
}

fn is_switch_activation(key: &str, is_held: bool) -> bool {
    !is_held && matches!(key, "enter" | "space")
}

fn interactive_switch(
    element: gpui::Stateful<gpui::Div>,
    accent: gpui::Hsla,
    preference: NotificationPreference,
    cx: &mut Context<NotificationsPage>,
) -> gpui::Stateful<gpui::Div> {
    element
        .tab_index(0)
        .focus_visible(move |style| style.border_2().border_color(accent))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle(preference, cx);
        }))
        .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
            if is_switch_activation(&event.keystroke.key, event.is_held) {
                cx.stop_propagation();
                this.toggle(preference, cx);
            }
        }))
}

impl NotificationsPage {
    pub fn new(prefs: NotificationPrefs) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            prefs,
        }
    }

    fn toggle(&mut self, preference: NotificationPreference, cx: &mut Context<Self>) {
        let value = match preference {
            NotificationPreference::Sound => &mut self.prefs.sound,
            NotificationPreference::CompletionSound => &mut self.prefs.completion_sound,
            NotificationPreference::InputSound => &mut self.prefs.input_sound,
            NotificationPreference::AttentionSound => &mut self.prefs.attention_sound,
            NotificationPreference::Desktop => &mut self.prefs.desktop,
            NotificationPreference::BackgroundOnly => &mut self.prefs.background_only,
            NotificationPreference::AgentUpdates => &mut self.prefs.agent_updates,
            NotificationPreference::KeepAwake => &mut self.prefs.keep_awake,
        };
        *value = !*value;
        cx.emit(NotificationsEvent::Changed(self.prefs));
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for NotificationsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for NotificationsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let accent = theme.accent;
        let sound = self.prefs.sound;
        let completion_sound = self.prefs.completion_sound;
        let input_sound = self.prefs.input_sound;
        let attention_sound = self.prefs.attention_sound;
        let desktop = self.prefs.desktop;
        let background_only = self.prefs.background_only;
        let agent_updates = self.prefs.agent_updates;
        let keep_awake = self.prefs.keep_awake;
        let toggle = |id: &'static str, label: &'static str, enabled: bool, interactive: bool| {
            // Keep the visual inside its 56×40 activation target.
            // Disabled subordinate controls remain named switches in the
            // accessibility tree, but have no focus or input handlers.
            div()
                .id(id)
                .flex_none()
                .w(px(widgets::SWITCH_WIDTH))
                .h(px(40.0))
                .flex()
                .items_center()
                .justify_center()
                .role(gpui::Role::Switch)
                .aria_label(label)
                .aria_toggled(if enabled {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .when(!interactive, |el| {
                    el.aria_description("Unavailable while its parent setting is off")
                })
                .child(widgets::toggle_switch(&theme, enabled, id))
        };
        let card = widgets::section_card(&theme)
            .mt_0()
            .child(
                widgets::card_row(&theme, true)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Session sounds")),
                    )
                    .child(interactive_switch(
                        toggle("notifications-sound-toggle", "Session sounds", sound, true),
                        accent,
                        NotificationPreference::Sound,
                        cx,
                    ))
                    .search_anchor(SettingId::SessionSounds),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!sound, |el| el.opacity(0.55))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Task completed")),
                    )
                    .child(
                        toggle(
                            "notifications-completion-sound-toggle",
                            "Task completed sound",
                            completion_sound,
                            sound,
                        )
                        .when(sound, |el| {
                            interactive_switch(
                                el,
                                accent,
                                NotificationPreference::CompletionSound,
                                cx,
                            )
                        }),
                    )
                    .search_anchor(SettingId::TaskCompletedSound),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!sound, |el| el.opacity(0.55))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Input required")),
                    )
                    .child(
                        toggle(
                            "notifications-input-sound-toggle",
                            "Input required sound",
                            input_sound,
                            sound,
                        )
                        .when(sound, |el| {
                            interactive_switch(el, accent, NotificationPreference::InputSound, cx)
                        }),
                    )
                    .search_anchor(SettingId::InputRequiredSound),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!sound, |el| el.opacity(0.55))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Errors and disconnections")),
                    )
                    .child(
                        toggle(
                            "notifications-attention-sound-toggle",
                            "Errors and disconnections sound",
                            attention_sound,
                            sound,
                        )
                        .when(sound, |el| {
                            interactive_switch(
                                el,
                                accent,
                                NotificationPreference::AttentionSound,
                                cx,
                            )
                        }),
                    )
                    .search_anchor(SettingId::ErrorSound),
            );
        let desktop_card = widgets::section_card(&theme)
            .mt_0()
            .child(
                widgets::card_row(&theme, true)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Desktop notifications")),
                    )
                    .child(interactive_switch(
                        toggle(
                            "notifications-desktop-toggle",
                            "Desktop notifications",
                            desktop,
                            true,
                        ),
                        accent,
                        NotificationPreference::Desktop,
                        cx,
                    ))
                    .search_anchor(SettingId::DesktopNotifications),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!desktop, |el| el.opacity(0.55))
                    .child(widgets::row_tile(&theme, icons::REFRESH))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Agent updates"))
                            .child(widgets::meta_line(
                                &theme,
                                vec![
                                    div()
                                        .child(SharedString::from(
                                            "Show a banner when monitored agent CLIs have updates.",
                                        ))
                                        .into_any_element(),
                                ],
                            )),
                    )
                    .child(
                        toggle(
                            "notifications-agent-updates-toggle",
                            "Agent update notifications",
                            agent_updates,
                            desktop,
                        )
                        .when(desktop, |el| {
                            interactive_switch(el, accent, NotificationPreference::AgentUpdates, cx)
                        }),
                    )
                    .search_anchor(SettingId::AgentUpdates),
            )
            .child(
                // Sub-option of the banner row: dimmed + inert while banners
                // are off (the harnesses not-installed treatment).
                widgets::card_row(&theme, false)
                    .when(!desktop, |el| el.opacity(0.55))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Only when in the background")),
                    )
                    .child(
                        toggle(
                            "notifications-background-toggle",
                            "Only notify when Onicode is in the background",
                            background_only,
                            desktop,
                        )
                        .when(desktop, |el| {
                            interactive_switch(
                                el,
                                accent,
                                NotificationPreference::BackgroundOnly,
                                cx,
                            )
                        }),
                    )
                    .search_anchor(SettingId::BackgroundOnly),
            );
        let power_card = widgets::section_card(&theme).mt_0().child(
            widgets::card_row(&theme, true)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(widgets::row_title(
                            &theme,
                            "Keep this Mac awake while agents run",
                        ))
                        .child(widgets::meta_line(
                            &theme,
                            vec![
                                div()
                                    .child(SharedString::from(
                                        "Idle sleep waits for agent turns and terminal commands. \
                                         The display still sleeps.",
                                    ))
                                    .into_any_element(),
                            ],
                        )),
                )
                .child(interactive_switch(
                    toggle(
                        "notifications-keep-awake-toggle",
                        "Keep this Mac awake while agents run",
                        keep_awake,
                        true,
                    ),
                    accent,
                    NotificationPreference::KeepAwake,
                    cx,
                ))
                .search_anchor(SettingId::KeepAwake),
        );

        let scrollbar = popover::rail(self, "notifications-page-scrollbar", &theme, cx);
        div()
            .id("notifications-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("notifications-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                .child(widgets::page_header(&theme, "Notifications", None))
                                .child(
                                    widgets::section(&theme, "Desktop", desktop_card).mt(px(24.0)),
                                )
                                .child(widgets::section(&theme, "Sounds", card))
                                .child(widgets::section(&theme, "Power", power_card)),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .child(widgets::page_reveal(&self.scroll.scroll))
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::is_switch_activation;

    #[test]
    fn switches_accept_enter_or_space_once_per_press() {
        assert!(is_switch_activation("enter", false));
        assert!(is_switch_activation("space", false));
        assert!(!is_switch_activation("escape", false));
        assert!(!is_switch_activation("space", true));
    }
}
