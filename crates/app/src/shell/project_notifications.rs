//! The shell's mirror of every engine's project notification preferences (`alerts.md`, "Project
//! mutes"). A project's mute and categories are kept by the engine that holds the project, in its
//! settings (`EngineSettings::project_notifications`), so its alerts and its own banners follow
//! them with or without a viewer. The shell follows that key on the app's own engine and on every
//! engine a `peer://` project names, and lays them over [`Preferences::project_notifications`] in
//! the app's spelling, so Settings and the project menu show each engine's mutes for its
//! projects. When the shell saves, each folder whose preferences moved goes to the engine holding
//! it as one `SetProjectNotifications`, already shown; the engine's values come back over every
//! folder with no call in flight, and a call it fails or refuses puts the engine's value back.
//!
//! [`Preferences::project_notifications`]: crate::settings::Preferences::project_notifications
use std::collections::{BTreeMap, BTreeSet};

use onicode_protocol::alerts::ProjectNotifications;
use onicode_protocol::engine_settings::{
    EngineSetting, EngineSettings, EngineSettingsEvent, SetProjectNotifications,
    WatchEngineSettings,
};
use onicode_protocol::{EngineId, OpId, Outcome, ProjectRef};

use super::*;
use crate::engine_client::{WatchUpdate, Watching};

/// Projects' preferences by folder.
type Projects = BTreeMap<String, ProjectNotifications>;

/// The engine holding `folder` (as the app spells it: `None`, the app's own) and the folder as
/// that engine names it.
fn owner(folder: &str) -> (Option<EngineId>, String) {
    match ProjectRef::parse_uri(folder) {
        Some(project) => (Some(project.engine), project.path),
        None => (None, folder.to_owned()),
    }
}

/// `path`, as `engine` names it, as the app spells it.
fn spelled(engine: Option<&EngineId>, path: &str) -> String {
    match engine {
        Some(engine) => ProjectRef {
            engine: engine.clone(),
            path: path.to_owned(),
        }
        .uri(),
        None => path.to_owned(),
    }
}

/// One folder's preferences to send to the engine holding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Edit {
    /// As the app spells it.
    pub(super) folder: String,
    /// The engine holding it: `None`, the app's own.
    pub(super) engine: Option<EngineId>,
    /// As that engine names it.
    pub(super) project: String,
    pub(super) notifications: ProjectNotifications,
    pub(super) op: OpId,
}

/// What the mirror knows of the engines and the calls in flight, apart from the watches: the
/// model the tests drive.
#[derive(Debug, Default)]
pub(super) struct Rules {
    /// Each engine's preferences as it last showed them, in its own spelling.
    held: BTreeMap<Option<EngineId>, Projects>,
    /// What the shell last sent or took from an engine, in the app's spelling: what an edit is
    /// told apart from.
    sent: Projects,
    /// The folders sent and not yet answered, each with its latest op.
    pending: BTreeMap<String, OpId>,
}

impl Rules {
    /// Rules that start from `own`, the app's own engine's as the window opens.
    pub(super) fn new(own: &Projects) -> Self {
        Self {
            held: BTreeMap::from([(None, own.clone())]),
            sent: own.clone(),
            pending: BTreeMap::new(),
        }
    }

    /// `engine` holds `projects` now.
    pub(super) fn hold(&mut self, engine: Option<EngineId>, projects: Projects) {
        self.held.insert(engine, projects);
    }

    /// Lets go of `engine`: its folders leave what the app shows at the next [`Self::show`].
    pub(super) fn forget(&mut self, engine: &Option<EngineId>) {
        self.held.remove(engine);
        self.pending.retain(|folder, _| owner(folder).0 != *engine);
    }

    /// A receipt on an engine's watch: whether it settled a call in flight.
    pub(super) fn settle(&mut self, op: OpId) -> bool {
        let Some(folder) = self
            .pending
            .iter()
            .find(|(_, pending)| **pending == op)
            .map(|(folder, _)| folder.clone())
        else {
            return false;
        };
        self.pending.remove(&folder);
        true
    }

    /// The call that sent `folder` under `op` ended, `applied` or not: whether the engine's value
    /// must be shown again (a failed or refused call). The receipt usually settled it first.
    pub(super) fn ended(&mut self, folder: &str, op: OpId, applied: bool) -> bool {
        if self.pending.get(folder) != Some(&op) {
            return false;
        }
        self.pending.remove(folder);
        !applied
    }

    /// What the app shows once `engine`'s preferences are laid over `shown`: that engine's value
    /// for each of its folders with no call in flight, and everything else as it shows. What was
    /// sent moves to the engine's values alike.
    pub(super) fn show(&mut self, engine: &Option<EngineId>, shown: &Projects) -> Projects {
        let pending = &self.pending;
        let ours = |folder: &str| owner(folder).0 != *engine || pending.contains_key(folder);
        let mut next: Projects = shown
            .iter()
            .filter(|(folder, _)| ours(folder.as_str()))
            .map(|(folder, rules)| (folder.clone(), rules.clone()))
            .collect();
        self.sent.retain(|folder, _| ours(folder.as_str()));
        for (path, rules) in self.held.get(engine).into_iter().flatten() {
            let folder = spelled(engine.as_ref(), path);
            if self.pending.contains_key(&folder) {
                continue;
            }
            next.insert(folder.clone(), rules.clone());
            self.sent.insert(folder, rules.clone());
        }
        next
    }

    /// The edits `shown` holds against what was last sent: one per folder that moved, routed to
    /// the engine holding it, each now in flight and taken as sent.
    pub(super) fn edits(&mut self, shown: &Projects) -> Vec<Edit> {
        let folders: BTreeSet<&String> = shown.keys().chain(self.sent.keys()).collect();
        let edits: Vec<Edit> = folders
            .into_iter()
            .filter_map(|folder| {
                let now = shown.get(folder).cloned().unwrap_or_default();
                if self.sent.get(folder).cloned().unwrap_or_default() == now {
                    return None;
                }
                let (engine, project) = owner(folder);
                Some(Edit {
                    folder: folder.clone(),
                    engine,
                    project,
                    notifications: now,
                    op: OpId::new(),
                })
            })
            .collect();
        for edit in &edits {
            self.pending.insert(edit.folder.clone(), edit.op);
            if edit.notifications == ProjectNotifications::default() {
                self.sent.remove(&edit.folder);
            } else {
                self.sent
                    .insert(edit.folder.clone(), edit.notifications.clone());
            }
        }
        edits
    }
}

/// One engine's settings watch and the task that follows it.
type SettingsWatch = (
    Watching<EngineSettings, EngineSettingsEvent>,
    gpui::Task<()>,
);

pub(super) struct Mirror {
    rules: Rules,
    /// The settings watch of each engine followed, by engine (`None`, the app's own).
    watches: BTreeMap<Option<EngineId>, SettingsWatch>,
}

impl Mirror {
    /// A mirror that starts from `own`, the app's own engine's preferences as the window opens.
    pub(super) fn new(own: &Projects) -> Self {
        Self {
            rules: Rules::new(own),
            watches: BTreeMap::new(),
        }
    }
}

impl Shell {
    /// The engine `engine` names, through the app's own.
    fn engine_named(&self, engine: Option<&EngineId>) -> Engine {
        match engine {
            Some(peer) => self.engine.on(peer.clone()),
            None => self.engine.clone(),
        }
    }

    /// Follows the project notification preferences of the app's own engine and of every engine
    /// a `peer://` project names, and lets go of the others' (their projects leave Settings).
    pub(super) fn follow_project_notifications(&mut self, cx: &mut Context<Self>) {
        let mut wanted: BTreeSet<Option<EngineId>> =
            self.followed_peers().into_iter().map(Some).collect();
        wanted.insert(None);
        let gone: Vec<Option<EngineId>> = self
            .project_notifications
            .watches
            .keys()
            .filter(|engine| !wanted.contains(*engine))
            .cloned()
            .collect();
        for engine in gone {
            self.project_notifications.watches.remove(&engine);
            self.project_notifications.rules.forget(&engine);
            self.show_project_notifications(&engine, cx);
        }
        for engine in wanted {
            if !self.project_notifications.watches.contains_key(&engine) {
                self.watch_project_notifications(engine, cx);
            }
        }
    }

    fn watch_project_notifications(&mut self, engine: Option<EngineId>, cx: &mut Context<Self>) {
        let watching = self
            .engine_named(engine.as_ref())
            .watch::<WatchEngineSettings>(|since| WatchEngineSettings { since });
        let updates = watching.updates.clone();
        let watched = engine.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this
                    .update(cx, |this, cx| {
                        this.take_project_notifications(&watched, update, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.project_notifications
            .watches
            .insert(engine, (watching, task));
    }

    /// Folds what `engine`'s settings watch brought: its projects' preferences and the receipts
    /// of the calls the shell sent it.
    pub(super) fn take_project_notifications(
        &mut self,
        engine: &Option<EngineId>,
        update: WatchUpdate<EngineSettings, EngineSettingsEvent>,
        cx: &mut Context<Self>,
    ) {
        let rules = &mut self.project_notifications.rules;
        let mut moved = false;
        match update {
            WatchUpdate::Snapshot(state) => {
                rules.hold(engine.clone(), state.project_notifications);
                moved = true;
            }
            WatchUpdate::Events(events) => {
                for event in events {
                    match event {
                        EngineSettingsEvent::Set(EngineSetting::ProjectNotifications(held)) => {
                            rules.hold(engine.clone(), held);
                            moved = true;
                        }
                        EngineSettingsEvent::Receipt { op, outcome } => {
                            // Refused, the engine's value comes back.
                            moved |= rules.settle(op) && matches!(outcome, Outcome::Rejected(_));
                        }
                        EngineSettingsEvent::Set(_) => {}
                    }
                }
            }
        }
        if moved {
            self.show_project_notifications(engine, cx);
        }
    }

    /// Lays what `engine` holds over the preferences every view reads.
    fn show_project_notifications(&mut self, engine: &Option<EngineId>, cx: &mut Context<Self>) {
        let next = self
            .project_notifications
            .rules
            .show(engine, &self.workspace.preferences.project_notifications);
        if next == self.workspace.preferences.project_notifications {
            return;
        }
        self.workspace.preferences.project_notifications = next;
        cx.set_global(self.workspace.preferences.clone());
        self.sync_settings(cx);
        cx.notify();
    }

    /// Sends each folder whose preferences the shell holds differently from what it last sent to
    /// the engine holding it. Called whenever the shell saves its state, after any edit.
    pub(super) fn send_project_notifications(&mut self, cx: &mut Context<Self>) {
        let edits = self
            .project_notifications
            .rules
            .edits(&self.workspace.preferences.project_notifications);
        if !edits.is_empty() {
            // Every view shows the edit in the frame it was made, before the engine answers.
            cx.set_global(self.workspace.preferences.clone());
        }
        for edit in edits {
            let call = self
                .engine_named(edit.engine.as_ref())
                .call::<SetProjectNotifications>(
                    Some(edit.op),
                    SetProjectNotifications {
                        project: edit.project,
                        notifications: edit.notifications,
                    },
                );
            let (folder, op, engine) = (edit.folder, edit.op, edit.engine);
            cx.spawn(async move |this, cx| {
                let outcome = call.await;
                let _ = this.update(cx, |this, cx| {
                    let applied = matches!(outcome, Ok(Outcome::Applied));
                    if !this.project_notifications.rules.ended(&folder, op, applied) {
                        return;
                    }
                    if let Err(error) = &outcome {
                        let error = format!("Could not save the notification settings: {error}");
                        this.error = Some(error);
                    }
                    this.show_project_notifications(&engine, cx);
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_protocol::alerts::{AGENT_FINISHED, Mute};
    use proptest::prelude::*;

    use super::*;

    fn studio() -> EngineId {
        EngineId("nStudio".into())
    }

    fn muted() -> ProjectNotifications {
        ProjectNotifications {
            muted: Some(Mute::UntilResumed),
            ..ProjectNotifications::default()
        }
    }

    /// The folders the model edits: two of the app's own engine's, two of another engine's.
    const FOLDERS: [&str; 4] = [
        "/work/a",
        "/work/b",
        "peer://nStudio/Users/me/api",
        "peer://nAttic/srv/site",
    ];

    #[derive(Clone, Debug)]
    enum Step {
        /// The owner changes a folder's preferences in Settings, and the shell saves.
        Edit(usize, Option<ProjectNotifications>),
        /// Another viewer changes a folder's preferences on its engine.
        Elsewhere(usize, Option<ProjectNotifications>),
        /// The engine answers the oldest call in flight: applied (its watch brings the value,
        /// then the receipt) or refused.
        Answer(bool),
    }

    fn notifications() -> impl Strategy<Value = Option<ProjectNotifications>> {
        prop::option::of(
            (any::<bool>(), prop::option::of(0..3_u64)).prop_map(|(off, until)| {
                let mut project = ProjectNotifications::default();
                project.set_enabled(AGENT_FINISHED, !off, 0);
                project.muted = until.map(|at| {
                    if at == 0 {
                        Mute::UntilResumed
                    } else {
                        Mute::Until(at)
                    }
                });
                project
            }),
        )
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            3 => (0..FOLDERS.len(), notifications())
                .prop_map(|(at, value)| Step::Edit(at, value)),
            1 => (0..FOLDERS.len(), notifications())
                .prop_map(|(at, value)| Step::Elsewhere(at, value)),
            3 => any::<bool>().prop_map(Step::Answer),
        ]
    }

    /// The engines as the model holds them: each one's folders in its own spelling.
    #[derive(Default)]
    struct Engines(BTreeMap<Option<EngineId>, Projects>);

    impl Engines {
        fn set(&mut self, engine: &Option<EngineId>, project: &str, value: ProjectNotifications) {
            let projects = self.0.entry(engine.clone()).or_default();
            if value == ProjectNotifications::default() {
                projects.remove(project);
            } else {
                projects.insert(project.to_owned(), value);
            }
        }

        /// Everything, as the app spells it.
        fn spelled(&self) -> Projects {
            self.0
                .iter()
                .flat_map(|(engine, projects)| {
                    projects
                        .iter()
                        .map(move |(path, rules)| (spelled(engine.as_ref(), path), rules.clone()))
                })
                .collect()
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// A model of the mirror against engines that apply or refuse each call in turn while
        /// another viewer edits too. Every edit goes to the engine holding its folder, in that
        /// engine's spelling; the app shows its own edit until the call is answered and never
        /// loses one to another engine's news; and once every call is answered the app shows
        /// exactly what the engines hold.
        #[test]
        fn edits_reach_the_owning_engine_and_the_app_shows_every_engine(
            steps in prop::collection::vec(step(), 0..60),
        ) {
            let mut rules = Rules::new(&Projects::new());
            let mut engines = Engines::default();
            let mut shown = Projects::new();
            let mut flight: Vec<Edit> = Vec::new();
            let show = |rules: &mut Rules,
                        engines: &Engines,
                        engine: &Option<EngineId>,
                        shown: &mut Projects| {
                rules.hold(engine.clone(), engines.0.get(engine).cloned().unwrap_or_default());
                *shown = rules.show(engine, shown);
            };
            for step in steps {
                match step {
                    Step::Edit(at, value) => {
                        let folder = FOLDERS[at].to_owned();
                        match value {
                            Some(value) if value != ProjectNotifications::default() => {
                                shown.insert(folder, value);
                            }
                            _ => {
                                shown.remove(&folder);
                            }
                        }
                        let before = shown.clone();
                        for edit in rules.edits(&shown) {
                            let (engine, project) = owner(&edit.folder);
                            prop_assert_eq!(&edit.engine, &engine, "routed to the folder's engine");
                            prop_assert_eq!(&edit.project, &project, "in that engine's spelling");
                            let sent = Some(&edit.notifications)
                                .filter(|value| **value != ProjectNotifications::default());
                            prop_assert_eq!(sent, before.get(&edit.folder));
                            flight.push(edit);
                        }
                        prop_assert!(rules.edits(&shown).is_empty(), "an edit is sent once");
                    }
                    Step::Elsewhere(at, value) => {
                        let (engine, project) = owner(FOLDERS[at]);
                        engines.set(&engine, &project, value.unwrap_or_default());
                        let mine: Projects = shown
                            .iter()
                            .filter(|(folder, _)| flight.iter().any(|edit| edit.folder == **folder))
                            .map(|(folder, value)| (folder.clone(), value.clone()))
                            .collect();
                        show(&mut rules, &engines, &engine, &mut shown);
                        for (folder, value) in mine {
                            if rules.pending.contains_key(&folder) {
                                prop_assert_eq!(shown.get(&folder), Some(&value), "an echo stays");
                            }
                        }
                    }
                    Step::Answer(applied) => {
                        if flight.is_empty() {
                            continue;
                        }
                        let edit = flight.remove(0);
                        if applied {
                            engines.set(&edit.engine, &edit.project, edit.notifications.clone());
                            show(&mut rules, &engines, &edit.engine, &mut shown);
                            rules.settle(edit.op);
                            prop_assert!(!rules.ended(&edit.folder, edit.op, true));
                        } else if rules.ended(&edit.folder, edit.op, false) {
                            show(&mut rules, &engines, &edit.engine, &mut shown);
                        }
                    }
                }
            }
            // Every call answered, as refused, the engines' news shown: the app shows them.
            for edit in std::mem::take(&mut flight) {
                if rules.ended(&edit.folder, edit.op, false) {
                    show(&mut rules, &engines, &edit.engine, &mut shown);
                }
            }
            for engine in [None, Some(studio()), Some(EngineId("nAttic".into()))] {
                show(&mut rules, &engines, &engine, &mut shown);
            }
            prop_assert_eq!(&shown, &engines.spelled());
            prop_assert!(rules.edits(&shown).is_empty(), "nothing left to send");
        }
    }

    /// Settings mutes a project of the app's own engine and one on another Mac: each goes to the
    /// engine holding it, as that engine names it. The app's own engine keeps it and the app
    /// shows what it holds; the other engine, away here, has its call wait in the outbox, and its
    /// echo shows while it waits. What that engine holds when its watch answers shows under its
    /// URI.
    #[gpui::test]
    fn a_mute_goes_to_the_engine_holding_the_project(cx: &mut TestAppContext) {
        let (window, _, _, ()) = super::super::tests::seeded_window(cx, |_| ());
        let peer = format!("peer://{}/Users/me/api", studio().0);
        window
            .update(cx, |shell, _, cx| {
                shell.follow_project_notifications(cx);
                shell
                    .workspace
                    .preferences
                    .mute_notification_project("/work/monocode", None);
                shell
                    .workspace
                    .preferences
                    .mute_notification_project(&peer, Some(4_000_000_000_000));
                shell.save_state(cx);
                assert!(
                    shell
                        .workspace
                        .preferences
                        .project_notifications
                        .contains_key(&peer),
                    "shown in the frame it was made"
                );
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(
                    shell.engine.settings_now().project_notifications,
                    BTreeMap::from([("/work/monocode".to_owned(), muted())]),
                    "the app's own engine keeps its project's mute"
                );
                let calls: Vec<serde_json::Value> =
                    crate::engine_client::calls_for_test(&shell.engine, cx)
                        .into_iter()
                        .filter(|(name, _)| *name == "SetProjectNotifications")
                        .map(|(_, params)| params)
                        .collect();
                assert_eq!(calls.len(), 2, "one call per project, to its engine");
                let to_peer = calls
                    .iter()
                    .find(|params| params["engine"] == "nStudio")
                    .expect("the peer project's mute goes to its engine");
                assert_eq!(to_peer["body"]["project"], "/Users/me/api");
                let own = calls
                    .iter()
                    .find(|params| params["engine"].is_null())
                    .expect("the local project's mute goes to the app's own engine");
                assert_eq!(own["body"]["project"], "/work/monocode");
                let prefs = &shell.workspace.preferences;
                assert!(prefs.project_muted("/work/monocode", 0));
                assert!(
                    prefs.project_muted(&peer, 0),
                    "a change for an engine that is away shows while it waits in the outbox"
                );
                assert!(
                    cx.global::<crate::settings::Preferences>()
                        .project_muted("/work/monocode", 0)
                );
            })
            .unwrap();
        // The other Mac's engine answers with what it holds, as another viewer set it.
        window
            .update(cx, |shell, _, cx| {
                shell.take_project_notifications(
                    &Some(studio()),
                    WatchUpdate::Snapshot(Box::new(EngineSettings {
                        project_notifications: BTreeMap::from([
                            ("/Users/me/api".to_owned(), muted()),
                            ("/Users/me/web".to_owned(), muted()),
                        ]),
                        ..EngineSettings::default()
                    })),
                    cx,
                );
                let prefs = &shell.workspace.preferences;
                assert!(prefs.project_muted(&peer, 0), "its echo, still waiting");
                assert!(
                    prefs.project_muted(&format!("peer://{}/Users/me/web", studio().0), 0),
                    "what that engine holds shows under its URI"
                );
                assert!(
                    prefs.project_muted("/work/monocode", 0),
                    "its own engine's stay"
                );
                assert!(!prefs.allows_project_notification(&peer, AGENT_FINISHED, Some(1), 2));
            })
            .unwrap();
    }
}
