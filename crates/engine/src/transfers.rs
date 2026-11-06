//! Files between this engine and its peers (`docs/onicode/decisions/one-machine.md`).
//!
//! As a destination, this engine owns the jobs started here (or forwarded here) and pulls from
//! the source engine over `StreamKind::File` streams on the peer link; as a source, it serves
//! the transfers a destination registered with `ServeTransfer`, and nothing else. The logic is
//! `onicode_transfer`'s; this is its RPC surface and its place in the engine.
//!
//! - `StartTransfer {from, to, conflict}` → `{transferId}`: forwarded to `to.deviceId` unless that
//!   is this engine.
//! - `ServeTransfer {transferId, paths, block?}` → `{}`: on the source.
//! - `WatchTransfers {}`: the whole list, then one `TransferUpdate` per change.
//! - `ControlTransfer {transferId, action}`, `ResolveConflict {transferId, choice, applyToAll}` →
//!   `{}`: on the destination.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, RwLock};

use futures::StreamExt as _;
use onicode_peer::PeerLinks;
use onicode_proto::files::{
    ControlTransferRequest, ResolveConflictRequest, ServeTransferRequest, StartTransferReply,
    StartTransferRequest, TransferUpdate,
};
use onicode_rpc::{RpcError, RpcReply, methods, parse_params};
use onicode_transfer::store::Store;
use onicode_transfer::{
    ClipStreams, Disk, FileStreams, LinksSlot, PeerOrigins, Served, Transfers, Tuning,
};
use tokio::sync::broadcast;

/// This engine's transfers, its file-stream handler and its source registry.
#[derive(Clone)]
pub struct EngineTransfers {
    device_id: String,
    served: Arc<Served>,
    files: Arc<FileStreams>,
    transfers: Transfers,
    links: LinksSlot,
}

impl EngineTransfers {
    /// Opens the jobs kept under `data_dir` (picking up those that were moving, when a runtime
    /// is running) for the engine `device_id`.
    #[must_use]
    pub fn open(data_dir: &Path, device_id: &str) -> Self {
        let served = Arc::new(Served::new());
        let links: LinksSlot = Arc::new(RwLock::new(None));
        {
            let links = Arc::clone(&links);
            served.set_bandwidth(Arc::new(move |device: &str| {
                links.read().ok()?.as_ref()?.live(device)?.bandwidth()
            }));
        }
        let transfers = Transfers::new(
            Arc::new(PeerOrigins::new(
                device_id.to_owned(),
                Arc::clone(&served),
                Arc::clone(&links),
            )),
            Arc::new(Disk),
            Store::at(onicode_transfer::transfers::store_dir(data_dir)),
            Tuning::default(),
        );
        if tokio::runtime::Handle::try_current().is_ok() {
            transfers.resume_kept();
        }
        Self {
            device_id: device_id.to_owned(),
            files: Arc::new(FileStreams::new(Arc::clone(&served))),
            served,
            transfers,
            links,
        }
    }

    /// The handler for `StreamKind::File` streams peers open to this engine.
    #[must_use]
    pub fn file_streams(&self) -> Arc<FileStreams> {
        Arc::clone(&self.files)
    }

    /// Hands `clip/...` file streams to the clipboard from now on.
    pub fn set_clip(&self, clip: Arc<dyn ClipStreams>) {
        self.files.set_clip(clip);
    }

    /// Reaches other engines over `links` from now on.
    pub fn set_links(&self, links: Arc<PeerLinks>) {
        *self
            .links
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(links);
    }

    /// The jobs.
    #[must_use]
    pub const fn transfers(&self) -> &Transfers {
        &self.transfers
    }

    /// The transfers this engine serves as a source.
    #[must_use]
    pub const fn served(&self) -> &Arc<Served> {
        &self.served
    }

    /// Saves every job's progress, for a clean shutdown.
    pub fn shutdown(&self) {
        self.transfers.save_all();
    }

    fn links(&self) -> Option<Arc<PeerLinks>> {
        self.links
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Answers one of the methods [`handles`] names.
    pub(crate) async fn dispatch(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        match method {
            methods::START_TRANSFER => {
                let mut request: StartTransferRequest = parse_params(params)?;
                if request.from.device_id.is_empty() {
                    request.from.device_id.clone_from(&self.device_id);
                }
                if !request.to.device_id.is_empty() && request.to.device_id != self.device_id {
                    // The destination owns the job: hand it the call.
                    let links = self.links().ok_or_else(|| {
                        RpcError::Failed(format!(
                            "cannot reach device {}: this engine is offline",
                            request.to.device_id
                        ))
                    })?;
                    let target = request.to.device_id.clone();
                    let params = serde_json::to_value(&request)
                        .map_err(|e| RpcError::Failed(e.to_string()))?;
                    let reply = links
                        .call(&target, methods::START_TRANSFER, params)
                        .await
                        .map_err(RpcError::from)?;
                    return Ok(RpcReply::Value(reply));
                }
                request.to.device_id.clone_from(&self.device_id);
                let transfer_id = self.transfers.start(request).map_err(failed)?;
                RpcReply::value(&StartTransferReply { transfer_id })
            }
            methods::SERVE_TRANSFER => {
                let request: ServeTransferRequest = parse_params(params)?;
                self.served.register_request(request).map_err(failed)?;
                RpcReply::value(&serde_json::json!({}))
            }
            methods::WATCH_TRANSFERS => Ok(RpcReply::Stream(watch(self.transfers.clone()))),
            methods::CONTROL_TRANSFER => {
                let request: ControlTransferRequest = parse_params(params)?;
                self.transfers
                    .control(&request.transfer_id, request.action)
                    .map_err(failed)?;
                RpcReply::value(&serde_json::json!({}))
            }
            methods::RESOLVE_CONFLICT => {
                let request: ResolveConflictRequest = parse_params(params)?;
                self.transfers
                    .resolve(&request.transfer_id, request.choice, request.apply_to_all)
                    .map_err(failed)?;
                RpcReply::value(&serde_json::json!({}))
            }
            other => Err(RpcError::UnknownMethod(other.to_owned())),
        }
    }
}

/// The methods [`EngineTransfers::dispatch`] answers.
pub(crate) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::START_TRANSFER
            | methods::SERVE_TRANSFER
            | methods::WATCH_TRANSFERS
            | methods::CONTROL_TRANSFER
            | methods::RESOLVE_CONFLICT
    )
}

fn failed(error: onicode_transfer::Error) -> RpcError {
    RpcError::Failed(error.to_string())
}

/// `WatchTransfers`: the whole list first, then each update. A watcher that falls behind gets
/// every job's current update instead of the ones it missed.
fn watch(transfers: Transfers) -> futures::stream::BoxStream<'static, serde_json::Value> {
    let updates = transfers.subscribe();
    let first = serde_json::to_value(transfers.list()).unwrap_or_default();
    let rest = futures::stream::unfold(
        (updates, transfers, VecDeque::<TransferUpdate>::new()),
        |(mut updates, transfers, mut backlog)| async move {
            loop {
                if let Some(update) = backlog.pop_front() {
                    let value = serde_json::to_value(update).ok()?;
                    return Some((value, (updates, transfers, backlog)));
                }
                match updates.recv().await {
                    Ok(update) => backlog.push_back(update),
                    Err(broadcast::error::RecvError::Lagged(_)) => backlog.extend(transfers.list()),
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    futures::stream::once(async move { first })
        .chain(rest)
        .boxed()
}
