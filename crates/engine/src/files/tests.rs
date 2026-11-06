//! The Files hub against its reference (engine.md "Tests"): a scripted disk changes, the file
//! watch's reports arrive in virtual time, and viewers join, fall behind, drop and come back at
//! cursors they held. Every watched directory must show what the model says the coalescer has
//! read, every viewer must end up showing the hub, and a listing at one cursor plus the events
//! after it must be the listing at the later one. Then the calls on a real disk: a repeated op.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use onicode_core::files::DirEntry;
use onicode_core::store::Store;
use onicode_protocol::files::{DeletePath, DirEvent, Entries, Listing, WatchDir};
use onicode_protocol::{Cursor, Item, OpId, Outcome};
use proptest::prelude::*;

use super::hub::FilesHub;
use crate::feed::{Feed, Limits};
use crate::fs_watch::{Change, SETTLE_MS};
use crate::probe::{DiskProbe, Scripted};
use crate::session::ThreadSpawn;
use crate::turn::Database;

const ROOT: &str = "/r";
const DIRS: [&str; 4] = ["/r", "/r/a", "/r/b", "/r/a/c"];

type Stream = BoxStream<'static, Item<Listing, DirEvent>>;

fn store() -> Database {
    Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()))
}

/// `count` files in `dir`, named after `seed` so two listings rarely match.
fn listing(dir: &str, seed: u8, count: u8) -> Vec<DirEntry> {
    (0..count)
        .map(|n| {
            let name = format!("f{seed}-{n}");
            DirEntry {
                path: format!("{dir}/{name}"),
                name,
                is_dir: false,
                ignored: n % 3 == 0,
            }
        })
        .collect()
}

struct Harness {
    disk: Arc<Scripted>,
    hub: Arc<FilesHub>,
    /// Virtual now, from the wall clock at the start: the schedules' repair reads lie 30 s past
    /// it, further than any run goes.
    now: u64,
}

impl Harness {
    fn new(limits: Limits) -> Self {
        let disk = Arc::new(Scripted::default());
        for (at, dir) in DIRS.iter().enumerate() {
            disk.dirs.lock().unwrap().insert(
                (*dir).to_owned(),
                Ok(listing(dir, u8::try_from(at).unwrap(), 2)),
            );
        }
        let hub = FilesHub::build(
            store(),
            Arc::new(ThreadSpawn),
            disk.clone(),
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

    fn watch(&self, path: &str, since: Option<Cursor>) -> Stream {
        block_on(self.hub.watch_dir(WatchDir {
            root: ROOT.into(),
            path: path.into(),
            since,
        }))
        .unwrap()
    }

    /// What a viewer joining now is shown first.
    fn shown(&self, path: &str) -> (Cursor, Listing) {
        let mut stream = self.watch(path, None);
        match stream.next().now_or_never() {
            Some(Some(Item::Snapshot { at, state })) => (at, state),
            other => panic!("a new watcher starts from a snapshot, not {other:?}"),
        }
    }

    fn disk(&self, dir: &str) -> Result<Vec<DirEntry>, String> {
        self.disk.dirs.lock().unwrap()[dir].clone()
    }
}

fn entries_of(listed: Result<Vec<DirEntry>, String>) -> Entries {
    match listed {
        Ok(entries) => Entries::Read(entries),
        Err(error) => Entries::Failed(error),
    }
}

/// A viewer of one directory: its stream, the listing it folded, its cursor, and every
/// (cursor, listing) it held, to come back at.
struct Viewer {
    dir: &'static str,
    stream: Option<Stream>,
    shown: Option<(Cursor, Listing)>,
    held: Vec<(Cursor, Listing)>,
}

impl Viewer {
    fn join(harness: &Harness, dir: &'static str) -> Self {
        Self {
            dir,
            stream: Some(harness.watch(dir, None)),
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
                        super::hub::DirFeed::apply(state, event);
                    }
                    cursor.seq = last;
                }
                Item::Resync => {
                    let since = self.shown.as_ref().map(|(cursor, _)| *cursor);
                    self.stream = Some(harness.watch(self.dir, since));
                }
            }
            if let Some(shown) = &self.shown {
                self.held.push(shown.clone());
            }
        }
    }

    /// Comes back at a cursor it held, with the listing it held there.
    fn rejoin(&mut self, harness: &Harness, pick: usize) {
        let back = (!self.held.is_empty()).then(|| self.held[pick % self.held.len()].clone());
        self.stream = None;
        let since = back.as_ref().map(|(cursor, _)| *cursor);
        self.shown = back;
        self.stream = Some(harness.watch(self.dir, since));
    }
}

/// The reference: what each watched directory shows, and the coalescer's open batch.
#[derive(Default)]
struct Model {
    shown: BTreeMap<&'static str, Entries>,
    open: Option<(u64, BTreeSet<&'static str>, bool)>,
}

#[derive(Clone, Debug)]
enum Step {
    /// A directory's children change on disk and the watch reports one path in it.
    Change {
        dir: usize,
        seed: u8,
        count: u8,
    },
    /// A directory becomes unreadable.
    Fail {
        dir: usize,
    },
    /// The watch dropped events.
    Overflow,
    Advance(u16),
    Rescan,
    Join {
        dir: usize,
    },
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
        4 => (0..DIRS.len(), any::<u8>(), 0..12_u8)
            .prop_map(|(dir, seed, count)| Step::Change { dir, seed, count }),
        1 => (0..DIRS.len()).prop_map(|dir| Step::Fail { dir }),
        1 => Just(Step::Overflow),
        3 => (0..400_u16).prop_map(Step::Advance),
        3 => Just(Step::Rescan),
        2 => (0..DIRS.len()).prop_map(|dir| Step::Join { dir }),
        4 => any::<usize>().prop_map(|viewer| Step::Pump { viewer }),
        1 => any::<usize>().prop_map(|viewer| Step::Drop { viewer }),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(viewer, pick)| Step::Rejoin { viewer, pick }),
    ]
}

/// Runs `steps` against the hub and the model. `limits` small enough make viewers fall out of
/// the ring and overflow their queues.
fn run(steps: &[Step], limits: Limits) {
    let mut harness = Harness::new(limits);
    let start = harness.now;
    // One watcher per directory that never reads: it keeps every topic, and the root's
    // schedule, alive across the run, as a sidebar that stays open does.
    let _anchors: Vec<Stream> = DIRS.iter().map(|dir| harness.watch(dir, None)).collect();
    let mut model = Model::default();
    for dir in DIRS {
        model.shown.insert(dir, entries_of(harness.disk(dir)));
    }
    let mut viewers: Vec<Viewer> = Vec::new();
    for step in steps {
        match step {
            Step::Change { dir, seed, count } => {
                let dir = DIRS[*dir];
                harness
                    .disk
                    .dirs
                    .lock()
                    .unwrap()
                    .insert(dir.to_owned(), Ok(listing(dir, *seed, *count)));
                harness.hub.note(
                    ROOT,
                    Change::Paths(vec![format!("{dir}/f{seed}")]),
                    harness.now,
                );
                let (_, touched, _) = model
                    .open
                    .get_or_insert_with(|| (harness.now, BTreeSet::new(), false));
                touched.insert(dir);
            }
            Step::Fail { dir } => {
                let dir = DIRS[*dir];
                harness
                    .disk
                    .dirs
                    .lock()
                    .unwrap()
                    .insert(dir.to_owned(), Err(format!("{dir}: Permission denied")));
                harness
                    .hub
                    .note(ROOT, Change::Paths(vec![dir.to_owned()]), harness.now);
                let (_, touched, _) = model
                    .open
                    .get_or_insert_with(|| (harness.now, BTreeSet::new(), false));
                // A directory that changes is a child of its parent that changed.
                touched.insert(dir);
                if let Some(parent) = DIRS
                    .into_iter()
                    .find(|parent| dir.rsplit_once('/').is_some_and(|(at, _)| at == *parent))
                {
                    touched.insert(parent);
                }
            }
            Step::Overflow => {
                harness.hub.note(ROOT, Change::Overflow, harness.now);
                model
                    .open
                    .get_or_insert_with(|| (harness.now, BTreeSet::new(), false))
                    .2 = true;
            }
            Step::Advance(ms) => harness.now += u64::from(*ms),
            Step::Rescan => {
                harness.hub.rescan(ROOT, harness.now);
                if let Some((first, touched, all)) = model.open.take() {
                    if first + SETTLE_MS <= harness.now {
                        for dir in DIRS {
                            if all || touched.contains(dir) {
                                model.shown.insert(dir, entries_of(harness.disk(dir)));
                            }
                        }
                    } else {
                        model.open = Some((first, touched, all));
                    }
                }
            }
            Step::Join { dir } => {
                if viewers.len() < 8 {
                    viewers.push(Viewer::join(&harness, DIRS[*dir]));
                }
            }
            Step::Pump { viewer } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    let viewer = &mut viewers[viewer % count];
                    viewer.pump(&harness);
                    let (_, hub) = harness.shown(viewer.dir);
                    let (_, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
                    assert_eq!(folded, &hub, "a viewer folds to the hub's listing");
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
        for dir in DIRS {
            assert_eq!(
                harness.shown(dir).1.entries,
                model.shown[dir],
                "{dir} shows what the coalescer read after {step:?}"
            );
        }
        assert!(harness.now < start + 25_000, "no run reaches a repair read");
    }
    for viewer in &mut viewers {
        viewer.pump(&harness);
        let (cursor, hub) = harness.shown(viewer.dir);
        let (held, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
        assert_eq!(folded, &hub, "every viewer ends up showing the hub");
        assert_eq!(held, &cursor, "and holds the hub's cursor");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn dir_watches_show_what_the_coalescer_read(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        run(&steps, Limits::default());
    }

    #[test]
    fn viewers_that_fall_out_of_the_ring_or_overflow_come_back_whole(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        // A ring of four listings, and a queue that a listing of a dozen entries overflows.
        run(&steps, Limits { ring: (4, 1 << 20), queue: (256, 600) });
    }
}

#[test]
fn a_burst_is_one_read_of_the_directories_it_touched() {
    let harness = Harness::new(Limits::default());
    let _a = harness.watch("/r/a", None);
    let _b = harness.watch("/r/b", None);
    let _c = harness.watch("/r/a/c", None);
    let (_, before) = harness.shown("/r/b");
    let reads = || harness.disk.reads.load(Ordering::Relaxed);
    let t0 = harness.now;
    for (dir, seed) in [("/r/a", 10), ("/r/b", 11)] {
        harness
            .disk
            .dirs
            .lock()
            .unwrap()
            .insert(dir.into(), Ok(listing(dir, seed, 3)));
    }
    harness
        .hub
        .note(ROOT, Change::Paths(vec!["/r/a/f10".into()]), t0);
    harness
        .hub
        .note(ROOT, Change::Paths(vec!["/r/b/f11".into()]), t0 + 100);
    harness.hub.note(
        ROOT,
        Change::Paths(vec!["/r/a/f10".into()]),
        t0 + SETTLE_MS - 1,
    );
    assert_eq!(harness.hub.next_due(ROOT), Some(t0 + SETTLE_MS));
    let read = reads();
    harness.hub.rescan(ROOT, t0 + SETTLE_MS - 1);
    assert_eq!(reads(), read, "nothing is read while the burst settles");
    assert_eq!(harness.shown("/r/b").1, before);

    harness.hub.rescan(ROOT, t0 + SETTLE_MS);
    assert_eq!(
        reads(),
        read + 2,
        "one read per touched directory, /r/a/c untouched"
    );
    let (_, a) = harness.shown("/r/a");
    let (_, b) = harness.shown("/r/b");
    assert_eq!(a.entries, Entries::Read(listing("/r/a", 10, 3)));
    assert_eq!(b.entries, Entries::Read(listing("/r/b", 11, 3)));
    assert_eq!(a.scan, b.scan, "read together, one scan");
    harness.hub.rescan(ROOT, t0 + 2 * SETTLE_MS);
    assert_eq!(reads(), read + 2, "a settled burst is read once");
}

#[test]
fn an_unchanged_reread_publishes_nothing() {
    let harness = Harness::new(Limits::default());
    let mut viewer = harness.watch("/r/a", None);
    let Some(Some(Item::Snapshot { .. })) = viewer.next().now_or_never() else {
        panic!("a snapshot first");
    };
    harness
        .hub
        .note(ROOT, Change::Paths(vec!["/r/a/f1-0".into()]), harness.now);
    harness.hub.rescan(ROOT, harness.now + SETTLE_MS);
    assert!(
        viewer.next().now_or_never().is_none(),
        "the same listing is not sent again"
    );
}

#[test]
fn reports_under_the_resolved_root_reach_the_root_as_viewers_name_it() {
    // A temporary directory on macOS is under /var, which the file watch reports as
    // /private/var.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let canonical = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let disk = Arc::new(Scripted::default());
    disk.dirs
        .lock()
        .unwrap()
        .insert(root.clone(), Ok(listing(&root, 1, 1)));
    let hub = FilesHub::build(
        store(),
        Arc::new(ThreadSpawn),
        disk.clone(),
        Limits::default(),
        false,
        0,
    );
    let mut viewer = block_on(hub.watch_dir(WatchDir {
        root: root.clone(),
        path: root.clone(),
        since: None,
    }))
    .unwrap();
    let Some(Some(Item::Snapshot { .. })) = viewer.next().now_or_never() else {
        panic!("a snapshot first");
    };
    disk.dirs
        .lock()
        .unwrap()
        .insert(root.clone(), Ok(listing(&root, 2, 2)));
    let now = onicode_core::time::now_ms();
    hub.note(&root, Change::Paths(vec![format!("{canonical}/f2-0")]), now);
    hub.rescan(&root, now + SETTLE_MS);
    let Some(Some(Item::Events { events, .. })) = viewer.next().now_or_never() else {
        panic!("the change reaches the viewer");
    };
    assert_eq!(
        events,
        vec![DirEvent::Listing(Listing {
            scan: 2,
            entries: Entries::Read(listing(&root, 2, 2)),
        })]
    );
}

#[test]
fn a_repeated_delete_is_one_delete_and_its_listing_comes_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "first").unwrap();
    let hub = FilesHub::build(
        store(),
        Arc::new(ThreadSpawn),
        Arc::new(DiskProbe),
        Limits::default(),
        false,
        0,
    );
    let mut viewer = block_on(hub.watch_dir(WatchDir {
        root: root.clone(),
        path: root.clone(),
        since: None,
    }))
    .unwrap();
    let Some(Some(Item::Snapshot { state, .. })) = viewer.next().now_or_never() else {
        panic!("a snapshot first");
    };
    assert!(matches!(&state.entries, Entries::Read(entries) if entries.len() == 1));

    let op = OpId::new();
    let delete = DeletePath {
        path: file.to_string_lossy().into_owned(),
    };
    let first = block_on(hub.delete(Some(op), delete.clone())).unwrap();
    assert_eq!(first, Outcome::Applied);
    assert!(!file.exists());
    let Some(Some(Item::Events { events, .. })) = viewer.next().now_or_never() else {
        panic!("the parent's new listing is out before the reply");
    };
    assert_eq!(
        events
            .last()
            .map(|DirEvent::Listing(listing)| &listing.entries),
        Some(&Entries::Read(Vec::new()))
    );

    // The same op again, after the file came back: the first receipt, and no second delete.
    std::fs::write(&file, "second").unwrap();
    let repeat = block_on(hub.delete(Some(op), delete.clone())).unwrap();
    assert_eq!(repeat, first);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");

    // A fresh op deletes it.
    block_on(hub.delete(Some(OpId::new()), delete)).unwrap();
    assert!(!file.exists());
}
