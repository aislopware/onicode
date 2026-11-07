//! The QUIC endpoints on this node's tailnet addresses, and who gets past them.
//!
//! After slopty's `slopty-net` listener (MIT): every caller is checked by Tailscale's whois
//! before its handshake, so an address the tailnet does not vouch for costs one packet (the
//! refusal) and gets no QUIC state; a node the tailnet names but grants no role finishes the
//! handshake only to be told so ([`close::NOT_GRANTED`]). An admitted caller's hello must ask
//! for a role its grant holds, and its device-id must not be one another node already holds
//! ([`close::DEVICE_MISMATCH`]): with no crypto of onicode's own, the tailnet node is the
//! identity and the device-id only names it.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use noq::VarInt;
use onicode_net::admission::{Admission, Admitted, Verdict};
use onicode_net::hello::{self, HANDSHAKE_TIMEOUT, HELLO_TIMEOUT};
use onicode_net::transport::{self, PORT};
use onicode_net::{Hello, NetError, Role, Session, close};
use onicode_tailnet::Tailnet;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::PeerError;

/// How many admitted sessions may wait for [`PeerEndpoint::start`]'s consumer.
const ACCEPTED_QUEUE: usize = 64;

/// Makes the endpoint for one local address: the OS's, bound to the tailnet's interface
/// ([`SystemSockets`]), or a simulated network's in tests.
pub trait Sockets: Send + Sync {
    /// A server endpoint on `local`, drawing noq's choices from `seed` when given.
    ///
    /// # Errors
    ///
    /// What binding the socket returns.
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint>;
}

/// The OS's UDP sockets, each bound to the interface that holds its address.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSockets;

impl Sockets for SystemSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        let socket = transport::udp_socket(local)?;
        crate::iface::bind_to_interface(&socket, local.ip())?;
        socket.bind(&local.into())?;
        transport::endpoint_on_udp(socket.into(), true, seed)
    }
}

/// How this node takes part.
#[derive(Clone, Debug)]
pub struct PeerConfig {
    /// The app identity (`{data_dir}/device-id`).
    pub device_id: String,
    /// What this node is to its peers.
    pub role: Role,
    /// The UDP port engines listen on, on every tailnet address ([`PORT`]): bound here unless
    /// [`Self::ephemeral`], and dialled on every engine found.
    pub port: u16,
    /// Bind an OS-chosen port rather than [`Self::port`]: a viewer, which serves nothing, on a
    /// machine whose own engine holds that port.
    pub ephemeral: bool,
    /// Draws every random choice the endpoints make, so a simulated run repeats.
    pub seed: Option<u64>,
    /// What this node's hello says it can do beyond the wire's baseline (desktopd's
    /// `onicode_desktop_link::CAP_DESKTOP`).
    pub caps: Vec<String>,
}

impl PeerConfig {
    /// `device_id` in `role`, on [`PORT`].
    #[must_use]
    pub fn new(device_id: impl Into<String>, role: Role) -> Self {
        Self {
            device_id: device_id.into(),
            role,
            port: PORT,
            ephemeral: false,
            seed: None,
            caps: Vec::new(),
        }
    }

    /// The same, bound on an OS-chosen port ([`Self::ephemeral`]).
    #[must_use]
    pub const fn on_ephemeral_port(mut self) -> Self {
        self.ephemeral = true;
        self
    }

    /// The same, drawing from `seed`.
    #[must_use]
    pub const fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
}

/// A session whose peer the tailnet vouched for.
#[derive(Debug)]
pub struct PeerSession {
    /// The link and its control stream.
    pub session: Session,
    /// Who the tailnet says the peer is.
    pub admitted: Admitted,
}

/// The endpoints on this node's tailnet addresses.
pub struct PeerEndpoint {
    endpoints: Vec<(SocketAddr, noq::Endpoint)>,
    /// The port engines listen on ([`PeerConfig::port`]).
    port: u16,
    admission: Admission,
    hello: Hello,
    /// The node each device-id was first seen on, while that node is on the tailnet.
    nodes: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for PeerEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerEndpoint")
            .field("local", &self.local_addrs())
            .field("device_id", &self.hello.device_id)
            .finish_non_exhaustive()
    }
}

impl PeerEndpoint {
    /// Binds an endpoint on each of this node's tailnet addresses at `config.port`. The IPv4
    /// address must bind; the IPv6 one is skipped, with a warning, when it does not.
    ///
    /// # Errors
    ///
    /// [`PeerError::NotRunning`] when Tailscale is not up; the daemon's error; the IPv4 bind's.
    pub async fn bind(
        tailnet: Arc<dyn Tailnet>,
        sockets: &dyn Sockets,
        config: PeerConfig,
    ) -> Result<Arc<Self>, PeerError> {
        let status = tailnet.status().await?;
        let me = status
            .me
            .as_ref()
            .filter(|_| status.running())
            .ok_or(PeerError::NotRunning)?;
        let mut endpoints = Vec::new();
        let bind_port = if config.ephemeral { 0 } else { config.port };
        for (i, ip) in me.ips.iter().enumerate() {
            let local = SocketAddr::new(*ip, bind_port);
            let seed = config
                .seed
                .map(|s| s ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
            match sockets.endpoint(local, seed) {
                Ok(endpoint) => endpoints.push((local, endpoint)),
                Err(source) if ip.is_ipv6() => {
                    tracing::warn!(%local, error = %source, "no IPv6 endpoint")
                }
                Err(source) => {
                    return Err(PeerError::Bind {
                        addr: local,
                        source,
                    });
                }
            }
        }
        if endpoints.is_empty() {
            return Err(PeerError::NotRunning);
        }
        Ok(Arc::new(Self {
            endpoints,
            port: config.port,
            admission: Admission::new(Some(tailnet)),
            hello: Hello {
                caps: config.caps,
                ..Hello::new(config.device_id, config.role)
            },
            nodes: Mutex::default(),
        }))
    }

    /// This node's hello.
    #[must_use]
    pub const fn hello(&self) -> &Hello {
        &self.hello
    }

    /// This node's device-id.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.hello.device_id
    }

    /// The port engines listen on: where every engine found is dialled.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The addresses the endpoints are bound on.
    #[must_use]
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.endpoints.iter().map(|(a, _)| *a).collect()
    }

    /// The admission every caller passes.
    #[must_use]
    pub const fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Accepts on every endpoint: each admitted, non-probe session arrives on the channel.
    /// Call it once; the loops end when the endpoints close ([`Self::close`]).
    pub fn start(self: &Arc<Self>) -> mpsc::Receiver<PeerSession> {
        let (tx, rx) = mpsc::channel(ACCEPTED_QUEUE);
        for (_, endpoint) in &self.endpoints {
            let (this, endpoint, tx) = (Arc::clone(self), endpoint.clone(), tx.clone());
            tokio::spawn(async move {
                while let Some(incoming) = endpoint.accept().await {
                    let (this, tx) = (Arc::clone(&this), tx.clone());
                    tokio::spawn(async move {
                        if let Some(session) = this.admit(incoming).await {
                            let _ = tx.send(session).await;
                        }
                    });
                }
            });
        }
        rx
    }

    /// Closes every endpoint and the links on them.
    pub fn close(&self) {
        for (_, endpoint) in &self.endpoints {
            endpoint.close(VarInt::from_u32(close::NORMAL), b"shutting down");
        }
    }

    /// Checks a caller before its handshake and, when admitted, exchanges hellos.
    async fn admit(&self, incoming: noq::Incoming) -> Option<PeerSession> {
        let remote = transport::canonical(incoming.remote_address());
        match self.admission.check(remote).await {
            Verdict::Refuse(why) => {
                tracing::debug!(%remote, why, "refused before the handshake");
                incoming.refuse();
                None
            }
            Verdict::Ungranted => {
                if let Ok(Ok(conn)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming).await {
                    conn.close(
                        VarInt::from_u32(close::NOT_GRANTED),
                        b"the tailnet grants no role here",
                    );
                }
                None
            }
            Verdict::Admit(admitted) => {
                let conn = match tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming).await {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(e)) => {
                        tracing::debug!(%remote, error = %e, "handshake failed");
                        return None;
                    }
                    Err(_elapsed) => return None,
                };
                let session = match hello::accept(conn, remote, &self.hello).await {
                    Ok(session) => session,
                    Err(e) => {
                        tracing::info!(%remote, error = %e, "no hello");
                        return None;
                    }
                };
                if let Err(e) = self.vet(&session.peer, &admitted, Some(&session)) {
                    tracing::info!(%remote, error = %e, "caller refused at the hello");
                    return None;
                }
                if session.peer.is_probe() {
                    // The prober closes the link once it has read this node's hello. Dropped
                    // here first, the link would close at once and abandon that hello unsent.
                    if tokio::time::timeout(HELLO_TIMEOUT, session.link.closed())
                        .await
                        .is_err()
                    {
                        session.link.close(close::PROBE, b"probe never closed");
                    }
                    return None;
                }
                Some(PeerSession { session, admitted })
            }
        }
    }

    /// The tailnet's word on `addr`, before any packet goes to it.
    async fn vouched(&self, addr: SocketAddr) -> Result<Admitted, PeerError> {
        match self.admission.check(addr).await {
            Verdict::Admit(admitted) => Ok(admitted),
            Verdict::Ungranted => Err(PeerError::NotGranted(addr.to_string())),
            Verdict::Refuse(why) => Err(PeerError::Refused(why)),
        }
    }

    /// Dials the peer at `addr` once the tailnet vouches for it, and exchanges hellos.
    ///
    /// # Errors
    ///
    /// [`PeerError::Refused`] or [`PeerError::NotGranted`] before any packet; the dial's error.
    pub async fn dial(&self, addr: SocketAddr) -> Result<PeerSession, PeerError> {
        let addr = transport::canonical(addr);
        let admitted = self.vouched(addr).await?;
        let session = hello::dial(self.endpoint_for(addr)?, addr, &self.hello).await?;
        self.vet(&session.peer, &admitted, Some(&session))?;
        Ok(PeerSession { session, admitted })
    }

    /// Reads the hello of the engine at `addr` within `budget`, once the tailnet vouches for
    /// it as an engine, and closes as a probe.
    ///
    /// # Errors
    ///
    /// As [`Self::dial`]; [`PeerError::NotGranted`] when its grant holds no engine role.
    pub async fn probe(
        &self,
        addr: SocketAddr,
        budget: Duration,
    ) -> Result<(Hello, Admitted), PeerError> {
        let addr = transport::canonical(addr);
        let admitted = self.vouched(addr).await?;
        if !admitted.grant.allows(Role::Engine) {
            return Err(PeerError::NotGranted(addr.to_string()));
        }
        let peer = hello::probe(self.endpoint_for(addr)?, addr, &self.hello, budget).await?;
        self.vet(&peer, &admitted, None)?;
        Ok((peer, admitted))
    }

    /// The endpoint whose address family reaches `addr`.
    fn endpoint_for(&self, addr: SocketAddr) -> Result<&noq::Endpoint, PeerError> {
        self.endpoints
            .iter()
            .find(|(local, _)| local.is_ipv4() == addr.is_ipv4())
            .map(|(_, endpoint)| endpoint)
            .ok_or(PeerError::NoEndpoint(addr))
    }

    /// Whether `peer`, vouched for as `admitted`, may be what its hello says; a link that may
    /// not is closed with the reason.
    fn vet(
        &self,
        peer: &Hello,
        admitted: &Admitted,
        session: Option<&Session>,
    ) -> Result<(), PeerError> {
        let refuse = |code: u32, reason: &'static str, error: PeerError| {
            if let Some(session) = session {
                session.link.close(code, reason.as_bytes());
            }
            Err(error)
        };
        if !admitted.grant.allows(peer.role) {
            return refuse(
                close::NOT_GRANTED,
                "role not granted",
                PeerError::NotGranted(peer.device_id.clone()),
            );
        }
        if peer.device_id.is_empty() || peer.device_id == self.hello.device_id {
            return refuse(
                close::DEVICE_MISMATCH,
                "device-id",
                PeerError::Net(NetError::DeviceMismatch),
            );
        }
        if admitted.node.is_empty() {
            return Ok(());
        }
        let mut nodes = self.nodes.lock();
        match nodes.get(&peer.device_id) {
            Some(node) if *node != admitted.node => {
                drop(nodes);
                refuse(
                    close::DEVICE_MISMATCH,
                    "device-id held by another node",
                    PeerError::Net(NetError::DeviceMismatch),
                )
            }
            Some(_) => Ok(()),
            None => {
                nodes.insert(peer.device_id.clone(), admitted.node.clone());
                Ok(())
            }
        }
    }

    /// The node `device` was seen on.
    #[must_use]
    pub fn node_of(&self, device: &str) -> Option<String> {
        self.nodes.lock().get(device).cloned()
    }

    /// Forgets the device-ids seen on nodes no longer on the tailnet, so a device that moved to
    /// a new node (Tailscale reinstalled) is let in from it.
    pub fn retain_nodes(&self, on_tailnet: &HashSet<String>) {
        self.nodes
            .lock()
            .retain(|_, node| on_tailnet.contains(node));
    }
}
