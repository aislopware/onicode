//! The Alerts group as an `onicode-rpc` service.
use std::sync::Arc;

use async_trait::async_trait;
use onicode_protocol::alerts::{SetViewportFocus, WatchAlerts};
use onicode_protocol::{Call, Outcome, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use super::hub::AlertsHub;
use crate::service::{Group, reply, routed};
use crate::settings::json_items;

/// Serves `WatchAlerts` and `SetViewportFocus` over an [`AlertsHub`].
pub struct AlertsService {
    hub: Arc<AlertsHub>,
}

impl AlertsService {
    /// The group over `hub`.
    #[must_use]
    pub fn new(hub: Arc<AlertsHub>) -> Self {
        Self { hub }
    }
}

const METHODS: [&str; 2] = [WatchAlerts::NAME, SetViewportFocus::NAME];

impl Group for AlertsService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for AlertsService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            WatchAlerts::NAME => {
                let (_, body) = routed::<WatchAlerts>(params)?;
                Ok(RpcReply::Stream(json_items(self.hub.watch(&body))))
            }
            SetViewportFocus::NAME => {
                let (_, body) = routed::<SetViewportFocus>(params)?;
                self.hub.set_focus(body, onicode_protocol::time::now_ms());
                reply(Ok(Outcome::Applied))
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
