//! How a [`WorkspaceEvent`] changes a [`WorkspaceState`]: the host applies its own events with
//! it, and so does every viewer's mirror. Every list keeps one order whatever order events for
//! different keys arrive in, so a watcher queue may merge what it holds (`queue.rs`).
use std::cmp::Reverse;

use onicode_protocol::workspace::{WorkspaceEvent, WorkspaceState};

/// Applies `event` to `state`.
pub fn apply(state: &mut WorkspaceState, event: &WorkspaceEvent) {
    match event {
        WorkspaceEvent::Session(summary) => {
            let row = summary.session.id;
            state.sessions.retain(|known| known.session.id != row);
            state.sessions.push((**summary).clone());
            // The store's order: newest first, then the later row.
            state.sessions.sort_by_key(|known| {
                (Reverse(known.session.updated_at), Reverse(known.session.id))
            });
        }
        WorkspaceEvent::Removed(row) => state.sessions.retain(|known| known.session.id != *row),
        WorkspaceEvent::Project(project) => {
            match state
                .projects
                .binary_search_by(|known| known.path.as_str().cmp(&project.path))
            {
                Ok(at) => state.projects[at] = project.clone(),
                Err(at) => state.projects.insert(at, project.clone()),
            }
        }
        WorkspaceEvent::Reminders(reminders) => state.reminders.clone_from(reminders),
        WorkspaceEvent::Notes(count) => state.notes = *count,
        WorkspaceEvent::AutomationDue(due) => {
            if !state
                .automations
                .iter()
                .any(|known| known.run.id == due.run.id)
            {
                state.automations.push((**due).clone());
            }
        }
        WorkspaceEvent::AutomationTaken(run) => {
            state.automations.retain(|known| known.run.id != *run);
        }
        WorkspaceEvent::Receipt { .. } => {}
    }
}

/// The key an event replaces state under: a later event of the same key supersedes an earlier
/// one whole, and events of different keys commute.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key<'a> {
    Session(i64),
    Project(&'a str),
    Reminders,
    Notes,
}

fn key(event: &WorkspaceEvent) -> Option<Key<'_>> {
    Some(match event {
        WorkspaceEvent::Session(summary) => Key::Session(summary.session.id),
        WorkspaceEvent::Removed(row) => Key::Session(*row),
        WorkspaceEvent::Project(project) => Key::Project(&project.path),
        WorkspaceEvent::Reminders(_) => Key::Reminders,
        WorkspaceEvent::Notes(_) => Key::Notes,
        WorkspaceEvent::AutomationDue(_)
        | WorkspaceEvent::AutomationTaken(_)
        | WorkspaceEvent::Receipt { .. } => return None,
    })
}

/// Whether `later` supersedes `earlier` whole: applying both leaves what applying `later` alone
/// does.
pub(crate) fn supersedes(later: &WorkspaceEvent, earlier: &WorkspaceEvent) -> bool {
    matches!((key(later), key(earlier)), (Some(a), Some(b)) if a == b)
}
