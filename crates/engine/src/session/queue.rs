//! A session as a [`Feed`]: its watch rides the engine's one [`Topic`](crate::feed::Topic),
//! under the rules every watch keeps (engine.md "Backpressure"). A slow watcher's queue merges
//! what it holds: an `Append` grows the pending `Append` of the same row (or the pending `Rows`
//! holding that row), a `Rows` patch absorbs the older patches of the rows it covers, and a
//! `Status` folds into the pending `Status`. Row events and the other events commute under
//! `reduce::apply`, so each merge leaves the state the drained events build exactly as the
//! unmerged ones would.
use onicode_protocol::session::{SessionEvent, SessionState};

use super::reduce::{apply, merge_turns};
use crate::feed::{Feed, Pending};

pub(crate) struct SessionFeed;

fn is_row_event(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Rows { .. } | SessionEvent::Append { .. }
    )
}

impl Feed for SessionFeed {
    type Event = SessionEvent;
    type State = SessionState;

    fn apply(state: &mut SessionState, event: &SessionEvent) {
        apply(state, event);
    }

    /// About how many bytes `event` takes on the wire: exact enough to bound memory, cheap on the
    /// streaming path.
    fn weight(event: &SessionEvent) -> usize {
        match event {
            SessionEvent::Rows { rows, .. } => {
                16 + rows
                    .iter()
                    .map(|row| 48 + row.role.len() + row.text.len())
                    .sum::<usize>()
            }
            SessionEvent::Append { text, .. } => 24 + text.len(),
            SessionEvent::BtwText { thread, text } => 32 + thread.len() + text.len(),
            SessionEvent::Status { status, turn } => {
                64 + status.len()
                    + turn
                        .as_ref()
                        .and_then(|turn| turn.provider_turn_id.as_ref())
                        .map_or(0, String::len)
            }
            other => serde_json::to_vec(other).map_or(256, |bytes| bytes.len()),
        }
    }

    fn absorb(pending: &mut Pending<Self>, event: SessionEvent) -> bool {
        match event {
            SessionEvent::Append { row, text } => {
                if let Some(at) = pending.rposition(is_row_event) {
                    let merged = match pending.event_mut(at) {
                        SessionEvent::Append {
                            row: held,
                            text: grown,
                        } if *held == row => {
                            grown.push_str(&text);
                            true
                        }
                        SessionEvent::Rows { start, rows }
                            if row >= *start && row - *start < rows.len() =>
                        {
                            rows[row - *start].text.push_str(&text);
                            true
                        }
                        _ => false,
                    };
                    if merged {
                        pending.reweigh(at);
                        return false;
                    }
                }
                pending.append(SessionEvent::Append { row, text })
            }
            SessionEvent::Rows { start, rows } => {
                // Everything pending from `start` on is replaced.
                pending.retain(|held| match held {
                    SessionEvent::Rows { start: from, .. } => *from < start,
                    SessionEvent::Append { row, .. } => *row < start,
                    _ => true,
                });
                if let Some(at) = pending.rposition(is_row_event)
                    && let SessionEvent::Rows {
                        start: from,
                        rows: held,
                    } = pending.event_mut(at)
                    && *from + held.len() >= start
                {
                    held.truncate(start - *from);
                    held.extend(rows);
                    pending.reweigh(at);
                    return false;
                }
                pending.append(SessionEvent::Rows { start, rows })
            }
            SessionEvent::Status { status, turn } => {
                if let Some(at) = pending.rposition(|held| !is_row_event(held))
                    && let SessionEvent::Status {
                        status: held,
                        turn: held_turn,
                    } = pending.event_mut(at)
                    && let Some(merged) = merge_turns(held_turn.as_ref(), turn.as_ref())
                {
                    *held = status;
                    *held_turn = merged;
                    pending.reweigh(at);
                    return false;
                }
                pending.append(SessionEvent::Status { status, turn })
            }
            other => pending.append(other),
        }
    }
}
