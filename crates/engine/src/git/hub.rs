//! The checkout hub: a [`Topic`] per watched checkout holding its status, refreshed from the
//! checkout's file watch, and the Git group's calls. Every Git command runs off the caller's
//! thread in a [`Lane`]; a call that changes a checkout rereads its status in the same slot
//! before it replies, so watchers hold the covering status first.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use futures::stream::BoxStream;
use onicode_core::files::is_equal_or_inside;
use onicode_core::{git, scm, worktree_management};
use onicode_protocol::git::{
    Branch, ChangeSet, ChangedFile, CheckpointStatus, Commit, CommitFile, CommitFiles, Covered,
    CreateBranch, CreatePr, CreateWorktree, Diff, DiffList, Discard, FileDiff, FileSides, GitEvent,
    GitHistory, GitSnapshot, GitStatus, History, NewWorktree, Pr, RangeContext, ReadCheckpoint,
    ReadSessionChanges, RemoveWorktree, RemovedWorktree, Restore, RestoreCheckpoint, SessionGit,
    Stage, StageTarget, StagedContext, Switch, Unstage, WatchGit, Worktree,
};
use onicode_protocol::{Cursor, Item, OpId};
use tokio::sync::Notify;

use super::lanes::{Lane, Lanes};
use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::fs_watch::{Change, FsWatch, Pump, Schedule, moves_status};
use crate::ledger::Ledger;
use crate::probe::{DiskProbe, Probe};
use crate::session::Spawn;
use crate::turn::{Database, locked};
use crate::workspace::WorkspaceHub;

/// A checkout's status as a watch's state.
pub(crate) struct GitFeed;

impl Feed for GitFeed {
    type Event = GitEvent;
    type State = GitStatus;

    fn apply(state: &mut GitStatus, event: &GitEvent) {
        let GitEvent::Status(status) = event;
        state.clone_from(status);
    }

    fn weight(event: &GitEvent) -> usize {
        let GitEvent::Status(status) = event;
        64 + status.index.as_ref().map_or(0, |index| {
            index
                .files
                .iter()
                .map(|file| 64 + file.path.len() + file.relative.len())
                .sum::<usize>()
        }) + status.versions.len() * 96
    }

    fn absorb(pending: &mut Pending<Self>, event: GitEvent) -> bool {
        // Every event is the whole status.
        pending.replace_or_append(event, |_, _| true)
    }
}

/// A watched checkout.
struct Checkout {
    topic: Topic<GitFeed>,
    schedule: Schedule,
    /// The newest read offered to the topic: an older read that finishes later is dropped.
    offered: u64,
    /// Dropped with the checkout: its pump thread ends.
    _watch: Option<FsWatch>,
}

#[derive(Default)]
struct Inner {
    checkouts: HashMap<String, Checkout>,
    /// Topics made this run: each takes a fresh epoch.
    topics: u64,
}

pub struct CheckoutHub {
    store: Database,
    spawn: Arc<dyn Spawn>,
    probe: Arc<dyn Probe>,
    ledger: Ledger,
    lanes: Lanes,
    limits: Limits,
    /// Start a platform file watch per checkout; tests drive changes themselves.
    watch_fs: bool,
    /// When this engine run started (Unix ms): topic epochs are numbered from it.
    started: u64,
    /// Numbers every status read this run.
    scans: AtomicU64,
    /// The workspace, told when a removed worktree moves sessions.
    workspace: OnceLock<Arc<WorkspaceHub>>,
    me: Weak<Self>,
    inner: Mutex<Inner>,
}

impl CheckoutHub {
    /// A hub over the real `git`, watching checkouts with the platform's file watch.
    pub fn new(store: Database, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::build(
            store,
            spawn,
            Arc::new(DiskProbe),
            Lanes::default(),
            Limits::default(),
            true,
            onicode_core::time::now_ms(),
        )
    }

    /// A hub over the real `git` without platform file watches: a status changes only through
    /// its own calls. For an app's tests, which never wait on FSEvents.
    pub fn quiet(store: Database, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::build(
            store,
            spawn,
            Arc::new(DiskProbe),
            Lanes::default(),
            Limits::default(),
            false,
            onicode_core::time::now_ms(),
        )
    }

    pub(crate) fn build(
        store: Database,
        spawn: Arc<dyn Spawn>,
        probe: Arc<dyn Probe>,
        lanes: Lanes,
        limits: Limits,
        watch_fs: bool,
        started: u64,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            ledger: Ledger::new(store.clone()),
            store,
            spawn,
            probe,
            lanes,
            limits,
            watch_fs,
            started,
            // Seeded from the start so a later run's scans follow an earlier run's.
            scans: AtomicU64::new(started.wrapping_shl(20)),
            workspace: OnceLock::new(),
            me: me.clone(),
            inner: Mutex::default(),
        })
    }

    /// The workspace whose sessions a removed worktree moves.
    pub fn attach(&self, workspace: Arc<WorkspaceHub>) {
        let _ = self.workspace.set(workspace);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `work` in `lane` on the hub's spawner, holding the lane's slot until it returns.
    pub(crate) async fn run<T, W>(&self, lane: Lane, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce(&Self) -> Result<T, String> + Send + 'static,
    {
        let slot = self.lanes.enter(lane).await?;
        let me = self.me.clone();
        let (reply, answer) = futures::channel::oneshot::channel();
        self.spawn.spawn(Box::pin(async move {
            let result = match me.upgrade() {
                Some(hub) => work(hub.as_ref()),
                None => Err("The engine stopped".to_owned()),
            };
            drop(slot);
            let _ = reply.send(result);
        }));
        answer
            .await
            .map_err(|_| "The engine stopped before this call finished".to_owned())?
    }

    /// Reads the status of `checkout` and offers it to its watchers. The caller holds a slot.
    fn read_status(&self, checkout: &str) -> Result<GitStatus, String> {
        self.read_offered(checkout).map(|(status, _)| status)
    }

    /// Reads the status of `checkout`, offers it to its watchers, and names the scan of the
    /// status they hold after the offer, if the checkout is watched.
    fn read_offered(&self, checkout: &str) -> Result<(GitStatus, Option<u64>), String> {
        let scan = self.scans.fetch_add(1, Ordering::Relaxed) + 1;
        let (index, versions) = self.probe.status(Path::new(checkout))?;
        let status = GitStatus {
            scan,
            index: Some(index),
            versions,
        };
        let held = self.offer(checkout, &status);
        Ok((status, held))
    }

    /// Publishes `status` to the checkout's watchers when it is newer than any read offered
    /// before and differs from what they hold. Names the scan of the status they then hold, which
    /// shows `status` or a newer read: `None` if the checkout is not watched.
    fn offer(&self, checkout: &str, status: &GitStatus) -> Option<u64> {
        let mut inner = self.lock();
        let watched = inner.checkouts.get_mut(checkout)?;
        if status.scan > watched.offered {
            watched.offered = status.scan;
            let held = watched.topic.state();
            if held.index != status.index || held.versions != status.versions {
                watched
                    .topic
                    .publish(vec![GitEvent::Status(Box::new(status.clone()))]);
            }
        }
        Some(watched.topic.state().scan)
    }

    /// Rereads a watched checkout after a call changed it: the reply that tells its caller
    /// which status of the watch shows the change. A status that cannot be read covers nothing.
    fn cover(&self, checkout: &str) -> Covered {
        if !self.lock().checkouts.contains_key(checkout) {
            return Covered::default();
        }
        Covered {
            scan: self.read_offered(checkout).ok().and_then(|(_, held)| held),
        }
    }

    /// The index the watchers of `checkout` hold, if it is watched.
    fn watched_index(&self, checkout: &str) -> Option<scm::DiffIndex> {
        self.lock()
            .checkouts
            .get(checkout)
            .and_then(|watched| watched.topic.state().index.clone())
    }

    /// `WatchGit`: the status, then every new one. After `Resync` the stream ends; resubscribe
    /// with the last cursor.
    pub async fn watch_git(
        self: &Arc<Self>,
        params: WatchGit,
    ) -> Result<BoxStream<'static, Item<GitStatus, GitEvent>>, String> {
        let WatchGit { checkout, since } = params;
        let joined = self.join(&checkout, since);
        let (id, notify) = match joined {
            Some(joined) => joined,
            None => {
                let at = checkout.clone();
                let first = self
                    .run(Lane::Foreground, move |hub| {
                        let scan = hub.scans.fetch_add(1, Ordering::Relaxed) + 1;
                        // A checkout that cannot be read yet is watched all the same: its first
                        // good read arrives as an event.
                        Ok(match hub.probe.status(Path::new(&at)) {
                            Ok((index, versions)) => GitStatus {
                                scan,
                                index: Some(index),
                                versions,
                            },
                            Err(_) => GitStatus {
                                scan,
                                ..GitStatus::default()
                            },
                        })
                    })
                    .await?;
                self.open(&checkout, first, since)
            }
        };
        let (draining, leaving) = (self.clone(), self.clone());
        let drained = checkout.clone();
        Ok(feed::stream(
            notify,
            move || draining.drain(&drained, id),
            move || leaving.leave(&checkout, id),
        ))
    }

    fn join(&self, checkout: &str, since: Option<Cursor>) -> Option<(u64, Arc<Notify>)> {
        let mut inner = self.lock();
        Some(inner.checkouts.get_mut(checkout)?.topic.join(since))
    }

    /// Watches `checkout`, starting from `first`, and joins it.
    fn open(&self, checkout: &str, first: GitStatus, since: Option<Cursor>) -> (u64, Arc<Notify>) {
        let now = onicode_core::time::now_ms();
        let mut inner = self.lock();
        let inner = &mut *inner;
        let topics = &mut inner.topics;
        let watched = inner
            .checkouts
            .entry(checkout.to_owned())
            .or_insert_with(|| {
                *topics += 1;
                let (watch, platform) = if self.watch_fs {
                    let root = PathBuf::from(checkout);
                    let mut roots = vec![root.clone()];
                    roots.extend(
                        scm::git_dirs(&root)
                            .into_iter()
                            .filter(|dir| !dir.starts_with(&root)),
                    );
                    let pump: Weak<dyn Pump> = self.me.clone();
                    let (watch, platform) = FsWatch::start(checkout.to_owned(), &roots, pump);
                    (Some(watch), platform)
                } else {
                    (None, true)
                };
                Checkout {
                    offered: first.scan,
                    topic: Topic::new(
                        self.started.wrapping_shl(20).wrapping_add(*topics),
                        first,
                        self.limits,
                    ),
                    schedule: Schedule::new(now, platform),
                    _watch: watch,
                }
            });
        watched.topic.join(since)
    }

    fn drain(&self, checkout: &str, id: u64) -> Option<Option<Item<GitStatus, GitEvent>>> {
        self.lock().checkouts.get_mut(checkout)?.topic.drain(id)
    }

    /// The watcher left; a checkout nobody watches is let go with its file watch.
    fn leave(&self, checkout: &str, id: u64) {
        let mut inner = self.lock();
        if inner
            .checkouts
            .get_mut(checkout)
            .is_some_and(|watched| watched.topic.leave(id))
        {
            inner.checkouts.remove(checkout);
        }
    }

    /// A change in `checkout` at `now`; one that cannot move its status is ignored.
    pub(crate) fn note(&self, checkout: &str, change: Change, now: u64) {
        let change = match change {
            Change::Paths(paths) => {
                let paths: Vec<String> = paths
                    .into_iter()
                    .filter(|path| moves_status(Path::new(path)))
                    .collect();
                if paths.is_empty() {
                    return;
                }
                Change::Paths(paths)
            }
            Change::Overflow => Change::Overflow,
        };
        if let Some(watched) = self.lock().checkouts.get_mut(checkout) {
            watched.schedule.note(now, change);
        }
    }

    /// Rereads `checkout` if a settled burst or its repair is due at `now`: a burst in the
    /// foreground lane, the periodic repair in the background one. Runs on the pump's thread.
    pub(crate) fn refresh(&self, checkout: &str, now: u64) {
        let due = self
            .lock()
            .checkouts
            .get_mut(checkout)
            .and_then(|watched| watched.schedule.take(now));
        let Some((_, repair)) = due else {
            return;
        };
        let lane = if repair {
            Lane::Background
        } else {
            Lane::Foreground
        };
        let Ok(slot) = futures::executor::block_on(self.lanes.enter(lane)) else {
            return;
        };
        let _ = self.read_status(checkout);
        drop(slot);
    }

    /// When `checkout` next has something to read.
    pub(crate) fn next_due(&self, checkout: &str) -> Option<u64> {
        self.lock()
            .checkouts
            .get(checkout)
            .map(|watched| watched.schedule.due())
    }

    /// A mutating call: `work` on the checkout in the foreground lane, then its covering status,
    /// once per op.
    async fn mutate<W>(
        &self,
        op: Option<OpId>,
        checkout: String,
        work: W,
    ) -> Result<Covered, String>
    where
        W: FnOnce(&Path) -> Result<(), String> + Send + 'static,
    {
        self.ledger
            .once(
                op,
                self.run(Lane::Foreground, move |hub| {
                    work(Path::new(&checkout))?;
                    Ok(hub.cover(&checkout))
                }),
            )
            .await
    }

    /// A read in the foreground lane.
    async fn read<T, W>(&self, checkout: String, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce(&Path) -> Result<T, String> + Send + 'static,
    {
        self.run(Lane::Foreground, move |_| work(Path::new(&checkout)))
            .await
    }

    /// `ReadStatus`.
    pub async fn read_status_now(&self, checkout: String) -> Result<GitStatus, String> {
        self.run(Lane::Foreground, move |hub| hub.read_status(&checkout))
            .await
    }

    /// `Diff`.
    pub async fn diff(&self, body: Diff) -> Result<DiffList, String> {
        let Diff { checkout, changes } = body;
        self.run(Lane::Foreground, move |hub| {
            let root = Path::new(&checkout);
            match changes {
                ChangeSet::WorkingTree => {
                    let known = hub.watched_index(&checkout);
                    let index = scm::review_index(root, known.as_ref())?;
                    let versions = scm::file_diff_versions(root, &index)
                        .ok()
                        .map(|versions| versions.into_iter().collect());
                    Ok(DiffList {
                        files: index.files,
                        versions,
                    })
                }
                ChangeSet::Commit { sha } => Ok(DiffList {
                    files: scm::commit_files(root, &sha)?,
                    versions: None,
                }),
                ChangeSet::Checkpoint { session } => Ok(DiffList {
                    files: checkpoint_files(session, &checkout)?,
                    versions: None,
                }),
            }
        })
        .await
    }

    /// `FileDiff`.
    pub async fn file_diff(&self, body: FileDiff) -> Result<FileSides, String> {
        let FileDiff {
            checkout,
            path,
            changes,
            staged,
        } = body;
        self.read(checkout.clone(), move |root| {
            let (staged, diff) = match changes {
                ChangeSet::WorkingTree => {
                    // Read now, not from a status a viewer last saw: staging moves it.
                    let staged = match staged {
                        Some(staged) => staged,
                        None => scm::diff_files(root)?
                            .files
                            .iter()
                            .find(|file| file.relative == path)
                            .is_some_and(|file| file.staged && !file.unstaged),
                    };
                    (staged, scm::file_diff(root, &path, staged)?)
                }
                ChangeSet::Commit { sha } => (false, scm::commit_file_diff(root, &sha, &path)?),
                ChangeSet::Checkpoint { session } => (
                    false,
                    checkpoints()?.exclusive(|store| {
                        store.file_diff(&session.to_string(), &checkout, &path)
                    })?,
                ),
            };
            Ok(FileSides { staged, diff })
        })
        .await
    }

    /// `Refs`.
    pub async fn refs(&self, checkout: String) -> Result<GitSnapshot, String> {
        self.read(checkout, git::snapshot).await
    }

    /// `Branches`.
    pub async fn branches(&self, checkout: String) -> Result<Vec<Branch>, String> {
        self.read(checkout, |root| {
            git::snapshot(root).map(|refs| refs.branches)
        })
        .await
    }

    /// `Worktrees`.
    pub async fn worktrees(&self, checkout: String) -> Result<Vec<Worktree>, String> {
        self.read(checkout, git::worktrees).await
    }

    /// `History`.
    pub async fn history(&self, body: History) -> Result<GitHistory, String> {
        let History { checkout, limit } = body;
        self.read(checkout, move |root| scm::history(root, limit))
            .await
    }

    /// `Switch`.
    pub async fn switch(&self, op: Option<OpId>, body: Switch) -> Result<Covered, String> {
        let Switch { checkout, branch } = body;
        self.mutate(op, checkout, move |root| git::checkout(root, &branch))
            .await
    }

    /// `CreateBranch`.
    pub async fn create_branch(
        &self,
        op: Option<OpId>,
        body: CreateBranch,
    ) -> Result<Covered, String> {
        let CreateBranch { checkout, name } = body;
        self.mutate(op, checkout, move |root| git::create_branch(root, &name))
            .await
    }

    /// `Stage`.
    pub async fn stage(&self, op: Option<OpId>, body: Stage) -> Result<Covered, String> {
        let Stage { checkout, target } = body;
        self.mutate(op, checkout, move |root| match target {
            StageTarget::All => scm::stage_all(root),
            StageTarget::Path(path) => scm::stage_file(root, &path),
            StageTarget::Contents { path, contents } => {
                scm::stage_contents(root, &path, contents.as_bytes())
            }
        })
        .await
    }

    /// `Unstage`.
    pub async fn unstage(&self, op: Option<OpId>, body: Unstage) -> Result<Covered, String> {
        let Unstage { checkout, path } = body;
        self.mutate(op, checkout, move |root| match path {
            Some(path) => scm::unstage_file(root, &path),
            None => scm::unstage_all(root),
        })
        .await
    }

    /// `Discard`.
    pub async fn discard(&self, op: Option<OpId>, body: Discard) -> Result<Covered, String> {
        let Discard { checkout, path } = body;
        self.mutate(op, checkout, move |root| match path {
            Some(path) => scm::discard_file(root, &path),
            None => scm::discard_all(root),
        })
        .await
    }

    /// `Commit`.
    pub async fn commit(&self, op: Option<OpId>, body: Commit) -> Result<Covered, String> {
        let Commit {
            checkout,
            message,
            amend,
            push,
        } = body;
        self.mutate(op, checkout, move |root| {
            scm::commit(root, &message, amend)?;
            if push {
                scm::push(root)?;
            }
            Ok(())
        })
        .await
    }

    /// `Push`.
    pub async fn push(&self, op: Option<OpId>, checkout: String) -> Result<Covered, String> {
        self.mutate(op, checkout, scm::push).await
    }

    /// `Pull`.
    pub async fn pull(&self, op: Option<OpId>, checkout: String) -> Result<Covered, String> {
        self.mutate(op, checkout, scm::pull).await
    }

    /// `Sync`.
    pub async fn sync(&self, op: Option<OpId>, checkout: String) -> Result<Covered, String> {
        self.mutate(op, checkout, scm::sync).await
    }

    /// `CommitFiles`: the files a commit changed, with their line counts.
    pub async fn commit_files(&self, body: CommitFiles) -> Result<Vec<CommitFile>, String> {
        let CommitFiles { checkout, sha } = body;
        self.read(checkout, move |root| git::commit_files(root, &sha))
            .await
    }

    /// `ReadDiffStats`: the working tree's line counts against `HEAD`.
    pub async fn diff_stats(&self, checkout: String) -> Result<scm::DiffStats, String> {
        self.read(checkout, scm::diff_stats).await
    }

    /// `HeadMessage`.
    pub async fn head_message(&self, checkout: String) -> Result<String, String> {
        self.read(checkout, scm::head_message).await
    }

    /// `CommitContext`.
    pub async fn commit_context(&self, checkout: String) -> Result<StagedContext, String> {
        self.read(checkout, scm::staged_context).await
    }

    /// `PrContext`.
    pub async fn pr_context(&self, checkout: String) -> Result<RangeContext, String> {
        self.read(checkout, scm::range_context).await
    }

    /// `PrStatus`: background work, which never takes a foreground slot.
    pub async fn pr_status(&self, checkout: String) -> Result<Option<Pr>, String> {
        self.run(Lane::Background, move |_| {
            scm::pr_status(Path::new(&checkout))
        })
        .await
    }

    /// `CreatePr`.
    pub async fn create_pr(&self, op: Option<OpId>, body: CreatePr) -> Result<String, String> {
        let CreatePr {
            checkout,
            title,
            body,
            base,
            head,
        } = body;
        self.ledger
            .once(
                op,
                self.run(Lane::Foreground, move |hub| {
                    let url = scm::pr_create(Path::new(&checkout), &title, &body, &base, &head)?;
                    hub.cover(&checkout);
                    Ok(url)
                }),
            )
            .await
    }

    /// `CreateWorktree`.
    pub async fn create_worktree(
        &self,
        op: Option<OpId>,
        body: CreateWorktree,
    ) -> Result<Worktree, String> {
        let CreateWorktree { checkout, worktree } = body;
        self.ledger
            .once(
                op,
                self.read(checkout, move |root| match worktree {
                    NewWorktree::Named { branch } => git::create_named_worktree(root, &branch),
                    NewWorktree::Project {
                        branch,
                        base,
                        existing,
                    } => git::create_project_worktree(root, &branch, base.as_deref(), existing),
                    NewWorktree::Session { base, session } => {
                        git::create_worktree(root, &base, session).map(|path| Worktree {
                            path: path.to_string_lossy().into_owned(),
                            ..Worktree::default()
                        })
                    }
                }),
            )
            .await
    }

    /// `RemoveWorktree`: with `preflight`, only Git's check that nothing would be lost.
    pub async fn remove_worktree(
        &self,
        op: Option<OpId>,
        body: RemoveWorktree,
    ) -> Result<RemovedWorktree, String> {
        let RemoveWorktree {
            checkout,
            path,
            preflight,
            delete_sessions,
        } = body;
        self.ledger
            .once(
                op,
                self.run(Lane::Foreground, move |hub| {
                    if preflight {
                        worktree_management::check(Path::new(&checkout), Path::new(&path))?;
                        return Ok(RemovedWorktree::default());
                    }
                    if delete_sessions {
                        let store = locked(&hub.store)?;
                        let worktrees = store.session_worktrees()?;
                        if store.sessions()?.iter().any(|session| {
                            is_equal_or_inside(
                                worktrees
                                    .get(&session.id)
                                    .and_then(|path| path.as_deref())
                                    .unwrap_or(&session.cwd),
                                &path,
                            )
                        }) {
                            return Err(
                                "Some sessions could not be deleted, so the worktree was kept."
                                    .into(),
                            );
                        }
                    }
                    let refs = git::snapshot(Path::new(&checkout))?;
                    let main = refs.worktrees.first().ok_or("No main working copy found")?;
                    worktree_management::remove(Path::new(&main.path), Path::new(&path))?;
                    let detached = locked(&hub.store)?.detach_worktree(&path, &main.path)?;
                    if let Some(workspace) = hub.workspace.get() {
                        for (session, _) in &detached {
                            workspace.refresh_session(session.id);
                        }
                    }
                    hub.cover(&main.path);
                    Ok(RemovedWorktree { detached })
                }),
            )
            .await
    }

    /// `ReadCheckpoint`: a session with nothing recorded, when this engine records no
    /// checkpoints, as monocode's card stays empty then.
    pub async fn read_checkpoint(&self, body: ReadCheckpoint) -> Result<CheckpointStatus, String> {
        let ReadCheckpoint { session, cwd } = body;
        self.run(Lane::Foreground, move |_| {
            onicode_core::checkpoint::store().map_or_else(
                || Ok(CheckpointStatus::default()),
                |store| store.exclusive(|store| store.status(&session.to_string(), &cwd)),
            )
        })
        .await
    }

    /// `RestoreCheckpoint`.
    pub async fn restore_checkpoint(
        &self,
        op: Option<OpId>,
        body: RestoreCheckpoint,
    ) -> Result<CheckpointStatus, String> {
        let RestoreCheckpoint {
            session,
            cwd,
            restore,
            path,
        } = body;
        self.ledger
            .once(
                op,
                self.run(Lane::Foreground, move |hub| {
                    let session_id = session.to_string();
                    let status = checkpoints()?.exclusive(|store| match restore {
                        Restore::Keep => store.keep(&session_id, &cwd, path.as_deref()),
                        Restore::Undo => store.undo(&session_id, &cwd, path.as_deref()),
                    })?;
                    hub.cover(&cwd);
                    Ok(status)
                }),
            )
            .await
    }

    /// `ReadSessionChanges`.
    pub async fn read_session_changes(
        &self,
        body: ReadSessionChanges,
    ) -> Result<SessionGit, String> {
        let ReadSessionChanges {
            checkout,
            paths,
            commit,
        } = body;
        self.read(checkout, move |root| {
            Ok(SessionGit {
                changes: git::session_changes(root, &paths),
                unpushed: commit.is_some_and(|commit| git::unpushed(root, &commit)),
            })
        })
        .await
    }
}

impl Pump for CheckoutHub {
    fn changed(&self, key: &str, change: Change, now: u64) {
        self.note(key, change, now);
    }

    fn due(&self, key: &str) -> Option<u64> {
        self.next_due(key)
    }

    fn beat(&self, key: &str, now: u64) {
        self.refresh(key, now);
    }
}

fn checkpoints() -> Result<&'static onicode_core::checkpoint::Store, String> {
    onicode_core::checkpoint::store().ok_or_else(|| "Session changes are unavailable".to_owned())
}

/// The latest recorded turn's files, as a commit lists them.
fn checkpoint_files(session: i64, cwd: &str) -> Result<Vec<ChangedFile>, String> {
    let status = checkpoints()?.exclusive(|store| store.status(&session.to_string(), cwd))?;
    Ok(status
        .files
        .into_iter()
        .map(|file| ChangedFile {
            path: file.path,
            relative: file.relative,
            status: file.status,
            additions: file.additions,
            deletions: file.deletions,
            staged: false,
            unstaged: true,
        })
        .collect())
}
