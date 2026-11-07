//! The Work group: what GitHub and schedules hand an engine to work on. The Inbox lists the
//! GitHub issues and pull requests of a person's projects and acts on them; Automations
//! are the runs an engine starts on a schedule or on an Inbox event; linked activity is what
//! changed on the GitHub items sessions are linked to.
//!
//! Every `gh` call stays in the engine: a viewer names what it shows
//! (the projects, the item) and renders what comes back. The mutating calls travel with an
//! [`OpId`](crate::OpId) in their [`Routed`](crate::Routed), and a repeat of the op is answered
//! with the first reply.
use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::automations::{Automation, AutomationRun, AutomationUpsert, DueAutomationRun};
use crate::files::Bytes;
use crate::inbox::{Checks, Details, Diff, Item, ProjectRepos, Status, Thread, ViewState};
use crate::sessions::{Groups, Session, WorkItem};
use crate::{Call, Outcome};

macro_rules! work_call {
    ($name:ident,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const NAME: &'static str = stringify!($name);
        }
    };
}

/// List the GitHub items of `projects`, as `state`'s filters ask. `cwd` is the
/// checkout `gh` runs in, and goes first when two checkouts share a repository.
/// `elsewhere` are projects other engines hold, with the repositories each said it checks out
/// ([`InboxRepos`]): their items are fetched here as a local project's are, each item naming
/// its project as the viewer spelled it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InboxList {
    pub cwd: PathBuf,
    pub projects: Vec<PathBuf>,
    pub elsewhere: Vec<ProjectRepos>,
    pub state: ViewState,
}

/// [`InboxList`]'s reply: the items, GitHub's connection and why the fetch failed, and the
/// engine's sessions, which the Inbox matches items against.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InboxListing {
    pub items: Vec<Item>,
    pub error: Option<String>,
    pub connection: Status,
    pub sessions: Vec<Session>,
}
work_call!(InboxList, reply: InboxListing);

/// The GitHub repositories the checkout at `project` belongs to (its own, then the one it was
/// forked from), read with `gh` where the checkout is: what a viewer hands another engine's
/// [`InboxList`] as `elsewhere` for a project this engine holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InboxRepos {
    pub project: PathBuf,
}
work_call!(InboxRepos, reply: Vec<String>);

/// An item's description and review state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadInboxDetails {
    pub cwd: PathBuf,
    pub item: Item,
}
work_call!(ReadInboxDetails, reply: Details);

/// An item's comments, reviews and commits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadInboxThread {
    pub cwd: PathBuf,
    pub item: Item,
}
work_call!(ReadInboxThread, reply: Thread);

/// A pull request's changes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadInboxDiff {
    pub cwd: PathBuf,
    pub item: Item,
}
work_call!(ReadInboxDiff, reply: Diff);

/// A pull request's checks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadInboxChecks {
    pub cwd: PathBuf,
    pub item: Item,
}
work_call!(ReadInboxChecks, reply: Checks);

/// Comment on an item, or reply to the comment `reply_to` names (empty for none). The reply is
/// GitHub's word on it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostInboxComment {
    pub cwd: PathBuf,
    pub item: Item,
    pub body: String,
    pub reply_to: String,
}
work_call!(PostInboxComment, reply: String);

/// Run one of an item's actions (merge, close, reopen, …); the reply is the item after it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunInboxAction {
    pub cwd: PathBuf,
    pub item: Item,
    pub action: String,
}
work_call!(RunInboxAction, reply: Item);

/// An image or video an item's text links to, fetched under the Inbox's host and redirect rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxMedia {
    pub url: String,
}
work_call!(InboxMedia, reply: Bytes);

/// A person's public avatar, as a small PNG.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxAvatar {
    pub url: String,
}
work_call!(InboxAvatar, reply: Bytes);

/// Every automation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAutomations {}
work_call!(ListAutomations, reply: Vec<Automation>);

/// An automation's runs, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRuns {
    pub automation: String,
}
work_call!(AutomationRuns, reply: Vec<AutomationRun>);

/// Create an automation, or replace the one its id names; the reply is it as saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpsertAutomation {
    pub automation: AutomationUpsert,
}
work_call!(UpsertAutomation, reply: Automation);

/// Turn an automation on or off; the reply is it as saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAutomationEnabled {
    pub automation: String,
    pub enabled: bool,
}
work_call!(SetAutomationEnabled, reply: Automation);

/// Start an automation's run now; the reply is the run for the viewer to start in a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAutomationNow {
    pub automation: String,
}
work_call!(RunAutomationNow, reply: DueAutomationRun);

/// Delete an automation and its runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteAutomation {
    pub automation: String,
}
work_call!(DeleteAutomation, reply: Outcome);

/// Whether an enabled automation waits on Inbox events, so a viewer keeps the Inbox polling.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HasEventAutomations {}
work_call!(HasEventAutomations, reply: bool);

/// A project's folders and folded groups.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGroups {
    pub project: String,
}
work_call!(ProjectGroups, reply: Groups);

/// One new comment, review, review comment or commit on a linked item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedEntry {
    pub id: String,
    pub kind: String,
    pub author: String,
    pub text: String,
    pub created_at: String,
    pub url: String,
}

/// What changed on one session's linked item since the session last saw it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedCard {
    pub session: i64,
    pub item: WorkItem,
    pub title: String,
    pub state: String,
    /// The item's last update, in ms.
    pub updated_at: i64,
    /// Newest first.
    pub entries: Vec<LinkedEntry>,
    /// A feed of the item could not be read.
    pub details_error: bool,
    /// A feed held a full page, so older entries may be missing.
    pub truncated: bool,
}

/// What changed on the GitHub items `sessions` link to, after each session's own update or the
/// time `seen` acknowledges for it, whichever is later. Archived sessions are left out. The
/// engine keeps the feeds it read, so an item that has not changed costs one request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedActivity {
    pub sessions: Vec<Session>,
    /// The last update acknowledged, in ms, by session row.
    pub seen: BTreeMap<i64, i64>,
}
work_call!(LinkedActivity, reply: Vec<LinkedCard>);

/// A GitHub issue or pull request a transcript links to, read with `gh` in `cwd`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadWorkItem {
    pub cwd: PathBuf,
    pub repo: String,
    /// `pr` or `issue`.
    pub kind: String,
    pub number: i64,
}
work_call!(ReadWorkItem, reply: Item);

/// A linked issue's or pull request's description and review state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadWorkItemDetails {
    pub cwd: PathBuf,
    pub repo: String,
    pub kind: String,
    pub number: i64,
}
work_call!(ReadWorkItemDetails, reply: Details);
