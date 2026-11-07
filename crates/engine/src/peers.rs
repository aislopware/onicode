//! The engine on the tailnet (engine.md, migration step 9; `topology.md`, `transport.md`): its
//! QUIC endpoint on this machine's tailnet addresses, the other engines the tailnet has, and one
//! link to each device, over which peers call this engine and this engine forwards its viewer's
//! calls and watches for other engines ([`crate::router`]).
//!
//! Ported by intent from zeron's `Peers` (`git show f01736b3^:crates/engine/src/peers.rs`): the
//! endpoint admits a caller by Tailscale's whois before its handshake and fails closed; links are
//! one per device and deduplicated; every call and watch takes a QUIC stream of its own. No relay
//! and no crypto of onicode's own: WireGuard already carries and vouches for every packet.
//! zeron's `StreamRouter` is not ported: every group rides RPC streams, so no other stream kind
//! has a part of the engine to serve it yet.
//!
//! The engine's id ([`Identity`]) is its tailnet node's stable id once it has joined, the same
//! string its hello names and the endpoint binds to that node at admission; it is kept in
//! `{data_dir}/device-id`, so desktopd's hello and an engine that has not joined yet use it too.
//! `ONICODE_PEERS=0` keeps an engine off the tailnet (tests that spawn the real binary).
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_net::Role;
use onicode_peer::{
    LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerError, PeerLinks, Sockets,
    SystemSockets,
};
use onicode_protocol::{EngineId, PROTOCOL};
use onicode_rpc::{CarrierClient, RpcError, RpcService};
use onicode_tailnet::{Relooking, Tailnet};
use tokio::task::JoinHandle;

use crate::router::{Hop, Peering, resume_watch};

/// The variable that keeps an engine off the tailnet when it is `0`.
pub const PEERS_ENV: &str = "ONICODE_PEERS";
/// The file in the data dir that keeps the engine's id.
pub const DEVICE_ID: &str = "device-id";
/// The hello capability that names the protocol a peer speaks: `protocol:10`.
pub const PROTOCOL_CAP: &str = "protocol:";
/// How long the engine waits before it tries the tailnet again while Tailscale is not up.
const JOIN_AGAIN: Duration = Duration::from_secs(5);

/// Whether this process may join the tailnet: unless `ONICODE_PEERS=0`.
#[must_use]
pub fn enabled() -> bool {
    !matches!(std::env::var(PEERS_ENV).as_deref().map(str::trim), Ok("0"))
}

/// What an engine's (or a phone's) hello says beyond the wire's baseline: the protocol it speaks.
#[must_use]
pub fn caps() -> Vec<String> {
    vec![format!("{PROTOCOL_CAP}{PROTOCOL}")]
}

/// The protocol `hello` says its sender speaks.
#[must_use]
pub fn protocol_of(hello: &onicode_net::Hello) -> Option<u32> {
    hello
        .caps
        .iter()
        .find_map(|cap| cap.strip_prefix(PROTOCOL_CAP)?.parse().ok())
}

/// Who this engine is: its id and its machine's name. Off the tailnet the id is the one it last
/// had there (`device-id`), else its host name; joining replaces both with the tailnet's.
pub struct Identity {
    held: Mutex<(EngineId, String)>,
}

impl Identity {
    /// The identity kept in `data_dir`, or the host's own for an engine with none.
    #[must_use]
    pub fn of(data_dir: Option<&Path>) -> Arc<Self> {
        let name = machine_name();
        let kept = data_dir
            .and_then(|dir| std::fs::read_to_string(dir.join(DEVICE_ID)).ok())
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        let id = kept.unwrap_or_else(|| name.clone());
        Arc::new(Self {
            held: Mutex::new((EngineId(id), name)),
        })
    }

    /// The engine's id.
    #[must_use]
    pub fn id(&self) -> EngineId {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .clone()
    }

    /// The machine's name.
    #[must_use]
    pub fn name(&self) -> String {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }

    /// Takes the tailnet's id and name.
    pub(crate) fn set(&self, id: EngineId, name: String) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = (id, name);
    }
}

/// The machine's name, as its host name says, for an engine off the tailnet.
fn machine_name() -> String {
    let mut name = [0u8; 256];
    // SAFETY: `gethostname` writes at most `name.len()` bytes into the buffer it is given.
    let written = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) };
    if written != 0 {
        return "This Mac".into();
    }
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    let host = String::from_utf8_lossy(name.get(..end).unwrap_or_default()).into_owned();
    let host = host.strip_suffix(".local").unwrap_or(&host).to_owned();
    if host.is_empty() {
        "This Mac".into()
    } else {
        host
    }
}

/// The engine's place on the tailnet, while it runs. Dropping it closes every link and the
/// endpoint.
pub struct Peers {
    endpoint: Arc<PeerEndpoint>,
    links: PeerLinks,
    directory: Arc<PeerDirectory>,
    id: EngineId,
    name: String,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Peers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peers")
            .field("id", &self.id)
            .field("links", &self.links)
            .finish_non_exhaustive()
    }
}

impl Peers {
    /// Joins the tailnet through this machine's Tailscale, as soon as it is up: binds on its
    /// addresses as the engine of this node, keeps the id in `data_dir`, finds the other engines,
    /// serves `service` to every peer, and redials at once on a network handover. Waits for as
    /// long as Tailscale is down.
    pub async fn join(data_dir: &Path, service: Arc<dyn RpcService>) -> Self {
        let tailnet: Arc<dyn Tailnet> = Arc::new(Relooking::system());
        loop {
            match Self::start_on(tailnet.clone(), &SystemSockets, None, service.clone()).await {
                Ok(peers) => {
                    peers.keep_id(data_dir);
                    peers.links.follow_path();
                    return peers;
                }
                Err(error) => {
                    eprintln!("The engine is not on the tailnet yet: {error}");
                    tokio::time::sleep(JOIN_AGAIN).await;
                }
            }
        }
    }

    /// Joins `tailnet` with `sockets` (a simulated tailnet in tests), drawing from `seed`, serving
    /// `service` to every peer.
    ///
    /// # Errors
    ///
    /// [`PeerError::NotRunning`] when the tailnet is not up here; the daemon's error; a bind's.
    pub async fn start_on(
        tailnet: Arc<dyn Tailnet>,
        sockets: &dyn Sockets,
        seed: Option<u64>,
        service: Arc<dyn RpcService>,
    ) -> Result<Self, PeerError> {
        let status = tailnet.status().await?;
        let me = status
            .me
            .as_ref()
            .filter(|_| status.running())
            .ok_or(PeerError::NotRunning)?;
        let id = EngineId(me.id.clone());
        let name = if me.host_name.is_empty() {
            me.name().split('.').next().unwrap_or_default().to_owned()
        } else {
            me.host_name.clone()
        };
        let mut config = PeerConfig::new(id.0.clone(), Role::Engine);
        config.caps = caps();
        config.seed = seed;
        let endpoint = PeerEndpoint::bind(tailnet.clone(), sockets, config).await?;
        let directory = PeerDirectory::new(endpoint.clone(), tailnet);
        let links = PeerLinks::new(
            endpoint.clone(),
            directory.clone(),
            LinksConfig {
                service: Some(service),
                handler: None,
                seed,
            },
        );
        let tasks = vec![directory.start(), links.start()];
        Ok(Self {
            endpoint,
            links,
            directory,
            id,
            name,
            tasks,
        })
    }

    /// Keeps the id in `data_dir`, for desktopd's hello and the engine's next start.
    fn keep_id(&self, data_dir: &Path) {
        let file: PathBuf = data_dir.join(DEVICE_ID);
        let kept = std::fs::read_to_string(&file).ok();
        if kept.as_deref().map(str::trim) != Some(self.id.0.as_str())
            && let Err(error) = std::fs::write(&file, format!("{}\n", self.id))
        {
            eprintln!("Cannot keep the engine's id: {error}");
        }
    }

    /// The engine's id on the tailnet.
    #[must_use]
    pub fn id(&self) -> &EngineId {
        &self.id
    }

    /// The machine's tailnet name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The endpoint on this machine's tailnet addresses.
    #[must_use]
    pub fn endpoint(&self) -> &Arc<PeerEndpoint> {
        &self.endpoint
    }

    /// The links to every device.
    #[must_use]
    pub fn links(&self) -> &PeerLinks {
        &self.links
    }

    /// The engines the tailnet has.
    #[must_use]
    pub fn directory(&self) -> &Arc<PeerDirectory> {
        &self.directory
    }

    /// Asks `engine` what it serves now, over its live link only: no dial for a status line.
    pub(crate) async fn ask_live(
        &self,
        engine: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Option<serde_json::Value> {
        let link = self.links.live(engine)?;
        CarrierClient::new(onicode_peer::carrier::carrier(link))
            .call(method, params)
            .await
            .ok()
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

/// A failed hop: the link or its call is gone (try later), or the engine answered with an error.
fn hop(error: PeerError) -> Hop {
    match error {
        PeerError::Rpc(error) => rpc_hop(error),
        other => Hop::Unreachable(other.to_string()),
    }
}

fn rpc_hop(error: RpcError) -> Hop {
    match error {
        RpcError::Transport(why) => Hop::Unreachable(why),
        RpcError::Closed => Hop::Unreachable("the link closed".into()),
        other => Hop::Refused(other),
    }
}

#[async_trait]
impl Peering for Peers {
    async fn call(
        &self,
        engine: &EngineId,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, Hop> {
        let link = self.links.ready(&engine.0).await.map_err(hop)?;
        CarrierClient::new(onicode_peer::carrier::carrier(link))
            .call(method, params)
            .await
            .map_err(rpc_hop)
    }

    async fn watch(
        &self,
        engine: &EngineId,
        method: &str,
        params: serde_json::Value,
    ) -> Result<BoxStream<'static, serde_json::Value>, Hop> {
        let subscription = self
            .links
            .subscribe(&engine.0, method, params)
            .await
            .map_err(hop)?
            .resuming(resume_watch());
        Ok(
            futures::stream::unfold(subscription, |mut subscription| async move {
                let item = subscription.recv().await?;
                Some((item, subscription))
            })
            .boxed(),
        )
    }

    async fn reached(&self, engine: &EngineId) {
        if self.links.reach(&engine.0).await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    fn protocol(&self, device: &str) -> Option<u32> {
        self.links
            .hello(device)
            .or_else(|| self.directory.get(device).map(|peer| peer.hello))
            .as_ref()
            .and_then(protocol_of)
    }
}
