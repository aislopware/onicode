//! The shell's mirror of the engine's `WatchWorkspace` (engine.md, migration step 4), as
//! `session::Viewer` is the conversation's of its session. The sidebar, history and workspace
//! views keep reading the shell's own maps; the engine's snapshot and events fill the parts it
//! owns (session summaries, folders, reminders, orchestration summaries and the note count), and
//! the shell changes them only through calls under ops it mints. A call's effect is echoed in
//! the same frame and laid over every update until the engine's receipt for its op arrives.
use onicode_core::agent::Settings;
use onicode_protocol::workspace::{
    CreateSession, NewSession, SessionSummary, WorkspaceEvent, WorkspaceState,
};
use onicode_protocol::{Call, OpId, Outcome, Rejection, SessionId};

use super::*;
use crate::engine_client::{self, WorkspaceUpdate};

/// A change the shell made ahead of the engine, applied again over every update.
pub(super) type Echo = Box<dyn Fn(&mut Shell, &mut Context<Shell>)>;

struct Echoed {
    op: OpId,
    echo: Echo,
    /// The call has answered; a snapshot already holds its effect.
    settled: bool,
}

#[derive(Default)]
pub(super) struct Mirror {
    /// The workspace as the engine last showed it; `None` until its snapshot arrives.
    state: Option<WorkspaceState>,
    echoes: Vec<Echoed>,
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

/// Every reminder occurrence still waiting to fire.
fn armed(reminders: &[onicode_core::store::Reminder]) -> HashSet<(i64, u64)> {
    reminders
        .iter()
        .filter(|reminder| reminder.fired_at.is_none())
        .map(|reminder| (reminder.session_id, reminder.due_at))
        .collect()
}

impl Shell {
    /// Starts mirroring the engine's workspace.
    pub(super) fn mirror_workspace(&mut self, cx: &mut Context<Self>) {
        let updates = engine_client::watch_workspace(&self.store, cx);
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
        match update {
            WorkspaceUpdate::Snapshot(state) => {
                if let Some(previous) = &self.mirror.state {
                    let rows: HashSet<i64> = state
                        .sessions
                        .iter()
                        .map(|summary| summary.session.id)
                        .collect();
                    removed.extend(
                        previous
                            .sessions
                            .iter()
                            .map(|summary| summary.session.id)
                            .filter(|row| !rows.contains(row)),
                    );
                }
                for summary in &state.sessions {
                    if summary.draft && self.mirror.summary(summary.session.id).is_none() {
                        self.draft_sessions.insert(summary.session.id);
                    }
                }
                due.extend(state.automations.iter().cloned());
                self.mirror.echoes.retain(|echoed| !echoed.settled);
                self.mirror.state = Some(*state);
            }
            WorkspaceUpdate::Events(events) => {
                let state = self.mirror.state.get_or_insert_with(Default::default);
                for event in &events {
                    match event {
                        WorkspaceEvent::Removed(row) => removed.push(*row),
                        WorkspaceEvent::AutomationDue(run) => due.push(run.as_ref().clone()),
                        WorkspaceEvent::Receipt { op, .. } => receipts.push(*op),
                        WorkspaceEvent::Session(summary)
                            if summary.draft
                                && !state
                                    .sessions
                                    .iter()
                                    .any(|known| known.session.id == summary.session.id) =>
                        {
                            self.draft_sessions.insert(summary.session.id);
                        }
                        _ => {}
                    }
                    onicode_engine::workspace::apply(state, event);
                }
            }
        }
        self.mirror
            .echoes
            .retain(|echoed| !receipts.contains(&echoed.op));
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

    /// Lays the engine's workspace over the shell's maps, then the echoes still in flight.
    fn show_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.mirror.state.as_ref() else {
            return;
        };
        for summary in &state.sessions {
            let row = summary.session.id;
            let session = &summary.session;
            if let Some(view) = self.sessions.get(&row) {
                view.update(cx, |view, cx| {
                    let shown = &mut view.summary;
                    if shown.title != session.title
                        || shown.pinned != session.pinned
                        || shown.archived != session.archived
                        || shown.work_item != session.work_item
                    {
                        shown.title.clone_from(&session.title);
                        shown.pinned = session.pinned;
                        shown.archived = session.archived;
                        shown.work_item.clone_from(&session.work_item);
                        cx.notify();
                    }
                });
            }
            self.session_summaries.insert(row, session.clone());
            if !self.order.contains(&row) {
                self.order.push(row);
            }
            self.session_worktrees
                .entry(row)
                .or_insert_with(|| summary.worktree.clone());
            if let Some(lead) = summary.started_by {
                self.started_by.insert(row, lead);
            }
            if let Some(automation) = &summary.automation {
                self.automation_state
                    .session_automations
                    .insert(row, automation.clone());
            }
        }
        for project in &state.projects {
            self.groups
                .insert(project.path.clone(), project.groups.clone());
        }
        self.reminders.clone_from(&state.reminders);
        for lead in &state.orchestrations {
            self.orchestration_summaries
                .insert(lead.lead, lead.summary.clone());
        }
        if self.note_count != Some(state.notes) {
            self.note_count = Some(state.notes);
            self.notify_sidebar(cx);
        }
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
        let op = OpId::new();
        if let Some(echo) = echo {
            echo(self, cx);
            self.mirror.echoes.push(Echoed {
                op,
                echo,
                settled: false,
            });
        }
        let previous = self.group_writes.take();
        self.group_writes = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let Ok(call) = this.update(cx, |this, cx| {
                engine_client::call::<C>(&this.store, Some(op), body, cx)
            }) else {
                return;
            };
            let reply = call.await;
            let _ = this.update(cx, |this, cx| {
                if reply.is_err() {
                    this.mirror.echoes.retain(|echoed| echoed.op != op);
                    this.show_workspace(cx);
                } else if let Some(echoed) =
                    this.mirror.echoes.iter_mut().find(|echoed| echoed.op == op)
                {
                    echoed.settled = true;
                }
                settle(this, reply, cx);
                this.invalidate_chrome(cx);
                cx.notify();
            });
        }));
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
        self.workspace_call::<C>(body, echo, cx, move |this, reply, _| match reply {
            Err(error) | Ok(Outcome::Rejected(Rejection::Denied(error))) => {
                this.error = Some(format!("Could not {what}: {error}"));
            }
            Ok(_) => {}
        });
    }

    /// Creates a session in the engine under a new id, answering with its stored summary and,
    /// unless it starts `Bare`, the settings it starts with.
    pub(super) fn create_in_engine(
        &mut self,
        provider: &str,
        cwd: &str,
        title: Option<String>,
        start: NewSession,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Result<(Session, Option<Settings>), String>> {
        let body = CreateSession {
            session: SessionId::new(),
            provider: provider.to_owned(),
            cwd: cwd.to_owned(),
            title,
            start,
            started_by: None,
        };
        let call = engine_client::call::<CreateSession>(&self.store, Some(OpId::new()), body, cx);
        cx.spawn(async move |_, _| {
            let created = call.await?;
            match created.outcome {
                Outcome::Applied => {}
                Outcome::Rejected(Rejection::Denied(reason)) => return Err(reason),
                Outcome::Rejected(rejection) => {
                    return Err(format!("The session was not created: {rejection:?}"));
                }
            }
            let summary = created.session.ok_or("The engine created no session")?;
            Ok((summary.session, created.settings.map(|settings| *settings)))
        })
    }

    /// Makes a call that changes nothing, off the main thread.
    pub(super) fn workspace_read<C: Call + 'static>(
        &mut self,
        body: C::Params,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Result<C::Reply, String>>
    where
        C::Params: Send + 'static,
        C::Reply: Send + 'static,
    {
        engine_client::call::<C>(&self.store, None, body, cx)
    }
}
