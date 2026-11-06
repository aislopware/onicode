//! One stream from a host engine: a subscription on a stream of its own over
//! the host's link in live mode, a channel the demo host feeds offline.
//! Dropping it ends the stream (live: the host cancels it).

use serde_json::Value;

pub(crate) enum HostStream {
    Peer(onicode_rpc::CarriedSubscription),
    Local(tokio::sync::mpsc::Receiver<Value>),
}

impl HostStream {
    /// The next item; `None` once the stream has ended.
    pub(crate) async fn recv(&mut self) -> Option<Value> {
        match self {
            Self::Peer(subscription) => subscription.recv().await,
            Self::Local(rx) => rx.recv().await,
        }
    }
}
