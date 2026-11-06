//! Original application destinations replace the project workspace temporarily.
use super::*;

impl Shell {
    pub(super) fn classic_surface_open(&self) -> bool {
        self.workspace.classic_layout
            && (self.settings.is_some() || self.app_view_open() || self.search_page())
    }

    pub(super) fn session_sidebar_available(&self) -> bool {
        !self.classic_surface_open()
    }

    pub(super) fn classic_app_surface_header(&self) -> AnyElement {
        let view = self
            .active_app_view
            .expect("an application destination is open");
        let inset = if !self.workspace.project_rail_open {
            self.strip_lead() + 6.
        } else {
            12.
        };
        titlebar_drag_area()
            .id("classic-app-surface-header")
            .debug_selector(|| "classic-app-surface-header".into())
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .border_b_1()
            .border_color(content(0.07))
            .flex()
            .items_center()
            .pl(px(inset))
            .pr(px(12. + self.window_controls))
            .gap_2()
            .child(
                icon(
                    if view == app_views::AppView::Notes {
                        "file"
                    } else {
                        view.icon()
                    },
                    14.,
                )
                .text_color(content(0.45)),
            )
            .child(
                div()
                    .debug_selector(|| "classic-app-surface-title".into())
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(content(1.))
                    .child(view.label()),
            )
            .into_any_element()
    }

    pub(super) fn classic_rail_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.close_settings(window, cx);
        } else {
            self.visit(-1, window, cx);
        }
    }
}
