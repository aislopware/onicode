//! Refused resumes (`resume.rs`): the conversation is told why in one error row, the engine
//! waits longer after each refusal, and stops once the limit window has passed. The scheduler's
//! clock is the test's: every beat names its time, and a scripted runner stands in for the
//! provider, so each turn ends exactly as the test says.
use std::sync::{Arc, Mutex};

use futures::executor::LocalPool;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::task::LocalSpawnExt;
use futures::{FutureExt, StreamExt};
use onicode_core::agent::Settings;
use onicode_core::store::Store;
use onicode_core::transcript::Row;
use onicode_core::usage::Limit;
use onicode_protocol::session::{SessionEvent, SessionState, WatchSession};
use onicode_protocol::{Item, SessionId};
use proptest::prelude::*;

use super::{CONTINUE, LIMIT_WINDOW, REFUSED, RETRY_FIRST, RETRY_MAX, refused_text, retry_wait};
use crate::session::{Runner, SessionHub, Spawn, TurnJob, Viewer};
use crate::turn::{Database, Update, locked};

type Work = Arc<Mutex<Vec<BoxFuture<'static, ()>>>>;
type Jobs = Arc<Mutex<Vec<TurnJob>>>;

/// When the limit the session ended at resets (Unix ms).
const RESET: i64 = 1_800_000_000_000;
/// The first beat a resume is due at.
const DUE: i64 = RESET + 30_000;
const REASON: &str = "Claude Code is not installed.";

/// The hub's work, held until the test runs it.
struct Queued(Work);

impl Spawn for Queued {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.lock().unwrap().push(work);
    }
}

fn row(role: &str, text: &str) -> Row {
    Row {
        role: role.into(),
        text: text.into(),
        first: true,
        sent_at: None,
        id: None,
    }
}

/// Arms a limit of session `id` resetting at `reset`, as a viewer's banner does once a turn
/// ended at it.
fn arm(store: &Database, id: i64, reset: i64) {
    locked(store)
        .unwrap()
        .save_agent_settings(
            id,
            &Settings {
                usage_limit: Some(Limit {
                    resets_at: Some(reset),
                    resume_at_reset: true,
                }),
                ..Settings::default()
            },
        )
        .unwrap();
}

/// An engine over one session whose last turn ended at a limit resetting at `RESET`, armed to
/// resume, with a viewer watching it.
struct World {
    pool: LocalPool,
    work: Work,
    jobs: Jobs,
    hub: Arc<SessionHub>,
    store: Database,
    id: i64,
    session: SessionId,
    stream: BoxStream<'static, Item<SessionState, SessionEvent>>,
    viewer: Viewer,
}

impl World {
    fn armed() -> Self {
        let mut store = Store::in_memory().unwrap();
        let id = store.create("codex", "/project").unwrap().id;
        store
            .write_rows(
                id,
                0,
                &[
                    row("user", "Refactor the parser"),
                    row("assistant", "Working on it"),
                    row("error", "You've hit your usage limit"),
                ],
            )
            .unwrap();
        let store: Database = Arc::new(Mutex::new(store));
        arm(&store, id, RESET);
        let work = Work::default();
        let jobs = Jobs::default();
        let runner: Runner = {
            let jobs = jobs.clone();
            Arc::new(move |job| jobs.lock().unwrap().push(job))
        };
        let hub = SessionHub::with_runner(store.clone(), Arc::new(Queued(work.clone())), runner);
        let session = hub.session_id(id).unwrap();
        let stream = hub
            .watch(WatchSession {
                session,
                since: None,
                window_turns: usize::MAX,
            })
            .unwrap();
        let mut world = Self {
            pool: LocalPool::new(),
            work,
            jobs,
            hub,
            store,
            id,
            session,
            stream,
            viewer: Viewer::default(),
        };
        world.pump();
        world
    }

    /// Runs the hub's work until nothing more can happen.
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

    /// The viewer takes in everything the hub published.
    fn pump(&mut self) {
        while let Some(Some(item)) = self.stream.next().now_or_never() {
            assert!(!matches!(item, Item::Resync), "the viewer keeps up");
            self.viewer.absorb(item);
        }
    }

    /// One scheduler beat at `now`: whether it sent the continuation.
    fn beat(&mut self, now: i64) -> bool {
        assert_eq!(self.hub.resume_limits(u64::try_from(now).unwrap()), Ok(()));
        self.settle();
        self.pump();
        let jobs = self.jobs.lock().unwrap();
        match jobs.last() {
            Some(job) => {
                assert_eq!(job.prompt, CONTINUE, "only the continuation is sent");
                true
            }
            None => false,
        }
    }

    /// The running resume ends before its prompt was saved, refused for `reason`.
    fn refuse(&mut self, reason: &str) {
        let job = self.jobs.lock().unwrap().pop().expect("a resume runs");
        // A turn's updates name where its rows start: here the transcript's end, as nothing
        // was saved.
        let start = self.stored_rows().len();
        job.updates
            .try_send(Update {
                start,
                status: reason.into(),
                finished: true,
                ..Update::default()
            })
            .unwrap();
        drop(job);
        self.settle();
        self.pump();
    }

    /// The running resume gets in: its prompt is saved, the provider answers and the turn ends.
    fn answer(&mut self) {
        let job = self.jobs.lock().unwrap().pop().expect("a resume runs");
        let at = self.stored_rows().len();
        let rows = vec![row("user", CONTINUE), row("assistant", "Picking up")];
        locked(&self.store)
            .unwrap()
            .write_rows(self.id, at, &rows)
            .unwrap();
        job.updates
            .try_send(Update {
                start: at,
                rows,
                turn_ready: true,
                status: "Working".into(),
                ..Update::default()
            })
            .unwrap();
        job.updates
            .try_send(Update {
                start: at + 2,
                status: "Ready".into(),
                finished: true,
                ..Update::default()
            })
            .unwrap();
        drop(job);
        self.settle();
        self.pump();
    }

    fn stored_rows(&self) -> Vec<Row> {
        locked(&self.store).unwrap().load(self.id).unwrap().rows
    }

    fn stored_limit(&self) -> Option<Limit> {
        locked(&self.store)
            .unwrap()
            .agent_settings(self.id)
            .unwrap()
            .and_then(|settings| settings.usage_limit)
    }

    /// The rows saying a resume was refused.
    fn refusals(&self) -> Vec<(usize, String)> {
        self.stored_rows()
            .into_iter()
            .enumerate()
            .filter(|(_, row)| row.role == "error" && row.text.starts_with(REFUSED))
            .map(|(at, row)| (at, row.text))
            .collect()
    }

    /// The viewer shows what the store holds, and the limit the engine holds.
    fn assert_viewer_follows(&self) {
        let shown: Vec<(String, String)> = self
            .viewer
            .state()
            .rows
            .iter()
            .map(|row| (row.role.clone(), row.text.clone()))
            .collect();
        let stored: Vec<(String, String)> = self
            .stored_rows()
            .into_iter()
            .map(|row| (row.role, row.text))
            .collect();
        assert_eq!(shown, stored, "the viewer shows the stored rows");
        assert_eq!(
            self.viewer.state().usage_limit,
            self.stored_limit(),
            "the viewer shows the limit the engine holds"
        );
    }
}

#[test]
fn the_waits_double_from_one_beat_to_fifteen_minutes() {
    let waits: Vec<i64> = (1..=8).map(retry_wait).collect();
    assert_eq!(
        waits,
        [
            RETRY_FIRST,
            60_000,
            120_000,
            240_000,
            480_000,
            RETRY_MAX,
            RETRY_MAX,
            RETRY_MAX
        ]
    );
    assert_eq!(retry_wait(u32::MAX), RETRY_MAX);
}

#[test]
fn the_row_says_why_in_one_short_line() {
    assert_eq!(
        refused_text(REASON, Some(RETRY_FIRST)),
        "Could not resume after the usage limit: Claude Code is not installed. Trying again in 30 s."
    );
    assert_eq!(
        refused_text("Ready", Some(120_000)),
        "Could not resume after the usage limit. Trying again in 2 min."
    );
    assert_eq!(
        refused_text("", None),
        "Could not resume after the usage limit. Automatic resume stopped; choose Resume to try again."
    );
    let long = format!("{}\nstack trace", "é".repeat(5_000));
    let text = refused_text(&long, Some(RETRY_MAX));
    assert!(!text.contains('\n') && !text.contains("stack trace"));
    assert!(text.len() < onicode_core::transcript::ROW_BYTES);
}

#[test]
fn a_refused_resume_says_why_and_waits_longer_each_time() {
    let mut world = World::armed();
    let mut now = DUE;
    let mut row_at = None;
    for count in 1..=8u32 {
        assert!(
            world.beat(now),
            "refusal {count}: sent once its wait is over"
        );
        world.refuse(REASON);
        let wait = retry_wait(count);
        let refusals = world.refusals();
        assert_eq!(
            refusals.len(),
            1,
            "every refusal rewrites the one row: {refusals:?}"
        );
        let (at, text) = &refusals[0];
        assert_eq!(*text, refused_text(REASON, Some(wait)));
        assert_eq!(
            *at,
            world.stored_rows().len() - 1,
            "the row ends the transcript"
        );
        assert_eq!(*row_at.get_or_insert(*at), *at, "the row stays in place");
        assert_eq!(
            world.stored_limit(),
            Some(Limit {
                resets_at: Some(RESET),
                resume_at_reset: true,
            }),
            "the limit is armed again"
        );
        world.assert_viewer_follows();
        // The beats inside the wait send nothing.
        let mut beat = now + RETRY_FIRST;
        while beat < now + wait {
            assert!(
                !world.beat(beat),
                "refusal {count}: nothing sent {beat} ms in"
            );
            beat += RETRY_FIRST;
        }
        now += wait;
    }
}

#[test]
fn a_refused_resume_stops_once_the_limit_window_has_passed() {
    let mut world = World::armed();
    let first = RESET + LIMIT_WINDOW - 60_000;
    assert!(world.beat(first));
    world.refuse(REASON);
    // Its 30 s wait still ends inside the window.
    assert!(world.beat(first + RETRY_FIRST));
    world.refuse(REASON);
    // The next 60 s would end past it: the engine stops.
    let refusals = world.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].1, refused_text(REASON, None));
    assert_eq!(
        world.stored_limit(),
        Some(Limit {
            resets_at: Some(RESET),
            resume_at_reset: false,
        }),
        "the limit stays for the banner, disarmed"
    );
    world.assert_viewer_follows();
    for beat in 1..=20 {
        assert!(
            !world.beat(first + RETRY_FIRST + beat * RETRY_MAX),
            "nothing is sent after the engine stopped"
        );
    }
}

#[test]
fn a_late_first_resume_is_still_sent_once() {
    // The Mac slept through the whole window: the owed continuation is still sent, and a
    // refusal then stops at once.
    let mut world = World::armed();
    let late = RESET + 2 * LIMIT_WINDOW;
    assert!(world.beat(late));
    world.refuse(REASON);
    assert_eq!(world.refusals()[0].1, refused_text(REASON, None));
    assert!(!world.beat(late + RETRY_MAX));
}

#[test]
fn a_resume_that_gets_in_ends_the_refusals_and_a_later_limit_starts_afresh() {
    let mut world = World::armed();
    assert!(world.beat(DUE));
    world.refuse(REASON);
    assert!(world.beat(DUE + RETRY_FIRST));
    world.refuse(REASON);
    assert!(world.beat(DUE + RETRY_FIRST + 60_000));
    world.answer();
    assert_eq!(
        world.stored_limit(),
        None,
        "the resume continued past the limit"
    );
    assert!(!world.beat(DUE + RETRY_MAX), "and is done");
    world.assert_viewer_follows();

    // Another limit, refused: its own row after the continuation, and its first wait.
    let later = RESET + LIMIT_WINDOW;
    arm(&world.store, world.id, later);
    assert!(world.beat(later + RETRY_FIRST));
    world.refuse(REASON);
    let refusals = world.refusals();
    assert_eq!(
        refusals.len(),
        2,
        "the old row stays above the continuation"
    );
    assert_eq!(refusals[1].1, refused_text(REASON, Some(RETRY_FIRST)));
    assert!(world.beat(later + 2 * RETRY_FIRST));
    world.assert_viewer_follows();
}

#[test]
fn a_restart_tries_again_at_once() {
    let mut world = World::armed();
    assert!(world.beat(DUE));
    world.refuse(REASON);
    assert!(world.beat(DUE + RETRY_FIRST));
    world.refuse(REASON);
    // Only this run kept the wait: a new engine on the same store sends on its first beat.
    let (store, id) = (world.store.clone(), world.id);
    let jobs = Jobs::default();
    let work = Work::default();
    let runner: Runner = {
        let jobs = jobs.clone();
        Arc::new(move |job| jobs.lock().unwrap().push(job))
    };
    let hub = SessionHub::with_runner(store, Arc::new(Queued(work.clone())), runner);
    world.session = hub.session_id(id).unwrap();
    world.stream = hub
        .watch(WatchSession {
            session: world.session,
            since: None,
            window_turns: usize::MAX,
        })
        .unwrap();
    world.viewer = Viewer::default();
    world.hub = hub;
    world.jobs = jobs;
    world.work = work;
    world.pump();
    assert!(world.beat(DUE + RETRY_FIRST + 1));
    world.refuse(REASON);
    let refusals = world.refusals();
    assert_eq!(refusals.len(), 1, "the row is rewritten");
    assert_eq!(
        refusals[0].1,
        refused_text(REASON, Some(RETRY_FIRST)),
        "the new run counts afresh"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// With beats every 30 s from a first due beat anywhere in the window, a resume refused `n`
    /// times is sent exactly at the beats the waits name, until it gets in or the window ends;
    /// the transcript always holds one refusal row, and nothing is sent once the engine stopped.
    #[test]
    fn refused_resumes_follow_the_schedule(
        start in 0..LIMIT_WINDOW / RETRY_FIRST,
        refusals in 1..40u32,
    ) {
        let mut world = World::armed();
        let first = DUE + start * RETRY_FIRST;
        // The model: when each send is due, and whether the engine stops.
        let mut due = first;
        let mut sends = vec![first];
        let mut stopped = false;
        for count in 1..=refusals {
            let next = due + retry_wait(count);
            if next > RESET + LIMIT_WINDOW {
                stopped = true;
                break;
            }
            due = next;
            sends.push(due);
        }
        let mut sent = Vec::new();
        let mut refused = 0;
        let end = due + RETRY_MAX * 2;
        let mut beat = first;
        while beat <= end {
            if world.beat(beat) {
                sent.push(beat);
                if refused < refusals {
                    refused += 1;
                    world.refuse(REASON);
                    prop_assert_eq!(world.refusals().len(), 1);
                } else {
                    world.answer();
                }
            }
            beat += RETRY_FIRST;
        }
        prop_assert_eq!(&sent, &sends);
        if stopped {
            prop_assert_eq!(world.refusals()[0].1.clone(), refused_text(REASON, None));
            prop_assert!(world.stored_limit().is_some_and(|limit| !limit.resume_at_reset));
        } else {
            prop_assert_eq!(world.stored_limit(), None);
        }
        world.assert_viewer_follows();
    }
}
