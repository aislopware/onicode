//! onicode-rpc: the envelopes every viewer speaks to an engine, over the engine's Unix socket
//! ([`serve_unix`], [`connect_unix`]), a peer's per-call QUIC streams ([`CarrierClient`]) and an
//! in-process pair for tests ([`memory_client`]).
//!
//! Framing: ndjson envelopes, one JSON object per line (or per message on a carrier):
//!
//! - client to server: `{id, method, params}` to invoke, `{id, cancel: true}` to stop a stream;
//! - server to client: `{id, ok}` / `{id, err}` for unary calls, `{id, item}`* then `{id, done:
//!   true}` (or `{id, err}`) for streams.
//!
//! The server dispatches into an [`RpcService`]; the [`RpcClient`] offers `call` and
//! `subscribe_scoped`. Both ends run over any pair of string channels, so every transport
//! exercises the same code path. What a caller may call is the service's policy: this crate
//! authenticates (the socket's uid and tokens, [`ipc_auth`]) and records who asked ([`Caller`]).

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

mod caller;
mod carrier;
mod client;
pub mod ipc_auth;
mod server;
mod unix;

pub use caller::{Caller, caller, with_caller};
pub use carrier::{CarriedSubscription, CarrierClient, Duplex, StreamCarrier, serve_carried};
pub use client::{RpcClient, RpcSubscription};
pub use ipc_auth::Scope;
pub use server::serve_connection;
pub use unix::{Admission, DialError, Gate, admits_uid, connect_unix, serve_unix};

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("unknown method: {0}")]
    UnknownMethod(String),
    #[error("bad params: {0}")]
    BadParams(String),
    /// Stable machine-readable capability failure preserved across transports.
    #[error("capability error: {0}")]
    Capability(String),
    #[error("{0}")]
    Failed(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("connection closed")]
    Closed,
}

/// A client-originated frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientFrame {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancel: bool,
}

/// A server-originated frame. Exactly one of `ok` / `err` / `item` / `done` is meaningful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerFrame {
    pub id: u64,
    /// `"ok": null` is a reply whose value is `null` (for example an absent
    /// `Option`), distinct from a frame without `ok`.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub ok: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

fn present_value<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// What a service returns for one invocation.
pub enum RpcReply {
    /// Unary response — sent as `{id, ok}`.
    Value(serde_json::Value),
    /// Stream — each item sent as `{id, item}`, then `{id, done: true}` when it ends.
    Stream(BoxStream<'static, serde_json::Value>),
}

impl RpcReply {
    /// Serialize a value into a unary reply.
    pub fn value<T: Serialize>(value: &T) -> Result<Self, RpcError> {
        serde_json::to_value(value)
            .map(RpcReply::Value)
            .map_err(|e| RpcError::Failed(format!("serialize response: {e}")))
    }
}

/// Server-side dispatch: one implementation serves every transport.
#[async_trait]
pub trait RpcService: Send + Sync + 'static {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError>;
}

/// Deserialize typed params out of the envelope's `params` value.
pub fn parse_params<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))
}

/// Spawn an in-memory server for `service` and return a connected client: the same envelopes and
/// dispatch loop as the socket, for tests that drive a service without one.
pub fn memory_client(service: Arc<dyn RpcService>) -> RpcClient {
    let (client_out, server_in) = tokio::sync::mpsc::channel::<String>(256);
    let (server_out, client_in) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(serve_connection(service, server_out, server_in));
    RpcClient::new(client_out, client_in)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::StreamExt;

    use super::*;

    struct TestService;

    struct CancelAwareService {
        dropped: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[async_trait]
    impl RpcService for CancelAwareService {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method != "Silent" {
                return Err(RpcError::UnknownMethod(method.into()));
            }
            let guard = DropSignal(self.dropped.lock().unwrap().take());
            let stream = futures::stream::unfold(guard, |guard| async move {
                let item = std::future::pending::<Option<(serde_json::Value, DropSignal)>>().await;
                drop(guard);
                item
            });
            Ok(RpcReply::Stream(stream.boxed()))
        }
    }

    #[async_trait]
    impl RpcService for TestService {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                "Echo" => Ok(RpcReply::Value(params)),
                "Count" => {
                    let n = params.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                    Ok(RpcReply::Stream(
                        futures::stream::iter((0..n).map(|i| serde_json::json!(i))).boxed(),
                    ))
                }
                "Never" => Ok(RpcReply::Stream(futures::stream::pending().boxed())),
                "Boom" => Err(RpcError::Failed("boom".into())),
                "Capability" => Err(RpcError::Capability("pull_requests.authentication".into())),
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    #[tokio::test]
    async fn memory_call_stream_and_error() {
        let client = memory_client(Arc::new(TestService));

        let echoed = client
            .call("Echo", serde_json::json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(echoed, serde_json::json!({"x": 1}));

        let mut items = client
            .subscribe_scoped("Count", serde_json::json!({"n": 3}))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(v) = items.recv().await {
            seen.push(v);
        }
        assert_eq!(
            seen,
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );
        assert!(!items.lost(), "the server ended the stream");

        let err = client
            .call("Boom", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, RpcError::Failed(m) if m == "boom"));

        let err = client
            .call("Capability", serde_json::Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RpcError::Capability(code) if code == "pull_requests.authentication"
        ));
    }

    #[tokio::test]
    async fn an_unknown_stream_ends_as_ended_not_lost() {
        let client = memory_client(Arc::new(TestService));
        let mut items = client
            .subscribe_scoped("FutureStream", serde_json::Value::Null)
            .await
            .unwrap();
        assert_eq!(items.recv().await, None);
        assert!(!items.lost(), "the server answered it");
    }

    #[tokio::test]
    async fn scoped_subscription_returns_before_first_item_and_cancels_silence() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let service = Arc::new(CancelAwareService {
            dropped: Mutex::new(Some(dropped_tx)),
        });
        let client = memory_client(service.clone());
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.subscribe_scoped("Silent", serde_json::Value::Null),
        )
        .await
        .unwrap()
        .unwrap();
        while service.dropped.lock().unwrap().is_some() {
            tokio::task::yield_now().await;
        }
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unary_null_reply_resolves_the_call() {
        let client = memory_client(Arc::new(TestService));
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.call("Echo", serde_json::Value::Null),
        )
        .await
        .expect("a null reply is a reply")
        .unwrap();
        assert_eq!(reply, serde_json::Value::Null);
        let frame: ServerFrame = serde_json::from_str(r#"{"id":1,"ok":null}"#).unwrap();
        assert_eq!(frame.ok, Some(serde_json::Value::Null));
        let frame: ServerFrame = serde_json::from_str(r#"{"id":1,"done":true}"#).unwrap();
        assert_eq!(frame.ok, None);
    }

    #[tokio::test]
    async fn dropping_stream_receiver_cancels_server_side() {
        let client = memory_client(Arc::new(TestService));
        let items = client
            .subscribe_scoped("Never", serde_json::Value::Null)
            .await
            .unwrap();
        drop(items);
        // The next unary call still works: the dead stream didn't wedge the connection.
        let echoed = client.call("Echo", serde_json::json!(2)).await.unwrap();
        assert_eq!(echoed, serde_json::json!(2));
    }
}
