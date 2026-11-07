//! Client side: request and stream multiplexing over one string-frame duplex. The duplex is the
//! engine's Unix socket ([`crate::connect_unix`]), a peer's QUIC stream ([`crate::CarrierClient`])
//! or an in-process pair ([`crate::memory_client`]); the envelopes are the same on all three.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::{ClientFrame, RpcError, ServerFrame};

/// Per-stream queue depth. Bounded: `route_frame` awaits a full queue, pausing the connection
/// reader: transport backpressure instead of unbounded growth when a consumer stalls behind a
/// fast producer.
const STREAM_QUEUE_CAP: usize = 256;

enum Pending {
    Call(oneshot::Sender<Result<serde_json::Value, RpcError>>),
    Stream {
        items: mpsc::Sender<serde_json::Value>,
        /// Set when the server ended the stream (`done` or `err`), before its sender drops: a
        /// stream that ends without it lost its connection.
        ended: Arc<AtomicBool>,
    },
}

struct Shared {
    pending: Mutex<HashMap<u64, Pending>>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A request frame as it is written: borrowed, so a call costs no copy of its method or params.
#[derive(Serialize)]
struct Request<'a> {
    id: u64,
    method: &'a str,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    params: &'a serde_json::Value,
}

/// A multiplexing RPC client over any string-frame duplex. Use one per connection; calls and
/// streams on it share the duplex.
pub struct RpcClient {
    out: mpsc::Sender<String>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
    /// Flips to true once the server side of the duplex is gone.
    closed: tokio::sync::watch::Receiver<bool>,
}

/// Owned stream receiver whose drop cancels the server's stream at once.
pub struct RpcSubscription {
    id: u64,
    items: mpsc::Receiver<serde_json::Value>,
    out: mpsc::Sender<String>,
    shared: Arc<Shared>,
    ended: Arc<AtomicBool>,
}

impl RpcSubscription {
    /// The next item; `None` once the stream ended or its connection closed ([`Self::lost`]).
    pub async fn recv(&mut self) -> Option<serde_json::Value> {
        self.items.recv().await
    }

    /// Whether the stream ended because its connection closed rather than because the server
    /// ended it. Meaningful once [`Self::recv`] returned `None`.
    #[must_use]
    pub fn lost(&self) -> bool {
        !self.ended.load(Ordering::Acquire)
    }
}

impl Drop for RpcSubscription {
    fn drop(&mut self) {
        if self.shared.lock().remove(&self.id).is_none() {
            return;
        }
        let Ok(frame) = serde_json::to_string(&ClientFrame {
            id: self.id,
            method: None,
            params: serde_json::Value::Null,
            cancel: true,
        }) else {
            return;
        };
        match self.out.try_send(frame) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
            Err(mpsc::error::TrySendError::Full(frame)) => {
                let out = self.out.clone();
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move {
                        let _ = out.send(frame).await;
                    });
                }
            }
        }
    }
}

impl RpcClient {
    /// Wraps an existing duplex: `out` carries client frames, `inbound` server frames. Spawns the
    /// reader, so it runs inside a tokio runtime; its calls may be awaited on any executor.
    #[must_use]
    pub fn new(out: mpsc::Sender<String>, mut inbound: mpsc::Receiver<String>) -> Self {
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
        });
        let reader_shared = shared.clone();
        let reader_out = out.clone();
        let (closed_tx, closed) = tokio::sync::watch::channel(false);
        let reader = tokio::spawn(async move {
            while let Some(payload) = inbound.recv().await {
                for line in payload.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let frame: ServerFrame = match serde_json::from_str(line) {
                        Ok(frame) => frame,
                        Err(err) => {
                            tracing::warn!(error = %err, "rpc: dropping malformed server frame");
                            continue;
                        }
                    };
                    route_frame(&reader_shared, &reader_out, frame).await;
                }
            }
            // Connection closed: fail every pending call; stream receivers end as their senders
            // drop, with `ended` unset, so they read as lost.
            let drained: Vec<Pending> = {
                let mut pending = reader_shared.lock();
                pending.drain().map(|(_, p)| p).collect()
            };
            for entry in drained {
                if let Pending::Call(tx) = entry {
                    let _ = tx.send(Err(RpcError::Closed));
                }
            }
            let _ = closed_tx.send(true);
        });
        Self {
            out,
            shared,
            next_id: AtomicU64::new(1),
            reader,
            closed,
        }
    }

    /// Resolves once the connection has closed (the server went away). An in-memory client's
    /// never does while its service lives.
    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        let _ = closed.wait_for(|closed| *closed).await;
    }

    /// Whether the connection has closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow() || self.out.is_closed()
    }

    /// Unary request.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        self.call_ref(method, &params).await
    }

    /// Unary request with borrowed params: a caller that may retry it on another connection
    /// keeps them without a copy.
    pub async fn call_ref(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.lock().insert(id, Pending::Call(tx));
        self.send(id, method, params).await.inspect_err(|_| {
            self.shared.lock().remove(&id);
        })?;
        rx.await.map_err(|_| RpcError::Closed)?
    }

    /// Typed unary request.
    pub async fn call_as<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, RpcError> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value).map_err(|e| RpcError::BadParams(e.to_string()))
    }

    /// A drop-cancelled stream, returned without waiting for its first item, so a stream that
    /// is silent for a while still returns.
    pub async fn subscribe_scoped(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcSubscription, RpcError> {
        self.subscribe_ref(method, &params).await
    }

    /// [`Self::subscribe_scoped`] with borrowed params.
    pub async fn subscribe_ref(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<RpcSubscription, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(STREAM_QUEUE_CAP);
        let ended = Arc::new(AtomicBool::new(false));
        self.shared.lock().insert(
            id,
            Pending::Stream {
                items: tx,
                ended: ended.clone(),
            },
        );
        // Own cancellation before the send, including cancellation while the outbound channel
        // is backpressured.
        let subscription = RpcSubscription {
            id,
            items: rx,
            out: self.out.clone(),
            shared: self.shared.clone(),
            ended,
        };
        self.send(id, method, params).await?;
        Ok(subscription)
    }

    async fn send(
        &self,
        id: u64,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<(), RpcError> {
        let json = serde_json::to_string(&Request { id, method, params })
            .map_err(|e| RpcError::Transport(format!("serialize frame: {e}")))?;
        self.out.send(json).await.map_err(|_| RpcError::Closed)
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Ends the stream `id` as the server ended it.
fn end_stream(shared: &Shared, id: u64) {
    if let Some(Pending::Stream { ended, .. }) = shared.lock().remove(&id) {
        ended.store(true, Ordering::Release);
    }
}

async fn route_frame(shared: &Arc<Shared>, out: &mpsc::Sender<String>, frame: ServerFrame) {
    let id = frame.id;
    if let Some(err) = frame.err {
        let error = wire_error(err);
        let entry = shared.lock().remove(&id);
        match entry {
            Some(Pending::Call(tx)) => {
                let _ = tx.send(Err(error));
            }
            Some(Pending::Stream { ended, .. }) => {
                ended.store(true, Ordering::Release);
                tracing::debug!(id, %error, "rpc: stream ended with error");
            }
            None => {}
        }
        return;
    }
    if let Some(value) = frame.ok {
        let mut pending = shared.lock();
        if matches!(pending.get(&id), Some(Pending::Call(_)))
            && let Some(Pending::Call(tx)) = pending.remove(&id)
        {
            let _ = tx.send(Ok(value));
        }
        return;
    }
    if let Some(item) = frame.item {
        // Clone the sender out of the lock: the bounded send must await (backpressure) without
        // holding `shared`.
        let tx = match shared.lock().get(&id) {
            Some(Pending::Stream { items, .. }) => Some(items.clone()),
            _ => None,
        };
        let dead = match tx {
            Some(tx) => tx.send(item).await.is_err(),
            None => false,
        };
        if dead {
            // The receiver was dropped: cancel at the server and forget the stream.
            shared.lock().remove(&id);
            if let Ok(json) = serde_json::to_string(&ClientFrame {
                id,
                method: None,
                params: serde_json::Value::Null,
                cancel: true,
            }) {
                let _ = out.send(json).await;
            }
        }
        return;
    }
    if frame.done {
        end_stream(shared, id);
    }
}

fn wire_error(error: String) -> RpcError {
    if let Some(method) = error.strip_prefix("unknown method: ") {
        RpcError::UnknownMethod(method.to_owned())
    } else if let Some(code) = error.strip_prefix("capability error: ") {
        RpcError::Capability(code.to_owned())
    } else {
        RpcError::Failed(error)
    }
}
