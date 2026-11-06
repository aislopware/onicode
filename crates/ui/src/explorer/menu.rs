//! A row's context menu: open, new folder, rename, trash, copy to another
//! device, reveal in Finder.

use gpui::prelude::*;
use gpui::{Context, KeyDownEvent, MouseButton, Pixels, Point, SharedString, Window, px};

use super::{ExplorerPage, Side};
use crate::icons::{self, icon};
use crate::popover;
use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Open,
    Preview,
    NewFolder,
    Rename,
    Trash,
    CopyTo { device_id: String, dir: String },
    Reveal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuItem {
    pub action: MenuAction,
    pub label: SharedString,
    pub icon: &'static str,
    pub separator_before: bool,
}

pub(crate) struct ExplorerMenu {
    pub side: Side,
    pub rows: Vec<usize>,
    pub items: Vec<MenuItem>,
    pub position: Point<Pixels>,
    pub active: usize,
}

fn item(action: MenuAction, label: impl Into<SharedString>, icon: &'static str) -> MenuItem {
    MenuItem {
        action,
        label: label.into(),
        icon,
        separator_before: false,
    }
}

impl ExplorerPage {
    /// What the menu offers for `rows` of a pane.
    pub(crate) fn menu_items(&self, side: Side, rows: &[usize]) -> Vec<MenuItem> {
        let pane = self.pane(side);
        let mut items = Vec::new();
        if let [only] = rows
            && let Some(row) = pane.rows.get(*only)
        {
            items.push(if row.is_folder() {
                item(MenuAction::Open, "Open", icons::FOLDER)
            } else {
                item(MenuAction::Preview, "Quick Look", icons::EYE)
            });
        }
        let mut new_folder = item(MenuAction::NewFolder, "New Folder", icons::ADD_CIRCLE);
        new_folder.separator_before = !items.is_empty();
        items.push(new_folder);
        if rows.len() == 1 {
            items.push(item(MenuAction::Rename, "Rename…", icons::PEN));
        }
        if !rows.is_empty() {
            items.push(item(
                MenuAction::Trash,
                "Move to Trash",
                icons::TRASH_BIN_MINIMALISTIC,
            ));
            let mut copies = Vec::new();
            if self.dual {
                let other = self.pane(side.other());
                if !other.device_id.is_empty()
                    && (other.device_id != pane.device_id || other.path != pane.path)
                {
                    copies.push(item(
                        MenuAction::CopyTo {
                            device_id: other.device_id.clone(),
                            dir: other.path.clone(),
                        },
                        format!("Copy to {}", super::format::file_name(&other.path)),
                        icons::COPY,
                    ));
                }
            }
            for device in self
                .devices
                .iter()
                .filter(|device| device.online && device.id != pane.device_id)
            {
                if let Some(home) = self.home(&device.id) {
                    copies.push(item(
                        MenuAction::CopyTo {
                            device_id: device.id.clone(),
                            dir: home.to_owned(),
                        },
                        format!("Copy to {}", device.name),
                        device.glyph,
                    ));
                }
            }
            if let Some(first) = copies.first_mut() {
                first.separator_before = true;
            }
            items.extend(copies);
            if Some(pane.device_id.as_str()) == self.local_device_id() {
                let mut reveal = item(
                    MenuAction::Reveal,
                    "Reveal in Finder",
                    icons::FOLDER_WITH_FILES,
                );
                reveal.separator_before = true;
                items.push(reveal);
            }
        }
        items
    }

    pub(crate) fn open_menu(
        &mut self,
        side: Side,
        row: Option<usize>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane(side).device_id.is_empty() {
            return;
        }
        let pane = self.pane_mut(side);
        // As in Finder, a right-click inside the selection keeps it.
        match row {
            Some(ix) if !pane.selected.contains(&ix) => pane.select_only(ix),
            Some(_) => {}
            None => {
                pane.clear_selection();
                pane.cursor = None;
            }
        }
        let rows = if row.is_some() {
            pane.action_rows()
        } else {
            Vec::new()
        };
        pane.focus.focus(window, cx);
        self.active = side;
        let items = self.menu_items(side, &rows);
        self.menu.open(ExplorerMenu {
            side,
            rows,
            items,
            position,
            active: 0,
        });
        cx.notify();
    }

    pub(crate) fn dispatch_menu(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.menu.as_open() else {
            return;
        };
        let side = menu.side;
        let rows = menu.rows.clone();
        let Some(action) = menu.items.get(index).map(|item| item.action.clone()) else {
            return;
        };
        self.close_menu(cx);
        match action {
            MenuAction::Open => {
                if let Some(ix) = rows.first() {
                    self.open_row(side, *ix, cx);
                }
            }
            MenuAction::Preview => {
                if let Some(ix) = rows.first() {
                    self.open_preview(side, *ix, cx);
                }
            }
            MenuAction::NewFolder => self.new_folder(side, cx),
            MenuAction::Rename => {
                if let Some(ix) = rows.first() {
                    self.begin_rename(side, *ix, window, cx);
                }
            }
            MenuAction::Trash => self.trash(side, &rows, cx),
            MenuAction::CopyTo { device_id, dir } => {
                let pane = self.pane(side);
                let from = pane.device_id.clone();
                let paths = rows
                    .iter()
                    .filter_map(|ix| pane.rows.get(*ix))
                    .map(|row| row.path.clone())
                    .collect();
                self.start_transfer(from, paths, device_id, dir, cx);
            }
            MenuAction::Reveal => self.reveal(side, &rows, cx),
        }
    }

    /// Arrow keys, Enter and Escape while the menu is open. Returns whether it took the key.
    pub(crate) fn menu_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(menu) = self.menu.as_open() else {
            return false;
        };
        let count = menu.items.len();
        let active = menu.active;
        match popover::classify_key(
            &event.keystroke.key,
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            popover::MenuKey::Escape => self.close_menu(cx),
            key @ (popover::MenuKey::Up | popover::MenuKey::Down) => {
                let delta = if matches!(key, popover::MenuKey::Up) {
                    -1
                } else {
                    1
                };
                if let (Some(next), Some(menu)) = (
                    popover::menu_step(Some(active), count, delta),
                    self.menu.open_mut(),
                ) {
                    menu.active = next;
                    cx.notify();
                }
            }
            popover::MenuKey::Enter => self.dispatch_menu(active, window, cx),
            _ => {}
        }
        window.prevent_default();
        cx.stop_propagation();
        true
    }

    pub(crate) fn render_menu(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.menu.get()?;
        let theme = theme.for_popup();
        let mut card = popover::popover_card(&theme)
            .w(px(220.0))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|page, _, _, cx| page.close_menu(cx)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (index, entry) in menu.items.iter().enumerate() {
            if entry.separator_before {
                card = card.child(popover::menu_separator());
            }
            let danger = entry.action == MenuAction::Trash;
            let key = format!("explorer-menu-{index}");
            card = card.child(
                popover::menu_row(&theme, menu.active == index, key.clone())
                    .id(SharedString::from(key.clone()))
                    .debug_selector(move || key.clone())
                    .role(gpui::Role::MenuItem)
                    .aria_label(entry.label.clone())
                    .when(danger, |row| row.text_color(theme.danger))
                    .on_click(cx.listener(move |page, _, window, cx| {
                        page.dispatch_menu(index, window, cx);
                    }))
                    .child(icon(entry.icon).size(px(16.0)).text_color(if danger {
                        theme.danger
                    } else {
                        theme.text_muted
                    }))
                    .child(entry.label.clone()),
            );
        }
        Some(popover::menu_at(
            "explorer-context-menu",
            menu.position,
            card.into_any_element(),
            self.menu.closing_since(),
        ))
    }
}
