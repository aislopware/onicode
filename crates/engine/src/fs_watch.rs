//! The engine's file watches: a recursive watch per watched root, whose events a [`Coalescer`]
//! gathers into one rescan per burst, and the thread that pumps them into a hub.
//!
//! The watch never blocks the thread that reports an event (FSEvents' own): it only sends on an
//! unbounded channel. Rescans run on the root's pump thread, so a turn writing files never waits
//! on a listing or a `git status`. The coalescer is pure and runs on the hub's clock, so its tests
//! run in virtual time.
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Weak;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// A burst of writes is read once, this long after it starts.
pub(crate) const SETTLE_MS: u64 = 300;
/// A watched root is read again this often, in case an event went missing.
pub(crate) const REPAIR_MS: u64 = 30_000;
/// Without a file watch, a root is read this often instead.
pub(crate) const POLL_MS: u64 = 2_000;
/// Past this many distinct paths, a burst rescans everything.
const MAX_PATHS: usize = 4096;
/// How long an idle pump waits before checking whether its root is still watched.
const IDLE_MS: u64 = 60_000;

/// What the platform watch reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// These paths changed.
    Paths(Vec<String>),
    /// Events were dropped or the stream overflowed: anything may have changed.
    Overflow,
}

/// What a settled burst touched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Batch {
    pub(crate) paths: BTreeSet<String>,
    /// Rescan everything: an overflow, too many paths, or a repair read.
    pub(crate) all: bool,
}

/// Gathers changes into batches: the first change of a burst opens a batch that settles
/// [`SETTLE_MS`] later, and everything until then joins it.
#[derive(Debug, Default)]
pub(crate) struct Coalescer {
    open: Option<(u64, Batch)>,
}

impl Coalescer {
    /// `change` happened at `now`.
    pub(crate) fn note(&mut self, now: u64, change: Change) {
        let (_, batch) = self.open.get_or_insert_with(|| (now, Batch::default()));
        match change {
            Change::Overflow => batch.all = true,
            Change::Paths(paths) => {
                if !batch.all {
                    batch.paths.extend(paths);
                }
                if batch.paths.len() > MAX_PATHS {
                    batch.all = true;
                }
            }
        }
        if batch.all {
            batch.paths.clear();
        }
    }

    /// When the open batch settles.
    pub(crate) fn due(&self) -> Option<u64> {
        self.open.as_ref().map(|(at, _)| at + SETTLE_MS)
    }

    /// The batch, if it settled by `now`.
    pub(crate) fn take(&mut self, now: u64) -> Option<Batch> {
        if self.due().is_some_and(|due| due <= now) {
            return self.open.take().map(|(_, batch)| batch);
        }
        None
    }
}

/// A watched root's schedule: its coalescer and its next repair (or poll) read.
#[derive(Debug)]
pub(crate) struct Schedule {
    coalescer: Coalescer,
    next_repair: u64,
    every: u64,
}

impl Schedule {
    /// A root first read at `now`, watched by the platform when `watched`.
    pub(crate) fn new(now: u64, watched: bool) -> Self {
        let every = if watched { REPAIR_MS } else { POLL_MS };
        Self {
            coalescer: Coalescer::default(),
            next_repair: now + every,
            every,
        }
    }

    pub(crate) fn note(&mut self, now: u64, change: Change) {
        self.coalescer.note(now, change);
    }

    /// The next time [`Self::take`] has something.
    pub(crate) fn due(&self) -> u64 {
        self.coalescer
            .due()
            .map_or(self.next_repair, |due| due.min(self.next_repair))
    }

    /// What to read at `now`: a settled batch, and whether the read is the periodic repair,
    /// which then covers everything.
    pub(crate) fn take(&mut self, now: u64) -> Option<(Batch, bool)> {
        let settled = self.coalescer.take(now);
        if now >= self.next_repair {
            self.next_repair = now + self.every;
            return Some((
                Batch {
                    paths: BTreeSet::new(),
                    all: true,
                },
                settled.is_none(),
            ));
        }
        settled.map(|batch| (batch, false))
    }
}

/// What a pump drives: a hub that keeps one [`Schedule`] per watched key.
pub(crate) trait Pump: Send + Sync + 'static {
    /// The platform watch of `key` reported `change` at `now`.
    fn changed(&self, key: &str, change: Change, now: u64);
    /// When `key` next has something to read; `None` once it is no longer watched.
    fn due(&self, key: &str) -> Option<u64>;
    /// Reads what is due for `key` at `now`, on the pump's thread.
    fn beat(&self, key: &str, now: u64);
}

/// A root's platform watch and its pump thread: dropping it stops both.
pub(crate) struct FsWatch {
    _watcher: Option<notify::RecommendedWatcher>,
    /// Keeps the pump's channel open when the platform watch could not start.
    _keep: mpsc::Sender<Change>,
}

impl FsWatch {
    /// Watches every existing root of `roots` recursively for `key`, and pumps what it reports
    /// into `pump`. `watched` says whether the platform watch started; without it the pump still
    /// runs the key's polls.
    pub(crate) fn start(key: String, roots: &[PathBuf], pump: Weak<dyn Pump>) -> (Self, bool) {
        let (sender, changes) = mpsc::channel::<Change>();
        let events = sender.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let change = match event {
                Ok(event) if event.kind.is_access() => return,
                Ok(event) => Change::Paths(
                    event
                        .paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect(),
                ),
                Err(_) => Change::Overflow,
            };
            let _ = events.send(change);
        })
        .ok()
        .and_then(|mut watcher| {
            // Every root in one batch: FSEvents restarts its stream once, at the commit, where a
            // `watch` per root restarts it per root. The commit reports a stream that failed to
            // start, so a root nothing watches polls instead.
            let mut paths = watcher.paths_mut();
            let mut watching = false;
            for root in roots {
                if root.exists() && paths.add(root, RecursiveMode::Recursive).is_ok() {
                    watching = true;
                }
            }
            let started = paths.commit().is_ok();
            (watching && started).then_some(watcher)
        });
        let watched = watcher.is_some();
        let _ = std::thread::Builder::new()
            .name("onicode-fs-watch".into())
            .spawn(move || run(&key, &changes, &pump));
        (
            Self {
                _watcher: watcher,
                _keep: sender,
            },
            watched,
        )
    }
}

/// The pump: hands every change to the hub and beats it when something is due, until the key
/// is no longer watched or the hub is gone.
fn run(key: &str, changes: &mpsc::Receiver<Change>, pump: &Weak<dyn Pump>) {
    loop {
        let wait = {
            let Some(hub) = pump.upgrade() else {
                return;
            };
            let Some(due) = hub.due(key) else {
                return;
            };
            due.saturating_sub(onicode_core::time::now_ms())
                .min(IDLE_MS)
        };
        match changes.recv_timeout(Duration::from_millis(wait)) {
            Ok(change) => {
                let Some(hub) = pump.upgrade() else {
                    return;
                };
                hub.changed(key, change, onicode_core::time::now_ms());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let Some(hub) = pump.upgrade() else {
            return;
        };
        hub.beat(key, onicode_core::time::now_ms());
    }
}

/// Folders whose churn never changes what Git status shows.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".next",
    ".turbo",
    ".cache",
    "__pycache__",
    ".venv",
    "DerivedData",
];
/// Parts of a Git directory that change without changing status.
const IGNORED_GIT_DIRS: &[&str] = &["objects", "logs", "lfs", "hooks"];

/// Whether a change at `path` can change a checkout's Git status: build output, dependency
/// folders and Git's object store never do.
pub(crate) fn moves_status(path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name == "index.lock" || name == ".DS_Store")
    {
        return false;
    }
    let mut in_git = false;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let Some(name) = name.to_str() else {
            continue;
        };
        if in_git && IGNORED_GIT_DIRS.contains(&name) {
            return false;
        }
        if IGNORED_DIRS.contains(&name) {
            return false;
        }
        // A main checkout's `.git`, or a common dir such as `repo.git`.
        in_git = name == ".git" || name.ends_with(".git");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_index_and_refs_move_status_but_build_output_does_not() {
        assert!(moves_status(Path::new("/repo/src/main.rs")));
        assert!(moves_status(Path::new("/repo/.git/index")));
        assert!(moves_status(Path::new("/repo/.git/HEAD")));
        assert!(moves_status(Path::new("/repo/.git/refs/heads/main")));
        assert!(!moves_status(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(!moves_status(Path::new("/repo/.git/index.lock")));
        assert!(!moves_status(Path::new("/repo/target/debug/app")));
        assert!(!moves_status(Path::new(
            "/repo/web/node_modules/x/index.js"
        )));
    }

    fn paths(names: &[&str]) -> Change {
        Change::Paths(names.iter().map(|name| (*name).to_owned()).collect())
    }

    #[test]
    fn a_burst_settles_into_one_batch_its_first_change_times() {
        let mut coalescer = Coalescer::default();
        assert_eq!(coalescer.due(), None);
        coalescer.note(1_000, paths(&["/r/a"]));
        coalescer.note(1_100, paths(&["/r/b"]));
        coalescer.note(1_299, paths(&["/r/a"]));
        assert_eq!(coalescer.due(), Some(1_000 + SETTLE_MS));
        assert_eq!(coalescer.take(1_299), None, "still settling");
        let batch = coalescer.take(1_300).expect("settled");
        assert_eq!(
            batch.paths.into_iter().collect::<Vec<_>>(),
            vec!["/r/a".to_owned(), "/r/b".to_owned()]
        );
        assert!(!batch.all);
        assert_eq!(coalescer.take(5_000), None, "one batch per burst");
        coalescer.note(5_000, paths(&["/r/c"]));
        assert_eq!(coalescer.due(), Some(5_000 + SETTLE_MS), "a new burst");
    }

    #[test]
    fn an_overflow_or_a_flood_rescans_everything() {
        let mut coalescer = Coalescer::default();
        coalescer.note(0, paths(&["/r/a"]));
        coalescer.note(10, Change::Overflow);
        coalescer.note(20, paths(&["/r/b"]));
        let batch = coalescer.take(SETTLE_MS).unwrap();
        assert!(batch.all && batch.paths.is_empty());
        let flood: Vec<String> = (0..=MAX_PATHS).map(|n| format!("/r/{n}")).collect();
        coalescer.note(0, Change::Paths(flood));
        assert!(coalescer.take(SETTLE_MS).unwrap().all);
    }

    #[test]
    fn a_schedule_repairs_on_its_interval_and_polls_without_a_watch() {
        let mut watched = Schedule::new(0, true);
        assert_eq!(watched.due(), REPAIR_MS);
        assert_eq!(watched.take(REPAIR_MS - 1), None);
        let (batch, repair) = watched.take(REPAIR_MS).unwrap();
        assert!(batch.all && repair);
        assert_eq!(watched.due(), 2 * REPAIR_MS);
        watched.note(2 * REPAIR_MS - 10, paths(&["/r/a"]));
        assert_eq!(watched.due(), 2 * REPAIR_MS, "the repair comes first");
        let polled = Schedule::new(0, false);
        assert_eq!(polled.due(), POLL_MS);
    }
}
