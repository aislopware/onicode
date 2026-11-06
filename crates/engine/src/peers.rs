//! This engine on the tailnet: its QUIC endpoints on the tailnet addresses, the engines the
//! tailnet has, and one link to each, over which peers call this engine's RPC surface and this
//! engine forwards `targetDeviceId` calls to them (`docs/onicode/decisions/topology.md`).

use std::sync::Arc;

use onicode_net::Role;
use onicode_peer::{
    LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerError, PeerLinks, Sockets,
    SystemSockets,
};
use onicode_rpc::RpcService;
use onicode_tailnet::{Relooking, Tailnet};
use tokio::task::JoinHandle;

/// The engine's place on the tailnet, while it runs.
pub struct Peers {
    endpoint: Arc<PeerEndpoint>,
    pub links: Arc<PeerLinks>,
    pub directory: Arc<PeerDirectory>,
    tasks: Vec<JoinHandle<()>>,
}

impl Peers {
    /// Binds on this machine's tailnet addresses as `device_id`, finds the user's other
    /// engines and serves `service` to them.
    ///
    /// # Errors
    ///
    /// [`PeerError::NotRunning`] when Tailscale is not up here; the daemon's error; a bind's.
    pub async fn start(device_id: &str, service: Arc<dyn RpcService>) -> Result<Self, PeerError> {
        Self::start_on(
            Arc::new(Relooking::system()),
            &SystemSockets,
            PeerConfig::new(device_id, Role::Engine),
            Some(service),
        )
        .await
    }

    /// [`Self::start`] on `tailnet` with `sockets`, serving `service` if any: a simulated
    /// tailnet in tests, where a viewer serves nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    pub async fn start_on(
        tailnet: Arc<dyn Tailnet>,
        sockets: &dyn Sockets,
        config: PeerConfig,
        service: Option<Arc<dyn RpcService>>,
    ) -> Result<Self, PeerError> {
        let seed = config.seed;
        let endpoint = PeerEndpoint::bind(tailnet.clone(), sockets, config).await?;
        let directory = PeerDirectory::new(endpoint.clone(), tailnet);
        let links = PeerLinks::new(
            endpoint.clone(),
            directory.clone(),
            LinksConfig {
                service,
                seed,
                ..LinksConfig::default()
            },
        );
        let tasks = vec![directory.start(), links.start()];
        Ok(Self {
            endpoint,
            links: Arc::new(links),
            directory,
            tasks,
        })
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        self.links.shutdown();
        self.endpoint.close();
        for task in &self.tasks {
            task.abort();
        }
    }
}
