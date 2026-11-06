//! Host RPCs over the client's peer links: one QUIC stream per call to the host engine
//! (`onicode_peer::PeerLinks`), plus the multi-call transfers built on it and the PR-status
//! watches. A host with no link — the client not on the tailnet yet, or the engine not found or
//! unreachable — reads as [`ClientError::HostUnavailable`].

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use onicode_peer::{PeerError, PeerLinks};
use onicode_proto::CheckoutChangeRequestStatus;
use onicode_rpc::{CarrierClient, RpcError, methods};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::b64;
use crate::client::ClientInner;
use crate::error::{ClientError, Result};
use crate::lock;
use crate::rpc::ProgressFn;

/// Default unary deadline (engine forward DEFAULT); long ones below.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Raw bytes per UploadChunk slice (a multiple of 3, so slices encode independently).
const UPLOAD_SLICE_BYTES: usize = 510_000;
const UPLOAD_PARALLEL: usize = 3;

pub(crate) fn deadline(method: &str) -> Duration {
    match method {
        methods::CREATE_WORKTREE => Duration::from_secs(120),
        methods::LIST_MODELS => Duration::from_secs(100),
        methods::UPLOAD_COMMIT => Duration::from_secs(150),
        _ => CALL_TIMEOUT,
    }
}

fn map_rpc(device_id: &str, method: &str, err: RpcError) -> ClientError {
    match err {
        RpcError::UnknownMethod(m) => ClientError::Unsupported(format!("{m} on {device_id}")),
        RpcError::BadParams(m) | RpcError::Failed(m) | RpcError::Capability(m) => {
            ClientError::HostError(format!("{method}: {m}"))
        }
        RpcError::Transport(m) => ClientError::HostUnavailable(format!("{device_id}: {m}")),
        RpcError::Closed => ClientError::HostUnavailable(format!("{device_id}: connection closed")),
    }
}

fn map_peer(device_id: &str, method: &str, err: PeerError) -> ClientError {
    match err {
        PeerError::Rpc(err) => map_rpc(device_id, method, err),
        err => ClientError::HostUnavailable(format!("{device_id}: {err}")),
    }
}

/// The host engines this client calls, through its links.
pub(crate) struct Hosts {
    inner: Weak<ClientInner>,
    watches: Mutex<HashMap<WatchKey, CancellationToken>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WatchKey {
    pub device_id: String,
    pub cwd: String,
    pub branch: String,
}

impl Hosts {
    pub(crate) fn new(inner: &Arc<ClientInner>) -> Self {
        Self {
            inner: Arc::downgrade(inner),
            watches: Mutex::new(HashMap::new()),
        }
    }

    /// The client's links, once it has joined the tailnet.
    fn links(&self, device_id: &str) -> Result<Arc<PeerLinks>> {
        self.inner
            .upgrade()
            .and_then(|inner| inner.peer_links.get().cloned())
            .ok_or_else(|| {
                ClientError::HostUnavailable(format!("{device_id}: not on the tailnet yet"))
            })
    }

    /// An RPC client to `device_id` over its link, dialled if need be.
    async fn client(&self, device_id: &str, method: &str) -> Result<CarrierClient> {
        let links = self.links(device_id)?;
        links
            .client(device_id)
            .await
            .map_err(|e| map_peer(device_id, method, e))
    }

    pub(crate) async fn voice_transport(
        &self,
        device_id: &str,
    ) -> Result<Arc<onicode_voice_session::PeerTransport>> {
        let client = self
            .client(device_id, methods::VOICE_CAPABILITIES_V2)
            .await?;
        Ok(Arc::new(onicode_voice_session::PeerTransport {
            client,
            host: device_id.into(),
        }))
    }

    pub(crate) fn shutdown(&self) {
        self.stop_watches();
    }

    /// One unary call with a deadline. A call whose stream broke (the link went down under it)
    /// is tried once more, on the link as it is redialled.
    pub(crate) async fn call(&self, device_id: &str, method: &str, params: Value) -> Result<Value> {
        let timeout = deadline(method);
        let mut last = None;
        for _ in 0..2 {
            let client = self.client(device_id, method).await?;
            match tokio::time::timeout(timeout, client.call(method, params.clone())).await {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(err @ (RpcError::Closed | RpcError::Transport(_)))) => {
                    last = Some(map_rpc(device_id, method, err));
                }
                Ok(Err(err)) => return Err(map_rpc(device_id, method, err)),
                Err(_) => {
                    return Err(ClientError::HostUnavailable(format!(
                        "{method} on {device_id} timed out"
                    )));
                }
            }
        }
        Err(last.unwrap_or_else(|| ClientError::HostUnavailable(device_id.to_owned())))
    }

    /// Open a host stream (terminal output, a subagent's transcript) on a stream of its own,
    /// which the host cancels once the subscription is dropped.
    pub(crate) async fn subscribe(
        &self,
        device_id: &str,
        method: &str,
        params: Value,
    ) -> Result<onicode_rpc::CarriedSubscription> {
        let client = self.client(device_id, method).await?;
        match tokio::time::timeout(CALL_TIMEOUT, client.subscribe(method, params)).await {
            Ok(Ok(subscription)) => Ok(subscription),
            Ok(Err(err)) => Err(map_rpc(device_id, method, err)),
            Err(_) => Err(ClientError::HostUnavailable(format!(
                "{method} on {device_id} timed out"
            ))),
        }
    }

    /// Chunked upload (`UploadChunk` ×N with `seq`, then `UploadCommit`).
    /// Returns the durable host path. Idempotent per `upload_id`.
    pub(crate) async fn upload(
        &self,
        device_id: &str,
        upload_id: &str,
        file_name: &str,
        data: &[u8],
        progress: Option<ProgressFn>,
    ) -> Result<String> {
        let slices: Vec<&[u8]> = if data.is_empty() {
            vec![&[][..]]
        } else {
            data.chunks(UPLOAD_SLICE_BYTES).collect()
        };
        let total = slices.len();
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let report = |done: usize| {
            if let Some(progress) = &progress {
                progress((done as f64 / total as f64).min(0.99));
            }
        };
        report(0);
        let mut next = 0usize;
        while next < total {
            let batch: Vec<(usize, &[u8])> = slices
                .iter()
                .enumerate()
                .skip(next)
                .take(UPLOAD_PARALLEL)
                .map(|(i, s)| (i, *s))
                .collect();
            next += batch.len();
            let calls = batch.into_iter().map(|(seq, slice)| {
                let done = done.clone();
                let report = &report;
                async move {
                    let params =
                        json!({ "uploadId": upload_id, "seq": seq, "data": b64::encode(slice) });
                    let mut last = None;
                    for attempt in 0..3u64 {
                        if attempt > 0 {
                            tokio::time::sleep(Duration::from_millis(
                                50 * attempt * (seq as u64 + 1),
                            ))
                            .await;
                        }
                        match self
                            .call(device_id, methods::UPLOAD_CHUNK, params.clone())
                            .await
                        {
                            Ok(_) => {
                                let now =
                                    done.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
                                report(now);
                                return Ok(());
                            }
                            Err(
                                err @ (ClientError::Unsupported(_) | ClientError::HostError(_)),
                            ) => return Err(err),
                            Err(err) => last = Some(err),
                        }
                    }
                    Err(last.unwrap_or(ClientError::HostUnavailable(device_id.to_owned())))
                }
            });
            for result in futures::future::join_all(calls).await {
                result?;
            }
        }
        let reply = self
            .call(
                device_id,
                methods::UPLOAD_COMMIT,
                json!({ "uploadId": upload_id, "fileName": file_name }),
            )
            .await?;
        if let Some(progress) = &progress {
            progress(1.0);
        }
        reply
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ClientError::HostError("UploadCommit returned no path".into()))
    }

    /// `ReadAttachmentChunk` until done.
    pub(crate) async fn read_attachment(&self, device_id: &str, path: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut offset: u64 = 0;
        for _ in 0..1000 {
            let reply = self
                .call(
                    device_id,
                    methods::READ_ATTACHMENT_CHUNK,
                    json!({ "path": path, "offset": offset }),
                )
                .await?;
            let data = reply
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default();
            out.extend(
                b64::decode(data)
                    .ok_or_else(|| ClientError::HostError("bad attachment chunk".into()))?,
            );
            if reply.get("done").and_then(Value::as_bool).unwrap_or(true) {
                return Ok(out);
            }
            let next = reply
                .get("nextOffset")
                .and_then(Value::as_u64)
                .unwrap_or(offset);
            if next <= offset {
                return Err(ClientError::HostError(
                    "attachment read made no progress".into(),
                ));
            }
            offset = next;
        }
        Err(ClientError::HostError("attachment too large".into()))
    }

    /// Converge the running PR-status streams onto `targets`.
    pub(crate) fn reconcile_watches(&self, inner: &Arc<ClientInner>, targets: HashSet<WatchKey>) {
        let mut watches = lock(&self.watches);
        watches.retain(|key, token| {
            let keep = targets.contains(key);
            if !keep {
                token.cancel();
            }
            keep
        });
        for key in targets {
            if watches.contains_key(&key) {
                continue;
            }
            let token = CancellationToken::new();
            watches.insert(key.clone(), token.clone());
            spawn_watch(Arc::downgrade(inner), key, token);
        }
    }

    pub(crate) fn stop_watches(&self) {
        for (_, token) in lock(&self.watches).drain() {
            token.cancel();
        }
    }
}

/// One `WatchCheckoutChangeRequest` stream, opened again 0.5s→5s whenever it ends (the last
/// status is kept through gaps).
fn spawn_watch(weak: Weak<ClientInner>, key: WatchKey, cancel: CancellationToken) {
    crate::runtime::spawn(async move {
        let mut backoff = Duration::from_millis(500);
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let Some(inner) = weak.upgrade() else { return };
            let Some(live) = inner.live() else { return };
            let opened = live.hosts.subscribe(
                &key.device_id,
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                json!({ "cwd": key.cwd, "branch": key.branch }),
            );
            let opened = tokio::select! {
                _ = cancel.cancelled() => return,
                opened = opened => opened,
            };
            drop(inner);
            if let Ok(mut stream) = opened {
                loop {
                    let item = tokio::select! {
                        _ = cancel.cancelled() => return,
                        item = stream.recv() => item,
                    };
                    let Some(item) = item else { break };
                    let Ok(status) = serde_json::from_value::<CheckoutChangeRequestStatus>(item)
                    else {
                        continue;
                    };
                    if status.device_id != key.device_id || status.cwd != key.cwd {
                        continue;
                    }
                    backoff = Duration::from_millis(500);
                    let Some(inner) = weak.upgrade() else { return };
                    inner.workspace.put_change_request(status);
                    inner.recompute_workspace();
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    });
}
