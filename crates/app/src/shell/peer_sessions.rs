//! Sessions in projects on other engines (engine.md, "Step 12 as built"). The app's own engine
//! lists only its own sessions, so for every engine a `peer://` project names, the shell follows
//! that engine's workspace through its own (`Engine::on`) and lays the sessions of those
//! projects over its maps as it lays its own engine's: rows, marks, drafts and previews. Each is
//! named by a row of the app's own (`engine_client::peer_row`), never one of an engine's rows,
//! and its cwd is spelled as the project's URI, so every view that keys or groups sessions by row
//! or project shows it as it shows a local one; its conversation drives turns through
//! `Engine::on`. An engine that goes offline leaves its rows as last seen, kept across launches,
//! and a turn for one is refused with the status the engine gives.
//!
//! A peer project's folders, its sessions' reminders and its orchestration summaries live in
//! that engine's workspace like its sessions: the shell lays them over its maps in the app's
//! spelling (the project's URI, the app's rows), and every change to them, as to a session's
//! pin, title or link, is a call to that engine naming its own rows. Its echo shows at once and
//! stays until the receipt arrives on that engine's watch; a call it fails or refuses takes the
//! echo back at once, showing what the engine holds.
use std::collections::BTreeSet;

use onicode_protocol::session_groups::Groups;
use onicode_protocol::sessions::Reminder;
use onicode_protocol::workspace::{SaveGroups, SessionSummary, WorkspaceEvent, WorkspaceState};
use onicode_protocol::{EngineId, ProjectRef, SessionKey};

use super::*;
use crate::engine_client::{self, WorkspaceUpdate};

#[derive(Default)]
pub(super) struct Peers {
    followed: HashMap<EngineId, Followed>,
    /// Each peer session's engine, by the row the app names it.
    engines: HashMap<i64, EngineId>,
}

struct Followed {
    /// The engine's workspace as it last arrived; `None` until it does.
    state: Option<WorkspaceState>,
    /// The paths of that engine's projects the workspace holds, as the shell last showed them.
    projects: BTreeSet<String>,
    /// What the shell last showed of each of its sessions, by the app's row.
    shown: HashMap<i64, Session>,
    _watch: Option<gpui::Task<()>>,
}

/// The paths of `engine`'s projects among `projects`.
fn projects_on(projects: &[String], engine: &EngineId) -> BTreeSet<String> {
    projects
        .iter()
        .filter_map(|project| ProjectRef::parse_uri(project))
        .filter(|project| project.engine == *engine)
        .map(|project| project.path)
        .collect()
}

/// `summary`, a session of `engine` (another engine), as the app shows it: under the app's row
/// for it and in its project's URI. Calls and turns made under that row reach it there.
pub(super) fn shown(engine: &Engine, summary: &SessionSummary) -> Session {
    let mut session = summary.session.clone();
    let Some(target) = engine.target() else {
        return session;
    };
    let id = engine_client::peer_row(&SessionKey {
        engine: target.clone(),
        session: summary.id,
    });
    engine.adopt(id, summary.session.id, summary.id);
    session.id = id;
    session.cwd = engine.app_path(&session.cwd);
    session
}

/// Lays the fields of `now` whose engine value moved since `before` over `shown`; with no
/// `before`, every field differs. Returns whether `shown` changed.
macro_rules! follow {
    ($shown:expr, $before:expr, $now:expr; $($field:ident),+) => {{
        let mut changed = false;
        $(
            if $before.is_none_or(|before: &Session| before.$field != $now.$field)
                && $shown.$field != $now.$field
            {
                $shown.$field.clone_from(&$now.$field);
                changed = true;
            }
        )+
        changed
    }};
}

impl Shell {
    /// The engine that holds session `id`: another engine for a peer session, else the app's.
    pub(super) fn session_engine(&self, id: i64) -> Engine {
        match self.peers.engines.get(&id) {
            Some(engine) => self.engine.on(engine.clone()),
            None => self.engine.clone(),
        }
    }

    /// The peer session `row` as its engine last showed it, with that engine: `None` for a
    /// session of the app's own engine, `Err` while its engine's workspace has not arrived, and
    /// no summary once that engine no longer holds it.
    pub(super) fn peer_summary(
        &self,
        row: i64,
    ) -> Option<Result<(Engine, Option<&SessionSummary>), String>> {
        let engine = self.peers.engines.get(&row)?;
        let on = self.engine.on(engine.clone());
        let Some(state) = self
            .peers
            .followed
            .get(engine)
            .and_then(|followed| followed.state.as_ref())
        else {
            return Some(Err("That machine has not answered yet".into()));
        };
        let stored = on.row(row);
        let summary = state
            .sessions
            .iter()
            .find(|summary| summary.session.id == stored);
        Some(Ok((on, summary)))
    }

    /// The app's row for the session `engine` (another engine) keeps at `stored`, while its
    /// workspace shows it.
    pub(super) fn peer_row_of(&self, engine: &Engine, stored: i64) -> Option<i64> {
        let target = engine.target()?;
        let summary = self
            .peers
            .followed
            .get(target)?
            .state
            .as_ref()?
            .sessions
            .iter()
            .find(|summary| summary.session.id == stored)?;
        Some(engine_client::peer_row(&SessionKey {
            engine: target.clone(),
            session: summary.id,
        }))
    }

    /// The id the store of the engine holding session `row` keeps it under for other tools.
    pub(super) fn session_key(&self, row: i64) -> Option<String> {
        match self.peer_summary(row) {
            Some(peer) => Some(peer.ok()?.1?.key.clone()),
            None => self.mirror.key(row),
        }
    }

    /// `ids` by the engine that holds each, as the rows that engine keeps them at: a call about
    /// several sessions goes to each of their engines.
    pub(super) fn split_by_engine(&self, ids: &[i64]) -> Vec<(Engine, Vec<i64>)> {
        let mut by_engine: Vec<(Engine, Vec<i64>)> = Vec::new();
        for id in ids {
            let engine = self.session_engine(*id);
            let row = engine.row(*id);
            match by_engine
                .iter_mut()
                .find(|(held, _)| held.target() == engine.target())
            {
                Some((_, rows)) => rows.push(row),
                None => by_engine.push((engine, vec![row])),
            }
        }
        by_engine
    }

    /// The `SaveGroups` call for `project`'s folders, and the engine it goes to: a peer
    /// project's, naming its path there and its sessions by the rows that engine keeps them at.
    pub(super) fn groups_call(&self, project: &str, mut groups: Groups) -> (Engine, SaveGroups) {
        let (engine, path) = self.engine.for_path(project);
        if engine.target().is_some() {
            for folder in &mut groups.folders {
                for id in &mut folder.session_ids {
                    *id = engine.row(*id);
                }
            }
        }
        (
            engine,
            SaveGroups {
                project: path,
                groups,
            },
        )
    }

    /// Names `engine` the holder of the session the app names `row`: one the shell created
    /// there, ahead of that engine's workspace event for it.
    pub(super) fn peers_adopt(&mut self, row: i64, engine: EngineId) {
        self.peers.engines.insert(row, engine);
    }

    /// The peer sessions the last launch saw, for the window's first frame: each under the app's
    /// row for it, with its engine.
    pub(super) fn kept_peer_sessions(engine: &Engine) -> Vec<(EngineId, Session)> {
        engine
            .peer_workspaces_now()
            .into_iter()
            .flat_map(|(peer, state)| {
                let on = engine.on(peer.clone());
                state
                    .sessions
                    .iter()
                    .filter(|summary| !summary.hidden)
                    .map(|summary| (peer.clone(), shown(&on, summary)))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Lists the kept peer sessions whose project the workspace holds, before the engines
    /// answer: their saved tabs come back with them.
    pub(super) fn show_kept_peer_sessions(&mut self, kept: Vec<(EngineId, Session)>) {
        for (engine, session) in kept {
            if !self.workspace.projects.contains(&session.cwd)
                && self.workspace.tab_of(session.id).is_none()
            {
                continue;
            }
            self.peers.engines.insert(session.id, engine);
            if !self.session_summaries.contains_key(&session.id) {
                self.order.push(session.id);
            }
            self.session_summaries.insert(session.id, session);
        }
    }

    /// Follows the workspace of every engine a `peer://` project names, and stops following
    /// one no project names any more; sessions of a project that left go with it.
    pub(super) fn follow_peers(&mut self, cx: &mut Context<Self>) {
        let wanted: HashSet<EngineId> = self
            .workspace
            .projects
            .iter()
            .filter_map(|project| ProjectRef::parse_uri(project))
            .map(|project| project.engine)
            .collect();
        let gone: Vec<_> = self
            .peers
            .followed
            .keys()
            .filter(|engine| !wanted.contains(*engine))
            .cloned()
            .collect();
        let mut moved = !gone.is_empty();
        for engine in gone {
            // Its calls' receipts arrive on a watch no longer held.
            self.mirror.settle_echoes(Some(&engine), true);
            if let Some(followed) = self.peers.followed.remove(&engine) {
                for row in followed.shown.into_keys() {
                    self.peers.engines.remove(&row);
                    self.forget_session(row, cx);
                }
            }
        }
        for engine in wanted {
            let projects = projects_on(&self.workspace.projects, &engine);
            match self.peers.followed.get(&engine) {
                Some(followed) if followed.projects == projects => {}
                Some(_) => {
                    self.show_peer(&engine, cx);
                    moved = true;
                }
                None => self.watch_peer(engine, projects, cx),
            }
        }
        // The folders and reminders of the sessions that came or went with a project.
        if moved {
            self.show_workspace(cx);
        }
    }

    fn watch_peer(&mut self, engine: EngineId, projects: BTreeSet<String>, cx: &mut Context<Self>) {
        let updates = engine_client::watch_workspace(&self.engine.on(engine.clone()), cx);
        let watched = engine.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this
                    .update(cx, |this, cx| {
                        this.apply_peer_workspace(&watched, update, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let shown = self
            .peers
            .engines
            .iter()
            .filter(|(_, held)| **held == engine)
            .filter_map(|(row, _)| Some((*row, self.session_summaries.get(row)?.clone())))
            .collect();
        self.peers.followed.insert(
            engine,
            Followed {
                state: None,
                projects,
                shown,
                _watch: Some(task),
            },
        );
    }

    /// Folds what `engine`'s workspace watch brought, then shows its sessions, folders,
    /// reminders and orchestration summaries, settles the echoes of the calls it answered and
    /// announces the reminders it fired.
    pub(super) fn apply_peer_workspace(
        &mut self,
        engine: &EngineId,
        update: WorkspaceUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(followed) = self.peers.followed.get_mut(engine) else {
            return;
        };
        let waiting = followed
            .state
            .as_ref()
            .map(|state| super::workspace_mirror::armed(&state.reminders))
            .unwrap_or_default();
        let mut receipts = Vec::new();
        let snapshot = matches!(update, WorkspaceUpdate::Snapshot(_));
        match update {
            WorkspaceUpdate::Snapshot(state) => followed.state = Some(*state),
            WorkspaceUpdate::Events(events) => {
                let state = followed.state.get_or_insert_with(Default::default);
                for event in &events {
                    if let WorkspaceEvent::Receipt { op, outcome } = event {
                        receipts.push((*op, outcome.clone()));
                    }
                    onicode_engine::workspace::apply(state, event);
                }
            }
        }
        let fired: Vec<(i64, u64)> = followed
            .state
            .iter()
            .flat_map(|state| &state.reminders)
            .filter(|reminder| {
                reminder.fired_at.is_some()
                    && waiting.contains(&(reminder.session_id, reminder.due_at))
            })
            .map(|reminder| (reminder.session_id, reminder.due_at))
            .collect();
        self.show_peer(engine, cx);
        // A call the engine refused shows its sessions as the engine holds them; a snapshot
        // holds what became of every call it answered.
        let mut rows = self.mirror.receipts(&receipts);
        if snapshot {
            rows.extend(self.mirror.settle_echoes(Some(engine), false));
        }
        self.reshow_peer_sessions(rows, cx);
        self.show_workspace(cx);
        let on = self.engine.on(engine.clone());
        for (stored, due_at) in fired {
            if let Some(row) = self.peer_row_of(&on, stored) {
                self.deliver_reminder_notice(row, due_at, cx);
            }
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Lays the peer sessions at `rows` over the shell's maps whole as their engines hold them:
    /// what an echo an engine refused leaves behind.
    pub(super) fn reshow_peer_sessions(&mut self, rows: HashSet<i64>, cx: &mut Context<Self>) {
        for row in rows {
            let Some(Ok((on, Some(summary)))) = self.peer_summary(row) else {
                continue;
            };
            let summary = summary.clone();
            let session = shown(&on, &summary);
            let Some(followed) = on
                .target()
                .and_then(|engine| self.peers.followed.get_mut(engine))
                .filter(|followed| followed.shown.contains_key(&row))
            else {
                continue;
            };
            followed.shown.insert(row, session.clone());
            self.show_peer_session(&on, &summary, &session, None, cx);
        }
    }

    /// Lays every followed engine's folders, reminders and orchestration summaries in the
    /// projects the workspace holds on it over the shell's maps, in the app's spelling: each
    /// project under its URI, each session under the app's row. Called as the app's own
    /// engine's are laid, before the echoes in flight.
    pub(super) fn show_peer_workspaces(&mut self) {
        self.reminders
            .retain(|reminder| !engine_client::is_peer_row(reminder.session_id));
        for (engine, followed) in &self.peers.followed {
            let Some(state) = followed.state.as_ref() else {
                continue;
            };
            // The app's row for each session it shows, by the row the engine keeps it at.
            let rows: HashMap<i64, i64> = state
                .sessions
                .iter()
                .filter_map(|summary| {
                    let row = engine_client::peer_row(&SessionKey {
                        engine: engine.clone(),
                        session: summary.id,
                    });
                    followed
                        .shown
                        .contains_key(&row)
                        .then_some((summary.session.id, row))
                })
                .collect();
            for project in &state.projects {
                if !followed.projects.contains(&project.path) {
                    continue;
                }
                let mut groups = project.groups.clone();
                for folder in &mut groups.folders {
                    folder.session_ids = folder
                        .session_ids
                        .iter()
                        .filter_map(|id| rows.get(id).copied())
                        .collect();
                }
                let uri = ProjectRef {
                    engine: engine.clone(),
                    path: project.path.clone(),
                }
                .uri();
                self.groups.insert(uri, groups);
            }
            self.reminders
                .extend(state.reminders.iter().filter_map(|reminder| {
                    Some(Reminder {
                        session_id: *rows.get(&reminder.session_id)?,
                        ..reminder.clone()
                    })
                }));
            for lead in &state.orchestrations {
                let Some(row) = rows.get(&lead.lead) else {
                    continue;
                };
                let mut summary = lead.summary.clone();
                for task in &mut summary.tasks {
                    task.session = task.session.and_then(|id| rows.get(&id).copied());
                }
                self.orchestration_summaries.insert(*row, summary);
            }
        }
        self.reminders
            .sort_by_key(|reminder| (reminder.due_at, reminder.session_id));
    }

    /// Lays `engine`'s sessions in the workspace's projects on it over the shell's maps, and
    /// drops the ones it no longer holds there.
    fn show_peer(&mut self, engine: &EngineId, cx: &mut Context<Self>) {
        let projects = projects_on(&self.workspace.projects, engine);
        let on = self.engine.on(engine.clone());
        let Some(followed) = self.peers.followed.get_mut(engine) else {
            return;
        };
        followed.projects.clone_from(&projects);
        let Some(state) = followed.state.as_ref() else {
            return;
        };
        let now: Vec<(SessionSummary, Session)> = state
            .sessions
            .iter()
            .filter(|summary| projects.contains(&summary.session.cwd))
            .map(|summary| (summary.clone(), shown(&on, summary)))
            .collect();
        let before = std::mem::take(&mut followed.shown);
        followed.shown = now
            .iter()
            .map(|(_, session)| (session.id, session.clone()))
            .collect();
        for (summary, session) in &now {
            self.peers.engines.insert(session.id, engine.clone());
            self.show_peer_session(&on, summary, session, before.get(&session.id), cx);
        }
        for row in before.into_keys() {
            if !self
                .peers
                .followed
                .get(engine)
                .is_some_and(|followed| followed.shown.contains_key(&row))
            {
                self.peers.engines.remove(&row);
                self.forget_session(row, cx);
            }
        }
    }

    /// One peer session over the shell's maps and its open conversation: only the fields whose
    /// engine value moved since `before`, as the workspace mirror lays the app's own.
    fn show_peer_session(
        &mut self,
        engine: &Engine,
        summary: &SessionSummary,
        now: &Session,
        before: Option<&Session>,
        cx: &mut Context<Self>,
    ) {
        let row = now.id;
        if let Some(view) = self.sessions.get(&row) {
            view.update(cx, |view, cx| {
                // The conversation owns the rest of its summary.
                if follow!(view.summary, before, now; title, pinned, archived, work_item) {
                    cx.notify();
                }
            });
        }
        match self.session_summaries.get_mut(&row) {
            Some(shown) if before.is_some() => {
                follow!(
                    shown, before, now;
                    title, provider, provider_id, has_messages, updated_at, archived, pinned,
                    work_item
                );
            }
            _ => {
                self.session_summaries.insert(row, now.clone());
            }
        }
        if !self.order.contains(&row) {
            self.order.push(row);
        }
        if summary.draft {
            self.draft_sessions.insert(row);
        } else {
            self.draft_sessions.remove(&row);
        }
        if summary.hidden {
            self.sidebar_hidden_sessions.insert(row);
        } else {
            self.sidebar_hidden_sessions.remove(&row);
        }
        self.session_worktrees.entry(row).or_insert_with(|| {
            summary
                .worktree
                .as_deref()
                .map(|worktree| engine.app_path(worktree))
        });
        if summary.model.is_some() {
            self.session_details.entry(row).or_default().model = summary.model.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_protocol::SessionId;
    use onicode_protocol::session_groups::Folder;
    use onicode_protocol::sessions::{Orchestration, SummaryTask};
    use onicode_protocol::workspace::{Lead, Project};
    use serde_json::json;

    use super::*;

    const APP: &str = "/Users/me/app";

    fn studio() -> EngineId {
        EngineId("nStudio".into())
    }

    /// The app's row for `summary`, a session of the studio.
    fn row_of(summary: &SessionSummary) -> i64 {
        engine_client::peer_row(&SessionKey {
            engine: studio(),
            session: summary.id,
        })
    }

    /// Opens the studio's project in `shell` and hands it the studio's workspace `state`.
    fn follow_studio(shell: &mut Shell, state: WorkspaceState, cx: &mut Context<Shell>) -> String {
        let project = ProjectRef {
            engine: studio(),
            path: APP.into(),
        }
        .uri();
        shell.workspace.add_project(&project);
        shell.follow_peers(cx);
        shell.apply_peer_workspace(&studio(), WorkspaceUpdate::Snapshot(Box::new(state)), cx);
        project
    }

    /// Which worktree a peer project's session works in comes from that machine's workspace, in
    /// the app's spelling, as a local session's comes from the app's own engine's.
    #[gpui::test]
    fn a_peer_sessions_worktree_comes_from_its_machine(cx: &mut TestAppContext) {
        let (window, a, _) = crate::shell::tests::window(cx);
        let mut mine = summary(a, APP, "On the studio");
        mine.worktree = Some("/Users/me/app-worktrees/feature".into());
        let row = row_of(&mine);
        window
            .update(cx, |shell, _, cx| {
                let state = WorkspaceState {
                    sessions: vec![mine],
                    ..WorkspaceState::default()
                };
                follow_studio(shell, state, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                let paths = shell.worktree_memberships(&[row, a]).unwrap();
                assert_eq!(
                    paths[&row].as_deref(),
                    Some("peer://nStudio/Users/me/app-worktrees/feature"),
                    "never the local row's, which shares the stored id"
                );
                assert_eq!(paths[&a], shell.mirror.worktree(a).unwrap());
            })
            .unwrap();
    }

    /// A peer project's folders, reminders and orchestration summaries show as the app spells
    /// them (its URI, the app's rows), and changing them, or pinning one of its sessions, goes
    /// to that engine naming its own rows. A change the engine cannot take is taken back at
    /// once, saying why, as a local one the engine refuses is.
    #[gpui::test]
    fn a_peer_projects_folders_and_reminders_are_its_engines(cx: &mut TestAppContext) {
        let (window, _, _) = crate::shell::tests::window(cx);
        let lead = summary(7, APP, "Lead");
        let worker = summary(8, APP, "Worker");
        let (lead_row, worker_row) = (row_of(&lead), row_of(&worker));
        let state = WorkspaceState {
            sessions: vec![lead, worker],
            projects: vec![Project {
                path: APP.into(),
                groups: Groups {
                    folders: vec![Folder {
                        id: "f".into(),
                        name: "Review".into(),
                        session_ids: vec![8, 99],
                        ..Folder::default()
                    }],
                    ..Groups::default()
                },
            }],
            reminders: vec![Reminder {
                session_id: 7,
                due_at: 5_000,
                fired_at: None,
            }],
            orchestrations: vec![Lead {
                lead: 7,
                summary: Orchestration {
                    tasks: vec![SummaryTask {
                        session: Some(8),
                        ..SummaryTask::default()
                    }],
                    live: true,
                },
            }],
            ..WorkspaceState::default()
        };
        let project = window
            .update(cx, |shell, _, cx| follow_studio(shell, state, cx))
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                assert_eq!(
                    shell.groups[&project].folders[0].session_ids,
                    vec![worker_row],
                    "the app's rows, and none the studio does not show"
                );
                assert!(
                    shell
                        .reminders
                        .iter()
                        .any(|r| r.session_id == lead_row && r.due_at == 5_000)
                );
                assert!(shell.reminders.iter().all(|r| r.session_id != 7));
                let run = &shell.orchestration_summaries[&lead_row];
                assert_eq!(run.tasks[0].session, Some(worker_row));
                let folder = Folder {
                    id: "g".into(),
                    name: "New".into(),
                    session_ids: vec![lead_row],
                    ..Folder::default()
                };
                let groups = Groups {
                    folders: vec![folder],
                    ..Groups::default()
                };
                shell.save_groups("save folders", project.clone(), groups, cx);
                assert_eq!(shell.groups[&project].folders[0].id, "g", "shown at once");
                shell.run_menu(MenuTarget::Session(lead_row), "pin", None, window, cx);
                assert!(shell.session_summaries[&lead_row].pinned, "shown at once");
                assert!(
                    shell
                        .sessions
                        .get(&lead_row)
                        .is_none_or(|chat| chat.read(cx).summary.pinned)
                );
            })
            .unwrap();
        cx.run_until_parked();
        let calls = window
            .update(cx, |shell, _, cx| {
                engine_client::calls_for_test(&shell.engine, cx)
            })
            .unwrap();
        let sent = |name: &str| {
            calls
                .iter()
                .find(|(called, _)| *called == name)
                .map(|(_, params)| params.clone())
                .unwrap_or_else(|| panic!("{name} goes to the studio"))
        };
        let saved = sent("SaveGroups");
        assert_eq!(saved["engine"], "nStudio");
        assert_eq!(saved["body"]["project"], APP);
        assert_eq!(
            saved["body"]["groups"]["folders"][0]["session_ids"],
            json!([7])
        );
        let pinned = sent("Pin");
        assert_eq!(pinned["engine"], "nStudio");
        assert_eq!(pinned["body"]["sessions"], json!([7]));
        window
            .update(cx, |shell, _, _| {
                // The studio is away: the calls wait in the engine's outbox until it answers,
                // so the echoes stay and nothing is reported.
                assert_eq!(
                    shell.groups[&project].folders[0].id, "g",
                    "a change waiting for the studio stays shown"
                );
                assert!(shell.session_summaries[&lead_row].pinned);
                assert!(shell.error.is_none(), "{:?}", shell.error);
            })
            .unwrap();
    }

    fn summary(row: i64, cwd: &str, title: &str) -> SessionSummary {
        SessionSummary {
            id: SessionId::new(),
            session: Session {
                id: row,
                title: title.into(),
                provider: "codex".into(),
                cwd: cwd.into(),
                provider_id: None,
                has_messages: true,
                updated_at: 1,
                archived: false,
                pinned: false,
                work_item: None,
            },
            key: row.to_string(),
            worktree: None,
            draft: false,
            started_by: None,
            automation: None,
            busy: false,
            unread: false,
            model: None,
            hidden: false,
        }
    }

    /// A project on another machine lists that engine's sessions in it, under rows of the
    /// app's own that never meet a local row, and its conversation reaches them there: calls
    /// name that engine and the row it stores the session at. A session the engine drops, or one
    /// in a project the workspace does not hold, is not listed.
    #[gpui::test]
    fn a_peer_project_lists_its_engines_sessions(cx: &mut TestAppContext) {
        let (window, a, _) = crate::shell::tests::window(cx);
        let studio = EngineId("nStudio".into());
        let project = ProjectRef {
            engine: studio.clone(),
            path: "/Users/me/app".into(),
        }
        .uri();
        // The peer's rows collide with the local session's on purpose.
        let mine = summary(a, "/Users/me/app", "On the studio");
        let elsewhere = summary(a + 1, "/Users/me/other", "Not opened here");
        let row = engine_client::peer_row(&SessionKey {
            engine: studio.clone(),
            session: mine.id,
        });
        window
            .update(cx, |shell, _, cx| {
                shell.workspace.add_project(&project);
                shell.follow_peers(cx);
                let state = WorkspaceState {
                    sessions: vec![mine.clone(), elsewhere.clone()],
                    ..WorkspaceState::default()
                };
                shell.apply_peer_workspace(&studio, WorkspaceUpdate::Snapshot(Box::new(state)), cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_ne!(row, a);
                assert!(engine_client::is_peer_row(row));
                let listed = &shell.session_summaries[&row];
                assert_eq!(listed.cwd, project);
                assert_eq!(listed.title, "On the studio");
                assert_eq!(shell.session_summaries[&a].cwd, "/work/another-project");
                assert!(
                    !shell
                        .session_summaries
                        .values()
                        .any(|s| s.title == "Not opened here"),
                    "a project the workspace does not hold lists nothing"
                );
                let engine = shell.session_engine(row);
                assert_eq!(engine.target(), Some(&studio));
                assert_eq!(engine.row(row), a, "the row the studio stores it at");
                assert_eq!(shell.session_engine(a).target(), None);
                let chat = shell.ensure_session(row, cx).expect("the row opens");
                assert_eq!(chat.read(cx).summary.cwd, project);
                let removed = WorkspaceEvent::Removed(a);
                shell.apply_peer_workspace(&studio, WorkspaceUpdate::Events(vec![removed]), cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert!(
                    !shell.session_summaries.contains_key(&row),
                    "a session its engine dropped goes"
                );
                assert!(
                    shell.session_summaries.contains_key(&a),
                    "the local row stays"
                );
            })
            .unwrap();
    }
}
