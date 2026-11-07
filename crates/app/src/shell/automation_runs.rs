//! What the shell does for automations, which their engines run (engine.md, "Automations"): the
//! engine that holds an automation's project claims its runs and runs each in a session of its
//! own with no viewer, and records how it ended. The shell keeps the Inbox polling while an
//! automation of any engine it follows waits on Inbox events, hands each such engine the items
//! that appeared in its projects, opens a run's session when the person asks, and gives a run
//! that starts while the app is open a tab, as monocode's background runs had.
use onicode_protocol::inbox::Item;
use onicode_protocol::work::HasEventAutomations;
use onicode_protocol::workspace::{ClaimInboxEvents, SessionSummary};
use onicode_protocol::{EngineId, ProjectRef};

use super::*;

#[derive(Default)]
pub(super) struct State {
    polling: bool,
    /// The engines whose automations wait on Inbox events (`None`, the app's own), as last asked.
    waiting: HashSet<Option<EngineId>>,
    /// A run's session to open once its engine's workspace brings it, while the person stays
    /// where they asked from (the navigation revision then).
    reveal: Option<(Option<EngineId>, String, u64)>,
    pub session_automations: HashMap<i64, String>,
}

impl Shell {
    /// Starts the automations' background work once the window is up: the Inbox polls when an
    /// automation waits on its events, and a saved app tab opens again.
    pub(super) fn initialize_automations(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                if !cfg!(test) {
                    this.ensure_background_inbox(window, cx);
                }
                this.poll_automations(cx);
                if this.navigation_revision == 0
                    && this.active_app_view.is_none()
                    && this.settings.is_none()
                {
                    let saved = this.workspace.scope_focus.get(&this.tab_scope()).cloned();
                    if let Some(tab @ TitleTab::App(_)) = saved {
                        this.open_title_tab(&tab, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// The engines whose automations the shell follows: the app's own and each one a project of
    /// the workspace names.
    fn automation_engines(&self) -> Vec<Engine> {
        std::iter::once(self.engine.clone())
            .chain(
                self.followed_peers()
                    .into_iter()
                    .map(|peer| self.engine.on(peer)),
            )
            .collect()
    }

    /// Keeps the Inbox polling in the background while an automation of any followed engine
    /// waits on its events.
    pub(super) fn poll_automations(&mut self, cx: &mut Context<Self>) {
        if self.automation_state.polling || cfg!(test) {
            return;
        }
        self.automation_state.polling = true;
        let asks: Vec<_> = self
            .automation_engines()
            .iter()
            .map(|engine| {
                let ask = crate::engine_client::call::<HasEventAutomations>(
                    engine,
                    None,
                    HasEventAutomations {},
                    cx,
                );
                (engine.target().cloned(), ask)
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut waiting = HashSet::new();
            for (engine, ask) in asks {
                // An engine that does not answer keeps nothing waiting until it does.
                if ask.await.unwrap_or(false) {
                    waiting.insert(engine);
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.automation_state.polling = false;
                let any = !waiting.is_empty();
                this.automation_state.waiting = waiting;
                if any {
                    this.ensure_background_inbox(window, cx);
                }
            });
        })
        .detach();
    }

    /// Inbox items that appeared: each engine whose automations wait on Inbox events is handed
    /// the items of its own projects, named as it names them, and claims and starts the runs
    /// they trigger; it retries a failed claim on its own.
    pub(super) fn inbox_automation_events(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        let waiting: Vec<Option<EngineId>> =
            self.automation_state.waiting.iter().cloned().collect();
        for target in waiting {
            let engine = match &target {
                Some(peer) => self.engine.on(peer.clone()),
                None => self.engine.clone(),
            };
            let items = items_on(&items, target.as_ref());
            if items.is_empty() {
                continue;
            }
            self.workspace_write_on::<ClaimInboxEvents>(
                &engine,
                "evaluate Inbox automations",
                ClaimInboxEvents { items },
                None,
                cx,
            );
        }
    }

    /// Opens the session `engine` (`None`, the app's own) keeps under `key`: a run the
    /// Automations view started or one of its history. A run's session that has not arrived yet
    /// opens when its engine's workspace brings it, unless the person moved on meanwhile.
    pub(super) fn open_automation_session(
        &mut self,
        engine: Option<EngineId>,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.automation_row(engine.as_ref(), &key) {
            Some(id) => self.open(id, window, cx),
            None => {
                self.automation_state.reveal = Some((engine, key, self.navigation_revision));
            }
        }
    }

    /// The app's row for the session `engine` keeps under `key`, once the shell shows it.
    fn automation_row(&self, engine: Option<&EngineId>, key: &str) -> Option<i64> {
        let row = match engine {
            None => self.mirror.row_for_key(key),
            Some(engine) => self.peer_row_for_key(engine, key),
        }?;
        self.session_summaries.contains_key(&row).then_some(row)
    }

    /// After a workspace update: opens the run's session the person asked for once it is shown,
    /// and lets the Automations view read again what its runs came to.
    pub(super) fn automations_moved(&mut self, changed: bool, cx: &mut Context<Self>) {
        if changed && let Some(view) = &self.automations {
            view.update(cx, |view, cx| view.refresh(cx));
        }
        let Some((engine, key, revision)) = self.automation_state.reveal.clone() else {
            return;
        };
        if self.navigation_revision != revision {
            self.automation_state.reveal = None;
            return;
        }
        let Some(id) = self.automation_row(engine.as_ref(), &key) else {
            return;
        };
        self.automation_state.reveal = None;
        cx.spawn(async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| this.activate_session(id, window, cx));
        })
        .detach();
    }

    /// A run that started while the app is open gets a tab, without changing the focused
    /// project: the session `row` turned busy for its automation (`before`, what the engine
    /// showed of it until now). Whether it got one.
    pub(super) fn automation_turn_started(
        &mut self,
        row: i64,
        before: Option<&SessionSummary>,
        now: &SessionSummary,
    ) -> bool {
        if now.automation.is_none() || !now.busy || before.is_some_and(|before| before.busy) {
            return false;
        }
        if self.workspace.tabs.contains(&row) || !self.session_summaries.contains_key(&row) {
            return false;
        }
        self.workspace.tabs.push(row);
        self.workspace.title_order.push(TitleTab::Session(row));
        true
    }
}

/// The items among `items` in projects of `engine` (`None`, the app's own), each naming its
/// project as that engine does.
fn items_on(items: &[Item], engine: Option<&EngineId>) -> Vec<Item> {
    items
        .iter()
        .filter_map(|item| {
            let project = ProjectRef::parse_uri(&item.project_path);
            match (project, engine) {
                (None, None) => Some(item.clone()),
                (Some(project), Some(engine)) if project.engine == *engine => Some(Item {
                    project_path: project.path,
                    ..item.clone()
                }),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_protocol::OpId;
    use onicode_protocol::automations::{Automation, AutomationUpsert};
    use onicode_protocol::work::UpsertAutomation;
    use serde_json::json;

    use super::*;

    fn studio() -> EngineId {
        EngineId("nStudio".into())
    }

    /// An automation as the studio's engine lists it: in its project, as it names it.
    fn on_the_studio(id: &str) -> Automation {
        serde_json::from_value(json!({
            "id": id,
            "name": "Studio review",
            "prompt": "Review the studio's changes",
            "harness": "codex",
            "model": "",
            "cwd": "/Users/me/app",
            "workspaceMode": "current",
            "reuseSession": false,
            "runtimeMode": "auto",
            "scheduleKind": "weekdays",
            "minute": 0,
            "time": "09:00",
            "dayOfWeek": 1,
            "triggers": [],
            "missedRunGraceMinutes": 720,
            "enabled": true,
            "nextRunAt": 4_102_444_800_000_i64,
            "createdAt": 1,
            "updatedAt": 1,
        }))
        .unwrap()
    }

    /// The Automations view lists the app's own engine's automations and a peer project's
    /// engine's together, the peer's in its project's URI; the peer's stay as last listed while
    /// that machine does not answer. An edit goes to the engine that holds the automation, its
    /// project named as that engine names it, and shows at once: the app's own engine answers
    /// it, and one for the studio, which is away, waits in the outbox with its edit still shown.
    #[gpui::test]
    fn the_view_lists_every_engines_automations_and_edits_each_on_its_own(cx: &mut TestAppContext) {
        let (window, _, _) = crate::shell::tests::window(cx);
        let project = ProjectRef {
            engine: studio(),
            path: "/Users/me/app".into(),
        }
        .uri();
        let local = window
            .update(cx, |shell, window, cx| {
                shell.workspace.add_project(&project);
                shell.follow_peers(cx);
                let mut draft =
                    AutomationUpsert::new("/work/monocode".into(), "codex".into(), String::new());
                draft.name = "Local review".into();
                draft.prompt = "Review this Mac's changes".into();
                let id = draft.id.clone();
                crate::engine_client::call::<UpsertAutomation>(
                    &shell.engine,
                    Some(OpId::new()),
                    UpsertAutomation { automation: draft },
                    cx,
                )
                .detach();
                shell.open_automations(window, cx);
                id
            })
            .unwrap();
        cx.run_until_parked();
        let theirs = onicode_protocol::automations::new_id();
        let view = window
            .update(cx, |shell, _, cx| {
                let view = shell.automations.clone().expect("the view is open");
                view.update(cx, |view, cx| {
                    view.refresh(cx);
                    // What the studio listed when it last answered.
                    view.listed(Some(studio()), vec![on_the_studio(&theirs)], cx);
                });
                view
            })
            .unwrap();
        cx.run_until_parked();
        let shown = |cx: &mut TestAppContext| view.read_with(cx, |view, _| view.shown().to_vec());
        let listed = shown(cx);
        assert_eq!(listed.len(), 2, "both engines' automations: {listed:?}");
        let mine = listed.iter().find(|a| a.id == local).expect("the Mac's");
        assert_eq!(mine.cwd, "/work/monocode");
        let studios = listed
            .iter()
            .find(|a| a.id == theirs)
            .expect("the studio's");
        assert_eq!(studios.cwd, project, "named by its project's URI");

        // The studio does not answer: its automations stay as last listed.
        view.update(cx, |view, cx| view.refresh(cx));
        cx.run_until_parked();
        assert!(shown(cx).iter().any(|a| a.id == theirs));

        view.update(cx, |view, cx| {
            view.rename_for_test(&theirs, "Studio nightly", cx);
            view.toggle_for_test(&local, false, cx);
        });
        cx.run_until_parked();
        let calls = window
            .update(cx, |shell, _, cx| {
                crate::engine_client::calls_for_test(&shell.engine, cx)
            })
            .unwrap();
        let upsert = calls
            .iter()
            .rev()
            .find(|(name, _)| *name == "UpsertAutomation")
            .map(|(_, params)| params.clone())
            .expect("the edit was sent");
        assert_eq!(upsert["engine"], "nStudio", "to the studio's engine");
        assert!(
            !upsert["op"].is_null(),
            "under an op, as the outbox keeps it"
        );
        assert_eq!(upsert["body"]["automation"]["id"], theirs.as_str());
        assert_eq!(upsert["body"]["automation"]["cwd"], "/Users/me/app");
        assert_eq!(upsert["body"]["automation"]["name"], "Studio nightly");
        let toggled = calls
            .iter()
            .rev()
            .find(|(name, _)| *name == "SetAutomationEnabled")
            .map(|(_, params)| params.clone())
            .expect("the toggle was sent");
        assert!(toggled["engine"].is_null(), "to the app's own engine");
        assert_eq!(toggled["body"]["automation"], local.as_str());
        let listed = shown(cx);
        let studios = listed.iter().find(|a| a.id == theirs).unwrap();
        assert_eq!(studios.name, "Studio nightly", "shown while it waits");
        assert_eq!(studios.cwd, project);
        let mine = listed.iter().find(|a| a.id == local).unwrap();
        assert!(!mine.enabled, "the Mac's engine took it");
    }

    fn item(number: i64, project: &str) -> Item {
        Item {
            kind: "pr".into(),
            repo: "acme/app".into(),
            project_path: project.into(),
            number,
            ..Item::default()
        }
    }

    /// Each engine is handed only the Inbox items of its own projects, named as it names them.
    #[test]
    fn inbox_items_go_to_the_engine_of_their_project() {
        let items = [
            item(1, "/Users/me/local"),
            item(2, "peer://nStudio/Users/me/app"),
            item(3, "peer://nAttic/srv/site"),
        ];
        let studio = EngineId("nStudio".into());
        let own = items_on(&items, None);
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].number, 1);
        let theirs = items_on(&items, Some(&studio));
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].number, 2);
        assert_eq!(theirs[0].project_path, "/Users/me/app");
    }
}
