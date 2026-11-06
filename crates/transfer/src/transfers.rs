//! The jobs this engine owns as a destination: started, persisted, run until done past dropped
//! links and restarts, paused, resumed, cancelled, and reported at most every 100 ms each.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use onicode_proto::files::{
    ConflictChoice, StartTransferRequest, TransferAction, TransferState, TransferUpdate,
};
use tokio::sync::broadcast;

use crate::job::{Cell, Job, lock};
use crate::pull::{self, RETRY_MAX, RETRY_MIN};
use crate::sink::Sink;
use crate::source::Origins;
use crate::store::Store;
use crate::{Error, PROGRESS_EVERY, Tuning};

/// Ticks between saves of a running job's progress.
const SAVE_EVERY_TICKS: u32 = 10;
/// Runs in a row that end on a changed item before the job gives up.
const CHANGED_MAX: u32 = 20;

struct Inner {
    origins: Arc<dyn Origins>,
    sink: Arc<dyn Sink>,
    store: Store,
    tuning: Tuning,
    jobs: Mutex<HashMap<String, Arc<Cell>>>,
    updates: broadcast::Sender<TransferUpdate>,
    ticker: OnceLock<tokio::task::JoinHandle<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(ticker) = self.ticker.get() {
            ticker.abort();
        }
        for cell in lock(&self.jobs).values() {
            if let Some(task) = lock(&cell.task).take() {
                task.abort();
            }
        }
    }
}

/// The jobs. Cheap to clone; every clone is the same set.
#[derive(Clone)]
pub struct Transfers(Arc<Inner>);

impl Transfers {
    /// Jobs reading through `origins`, writing to `sink`, kept in `store`.
    #[must_use]
    pub fn new(
        origins: Arc<dyn Origins>,
        sink: Arc<dyn Sink>,
        store: Store,
        tuning: Tuning,
    ) -> Self {
        let (updates, _) = broadcast::channel(1024);
        Self(Arc::new(Inner {
            origins,
            sink,
            store,
            tuning,
            jobs: Mutex::new(HashMap::new()),
            updates,
            ticker: OnceLock::new(),
        }))
    }

    /// Picks up the jobs kept from before a restart: those that were moving start again from
    /// the blocks on disk, paused ones stay paused. Must run inside a runtime.
    pub fn resume_kept(&self) {
        for mut job in self.0.store.load() {
            if job.state.is_final() {
                self.0.store.remove(&job.id);
                continue;
            }
            let paused = job.state == TransferState::Paused;
            if !paused {
                job.state = TransferState::Queued;
            }
            let cell = Arc::new(Cell::new(job));
            lock(&self.0.jobs).insert(cell.id(), Arc::clone(&cell));
            if !paused {
                self.spawn(&cell);
            }
        }
        self.tick();
    }

    /// Starts copying `request.from` into `request.to.dir`, here. Returns the job's id.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for no paths, a relative path, or a destination that is not a folder.
    pub fn start(&self, request: StartTransferRequest) -> Result<String, Error> {
        if request.from.paths.is_empty()
            || request
                .from
                .paths
                .iter()
                .any(|p| !Path::new(p).is_absolute())
        {
            return Err(Error::Refused("copy absolute paths".into()));
        }
        if !Path::new(&request.to.dir).is_dir() {
            return Err(Error::Refused(format!(
                "{} is not a folder",
                request.to.dir
            )));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let job = Job::new(
            id.clone(),
            request.from,
            request.to,
            request.conflict,
            self.0.tuning,
        );
        let _ = self.0.store.save(&job);
        let cell = Arc::new(Cell::new(job));
        lock(&self.0.jobs).insert(id.clone(), Arc::clone(&cell));
        self.spawn(&cell);
        self.tick();
        Ok(id)
    }

    /// Pauses, resumes or cancels `id`.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a job not here.
    pub fn control(&self, id: &str, action: TransferAction) -> Result<(), Error> {
        let cell = self.cell(id)?;
        match action {
            TransferAction::Pause => {
                if cell.state().is_final() {
                    return Ok(());
                }
                stop(&cell);
                cell.set_state(TransferState::Paused, None);
                let _ = self.0.store.save(&cell.snapshot());
            }
            TransferAction::Resume => {
                if matches!(cell.state(), TransferState::Paused | TransferState::Failed) {
                    cell.set_state(TransferState::Queued, None);
                    self.spawn(&cell);
                }
            }
            TransferAction::Cancel => {
                if cell.state() == TransferState::Done {
                    return Ok(());
                }
                stop(&cell);
                cell.set_state(TransferState::Cancelled, None);
                let staging = cell.snapshot().staging();
                let (sink, store, id) = (
                    Arc::clone(&self.0.sink),
                    self.0.store.clone(),
                    id.to_owned(),
                );
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = sink.remove(&staging) {
                        tracing::warn!(%error, "a cancelled transfer's partial files stay");
                    }
                    store.remove(&id);
                });
            }
        }
        Ok(())
    }

    /// Answers the conflict `id` waits on.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a job not here or not in conflict.
    pub fn resolve(
        &self,
        id: &str,
        choice: ConflictChoice,
        apply_to_all: bool,
    ) -> Result<(), Error> {
        let cell = self.cell(id)?;
        if cell.state() != TransferState::Conflict {
            return Err(Error::Refused(
                "the transfer is not waiting on a conflict".into(),
            ));
        }
        cell.give_answer(choice, apply_to_all);
        Ok(())
    }

    /// Every job as it stands, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<TransferUpdate> {
        let mut cells: Vec<Arc<Cell>> = lock(&self.0.jobs).values().cloned().collect();
        cells.sort_by_key(|cell| cell.snapshot().created_ms);
        cells.iter().map(|cell| cell.update()).collect()
    }

    /// Updates from now on, each job at most every [`PROGRESS_EVERY`]. Subscribe before
    /// [`Self::list`] so nothing falls between.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<TransferUpdate> {
        self.0.updates.subscribe()
    }

    /// The update `id` reports now.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<TransferUpdate> {
        self.cell(id).ok().map(|cell| cell.update())
    }

    /// Saves every job, for a clean shutdown.
    pub fn save_all(&self) {
        for cell in lock(&self.0.jobs).values() {
            let job = cell.snapshot();
            if !job.state.is_final() {
                let _ = self.0.store.save(&job);
            }
        }
    }

    fn cell(&self, id: &str) -> Result<Arc<Cell>, Error> {
        lock(&self.0.jobs)
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Refused(format!("no transfer {id} here")))
    }

    fn spawn(&self, cell: &Arc<Cell>) {
        let inner = Arc::downgrade(&self.0);
        let task = tokio::spawn(drive(inner, Arc::clone(cell)));
        if let Some(old) = lock(&cell.task).replace(task) {
            old.abort();
        }
    }

    /// Starts the reporter once.
    fn tick(&self) {
        let inner = Arc::downgrade(&self.0);
        self.0.ticker.get_or_init(|| tokio::spawn(report(inner)));
    }
}

fn stop(cell: &Cell) {
    if let Some(task) = lock(&cell.task).take() {
        task.abort();
    }
    lock(&cell.files).clear();
}

/// Runs `cell`'s job until it is done, fails, or is stopped.
async fn drive(inner: Weak<Inner>, cell: Arc<Cell>) {
    let mut wait = RETRY_MIN;
    let mut changed = 0_u32;
    loop {
        let Some(this) = inner.upgrade() else {
            return;
        };
        let (origins, sink, store) = (
            Arc::clone(&this.origins),
            Arc::clone(&this.sink),
            this.store.clone(),
        );
        drop(this);
        let job = cell.snapshot();
        if job.state != TransferState::Conflict {
            cell.set_state(TransferState::Running, job.error.clone());
        }
        let before = cell.bytes_done.load(Ordering::Relaxed);
        let result = match origins.connect(&job.id, &job.from, &job.tuning).await {
            Ok(source) => {
                pull::run(Arc::clone(&cell), source, Arc::clone(&sink), store.clone()).await
            }
            Err(error) => Err(error),
        };
        if cell.bytes_done.load(Ordering::Relaxed) > before {
            wait = RETRY_MIN;
        }
        match result {
            Ok(()) => {
                cell.set_state(TransferState::Done, None);
                store.remove(&job.id);
                return;
            }
            Err(Error::Unknown) => tokio::time::sleep(RETRY_MIN).await,
            Err(Error::Changed) if changed < CHANGED_MAX => {
                changed = changed.saturating_add(1);
                tokio::time::sleep(wait).await;
            }
            Err(error) if error.is_transient() => {
                cell.set_state(TransferState::Running, Some(error.to_string()));
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2).min(RETRY_MAX);
            }
            Err(error) => {
                tracing::warn!(id = %job.id, %error, "transfer failed");
                cell.set_state(TransferState::Failed, Some(error.to_string()));
                let _ = store.save(&cell.snapshot());
                return;
            }
        }
        let _ = store.save(&cell.snapshot());
    }
}

/// Sends each job's update when it changed, at most every [`PROGRESS_EVERY`], and saves the
/// progress of moving jobs now and then.
async fn report(inner: Weak<Inner>) {
    let mut ticks = 0_u32;
    let mut interval = tokio::time::interval(PROGRESS_EVERY);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        ticks = ticks.wrapping_add(1);
        let save = ticks.is_multiple_of(SAVE_EVERY_TICKS);
        let cells: Vec<Arc<Cell>> = lock(&inner.jobs).values().cloned().collect();
        for cell in cells {
            measure(&cell);
            let update = cell.update();
            let changed = lock(&cell.sent).as_ref() != Some(&update);
            if changed {
                *lock(&cell.sent) = Some(update.clone());
                let _ = inner.updates.send(update);
            }
            if save && cell.dirty.swap(false, Ordering::Relaxed) && !cell.state().is_final() {
                let (store, job) = (inner.store.clone(), cell.snapshot());
                tokio::task::spawn_blocking(move || store.save(&job));
            }
        }
    }
}

/// Folds the bytes since the last tick into the job's rate.
fn measure(cell: &Cell) {
    let now = cell.bytes_done.load(Ordering::Relaxed);
    let mut rate = lock(&cell.rate);
    let (smoothed, last) = *rate;
    let ticks_per_sec =
        u64::try_from(Duration::from_secs(1).as_millis() / PROGRESS_EVERY.as_millis())
            .unwrap_or(10);
    let instant = now.saturating_sub(last).saturating_mul(ticks_per_sec);
    *rate = (
        smoothed
            .saturating_mul(7)
            .saturating_add(instant.saturating_mul(3))
            / 10,
        now,
    );
}

/// Where the jobs of the engine whose data folder is `data_dir` are kept.
#[must_use]
pub fn store_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("transfers")
}
