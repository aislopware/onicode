//! The Files hub: a [`Topic`] per watched directory, grouped by the root whose file watch they
//! share, and the Files group's calls. Listings and every other disk access run off the
//! caller's thread (on the hub's [`Spawn`]), so a slow disk never stalls the engine's other
//! calls; rescans run on the root's pump thread.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use futures::stream::BoxStream;
use onicode_core::files::{self, DirEntry, document};
use onicode_protocol::files::{
    AdoptAttachments, Bytes, Create, DeletePath, Deleted, DirEvent, Drive, Entries, FileContents,
    ImageFile, ImportFiles, ListDir, Listing, Move, NewPath, NoteImageAsset, PathKind, PathList,
    PathStat, Read, Relisted, SaveAttachments, SaveNoteImages, SearchContent, SearchHomeFolders,
    SearchPaths, SearchResult, Stat, TextEncoding, TextFile, WatchDir, Write, Written,
};
use onicode_protocol::{Cursor, Item, OpId};

use super::digest;
use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::fs_watch::{Batch, Change, FsWatch, Pump, Schedule};
use crate::ledger::Ledger;
use crate::probe::{DiskProbe, Probe};
use crate::searches::Searches;
use crate::session::Spawn;
use crate::turn::{Database, locked};

/// A directory's listing as a watch's state.
pub(crate) struct DirFeed;

impl Feed for DirFeed {
    type Event = DirEvent;
    type State = Listing;

    fn apply(state: &mut Listing, event: &DirEvent) {
        let DirEvent::Listing(listing) = event;
        state.clone_from(listing);
    }

    fn weight(event: &DirEvent) -> usize {
        let DirEvent::Listing(listing) = event;
        match &listing.entries {
            Entries::Read(entries) => {
                32 + entries
                    .iter()
                    .map(|entry| 48 + entry.name.len() + entry.path.len())
                    .sum::<usize>()
            }
            Entries::Failed(error) => 32 + error.len(),
        }
    }

    fn absorb(pending: &mut Pending<Self>, event: DirEvent) -> bool {
        // Every event is the whole listing.
        pending.replace_or_append(event, |_, _| true)
    }
}

/// A watched root: the directories watched under it, its schedule and its file watch.
struct Root {
    /// The root as the file watch reports it (symlinks resolved), and as viewers name it.
    canonical: Option<String>,
    dirs: HashMap<String, Topic<DirFeed>>,
    schedule: Schedule,
    /// Dropped with the root: its pump thread ends.
    _watch: Option<FsWatch>,
}

/// A directory whose first watcher is reading it: a change to it meanwhile (a call that relists
/// it, or a file watch report under it) means that read may be older than the change.
#[derive(Default)]
struct Opening {
    readers: usize,
    changes: u64,
}

#[derive(Default)]
struct Inner {
    roots: HashMap<String, Root>,
    /// Topics made this run: each takes a fresh epoch.
    topics: u64,
    /// The last scan numbered: listings read together share one, and every read takes its number
    /// before it reads, so a higher number was read later. Numbered from the engine's start, so
    /// a reply the ledger kept from an earlier run never names a scan past this run's.
    scans: u64,
    /// Directories being read for their first watcher.
    opening: HashMap<String, Opening>,
}

impl Inner {
    fn scan(&mut self) -> u64 {
        self.scans += 1;
        self.scans
    }

    /// Marks a read of `dir` for its first watcher as possibly older than a change to it.
    fn changed(&mut self, dir: &str) {
        if let Some(opening) = self.opening.get_mut(dir) {
            opening.changes += 1;
        }
    }
}

/// How many times a first watcher rereads a directory that kept changing while it was read
/// before it takes the last read; the file watch's next report covers the rest.
const OPEN_READS: usize = 3;

pub struct FilesHub {
    store: Database,
    spawn: Arc<dyn Spawn>,
    probe: Arc<dyn Probe>,
    ledger: Ledger,
    limits: Limits,
    /// Start a platform file watch per root; tests drive changes themselves.
    watch_fs: bool,
    /// When this engine run started (Unix ms): topic epochs are numbered from it.
    started: u64,
    me: Weak<Self>,
    inner: Mutex<Inner>,
    /// The searches running, by search id.
    searches: Searches,
}

fn entries(listed: Result<Vec<DirEntry>, String>) -> Entries {
    match listed {
        Ok(entries) => Entries::Read(entries),
        Err(error) => Entries::Failed(error),
    }
}

fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |at| &path[..at])
}

/// Whether a change at one of `batch`'s paths can change the listing of `dir`: a child came,
/// went or was renamed, the directory itself went, or the ignore rules over it changed.
fn touches(batch: &Batch, dir: &str) -> bool {
    batch.all
        || batch.paths.iter().any(|path| {
            let at = parent(path);
            at == dir
                || path == dir
                || (path.ends_with("/.gitignore") && files::is_equal_or_inside(dir, at))
                || path.ends_with("/.git/info/exclude")
        })
}

/// Publishes `listing` unless the topic holds a listing read after it or one that reads the
/// same: a read that finishes after a newer one never shows the older state again.
fn offer(topic: &mut Topic<DirFeed>, listing: Listing) {
    let held = topic.state();
    if listing.scan > held.scan && listing.entries != held.entries {
        topic.publish(vec![DirEvent::Listing(listing)]);
    }
}

/// A first watcher's read of a directory, in [`Inner::opening`] until it ends however it ends.
struct Reading<'a> {
    hub: &'a FilesHub,
    path: String,
}

impl<'a> Reading<'a> {
    fn start(hub: &'a FilesHub, path: &str) -> Self {
        hub.lock()
            .opening
            .entry(path.to_owned())
            .or_default()
            .readers += 1;
        Self {
            hub,
            path: path.to_owned(),
        }
    }
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        let mut inner = self.hub.lock();
        if let Some(opening) = inner.opening.get_mut(&self.path) {
            opening.readers -= 1;
            if opening.readers == 0 {
                inner.opening.remove(&self.path);
            }
        }
    }
}

impl FilesHub {
    /// A hub over the real disk, watching roots with the platform's file watch.
    pub fn new(store: Database, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::build(
            store,
            spawn,
            Arc::new(DiskProbe),
            Limits::default(),
            true,
            onicode_core::time::now_ms(),
        )
    }

    /// A hub over the real disk without platform file watches: listings change only through its
    /// own calls. For an app's tests, which never wait on FSEvents.
    pub fn quiet(store: Database, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::build(
            store,
            spawn,
            Arc::new(DiskProbe),
            Limits::default(),
            false,
            onicode_core::time::now_ms(),
        )
    }

    pub(crate) fn build(
        store: Database,
        spawn: Arc<dyn Spawn>,
        probe: Arc<dyn Probe>,
        limits: Limits,
        watch_fs: bool,
        started: u64,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            ledger: Ledger::new(store.clone()),
            store,
            spawn,
            probe,
            limits,
            watch_fs,
            started,
            me: me.clone(),
            inner: Mutex::new(Inner {
                scans: started.wrapping_shl(20),
                ..Inner::default()
            }),
            searches: Searches::default(),
        })
    }

    /// `StopSearch`: stops the path or content search running under `search`, or the one that
    /// arrives under it next.
    pub fn stop_search(&self, search: &str) {
        self.searches.stop(search);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Numbers a read, as a rescan does before it reads.
    #[cfg(test)]
    pub(super) fn next_scan(&self) -> u64 {
        self.lock().scan()
    }

    /// Runs `work` on the hub's spawner and waits for it.
    async fn blocking<T, W>(&self, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let (reply, answer) = futures::channel::oneshot::channel();
        self.spawn.spawn(Box::pin(async move {
            let _ = reply.send(work());
        }));
        answer
            .await
            .map_err(|_| "The engine stopped before this call finished".to_owned())?
    }

    /// `WatchDir`: the listing, then every new one. After `Resync` the stream ends; resubscribe
    /// with the last cursor.
    pub async fn watch_dir(
        self: &Arc<Self>,
        params: WatchDir,
    ) -> Result<BoxStream<'static, Item<Listing, DirEvent>>, String> {
        let WatchDir { root, path, since } = params;
        let joined = self.join(&root, &path, since);
        let (id, notify) = match joined {
            Some(joined) => joined,
            None => self.first_watch(&root, &path, since).await?,
        };
        let (draining, leaving) = (self.clone(), self.clone());
        let (drain_root, drain_path) = (root.clone(), path.clone());
        Ok(feed::stream(
            notify,
            move || draining.drain(&drain_root, &drain_path, id),
            move || leaving.leave(&root, &path, id),
        ))
    }

    /// Joins the topic of `path` under `root` if it is watched already.
    fn join(
        &self,
        root: &str,
        path: &str,
        since: Option<Cursor>,
    ) -> Option<(u64, Arc<tokio::sync::Notify>)> {
        let mut inner = self.lock();
        let topic = inner.roots.get_mut(root)?.dirs.get_mut(path)?;
        Some(topic.join(since))
    }

    /// The first watcher of `path` under `root`: reads it, and reads it again when a call or the
    /// file watch changed it during the read (a call relists only what is watched, so the change
    /// would otherwise wait for the next report), then watches it from the last read.
    async fn first_watch(
        &self,
        root: &str,
        path: &str,
        since: Option<Cursor>,
    ) -> Result<(u64, Arc<tokio::sync::Notify>), String> {
        let _reading = Reading::start(self, path);
        let mut reads = 0;
        loop {
            let (scan, changes) = {
                let mut inner = self.lock();
                let changes = inner.opening.get(path).map_or(0, |opening| opening.changes);
                (inner.scan(), changes)
            };
            let probe = self.probe.clone();
            let at = PathBuf::from(path);
            let listed = self
                .blocking(move || Ok(entries(probe.list_dir(&at))))
                .await?;
            reads += 1;
            let listing = Listing {
                scan,
                entries: listed,
            };
            if let Some(joined) = self.open(root, path, listing, since, changes, reads) {
                return Ok(joined);
            }
        }
    }

    /// Watches `path` under `root` from `listing` (read while its changes stood at `changes`) and
    /// joins it; `None` when it changed since and is worth reading again. A watch another first
    /// watcher opened meanwhile is joined, taking this read if it is the newer.
    fn open(
        &self,
        root: &str,
        path: &str,
        listing: Listing,
        since: Option<Cursor>,
        changes: u64,
        reads: usize,
    ) -> Option<(u64, Arc<tokio::sync::Notify>)> {
        let now = onicode_core::time::now_ms();
        let mut inner = self.lock();
        let inner = &mut *inner;
        let changed = inner
            .opening
            .get(path)
            .is_some_and(|opening| opening.changes != changes);
        if changed && reads < OPEN_READS {
            return None;
        }
        let watch_fs = self.watch_fs;
        let pump: Weak<dyn Pump> = self.me.clone();
        let entry = inner.roots.entry(root.to_owned()).or_insert_with(|| {
            let (watch, watched) = if watch_fs {
                let (watch, watched) =
                    FsWatch::start(root.to_owned(), &[PathBuf::from(root)], pump);
                (Some(watch), watched)
            } else {
                (None, true)
            };
            Root {
                canonical: std::fs::canonicalize(root)
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned()),
                dirs: HashMap::new(),
                schedule: Schedule::new(now, watched),
                _watch: watch,
            }
        });
        let topic = match entry.dirs.entry(path.to_owned()) {
            std::collections::hash_map::Entry::Occupied(held) => {
                let topic = held.into_mut();
                offer(topic, listing);
                topic
            }
            std::collections::hash_map::Entry::Vacant(free) => {
                inner.topics += 1;
                free.insert(Topic::new(
                    self.started.wrapping_shl(20).wrapping_add(inner.topics),
                    listing,
                    self.limits,
                ))
            }
        };
        Some(topic.join(since))
    }

    fn drain(&self, root: &str, path: &str, id: u64) -> Option<Option<Item<Listing, DirEvent>>> {
        self.lock()
            .roots
            .get_mut(root)?
            .dirs
            .get_mut(path)?
            .drain(id)
    }

    /// The watcher left; a directory nobody watches is let go, and so is a root with none.
    fn leave(&self, root: &str, path: &str, id: u64) {
        let mut inner = self.lock();
        let Some(watched) = inner.roots.get_mut(root) else {
            return;
        };
        if watched
            .dirs
            .get_mut(path)
            .is_some_and(|topic| topic.leave(id))
        {
            watched.dirs.remove(path);
        }
        if watched.dirs.is_empty() {
            inner.roots.remove(root);
        }
    }

    /// Reads `dirs` again where they are watched, under any root, and publishes what changed:
    /// the listings that cover a change a call just made. Runs on the calling thread: a mutating
    /// call's own work thread, before it replies.
    pub(super) fn relist(&self, dirs: &[String]) -> Vec<Relisted> {
        let (watched, scan) = {
            let mut inner = self.lock();
            for dir in dirs {
                inner.changed(dir);
            }
            let mut watched: Vec<String> = dirs
                .iter()
                .filter(|dir| {
                    inner
                        .roots
                        .values()
                        .any(|root| root.dirs.contains_key(*dir))
                })
                .cloned()
                .collect();
            if watched.is_empty() {
                return Vec::new();
            }
            watched.sort_unstable();
            watched.dedup();
            // Numbered after the change and before the reads: it orders after every read that
            // could have missed the change.
            (watched, inner.scan())
        };
        let mut covered = Vec::new();
        for dir in watched {
            let listed = entries(self.probe.list_dir(Path::new(&dir)));
            covered.extend(self.publish(None, &dir, scan, listed));
        }
        covered
    }

    /// Offers `listed`, read at `scan`, as the listing of `dir` (under `root`, or under every root
    /// watching it); the listing each watching root holds after it covers the read.
    pub(super) fn publish(
        &self,
        root: Option<&str>,
        dir: &str,
        scan: u64,
        listed: Entries,
    ) -> Vec<Relisted> {
        let mut inner = self.lock();
        let mut covered = Vec::new();
        for (key, watched) in &mut inner.roots {
            if root.is_some_and(|root| root != key) {
                continue;
            }
            if let Some(topic) = watched.dirs.get_mut(dir) {
                offer(
                    topic,
                    Listing {
                        scan,
                        entries: listed.clone(),
                    },
                );
                covered.push(Relisted {
                    root: key.clone(),
                    path: dir.to_owned(),
                    scan: topic.state().scan,
                });
            }
        }
        covered
    }

    /// A change under `root` at `now`, named as the file watch reports it: paths under the
    /// root's resolved path are renamed under the root as viewers name it.
    pub(crate) fn note(&self, root: &str, change: Change, now: u64) {
        let mut inner = self.lock();
        let Some(canonical) = inner
            .roots
            .get(root)
            .map(|watched| watched.canonical.clone())
        else {
            return;
        };
        let change = match (change, canonical) {
            (Change::Paths(paths), Some(canonical)) if canonical != root => Change::Paths(
                paths
                    .into_iter()
                    .map(|path| match path.strip_prefix(canonical.as_str()) {
                        Some(rest) => format!("{root}{rest}"),
                        None => path,
                    })
                    .collect(),
            ),
            (change, _) => change,
        };
        // A directory being read for its first watcher reads again.
        match &change {
            Change::Paths(paths) => {
                for path in paths {
                    inner.changed(parent(path));
                    inner.changed(path);
                }
            }
            Change::Overflow => {
                for opening in inner.opening.values_mut() {
                    opening.changes += 1;
                }
            }
        }
        if let Some(watched) = inner.roots.get_mut(root) {
            watched.schedule.note(now, change);
        }
    }

    /// Rereads what is due under `root` at `now`: the directories a settled burst touched, or
    /// every one on the periodic repair.
    pub(crate) fn rescan(&self, root: &str, now: u64) {
        let (dirs, scan) = {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let Some(watched) = inner.roots.get_mut(root) else {
                return;
            };
            let Some((batch, _)) = watched.schedule.take(now) else {
                return;
            };
            let dirs: Vec<String> = watched
                .dirs
                .keys()
                .filter(|dir| touches(&batch, dir))
                .cloned()
                .collect();
            (dirs, inner.scan())
        };
        for dir in dirs {
            let listed = entries(self.probe.list_dir(Path::new(&dir)));
            self.publish(Some(root), &dir, scan, listed);
        }
    }

    /// When `root` next has something to rescan.
    pub(crate) fn next_due(&self, root: &str) -> Option<u64> {
        self.lock()
            .roots
            .get(root)
            .map(|watched| watched.schedule.due())
    }

    /// `ListDir`. A path of `~`, or under `~/`, is read under this engine's home: a viewer on
    /// another machine starts "Open folder on a machine" there without knowing where it is.
    pub async fn list_dir(&self, body: ListDir) -> Result<Vec<DirEntry>, String> {
        let probe = self.probe.clone();
        self.blocking(move || probe.list_dir(&home_relative(&body.path)))
            .await
    }

    /// `Stat`. A path of `~`, or under `~/`, is read under this engine's home, as `Read` and
    /// `Write` read it: a viewer on another machine names a file a transcript spells under `~`
    /// without knowing where this engine's home is.
    pub async fn stat(&self, body: Stat) -> Result<PathStat, String> {
        self.blocking(move || Ok(stat(&home_relative(&body.path))))
            .await
    }

    /// `Read`.
    pub async fn read(&self, body: Read) -> Result<FileContents, String> {
        self.blocking(move || read(Path::new(&body.path), body.unless))
            .await
    }

    /// `Write`.
    pub async fn write(&self, op: Option<OpId>, body: Write) -> Result<Written, String> {
        self.ledger
            .once(op, self.blocking(move || write(&body)))
            .await
    }

    /// `Create`.
    pub async fn create(&self, op: Option<OpId>, body: Create) -> Result<NewPath, String> {
        let hub = self.me.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    let path = files::create_path(Path::new(&body.parent), &body.name, body.dir)?;
                    let listed = hub.upgrade().map_or_else(Vec::new, |hub| {
                        hub.relist(&files::dirs_touched_by_create(&body.parent, &body.name))
                    });
                    Ok(NewPath { path, listed })
                }),
            )
            .await
    }

    /// `Move`: a rename, a move into another folder, or a copy into one.
    pub async fn relocate(&self, op: Option<OpId>, body: Move) -> Result<NewPath, String> {
        use onicode_protocol::files::Destination;
        let hub = self.me.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    let from = Path::new(&body.from);
                    let (path, touched) = match &body.to {
                        Destination::Rename(name) => {
                            let path = files::rename_path(from, name)?;
                            let mut touched = files::dirs_touched_by_create(
                                &files::parent_path(&body.from),
                                name,
                            );
                            touched.extend(files::dirs_touched_by_move(&body.from, &path));
                            (path, touched)
                        }
                        Destination::Into(folder) => {
                            let path = files::move_path(from, Path::new(folder))?;
                            let touched = files::dirs_touched_by_move(&body.from, &path);
                            (path, touched)
                        }
                        Destination::CopyInto(folder) => {
                            let path = files::copy_path(from, Path::new(folder))?;
                            (path, vec![folder.clone()])
                        }
                    };
                    let listed = hub
                        .upgrade()
                        .map_or_else(Vec::new, |hub| hub.relist(&touched));
                    Ok(NewPath { path, listed })
                }),
            )
            .await
    }

    /// `ImportFiles`: a file or folder the viewer copies in from its own disk.
    pub async fn import(&self, op: Option<OpId>, body: ImportFiles) -> Result<NewPath, String> {
        let hub = self.me.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    let path = import(&body)?;
                    let listed = hub
                        .upgrade()
                        .map_or_else(Vec::new, |hub| hub.relist(std::slice::from_ref(&body.into)));
                    Ok(NewPath { path, listed })
                }),
            )
            .await
    }

    /// `DeletePath`.
    pub async fn delete(&self, op: Option<OpId>, body: DeletePath) -> Result<Deleted, String> {
        let hub = self.me.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    files::delete_path(Path::new(&body.path))?;
                    let listed = hub.upgrade().map_or_else(Vec::new, |hub| {
                        hub.relist(&[files::parent_path(&body.path), body.path.clone()])
                    });
                    Ok(Deleted { listed })
                }),
            )
            .await
    }

    /// `SearchPaths`.
    pub async fn search_paths(&self, body: SearchPaths) -> Result<PathList, String> {
        let running = self.searches.start(&body.search);
        let cancel = running.flag();
        let root = body.root;
        let listed = self
            .blocking(move || files::list_project_files_sync_cancellable(&root, Some(&*cancel)))
            .await;
        Ok(PathList {
            files: if running.stopped() {
                Vec::new()
            } else {
                listed?
            },
        })
    }

    /// `SearchContent`.
    pub async fn search_content(&self, body: SearchContent) -> Result<SearchResult, String> {
        let running = self.searches.start(&body.options.search_id);
        let cancel = running.flag();
        let options = body.options;
        let found = self
            .blocking(move || {
                onicode_core::search::project::search_project(
                    Path::new(&options.cwd),
                    &options,
                    &cancel,
                )
            })
            .await;
        if running.stopped() {
            return Ok(SearchResult {
                matches: Vec::new(),
                truncated: false,
            });
        }
        found
    }

    /// `ListDrives`.
    pub async fn list_drives(&self) -> Result<Vec<Drive>, String> {
        self.blocking(|| Ok(drives())).await
    }

    /// `SearchHomeFolders`.
    pub async fn search_home_folders(
        &self,
        body: SearchHomeFolders,
    ) -> Result<Vec<String>, String> {
        self.blocking(move || Ok(home_folders(&body.query, body.limit)))
            .await
    }

    /// `SaveAttachments`.
    pub async fn save_attachments(
        &self,
        op: Option<OpId>,
        body: SaveAttachments,
    ) -> Result<Vec<String>, String> {
        let store = self.store.clone();
        self.ledger
            .once(op, self.blocking(move || save_attachments(&store, body)))
            .await
    }

    /// `AdoptAttachments`.
    pub async fn adopt_attachments(
        &self,
        op: Option<OpId>,
        body: AdoptAttachments,
    ) -> Result<Vec<String>, String> {
        let store = self.store.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    let strings = |paths: Vec<String>| -> Vec<PathBuf> {
                        paths.into_iter().map(PathBuf::from).collect()
                    };
                    let adopted = locked(&store)?.adopt_attachments(
                        body.session,
                        &std::env::temp_dir().join(QUICK_COMPOSER_STAGING),
                        strings(body.paths),
                        &strings(body.staged),
                    )?;
                    Ok(adopted
                        .into_iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect())
                }),
            )
            .await
    }

    /// `SaveNoteImages`.
    pub async fn save_note_images(
        &self,
        op: Option<OpId>,
        body: SaveNoteImages,
    ) -> Result<Vec<NoteImageAsset>, String> {
        let store = self.store.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    let store = locked(&store)?;
                    let mut images = Vec::new();
                    let mut failure = None;
                    for path in &body.paths {
                        match store.save_note_image(&body.note, Path::new(path)) {
                            Ok(image) => images.push(image),
                            Err(error) => failure = Some(error),
                        }
                    }
                    if images.is_empty() {
                        return Err(failure.unwrap_or_else(|| {
                            "Drop a PNG, JPG, GIF, WebP, or SVG image.".into()
                        }));
                    }
                    Ok(images)
                }),
            )
            .await
    }
}

impl Pump for FilesHub {
    fn changed(&self, key: &str, change: Change, now: u64) {
        self.note(key, change, now);
    }

    fn due(&self, key: &str) -> Option<u64> {
        self.next_due(key)
    }

    fn beat(&self, key: &str, now: u64) {
        self.rescan(key, now);
    }
}

fn unix_ms(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
}

fn stat(path: &Path) -> PathStat {
    let Ok(meta) = std::fs::metadata(path) else {
        return PathStat::default();
    };
    let kind = if meta.is_file() {
        PathKind::File
    } else if meta.is_dir() {
        PathKind::Dir
    } else {
        PathKind::Other
    };
    PathStat {
        kind,
        size: meta.len(),
        modified_ms: meta.modified().ok().and_then(unix_ms),
    }
}

/// The editor's read of `path`, under its caps; [`FileContents::Unchanged`] when the file's
/// digest is still `unless`.
fn read(path: &Path, unless: Option<u64>) -> Result<FileContents, String> {
    Ok(match document::read_document(path)? {
        document::Document::Text(text) => {
            let digest = digest(&text.disk);
            if unless == Some(digest) {
                return Ok(FileContents::Unchanged);
            }
            FileContents::Text(TextFile {
                text: text.text,
                encoding: TextEncoding {
                    crlf: text.crlf,
                    cr: text.cr,
                    bom: text.bom,
                },
                digest,
            })
        }
        document::Document::Image {
            bytes,
            format,
            width,
            height,
            original_size,
        } => {
            let digest = digest(&bytes);
            if unless == Some(digest) {
                return Ok(FileContents::Unchanged);
            }
            FileContents::Image(ImageFile {
                bytes: Bytes(bytes),
                format: format.to_owned(),
                width,
                height,
                original_size: original_size as u64,
                digest,
            })
        }
        document::Document::Video { size, modified, .. } => FileContents::Video {
            size,
            modified_ms: modified.and_then(unix_ms),
        },
        document::Document::Unsupported(reason) => FileContents::Unsupported(reason),
    })
}

/// Writes `body`'s item into its folder under its name, or the name Finder gives a copy when
/// that is taken, each folder before what it holds. A copy that fails partway is removed whole.
fn import(body: &ImportFiles) -> Result<String, String> {
    let into = Path::new(&body.into);
    if !into.is_dir() {
        return Err(format!("{} is not a folder", into.display()));
    }
    let leaf = Path::new(&body.name);
    if !matches!(
        leaf.components().collect::<Vec<_>>().as_slice(),
        [std::path::Component::Normal(_)]
    ) {
        return Err(format!("{} is not a file name", body.name));
    }
    if body.entries.is_empty() {
        return Err("Nothing to copy".into());
    }
    let name = files::unique_name(&body.name, |taken| into.join(taken).exists());
    let root = into.join(name);
    let written = body.entries.iter().try_for_each(|entry| {
        let relative = Path::new(&entry.path);
        if !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(format!("{} is outside the copied folder", entry.path));
        }
        let at = root.join(relative);
        let made = match &entry.bytes {
            None => std::fs::create_dir_all(&at),
            Some(bytes) => at
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&at, &bytes.0)),
        };
        made.map_err(|error| format!("{}: {error}", at.display()))
    });
    if let Err(error) = written {
        let _ = if root.is_dir() {
            std::fs::remove_dir_all(&root)
        } else {
            std::fs::remove_file(&root)
        };
        return Err(error);
    }
    Ok(root.to_string_lossy().into_owned())
}

/// The editor's save: refused when the file on disk is no longer the one read as
/// `body.expected`.
fn write(body: &Write) -> Result<Written, String> {
    const CHANGED: &str = "The file changed on disk. Reload it before saving your edits.";
    let path = Path::new(&body.path);
    let document::Document::Text(on_disk) = document::read_document(path)? else {
        return Err(CHANGED.into());
    };
    if digest(&on_disk.disk) != body.expected {
        return Err(CHANGED.into());
    }
    let TextEncoding { crlf, cr, bom } = body.encoding;
    let read = document::TextDocument {
        text: String::new(),
        crlf,
        cr,
        bom,
        disk: on_disk.disk,
    };
    let bytes = document::save_document(path, &read, &body.text)?;
    Ok(Written {
        digest: digest(&bytes),
    })
}

/// The root and every mounted volume; the boot volume, a link to the root, is the root.
fn drives() -> Vec<Drive> {
    let mut drives = vec![Drive {
        name: "/".into(),
        path: "/".into(),
    }];
    if let Ok(volumes) = std::fs::read_dir("/Volumes") {
        for volume in volumes.flatten() {
            let name = volume.file_name().to_string_lossy().into_owned();
            let path = volume.path();
            let is_link = volume.file_type().is_ok_and(|kind| kind.is_symlink());
            if is_link {
                continue;
            }
            if path.is_dir() {
                drives.push(Drive {
                    name,
                    path: path.to_string_lossy().into_owned(),
                });
            }
        }
    }
    drives
}

/// Folders under home whose name holds `query`, breadth first, skipping hidden folders,
/// `Library` and dependency folders, within a bounded walk.
/// `path`, a leading `~` read as this engine's home.
fn home_relative(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home,
        (Some(rest), Some(home)) => match rest.strip_prefix('/') {
            Some(under) => home.join(under),
            None => PathBuf::from(path),
        },
        _ => PathBuf::from(path),
    }
}

fn home_folders(query: &str, limit: u32) -> Vec<String> {
    const MAX_DEPTH: usize = 4;
    const MAX_VISITS: usize = 20_000;
    const SKIP: &[&str] = &["Library", "node_modules", "target", "DerivedData"];
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let needle = query.trim().to_lowercase();
    let limit = usize::try_from(limit.clamp(1, 200)).unwrap_or(200);
    let mut found = Vec::new();
    let mut frontier = vec![home];
    let mut visits = 0;
    for _ in 0..MAX_DEPTH {
        let mut next = Vec::new();
        for dir in frontier {
            let Ok(children) = std::fs::read_dir(&dir) else {
                continue;
            };
            for child in children.flatten() {
                visits += 1;
                if visits > MAX_VISITS || found.len() >= limit {
                    return found;
                }
                if !child.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let name = child.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || SKIP.contains(&name.as_str()) {
                    continue;
                }
                if name.to_lowercase().contains(&needle) {
                    found.push(child.path().to_string_lossy().into_owned());
                }
                next.push(child.path());
            }
        }
        frontier = next;
    }
    found
}

/// The quick composer's staging folder, under the system's temporary one: pasted images wait
/// there for the session a launch creates ([`AdoptAttachments`]).
const QUICK_COMPOSER_STAGING: &str = "onicode-quick-composer";

/// Writes pasted images and the viewer's files into the session's attachments folder (or the
/// quick composer's staging folder), under the composer's limits when a session holds them. A
/// named file keeps its name, in a folder of its own; a name that is no file name is refused.
fn save_attachments(store: &Database, body: SaveAttachments) -> Result<Vec<String>, String> {
    use onicode_core::composer::{MAX_ATTACHMENTS, MAX_EMBED_BYTES, validate_attachments};
    let SaveAttachments {
        session,
        kept,
        images,
    } = body;
    let root = match session {
        Some(id) => {
            if kept.len() + images.len() > MAX_ATTACHMENTS
                || images
                    .iter()
                    .map(|image| image.bytes.0.len() as u64)
                    .sum::<u64>()
                    > MAX_EMBED_BYTES
            {
                return Err("Attach up to 20 files totalling 20 MB.".to_owned());
            }
            // Kept with the conversation so deleting it removes them.
            locked(store)?
                .thread_attachments_dir(id)?
                .unwrap_or_else(|| std::env::temp_dir().join("onicode-attachments"))
        }
        None => std::env::temp_dir().join(QUICK_COMPOSER_STAGING),
    };
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let mut created = Vec::new();
    for image in images {
        let path = match &image.name {
            Some(name) => {
                let file = Path::new(name)
                    .file_name()
                    .filter(|file| file.len() == name.len())
                    .ok_or_else(|| format!("{name} is not a file name."))?;
                let folder = root.join(onicode_core::new_id());
                std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
                folder.join(file)
            }
            None => root.join(format!(
                "pasted-{}.{}",
                onicode_core::new_id(),
                image.extension
            )),
        };
        std::fs::write(&path, &image.bytes.0).map_err(|error| error.to_string())?;
        created.push(path);
    }
    if session.is_some() {
        let all: Vec<PathBuf> = kept
            .iter()
            .map(PathBuf::from)
            .chain(created.iter().cloned())
            .collect();
        validate_attachments(&all)?;
    }
    Ok(created
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}
