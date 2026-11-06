//! Every group the engine serves, as one `onicode-rpc` service.
use async_trait::async_trait;
use onicode_rpc::{RpcError, RpcReply, RpcService};

use crate::session::SessionService;
use crate::workspace::WorkspaceService;

pub struct EngineService {
    session: SessionService,
    workspace: WorkspaceService,
}

impl EngineService {
    #[must_use]
    pub fn new(session: SessionService, workspace: WorkspaceService) -> Self {
        Self { session, workspace }
    }
}

#[async_trait]
impl RpcService for EngineService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if WorkspaceService::serves(method) {
            self.workspace.handle(method, params).await
        } else {
            self.session.handle(method, params).await
        }
    }
}
