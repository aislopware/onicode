//! The explorer's render: the device sidebar, the panes with their virtualised
//! rows, the transfers tray and the preview over them. Labels are computed when
//! data arrives; render only lays them out.

use std::ops::Range;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, ExternalPaths, KeyDownEvent, MouseButton, MouseDownEvent, SharedString,
    Window, div, px, uniform_list,
};
use gpui_base::input::Input;

use super::pane::Listing;
use super::{DragGhost, ExplorerDrag, ExplorerPage, Roots, Side, promise};
use crate::file_icons::{self, FileIconIdentity};
use crate::icons::{self, icon};
use crate::theme::Theme;

const ROW_HEIGHT: f32 = 24.0;
const SIZE_WIDTH: f32 = 76.0;
const MODIFIED_WIDTH: f32 = 148.0;
const KIND_WIDTH: f32 = 104.0;
const SIDEBAR_WIDTH: f32 = 208.0;

impl Side {
    fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }
}

fn tool_button(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    on: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Button)
        .aria_label(label)
        .size(px(26.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .when(on, |el| el.bg(crate::theme::wash(0.1)))
        .hover(|style| style.bg(crate::theme::wash(0.12)))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .tooltip(crate::settings::widgets::text_tooltip(label))
        .child(icon(glyph).size(px(15.0)).text_color(if on {
            theme.text
        } else {
            theme.text_muted
        }))
}

fn status(text: SharedString, theme: &Theme) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(theme.text_faint)
        .child(text)
}

/// A sidebar place: a device's home or one of its roots.
struct Place {
    id: gpui::ElementId,
    glyph: &'static str,
    label: SharedString,
    current: bool,
    indent: f32,
    device: String,
    path: Option<String>,
}

impl ExplorerPage {
    /// A press on a row: select it the Finder way, and open it on a double click.
    fn press_row(
        &mut self,
        side: Side,
        ix: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.row_pressed = true;
        self.set_active(side, cx);
        let pane = self.pane_mut(side);
        pane.focus.focus(window, cx);
        if event.modifiers.shift {
            pane.extend_to(ix);
        } else if event.modifiers.secondary(cx) {
            pane.toggle(ix);
        } else if !pane.selected.contains(&ix) || event.click_count >= 2 {
            pane.select_only(ix);
        } else {
            // Pressed inside the selection: keep it, so a drag carries all of it.
            pane.cursor = Some(ix);
        }
        if event.click_count >= 2 {
            self.open_row(side, ix, cx);
        } else {
            self.follow_preview(side, cx);
        }
        cx.notify();
    }

    /// A press on a pane's empty space clears its selection.
    fn press_pane(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.row_pressed) {
            return;
        }
        self.set_active(side, cx);
        let pane = self.pane_mut(side);
        pane.focus.focus(window, cx);
        if pane.clear_selection() {
            cx.notify();
        }
    }

    fn on_key(
        &mut self,
        side: Side,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu_key(event, window, cx) || self.rename.is_some() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let cursor = self.pane(side).cursor;
        let handled = match keystroke.key.as_str() {
            "up" if modifiers.platform => {
                self.go_up(side, cx);
                true
            }
            "down" if modifiers.platform => {
                if let Some(cursor) = cursor {
                    self.open_row(side, cursor, cx);
                }
                true
            }
            "up" | "down" | "home" | "end" => {
                let pane = self.pane_mut(side);
                let last = pane.rows.len().checked_sub(1);
                let moved = match keystroke.key.as_str() {
                    "up" => pane.step(-1, modifiers.shift),
                    "down" => pane.step(1, modifiers.shift),
                    "home" => last.inspect(|_| pane.select_only(0)).is_some(),
                    _ => last.inspect(|last| pane.select_only(*last)).is_some(),
                };
                if moved {
                    self.follow_preview(side, cx);
                    cx.notify();
                }
                true
            }
            "enter" => {
                if let Some(cursor) = cursor {
                    self.open_row(side, cursor, cx);
                }
                true
            }
            "backspace" if modifiers.platform => {
                let rows = self.pane(side).action_rows();
                self.trash(side, &rows, cx);
                true
            }
            "backspace" => {
                self.go_up(side, cx);
                true
            }
            "space" => {
                if !self.close_preview(cx)
                    && let Some(cursor) = cursor
                {
                    self.open_preview(side, cursor, cx);
                }
                true
            }
            "escape" => {
                if !self.close_preview(cx) && self.pane_mut(side).clear_selection() {
                    cx.notify();
                }
                true
            }
            "tab" if self.dual => {
                let other = side.other();
                self.set_active(other, cx);
                self.pane(other).focus.focus(window, cx);
                true
            }
            _ => {
                let plain = !modifiers.platform
                    && !modifiers.control
                    && !modifiers.alt
                    && !modifiers.function;
                match keystroke.key_char.as_deref() {
                    Some(text)
                        if plain && !text.is_empty() && !text.chars().any(char::is_control) =>
                    {
                        if self.pane_mut(side).type_select(text, Instant::now()) {
                            self.follow_preview(side, cx);
                            cx.notify();
                        }
                        true
                    }
                    _ => false,
                }
            }
        };
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn render_rows(
        &mut self,
        side: Side,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let pane = self.pane(side);
        let focused = pane.focus.is_focused(window);
        // One payload for the whole selection, shared by its rows.
        let selection_drag = (pane.selected.len() > 1)
            .then(|| pane.selected.first().map(|ix| self.drag_for(side, *ix)))
            .flatten();
        let renaming = self
            .rename
            .as_ref()
            .filter(|rename| rename.side == side)
            .map(|rename| (rename.path.as_str(), rename.input.clone()));
        let mut rows = Vec::with_capacity(range.len());
        for ix in range {
            let Some(row) = pane.rows.get(ix) else {
                continue;
            };
            let selected = pane.selected.contains(&ix);
            let cursor = pane.cursor == Some(ix);
            let drag = match &selection_drag {
                Some(drag) if selected => drag.clone(),
                _ => self.drag_for(side, ix),
            };
            let label: SharedString = match drag.items.len() {
                1 => row.name.clone(),
                n => format!("{n} items").into(),
            };
            let identity = if row.is_tree() {
                FileIconIdentity::directory(&row.name, false)
            } else {
                FileIconIdentity::file(&row.name)
            };
            let name = match &renaming {
                Some((path, input)) if *path == row.path => div()
                    .id("explorer-rename")
                    .flex_1()
                    .min_w_0()
                    .h(px(20.0))
                    .px(px(4.0))
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.accent)
                    .bg(theme.input_glass_bg())
                    .text_size(px(12.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|page, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            page.cancel_rename(window, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    }))
                    .child(Input::new(input))
                    .into_any_element(),
                _ => div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(row.name.clone())
                    .into_any_element(),
            };
            let state = self.state.clone();
            let mut element = div()
                .id(("explorer-row", ix))
                .debug_selector(move || format!("explorer-row-{}-{ix}", side.key()))
                .role(gpui::Role::Row)
                .aria_label(row.name.clone())
                .aria_selected(selected)
                .h(px(ROW_HEIGHT))
                .w_full()
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .rounded(px(5.0))
                .cursor_default()
                .when(selected, |el| {
                    el.bg(if focused {
                        theme.accent.opacity(0.32)
                    } else {
                        crate::theme::wash(0.1)
                    })
                })
                .when(!selected, |el| {
                    el.when(cursor && focused, |el| el.bg(crate::theme::wash(0.04)))
                        .hover(|style| style.bg(crate::theme::wash(0.05)))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |page, event: &MouseDownEvent, window, cx| {
                        page.press_row(side, ix, event, window, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |page, event: &MouseDownEvent, window, cx| {
                        page.row_pressed = true;
                        page.open_menu(side, Some(ix), event.position, window, cx);
                    }),
                )
                .on_drag(drag, move |_, _, _, cx| {
                    let label = label.clone();
                    cx.new(|_| DragGhost { label })
                })
                .external_drag_payload(move |drag: &ExplorerDrag, _, cx| {
                    promise::external_payload(drag, state.read(cx))
                });
            if row.is_folder() {
                let dir = row.path.clone();
                let external_dir = row.path.clone();
                element = element
                    .drag_over::<ExplorerDrag>(|style, _, _, _| style.bg(crate::theme::wash(0.14)))
                    .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(crate::theme::wash(0.14)))
                    .on_drop(cx.listener(move |page, drag: &ExplorerDrag, _, cx| {
                        cx.stop_propagation();
                        let device = page.pane(side).device_id.clone();
                        page.drop_rows(device, dir.clone(), drag, cx);
                    }))
                    .on_drop(cx.listener(move |page, paths: &ExternalPaths, _, cx| {
                        cx.stop_propagation();
                        let device = page.pane(side).device_id.clone();
                        page.drop_external(device, external_dir.clone(), paths, cx);
                    }));
            }
            rows.push(
                element
                    .child(
                        file_icons::icon(identity, theme.appearance)
                            .size(px(15.0))
                            .flex_none(),
                    )
                    .child(name)
                    .child(
                        div()
                            .w(px(SIZE_WIDTH))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(row.size.clone()),
                    )
                    .child(
                        div()
                            .w(px(MODIFIED_WIDTH))
                            .flex_none()
                            .pl(px(12.0))
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(row.modified.clone()),
                    )
                    .child(
                        div()
                            .w(px(KIND_WIDTH))
                            .flex_none()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(row.kind_label.clone()),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    fn render_pane(&self, side: Side, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pane = self.pane(side);
        let active = self.dual && self.active == side;
        let device = self.device(&pane.device_id);
        let device_name = device.map_or_else(|| SharedString::from("…"), |d| d.name.clone());
        let glyph = device.map_or(icons::LAPTOP, |d| d.glyph);
        let header = div()
            .h(px(34.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                tool_button(
                    if side == Side::Left {
                        "explorer-up-left"
                    } else {
                        "explorer-up-right"
                    },
                    "Enclosing folder",
                    icons::ARROW_UP,
                    false,
                    theme,
                )
                .when(pane.parent.is_none(), |el| el.opacity(0.4))
                .on_click(cx.listener(move |page, _, _, cx| page.go_up(side, cx))),
            )
            .child(icon(glyph).size(px(14.0)).text_color(theme.text_muted))
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(device_name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(pane.path.clone())),
            )
            .child(
                tool_button(
                    if side == Side::Left {
                        "explorer-refresh-left"
                    } else {
                        "explorer-refresh-right"
                    },
                    "Reload",
                    icons::REFRESH,
                    false,
                    theme,
                )
                .on_click(cx.listener(move |page, _, _, cx| page.reload(side, cx))),
            );
        let columns = div()
            .h(px(24.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child(div().w(px(15.0)).flex_none())
            .child(div().flex_1().min_w_0().child("Name"))
            .child(
                div()
                    .w(px(SIZE_WIDTH))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .child("Size"),
            )
            .child(
                div()
                    .w(px(MODIFIED_WIDTH))
                    .flex_none()
                    .pl(px(12.0))
                    .child("Modified"),
            )
            .child(div().w(px(KIND_WIDTH)).flex_none().child("Kind"));
        let body: AnyElement = match &pane.listing {
            Listing::Failed(message) => status(message.clone(), theme).into_any_element(),
            Listing::Loading if pane.rows.is_empty() => {
                status("Loading…".into(), theme).into_any_element()
            }
            Listing::Ready if pane.rows.is_empty() => {
                status("Empty folder".into(), theme).into_any_element()
            }
            Listing::Loading | Listing::Ready => uniform_list(
                ("explorer-rows", side.index()),
                pane.rows.len(),
                cx.processor(move |page, range: Range<usize>, window, cx| {
                    page.render_rows(side, range, window, cx)
                }),
            )
            .size_full()
            .px(px(8.0))
            .track_scroll(&pane.scroll)
            .into_any_element(),
        };
        let list = div()
            .id(("explorer-list", side.index()))
            .debug_selector(move || format!("explorer-list-{}", side.key()))
            .role(gpui::Role::Grid)
            .aria_label("Files")
            .flex_1()
            .min_h_0()
            .track_focus(&pane.focus)
            .on_key_down(cx.listener(move |page, event: &KeyDownEvent, window, cx| {
                page.on_key(side, event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |page, _, window, cx| page.press_pane(side, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |page, event: &MouseDownEvent, window, cx| {
                    if !std::mem::take(&mut page.row_pressed) {
                        page.open_menu(side, None, event.position, window, cx);
                    }
                }),
            )
            .drag_over::<ExplorerDrag>(|style, _, _, _| style.bg(crate::theme::wash(0.04)))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(crate::theme::wash(0.04)))
            .on_drop(cx.listener(move |page, drag: &ExplorerDrag, _, cx| {
                let pane = page.pane(side);
                let (device, dir) = (pane.device_id.clone(), pane.path.clone());
                page.drop_rows(device, dir, drag, cx);
            }))
            .on_drop(cx.listener(move |page, paths: &ExternalPaths, _, cx| {
                let pane = page.pane(side);
                let (device, dir) = (pane.device_id.clone(), pane.path.clone());
                page.drop_external(device, dir, paths, cx);
            }))
            .child(body);
        div()
            .id(("explorer-pane", side.index()))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .border_1()
            .border_color(if active {
                theme.border_strong
            } else {
                theme.border
            })
            .bg(crate::theme::wash(0.02))
            .child(header)
            .child(columns)
            .child(list)
            .into_any_element()
    }

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let active = self.pane(self.active);
        let mut sidebar = div()
            .id("explorer-sidebar")
            .debug_selector(|| "explorer-sidebar".into())
            .role(gpui::Role::Navigation)
            .aria_label("Devices")
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .pr(px(8.0))
            .flex()
            .flex_col()
            .gap(px(2.0));
        let mut root_ix = 0_usize;
        for (device_ix, device) in self.devices.iter().enumerate() {
            let home = self.home(&device.id).map(str::to_owned);
            let current = active.device_id == device.id;
            sidebar = sidebar.child(
                self.sidebar_row(
                    Place {
                        id: ("explorer-device", device_ix).into(),
                        glyph: device.glyph,
                        label: device.name.clone(),
                        current: current && home.as_deref() == Some(active.path.as_str()),
                        indent: 0.0,
                        device: device.id.clone(),
                        path: home,
                    },
                    theme,
                    cx,
                )
                .when(!device.online, |el| el.opacity(0.45))
                .mt(px(if device_ix == 0 { 0.0 } else { 10.0 })),
            );
            if let Some(Roots::Ready { entries, .. }) = self.roots.get(&device.id) {
                for entry in entries {
                    root_ix += 1;
                    sidebar = sidebar.child(self.sidebar_row(
                        Place {
                            id: ("explorer-root", root_ix).into(),
                            glyph: entry.icon,
                            label: entry.label.clone(),
                            current: current && active.path == entry.path,
                            indent: 14.0,
                            device: device.id.clone(),
                            path: Some(entry.path.clone()),
                        },
                        theme,
                        cx,
                    ));
                }
            }
        }
        sidebar.into_any_element()
    }

    /// A sidebar place: a click shows it in the active pane, a drop copies there.
    fn sidebar_row(
        &self,
        place: Place,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Place {
            id,
            glyph,
            label,
            current,
            indent,
            device,
            path,
        } = place;
        let open = (device.clone(), path.clone());
        let drop_rows = (device.clone(), path.clone());
        let drop_paths = (device, path.clone());
        div()
            .id(id)
            .role(gpui::Role::Link)
            .aria_label(label.clone())
            .h(px(26.0))
            .pl(px(8.0 + indent))
            .pr(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .when(current, |el| el.bg(crate::theme::wash(0.1)))
            .hover(|style| style.bg(crate::theme::wash(0.06)))
            .drag_over::<ExplorerDrag>(|style, _, _, _| style.bg(crate::theme::wash(0.16)))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(crate::theme::wash(0.16)))
            .on_click(cx.listener(move |page, _, window, cx| {
                let (device, path) = open.clone();
                let side = page.active;
                page.navigate(side, device, path.unwrap_or_else(|| "~".into()), None, cx);
                page.pane(side).focus.focus(window, cx);
            }))
            .on_drop(cx.listener(move |page, drag: &ExplorerDrag, _, cx| {
                if let (device, Some(dir)) = drop_rows.clone() {
                    page.drop_rows(device, dir, drag, cx);
                }
            }))
            .on_drop(cx.listener(move |page, paths: &ExternalPaths, _, cx| {
                if let (device, Some(dir)) = drop_paths.clone() {
                    page.drop_external(device, dir, paths, cx);
                }
            }))
            .child(icon(glyph).size(px(14.0)).text_color(theme.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.5))
                    .text_color(if current {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .child(label),
            )
            .when_some(path.filter(|_| indent > 0.0), |el, path| {
                el.tooltip(crate::settings::widgets::text_tooltip(path))
            })
    }

    fn render_preview(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let preview = self.preview.as_ref()?;
        let theme = theme.for_popup();
        Some(
            div()
                .id("explorer-preview")
                .debug_selector(|| "explorer-preview".into())
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.scrim().opacity(0.35))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|page, _, _, cx| {
                        page.close_preview(cx);
                    }),
                )
                .child(
                    div()
                        .id("explorer-preview-card")
                        .w(gpui::relative(0.72))
                        .h(gpui::relative(0.82))
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface_dialog)
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .h(px(34.0))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .text_color(theme.text)
                                        .child(preview.name.clone()),
                                )
                                .child(
                                    tool_button(
                                        "explorer-preview-close",
                                        "Close preview",
                                        icons::CLOSE,
                                        false,
                                        &theme,
                                    )
                                    .on_click(cx.listener(
                                        |page, _, _, cx| {
                                            page.close_preview(cx);
                                        },
                                    )),
                                ),
                        )
                        .child(div().flex_1().min_h_0().child(preview.surface.clone())),
                )
                .into_any_element(),
        )
    }
}

impl Render for ExplorerPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let toolbar = div()
            .h(px(40.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .text_size(crate::typography::ui_rems(15.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Files"),
            )
            .child(
                tool_button(
                    "explorer-hidden",
                    "Show hidden files",
                    if self.show_hidden {
                        icons::EYE
                    } else {
                        icons::EYE_CLOSED
                    },
                    self.show_hidden,
                    &theme,
                )
                .on_click(cx.listener(|page, _, _, cx| page.toggle_hidden(cx))),
            )
            .child(
                tool_button(
                    "explorer-split",
                    "Two panes",
                    icons::SPLIT_COLUMNS,
                    self.dual,
                    &theme,
                )
                .on_click(cx.listener(|page, _, window, cx| page.toggle_dual(window, cx))),
            );
        let panes: Vec<AnyElement> = self
            .sides()
            .iter()
            .map(|side| self.render_pane(*side, &theme, cx))
            .collect();
        div()
            .id("explorer")
            .debug_selector(|| "explorer".into())
            .relative()
            .size_full()
            .pt(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(12.0))
                    .pb(px(12.0))
                    .flex()
                    .gap(px(8.0))
                    .child(self.render_sidebar(&theme, cx))
                    .children(panes),
            )
            .child(self.tray.clone())
            .children(self.render_preview(&theme, cx))
            .children(self.render_menu(&theme, cx))
    }
}
