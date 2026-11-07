//! The Engine group: greeting, describing, updating and stopping an engine, naming the sessions
//! the app still knows by row, and the machines the engine reaches over the tailnet
//! ([`WatchMachines`]). [`Hello`], [`EngineInfo`] and [`UpdateEngine`] are frozen: their shapes
//! never change and use only primitives and [`EngineId`], so engines on different
//! [`PROTOCOL`](crate::PROTOCOL)s can still greet each other and offer an update.
use serde::{Deserialize, Serialize};

use crate::{Call, Cursor, EngineId, Outcome, SessionId, Watch};

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

/// How busy an engine is: what the machines list shows beside it. Asked of a peer's engine by the
/// engine that lists it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineLoad {}

impl Call for EngineLoad {
    type Params = Self;
    type Reply = Load;

    const NAME: &'static str = "EngineLoad";
}

/// [`EngineLoad`]'s reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Load {
    /// The sessions with a turn running now.
    pub live_agents: u32,
}

/// Watch the machines this engine knows: itself, every engine the tailnet has, and every device
/// linked to it now (a phone), with how each link travels. The app's own engine answers it; it is
/// not forwarded in practice, since only the local engine knows its own links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchMachines {
    pub since: Option<Cursor>,
}

impl Watch for WatchMachines {
    type Event = MachineEvent;
    type Params = Self;
    type State = Machines;

    const NAME: &'static str = "WatchMachines";
}

/// Every machine, sorted by [`Machine::engine`], so the list reads the same however its events
/// arrived.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machines {
    pub machines: Vec<Machine>,
}

impl Machines {
    /// Applies `event`: a machine replaced or added in its place, or one gone.
    pub fn apply(&mut self, event: &MachineEvent) {
        match event {
            MachineEvent::Changed(machine) => {
                match self
                    .machines
                    .binary_search_by(|held| held.engine.cmp(&machine.engine))
                {
                    Ok(at) => self.machines[at] = machine.clone(),
                    Err(at) => self.machines.insert(at, machine.clone()),
                }
            }
            MachineEvent::Gone(engine) => {
                if let Ok(at) = self
                    .machines
                    .binary_search_by(|held| held.engine.cmp(engine))
                {
                    self.machines.remove(at);
                }
            }
        }
    }

    /// The machine `engine`, if listed.
    #[must_use]
    pub fn get(&self, engine: &EngineId) -> Option<&Machine> {
        self.machines
            .binary_search_by(|held| held.engine.cmp(engine))
            .ok()
            .and_then(|at| self.machines.get(at))
    }
}

/// What changed in [`Machines`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineEvent {
    /// The machine as it is now, new or changed.
    Changed(Machine),
    /// The machine is no longer listed: off the tailnet, or a phone that unlinked.
    Gone(EngineId),
}

impl MachineEvent {
    /// The machine the event is about.
    #[must_use]
    pub const fn engine(&self) -> &EngineId {
        match self {
            Self::Changed(machine) => &machine.engine,
            Self::Gone(engine) => engine,
        }
    }
}

/// One machine of the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub engine: EngineId,
    /// Its tailnet name, or its host name for this engine off the tailnet.
    pub name: String,
    pub kind: MachineKind,
    pub link: MachineLink,
    /// How the link travels, as this engine's tailnet daemon reports it.
    pub path: Option<LinkPath>,
    /// The link's smoothed round trip, in microseconds.
    pub rtt_us: Option<u64>,
    /// Its build, as its hello names it.
    pub version: String,
    /// Its [`PROTOCOL`](crate::PROTOCOL); `None` when its hello did not say. One that differs from
    /// this engine's is answered only the frozen calls: "Needs update".
    pub protocol: Option<u32>,
    /// The same tailnet user's machine, rather than a node granted to it.
    pub owner: bool,
    /// Sessions with a turn running, as it last said; `None` until it answers.
    pub live_agents: Option<u32>,
}

/// What a machine is to this engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineKind {
    /// This engine.
    This,
    /// Another engine.
    Engine,
    /// A phone or another viewer that serves nothing, linked to this engine now.
    Viewer,
}

/// Where this engine's link to a machine stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineLink {
    /// This engine itself.
    Here,
    /// Linked now.
    Live,
    /// Being dialled.
    Dialing,
    /// Found on the tailnet, not linked: dialled when something is sent to it.
    Found,
    /// Not reachable; the next dial is due in `retry_in_ms`.
    Offline { reason: String, retry_in_ms: u64 },
}

/// How a link travels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPath {
    /// Straight over UDP, to this address.
    Direct(String),
    /// Through another node acting as a relay.
    PeerRelay(String),
    /// Through a DERP server in this region.
    Derp(String),
    /// Nothing flowed lately, so the daemon has chosen no path.
    Idle,
}
