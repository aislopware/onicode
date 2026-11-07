//! Every group the engine serves, as one `onicode-rpc` service.
use async_trait::async_trait;
use onicode_protocol::files::StopSearch;
use onicode_protocol::{Call, OpId, Outcome, Routed};
use onicode_rpc::{RpcError, RpcReply, RpcService, parse_params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::files::FilesService;
use crate::git::GitService;
use crate::session::SessionService;
use crate::workspace::WorkspaceService;

pub struct EngineService {
    session: SessionService,
    workspace: WorkspaceService,
    files: FilesService,
    git: GitService,
}

impl EngineService {
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
        }
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
        if WorkspaceService::serves(method) {
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

/// A Files or Git call's body and op, if it is for this engine.
pub(crate) fn routed<T: DeserializeOwned>(
    params: serde_json::Value,
) -> Result<(Option<OpId>, T), RpcError> {
    let routed: Routed<T> = parse_params(params)?;
    if routed.engine.is_some() {
        // Forwarding to other engines arrives with peers (engine.md, migration step 9).
        return Err(RpcError::Failed(
            "This engine serves only its own files and checkouts".into(),
        ));
    }
    Ok((routed.op, routed.body))
}

/// A call's reply, or its failure as the error a viewer shows.
pub(crate) fn reply<T: Serialize>(result: Result<T, String>) -> Result<RpcReply, RpcError> {
    RpcReply::value(&result.map_err(RpcError::Failed)?)
}
