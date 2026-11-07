//! The Git group as an `onicode-rpc` service: `WatchGit` and the Git calls, each wrapped in a
//! `Routed` that names this engine (`None`) and, for a mutating call, its op.
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use onicode_protocol::git::{
    self, Branches, Commit, CommitContext, CommitFiles, CreateBranch, CreatePr, CreateWorktree,
    Diff, Discard, FileDiff, HeadMessage, History, PrContext, PrStatus, Pull, Push, ReadCheckpoint,
    ReadDiffStats, ReadSessionChanges, ReadStatus, Refs, RemoveWorktree, RestoreCheckpoint, Stage,
    Switch, Unstage, WatchGit, Worktrees,
};
use onicode_protocol::{Call, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use super::hub::CheckoutHub;
use crate::service::{reply, routed};

pub struct GitService {
    hub: Arc<CheckoutHub>,
}

impl GitService {
    #[must_use]
    pub fn new(hub: Arc<CheckoutHub>) -> Self {
        Self { hub }
    }
}

/// Every method this service serves.
const METHODS: [&str; 29] = [
    WatchGit::NAME,
    ReadStatus::NAME,
    Diff::NAME,
    FileDiff::NAME,
    Branches::NAME,
    Refs::NAME,
    History::NAME,
    CommitFiles::NAME,
    ReadDiffStats::NAME,
    Switch::NAME,
    CreateBranch::NAME,
    Stage::NAME,
    Unstage::NAME,
    Discard::NAME,
    Commit::NAME,
    Push::NAME,
    Pull::NAME,
    git::Sync::NAME,
    HeadMessage::NAME,
    CommitContext::NAME,
    PrContext::NAME,
    PrStatus::NAME,
    CreatePr::NAME,
    Worktrees::NAME,
    CreateWorktree::NAME,
    RemoveWorktree::NAME,
    ReadCheckpoint::NAME,
    RestoreCheckpoint::NAME,
    ReadSessionChanges::NAME,
];

impl GitService {
    /// Whether `method` is one of the Git group's.
    #[must_use]
    pub fn serves(method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for GitService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            WatchGit::NAME => {
                let (_, body) = routed::<WatchGit>(params)?;
                let items = hub.watch_git(body).await.map_err(RpcError::Failed)?;
                Ok(RpcReply::Stream(
                    items
                        .filter_map(|item| futures::future::ready(serde_json::to_value(&item).ok()))
                        .boxed(),
                ))
            }
            ReadStatus::NAME => {
                let (_, ReadStatus { checkout }) = routed(params)?;
                reply(hub.read_status_now(checkout).await)
            }
            Diff::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.diff(body).await)
            }
            FileDiff::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.file_diff(body).await)
            }
            Branches::NAME => {
                let (_, Branches { checkout }) = routed(params)?;
                reply(hub.branches(checkout).await)
            }
            Refs::NAME => {
                let (_, Refs { checkout }) = routed(params)?;
                reply(hub.refs(checkout).await)
            }
            History::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.history(body).await)
            }
            CommitFiles::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.commit_files(body).await)
            }
            ReadDiffStats::NAME => {
                let (_, ReadDiffStats { checkout }) = routed(params)?;
                reply(hub.diff_stats(checkout).await)
            }
            Switch::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.switch(op, body).await)
            }
            CreateBranch::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create_branch(op, body).await)
            }
            Stage::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.stage(op, body).await)
            }
            Unstage::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.unstage(op, body).await)
            }
            Discard::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.discard(op, body).await)
            }
            Commit::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.commit(op, body).await)
            }
            Push::NAME => {
                let (op, Push { checkout }) = routed(params)?;
                reply(hub.push(op, checkout).await)
            }
            Pull::NAME => {
                let (op, Pull { checkout }) = routed(params)?;
                reply(hub.pull(op, checkout).await)
            }
            git::Sync::NAME => {
                let (op, git::Sync { checkout }) = routed(params)?;
                reply(hub.sync(op, checkout).await)
            }
            HeadMessage::NAME => {
                let (_, HeadMessage { checkout }) = routed(params)?;
                reply(hub.head_message(checkout).await)
            }
            CommitContext::NAME => {
                let (_, CommitContext { checkout }) = routed(params)?;
                reply(hub.commit_context(checkout).await)
            }
            PrContext::NAME => {
                let (_, PrContext { checkout }) = routed(params)?;
                reply(hub.pr_context(checkout).await)
            }
            PrStatus::NAME => {
                let (_, PrStatus { checkout }) = routed(params)?;
                reply(hub.pr_status(checkout).await)
            }
            CreatePr::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create_pr(op, body).await)
            }
            Worktrees::NAME => {
                let (_, Worktrees { checkout }) = routed(params)?;
                reply(hub.worktrees(checkout).await)
            }
            CreateWorktree::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create_worktree(op, body).await)
            }
            RemoveWorktree::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.remove_worktree(op, body).await)
            }
            ReadCheckpoint::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.read_checkpoint(body).await)
            }
            RestoreCheckpoint::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.restore_checkpoint(op, body).await)
            }
            ReadSessionChanges::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.read_session_changes(body).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
