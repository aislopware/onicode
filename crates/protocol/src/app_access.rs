//! The App access group: the store side of Operator's app actions. An agent's `onicode app` call
//! reaches the viewer that runs its thread, which checks the thread may make it and shows its
//! effect; what the action reads or writes in the engine's store comes here.
//!
//! The actions' inputs and replies are the Operator tool's JSON, carried as `String`s: the
//! engine and the tool agree on them, and a viewer passes them through. Every mutating call
//! travels with an [`OpId`](crate::OpId) in its [`Routed`](crate::Routed), and a repeat of the op
//! is answered with the first reply; an action's own request id makes it idempotent across
//! viewers on top of that.
use serde::{Deserialize, Serialize};

use crate::accounts::Profile;
use crate::agent::Settings;
use crate::provider::Kind;
use crate::sessions::Session;
use crate::{Call, Outcome};

macro_rules! app_call {
    ($name:ident,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const NAME: &'static str = stringify!($name);
        }
    };
}

/// The reply kept for a mutating action of the thread at `session` under its request id: the
/// action's fingerprint and its result, as the JSON `[fingerprint, result]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAppReceipt {
    pub session: i64,
    pub request: String,
}
app_call!(ReadAppReceipt, reply: Option<String>);

/// Keep a mutating action's reply, before the agent hears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveAppReceipt {
    pub session: i64,
    pub request: String,
    pub receipt: String,
}
app_call!(SaveAppReceipt, reply: Outcome);

/// `models.list`: every provider's models, each read for the account given for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppModels {
    pub profiles: Vec<(Kind, Profile)>,
}
app_call!(AppModels, reply: String);

/// `sessions.start`: a session in `project` that the thread at `caller` starts with `prompt`
/// waiting in its composer, configured from `settings` (the caller's, with the account for
/// `kind`) and the action's `input`. One per request id: a repeat finds the first.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartAppSession {
    pub caller: i64,
    pub request: String,
    pub fingerprint: String,
    /// The action's input, as JSON.
    pub input: String,
    /// The caller's provider.
    pub provider: String,
    pub kind: Kind,
    pub project: String,
    pub prompt: String,
    pub settings: Settings,
}

/// [`StartAppSession`]'s reply: the action's result as JSON, and the session with its settings
/// when this call made it, for the viewer to open.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppStarted {
    pub value: String,
    pub started: Option<(Session, Settings)>,
}
app_call!(StartAppSession, reply: AppStarted);

/// `sessions.read`: a page of the session's conversation, as the action's `input` asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAppConversation {
    pub session: i64,
    /// The action's input, as JSON.
    pub input: String,
    pub busy: bool,
    pub draft: bool,
}
app_call!(ReadAppConversation, reply: String);

/// `notes.list`, `notes.read` and `notes.write` over the notes of `projects`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppNotes {
    pub caller: i64,
    pub request: String,
    pub action: String,
    /// The action's input, as JSON.
    pub input: String,
    pub projects: Vec<String>,
}
app_call!(AppNotes, reply: String);
