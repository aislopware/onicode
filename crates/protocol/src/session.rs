//! The Session group: one session's state and events ([`WatchSession`]) and the commands that
//! drive its turns. Every command but the reads ([`ReadTranscript`], [`ReadSession`],
//! [`RecallLastTurn`] and [`ReadBtw`]) is mutating: it travels with an [`OpId`] in its
//! [`Routed`](crate::Routed) and replies an [`Outcome`] (or a reply holding one), which a
//! [`SessionEvent::Receipt`] repeats to every watcher.
use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::accounts::Profile;
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
    /// The provider thread the session continues; `None` once it let its thread go.
    ProviderId(Option<String>),
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
    /// The answer to the by-the-way thread `thread` grew by `text` ([`Btw`]). Not kept in the
    /// state: the call's reply holds the whole answer.
    BtwText {
        thread: String,
        text: String,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Send {
    pub session: SessionId,
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<PathBuf>,
    /// What the viewer's composer prepared for the turn; `None` runs it under the session's
    /// stored settings. The engine ignores it from an agent's scoped token.
    #[serde(default)]
    pub turn: Option<Box<TurnContext>>,
}
session_call!(Send, agent: true, reply: Outcome);

/// A turn as the viewer's composer prepared it: the settings it runs under, which the viewer
/// may not have saved yet, and the context those settings never save (`Settings`' transport-only
/// fields). The composer stays in the viewer (engine.md, "Boundary"), so this is how its work
/// reaches the engine that runs the turn.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnContext {
    /// The session as the viewer holds it; its `id` must be the session's own row.
    pub summary: crate::sessions::Session,
    pub settings: Settings,
    /// The prompt with the transport-only context (selected MCP servers, completions); the raw
    /// text stays the transcript's and the queue's label.
    pub prepared_prompt: Option<String>,
    /// The model identity captured as the turn was submitted.
    pub turn_model: Option<TurnInfo>,
    /// Environment the viewer adds to the provider's run.
    pub environment: HashMap<String, String>,
    /// The queued message this turn sends, by its id.
    pub queued: Option<String>,
    /// A message the app sent on the person's behalf, shown as such.
    pub internal: bool,
}

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

/// Send the queued message `id` as the next turn now: it leaves the stored queue, and a running
/// turn is stopped first, the message following when it ends. Its receipt arrives with that
/// turn's first update, as a [`Send`]'s does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendNow {
    pub session: SessionId,
    pub id: MessageId,
}
session_call!(SendNow, agent: false, reply: Outcome);

/// Steer the queued message `id` into the running turn now: it leaves the stored queue, and
/// returns to its head, paused with the error, if the provider refuses it. Its receipt arrives
/// when the provider accepts or refuses it, as a [`Steer`]'s does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerNow {
    pub session: SessionId,
    pub id: MessageId,
}
session_call!(SteerNow, agent: false, reply: Outcome);

/// Replace the session's provider and agent settings, unless the stored settings are newer by
/// `revision` (`Rejected(StaleRev)`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Configure {
    pub session: SessionId,
    pub provider: String,
    pub settings: Settings,
}
session_call!(Configure, agent: false, reply: Outcome);

/// Read what the session's store holds besides its rows: what a viewer opening it starts from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSession {
    pub session: SessionId,
}
session_call!(ReadSession, agent: false, reply: SessionRecord);

/// [`ReadSession`]'s reply.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRecord {
    /// `None` until the session's settings were first saved.
    pub settings: Option<Box<Settings>>,
    pub context: Option<context::Usage>,
    pub queue: follow_up::State,
    /// Why the session takes no more turns: an edit changed the provider's context and the
    /// transcript could not follow.
    pub edit_recovery: Option<String>,
}

/// Replace the session's queue and saved draft whole, keeping copies of their attachments with
/// the session. Replies with the queue as saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveQueue {
    pub session: SessionId,
    pub queue: follow_up::State,
}
session_call!(SaveQueue, agent: false, reply: follow_up::State);

/// Recall the last turn for editing: its prompt goes back to the viewer's composer, and the
/// engine holds the edit for [`EditLastTurn`]. `turn` is the provider's id of the turn, when the
/// viewer knows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallLastTurn {
    pub session: SessionId,
    pub turn: Option<String>,
}
session_call!(RecallLastTurn, agent: false, reply: RecalledTurn);

/// [`RecallLastTurn`]'s reply: the turn as the store holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecalledTurn {
    /// The transcript index the edit drops rows from.
    pub start: usize,
    pub prompt: String,
    pub attachments: Vec<PathBuf>,
    pub provider: String,
    pub provider_id: String,
    pub account_id: String,
    pub cwd: String,
    /// The whole transcript the edit was recalled against.
    pub rows: Vec<Row>,
}

/// Rewind the recalled last turn in the provider, then drop it from the transcript, so the
/// viewer's next [`Send`] replaces it. `attachments` are the edited prompt's, checked first;
/// `model` is the one the provider rewinds with. Without a recalled edit it is `Rejected(Gone)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditLastTurn {
    pub session: SessionId,
    pub attachments: Vec<PathBuf>,
    pub model: Option<String>,
}
session_call!(EditLastTurn, agent: false, reply: EditedTurn);

/// [`EditLastTurn`]'s reply. A refusal that begins "Provider recovery required:" left the
/// provider's context changed: the session takes no more turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditedTurn {
    pub outcome: Outcome,
    /// The provider session the transcript is bound to now.
    pub provider_id: Option<String>,
}

/// Drop every turn from `turn` (a transcript row index where a prompt starts) on. When the dropped
/// turn is the provider thread's last and the provider rewinds natively (Codex, Pi), its thread
/// rewinds too; otherwise the session leaves its provider thread, and its next prompt starts a new
/// one carrying the kept turns as a handoff brief. `Rejected(Gone)` when `turn` is past the end or
/// not a prompt, `Denied` while a turn runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewind {
    pub session: SessionId,
    pub turn: usize,
}
session_call!(Rewind, agent: false, reply: Outcome);

/// A new session `into` (the viewer's id), in the same project on the same provider, account and
/// settings, holding this one's turns before `turn` (a transcript row index where a prompt starts;
/// past the end keeps every turn). It starts with no provider thread: its next prompt carries the
/// kept turns as a handoff brief. The new session arrives in the workspace like any other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub session: SessionId,
    pub turn: usize,
    pub into: SessionId,
}
session_call!(Fork, agent: false, reply: Outcome);

/// A by-the-way thread: an aside about the session, on a provider of its own, that never joins
/// the session's turns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwThread {
    pub id: String,
    /// The transcript index of the turn it asks about.
    pub source_turn: usize,
    /// The session as the thread saw it, the reference its answers start from.
    pub snapshot: String,
    pub provider: String,
    pub model: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub account: Profile,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub model_settings: HashMap<String, String>,
    pub rows: Vec<Row>,
    pub draft: String,
    /// `draft`, `running`, `completed`, `stopped` or `error`.
    pub status: String,
    pub error: Option<String>,
}

/// A session's by-the-way threads, at a revision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwThreads {
    pub revision: u64,
    pub threads: Vec<BtwThread>,
}

/// Read the session's by-the-way threads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadBtw {
    pub session: SessionId,
}
session_call!(ReadBtw, agent: false, reply: BtwThreads);

/// Keep the session's by-the-way threads, unless the stored ones are at this revision or newer
/// (`Rejected(StaleRev)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveBtw {
    pub session: SessionId,
    pub threads: BtwThreads,
}
session_call!(SaveBtw, agent: false, reply: Outcome);

/// Ask an aside about the session without adding to its turns: answer `thread`, whose rows end
/// with the question. The answer streams to watchers as [`SessionEvent::BtwText`], is kept with
/// the thread, and comes whole in the reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Btw {
    pub session: SessionId,
    pub thread: BtwThread,
}
session_call!(Btw, agent: false, reply: BtwAnswer);

/// [`Btw`]'s reply: `Rejected(Denied("Stopped"))` when it was stopped. A repeat answered from
/// the ledger carries no `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtwAnswer {
    pub outcome: Outcome,
    pub text: Option<String>,
}

/// Stop the by-the-way thread `thread`'s running answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopBtw {
    pub session: SessionId,
    pub thread: String,
}
session_call!(StopBtw, agent: false, reply: Outcome);

/// Hand the session to `provider`: its provider thread is let go, or with `thread`, bound to that
/// one (switching back to the provider a pending switch came from). `context` is the context
/// meter to keep for the new model. The composer's own settings (the pending switch, the handoff
/// brief, the account and model) follow in a [`Configure`]. Publishes `ProviderId`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchProvider {
    pub session: SessionId,
    pub provider: String,
    pub thread: Option<String>,
    pub context: Option<context::Usage>,
}
session_call!(SwitchProvider, agent: false, reply: Outcome);

/// Let the session's provider thread go, if it is still `expected`: the account it ran on changed
/// and the thread could not move with it. Publishes `ProviderId`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetThread {
    pub session: SessionId,
    pub expected: Option<String>,
}
session_call!(ResetThread, agent: false, reply: Outcome);

/// Compact the provider thread's context: the provider's own compaction, recorded as a
/// `contextCompaction` tool row at `at` under the tool id `tool`. `settings` are the composer's
/// (the engine sets the account's home and Claude hooks itself). [`Stop`] stops it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Compact {
    pub session: SessionId,
    pub at: usize,
    pub tool: String,
    pub model: Option<String>,
    pub settings: Settings,
}
session_call!(Compact, agent: false, reply: Compacted);

/// What a [`Compact`] came to: the tool row as it ended (completed, failed or cancelled), the
/// context meter after it, the commands the provider announced, and why it failed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Compacted {
    pub row: Row,
    pub context: context::Usage,
    pub commands: Option<Vec<NativeCommand>>,
    pub error: Option<String>,
}

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
