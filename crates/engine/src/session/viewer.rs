//! A watcher's side of a session: a mirror of the host's state built from the items it is sent,
//! and the fold of everything since the last beat back into one `turn::Update`. The app's
//! conversation still merges `Update`s (its queue, approvals and questions move into the engine
//! later), so this is how it reads a turn the hub runs: one update per beat of its pulse.
use std::collections::HashMap;

use onicode_protocol::session::{SessionEvent, SessionState};
use onicode_protocol::{Cursor, Item, MessageId, OpId, Outcome, Rejection};

use super::reduce::apply;
use crate::turn::Update;

/// What a mutating call's receipt means to the viewer that made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// A `Send`; `queued` is the queued message it launched, delivered by the receipt.
    Launch { queued: Option<String> },
    /// A `Steer` of the queued message `id`: delivered by an applied receipt, failed by a denied
    /// one.
    Steer { id: String },
}

#[derive(Default)]
pub struct Viewer {
    state: SessionState,
    cursor: Option<Cursor>,
    expected: HashMap<OpId, Expect>,
    echoes: Vec<MessageId>,
    /// Folded since the last `take`.
    pending: Update,
    /// An update (a `Status`) arrived since the last `take`.
    updated: bool,
    /// The first row changed since the last `take`.
    dirty: Option<usize>,
    /// The steers accepted in this turn, and the queued message it launched: repeated in the
    /// update that finishes it, as the turn's own final update repeats them.
    accepted: Vec<String>,
    delivered: Option<String>,
}

impl Viewer {
    /// The host's state as this viewer last heard it.
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Where this viewer stands, to resume from.
    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// Remembers what `op`'s receipt will mean.
    pub fn expect(&mut self, op: OpId, expect: Expect) {
        self.expected.insert(op, expect);
    }

    /// Notes a prompt shown before the host has it: pending until the host's row with `id`
    /// arrives.
    pub fn echo(&mut self, id: MessageId) {
        self.echoes.push(id);
    }

    /// The echoes the host has not answered with a row yet.
    #[must_use]
    pub fn pending_echoes(&self) -> &[MessageId] {
        &self.echoes
    }

    /// Takes in one item; `true` when it finished the running turn.
    pub fn absorb(&mut self, item: Item<SessionState, SessionEvent>) -> bool {
        match item {
            Item::Snapshot { at, state } => {
                let finished = self.cursor.is_some()
                    && state.turn.as_ref().is_some_and(|turn| turn.finished)
                    && !self.state.turn.as_ref().is_some_and(|turn| turn.finished);
                if self.cursor.is_some() {
                    self.catch_up(&state);
                }
                self.cursor = Some(at);
                self.state = state;
                self.resolve_echoes();
                finished
            }
            Item::Events { last, events, .. } => {
                let mut finished = false;
                for event in &events {
                    finished |= self.event(event);
                }
                if let Some(cursor) = &mut self.cursor {
                    cursor.seq = last;
                }
                self.resolve_echoes();
                finished
            }
            Item::Resync => false,
        }
    }

    /// The turn was refused before it started: it finishes with `reason`.
    pub fn refuse(&mut self, reason: String) {
        self.pending.status = reason;
        self.pending.finished = true;
        self.updated = true;
    }

    /// A steer of the queued message `id` that never reached the host: it fails with `error` in
    /// the next update.
    pub fn fail_steer(&mut self, id: String, error: String) {
        self.pending.follow_up_errors.push((id, error));
        self.updated = true;
    }

    /// Everything since the last call as one update, once an update arrived. Rows carry no ids:
    /// the view patches by index and its rows equal the store's.
    pub fn take(&mut self) -> Option<Update> {
        if !self.updated {
            return None;
        }
        self.updated = false;
        let mut update = std::mem::take(&mut self.pending);
        let end = self.state.first_row + self.state.rows.len();
        let start = self.dirty.take().unwrap_or(end).max(self.state.first_row);
        update.start = start;
        update.rows =
            self.state.rows[(start - self.state.first_row).min(self.state.rows.len())..].to_vec();
        for row in &mut update.rows {
            row.id = None;
        }
        if update.finished {
            for id in self.accepted.drain(..) {
                if !update.follow_up_delivered.contains(&id) {
                    update.follow_up_delivered.push(id);
                }
            }
            update.delivered = update.delivered.or(self.delivered.take());
            self.expected.clear();
        }
        Some(update)
    }

    /// Folds one event into the pending update and the mirror; `true` when it finished the turn.
    fn event(&mut self, event: &SessionEvent) -> bool {
        let mut finished = false;
        match event {
            SessionEvent::Rows { start, .. } => self.touch(*start),
            SessionEvent::Append { row, .. } => self.touch(*row),
            SessionEvent::Status { status, turn } => {
                self.updated = true;
                self.pending.status.clone_from(status);
                if let Some(turn) = turn {
                    if turn.provider_turn_id.is_some() {
                        self.pending
                            .provider_turn_id
                            .clone_from(&turn.provider_turn_id);
                    }
                    self.pending.turn_ready |= turn.ready;
                    if turn.finished {
                        self.pending.finished = true;
                        finished = true;
                    }
                }
            }
            SessionEvent::Approval(approval) => self.pending.approvals.push(approval.clone()),
            SessionEvent::ApprovalResolved(id) => self.pending.approvals_resolved.push(id.clone()),
            SessionEvent::Question(question) => self.pending.questions.push(question.clone()),
            SessionEvent::QuestionResolved(id) => self.pending.questions_resolved.push(id.clone()),
            SessionEvent::Settings(settings) => {
                // What the provider reported changing: the model and the controls that moved.
                if settings.model.is_some() && settings.model != self.state.settings.model {
                    self.pending.config_model.clone_from(&settings.model);
                }
                for (key, value) in &settings.model_settings {
                    if self.state.settings.model_settings.get(key) != Some(value) {
                        self.pending
                            .config_settings
                            .get_or_insert_with(HashMap::new)
                            .insert(key.clone(), value.clone());
                    }
                }
            }
            SessionEvent::Title(title) => self.pending.title = Some(title.clone()),
            // A thread let go reaches the conversation through the call that let it go.
            SessionEvent::ProviderId(id) => {
                if let Some(id) = id {
                    self.pending.provider_id = Some(id.clone());
                }
            }
            SessionEvent::Model(model) => self.pending.model = Some(model.clone()),
            SessionEvent::Worktree(cwd) => self.pending.worktree_cwd = Some(cwd.clone()),
            SessionEvent::Context(usage) => self.pending.context = Some(*usage),
            SessionEvent::Metrics(metrics) => self.pending.metrics = Some(*metrics),
            SessionEvent::UsageLimit(limit) => self.pending.usage_limit.clone_from(limit),
            SessionEvent::Background(tasks) => self.pending.background = Some(tasks.clone()),
            SessionEvent::Commands(commands) => self.pending.commands = Some(commands.clone()),
            SessionEvent::Handoff(handoff) => self.pending.handoff = Some(handoff.clone()),
            SessionEvent::Receipt { op, outcome } => self.receipt(*op, outcome),
            SessionEvent::TurnFinished { info, .. } => self.pending.git.clone_from(&info.git),
            SessionEvent::Queue { .. } | SessionEvent::BtwText { .. } => {}
        }
        apply(&mut self.state, event);
        finished
    }

    fn receipt(&mut self, op: OpId, outcome: &Outcome) {
        match (self.expected.remove(&op), outcome) {
            (Some(Expect::Launch { queued: Some(id) }), Outcome::Applied) => {
                self.delivered = Some(id.clone());
                self.pending.delivered = Some(id);
            }
            (Some(Expect::Steer { id }), Outcome::Applied) => {
                self.accepted.push(id.clone());
                self.pending.follow_up_delivered.push(id);
            }
            (Some(Expect::Steer { id }), Outcome::Rejected(Rejection::Denied(error))) => {
                self.pending.follow_up_errors.push((id, error.clone()));
            }
            // A launch the host refused finishes with its reason; a steer the turn outlived
            // fails in the view when the turn finishes.
            _ => {}
        }
    }

    fn touch(&mut self, row: usize) {
        self.dirty = Some(self.dirty.map_or(row, |dirty| dirty.min(row)));
    }

    /// A fresh snapshot in the middle of a turn: what changed since the mirror, as events would
    /// have said it.
    fn catch_up(&mut self, next: &SessionState) {
        let held = &self.state;
        let first = held.first_row.min(next.first_row);
        let changed = (first..)
            .take_while(|at| {
                let before = at
                    .checked_sub(held.first_row)
                    .and_then(|at| held.rows.get(at));
                let after = at
                    .checked_sub(next.first_row)
                    .and_then(|at| next.rows.get(at));
                (before.is_some() || after.is_some()) && before == after
            })
            .last()
            .map_or(first, |at| at + 1);
        let end = held.first_row + held.rows.len();
        if changed < end.max(next.first_row + next.rows.len()) {
            self.dirty = Some(self.dirty.map_or(changed, |dirty| dirty.min(changed)));
        }
        for approval in &next.approvals {
            if !held.approvals.iter().any(|known| known.id == approval.id) {
                self.pending.approvals.push(approval.clone());
            }
        }
        for approval in &held.approvals {
            if !next.approvals.iter().any(|known| known.id == approval.id) {
                self.pending.approvals_resolved.push(approval.id.clone());
            }
        }
        for question in &next.questions {
            if !held.questions.iter().any(|known| known.id == question.id) {
                self.pending.questions.push(question.clone());
            }
        }
        for question in &held.questions {
            if !next.questions.iter().any(|known| known.id == question.id) {
                self.pending.questions_resolved.push(question.id.clone());
            }
        }
        self.pending.status.clone_from(&next.status);
        if let Some(turn) = &next.turn {
            self.pending.finished |= turn.finished;
            self.pending.turn_ready |= turn.ready;
        }
        self.updated = true;
    }

    fn resolve_echoes(&mut self) {
        let rows = &self.state.rows;
        self.echoes
            .retain(|echo| !rows.iter().any(|row| row.id == Some(echo.0)));
    }
}
