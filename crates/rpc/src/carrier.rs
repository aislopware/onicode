//! RPC over a carrier that opens one string-frame duplex per call: a QUIC stream between two
//! peers (`onicode-peer`), where a call's stream ending is its cancellation and no call waits
//! behind another's bytes.
//!
//! Each call runs the ordinary [`RpcClient`] and [`serve_connection`] over its own duplex, so
//! the envelopes and the dispatch are the ones every other transport uses. The client keeps its
//! sending half open until the reply is in (the server drops a call whose inbound ends, as
//! [`serve_connection`] does for every transport), and a subscription keeps it open while it
//! lives: dropping a [`CarriedSubscription`] ends the duplex, which ends the call at the server.
//!
//! The serving side applies the peer rules the device-room relay applies ([`crate::peer_refused`])
//! and runs each call as [`Caller::Peer`].

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{Caller, RpcClient, RpcError, RpcReply, RpcService, RpcSubscription, serve_connection};

/// A string-frame duplex: frames to the far end, frames from it. Either end closing its half
/// ends the call.
pub type Duplex = (mpsc::Sender<String>, mpsc::Receiver<String>);

/// Opens one duplex per call to a peer.
#[async_trait]
pub trait StreamCarrier: Send + Sync + 'static {
    /// A fresh duplex to the peer's [`serve_carried`].
    async fn open_duplex(&self) -> Result<Duplex, RpcError>;
}

/// An RPC client that gives every call a duplex of its own from `carrier`.
#[derive(Clone)]
pub struct CarrierClient {
    carrier: Arc<dyn StreamCarrier>,
}

impl CarrierClient {
    pub fn new(carrier: Arc<dyn StreamCarrier>) -> Self {
        Self { carrier }
    }

    /// Unary request on a duplex of its own, ended once the reply is in.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let (out, inbound) = self.carrier.open_duplex().await?;
        let client = RpcClient::new(out, inbound);
        client.call(method, params).await
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

    /// Streaming request on a duplex of its own, which lives as long as the subscription.
    pub async fn subscribe(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<CarriedSubscription, RpcError> {
        let (out, inbound) = self.carrier.open_duplex().await?;
        let client = RpcClient::new(out, inbound);
        let inner = client.subscribe_scoped(method, params).await?;
        Ok(CarriedSubscription {
            inner,
            _client: client,
        })
    }
}

/// A stream of items over a duplex of its own. Dropping it cancels the call and ends the
/// duplex.
pub struct CarriedSubscription {
    // Dropped first: its cancel frame goes out while the client still holds the duplex.
    inner: RpcSubscription,
    // The client's reader routes the items; dropping it would end them.
    _client: RpcClient,
}

impl CarriedSubscription {
    /// The next item; `None` once the stream ends or the duplex closes.
    pub async fn recv(&mut self) -> Option<serde_json::Value> {
        self.inner.recv().await
    }
}

/// Serves `service` on one carried duplex from the peer `device` (`out` to it, `inbound` from
/// it) until the peer ends it: methods only this device's own clients may call are refused,
/// and every call runs as [`Caller::Peer`].
pub async fn serve_carried(
    service: Arc<dyn RpcService>,
    device: Option<String>,
    out: mpsc::Sender<String>,
    inbound: mpsc::Receiver<String>,
) {
    let guard: Arc<dyn RpcService> = Arc::new(PeerGuard {
        inner: service,
        device,
    });
    serve_connection(guard, out, inbound).await;
}

/// The peer rules, applied at dispatch entry: `serve_connection` spawns each call, and a
/// spawned task starts with no caller, so this is where the caller is set.
struct PeerGuard {
    inner: Arc<dyn RpcService>,
    device: Option<String>,
}

#[async_trait]
impl RpcService for PeerGuard {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if crate::peer_refused(method, &params) {
            return Err(RpcError::Failed(if crate::local_only(method) {
                format!("{method} is only answered over this device's local IPC")
            } else {
                format!("{method}: only this device's own window can do that")
            }));
        }
        let caller = Caller::Peer {
            device: self.device.clone(),
        };
        crate::with_caller(caller, self.inner.handle(method, params)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A carrier whose every duplex is served in memory, counting the duplexes opened.
    struct Memory {
        service: Arc<dyn RpcService>,
        opened: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl StreamCarrier for Memory {
        async fn open_duplex(&self) -> Result<Duplex, RpcError> {
            self.opened
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (client_out, server_in) = mpsc::channel::<String>(16);
            let (server_out, client_in) = mpsc::channel::<String>(16);
            tokio::spawn(serve_carried(
                Arc::clone(&self.service),
                Some("peer".to_owned()),
                server_out,
                server_in,
            ));
            Ok((client_out, client_in))
        }
    }

    struct Echo;

    #[async_trait]
    impl RpcService for Echo {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                "Who" => RpcReply::value(&crate::caller().record()),
                "Count" => {
                    let n = params.as_u64().unwrap_or(0);
                    Ok(RpcReply::Stream(Box::pin(futures::stream::iter(
                        (0..n).map(serde_json::Value::from),
                    ))))
                }
                _ => Ok(RpcReply::Value(params)),
            }
        }
    }

    /// Each call takes a duplex of its own and runs as the peer; a stream's items arrive over
    /// its duplex; what only this device's own clients may call is refused.
    #[tokio::test]
    async fn every_call_has_its_own_duplex_and_runs_as_the_peer() {
        let memory = Arc::new(Memory {
            service: Arc::new(Echo),
            opened: Default::default(),
        });
        let client = CarrierClient::new(Arc::clone(&memory) as Arc<dyn StreamCarrier>);
        assert_eq!(
            client.call("Echo", serde_json::json!(7)).await.unwrap(),
            serde_json::json!(7)
        );
        let who = client.call("Who", serde_json::Value::Null).await.unwrap();
        assert_eq!(who, serde_json::json!({ "peer": true, "device": "peer" }));
        let mut items = client
            .subscribe("Count", serde_json::json!(3))
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(item) = items.recv().await {
            got.push(item);
        }
        assert_eq!(
            got,
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );
        let refused = client
            .call(crate::methods::ACCESS_TOKEN, serde_json::Value::Null)
            .await;
        assert!(matches!(refused, Err(RpcError::Failed(_))), "{refused:?}");
        assert_eq!(memory.opened.load(std::sync::atomic::Ordering::Relaxed), 4);
    }
}
