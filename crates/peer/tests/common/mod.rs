//! Engines on a simulated tailnet, for the peer tests.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use onicode_net::Role;
use onicode_net::transport::endpoint_on;
use onicode_net_sim::{Net, NodeId, SimTailnet};
use onicode_peer::{LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerLinks, Sockets};
use onicode_tailnet::Tailnet;

/// The simulated network's sockets.
pub struct SimSockets(pub Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// An engine on one node: its endpoint, directory and links, accepting.
pub struct Engine {
    pub endpoint: Arc<PeerEndpoint>,
    pub directory: Arc<PeerDirectory>,
    pub links: PeerLinks,
}

impl Engine {
    /// Keeps its directory current from the node's watch.
    pub fn watch(&self) -> tokio::task::JoinHandle<()> {
        self.directory.start()
    }
}

/// An engine `device` on `node`, reading the node's own `LocalAPI`.
pub async fn engine(tailnet: &SimTailnet, node: NodeId, device: &str, seed: u64) -> Engine {
    engine_with(
        tailnet,
        tailnet.local_api(node),
        device,
        seed,
        LinksConfig::default(),
    )
    .await
}

/// An engine `device` on the node whose `LocalAPI` is `api`, serving as `config` says.
pub async fn engine_with(
    tailnet: &SimTailnet,
    api: Arc<dyn Tailnet>,
    device: &str,
    seed: u64,
    mut config: LinksConfig,
) -> Engine {
    let sockets = SimSockets(tailnet.net().clone());
    let endpoint = PeerEndpoint::bind(
        Arc::clone(&api),
        &sockets,
        PeerConfig::new(device, Role::Engine).with_seed(seed),
    )
    .await
    .expect("the engine binds its tailnet addresses");
    let directory = PeerDirectory::new(Arc::clone(&endpoint), api);
    config.seed = Some(seed);
    let links = PeerLinks::new(Arc::clone(&endpoint), Arc::clone(&directory), config);
    links.start();
    Engine {
        endpoint,
        directory,
        links,
    }
}

/// The devices `engine`'s directory has found, sorted.
pub fn found(engine: &Engine) -> Vec<String> {
    engine
        .directory
        .peers()
        .into_iter()
        .map(|p| p.device_id)
        .collect()
}
