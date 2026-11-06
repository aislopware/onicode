//! A simulated tailnet: nodes with users and tags, grants between them, `Online` flaps, paths
//! that move between direct, peer relay and DERP, and a fake `LocalAPI` per node, all from one
//! model on one in-memory [`Net`].
//!
//! Each node's [`SimTailnet::local_api`] answers `status`, `whois` and `watch` as that node's
//! daemon would, from the model as it stands, so every node sees the same tailnet. Node `i` has
//! the addresses `100.64.x.y` (`x.y` = `i + 1`) and `fd7a:115c:a1e0::{i + 1}`. A node's
//! `LocalAPI` can be made to fail ([`ApiFault`]) the ways the real one does: gone, refusing the
//! token, or stuck past the client's timeout.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use onicode_tailnet::status::CurrentTailnet;
use onicode_tailnet::whois::{UserProfile, WhoNode};
use onicode_tailnet::{
    BackendState, CAP, LocalApiError, Node, Notify, Status, Tailnet, Watch, WhoIs,
};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::shape::Link;
use crate::sim::{Faults, Net, Socket};

/// The user id the control plane gives every tagged node's profile (`tagged-devices`).
pub const TAGGED_DEVICES: i64 = 1 << 40;
/// How long a stuck `LocalAPI` holds a call: past the real client's timeout, which then fails.
pub const SLOW_API: Duration = Duration::from_secs(2);
/// The `MagicDNS` suffix of every node.
pub const SUFFIX: &str = "sim.ts.net";

/// A node of a [`SimTailnet`], by the order it was added in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

/// What a node is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSpec {
    /// Its host name; its `MagicDNS` name is this under [`SUFFIX`].
    pub name: String,
    /// The user it belongs to ([`TAGGED_DEVICES`] for a tagged node).
    pub user: i64,
    /// Its tags; a tagged node belongs to no user.
    pub tags: Vec<String>,
    /// `macOS`, `linux`, `iOS`, …
    pub os: String,
}

impl NodeSpec {
    /// A Mac belonging to `user`.
    #[must_use]
    pub fn owned(name: &str, user: i64) -> Self {
        Self {
            name: name.to_owned(),
            user,
            tags: Vec::new(),
            os: "macOS".to_owned(),
        }
    }

    /// A Linux box carrying `tag` (a VPS joined with a pre-auth key).
    #[must_use]
    pub fn tagged(name: &str, tag: &str) -> Self {
        Self {
            name: name.to_owned(),
            user: TAGGED_DEVICES,
            tags: vec![tag.to_owned()],
            os: "linux".to_owned(),
        }
    }

    /// The same node on another OS.
    #[must_use]
    pub fn on(mut self, os: &str) -> Self {
        os.clone_into(&mut self.os);
        self
    }
}

/// How a pair of nodes reach each other, as their daemons report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimPath {
    /// Straight over UDP.
    Direct,
    /// Through another node acting as a Tailscale peer relay.
    PeerRelay,
    /// Through a DERP server in this region: TCP, and a detour.
    Derp(String),
}

/// How a node's `LocalAPI` fails, if it does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApiFault {
    /// It answers.
    #[default]
    Up,
    /// Nothing listens: Tailscale is not running.
    Down,
    /// It refuses the token (an App Store extension back on a new port and token).
    Unauthorized,
    /// It accepts and never answers in time.
    Slow,
}

#[derive(Debug)]
struct SimNode {
    spec: NodeSpec,
    online: bool,
    asleep: bool,
    api: ApiFault,
}

#[derive(Debug, Default)]
struct Model {
    nodes: Vec<SimNode>,
    /// Paths by the pair of nodes, smaller index first; unlisted pairs are direct.
    paths: BTreeMap<(usize, usize), SimPath>,
    /// Roles granted under [`CAP`], by source node and destination node.
    grants: BTreeMap<(usize, usize), Vec<String>>,
    /// Open watches, by the node whose daemon serves them.
    watchers: Vec<(usize, mpsc::Sender<Notify>)>,
}

/// A simulated tailnet over an in-memory [`Net`]. Cloning it shares it.
#[derive(Clone, Debug)]
pub struct SimTailnet {
    net: Net,
    model: Arc<Mutex<Model>>,
}

/// Node `i`'s IPv4 tailnet address.
#[must_use]
pub fn ipv4(node: NodeId) -> IpAddr {
    let n = u32::try_from(node.0).unwrap_or(u32::MAX).saturating_add(1);
    IpAddr::V4(Ipv4Addr::from(
        u32::from(Ipv4Addr::new(100, 64, 0, 0)).saturating_add(n),
    ))
}

/// Node `i`'s IPv6 tailnet address.
#[must_use]
pub fn ipv6(node: NodeId) -> IpAddr {
    let n = u128::try_from(node.0)
        .unwrap_or(u128::MAX)
        .saturating_add(1);
    let base = u128::from(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0));
    IpAddr::V6(Ipv6Addr::from(base.saturating_add(n)))
}

/// Node `i`'s stable id at the control plane.
#[must_use]
pub fn stable_id(node: NodeId) -> String {
    format!("n{}", node.0)
}

impl SimTailnet {
    /// An empty tailnet on a fresh [`Net`] with `faults` on every direct path, drawing from
    /// `seed`. Made on the runtime that drives it.
    #[must_use]
    pub fn new(faults: Faults, seed: u64) -> Self {
        Self {
            net: Net::new(faults, seed),
            model: Arc::default(),
        }
    }

    /// The network under it.
    #[must_use]
    pub const fn net(&self) -> &Net {
        &self.net
    }

    /// A DERP path's link: a TCP relay a few tens of milliseconds away.
    #[must_use]
    pub const fn derp_link() -> Link {
        Link {
            delay: Duration::from_millis(40),
            jitter: Duration::from_millis(10),
            loss: 0.0,
            rate: 0,
            queue: 0,
        }
    }

    /// Add a node, online and with a working `LocalAPI`.
    pub fn add(&self, spec: NodeSpec) -> NodeId {
        let mut model = self.model.lock();
        model.nodes.push(SimNode {
            spec,
            online: true,
            asleep: false,
            api: ApiFault::Up,
        });
        let id = NodeId(model.nodes.len().saturating_sub(1));
        model.changed();
        id
    }

    /// The node's tailnet addresses, IPv4 first.
    #[must_use]
    pub fn ips(&self, node: NodeId) -> [IpAddr; 2] {
        [ipv4(node), ipv6(node)]
    }

    /// A socket of `node`'s at `port` on its IPv4 (or, with `v6`, IPv6) tailnet address.
    ///
    /// # Errors
    ///
    /// `AddrInUse` if the node already has a socket there.
    pub fn bind(&self, node: NodeId, v6: bool, port: u16) -> io::Result<Socket> {
        let ip = if v6 { ipv6(node) } else { ipv4(node) };
        self.net.bind(SocketAddr::new(ip, port))
    }

    /// Grant `src` the onicode `roles` on `dst`, as a tailnet policy's `app` grant does.
    pub fn grant(&self, src: NodeId, dst: NodeId, roles: &[&str]) {
        let mut model = self.model.lock();
        model.grants.insert(
            (src.0, dst.0),
            roles.iter().map(|r| (*r).to_owned()).collect(),
        );
        model.changed();
    }

    /// The control plane says `node` is online or not. A hint only: packets still flow.
    pub fn set_online(&self, node: NodeId, online: bool) {
        let mut model = self.model.lock();
        if let Some(n) = model.nodes.get_mut(node.0) {
            n.online = online;
        }
        model.changed();
    }

    /// `node` sleeps (`true`) or wakes: off the network and offline while asleep.
    pub fn set_asleep(&self, node: NodeId, asleep: bool) {
        for ip in self.ips(node) {
            self.net.set_down(ip, asleep);
        }
        let mut model = self.model.lock();
        if let Some(n) = model.nodes.get_mut(node.0) {
            n.asleep = asleep;
            n.online = !asleep;
        }
        model.changed();
    }

    /// Packets between `a` and `b` take `path` over `link` from now on.
    pub fn set_path(&self, a: NodeId, b: NodeId, path: SimPath, link: Link) {
        for (x, y) in [(ipv4(a), ipv4(b)), (ipv6(a), ipv6(b))] {
            self.net.set_path(x, y, Faults::over(link));
        }
        let mut model = self.model.lock();
        model.paths.insert(pair(a.0, b.0), path);
        model.changed();
    }

    /// `node`'s `LocalAPI` fails as `fault` says from now on; a watch it served ends when it
    /// stops answering.
    pub fn set_api(&self, node: NodeId, fault: ApiFault) {
        let mut model = self.model.lock();
        if let Some(n) = model.nodes.get_mut(node.0) {
            n.api = fault;
        }
        if fault != ApiFault::Up {
            model.watchers.retain(|(at, _)| *at != node.0);
        }
    }

    /// `node`'s `LocalAPI`.
    #[must_use]
    pub fn local_api(&self, node: NodeId) -> Arc<dyn Tailnet> {
        Arc::new(SimLocalApi {
            model: Arc::clone(&self.model),
            node: node.0,
        })
    }
}

const fn pair(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

impl Model {
    /// Tell every open watch that something changed.
    fn changed(&mut self) {
        self.watchers
            .retain(|(_, tx)| match tx.try_send(Notify::default()) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
    }

    /// Node `i` as node `viewer`'s daemon reports it.
    fn view(&self, viewer: usize, i: usize) -> Option<Node> {
        let n = self.nodes.get(i)?;
        let id = NodeId(i);
        let path = (viewer != i).then(|| {
            self.paths
                .get(&pair(viewer, i))
                .cloned()
                .unwrap_or(SimPath::Direct)
        });
        let (cur_addr, relay, peer_relay, active) = match path {
            None => (String::new(), "fra".to_owned(), String::new(), false),
            Some(SimPath::Direct) => (
                format!("192.0.2.{}:41641", i % 250 + 1),
                "fra".to_owned(),
                String::new(),
                true,
            ),
            Some(SimPath::PeerRelay) => (
                String::new(),
                "fra".to_owned(),
                "100.64.0.250:40000:vni:1".to_owned(),
                true,
            ),
            Some(SimPath::Derp(region)) => (String::new(), region, String::new(), true),
        };
        Some(Node {
            id: stable_id(id),
            host_name: n.spec.name.clone(),
            dns_name: format!("{}.{SUFFIX}.", n.spec.name),
            os: n.spec.os.clone(),
            ips: vec![ipv4(id), ipv6(id)],
            tags: n.spec.tags.clone(),
            online: n.online && !n.asleep,
            user: n.spec.user,
            cur_addr,
            relay,
            peer_relay,
            active,
        })
    }

    fn status(&self, viewer: usize) -> Status {
        let peer = (0..self.nodes.len())
            .filter(|i| *i != viewer)
            .filter_map(|i| Some((format!("nodekey:{i}"), self.view(viewer, i)?)))
            .collect();
        Status {
            backend_state: BackendState::Running,
            me: self.view(viewer, viewer),
            peer,
            magic_dns_suffix: SUFFIX.to_owned(),
            current_tailnet: Some(CurrentTailnet {
                name: "sim".to_owned(),
            }),
        }
    }

    fn whois(&self, viewer: usize, ip: IpAddr) -> Option<WhoIs> {
        let ip = ip.to_canonical();
        let i = (0..self.nodes.len()).find(|i| ipv4(NodeId(*i)) == ip || ipv6(NodeId(*i)) == ip)?;
        let n = self.nodes.get(i)?;
        let id = NodeId(i);
        let mut cap_map = BTreeMap::new();
        if let Some(roles) = self.grants.get(&(i, viewer)) {
            cap_map.insert(CAP.to_owned(), vec![serde_json::json!({ "roles": roles })]);
        }
        let tagged = !n.spec.tags.is_empty();
        Some(WhoIs {
            node: WhoNode {
                stable_id: stable_id(id),
                name: format!("{}.{SUFFIX}.", n.spec.name),
                user: n.spec.user,
                tags: n.spec.tags.clone(),
                addresses: vec![format!("{}/32", ipv4(id)), format!("{}/128", ipv6(id))],
            },
            user_profile: UserProfile {
                id: n.spec.user,
                login_name: if tagged {
                    "tagged-devices".to_owned()
                } else {
                    format!("user{}@sim", n.spec.user)
                },
                display_name: String::new(),
            },
            cap_map,
        })
    }
}

/// One node's fake `LocalAPI`.
struct SimLocalApi {
    model: Arc<Mutex<Model>>,
    node: usize,
}

impl SimLocalApi {
    /// Fails as the node's [`ApiFault`] says, after the time that takes.
    async fn answering(&self) -> Result<(), LocalApiError> {
        let fault = self
            .model
            .lock()
            .nodes
            .get(self.node)
            .map_or(ApiFault::Down, |n| n.api);
        match fault {
            ApiFault::Up => Ok(()),
            ApiFault::Down => Err(LocalApiError::Io(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "nothing listens at the local API",
            ))),
            ApiFault::Unauthorized => Err(LocalApiError::Status {
                code: 401,
                body: "bad token".to_owned(),
            }),
            ApiFault::Slow => {
                tokio::time::sleep(SLOW_API).await;
                Err(LocalApiError::Timeout)
            }
        }
    }
}

#[async_trait::async_trait]
impl Tailnet for SimLocalApi {
    async fn status(&self) -> Result<Status, LocalApiError> {
        self.answering().await?;
        Ok(self.model.lock().status(self.node))
    }

    async fn whois(&self, from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError> {
        self.answering().await?;
        Ok(self.model.lock().whois(self.node, from.ip()))
    }

    async fn watch(&self) -> Result<Watch, LocalApiError> {
        self.answering().await?;
        let (tx, rx) = mpsc::channel(64);
        let _initial = tx.try_send(Notify {
            state: Some(BackendState::Running),
            error: None,
        });
        self.model.lock().watchers.push((self.node, tx));
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use onicode_tailnet::{Grant, Path, Role};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn every_node_sees_one_tailnet_from_its_own_side() {
        let tailnet = SimTailnet::new(Faults::CLEAR, 1);
        let mac = tailnet.add(NodeSpec::owned("mac", 2));
        let laptop = tailnet.add(NodeSpec::owned("laptop", 2));
        let vps = tailnet.add(NodeSpec::tagged("vps", "tag:onicode"));
        let stranger = tailnet.add(NodeSpec::owned("other", 7));
        tailnet.set_path(
            mac,
            laptop,
            SimPath::Derp("nyc".to_owned()),
            SimTailnet::derp_link(),
        );
        tailnet.grant(vps, mac, &["engine"]);

        let api = tailnet.local_api(mac);
        let status = api.status().await.unwrap();
        assert_eq!(status.me.as_ref().unwrap().ips, vec![ipv4(mac), ipv6(mac)]);
        assert_eq!(status.peer.len(), 3);
        let laptop_seen = status.node_at(ipv4(laptop)).unwrap();
        assert_eq!(laptop_seen.path(), Path::Derp("nyc".to_owned()));
        assert!(matches!(
            status.node_at(ipv4(vps)).unwrap().path(),
            Path::Direct(_)
        ));

        let at = |n| SocketAddr::new(ipv4(n), 7433);
        let owner = status.me.as_ref().map(|me| me.user);
        let mine = api.whois(at(laptop)).await.unwrap().unwrap();
        assert_eq!(Grant::of(&mine, owner), Grant::OWNER);
        let granted = api.whois(at(vps)).await.unwrap().unwrap();
        assert!(Grant::of(&granted, owner).allows(Role::Engine));
        let other = api.whois(at(stranger)).await.unwrap().unwrap();
        assert!(!Grant::of(&other, owner).any());
        assert!(
            api.whois("100.64.9.9:1".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
        let from_laptop = tailnet
            .local_api(laptop)
            .whois(at(vps))
            .await
            .unwrap()
            .unwrap();
        assert!(
            !Grant::of(&from_laptop, Some(2)).any(),
            "the grant names the mac only"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_watch_hears_each_change_and_a_failing_api_fails_as_the_real_one() {
        let tailnet = SimTailnet::new(Faults::CLEAR, 1);
        let mac = tailnet.add(NodeSpec::owned("mac", 2));
        let laptop = tailnet.add(NodeSpec::owned("laptop", 2));
        let api = tailnet.local_api(mac);
        let mut watch = api.watch().await.unwrap();
        assert_eq!(
            watch.recv().await.unwrap().state,
            Some(BackendState::Running)
        );
        tailnet.set_online(laptop, false);
        assert!(watch.recv().await.is_some());
        assert!(
            !api.status()
                .await
                .unwrap()
                .node_at(ipv4(laptop))
                .unwrap()
                .online
        );

        tailnet.set_api(mac, ApiFault::Unauthorized);
        assert!(
            watch.recv().await.is_none(),
            "the watch ends with the daemon"
        );
        assert!(matches!(
            api.status().await,
            Err(LocalApiError::Status { code: 401, .. })
        ));
        tailnet.set_api(mac, ApiFault::Slow);
        let start = tokio::time::Instant::now();
        assert!(matches!(api.status().await, Err(LocalApiError::Timeout)));
        assert_eq!(start.elapsed(), SLOW_API);
        tailnet.set_api(mac, ApiFault::Down);
        assert!(matches!(
            api.whois(SocketAddr::new(ipv4(laptop), 1)).await,
            Err(LocalApiError::Io(_))
        ));
    }
}
