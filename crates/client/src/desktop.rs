//! A Mac's desktop, asked of its engine: where its `onicode-desktopd` listens, its privacy
//! grants, and the guide to a pane that is off. The picture itself goes straight between this
//! device and desktopd (`onicode-desktop-client`); the engine only brokers.

use onicode_desktop_link::Pane;
use onicode_desktop_link::broker::{DesktopEndpoint, GrantsReply, OpenGuideParams};
use onicode_rpc::methods;
use serde::de::DeserializeOwned;

use crate::Client;
use crate::error::{ClientError, Result};

impl Client {
    /// Ask the Mac `device_id`'s engine for its desktop: it starts desktopd when it is not
    /// running and answers with where it listens on the tailnet, its grants and its displays.
    ///
    /// # Errors
    ///
    /// The host is unreachable or refuses; an answer that does not read.
    pub async fn open_desktop(&self, device_id: &str) -> Result<DesktopEndpoint> {
        self.desktop_rpc(device_id, methods::OPEN_DESKTOP, serde_json::json!({}))
            .await
    }

    /// The Mac `device_id`'s privacy grants, read fresh when desktopd is not running. Never a
    /// prompt on the Mac.
    ///
    /// # Errors
    ///
    /// As [`Self::open_desktop`].
    pub async fn desktop_grants(&self, device_id: &str) -> Result<GrantsReply> {
        self.desktop_rpc(device_id, methods::DESKTOP_GRANTS, serde_json::json!({}))
            .await
    }

    /// Open the guide to `pane` on the Mac `device_id`'s own screen, for the person at it to
    /// turn the grant on (System Settings at the pane, the helper to drag in).
    ///
    /// # Errors
    ///
    /// As [`Self::open_desktop`].
    pub async fn open_desktop_guide(&self, device_id: &str, pane: Pane) -> Result<()> {
        let params = serde_json::to_value(OpenGuideParams { pane })
            .map_err(|e| ClientError::Internal(e.to_string()))?;
        self.inner
            .host_rpc(device_id, methods::DESKTOP_OPEN_GUIDE, params)
            .await?;
        Ok(())
    }

    async fn desktop_rpc<T: DeserializeOwned>(
        &self,
        device_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let reply = self.inner.host_rpc(device_id, method, params).await?;
        serde_json::from_value(reply).map_err(|e| ClientError::HostError(format!("{method}: {e}")))
    }
}
