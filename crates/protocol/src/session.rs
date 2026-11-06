//! The Session group: one session's state and events ([`WatchSession`]) and the commands that
//! drive its turns. Every command but [`ReadTranscript`] is mutating: it travels with an
//! [`OpId`] in its [`Routed`](crate::Routed) and replies an [`Outcome`], which a
//! [`SessionEvent::Receipt`] repeats to every watcher.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::{Approval, Handoff, Question, Settings, TurnInfo, TurnMetrics};
use crate::provider::{BackgroundTask, NativeCommand};
use crate::transcript::Row;
use crate::{Call, Cursor, MessageId, OpId, Outcome, SessionId, Watch, context, follow_up, usage};

/// Where the running turn stands: `turn::Update`'s `provider_turn_id`, `turn_ready` and
/// `finished`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnState {
    /// The provider's id for the turn, once it reported one.
    pub provider_turn_id: Option<String>,
    /// The provider takes steers now.
    pub ready: bool,
    /// The turn ended.
    pub finished: bool,
}

/// One change to a session, in the order the host made it. `turn::Update` split into one
/// variant per field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionEvent {
    /// Replace the rows from `start` on with `rows`.
    Rows {
        start: usize,
        rows: Vec<Row>,
    },
    /// Row `row`'s text grew by `text`: the streaming fast path.
    Append {
        row: usize,
        text: String,
    },
    Status {
        status: String,
        turn: Option<TurnState>,
    },
    Approval(Approval),
    /// The approval with this id was answered.
    ApprovalResolved(String),
    Question(Question),
    /// The question with this id was answered.
    QuestionResolved(String),
    /// The queue at revision `rev`.
    Queue {
        rev: u64,
        state: follow_up::State,
    },
    /// Boxed: the settings are by far the largest variant.
    Settings(Box<Settings>),
    /// A title the host set.
    Title(String),
    ProviderId(String),
    Model(String),
    /// The worktree the session now runs in.
    Worktree(String),
    Context(context::Usage),
    Metrics(TurnMetrics),
    UsageLimit(Option<usage::Limit>),
    /// The tasks a yielded turn still waits on.
    Background(Vec<BackgroundTask>),
    Commands(Vec<NativeCommand>),
    Handoff(Option<Handoff>),
    TurnFinished {
        turn: usize,
        info: TurnInfo,
    },
    /// What became of a mutating call: what a pending echo waits for.
    Receipt {
        op: OpId,
        outcome: Outcome,
    },
}

/// A session as of a [`Cursor`]: what a watcher starts from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionState {
    /// The transcript index of `rows[0]`: a snapshot holds the last `window_turns` turns, and
    /// [`ReadTranscript`] reads the ones before.
    pub first_row: usize,
    pub rows: Vec<Row>,
    pub status: String,
    pub turn: Option<TurnState>,
    pub approvals: Vec<Approval>,
    pub questions: Vec<Question>,
    pub queue_rev: u64,
    pub queue: follow_up::State,
    pub settings: Settings,
    pub title: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub worktree: Option<String>,
    pub context: Option<context::Usage>,
    pub usage_limit: Option<usage::Limit>,
    pub background: Vec<BackgroundTask>,
    pub commands: Vec<NativeCommand>,
    pub handoff: Option<Handoff>,
}

/// Watch one session: a snapshot (or, with a `since` the host's ring still covers, only the
/// events after it), then every event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchSession {
    pub session: SessionId,
    /// The last cursor this viewer holds, to resume from.
    pub since: Option<Cursor>,
    /// How many of the latest turns a snapshot holds.
    pub window_turns: usize,
}

impl Watch for WatchSession {
    type Event = SessionEvent;
    type Params = Self;
    type State = SessionState;

    const NAME: &'static str = "WatchSession";
}

macro_rules! session_call {
    ($name:ident,agent: $agent:literal,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const AGENT: bool = $agent;
            const NAME: &'static str = stringify!($name);
        }
    };
}

/// Send a prompt: it starts a turn, or queues behind the running one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Send {
    pub session: SessionId,
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<PathBuf>,
}
session_call!(Send, agent: true, reply: Outcome);

/// Steer the running turn with a message, without stopping it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steer {
    pub session: SessionId,
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<PathBuf>,
}
session_call!(Steer, agent: true, reply: Outcome);

/// Stop the running turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub session: SessionId,
}
session_call!(Stop, agent: false, reply: Outcome);

/// The answer to an approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Allow,
    /// Allow, and stop asking for the rest of the session.
    AllowForSession,
    Deny,
}

/// Answer an approval. The first answer to reach the host wins; a later one is
/// `Rejected(AlreadyResolved)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decide {
    pub session: SessionId,
    pub approval: String,
    pub answer: Verdict,
}
session_call!(Decide, agent: false, reply: Outcome);

/// Answer a question: per item, the chosen option ids or typed answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub session: SessionId,
    pub question: String,
    pub answers: Vec<Vec<String>>,
}
session_call!(Answer, agent: false, reply: Outcome);

/// Keep a question open past its automatic resolution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepQuestionOpen {
    pub session: SessionId,
    pub question: String,
}
session_call!(KeepQuestionOpen, agent: false, reply: Outcome);

/// One edit of the queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueChange {
    /// Queue a new message under the edit's id.
    Add {
        text: String,
        attachments: Vec<PathBuf>,
        steer: bool,
    },
    /// Retype the message.
    Text {
        text: String,
        attachments: Vec<PathBuf>,
    },
    /// Move the message to index `to`.
    Move {
        to: usize,
    },
    Remove,
}

/// Edit the queued message `id`, as seen at revision `base_rev`. An edit made against an older
/// revision is `Rejected(StaleRev)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEdit {
    pub session: SessionId,
    pub id: MessageId,
    pub base_rev: u64,
    pub edit: QueueChange,
}
session_call!(QueueEdit, agent: false, reply: Outcome);

/// Stop the running turn and send the queued message `id` now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendNow {
    pub session: SessionId,
    pub id: MessageId,
}
session_call!(SendNow, agent: false, reply: Outcome);

/// Steer the queued message `id` into the running turn now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerNow {
    pub session: SessionId,
    pub id: MessageId,
}
session_call!(SteerNow, agent: false, reply: Outcome);

/// Replace the session's agent settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Configure {
    pub session: SessionId,
    pub settings: Settings,
}
session_call!(Configure, agent: false, reply: Outcome);

/// Replace the last turn's prompt and run it again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditLastTurn {
    pub session: SessionId,
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<PathBuf>,
}
session_call!(EditLastTurn, agent: false, reply: Outcome);

/// Drop every turn from `turn` on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewind {
    pub session: SessionId,
    pub turn: usize,
}
session_call!(Rewind, agent: false, reply: Outcome);

/// A new session `into` holding this one's turns up to and including `turn`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub session: SessionId,
    pub turn: usize,
    pub into: SessionId,
}
session_call!(Fork, agent: false, reply: Outcome);

/// Ask an aside about the session without adding to its turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Btw {
    pub session: SessionId,
    pub message: MessageId,
    pub text: String,
}
session_call!(Btw, agent: false, reply: Outcome);

/// Hand the session to another provider, account or model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchProvider {
    pub session: SessionId,
    pub provider: String,
    pub account: String,
    pub model: Option<String>,
}
session_call!(SwitchProvider, agent: false, reply: Outcome);

/// Read `turns` turns before `before_turn` (the latest when `None`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadTranscript {
    pub session: SessionId,
    pub before_turn: Option<usize>,
    pub turns: usize,
}
session_call!(ReadTranscript, agent: true, reply: TranscriptPage);

/// Rows read by [`ReadTranscript`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPage {
    /// The transcript index of `rows[0]`.
    pub first_row: usize,
    pub rows: Vec<Row>,
    /// Earlier turns remain.
    pub more: bool,
}
