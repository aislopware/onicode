//! The tailnet as a list of engines the person typed, for a node with no `LocalAPI` to read
//! (iOS: the Tailscale app keeps its daemon to itself).
//!
//! Each entry is a `MagicDNS` name (`studio`, `studio.tail1234.ts.net`) or a tailnet address
//! (`100.101.102.103`, `fd7a:115c:a1e0::1`). As a [`Tailnet`]:
//!
//! * `status`: this node holds the tailnet addresses its own interfaces carry (the system Tailscale
//!   client put them there), and each entry that resolves to a tailnet address is a node of this
//!   node's user, online, so the [`crate::PeerDirectory`] probes it for an engine.
//! * `whois`: an entry's address is that node, of this node's user, so admission lets it in as the
//!   owner's own machine; any other address is no node. `WireGuard` already binds a tailnet source
//!   address to the node that holds it, so the list only says which of them to talk to.
//! * `watch`: a notification on every change of the list ([`EngineList::set_hosts`]), on
//!   [`EngineList::poke`], and every [`RELOOK`], so an engine that was away is probed again.
//!
//! The engine at the far end admits this node by its own daemon's whois, as it admits any
//! viewer: the list grants this node nothing there.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use onicode_net::admission::on_tailnet;
use onicode_tailnet::whois::{UserProfile, WhoNode};
use onicode_tailnet::{BackendState, LocalApiError, Node, Notify, Status, Tailnet, Watch, WhoIs};
use parking_lot::Mutex;
use tokio::sync::mpsc;

/// How often the list is offered for a fresh look while nothing else changes it.
pub const RELOOK: Duration = Duration::from_secs(30);

/// The user every listed engine and this node belong to.
const USER: i64 = 0;

/// The addresses this node holds on the tailnet.
type LocalAddrs = Box<dyn Fn() -> io::Result<Vec<IpAddr>> + Send + Sync>;

/// A listed engine, as last resolved.
#[derive(Clone, Debug)]
struct Listed {
    host: String,
    ips: Vec<IpAddr>,
}

/// The engines the person listed, read as a tailnet.
pub struct EngineList {
    hosts: Mutex<Vec<String>>,
    resolved: Mutex<Vec<Listed>>,
    local: LocalAddrs,
    watchers: Mutex<Vec<mpsc::Sender<Notify>>>,
}

impl std::fmt::Debug for EngineList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineList")
            .field("hosts", &*self.hosts.lock())
            .finish_non_exhaustive()
    }
}

impl EngineList {
    /// `hosts` on this machine, whose tailnet addresses its interfaces hold.
    #[must_use]
    pub fn system(hosts: Vec<String>) -> Arc<Self> {
        Self::with_local(hosts, Box::new(system_addrs))
    }

    /// `hosts`, with this node at `addrs`: a simulated tailnet's node in tests.
    #[must_use]
    pub fn at(hosts: Vec<String>, addrs: Vec<IpAddr>) -> Arc<Self> {
        Self::with_local(hosts, Box::new(move || Ok(addrs.clone())))
    }

    fn with_local(hosts: Vec<String>, local: LocalAddrs) -> Arc<Self> {
        Arc::new(Self {
            hosts: Mutex::new(clean(hosts)),
            resolved: Mutex::default(),
            local,
            watchers: Mutex::default(),
        })
    }

    /// The engines listed now.
    #[must_use]
    pub fn hosts(&self) -> Vec<String> {
        self.hosts.lock().clone()
    }

    /// Lists `hosts` in place of the engines before, and has them looked at now.
    pub fn set_hosts(&self, hosts: Vec<String>) {
        *self.hosts.lock() = clean(hosts);
        self.poke();
    }

    /// Has the list looked at now (the app came to the foreground, the network changed).
    pub fn poke(&self) {
        self.watchers.lock().retain(|tx| {
            !matches!(
                tx.try_send(Notify::default()),
                Err(mpsc::error::TrySendError::Closed(_))
            )
        });
    }

    /// Resolves every listed engine to its tailnet addresses; an entry that resolves to none
    /// is left out until it does.
    async fn resolve(&self) -> Vec<Listed> {
        let hosts = self.hosts();
        let mut listed = Vec::with_capacity(hosts.len());
        for host in hosts {
            let ips = resolve(&host).await;
            if ips.is_empty() {
                tracing::debug!(host, "a listed engine has no tailnet address now");
                continue;
            }
            listed.push(Listed { host, ips });
        }
        *self.resolved.lock() = listed.clone();
        listed
    }
}

#[async_trait::async_trait]
impl Tailnet for EngineList {
    async fn status(&self) -> Result<Status, LocalApiError> {
        let local = (self.local)().map_err(LocalApiError::Io)?;
        let listed = self.resolve().await;
        let running = !local.is_empty();
        let me = running.then(|| node("self", "self", local));
        let peer = listed
            .into_iter()
            .map(|l| (l.host.clone(), node(&id(&l.host), &l.host, l.ips)))
            .collect::<BTreeMap<_, _>>();
        Ok(Status {
            backend_state: if running {
                BackendState::Running
            } else {
                BackendState::Stopped
            },
            me,
            peer,
            magic_dns_suffix: String::new(),
            current_tailnet: None,
        })
    }

    async fn whois(&self, from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError> {
        let ip = from.ip().to_canonical();
        let known = self.resolved.lock().clone();
        let listed = if known.iter().any(|l| l.ips.contains(&ip)) {
            known
        } else {
            self.resolve().await
        };
        Ok(listed
            .into_iter()
            .find(|l| l.ips.contains(&ip))
            .map(|l| WhoIs {
                node: WhoNode {
                    stable_id: id(&l.host),
                    name: l.host.clone(),
                    user: USER,
                    tags: Vec::new(),
                    addresses: l.ips.iter().map(|ip| ip.to_string()).collect(),
                },
                user_profile: UserProfile {
                    id: USER,
                    login_name: String::new(),
                    display_name: String::new(),
                },
                cap_map: BTreeMap::new(),
            }))
    }

    async fn watch(&self) -> Result<Watch, LocalApiError> {
        let (tx, rx) = mpsc::channel(4);
        let ticker = tx.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(RELOOK);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            every.tick().await;
            loop {
                every.tick().await;
                if let Err(mpsc::error::TrySendError::Closed(_)) =
                    ticker.try_send(Notify::default())
                {
                    return;
                }
            }
        });
        self.watchers.lock().push(tx);
        Ok(rx)
    }
}

/// A listed engine's node id: stable for as long as the entry is listed.
fn id(host: &str) -> String {
    format!("listed:{host}")
}

fn node(id: &str, host: &str, ips: Vec<IpAddr>) -> Node {
    Node {
        id: id.to_owned(),
        host_name: host.to_owned(),
        dns_name: format!("{host}."),
        os: String::new(),
        ips,
        tags: Vec::new(),
        online: true,
        user: USER,
        cur_addr: String::new(),
        relay: String::new(),
        peer_relay: String::new(),
        active: false,
    }
}

/// Entries trimmed, empty ones and repeats dropped, in the order given.
fn clean(hosts: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(hosts.len());
    for host in hosts {
        let host = host.trim().trim_end_matches('.').to_owned();
        if !host.is_empty() && !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

/// `host`'s tailnet addresses, IPv4 first: the address itself, or what `MagicDNS` answers.
async fn resolve(host: &str) -> Vec<IpAddr> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return if on_tailnet(ip) { vec![ip] } else { Vec::new() };
    }
    let mut ips: Vec<IpAddr> = match tokio::net::lookup_host((host, 0)).await {
        Ok(found) => found
            .map(|a| a.ip().to_canonical())
            .filter(|ip| on_tailnet(*ip))
            .collect(),
        Err(e) => {
            tracing::debug!(host, error = %e, "a listed engine's name does not resolve");
            Vec::new()
        }
    };
    ips.sort_by_key(IpAddr::is_ipv6);
    ips.dedup();
    ips
}

#[cfg(unix)]
fn system_addrs() -> io::Result<Vec<IpAddr>> {
    crate::iface::tailnet_addrs()
}

#[cfg(not(unix))]
fn system_addrs() -> io::Result<Vec<IpAddr>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(hosts: &[&str]) -> Arc<EngineList> {
        EngineList::at(
            hosts.iter().map(|h| (*h).to_owned()).collect(),
            vec!["100.64.0.9".parse().unwrap()],
        )
    }

    /// Listed addresses are nodes of this node's user; an address outside the tailnet, a
    /// blank entry and a repeat are not listed at all.
    #[tokio::test]
    async fn listed_addresses_are_the_owners_nodes() {
        let engines = list(&[
            " 100.64.0.2 ",
            "100.64.0.2",
            "",
            "192.0.2.1",
            "[fd7a:115c:a1e0::3]",
        ]);
        let status = engines.status().await.unwrap();
        assert!(status.running());
        assert_eq!(
            status.me.as_ref().unwrap().ips,
            vec!["100.64.0.9".parse::<IpAddr>().unwrap()]
        );
        let ids: Vec<_> = status.peer.values().map(|n| n.id.clone()).collect();
        assert_eq!(ids, vec!["listed:100.64.0.2", "listed:[fd7a:115c:a1e0::3]"]);
        let who = engines
            .whois("100.64.0.2:7000".parse().unwrap())
            .await
            .unwrap()
            .expect("a listed engine is a node");
        assert_eq!(who.node.stable_id, "listed:100.64.0.2");
        assert_eq!(
            onicode_tailnet::Grant::of(&who, status.me.map(|me| me.user)),
            onicode_tailnet::Grant::OWNER
        );
        assert!(
            engines
                .whois("100.64.0.7:7000".parse().unwrap())
                .await
                .unwrap()
                .is_none(),
            "an address nobody listed is no node"
        );
    }

    /// Without a tailnet address of its own the node is not up; a new list is looked at once.
    #[tokio::test]
    async fn no_address_is_not_running_and_a_new_list_notifies() {
        let engines = EngineList::at(vec!["100.64.0.2".into()], Vec::new());
        assert!(!engines.status().await.unwrap().running());
        let mut watch = engines.watch().await.unwrap();
        engines.set_hosts(vec!["100.64.0.4".into()]);
        assert!(watch.recv().await.is_some());
        assert_eq!(engines.hosts(), vec!["100.64.0.4"]);
    }
}
