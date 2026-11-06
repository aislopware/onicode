//! This device joining the tailnet by itself: a QUIC endpoint on its tailnet addresses, the
//! engines it finds there and one link to each, as a viewer that serves no RPC. The app may
//! start before Tailscale is up, so binding is tried again, with backoff that a new network
//! path or the foreground cuts short, until it is.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use onicode_peer::{
    EngineList, LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerError, PeerLinks,
    Sockets, StreamHandler,
};
use onicode_tailnet::Tailnet;
use tokio::task::JoinHandle;

use crate::client::ClientInner;
use crate::lock;

/// The first wait before binding again; doubles to [`BIND_RETRY_CAP`].
const BIND_RETRY_BASE: Duration = Duration::from_secs(1);
const BIND_RETRY_CAP: Duration = Duration::from_secs(30);

/// This device on the tailnet, while the client holds it.
pub(crate) struct Node {
    /// The engines the person listed, read as the tailnet when this device has no `LocalAPI`
    /// to read (iOS).
    pub(crate) engines: Option<Arc<EngineList>>,
    /// Binds until Tailscale is up, then starts the links.
    joining: JoinHandle<()>,
    bound: Arc<Mutex<Option<Bound>>>,
}

/// What runs once the endpoint is bound.
struct Bound {
    endpoint: Arc<PeerEndpoint>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Bound {
    fn drop(&mut self) {
        self.endpoint.close();
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.joining.abort();
        lock(&self.bound).take();
    }
}

/// How to join: where the tailnet is read, the sockets to bind, as whom.
pub(crate) struct Join {
    pub(crate) tailnet: Arc<dyn Tailnet>,
    pub(crate) sockets: Arc<dyn Sockets>,
    pub(crate) config: PeerConfig,
    pub(crate) engines: Option<Arc<EngineList>>,
    /// Redial at once on every OS network path report ([`PeerLinks::follow_path`]).
    pub(crate) follow_path: bool,
}

impl Node {
    /// Starts joining on the client's runtime; the links land in the client once bound.
    pub(crate) fn start(inner: &Arc<ClientInner>, join: Join) -> Self {
        let Join {
            tailnet,
            sockets,
            config,
            engines,
            follow_path,
        } = join;
        let bound: Arc<Mutex<Option<Bound>>> = Arc::default();
        let slot = bound.clone();
        let weak = Arc::downgrade(inner);
        let cancel = inner.cancel.clone();
        let wake = inner.links_wake.clone();
        let handler: Arc<dyn StreamHandler> = Arc::new(inner.remote_terminals.clone());
        let joining = crate::runtime::shared().spawn(async move {
            let seed = config.seed;
            let mut retry = BIND_RETRY_BASE;
            let endpoint = loop {
                let woken = wake.notified();
                match PeerEndpoint::bind(tailnet.clone(), &*sockets, config.clone()).await {
                    Ok(endpoint) => break endpoint,
                    Err(PeerError::NotRunning) => {
                        tracing::debug!("not on the tailnet yet");
                    }
                    Err(error) => tracing::warn!(%error, "joining the tailnet failed"),
                }
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(retry) => {}
                    () = woken => retry = BIND_RETRY_BASE,
                }
                retry = (retry * 2).min(BIND_RETRY_CAP);
            };
            let directory = PeerDirectory::new(endpoint.clone(), tailnet);
            let links = Arc::new(PeerLinks::new(
                endpoint.clone(),
                directory.clone(),
                LinksConfig {
                    service: None,
                    handler: Some(handler),
                    seed,
                },
            ));
            let tasks = vec![directory.start(), links.start()];
            *lock(&slot) = Some(Bound { endpoint, tasks });
            if follow_path {
                links.follow_path();
            }
            if let Some(inner) = weak.upgrade() {
                inner.set_peer_links(links);
            }
        });
        Self {
            engines,
            joining,
            bound,
        }
    }
}
