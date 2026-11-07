//! The workspace as a [`Feed`]: its watch rides the engine's one [`Topic`](crate::feed::Topic),
//! under the rules every watch keeps (engine.md "Backpressure"). A pending event is replaced by a
//! later one that supersedes it (`reduce::supersedes`).
use onicode_protocol::workspace::{WorkspaceEvent, WorkspaceState};

use super::reduce::{apply, supersedes};
use crate::feed::{Feed, Pending};

pub(crate) struct WorkspaceFeed;

impl Feed for WorkspaceFeed {
    type Event = WorkspaceEvent;
    type State = WorkspaceState;

    fn apply(state: &mut WorkspaceState, event: &WorkspaceEvent) {
        apply(state, event);
    }

    /// About how many bytes `event` takes on the wire.
    fn weight(event: &WorkspaceEvent) -> usize {
        match event {
            WorkspaceEvent::Removed(_) | WorkspaceEvent::Notes(_) => 16,
            WorkspaceEvent::Receipt { .. } => 64,
            other => serde_json::to_vec(other).map_or(256, |bytes| bytes.len()),
        }
    }

    fn absorb(pending: &mut Pending<Self>, event: WorkspaceEvent) -> bool {
        pending.replace_or_append(event, supersedes)
    }
}
