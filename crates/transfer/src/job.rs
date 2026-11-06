//! A job as it is kept: what it copies, what is decided, what is on disk.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use onicode_proto::files::{
    ConflictChoice, ConflictPolicy, TransferConflict, TransferFrom, TransferState, TransferTo,
    TransferUpdate,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::manifest::{Entry, Kind};
use crate::sink::Partial;
use crate::spans::Spans;
use crate::{PARTIAL_PREFIX, Tuning, wire};

/// What becomes of one requested path at the destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "do")]
pub enum Decision {
    /// It lands as `name`, replacing what is there if `replace`.
    Place {
        /// Its name in the destination folder.
        name: String,
        /// Whether something there goes first.
        replace: bool,
    },
    /// It is left out.
    Skip,
}

/// One requested path: a top-level item of the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    /// Its name in the manifest.
    pub name: String,
    /// What becomes of it, once decided.
    #[serde(default)]
    pub decision: Option<Decision>,
    /// Whether it is in place.
    #[serde(default)]
    pub placed: bool,
}

/// A job, as persisted. The manifest is kept beside it ([`crate::store`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Its id.
    pub id: String,
    /// The source.
    pub from: TransferFrom,
    /// The destination.
    pub to: TransferTo,
    /// What a taken name does from now on.
    pub policy: ConflictPolicy,
    /// Where it stands.
    pub state: TransferState,
    /// The requested paths, in manifest order, once the manifest is in.
    #[serde(default)]
    pub roots: Vec<Root>,
    /// Items finished in the staging folder (or placed).
    #[serde(default)]
    pub done: Spans,
    /// Blocks on disk of files not yet finished.
    #[serde(default)]
    pub partial: BTreeMap<u32, Spans>,
    /// Why it failed, or the last error it is retrying past.
    #[serde(default)]
    pub error: Option<String>,
    /// The conflict it waits on.
    #[serde(default)]
    pub conflict: Option<TransferConflict>,
    /// Its numbers.
    pub tuning: Tuning,
    /// When it was started, epoch milliseconds.
    #[serde(default)]
    pub created_ms: i64,
    /// The manifest, persisted apart.
    #[serde(skip)]
    pub manifest: Arc<Vec<Entry>>,
}

impl Job {
    /// A new job, queued.
    #[must_use]
    pub fn new(
        id: String,
        from: TransferFrom,
        to: TransferTo,
        policy: ConflictPolicy,
        tuning: Tuning,
    ) -> Self {
        Self {
            id,
            from,
            to,
            policy,
            state: TransferState::Queued,
            roots: Vec::new(),
            done: Spans::default(),
            partial: BTreeMap::new(),
            error: None,
            conflict: None,
            tuning,
            created_ms: crate::millis(std::time::SystemTime::now()),
            manifest: Arc::new(Vec::new()),
        }
    }

    /// The staging folder: `.onicode-partial-<id>` in the destination folder.
    #[must_use]
    pub fn staging(&self) -> PathBuf {
        PathBuf::from(&self.to.dir).join(format!("{PARTIAL_PREFIX}{}", self.id))
    }

    /// Takes `entries` as the manifest, keeping the progress of items unchanged since the last
    /// one (same path, kind, size, mtime and target) and dropping the rest.
    pub fn adopt(&mut self, entries: Vec<Entry>) {
        let old = Arc::clone(&self.manifest);
        let by_rel: HashMap<&str, &Entry> = old.iter().map(|e| (e.rel.as_str(), e)).collect();
        let mut done = Spans::default();
        let mut partial = BTreeMap::new();
        for entry in &entries {
            let Some(before) = by_rel.get(entry.rel.as_str()) else {
                continue;
            };
            let same = before.kind == entry.kind
                && before.size == entry.size
                && before.mtime_ms == entry.mtime_ms
                && before.target == entry.target;
            if !same {
                continue;
            }
            if self.done.contains(u64::from(before.index)) {
                done.insert(u64::from(entry.index));
            } else if let Some(blocks) = self.partial.get(&before.index) {
                partial.insert(entry.index, blocks.clone());
            }
        }
        let mut names: Vec<String> = Vec::new();
        for entry in &entries {
            let root = entry.root();
            if !names.iter().any(|n| n == root) {
                names.push(root.to_owned());
            }
        }
        let mut roots: Vec<Root> = names
            .into_iter()
            .map(|name| {
                self.roots
                    .iter()
                    .find(|r| r.name == name)
                    .cloned()
                    .unwrap_or(Root {
                        name,
                        decision: None,
                        placed: false,
                    })
            })
            .collect();
        // A root placed stays placed: its items count as done whatever the source did since.
        for root in roots.iter_mut().filter(|r| r.placed) {
            for entry in entries.iter().filter(|e| e.root() == root.name) {
                done.insert(u64::from(entry.index));
            }
        }
        self.done = done;
        self.partial = partial;
        self.roots = roots;
        self.manifest = Arc::new(entries);
    }

    /// The decision for the root named `name`.
    #[must_use]
    pub fn decision(&self, name: &str) -> Option<&Decision> {
        self.roots
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| r.decision.as_ref())
    }

    /// Whether `entry` is to be copied: its root is placed somewhere and not yet in place.
    #[must_use]
    pub fn active(&self, entry: &Entry) -> bool {
        self.roots.iter().any(|r| {
            r.name == entry.root()
                && !r.placed
                && matches!(r.decision, Some(Decision::Place { .. }))
        })
    }

    /// File bytes in the job, and those done.
    #[must_use]
    pub fn bytes(&self) -> (u64, u64) {
        let block = self.tuning.block;
        let mut total = 0_u64;
        let mut done = 0_u64;
        for entry in self.manifest.iter().filter(|e| e.kind == Kind::File) {
            if self.skipped(entry) {
                continue;
            }
            total = total.saturating_add(entry.size);
            if self.done.contains(u64::from(entry.index)) {
                done = done.saturating_add(entry.size);
            } else if let Some(blocks) = self.partial.get(&entry.index) {
                for &(start, end) in blocks.spans() {
                    let from = start.saturating_mul(block);
                    let to = end.saturating_mul(block).min(entry.size);
                    done = done.saturating_add(to.saturating_sub(from));
                }
            }
        }
        (total, done)
    }

    /// Whether `entry`'s root is skipped.
    #[must_use]
    pub fn skipped(&self, entry: &Entry) -> bool {
        matches!(self.decision(entry.root()), Some(Decision::Skip))
    }

    /// Items in the job (skipped ones left out), and those done.
    #[must_use]
    pub fn items(&self) -> (u32, u32) {
        let mut total = 0_u32;
        let mut done = 0_u32;
        for entry in self.manifest.iter() {
            if self.skipped(entry) {
                continue;
            }
            total = total.saturating_add(1);
            if self.done.contains(u64::from(entry.index)) {
                done = done.saturating_add(1);
            }
        }
        (total, done)
    }
}

/// A live job: the persisted [`Job`] and what its run shares.
pub struct Cell {
    job: Mutex<Job>,
    /// File bytes written and checked, kept current as blocks land.
    pub bytes_done: AtomicU64,
    /// The item last started.
    pub current: Mutex<Option<String>>,
    /// The owner's answer to the conflict asked, until the run takes it.
    pub answer: Mutex<Option<(ConflictChoice, bool)>>,
    /// Told when [`Self::answer`] is set.
    pub answered: Notify,
    /// Open partial files of the run.
    pub files: Mutex<HashMap<u32, Arc<dyn Partial>>>,
    /// The rate, bytes per second, and the bytes it was last measured at.
    pub rate: Mutex<(u64, u64)>,
    /// The last update sent.
    pub sent: Mutex<Option<TransferUpdate>>,
    /// The run's task.
    pub task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Whether the job changed since it was last persisted.
    pub dirty: std::sync::atomic::AtomicBool,
}

impl Cell {
    /// A cell for `job`.
    #[must_use]
    pub fn new(job: Job) -> Self {
        let (_, done) = job.bytes();
        Self {
            job: Mutex::new(job),
            bytes_done: AtomicU64::new(done),
            current: Mutex::new(None),
            answer: Mutex::new(None),
            answered: Notify::new(),
            files: Mutex::new(HashMap::new()),
            rate: Mutex::new((0, done)),
            sent: Mutex::new(None),
            task: Mutex::new(None),
            dirty: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// The job, locked. Never held across an await.
    pub fn job(&self) -> MutexGuard<'_, Job> {
        self.dirty.store(true, Ordering::Relaxed);
        lock(&self.job)
    }

    /// A copy of the job.
    #[must_use]
    pub fn snapshot(&self) -> Job {
        lock(&self.job).clone()
    }

    /// The job's id.
    #[must_use]
    pub fn id(&self) -> String {
        lock(&self.job).id.clone()
    }

    /// The job's state.
    #[must_use]
    pub fn state(&self) -> TransferState {
        lock(&self.job).state
    }

    /// Sets the state, and the error with it.
    pub fn set_state(&self, state: TransferState, error: Option<String>) {
        let mut job = self.job();
        job.state = state;
        job.error = error;
        if state != TransferState::Conflict {
            job.conflict = None;
        }
    }

    /// Counts the bytes done from the job's progress again.
    pub fn recount(&self) {
        let (_, done) = lock(&self.job).bytes();
        self.bytes_done.store(done, Ordering::Relaxed);
    }

    /// The update the job reports now.
    #[must_use]
    pub fn update(&self) -> TransferUpdate {
        let job = lock(&self.job);
        let (bytes, _) = job.bytes();
        let (items, items_done) = job.items();
        let state = job.state;
        let running = state == TransferState::Running;
        TransferUpdate {
            transfer_id: job.id.clone(),
            from: job.from.clone(),
            to: job.to.clone(),
            state,
            items,
            items_done,
            bytes,
            bytes_done: self.bytes_done.load(Ordering::Relaxed).min(bytes),
            bytes_per_sec: if running { lock(&self.rate).0 } else { 0 },
            current: if running {
                lock(&self.current).clone()
            } else {
                None
            },
            conflict: job.conflict.clone(),
            error: job.error.clone(),
        }
    }

    /// Waits for the owner's answer to a conflict.
    pub async fn answer(&self) -> (ConflictChoice, bool) {
        loop {
            let notified = self.answered.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(answer) = lock(&self.answer).take() {
                return answer;
            }
            notified.await;
        }
    }

    /// Records the owner's answer.
    pub fn give_answer(&self, choice: ConflictChoice, apply_to_all: bool) {
        *lock(&self.answer) = Some((choice, apply_to_all));
        self.answered.notify_waiters();
    }
}

/// `m` locked, past a poisoning.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The path in the staging folder of `entry`.
#[must_use]
pub fn staged(staging: &std::path::Path, entry: &Entry) -> PathBuf {
    staging.join(&entry.rel)
}

/// Block `index`'s bytes in a file of `size`.
#[must_use]
pub fn block_bytes(size: u64, block: u64, index: u64) -> u64 {
    let start = index.saturating_mul(block);
    start.saturating_add(block).min(size).saturating_sub(start)
}

/// The byte spans of `entry` still to fetch, from the blocks on disk.
#[must_use]
pub fn missing(entry: &Entry, blocks: Option<&Spans>, block: u64) -> Vec<(u64, u64)> {
    let count = wire::block_count(entry.size, block);
    let empty = Spans::default();
    blocks
        .unwrap_or(&empty)
        .missing(count)
        .into_iter()
        .map(|(s, e)| {
            (
                s.saturating_mul(block),
                e.saturating_mul(block).min(entry.size),
            )
        })
        .collect()
}
