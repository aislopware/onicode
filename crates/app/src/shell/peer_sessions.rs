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
//! Folders, reminders and orchestration in a peer project stay on that engine: the shell shows
//! its sessions, ungrouped.
use std::collections::BTreeSet;

use onicode_protocol::workspace::{SessionSummary, WorkspaceState};
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
        for engine in gone {
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
                Some(_) => self.show_peer(&engine, cx),
                None => self.watch_peer(engine, projects, cx),
            }
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

    /// Folds what `engine`'s workspace watch brought, then shows its sessions.
    pub(super) fn apply_peer_workspace(
        &mut self,
        engine: &EngineId,
        update: WorkspaceUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(followed) = self.peers.followed.get_mut(engine) else {
            return;
        };
        match update {
            WorkspaceUpdate::Snapshot(state) => followed.state = Some(*state),
            WorkspaceUpdate::Events(events) => {
                let state = followed.state.get_or_insert_with(Default::default);
                for event in &events {
                    onicode_engine::workspace::apply(state, event);
                }
            }
        }
        self.show_peer(engine, cx);
        self.invalidate_chrome(cx);
        cx.notify();
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
    use onicode_protocol::workspace::WorkspaceEvent;

    use super::*;

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
