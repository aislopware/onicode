//! The one reducer: how a `SessionEvent` changes a `SessionState`. The host applies it to keep
//! the state its snapshots are cut from, every watcher applies it to what it was sent, and the
//! watcher queue's merges are exactly the ones that leave its result unchanged.
use onicode_protocol::session::{SessionEvent, SessionState, TurnState};

/// Applies `event` to `state`.
pub fn apply(state: &mut SessionState, event: &SessionEvent) {
    match event {
        SessionEvent::Rows { start, rows } => {
            if *start < state.first_row {
                state.first_row = *start;
                state.rows.clone_from(rows);
            } else {
                state.rows.truncate(start - state.first_row);
                state.rows.extend(rows.iter().cloned());
            }
        }
        SessionEvent::Append { row, text } => {
            if let Some(row) = row
                .checked_sub(state.first_row)
                .and_then(|at| state.rows.get_mut(at))
            {
                row.text.push_str(text);
            }
        }
        SessionEvent::Status { status, turn } => {
            state.status.clone_from(status);
            if let Some(turn) = turn {
                merge_turn(&mut state.turn, turn);
                if turn.finished {
                    // A finished turn waits on nothing.
                    state.approvals.clear();
                    state.questions.clear();
                    state.background.clear();
                }
            }
        }
        SessionEvent::Approval(approval) => {
            if !state.approvals.iter().any(|known| known.id == approval.id) {
                state.approvals.push(approval.clone());
            }
        }
        SessionEvent::ApprovalResolved(id) => state.approvals.retain(|known| &known.id != id),
        SessionEvent::Question(question) => {
            if !state.questions.iter().any(|known| known.id == question.id) {
                state.questions.push(question.clone());
            }
        }
        SessionEvent::QuestionResolved(id) => state.questions.retain(|known| &known.id != id),
        SessionEvent::Queue { rev, state: queue } => {
            state.queue_rev = *rev;
            state.queue.clone_from(queue);
        }
        SessionEvent::Settings(settings) => state.settings = (**settings).clone(),
        SessionEvent::Title(title) => state.title.clone_from(title),
        SessionEvent::ProviderId(id) => state.provider_id.clone_from(id),
        SessionEvent::Model(model) => state.model = Some(model.clone()),
        SessionEvent::Worktree(cwd) => state.worktree = Some(cwd.clone()),
        SessionEvent::Context(usage) => state.context = Some(*usage),
        SessionEvent::UsageLimit(limit) => state.usage_limit.clone_from(limit),
        SessionEvent::Background(tasks) => state.background.clone_from(tasks),
        SessionEvent::Commands(commands) => state.commands.clone_from(commands),
        SessionEvent::Handoff(handoff) => state.handoff.clone_from(handoff),
        SessionEvent::TurnFinished { turn, info } => {
            state.settings.turns.insert(*turn, info.clone());
        }
        // A turn's metrics live in its `TurnFinished` receipt; a receipt changes nothing, and a
        // by-the-way answer is the `Btw` call's, not the session's.
        SessionEvent::Metrics(_) | SessionEvent::Receipt { .. } | SessionEvent::BtwText { .. } => {}
    }
}

/// Folds a `Status` event's `turn` into the running turn's, or starts the next turn's.
pub(crate) fn merge_turn(into: &mut Option<TurnState>, next: &TurnState) {
    match into {
        Some(current) if !current.finished => {
            if next.provider_turn_id.is_some() {
                current.provider_turn_id.clone_from(&next.provider_turn_id);
            }
            current.ready = (current.ready || next.ready) && !next.finished;
            current.finished = next.finished;
        }
        _ => {
            *into = Some(TurnState {
                provider_turn_id: next.provider_turn_id.clone(),
                ready: next.ready && !next.finished,
                finished: next.finished,
            });
        }
    }
}

/// `first` then `second`, as one `turn`: `None` when the two cannot be one (a finished turn
/// followed by the next one's start).
pub(crate) fn merge_turns(
    first: Option<&TurnState>,
    second: Option<&TurnState>,
) -> Option<Option<TurnState>> {
    match (first, second) {
        (None, second) => Some(second.cloned()),
        (Some(first), None) => Some(Some(first.clone())),
        (Some(first), Some(_)) if first.finished => None,
        (Some(first), Some(second)) => Some(Some(TurnState {
            provider_turn_id: second
                .provider_turn_id
                .clone()
                .or_else(|| first.provider_turn_id.clone()),
            ready: first.ready || second.ready,
            finished: second.finished,
        })),
    }
}
