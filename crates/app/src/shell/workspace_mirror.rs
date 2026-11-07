//! The shell's mirror of the engine's `WatchWorkspace` (engine.md, migration step 4), as
//! `session::Viewer` is the conversation's of its session. The sidebar, history and workspace
//! views keep reading the shell's own maps; the engine's snapshot and events fill the parts it
//! owns (session summaries, folders, reminders and the note count), and
//! the shell changes them only through calls under ops it mints. A call's effect is echoed in
//! the same frame and laid over every update until the engine's receipt for its op arrives.
//!
//! A session's fields reach the shell's maps and its open conversation only when the engine's
//! value of them moves (`follow`). A conversation changes some ahead of the engine (a title
//! seeded from the first prompt, a provider's placeholder title, its `has_messages` and time)
//! and the store's draft mark follows its saved queue; an update about anything else, or one
//! that leaves those fields as they were, never puts back an older value, so nothing on the
//! sidebar flickers or loses its mark when a snapshot or an event lands.
use onicode_protocol::agent::Settings;
use onicode_protocol::workspace::{
    CreateSession, NewSession, SessionSummary, WorkspaceEvent, WorkspaceState,
};
use onicode_protocol::{Call, EngineId, OpId, Outcome, Rejection, SessionId};

use super::*;
use crate::engine_client::{self, WorkspaceUpdate};

/// A change the shell made ahead of the engine, applied again over every update.
pub(super) type Echo = Box<dyn Fn(&mut Shell, &mut Context<Shell>)>;

struct Echoed {
    op: OpId,
    echo: Echo,
    /// The engine the call went to: `None` for the app's own; its workspace watch brings the
    /// receipt.
    engine: Option<EngineId>,
    /// The call has answered; a snapshot already holds its effect.
    settled: bool,
    /// The sessions the echo marks: shown as the engine holds them again if it refuses the call.
    rows: HashSet<i64>,
}

#[derive(Default)]
pub(super) struct Mirror {
    /// The workspace as the engine last showed it; `None` until its snapshot arrives.
    state: Option<WorkspaceState>,
    echoes: Vec<Echoed>,
    /// While an echo first runs, the sessions it marks.
    marking: Option<HashSet<i64>>,
    _watch: Option<gpui::Task<()>>,
}

impl Mirror {
    /// The session the store keeps under `key` for other tools.
    pub(super) fn row_for_key(&self, key: &str) -> Option<i64> {
        self.state
            .as_ref()?
            .sessions
            .iter()
            .find(|summary| summary.key == key)
            .map(|summary| summary.session.id)
    }

    /// The id the store keeps the session at `row` under for other tools.
    pub(super) fn key(&self, row: i64) -> Option<String> {
        self.summary(row).map(|summary| summary.key.clone())
    }

    /// The session at `row` as the engine holds it.
    pub(super) fn session(&self, row: i64) -> Option<Session> {
        self.summary(row).map(|summary| summary.session.clone())
    }

    /// The worktree the engine holds the session at `row` in; `Err` until its workspace arrives.
    pub(super) fn worktree(&self, row: i64) -> Result<Option<String>, String> {
        if self.state.is_none() {
            return Err("The engine has not answered yet".into());
        }
        Ok(self
            .summary(row)
            .and_then(|summary| summary.worktree.clone()))
    }

    /// Takes the echoes of calls made to `engine` that a snapshot of it holds (`settled`) or,
    /// with `all`, every one: the sessions they marked, to show as that engine holds them.
    pub(super) fn settle_echoes(&mut self, engine: Option<&EngineId>, all: bool) -> HashSet<i64> {
        let mut rows = HashSet::new();
        self.echoes.retain(|echoed| {
            let done = echoed.engine.as_ref() == engine && (all || echoed.settled);
            if done {
                rows.extend(echoed.rows.iter().copied());
            }
            !done
        });
        rows
    }

    /// Takes the echoes of the calls `receipts` answer: the sessions marked by those the engine
    /// refused, to show as it holds them.
    pub(super) fn receipts(&mut self, receipts: &[(OpId, Outcome)]) -> HashSet<i64> {
        let mut rows = HashSet::new();
        self.echoes.retain(|echoed| {
            let Some((_, outcome)) = receipts.iter().find(|(op, _)| *op == echoed.op) else {
                return true;
            };
            if matches!(outcome, Outcome::Rejected(_)) {
                rows.extend(echoed.rows.iter().copied());
            }
            false
        });
        rows
    }

    /// The engine's workspace as it last arrived, for a test to assert on.
    #[cfg(test)]
    pub(super) fn state(&self) -> &WorkspaceState {
        self.state
            .as_ref()
            .expect("the engine's workspace has arrived")
    }

    fn summary(&self, row: i64) -> Option<&SessionSummary> {
        self.state
            .as_ref()?
            .sessions
            .iter()
            .find(|summary| summary.session.id == row)
    }
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

/// What the sidebar shows of the engine's workspace before the mirror's first snapshot: the
/// engine's workspace as the window opens, in the shell's own maps.
pub(super) fn sidebar_metadata(
    state: WorkspaceState,
) -> onicode_protocol::sessions::SessionSidebarMetadata {
    let mut metadata = onicode_protocol::sessions::SessionSidebarMetadata::default();
    for summary in state.sessions {
        let row = summary.session.id;
        if summary.draft {
            metadata.drafts.insert(row);
        }
        if summary.model.is_some() {
            metadata
                .details
                .insert(row, saved_details(summary.model, summary.worktree.clone()));
        }
        metadata.worktrees.insert(row, summary.worktree);
    }
    metadata.groups = state
        .projects
        .into_iter()
        .map(|project| (project.path, project.groups))
        .collect();
    metadata.reminders = state.reminders;
    metadata
}

/// What the sidebar shows of a session no conversation is loaded for.
fn saved_details(
    model: Option<String>,
    worktree_cwd: Option<String>,
) -> onicode_protocol::sessions::MobileSessionMetadata {
    onicode_protocol::sessions::MobileSessionMetadata {
        model,
        worktree_cwd,
        ..Default::default()
    }
}

/// Every reminder occurrence still waiting to fire.
pub(super) fn armed(reminders: &[onicode_protocol::sessions::Reminder]) -> HashSet<(i64, u64)> {
    reminders
        .iter()
        .filter(|reminder| reminder.fired_at.is_none())
        .map(|reminder| (reminder.session_id, reminder.due_at))
        .collect()
}

impl Shell {
    /// Starts mirroring the engine's workspace.
    pub(super) fn mirror_workspace(&mut self, cx: &mut Context<Self>) {
        let updates = engine_client::watch_workspace(&self.engine, cx);
        self.mirror._watch = Some(cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this
                    .update(cx, |this, cx| this.apply_workspace(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn apply_workspace(&mut self, update: WorkspaceUpdate, cx: &mut Context<Self>) {
        let waiting = armed(
            self.mirror
                .state
                .as_ref()
                .map_or(&self.reminders, |state| &state.reminders),
        );
        let mut removed = Vec::new();
        let mut due = Vec::new();
        let mut receipts = Vec::new();
        // What the engine showed of each session this update changes, from before it.
        let mut before: HashMap<i64, Option<SessionSummary>> = HashMap::new();
        match update {
            WorkspaceUpdate::Snapshot(state) => {
                let mut known: HashMap<i64, SessionSummary> = self
                    .mirror
                    .state
                    .take()
                    .map(|previous| previous.sessions)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|summary| (summary.session.id, summary))
                    .collect();
                for summary in &state.sessions {
                    let prior = known.remove(&summary.session.id);
                    if prior.as_ref() != Some(summary) {
                        before.insert(summary.session.id, prior);
                    }
                }
                removed.extend(known.into_keys());
                due.extend(state.automations.iter().cloned());
                // The snapshot holds what became of every answered call: its sessions show it.
                let settled = self.mirror.settle_echoes(None, false);
                before.extend(settled.into_iter().map(|row| (row, None)));
                self.mirror.state = Some(*state);
            }
            WorkspaceUpdate::Events(events) => {
                let state = self.mirror.state.get_or_insert_with(Default::default);
                for event in &events {
                    match event {
                        WorkspaceEvent::Removed(row) => removed.push(*row),
                        WorkspaceEvent::AutomationDue(run) => due.push(run.as_ref().clone()),
                        WorkspaceEvent::Receipt { op, outcome } => {
                            receipts.push((*op, outcome.clone()))
                        }
                        WorkspaceEvent::Session(summary) => {
                            let row = summary.session.id;
                            before.entry(row).or_insert_with(|| {
                                state
                                    .sessions
                                    .iter()
                                    .find(|known| known.session.id == row)
                                    .cloned()
                            });
                        }
                        _ => {}
                    }
                    onicode_engine::workspace::apply(state, event);
                }
            }
        }
        // An echo the engine refused leaves its sessions as the engine holds them, whole.
        let refused = self.mirror.receipts(&receipts);
        before.extend(refused.into_iter().map(|row| (row, None)));
        self.show_sessions(&before, cx);
        self.show_workspace(cx);
        for row in removed {
            if !self.deleting.contains(&row) && self.session_summaries.contains_key(&row) {
                self.forget_session(row, cx);
            }
        }
        let fired: Vec<_> = self
            .reminders
            .iter()
            .filter(|reminder| {
                reminder.fired_at.is_some()
                    && waiting.contains(&(reminder.session_id, reminder.due_at))
            })
            .map(|reminder| (reminder.session_id, reminder.due_at))
            .collect();
        for (row, due_at) in fired {
            self.deliver_reminder_notice(row, due_at, cx);
        }
        for run in due {
            self.launch_automation(run, false, cx);
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Lays what the engine changed of the sessions in `before` over the shell's maps and their
    /// open conversations: only the fields whose engine value moved since `before`, every field
    /// of a session the mirror had not seen.
    fn show_sessions(
        &mut self,
        before: &HashMap<i64, Option<SessionSummary>>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.mirror.state.as_ref() else {
            return;
        };
        for summary in &state.sessions {
            let row = summary.session.id;
            let Some(prior) = before.get(&row) else {
                continue;
            };
            let was = prior.as_ref().map(|prior| &prior.session);
            let now = &summary.session;
            // A draft the engine's store gained or lost; a session new to the mirror only adds
            // its mark, since the shell may have marked it first.
            let draft = match prior {
                Some(prior) if prior.draft != summary.draft => Some(summary.draft),
                None if summary.draft => Some(true),
                _ => None,
            };
            if let Some(view) = self.sessions.get(&row) {
                view.update(cx, |view, cx| {
                    // The conversation owns the rest of its summary.
                    let mut changed =
                        follow!(view.summary, was, now; title, pinned, archived, work_item);
                    if let Some(draft) = draft
                        && view.draft_hint != draft
                    {
                        view.draft_hint = draft;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                });
            }
            match self.session_summaries.get_mut(&row) {
                Some(shown) if was.is_some() => {
                    follow!(
                        shown, was, now;
                        title, provider, cwd, provider_id, has_messages, updated_at, archived,
                        pinned, work_item
                    );
                }
                _ => {
                    self.session_summaries.insert(row, now.clone());
                }
            }
            match draft {
                Some(true) => {
                    self.draft_sessions.insert(row);
                }
                Some(false) => {
                    self.draft_sessions.remove(&row);
                }
                None => {}
            }
            if !self.order.contains(&row) {
                self.order.push(row);
            }
            self.session_worktrees
                .entry(row)
                .or_insert_with(|| summary.worktree.clone());
            if let Some(automation) = &summary.automation {
                self.automation_state
                    .session_automations
                    .insert(row, automation.clone());
            }
            // The saved model when the engine's value of it moved; a session new to the mirror
            // only adds it, since the shell may have set it first.
            let model = match prior {
                Some(prior) if prior.model != summary.model => Some(summary.model.clone()),
                None if summary.model.is_some() => Some(summary.model.clone()),
                _ => None,
            };
            if let Some(model) = model {
                self.session_details
                    .entry(row)
                    .or_insert_with(|| saved_details(None, summary.worktree.clone()))
                    .model = model;
            }
        }
    }

    /// Lays the sessions at `rows` over the shell's maps whole as their engines hold them (the
    /// app's own, or the machine a peer session is on), then the rest of the workspaces: what an
    /// echo an engine refused leaves behind.
    pub(super) fn reshow_workspace(&mut self, rows: HashSet<i64>, cx: &mut Context<Self>) {
        let (peers, own): (HashSet<i64>, HashSet<i64>) = rows
            .into_iter()
            .partition(|row| self.peer_summary(*row).is_some());
        let before = own.into_iter().map(|row| (row, None)).collect();
        self.show_sessions(&before, cx);
        self.reshow_peer_sessions(peers, cx);
        self.show_workspace(cx);
    }

    /// Lays the engines' folders, reminders and note count over the
    /// shell's maps (the app's own engine's, then each followed machine's in its projects), then
    /// the echoes still in flight.
    pub(super) fn show_workspace(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.mirror.state.as_ref() {
            for project in &state.projects {
                self.groups
                    .insert(project.path.clone(), project.groups.clone());
            }
            self.reminders.clone_from(&state.reminders);
            if self.note_count != Some(state.notes) {
                self.note_count = Some(state.notes);
                self.notify_sidebar(cx);
            }
        }
        self.show_peer_workspaces();
        let echoes = std::mem::take(&mut self.mirror.echoes);
        for echoed in &echoes {
            (echoed.echo)(self, cx);
        }
        self.mirror.echoes = echoes;
    }

    /// Changes the session's summary as the sidebar and its open conversation show it.
    pub(super) fn mark_session(
        &mut self,
        row: i64,
        cx: &mut Context<Self>,
        change: impl Fn(&mut Session),
    ) {
        if let Some(rows) = self.mirror.marking.as_mut() {
            rows.insert(row);
        }
        if let Some(summary) = self.session_summaries.get_mut(&row) {
            change(summary);
        }
        if let Some(view) = self.sessions.get(&row) {
            view.update(cx, |view, cx| {
                change(&mut view.summary);
                cx.notify();
            });
        }
    }

    /// Makes a mutating workspace call under a new op, after the shell's earlier writes. `echo`
    /// shows its effect now and over every update until the engine's receipt; `settle` gets the
    /// reply.
    pub(super) fn workspace_call<C: Call + 'static>(
        &mut self,
        body: C::Params,
        echo: Option<Echo>,
        cx: &mut Context<Self>,
        settle: impl FnOnce(&mut Self, Result<C::Reply, String>, &mut Context<Self>) + 'static,
    ) where
        C::Params: Send + 'static,
        C::Reply: Send + 'static,
    {
        let engine = self.engine.clone();
        self.workspace_call_on::<C>(&engine, body, echo, cx, settle);
    }

    /// [`Self::workspace_call`] made to `engine`: another engine's, for a call about one of its
    /// sessions (`Shell::session_engine`), whose body names the row it stores it at.
    pub(super) fn workspace_call_on<C: Call + 'static>(
        &mut self,
        engine: &Engine,
        body: C::Params,
        echo: Option<Echo>,
        cx: &mut Context<Self>,
        settle: impl FnOnce(&mut Self, Result<C::Reply, String>, &mut Context<Self>) + 'static,
    ) where
        C::Params: Send + 'static,
        C::Reply: Send + 'static,
    {
        self.workspace_send::<C>(engine, body, echo, |_| false, cx, settle);
    }

    /// Makes the call: its echo shows now and over every update until the receipt of its op
    /// arrives on its engine's workspace watch. A call that fails, or that `refused` says the
    /// engine turned down, takes its echo back at once: what it marked shows as its engine holds
    /// it.
    fn workspace_send<C: Call + 'static>(
        &mut self,
        engine: &Engine,
        body: C::Params,
        echo: Option<Echo>,
        refused: impl FnOnce(&C::Reply) -> bool + 'static,
        cx: &mut Context<Self>,
        settle: impl FnOnce(&mut Self, Result<C::Reply, String>, &mut Context<Self>) + 'static,
    ) where
        C::Params: Send + 'static,
        C::Reply: Send + 'static,
    {
        let engine = engine.clone();
        let op = OpId::new();
        if let Some(echo) = echo {
            self.mirror.marking = Some(HashSet::new());
            echo(self, cx);
            let rows = self.mirror.marking.take().unwrap_or_default();
            self.mirror.echoes.push(Echoed {
                op,
                echo,
                engine: engine.target().cloned(),
                settled: false,
                rows,
            });
        }
        // Another engine's call neither waits behind the app's own engine's writes nor holds
        // them up while that engine is offline.
        let peer = engine.target().is_some();
        let previous = if peer { None } else { self.group_writes.take() };
        let write = cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let Ok(call) = this.update(cx, |_, cx| {
                engine_client::call::<C>(&engine, Some(op), body, cx)
            }) else {
                return;
            };
            let reply = call.await;
            let _ = this.update(cx, |this, cx| {
                let refused = match &reply {
                    Ok(reply) => refused(reply),
                    Err(_) => true,
                };
                if refused {
                    let mut rows = HashSet::new();
                    this.mirror.echoes.retain(|echoed| {
                        let refused = echoed.op == op;
                        if refused {
                            rows.extend(echoed.rows.iter().copied());
                        }
                        !refused
                    });
                    this.reshow_workspace(rows, cx);
                } else if let Some(echoed) =
                    this.mirror.echoes.iter_mut().find(|echoed| echoed.op == op)
                {
                    echoed.settled = true;
                }
                settle(this, reply, cx);
                this.invalidate_chrome(cx);
                cx.notify();
            });
        });
        if peer {
            write.detach();
        } else {
            self.group_writes = Some(write);
        }
    }

    /// [`Self::workspace_call`] for a call answered with an [`Outcome`]: a failure, or a refusal
    /// with a reason, shows as "Could not {what}". Any other rejection means the engine moved on
    /// first, and the mirror already shows what it holds.
    pub(super) fn workspace_write<C: Call<Reply = Outcome> + 'static>(
        &mut self,
        what: &'static str,
        body: C::Params,
        echo: Option<Echo>,
        cx: &mut Context<Self>,
    ) where
        C::Params: Send + 'static,
    {
        let engine = self.engine.clone();
        self.workspace_write_on::<C>(&engine, what, body, echo, cx);
    }

    /// [`Self::workspace_write`] made to `engine`, as [`Self::workspace_call_on`].
    pub(super) fn workspace_write_on<C: Call<Reply = Outcome> + 'static>(
        &mut self,
        engine: &Engine,
        what: &'static str,
        body: C::Params,
        echo: Option<Echo>,
        cx: &mut Context<Self>,
    ) where
        C::Params: Send + 'static,
    {
        let refused = |reply: &Outcome| matches!(reply, Outcome::Rejected(_));
        self.workspace_send::<C>(
            engine,
            body,
            echo,
            refused,
            cx,
            move |this, reply, _| match reply {
                Err(error) | Ok(Outcome::Rejected(Rejection::Denied(error))) => {
                    this.error = Some(format!("Could not {what}: {error}"));
                }
                Ok(_) => {}
            },
        );
    }

    /// Creates a session in the engine under a new id, answering with its stored summary and,
    /// unless it starts `Bare`, the settings it starts with, in the app's spelling. A project on
    /// another machine creates it there, its checkout named as that machine names it.
    pub(super) fn create_in_engine(
        &mut self,
        provider: &str,
        cwd: &str,
        title: Option<String>,
        mut start: NewSession,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Result<(Session, Option<Settings>), String>> {
        let (engine, cwd) = self.engine.for_path(cwd);
        let checkout = match &mut start {
            NewSession::Settings(settings) => settings.worktree_cwd.as_mut(),
            NewSession::Defaults(defaults) => defaults.worktree_cwd.as_mut(),
            NewSession::Bare => None,
        };
        if let Some(checkout) = checkout {
            *checkout = engine.path_of(checkout);
        }
        let body = CreateSession {
            session: SessionId::new(),
            provider: provider.to_owned(),
            cwd,
            title,
            start,
        };
        let call = engine_client::call::<CreateSession>(&engine, Some(OpId::new()), body, cx);
        cx.spawn(async move |this, cx| {
            let created = call.await?;
            match created.outcome {
                Outcome::Applied => {}
                Outcome::Rejected(Rejection::Denied(reason)) => return Err(reason),
                Outcome::Rejected(rejection) => {
                    return Err(format!("The session was not created: {rejection:?}"));
                }
            }
            let summary = created.session.ok_or("The engine created no session")?;
            let session = match engine.target() {
                Some(peer) => {
                    let session = super::peer_sessions::shown(&engine, &summary);
                    let (peer, row) = (peer.clone(), session.id);
                    this.update(cx, |this, _| this.peers_adopt(row, peer))
                        .map_err(|error| error.to_string())?;
                    session
                }
                None => summary.session,
            };
            let settings = created.settings.map(|settings| {
                let mut settings = *settings;
                settings.worktree_cwd = settings
                    .worktree_cwd
                    .map(|checkout| engine.app_path(&checkout));
                settings
            });
            Ok((session, settings))
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_protocol::follow_up::{Message, State};
    use onicode_protocol::session::SaveQueue;
    use onicode_protocol::workspace::{Pin, Rename};

    use super::*;

    /// Saves session `row`'s queue in its engine, with a draft or without one.
    fn save_draft(shell: &Shell, row: i64, revision: u64, draft: bool, cx: &mut App) {
        let queue = State {
            revision,
            draft: draft.then(|| Message {
                id: "draft".into(),
                text: "Unsent".into(),
                ..Message::default()
            }),
            ..State::default()
        };
        engine_client::session_call::<SaveQueue>(
            &shell.engine,
            row,
            Some(OpId::new()),
            move |session| SaveQueue { session, queue },
            cx,
        )
        .detach();
    }

    #[gpui::test]
    fn the_engine_moves_only_what_it_changed_so_no_mark_flickers(cx: &mut TestAppContext) {
        let (window, a, b) = crate::shell::tests::window(cx);
        // The conversation names itself from its first prompt before the engine hears of it,
        // and another session's draft lands in the engine's store.
        window
            .update(cx, |shell, _, cx| {
                shell.sessions[&a].update(cx, |chat, _| {
                    chat.summary.title = "Seeded from the prompt".into();
                });
                save_draft(shell, b, 1, true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(
                    shell.draft_sessions.contains(&b),
                    "the engine's draft marks it"
                );
                // An event for the session that leaves its title alone, and one for another.
                shell.workspace_write::<Pin>(
                    "pin the sessions",
                    Pin {
                        sessions: vec![a, b],
                        pinned: true,
                    },
                    None,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let chat = shell.sessions[&a].read(cx);
                assert_eq!(chat.summary.title, "Seeded from the prompt");
                assert!(chat.summary.pinned, "what the engine changed still moves");
                assert!(shell.draft_sessions.contains(&b), "and no mark was lost");
                save_draft(shell, b, 2, false, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(
                    !shell.draft_sessions.contains(&b),
                    "a sent draft unmarks it"
                );
                // A rename the engine refuses: the echo shows at once, then the engine's title.
                let echo: Echo = Box::new(move |shell: &mut Shell, cx: &mut Context<Shell>| {
                    shell.mark_session(a, cx, |summary| summary.title = "Refused".into());
                });
                shell.workspace_write::<Rename>(
                    "rename the session",
                    Rename {
                        session: a,
                        title: "Refused".into(),
                        expected: Some("A title it never had".into()),
                    },
                    Some(echo),
                    cx,
                );
                assert_eq!(shell.sessions[&a].read(cx).summary.title, "Refused");
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let engine = shell.mirror.session(a).unwrap().title;
                assert_ne!(engine, "Refused");
                assert_eq!(shell.sessions[&a].read(cx).summary.title, engine);
                assert_eq!(shell.session_summaries[&a].title, engine);
            })
            .unwrap();
    }
}
