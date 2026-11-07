//! Server side: the dispatch loop over string frames, shared by every transport.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::{ClientFrame, RpcError, RpcReply, RpcService, ServerFrame};

/// The requests one connection runs, aborted together when it ends, however it ends: a closed
/// inbound, or the connection's task dropped.
#[derive(Default)]
struct Running(HashMap<u64, AbortHandle>);

impl Drop for Running {
    fn drop(&mut self) {
        for task in self.0.values() {
            task.abort();
        }
    }
}

/// Serve one connection: read client frames from `inbound`, write server frames to `out`.
/// Returns when `inbound` closes; every request still running is aborted then, or when this
/// future is dropped.
pub async fn serve_connection(
    service: Arc<dyn RpcService>,
    out: mpsc::Sender<String>,
    mut inbound: mpsc::Receiver<String>,
) {
    let mut running = Running::default();
    while let Some(payload) = inbound.recv().await {
        // ndjson: a transport may batch several frames per message.
        for line in payload.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let frame: ClientFrame = match serde_json::from_str(line) {
                Ok(frame) => frame,
                Err(err) => {
                    tracing::warn!(error = %err, "rpc: dropping malformed client frame");
                    continue;
                }
            };
            running.0.retain(|_, task| !task.is_finished());
            if frame.cancel {
                if let Some(task) = running.0.remove(&frame.id) {
                    task.abort();
                }
                continue;
            }
            let Some(method) = frame.method else {
                tracing::warn!(id = frame.id, "rpc: frame has neither method nor cancel");
                continue;
            };
            let task = tokio::spawn(handle_request(
                service.clone(),
                out.clone(),
                frame.id,
                method,
                frame.params,
            ));
            running.0.insert(frame.id, task.abort_handle());
        }
    }
}

async fn send(out: &mpsc::Sender<String>, frame: ServerFrame) -> Result<(), RpcError> {
    match serde_json::to_string(&frame) {
        Ok(json) => out.send(json).await.map_err(|_| RpcError::Closed),
        Err(err) => {
            tracing::error!(error = %err, "rpc: failed to serialize server frame");
            Err(RpcError::Closed)
        }
    }
}

async fn handle_request(
    service: Arc<dyn RpcService>,
    out: mpsc::Sender<String>,
    id: u64,
    method: String,
    params: serde_json::Value,
) {
    match service.handle(&method, params).await {
        Ok(RpcReply::Value(value)) => {
            let _ = send(
                &out,
                ServerFrame {
                    id,
                    ok: Some(value),
                    ..Default::default()
                },
            )
            .await;
        }
        Ok(RpcReply::Stream(mut stream)) => {
            while let Some(item) = stream.next().await {
                let frame = ServerFrame {
                    id,
                    item: Some(item),
                    ..Default::default()
                };
                if send(&out, frame).await.is_err() {
                    return; // connection gone
                }
            }
            let _ = send(
                &out,
                ServerFrame {
                    id,
                    done: true,
                    ..Default::default()
                },
            )
            .await;
        }
        Err(err) => {
            let _ = send(
                &out,
                ServerFrame {
                    id,
                    err: Some(err.to_string()),
                    ..Default::default()
                },
            )
            .await;
        }
    }
}
