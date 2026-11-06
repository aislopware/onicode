//! FsWatchHub — one `notify` watcher shared by every subsystem that live-watches
//! folders (diff-sync checkouts, space folders).
//!
//! Every `notify::RecommendedWatcher` owns a dedicated OS thread (the FSEvents
//! run loop on macOS, the inotify loop on Linux). One watcher per checkout and
//! per space folder put ~25-30 idle threads on a normal device at boot. The hub
//! instead installs every path on a single watcher and routes each event to
//! the registrations whose path covers it, so the whole engine pays for one
//! watcher thread. Exception: inotify/kqueue get one watcher per installed
//! root (see [`SHARED_WATCHER`]); they still share the routing and batching.
//!
//! Semantics match a private watcher per registration:
//!
//! - a [`notify::RecursiveMode::NonRecursive`] registration only sees events for its folder and
//!   that folder's direct children (notify's own rule);
//! - pathless events (inotify queue overflow → rescan) reach every registration, since an overflow
//!   loses events for all of them;
//! - dropping the [`FsWatch`] handle stops delivery immediately and releases the OS watch once no
//!   other registration needs it.
//!
//! Handlers also receive synthetic *rescan* events
//! ([`notify::Event::need_rescan`]): once when the registration's watch goes
//! live (anything before that was unobserved), and whenever events may have
//! been lost (see below). Callers treat them like any change.
//!
//! **Batching.** Registration changes are coalesced on a short-lived worker
//! thread ([`BATCH_QUIET`] of quiet, at most [`BATCH_MAX`]) and applied in one
//! pass, so a boot or a burst of entries is one reconcile, not one per path.
//!
//! **Seams.** The OS watchers come from an [`Opener`] and the batch worker
//! waits through a [`Pace`]; tests swap in a scripted OS and a hand-stepped
//! pace, so routing, batching and restart-gap handling are proven without
//! depending on when FSEvents or the scheduler gets round to anything.
//!
//! **Restart gaps.** notify 7's FSEvents backend stops the stream, joins its
//! thread and starts a new stream (from "now" — the event id is not settable)
//! on *every* `watch`/`unwatch`, so each change drops events for every other
//! path for that moment. While a pass changes the main watcher, the hub runs
//! a temporary *bridge* stream on the surviving paths' common ancestor, started
//! before the first restart and stopped after the last: the surviving paths
//! stay covered throughout and nothing needs re-capturing. If the bridge cannot
//! be opened, the pass ends by sending a rescan to every registration that was
//! live across it (one per registration per pass; callers debounce). inotify
//! watches are per path and never restart, so this does not apply there.
//!
//! OS watches are the minimal cover of the registrations: identical paths are
//! installed once (recursive if any registration wants recursion) and a path
//! under a recursively watched ancestor is not installed separately. When the
//! covering watch of a live registration is replaced (a nested path takes over
//! from a dropped recursive ancestor, a mode change), the registration gets a
//! rescan: the swap itself was unobserved.
//!
//! Watch failures (the folder vanished, inotify limits) are logged and the
//! registration stays without a live watch, exactly like a private watcher
//! whose `watch()` failed — callers keep their repair ticks for that case.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

/// A batch applies once registrations have been quiet this long…
const BATCH_QUIET: Duration = Duration::from_millis(100);
/// …or once its worker has waited out this much of quiet intervals since its
/// first change, whichever comes first.
const BATCH_MAX: Duration = Duration::from_secs(1);
/// Whether a watch change restarts the stream for every path (FSEvents).
const STREAM_RESTARTS_ON_CHANGE: bool = cfg!(target_os = "macos");
/// Whether every root can share one watcher. inotify (and kqueue) build a
/// recursive watch by walking the tree *following symlinks*, so in a shared
/// instance a checkout that symlinks into another checkout (`npm link`) would
/// alias that checkout's watch descriptors: its events would be relabelled
/// and unwatching either root would strip the other. Those backends get one
/// watcher per installed root instead, exactly as before the hub.
const SHARED_WATCHER: bool = cfg!(target_os = "macos");

type Handler = Arc<dyn Fn(&notify::Event) + Send + Sync>;
/// Where an OS watcher delivers what it observes.
type Sink = Arc<dyn Fn(notify::Result<notify::Event>) + Send + Sync>;
/// Opens an OS watcher delivering to a [`Sink`]: [`recommended_watcher`] in
/// production, a scripted OS in tests.
type Opener = Arc<dyn Fn(Sink) -> notify::Result<Box<dyn OsWatcher>> + Send + Sync>;

/// The part of a `notify` watcher the hub drives.
trait OsWatcher: Send {
    fn watch(&mut self, path: &Path, mode: notify::RecursiveMode) -> notify::Result<()>;
    fn unwatch(&mut self, path: &Path) -> notify::Result<()>;
}

impl OsWatcher for notify::RecommendedWatcher {
    fn watch(&mut self, path: &Path, mode: notify::RecursiveMode) -> notify::Result<()> {
        notify::Watcher::watch(self, path, mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        notify::Watcher::unwatch(self, path)
    }
}

/// The platform's watcher (FSEvents, inotify, …).
fn recommended_watcher(sink: Sink) -> notify::Result<Box<dyn OsWatcher>> {
    let watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| sink(event))?;
    Ok(Box::new(watcher))
}

/// How the batch worker waits out a quiet interval.
trait Pace: Send + Sync {
    fn quiet(&self, interval: Duration);
    /// The worker has cleared [`Batch::worker`] and is exiting.
    fn idle(&self) {}
}

/// Production [`Pace`]: sleep the interval.
struct Sleep;

impl Pace for Sleep {
    fn quiet(&self, interval: Duration) {
        std::thread::sleep(interval);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The synthetic "anything may have changed" signal.
fn rescan_event() -> notify::Event {
    notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)
}

struct Registration {
    /// Canonical path — what FSEvents/inotify report events under.
    path: PathBuf,
    recursive: bool,
    handler: Handler,
    /// Shared with the [`FsWatch`]: an installed OS watch covers `path`.
    attached: Arc<AtomicBool>,
}

impl Registration {
    fn covers(&self, path: &Path) -> bool {
        path.starts_with(&self.path)
            && (self.recursive || path == self.path || path.parent() == Some(self.path.as_path()))
    }
}

#[derive(Default)]
struct Routes {
    next_id: u64,
    regs: HashMap<u64, Registration>,
}

#[derive(Default)]
struct OsState {
    /// The shared watcher ([`SHARED_WATCHER`] backends). Created lazily on the
    /// first install. On macOS an FSEvents watcher with no paths has no
    /// run-loop thread at all.
    watcher: Option<Box<dyn OsWatcher>>,
    /// One watcher per installed root on the other backends. Dropping one
    /// closes its instance, releasing every OS watch it added — including
    /// those a failed recursive `watch` left behind.
    roots: HashMap<PathBuf, Box<dyn OsWatcher>>,
    /// Canonical path → recursive, as installed.
    installed: HashMap<PathBuf, bool>,
    /// Wanted paths whose watch failed. Not retried while still wanted (every
    /// FSEvents watch/unwatch restarts the stream for *all* paths); forgotten
    /// once nothing registers them, so a fresh registration retries.
    failed: HashSet<PathBuf>,
}

#[derive(Default)]
struct Batch {
    /// Bumped on every registration change.
    generation: u64,
    /// A batch worker thread is pending or applying.
    worker: bool,
}

#[derive(Default)]
struct Stats {
    /// Passes that changed the main watcher.
    passes: AtomicUsize,
    /// Bridge streams opened.
    bridges: AtomicUsize,
}

pub(crate) struct FsWatchHub {
    /// Shared with the watcher callbacks. Never held across a watcher call:
    /// FSEvents `watch`/`unwatch`/drop stop and join the run-loop thread,
    /// which may be inside the callback waiting on this lock.
    routes: Arc<Mutex<Routes>>,
    /// Serializes OS watch changes.
    os: Mutex<OsState>,
    open: Opener,
    batch: Mutex<Batch>,
    /// Paces the batch worker. `None` (tests only): no worker; registration
    /// changes wait for an explicit [`FsWatchHub::apply`].
    pace: Option<Arc<dyn Pace>>,
    /// [`STREAM_RESTARTS_ON_CHANGE`]; a field so tests can drive both modes.
    restarts_on_change: bool,
    /// [`SHARED_WATCHER`]; a field so tests can drive both modes.
    shared_watcher: bool,
    bridge_enabled: AtomicBool,
    #[cfg_attr(not(test), allow(dead_code))]
    stats: Stats,
}

/// A registration. Dropping it stops delivery and releases the OS watch.
pub(crate) struct FsWatch {
    hub: Arc<FsWatchHub>,
    id: u64,
    attached: Arc<AtomicBool>,
}

impl FsWatch {
    /// Whether an installed OS watch currently covers the path. False until
    /// the registration's batch applies, and for good if the watch failed.
    pub(crate) fn is_live(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }
}

impl Drop for FsWatch {
    fn drop(&mut self) {
        if lock(&self.hub.routes).regs.remove(&self.id).is_some() {
            self.hub.schedule();
        }
    }
}

/// Resets [`Batch::worker`] if a pass panics, so later changes still get a
/// worker. A normal exit resets it in [`FsWatchHub::run_batches`] itself.
struct WorkerGuard<'a>(&'a FsWatchHub);

impl Drop for WorkerGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            lock(&self.0.batch).worker = false;
        }
    }
}

impl FsWatchHub {
    pub(crate) fn new() -> Arc<Self> {
        let pace: Arc<dyn Pace> = Arc::new(Sleep);
        Self::with(
            Arc::new(recommended_watcher),
            Some(pace),
            SHARED_WATCHER,
            STREAM_RESTARTS_ON_CHANGE,
        )
    }

    fn with(
        open: Opener,
        pace: Option<Arc<dyn Pace>>,
        shared_watcher: bool,
        restarts_on_change: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            routes: Arc::new(Mutex::new(Routes::default())),
            os: Mutex::new(OsState::default()),
            open,
            batch: Mutex::new(Batch::default()),
            pace,
            restarts_on_change,
            shared_watcher,
            bridge_enabled: AtomicBool::new(true),
            stats: Stats::default(),
        })
    }

    /// The process-wide hub.
    pub(crate) fn global() -> Arc<Self> {
        static HUB: OnceLock<Arc<FsWatchHub>> = OnceLock::new();
        HUB.get_or_init(Self::new).clone()
    }

    /// Route events under `path` to `handler` until the returned handle drops.
    /// Returns at once; the OS watch is installed by the next batch, which
    /// then sends `handler` a rescan event. Canonicalizes `path` (a blocking
    /// stat) — call from the blocking pool.
    pub(crate) fn watch(
        self: &Arc<Self>,
        path: &Path,
        mode: notify::RecursiveMode,
        handler: impl Fn(&notify::Event) + Send + Sync + 'static,
    ) -> FsWatch {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let attached = Arc::new(AtomicBool::new(false));
        let id = {
            let mut routes = lock(&self.routes);
            routes.next_id += 1;
            let id = routes.next_id;
            routes.regs.insert(
                id,
                Registration {
                    path,
                    recursive: matches!(mode, notify::RecursiveMode::Recursive),
                    handler: Arc::new(handler),
                    attached: attached.clone(),
                },
            );
            id
        };
        self.schedule();
        FsWatch {
            hub: self.clone(),
            id,
            attached,
        }
    }

    /// Note a registration change; start a batch worker unless one is pending.
    /// The worker is a short-lived plain thread (not the tokio blocking pool):
    /// a task queued on a runtime that shuts down before running it would
    /// leave `worker` set and wedge the hub for the rest of the process.
    fn schedule(self: &Arc<Self>) {
        let pace = {
            let mut batch = lock(&self.batch);
            batch.generation += 1;
            let Some(pace) = self.pace.clone() else {
                return;
            };
            if batch.worker {
                return;
            }
            batch.worker = true;
            pace
        };
        let hub = self.clone();
        let spawned = std::thread::Builder::new()
            .name("fs-watch batch".into())
            .spawn(move || hub.run_batches(&*pace));
        if let Err(err) = spawned {
            tracing::debug!(error = %err, "fs-watch: batch worker spawn failed; applying inline");
            lock(&self.batch).worker = false;
            self.apply();
        }
    }

    fn run_batches(&self, pace: &dyn Pace) {
        let _guard = WorkerGuard(self);
        loop {
            let mut waited = Duration::ZERO;
            let mut seen = lock(&self.batch).generation;
            loop {
                pace.quiet(BATCH_QUIET);
                waited += BATCH_QUIET;
                let now = lock(&self.batch).generation;
                let quiet = now == seen;
                seen = now;
                if quiet || waited >= BATCH_MAX {
                    break;
                }
            }
            self.apply();
            // Changes that landed during the pass need another one. Release
            // `worker` under the same lock as the check: a `schedule` between
            // the two would see a worker that is about to exit and be lost.
            let mut batch = lock(&self.batch);
            if batch.generation == seen {
                batch.worker = false;
                drop(batch);
                pace.idle();
                return;
            }
        }
    }

    /// Bring the installed OS watches in line with the registrations and
    /// deliver the resulting rescan signals. Blocking. Idempotent.
    pub(crate) fn apply(&self) {
        let rescans = {
            let mut os = lock(&self.os);
            self.reconcile_os(&mut os)
        };
        let event = rescan_event();
        for handler in rescans {
            handler(&event);
        }
    }

    fn reconcile_os(&self, os: &mut OsState) -> Vec<Handler> {
        // Wanted set: distinct paths, recursive if any registration recurses.
        let mut wanted: HashMap<PathBuf, bool> = HashMap::new();
        for reg in lock(&self.routes).regs.values() {
            *wanted.entry(reg.path.clone()).or_default() |= reg.recursive;
        }
        os.failed.retain(|path| wanted.contains_key(path));
        let before = os.installed.clone();
        let mut bridge: Option<Box<dyn OsWatcher>> = None;
        let mut bridge_tried = false;
        let mut touched = false;
        // A failed install may uncover paths it would have covered — iterate
        // until a pass adds no new failures (bounded: `failed` only grows).
        loop {
            let desired = minimal_cover(&wanted, &os.failed);
            if desired == os.installed {
                break;
            }
            if self.restarts_on_change && !bridge_tried {
                bridge_tried = true;
                bridge = self.open_bridge(&os.installed, &desired);
            }
            // Removals before additions, so an outgoing watch never overlaps an
            // incoming one on the same watcher.
            let stale: Vec<PathBuf> = os
                .installed
                .iter()
                .filter(|(path, recursive)| desired.get(*path) != Some(*recursive))
                .map(|(path, _)| path.clone())
                .collect();
            for path in stale {
                os.installed.remove(&path);
                if os.roots.remove(&path).is_some() {
                    touched = true;
                } else if let Some(watcher) = os.watcher.as_mut() {
                    touched = true;
                    if let Err(err) = watcher.unwatch(&path) {
                        tracing::debug!(path = %path.display(), error = %err, "fs-watch: unwatch failed");
                    }
                }
            }
            let mut new_failure = false;
            for (path, recursive) in desired {
                if os.installed.get(&path) == Some(&recursive) {
                    continue;
                }
                match self.install(os, &path, recursive, &mut touched) {
                    Ok(()) => {
                        os.installed.insert(path, recursive);
                    }
                    Err(err) => {
                        tracing::debug!(path = %path.display(), error = %err, "fs-watch: watch failed");
                        os.failed.insert(path);
                        new_failure = true;
                    }
                }
            }
            if !new_failure {
                break;
            }
        }
        if touched {
            self.stats.passes.fetch_add(1, Ordering::Relaxed);
        }
        // Only now — the main stream is back up — may the bridge stop.
        let bridged = bridge.is_some();
        drop(bridge);
        let gap = self.restarts_on_change && touched && !bridged;

        let routes = lock(&self.routes);
        let mut rescans = Vec::new();
        for reg in routes.regs.values() {
            if !is_covered(&reg.path, &os.installed) {
                reg.attached.store(false, Ordering::Release);
                continue;
            }
            let was_attached = reg.attached.swap(true, Ordering::AcqRel);
            // New: everything before the watch went live was unobserved.
            // Gap: events may have been dropped while the stream restarted.
            // Moved: the OS watch covering it was replaced (a recursive root
            // dropped for a nested one, a mode change) — the bridge only spans
            // unchanged roots, so the swap itself was unobserved.
            let moved =
                covering_roots(&reg.path, &before) != covering_roots(&reg.path, &os.installed);
            if !was_attached || moved || (gap && is_covered(&reg.path, &before)) {
                rescans.push(reg.handler.clone());
            }
        }
        rescans
    }

    /// Start a stream covering every installed path that survives this pass,
    /// so the main stream's restarts drop nothing for them. One recursive
    /// watch on their common ancestor: FSEvents filters a volume-wide journal
    /// by path prefix, so a broad path costs no per-directory watches — only
    /// unrelated events to filter in [`dispatch`] for the bridge's short life.
    fn open_bridge(
        &self,
        installed: &HashMap<PathBuf, bool>,
        desired: &HashMap<PathBuf, bool>,
    ) -> Option<Box<dyn OsWatcher>> {
        if !self.bridge_enabled.load(Ordering::Relaxed) {
            return None;
        }
        let survivors: Vec<&PathBuf> = installed
            .iter()
            .filter(|(path, recursive)| desired.get(*path) == Some(*recursive))
            .map(|(path, _)| path)
            .collect();
        let ancestor = common_ancestor(&survivors)?;
        let routes = self.routes.clone();
        let sink: Sink = Arc::new(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                dispatch(&routes, &event);
            }
        });
        let opened = (self.open)(sink).and_then(|mut watcher| {
            watcher
                .watch(&ancestor, notify::RecursiveMode::Recursive)
                .map(|()| watcher)
        });
        match opened {
            Ok(watcher) => {
                self.stats.bridges.fetch_add(1, Ordering::Relaxed);
                Some(watcher)
            }
            Err(err) => {
                tracing::debug!(path = %ancestor.display(), error = %err,
                    "fs-watch: bridge failed; surviving watches get a rescan");
                None
            }
        }
    }

    fn install(
        &self,
        os: &mut OsState,
        path: &Path,
        recursive: bool,
        touched: &mut bool,
    ) -> Result<(), notify::Error> {
        // FSEvents restarts the stream for every path even when `watch` fails —
        // skip the obvious failure without touching the watcher.
        if !path.exists() {
            return Err(notify::Error::path_not_found().add_path(path.to_path_buf()));
        }
        let new_watcher = || {
            let routes = self.routes.clone();
            let sink: Sink = Arc::new(move |event: notify::Result<notify::Event>| match event {
                Ok(event) => dispatch(&routes, &event),
                Err(err) => tracing::debug!(error = %err, "fs-watch: watcher error"),
            });
            (self.open)(sink)
        };
        let mode = if recursive {
            notify::RecursiveMode::Recursive
        } else {
            notify::RecursiveMode::NonRecursive
        };
        if !self.shared_watcher {
            // On failure the watcher drops here, with any partial watches.
            let mut watcher = new_watcher()?;
            *touched = true;
            watcher.watch(path, mode)?;
            os.roots.insert(path.to_path_buf(), watcher);
            return Ok(());
        }
        if os.watcher.is_none() {
            os.watcher = Some(new_watcher()?);
        }
        *touched = true;
        os.watcher
            .as_mut()
            .expect("watcher just created")
            .watch(path, mode)
    }

    #[cfg(test)]
    fn installed(&self) -> HashMap<PathBuf, bool> {
        lock(&self.os).installed.clone()
    }
}

fn root_covers(root: &Path, recursive: bool, path: &Path) -> bool {
    path == root || (recursive && path.starts_with(root))
}

fn is_covered(path: &Path, installed: &HashMap<PathBuf, bool>) -> bool {
    installed
        .iter()
        .any(|(root, recursive)| root_covers(root, *recursive, path))
}

fn covering_roots<'a>(
    path: &Path,
    installed: &'a HashMap<PathBuf, bool>,
) -> std::collections::BTreeSet<(&'a PathBuf, bool)> {
    installed
        .iter()
        .filter(|(root, recursive)| root_covers(root, **recursive, path))
        .map(|(root, recursive)| (root, *recursive))
        .collect()
}

fn common_ancestor(paths: &[&PathBuf]) -> Option<PathBuf> {
    let (first, rest) = paths.split_first()?;
    let mut ancestor: &Path = first;
    for path in rest {
        while !path.starts_with(ancestor) {
            ancestor = ancestor.parent()?;
        }
    }
    Some(ancestor.to_path_buf())
}

/// Paths to install: every wanted, non-failed path not already inside another
/// installable recursive path.
fn minimal_cover(
    wanted: &HashMap<PathBuf, bool>,
    failed: &HashSet<PathBuf>,
) -> HashMap<PathBuf, bool> {
    let candidates: Vec<(&PathBuf, bool)> = wanted
        .iter()
        .filter(|(path, _)| !failed.contains(*path))
        .map(|(path, recursive)| (path, *recursive))
        .collect();
    candidates
        .iter()
        .filter(|(path, _)| {
            !candidates
                .iter()
                .any(|(other, recursive)| *recursive && other != path && path.starts_with(other))
        })
        .map(|(path, recursive)| ((*path).clone(), *recursive))
        .collect()
}

fn dispatch(routes: &Mutex<Routes>, event: &notify::Event) {
    // Collect under the lock, call outside it: a handler must never be able to
    // stall registration bookkeeping.
    let handlers: Vec<Handler> = lock(routes)
        .regs
        .values()
        .filter(|reg| event.paths.is_empty() || event.paths.iter().any(|path| reg.covers(path)))
        .map(|reg| reg.handler.clone())
        .collect();
    for handler in handlers {
        handler(event);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Condvar;
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    use notify::RecursiveMode::{NonRecursive, Recursive};

    use super::*;

    fn reg(path: &str, recursive: bool) -> Registration {
        Registration {
            path: PathBuf::from(path),
            recursive,
            handler: Arc::new(|_| {}),
            attached: Arc::default(),
        }
    }

    /// Kind of signal a test handler saw.
    #[derive(Debug, PartialEq)]
    enum Seen {
        Event,
        Rescan,
    }

    fn recorder() -> (
        impl Fn(&notify::Event) + Send + Sync + 'static,
        Receiver<Seen>,
    ) {
        let (tx, rx) = channel();
        let handler = move |event: &notify::Event| {
            let _ = tx.send(if event.need_rescan() {
                Seen::Rescan
            } else {
                Seen::Event
            });
        };
        (handler, rx)
    }

    fn drain(rx: &Receiver<Seen>) -> Vec<Seen> {
        rx.try_iter().collect()
    }

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        (tmp, root)
    }

    fn change(path: &Path) -> notify::Event {
        notify::Event::new(notify::EventKind::Any).add_path(path.to_path_buf())
    }

    /// What a [`FakeOs`] watcher was asked to do, by watcher id.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum Op {
        Open(u64),
        Watch(u64, PathBuf, bool),
        Unwatch(u64, PathBuf),
        Close(u64),
    }

    /// A scripted OS behind the hub's [`Opener`]. Its watchers log every
    /// call, deliver [`FakeOs::emit`]ted events synchronously to the sinks
    /// whose watches cover them (notify's rule: a non-recursive watch sees
    /// its folder and direct children), and model FSEvents' restart gap: an
    /// event queued with [`FakeOs::on_restart`] happens while that watcher
    /// restarts its stream on its next `watch`/`unwatch`, so it misses it and
    /// only the other open watchers (a bridge) can deliver it.
    #[derive(Default)]
    struct FakeOs {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        next_id: u64,
        open: HashMap<u64, FakeStream>,
        log: Vec<Op>,
        in_gap: Vec<(u64, notify::Event)>,
    }

    struct FakeStream {
        sink: Sink,
        paths: HashMap<PathBuf, bool>,
    }

    struct FakeWatcher {
        os: Arc<FakeOs>,
        id: u64,
    }

    impl OsWatcher for FakeWatcher {
        fn watch(&mut self, path: &Path, mode: notify::RecursiveMode) -> notify::Result<()> {
            let recursive = matches!(mode, Recursive);
            {
                let mut state = lock(&self.os.state);
                state
                    .log
                    .push(Op::Watch(self.id, path.to_path_buf(), recursive));
                let stream = state.open.get_mut(&self.id).expect("watcher open");
                stream.paths.insert(path.to_path_buf(), recursive);
            }
            self.os.restart(self.id);
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            {
                let mut state = lock(&self.os.state);
                state.log.push(Op::Unwatch(self.id, path.to_path_buf()));
                let stream = state.open.get_mut(&self.id).expect("watcher open");
                stream.paths.remove(path);
            }
            self.os.restart(self.id);
            Ok(())
        }
    }

    impl Drop for FakeWatcher {
        fn drop(&mut self) {
            let mut state = lock(&self.os.state);
            state.open.remove(&self.id);
            state.log.push(Op::Close(self.id));
        }
    }

    impl FakeOs {
        fn opener(self: &Arc<Self>) -> Opener {
            let os = self.clone();
            Arc::new(move |sink: Sink| -> notify::Result<Box<dyn OsWatcher>> {
                let mut state = lock(&os.state);
                state.next_id += 1;
                let id = state.next_id;
                state.open.insert(
                    id,
                    FakeStream {
                        sink,
                        paths: HashMap::new(),
                    },
                );
                state.log.push(Op::Open(id));
                let watcher: Box<dyn OsWatcher> = Box::new(FakeWatcher { os: os.clone(), id });
                Ok(watcher)
            })
        }

        /// Deliver `event` through every open watcher (but `skip`) whose
        /// watches cover it, on this thread, outside the fake's lock.
        fn deliver(&self, event: &notify::Event, skip: Option<u64>) {
            let sinks: Vec<Sink> = lock(&self.state)
                .open
                .iter()
                .filter(|(id, _)| Some(**id) != skip)
                .filter(|(_, stream)| {
                    event.paths.is_empty()
                        || event.paths.iter().any(|path| {
                            stream.paths.iter().any(|(root, recursive)| {
                                path.starts_with(root)
                                    && (*recursive
                                        || path == root
                                        || path.parent() == Some(root.as_path()))
                            })
                        })
                })
                .map(|(_, stream)| stream.sink.clone())
                .collect();
            for sink in sinks {
                sink(Ok(event.clone()));
            }
        }

        fn emit(&self, path: &Path) {
            self.deliver(&change(path), None);
        }

        /// A change to `path` while watcher `id` next restarts its stream.
        fn on_restart(&self, id: u64, path: &Path) {
            lock(&self.state).in_gap.push((id, change(path)));
        }

        fn restart(&self, id: u64) {
            let gap: Vec<notify::Event> = {
                let mut state = lock(&self.state);
                let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut state.in_gap)
                    .into_iter()
                    .partition(|(watcher, _)| *watcher == id);
                state.in_gap = later;
                now.into_iter().map(|(_, event)| event).collect()
            };
            for event in &gap {
                self.deliver(event, Some(id));
            }
        }

        fn take_log(&self) -> Vec<Op> {
            std::mem::take(&mut lock(&self.state).log)
        }
    }

    /// A [`Pace`] the test steps by hand: the batch worker parks at every
    /// quiet interval until [`Stepper::step`] releases it, so what lands
    /// "during" an interval is decided by the test, not the scheduler.
    #[derive(Default)]
    struct Stepper {
        state: Mutex<StepState>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct StepState {
        parked: bool,
        released: u64,
        consumed: u64,
        idles: u64,
    }

    impl Pace for Stepper {
        fn quiet(&self, _interval: Duration) {
            let mut state = lock(&self.state);
            state.parked = true;
            self.changed.notify_all();
            while state.released == state.consumed {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            state.consumed += 1;
            state.parked = false;
            self.changed.notify_all();
        }

        fn idle(&self) {
            lock(&self.state).idles += 1;
            self.changed.notify_all();
        }
    }

    impl Stepper {
        fn wait_until(&self, done: impl Fn(&StepState) -> bool) {
            let mut state = lock(&self.state);
            while !done(&state) {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }

        /// Wait for the batch worker to park in a quiet interval.
        fn wait_parked(&self) {
            self.wait_until(|state| state.parked);
        }

        /// End the parked worker's quiet interval and wait until it parks in
        /// the next or exits. Returns whether it exited (went idle).
        fn step(&self) -> bool {
            self.wait_parked();
            let idles = {
                let mut state = lock(&self.state);
                state.released += 1;
                self.changed.notify_all();
                state.idles
            };
            self.wait_until(|state| {
                state.consumed == state.released && (state.parked || state.idles > idles)
            });
            lock(&self.state).idles > idles
        }

        /// Step the pending batch worker until it has applied and exited.
        fn run_until_idle(&self) {
            while !self.step() {}
        }
    }

    /// A hub on a [`FakeOs`] with no batch worker (changes wait for
    /// `apply`): FSEvents-like (`shared`: one watcher whose stream restarts
    /// on every change) or inotify-like (a watcher per root, no restarts).
    fn fake_hub(shared: bool) -> (Arc<FsWatchHub>, Arc<FakeOs>) {
        let os = Arc::new(FakeOs::default());
        (FsWatchHub::with(os.opener(), None, shared, shared), os)
    }

    /// An FSEvents-like hub on a [`FakeOs`] whose batch worker the test steps.
    fn stepped_hub() -> (Arc<FsWatchHub>, Arc<FakeOs>, Arc<Stepper>) {
        let os = Arc::new(FakeOs::default());
        let stepper = Arc::new(Stepper::default());
        let pace: Arc<dyn Pace> = stepper.clone();
        (
            FsWatchHub::with(os.opener(), Some(pace), true, true),
            os,
            stepper,
        )
    }

    fn passes(hub: &FsWatchHub) -> usize {
        hub.stats.passes.load(Ordering::Relaxed)
    }

    #[test]
    fn non_recursive_registration_sees_only_direct_children() {
        let flat = reg("/r/space", false);
        assert!(flat.covers(Path::new("/r/space")));
        assert!(flat.covers(Path::new("/r/space/.git")));
        assert!(!flat.covers(Path::new("/r/space/src/main.rs")));
        assert!(!flat.covers(Path::new("/r/spaceship")));
        let deep = reg("/r/space", true);
        assert!(deep.covers(Path::new("/r/space/src/main.rs")));
        assert!(!deep.covers(Path::new("/r/other/x")));
    }

    #[test]
    fn cover_dedupes_merges_modes_and_skips_nested_paths() {
        let wanted: HashMap<PathBuf, bool> = [
            ("/r/main", true),
            ("/r/main/.git/worktrees/wt", true), // inside a recursive watch
            ("/r/flat", false),
            ("/r/flat/sub", true), // a non-recursive parent covers nothing
        ]
        .into_iter()
        .map(|(p, r)| (PathBuf::from(p), r))
        .collect();
        let cover = minimal_cover(&wanted, &HashSet::new());
        let mut paths: Vec<_> = cover.keys().cloned().collect();
        paths.sort();
        assert_eq!(
            paths,
            ["/r/flat", "/r/flat/sub", "/r/main"]
                .map(PathBuf::from)
                .to_vec()
        );
        // A failed ancestor no longer covers its descendants.
        let failed: HashSet<PathBuf> = [PathBuf::from("/r/main")].into();
        assert!(
            minimal_cover(&wanted, &failed).contains_key(Path::new("/r/main/.git/worktrees/wt"))
        );
    }

    #[test]
    fn common_ancestor_of_survivors() {
        let a = PathBuf::from("/u/me/projects/a");
        let b = PathBuf::from("/u/me/projects/b/.git");
        let c = PathBuf::from("/u/me/.onicode/wt");
        assert_eq!(common_ancestor(&[&a]), Some(a.clone()));
        assert_eq!(
            common_ancestor(&[&a, &b]),
            Some(PathBuf::from("/u/me/projects"))
        );
        assert_eq!(common_ancestor(&[&a, &b, &c]), Some(PathBuf::from("/u/me")));
        assert_eq!(common_ancestor(&[]), None);
    }

    #[test]
    fn events_route_by_path_and_pathless_events_broadcast() {
        let routes = Mutex::new(Routes::default());
        let hits = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        for (id, (name, path, recursive)) in [
            ("a", "/r/a", true),
            ("b", "/r/b", true),
            ("flat", "/r", false),
        ]
        .into_iter()
        .enumerate()
        {
            let hits = hits.clone();
            lock(&routes).regs.insert(
                id as u64,
                Registration {
                    path: PathBuf::from(path),
                    recursive,
                    handler: Arc::new(move |_| lock(&hits).push(name)),
                    attached: Arc::default(),
                },
            );
        }
        let event =
            |path: &str| notify::Event::new(notify::EventKind::Any).add_path(PathBuf::from(path));
        dispatch(&routes, &event("/r/a/src/x.rs"));
        assert_eq!(std::mem::take(&mut *lock(&hits)), vec!["a"]);
        dispatch(&routes, &event("/r/b"));
        let mut got = std::mem::take(&mut *lock(&hits));
        got.sort();
        assert_eq!(got, vec!["b", "flat"]); // `/r/b` is a direct child of `/r`
        dispatch(&routes, &notify::Event::new(notify::EventKind::Other));
        assert_eq!(lock(&hits).len(), 3, "a rescan reaches every registration");
    }

    /// Through the batch worker and one shared watcher: two folders, each
    /// event reaches only its own registration, each gets exactly one rescan
    /// when it goes live, a dropped handle stops delivery at once, and its
    /// OS watch goes in the next batch, under a bridge for the survivor.
    #[test]
    fn shared_watcher_routes_events_and_unwatches_on_drop() {
        let (_tmp, root) = tree();
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("deep")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (hub, os, pace) = stepped_hub();

        let (on_a, a_rx) = recorder();
        let (on_b, b_rx) = recorder();
        let watch_a = hub.watch(&a, Recursive, on_a);
        let watch_b = hub.watch(&b, NonRecursive, on_b);
        assert!(!watch_a.is_live(), "live only once the batch applies");
        assert!(!watch_b.is_live(), "live only once the batch applies");
        pace.run_until_idle();
        assert_eq!(drain(&a_rx), [Seen::Rescan]);
        assert_eq!(drain(&b_rx), [Seen::Rescan]);
        assert!(watch_a.is_live() && watch_b.is_live());
        assert_eq!(
            hub.installed(),
            HashMap::from([(a.clone(), true), (b.clone(), false)])
        );
        let log = os.take_log();
        assert_eq!(
            log.first(),
            Some(&Op::Open(1)),
            "one shared watcher: {log:?}"
        );
        assert_eq!(
            log[1..].iter().cloned().collect::<HashSet<_>>(),
            HashSet::from([
                Op::Watch(1, a.clone(), true),
                Op::Watch(1, b.clone(), false)
            ]),
            "{log:?}"
        );

        os.emit(&a.join("deep/x.txt"));
        assert_eq!(drain(&a_rx), [Seen::Event]);
        assert!(drain(&b_rx).is_empty(), "a's event must not reach b");
        os.emit(&b.join("y.txt"));
        assert_eq!(drain(&b_rx), [Seen::Event]);
        assert!(drain(&a_rx).is_empty(), "b's event must not reach a");

        drop(watch_a);
        // The OS still watches `a` until the batch applies; the hub must not
        // route to the dropped handle meanwhile.
        os.emit(&a.join("deep/x.txt"));
        assert!(drain(&a_rx).is_empty(), "a dropped handle receives nothing");
        pace.run_until_idle();
        assert_eq!(hub.installed(), HashMap::from([(b.clone(), false)]));
        assert_eq!(
            os.take_log(),
            [
                Op::Open(2),
                Op::Watch(2, b.clone(), true),
                Op::Unwatch(1, a.clone()),
                Op::Close(2),
            ],
            "dropped path unwatched, the survivor bridged across the restart"
        );
        assert!(drain(&b_rx).is_empty(), "a bridged restart needs no rescan");

        drop(watch_b);
        pace.run_until_idle();
        assert!(hub.installed().is_empty());
    }

    /// A burst of registrations is one pass over the OS watcher (one stream
    /// restart on FSEvents), not one per path; so is a burst of drops.
    #[test]
    fn registration_burst_applies_in_one_pass() {
        let (_tmp, root) = tree();
        let (hub, os, pace) = stepped_hub();
        let watches: Vec<FsWatch> = (0..8)
            .map(|i| {
                let dir = root.join(format!("d{i}"));
                std::fs::create_dir(&dir).unwrap();
                hub.watch(&dir, Recursive, |_| {})
            })
            .collect();
        pace.run_until_idle();
        assert!(watches.iter().all(FsWatch::is_live));
        assert_eq!(hub.installed().len(), 8);
        assert_eq!(passes(&hub), 1);
        let log = os.take_log();
        assert_eq!(log.iter().filter(|op| matches!(op, Op::Open(_))).count(), 1);
        assert_eq!(
            log.iter().filter(|op| matches!(op, Op::Watch(..))).count(),
            8
        );

        drop(watches);
        pace.run_until_idle();
        assert!(hub.installed().is_empty());
        assert_eq!(passes(&hub), 2, "one pass for the drops");
    }

    /// Changes that never go quiet still apply once the worker has waited
    /// out [`BATCH_MAX`] — and not one interval sooner.
    #[test]
    fn a_steady_trickle_applies_at_batch_max() {
        let (_tmp, root) = tree();
        let (hub, _os, pace) = stepped_hub();
        let register = |i: u128| {
            let dir = root.join(format!("d{i}"));
            std::fs::create_dir(&dir).unwrap();
            hub.watch(&dir, Recursive, |_| {})
        };
        let mut watches = vec![register(0)];
        pace.wait_parked();
        let intervals = BATCH_MAX.as_millis() / BATCH_QUIET.as_millis();
        for i in 1..=intervals {
            watches.push(register(i));
            let applied = pace.step();
            assert_eq!(applied, i == intervals, "interval {i} of {intervals}");
            let installed = if applied { watches.len() } else { 0 };
            assert_eq!(hub.installed().len(), installed, "interval {i}");
        }
        assert_eq!(passes(&hub), 1);
        drop(watches);
        pace.run_until_idle();
    }

    /// FSEvents restarts the shared stream on every watch change and misses
    /// what happens meanwhile. With the bridge, a change in that gap still
    /// reaches a surviving registration, which gets no rescan; without it
    /// (bridge unavailable), the change is lost and each surviving
    /// registration gets exactly one rescan per pass, however many restarts.
    #[test]
    fn restart_gap_is_bridged_or_compensated() {
        let (_tmp, root) = tree();
        let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
        for dir in [&a, &b, &c] {
            std::fs::create_dir(dir).unwrap();
        }
        let (hub, os) = fake_hub(true);
        let (on_a, a_rx) = recorder();
        let _watch_a = hub.watch(&a, Recursive, on_a);
        hub.apply();
        assert_eq!(drain(&a_rx), [Seen::Rescan]);
        assert_eq!(os.take_log(), [Op::Open(1), Op::Watch(1, a.clone(), true)]);

        let watch_b = hub.watch(&b, Recursive, |_| {});
        os.on_restart(1, &a.join("x"));
        hub.apply();
        assert!(watch_b.is_live());
        assert_eq!(
            hub.stats.bridges.load(Ordering::Relaxed),
            1,
            "bridge opened"
        );
        assert_eq!(
            os.take_log(),
            [
                Op::Open(2),
                Op::Watch(2, a.clone(), true),
                Op::Watch(1, b.clone(), true),
                Op::Close(2),
            ],
            "the bridge spans the main stream's restart"
        );
        assert_eq!(
            drain(&a_rx),
            [Seen::Event],
            "the bridge delivered the gap's change; no rescan needed"
        );

        hub.bridge_enabled.store(false, Ordering::Relaxed);
        let _watch_c = hub.watch(&c, Recursive, |_| {});
        drop(watch_b);
        os.on_restart(1, &a.join("y"));
        hub.apply();
        assert_eq!(
            os.take_log(),
            [Op::Unwatch(1, b.clone()), Op::Watch(1, c.clone(), true)],
            "two restarts in one pass"
        );
        assert_eq!(
            drain(&a_rx),
            [Seen::Rescan],
            "the gap's change is lost; one compensating rescan per pass"
        );
        assert_eq!(hub.stats.bridges.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn missing_path_is_not_live_and_does_not_block_others() {
        let (_tmp, root) = tree();
        let (hub, os) = fake_hub(true);
        let (on_gone, gone_rx) = recorder();
        let gone = hub.watch(&root.join("gone"), Recursive, on_gone);
        let here = hub.watch(&root, NonRecursive, |_| {});
        hub.apply();
        assert!(!gone.is_live());
        assert!(drain(&gone_rx).is_empty(), "a failed watch never attaches");
        assert!(here.is_live());
        assert_eq!(hub.installed().keys().collect::<Vec<_>>(), vec![&root]);
        assert_eq!(
            os.take_log(),
            [Op::Open(1), Op::Watch(1, root.clone(), false)],
            "a missing path never touches the watcher (each call restarts FSEvents)"
        );
        drop(here);
        drop(gone);
        hub.apply();
        assert!(hub.installed().is_empty());
    }

    #[test]
    fn overlapping_registrations_share_one_install_until_the_last_drops() {
        let (_tmp, root) = tree();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let (hub, _os) = fake_hub(true);
        let flat = hub.watch(&root, NonRecursive, |_| {});
        let nested = hub.watch(&root.join("nested"), Recursive, |_| {});
        let deep = hub.watch(&root, Recursive, |_| {});
        hub.apply();
        // Same path merges to recursive; the nested path rides along inside it.
        assert_eq!(hub.installed(), HashMap::from([(root.clone(), true)]));
        assert!(nested.is_live());
        drop(deep);
        hub.apply();
        assert_eq!(
            hub.installed(),
            HashMap::from([(root.clone(), false), (root.join("nested"), true)])
        );
        drop(flat);
        drop(nested);
        hub.apply();
        assert!(hub.installed().is_empty());
    }

    #[test]
    fn replaced_cover_rescans_the_registration() {
        let (_tmp, root) = tree();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        for shared in [true, false] {
            let (hub, _os) = fake_hub(shared);
            let (on_nested, nested_rx) = recorder();
            let _nested = hub.watch(&root.join("nested"), Recursive, on_nested);
            let deep = hub.watch(&root, Recursive, |_| {});
            hub.apply();
            drain(&nested_rx);
            // `nested` rode inside `root`'s watch; it now gets its own, and
            // nothing observed the swap.
            drop(deep);
            hub.apply();
            assert_eq!(
                hub.installed(),
                HashMap::from([(root.join("nested"), true)])
            );
            assert!(
                drain(&nested_rx).contains(&Seen::Rescan),
                "shared: {shared}"
            );
            // A pass that leaves its cover alone sends nothing.
            let _other = hub.watch(&root, NonRecursive, |_| {});
            hub.apply();
            assert!(
                !drain(&nested_rx).contains(&Seen::Rescan),
                "shared: {shared}"
            );
        }
    }

    #[test]
    fn per_root_watchers_route_events_and_close_on_drop() {
        let (_tmp, root) = tree();
        let (a, b) = (root.join("a"), root.join("b"));
        for dir in [&a, &b] {
            std::fs::create_dir(dir).unwrap();
        }
        let (hub, os) = fake_hub(false);
        let (on_a, a_rx) = recorder();
        let watch_a = hub.watch(&a, Recursive, on_a);
        let (on_b, b_rx) = recorder();
        let _watch_b = hub.watch(&b, Recursive, on_b);
        hub.apply();
        assert_eq!(lock(&hub.os).roots.len(), 2);
        assert!(lock(&hub.os).watcher.is_none());
        assert_eq!(drain(&a_rx), [Seen::Rescan]);
        assert_eq!(drain(&b_rx), [Seen::Rescan]);
        let a_watcher = os
            .take_log()
            .iter()
            .find_map(|op| match op {
                Op::Watch(id, path, true) if *path == a => Some(*id),
                _ => None,
            })
            .expect("a has its own watcher");

        os.emit(&b.join("x"));
        assert_eq!(drain(&b_rx), [Seen::Event]);
        assert!(drain(&a_rx).is_empty());
        drop(watch_a);
        hub.apply();
        assert_eq!(lock(&hub.os).roots.keys().collect::<Vec<_>>(), [&b]);
        assert_eq!(
            os.take_log(),
            [Op::Close(a_watcher)],
            "dropping a root closes its watcher; no unwatch, no other watcher touched"
        );
    }

    /// Smoke: the platform watcher really delivers through the hub, with the
    /// real batch worker. The hub's logic is proven on the fake OS above;
    /// this only checks the wiring, so its bound is generous and it keeps
    /// writing until a change arrives (a fresh FSEvents stream may start a
    /// moment after `watch` returns, and a loaded host delays delivery).
    #[test]
    fn platform_watcher_delivers_changes() {
        let (_tmp, root) = tree();
        let hub = FsWatchHub::new();
        let (on_root, rx) = recorder();
        let watch = hub.watch(&root, Recursive, on_root);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(60)),
            Ok(Seen::Rescan),
            "the watch goes live"
        );
        assert!(watch.is_live());
        let deadline = Instant::now() + Duration::from_secs(60);
        for n in 0_u64.. {
            assert!(Instant::now() < deadline, "no change delivered within 60 s");
            std::fs::write(root.join("x.txt"), n.to_string()).unwrap();
            if matches!(rx.recv_timeout(Duration::from_secs(1)), Ok(Seen::Event)) {
                break;
            }
        }
    }
}
