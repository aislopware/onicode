//! The workspace hub's reference (engine.md "Tests"): the calls applied to plain maps, with none
//! of the hub's store, ledger, numbering, ring or queues. Whatever the hub does with those, what
//! it shows must be what this model holds.
use std::collections::{BTreeMap, BTreeSet};

use onicode_core::session_groups::{Groups, WorkItem};
use onicode_protocol::workspace::{ReminderChange, WorkspaceState};
use onicode_protocol::{Outcome, Rejection};

/// What the sidebar shows of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Mark {
    pub(super) title: String,
    pub(super) archived: bool,
    pub(super) pinned: bool,
    pub(super) work_item: Option<WorkItem>,
}

/// The parts of the workspace the model holds, as comparable values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Shown {
    pub(super) sessions: BTreeMap<i64, Mark>,
    /// Only the projects whose folders were saved; every other project has none.
    pub(super) groups: BTreeMap<String, Groups>,
    /// Due time and when it fired, by session.
    pub(super) reminders: BTreeMap<i64, (u64, Option<u64>)>,
    pub(super) notes: usize,
}

impl Shown {
    /// What `state` shows.
    pub(super) fn of(state: &WorkspaceState) -> Self {
        Self {
            sessions: state
                .sessions
                .iter()
                .map(|summary| {
                    (
                        summary.session.id,
                        Mark {
                            title: summary.session.title.clone(),
                            archived: summary.session.archived,
                            pinned: summary.session.pinned,
                            work_item: summary.session.work_item.clone(),
                        },
                    )
                })
                .collect(),
            groups: state
                .projects
                .iter()
                .filter(|project| project.groups != Groups::default())
                .map(|project| (project.path.clone(), project.groups.clone()))
                .collect(),
            reminders: state
                .reminders
                .iter()
                .map(|reminder| (reminder.session_id, (reminder.due_at, reminder.fired_at)))
                .collect(),
            notes: state.notes,
        }
    }
}

#[derive(Default)]
pub(super) struct WorkspaceModel {
    pub(super) shown: Shown,
    /// Created sessions nobody has prompted yet.
    unprompted: BTreeSet<i64>,
}

const GONE: Outcome = Outcome::Rejected(Rejection::Gone);

impl WorkspaceModel {
    pub(super) fn create(&mut self, row: i64, title: String) {
        self.unprompted.insert(row);
        self.shown.sessions.insert(
            row,
            Mark {
                title,
                archived: false,
                pinned: false,
                work_item: None,
            },
        );
    }

    pub(super) fn rename(&mut self, row: i64, title: &str, expected: Option<&str>) -> Outcome {
        let Some(mark) = self.shown.sessions.get_mut(&row) else {
            return GONE;
        };
        if expected.is_some_and(|expected| expected != mark.title) {
            return Outcome::Rejected(Rejection::StaleRev);
        }
        title.clone_into(&mut mark.title);
        Outcome::Applied
    }

    pub(super) fn archive(&mut self, row: i64, archived: bool) -> Outcome {
        let Some(mark) = self.shown.sessions.get_mut(&row) else {
            return GONE;
        };
        mark.archived = archived;
        Outcome::Applied
    }

    pub(super) fn pin(&mut self, rows: &[i64], pinned: bool) -> Outcome {
        let mut any = false;
        for row in rows {
            if let Some(mark) = self.shown.sessions.get_mut(row) {
                mark.pinned = pinned;
                any = true;
            }
        }
        if any { Outcome::Applied } else { GONE }
    }

    /// A viewer prompted the session: it is the person's now.
    pub(super) fn send(&mut self, row: i64) {
        self.unprompted.remove(&row);
    }

    /// The session's last viewer closed it: unprompted, it goes.
    pub(super) fn close(&mut self, row: i64) {
        if self.unprompted.remove(&row) {
            self.delete(row);
        }
    }

    pub(super) fn delete(&mut self, row: i64) -> Outcome {
        self.unprompted.remove(&row);
        if self.shown.sessions.remove(&row).is_none() {
            return GONE;
        }
        self.shown.reminders.remove(&row);
        for groups in self.shown.groups.values_mut() {
            if groups.folder_of(row).is_some() {
                groups.remove_sessions(&[row]);
            }
        }
        self.shown
            .groups
            .retain(|_, groups| *groups != Groups::default());
        Outcome::Applied
    }

    pub(super) fn set_work_item(
        &mut self,
        row: i64,
        item: Option<WorkItem>,
        if_unlinked: bool,
    ) -> Outcome {
        let Some(mark) = self.shown.sessions.get_mut(&row) else {
            return GONE;
        };
        if if_unlinked && mark.work_item.is_some() {
            return Outcome::Rejected(Rejection::AlreadyResolved);
        }
        mark.work_item = item;
        Outcome::Applied
    }

    pub(super) fn set_reminder(&mut self, rows: &[i64], change: &ReminderChange) -> Outcome {
        match *change {
            ReminderChange::At(due) => {
                for row in rows {
                    self.shown.reminders.insert(*row, (due, None));
                }
                Outcome::Applied
            }
            ReminderChange::Snooze { expected, due_at } => {
                let mut moved = false;
                for row in rows {
                    if let Some(reminder) = self.shown.reminders.get_mut(row)
                        && reminder.0 == expected
                    {
                        *reminder = (due_at, None);
                        moved = true;
                    }
                }
                if moved {
                    Outcome::Applied
                } else {
                    Outcome::Rejected(Rejection::StaleRev)
                }
            }
            ReminderChange::Clear { expected } => {
                for row in rows {
                    if self
                        .shown
                        .reminders
                        .get(row)
                        .is_some_and(|reminder| expected.is_none_or(|due| reminder.0 == due))
                    {
                        self.shown.reminders.remove(row);
                    }
                }
                Outcome::Applied
            }
        }
    }

    pub(super) fn save_groups(&mut self, project: &str, groups: Groups) {
        if groups == Groups::default() {
            self.shown.groups.remove(project);
        } else {
            self.shown.groups.insert(project.to_owned(), groups);
        }
    }

    /// The schedulers' beat at `now`: due reminders fire, once.
    pub(super) fn tick(&mut self, now: u64) {
        for reminder in self.shown.reminders.values_mut() {
            if reminder.0 <= now && reminder.1.is_none() {
                reminder.1 = Some(now);
            }
        }
    }
}
