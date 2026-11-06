//! The tailnet as the local daemon sees it (`GET /localapi/v0/status`, `ipnstate.Status`).
//!
//! Ported from slopty's `slopty-tailnet` `status.rs` (MIT); [`BackendState`] from
//! `slopty-proto`.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

/// Where the daemon stands (`ipn.State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendState {
    /// Just started, with no state yet.
    NoState,
    /// Another user of the machine owns the daemon.
    InUseOtherUser,
    /// Signed out: the node must log in.
    NeedsLogin,
    /// Logged in, waiting for an admin to approve the machine.
    NeedsMachineAuth,
    /// Turned off by its user.
    Stopped,
    /// Connecting.
    Starting,
    /// Logged in and up.
    Running,
    /// A state this build does not know.
    #[serde(other)]
    Other,
}

impl BackendState {
    /// The state `ipn.State` numbers as `n`, as `watch-ipn-bus` writes it.
    #[must_use]
    pub const fn from_ipn(n: i64) -> Self {
        match n {
            0 => Self::NoState,
            1 => Self::InUseOtherUser,
            2 => Self::NeedsLogin,
            3 => Self::NeedsMachineAuth,
            4 => Self::Stopped,
            5 => Self::Starting,
            6 => Self::Running,
            _ => Self::Other,
        }
    }
}

impl std::fmt::Display for BackendState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoState => "NoState",
            Self::InUseOtherUser => "InUseOtherUser",
            Self::NeedsLogin => "NeedsLogin",
            Self::NeedsMachineAuth => "NeedsMachineAuth",
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Other => "an unknown state",
        })
    }
}

/// The daemon's view: this node, its peers and the tailnet's name.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Status {
    /// Where the daemon stands: [`BackendState::Running`] once the node is logged in and up.
    pub backend_state: BackendState,
    /// This node, absent before login.
    #[serde(rename = "Self")]
    pub me: Option<Node>,
    /// Every peer the node's map holds, by public key.
    #[serde(default, deserialize_with = "crate::null_as_default")]
    pub peer: BTreeMap<String, Node>,
    /// The suffix of every `MagicDNS` name, e.g. `tail1234.ts.net`.
    #[serde(rename = "MagicDNSSuffix", default)]
    pub magic_dns_suffix: String,
    /// The tailnet, absent before login.
    #[serde(default)]
    pub current_tailnet: Option<CurrentTailnet>,
}

/// The tailnet the node is on.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CurrentTailnet {
    /// Its name as the control plane gives it.
    pub name: String,
}

/// A node of the tailnet (`ipnstate.PeerStatus`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Node {
    /// The node's id at the control plane.
    #[serde(rename = "ID")]
    pub id: String,
    /// The machine's own host name.
    pub host_name: String,
    /// Its `MagicDNS` name, with a trailing dot.
    #[serde(rename = "DNSName")]
    pub dns_name: String,
    /// `macOS`, `iOS`, `linux`, …
    #[serde(rename = "OS")]
    pub os: String,
    /// Its tailnet addresses, IPv4 first.
    #[serde(
        rename = "TailscaleIPs",
        default,
        deserialize_with = "crate::null_as_default"
    )]
    pub ips: Vec<IpAddr>,
    /// Its tags; a tagged node belongs to no user.
    #[serde(default, deserialize_with = "crate::null_as_default")]
    pub tags: Vec<String>,
    /// Whether it is connected to the control plane.
    #[serde(default)]
    pub online: bool,
    /// The user it belongs to (meaningless when it is tagged).
    #[serde(rename = "UserID", default)]
    pub user: i64,
    /// The address a direct path uses, empty when there is none.
    #[serde(default)]
    pub cur_addr: String,
    /// Its home DERP region.
    #[serde(default)]
    pub relay: String,
    /// The peer relay a path goes through, empty when none.
    #[serde(default)]
    pub peer_relay: String,
    /// Whether traffic has flowed recently.
    #[serde(default)]
    pub active: bool,
}

/// How packets to a peer travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Path {
    /// Straight to it over UDP, at this address.
    Direct(SocketAddr),
    /// Through another node of the tailnet acting as a relay.
    PeerRelay(String),
    /// Through a DERP server in this region: TCP, and often a detour.
    Derp(String),
    /// Nothing has flowed lately, so no path is chosen yet: the next packet starts on DERP
    /// while a direct path is tried.
    Idle,
}

impl Node {
    /// The path the daemon uses to this node now.
    #[must_use]
    pub fn path(&self) -> Path {
        if let Ok(direct) = self.cur_addr.parse() {
            Path::Direct(direct)
        } else if !self.peer_relay.is_empty() {
            Path::PeerRelay(self.peer_relay.clone())
        } else if self.active {
            Path::Derp(self.relay.clone())
        } else {
            Path::Idle
        }
    }

    /// Its tailnet IPv4 address, which every node has.
    #[must_use]
    pub fn ipv4(&self) -> Option<IpAddr> {
        self.ips.iter().copied().find(IpAddr::is_ipv4)
    }

    /// Its `MagicDNS` name without the trailing dot.
    #[must_use]
    pub fn name(&self) -> &str {
        self.dns_name.trim_end_matches('.')
    }

    /// Whether `ip` is one of its addresses.
    #[must_use]
    pub fn has(&self, ip: IpAddr) -> bool {
        self.ips.contains(&ip.to_canonical())
    }
}

impl Status {
    /// Whether the node is logged in and up.
    #[must_use]
    pub fn running(&self) -> bool {
        self.backend_state == BackendState::Running
    }

    /// The peer (or this node) with address `ip`.
    #[must_use]
    pub fn node_at(&self, ip: IpAddr) -> Option<&Node> {
        self.me.iter().chain(self.peer.values()).find(|n| n.has(ip))
    }
}

/// A status fixture, shared with the other modules' tests.
#[cfg(test)]
pub mod tests {
    use super::*;

    /// A status as tailscale 1.102 writes it, trimmed to what is read here.
    ///
    /// This Mac, a laptop idle on its DERP region, a tagged gateway reached directly, a peer
    /// through a peer relay, and a phone that is talking over DERP.
    pub const STATUS: &str = r#"{
      "Version": "1.102.4", "BackendState": "Running", "MagicDNSSuffix": "tail1234.ts.net",
      "CurrentTailnet": { "Name": "me@example.com", "MagicDNSSuffix": "tail1234.ts.net" },
      "Self": { "ID": "n1", "HostName": "mac-studio", "DNSName": "mac-studio.tail1234.ts.net.",
        "OS": "macOS", "TailscaleIPs": ["100.64.0.3", "fd7a:115c:a1e0::3"], "Tags": null,
        "Online": true, "UserID": 2, "CurAddr": "", "Relay": "fra", "PeerRelay": "", "Active": false },
      "Peer": {
        "nodekey:a": { "ID": "n2", "HostName": "laptop", "DNSName": "laptop.tail1234.ts.net.",
          "OS": "macOS", "TailscaleIPs": ["100.64.0.4", "fd7a:115c:a1e0::4"], "Online": true,
          "UserID": 2, "CurAddr": "", "Relay": "fra", "Active": false },
        "nodekey:b": { "ID": "n3", "HostName": "gateway", "DNSName": "gateway.tail1234.ts.net.",
          "OS": "linux", "TailscaleIPs": ["100.64.0.5"], "Tags": ["tag:onicode"],
          "Online": true, "UserID": 9, "CurAddr": "192.168.1.20:41641", "Relay": "fra", "Active": true },
        "nodekey:c": { "ID": "n4", "HostName": "far", "DNSName": "far.tail1234.ts.net.",
          "OS": "linux", "TailscaleIPs": ["100.64.0.6"], "Online": false, "UserID": 2,
          "CurAddr": "", "PeerRelay": "100.64.0.5:40000:vni:7", "Relay": "nyc", "Active": true },
        "nodekey:d": { "ID": "n5", "HostName": "localhost", "DNSName": "phone.tail1234.ts.net.",
          "OS": "iOS", "TailscaleIPs": ["100.64.0.7"], "Online": true, "UserID": 2,
          "CurAddr": "", "Relay": "fra", "Active": true }
      }
    }"#;

    /// [`STATUS`], read.
    pub fn status() -> Status {
        serde_json::from_str(STATUS).unwrap()
    }

    /// Each peer's path reads as the daemon chose it, and an address finds its node.
    #[test]
    fn each_peer_says_how_it_is_reached() {
        let s = status();
        assert!(s.running());
        let path = |ip: &str| s.node_at(ip.parse().unwrap()).unwrap().path();
        assert_eq!(
            path("100.64.0.4"),
            Path::Idle,
            "nothing flowing, no path yet"
        );
        assert_eq!(
            path("100.64.0.5"),
            Path::Direct("192.168.1.20:41641".parse().unwrap())
        );
        assert_eq!(
            path("100.64.0.6"),
            Path::PeerRelay("100.64.0.5:40000:vni:7".into())
        );
        assert_eq!(path("100.64.0.7"), Path::Derp("fra".into()));
        let me = s.me.as_ref().unwrap();
        assert_eq!(me.name(), "mac-studio.tail1234.ts.net");
        assert_eq!(me.ipv4(), Some("100.64.0.3".parse().unwrap()));
        assert!(me.tags.is_empty(), "null tags read as none");
        assert!(
            s.node_at("fd7a:115c:a1e0::4".parse().unwrap()).is_some(),
            "by IPv6 too"
        );
        assert!(
            s.node_at("::ffff:100.64.0.4".parse().unwrap()).is_some(),
            "mapped IPv4"
        );
        assert!(s.node_at("100.64.9.9".parse().unwrap()).is_none());
    }

    /// Before login there is no node and no peer map, and that is not an error.
    #[test]
    fn a_node_that_is_not_logged_in_has_no_peers() {
        let s: Status =
            serde_json::from_str(r#"{"BackendState":"NeedsLogin","Self":null,"Peer":null}"#)
                .unwrap();
        assert_eq!(s.backend_state, BackendState::NeedsLogin);
        assert!(!s.running());
        assert!(s.me.is_none() && s.peer.is_empty());
    }

    /// Every state the daemon writes reads as itself, and one this build does not know reads as
    /// not up rather than failing the whole status.
    #[test]
    fn each_backend_state_reads_by_name() {
        use BackendState::{
            InUseOtherUser, NeedsLogin, NeedsMachineAuth, NoState, Other, Running, Starting,
            Stopped,
        };
        let known = [
            NoState,
            InUseOtherUser,
            NeedsLogin,
            NeedsMachineAuth,
            Stopped,
            Starting,
            Running,
        ];
        for state in known {
            let s: Status =
                serde_json::from_str(&format!(r#"{{"BackendState":"{state}"}}"#)).unwrap();
            assert_eq!(s.backend_state, state);
            assert_eq!(s.running(), state == Running);
        }
        let s: Status = serde_json::from_str(r#"{"BackendState":"Rebooting"}"#).unwrap();
        assert_eq!(s.backend_state, Other);
        assert!(!s.running());
    }
}
