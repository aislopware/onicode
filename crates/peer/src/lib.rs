//! onicode's peers over the tailnet: many clients and many engines, each pair joined by one
//! plaintext QUIC link that the tailnet carries, encrypts and vouches for.
//!
//! * [`PeerEndpoint`]: the QUIC endpoints on this node's tailnet addresses, each socket bound to
//!   the tailnet's interface, and the check of every caller by Tailscale's whois before its
//!   handshake (failing closed).
//! * [`PeerDirectory`]: which engines the tailnet has, from its status, kept current by its change
//!   notifications and confirmed by a probe of each.
//! * [`PeerLinks`]: one link per device: dials deduplicated (both ends keep the link the smaller
//!   device-id dialled), redialled with backoff, kept alive on the control stream, with the path
//!   each takes; RPC calls, streams and datagrams to a device by its id.
//! * [`carrier`]: the existing RPC envelopes over one QUIC stream per call.
//!
//! No relay, no pairing and no crypto of its own: a peer is who the tailnet says it is.

#![deny(unsafe_code)]

pub mod carrier;
pub mod directory;
pub mod endpoint;
#[cfg(unix)]
#[allow(unsafe_code)]
mod iface;
pub mod links;

pub use carrier::PeerCarrier;
pub use directory::{Peer, PeerDirectory};
pub use endpoint::{PeerConfig, PeerEndpoint, PeerSession, Sockets, SystemSockets};
pub use links::{LinkInfo, LinkState, LinksConfig, PathReport, PeerLinks, StreamHandler};
use onicode_net::NetError;
use onicode_tailnet::LocalApiError;

/// What reaching a peer can fail with.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// The link, or the dial, failed.
    #[error(transparent)]
    Net(#[from] NetError),
    /// The tailnet daemon did not answer.
    #[error(transparent)]
    Tailnet(#[from] LocalApiError),
    /// The call over the link failed.
    #[error(transparent)]
    Rpc(#[from] onicode_rpc::RpcError),
    /// Tailscale is not logged in and up on this machine.
    #[error("tailscale is not running on this machine")]
    NotRunning,
    /// The tailnet does not vouch for the address.
    #[error("refused: {0}")]
    Refused(&'static str),
    /// The tailnet grants the peer no role it would need.
    #[error("the tailnet grants {0} no role here")]
    NotGranted(String),
    /// No engine with this device-id is on the tailnet now.
    #[error("no device {0} on the tailnet")]
    Unknown(String),
    /// No endpoint of this node can reach the address (no IPv6 socket, say).
    #[error("no socket of this node reaches {0}")]
    NoEndpoint(std::net::SocketAddr),
    /// Binding a socket on a tailnet address failed.
    #[error("bind {addr}: {source}")]
    Bind {
        /// The address.
        addr: std::net::SocketAddr,
        /// What the OS said.
        #[source]
        source: std::io::Error,
    },
    /// The device was not reachable at the last try; the next is due later.
    #[error("{device} is not reachable ({reason}); trying again in {retry_in:?}")]
    Unreachable {
        /// The device.
        device: String,
        /// The last failure.
        reason: String,
        /// When the next dial is due.
        retry_in: std::time::Duration,
    },
    /// The links are shut down.
    #[error("the links are shut down")]
    Shutdown,
}

impl From<PeerError> for onicode_rpc::RpcError {
    /// A call that never reached its peer: transient, so the caller may retry.
    fn from(error: PeerError) -> Self {
        match error {
            PeerError::Rpc(error) => error,
            error => Self::Transport(error.to_string()),
        }
    }
}
