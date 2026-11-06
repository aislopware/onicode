//! Which peers an endpoint lets in, and in what role: decided once per incoming connection,
//! before the handshake.
//!
//! Ported from slopty's `slopty-net` `admission.rs` (MIT), reduced to whois: with no
//! encryption and no pairing, the tailnet is the boundary, and Tailscale says who is on the
//! other end. A peer is let in when it is:
//!
//! * on loopback, as the owner;
//! * in a range the settings list (a plain VPN), as the owner, since an address is all such a
//!   network says;
//! * a node of the tailnet the local Tailscale vouches for: the user's own machines as the owner,
//!   another user's or a tagged node in the roles a tailnet grant gives it
//!   (`onicode_tailnet::policy`).
//!
//! Where no Tailscale this process can read answers, every tailnet address is refused: fail
//! closed. A node the tailnet names but grants no role here finishes the handshake only to be
//! closed with `NOT_GRANTED` ([`Verdict::Ungranted`]), so its owner learns that a grant is what
//! is missing. Anything else is refused before the handshake: one packet, no state.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use onicode_tailnet::{Grant, Tailnet, WhoIs};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

/// An address range in CIDR notation (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`); a bare address
/// is a range of one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

/// A range that does not parse.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadRange(String);

impl Cidr {
    /// `net/prefix`, with the host bits of `net` cleared.
    ///
    /// # Errors
    ///
    /// A prefix past the family's bits.
    pub fn new(net: IpAddr, prefix: u8) -> Result<Self, BadRange> {
        let bits = if net.is_ipv4() { 32 } else { 128 };
        if prefix > bits {
            return Err(BadRange(format!("{net}/{prefix}: prefix past {bits}")));
        }
        let net = match net {
            IpAddr::V4(v4) => IpAddr::V4((u32::from(v4) & mask32(prefix)).into()),
            IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & mask128(prefix)).into()),
        };
        Ok(Self { net, prefix })
    }

    /// Whether `ip` falls in the range. An IPv4-mapped IPv6 address counts as its IPv4 self.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                u32::from(ip) & mask32(self.prefix) == u32::from(net)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                u128::from(ip) & mask128(self.prefix) == u128::from(net)
            }
            (IpAddr::V4(_), IpAddr::V6(_)) | (IpAddr::V6(_), IpAddr::V4(_)) => false,
        }
    }
}

fn mask32(prefix: u8) -> u32 {
    u32::MAX
        .checked_shl(32_u32.saturating_sub(u32::from(prefix)))
        .unwrap_or(0)
}

fn mask128(prefix: u8) -> u128 {
    u128::MAX
        .checked_shl(128_u32.saturating_sub(u32::from(prefix)))
        .unwrap_or(0)
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.net, self.prefix)
    }
}

impl std::str::FromStr for Cidr {
    type Err = BadRange;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let bad = || BadRange(format!("{s:?}: not an address range"));
        let (net, prefix) = if let Some((net, prefix)) = s.split_once('/') {
            let net: IpAddr = net.parse().map_err(|_ip| bad())?;
            (net, prefix.parse::<u8>().map_err(|_n| bad())?)
        } else {
            let net: IpAddr = s.parse().map_err(|_ip| bad())?;
            (net, if net.is_ipv4() { 32 } else { 128 })
        };
        Self::new(net, prefix)
    }
}

impl TryFrom<String> for Cidr {
    type Error = BadRange;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Cidr> for String {
    fn from(c: Cidr) -> Self {
        c.to_string()
    }
}

/// Tailscale's addresses: its CGNAT block and its IPv6 ULA prefix.
pub const TAILNET: &[&str] = &["100.64.0.0/10", "fd7a:115c:a1e0::/48"];

/// How long the local node's owner, and a caller's whois, are taken as read before they are
/// asked again.
pub const WHOIS_TTL: Duration = Duration::from_secs(60);

/// Who a let-in peer is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    /// The roles it may take.
    pub grant: Grant,
    /// Its tailnet node's stable id; empty for loopback and a listed range, which name none.
    pub node: String,
    /// Its `MagicDNS` name, for the log; empty where `node` is.
    pub name: String,
}

/// What the check decided of a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Let in.
    Admit(Admitted),
    /// A node the tailnet names, granted no role here: told so once the handshake is done.
    Ungranted,
    /// Refused, and why, for the log.
    Refuse(&'static str),
}

/// Who may connect: loopback always, the listed ranges, and the tailnet by its word.
///
/// Its clones share one list of ranges and one whois cache.
#[derive(Clone)]
pub struct Admission {
    allow: Arc<RwLock<Vec<Cidr>>>,
    tailnet: Option<Arc<dyn Tailnet>>,
    owner: Arc<Mutex<Option<Owner>>>,
    whois: Arc<Mutex<HashMap<IpAddr, Cached>>>,
}

impl fmt::Debug for Admission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Admission")
            .field("allow", &*self.allow.read())
            .field("tailnet", &self.tailnet.is_some())
            .finish_non_exhaustive()
    }
}

/// The user the local node belongs to, `None` when it is tagged, and when that was read.
#[derive(Clone, Copy, Debug)]
struct Owner {
    read: Instant,
    user: Option<i64>,
}

#[derive(Clone, Debug)]
struct Cached {
    read: Instant,
    who: WhoIs,
}

impl Admission {
    /// Loopback, the ranges in `allow`, and the tailnet through `tailnet`; without one, no
    /// tailnet peer.
    #[must_use]
    pub fn new(tailnet: Option<Arc<dyn Tailnet>>, allow: Vec<Cidr>) -> Self {
        Self {
            allow: Arc::new(RwLock::new(allow)),
            tailnet,
            owner: Arc::default(),
            whois: Arc::default(),
        }
    }

    /// The ranges let in by address besides loopback and the tailnet.
    #[must_use]
    pub fn ranges(&self) -> Vec<Cidr> {
        self.allow.read().clone()
    }

    /// Let in `allow` by address from now on, in place of the ranges before, on every clone.
    pub fn set_ranges(&self, allow: Vec<Cidr>) {
        *self.allow.write() = allow;
    }

    /// Forgets every whois and the owner read so far: the tailnet changed (a grant, a node's
    /// tags), so the next check asks the daemon again. Never lets anyone in by itself.
    pub fn forget(&self) {
        self.whois.lock().clear();
        *self.owner.lock() = None;
    }

    /// Whether the peer at `peer` may connect, and as what. A tailnet address is let in only
    /// as Tailscale vouches for it: with the wire unencrypted, an address alone is what a host
    /// on the same LAN could forge.
    pub async fn check(&self, peer: SocketAddr) -> Verdict {
        let ip = peer.ip().to_canonical();
        if ip.is_loopback() || self.allow.read().iter().any(|c| c.contains(ip)) {
            return Verdict::Admit(Admitted {
                grant: Grant::OWNER,
                node: String::new(),
                name: String::new(),
            });
        }
        if !on_tailnet(ip) {
            return Verdict::Refuse("outside the tailnet and the allowed ranges");
        }
        let Some(tailnet) = &self.tailnet else {
            return Verdict::Refuse("no Tailscale this process can read to say who is calling");
        };
        let who = match self
            .whois(tailnet.as_ref(), SocketAddr::new(ip, peer.port()))
            .await
        {
            Ok(Some(who)) => who,
            Ok(None) => return Verdict::Refuse("no node of the tailnet has this address"),
            Err(e) => {
                tracing::warn!(%peer, error = %e, "tailscale could not say who is calling");
                return Verdict::Refuse("tailscale could not say who is calling");
            }
        };
        let grant = Grant::of(&who, self.owner(tailnet.as_ref()).await);
        if grant.any() {
            Verdict::Admit(Admitted {
                grant,
                node: who.node.stable_id.clone(),
                name: who.node.name.clone(),
            })
        } else {
            tracing::info!(%peer, node = %who.node.name, user = %who.user_profile.login_name, "the tailnet grants no role");
            Verdict::Ungranted
        }
    }

    /// The whois of `peer`, from the cache while it is fresh.
    async fn whois(
        &self,
        tailnet: &dyn Tailnet,
        peer: SocketAddr,
    ) -> Result<Option<WhoIs>, onicode_tailnet::LocalApiError> {
        let ip = peer.ip();
        if let Some(cached) = self.whois.lock().get(&ip)
            && cached.read.elapsed() < WHOIS_TTL
        {
            return Ok(Some(cached.who.clone()));
        }
        let who = tailnet.whois(peer).await?;
        let mut cache = self.whois.lock();
        cache.retain(|_, c| c.read.elapsed() < WHOIS_TTL);
        match &who {
            Some(who) => {
                cache.insert(
                    ip,
                    Cached {
                        read: Instant::now(),
                        who: who.clone(),
                    },
                );
            }
            None => {
                cache.remove(&ip);
            }
        }
        Ok(who)
    }

    /// The user this node belongs to, `None` when it is tagged (a tagged node belongs to
    /// nobody). Read at most once a [`WHOIS_TTL`]; a daemon that does not answer keeps the
    /// last word, and with none no caller is the owner.
    async fn owner(&self, tailnet: &dyn Tailnet) -> Option<i64> {
        let known = *self.owner.lock();
        if let Some(known) = known
            && known.read.elapsed() < WHOIS_TTL
        {
            return known.user;
        }
        match tailnet.status().await {
            Ok(status) => {
                let user = status.me.filter(|me| me.tags.is_empty()).map(|me| me.user);
                *self.owner.lock() = Some(Owner {
                    read: Instant::now(),
                    user,
                });
                user
            }
            Err(e) => {
                tracing::warn!(error = %e, "tailscale did not say who owns this node");
                known.and_then(|known| known.user)
            }
        }
    }
}

/// Whether `ip` is one of Tailscale's addresses.
#[must_use]
pub fn on_tailnet(ip: IpAddr) -> bool {
    TAILNET
        .iter()
        .filter_map(|c| c.parse::<Cidr>().ok())
        .any(|c| c.contains(ip))
}

#[cfg(test)]
mod tests {
    use onicode_net_sim::tailnet::ipv4;
    use onicode_net_sim::{ApiFault, Faults, NodeSpec, SimTailnet};
    use onicode_tailnet::Role;

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn at(s: &str) -> SocketAddr {
        SocketAddr::new(ip(s), 5000)
    }

    /// With no Tailscale to ask, loopback and a listed range get in and nothing else does: not
    /// the tailnet by its address alone, and not the LAN.
    #[tokio::test]
    async fn without_a_daemon_only_loopback_and_the_listed_ranges_get_in() {
        let a = Admission::new(None, vec!["10.0.0.0/8".parse().unwrap()]);
        for yes in ["127.0.0.1", "::1", "::ffff:127.0.0.1", "10.1.2.3"] {
            assert!(
                matches!(a.check(at(yes)).await, Verdict::Admit(ref ad) if ad.grant == Grant::OWNER),
                "{yes}"
            );
        }
        for no in [
            "100.64.0.3",
            "::ffff:100.101.102.103",
            "fd7a:115c:a1e0:ab12::1",
            "192.168.1.9",
            "fd00::1",
            "fe80::1",
            "8.8.8.8",
            "0.0.0.0",
            "::",
        ] {
            assert!(matches!(a.check(at(no)).await, Verdict::Refuse(_)), "{no}");
        }
    }

    /// Through the tailnet: the owner's other machine is the owner, another user's machine is
    /// ungranted, a tagged node gets what its grant names, an address no node has nothing, and
    /// a daemon that fails refuses rather than lets in.
    #[tokio::test(start_paused = true)]
    async fn the_tailnet_says_who_is_calling_and_what_they_may_do() {
        let tailnet = SimTailnet::new(Faults::CLEAR, 1);
        let me = tailnet.add(NodeSpec::owned("mac", 2));
        let laptop = tailnet.add(NodeSpec::owned("laptop", 2));
        let other = tailnet.add(NodeSpec::owned("other", 7));
        let vps = tailnet.add(NodeSpec::tagged("vps", "tag:onicode"));
        let bare = tailnet.add(NodeSpec::tagged("ci", "tag:ci"));
        tailnet.grant(vps, me, &["engine"]);
        let a = Admission::new(Some(tailnet.local_api(me)), Vec::new());
        let peer = |n| SocketAddr::new(ipv4(n), 7433);
        let Verdict::Admit(mine) = a.check(peer(laptop)).await else {
            panic!("the owner's laptop")
        };
        assert_eq!(mine.grant, Grant::OWNER);
        assert_eq!(mine.node, onicode_net_sim::tailnet::stable_id(laptop));
        assert_eq!(
            a.check(peer(other)).await,
            Verdict::Ungranted,
            "another user's node"
        );
        assert_eq!(
            a.check(peer(bare)).await,
            Verdict::Ungranted,
            "a tagged node with no grant"
        );
        let Verdict::Admit(granted) = a.check(peer(vps)).await else {
            panic!("the granted node")
        };
        assert!(
            granted.grant.allows(Role::Engine)
                && !granted.grant.allows(Role::Viewer)
                && !granted.grant.owner
        );
        assert!(
            matches!(a.check(at("100.64.9.9")).await, Verdict::Refuse(_)),
            "no such node"
        );

        // Cached for a minute: a daemon gone a moment ago still vouches for whom it named.
        tailnet.set_api(me, ApiFault::Down);
        assert!(
            matches!(a.check(peer(laptop)).await, Verdict::Admit(_)),
            "from the cache"
        );
        tokio::time::sleep(WHOIS_TTL).await;
        assert!(
            matches!(a.check(peer(laptop)).await, Verdict::Refuse(_)),
            "fail closed once stale"
        );
    }

    #[test]
    fn ranges_parse_normalise_and_refuse_nonsense() {
        let c: Cidr = "10.1.2.3/8".parse().unwrap();
        assert_eq!(c.to_string(), "10.0.0.0/8", "host bits cleared");
        assert_eq!(
            "fd7a:115c:a1e0::5/48".parse::<Cidr>().unwrap().to_string(),
            "fd7a:115c:a1e0::/48"
        );
        assert_eq!("1.2.3.4".parse::<Cidr>().unwrap().to_string(), "1.2.3.4/32");
        let everything: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(everything.contains(ip("8.8.8.8")) && !everything.contains(ip("::1")));
        for bad in [
            "",
            "10.0.0.0/33",
            "::/129",
            "10.0.0/8",
            "peer",
            "10.0.0.0/x",
            "/8",
        ] {
            assert!(bad.parse::<Cidr>().is_err(), "{bad:?}");
        }
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Cidr>(&json).unwrap(), c);
        assert!(on_tailnet(ip("100.127.255.254")) && !on_tailnet(ip("100.128.0.0")));
        assert!(on_tailnet(ip("::ffff:100.64.0.1")) && !on_tailnet(ip("100.63.255.255")));
    }
}
