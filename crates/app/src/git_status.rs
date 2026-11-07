//! The project's Git status, shared by the right panel and the title bar:
//! GitChangesPanel's `useDiffIndex`, useGitFileStatuses and
//! useProjectDiffStats over the engine's checkout status. While status is
//! visible the engine's `WatchGit` keeps it current from its file watch;
//! otherwise a `ReadStatus` call reads it on demand. Reads never overlap; a
//! read requested meanwhile runs once the current one returns, and only a
//! changed snapshot notifies. A stage, unstage or discard shows in the frame
//! it is made, as an echo over the engine's status until a status that
//! covers it arrives. A call that changed the checkout replies with the scan
//! of the engine's covering status ([`Covered`]): a watched checkout is never
//! read again for it, and its echo stays until the watch reaches that scan.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter, Task};
use onicode_protocol::OpId;
use onicode_protocol::git::{Covered, GitEvent, GitStatus, ReadStatus, WatchGit};
use onicode_protocol::scm::{DiffIndex, DiffStats};

use crate::engine_client::{self, WatchUpdate, Watching};

const RESUME_TTL: Duration = Duration::from_secs(30);

/// A disk or Git change: Changes, History and the Explorer reread.
pub(crate) struct GitChanged;

#[derive(Default, Clone, PartialEq)]
pub(crate) struct StatusMap {
    pub files: HashMap<String, String>,
    pub dirs: HashMap<String, String>,
}
fn priority(status: &str) -> u8 {
    match status {
        "modified" => 3,
        "deleted" => 2,
        "added" | "untracked" => 1,
        _ => 0,
    }
}
/// useGitFileStatuses's `buildStatusMaps`: each folder takes its most
/// significant descendant status.
pub(crate) fn status_maps(index: &DiffIndex, root: &str) -> StatusMap {
    let mut map = StatusMap::default();
    for file in &index.files {
        map.files.insert(file.path.clone(), file.status.clone());
        let prio = priority(&file.status);
        if prio == 0 {
            continue;
        }
        let mut dir = parent(&file.path);
        while dir.len() > root.len() {
            let existing = map.dirs.get(dir).map_or(0, |s| priority(s));
            if prio <= existing {
                break;
            }
            map.dirs.insert(dir.to_owned(), file.status.clone());
            dir = parent(dir);
        }
    }
    map
}
fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// A Git change the view shows before the engine confirms it: a file or
/// folder (relative to the checkout), or everything without a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GitEcho {
    Stage(Option<String>),
    Unstage(Option<String>),
    Discard(Option<String>),
}

impl GitEcho {
    /// `index` as it reads once this change is made.
    fn apply(&self, index: &mut DiffIndex) {
        let covers = |path: &Option<String>, relative: &str| {
            path.as_deref().is_none_or(|path| {
                relative == path
                    || relative
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        };
        match self {
            Self::Stage(path) => {
                for file in &mut index.files {
                    if file.unstaged && covers(path, &file.relative) {
                        file.staged = true;
                        file.unstaged = false;
                    }
                }
            }
            Self::Unstage(path) => {
                for file in &mut index.files {
                    if file.staged && covers(path, &file.relative) {
                        file.staged = false;
                        file.unstaged = true;
                        if file.status == "added" {
                            "untracked".clone_into(&mut file.status);
                        }
                    }
                }
            }
            Self::Discard(path) => index.files.retain_mut(|file| {
                if !file.unstaged || !covers(path, &file.relative) {
                    return true;
                }
                // The staged side stays; an untracked or unstaged-only file goes.
                file.unstaged = false;
                file.staged && file.status != "untracked"
            }),
        }
    }
}

/// What an echo waits for before the engine's status shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Until {
    /// Its call's reply.
    Call,
    /// A status read at or after this scan.
    Scan(u64),
    /// The `ReadStatus` sent this many reads in to return.
    Read(u64),
}

struct Echo {
    op: OpId,
    echo: GitEcho,
    until: Until,
}

pub(crate) struct ProjectGit {
    /// The active session's working folder (its worktree, if it has one):
    /// Sidebar.tsx's `gitRoot`.
    project: String,
    pub root: String,
    /// What the views show: the engine's index with pending echoes over it.
    pub index: Option<DiffIndex>,
    pub stats: Option<DiffStats>,
    pub statuses: StatusMap,
    /// Bumped on every observed change, for views that reread on it.
    pub epoch: u64,
    pub review_cache: crate::review::ReviewCache,
    /// The engine's last status of `project`.
    status: Option<GitStatus>,
    /// Changes made here that the engine has not settled yet, oldest first.
    echoes: Vec<Echo>,
    /// `ReadStatus` calls sent, and returned: reads never overlap.
    reads_sent: u64,
    reads_done: u64,
    remembered: HashMap<String, (u64, DiffIndex)>,
    clock: u64,
    in_flight: bool,
    pending: bool,
    stats_loaded: Option<Instant>,
    /// The engine's watch of `project`, while status is visible.
    watch: Option<(Watching<GitStatus, GitEvent>, Task<()>)>,
}
impl EventEmitter<GitChanged> for ProjectGit {}

impl ProjectGit {
    pub(crate) fn new(project: String, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root: project.clone(),
            project: String::new(),
            index: None,
            stats: None,
            statuses: StatusMap::default(),
            epoch: 0,
            review_cache: Default::default(),
            status: None,
            echoes: Vec::new(),
            reads_sent: 0,
            reads_done: 0,
            remembered: HashMap::new(),
            clock: 0,
            in_flight: false,
            pending: false,
            stats_loaded: None,
            watch: None,
        };
        this.set_project(project, cx);
        this
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    pub(crate) fn available(&self) -> bool {
        !self.project.is_empty() && self.project != "~"
    }

    /// The first frame of a switch already shows the new checkout's last
    /// snapshot, never the previous one's files and actions.
    pub(crate) fn set_project(&mut self, project: String, cx: &mut Context<Self>) {
        if project == self.project {
            return;
        }
        self.project = project.clone();
        self.root = project.clone();
        self.status = None;
        self.echoes.clear();
        self.index = self.remembered.get(&project).map(|(_, i)| i.clone());
        self.stats = self.index.as_ref().map(stats_of);
        self.statuses = self
            .index
            .as_ref()
            .map(|i| status_maps(i, &self.root))
            .unwrap_or_default();
        self.stats_loaded = None;
        self.epoch += 1;
        cx.emit(GitChanged);
        cx.notify();
        self.warm_choices(cx);
        if self.watch.take().is_some() {
            // Watch the new checkout instead; its snapshot is the first read.
            self.set_polling(true, cx);
        } else {
            self.reload(cx);
        }
    }

    /// While the project's Git status is visible, the engine reads it again
    /// whenever the checkout or its Git directory changes on disk, and every
    /// 30s besides.
    pub(crate) fn set_polling(&mut self, polling: bool, cx: &mut Context<Self>) {
        if polling == self.watch.is_some() {
            return;
        }
        if !polling || !self.available() {
            self.watch = None;
            return;
        }
        let checkout = self.project.clone();
        let watched = checkout.clone();
        let watching = engine_client::local(cx).watch::<WatchGit>(move |since| WatchGit {
            checkout: watched.clone(),
            since,
        });
        let updates = watching.updates.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                let status = match update {
                    WatchUpdate::Snapshot(status) => Some(*status),
                    WatchUpdate::Events(events) => events
                        .into_iter()
                        .last()
                        .map(|GitEvent::Status(status)| *status),
                };
                let Some(status) = status else {
                    continue;
                };
                if this
                    .update(cx, |this, cx| {
                        if this.project == checkout {
                            this.settle_status(status, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        self.watch = Some((watching, task));
    }

    /// useProjectDiffStats reads again on focus once its snapshot is 30s old.
    pub(crate) fn resume(&mut self, cx: &mut Context<Self>) {
        if self
            .stats_loaded
            .is_none_or(|at| at.elapsed() >= RESUME_TTL)
            || self.watch.is_some()
        {
            self.warm_choices(cx);
            self.reload(cx);
        }
    }

    /// A mutation or file operation: reread now and tell every view.
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        cx.emit(GitChanged);
        self.warm_choices(cx);
        self.reload(cx);
    }

    /// Shows `echo` now, until [`Self::applied`] brings the status that
    /// covers `op`, or [`Self::settle`] drops it.
    pub(crate) fn echo(&mut self, op: OpId, echo: GitEcho, cx: &mut Context<Self>) {
        self.echoes.push(Echo {
            op,
            echo,
            until: Until::Call,
        });
        self.show(cx);
    }

    /// The call made under `op` failed: its echo goes now.
    pub(crate) fn settle(&mut self, op: OpId, cx: &mut Context<Self>) {
        let before = self.echoes.len();
        self.echoes.retain(|pending| pending.op != op);
        if self.echoes.len() != before {
            self.show(cx);
        }
    }

    /// A call on `checkout` made here (under `op`, if it echoed) applied and
    /// the engine named the status that covers it. A watched checkout takes
    /// that status from its watch, so nothing is read again; otherwise one
    /// read brings it. The echo stays until the covering status is held, so
    /// the view never shows the state from before the call.
    pub(crate) fn applied(
        &mut self,
        op: Option<OpId>,
        checkout: &str,
        covered: Covered,
        cx: &mut Context<Self>,
    ) {
        self.epoch += 1;
        cx.emit(GitChanged);
        self.warm_choices(cx);
        // A call on another checkout (or a project that moved on) covers
        // nothing held here: the project's status is read again, as for any
        // change.
        let scan = covered.scan.filter(|_| checkout == self.project);
        let until = match scan {
            Some(scan) if self.watch.is_some() => Until::Scan(scan),
            scan => {
                // The covering read is the next one sent: now, or once the
                // read in flight returns.
                let read = Until::Read(self.reads_sent + 1);
                self.reload(cx);
                scan.map_or(read, Until::Scan)
            }
        };
        if let Some(op) = op {
            for pending in &mut self.echoes {
                if pending.op == op {
                    pending.until = until;
                }
            }
        }
        if self.release() {
            self.show(cx);
        }
    }

    /// Drops the echoes the held status covers; whether any went.
    fn release(&mut self) -> bool {
        let scan = self.status.as_ref().map(|status| status.scan);
        let done = self.reads_done;
        let before = self.echoes.len();
        self.echoes.retain(|pending| match pending.until {
            Until::Call => true,
            Until::Scan(until) => scan.is_none_or(|scan| scan < until),
            Until::Read(read) => done < read,
        });
        self.echoes.len() != before
    }

    /// Project selection warms both menus before the user opens either one.
    fn warm_choices(&self, cx: &mut Context<Self>) {
        if !self.available() {
            return;
        }
        crate::git_cache::load(self.project.clone(), cx).detach();
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        if !self.available() {
            return;
        }
        if self.in_flight {
            self.pending = true;
            return;
        }
        self.in_flight = true;
        self.reads_sent += 1;
        let project = self.project.clone();
        let read = engine_client::local(cx).call::<ReadStatus>(
            None,
            ReadStatus {
                checkout: project.clone(),
            },
        );
        cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |this, cx| {
                this.in_flight = false;
                this.reads_done += 1;
                // A slow or unavailable checkout keeps its last snapshot, and
                // never takes another checkout's.
                if this.project == project
                    && let Ok(status) = result
                {
                    this.settle_status(status, cx);
                }
                if this.release() {
                    this.show(cx);
                }
                if std::mem::take(&mut this.pending) {
                    this.reload(cx);
                }
            });
        })
        .detach();
    }

    /// Takes `status` from the engine when it is newer than the one held.
    fn settle_status(&mut self, status: GitStatus, cx: &mut Context<Self>) {
        if status.index.is_none() {
            // A read that failed keeps the last snapshot.
            return;
        }
        if self
            .status
            .as_ref()
            .is_some_and(|held| held.scan >= status.scan)
        {
            return;
        }
        self.stats_loaded = Some(Instant::now());
        let changed = self
            .status
            .as_ref()
            .is_none_or(|held| held.index != status.index || held.versions != status.versions);
        self.status = Some(status);
        if self.release() || changed {
            self.show(cx);
        }
    }

    /// Recomputes what the views show from the engine's status and the
    /// pending echoes; notifies, and tells the views to reread, when it moved.
    fn show(&mut self, cx: &mut Context<Self>) {
        let Some(mut next) = self.status.as_ref().and_then(|s| s.index.clone()) else {
            return;
        };
        let project = self.project.clone();
        self.remember(&project, &next);
        for pending in &self.echoes {
            pending.echo.apply(&mut next);
        }
        if self.index.as_ref() == Some(&next) {
            return;
        }
        let had = self.index.is_some();
        self.stats = Some(stats_of(&next));
        self.statuses = status_maps(&next, &self.root);
        self.index = Some(next);
        if had {
            self.epoch += 1;
            cx.emit(GitChanged);
        }
        cx.notify();
    }

    /// rememberIndex: recent checkouts open on their last snapshot, within 24
    /// checkouts and 20,000 files.
    fn remember(&mut self, project: &str, index: &DiffIndex) {
        self.clock += 1;
        self.remembered
            .insert(project.to_owned(), (self.clock, index.clone()));
        let mut files: usize = self.remembered.values().map(|(_, i)| i.files.len()).sum();
        while self.remembered.len() > 24 || files > 20_000 {
            let Some(oldest) = self
                .remembered
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((_, gone)) = self.remembered.remove(&oldest) {
                files -= gone.files.len();
            }
        }
    }

    pub(crate) fn has_changes(&self) -> bool {
        self.stats.as_ref().is_some_and(|s| s.files > 0)
    }
}
fn stats_of(index: &DiffIndex) -> DiffStats {
    DiffStats {
        files: index.files.len() as i64,
        additions: index.additions,
        deletions: index.deletions,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use gpui::{AppContext as _, Entity, TestAppContext};
    use onicode_protocol::Call as _;
    use onicode_protocol::git::{Stage, StageTarget, Unstage};
    use onicode_protocol::scm::ChangedFile;

    use super::*;
    fn file(path: &str, status: &str) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            relative: path.trim_start_matches("/r/").into(),
            status: status.into(),
            additions: 0,
            deletions: 0,
            staged: false,
            unstaged: true,
        }
    }
    #[test]
    fn folders_take_their_most_significant_status() {
        let index = DiffIndex {
            files: vec![
                file("/r/a/b/new.rs", "untracked"),
                file("/r/a/b/old.rs", "modified"),
                file("/r/a/gone.rs", "deleted"),
            ],
            ..Default::default()
        };
        let map = status_maps(&index, "/r");
        assert_eq!(map.dirs["/r/a/b"], "modified");
        assert_eq!(map.dirs["/r/a"], "modified");
        assert!(!map.dirs.contains_key("/r"));
        assert_eq!(map.files["/r/a/gone.rs"], "deleted");
    }

    #[test]
    fn echoes_move_files_between_sides_as_git_will() {
        let mut index = DiffIndex {
            files: vec![
                file("/r/a/b/new.rs", "untracked"),
                file("/r/a/b/old.rs", "modified"),
                file("/r/ab.rs", "modified"),
            ],
            ..Default::default()
        };
        GitEcho::Stage(Some("a/b".into())).apply(&mut index);
        assert!(index.files[0].staged && !index.files[0].unstaged);
        assert!(index.files[1].staged && !index.files[1].unstaged);
        assert!(!index.files[2].staged, "a folder covers only its own files");
        GitEcho::Unstage(Some("a/b/old.rs".into())).apply(&mut index);
        assert!(!index.files[1].staged && index.files[1].unstaged);
        GitEcho::Discard(None).apply(&mut index);
        let left: Vec<_> = index.files.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(left, vec!["a/b/new.rs"], "only the staged side stays");
    }

    /// A checkout of its own with `a.txt` edited since its one commit, gone with the test.
    struct Repo(std::path::PathBuf);

    impl Repo {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "onicode-git-status-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&root).unwrap();
            let repo = Self(root);
            repo.git(&["init"]);
            std::fs::write(repo.0.join("a.txt"), "base\n").unwrap();
            repo.git(&["add", "."]);
            repo.git(&["commit", "-m", "base"]);
            std::fs::write(repo.0.join("a.txt"), "edit\n").unwrap();
            repo
        }

        #[expect(clippy::disallowed_methods, reason = "a test builds its git fixture")]
        fn git(&self, args: &[&str]) {
            let result = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&self.0)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }

        fn cwd(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn staged(git: &ProjectGit) -> bool {
        git.index
            .as_ref()
            .and_then(|index| index.files.iter().find(|file| file.relative == "a.txt"))
            .is_some_and(|file| file.staged && !file.unstaged)
    }

    /// Runs `call` on the engine the way a view does, and hands its reply to `git`.
    fn run(
        git: &Entity<ProjectGit>,
        op: OpId,
        call: impl Future<Output = Result<Covered, String>> + 'static,
        checkout: String,
        cx: &mut TestAppContext,
    ) {
        git.update(cx, |_, cx| {
            cx.spawn(async move |git, cx| {
                let covered = call.await.unwrap();
                let _ = git.update(cx, |git, cx| git.applied(Some(op), &checkout, covered, cx));
            })
            .detach();
        });
    }

    #[gpui::test]
    fn an_echo_holds_until_its_covering_status_and_a_watched_checkout_is_not_read_again(
        cx: &mut TestAppContext,
    ) {
        let repo = Repo::new();
        let cwd = repo.cwd();
        let engine = cx.update(engine_client::Engine::memory);
        cx.update(|cx| engine_client::set_local(&engine, cx));
        let reads = |cx: &mut TestAppContext| {
            cx.update(|cx| engine_client::calls_for_test(&engine, cx))
                .iter()
                .filter(|(name, _)| *name == ReadStatus::NAME)
                .count()
        };
        let git = cx.new(|cx| ProjectGit::new(cwd.clone(), cx));
        git.update(cx, |git, cx| git.set_polling(true, cx));
        cx.run_until_parked();
        let before = reads(cx);
        git.read_with(cx, |git, _| assert!(!staged(git)));

        // Watched: the echo shows in the frame, the watch brings the covering status, and
        // nothing reads it again.
        let op = OpId::new();
        git.update(cx, |git, cx| {
            git.echo(op, GitEcho::Stage(Some("a.txt".into())), cx);
            assert!(staged(git), "the same frame");
        });
        let stage = cx.update(|cx| {
            engine_client::local(cx).call::<Stage>(
                Some(op),
                Stage {
                    checkout: cwd.clone(),
                    target: StageTarget::Path("a.txt".into()),
                },
            )
        });
        run(&git, op, stage, cwd.clone(), cx);
        cx.run_until_parked();
        git.read_with(cx, |git, _| {
            assert!(staged(git));
            assert!(git.echoes.is_empty(), "the covering status took over");
        });
        assert_eq!(reads(cx), before, "a watched checkout is not read again");

        // Unwatched: one read brings the covering status, and the echo holds until it does.
        git.update(cx, |git, cx| git.set_polling(false, cx));
        cx.run_until_parked();
        let op = OpId::new();
        git.update(cx, |git, cx| {
            git.echo(op, GitEcho::Unstage(Some("a.txt".into())), cx);
            assert!(!staged(git));
        });
        let unstage = cx.update(|cx| {
            engine_client::local(cx).call::<Unstage>(
                Some(op),
                Unstage {
                    checkout: cwd.clone(),
                    path: Some("a.txt".into()),
                },
            )
        });
        run(&git, op, unstage, cwd.clone(), cx);
        cx.run_until_parked();
        git.read_with(cx, |git, _| {
            assert!(!staged(git));
            assert!(git.echoes.is_empty());
        });
        assert_eq!(reads(cx), before + 1, "one read covers it");

        // A status read before a call that arrives after its reply never shows the state from
        // before the call.
        git.update(cx, |git, cx| git.set_polling(true, cx));
        cx.run_until_parked();
        let op = OpId::new();
        git.update(cx, |git, cx| {
            let held = git.status.clone().unwrap();
            git.echo(op, GitEcho::Stage(Some("a.txt".into())), cx);
            git.applied(
                Some(op),
                &cwd,
                Covered {
                    scan: Some(held.scan + 5),
                },
                cx,
            );
            git.settle_status(
                GitStatus {
                    scan: held.scan + 1,
                    ..held.clone()
                },
                cx,
            );
            assert!(staged(git), "the older status waits under the echo");
            let mut covering = held;
            covering.scan += 5;
            for file in &mut covering.index.as_mut().unwrap().files {
                file.staged = true;
                file.unstaged = false;
            }
            git.settle_status(covering, cx);
            assert!(staged(git));
            assert!(git.echoes.is_empty());
        });
        assert_eq!(reads(cx), before + 1);
    }
}
