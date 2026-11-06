//! The Files hub: a [`Topic`] per watched directory, grouped by the root whose file watch they
//! share, and the Files group's calls. Listings and every other disk access run off the
//! caller's thread (on the hub's [`Spawn`]), so a slow disk never stalls the engine's other
//! calls; rescans run on the root's pump thread.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use futures::stream::BoxStream;
use onicode_core::files::{self, DirEntry, document};
use onicode_protocol::files::{
    Bytes, Create, DeletePath, DirEvent, Drive, Entries, FileContents, ImageFile, ListDir, Listing,
    Move, NewPath, NoteImageAsset, PathKind, PathList, PathStat, Read, SaveAttachments,
    SaveNoteImages, SearchContent, SearchHomeFolders, SearchPaths, SearchResult, Stat,
    TextEncoding, TextFile, WatchDir, Write, Written,
};
use onicode_protocol::{Item, OpId, Outcome};

use super::digest;
use crate::feed::{self, Feed, Limits, Topic};
use crate::fs_watch::{Batch, Change, FsWatch, Pump, Schedule};
use crate::ledger::Ledger;
use crate::probe::{DiskProbe, Probe};
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

    fn supersedes(_: &DirEvent, _: &DirEvent) -> bool {
        // Every event is the whole listing.
        true
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

#[derive(Default)]
struct Inner {
    roots: HashMap<String, Root>,
    /// Topics made this run: each takes a fresh epoch.
    topics: u64,
    /// Scans made this run: listings read together share one.
    scans: u64,
}

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
    /// The flag of the search running under each search id: a newer search stops it.
    searches: Mutex<HashMap<String, Arc<AtomicBool>>>,
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
            inner: Mutex::default(),
            searches: Mutex::default(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
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
            None => {
                let probe = self.probe.clone();
                let at = PathBuf::from(&path);
                let listed = self
                    .blocking(move || Ok(entries(probe.list_dir(&at))))
                    .await?;
                self.open(&root, &path, listed, since)
            }
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
        since: Option<onicode_protocol::Cursor>,
    ) -> Option<(u64, Arc<tokio::sync::Notify>)> {
        let mut inner = self.lock();
        let topic = inner.roots.get_mut(root)?.dirs.get_mut(path)?;
        Some(topic.join(since))
    }

    /// Watches `path` under `root`, starting from `listed`, and joins it.
    fn open(
        &self,
        root: &str,
        path: &str,
        listed: Entries,
        since: Option<onicode_protocol::Cursor>,
    ) -> (u64, Arc<tokio::sync::Notify>) {
        let now = onicode_core::time::now_ms();
        let mut inner = self.lock();
        let inner = &mut *inner;
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
        let topics = &mut inner.topics;
        let scans = &mut inner.scans;
        let started = self.started;
        let limits = self.limits;
        let topic = entry.dirs.entry(path.to_owned()).or_insert_with(|| {
            *topics += 1;
            *scans += 1;
            Topic::new(
                started.wrapping_shl(20).wrapping_add(*topics),
                Listing {
                    scan: *scans,
                    entries: listed,
                },
                limits,
            )
        });
        topic.join(since)
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

    /// Reads `dirs` again where they are watched, under any root, and publishes what changed.
    /// Runs on the calling thread: a mutating call's own work thread, before it replies.
    fn relist(&self, dirs: &[String]) {
        let watched: Vec<String> = {
            let inner = self.lock();
            dirs.iter()
                .filter(|dir| {
                    inner
                        .roots
                        .values()
                        .any(|root| root.dirs.contains_key(*dir))
                })
                .cloned()
                .collect()
        };
        if watched.is_empty() {
            return;
        }
        let scan = {
            let mut inner = self.lock();
            inner.scans += 1;
            inner.scans
        };
        for dir in watched {
            let listed = entries(self.probe.list_dir(Path::new(&dir)));
            self.publish(None, &dir, scan, listed);
        }
    }

    /// Publishes `listed` as the listing of `dir` (under `root`, or under every root watching
    /// it), when it differs from the one watchers hold.
    fn publish(&self, root: Option<&str>, dir: &str, scan: u64, listed: Entries) {
        let mut inner = self.lock();
        for (key, watched) in &mut inner.roots {
            if root.is_some_and(|root| root != key) {
                continue;
            }
            if let Some(topic) = watched.dirs.get_mut(dir)
                && topic.state().entries != listed
            {
                topic.publish(vec![DirEvent::Listing(Listing {
                    scan,
                    entries: listed.clone(),
                })]);
            }
        }
    }

    /// A change under `root` at `now`, named as the file watch reports it: paths under the
    /// root's resolved path are renamed under the root as viewers name it.
    pub(crate) fn note(&self, root: &str, change: Change, now: u64) {
        let mut inner = self.lock();
        let Some(watched) = inner.roots.get_mut(root) else {
            return;
        };
        let change = match (change, &watched.canonical) {
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
        watched.schedule.note(now, change);
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
            inner.scans += 1;
            (dirs, inner.scans)
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

    /// `ListDir`.
    pub async fn list_dir(&self, body: ListDir) -> Result<Vec<DirEntry>, String> {
        let probe = self.probe.clone();
        self.blocking(move || probe.list_dir(Path::new(&body.path)))
            .await
    }

    /// `Stat`.
    pub async fn stat(&self, body: Stat) -> Result<PathStat, String> {
        self.blocking(move || Ok(stat(Path::new(&body.path)))).await
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
                    if let Some(hub) = hub.upgrade() {
                        hub.relist(&files::dirs_touched_by_create(&body.parent, &body.name));
                    }
                    Ok(NewPath { path })
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
                    if let Some(hub) = hub.upgrade() {
                        hub.relist(&touched);
                    }
                    Ok(NewPath { path })
                }),
            )
            .await
    }

    /// `DeletePath`.
    pub async fn delete(&self, op: Option<OpId>, body: DeletePath) -> Result<Outcome, String> {
        let hub = self.me.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    files::delete_path(Path::new(&body.path))?;
                    if let Some(hub) = hub.upgrade() {
                        hub.relist(&[files::parent_path(&body.path), body.path.clone()]);
                    }
                    Ok(Outcome::Applied)
                }),
            )
            .await
    }

    /// The flag a search under `id` runs with: a newer one under the same id stops it.
    fn search_flag(&self, id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if !id.is_empty()
            && let Some(older) = self
                .searches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id.to_owned(), flag.clone())
        {
            older.store(true, Ordering::Release);
        }
        flag
    }

    /// Lets go of the search under `id` if `flag` is still its.
    fn search_done(&self, id: &str, flag: &Arc<AtomicBool>) {
        let mut searches = self.searches.lock().unwrap_or_else(PoisonError::into_inner);
        if searches
            .get(id)
            .is_some_and(|known| Arc::ptr_eq(known, flag))
        {
            searches.remove(id);
        }
    }

    /// `SearchPaths`.
    pub async fn search_paths(&self, body: SearchPaths) -> Result<PathList, String> {
        let flag = self.search_flag(&body.search);
        let cancel = flag.clone();
        let root = body.root;
        let listed = self
            .blocking(move || files::list_project_files_sync_cancellable(&root, Some(&*cancel)))
            .await;
        self.search_done(&body.search, &flag);
        Ok(PathList {
            files: if flag.load(Ordering::Acquire) {
                Vec::new()
            } else {
                listed?
            },
        })
    }

    /// `SearchContent`.
    pub async fn search_content(&self, body: SearchContent) -> Result<SearchResult, String> {
        let id = body.options.search_id.clone();
        let flag = self.search_flag(&id);
        let cancel = flag.clone();
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
        self.search_done(&id, &flag);
        if flag.load(Ordering::Acquire) {
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

/// Writes pasted images into the session's attachments folder (or the quick composer's staging
/// folder), under the composer's limits when a session holds them.
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
        None => std::env::temp_dir().join("onicode-quick-composer"),
    };
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let mut created = Vec::new();
    for image in images {
        let path = root.join(format!(
            "pasted-{}.{}",
            onicode_core::new_id(),
            image.extension
        ));
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
