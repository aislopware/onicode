//! Every group the engine serves, as one `onicode-rpc` service.
use std::sync::Arc;

use async_trait::async_trait;
use onicode_protocol::engine::{
    EngineDetails, EngineInfo, Hello, ResolveSession, StopEngine, UpdateEngine, UpdateStarted,
};
use onicode_protocol::files::StopSearch;
use onicode_protocol::{Call, EngineId, OpId, Outcome, PROTOCOL, Routed};
use onicode_rpc::{RpcError, RpcReply, RpcService, parse_params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Notify;

use crate::files::FilesService;
use crate::git::GitService;
use crate::session::{SessionHub, SessionService};
use crate::workspace::WorkspaceService;

/// This engine's build, as `Hello` and `EngineInfo` name it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A group of calls and watches that is not one of the first four: it says which methods it
/// serves, and serves them.
pub trait Group: RpcService {
    /// Whether `method` is one of this group's.
    fn serves(&self, method: &str) -> bool;
}

/// The Engine group: the greeting, what the engine is, updating and stopping it, and the
/// sessions the app still names by row.
pub struct EngineGroup {
    session: Arc<SessionHub>,
    stop: Arc<Notify>,
}

impl EngineGroup {
    /// The group over `session`'s rows, telling `stop` when a viewer stops the engine.
    #[must_use]
    pub fn new(session: Arc<SessionHub>, stop: Arc<Notify>) -> Self {
        Self { session, stop }
    }
}

/// The engine's own greeting.
#[must_use]
pub fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL,
        version: VERSION.into(),
        capabilities: Vec::new(),
    }
}

/// The machine's name, as its host name says; the tailnet's device name replaces it with peers
/// (engine.md, migration step 9).
fn machine_name() -> String {
    let mut name = [0u8; 256];
    // SAFETY: `gethostname` writes at most `name.len()` bytes into the buffer it is given.
    let written = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) };
    if written != 0 {
        return "This Mac".into();
    }
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    let host = String::from_utf8_lossy(name.get(..end).unwrap_or_default()).into_owned();
    let host = host.strip_suffix(".local").unwrap_or(&host).to_owned();
    if host.is_empty() {
        "This Mac".into()
    } else {
        host
    }
}

const ENGINE_METHODS: [&str; 5] = [
    Hello::NAME,
    EngineInfo::NAME,
    UpdateEngine::NAME,
    StopEngine::NAME,
    ResolveSession::NAME,
];

impl Group for EngineGroup {
    fn serves(&self, method: &str) -> bool {
        ENGINE_METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for EngineGroup {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == Hello::NAME {
            // The peer's own greeting: the socket's policy reads it before this (`host.rs`).
            routed::<Hello>(params)?;
            return RpcReply::value(&hello());
        }
        if method == EngineInfo::NAME {
            routed::<EngineInfo>(params)?;
            let name = machine_name();
            return RpcReply::value(&EngineDetails {
                engine: EngineId(name.clone()),
                name,
                version: VERSION.into(),
                protocol: PROTOCOL,
            });
        }
        if method == UpdateEngine::NAME {
            routed::<UpdateEngine>(params)?;
            return RpcReply::value(&UpdateStarted {
                started: false,
                detail: "This engine updates with the app beside it".into(),
            });
        }
        if method == StopEngine::NAME {
            routed::<StopEngine>(params)?;
            self.stop.notify_one();
            return reply(Ok(Outcome::Applied));
        }
        let (_, ResolveSession { row }) = routed(params)?;
        RpcReply::value(&self.session.session_id(row).ok())
    }
}

/// The engine's service: every group, one dispatch.
pub struct EngineService {
    session: SessionService,
    workspace: WorkspaceService,
    files: FilesService,
    git: GitService,
    /// The Agents, Engine settings and later groups, asked in order.
    groups: Vec<Arc<dyn Group>>,
}

impl EngineService {
    /// The service over the first four groups; [`Self::with`] adds the rest.
    #[must_use]
    pub fn new(
        session: SessionService,
        workspace: WorkspaceService,
        files: FilesService,
        git: GitService,
    ) -> Self {
        Self {
            session,
            workspace,
            files,
            git,
            groups: Vec::new(),
        }
    }

    /// This service, also serving `group`.
    #[must_use]
    pub fn with(mut self, group: Arc<dyn Group>) -> Self {
        self.groups.push(group);
        self
    }
}

#[async_trait]
impl RpcService for EngineService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == StopSearch::NAME {
            // A search id names one search, whichever group runs it.
            let (_, StopSearch { search }) = routed(params)?;
            self.files.stop_search(&search);
            self.workspace.stop_search(&search);
            return reply(Ok(Outcome::Applied));
        }
        if let Some(group) = self.groups.iter().find(|group| group.serves(method)) {
            group.handle(method, params).await
        } else if WorkspaceService::serves(method) {
            self.workspace.handle(method, params).await
        } else if FilesService::serves(method) {
            self.files.handle(method, params).await
        } else if GitService::serves(method) {
            self.git.handle(method, params).await
        } else {
            self.session.handle(method, params).await
        }
    }
}

/// A call's body and op, if it is for this engine.
pub(crate) fn routed<T: DeserializeOwned>(
    params: serde_json::Value,
) -> Result<(Option<OpId>, T), RpcError> {
    let routed: Routed<T> = parse_params(params)?;
    if routed.engine.is_some() {
        // Forwarding to other engines arrives with peers (engine.md, migration step 9).
        return Err(RpcError::Failed(
            "This engine serves only its own calls".into(),
        ));
    }
    Ok((routed.op, routed.body))
}

/// A call's reply, or its failure as the error a viewer shows.
pub(crate) fn reply<T: Serialize>(result: Result<T, String>) -> Result<RpcReply, RpcError> {
    RpcReply::value(&result.map_err(RpcError::Failed)?)
}
