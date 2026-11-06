//! The explorer's engine calls. Every call goes to this Mac's engine, which serves it or
//! forwards it to the device named by `targetDeviceId`.

use anyhow::Context as _;
use onicode_proto::files::{
    ControlTransferRequest, ListPathRequest, PathListing, PathMutateRequest, PathOp, PathRoots,
    ResolveConflictRequest, StartTransferReply, StartTransferRequest, TransferAction,
    TransferUpdate,
};
use onicode_rpc::methods;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::files::client::request_params;
use crate::state::EngineHandle;

/// Calls on any device's files, addressed by device id.
#[derive(Clone)]
pub(crate) struct ExplorerClient {
    engine: EngineHandle,
    local_device_id: Option<String>,
}

/// One item of the `WatchTransfers` stream: the whole list first, then one job a change.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum TransferFrame {
    List(Vec<TransferUpdate>),
    One(Box<TransferUpdate>),
}

impl TransferFrame {
    pub(crate) fn parse(value: serde_json::Value) -> Option<Self> {
        match serde_json::from_value(value) {
            Ok(frame) => Some(frame),
            Err(error) => {
                tracing::warn!(%error, "dropping malformed transfer frame");
                None
            }
        }
    }
}

impl ExplorerClient {
    pub(crate) fn new(engine: EngineHandle, local_device_id: Option<String>) -> Self {
        Self {
            engine,
            local_device_id,
        }
    }

    /// The `targetDeviceId` for `device`: none when it is this Mac.
    fn target<'a>(&self, device: &'a str) -> Option<&'a str> {
        (self.local_device_id.as_deref() != Some(device)).then_some(device)
    }

    async fn call<Request, Response>(
        &self,
        method: &str,
        device: Option<&str>,
        request: &Request,
    ) -> anyhow::Result<Response>
    where
        Request: Serialize,
        Response: DeserializeOwned,
    {
        let params = request_params(request, device.and_then(|device| self.target(device)))?;
        let value = self.engine.client().call(method, params).await?;
        serde_json::from_value(value).with_context(|| format!("{method} reply"))
    }

    pub(crate) async fn list_path(
        &self,
        device: &str,
        path: &str,
        show_hidden: bool,
    ) -> anyhow::Result<PathListing> {
        let request = ListPathRequest {
            path: path.to_owned(),
            show_hidden,
        };
        self.call(methods::LIST_PATH, Some(device), &request).await
    }

    pub(crate) async fn path_roots(&self, device: &str) -> anyhow::Result<PathRoots> {
        self.call(methods::PATH_ROOTS, Some(device), &serde_json::json!({}))
            .await
    }

    pub(crate) async fn mutate(
        &self,
        device: &str,
        op: PathOp,
        path: String,
        to: Option<String>,
    ) -> anyhow::Result<()> {
        let request = PathMutateRequest { op, path, to };
        let _: serde_json::Value = self
            .call(methods::PATH_MUTATE, Some(device), &request)
            .await?;
        Ok(())
    }

    /// Starts a copy. This Mac's engine takes it and hands it to the destination's.
    pub(crate) async fn start_transfer(
        &self,
        request: &StartTransferRequest,
    ) -> anyhow::Result<String> {
        let reply: StartTransferReply = self.call(methods::START_TRANSFER, None, request).await?;
        Ok(reply.transfer_id)
    }

    /// Pauses, resumes or cancels a job on `device`, the engine that owns it.
    pub(crate) async fn control(
        &self,
        device: &str,
        transfer_id: String,
        action: TransferAction,
    ) -> anyhow::Result<()> {
        let request = ControlTransferRequest {
            transfer_id,
            action,
        };
        let _: serde_json::Value = self
            .call(methods::CONTROL_TRANSFER, Some(device), &request)
            .await?;
        Ok(())
    }

    /// Answers a job's conflict on `device`, the engine that owns it.
    pub(crate) async fn resolve(
        &self,
        device: &str,
        request: &ResolveConflictRequest,
    ) -> anyhow::Result<()> {
        let _: serde_json::Value = self
            .call(methods::RESOLVE_CONFLICT, Some(device), request)
            .await?;
        Ok(())
    }

    /// The jobs `device` owns, as `WatchTransfers` frames.
    pub(crate) async fn watch_transfers(
        &self,
        device: &str,
    ) -> anyhow::Result<mpsc::Receiver<serde_json::Value>> {
        let params = request_params(&serde_json::json!({}), self.target(device))?;
        Ok(self
            .engine
            .client()
            .subscribe(methods::WATCH_TRANSFERS, params)
            .await?)
    }
}
