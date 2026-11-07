//! FileTree.tsx: the Explorer header, project root row and lazily listed
//! tree with Git colours, inline create/rename, clipboard, context menu,
//! dropped files and VS Code-style keyboard navigation. Every folder shown
//! is fed by the engine's `WatchDir`, and every file operation is an engine
//! call whose effect shows in the frame it is made, as an echo over the
//! listings until the folder's listing reaches the scan its reply names.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, KeyDownEvent, ListAlignment, ListState,
    MouseButton, MouseDownEvent, Point, PromptLevel, Role, SharedString, StyleRefinement,
    Subscription, Task, Window, actions, div, list, px, rgb,
};
use onicode_core::files;
use onicode_protocol::OpId;
use onicode_protocol::files::{
    Bytes, Create, DeletePath, Destination, DirEntry, DirEvent, Entries, ImportEntry, ImportFiles,
    Listing, Move, NameIssueKind, NewPath, Relisted, Severity, WatchDir,
};

use crate::engine_client::{self, Engine, WatchUpdate, Watching};
use crate::file_icons::file_type_icon;
use crate::git_status::{GitChanged, ProjectGit};
use crate::input::{self, Input};
use crate::popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover};
use crate::project_search::{ProjectSearch, SearchEvent};
use crate::right_panel::*;
use crate::theme::*;

const MOD: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+"
};
const SHIFT: &str = if cfg!(target_os = "macos") {
    "⇧"
} else {
    "Shift+"
};
const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else {
    "Open Containing Folder"
};
const ROW: f32 = 30.;
const ENTRY_FONT_SIZE: f32 = 13.;

/// A reference to an existing file, distinct from files copied into the tree.
#[derive(Clone)]
pub(crate) struct DraggedFile(pub PathBuf);

impl Render for DraggedFile {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .0
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(stroke())
            .bg(pane_fill())
            .text_size(px(12.))
            .text_color(content(1.))
            .child(file_type_icon(&name, false, false, 16.))
            .child(div().max_w(px(240.)).truncate().child(name))
    }
}

actions!(
    explorer,
    [
        Down,
        Up,
        PageDown,
        PageUp,
        First,
        Last,
        Expand,
        Collapse,
        Activate,
        OpenSelected,
        Rename,
        Delete,
        ClearCut,
        CopyEntry,
        CutEntry,
        PasteEntry,
        CopyPath,
        CancelName
    ]
);

pub(crate) fn bind(cx: &mut App) {
    let c = Some("Explorer");
    cx.bind_keys([
        KeyBinding::new("down", Down, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("pagedown", PageDown, c),
        KeyBinding::new("pageup", PageUp, c),
        KeyBinding::new("home", First, c),
        KeyBinding::new("end", Last, c),
        KeyBinding::new("right", Expand, c),
        KeyBinding::new("left", Collapse, c),
        KeyBinding::new("enter", Activate, c),
        KeyBinding::new("cmd-down", OpenSelected, c),
        KeyBinding::new("f2", Rename, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("backspace", Delete, c),
        KeyBinding::new("escape", ClearCut, c),
        KeyBinding::new(os_keys!("cmd-c", "ctrl-c"), CopyEntry, c),
        KeyBinding::new(os_keys!("cmd-x", "ctrl-x"), CutEntry, c),
        KeyBinding::new(os_keys!("cmd-v", "ctrl-v"), PasteEntry, c),
        KeyBinding::new(os_keys!("cmd-shift-c", "ctrl-shift-c"), CopyPath, c),
        KeyBinding::new("escape", CancelName, Some("ExplorerName")),
    ]);
}

#[derive(Clone, PartialEq)]
struct Clip {
    cut: bool,
    path: String,
    is_dir: bool,
}
#[derive(Clone)]
struct Target {
    path: String,
    is_dir: bool,
    root: bool,
}
#[derive(Clone, PartialEq)]
enum NameMode {
    Create {
        id: u64,
        parent: String,
        is_dir: bool,
    },
    Rename {
        path: String,
        is_dir: bool,
    },
}
#[derive(Clone, PartialEq)]
enum Line {
    Note {
        depth: usize,
        text: String,
    },
    Entry {
        entry: DirEntry,
        depth: usize,
        open: bool,
    },
    Name {
        depth: usize,
        is_dir: bool,
    },
    Issue {
        depth: usize,
        text: Vec<(String, bool)>,
        error: bool,
    },
}
struct OpenMenu {
    target: Target,
    anchor: Point<gpui::Pixels>,
    menu: Entity<Menu>,
    _events: Subscription,
}
#[derive(Default, Clone)]
struct Saved {
    expanded: HashSet<String>,
    selected: Option<String>,
}

/// A file change made here, shown in its folder before the engine's listing
/// shows it.
#[derive(Clone, Debug, PartialEq)]
enum Echo {
    Add(DirEntry),
    Remove(String),
}

impl Echo {
    fn folder(&self) -> String {
        match self {
            Self::Add(entry) => parent_of(&entry.path),
            Self::Remove(path) => parent_of(path),
        }
    }

    /// Whether `entries` show this change already.
    fn shown(&self, entries: &[DirEntry]) -> bool {
        match self {
            Self::Add(entry) => entries.iter().any(|e| e.name == entry.name),
            Self::Remove(path) => entries.iter().all(|e| &e.path != path),
        }
    }

    /// `entries` as they read once this change is made, in `files::list_dir`'s
    /// order.
    fn apply(&self, entries: &mut Vec<DirEntry>) {
        match self {
            Self::Add(entry) => {
                if !self.shown(entries) {
                    let at = entries.partition_point(|e| {
                        entry
                            .is_dir
                            .cmp(&e.is_dir)
                            .then_with(|| files::compare_natural_names(&e.name, &entry.name))
                            .is_lt()
                    });
                    entries.insert(at, entry.clone());
                }
            }
            Self::Remove(path) => entries.retain(|e| &e.path != path),
        }
    }
}

/// An echo and the call it stands for.
struct Pending {
    op: OpId,
    echo: Echo,
}

/// An applied call's echo, until its folder's listing reaches `until`: the
/// scan of the listing that covers the call.
struct Settled {
    echo: Echo,
    until: u64,
}

/// How a call the explorer echoed came back.
enum Landed<'a> {
    Failed,
    /// Applied: the listings that cover it, and the path it made, which a
    /// copy's echo takes when the engine named it otherwise.
    Applied {
        listed: &'a [Relisted],
        created: Option<&'a str>,
    },
}

/// What a file operation's call came back with: the path it made, if any,
/// and the listings that cover it.
type Ran = (Option<String>, Vec<Relisted>);

pub(crate) struct Explorer {
    git: Entity<ProjectGit>,
    cwd: String,
    enabled: bool,
    show_excluded: bool,
    expanded: HashSet<String>,
    selected: Option<String>,
    /// loadExpanded / loadSelected: remembered per project for the session.
    saved: HashMap<String, Saved>,
    /// listCachedDir: what each folder shows, its listing with the echoes
    /// over it, or its error.
    dirs: HashMap<String, Result<Vec<DirEntry>, String>>,
    /// The engine's last listing of each folder, kept after its watch goes.
    listed: HashMap<String, Result<Vec<DirEntry>, String>>,
    /// The scan each folder's listing was read at.
    scans: HashMap<String, u64>,
    /// The engine's watch of every folder the tree shows, while Files is.
    watches: HashMap<String, (Watching<Listing, DirEvent>, Task<()>)>,
    /// Calls not replied to yet, oldest first.
    echoes: Vec<Pending>,
    /// Echoes of applied calls, until their folder's covering listing.
    settled: Vec<Settled>,
    naming: Option<NameMode>,
    name_input: Entity<Input>,
    name_attempted: bool,
    name_busy: bool,
    name_error: Option<String>,
    clip: Option<Clip>,
    menu: Option<OpenMenu>,
    search: Option<Entity<ProjectSearch>>,
    search_events: Option<Subscription>,
    drag_over: Option<String>,
    op_error: Option<String>,
    lines: Vec<Line>,
    list: ListState,
    focus: FocusHandle,
    keyboard: bool,
    typeahead: (String, Option<Instant>),
    next_id: u64,
    _subscriptions: Vec<Subscription>,
}

fn parent_of(path: &str) -> String {
    files::parent_path(path)
}
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Shows `path` in Finder on the viewer's own Mac, or says why it cannot.
pub(crate) fn reveal_path(path: &Path, cx: &App) -> Result<(), String> {
    on_this_mac(path)?;
    cx.reveal_path(path);
    Ok(())
}

/// Opens `path` with its registered app on the viewer's own Mac, or says why it cannot.
pub(crate) fn open_with_default_app(path: &Path, cx: &App) -> Result<(), String> {
    on_this_mac(path)?;
    cx.open_with_system(path);
    Ok(())
}

/// A file that is gone, or one on another engine, has nothing to show here.
fn on_this_mac(path: &Path) -> Result<(), String> {
    if path
        .to_str()
        .is_some_and(|path| onicode_protocol::ProjectRef::parse_uri(path).is_some())
    {
        return Err("Files on another machine stay on it.".into());
    }
    if !path.exists() {
        return Err(format!("{}: No such file or directory", path.display()));
    }
    Ok(())
}

/// How much one Finder item copied into a project on another engine may hold: what one call
/// carries over the tailnet (64 MB a frame), less the third base64 adds.
const IMPORT_LIMIT: u64 = 40 << 20;

/// `from`, on this Mac, read whole as an [`ImportFiles`] item: the item itself, then each
/// folder before what it holds. Refused past [`IMPORT_LIMIT`].
fn import_entries(from: &Path) -> Result<Vec<ImportEntry>, String> {
    fn walk(
        at: &Path,
        relative: String,
        entries: &mut Vec<ImportEntry>,
        total: &mut u64,
    ) -> Result<(), String> {
        let meta = std::fs::metadata(at).map_err(|error| format!("{}: {error}", at.display()))?;
        if !meta.is_dir() {
            *total = total.saturating_add(meta.len());
            if *total > IMPORT_LIMIT {
                return Err("Too large to copy to another machine (over 40 MB).".into());
            }
            let bytes = std::fs::read(at).map_err(|error| format!("{}: {error}", at.display()))?;
            entries.push(ImportEntry {
                path: relative,
                bytes: Some(Bytes(bytes)),
            });
            return Ok(());
        }
        entries.push(ImportEntry {
            path: relative.clone(),
            bytes: None,
        });
        let mut children: Vec<_> = std::fs::read_dir(at)
            .map_err(|error| format!("{}: {error}", at.display()))?
            .filter_map(Result::ok)
            .collect();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let name = child.file_name().to_string_lossy().into_owned();
            let under = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            walk(&child.path(), under, entries, total)?;
        }
        Ok(())
    }
    let mut entries = Vec::new();
    walk(from, String::new(), &mut entries, &mut 0)?;
    Ok(entries)
}

/// `listed`, as `engine` named its folders, in the app's spelling.
fn respell_listed(engine: &Engine, listed: Vec<Relisted>) -> Vec<Relisted> {
    if engine.target().is_none() {
        return listed;
    }
    listed
        .into_iter()
        .map(|relisted| Relisted {
            root: engine.app_path(&relisted.root),
            path: engine.app_path(&relisted.path),
            scan: relisted.scan,
        })
        .collect()
}

/// `moved`, as `engine` named its paths, in the app's spelling.
fn respell_new_path(engine: &Engine, moved: NewPath) -> NewPath {
    NewPath {
        path: engine.app_path(&moved.path),
        listed: respell_listed(engine, moved.listed),
    }
}

/// `listing`, as `engine` named its entries, in the app's spelling.
fn respell_listing(engine: &Engine, mut listing: Listing) -> Listing {
    if engine.target().is_some()
        && let Entries::Read(entries) = &mut listing.entries
    {
        for entry in entries {
            entry.path = engine.app_path(&entry.path);
        }
    }
    listing
}

/// How a create, rename or move came back.
fn landed(result: &Result<NewPath, String>) -> Landed<'_> {
    match result {
        Ok(moved) => Landed::Applied {
            listed: &moved.listed,
            created: Some(&moved.path),
        },
        Err(_) => Landed::Failed,
    }
}

pub(crate) struct OpenFile(pub String);
impl EventEmitter<OpenFile> for Explorer {}
/// Shared file-action toast state. Name validation remains local to its field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileActionError(pub Option<String>);
impl EventEmitter<FileActionError> for Explorer {}
pub(crate) struct OpenSearchMatch {
    pub cwd: String,
    pub matched: onicode_protocol::search::project::SearchMatch,
    pub pin: bool,
}
impl EventEmitter<OpenSearchMatch> for Explorer {}

impl Explorer {
    pub(crate) fn new(
        git: Entity<ProjectGit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| {
            let mut input = Input::field(
                "Type file name. Press Enter to confirm or Escape to cancel.",
                "",
                content(0.35),
                cx,
            );
            input.set_font_size(ENTRY_FONT_SIZE);
            input
        });
        let name_focus = name_input.read(cx).focus_handle(cx);
        let subscriptions = vec![
            cx.subscribe(&git, |this, _, _: &GitChanged, cx| this.git_changed(cx)),
            cx.observe(&git, |this, _, cx| {
                // Git colours follow every status read.
                if this.enabled {
                    cx.notify();
                }
                let _ = this;
            }),
            cx.subscribe_in(
                &name_input,
                window,
                |this, _, _: &input::Submitted, window, cx| this.finish_name(true, window, cx),
            ),
            cx.subscribe(&name_input, |this, _, _: &input::Changed, cx| {
                this.name_error = None;
                this.rebuild(cx);
            }),
            cx.on_blur(&name_focus, window, |this, window, cx| {
                if this.naming.is_some() && !this.name_busy {
                    let ok = this.issue(cx).is_none_or(|i| i.severity != Severity::Error);
                    this.finish_name(ok, window, cx);
                }
            }),
        ];
        Self {
            git,
            cwd: String::new(),
            enabled: false,
            show_excluded: false,
            expanded: HashSet::new(),
            selected: None,
            saved: HashMap::new(),
            dirs: HashMap::new(),
            listed: HashMap::new(),
            scans: HashMap::new(),
            watches: HashMap::new(),
            echoes: Vec::new(),
            settled: Vec::new(),
            naming: None,
            name_input,
            name_attempted: false,
            name_busy: false,
            name_error: None,
            clip: None,
            menu: None,
            search: None,
            search_events: None,
            drag_over: None,
            op_error: None,
            lines: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(300.)),
            focus: cx.focus_handle(),
            keyboard: false,
            typeahead: (String::new(), None),
            next_id: 0,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn set_show_excluded(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_excluded != show {
            self.show_excluded = show;
            self.rebuild(cx);
        }
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let was = self.enabled;
        self.enabled = enabled;
        if enabled {
            if !self.sync_project(cx) && !was {
                self.rebuild(cx);
            }
        } else {
            self.watches.clear();
        }
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.set_active(enabled, cx));
        }
    }

    pub(crate) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_project(cx);
        self.menu = None;
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.focus(window, cx));
            return;
        }
        let search = cx.new(|cx| ProjectSearch::new(self.cwd.clone(), self.enabled, cx));
        self.search_events = Some(cx.subscribe_in(
            &search,
            window,
            |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Close => {
                    this.close_search(window, cx);
                }
                SearchEvent::Open { cwd, matched, pin } => cx.emit(OpenSearchMatch {
                    cwd: cwd.clone(),
                    matched: matched.clone(),
                    pin: *pin,
                }),
            },
        ));
        search.update(cx, |search, cx| search.focus(window, cx));
        self.search = Some(search);
        cx.notify();
    }

    /// Escape also reaches this from the editor after opening a search result.
    pub(crate) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(search) = &self.search else {
            return false;
        };
        if search.read(cx).is_composing(cx) {
            return true;
        }
        self.search = None;
        self.search_events = None;
        window.focus(&self.focus, cx);
        cx.notify();
        true
    }

    /// notifyDirsChanged on window focus: the engine keeps every watched
    /// folder current, so this only makes sure each shown folder is watched.
    pub(crate) fn resume(&mut self, cx: &mut Context<Self>) {
        if self.enabled {
            self.sync_watches(cx);
        }
    }

    /// A hidden tree stays intact; it follows the project when Files is shown.
    fn git_changed(&mut self, cx: &mut Context<Self>) {
        if self.enabled {
            self.sync_project(cx);
        }
    }

    fn sync_project(&mut self, cx: &mut Context<Self>) -> bool {
        let root = self.git.read(cx).root.clone();
        if root == self.cwd {
            return false;
        }
        if !self.cwd.is_empty() {
            self.saved.insert(
                self.cwd.clone(),
                Saved {
                    expanded: std::mem::take(&mut self.expanded),
                    selected: self.selected.take(),
                },
            );
        }
        self.cwd = root.clone();
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.set_root(root.clone(), cx));
        }
        let saved = self.saved.get(&root).cloned().unwrap_or_else(|| Saved {
            expanded: HashSet::from([root.clone()]),
            selected: None,
        });
        self.expanded = saved.expanded;
        self.selected = saved.selected;
        self.naming = None;
        self.clip = None;
        self.menu = None;
        self.set_operation_error(None, cx);
        self.list.scroll_to(gpui::ListOffset::default());
        // The new project's folders are watched under its own root.
        self.watches.clear();
        self.rebuild(cx);
        true
    }

    fn available(&self) -> bool {
        !self.cwd.is_empty() && self.cwd != "~"
    }

    /// Watches every folder the tree shows and lets the rest go: their last
    /// listings stay, as monocode's cache did.
    fn sync_watches(&mut self, cx: &mut Context<Self>) {
        if !self.enabled || !self.available() {
            self.watches.clear();
            return;
        }
        let shown = self.shown_dirs();
        self.watches.retain(|path, _| shown.contains(path));
        for path in shown {
            if !self.watches.contains_key(&path) {
                self.watch_dir(path, cx);
            }
        }
    }

    /// The root and every expanded folder whose parents are all expanded.
    fn shown_dirs(&self) -> HashSet<String> {
        let mut shown = HashSet::from([self.cwd.clone()]);
        let mut expanded: Vec<&String> = self
            .expanded
            .iter()
            .filter(|p| *p != &self.cwd && files::is_equal_or_inside(p, &self.cwd))
            .collect();
        // Parents first: a folder's path is longer than its parent's.
        expanded.sort_by_key(|p| p.len());
        for path in expanded {
            if shown.contains(&parent_of(path)) {
                shown.insert(path.clone());
            }
        }
        shown
    }

    fn watch_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let engine = self.engine(cx);
        let root = engine.path_of(&self.cwd);
        let watched = engine.path_of(&path);
        let watching = engine.watch::<WatchDir>(move |since| WatchDir {
            root: root.clone(),
            path: watched.clone(),
            since,
        });
        let updates = watching.updates.clone();
        let dir = path.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                let listing = match update {
                    WatchUpdate::Snapshot(listing) => *listing,
                    WatchUpdate::Events(events) => match events.into_iter().last() {
                        Some(DirEvent::Listing(listing)) => listing,
                        None => continue,
                    },
                };
                let listing = respell_listing(&engine, listing);
                if this
                    .update(cx, |this, cx| this.listed(&dir, listing, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        self.watches.insert(path, (watching, task));
    }

    /// The engine's listing of `dir`. The echoes of applied calls it covers
    /// (it was read at or after the scan their replies named) give way to it;
    /// a listing read before a call, arriving after its reply, leaves them.
    fn listed(&mut self, dir: &str, listing: Listing, cx: &mut Context<Self>) {
        let entries = match listing.entries {
            Entries::Read(entries) => Ok(entries),
            Entries::Failed(error) => Err(error),
        };
        self.settled
            .retain(|held| held.echo.folder() != dir || listing.scan < held.until);
        self.scans.insert(dir.to_owned(), listing.scan);
        self.listed.insert(dir.to_owned(), entries);
        self.refold(dir);
        self.rebuild(cx);
    }

    /// What `dir` shows: its listing with every echo over it.
    fn refold(&mut self, dir: &str) {
        let Some(listed) = self.listed.get(dir) else {
            return;
        };
        let mut shown = listed.clone();
        if let Ok(entries) = &mut shown {
            let echoes = self
                .echoes
                .iter()
                .map(|p| &p.echo)
                .chain(self.settled.iter().map(|held| &held.echo));
            for echo in echoes.filter(|echo| echo.folder() == dir) {
                echo.apply(entries);
            }
        }
        self.dirs.insert(dir.to_owned(), shown);
    }

    /// Shows `echoes` now, until the call made under `op` replies.
    fn echo(&mut self, op: OpId, echoes: Vec<Echo>, cx: &mut Context<Self>) {
        let folders: HashSet<String> = echoes.iter().map(Echo::folder).collect();
        self.echoes
            .extend(echoes.into_iter().map(|echo| Pending { op, echo }));
        for folder in &folders {
            self.refold(folder);
        }
        self.rebuild(cx);
    }

    /// The call made under `op` replied. A failed call's echoes go now. An
    /// applied call's stay until their folder's listing reaches the scan the
    /// reply names for it (the engine relisted it before replying), so a
    /// listing read before the call never shows the old state between. A
    /// folder the engine was not watching yet starts from a listing read after
    /// the call: its next listing covers it. A copy's echo takes the name the
    /// engine gave it.
    fn settle(&mut self, op: OpId, landed: Landed<'_>, cx: &mut Context<Self>) {
        let mut folders = HashSet::new();
        let mut kept = Vec::new();
        let pending = std::mem::take(&mut self.echoes);
        for held in pending {
            if held.op != op {
                self.echoes.push(held);
                continue;
            }
            let mut echo = held.echo;
            folders.insert(echo.folder());
            let Landed::Applied { listed, created } = &landed else {
                continue;
            };
            if let (Echo::Add(entry), Some(created)) = (&mut echo, created)
                && entry.path != *created
                && parent_of(created) == parent_of(&entry.path)
            {
                basename(created).clone_into(&mut entry.name);
                (*created).clone_into(&mut entry.path);
            }
            let folder = echo.folder();
            if !self.watches.contains_key(&folder) {
                continue;
            }
            let held_scan = self.scans.get(&folder).copied();
            let until = listed
                .iter()
                .find(|relisted| relisted.root == self.cwd && relisted.path == folder)
                .map_or_else(
                    || held_scan.map_or(0, |scan| scan + 1),
                    |relisted| relisted.scan,
                );
            if held_scan.is_none_or(|scan| scan < until) {
                kept.push(Settled { echo, until });
            }
        }
        self.settled.extend(kept);
        for folder in &folders {
            self.refold(folder);
        }
        self.rebuild(cx);
    }

    /// The engine that serves the project ([`engine_client::for_project`]).
    fn engine(&self, cx: &mut App) -> Engine {
        engine_client::for_project(&self.cwd, cx).0
    }

    /// Moves or copies `from` on the project's engine under `op`; the reply's paths come back
    /// in the app's spelling.
    fn moving(
        &self,
        op: OpId,
        from: &str,
        to: Destination,
        cx: &mut App,
    ) -> impl Future<Output = Result<NewPath, String>> + 'static + use<> {
        let engine = self.engine(cx);
        let to = match to {
            Destination::Rename(name) => Destination::Rename(name),
            Destination::Into(dest) => Destination::Into(engine.path_of(&dest)),
            Destination::CopyInto(dest) => Destination::CopyInto(engine.path_of(&dest)),
        };
        let call = engine.call::<Move>(
            Some(op),
            Move {
                from: engine.path_of(from),
                to,
            },
        );
        async move { call.await.map(|moved| respell_new_path(&engine, moved)) }
    }

    fn forget(&mut self, path: &str) {
        let prefix = format!("{path}/");
        let gone = |k: &String| k == path || k.starts_with(&prefix);
        self.dirs.retain(|k, _| !gone(k));
        self.listed.retain(|k, _| !gone(k));
        self.scans.retain(|k, _| !gone(k));
        self.watches.retain(|k, _| !gone(k));
    }

    fn is_dir_at(&self, path: &str) -> bool {
        if path == self.cwd {
            return true;
        }
        match self.dirs.get(&parent_of(path)) {
            Some(Ok(entries)) => entries
                .iter()
                .find(|e| e.path == path)
                .map(|e| e.is_dir)
                .unwrap_or_else(|| self.dirs.contains_key(path)),
            _ => self.dirs.contains_key(path),
        }
    }

    fn create_parent(&self, at: Option<&str>) -> String {
        files::create_parent_of(&self.cwd, at, |p| self.is_dir_at(p))
    }

    fn siblings(&self, parent: &str, except: Option<&str>) -> Vec<String> {
        match self.dirs.get(parent) {
            Some(Ok(entries)) => entries
                .iter()
                .map(|e| e.name.clone())
                .filter(|n| Some(n.as_str()) != except)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn issue(&self, cx: &App) -> Option<files::NameIssue> {
        let value = self.name_input.read(cx).content.to_string();
        let siblings = match self.naming.as_ref()? {
            NameMode::Create { parent, .. } => self.siblings(parent, None),
            NameMode::Rename { path, .. } => self.siblings(&parent_of(path), Some(basename(path))),
        };
        files::validate_file_name(&value, &siblings)
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.sync_watches(cx);
        let mut lines = Vec::new();
        if self.available() && self.expanded.contains(&self.cwd) {
            self.push_children(&self.cwd.clone(), 0, &mut lines, cx);
        }
        // Splice only the rows that changed. Replacing the whole range makes
        // ListState reset its scroll top to row 0 on every toggle/refresh.
        let (start, old_end, new_end) = changed_range(&self.lines, &lines);
        self.lines = lines;
        if start < old_end || start < new_end {
            self.list.splice(start..old_end, new_end - start);
        }
        cx.notify();
    }

    fn push_children(&self, parent: &str, depth: usize, lines: &mut Vec<Line>, cx: &App) {
        let listing = self.dirs.get(parent);
        // A name being created shows as its echo until the call replies.
        let creating = match &self.naming {
            Some(NameMode::Create {
                parent: p, is_dir, ..
            }) if p == parent && !self.name_busy => Some(*is_dir),
            _ => None,
        };
        if let Some(Err(error)) = listing {
            lines.push(Line::Note {
                depth,
                text: error.clone(),
            });
        }
        if creating == Some(true) {
            self.push_name(depth, true, lines, cx);
        }
        if listing.is_none() {
            lines.push(Line::Note {
                depth,
                text: "…".into(),
            });
        }
        let entries: Vec<&DirEntry> = match listing {
            Some(Ok(entries)) => entries
                .iter()
                .filter(|e| self.show_excluded || !e.ignored)
                .collect(),
            _ => Vec::new(),
        };
        for entry in entries.iter().filter(|e| e.is_dir) {
            self.push_entry(entry, depth, lines, cx);
        }
        if creating == Some(false) {
            self.push_name(depth, false, lines, cx);
        }
        for entry in entries.iter().filter(|e| !e.is_dir) {
            self.push_entry(entry, depth, lines, cx);
        }
    }

    fn push_entry(&self, entry: &DirEntry, depth: usize, lines: &mut Vec<Line>, cx: &App) {
        let open = entry.is_dir && self.expanded.contains(&entry.path);
        if matches!(&self.naming, Some(NameMode::Rename { path, .. }) if path == &entry.path) {
            self.push_name(depth, entry.is_dir, lines, cx);
        } else {
            lines.push(Line::Entry {
                entry: entry.clone(),
                depth,
                open,
            });
        }
        if open {
            self.push_children(&entry.path, depth + 1, lines, cx);
        }
    }

    /// NameRow and its NameIssueView.
    fn push_name(&self, depth: usize, is_dir: bool, lines: &mut Vec<Line>, cx: &App) {
        lines.push(Line::Name { depth, is_dir });
        let value = self.name_input.read(cx).content.to_string();
        let issue = self.issue(cx);
        let show = self.name_error.is_some()
            || issue.as_ref().is_some_and(|i| {
                i.severity == Severity::Warning
                    || (i.kind != NameIssueKind::Empty && !value.is_empty())
                    || (i.kind == NameIssueKind::Empty && self.name_attempted)
            });
        if !show {
            return;
        }
        let text: Vec<(String, bool)> = if let Some(error) = &self.name_error {
            vec![(error.clone(), false)]
        } else if let Some(issue) = &issue {
            match issue.kind {
                NameIssueKind::Empty => {
                    vec![("A file or folder name must be provided.".into(), false)]
                }
                NameIssueKind::Slash => {
                    vec![(
                        "A file or folder name cannot start with a slash.".into(),
                        false,
                    )]
                }
                NameIssueKind::Exists => vec![
                    ("A file or folder ".into(), false),
                    (issue.name.clone(), true),
                    (
                        " already exists at this location. Please choose a different name.".into(),
                        false,
                    ),
                ],
                NameIssueKind::Invalid => vec![
                    ("The name ".into(), false),
                    (issue.name.clone(), true),
                    (
                        " is not valid as a file or folder name. Please choose a different name."
                            .into(),
                        false,
                    ),
                ],
                NameIssueKind::Whitespace => vec![(
                    "Leading or trailing whitespace detected in file or folder name.".into(),
                    false,
                )],
            }
        } else {
            return;
        };
        let error = self.name_error.is_some()
            || issue.as_ref().is_none_or(|i| i.severity == Severity::Error);
        lines.push(Line::Issue { depth, text, error });
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        self.selected = Some(path);
        cx.notify();
    }

    fn toggle(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_owned());
        }
        self.rebuild(cx);
    }

    /// The folders are watched from the next rebuild.
    fn expand(&mut self, dirs: &[String]) {
        self.expanded.extend(dirs.iter().cloned());
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.naming = None;
        self.expanded = HashSet::from([self.cwd.clone()]);
        self.rebuild(cx);
    }

    fn start_create(
        &mut self,
        is_dir: bool,
        at: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = self.create_parent(at.as_deref().or(self.selected.as_deref()));
        self.next_id += 1;
        self.expand(&[self.cwd.clone(), parent.clone()]);
        self.begin_name(
            NameMode::Create {
                id: self.next_id,
                parent,
                is_dir,
            },
            String::new(),
            window,
            cx,
        );
    }

    fn start_rename(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.cwd {
            return;
        }
        self.menu = None;
        let is_dir = self.is_dir_at(&path);
        let name = basename(&path).to_owned();
        self.selected = Some(path.clone());
        self.begin_name(NameMode::Rename { path, is_dir }, name.clone(), window, cx);
        // Files select their stem, folders the whole name.
        let end = if is_dir {
            name.len()
        } else {
            name.rfind('.').filter(|&d| d > 0).unwrap_or(name.len())
        };
        self.name_input
            .update(cx, |i, cx| i.select_range(0..end, cx));
    }

    fn begin_name(
        &mut self,
        mode: NameMode,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.naming = Some(mode);
        self.name_attempted = false;
        self.name_busy = false;
        self.name_error = None;
        self.name_input.update(cx, |i, cx| i.set_text(initial, cx));
        let focus = self.name_input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.rebuild(cx);
        if let Some(ix) = self
            .lines
            .iter()
            .position(|l| matches!(l, Line::Name { .. }))
        {
            self.list.scroll_to_reveal_item(ix);
        }
    }

    fn cancel_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        window.focus(&self.focus, cx);
        self.rebuild(cx);
    }

    fn finish_name(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.naming.clone() else {
            return;
        };
        if self.name_busy {
            return;
        }
        if !commit {
            self.cancel_name(window, cx);
            return;
        }
        if self
            .issue(cx)
            .is_some_and(|i| i.severity == Severity::Error)
        {
            self.name_attempted = true;
            self.rebuild(cx);
            return;
        }
        let raw = self.name_input.read(cx).content.to_string();
        let name = files::well_formed_file_name(&raw);
        let nested = name.contains(['/', '\\']);
        match mode {
            NameMode::Rename { path, is_dir } => {
                if name.is_empty() || (name == basename(&path) && !raw.contains(['/', '\\'])) {
                    self.cancel_name(window, cx);
                    return;
                }
                self.name_busy = true;
                let parent = parent_of(&path);
                let mut echoes = vec![Echo::Remove(path.clone())];
                if !nested {
                    echoes.push(Echo::Add(DirEntry {
                        name: name.clone(),
                        path: files::join_path(&parent, &name),
                        is_dir,
                        ignored: false,
                    }));
                }
                let op = OpId::new();
                let call = self.moving(op, &path, Destination::Rename(name.clone()), cx);
                self.echo(op, echoes, cx);
                cx.spawn_in(window, async move |this, cx| {
                    let result = call.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.settle(op, landed(&result), cx);
                        match result {
                            Ok(NewPath { path: next, .. }) => {
                                if is_dir {
                                    this.forget(&path);
                                }
                                let touched = files::dirs_touched_by_create(&parent, &name);
                                this.naming = None;
                                this.expand(&touched);
                                this.remap(&path, &next);
                                this.after_op(window, cx);
                            }
                            Err(error) => this.name_failed(error, cx),
                        }
                    });
                })
                .detach();
            }
            NameMode::Create { id, parent, is_dir } => {
                let as_folder = is_dir || raw.ends_with(['/', '\\']);
                self.name_busy = true;
                // Only the first new name shows: the folders under it are not listed yet.
                let echoes: Vec<Echo> = name
                    .split(['/', '\\'])
                    .find(|segment| !segment.is_empty())
                    .map(|first| {
                        Echo::Add(DirEntry {
                            name: first.to_owned(),
                            path: files::join_path(&parent, first),
                            is_dir: as_folder || nested,
                            ignored: false,
                        })
                    })
                    .into_iter()
                    .collect();
                let op = OpId::new();
                let engine = self.engine(cx);
                let create = engine.call::<Create>(
                    Some(op),
                    Create {
                        parent: engine.path_of(&parent),
                        name: name.clone(),
                        dir: as_folder,
                    },
                );
                let call = async move { create.await.map(|made| respell_new_path(&engine, made)) };
                self.echo(op, echoes, cx);
                cx.spawn_in(window, async move |this, cx| {
                    let result = call.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.settle(op, landed(&result), cx);
                        match result {
                            Ok(NewPath { path: created, .. }) => {
                                let touched = files::dirs_touched_by_create(&parent, &name);
                                if matches!(&this.naming, Some(NameMode::Create { id: i, .. }) if *i == id) {
                                    this.naming = None;
                                }
                                this.expand(&touched);
                                this.selected = Some(created);
                                this.after_op(window, cx);
                            }
                            Err(error) => this.name_failed(error, cx),
                        }
                    });
                })
                .detach();
            }
        }
    }

    fn name_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.name_busy = false;
        self.name_error = Some(error);
        self.rebuild(cx);
    }

    fn after_op(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.name_busy = false;
        window.focus(&self.focus, cx);
        self.git.update(cx, |git, cx| git.changed(cx));
        self.rebuild(cx);
    }

    fn remap(&mut self, from: &str, to: &str) {
        self.expanded = self
            .expanded
            .iter()
            .map(|p| files::rebase_path(p, from, to))
            .collect();
        if let Some(selected) = &self.selected {
            self.selected = Some(files::rebase_path(selected, from, to));
        }
        if let Some(clip) = &mut self.clip
            && (clip.path == from || clip.path.starts_with(&format!("{from}/")))
        {
            clip.path = files::rebase_path(&clip.path, from, to);
        }
    }

    fn set_operation_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.op_error = error.clone();
        cx.emit(FileActionError(error));
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.set_operation_error(Some(error), cx);
        self.rebuild(cx);
    }

    /// `run`: `work` replies with the path it made, if any, and the listings
    /// that cover it, while `echo` shows its effect. Operation errors route to
    /// the shared toast until the next action.
    fn run(
        &mut self,
        echo: Option<(OpId, Vec<Echo>)>,
        work: impl Future<Output = Result<Ran, String>> + 'static,
        touched: Vec<String>,
        forget: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_operation_error(None, cx);
        let op = echo.map(|(op, echoes)| {
            self.echo(op, echoes, cx);
            op
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(op) = op {
                    let reply = match &result {
                        Ok((created, listed)) => Landed::Applied {
                            listed,
                            created: created.as_deref(),
                        },
                        Err(_) => Landed::Failed,
                    };
                    this.settle(op, reply, cx);
                }
                match result {
                    Ok((created, _)) => {
                        if let Some(path) = &forget {
                            this.forget(path);
                        }
                        this.expand(&touched);
                        if let Some(created) = created {
                            this.selected = Some(created);
                        }
                        this.after_op(window, cx);
                    }
                    Err(error) => this.fail(error, cx),
                }
            });
        })
        .detach();
    }

    fn delete(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.cwd {
            return;
        }
        let is_dir = self.is_dir_at(&path);
        let label = basename(&path).to_owned();
        let answer = window.prompt(
            PromptLevel::Warning,
            &if is_dir {
                format!("Delete folder “{label}” and everything inside it?")
            } else {
                format!("Delete “{label}”?")
            },
            None,
            &["OK", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                let parent = parent_of(&path);
                this.selected = match &this.selected {
                    Some(s) if s == &path || s.starts_with(&format!("{path}/")) => {
                        Some(parent.clone())
                    }
                    other => other.clone(),
                };
                if this
                    .clip
                    .as_ref()
                    .is_some_and(|c| c.path == path || c.path.starts_with(&format!("{path}/")))
                {
                    this.clip = None;
                }
                let op = OpId::new();
                let engine = this.engine(cx);
                let call = engine.call::<DeletePath>(
                    Some(op),
                    DeletePath {
                        path: engine.path_of(&path),
                    },
                );
                this.run(
                    Some((op, vec![Echo::Remove(path.clone())])),
                    async move {
                        call.await
                            .map(|deleted| (None, respell_listed(&engine, deleted.listed)))
                    },
                    vec![parent],
                    is_dir.then_some(path),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    /// The entry a move of `from` into `dest` adds, unless its name is taken
    /// there (the move then fails).
    fn arriving(&self, from: &str, is_dir: bool, dest: &str) -> Vec<Echo> {
        let name = basename(from).to_owned();
        if self.siblings(dest, None).contains(&name) {
            return Vec::new();
        }
        vec![Echo::Add(DirEntry {
            path: files::join_path(dest, &name),
            name,
            is_dir,
            ignored: false,
        })]
    }

    /// The entry a copy of `from` into `dest` adds: under its own name, or
    /// renamed as Finder does when `taken` holds it. The engine picks the
    /// same name; should it differ, the echo takes the engine's.
    fn copied(from: &str, is_dir: bool, dest: &str, taken: &HashSet<String>) -> Echo {
        let name = files::unique_name(basename(from), |name| taken.contains(name));
        Echo::Add(DirEntry {
            path: files::join_path(dest, &name),
            name,
            is_dir,
            ignored: false,
        })
    }

    /// The names in `dest` as it shows now: what a copy's name steps past.
    fn taken(&self, dest: &str) -> HashSet<String> {
        self.siblings(dest, None).into_iter().collect()
    }

    fn paste(&mut self, at: String, window: &mut Window, cx: &mut Context<Self>) {
        let dest = self.create_parent(Some(&at));
        let Some(clip) = self.clip.clone() else {
            // clipboardFilePaths: files copied in Finder.
            let paths: Vec<PathBuf> = cx
                .read_from_clipboard()
                .map(|item| {
                    item.entries()
                        .iter()
                        .filter_map(|e| match e {
                            ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
                            _ => None,
                        })
                        .flatten()
                        .collect()
                })
                .unwrap_or_default();
            self.copy_external(paths, dest, window, cx);
            return;
        };
        if clip.is_dir && (dest == clip.path || dest.starts_with(&format!("{}/", clip.path))) {
            self.fail("Cannot paste a folder into itself.".into(), cx);
            return;
        }
        let from = clip.path.clone();
        let op = OpId::new();
        if clip.cut {
            let mut echoes = Vec::new();
            if parent_of(&from) != dest {
                echoes.push(Echo::Remove(from.clone()));
                echoes.extend(self.arriving(&from, clip.is_dir, &dest));
            }
            let call = self.moving(op, &from, Destination::Into(dest.clone()), cx);
            self.echo(op, echoes, cx);
            cx.spawn_in(window, async move |this, cx| {
                let result = call.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.settle(op, landed(&result), cx);
                    match result {
                        Ok(NewPath { path: created, .. }) => {
                            if clip.is_dir {
                                this.forget(&from);
                            }
                            this.remap(&from, &created);
                            this.clip = None;
                            this.expand(std::slice::from_ref(&dest));
                            this.selected = Some(created);
                            this.after_op(window, cx);
                        }
                        Err(error) => this.fail(error, cx),
                    }
                });
            })
            .detach();
        } else {
            let echo = Self::copied(&from, clip.is_dir, &dest, &self.taken(&dest));
            let call = self.moving(op, &from, Destination::CopyInto(dest.clone()), cx);
            self.run(
                Some((op, vec![echo])),
                async move { call.await.map(|moved| (Some(moved.path), moved.listed)) },
                vec![dest],
                None,
                window,
                cx,
            );
        }
    }

    /// Copies files from outside the tree (Finder) into `dest`, one after
    /// another until one fails, as monocode did. Every copy shows in the frame
    /// it is asked for; one that fails takes its echo, and those after it
    /// (never made) theirs, with it.
    fn copy_external(
        &mut self,
        paths: Vec<PathBuf>,
        dest: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.set_operation_error(None, cx);
        let mut taken = self.taken(&dest);
        let mut copies = Vec::new();
        for from in paths {
            // Finder's paths are on this Mac: what they are is known here.
            let is_dir = from.is_dir();
            let from = from.to_string_lossy().into_owned();
            let echo = Self::copied(&from, is_dir, &dest, &taken);
            if let Echo::Add(entry) = &echo {
                taken.insert(entry.name.clone());
            }
            let op = OpId::new();
            self.echo(op, vec![echo], cx);
            copies.push((op, from));
        }
        // Finder's files are this Mac's: the app's own engine copies them, and another engine,
        // which cannot read this disk, is handed their bytes.
        let engine = self.engine(cx);
        let into = engine.path_of(&dest);
        cx.spawn_in(window, async move |this, cx| {
            let mut created = None;
            for (at, (op, from)) in copies.iter().enumerate() {
                let result = if engine.target().is_some() {
                    let source = PathBuf::from(from);
                    let read = cx
                        .background_executor()
                        .spawn(async move { import_entries(&source) })
                        .await;
                    match read {
                        Ok(entries) => {
                            engine
                                .call::<ImportFiles>(
                                    Some(*op),
                                    ImportFiles {
                                        into: into.clone(),
                                        name: basename(from).to_owned(),
                                        entries,
                                    },
                                )
                                .await
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    engine
                        .call::<Move>(
                            Some(*op),
                            Move {
                                from: from.clone(),
                                to: Destination::CopyInto(into.clone()),
                            },
                        )
                        .await
                }
                .map(|moved| respell_new_path(&engine, moved));
                let failed = this.update_in(cx, |this, _, cx| {
                    this.settle(*op, landed(&result), cx);
                    match result {
                        Ok(moved) => {
                            created = Some(moved.path);
                            None
                        }
                        Err(error) => {
                            for (never, _) in &copies[at + 1..] {
                                this.settle(*never, Landed::Failed, cx);
                            }
                            Some(error)
                        }
                    }
                });
                match failed {
                    Ok(None) => {}
                    Ok(Some(error)) => {
                        let _ = this.update_in(cx, |this, _, cx| this.fail(error, cx));
                        return;
                    }
                    Err(_) => return,
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.expand(std::slice::from_ref(&dest));
                if let Some(created) = created {
                    this.selected = Some(created);
                }
                this.after_op(window, cx);
            });
        })
        .detach();
    }

    fn duplicate(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.cwd {
            return;
        }
        let parent = parent_of(&path);
        let is_dir = self.is_dir_at(&path);
        let op = OpId::new();
        let echo = Self::copied(&path, is_dir, &parent, &self.taken(&parent));
        let call = self.moving(op, &path, Destination::CopyInto(parent.clone()), cx);
        self.run(
            Some((op, vec![echo])),
            async move { call.await.map(|moved| (Some(moved.path), moved.listed)) },
            vec![parent],
            None,
            window,
            cx,
        );
    }

    /// Revealing opens Finder on this machine, not on an engine.
    fn reveal(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let revealed = reveal_path(Path::new(&path), cx).map(|()| (None, Vec::new()));
        self.run(None, async move { revealed }, Vec::new(), None, window, cx);
    }

    fn open_menu(
        &mut self,
        target: Target,
        at: Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.naming = None;
        self.selected = Some(target.path.clone());
        let paste_parent = if target.is_dir {
            target.path.clone()
        } else {
            parent_of(&target.path)
        };
        let paste_blocked = self.clip.as_ref().is_some_and(|c| {
            c.is_dir
                && (paste_parent == c.path || paste_parent.starts_with(&format!("{}/", c.path)))
        });
        let root = target.root;
        let items: Vec<MenuItem> = vec![
            MenuAction::new("new-file", "New File").into(),
            MenuAction::new("new-folder", "New Folder").into(),
            MenuItem::Separator,
            MenuAction::new("cut", "Cut")
                .shortcut(format!("{MOD}X"))
                .disabled(root)
                .into(),
            MenuAction::new("copy", "Copy")
                .shortcut(format!("{MOD}C"))
                .disabled(root)
                .into(),
            MenuAction::new("paste", "Paste")
                .shortcut(format!("{MOD}V"))
                .disabled(paste_blocked)
                .into(),
            MenuAction::new("duplicate", "Duplicate")
                .disabled(root)
                .into(),
            MenuItem::Separator,
            MenuAction::new("copy-path", "Copy Path")
                .shortcut(format!("{MOD}{SHIFT}C"))
                .into(),
            MenuAction::new("copy-relative-path", "Copy Relative Path").into(),
            MenuItem::Separator,
            MenuAction::new("rename", "Rename")
                .shortcut("F2")
                .disabled(root)
                .into(),
            MenuAction::new("delete", "Delete")
                .shortcut("⌫")
                .disabled(root)
                .danger()
                .into(),
            MenuItem::Separator,
            MenuAction::new("reveal", REVEAL_LABEL).into(),
        ];
        self.menu = None;
        let menu = cx.new(|cx| Menu::new("File actions", items, window, cx));
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
            let Some(open) = this.menu.take() else {
                return;
            };
            if let MenuEvent::Picked(id, _) = event {
                this.run_action(id, open.target, window, cx);
            }
            cx.notify();
        });
        self.menu = Some(OpenMenu {
            target,
            anchor: at,
            menu,
            _events: events,
        });
        self.rebuild(cx);
    }

    fn run_action(
        &mut self,
        id: &str,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            "new-file" => self.start_create(false, Some(target.path), window, cx),
            "new-folder" => self.start_create(true, Some(target.path), window, cx),
            "cut" | "copy" if !target.root => {
                self.clip = Some(Clip {
                    cut: id == "cut",
                    path: target.path,
                    is_dir: target.is_dir,
                });
                self.rebuild(cx);
            }
            "paste" => self.paste(target.path, window, cx),
            "duplicate" => self.duplicate(target.path, window, cx),
            "copy-path" => cx.write_to_clipboard(ClipboardItem::new_string(target.path)),
            "copy-relative-path" => cx.write_to_clipboard(ClipboardItem::new_string(
                files::display_path(&target.path, Some(&self.cwd)),
            )),
            "rename" => self.start_rename(target.path, window, cx),
            "delete" => self.delete(target.path, window, cx),
            "reveal" => self.reveal(target.path, window, cx),
            _ => {}
        }
    }

    /// The rows keyboard navigation walks: the root, then each entry shown.
    fn nav_rows(&self) -> Vec<(String, bool)> {
        let mut rows = vec![(self.cwd.clone(), true)];
        rows.extend(self.lines.iter().filter_map(|l| match l {
            Line::Entry { entry, .. } => Some((entry.path.clone(), entry.is_dir)),
            _ => None,
        }));
        rows
    }

    fn focus_row(&mut self, path: String, cx: &mut Context<Self>) {
        self.keyboard = true;
        if let Some(ix) = self
            .lines
            .iter()
            .position(|l| matches!(l, Line::Entry { entry, .. } if entry.path == path))
        {
            self.list.scroll_to_reveal_item(ix);
        }
        self.select(path, cx);
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.nav_rows();
        let current = self.selected.clone().unwrap_or_else(|| self.cwd.clone());
        let ix = rows.iter().position(|(p, _)| *p == current);
        let next = match ix {
            None if delta > 0 => 0,
            None => rows.len() - 1,
            Some(ix) => (ix as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
        };
        if let Some((path, _)) = rows.get(next).cloned() {
            self.focus_row(path, cx);
        }
    }

    fn page(&self, window: &Window) -> isize {
        let height = self.list.viewport_bounds().size.height.as_f32();
        let _ = window;
        ((height / ROW).floor() as isize - 1).max(1)
    }

    fn current(&self) -> (String, bool) {
        let path = self.selected.clone().unwrap_or_else(|| self.cwd.clone());
        let is_dir = self.is_dir_at(&path);
        (path, is_dir)
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        let (path, is_dir) = self.current();
        if is_dir {
            self.toggle(&path, cx);
        } else {
            cx.emit(OpenFile(path));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        if m.control || m.alt || m.platform || m.function {
            return;
        }
        if keystroke.key == "space" {
            self.activate(cx);
            cx.stop_propagation();
            return;
        }
        // Type a name prefix to jump to the next matching row; repeating one
        // letter cycles through its matches.
        let Some(ch) = keystroke
            .key_char
            .as_ref()
            .filter(|c| c.chars().count() == 1)
        else {
            return;
        };
        let now = Instant::now();
        let fresh = self
            .typeahead
            .1
            .is_none_or(|at| now.duration_since(at).as_millis() > 700);
        if fresh {
            self.typeahead.0 = ch.clone();
        } else {
            self.typeahead.0.push_str(ch);
        }
        self.typeahead.1 = Some(now);
        let lower = self.typeahead.0.to_lowercase();
        let first = lower.chars().next().unwrap_or_default();
        let cycling = lower.chars().all(|c| c == first);
        let needle = if cycling { first.to_string() } else { lower };
        let rows = self.nav_rows();
        let current = self.selected.clone().unwrap_or_else(|| self.cwd.clone());
        let ix = rows.iter().position(|(p, _)| *p == current);
        let start = if cycling {
            ix.map_or(0, |i| i + 1)
        } else {
            ix.unwrap_or(0)
        };
        let ordered = rows[start.min(rows.len())..]
            .iter()
            .chain(&rows[..start.min(rows.len())]);
        if let Some((path, _)) = ordered
            .filter(|(p, _)| *p != self.cwd)
            .find(|(p, _)| basename(p).to_lowercase().starts_with(&needle))
            .cloned()
        {
            self.focus_row(path, cx);
        }
        cx.stop_propagation();
    }

    fn header_icon(
        id: &'static str,
        label: SharedString,
        path: &'static str,
        enabled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .group(id)
            .h(px(24.))
            .min_w_0()
            .flex_1()
            .self_center()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .when(!enabled, |d| d.opacity(0.4))
            .when(enabled, |d| d.hover(|s| s.bg(content(0.05))))
            .child(icon(path, 14.).text_color(content(0.5)).when(enabled, |i| {
                i.group_hover(id, |s| s.text_color(content(1.)))
            }))
    }

    fn line(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let statuses = &self.git.read(cx).statuses;
        let pad = |depth: usize| px(28. + depth as f32 * 12.);
        match self.lines[ix].clone() {
            Line::Note { depth, text } => div()
                .pl(pad(depth))
                .pr_2()
                .truncate()
                .text_size(px(12.))
                .text_color(content(0.5))
                .child(text)
                .into_any_element(),
            Line::Issue { depth, text, error } => div()
                .pl(pad(depth))
                .pr_2()
                .pb_1()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(rgb(if error { RED_400 } else { AMBER_400 }))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .children(text.into_iter().map(|(t, bold)| {
                            div()
                                .when(bold, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .child(t)
                        })),
                )
                .into_any_element(),
            Line::Name { depth, is_dir } => {
                let leaf = files::leaf_name(&self.name_input.read(cx).content);
                div()
                    .key_context("ExplorerName")
                    .on_action(cx.listener(|this, _: &CancelName, window, cx| {
                        this.finish_name(false, window, cx)
                    }))
                    .h(px(ROW))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .bg(content(0.1))
                    .pl(px(8. + depth as f32 * 12.))
                    .pr_2()
                    .child(
                        div()
                            .size_4()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(is_dir, |d| {
                                d.child(icon("chevron-right", 14.).text_color(content(0.5)))
                            }),
                    )
                    .child(file_type_icon(&leaf, is_dir, false, 16.))
                    .child(
                        div()
                            .h(px(20.))
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .bg(content(0.1))
                            .border_1()
                            .border_color(accent_color())
                            .px_1()
                            .text_size(px(ENTRY_FONT_SIZE))
                            .text_color(content(1.))
                            .when(self.name_busy, |d| d.opacity(0.5))
                            .child(self.name_input.clone()),
                    )
                    .into_any_element()
            }
            Line::Entry { entry, depth, open } => {
                let selected = self.selected.as_deref() == Some(entry.path.as_str());
                let cut = self
                    .clip
                    .as_ref()
                    .is_some_and(|c| c.cut && c.path == entry.path);
                let over = self.drag_over.as_deref() == Some(entry.path.as_str());
                let status = if entry.is_dir {
                    statuses.dirs.get(&entry.path)
                } else {
                    statuses.files.get(&entry.path)
                };
                let git_color: Option<Hsla> = status.and_then(|s| match s.as_str() {
                    "modified" => Some(rgb(AMBER_400).into()),
                    "added" | "untracked" => Some(rgb(DIFF_ADD).into()),
                    "deleted" => Some(rgb(DIFF_DEL).into()),
                    _ => None,
                });
                let path = entry.path.clone();
                let menu_path = entry.path.clone();
                let drop_path = entry.path.clone();
                let over_path = entry.path.clone();
                let is_dir = entry.is_dir;
                div()
                    .id(("explorer-row", ix))
                    .role(Role::TreeItem)
                    .aria_label(SharedString::from(entry.path.clone()))
                    .when(is_dir, |d| d.aria_expanded(open))
                    .aria_selected(selected)
                    .h(px(ROW))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl(px(8. + depth as f32 * 12.))
                    .pr_2()
                    .text_size(px(ENTRY_FONT_SIZE))
                    .line_height(px(ENTRY_FONT_SIZE))
                    .text_color(content(1.))
                    .when(selected || over, |d| d.bg(selection()))
                    .when(!selected && !over, |d| d.hover(|s| s.bg(content(0.05))))
                    .when(selected && focused && self.keyboard, |d| {
                        d.border_1().border_color(accent_color().opacity(0.4))
                    })
                    .when(cut, |d| d.opacity(0.5))
                    .when(!is_dir, |d| {
                        d.on_drag(DraggedFile(PathBuf::from(&entry.path)), |file, _, _, cx| {
                            cx.new(|_| file.clone())
                        })
                    })
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.keyboard = false;
                        window.focus(&this.focus, cx);
                        this.select(path.clone(), cx);
                        if is_dir {
                            this.toggle(&path, cx);
                        } else {
                            cx.emit(OpenFile(path.clone()));
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_menu(
                                Target {
                                    path: menu_path.clone(),
                                    is_dir,
                                    root: false,
                                },
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .on_drag_move::<ExternalPaths>(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                            if event.bounds.contains(&event.event.position) {
                                let target = this.create_parent(Some(&over_path));
                                if this.drag_over.as_ref() != Some(&target) {
                                    this.drag_over = Some(target);
                                    cx.notify();
                                }
                            }
                        },
                    ))
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                        this.drag_over = None;
                        let dest = this.create_parent(Some(&drop_path));
                        this.copy_external(paths.paths().to_vec(), dest, window, cx);
                    }))
                    .child(
                        div()
                            .size_4()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(is_dir, |d| {
                                d.child(
                                    icon(
                                        if open {
                                            "chevron-down"
                                        } else {
                                            "chevron-right"
                                        },
                                        14.,
                                    )
                                    .text_color(content(0.5)),
                                )
                            }),
                    )
                    .child(file_type_icon(&entry.name, is_dir, open, 16.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .line_height(px(ENTRY_FONT_SIZE * 1.4))
                            .when(entry.ignored, |d| d.italic().text_color(content(0.5)))
                            .when_some(git_color.filter(|_| !entry.ignored), |d, c| d.text_color(c))
                            .child(entry.name.clone()),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Focusable for Explorer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search
            .as_ref()
            .map_or_else(|| self.focus.clone(), |search| search.focus_handle(cx))
    }
}

impl Render for Explorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(search) = &self.search {
            return search
                .clone()
                .cached(StyleRefinement::default().flex_1().min_h_0().w_full())
                .into_any_element();
        }
        if !self.available() {
            return div()
                .px_3()
                .py_2()
                .text_size(px(12.))
                .text_color(content(0.5))
                .child("No project folder")
                .into_any_element();
        }
        let root_open = self.expanded.contains(&self.cwd);
        let name: SharedString = basename(&self.cwd).to_uppercase().into();
        let root_selected = self.drag_over.as_deref() == Some(self.cwd.as_str());
        let focused = self.focus.contains_focused(window, cx);
        let entity = cx.entity();
        let cwd = self.cwd.clone();
        let menu = self
            .menu
            .as_ref()
            .map(|open| menu_popover(&open.menu, Anchor::Point(open.anchor), cx).gap(0.));
        div()
            .id("explorer")
            .role(Role::TabPanel)
            .aria_label("Explorer")
            .key_context("Explorer")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &PageDown, window, cx| {
                let n = this.page(window);
                this.step(n, cx)
            }))
            .on_action(cx.listener(|this, _: &PageUp, window, cx| {
                let n = this.page(window);
                this.step(-n, cx)
            }))
            .on_action(cx.listener(|this, _: &First, _, cx| this.step(-isize::MAX / 2, cx)))
            .on_action(cx.listener(|this, _: &Last, _, cx| this.step(isize::MAX / 2, cx)))
            .on_action(cx.listener(|this, _: &Expand, _, cx| {
                let (path, is_dir) = this.current();
                if !is_dir {
                    return;
                }
                if !this.expanded.contains(&path) {
                    this.toggle(&path, cx);
                } else {
                    let rows = this.nav_rows();
                    if let Some(ix) = rows.iter().position(|(p, _)| *p == path)
                        && let Some((next, _)) = rows.get(ix + 1).cloned()
                        && parent_of(&next) == path
                    {
                        this.focus_row(next, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| {
                let (path, is_dir) = this.current();
                if is_dir && this.expanded.contains(&path) {
                    this.toggle(&path, cx);
                } else if path != this.cwd {
                    this.focus_row(parent_of(&path), cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Activate, _, cx| this.activate(cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.activate(cx)))
            .on_action(cx.listener(|this, _: &Rename, window, cx| {
                let (path, _) = this.current();
                this.start_rename(path, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Delete, window, cx| {
                let (path, _) = this.current();
                this.delete(path, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ClearCut, _, cx| {
                if this.clip.as_ref().is_some_and(|c| c.cut) {
                    this.clip = None;
                    this.rebuild(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, action: &CopyEntry, _, cx| {
                let _ = action;
                let (path, is_dir) = this.current();
                if path != this.cwd {
                    this.clip = Some(Clip {
                        cut: false,
                        path,
                        is_dir,
                    });
                    this.rebuild(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CutEntry, _, cx| {
                let (path, is_dir) = this.current();
                if path != this.cwd {
                    this.clip = Some(Clip {
                        cut: true,
                        path,
                        is_dir,
                    });
                    this.rebuild(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PasteEntry, window, cx| {
                let (path, _) = this.current();
                this.paste(path, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| {
                let (path, _) = this.current();
                cx.write_to_clipboard(ClipboardItem::new_string(path));
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_menu(
                        Target {
                            path: this.cwd.clone(),
                            is_dir: true,
                            root: true,
                        },
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .child(
                pane_header("explorer-header", "Explorer")
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .child(
                        Self::header_icon("explorer-new-file", "New File".into(), "file-add", true)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_create(false, None, window, cx)
                            })),
                    )
                    .child(
                        Self::header_icon(
                            "explorer-new-folder",
                            "New Folder".into(),
                            "folder-add",
                            true,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_create(true, None, window, cx)
                        })),
                    )
                    .child(
                        Self::header_icon(
                            "explorer-collapse",
                            "Collapse All".into(),
                            "fold-vertical",
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
                    )
                    .child(
                        Self::header_icon(
                            "explorer-search",
                            format!("Search in files ({MOD}Shift+F)").into(),
                            "search",
                            true,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.open_search(window, cx))),
                    ),
            )
            .child(
                div()
                    .h(px(32.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .id("explorer-root")
                            .role(Role::TreeItem)
                            .aria_label(SharedString::from(cwd.clone()))
                            .aria_expanded(root_open)
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .pl_2()
                            .when(root_selected, |d| d.bg(selection()))
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.prevent_default()
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.focus, cx);
                                let cwd = this.cwd.clone();
                                this.select(cwd.clone(), cx);
                                this.toggle(&cwd, cx);
                            }))
                            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                                this.drag_over = None;
                                let dest = this.cwd.clone();
                                this.copy_external(paths.paths().to_vec(), dest, window, cx);
                            }))
                            .child(
                                div()
                                    .size_4()
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        icon(
                                            if root_open {
                                                "chevron-down"
                                            } else {
                                                "chevron-right"
                                            },
                                            14.,
                                        )
                                        .text_color(content(0.5)),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(content(0.5))
                                    .child(name),
                            ),
                    ),
            )
            .child(
                div()
                    .id("explorer-tree")
                    .role(Role::Tree)
                    .flex_1()
                    .min_h_0()
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        this.drag_over = None;
                        let dest = this.cwd.clone();
                        this.copy_external(paths.paths().to_vec(), dest, window, cx);
                    }))
                    .when(root_open, |d| {
                        d.child(
                            list(self.list.clone(), move |ix, _, cx| {
                                entity.update(cx, |this, cx| this.line(ix, focused, cx))
                            })
                            .size_full(),
                        )
                    }),
            )
            .children(menu)
            .into_any_element()
    }
}

/// Smallest `old[start..old_end]` -> `new[start..new_end]` replacement that
/// turns `old` into `new` (common prefix and suffix are left untouched).
fn changed_range<T: PartialEq>(old: &[T], new: &[T]) -> (usize, usize, usize) {
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[start..]
        .iter()
        .rev()
        .zip(new[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (start, old.len() - suffix, new.len() - suffix)
}

#[cfg(test)]
mod changed_range_tests {
    use super::changed_range;

    #[test]
    fn expanding_a_folder_only_touches_the_inserted_rows() {
        let old = [1, 2, 3, 4];
        let new = [1, 2, 9, 9, 3, 4];
        assert_eq!(changed_range(&old, &new), (2, 2, 4));
    }

    #[test]
    fn collapsing_and_flipping_a_row() {
        let old = [1, 2, 5, 6, 3];
        let new = [1, 7, 3];
        assert_eq!(changed_range(&old, &new), (1, 4, 2));
        assert_eq!(changed_range(&old, &old), (5, 5, 5));
        assert_eq!(changed_range::<i32>(&[], &[1]), (0, 0, 1));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    /// What another engine answers names its own paths; the explorer keeps them as the app
    /// spells that engine's project, and the app's own engine's paths stay as they are.
    #[gpui::test]
    fn another_engines_paths_come_back_as_its_project_uri(cx: &mut TestAppContext) {
        let own = cx.update(Engine::memory);
        let away = own.on(onicode_protocol::EngineId("nMac".into()));
        let entry = |path: &str| DirEntry {
            name: basename(path).to_owned(),
            path: path.into(),
            is_dir: false,
            ignored: false,
        };
        let listing = || Listing {
            scan: 3,
            entries: Entries::Read(vec![entry("/code/a.rs")]),
        };
        let Entries::Read(entries) = respell_listing(&away, listing()).entries else {
            panic!("a listing that was read");
        };
        assert_eq!(entries[0].path, "peer://nMac/code/a.rs");
        assert_eq!(respell_listing(&own, listing()), listing());
        let moved = respell_new_path(
            &away,
            NewPath {
                path: "/code/b.rs".into(),
                listed: vec![Relisted {
                    root: "/code".into(),
                    path: "/code".into(),
                    scan: 4,
                }],
            },
        );
        assert_eq!(moved.path, "peer://nMac/code/b.rs");
        assert_eq!(moved.listed[0].root, "peer://nMac/code");
        assert_eq!(away.path_of(&moved.path), "/code/b.rs");
    }

    /// A folder of its own under the temp dir, gone with the test.
    struct Folder(PathBuf);

    impl Folder {
        fn new(files: &[&str]) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "onicode-explorer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&root).unwrap();
            for name in files {
                std::fs::write(root.join(name), "").unwrap();
            }
            Self(root)
        }

        fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An explorer showing `folder`, its listing in.
    fn open<'a>(
        folder: &Folder,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Explorer>, &'a mut VisualTestContext) {
        let root = folder.path();
        let (explorer, cx) = cx.add_window_view(|window, cx| {
            let git = cx.new(|cx| ProjectGit::new(root.clone(), cx));
            Explorer::new(git, window, cx)
        });
        explorer.update(cx, |explorer, cx| explorer.set_enabled(true, cx));
        cx.run_until_parked();
        (explorer, cx)
    }

    fn names(explorer: &Explorer, dir: &str) -> Vec<String> {
        match explorer.dirs.get(dir) {
            Some(Ok(entries)) => entries.iter().map(|entry| entry.name.clone()).collect(),
            _ => Vec::new(),
        }
    }

    fn entry(dir: &str, name: &str) -> DirEntry {
        DirEntry {
            name: name.into(),
            path: files::join_path(dir, name),
            is_dir: false,
            ignored: false,
        }
    }

    #[gpui::test]
    fn a_listing_read_before_a_call_never_shows_after_its_reply(cx: &mut TestAppContext) {
        let folder = Folder::new(&["a.txt"]);
        let root = folder.path();
        let (explorer, cx) = open(&folder, cx);
        let held = explorer.read_with(cx, |explorer, _| {
            assert_eq!(names(explorer, &root), ["a.txt"]);
            explorer.scans[&root]
        });
        let op = OpId::new();
        let made = entry(&root, "b.txt");
        explorer.update(cx, |explorer, cx| {
            explorer.echo(op, vec![Echo::Add(made.clone())], cx);
            assert_eq!(names(explorer, &root), ["a.txt", "b.txt"], "the same frame");
            // The engine relisted the folder at `held + 5` before it replied.
            let listed = [Relisted {
                root: root.clone(),
                path: root.clone(),
                scan: held + 5,
            }];
            explorer.settle(
                op,
                Landed::Applied {
                    listed: &listed,
                    created: Some(&made.path),
                },
                cx,
            );
            assert_eq!(names(explorer, &root), ["a.txt", "b.txt"]);
            // A listing read before the call arrives after its reply.
            explorer.listed(
                &root,
                Listing {
                    scan: held + 1,
                    entries: Entries::Read(vec![entry(&root, "a.txt")]),
                },
                cx,
            );
            assert_eq!(
                names(explorer, &root),
                ["a.txt", "b.txt"],
                "the older listing never shows the old state"
            );
            explorer.listed(
                &root,
                Listing {
                    scan: held + 5,
                    entries: Entries::Read(vec![entry(&root, "a.txt"), made.clone()]),
                },
                cx,
            );
            assert_eq!(names(explorer, &root), ["a.txt", "b.txt"]);
            assert!(
                explorer.settled.is_empty(),
                "the covering listing takes over"
            );
            // Later listings are the engine's alone.
            explorer.listed(
                &root,
                Listing {
                    scan: held + 6,
                    entries: Entries::Read(vec![entry(&root, "a.txt")]),
                },
                cx,
            );
            assert_eq!(names(explorer, &root), ["a.txt"]);
        });
    }

    #[gpui::test]
    fn duplicates_and_finder_copies_show_in_the_frame_they_are_made(cx: &mut TestAppContext) {
        let folder = Folder::new(&["a.txt", "a copy.txt"]);
        let outside = Folder::new(&["n.md", "a.txt"]);
        let root = folder.path();
        let (explorer, cx) = open(&folder, cx);
        let original = files::join_path(&root, "a.txt");
        cx.update(|window, cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.duplicate(original.clone(), window, cx);
                assert_eq!(
                    names(explorer, &root),
                    ["a copy 2.txt", "a copy.txt", "a.txt"],
                    "the copy shows under the name Finder gives it, before the engine answers"
                );
            });
        });
        cx.run_until_parked();
        explorer.read_with(cx, |explorer, _| {
            assert_eq!(
                names(explorer, &root),
                ["a copy 2.txt", "a copy.txt", "a.txt"]
            );
            assert!(explorer.echoes.is_empty() && explorer.settled.is_empty());
        });

        let finder = vec![outside.0.join("n.md"), outside.0.join("a.txt")];
        cx.update(|window, cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.copy_external(finder, root.clone(), window, cx);
                assert_eq!(
                    names(explorer, &root),
                    [
                        "a copy 2.txt",
                        "a copy 3.txt",
                        "a copy.txt",
                        "a.txt",
                        "n.md"
                    ],
                    "every copy shows at once, each under its own name"
                );
            });
        });
        cx.run_until_parked();
        explorer.read_with(cx, |explorer, _| {
            assert_eq!(
                names(explorer, &root),
                [
                    "a copy 2.txt",
                    "a copy 3.txt",
                    "a copy.txt",
                    "a.txt",
                    "n.md"
                ]
            );
            assert!(explorer.echoes.is_empty() && explorer.settled.is_empty());
            assert_eq!(
                explorer.selected.as_deref(),
                Some(files::join_path(&root, "a copy 3.txt").as_str()),
                "the last copy is selected, as monocode did"
            );
        });
        assert!(folder.0.join("n.md").exists() && folder.0.join("a copy 3.txt").exists());

        // A copy that fails takes its echo, and the one after it (never made) too.
        let gone = vec![outside.0.join("missing.md"), outside.0.join("n.md")];
        cx.update(|window, cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.copy_external(gone, root.clone(), window, cx);
                assert!(names(explorer, &root).contains(&"missing.md".to_owned()));
            });
        });
        cx.run_until_parked();
        explorer.read_with(cx, |explorer, _| {
            let shown = names(explorer, &root);
            assert!(!shown.contains(&"missing.md".to_owned()));
            assert!(!shown.contains(&"n copy.md".to_owned()));
            assert!(explorer.op_error.is_some());
        });
        assert!(
            !folder.0.join("n copy.md").exists(),
            "copies stop at the first failure"
        );
    }

    /// Finder's files dropped into a project on another engine go to it with their bytes, each
    /// folder before what it holds, since that engine cannot read this Mac's disk: nothing asks
    /// it to copy a path of this Mac. One it cannot take says why, and leaves no echo.
    #[gpui::test]
    fn finder_files_dropped_on_another_machine_carry_their_bytes(cx: &mut TestAppContext) {
        let outside = Folder::new(&[]);
        std::fs::create_dir(outside.0.join("docs")).unwrap();
        std::fs::write(outside.0.join("docs/a.txt"), "hello").unwrap();
        let root = "peer://nStudio/srv/app".to_owned();
        let (explorer, cx) = cx.add_window_view(|window, cx| {
            let git = cx.new(|cx| ProjectGit::new(root.clone(), cx));
            Explorer::new(git, window, cx)
        });
        explorer.update(cx, |explorer, cx| explorer.set_enabled(true, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.copy_external(vec![outside.0.join("docs")], root.clone(), window, cx);
            });
        });
        cx.run_until_parked();
        let calls = cx.update(|_, cx| engine_client::calls_for_test(&engine_client::local(cx), cx));
        assert!(calls.iter().all(|(name, _)| *name != "Move"));
        let (_, import) = calls
            .iter()
            .find(|(name, _)| *name == "ImportFiles")
            .expect("the bytes go to the project's engine");
        assert_eq!(import["engine"], "nStudio");
        let body: ImportFiles = serde_json::from_value(import["body"].clone()).unwrap();
        assert_eq!(
            (body.into.as_str(), body.name.as_str()),
            ("/srv/app", "docs")
        );
        assert_eq!(
            body.entries,
            vec![
                ImportEntry {
                    path: String::new(),
                    bytes: None,
                },
                ImportEntry {
                    path: "a.txt".into(),
                    bytes: Some(Bytes(b"hello".to_vec())),
                },
            ]
        );
        explorer.read_with(cx, |explorer, _| {
            // The studio is away: the import waits in the engine's outbox until it answers.
            assert!(explorer.op_error.is_none(), "{:?}", explorer.op_error);
        });
    }
}
