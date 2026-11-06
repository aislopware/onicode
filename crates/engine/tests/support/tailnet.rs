//! Engines joined over a simulated tailnet (`onicode_net_sim`), the way production engines
//! join over the real one ([`onicode_engine::peers::Peers`]).

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use onicode_engine::EngineCore;
use onicode_engine::peers::{Peers, StreamRouter};
use onicode_net::Role;
use onicode_net::transport::endpoint_on;
use onicode_net_sim::{Faults, Net, NodeId, NodeSpec, SimTailnet};
use onicode_peer::{PeerConfig, Sockets};
use onicode_rpc::RpcService;

/// The tailnet's user every node here belongs to.
const USER: i64 = 1;

struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// One simulated tailnet of one user's machines.
pub struct Tailnet {
    pub sim: SimTailnet,
    seed: AtomicU64,
}

impl Tailnet {
    pub fn new(seed: u64) -> Self {
        Self {
            sim: SimTailnet::new(Faults::CLEAR, seed),
            seed: AtomicU64::new(seed),
        }
    }

    /// A new machine on it.
    pub fn node(&self) -> NodeId {
        let n = self.seed.load(Ordering::Relaxed);
        self.sim.add(NodeSpec::owned(&format!("node-{n}"), USER))
    }

    /// `device` on `node` as `role`, serving `service` to its peers if any.
    pub async fn join(
        &self,
        node: NodeId,
        device: &str,
        role: Role,
        service: Option<Arc<dyn RpcService>>,
        streams: Option<Arc<StreamRouter>>,
    ) -> Peers {
        let seed = self.seed.fetch_add(1, Ordering::Relaxed);
        Peers::start_on(
            self.sim.local_api(node),
            &SimSockets(self.sim.net().clone()),
            PeerConfig::new(device, role).with_seed(seed),
            service,
            streams,
        )
        .await
        .expect("the device joins the simulated tailnet")
    }
}

impl Tailnet {
    /// `core` on a machine of its own, with links to every engine on the tailnet: the machine
    /// and the links.
    pub async fn engine(&self, core: &EngineCore) -> (NodeId, Peers) {
        let node = self.node();
        let peers = self
            .join(
                node,
                &core.device_id,
                Role::Engine,
                Some(core.rpc_service()),
                Some(core.streams().clone()),
            )
            .await;
        core.set_links(peers.links.clone());
        (node, peers)
    }
}

/// `cores` each on a machine of their own on one tailnet, linked while the result lives: each
/// one's machine and its links, in the order given.
pub async fn mesh(cores: &[&EngineCore], seed: u64) -> (Tailnet, Vec<(NodeId, Peers)>) {
    let tailnet = Tailnet::new(seed);
    let mut joined = Vec::new();
    for core in cores {
        joined.push(tailnet.engine(core).await);
    }
    // Each directory reads the tailnet once all have joined, so none waits on the watch
    // that reports the later ones.
    for (_, peers) in &joined {
        let found = peers
            .directory
            .refresh()
            .await
            .expect("the simulated tailnet answers");
        assert_eq!(found.len(), cores.len() - 1, "every other engine is found");
    }
    (tailnet, joined)
}
