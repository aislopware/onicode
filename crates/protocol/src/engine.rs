//! The Engine group: greeting, describing, updating and stopping an engine, and naming the
//! sessions the app still knows by row. [`Hello`], [`EngineInfo`] and [`UpdateEngine`] are
//! frozen: their shapes never change and use only primitives and [`EngineId`], so engines on
//! different [`PROTOCOL`](crate::PROTOCOL)s can still greet each other and offer an update.
use serde::{Deserialize, Serialize};

use crate::{Call, EngineId, Outcome, SessionId};

/// The first call on every connection, sent by the side that opened it and answered with the
/// other side's own. Frozen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The sender's [`PROTOCOL`](crate::PROTOCOL).
    pub protocol: u32,
    /// The sender's build, as its release feed names it.
    pub version: String,
    /// What the sender does beyond its protocol, by name.
    pub capabilities: Vec<String>,
}

impl Call for Hello {
    type Params = Self;
    type Reply = Self;

    const AGENT: bool = true;
    const NAME: &'static str = "Hello";
}

/// Describe the engine. Frozen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {}

impl Call for EngineInfo {
    type Params = Self;
    type Reply = EngineDetails;

    const AGENT: bool = true;
    const NAME: &'static str = "EngineInfo";
}

/// [`EngineInfo`]'s reply. Frozen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineDetails {
    pub engine: EngineId,
    /// The machine's name, as the person named it.
    pub name: String,
    pub version: String,
    pub protocol: u32,
}

/// Update the engine to `version`, or to the newest release when `None`. Frozen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEngine {
    pub version: Option<String>,
}

impl Call for UpdateEngine {
    type Params = Self;
    type Reply = UpdateStarted;

    const NAME: &'static str = "UpdateEngine";
}

/// [`UpdateEngine`]'s reply. Frozen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStarted {
    /// Whether an update began; when it did not, `detail` says why.
    pub started: bool,
    /// The version it is updating to, or why it is not.
    pub detail: String,
}

/// Stop the engine, and the agents it runs with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopEngine {}

impl Call for StopEngine {
    type Params = Self;
    type Reply = Outcome;

    const NAME: &'static str = "StopEngine";
}

/// The [`SessionId`] of the session the engine stores under `row`, `None` when no session is
/// stored there. The app's views name sessions by row until `SessionKey` replaces it (engine.md,
/// migration step 9); the row stays the engine's, so a viewer asks rather than reads the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveSession {
    pub row: i64,
}

impl Call for ResolveSession {
    type Params = Self;
    type Reply = Option<SessionId>;

    const NAME: &'static str = "ResolveSession";
}
