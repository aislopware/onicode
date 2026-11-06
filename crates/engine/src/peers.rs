//! This engine on the tailnet: its QUIC endpoints on the tailnet addresses, the engines the
//! tailnet has, and one link to each, over which peers call this engine's RPC surface and this
//! engine forwards `targetDeviceId` calls to them (`docs/onicode/decisions/topology.md`).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use bytes::Bytes;
use onicode_net::{Role, StreamHead, StreamKind, StreamRx, StreamTx};
use onicode_peer::{
    LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerError, PeerLinks, Sockets,
    StreamHandler, SystemSockets,
};
use onicode_rpc::RpcService;
use onicode_tailnet::{Relooking, Tailnet};
use tokio::task::JoinHandle;

/// Hands each stream a peer opens to the part of the engine serving its kind (documents,
/// the registry, terminals, tunnels, files), and each datagram to its channel's. Parts
/// register on the engine's core at boot, before or after the tailnet comes up; a stream
/// of a kind nothing serves is dropped, which resets it.
#[derive(Default)]
pub struct StreamRouter {
    streams: RwLock<HashMap<StreamKind, Arc<dyn StreamHandler>>>,
    datagrams: RwLock<HashMap<u8, Arc<dyn StreamHandler>>>,
}

impl StreamRouter {
    /// `handler` takes the streams of `kind` from now on.
    pub fn serve(&self, kind: StreamKind, handler: Arc<dyn StreamHandler>) {
        self.streams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(kind, handler);
    }

    /// `handler` takes the datagrams on `channel` from now on.
    pub fn serve_datagrams(&self, channel: u8, handler: Arc<dyn StreamHandler>) {
        self.datagrams
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(channel, handler);
    }
}

impl StreamHandler for StreamRouter {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        let handler = self
            .streams
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&head.kind)
            .cloned();
        match handler {
            Some(handler) => handler.stream(device, head, tx, rx),
            None => {
                tracing::debug!(device, kind = ?head.kind, "no part of this engine serves the stream")
            }
        }
    }

    fn datagram(&self, device: &str, channel: u8, data: Bytes) {
        let handler = self
            .datagrams
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&channel)
            .cloned();
        if let Some(handler) = handler {
            handler.datagram(device, channel, data);
        }
    }
}

/// The engine's place on the tailnet, while it runs.
pub struct Peers {
    endpoint: Arc<PeerEndpoint>,
    pub links: Arc<PeerLinks>,
    pub directory: Arc<PeerDirectory>,
    tasks: Vec<JoinHandle<()>>,
}

impl Peers {
    /// Binds on this machine's tailnet addresses as `device_id`, finds the user's other
    /// engines and serves `service` to them; a network path handover redials at once.
    ///
    /// # Errors
    ///
    /// [`PeerError::NotRunning`] when Tailscale is not up here; the daemon's error; a bind's.
    pub async fn start(
        device_id: &str,
        service: Arc<dyn RpcService>,
        streams: Arc<StreamRouter>,
    ) -> Result<Self, PeerError> {
        let peers = Self::start_on(
            Arc::new(Relooking::system()),
            &SystemSockets,
            PeerConfig::new(device_id, Role::Engine),
            Some(service),
            Some(streams),
        )
        .await?;
        peers.links.follow_path();
        Ok(peers)
    }

    /// [`Self::start`] on `tailnet` with `sockets`, serving `service` and `streams` if any: a
    /// simulated tailnet in tests, where a viewer serves nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    pub async fn start_on(
        tailnet: Arc<dyn Tailnet>,
        sockets: &dyn Sockets,
        config: PeerConfig,
        service: Option<Arc<dyn RpcService>>,
        streams: Option<Arc<StreamRouter>>,
    ) -> Result<Self, PeerError> {
        let seed = config.seed;
        let endpoint = PeerEndpoint::bind(tailnet.clone(), sockets, config).await?;
        let directory = PeerDirectory::new(endpoint.clone(), tailnet);
        let links = PeerLinks::new(
            endpoint.clone(),
            directory.clone(),
            LinksConfig {
                service,
                handler: streams.map(|streams| streams as Arc<dyn StreamHandler>),
                seed,
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
