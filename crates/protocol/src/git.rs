//! The Git group: a checkout's status and branch as the engine's file watch keeps them
//! ([`WatchGit`]), and the calls behind the Changes, review, history, branch and worktree views.
//! The mutating calls travel with an [`OpId`](crate::OpId) in their [`Routed`](crate::Routed): a
//! repeat of the op is answered with the first reply and runs nothing again. A call that changes
//! a watched checkout rereads its status before it replies, and its reply ([`Covered`]) names the
//! scan of the status that covers it: a viewer keeps its echo until it holds a status at or after
//! that scan, and reads nothing more itself.
//!
//! The engine runs Git in two lanes: what a person waits on (these calls and the watch's
//! refreshes) never queues behind background work such as pull-request discovery
//! ([`PrStatus`]) or the watch's periodic repair reads.
use std::collections::BTreeMap;

pub use onicode_core::checkpoint::{CheckpointFile, CheckpointStatus};
pub use onicode_core::git::{
    Branch, CommitFile, SessionChanges, Snapshot as GitSnapshot, Worktree,
};
pub use onicode_core::scm::{
    ChangedFile, DiffIndex, DiffStats, FileDiff as DiffSides, FileDiffVersion,
    History as GitHistory, HistoryCommit, HistoryRef, Pr, RangeContext, StagedContext,
};
use serde::{Deserialize, Serialize};

use crate::agent::Settings;
use crate::sessions::Session;
use crate::{Call, Cursor, Watch};

/// A checkout's status as of a read: the changed files with the branch, upstream and counts, and
/// each change's version (`staged:` or `unstaged:` and the relative path), which tells a review
/// what to reread.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    /// Orders the engine's status reads, across engine runs too: a viewer drops a read older
    /// than one it holds.
    pub scan: u64,
    /// `None` until a read succeeds; a read that fails keeps the last one.
    pub index: Option<DiffIndex>,
    pub versions: BTreeMap<String, FileDiffVersion>,
}

/// One change to a watched checkout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitEvent {
    /// The status changed: replace it whole.
    Status(Box<GitStatus>),
}

/// Watch a checkout's status: the last read, then a new one whenever the checkout or its Git
/// directory changes on disk (coalesced, 300 ms after a burst starts) and every 30 s besides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchGit {
    pub checkout: String,
    /// The last cursor this viewer holds, to resume from.
    pub since: Option<Cursor>,
}

impl Watch for WatchGit {
    type Event = GitEvent;
    type Params = Self;
    type State = GitStatus;

    const NAME: &'static str = "WatchGit";
}

macro_rules! git_call {
    ($name:ident,agent: $agent:literal,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const AGENT: bool = $agent;
            const NAME: &'static str = stringify!($name);
        }
    };
}

/// A checkout's status, read now; a watched checkout's watchers get it too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadStatus {
    pub checkout: String,
}
git_call!(ReadStatus, agent: true, reply: GitStatus);

/// Whose changes a review lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeSet {
    /// The working tree, staged and unstaged.
    WorkingTree,
    /// A commit, against its first parent.
    Commit { sha: String },
    /// The latest recorded turn of the session stored at `session`.
    Checkpoint { session: i64 },
}

/// The files a change set holds; for the working tree, with their versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub checkout: String,
    pub changes: ChangeSet,
}
git_call!(Diff, agent: true, reply: DiffList);

/// [`Diff`]'s reply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffList {
    pub files: Vec<ChangedFile>,
    pub versions: Option<BTreeMap<String, FileDiffVersion>>,
}

/// One file's two sides in a change set: for the working tree, HEAD against the index when
/// staged, else the index against the disk. Sides over 8 MB come back `too_large` and empty,
/// binary ones `binary`, as the review shows them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub checkout: String,
    /// Relative to the checkout.
    pub path: String,
    pub changes: ChangeSet,
    /// Which working-tree side to read; `None` lets the engine decide from the checkout's
    /// status as it reads it: a fully staged file compares HEAD with the index.
    pub staged: Option<bool>,
}
git_call!(FileDiff, agent: true, reply: FileSides);

/// [`FileDiff`]'s reply: the two sides, and whether they are the staged ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSides {
    pub staged: bool,
    pub diff: DiffSides,
}

/// The checkout's branches, current branch and worktrees, for the branch and worktree pickers;
/// `here` names the worktree the checkout is, its symlinks resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refs {
    pub checkout: String,
}
git_call!(Refs, agent: true, reply: GitSnapshot);

/// The checkout's local and remote branches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branches {
    pub checkout: String,
}
git_call!(Branches, agent: true, reply: Vec<Branch>);

/// The checkout's worktrees, the main one first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktrees {
    pub checkout: String,
}
git_call!(Worktrees, agent: true, reply: Vec<Worktree>);

/// The commit graph from HEAD, newest first: 200 commits unless `limit` says otherwise, 500 at
/// most.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub checkout: String,
    pub limit: Option<u32>,
}
git_call!(History, agent: true, reply: GitHistory);

/// The files a commit changed against its first parent, as `git show --numstat` counts them:
/// a transcript's commit card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitFiles {
    pub checkout: String,
    pub sha: String,
}
git_call!(CommitFiles, agent: true, reply: Vec<CommitFile>);

/// The checkout's changed files and lines, read now: the project rail's counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadDiffStats {
    pub checkout: String,
}
git_call!(ReadDiffStats, agent: true, reply: DiffStats);

/// What a call that changes a checkout answers: the scan of the status that covers it, which
/// the checkout's watchers hold by the time the reply arrives (they may not have it in hand yet);
/// `None` when nothing watches the checkout, so a viewer that shows it reads it itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Covered {
    pub scan: Option<u64>,
}

/// Check out a branch, tracking a remote one locally. Dirty files are never overwritten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switch {
    pub checkout: String,
    pub branch: Branch,
}
git_call!(Switch, agent: false, reply: Covered);

/// Create a branch at HEAD and switch to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBranch {
    pub checkout: String,
    pub name: String,
}
git_call!(CreateBranch, agent: false, reply: Covered);

/// What [`Stage`] stages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageTarget {
    All,
    /// A file or folder, relative to the checkout.
    Path(String),
    /// These contents as the file's staged side: a staged hunk.
    Contents {
        path: String,
        contents: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub checkout: String,
    pub target: StageTarget,
}
git_call!(Stage, agent: false, reply: Covered);

/// Unstage a file or folder, or everything without `path`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unstage {
    pub checkout: String,
    pub path: Option<String>,
}
git_call!(Unstage, agent: false, reply: Covered);

/// Throw away a file's unstaged changes (deleting it when untracked), or every unstaged change
/// without `path`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discard {
    pub checkout: String,
    pub path: Option<String>,
}
git_call!(Discard, agent: false, reply: Covered);

/// Commit what is staged (amending HEAD with `amend`), then push with `push`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub checkout: String,
    pub message: String,
    pub amend: bool,
    pub push: bool,
}
git_call!(Commit, agent: false, reply: Covered);

/// Push the branch, setting its upstream the first time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    pub checkout: String,
}
git_call!(Push, agent: false, reply: Covered);

/// Pull the branch's upstream, fast-forward only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pull {
    pub checkout: String,
}
git_call!(Pull, agent: false, reply: Covered);

/// Pull, then push.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sync {
    pub checkout: String,
}
git_call!(Sync, agent: false, reply: Covered);

/// HEAD's commit message, which an amend starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadMessage {
    pub checkout: String,
}
git_call!(HeadMessage, agent: true, reply: String);

/// What is staged, as a commit message is written from it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitContext {
    pub checkout: String,
}
git_call!(CommitContext, agent: true, reply: StagedContext);

/// The branch's commits and diff against its base, as a pull request is written from them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrContext {
    pub checkout: String,
}
git_call!(PrContext, agent: true, reply: RangeContext);

/// The branch's open pull request, if `gh` finds one. Background work: it never holds up a call
/// a person waits on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatus {
    pub checkout: String,
}
git_call!(PrStatus, agent: true, reply: Option<Pr>);

/// Open a pull request from `head` into `base` with `gh`; the reply is its URL.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePr {
    pub checkout: String,
    pub title: String,
    pub body: String,
    pub base: String,
    pub head: String,
}
git_call!(CreatePr, agent: false, reply: String);

/// Which worktree [`CreateWorktree`] makes, beside the main checkout's others.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NewWorktree {
    /// A new branch at the checkout's HEAD: the sidebar's worktree switcher.
    Named { branch: String },
    /// A branch from `base`, or the existing branch with `existing`: Settings and the app bridge.
    Project {
        branch: String,
        base: Option<String>,
        existing: bool,
    },
    /// A temporary branch from `base` for the session stored at `session`: a new chat that runs
    /// in its own worktree.
    Session { base: String, session: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorktree {
    pub checkout: String,
    pub worktree: NewWorktree,
}
git_call!(CreateWorktree, agent: false, reply: Worktree);

/// Remove the linked worktree at `path` from the project `checkout` belongs to, refusing one with
/// changes Git would lose. With `preflight`, only check. With `delete_sessions`, sessions still
/// in it keep it. Sessions left in it move to the main checkout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub checkout: String,
    pub path: String,
    pub preflight: bool,
    pub delete_sessions: bool,
}
git_call!(RemoveWorktree, agent: false, reply: RemovedWorktree);

/// [`RemoveWorktree`]'s reply: the sessions that moved to the main checkout, with their
/// settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemovedWorktree {
    pub detached: Vec<(Session, Settings)>,
}

/// The latest recorded turn of the session stored at `session`, in the checkout `cwd`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadCheckpoint {
    pub session: i64,
    pub cwd: String,
}
git_call!(ReadCheckpoint, agent: true, reply: CheckpointStatus);

/// What [`RestoreCheckpoint`] does with the recorded turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Restore {
    /// Keep the turn's changes: the recording lets them go.
    Keep,
    /// Put the files back as they were before the turn.
    Undo,
}

/// Keep or undo the latest recorded turn, or one file of it with `path`; the reply is what is
/// still recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreCheckpoint {
    pub session: i64,
    pub cwd: String,
    pub restore: Restore,
    pub path: Option<String>,
}
git_call!(RestoreCheckpoint, agent: false, reply: CheckpointStatus);

/// Whether a session's own edits (`paths`) are staged or unstaged in `checkout`, and whether its
/// last `commit` is still unpushed: the conversation's Git actions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSessionChanges {
    pub checkout: String,
    pub paths: Vec<String>,
    pub commit: Option<String>,
}
git_call!(ReadSessionChanges, agent: false, reply: SessionGit);

/// [`ReadSessionChanges`]'s reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGit {
    pub changes: SessionChanges,
    pub unpushed: bool,
}
