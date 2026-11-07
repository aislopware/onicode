//! The RPC envelopes over QUIC: one bidirectional stream per call, each envelope a
//! length-prefixed frame ([`onicode_net::stream::frame`]).
//!
//! A call's stream is its whole life: the client finishes its half once the reply is in (or
//! the subscription is dropped), which is the call's cancellation at the server; a stream
//! reset by either end fails the call. No call waits behind another's bytes.

use std::sync::Arc;

use async_trait::async_trait;
use onicode_net::{Link, StreamHead, StreamRx, StreamTx, close};
use onicode_rpc::{Duplex, RpcError, StreamCarrier};
use tokio::sync::mpsc;

/// Frames a call may have queued each way before its reader catches up.
const QUEUE: usize = 64;
/// Bytes a call's stream may hold queued ahead of QUIC before its writer waits: past this a slow
/// reader holds back the serving side (a watch's queue then overflows to `Resync` at the engine,
/// engine.md "Backpressure") instead of the stream's queue growing without bound.
pub const BACKLOG: usize = 256 << 10;

/// Opens an RPC stream per call on a link.
#[derive(Clone, Debug)]
pub struct PeerCarrier {
    link: Link,
}

impl PeerCarrier {
    /// Calls over `link`.
    #[must_use]
    pub const fn new(link: Link) -> Self {
        Self { link }
    }
}

#[async_trait]
impl StreamCarrier for PeerCarrier {
    async fn open_duplex(&self) -> Result<Duplex, RpcError> {
        let (tx, rx) = self
            .link
            .open(&StreamHead::rpc())
            .await
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        Ok(duplex(tx, rx))
    }
}

/// The string-frame duplex over one stream: what is sent on the returned sender goes out as
/// frames, and the stream's frames arrive on the receiver. The sender's last clone dropped
/// finishes the stream; the stream's end, or an unreadable frame, ends the receiver. The writer
/// keeps at most [`BACKLOG`] bytes queued on the stream, so QUIC's flow control reaches the
/// sender of what goes out.
#[must_use]
pub fn duplex(tx: StreamTx, mut rx: StreamRx) -> Duplex {
    let (out, mut outgoing) = mpsc::channel::<String>(QUEUE);
    let (incoming, inbound) = mpsc::channel::<String>(QUEUE);
    tokio::spawn(async move {
        while let Some(frame) = outgoing.recv().await {
            tx.room(BACKLOG).await;
            if tx.send_frame(frame.as_bytes()).is_err() {
                return;
            }
        }
        tx.finish();
    });
    tokio::spawn(async move {
        loop {
            match rx.frame().await {
                Ok(Some(frame)) => {
                    let Ok(text) = String::from_utf8(frame.to_vec()) else {
                        rx.stop(close::PROTOCOL);
                        return;
                    };
                    if incoming.send(text).await.is_err() {
                        rx.stop(close::NORMAL);
                        return;
                    }
                }
                Ok(None) | Err(_) => return,
            }
        }
    });
    (out, inbound)
}

/// A carrier for `link`, shared.
#[must_use]
pub fn carrier(link: Link) -> Arc<dyn StreamCarrier> {
    Arc::new(PeerCarrier::new(link))
}
