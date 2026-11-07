//! `turn::Update` into `SessionEvent`s, one variant per field (engine.md "Protocol"). Rows are
//! diffed against the host's state, so a streaming update is an `Append` of what the row grew by
//! and an unchanged tail sends nothing. The receipts that `delivered`, `follow_up_delivered` and
//! `follow_up_errors` become are the hub's: they need its record of what each op was.
use std::collections::BTreeMap;

use onicode_core::transcript::Row;
use onicode_protocol::session::{SessionEvent, SessionState, TurnState};
use uuid::Uuid;

use crate::turn::Update;

/// `update` as events against `state`, the host's state before it, ending with its `Status`.
/// `prompts` names the prompt rows' ids by transcript index; the update's prompt rows carry them.
/// `follow_up_delivered`, `follow_up_errors` and `delivered` are left to the caller.
pub(crate) fn split(
    state: &SessionState,
    mut update: Update,
    prompts: &BTreeMap<usize, Uuid>,
) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    if let Some(cwd) = update.worktree_cwd {
        events.push(SessionEvent::Worktree(cwd));
    }
    if let Some(commands) = update.commands {
        events.push(SessionEvent::Commands(commands));
    }
    stamp(&mut update.rows, update.start, prompts);
    rows(state, update.start, update.rows, &mut events);
    if let Some(id) = update.provider_id {
        events.push(SessionEvent::ProviderId(Some(id)));
    }
    if let Some(handoff) = update.handoff {
        events.push(SessionEvent::Handoff(handoff));
    }
    if update.config_model.is_some() || update.config_settings.is_some() {
        let mut settings = state.settings.clone();
        if let Some(model) = update.config_model {
            settings.model = Some(model);
        }
        if let Some(changed) = update.config_settings {
            settings.model_settings.extend(changed);
        }
        events.push(SessionEvent::Settings(Box::new(settings)));
    }
    if let Some(model) = update.model {
        events.push(SessionEvent::Model(model));
    }
    if let Some(title) = update.title {
        events.push(SessionEvent::Title(title));
    }
    if let Some(usage) = update.context {
        events.push(SessionEvent::Context(usage));
    }
    if let Some(metrics) = update.metrics {
        events.push(SessionEvent::Metrics(metrics));
    }
    if let Some(tasks) = update.background {
        events.push(SessionEvent::Background(tasks));
    }
    // Asked, then answered: an approval raised and resolved in one update ends resolved.
    events.extend(update.approvals.into_iter().map(SessionEvent::Approval));
    events.extend(
        update
            .approvals_resolved
            .into_iter()
            .map(SessionEvent::ApprovalResolved),
    );
    events.extend(update.questions.into_iter().map(SessionEvent::Question));
    events.extend(
        update
            .questions_resolved
            .into_iter()
            .map(SessionEvent::QuestionResolved),
    );
    if let Some(limit) = update.usage_limit {
        events.push(SessionEvent::UsageLimit(Some(limit)));
    }
    events.push(SessionEvent::Status {
        status: update.status,
        turn: Some(TurnState {
            provider_turn_id: update.provider_turn_id,
            ready: update.turn_ready,
            finished: update.finished,
        }),
    });
    events
}

/// Puts each prompt's id on its first row.
pub(crate) fn stamp(rows: &mut [Row], start: usize, prompts: &BTreeMap<usize, Uuid>) {
    for (at, row) in rows.iter_mut().enumerate() {
        if row.first
            && row.is_prompt()
            && let Some(id) = prompts.get(&(start + at))
        {
            row.id = Some(*id);
        }
    }
}

/// The events that make `state`'s rows from `start` on equal `next`: none when they already
/// are, an `Append` per row when rows only grew, otherwise one `Rows` from the first difference.
pub(crate) fn rows(
    state: &SessionState,
    start: usize,
    mut next: Vec<Row>,
    events: &mut Vec<SessionEvent>,
) {
    let Some(base) = start.checked_sub(state.first_row) else {
        events.push(SessionEvent::Rows { start, rows: next });
        return;
    };
    if base > state.rows.len() {
        // Past the end: the rows follow the last one, as a merge of `start` and `rows` puts them.
        events.push(SessionEvent::Rows {
            start: state.first_row + state.rows.len(),
            rows: next,
        });
        return;
    }
    let held = &state.rows[base..];
    let same = held
        .iter()
        .zip(&next)
        .take_while(|(before, after)| before == after)
        .count();
    if same == next.len() && same == held.len() {
        return;
    }
    if held.len() == next.len() {
        let grown: Option<Vec<_>> = held
            .iter()
            .zip(&next)
            .enumerate()
            .skip(same)
            .filter(|(_, (before, after))| before != after)
            .map(|(at, (before, after))| {
                let grew = before.role == after.role
                    && before.first == after.first
                    && before.sent_at == after.sent_at
                    && before.id == after.id
                    && after.text.len() > before.text.len()
                    && after.text.starts_with(&before.text);
                grew.then(|| SessionEvent::Append {
                    row: start + at,
                    text: after.text[before.text.len()..].to_owned(),
                })
            })
            .collect();
        if let Some(grown) = grown {
            events.extend(grown);
            return;
        }
    }
    events.push(SessionEvent::Rows {
        start: start + same,
        rows: next.split_off(same),
    });
}
