//! Which engines the tailnet has: read from the daemon's status, refreshed on each of its change
//! notifications, and confirmed by a probe of each candidate's hello.
//!
//! A candidate is a node of this node's own user, or any other node (a tagged VPS) whose grant
//! lets it be an engine here. Phones are never engines. `Online` is the control plane's hint:
//! a node it calls offline is not probed, but one already found stays found, and no link is
//! ever dropped on its word (`PeerLinks` keeps links alive on their own control streams).

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use onicode_net::Hello;
use onicode_tailnet::{Node, Path, Role, Status, Tailnet};
use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::{PeerEndpoint, PeerError};

/// How long a candidate has to answer its probe.
pub const PROBE_BUDGET: Duration = Duration::from_millis(1500);
/// How long after a watch ends (the daemon restarted) it is opened again.
pub const REWATCH: Duration = Duration::from_secs(1);

/// An engine on the tailnet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Its app identity.
    pub device_id: String,
    /// Its tailnet node's stable id.
    pub node: String,
    /// Its `MagicDNS` name.
    pub name: String,
    /// The address its probe answered on.
    pub addr: SocketAddr,
    /// Its hello.
    pub hello: Hello,
    /// Whether it is this node's own user's machine.
    pub owner: bool,
}

#[derive(Default)]
struct State {
    peers: BTreeMap<String, Peer>,
    status: Option<Status>,
}

/// The engines of the tailnet, as last found.
pub struct PeerDirectory {
    endpoint: Arc<PeerEndpoint>,
    tailnet: Arc<dyn Tailnet>,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
    refreshing: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for PeerDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerDirectory")
            .field("peers", &self.state.lock().peers.len())
            .finish_non_exhaustive()
    }
}

/// Whether a node on `os` is a phone, which serves nothing.
#[must_use]
pub fn is_phone(os: &str) -> bool {
    ["ios", "ipados", "android"]
        .iter()
        .any(|p| os.eq_ignore_ascii_case(p))
}

impl PeerDirectory {
    /// An empty directory that probes through `endpoint` and reads `tailnet`.
    #[must_use]
    pub fn new(endpoint: Arc<PeerEndpoint>, tailnet: Arc<dyn Tailnet>) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            tailnet,
            state: Mutex::default(),
            changes: watch::channel(0).0,
            refreshing: tokio::sync::Mutex::new(()),
        })
    }

    /// Keeps the directory current: a refresh now, and one after each change the daemon
    /// reports (a burst of changes is one refresh). A watch that ends is opened again after
    /// [`REWATCH`]. Ends when the handle is aborted.
    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match this.tailnet.watch().await {
                    Ok(mut watch) => {
                        this.refresh_logged().await;
                        while watch.recv().await.is_some() {
                            while watch.try_recv().is_ok() {}
                            // A grant or a tag may have changed: what admission read is stale.
                            this.endpoint.admission().forget();
                            this.refresh_logged().await;
                        }
                    }
                    Err(e) => tracing::info!(error = %e, "tailscale's changes cannot be watched"),
                }
                tokio::time::sleep(REWATCH).await;
            }
        })
    }

    async fn refresh_logged(&self) {
        if let Err(e) = self.refresh().await {
            tracing::info!(error = %e, "peer discovery");
        }
    }

    /// Reads the tailnet's status and probes every candidate in parallel, each within
    /// [`PROBE_BUDGET`]; returns the engines found.
    ///
    /// # Errors
    ///
    /// The daemon's error, or [`PeerError::NotRunning`]; the engines found before stay.
    pub async fn refresh(&self) -> Result<Vec<Peer>, PeerError> {
        let _one_at_a_time = self.refreshing.lock().await;
        let status = self.tailnet.status().await?;
        if !status.running() {
            return Err(PeerError::NotRunning);
        }
        let owner = status
            .me
            .as_ref()
            .filter(|me| me.tags.is_empty())
            .map(|me| me.user);
        let mut probes = JoinSet::new();
        for node in status
            .peer
            .values()
            .filter(|n| n.online && !is_phone(&n.os))
        {
            let Some(ip) = node.ipv4().or_else(|| node.ips.first().copied()) else {
                continue;
            };
            let addr = SocketAddr::new(ip, self.endpoint.port());
            let (endpoint, node_id) = (Arc::clone(&self.endpoint), node.id.clone());
            probes.spawn(async move { (node_id, addr, endpoint.probe(addr, PROBE_BUDGET).await) });
        }
        let mut found = BTreeMap::new();
        let mut probed = HashSet::new();
        while let Some(done) = probes.join_next().await {
            let Ok((node_id, addr, answer)) = done else {
                continue;
            };
            probed.insert(node_id.clone());
            match answer {
                Ok((hello, admitted)) if hello.role == Role::Engine && admitted.node == node_id => {
                    let owner_machine = admitted.grant.owner && owner.is_some();
                    let peer = Peer {
                        device_id: hello.device_id.clone(),
                        node: node_id,
                        name: admitted.name.trim_end_matches('.').to_owned(),
                        addr,
                        hello,
                        owner: owner_machine,
                    };
                    found.insert(peer.device_id.clone(), peer);
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(%addr, error = %e, "not an engine here"),
            }
        }
        let on_tailnet: HashSet<String> = status.peer.values().map(|n| n.id.clone()).collect();
        self.endpoint.retain_nodes(&on_tailnet);
        let peers = {
            let mut state = self.state.lock();
            // Kept on the hint's word alone: found before, still on the tailnet, not probed now
            // because the control plane calls it offline.
            for (device, peer) in &state.peers {
                if !found.contains_key(device)
                    && on_tailnet.contains(&peer.node)
                    && !probed.contains(&peer.node)
                {
                    found.insert(device.clone(), peer.clone());
                }
            }
            state.peers = found;
            state.status = Some(status);
            state.peers.values().cloned().collect::<Vec<_>>()
        };
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
        Ok(peers)
    }

    /// The engines as last found, by device-id.
    #[must_use]
    pub fn peers(&self) -> Vec<Peer> {
        self.state.lock().peers.values().cloned().collect()
    }

    /// The engine `device`, if found.
    #[must_use]
    pub fn get(&self, device: &str) -> Option<Peer> {
        self.state.lock().peers.get(device).cloned()
    }

    /// The tailnet node with stable id `node`, as the last status had it.
    #[must_use]
    pub fn node(&self, node: &str) -> Option<Node> {
        let state = self.state.lock();
        let status = state.status.as_ref()?;
        status.peer.values().find(|n| n.id == node).cloned()
    }

    /// The path the daemon takes to `node`, as the last status had it.
    #[must_use]
    pub fn path(&self, node: &str) -> Option<Path> {
        self.node(node).map(|n| n.path())
    }

    /// Drops `device` until a refresh finds it again (its address answered as another device).
    pub fn forget(&self, device: &str) {
        if self.state.lock().peers.remove(device).is_some() {
            self.changes.send_modify(|n| *n = n.wrapping_add(1));
        }
    }

    /// Changes as a counter that moves on each refresh.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
}
