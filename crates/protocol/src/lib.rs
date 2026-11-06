//! onicode-protocol: what a viewer (the app, another Mac, the iPhone) says to an engine and what
//! the engine answers, as plain serde types with no IO (`docs/onicode/decisions/engine.md`).
//!
//! A [`Call`] is one request and one reply; a [`Watch`] is a snapshot at a [`Cursor`] and then
//! every event after it, as [`Item`]s. Both ride `onicode-rpc`'s JSON envelopes under their
//! `NAME`, the body wrapped in a [`Routed`] that names the target engine and, for a mutating call,
//! its [`OpId`]. [`METHODS`] is the table of them all, pinned with the shape of every type by the
//! golden test in `tests/golden.rs`: change a shape and that test fails until [`PROTOCOL`] is
//! bumped.
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub mod engine;
pub mod files;
pub mod git;
mod ids;
pub mod session;
pub mod workspace;

pub use ids::{EngineId, MessageId, OpId, SessionId, TerminalId};

/// The protocol's version. Peers with different values answer each other only the calls whose
/// shapes are frozen: [`engine::Hello`], [`engine::EngineInfo`] and [`engine::UpdateEngine`].
/// Separate from net's `WIRE`, so a mismatch can still say "Needs update".
pub const PROTOCOL: u32 = 5;

/// One request and its reply.
pub trait Call {
    /// The envelope's `method`.
    const NAME: &'static str;
    /// Whether an agent's `onicode mcp` may make this call with its scoped token.
    const AGENT: bool = false;
    type Params: Serialize + DeserializeOwned;
    type Reply: Serialize + DeserializeOwned;
}

/// A snapshot and then every event after it, as a stream of [`Item`]s.
pub trait Watch {
    /// The envelope's `method`.
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned;
    type State: Serialize + DeserializeOwned;
    type Event: Serialize + DeserializeOwned;
}

/// A place in one session's events: `seq` counts them, and `epoch` names the engine run that
/// numbered them, so a cursor from before a restart never matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor {
    pub epoch: u64,
    pub seq: u64,
}

/// One item of a [`Watch`]'s stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Item<S, E> {
    /// The whole state as of `at`.
    Snapshot { at: Cursor, state: S },
    /// The events numbered `first` to `last`, in order. A slow watcher's queue merges what it
    /// holds (engine.md "Backpressure"), so there can be fewer events than numbers; the watcher's
    /// cursor moves to `last`. An `Events` with `last == first - 1` and no events says the
    /// watcher missed nothing.
    Events {
        first: u64,
        last: u64,
        events: Vec<E>,
    },
    /// The watcher fell behind or its cursor is gone: resubscribe with the last cursor it holds.
    Resync,
}

/// A call's or a watch's params on their way to an engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routed<T> {
    /// The engine that serves it; `None` is the engine this connection reaches.
    pub engine: Option<EngineId>,
    /// The mutating call's id, minted by the viewer: a repeat gets the first receipt.
    pub op: Option<OpId>,
    pub body: T,
}

/// What became of a mutating call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Applied,
    Rejected(Rejection),
}

/// Why a mutating call was not applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rejection {
    /// Another device answered first.
    AlreadyResolved,
    /// The edit was made against a revision that has since moved.
    StaleRev,
    /// What it acts on no longer exists.
    Gone,
    /// Refused, with why.
    Denied(String),
}

/// Whether a [`Method`] is a [`Call`] or a [`Watch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Call,
    Watch,
}

/// One row of [`METHODS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Method {
    pub name: &'static str,
    pub kind: MethodKind,
    /// [`Call::AGENT`]; watches are never an agent's.
    pub agent: bool,
}

const fn call<C: Call>() -> Method {
    Method {
        name: C::NAME,
        kind: MethodKind::Call,
        agent: C::AGENT,
    }
}

const fn watch<W: Watch>() -> Method {
    Method {
        name: W::NAME,
        kind: MethodKind::Watch,
        agent: false,
    }
}

/// Every method an engine serves, by group in the order of engine.md's table.
pub const METHODS: &[Method] = &[
    // Engine.
    call::<engine::Hello>(),
    call::<engine::EngineInfo>(),
    call::<engine::UpdateEngine>(),
    call::<engine::StopEngine>(),
    // Session.
    watch::<session::WatchSession>(),
    call::<session::Send>(),
    call::<session::Steer>(),
    call::<session::Stop>(),
    call::<session::Decide>(),
    call::<session::Answer>(),
    call::<session::KeepQuestionOpen>(),
    call::<session::QueueEdit>(),
    call::<session::SendNow>(),
    call::<session::SteerNow>(),
    call::<session::Configure>(),
    call::<session::EditLastTurn>(),
    call::<session::Rewind>(),
    call::<session::Fork>(),
    call::<session::Btw>(),
    call::<session::SwitchProvider>(),
    call::<session::ReadTranscript>(),
    call::<session::ReadSession>(),
    call::<session::SaveQueue>(),
    call::<session::RecallLastTurn>(),
    call::<session::ReadBtw>(),
    call::<session::SaveBtw>(),
    call::<session::StopBtw>(),
    // Workspace.
    watch::<workspace::WatchWorkspace>(),
    call::<workspace::CreateSession>(),
    call::<workspace::Rename>(),
    call::<workspace::Archive>(),
    call::<workspace::Pin>(),
    call::<workspace::Delete>(),
    call::<workspace::SetWorkItem>(),
    call::<workspace::SetReminder>(),
    call::<workspace::SaveGroups>(),
    call::<workspace::SearchSessions>(),
    call::<workspace::PreviewSession>(),
    call::<workspace::MarkSeen>(),
    call::<workspace::SaveOrchestration>(),
    call::<workspace::GenerateTitle>(),
    call::<workspace::NameWorktree>(),
    call::<workspace::ListNotes>(),
    call::<workspace::CreateNote>(),
    call::<workspace::UpsertNote>(),
    call::<workspace::DeleteNote>(),
    call::<workspace::SaveResponseNote>(),
    call::<workspace::PrepareAutomationRun>(),
    call::<workspace::FinishAutomationRun>(),
    call::<workspace::ClaimInboxEvents>(),
    // Files.
    watch::<files::WatchDir>(),
    call::<files::ListDir>(),
    call::<files::Stat>(),
    call::<files::Read>(),
    call::<files::Write>(),
    call::<files::Create>(),
    call::<files::Move>(),
    call::<files::DeletePath>(),
    call::<files::SearchPaths>(),
    call::<files::SearchContent>(),
    call::<files::ListDrives>(),
    call::<files::SearchHomeFolders>(),
    call::<files::SaveAttachments>(),
    call::<files::SaveNoteImages>(),
    // Git.
    watch::<git::WatchGit>(),
    call::<git::ReadStatus>(),
    call::<git::Diff>(),
    call::<git::FileDiff>(),
    call::<git::Branches>(),
    call::<git::Refs>(),
    call::<git::History>(),
    call::<git::Switch>(),
    call::<git::CreateBranch>(),
    call::<git::Stage>(),
    call::<git::Unstage>(),
    call::<git::Discard>(),
    call::<git::Commit>(),
    call::<git::Push>(),
    call::<git::Pull>(),
    call::<git::Sync>(),
    call::<git::HeadMessage>(),
    call::<git::CommitContext>(),
    call::<git::PrContext>(),
    call::<git::PrStatus>(),
    call::<git::CreatePr>(),
    call::<git::Worktrees>(),
    call::<git::CreateWorktree>(),
    call::<git::RemoveWorktree>(),
    call::<git::ReadCheckpoint>(),
    call::<git::RestoreCheckpoint>(),
    call::<git::ReadSessionChanges>(),
];

/// The row of [`METHODS`] named `name`.
#[must_use]
pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|method| method.name == name)
}

/// The data types the UI renders. They live in `onicode-core` until step 6 of the engine plan
/// moves them here; the paths below are where they will stay.
pub mod agent {
    pub use onicode_core::agent::{
        AccessMode, Approval, Handoff, ProviderSwitch, Question, QuestionItem, QuestionOption,
        Settings, TurnInfo, TurnMetrics,
    };
}

/// The account a session or a background chore runs on.
pub mod accounts {
    pub use onicode_core::accounts::Profile;
}

/// Scheduled and event-triggered runs.
pub mod automations {
    pub use onicode_core::automations::{
        Automation, AutomationRun, AutomationTrigger, DueAutomationRun,
    };
}

/// The context window's use, as the UI shows it.
pub mod context {
    pub use onicode_core::context::Usage;
}

/// The queue of messages typed while a turn runs.
pub mod follow_up {
    pub use onicode_core::follow_up::{Message, State};
}

/// An issue, pull request or work item a tracker lists.
pub mod inbox {
    pub use onicode_core::inbox::{Item, Label, Person};
}

/// Notes, and a response kept as one.
pub mod notes {
    pub use onicode_core::notes::{Note, NoteUpsert};
    pub use onicode_core::response::ResponseNote;
}

/// What the provider offers and leaves running.
pub mod provider {
    pub use onicode_core::provider::{BackgroundTask, Kind, NativeCommand, NativeSubcommand};
}

/// A search over saved conversations.
pub mod search {
    pub use onicode_core::search::{Hit, Target};
}

/// What the sidebar shows of a session: its stored summary, folders, reminders, linked work
/// item and orchestration, and the model background chores such as titles run on.
pub mod sessions {
    pub use onicode_core::orchestration::{Summary as Orchestration, SummaryTask};
    pub use onicode_core::session_groups::{Folder, Groups, WorkItem};
    pub use onicode_core::session_title::TextModel;
    pub use onicode_core::store::{Reminder, Session};
}

/// A transcript's rows.
pub mod transcript {
    pub use onicode_core::transcript::Row;
}

/// The provider's usage limit.
pub mod usage {
    pub use onicode_core::usage::Limit;
}
