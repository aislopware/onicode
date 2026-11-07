//! The Terminal group as an `onicode-rpc` service: `WatchTerminal` and the terminal calls, each
//! wrapped in a `Routed` that names this engine (`None`).
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use onicode_protocol::terminal::{
    CloseTerminal, ListTerminals, OpenTerminal, ReadForeground, ResizeTerminal, StopForeground,
    TerminalInput, WatchTerminal,
};
use onicode_protocol::{Call, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use super::hub::TerminalHub;
use crate::service::{reply, routed};

pub struct TerminalService {
    hub: Arc<TerminalHub>,
}

impl TerminalService {
    #[must_use]
    pub fn new(hub: Arc<TerminalHub>) -> Self {
        Self { hub }
    }
}

/// Every method this service serves.
const METHODS: [&str; 8] = [
    WatchTerminal::NAME,
    OpenTerminal::NAME,
    ListTerminals::NAME,
    ResizeTerminal::NAME,
    CloseTerminal::NAME,
    TerminalInput::NAME,
    ReadForeground::NAME,
    StopForeground::NAME,
];

impl TerminalService {
    /// Whether `method` is one of the Terminal group's.
    #[must_use]
    pub fn serves(method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for TerminalService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            WatchTerminal::NAME => {
                let (_, body) = routed::<WatchTerminal>(params)?;
                let items = hub.watch(body).map_err(RpcError::Failed)?;
                Ok(RpcReply::Stream(
                    items
                        .filter_map(|item| futures::future::ready(serde_json::to_value(&item).ok()))
                        .boxed(),
                ))
            }
            OpenTerminal::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.open(body).await)
            }
            ListTerminals::NAME => {
                let (_, ListTerminals {}) = routed(params)?;
                reply(Ok::<_, String>(hub.list()))
            }
            ResizeTerminal::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.resize(body))
            }
            CloseTerminal::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.close(body))
            }
            TerminalInput::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.input(body))
            }
            ReadForeground::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.read_foreground(body).await)
            }
            StopForeground::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.stop_foreground(body))
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
