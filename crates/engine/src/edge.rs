//! Where account-scoped HTTP calls go (Cloud, the vault, checkout diff sidecars) and the bearer
//! they carry. Session docs and the registry never use it: they sync between engines over the
//! tailnet.

use std::sync::Arc;

/// Edge connection config. The bearer is a provider, never a snapshot: every request re-reads it,
/// so `WorkOS` access-token refreshes take effect without an engine restart. Dev bearers (which
/// never expire) ride the same seam as a [`onicode_rpc::StaticToken`].
#[derive(Clone)]
pub struct EdgeConfig {
    /// Edge base URL (`http(s)://…`).
    pub url: String,
    /// Fresh-bearer provider, consulted per request. Temporary failures keep the session.
    pub token: Arc<dyn onicode_rpc::TokenSource>,
    /// This engine's device id, carried on requests so the edge can attribute them in logs.
    /// Empty = omitted (tests).
    pub device_id: String,
}

impl std::fmt::Debug for EdgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeConfig")
            .field("url", &self.url)
            .field("token", &"<provider>")
            .finish()
    }
}

impl EdgeConfig {
    /// The edge at `url`, authorized by `token`.
    pub fn new(url: impl Into<String>, token: Arc<dyn onicode_rpc::TokenSource>) -> Self {
        Self {
            url: url.into(),
            token,
            device_id: String::new(),
        }
    }

    /// Attribute this engine's requests in edge logs.
    #[must_use]
    pub fn with_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = device_id.into();
        self
    }

    /// Fixed bearer — dev mode and tests, where tokens never expire.
    pub fn with_static_token(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::new(url, Arc::new(onicode_rpc::StaticToken(token.into())))
    }

    /// The current bearer, or a distinct signed-out/temporarily-unavailable error.
    ///
    /// # Errors
    ///
    /// The token source's.
    pub async fn bearer(&self) -> Result<String, onicode_rpc::TokenError> {
        self.token.token().await
    }

    /// Ticks when the bearer changes, if the source reports that.
    pub fn token_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.token.subscribe()
    }
}
