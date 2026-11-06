//! The Workspace group: what the sidebar, history and the workspace views show of every session
//! an engine holds ([`WatchWorkspace`]), and the calls that change it. Every call but
//! [`SearchSessions`] and [`ListNotes`] is mutating: it travels with an [`OpId`] in its
//! [`Routed`](crate::Routed), and a repeat of the op is answered with the first reply. A call
//! that changes the workspace's state publishes the change as events, then a
//! [`WorkspaceEvent::Receipt`] naming its op.
//!
//! Sessions here are keyed by the engine's row id beside their [`SessionId`]: folders
//! ([`Groups`]), reminders and the app's saved layout all name sessions by it until `SessionKey`
//! replaces it (engine.md, migration step 9).
use serde::{Deserialize, Serialize};

use crate::accounts::Profile;
use crate::agent::{AccessMode, Settings};
use crate::automations::DueAutomationRun;
use crate::notes::{Note, NoteUpsert, ResponseNote};
use crate::search::Hit;
use crate::sessions::{Groups, Orchestration, Reminder, Session, TextModel, WorkItem};
use crate::{Call, Cursor, OpId, Outcome, SessionId, Watch};

/// One session as the sidebar lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    /// The stored summary; `session.id` is the engine's row id.
    pub session: Session,
    /// The id the store keeps the session under for other tools: monocode's own in a store
    /// shared with it, else the row id.
    pub key: String,
    /// The checkout the session runs in, when it is not the project's own.
    pub worktree: Option<String>,
    /// The composer holds a saved draft.
    pub draft: bool,
    /// The session an operator thread started it from.
    pub started_by: Option<i64>,
    /// The automation whose runs it holds.
    pub automation: Option<String>,
    /// A turn is running in the engine.
    pub busy: bool,
    /// A turn finished since a viewer last showed the session ([`MarkSeen`]).
    pub unread: bool,
}

/// A project the engine has sessions or folders in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub path: String,
    pub groups: Groups,
}

/// An orchestration lead's last run, which the sidebar lists its agents from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lead {
    pub lead: i64,
    pub summary: Orchestration,
}

/// The workspace as of a [`Cursor`]: what a watcher starts from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceState {
    /// Newest first, as the store orders them.
    pub sessions: Vec<SessionSummary>,
    /// By path.
    pub projects: Vec<Project>,
    /// By due time, then session.
    pub reminders: Vec<Reminder>,
    /// By lead.
    pub orchestrations: Vec<Lead>,
    /// How many notes there are.
    pub notes: usize,
    /// Automation runs the scheduler claimed that no viewer has started yet
    /// ([`PrepareAutomationRun`]), oldest first.
    pub automations: Vec<DueAutomationRun>,
}

/// One change to the workspace, in the order the engine made it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkspaceEvent {
    /// A session appeared or changed: replace it whole.
    Session(Box<SessionSummary>),
    /// The session with this row id was deleted.
    Removed(i64),
    /// A project's folders changed, or the project appeared.
    Project(Project),
    /// Every reminder, after any change to one. A reminder whose `fired_at` became set is due
    /// now: viewers announce it.
    Reminders(Vec<Reminder>),
    Orchestration(Lead),
    Notes(usize),
    /// The scheduler claimed a run.
    AutomationDue(Box<DueAutomationRun>),
    /// A viewer started the run with this id, or it was given up.
    AutomationTaken(String),
    /// What became of a mutating call.
    Receipt {
        op: OpId,
        outcome: Outcome,
    },
}

/// Watch the workspace: a snapshot (or, with a `since` the engine's ring still covers, only the
/// events after it), then every event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchWorkspace {
    /// The last cursor this viewer holds, to resume from.
    pub since: Option<Cursor>,
}

impl Watch for WatchWorkspace {
    type Event = WorkspaceEvent;
    type Params = Self;
    type State = WorkspaceState;

    const NAME: &'static str = "WatchWorkspace";
}

macro_rules! workspace_call {
    ($name:ident,agent: $agent:literal,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const AGENT: bool = $agent;
            const NAME: &'static str = stringify!($name);
        }
    };
}

/// A new session's starting settings and title.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NewSession {
    /// No saved settings: the provider's own defaults.
    Bare,
    /// Exactly these settings.
    Settings(Box<Settings>),
    /// The project's saved defaults for the provider, with the viewer's choices over them.
    Defaults(SessionDefaults),
}

/// The choices a new chat starts from, as the viewer read them from its preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDefaults {
    /// A model chosen over the saved one; it drops the saved model's controls.
    pub model: Option<String>,
    pub account: Profile,
    pub access: AccessMode,
    pub effort: Option<String>,
    pub worktree_cwd: Option<String>,
}

/// Create a session under the viewer's id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSession {
    pub session: SessionId,
    pub provider: String,
    pub cwd: String,
    pub title: Option<String>,
    pub start: NewSession,
    /// The session an operator thread starts it from.
    pub started_by: Option<i64>,
}
workspace_call!(CreateSession, agent: true, reply: Created);

/// [`CreateSession`]'s reply, and [`PrepareAutomationRun`]'s.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Created {
    pub outcome: Outcome,
    pub session: Option<Box<SessionSummary>>,
    /// The settings the session starts with.
    pub settings: Option<Box<Settings>>,
}

/// Retitle a session. With `expected`, only while its title is still that one: a generated
/// title never overwrites a name the person typed (`Rejected(StaleRev)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rename {
    pub session: i64,
    pub title: String,
    pub expected: Option<String>,
}
workspace_call!(Rename, agent: true, reply: Outcome);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archive {
    pub session: i64,
    pub archived: bool,
}
workspace_call!(Archive, agent: true, reply: Outcome);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub sessions: Vec<i64>,
    pub pinned: bool,
}
workspace_call!(Pin, agent: false, reply: Outcome);

/// Delete a session, its settings, reminder, folder entry and recorded turns for good.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delete {
    pub session: i64,
}
workspace_call!(Delete, agent: false, reply: Outcome);

/// Link a session to an issue or pull request, or unlink it. With `if_unlinked`, a session that
/// already has a link keeps it (`Rejected(AlreadyResolved)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetWorkItem {
    pub session: i64,
    pub item: Option<WorkItem>,
    pub if_unlinked: bool,
}
workspace_call!(SetWorkItem, agent: true, reply: Outcome);

/// What [`SetReminder`] does to each session's reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReminderChange {
    /// Schedule it (or reschedule and re-arm it) for this Unix millisecond.
    At(u64),
    /// Move the occurrence due at `expected` to `due_at`, only if it is still that one.
    Snooze { expected: u64, due_at: u64 },
    /// Clear it; with `expected`, only the occurrence due then.
    Clear { expected: Option<u64> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetReminder {
    pub sessions: Vec<i64>,
    pub change: ReminderChange,
}
workspace_call!(SetReminder, agent: false, reply: Outcome);

/// Replace a project's folders and folded groups.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveGroups {
    pub project: String,
    pub groups: Groups,
}
workspace_call!(SaveGroups, agent: true, reply: Outcome);

/// Search every saved message body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSessions {
    pub query: String,
}
workspace_call!(SearchSessions, agent: true, reply: SearchResults);

/// [`SearchSessions`]'s reply.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchResults {
    pub hits: Vec<Hit>,
    /// More matched than the engine returns.
    pub truncated: bool,
}

/// What the sidebar's hover card shows of a session beyond its summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewSession {
    pub session: i64,
}
workspace_call!(PreviewSession, agent: false, reply: Preview);

/// [`PreviewSession`]'s reply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub model: Option<String>,
    pub worktree: Option<String>,
}

/// A viewer showed the session: its finished turns are read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkSeen {
    pub session: i64,
}
workspace_call!(MarkSeen, agent: false, reply: Outcome);

/// Keep a lead's orchestration summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SaveOrchestration {
    pub lead: i64,
    pub summary: Orchestration,
}
workspace_call!(SaveOrchestration, agent: false, reply: Outcome);

/// Name the session's title from its first prompt: `seed` stands in at once (replacing
/// `placeholder` unless the person renamed it), and the generated title replaces `seed` when it
/// arrives. A pasted issue or pull request links an unlinked session. Both reach watchers as
/// [`WorkspaceEvent::Session`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerateTitle {
    pub session: i64,
    pub message: String,
    pub placeholder: String,
    pub seed: String,
    /// Where the provider runs.
    pub cwd: String,
    pub model: TextModel,
}
workspace_call!(GenerateTitle, agent: false, reply: Outcome);

/// Name a new checkout's temporary branch from the first prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NameWorktree {
    pub session: i64,
    /// The project's own checkout.
    pub root: String,
    /// The worktree, which must still be the session's and on its temporary branch.
    pub cwd: String,
    pub message: String,
    pub model: TextModel,
}
workspace_call!(NameWorktree, agent: false, reply: Named);

/// [`NameWorktree`]'s reply: the branch the worktree is on now, when it was renamed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Named {
    pub branch: Option<String>,
}

/// Every note.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListNotes {}
workspace_call!(ListNotes, agent: true, reply: Vec<Note>);

/// A new empty note under the viewer's id, in the project at `cwd` when there is one. The viewer
/// mints the id, so the note opens before the engine answers; an id already in use is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNote {
    pub id: String,
    pub cwd: Option<String>,
}
workspace_call!(CreateNote, agent: false, reply: Note);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UpsertNote {
    pub note: NoteUpsert,
}
workspace_call!(UpsertNote, agent: true, reply: Note);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteNote {
    pub id: String,
}
workspace_call!(DeleteNote, agent: false, reply: Outcome);

/// Keep a response, or a selection of it, as a note.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveResponseNote {
    pub note: ResponseNote,
}
workspace_call!(SaveResponseNote, agent: false, reply: Outcome);

/// Start an automation run, one the scheduler claimed or one the person started: create its
/// session (or take the reusable one, `reuse`) with `settings`, link and file it, and record the
/// run as running. The viewer then sends its prompt. A run the scheduler claimed is taken as the
/// engine holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrepareAutomationRun {
    pub due: Box<DueAutomationRun>,
    pub reuse: Option<i64>,
    pub settings: Box<Settings>,
}
workspace_call!(PrepareAutomationRun, agent: false, reply: Created);

/// Record how a run ended: `succeeded`, `failed` or `cancelled`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishAutomationRun {
    pub run: String,
    pub status: String,
    pub error: Option<String>,
}
workspace_call!(FinishAutomationRun, agent: false, reply: Outcome);

/// Inbox items that appeared: claim the event automations they trigger. Claimed runs arrive as
/// [`WorkspaceEvent::AutomationDue`]; items whose claim failed are retried on the next tick.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimInboxEvents {
    pub items: Vec<crate::inbox::Item>,
}
workspace_call!(ClaimInboxEvents, agent: false, reply: Outcome);
