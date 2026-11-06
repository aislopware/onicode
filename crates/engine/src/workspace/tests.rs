//! The workspace hub against its reference (engine.md "Tests"). Seeded interleavings of calls,
//! scheduler beats in virtual time, and viewers that join, drop, fall behind and come back with
//! their cursor: the hub must show what the `WorkspaceModel` holds, every viewer must end up
//! showing the hub, and a snapshot plus the events after it must be the later snapshot. Nothing
//! here waits on a clock, runs a provider or touches the network.
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use futures::executor::{LocalPool, block_on};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::task::LocalSpawnExt;
use futures::{FutureExt, StreamExt};
use onicode_core::automations::{AutomationUpsert, Service};
use onicode_core::session_groups::{Folder, Groups, WorkItem};
use onicode_core::session_title::{GeneratedTitle, TextModel, WorkItemHint};
use onicode_core::store::Store;
use onicode_protocol::workspace::{
    Archive, CreateSession, Created, Delete, GenerateTitle, NewSession, Pin, ReminderChange,
    Rename, SaveGroups, SetReminder, SetWorkItem, WatchWorkspace, WorkspaceEvent, WorkspaceState,
};
use onicode_protocol::{Cursor, Item, OpId, Outcome, Rejection, SessionId};
use proptest::prelude::*;

use super::chores::Chores;
use super::hub::{Limits, WorkspaceHub};
use super::model::{Shown, WorkspaceModel};
use super::reduce::apply;
use crate::session::Spawn;
use crate::turn::Database;

type Work = Arc<Mutex<Vec<BoxFuture<'static, ()>>>>;
type Stream = BoxStream<'static, Item<WorkspaceState, WorkspaceEvent>>;

/// The hub's background work, held until the test runs it.
struct Queued(Work);

impl Spawn for Queued {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.lock().unwrap().push(work);
    }
}

/// Chores that answer at once: a title from the prompt, a work item from a pasted link.
struct Scripted;

impl Chores for Scripted {
    fn title(
        &self,
        _: TextModel,
        _: &Path,
        message: &str,
        _: Arc<AtomicBool>,
    ) -> Option<GeneratedTitle> {
        Some(GeneratedTitle {
            title: format!("About {message}"),
            work_item: None,
        })
    }

    fn work_item(&self, message: &str, _: &Path, _: Option<&WorkItemHint>) -> Option<WorkItem> {
        onicode_core::session_groups::parse_github_work_item(message)
    }

    fn branch(&self, _: TextModel, _: &Path, _: &str, _: Arc<AtomicBool>) -> Option<String> {
        None
    }
}

const PROJECT: &str = "/project";

struct Harness {
    pool: LocalPool,
    work: Work,
    hub: Arc<WorkspaceHub>,
    store: Database,
}

impl Harness {
    fn new(limits: Limits) -> Self {
        let store = Store::open(Path::new(":memory:")).unwrap();
        Self::over(Arc::new(Mutex::new(store)), limits, 0)
    }

    /// A hub over `store` that started at `started` (Unix ms), as after a restart.
    fn over(store: Database, limits: Limits, started: u64) -> Self {
        let work = Work::default();
        let hub = WorkspaceHub::build(
            store.clone(),
            Arc::new(Queued(work.clone())),
            Arc::new(Scripted),
            limits,
            started,
        );
        Self {
            pool: LocalPool::new(),
            work,
            hub,
            store,
        }
    }

    /// Runs the hub's background work until nothing more can happen.
    fn settle(&mut self) {
        loop {
            let work = std::mem::take(&mut *self.work.lock().unwrap());
            let idle = work.is_empty();
            for work in work {
                self.pool.spawner().spawn_local(work).unwrap();
            }
            self.pool.run_until_stalled();
            if idle && self.work.lock().unwrap().is_empty() {
                return;
            }
        }
    }

    fn create(&self, op: Option<OpId>, title: Option<String>) -> Created {
        block_on(self.hub.create_session(
            op,
            CreateSession {
                session: SessionId::new(),
                provider: "codex".into(),
                cwd: PROJECT.into(),
                title,
                start: NewSession::Bare,
                started_by: None,
            },
        ))
        .unwrap()
    }

    fn state(&self) -> WorkspaceState {
        self.hub.snapshot().unwrap().1
    }

    /// A viewer's turn in session `row`: it starts, writes its prompt and ends.
    fn prompt(&self, row: i64) {
        self.hub.turn_started(row);
        self.write_prompt(row);
        self.hub.turn_finished(row);
    }

    /// A prompt row in session `row`, written behind the hub's back.
    fn write_prompt(&self, row: i64) {
        crate::turn::locked(&self.store)
            .unwrap()
            .write_rows(
                row,
                0,
                &[onicode_core::transcript::Row {
                    role: "user".into(),
                    text: "Look at the build".into(),
                    first: true,
                    sent_at: Some(1),
                    id: None,
                }],
            )
            .unwrap();
    }
}

/// A viewer: its stream, the state it folded, and the cursor it holds.
struct Viewer {
    stream: Option<Stream>,
    state: Option<WorkspaceState>,
    cursor: Option<Cursor>,
}

impl Viewer {
    fn join(hub: &Arc<WorkspaceHub>) -> Self {
        let mut viewer = Self {
            stream: None,
            state: None,
            cursor: None,
        };
        viewer.rejoin(hub);
        viewer
    }

    fn rejoin(&mut self, hub: &Arc<WorkspaceHub>) {
        self.stream = Some(hub.watch(WatchWorkspace { since: self.cursor }).unwrap());
    }

    /// Folds whatever is ready; a `Resync` resubscribes with the cursor held.
    fn pump(&mut self, hub: &Arc<WorkspaceHub>) {
        loop {
            let Some(stream) = self.stream.as_mut() else {
                return;
            };
            let Some(Some(item)) = stream.next().now_or_never() else {
                return;
            };
            match item {
                Item::Snapshot { at, state } => {
                    self.state = Some(state);
                    self.cursor = Some(at);
                }
                Item::Events {
                    first,
                    last,
                    events,
                } => {
                    let cursor = self.cursor.as_mut().expect("events after a snapshot");
                    assert_eq!(first, cursor.seq + 1, "no gap and no repeat");
                    let state = self.state.as_mut().expect("events after a snapshot");
                    for event in &events {
                        apply(state, event);
                    }
                    cursor.seq = last;
                }
                Item::Resync => self.rejoin(hub),
            }
        }
    }
}

fn json(state: &WorkspaceState) -> serde_json::Value {
    serde_json::to_value(state).unwrap()
}

#[derive(Clone, Debug)]
enum Step {
    Create(Option<u8>),
    Rename {
        at: usize,
        title: u8,
        stale: bool,
    },
    Archive {
        at: usize,
        archived: bool,
    },
    Pin {
        at: usize,
        pinned: bool,
    },
    Delete(usize),
    WorkItem {
        at: usize,
        number: Option<u8>,
        if_unlinked: bool,
    },
    Remind {
        at: usize,
        change: u8,
        due: u8,
    },
    Fold(Vec<usize>),
    Tick(u8),
    /// Another viewer prompts the session: a turn starts, writes its prompt and ends.
    Send(usize),
    /// The session's last viewer closes it.
    Close(usize),
    /// Repeat the last mutating call under its op.
    Repeat,
    Join,
    Drop(usize),
    Rejoin(usize),
    Pump(usize),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => proptest::option::of(any::<u8>()).prop_map(Step::Create),
        3 => (any::<usize>(), any::<u8>(), any::<bool>())
            .prop_map(|(at, title, stale)| Step::Rename { at, title, stale }),
        2 => (any::<usize>(), any::<bool>()).prop_map(|(at, archived)| Step::Archive { at, archived }),
        2 => (any::<usize>(), any::<bool>()).prop_map(|(at, pinned)| Step::Pin { at, pinned }),
        1 => any::<usize>().prop_map(Step::Delete),
        2 => (any::<usize>(), proptest::option::of(any::<u8>()), any::<bool>())
            .prop_map(|(at, number, if_unlinked)| Step::WorkItem { at, number, if_unlinked }),
        2 => (any::<usize>(), any::<u8>(), any::<u8>())
            .prop_map(|(at, change, due)| Step::Remind { at, change, due }),
        1 => proptest::collection::vec(any::<usize>(), 0..3).prop_map(Step::Fold),
        2 => any::<u8>().prop_map(Step::Tick),
        2 => any::<usize>().prop_map(Step::Send),
        2 => any::<usize>().prop_map(Step::Close),
        2 => Just(Step::Repeat),
        1 => Just(Step::Join),
        1 => any::<usize>().prop_map(Step::Drop),
        1 => any::<usize>().prop_map(Step::Rejoin),
        3 => any::<usize>().prop_map(Step::Pump),
    ]
}

fn item(number: u8) -> WorkItem {
    onicode_core::session_groups::parse_github_work_item(&format!(
        "https://github.com/acme/app/pull/{}",
        u64::from(number) + 1
    ))
    .unwrap()
}

/// The last mutating call, to repeat under its op.
enum Last {
    Rename(Rename),
    Archive(Archive),
    Pin(Pin),
    SetReminder(SetReminder),
}

fn run_script(limits: Limits, steps: &[Step]) {
    let harness = Harness::new(limits);
    let hub = harness.hub.clone();
    let mut model = WorkspaceModel::default();
    let mut rows: Vec<i64> = Vec::new();
    let mut viewers: Vec<Viewer> = vec![Viewer::join(&hub)];
    let mut last: Option<(OpId, Last, Outcome)> = None;
    let mut clock = 0u64;
    for step in steps {
        let pick = |at: usize| (!rows.is_empty()).then(|| rows[at % rows.len()]);
        match step.clone() {
            Step::Create(title) => {
                let title = title.map(|n| format!("Session {n}"));
                let created = harness.create(Some(OpId::new()), title);
                assert_eq!(created.outcome, Outcome::Applied);
                let summary = created.session.unwrap();
                model.create(summary.session.id, summary.session.title.clone());
                rows.push(summary.session.id);
            }
            Step::Rename { at, title, stale } => {
                let Some(row) = pick(at) else { continue };
                let title = format!("Title {title}");
                let current = model.shown.sessions.get(&row).map(|m| m.title.clone());
                let expected = if stale {
                    Some("A title it never had".to_owned())
                } else {
                    current
                };
                let body = Rename {
                    session: row,
                    title: title.clone(),
                    expected: expected.clone(),
                };
                let op = OpId::new();
                let outcome = block_on(hub.rename(Some(op), body.clone())).unwrap();
                assert_eq!(outcome, model.rename(row, &title, expected.as_deref()));
                last = Some((op, Last::Rename(body), outcome));
            }
            Step::Archive { at, archived } => {
                let Some(row) = pick(at) else { continue };
                let body = Archive {
                    session: row,
                    archived,
                };
                let op = OpId::new();
                let outcome = block_on(hub.archive(Some(op), body.clone())).unwrap();
                assert_eq!(outcome, model.archive(row, archived));
                last = Some((op, Last::Archive(body), outcome));
            }
            Step::Pin { at, pinned } => {
                let Some(row) = pick(at) else { continue };
                let body = Pin {
                    sessions: vec![row],
                    pinned,
                };
                let op = OpId::new();
                let outcome = block_on(hub.pin(Some(op), body.clone())).unwrap();
                assert_eq!(outcome, model.pin(&[row], pinned));
                last = Some((op, Last::Pin(body), outcome));
            }
            Step::Delete(at) => {
                let Some(row) = pick(at) else { continue };
                let outcome =
                    block_on(hub.delete(Some(OpId::new()), Delete { session: row })).unwrap();
                assert_eq!(outcome, model.delete(row));
            }
            Step::WorkItem {
                at,
                number,
                if_unlinked,
            } => {
                let Some(row) = pick(at) else { continue };
                let linked = number.map(item);
                let body = SetWorkItem {
                    session: row,
                    item: linked.clone(),
                    if_unlinked,
                };
                let outcome = block_on(hub.set_work_item(Some(OpId::new()), body)).unwrap();
                assert_eq!(outcome, model.set_work_item(row, linked, if_unlinked));
            }
            Step::Remind { at, change, due } => {
                // Reminders go only to sessions that still exist.
                let Some(row) = pick(at).filter(|row| model.shown.sessions.contains_key(row))
                else {
                    continue;
                };
                let due = u64::from(due) * 1_000;
                let change = match change % 3 {
                    0 => ReminderChange::At(due),
                    1 => ReminderChange::Snooze {
                        expected: model.shown.reminders.get(&row).map_or(due, |r| r.0),
                        due_at: due + 500,
                    },
                    _ => ReminderChange::Clear {
                        expected: (change % 2 == 0).then_some(due),
                    },
                };
                let body = SetReminder {
                    sessions: vec![row],
                    change: change.clone(),
                };
                let op = OpId::new();
                let outcome = block_on(hub.set_reminder(Some(op), body.clone())).unwrap();
                assert_eq!(outcome, model.set_reminder(&[row], &change));
                last = Some((op, Last::SetReminder(body), outcome));
            }
            Step::Fold(picked) => {
                let sessions: Vec<i64> = picked.into_iter().filter_map(pick).collect();
                let groups = Groups {
                    folders: if sessions.is_empty() {
                        Vec::new()
                    } else {
                        vec![Folder {
                            id: "folder".into(),
                            name: "Folder".into(),
                            session_ids: sessions,
                            ..Folder::default()
                        }]
                    },
                    ..Groups::default()
                };
                let body = SaveGroups {
                    project: PROJECT.into(),
                    groups: groups.clone(),
                };
                let outcome = block_on(hub.save_groups(Some(OpId::new()), body)).unwrap();
                assert_eq!(outcome, Outcome::Applied);
                model.save_groups(PROJECT, groups);
            }
            Step::Tick(by) => {
                clock += u64::from(by) * 100;
                hub.tick(clock).unwrap();
                model.tick(clock);
            }
            Step::Send(at) => {
                let Some(row) = pick(at).filter(|row| model.shown.sessions.contains_key(row))
                else {
                    continue;
                };
                harness.prompt(row);
                model.send(row);
            }
            Step::Close(at) => {
                let Some(row) = pick(at) else { continue };
                hub.session_closed(row);
                model.close(row);
            }
            Step::Repeat => {
                let Some((op, call, outcome)) = last.take() else {
                    continue;
                };
                let before = Shown::of(&harness.state());
                let again = match &call {
                    Last::Rename(body) => block_on(hub.rename(Some(op), body.clone())),
                    Last::Archive(body) => block_on(hub.archive(Some(op), body.clone())),
                    Last::Pin(body) => block_on(hub.pin(Some(op), body.clone())),
                    Last::SetReminder(body) => block_on(hub.set_reminder(Some(op), body.clone())),
                }
                .unwrap();
                assert_eq!(again, outcome, "a repeated op gets its first receipt");
                assert_eq!(
                    Shown::of(&harness.state()),
                    before,
                    "and changes nothing again"
                );
                last = Some((op, call, outcome));
            }
            Step::Join => viewers.push(Viewer::join(&hub)),
            Step::Drop(at) => {
                if !viewers.is_empty() {
                    let at = at % viewers.len();
                    viewers[at].stream = None;
                }
            }
            Step::Rejoin(at) => {
                if !viewers.is_empty() {
                    let at = at % viewers.len();
                    if viewers[at].stream.is_none() {
                        viewers[at].rejoin(&hub);
                    }
                }
            }
            Step::Pump(at) => {
                if !viewers.is_empty() {
                    let at = at % viewers.len();
                    viewers[at].pump(&hub);
                }
            }
        }
        assert_eq!(Shown::of(&harness.state()), model.shown, "after {step:?}");
    }
    // Every viewer, back with the cursor it holds, converges on the hub.
    let full = json(&harness.state());
    for viewer in &mut viewers {
        if viewer.stream.is_none() {
            viewer.rejoin(&hub);
        }
        viewer.pump(&hub);
        assert_eq!(viewer.state.as_ref().map(json), Some(full.clone()));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn the_hub_shows_the_model_and_every_viewer_converges(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        run_script(Limits::default(), &steps);
    }

    #[test]
    fn small_rings_and_queues_drop_viewers_to_snapshots_without_a_gap(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        run_script(Limits { ring: (6, 1 << 20), queue: (2, 1 << 20) }, &steps);
    }
}

#[test]
fn a_snapshot_and_the_events_after_it_are_the_later_snapshot() {
    let harness = Harness::new(Limits::default());
    let hub = harness.hub.clone();
    let row = harness.create(None, None).session.unwrap().session.id;
    let mut viewer = Viewer::join(&hub);
    viewer.pump(&hub);
    let (at, _) = hub.snapshot().unwrap();
    block_on(hub.rename(
        None,
        Rename {
            session: row,
            title: "Renamed".into(),
            expected: None,
        },
    ))
    .unwrap();
    block_on(hub.pin(
        None,
        Pin {
            sessions: vec![row],
            pinned: true,
        },
    ))
    .unwrap();
    viewer.pump(&hub);
    let (later, state) = hub.snapshot().unwrap();
    assert_eq!(later.epoch, at.epoch);
    assert_eq!(later.seq, at.seq + 2);
    assert_eq!(viewer.cursor, Some(later));
    assert_eq!(viewer.state.as_ref().map(json), Some(json(&state)));
}

#[test]
fn two_renames_race_to_the_one_that_reached_the_hub_last() {
    let mut harness = Harness::new(Limits::default());
    let hub = harness.hub.clone();
    let row = harness.create(None, None).session.unwrap().session.id;
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    for title in ["From the Mac", "From the phone"] {
        let hub = hub.clone();
        let outcomes = outcomes.clone();
        harness
            .pool
            .spawner()
            .spawn_local(async move {
                let body = Rename {
                    session: row,
                    title: title.into(),
                    expected: None,
                };
                let outcome = hub.rename(Some(OpId::new()), body).await.unwrap();
                outcomes.lock().unwrap().push((title, outcome));
            })
            .unwrap();
    }
    harness.settle();
    let outcomes = outcomes.lock().unwrap().clone();
    assert_eq!(
        outcomes,
        vec![
            ("From the Mac", Outcome::Applied),
            ("From the phone", Outcome::Applied)
        ]
    );
    let state = harness.state();
    assert_eq!(state.sessions[0].session.title, "From the phone");
    let stored = harness.store.lock().unwrap().sessions().unwrap();
    assert_eq!(stored[0].title, "From the phone", "the store agrees");
}

#[test]
fn a_repeated_create_gets_the_first_session_and_makes_no_second() {
    let harness = Harness::new(Limits::default());
    let op = OpId::new();
    let first = harness.create(Some(op), Some("Once".into()));
    let again = harness.create(Some(op), Some("Once".into()));
    assert_eq!(
        first.session.as_ref().map(|s| s.session.id),
        again.session.as_ref().map(|s| s.session.id)
    );
    assert_eq!(harness.state().sessions.len(), 1);
    assert_eq!(harness.store.lock().unwrap().sessions().unwrap().len(), 1);
}

#[test]
fn a_created_session_keeps_the_viewers_id() {
    let harness = Harness::new(Limits::default());
    let id = SessionId::new();
    let created = block_on(harness.hub.create_session(
        None,
        CreateSession {
            session: id,
            provider: "codex".into(),
            cwd: PROJECT.into(),
            title: None,
            start: NewSession::Bare,
            started_by: None,
        },
    ))
    .unwrap();
    let summary = created.session.unwrap();
    assert_eq!(summary.id, id);
    let store = harness.store.lock().unwrap();
    assert_eq!(
        store.session_by_uid(id.0).unwrap(),
        Some(summary.session.id)
    );
}

#[test]
fn a_rename_the_person_made_wins_over_a_generated_title() {
    let mut harness = Harness::new(Limits::default());
    let hub = harness.hub.clone();
    let created = harness.create(None, None).session.unwrap();
    let row = created.session.id;
    let placeholder = created.session.title.clone();
    let model = TextModel::automatic(
        onicode_core::provider::Kind::Codex,
        onicode_core::accounts::Profile::default(),
    );
    let generate = |message: &str| GenerateTitle {
        session: row,
        message: message.into(),
        placeholder: placeholder.clone(),
        seed: message.into(),
        cwd: PROJECT.into(),
        model: model.clone(),
    };
    // The seed stands in at once, the generated title follows, and a pasted pull request links
    // the session.
    let message = "review https://github.com/acme/app/pull/1142";
    block_on(hub.generate_title(None, generate(message))).unwrap();
    assert_eq!(harness.state().sessions[0].session.title, message);
    harness.settle();
    let summary = harness.state().sessions[0].clone();
    assert_eq!(summary.session.title, format!("About {message}"));
    assert_eq!(
        summary.session.work_item.map(|item| item.number),
        Some(1142)
    );
    // A title generated after the person renamed the session never replaces their name.
    block_on(hub.rename(
        None,
        Rename {
            session: row,
            title: placeholder.clone(),
            expected: None,
        },
    ))
    .unwrap();
    block_on(hub.generate_title(None, generate("second"))).unwrap();
    block_on(hub.rename(
        None,
        Rename {
            session: row,
            title: "Mine".into(),
            expected: None,
        },
    ))
    .unwrap();
    harness.settle();
    assert_eq!(harness.state().sessions[0].session.title, "Mine");
    // A session that has a link keeps it.
    let outcome = block_on(hub.set_work_item(
        None,
        SetWorkItem {
            session: row,
            item: Some(item(7)),
            if_unlinked: true,
        },
    ))
    .unwrap();
    assert_eq!(outcome, Outcome::Rejected(Rejection::AlreadyResolved));
}

#[test]
fn reminders_fire_once_when_virtual_time_reaches_them() {
    let harness = Harness::new(Limits::default());
    let hub = harness.hub.clone();
    let row = harness.create(None, None).session.unwrap().session.id;
    let mut viewer = Viewer::join(&hub);
    viewer.pump(&hub);
    block_on(hub.set_reminder(
        None,
        SetReminder {
            sessions: vec![row],
            change: ReminderChange::At(10_000),
        },
    ))
    .unwrap();
    viewer.pump(&hub);
    let fired = |viewer: &Viewer| {
        viewer
            .state
            .as_ref()
            .unwrap()
            .reminders
            .iter()
            .filter(|reminder| reminder.fired_at.is_some())
            .count()
    };
    hub.tick(9_999).unwrap();
    viewer.pump(&hub);
    assert_eq!(fired(&viewer), 0);
    let before = hub.snapshot().unwrap().0;
    hub.tick(10_000).unwrap();
    viewer.pump(&hub);
    assert_eq!(fired(&viewer), 1);
    assert_eq!(
        viewer.state.as_ref().unwrap().reminders[0].fired_at,
        Some(10_000)
    );
    // Later beats deliver nothing more.
    let after = hub.snapshot().unwrap().0;
    assert_eq!(after.seq, before.seq + 1);
    hub.tick(50_000).unwrap();
    assert_eq!(hub.snapshot().unwrap().0, after);
    // Snoozing re-arms only the occurrence that fired.
    let stale = block_on(hub.set_reminder(
        None,
        SetReminder {
            sessions: vec![row],
            change: ReminderChange::Snooze {
                expected: 9_000,
                due_at: 60_000,
            },
        },
    ))
    .unwrap();
    assert_eq!(stale, Outcome::Rejected(Rejection::StaleRev));
    block_on(hub.set_reminder(
        None,
        SetReminder {
            sessions: vec![row],
            change: ReminderChange::Snooze {
                expected: 10_000,
                due_at: 60_000,
            },
        },
    ))
    .unwrap();
    hub.tick(59_999).unwrap();
    viewer.pump(&hub);
    assert_eq!(fired(&viewer), 0);
    hub.tick(60_000).unwrap();
    viewer.pump(&hub);
    assert_eq!(fired(&viewer), 1);
}

fn hourly(cwd: &str, next_run_at: i64) -> AutomationUpsert {
    AutomationUpsert {
        id: onicode_core::automations::new_id(),
        name: "Nightly review".into(),
        prompt: "Review what changed".into(),
        harness: "codex".into(),
        model: String::new(),
        model_settings: std::collections::HashMap::new(),
        cwd: cwd.into(),
        workspace_mode: "current".into(),
        worktree_cwd: String::new(),
        session_folder_id: String::new(),
        reuse_session: false,
        runtime_mode: "supervised".into(),
        trigger_kind: "time".into(),
        trigger_event: String::new(),
        schedule_kind: "hourly".into(),
        minute: 0,
        time: "09:00".into(),
        day_of_week: 1,
        triggers: None,
        missed_run_grace_minutes: 60,
        enabled: true,
        next_run_at,
    }
}

#[test]
fn automations_come_due_in_virtual_time_once_and_return_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.db");
    let store: Database = Arc::new(Mutex::new(Store::open(&path).unwrap()));
    // Upserting checks the wall clock: the first run is an hour from now, and the hub's virtual
    // time takes it from there.
    let due_at = i64::try_from(onicode_core::time::now_ms()).unwrap() + 3_600_000;
    let project = dir.path().to_string_lossy().into_owned();
    Service::open(&path)
        .unwrap()
        .upsert(hourly(&project, due_at))
        .unwrap();
    let start = u64::try_from(due_at).unwrap() - 10;
    let harness = Harness::over(store.clone(), Limits::default(), start);
    let hub = harness.hub.clone();
    let mut viewer = Viewer::join(&hub);
    hub.tick(start).unwrap();
    viewer.pump(&hub);
    assert!(viewer.state.as_ref().unwrap().automations.is_empty());
    let at = u64::try_from(due_at).unwrap();
    hub.tick(at).unwrap();
    hub.tick(at).unwrap();
    viewer.pump(&hub);
    let due = viewer.state.as_ref().unwrap().automations.clone();
    assert_eq!(due.len(), 1, "claimed once");
    assert_eq!(due[0].run.status, "pending");
    // An engine that starts later hands the unstarted run out again.
    drop(viewer);
    drop(harness);
    let restarted = Harness::over(store, Limits::default(), at + 1);
    restarted.hub.tick(at + 1).unwrap();
    let again = restarted.state().automations;
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].run.id, due[0].run.id);
    // Starting it makes its session in the project and takes it off the list.
    let prepared = block_on(restarted.hub.prepare_automation_run(
        Some(OpId::new()),
        onicode_protocol::workspace::PrepareAutomationRun {
            due: Box::new(again[0].clone()),
            reuse: None,
            settings: Box::default(),
        },
    ))
    .unwrap();
    assert_eq!(prepared.outcome, Outcome::Applied);
    let session = prepared.session.unwrap();
    assert_eq!(session.session.title, "Nightly review");
    assert_eq!(
        session.automation.as_deref(),
        Some(due[0].automation.id.as_str())
    );
    let state = restarted.state();
    assert!(state.automations.is_empty());
    assert!(
        state
            .sessions
            .iter()
            .any(|s| s.session.id == session.session.id)
    );
}

#[test]
fn remembered_controls_survive_the_mapped_native_model_default() {
    let remembered = onicode_core::agent::Settings {
        model: Some("model-a".into()),
        effort: Some("high".into()),
        model_settings: [("reasoningEffort".into(), "high".into())].into(),
        ..Default::default()
    };
    let selected = super::new_session_settings(Some(remembered.clone()), Some("model-a".into()));
    assert_eq!(selected.effort.as_deref(), Some("high"));
    assert_eq!(
        selected
            .model_settings
            .get("reasoningEffort")
            .map(String::as_str),
        Some("high")
    );
    let changed = super::new_session_settings(Some(remembered), Some("model-b".into()));
    assert_eq!(changed.model.as_deref(), Some("model-b"));
    assert!(changed.effort.is_none());
    assert!(changed.model_settings.is_empty());
}

#[test]
fn a_session_nobody_prompted_goes_when_its_last_viewer_closes_it() {
    let harness = Harness::new(Limits::default());
    let mut viewer = Viewer::join(&harness.hub);
    let opened = harness.create(Some(OpId::new()), None).session.unwrap();
    harness.hub.session_closed(opened.session.id);
    assert!(
        harness.state().sessions.is_empty(),
        "open and close leaves nothing"
    );
    viewer.pump(&harness.hub);
    assert!(
        viewer.state.as_ref().unwrap().sessions.is_empty(),
        "every viewer sees it go"
    );
    // Another viewer of the engine prompts it before the close: it stays.
    let used = harness.create(Some(OpId::new()), None).session.unwrap();
    harness.prompt(used.session.id);
    harness.hub.session_closed(used.session.id);
    // Rows that reached the store some other way keep it too: the engine looks before it goes.
    let written = harness.create(Some(OpId::new()), None).session.unwrap();
    harness.write_prompt(written.session.id);
    harness.hub.session_closed(written.session.id);
    let rows: Vec<i64> = harness
        .state()
        .sessions
        .iter()
        .map(|summary| summary.session.id)
        .collect();
    assert!(rows.contains(&used.session.id));
    assert!(rows.contains(&written.session.id));
    viewer.pump(&harness.hub);
    assert_eq!(json(viewer.state.as_ref().unwrap()), json(&harness.state()));
}
