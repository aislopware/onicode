//! AddRemoteProjectDialog.tsx, "Open folder on a machine", on the tailnet: the machine chips are
//! the engines `WatchMachines` lists, and the folder list is that engine's `ListDir`, reached
//! through the app's own engine (`Engine::on`). A path field that holds a name rather than a path
//! searches the folders under the machine's home (`SearchHomeFolders`). Opening adds the folder
//! as a project spelled `peer://<engine>/<path>`, which every view reaches on that engine.
use gpui::EventEmitter;
use onicode_protocol::engine::{MachineEvent, MachineKind, Machines, WatchMachines};
use onicode_protocol::files::{ListDir, PathKind, SearchHomeFolders, Stat};
use onicode_protocol::{EngineId, ProjectRef};

use super::*;
use crate::engine_client::{WatchUpdate, Watching};
use crate::settings::widgets::secondary_button;

/// How many folders a search of the machine's home lists.
const SEARCH_LIMIT: u32 = 50;

pub(super) struct Open {
    picker: Entity<PeerProjectPicker>,
    restore: Option<FocusHandle>,
    _events: Subscription,
}

/// A folder the list shows: its name and its path on the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Folder {
    name: String,
    path: String,
}

/// What the folder list shows: one directory's subfolders, or the folders a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Directory {
    /// The directory listed; `None` for search results.
    path: Option<String>,
    parent: Option<String>,
    entries: Vec<Folder>,
}

enum Picked {
    Open(String),
    Connections,
    Cancel,
}

/// The machines the modal offers: every other engine, by name.
fn engines(machines: &Machines) -> Vec<(EngineId, String)> {
    let mut engines: Vec<_> = machines
        .machines
        .iter()
        .filter(|machine| matches!(machine.kind, MachineKind::Engine))
        .map(|machine| (machine.engine.clone(), machine.name.clone()))
        .collect();
    engines.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    engines
}

/// The folder above `path` on the machine; `None` at its root.
fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let cut = trimmed.rfind('/')?;
    Some(if cut == 0 {
        "/".to_owned()
    } else {
        trimmed[..cut].to_owned()
    })
}

/// The last part of `path`.
fn name_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

/// A listing of `asked` (`~`: the machine's home) as the folder list shows it: subfolders only,
/// by name. The home's own path is read off its children, as the engine names them.
fn directory(asked: &str, entries: Vec<onicode_protocol::files::DirEntry>) -> Directory {
    let path = if asked == "~" {
        entries
            .first()
            .and_then(|entry| parent_of(&entry.path))
            .unwrap_or_else(|| asked.to_owned())
    } else {
        asked.to_owned()
    };
    let mut folders: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.is_dir && !entry.name.starts_with('.'))
        .map(|entry| Folder {
            name: entry.name,
            path: entry.path,
        })
        .collect();
    folders.sort_by(|a, b| onicode_protocol::files::compare_natural_names(&a.name, &b.name));
    Directory {
        parent: path.starts_with('/').then(|| parent_of(&path)).flatten(),
        path: Some(path),
        entries: folders,
    }
}

struct PeerProjectPicker {
    engine: Engine,
    machines: Vec<(EngineId, String)>,
    loaded: bool,
    machine: Option<EngineId>,
    path: Entity<input::Input>,
    directory: Option<Directory>,
    loading: bool,
    opening: bool,
    error: String,
    version: u64,
    focus: FocusHandle,
    _machines: Option<(Watching<Machines, MachineEvent>, gpui::Task<()>)>,
    _events: Vec<Subscription>,
}
impl EventEmitter<Picked> for PeerProjectPicker {}
impl Focusable for PeerProjectPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl PeerProjectPicker {
    fn new(engine: Engine, machine: Option<EngineId>, cx: &mut Context<Self>) -> Self {
        let path = cx.new(|cx| {
            let mut input = input::Input::field(
                "Folder path on the machine",
                "/Users/me/code/my-app",
                content(0.35),
                cx,
            );
            input.set_font_size(12.);
            input
        });
        let events = vec![
            cx.subscribe(&path, |this, _, _: &input::Changed, cx| this.typed(cx)),
            cx.subscribe(&path, |this, _, _: &input::Submitted, cx| this.open(cx)),
        ];
        let mut this = Self {
            focus: cx.focus_handle(),
            engine,
            machines: vec![],
            loaded: false,
            machine,
            path,
            directory: None,
            loading: false,
            opening: false,
            error: String::new(),
            version: 0,
            _machines: None,
            _events: events,
        };
        this.follow_machines(cx);
        this
    }

    /// Follows the engines on the tailnet, so a machine that joins or leaves shows at once.
    fn follow_machines(&mut self, cx: &mut Context<Self>) {
        let mut all = self.engine.machines_now();
        if let Some(machines) = &all {
            self.machines_listed(engines(machines), cx);
        }
        let watching = self
            .engine
            .watch::<WatchMachines>(|since| WatchMachines { since });
        let updates = watching.updates.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                match update {
                    WatchUpdate::Snapshot(machines) => all = Some(*machines),
                    WatchUpdate::Events(events) => {
                        if let Some(machines) = all.as_mut() {
                            for event in &events {
                                machines.apply(event);
                            }
                        }
                    }
                }
                let listed = all.as_ref().map(engines).unwrap_or_default();
                if this
                    .update(cx, |this, cx| this.machines_listed(listed, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self._machines = Some((watching, task));
    }

    fn machines_listed(&mut self, machines: Vec<(EngineId, String)>, cx: &mut Context<Self>) {
        let first = !self.loaded;
        self.loaded = true;
        if machines == self.machines && !first {
            return;
        }
        let before = self.machine().map(|(engine, _)| engine.clone());
        self.machines = machines;
        let now = self.machine().map(|(engine, _)| engine.clone());
        if now.is_some() && (first || now != before) {
            self.directory = None;
            self.browse(None, cx);
        }
        cx.notify();
    }

    fn machine(&self) -> Option<&(EngineId, String)> {
        self.machine
            .as_ref()
            .and_then(|id| self.machines.iter().find(|(engine, _)| engine == id))
            .or_else(|| self.machines.first())
    }

    fn select_machine(&mut self, id: EngineId, cx: &mut Context<Self>) {
        if self.machine().is_some_and(|(engine, _)| *engine == id) {
            return;
        }
        self.machine = Some(id);
        self.directory = None;
        self.path
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.browse(None, cx);
    }

    /// The field moved: a name rather than a path searches the machine's home.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let text = self.path.read(cx).content.trim().to_owned();
        let shown = self
            .directory
            .as_ref()
            .and_then(|directory| directory.path.as_deref());
        if text.is_empty() || text.starts_with('/') || text.starts_with('~') {
            cx.notify();
            return;
        }
        if shown == Some(text.as_str()) {
            return;
        }
        self.search(text, cx);
    }

    fn browse(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Some((machine, _)) = self.machine().cloned() else {
            return;
        };
        let asked = path.unwrap_or_else(|| "~".to_owned());
        let list = self.engine.on(machine).call::<ListDir>(
            None,
            ListDir {
                path: asked.clone(),
            },
        );
        self.version += 1;
        let version = self.version;
        self.loading = true;
        self.opening = false;
        self.error.clear();
        cx.spawn(async move |this, cx| {
            let result = list.await;
            let _ = this.update(cx, |this, cx| {
                if this.version != version {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(entries) => {
                        let directory = directory(&asked, entries);
                        if let Some(path) = directory.path.clone() {
                            this.path.update(cx, |input, cx| input.set_text(path, cx));
                        }
                        this.directory = Some(directory);
                    }
                    Err(error) => this.error = error,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        let Some((machine, _)) = self.machine().cloned() else {
            return;
        };
        let search = self.engine.on(machine).call::<SearchHomeFolders>(
            None,
            SearchHomeFolders {
                query,
                limit: SEARCH_LIMIT,
            },
        );
        self.version += 1;
        let version = self.version;
        self.loading = true;
        self.error.clear();
        cx.spawn(async move |this, cx| {
            let result = search.await;
            let _ = this.update(cx, |this, cx| {
                if this.version != version {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(found) => {
                        this.directory = Some(Directory {
                            path: None,
                            parent: None,
                            entries: found
                                .into_iter()
                                .map(|path| Folder {
                                    name: name_of(&path),
                                    path,
                                })
                                .collect(),
                        });
                    }
                    Err(error) => this.error = error,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        if self.opening {
            return;
        }
        let Some((machine, name)) = self.machine().cloned() else {
            return;
        };
        let path = self.path.read(cx).content.trim().to_owned();
        if path.is_empty() {
            return;
        }
        if !path.starts_with('/') {
            self.error = "Type the folder's full path on the machine, or pick it below.".into();
            cx.notify();
            return;
        }
        let stat = self
            .engine
            .on(machine.clone())
            .call::<Stat>(None, Stat { path: path.clone() });
        self.version += 1;
        let version = self.version;
        self.opening = true;
        self.loading = false;
        self.error.clear();
        cx.spawn(async move |this, cx| {
            let result = stat.await;
            let _ = this.update(cx, |this, cx| {
                if this.version != version {
                    return;
                }
                this.opening = false;
                match result {
                    Ok(stat) if stat.kind == PathKind::Dir => {
                        let project = ProjectRef {
                            engine: machine,
                            path,
                        };
                        cx.emit(Picked::Open(project.uri()));
                    }
                    Ok(_) => this.error = format!("{name} has no folder at {path}."),
                    Err(error) => this.error = error,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn folder_row(
        &self,
        id: usize,
        name: String,
        path: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(("remote-folder", id))
            .role(Role::Button)
            .aria_label(format!("Browse {name}"))
            .w_full()
            .rounded_md()
            .px_2()
            .py(px(6.))
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(content(0.75))
            .cursor_pointer()
            .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
            .child(icon("folder", 14.).text_color(content(0.45)))
            .child(div().min_w_0().flex_1().truncate().child(name))
            .child(icon("chevron-right", 12.).text_color(content(0.3)))
            .on_click(cx.listener(move |this, _, _, cx| this.browse(Some(path.clone()), cx)))
            .into_any_element()
    }
}
impl Render for PeerProjectPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let machine = self.machine().cloned();
        let mut view = div().id("remote-project-picker").debug_selector(|| "remote-project-picker".into()).role(Role::Dialog).aria_label("Open folder on a machine")
            .track_focus(&self.focus)
            .p_4().flex().flex_col().gap_3().min_h_0().text_color(content(1.))
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" { cx.stop_propagation(); cx.emit(Picked::Cancel); }
            }))
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Open folder on a machine"))
            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(content(0.55))
                .child("Sessions in this project run on that machine, using its checkout and its Codex or Claude Code sign-in. They keep running when you close Onicode here."));
        if !self.loaded {
            view = view.child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.45))
                    .child("Loading machines…"),
            );
        } else if let Some((machine, machine_name)) = machine {
            if self.machines.len() > 1 {
                view = view.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(content(0.55))
                                .child("Machine"),
                        )
                        .child(div().flex().flex_wrap().gap_2().children(
                            self.machines.iter().map(|(engine, name)| {
                                let id = engine.clone();
                                secondary_button(
                                    SharedString::from(format!("remote-project-machine-{id}")),
                                    name.clone(),
                                    false,
                                    false,
                                )
                                .when(*engine == machine, |d| d.bg(content(0.12)))
                                .child(icon("globe", 14.))
                                .child(name.clone())
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.select_machine(id.clone(), cx),
                                ))
                            }),
                        )),
                );
            } else {
                view = view.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(content(0.55))
                        .child(icon("globe", 14.))
                        .child(format!("On {machine_name}")),
                );
            }
            let path = self.path.read(cx).content.trim().to_owned();
            view = view.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .h_8()
                            .min_w_0()
                            .flex_1()
                            .rounded_md()
                            .border_1()
                            .border_color(content(0.1))
                            .bg(content(0.03))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .font_family("Menlo")
                            .child(self.path.clone()),
                    )
                    .child(
                        secondary_button(
                            "browse-remote-path",
                            "Browse path",
                            false,
                            !path.starts_with('/') && !path.starts_with('~'),
                        )
                        .child("Browse")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.path.read(cx).content.trim().to_owned();
                            if path.starts_with('/') || path.starts_with('~') {
                                this.browse(Some(path), cx);
                            }
                        })),
                    ),
            );
            let mut folders = div()
                .id("remote-project-folders")
                .role(Role::List)
                .aria_label("Folders")
                .min_h(px(96.))
                .max_h(px(280.))
                .overflow_y_scroll()
                .rounded_md()
                .border_1()
                .border_color(content(0.1))
                .p_1()
                .flex()
                .flex_col();
            if let Some(directory) = &self.directory {
                if let Some(parent) = &directory.parent {
                    folders = folders.child(self.folder_row(0, "..".into(), parent.clone(), cx));
                }
                folders =
                    folders.children(directory.entries.iter().enumerate().map(|(ix, entry)| {
                        self.folder_row(ix + 1, entry.name.clone(), entry.path.clone(), cx)
                    }));
                if directory.entries.is_empty() {
                    folders = folders.child(
                        div()
                            .px_2()
                            .py(px(6.))
                            .text_size(px(12.))
                            .text_color(content(0.45))
                            .child(if directory.path.is_some() {
                                "No subfolders"
                            } else {
                                "No folders match"
                            }),
                    );
                }
            } else if self.loading {
                folders = folders.child(
                    div()
                        .px_2()
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(content(0.45))
                        .child("Loading folders…"),
                );
            }
            view = view.child(folders);
        } else {
            view = view.child(div().text_size(px(12.)).line_height(px(17.)).text_color(content(0.55))
                .child("No machines are on your tailnet yet. Add one in Settings, then open a folder on it here."));
        }
        if !self.error.is_empty() {
            view = view.child(
                div()
                    .id("remote-project-error")
                    .role(Role::Alert)
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .text_color(rgb(0xf87171))
                    .child(self.error.clone()),
            );
        }
        view.child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    crate::modal::ghost_button("cancel-remote-project", "Cancel")
                        .cursor_pointer()
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Picked::Cancel))),
                )
                .when(self.loaded && self.machines.is_empty(), |d| {
                    d.child(
                        crate::modal::primary_button("add-project-machine", "Add a machine")
                            .cursor_pointer()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Picked::Connections))),
                    )
                })
                .when(!self.machines.is_empty(), |d| {
                    d.child(
                        crate::modal::disabled(
                            crate::modal::primary_button(
                                "open-remote-project",
                                if self.opening { "Opening…" } else { "Open" },
                            ),
                            self.opening || self.path.read(cx).content.trim().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.open(cx))),
                    )
                }),
        )
    }
}

impl Shell {
    /// "Open folder on a machine", on `machine` when one is named (a Machines row), else on the
    /// first engine the tailnet lists.
    pub(super) fn open_peer_project(
        &mut self,
        machine: Option<EngineId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restore = self
            .peer_project_picker
            .as_ref()
            .and_then(|o| o.restore.clone())
            .or_else(|| window.focused(cx));
        self.close_menu(cx);
        self.close_preview(cx);
        let engine = self.engine.clone();
        let picker = cx.new(|cx| PeerProjectPicker::new(engine, machine, cx));
        let events = cx.subscribe_in(&picker, window, |this, _, event: &Picked, window, cx| {
            this.close_peer_project(window, cx);
            match event {
                Picked::Open(project) => {
                    this.workspace.add_project(project);
                    this.follow_peers(cx);
                    this.select_project(project.clone(), window, cx);
                }
                Picked::Connections => this.open_add_machine(window, cx),
                Picked::Cancel => {}
            }
            this.invalidate_chrome(cx);
            cx.notify();
        });
        window.focus(&picker.focus_handle(cx), cx);
        self.peer_project_picker = Some(Open {
            picker,
            restore,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn close_peer_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.peer_project_picker.take() {
            if let Some(restore) = open.restore {
                window.focus(&restore, cx);
            } else {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn peer_project_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.peer_project_picker.as_ref()?;
        let viewport = window.viewport_size();
        let width = 480f32.min(viewport.width.as_f32() - 24.);
        let anchor = Anchor::Point(gpui::point(
            px((viewport.width.as_f32() - width) / 2.),
            px((viewport.height.as_f32() * 0.16).max(16.)),
        ));
        let weak = cx.weak_entity();
        Some(
            div()
                .absolute()
                .size_full()
                .child(crate::modal::scrim("remote-project-scrim"))
                .child(
                    popover("remote-project-modal", anchor)
                        .modal_surface(self.workspace.boxy_layout())
                        .width(width)
                        .max_height(viewport.height.as_f32() * 0.7)
                        .gap(0.)
                        .padding(0.)
                        .priority(80)
                        .on_dismiss(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| this.close_peer_project(window, cx));
                        })
                        .child(open.picker.clone()),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use onicode_protocol::files::DirEntry;

    use super::*;

    fn entry(path: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name_of(path),
            path: path.into(),
            is_dir,
            ignored: false,
        }
    }

    /// The home's listing names the home by its children's paths; only visible folders are
    /// listed, by name, under a row up to the parent.
    #[test]
    fn a_listing_shows_folders_under_their_parent() {
        let listed = directory(
            "~",
            vec![
                entry("/Users/me/notes.txt", false),
                entry("/Users/me/code", true),
                entry("/Users/me/.config", true),
                entry("/Users/me/Archive 2", true),
            ],
        );
        assert_eq!(listed.path.as_deref(), Some("/Users/me"));
        assert_eq!(listed.parent.as_deref(), Some("/Users"));
        let names: Vec<_> = listed.entries.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Archive 2", "code"]);
        let root = directory("/", Vec::new());
        assert_eq!(root.parent, None);
        assert_eq!(parent_of("/Users"), Some("/".into()));
        assert_eq!(name_of("/Users/me/code/"), "code");
    }

    #[gpui::test]
    fn escape_closes_the_folder_browser_without_switching_projects(cx: &mut gpui::TestAppContext) {
        let (window, _, _) = crate::shell::tests::window(cx);
        let previous = window
            .update(cx, |shell, window, cx| {
                let previous = shell.workspace.project.clone();
                shell.open_peer_project(None, window, cx);
                assert!(shell.peer_project_picker.is_some());
                previous
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("remote-project-picker").is_some());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.peer_project_picker.is_none());
                assert_eq!(shell.workspace.project, previous);
            })
            .unwrap();
    }
}
