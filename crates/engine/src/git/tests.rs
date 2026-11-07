//! The checkout hub against its reference (engine.md "Tests"): a scripted checkout changes, the
//! file watch reports arrive in virtual time, calls reread it, and viewers join, fall behind,
//! drop and come back at cursors they held. Every watcher must show what the model says was
//! last read, and a status at one cursor plus the events after it must be the later one. Then
//! the two lanes, scheduled by hand, and a repeated commit against a real repository.
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use futures::executor::{LocalPool, block_on};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::task::LocalSpawnExt;
use futures::{FutureExt, StreamExt};
use onicode_core::scm::{ChangedFile, DiffIndex};
use onicode_core::store::Store;
use onicode_protocol::git::{Commit, GitEvent, GitStatus, Stage, StageTarget, WatchGit};
use onicode_protocol::{Cursor, Item, OpId};
use proptest::prelude::*;

use super::hub::{CheckoutHub, GitFeed};
use super::lanes::{Lane, Lanes};
use crate::feed::{Feed, Limits};
use crate::fs_watch::{Change, SETTLE_MS};
use crate::probe::{DiskProbe, Scripted, Status};
use crate::session::{Spawn, ThreadSpawn};
use crate::turn::Database;

const CHECKOUT: &str = "/w";

type Stream = BoxStream<'static, Item<GitStatus, GitEvent>>;

fn store() -> Database {
    Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()))
}

/// A status with `count` changed files, on a branch named after `seed`.
fn status(seed: u8, count: u8) -> Status {
    let files = (0..count)
        .map(|n| ChangedFile {
            path: format!("{CHECKOUT}/src/f{n}.rs"),
            relative: format!("src/f{n}.rs"),
            status: "M".into(),
            additions: i64::from(seed),
            deletions: i64::from(n),
            staged: n % 2 == 0,
            unstaged: n % 2 == 1,
        })
        .collect();
    (
        DiffIndex {
            branch: Some(format!("b{seed}")),
            files,
            ..DiffIndex::default()
        },
        BTreeMap::new(),
    )
}

struct Harness {
    disk: Arc<Scripted>,
    hub: Arc<CheckoutHub>,
    /// Virtual now, from the wall clock at the start: the repair read lies 30 s past it,
    /// further than any run goes.
    now: u64,
}

impl Harness {
    fn new(limits: Limits) -> Self {
        let disk = Arc::new(Scripted::default());
        disk.statuses
            .lock()
            .unwrap()
            .insert(CHECKOUT.into(), Ok(status(0, 1)));
        let hub = CheckoutHub::build(
            store(),
            Arc::new(ThreadSpawn),
            disk.clone(),
            Lanes::default(),
            limits,
            false,
            0,
        );
        Self {
            disk,
            hub,
            now: onicode_core::time::now_ms(),
        }
    }

    fn watch(&self, since: Option<Cursor>) -> Stream {
        block_on(self.hub.watch_git(WatchGit {
            checkout: CHECKOUT.into(),
            since,
        }))
        .unwrap()
    }

    /// What a viewer joining now is shown first.
    fn shown(&self) -> (Cursor, GitStatus) {
        let mut stream = self.watch(None);
        match stream.next().now_or_never() {
            Some(Some(Item::Snapshot { at, state })) => (at, state),
            other => panic!("a new watcher starts from a snapshot, not {other:?}"),
        }
    }

    fn set(&self, read: Result<Status, String>) {
        self.disk
            .statuses
            .lock()
            .unwrap()
            .insert(CHECKOUT.into(), read);
    }
}

/// A viewer of the checkout: its stream, the status it folded, its cursor, and every (cursor,
/// status) it held, to come back at.
struct Viewer {
    stream: Option<Stream>,
    shown: Option<(Cursor, GitStatus)>,
    held: Vec<(Cursor, GitStatus)>,
}

impl Viewer {
    fn join(harness: &Harness) -> Self {
        Self {
            stream: Some(harness.watch(None)),
            shown: None,
            held: Vec::new(),
        }
    }

    /// Folds whatever is ready; a `Resync` resubscribes with the cursor held.
    fn pump(&mut self, harness: &Harness) {
        loop {
            let Some(stream) = self.stream.as_mut() else {
                return;
            };
            let Some(Some(item)) = stream.next().now_or_never() else {
                return;
            };
            match item {
                Item::Snapshot { at, state } => self.shown = Some((at, state)),
                Item::Events {
                    first,
                    last,
                    events,
                } => {
                    let (cursor, state) = self.shown.as_mut().expect("events after a snapshot");
                    assert_eq!(first, cursor.seq + 1, "no gap and no repeat");
                    for event in &events {
                        GitFeed::apply(state, event);
                    }
                    cursor.seq = last;
                }
                Item::Resync => {
                    let since = self.shown.as_ref().map(|(cursor, _)| *cursor);
                    self.stream = Some(harness.watch(since));
                }
            }
            if let Some(shown) = &self.shown {
                self.held.push(shown.clone());
            }
        }
    }

    /// Comes back at a cursor it held, with the status it held there.
    fn rejoin(&mut self, harness: &Harness, pick: usize) {
        let back = (!self.held.is_empty()).then(|| self.held[pick % self.held.len()].clone());
        self.stream = None;
        let since = back.as_ref().map(|(cursor, _)| *cursor);
        self.shown = back;
        self.stream = Some(harness.watch(since));
    }
}

#[derive(Clone, Debug)]
enum Step {
    /// The checkout's status changes and the watch reports a source file.
    Change {
        seed: u8,
        count: u8,
    },
    /// Build output churns: nothing to read.
    Churn,
    /// The next read fails.
    Fail,
    Overflow,
    Advance(u16),
    /// The pump's beat.
    Refresh,
    /// A viewer's `ReadStatus`.
    Read,
    Join,
    Pump {
        viewer: usize,
    },
    Drop {
        viewer: usize,
    },
    Rejoin {
        viewer: usize,
        pick: usize,
    },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (any::<u8>(), 0..24_u8).prop_map(|(seed, count)| Step::Change { seed, count }),
        1 => Just(Step::Churn),
        1 => Just(Step::Fail),
        1 => Just(Step::Overflow),
        3 => (0..400_u16).prop_map(Step::Advance),
        3 => Just(Step::Refresh),
        1 => Just(Step::Read),
        2 => Just(Step::Join),
        4 => any::<usize>().prop_map(|viewer| Step::Pump { viewer }),
        1 => any::<usize>().prop_map(|viewer| Step::Drop { viewer }),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(viewer, pick)| Step::Rejoin { viewer, pick }),
    ]
}

/// Runs `steps` against the hub and the model: the status watchers hold is the last good read,
/// and a read happens when a burst that can move the status settles, or when a viewer asks.
fn run(steps: &[Step], limits: Limits) {
    let mut harness = Harness::new(limits);
    let start = harness.now;
    // A watcher that never reads keeps the checkout watched across the run.
    let _anchor = harness.watch(None);
    let mut shown = Some(status(0, 1).0);
    let mut open: Option<u64> = None;
    let mut viewers: Vec<Viewer> = Vec::new();
    for step in steps {
        match step {
            Step::Change { seed, count } => {
                harness.set(Ok(status(*seed, *count)));
                harness.hub.note(
                    CHECKOUT,
                    Change::Paths(vec![format!("{CHECKOUT}/src/f{seed}.rs")]),
                    harness.now,
                );
                open.get_or_insert(harness.now);
            }
            Step::Churn => {
                harness.hub.note(
                    CHECKOUT,
                    Change::Paths(vec![
                        format!("{CHECKOUT}/target/debug/app"),
                        format!("{CHECKOUT}/.git/objects/ab/cd"),
                        format!("{CHECKOUT}/.git/index.lock"),
                    ]),
                    harness.now,
                );
            }
            Step::Fail => harness.set(Err("index file corrupt".into())),
            Step::Overflow => {
                harness.hub.note(CHECKOUT, Change::Overflow, harness.now);
                open.get_or_insert(harness.now);
            }
            Step::Advance(ms) => harness.now += u64::from(*ms),
            Step::Refresh => {
                let reads = harness.disk.reads.load(Ordering::Relaxed);
                harness.hub.refresh(CHECKOUT, harness.now);
                let settled = open.is_some_and(|first| first + SETTLE_MS <= harness.now);
                if settled {
                    open = None;
                    if let Ok((index, _)) = &harness.disk.statuses.lock().unwrap()[CHECKOUT] {
                        shown = Some(index.clone());
                    }
                }
                assert_eq!(
                    harness.disk.reads.load(Ordering::Relaxed),
                    reads + usize::from(settled),
                    "a read only when a burst that moves the status settled"
                );
            }
            Step::Read => {
                let read = block_on(harness.hub.read_status_now(CHECKOUT.into()));
                match &harness.disk.statuses.lock().unwrap()[CHECKOUT] {
                    Ok((index, _)) => {
                        assert_eq!(read.unwrap().index.as_ref(), Some(index));
                        shown = Some(index.clone());
                    }
                    Err(error) => assert_eq!(read.unwrap_err(), *error),
                }
            }
            Step::Join => {
                if viewers.len() < 8 {
                    viewers.push(Viewer::join(&harness));
                }
            }
            Step::Pump { viewer } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    let viewer = &mut viewers[viewer % count];
                    viewer.pump(&harness);
                    let (_, hub) = harness.shown();
                    let (_, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
                    assert_eq!(folded, &hub, "a viewer folds to the hub's status");
                }
            }
            Step::Drop { viewer } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    viewers.remove(viewer % count);
                }
            }
            Step::Rejoin { viewer, pick } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    viewers[viewer % count].rejoin(&harness, *pick);
                }
            }
        }
        assert_eq!(
            harness.shown().1.index,
            shown,
            "watchers hold the last good read after {step:?}"
        );
        assert!(harness.now < start + 25_000, "no run reaches a repair read");
    }
    for viewer in &mut viewers {
        viewer.pump(&harness);
        let (cursor, hub) = harness.shown();
        let (held, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
        assert_eq!(folded, &hub, "every viewer ends up showing the hub");
        assert_eq!(held, &cursor, "and holds the hub's cursor");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn git_watches_show_the_last_good_read(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        run(&steps, Limits::default());
    }

    #[test]
    fn git_viewers_that_fall_out_of_the_ring_or_overflow_come_back_whole(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        // A ring of four statuses, and a queue that a status of a dozen files overflows.
        run(&steps, Limits { ring: (4, 1 << 20), queue: (256, 900) });
    }
}

/// Work the hub hands its spawner, held until the test runs it, in order.
#[derive(Clone, Default)]
struct Queued(Arc<Mutex<VecDeque<BoxFuture<'static, ()>>>>);

impl Spawn for Queued {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.lock().unwrap().push_back(work);
    }
}

impl Queued {
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// Runs the queued work at `at` to its end.
    fn run(&self, at: usize) {
        let work = self.0.lock().unwrap().remove(at).expect("queued work");
        block_on(work);
    }
}

#[test]
fn a_background_flood_never_delays_a_foreground_call_beyond_one_slot() {
    let queued = Queued::default();
    // One foreground slot and two background ones, so one busy call fills the foreground.
    let hub = CheckoutHub::build(
        store(),
        Arc::new(queued.clone()),
        Arc::new(Scripted::default()),
        Lanes::new(1, 2),
        Limits::default(),
        false,
        0,
    );
    let ran: Arc<Mutex<Vec<String>>> = Arc::default();
    let replies: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut pool = LocalPool::new();
    let call = |lane: Lane, name: String| {
        let (hub, ran, replies) = (hub.clone(), ran.clone(), replies.clone());
        let label = name.clone();
        pool.spawner()
            .spawn_local(async move {
                let reply = hub
                    .run(lane, move |_| {
                        ran.lock().unwrap().push(label.clone());
                        Ok(label)
                    })
                    .await
                    .unwrap();
                replies.lock().unwrap().push(reply);
            })
            .unwrap();
    };

    call(Lane::Foreground, "busy".into());
    for n in 0..20 {
        call(Lane::Background, format!("background {n}"));
    }
    call(Lane::Foreground, "person".into());
    pool.run_until_stalled();
    assert_eq!(
        queued.len(),
        3,
        "the busy call and two background calls hold every slot; the rest wait"
    );

    // The busy call ends: its slot goes to the person's call, not to the background flood.
    queued.run(0);
    pool.run_until_stalled();
    assert_eq!(queued.len(), 3, "the person's call took the freed slot");
    queued.run(2);
    pool.run_until_stalled();
    assert_eq!(*ran.lock().unwrap(), ["busy", "person"]);
    assert_eq!(*replies.lock().unwrap(), ["busy", "person"]);

    // The flood drains through its own two slots.
    while queued.len() != 0 {
        queued.run(0);
        pool.run_until_stalled();
    }
    assert_eq!(ran.lock().unwrap().len(), 22);
    assert!(
        ran.lock().unwrap()[2..]
            .iter()
            .all(|name| name.starts_with("background"))
    );
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn a_repeated_commit_is_one_commit_and_its_status_comes_first() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project");
    let hooks = temp.path().join("hooks");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::create_dir(&hooks).unwrap();
    git(&cwd, &["init", "-b", "main"]);
    for (key, value) in [
        ("core.autocrlf", "false"),
        ("user.name", "Engine test"),
        ("user.email", "engine@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", hooks.to_str().unwrap()),
    ] {
        git(&cwd, &["config", key, value]);
    }
    let checkout = cwd.to_string_lossy().into_owned();
    let hub = CheckoutHub::build(
        store(),
        Arc::new(ThreadSpawn),
        Arc::new(DiskProbe),
        Lanes::default(),
        Limits::default(),
        false,
        0,
    );
    let mut viewer = block_on(hub.watch_git(WatchGit {
        checkout: checkout.clone(),
        since: None,
    }))
    .unwrap();
    let Some(Some(Item::Snapshot { .. })) = viewer.next().now_or_never() else {
        panic!("a snapshot first");
    };

    std::fs::write(cwd.join("a.txt"), "a\n").unwrap();
    let stage = |checkout: &str| Stage {
        checkout: checkout.to_owned(),
        target: StageTarget::All,
    };
    let covered = block_on(hub.stage(Some(OpId::new()), stage(&checkout))).unwrap();
    let Some(Some(Item::Events { events, .. })) = viewer.next().now_or_never() else {
        panic!("the staged status is out before the reply");
    };
    let GitEvent::Status(staged) = events.last().unwrap();
    assert_eq!(
        covered.scan,
        Some(staged.scan),
        "the reply names the status that shows it"
    );
    let files = &staged.index.as_ref().unwrap().files;
    assert!(
        files
            .iter()
            .any(|file| file.relative == "a.txt" && file.staged)
    );

    let op = OpId::new();
    let commit = Commit {
        checkout: checkout.clone(),
        message: "First".into(),
        amend: false,
        push: false,
    };
    let first = block_on(hub.commit(Some(op), commit.clone())).unwrap();
    assert_eq!(git(&cwd, &["rev-list", "--count", "HEAD"]), "1");
    let Some(Some(Item::Events { events, .. })) = viewer.next().now_or_never() else {
        panic!("the clean status is out before the reply");
    };
    let GitEvent::Status(clean) = events.last().unwrap();
    assert!(clean.index.as_ref().unwrap().files.is_empty());
    assert_eq!(first.scan, Some(clean.scan));

    // A call that changes nothing the watch shows names the status it already holds.
    let again = block_on(hub.stage(Some(OpId::new()), stage(&checkout))).unwrap();
    assert_eq!(again.scan, Some(clean.scan));
    assert!(
        viewer.next().now_or_never().is_none(),
        "nothing new to send"
    );

    // The same op again, with something new staged: the first receipt, and no second commit.
    std::fs::write(cwd.join("b.txt"), "b\n").unwrap();
    block_on(hub.stage(Some(OpId::new()), stage(&checkout))).unwrap();
    let repeat = block_on(hub.commit(Some(op), commit.clone())).unwrap();
    assert_eq!(repeat, first);
    assert_eq!(git(&cwd, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(git(&cwd, &["diff", "--cached", "--name-only"]), "b.txt");

    // A fresh op commits it.
    block_on(hub.commit(Some(OpId::new()), commit)).unwrap();
    assert_eq!(git(&cwd, &["rev-list", "--count", "HEAD"]), "2");
}
