//! Files across machines: an explorer over every peer's filesystem, with drag
//! and drop between peers and to and from Finder
//! (docs/onicode/decisions/one-machine.md).
//!
//! Bytes never pass through here. A drop asks this Mac's engine to start a
//! transfer; the destination engine pulls from the source and reports progress
//! on `WatchTransfers`, which the tray shows.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Local, Utc};
use gpui::prelude::*;
use gpui::{
    Context, Entity, ExternalPaths, Focusable as _, SharedString, Subscription, Task, Window, div,
    px,
};
use gpui_base::input::{InputEvent, InputState};
use onicode_proto::files::{
    ConflictPolicy, PathOp, PathRoots, StartTransferRequest, TransferFrom, TransferTo,
};

use crate::files::{FilesEditorSettings, FilesSurface};
use crate::icons;
use crate::popover::{self, Popup};
use crate::state::AppState;

mod client;
mod format;
mod menu;
mod pane;
mod promise;
#[cfg(test)]
mod tests;
mod tray;
mod view;

use client::ExplorerClient;
use pane::Pane;
use tray::TransferTray;

/// One of the two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Left,
    Right,
}

impl Side {
    pub(crate) fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// One dragged row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DragItem {
    pub path: String,
    pub name: SharedString,
    pub is_dir: bool,
}

/// Rows dragged out of a pane: where they live and what they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExplorerDrag {
    pub device_id: String,
    pub items: Arc<[DragItem]>,
}

/// What follows the pointer while rows are dragged inside the window.
pub(crate) struct DragGhost {
    label: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::Theme::of(cx).for_popup();
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .shadow(crate::theme::card_selected_shadows())
            .text_size(px(11.5))
            .text_color(theme.text)
            .child(self.label.clone())
    }
}

/// A device the explorer can browse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceEntry {
    pub id: String,
    pub name: SharedString,
    pub glyph: &'static str,
    pub online: bool,
    pub local: bool,
}

/// A place in the sidebar under a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootEntry {
    pub label: SharedString,
    pub path: String,
    pub icon: &'static str,
}

pub(crate) enum Roots {
    Loading,
    Ready {
        home: String,
        entries: Vec<RootEntry>,
    },
    Failed,
}

fn root_entries(roots: PathRoots) -> Roots {
    let mut entries = vec![RootEntry {
        label: "Home".into(),
        path: roots.home.clone(),
        icon: icons::HOME,
    }];
    entries.extend(roots.volumes.into_iter().map(|volume| RootEntry {
        label: volume.name.into(),
        path: volume.path,
        icon: icons::HARD_DRIVE,
    }));
    entries.extend(roots.recent.into_iter().take(8).map(|path| RootEntry {
        label: format::file_name(&path).to_owned().into(),
        path,
        icon: icons::FOLDER,
    }));
    Roots::Ready {
        home: roots.home,
        entries,
    }
}

struct Rename {
    side: Side,
    path: String,
    input: Entity<InputState>,
    _events: Subscription,
}

struct Preview {
    path: String,
    name: SharedString,
    surface: Entity<FilesSurface>,
}

/// The Files page.
pub struct ExplorerPage {
    state: Entity<AppState>,
    devices: Vec<DeviceEntry>,
    engine_ready: bool,
    roots: HashMap<String, Roots>,
    root_tasks: HashMap<String, Task<()>>,
    left: Pane,
    right: Pane,
    active: Side,
    dual: bool,
    show_hidden: bool,
    tray: Entity<TransferTray>,
    menu: Popup<menu::ExplorerMenu>,
    rename: Option<Rename>,
    preview: Option<Preview>,
    /// Set by a row's mouse-down so the pane under it keeps the selection.
    row_pressed: bool,
    _observe: Subscription,
}

impl ExplorerPage {
    /// The page over `state`'s devices; it lists folders once this Mac's engine is up.
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let tray = cx.new(|_| TransferTray::new(state.clone()));
        let observe = cx.observe(&state, |page: &mut Self, _, cx| page.sync_devices(cx));
        let mut page = Self {
            state,
            devices: Vec::new(),
            engine_ready: false,
            roots: HashMap::new(),
            root_tasks: HashMap::new(),
            left: Pane::new(String::new(), "~".into(), cx.focus_handle()),
            right: Pane::new(String::new(), "~".into(), cx.focus_handle()),
            active: Side::Left,
            dual: false,
            show_hidden: false,
            tray,
            menu: Popup::default(),
            rename: None,
            preview: None,
            row_pressed: false,
            _observe: observe,
        };
        page.sync_devices(cx);
        page
    }

    pub(crate) fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub(crate) fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    /// The panes on screen, left first.
    pub(crate) fn sides(&self) -> &'static [Side] {
        if self.dual {
            &[Side::Left, Side::Right]
        } else {
            &[Side::Left]
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<ExplorerClient> {
        let state = self.state.read(cx);
        Some(ExplorerClient::new(
            state.engine()?.clone(),
            state.local_device_id.clone(),
        ))
    }

    pub(crate) fn device(&self, id: &str) -> Option<&DeviceEntry> {
        self.devices.iter().find(|device| device.id == id)
    }

    pub(crate) fn local_device_id(&self) -> Option<&str> {
        self.devices
            .iter()
            .find(|device| device.local)
            .map(|device| device.id.as_str())
    }

    /// Takes in the device list and starts what waits on it: panes waiting for
    /// this Mac, sidebar places of devices that came online, transfer watches.
    fn sync_devices(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let now = Utc::now();
        let local = state.local_device_id.clone();
        let engine_ready = state.engine().is_some();
        let mut devices: Vec<DeviceEntry> = local
            .iter()
            .map(|id| DeviceEntry {
                id: id.clone(),
                name: "This Mac".into(),
                glyph: icons::LAPTOP,
                online: true,
                local: true,
            })
            .collect();
        let mut others: Vec<DeviceEntry> = state
            .devices
            .iter()
            // Phones and browsers are clients, not engines with files to browse.
            .filter(|device| {
                Some(&device.id) != local.as_ref()
                    && !matches!(device.platform.as_str(), "ios" | "android" | "web")
            })
            .map(|device| DeviceEntry {
                id: device.id.clone(),
                name: device.name.clone().into(),
                glyph: crate::settings::devices::device_glyph(&device.platform),
                online: state.device_online(&device.id, now),
                local: false,
            })
            .collect();
        others.sort_by(|a, b| a.name.cmp(&b.name));
        devices.extend(others);
        if devices == self.devices && engine_ready == self.engine_ready {
            return;
        }
        self.devices = devices;
        self.engine_ready = engine_ready;
        if !engine_ready {
            cx.notify();
            return;
        }
        if let Some(local) = local {
            for side in [Side::Left, Side::Right] {
                if self.pane(side).device_id.is_empty() {
                    self.navigate(side, local.clone(), "~".into(), None, cx);
                }
            }
        }
        let online: Vec<String> = self
            .devices
            .iter()
            .filter(|device| device.online)
            .map(|device| device.id.clone())
            .collect();
        for id in &online {
            if !self.roots.contains_key(id) {
                self.load_roots(id.clone(), cx);
            }
        }
        self.tray
            .update(cx, |tray, cx| tray.sync_watches(&online, cx));
        cx.notify();
    }

    fn load_roots(&mut self, device: String, cx: &mut Context<Self>) {
        let Some(client) = self.client(cx) else {
            return;
        };
        self.roots.insert(device.clone(), Roots::Loading);
        let key = device.clone();
        let task = cx.spawn(async move |page, cx| {
            let roots = match client.path_roots(&device).await {
                Ok(roots) => root_entries(roots),
                Err(error) => {
                    tracing::debug!(%error, %device, "path roots unavailable");
                    Roots::Failed
                }
            };
            page.update(cx, |page, cx| {
                page.roots.insert(device, roots);
                cx.notify();
            })
            .ok();
        });
        self.root_tasks.insert(key, task);
    }

    /// The home folder of `device`, once its places are known.
    pub(crate) fn home(&self, device: &str) -> Option<&str> {
        match self.roots.get(device) {
            Some(Roots::Ready { home, .. }) => Some(home),
            _ => None,
        }
    }

    /// Shows `path` on `device` in a pane. `came_from` puts the cursor on that row.
    pub(crate) fn navigate(
        &mut self,
        side: Side,
        device: String,
        path: String,
        came_from: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.preview = None;
        self.rename = None;
        let generation = self
            .pane_mut(side)
            .begin_navigation(device.clone(), path.clone());
        let Some(client) = self.client(cx) else {
            self.pane_mut(side)
                .fail("This Mac's engine is still starting.");
            cx.notify();
            return;
        };
        let show_hidden = self.show_hidden;
        let task = cx.spawn(async move |page, cx| {
            let result = client.list_path(&device, &path, show_hidden).await;
            page.update(cx, |page, cx| {
                let pane = page.pane_mut(side);
                if pane.generation != generation {
                    return;
                }
                match result {
                    Ok(listing) => pane.apply_listing(&listing, came_from.as_deref(), Local::now()),
                    Err(error) => pane.fail(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        });
        self.pane_mut(side)._load = Some(task);
        cx.notify();
    }

    pub(crate) fn reload(&mut self, side: Side, cx: &mut Context<Self>) {
        let pane = self.pane(side);
        let (device, path) = (pane.device_id.clone(), pane.path.clone());
        self.navigate(side, device, path, None, cx);
    }

    /// Reloads every pane showing `dir` on `device`.
    fn reload_showing(&mut self, device: &str, dir: &str, cx: &mut Context<Self>) {
        for side in [Side::Left, Side::Right] {
            let pane = self.pane(side);
            if pane.device_id == device && pane.path == dir {
                self.reload(side, cx);
            }
        }
    }

    pub(crate) fn go_up(&mut self, side: Side, cx: &mut Context<Self>) {
        let pane = self.pane(side);
        let Some(parent) = pane.parent.clone() else {
            return;
        };
        let (device, from) = (pane.device_id.clone(), pane.path.clone());
        self.navigate(side, device, parent, Some(from), cx);
    }

    /// Enter on a row: into a folder, or a preview of anything else.
    pub(crate) fn open_row(&mut self, side: Side, ix: usize, cx: &mut Context<Self>) {
        let pane = self.pane(side);
        let Some(row) = pane.rows.get(ix) else {
            return;
        };
        if row.is_folder() {
            let (device, path) = (pane.device_id.clone(), row.path.clone());
            self.navigate(side, device, path, None, cx);
        } else {
            self.open_preview(side, ix, cx);
        }
    }

    pub(crate) fn set_active(&mut self, side: Side, cx: &mut Context<Self>) {
        if self.active != side {
            self.active = side;
            cx.notify();
        }
    }

    pub(crate) fn toggle_dual(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dual = !self.dual;
        if self.dual {
            // The new pane starts where the first one is.
            let (device, path) = (self.left.device_id.clone(), self.left.path.clone());
            if !device.is_empty() {
                self.navigate(Side::Right, device, path, None, cx);
            }
        } else if self.active == Side::Right {
            self.active = Side::Left;
            self.left.focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        for side in self.sides() {
            self.reload(*side, cx);
        }
    }

    /// The rows a drag from `ix` carries: the selection when `ix` is in it.
    pub(crate) fn drag_for(&self, side: Side, ix: usize) -> ExplorerDrag {
        let pane = self.pane(side);
        let rows = if pane.selected.contains(&ix) {
            pane.selected.iter().copied().collect::<Vec<_>>()
        } else {
            vec![ix]
        };
        ExplorerDrag {
            device_id: pane.device_id.clone(),
            items: rows
                .into_iter()
                .filter_map(|ix| pane.rows.get(ix))
                .map(|row| DragItem {
                    path: row.path.clone(),
                    name: row.name.clone(),
                    is_dir: row.is_tree(),
                })
                .collect(),
        }
    }

    /// Copies `paths` on `from` into `dir` on `to`.
    pub(crate) fn start_transfer(
        &mut self,
        from: String,
        paths: Vec<String>,
        to: String,
        dir: String,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() || from.is_empty() || to.is_empty() {
            return;
        }
        // Dropped back where it already is.
        if from == to
            && paths
                .iter()
                .all(|path| format::parent(path) == Some(dir.as_str()))
        {
            return;
        }
        let Some(client) = self.client(cx) else {
            crate::toast::error(cx, "This Mac's engine is still starting.");
            return;
        };
        self.tray.update(cx, |tray, cx| tray.watch(to.clone(), cx));
        let request = StartTransferRequest {
            from: TransferFrom {
                device_id: from,
                paths,
            },
            to: TransferTo { device_id: to, dir },
            conflict: ConflictPolicy::Ask,
        };
        cx.spawn(async move |_, cx| {
            if let Err(error) = client.start_transfer(&request).await {
                cx.update(|cx| crate::toast::error(cx, format!("Copy failed: {error:#}")));
            }
        })
        .detach();
    }

    /// Rows dropped on `dir` of `device`.
    pub(crate) fn drop_rows(
        &mut self,
        device: String,
        dir: String,
        drag: &ExplorerDrag,
        cx: &mut Context<Self>,
    ) {
        let paths = drag.items.iter().map(|item| item.path.clone()).collect();
        self.start_transfer(drag.device_id.clone(), paths, device, dir, cx);
    }

    /// Finder's files dropped on `dir` of `device`: the destination pulls them from this Mac.
    pub(crate) fn drop_external(
        &mut self,
        device: String,
        dir: String,
        paths: &ExternalPaths,
        cx: &mut Context<Self>,
    ) {
        let Some(local) = self.local_device_id().map(str::to_owned) else {
            return;
        };
        let paths = paths
            .paths()
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        self.start_transfer(local, paths, device, dir, cx);
    }

    /// Runs one `PathMutate` per path, then reloads the folder.
    fn mutate(
        &mut self,
        side: Side,
        op: PathOp,
        changes: Vec<(String, Option<String>)>,
        select: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client(cx) else {
            return;
        };
        let pane = self.pane(side);
        let (device, dir) = (pane.device_id.clone(), pane.path.clone());
        cx.spawn(async move |page, cx| {
            let mut failure = None;
            for (path, to) in changes {
                if let Err(error) = client.mutate(&device, op, path, to).await {
                    failure = Some(error);
                    break;
                }
            }
            page.update(cx, |page, cx| {
                if let Some(error) = failure {
                    crate::toast::error(cx, format!("{error:#}"));
                }
                if let Some(select) = select {
                    let pane = page.pane(side);
                    if pane.device_id == device && pane.path == dir {
                        page.navigate(side, device.clone(), dir.clone(), Some(select), cx);
                        return;
                    }
                }
                page.reload_showing(&device, &dir, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn new_folder(&mut self, side: Side, cx: &mut Context<Self>) {
        let pane = self.pane(side);
        if pane.device_id.is_empty() {
            return;
        }
        let taken = |name: &str| pane.rows.iter().any(|row| row.name.as_ref() == name);
        let name = std::iter::once("untitled folder".to_string())
            .chain((2..).map(|n| format!("untitled folder {n}")))
            .find(|name| !taken(name))
            .unwrap_or_else(|| "untitled folder".to_string());
        let path = format::join(&pane.path, &name);
        self.mutate(
            side,
            PathOp::Mkdir,
            vec![(path.clone(), None)],
            Some(path),
            cx,
        );
    }

    pub(crate) fn trash(&mut self, side: Side, rows: &[usize], cx: &mut Context<Self>) {
        let pane = self.pane(side);
        let changes: Vec<(String, Option<String>)> = rows
            .iter()
            .filter_map(|ix| pane.rows.get(*ix))
            .map(|row| (row.path.clone(), None))
            .collect();
        if !changes.is_empty() {
            self.mutate(side, PathOp::Trash, changes, None, cx);
        }
    }

    pub(crate) fn begin_rename(
        &mut self,
        side: Side,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.pane(side).rows.get(ix) else {
            return;
        };
        let name = row.name.to_string();
        let path = row.path.clone();
        let stem = if row.is_folder() {
            name.len()
        } else {
            name.rfind('.').filter(|dot| *dot > 0).unwrap_or(name.len())
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        input.update(cx, |input, cx| input.set_selected_range(0..stem, cx));
        let events = cx.subscribe_in(&input, window, |page, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => page.submit_rename(window, cx),
            InputEvent::Blur if page.rename.take().is_some() => cx.notify(),
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        self.rename = Some(Rename {
            side,
            path,
            input,
            _events: events,
        });
        cx.notify();
    }

    fn submit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        let name = rename.input.read(cx).value().trim().to_string();
        self.pane(rename.side).focus.focus(window, cx);
        cx.notify();
        if name.is_empty()
            || name == format::file_name(&rename.path)
            || name.contains(['/', '\0'])
            || name == "."
            || name == ".."
        {
            return;
        }
        let dir = format::parent(&rename.path).unwrap_or("/").to_owned();
        let renamed = format::join(&dir, &name);
        self.mutate(
            rename.side,
            PathOp::Rename,
            vec![(rename.path, Some(name))],
            Some(renamed),
            cx,
        );
    }

    pub(crate) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(rename) = self.rename.take() {
            self.pane(rename.side).focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn reveal(&mut self, side: Side, rows: &[usize], cx: &mut Context<Self>) {
        let pane = self.pane(side);
        if Some(pane.device_id.as_str()) != self.local_device_id() {
            return;
        }
        if let Some(row) = rows.first().and_then(|ix| pane.rows.get(*ix)) {
            cx.reveal_path(std::path::Path::new(&row.path));
        }
    }

    /// Space: shows a file with the files preview. The preview reads through a
    /// chat on the file's device, as link-opened host files do.
    pub(crate) fn open_preview(&mut self, side: Side, ix: usize, cx: &mut Context<Self>) {
        let pane = self.pane(side);
        let Some(row) = pane.rows.get(ix).filter(|row| !row.is_tree()) else {
            self.preview = None;
            cx.notify();
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.path == row.path)
        {
            return;
        }
        let device = pane.device_id.clone();
        let (path, name) = (row.path.clone(), row.name.clone());
        let chat_id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.device_id == device && !chat.archived)
            .map(|chat| chat.id.clone());
        let Some(chat_id) = chat_id else {
            let device_name = self
                .device(&device)
                .map_or_else(|| SharedString::from("that device"), |d| d.name.clone());
            crate::toast::show(
                cx,
                crate::toast::ToastKind::Info,
                format!("Preview reads through a chat on {device_name}; start one there first."),
            );
            return;
        };
        let state = self.state.clone();
        let surface = cx.new(|cx| {
            let mut surface = FilesSurface::new_editor(
                state,
                chat_id,
                path.clone(),
                FilesEditorSettings {
                    autosave_enabled: false,
                    autosave_delay_ms: 1000,
                    editor_font_size: 13.0,
                    word_wrap: true,
                    show_all_files: false,
                },
                cx,
            );
            surface.ensure_loaded(cx);
            surface
        });
        self.preview = Some(Preview {
            path,
            name,
            surface,
        });
        cx.notify();
    }

    pub(crate) fn close_preview(&mut self, cx: &mut Context<Self>) -> bool {
        if self.preview.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Keeps an open preview on the cursor as it moves, as Quick Look does.
    fn follow_preview(&mut self, side: Side, cx: &mut Context<Self>) {
        if self.preview.is_some()
            && let Some(cursor) = self.pane(side).cursor
        {
            self.open_preview(side, cursor, cx);
        }
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.begin_close() {
            popover::reap_popup(cx, |page: &mut Self| &mut page.menu);
            cx.notify();
        }
    }
}
