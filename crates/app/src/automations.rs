//! Native counterpart of AutomationsView: persisted list, example gallery,
//! trigger editor and run history.
//!
//! An automation belongs to the engine that holds its project, which stores, schedules and runs
//! it (engine.md, "Automations"). The view lists the automations of the app's own engine and of
//! every engine a `peer://` project names together, each in the app's spelling (its project's
//! URI, so its row shows that machine's tile as every peer project does), and sends every edit,
//! run and history read to the engine its project names. An edit shows at once and stays laid
//! over each engine's list until that engine answers; a peer that cannot be reached keeps its
//! automations as last listed, and an edit for it waits in the engine's outbox.
use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Role, SharedString,
    Subscription, Window, div, px, rgb,
};
use onicode_core::automations;
use onicode_protocol::agents::{Catalog, CatalogRead};
use onicode_protocol::automations::{
    Automation, AutomationRun, AutomationTrigger, AutomationUpsert,
};
use onicode_protocol::provider::Kind;
use onicode_protocol::session_groups::Folder;
use onicode_protocol::work::{
    AutomationRuns, DeleteAutomation, ListAutomations, ProjectGroups, RunAutomationNow,
    SetAutomationEnabled, UpsertAutomation,
};
use onicode_protocol::{EngineId, OpId, ProjectRef};
use serde::Deserialize;

use crate::engine_client::{Engine, call};
use crate::input::{self, Input};
use crate::pickers::{ModelList, ModelPicked, ProjectPicked, ProjectPicker, cached_model_name};
use crate::popover::{Anchor, AnchorSlot, popover};
use crate::theme::*;

mod editor;
mod gallery;
mod trigger_menu;

/// Event providers and schedules are separate from agent harness branding.
fn trigger_mark(kind: &str, side: f32) -> AnyElement {
    let (path, color) = match kind {
        "time" => ("clock", content(0.45)),
        "github" => ("inbox-github", content(0.65)),
        _ => ("zap", content(0.45)),
    };
    icon(path, side).text_color(color).into_any_element()
}

pub(crate) enum Event {
    Close,
    /// Open the session the engine (`None`, the app's own) keeps under this key: a run's.
    OpenSession(Option<EngineId>, String),
}

/// An edit made ahead of its engine's answer, laid over every list until the call answers.
enum Echo {
    Saved(Box<Automation>),
    Enabled(String, bool),
    Deleted(String),
}

struct Pending {
    op: OpId,
    echo: Echo,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Template {
    id: String,
    category: String,
    #[serde(default)]
    popular: bool,
    icon: String,
    name: String,
    description: String,
    prompt: String,
    trigger: serde_json::Value,
    trigger_label: String,
}
struct TriggerFields {
    id: String,
    time: Entity<Input>,
    repos: Entity<Input>,
    actor: Entity<Input>,
}
struct Editor {
    draft: AutomationUpsert,
    name: Entity<Input>,
    prompt: Entity<Input>,
    completion: Entity<crate::composer_completion::Completion>,
    fields: Vec<TriggerFields>,
    saved: bool,
    dirty: bool,
    _subscriptions: Vec<Subscription>,
}
#[derive(Clone)]
enum Choice {
    Access,
    Workspace,
    Conversation,
    Folder,
    Grace,
    AddTrigger,
    TriggerEvent(String),
    Schedule(String),
    Time(String),
    Minute(String),
    Weekday(String),
    ModelSetting(String),
}
struct ChoiceMenu {
    field: Choice,
    options: Vec<(String, String)>,
    anchor: AnchorSlot,
}

pub(crate) struct Automations {
    engine: Engine,
    cwd: String,
    projects: Vec<String>,
    appearances: HashMap<String, onicode_protocol::project_appearance::ProjectAppearance>,
    /// Every engine's automations as shown: each engine's last list, the edits in flight laid
    /// over them, newest first.
    automations: Vec<Automation>,
    /// Each engine's automations as it last listed them, in the app's spelling.
    listed: HashMap<Option<EngineId>, Vec<Automation>>,
    pending: Vec<Pending>,
    runs: Vec<AutomationRun>,
    selected: Option<String>,
    editor: Option<Editor>,
    loading: bool,
    error: Option<String>,
    /// The automations whose save, or whose run now, waits on its engine.
    saving: HashSet<String>,
    running: HashSet<String>,
    gallery: bool,
    category: String,
    templates: Vec<Template>,
    filter: Entity<Input>,
    focus: FocusHandle,
    _filter_subscription: Subscription,
    history: bool,
    advanced: bool,
    confirm_delete: bool,
    folders: Vec<Folder>,
    choice: Option<ChoiceMenu>,
    models: Option<Entity<ModelList>>,
    model_controls: Vec<onicode_protocol::catalog::ModelControl>,
    project_picker: Option<Entity<ProjectPicker>>,
    picker_subscription: Option<Subscription>,
    project_anchor: AnchorSlot,
    model_anchor: AnchorSlot,
    trigger_anchor: AnchorSlot,
    trigger_menu: Option<Entity<trigger_menu::TriggerMenu>>,
    trigger_subscription: Option<Subscription>,
    github_connected: bool,
    request_generation: u64,
}
impl EventEmitter<Event> for Automations {}
impl Focusable for Automations {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Automations {
    pub(crate) fn new(
        engine: Engine,
        cwd: String,
        projects: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::inbox::start_relative_clock(cx);
        let filter =
            cx.new(|cx| Input::field("Filter automations", "Filter automations", content(0.4), cx));
        let subscription = cx.subscribe(&filter, |_, _, _: &input::Changed, cx| cx.notify());
        let mut this = Self {
            engine,
            cwd,
            projects,
            appearances: HashMap::new(),
            automations: Vec::new(),
            listed: HashMap::new(),
            pending: Vec::new(),
            runs: Vec::new(),
            selected: None,
            editor: None,
            loading: true,
            error: None,
            saving: HashSet::new(),
            running: HashSet::new(),
            gallery: true,
            category: "popular".into(),
            templates: serde_json::from_str(include_str!("automations/templates.json"))
                .unwrap_or_default(),
            filter,
            focus: cx.focus_handle(),
            _filter_subscription: subscription,
            history: false,
            advanced: false,
            confirm_delete: false,
            folders: Vec::new(),
            choice: None,
            models: None,
            model_controls: Vec::new(),
            project_picker: None,
            picker_subscription: None,
            project_anchor: AnchorSlot::default(),
            model_anchor: AnchorSlot::default(),
            trigger_anchor: AnchorSlot::default(),
            trigger_menu: None,
            trigger_subscription: None,
            github_connected: false,
            request_generation: 0,
        };
        this.refresh(cx);
        this.load_folders(cx);
        this
    }

    pub(crate) fn set_appearances(
        &mut self,
        appearances: HashMap<String, onicode_protocol::project_appearance::ProjectAppearance>,
        cx: &mut Context<Self>,
    ) {
        self.appearances = appearances;
        cx.notify();
    }

    fn project_label(&self, path: &str) -> String {
        self.appearances
            .get(path)
            .map_or_else(|| project_name(path), |appearance| appearance.label(path))
    }

    fn project_mark(&self, path: &str, side: f32) -> AnyElement {
        crate::project_settings::project_icon(
            &self.appearances.get(path).cloned().unwrap_or_default(),
            path,
            side,
        )
    }

    pub(crate) fn set_context(
        &mut self,
        cwd: String,
        projects: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.cwd = cwd;
        self.projects = projects;
        self.load_folders(cx);
        cx.notify();
    }

    fn show_gallery(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self.selected = None;
        self.runs.clear();
        self.gallery = true;
        self.history = false;
        self.confirm_delete = false;
        self.choice = None;
        self.trigger_menu = None;
        self.models = None;
        self.project_picker = None;
        self.error = None;
        self.request_generation += 1;
        cx.notify();
    }

    /// The engines whose automations the view lists: the app's own, and each one a project of
    /// the workspace names.
    fn engines(&self) -> Vec<Engine> {
        let mut peers: Vec<EngineId> = Vec::new();
        for project in self
            .projects
            .iter()
            .filter_map(|path| ProjectRef::parse_uri(path))
        {
            if !peers.contains(&project.engine) {
                peers.push(project.engine);
            }
        }
        std::iter::once(self.engine.clone())
            .chain(peers.into_iter().map(|peer| self.engine.on(peer)))
            .collect()
    }

    /// The engine that holds the automation `id`: the one its project names.
    fn owner(&self, id: &str) -> Option<Engine> {
        let automation = self.automations.iter().find(|a| a.id == id)?;
        Some(self.engine.for_path(&automation.cwd).0)
    }

    /// Asks every engine for its automations, and the selected one's engine for its runs. An
    /// engine that does not answer keeps what it last listed.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let engines = self.engines();
        let wanted: Vec<Option<EngineId>> = engines.iter().map(|e| e.target().cloned()).collect();
        self.listed.retain(|engine, _| wanted.contains(engine));
        for engine in engines {
            let target = engine.target().cloned();
            let work = call::<ListAutomations>(&engine, None, ListAutomations {}, cx);
            cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(list) => this.listed(target, list, cx),
                    Err(error) => {
                        if target.is_none() {
                            this.loading = false;
                            this.error = Some(error);
                            cx.notify();
                        }
                    }
                });
            })
            .detach();
        }
        let Some(selected) = self.selected.clone() else {
            return;
        };
        let Some(engine) = self.owner(&selected) else {
            return;
        };
        let work = call::<AutomationRuns>(
            &engine,
            None,
            AutomationRuns {
                automation: selected.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(runs) = result
                    && this.selected.as_ref() == Some(&selected)
                {
                    this.runs = runs;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// What `engine` (`None`, the app's own) listed, spelled as the app spells its paths.
    pub(crate) fn listed(
        &mut self,
        engine: Option<EngineId>,
        list: Vec<Automation>,
        cx: &mut Context<Self>,
    ) {
        let on = match &engine {
            Some(peer) => self.engine.on(peer.clone()),
            None => {
                self.loading = false;
                self.engine.clone()
            }
        };
        let list = list
            .into_iter()
            .map(|automation| spelled(&on, automation))
            .collect();
        self.listed.insert(engine, list);
        self.show_listed(cx);
    }

    /// Lays the edits in flight over every engine's last list, newest first; a selection whose
    /// automation went leaves the editor unless it holds changes.
    fn show_listed(&mut self, cx: &mut Context<Self>) {
        let mut shown: Vec<Automation> = Vec::new();
        for automation in self.listed.values().flatten() {
            // An automation moving between engines is listed by both until the old one deletes
            // it: the newer copy shows.
            match shown.iter_mut().find(|known| known.id == automation.id) {
                Some(known) if known.updated_at < automation.updated_at => {
                    *known = automation.clone();
                }
                Some(_) => {}
                None => shown.push(automation.clone()),
            }
        }
        for pending in &self.pending {
            match &pending.echo {
                Echo::Saved(saved) => {
                    shown.retain(|known| known.id != saved.id);
                    shown.push((**saved).clone());
                }
                Echo::Enabled(id, enabled) => {
                    if let Some(known) = shown.iter_mut().find(|known| known.id == *id) {
                        known.enabled = *enabled;
                    }
                }
                Echo::Deleted(id) => shown.retain(|known| known.id != *id),
            }
        }
        shown.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        self.automations = shown;
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.automations.iter().any(|a| &a.id == id))
        {
            self.selected = None;
            if !self.editor.as_ref().is_some_and(|e| e.dirty) {
                self.editor = None;
                self.gallery = true;
            }
        }
        cx.notify();
    }

    /// Settles the edit made under `op`: its engine answered, with `saved` when it holds the
    /// automation as saved now, or `gone` when it deleted it.
    fn settle(
        &mut self,
        op: OpId,
        engine: Option<EngineId>,
        saved: Option<Automation>,
        gone: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.pending.retain(|pending| pending.op != op);
        let on = match &engine {
            Some(peer) => self.engine.on(peer.clone()),
            None => self.engine.clone(),
        };
        let list = self.listed.entry(engine).or_default();
        if let Some(id) = gone {
            list.retain(|known| known.id != id);
        }
        if let Some(saved) = saved {
            let saved = spelled(&on, saved);
            list.retain(|known| known.id != saved.id);
            list.push(saved);
        }
        self.show_listed(cx);
    }

    fn load_folders(&mut self, cx: &mut Context<Self>) {
        let cwd = self
            .editor
            .as_ref()
            .map(|e| e.draft.cwd.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let expected = cwd.clone();
        let (engine, project) = self.engine.for_path(&cwd);
        let work = call::<ProjectGroups>(&engine, None, ProjectGroups { project }, cx);
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .editor
                    .as_ref()
                    .map(|e| e.draft.cwd.as_str())
                    .unwrap_or(&this.cwd)
                    == expected
                {
                    this.folders = result.map(|g| g.folders).unwrap_or_default();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn begin(&mut self, template: Option<Template>, cx: &mut Context<Self>) {
        let preferences = cx
            .try_global::<crate::settings::Preferences>()
            .map(|preferences| &preferences.engine);
        let kind = preferences
            .and_then(|p| p.project_providers.get(&self.cwd))
            .and_then(|p| p.provider.as_deref())
            .or_else(|| preferences.and_then(|p| p.default_provider.as_deref()))
            .map(Kind::from_id)
            .unwrap_or(Kind::Codex);
        let model = preferences
            .and_then(|p| p.project_providers.get(&self.cwd))
            .and_then(|p| p.models.get(kind.id()))
            .or_else(|| preferences.and_then(|p| p.default_models.get(kind.id())))
            .cloned()
            .unwrap_or_default();
        let mut draft = AutomationUpsert::new(self.cwd.clone(), kind.id().into(), model);
        if let Some(template) = template {
            draft.name = template.name;
            draft.prompt = template.prompt;
            let kind = template.trigger["kind"].as_str().unwrap_or("time");
            let event = template.trigger["event"].as_str().unwrap_or("weekdays");
            let mut trigger = AutomationTrigger::new(kind, event);
            if let Some(value) = template.trigger["time"].as_str() {
                trigger.time = value.into();
            }
            if let Some(value) = template.trigger["dayOfWeek"].as_i64() {
                trigger.day_of_week = value;
            }
            if let Some(value) = template.trigger["minute"].as_i64() {
                trigger.minute = value;
            }
            draft.triggers = Some(vec![trigger]);
        }
        self.selected = None;
        self.runs.clear();
        self.set_editor(draft, false, cx);
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(automation) = self.automations.iter().find(|a| a.id == id) else {
            return;
        };
        let draft = AutomationUpsert::from_automation(automation);
        self.selected = Some(id);
        self.runs.clear();
        self.set_editor(draft, true, cx);
        self.refresh(cx);
    }

    fn set_editor(&mut self, draft: AutomationUpsert, saved: bool, cx: &mut Context<Self>) {
        let name = cx.new(|cx| {
            let mut input = Input::automation_name(cx);
            input.set_text(draft.name.clone(), cx);
            input
        });
        let prompt = cx.new(|cx| {
            let mut input = Input::automation_instructions(cx);
            input.set_text(draft.prompt.clone(), cx);
            input
        });
        let completion = cx.new(|cx| {
            crate::composer_completion::Completion::new(prompt.clone(), draft.cwd.clone(), cx)
                .inline_field()
        });
        completion.update(cx, |completion, cx| {
            completion.set_provider(&draft.harness, cx)
        });
        let mut subscriptions = Vec::new();
        for field in [&name, &prompt] {
            subscriptions.push(cx.subscribe(field, |this, _, _: &input::Changed, cx| {
                if let Some(editor) = &mut this.editor {
                    editor.dirty = true;
                }
                cx.notify();
            }));
        }
        self.editor = Some(Editor {
            draft,
            name,
            prompt,
            completion,
            fields: Vec::new(),
            saved,
            dirty: !saved,
            _subscriptions: subscriptions,
        });
        self.rebuild_trigger_fields(cx);
        self.gallery = false;
        self.history = false;
        self.confirm_delete = false;
        self.choice = None;
        self.trigger_menu = None;
        self.models = None;
        self.project_picker = None;
        self.error = None;
        self.load_folders(cx);
        self.load_controls(cx);
        cx.notify();
    }

    fn rebuild_trigger_fields(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let triggers = editor.draft.triggers.clone().unwrap_or_default();
        editor
            .fields
            .retain(|fields| triggers.iter().any(|t| t.id == fields.id));
        for trigger in triggers {
            if editor.fields.iter().any(|f| f.id == trigger.id) {
                continue;
            }
            let field = |label: &str, placeholder: &str, text: String, cx: &mut Context<Self>| {
                cx.new(|cx| {
                    let mut input = Input::field(
                        label.to_string(),
                        placeholder.to_string(),
                        content(0.35),
                        cx,
                    );
                    input.set_text(text, cx);
                    input
                })
            };
            let time = field("Trigger time", "09:00", trigger.time, cx);
            let repos = field(
                "Trigger repositories",
                "Any repository",
                trigger
                    .repos
                    .iter()
                    .chain(std::iter::once(&trigger.repo))
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                cx,
            );
            let actor = field("Trigger actor", "Anyone", trigger.actor, cx);
            for input in [&time, &repos, &actor] {
                editor._subscriptions.push(cx.subscribe(
                    input,
                    |this, _, _: &input::Changed, cx| {
                        if let Some(e) = &mut this.editor {
                            e.dirty = true;
                        }
                        cx.notify();
                    },
                ));
            }
            editor.fields.push(TriggerFields {
                id: trigger.id,
                time,
                repos,
                actor,
            });
        }
    }

    fn collected(&self, cx: &App) -> Option<AutomationUpsert> {
        let editor = self.editor.as_ref()?;
        let mut draft = editor.draft.clone();
        draft.name = editor.name.read(cx).content.trim().to_string();
        // Runs later without the picker's index, so save full mention paths.
        draft.prompt = editor
            .completion
            .read(cx)
            .expand_mentions(&editor.prompt.read(cx).content);
        for trigger in draft.triggers.get_or_insert_with(Vec::new) {
            if let Some(fields) = editor.fields.iter().find(|f| f.id == trigger.id) {
                trigger.time = fields.time.read(cx).content.trim().to_string();
                trigger.repo.clear();
                trigger.repos = fields
                    .repos
                    .read(cx)
                    .content
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                trigger.actor = fields.actor.read(cx).content.trim().to_string();
            }
        }
        draft.sync_triggers(onicode_protocol::time::now_ms() as i64);
        Some(draft)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.collected(cx) else {
            return;
        };
        if self.saving.contains(&draft.id) {
            return;
        }
        self.error = None;
        let previous = self.automations.iter().find(|a| a.id == draft.id);
        let echo = echoed(&draft, previous);
        // Moved to a project on another engine: that engine takes it, then the old one lets go.
        let moved_from = previous
            .map(|previous| self.engine.for_path(&previous.cwd).0)
            .filter(|old| old.target() != self.engine.for_path(&draft.cwd).0.target());
        let (engine, body) = self.routed(draft.clone());
        let target = engine.target().cloned();
        let op = OpId::new();
        self.pending.push(Pending {
            op,
            echo: Echo::Saved(Box::new(echo.clone())),
        });
        self.saving.insert(draft.id.clone());
        self.selected = Some(draft.id.clone());
        self.set_editor(AutomationUpsert::from_automation(&echo), true, cx);
        self.show_listed(cx);
        let work =
            call::<UpsertAutomation>(&engine, Some(op), UpsertAutomation { automation: body }, cx);
        let id = draft.id;
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.saving.remove(&id);
                match result {
                    Ok(saved) => {
                        let shown = spelled(&engine, saved.clone());
                        if let Some(editor) = &this.editor
                            && editor.draft.id == shown.id
                            && !editor.dirty
                        {
                            this.set_editor(AutomationUpsert::from_automation(&shown), true, cx);
                        }
                        this.settle(op, target, Some(saved), None, cx);
                        if let Some(old) = moved_from {
                            this.forget(&old, id.clone(), cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.settle(op, target, None, None, cx);
                    }
                }
                this.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// `draft` as the engine its project names takes it: that engine, and the draft with its
    /// project and checkout spelled as it names them.
    fn routed(&self, mut draft: AutomationUpsert) -> (Engine, AutomationUpsert) {
        let (engine, cwd) = self.engine.for_path(&draft.cwd);
        draft.cwd = cwd;
        if !draft.worktree_cwd.is_empty() {
            draft.worktree_cwd = engine.path_of(&draft.worktree_cwd);
        }
        (engine, draft)
    }

    /// Deletes the automation `id` from `engine`, which held it before it moved elsewhere.
    fn forget(&mut self, engine: &Engine, id: String, cx: &mut Context<Self>) {
        let target = engine.target().cloned();
        let op = OpId::new();
        let work = call::<DeleteAutomation>(
            engine,
            Some(op),
            DeleteAutomation {
                automation: id.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(error);
                }
                if let Some(list) = this.listed.get_mut(&target) {
                    list.retain(|known| known.id != id);
                }
                this.show_listed(cx);
            });
        })
        .detach();
    }

    fn toggle(&mut self, id: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.owner(&id) else {
            return;
        };
        let target = engine.target().cloned();
        let op = OpId::new();
        self.pending.push(Pending {
            op,
            echo: Echo::Enabled(id.clone(), enabled),
        });
        if let Some(editor) = &mut self.editor
            && editor.draft.id == id
            && !editor.dirty
        {
            editor.draft.enabled = enabled;
        }
        self.show_listed(cx);
        let work = call::<SetAutomationEnabled>(
            &engine,
            Some(op),
            SetAutomationEnabled {
                automation: id,
                enabled,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(saved) => {
                        if let Some(editor) = &mut this.editor
                            && editor.draft.id == saved.id
                            && !editor.dirty
                        {
                            editor.draft.enabled = saved.enabled;
                        }
                        this.settle(op, target, Some(saved), None, cx);
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.settle(op, target, None, None, cx);
                    }
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(automation) = self
            .selected
            .as_ref()
            .and_then(|id| self.automations.iter().find(|a| &a.id == id))
            .map(|automation| automation.id.clone())
        else {
            return;
        };
        if self.running.contains(&automation) {
            return;
        }
        let Some(engine) = self.owner(&automation) else {
            return;
        };
        let target = engine.target().cloned();
        self.running.insert(automation.clone());
        let work = call::<RunAutomationNow>(
            &engine,
            Some(OpId::new()),
            RunAutomationNow {
                automation: automation.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.running.remove(&automation);
                match result {
                    // The engine runs it; the person sees it where it runs.
                    Ok(run) => match (run.session_id, run.error) {
                        (Some(key), _) if run.status != "failed" => {
                            cx.emit(Event::OpenSession(target, key));
                        }
                        (_, Some(error)) => this.error = Some(error),
                        _ => {}
                    },
                    Err(error) => this.error = Some(error),
                }
                this.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Whether the open automation's run now waits on its engine.
    fn starting(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|id| self.running.contains(id))
    }

    /// Whether the open automation's save waits on its engine.
    fn saving_open(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|editor| self.saving.contains(&editor.draft.id))
    }

    /// Asks with the standard confirmation before deleting the open automation.
    fn ask_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = window.prompt(
            gpui::PromptLevel::Warning,
            "Delete this automation?",
            Some("Its run history will be deleted too."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if prompt.await == Ok(0) {
                let _ = this.update(cx, |this, cx| this.delete(cx));
            }
        })
        .detach();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(engine) = self.owner(&id) else {
            return;
        };
        let target = engine.target().cloned();
        let op = OpId::new();
        self.pending.push(Pending {
            op,
            echo: Echo::Deleted(id.clone()),
        });
        self.confirm_delete = false;
        self.editor = None;
        self.selected = None;
        self.gallery = true;
        self.runs.clear();
        self.show_listed(cx);
        let work = call::<DeleteAutomation>(
            &engine,
            Some(op),
            DeleteAutomation {
                automation: id.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.settle(op, target, None, Some(id.as_str()), cx),
                    Err(error) => {
                        this.error = Some(error);
                        this.settle(op, target, None, None, cx);
                    }
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn load_controls(&mut self, cx: &mut Context<Self>) {
        self.model_controls.clear();
        self.request_generation += 1;
        let generation = self.request_generation;
        let Some(editor) = &self.editor else {
            return;
        };
        let kind = Kind::from_id(&editor.draft.harness);
        let model =
            onicode_protocol::sessions::native_model(&editor.draft.model, &editor.draft.harness)
                .unwrap_or_default();
        // The project's engine lists its agents' models; another machine's accounts are its own.
        let engine = self.engine.for_path(&editor.draft.cwd).0;
        let profile = crate::accounts::get(cx)
            .filter(|_| engine.target().is_none())
            .map(|a| a.read(cx).selected(kind, &editor.draft.cwd))
            .unwrap_or_default();
        let work = call::<Catalog>(
            &engine,
            None,
            Catalog {
                provider: kind,
                account: profile.id,
                read: CatalogRead::Fresh,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                if let Ok(Some(models)) = result
                    && let Some(model) = models
                        .iter()
                        .find(|m| m.id == model)
                        .or_else(|| models.iter().find(|m| m.default))
                {
                    this.model_controls = model.controls.clone();
                    if !model.efforts.is_empty()
                        && !this.model_controls.iter().any(|c| {
                            matches!(
                                c.id.as_str(),
                                "reasoningEffort" | "reasoning_effort" | "effort" | "reasoning"
                            )
                        })
                    {
                        this.model_controls
                            .push(onicode_protocol::catalog::ModelControl {
                                id: "reasoningEffort".into(),
                                name: "Reasoning".into(),
                                current: model
                                    .default_effort
                                    .clone()
                                    .unwrap_or_else(|| "medium".into()),
                                choices: model
                                    .efforts
                                    .iter()
                                    .map(|e| (e.clone(), e.clone()))
                                    .collect(),
                            });
                    }
                    if !model.tiers.is_empty()
                        && !this
                            .model_controls
                            .iter()
                            .any(|c| c.id == "serviceTier" || c.id == "service_tier")
                    {
                        let mut choices = vec![("default".into(), "Standard".into())];
                        choices.extend(model.tiers.clone());
                        this.model_controls
                            .push(onicode_protocol::catalog::ModelControl {
                                id: "serviceTier".into(),
                                name: "Speed".into(),
                                current: "default".into(),
                                choices,
                            });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let kind = Kind::from_id(&editor.draft.harness);
        let profile = crate::accounts::get(cx)
            .map(|a| a.read(cx).selected(kind, &editor.draft.cwd))
            .unwrap_or_default();
        let picker = cx.new(|cx| {
            ModelList::new_for(
                kind,
                onicode_protocol::sessions::native_model(
                    &editor.draft.model,
                    &editor.draft.harness,
                ),
                &editor.draft.cwd,
                profile,
                cx,
            )
        });
        self.picker_subscription = Some(cx.subscribe_in(
            &picker,
            window,
            |this, _, picked: &ModelPicked, window, cx| {
                if let Some(editor) = &mut this.editor {
                    let current = onicode_protocol::sessions::native_model(
                        &editor.draft.model,
                        &editor.draft.harness,
                    );
                    let picked_model = picked.1.as_deref().and_then(|model| {
                        onicode_protocol::sessions::native_model(model, picked.0.id())
                    });
                    if Kind::from_id(&editor.draft.harness) != picked.0 || current != picked_model {
                        editor.draft.harness = picked.0.id().into();
                        editor.draft.model = picked.1.clone().unwrap_or_default();
                        editor.completion.update(cx, |completion, cx| {
                            completion.set_provider(picked.0.id(), cx)
                        });
                        editor.dirty = true;
                    }
                }
                this.models = None;
                this.load_controls(cx);
                window.focus(&this.focus, cx);
                cx.notify();
            },
        ));
        window.focus(&picker.read(cx).search_focus(cx), cx);
        self.models = Some(picker);
        self.choice = None;
        self.trigger_menu = None;
        cx.notify();
    }

    fn choose_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let picker = cx.new(|cx| {
            ProjectPicker::new(&self.projects, &editor.draft.cwd, cx)
                .appearance(self.appearances.clone())
        });
        self.picker_subscription = Some(cx.subscribe_in(
            &picker,
            window,
            |this, _, picked: &ProjectPicked, window, cx| {
                this.project_picker = None;
                match picked {
                    ProjectPicked::Project(path) => {
                        if let Some(editor) = &mut this.editor {
                            editor.draft.cwd = path.clone();
                            editor.draft.session_folder_id.clear();
                            editor
                                .completion
                                .update(cx, |completion, cx| completion.set_cwd(path, cx));
                            editor.dirty = true;
                        }
                        this.load_folders(cx);
                        this.load_controls(cx);
                    }
                    ProjectPicked::New => this.browse_project(cx),
                    // Automations' picker never offers machines.
                    ProjectPicked::Remote => {}
                }
                window.focus(&this.focus, cx);
                cx.notify();
            },
        ));
        window.focus(&picker.read(cx).search_focus(cx), cx);
        self.project_picker = Some(picker);
        self.choice = None;
        self.trigger_menu = None;
        cx.notify();
    }

    fn browse_project(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose project".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = prompt.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Ok(Some(paths))) = result
                    && let Some(path) = paths.first()
                {
                    let path = path.to_string_lossy().to_string();
                    if !this.projects.contains(&path) {
                        this.projects.push(path.clone());
                    }
                    if let Some(e) = &mut this.editor {
                        e.draft.cwd = path;
                        e.completion
                            .update(cx, |completion, cx| completion.set_cwd(&e.draft.cwd, cx));
                        e.draft.session_folder_id.clear();
                        e.dirty = true;
                    }
                    this.load_folders(cx);
                    this.load_controls(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// What a shell test reads and does through the view, as the person would.
#[cfg(test)]
impl Automations {
    pub(crate) fn shown(&self) -> &[Automation] {
        &self.automations
    }

    /// Opens automation `id`, gives it `name` and saves it.
    pub(crate) fn rename_for_test(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.select(id.to_owned(), cx);
        if let Some(editor) = &self.editor {
            editor
                .name
                .update(cx, |input, cx| input.set_text(name.to_owned(), cx));
        }
        self.save(cx);
    }

    pub(crate) fn toggle_for_test(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        self.toggle(id.to_owned(), enabled, cx);
    }
}

/// `automation`, as `engine` listed it, in the app's spelling: its project and checkout under
/// that engine's URI when it is another machine's.
fn spelled(engine: &Engine, mut automation: Automation) -> Automation {
    automation.cwd = engine.app_path(&automation.cwd);
    if !automation.worktree_cwd.is_empty() {
        automation.worktree_cwd = engine.app_path(&automation.worktree_cwd);
    }
    automation
}

/// The automation `draft` saves as, shown until its engine answers: what it last ran keeps.
fn echoed(draft: &AutomationUpsert, previous: Option<&Automation>) -> Automation {
    let now = onicode_protocol::automations::now_millis();
    Automation {
        id: draft.id.clone(),
        name: draft.name.trim().to_string(),
        prompt: draft.prompt.clone(),
        harness: draft.harness.clone(),
        model: draft.model.clone(),
        model_settings: draft.model_settings.clone(),
        cwd: draft.cwd.clone(),
        workspace_mode: draft.workspace_mode.clone(),
        worktree_cwd: if draft.workspace_mode == "existing" {
            draft.worktree_cwd.trim().to_string()
        } else {
            String::new()
        },
        session_folder_id: draft.session_folder_id.trim().to_string(),
        reuse_session: draft.reuse_session,
        runtime_mode: draft.runtime_mode.clone(),
        trigger_kind: draft.trigger_kind.clone(),
        trigger_event: draft.trigger_event.trim().to_string(),
        schedule_kind: draft.schedule_kind.clone(),
        minute: draft.minute,
        time: draft.time.clone(),
        day_of_week: draft.day_of_week,
        triggers: Some(onicode_protocol::automations::normalize_triggers(draft)),
        missed_run_grace_minutes: draft.missed_run_grace_minutes,
        enabled: draft.enabled,
        next_run_at: draft.next_run_at,
        last_run_at: previous.and_then(|p| p.last_run_at),
        last_run_status: previous.and_then(|p| p.last_run_status.clone()),
        last_run_error: previous.and_then(|p| p.last_run_error.clone()),
        last_session_id: previous.and_then(|p| p.last_session_id.clone()),
        created_at: previous.map_or(now, |p| p.created_at),
        updated_at: now,
    }
}

fn project_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.into())
}
fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .px(px(12.))
        .h(px(28.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .text_size(px(12.))
        .text_color(content(0.8))
        .border_1()
        .border_color(content(0.15))
        .hover(|s| s.bg(content(0.1)).border_color(content(0.3)))
        .child(label)
}
impl Render for Automations {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.filter.read(cx).content.to_lowercase();
        let visible: Vec<_> = self
            .automations
            .iter()
            .filter(|a| {
                format!("{} {} {}", a.name, a.prompt, a.cwd)
                    .to_lowercase()
                    .contains(query.trim())
            })
            .cloned()
            .collect();
        let mut list = div()
            .id("automations-list")
            .role(Role::ListBox)
            .aria_label("Automations")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(6.))
            .flex()
            .flex_col()
            .gap(px(2.));
        if visible.is_empty() {
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(32.))
                    .text_size(px(12.))
                    .text_color(content(0.45))
                    .child(if self.loading {
                        "Loading automations…"
                    } else if query.trim().is_empty() {
                        "No automations yet"
                    } else {
                        "No matching automations"
                    }),
            );
        }
        for automation in visible {
            let id = automation.id.clone();
            let toggle_id = id.clone();
            let enabled = automation.enabled;
            let active = !self.gallery && self.selected.as_deref() == Some(&id);
            let kind = Kind::from_id(&automation.harness);
            let model =
                onicode_protocol::sessions::native_model(&automation.model, &automation.harness);
            list = list.child(
                div()
                    .id(SharedString::from(format!("automation-{id}")))
                    .relative()
                    .rounded(px(6.))
                    .when(active, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.05)))
                    .child(
                        div()
                            .id(SharedString::from(format!("open-{id}")))
                            .role(Role::Button)
                            .aria_label(format!("Open {}", automation.name))
                            .px(px(10.))
                            .py(px(8.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .pr(px(36.))
                                    .truncate()
                                    .text_size(px(10.))
                                    .text_color(content(0.45))
                                    .child(automation.trigger_label()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(automation.name),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .text_size(px(11.))
                                    .text_color(content(0.45))
                                    .child(self.project_mark(&automation.cwd, 12.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .child(self.project_label(&automation.cwd)),
                                    )
                                    .child(provider_icon(kind.id(), 13.))
                                    .child(
                                        div()
                                            .max_w(px(100.))
                                            .truncate()
                                            .child(cached_model_name(kind, model.as_deref())),
                                    ),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)),
                            ),
                    )
                    .child(
                        div().absolute().right(px(10.)).top(px(8.)).child(
                            editor::toggle(
                                SharedString::from(format!("toggle-{toggle_id}")),
                                enabled,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.toggle(toggle_id.clone(), !enabled, cx),
                            )),
                        ),
                    ),
            );
        }
        let aside = div()
            .w(px(280.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(content(0.07))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(36.))
                    .px(px(8.))
                    .border_b_1()
                    .border_color(content(0.07))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(icon("search", 12.))
                    .child(div().flex_1().min_w_0().child(self.filter.clone()))
                    .child(
                        icon_button("new-automation", "New automation", "plus").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.show_gallery(cx);
                            }),
                        ),
                    ),
            )
            .child(list);
        let main_content = if self.gallery || self.editor.is_none() {
            self.render_gallery(cx)
        } else {
            self.render_editor(&crate::inbox::RelativeNow::read(window, cx), cx)
        };
        let mut root = div()
            .id("automations-view")
            .role(Role::Region)
            .aria_label("Automations")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(content(1.))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    // Let the trigger menu close its active submenu first.
                    if this.trigger_menu.is_some() {
                        cx.propagate();
                        return;
                    }
                    if this.confirm_delete {
                        this.confirm_delete = false;
                    } else if this.choice.is_some()
                        || this.trigger_menu.is_some()
                        || this.models.is_some()
                        || this.project_picker.is_some()
                    {
                        this.choice = None;
                        this.trigger_menu = None;
                        this.models = None;
                        this.project_picker = None;
                    } else if this.editor.is_some() && !this.gallery {
                        this.show_gallery(cx);
                    } else {
                        cx.emit(Event::Close);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(
                div().flex_1().min_h_0().flex().child(aside).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .when_some(self.error.clone(), |d, error| {
                            d.child(
                                div()
                                    .m(px(16.))
                                    .p(px(10.))
                                    .rounded(px(8.))
                                    .bg(gpui::rgba(0xf8717114))
                                    .text_size(px(12.))
                                    .text_color(rgb(0xfca5a5))
                                    .child(error),
                            )
                        })
                        .child(main_content),
                ),
            );
        if let Some(menu) = &self.trigger_menu {
            root = root.child(
                menu.read(cx)
                    .popover(Anchor::Slot(self.trigger_anchor.clone()), cx),
            );
        }
        if let Some(menu) = &self.choice {
            let weak = cx.weak_entity();
            let field = menu.field.clone();
            root = root.child(
                popover("automation-choice", Anchor::Slot(menu.anchor.clone()))
                    .width(250.)
                    .on_dismiss(move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.choice = None;
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .id("automation-choices")
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .p(px(4.))
                            .children(menu.options.iter().enumerate().map(
                                |(index, (value, label))| {
                                    let field = field.clone();
                                    let value = value.clone();
                                    div()
                                        .id(("automation-choice", index))
                                        .role(Role::MenuItem)
                                        .aria_label(label.clone())
                                        .px(px(8.))
                                        .py(px(8.))
                                        .rounded(px(6.))
                                        .text_size(px(13.))
                                        .hover(|s| s.bg(content(0.07)))
                                        .child(label.clone())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.apply_choice(field.clone(), value.clone(), cx)
                                        }))
                                },
                            )),
                    ),
            );
        }
        if let Some(picker) = &self.models {
            let weak = cx.weak_entity();
            root = root.child(
                popover(
                    "automation-model-picker",
                    Anchor::Slot(self.model_anchor.clone()),
                )
                .width(340.)
                .on_dismiss(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.models = None;
                        cx.notify();
                    });
                })
                .child(self.render_model_settings(cx))
                .child(div().h(px(440.)).child(picker.clone())),
            );
        }
        if let Some(picker) = &self.project_picker {
            let weak = cx.weak_entity();
            root = root.child(
                popover(
                    "automation-project-picker",
                    Anchor::Slot(self.project_anchor.clone()),
                )
                .width(320.)
                .on_dismiss(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.project_picker = None;
                        cx.notify();
                    });
                })
                .child(picker.clone()),
            );
        }
        root
    }
}
