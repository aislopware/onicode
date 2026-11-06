//! The reference the hub is tested against (engine.md "Tests → Session model"): a session as a
//! view that merges `turn::Update`s directly, the way the app did before the hub. Whatever the
//! hub splits, numbers, queues, merges, rings and resyncs, a watcher must end up showing this.
use onicode_core::agent::{Approval, Handoff, Question};
use onicode_core::context::Usage;
use onicode_core::provider::{BackgroundTask, NativeCommand};
use onicode_core::transcript::Row;
use onicode_core::usage::Limit;
use onicode_protocol::session::SessionState;

use crate::turn::Update;

#[derive(Clone, Debug, Default)]
pub(crate) struct SessionModel {
    pub(crate) rows: Vec<Row>,
    pub(crate) status: String,
    pub(crate) approvals: Vec<Approval>,
    pub(crate) questions: Vec<Question>,
    pub(crate) title: String,
    pub(crate) provider_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) worktree: Option<String>,
    pub(crate) context: Option<Usage>,
    pub(crate) usage_limit: Option<Limit>,
    pub(crate) background: Vec<BackgroundTask>,
    pub(crate) commands: Vec<NativeCommand>,
    pub(crate) handoff: Option<Handoff>,
    pub(crate) provider_turn_id: Option<String>,
    pub(crate) finished: bool,
}

impl SessionModel {
    /// A model of the session `state` shows.
    pub(crate) fn of(state: &SessionState) -> Self {
        Self {
            rows: state.rows.clone(),
            status: state.status.clone(),
            approvals: state.approvals.clone(),
            questions: state.questions.clone(),
            title: state.title.clone(),
            provider_id: state.provider_id.clone(),
            model: state.model.clone(),
            worktree: state.worktree.clone(),
            context: state.context,
            usage_limit: state.usage_limit.clone(),
            background: state.background.clone(),
            commands: state.commands.clone(),
            handoff: state.handoff.clone(),
            provider_turn_id: state
                .turn
                .as_ref()
                .and_then(|turn| turn.provider_turn_id.clone()),
            finished: state.turn.as_ref().is_some_and(|turn| turn.finished),
        }
    }

    /// Merges one update, field by field.
    pub(crate) fn update(&mut self, update: &Update) {
        self.rows.truncate(update.start);
        self.rows.extend(update.rows.iter().cloned());
        if let Some(cwd) = &update.worktree_cwd {
            self.worktree = Some(cwd.clone());
        }
        if let Some(commands) = &update.commands {
            self.commands.clone_from(commands);
        }
        if let Some(id) = &update.provider_id {
            self.provider_id = Some(id.clone());
        }
        if let Some(handoff) = &update.handoff {
            self.handoff.clone_from(handoff);
        }
        if let Some(model) = &update.model {
            self.model = Some(model.clone());
        }
        if let Some(title) = &update.title {
            self.title.clone_from(title);
        }
        if let Some(usage) = update.context {
            self.context = Some(usage);
        }
        if let Some(tasks) = &update.background {
            self.background.clone_from(tasks);
        }
        for approval in &update.approvals {
            if !self.approvals.iter().any(|known| known.id == approval.id) {
                self.approvals.push(approval.clone());
            }
        }
        self.approvals
            .retain(|known| !update.approvals_resolved.contains(&known.id));
        for question in &update.questions {
            if !self.questions.iter().any(|known| known.id == question.id) {
                self.questions.push(question.clone());
            }
        }
        self.questions
            .retain(|known| !update.questions_resolved.contains(&known.id));
        if let Some(limit) = &update.usage_limit {
            self.usage_limit = Some(limit.clone());
        }
        if self.finished {
            // The next turn's.
            self.provider_turn_id = None;
        }
        if update.provider_turn_id.is_some() {
            self.provider_turn_id.clone_from(&update.provider_turn_id);
        }
        self.status.clone_from(&update.status);
        self.finished = update.finished;
        if update.finished {
            self.approvals.clear();
            self.questions.clear();
            self.background.clear();
        }
    }

    /// Why `state` does not show this model, if it does not. Rows are compared from
    /// `state.first_row` on and without their ids, which only the host adds.
    pub(crate) fn differs(&self, state: &SessionState) -> Option<String> {
        let ours: Vec<_> = self
            .rows
            .iter()
            .skip(state.first_row)
            .map(|row| (&row.role, &row.text, row.first, row.sent_at))
            .collect();
        let theirs: Vec<_> = state
            .rows
            .iter()
            .map(|row| (&row.role, &row.text, row.first, row.sent_at))
            .collect();
        if ours != theirs {
            return Some(format!("rows {ours:?} != {theirs:?}"));
        }
        let other = Self::of(state);
        let handoff = |model: &Self| format!("{:?}", model.handoff);
        let same = [
            ("status", self.status == other.status),
            ("approvals", self.approvals == other.approvals),
            ("questions", self.questions == other.questions),
            ("title", self.title == other.title),
            ("provider_id", self.provider_id == other.provider_id),
            ("model", self.model == other.model),
            ("worktree", self.worktree == other.worktree),
            ("context", self.context == other.context),
            ("usage_limit", self.usage_limit == other.usage_limit),
            ("background", self.background == other.background),
            ("commands", self.commands == other.commands),
            ("handoff", handoff(self) == handoff(&other)),
            (
                "provider_turn_id",
                self.provider_turn_id == other.provider_turn_id,
            ),
            ("finished", self.finished == other.finished),
        ];
        let differ: Vec<_> = same
            .iter()
            .filter(|(_, same)| !same)
            .map(|(name, _)| *name)
            .collect();
        (!differ.is_empty()).then(|| format!("{differ:?} differ\nmodel {self:?}\nstate {other:?}"))
    }
}
