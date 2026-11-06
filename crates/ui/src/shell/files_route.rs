//! The Files route: the cross-machine explorer, opened from the sidebar footer
//! or the command palette.

use super::*;
use crate::explorer::ExplorerPage;

impl Shell {
    /// Shows the explorer, recording a navigation unless it is already shown.
    pub(super) fn open_files(&mut self, cx: &mut Context<Self>) {
        self.enter_files_route(cx);
        if self.nav.current() != &NavEntry::Files {
            self.nav.push(NavEntry::Files);
        }
        cx.notify();
    }

    /// Shows the explorer's route without recording a navigation.
    pub(super) fn enter_files_route(&mut self, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Files) {
            return;
        }
        self.set_route(Route::Files, cx);
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// The explorer page, made on the route's first render.
    pub(super) fn render_files_route(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let page = self
            .files_page
            .get_or_insert_with(|| cx.new(|cx| ExplorerPage::new(self.state.clone(), cx)))
            .clone();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(page)
            .into_any_element()
    }

    pub(super) fn render_files_button(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = matches!(self.route, Route::Files);
        div()
            .id("open-files")
            .role(gpui::Role::Button)
            .aria_label("Files")
            .tab_index(0)
            .debug_selector(|| "open-files".into())
            .focus_visible(|style| style.border_2().border_color(theme.accent))
            .size(px(SIDEBAR_FOOTER_ACTION_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .bg(if on {
                theme.glass_hover()
            } else {
                motion::hover_blend(
                    "open-files",
                    theme.glass_hover().opacity(0.0),
                    theme.glass_hover(),
                )
            })
            .on_hover(motion::hover_listener("open-files"))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_focus_pending = false;
                this.open_files(cx);
            }))
            .tooltip(crate::settings::widgets::text_tooltip("Files"))
            .child(
                icon(icons::FOLDER_WITH_FILES)
                    .size(px(15.0))
                    .text_color(if on {
                        theme.text
                    } else {
                        motion::hover_blend("open-files", theme.text_muted, theme.text)
                    }),
            )
            .into_any_element()
    }
}
