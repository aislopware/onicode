//! Which peers an endpoint lets in, and in what role: decided once per incoming connection,
//! before the handshake.
//!
//! Ported from slopty's `slopty-net` `admission.rs` (MIT), reduced to whois: with no
//! encryption and no pairing, the tailnet is the boundary, and Tailscale says who is on the
//! other end. A peer is let in when it is:
//!
//! * on loopback, as the owner;
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
use parking_lot::Mutex;
use tokio::time::Instant;

/// How long the local node's owner, and a caller's whois, are taken as read before they are
/// asked again.
pub const WHOIS_TTL: Duration = Duration::from_secs(60);

/// Who a let-in peer is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    /// The roles it may take.
    pub grant: Grant,
    /// Its tailnet node's stable id; empty for loopback, which names none.
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

/// Who may connect: loopback always, and the tailnet by its word.
///
/// Its clones share one whois cache.
#[derive(Clone)]
pub struct Admission {
    tailnet: Option<Arc<dyn Tailnet>>,
    owner: Arc<Mutex<Option<Owner>>>,
    whois: Arc<Mutex<HashMap<IpAddr, Cached>>>,
}

impl fmt::Debug for Admission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Admission")
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
    /// Loopback, and the tailnet through `tailnet`; without one, no tailnet peer.
    #[must_use]
    pub fn new(tailnet: Option<Arc<dyn Tailnet>>) -> Self {
        Self {
            tailnet,
            owner: Arc::default(),
            whois: Arc::default(),
        }
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
        if ip.is_loopback() {
            return Verdict::Admit(Admitted {
                grant: Grant::OWNER,
                node: String::new(),
                name: String::new(),
            });
        }
        if !on_tailnet(ip) {
            return Verdict::Refuse("outside the tailnet");
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

/// Whether `ip` is one of Tailscale's addresses: its CGNAT block `100.64.0.0/10` or its IPv6 ULA
/// prefix `fd7a:115c:a1e0::/48`. An IPv4-mapped IPv6 address counts as its IPv4 self.
#[must_use]
pub fn on_tailnet(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => u32::from(v4) & 0xffc0_0000 == 0x6440_0000,
        IpAddr::V6(v6) => u128::from(v6) >> 80 == 0xfd7a_115c_a1e0,
    }
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

    /// With no Tailscale to ask, loopback gets in and nothing else does: not the tailnet by its
    /// address alone, and not the LAN.
    #[tokio::test]
    async fn without_a_daemon_only_loopback_gets_in() {
        let a = Admission::new(None);
        for yes in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            assert!(
                matches!(a.check(at(yes)).await, Verdict::Admit(ref ad) if ad.grant == Grant::OWNER),
                "{yes}"
            );
        }
        for no in [
            "100.64.0.3",
            "::ffff:100.101.102.103",
            "fd7a:115c:a1e0:ab12::1",
            "10.1.2.3",
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
        let a = Admission::new(Some(tailnet.local_api(me)));
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
    fn tailscale_s_addresses_are_its_cgnat_block_and_ula_prefix() {
        for yes in [
            "100.64.0.0",
            "100.127.255.254",
            "::ffff:100.64.0.1",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff",
        ] {
            assert!(on_tailnet(ip(yes)), "{yes}");
        }
        for no in [
            "100.63.255.255",
            "100.128.0.0",
            "10.1.2.3",
            "fd7a:115c:a1e1::1",
            "fd7a:115c:a1df:ffff::1",
            "::1",
        ] {
            assert!(!on_tailnet(ip(no)), "{no}");
        }
    }
}
