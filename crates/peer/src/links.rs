//! One link per device, kept up.
//!
//! * Dials are deduplicated: callers that want the same device while it is being dialled wait for
//!   the one dial. When both ends dial each other at once, both keep the link the smaller device-id
//!   dialled and close the other with [`close::DUPLICATE`]; each end decides alone, by the same
//!   rule, so they agree without a round trip.
//! * A link that drops is redialled by the end that dialled it, after slopty's backoff
//!   ([`onicode_net::redial`]), spread by a seeded draw so a simulated run repeats; a peer on
//!   another build is tried again only after [`redial::WRONG_BUILD`].
//! * Each link is kept alive on its control stream: a ping every [`PING_INTERACTIVE`] while the
//!   person is using the device, every [`PING_IDLE`] otherwise, each answered at once; a link that
//!   hears nothing for [`SILENT_AFTER`] intervals is closed with [`close::SILENT`]. Nothing else
//!   drops a link: not the control plane's `Online` flag, not a status read.
//! * Each ping carries the path this end's daemon takes to the other, so both ends can show how the
//!   link travels (direct, peer relay, DERP).
//! * The streams the peer opens are served: RPC streams by the RPC service
//!   ([`onicode_rpc::serve_carried`]), every other kind by the [`StreamHandler`], as are the
//!   datagrams.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use onicode_net::admission::Admitted;
use onicode_net::hello::{HANDSHAKE_TIMEOUT, HELLO_TIMEOUT};
use onicode_net::redial::{self, Redial};
use onicode_net::{Hello, Link, NetError, StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_rpc::{CarriedSubscription, CarrierClient, RpcService};
use onicode_tailnet::Path;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::{PeerCarrier, PeerDirectory, PeerEndpoint, PeerError, PeerSession};

/// How often a link pings while the person is using the device.
pub const PING_INTERACTIVE: Duration = Duration::from_secs(1);
/// How often an idle link pings.
pub const PING_IDLE: Duration = Duration::from_secs(10);
/// Ping intervals of silence after which a link is taken for dead.
pub const SILENT_AFTER: u32 = 3;
/// The longest control frame read.
const MAX_CONTROL: usize = 16 << 10;

/// Serves the streams (other than RPC and control) and datagrams a peer sends.
pub trait StreamHandler: Send + Sync + 'static {
    /// A stream `device` opened, past its head. Called on the link's task: spawn to do work.
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx);

    /// A datagram `device` sent on `channel`.
    fn datagram(&self, device: &str, channel: u8, data: Bytes) {
        let _ = (device, channel, data);
    }
}

/// What the links serve, and how they draw.
#[derive(Clone, Default)]
pub struct LinksConfig {
    /// Answers the RPC calls peers make; without it their RPC streams are reset.
    pub service: Option<Arc<dyn RpcService>>,
    /// Takes every other stream and the datagrams; without it they are dropped.
    pub handler: Option<Arc<dyn StreamHandler>>,
    /// Draws the redial spread, so a simulated run repeats; the clock's draw without it.
    pub seed: Option<u64>,
}

/// How a link travels, as one end's daemon reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", content = "at", rename_all = "kebab-case")]
pub enum PathReport {
    /// Straight over UDP, to this address.
    Direct(String),
    /// Through another node acting as a relay.
    PeerRelay(String),
    /// Through a DERP server in this region.
    Derp(String),
    /// Nothing flowed lately, so the daemon has chosen no path.
    Idle,
}

impl From<Path> for PathReport {
    fn from(path: Path) -> Self {
        match path {
            Path::Direct(addr) => Self::Direct(addr.to_string()),
            Path::PeerRelay(relay) => Self::PeerRelay(relay),
            Path::Derp(region) => Self::Derp(region),
            Path::Idle => Self::Idle,
        }
    }
}

/// The control stream's frames past the hello.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Control {
    Ping {
        n: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathReport>,
    },
    Pong {
        n: u64,
    },
}

/// A live link, as [`PeerLinks::links`] reports it.
#[derive(Clone, Debug)]
pub struct LinkInfo {
    /// The peer's device-id.
    pub device: String,
    /// The peer's hello.
    pub peer: Hello,
    /// Its tailnet node's stable id.
    pub node: String,
    /// Its `MagicDNS` name.
    pub name: String,
    /// Its address.
    pub remote: SocketAddr,
    /// Whether this end dialled.
    pub dialed: bool,
    /// The link's id at this end's endpoint.
    pub id: usize,
    /// When it came up.
    pub since: Instant,
    /// The smoothed round trip.
    pub rtt: Option<Duration>,
    /// How this end's daemon reaches the peer.
    pub path: Option<PathReport>,
    /// How the peer's daemon reaches this end, as it last said.
    pub peer_path: Option<PathReport>,
    /// Whether it pings at the interactive rate.
    pub interactive: bool,
}

/// Where a device's link stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// No link and none wanted.
    Idle,
    /// Being dialled.
    Dialing,
    /// Up.
    Live,
    /// The last dial or link failed; the next dial is due at `until`.
    Backoff {
        /// When the next dial is due.
        until: Instant,
        /// The failure.
        reason: String,
    },
}

/// Why a device is backing off.
#[derive(Clone, Debug)]
enum Failure {
    WrongBuild(onicode_net::WrongBuild),
    Other(String),
}

impl Failure {
    fn of(e: &PeerError) -> Self {
        match e {
            PeerError::Net(NetError::WrongBuild(wrong)) => Self::WrongBuild(wrong.clone()),
            other => Self::Other(other.to_string()),
        }
    }

    fn of_net(e: &NetError) -> Self {
        match e {
            NetError::WrongBuild(wrong) => Self::WrongBuild(wrong.clone()),
            other => Self::Other(other.to_string()),
        }
    }

    fn error(&self, device: &str, until: Instant) -> PeerError {
        match self {
            Self::WrongBuild(wrong) => PeerError::Net(NetError::WrongBuild(wrong.clone())),
            Self::Other(reason) => PeerError::Unreachable {
                device: device.to_owned(),
                reason: reason.clone(),
                retry_in: until.saturating_duration_since(Instant::now()),
            },
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::WrongBuild(wrong) => wrong.to_string(),
            Self::Other(reason) => reason.clone(),
        }
    }
}

struct Live {
    link: Link,
    peer: Hello,
    admitted: Admitted,
    remote: SocketAddr,
    dialed: bool,
    since: Instant,
    interactive: watch::Sender<bool>,
    peer_path: Arc<Mutex<Option<PathReport>>>,
    /// The control, stream and datagram tasks, ended with the link.
    tasks: Vec<AbortHandle>,
}

impl Drop for Live {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

enum Slot {
    Idle,
    Dialing,
    Live(Box<Live>),
    Backoff { until: Instant, failure: Failure },
}

struct Entry {
    slot: Slot,
    /// Changes whenever the slot is taken over, so a task acting for an older one stops.
    epoch: u64,
    redial: Redial,
}

struct Inner {
    endpoint: Arc<PeerEndpoint>,
    directory: Arc<PeerDirectory>,
    config: LinksConfig,
    entries: Mutex<HashMap<String, Entry>>,
    epochs: AtomicU64,
    draws: Mutex<u64>,
    changes: watch::Sender<u64>,
    /// Moves when every backoff is cut short ([`PeerLinks::reset`]).
    kick: watch::Sender<u64>,
    shut: AtomicBool,
}

/// One link per device. Cheap to clone; every clone is the same set.
#[derive(Clone)]
pub struct PeerLinks(Arc<Inner>);

impl std::fmt::Debug for PeerLinks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerLinks")
            .field("links", &self.0.entries.lock().len())
            .finish_non_exhaustive()
    }
}

impl PeerLinks {
    /// The links of `endpoint`, dialling devices `directory` has found.
    #[must_use]
    pub fn new(
        endpoint: Arc<PeerEndpoint>,
        directory: Arc<PeerDirectory>,
        config: LinksConfig,
    ) -> Self {
        let draws = config
            .seed
            .map_or_else(|| u64::from(redial::unit()) | 1, |s| s | 1);
        Self(Arc::new(Inner {
            endpoint,
            directory,
            config,
            entries: Mutex::default(),
            epochs: AtomicU64::new(1),
            draws: Mutex::new(draws),
            changes: watch::channel(0).0,
            kick: watch::channel(0).0,
            shut: AtomicBool::new(false),
        }))
    }

    /// Takes in every session the endpoint accepts. Call it once.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        let mut accepted = self.0.endpoint.start();
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(session) = accepted.recv().await {
                this.adopt(session);
            }
        })
    }

    /// The link to `device`, dialled if there is none: callers at once share one dial.
    ///
    /// # Errors
    ///
    /// [`PeerError::Unknown`] when the directory has not found it; the dial's failure, or the
    /// last one while the device backs off ([`PeerError::Unreachable`], or the
    /// [`NetError::WrongBuild`] that holds it back).
    pub async fn ready(&self, device: &str) -> Result<Link, PeerError> {
        let mut changes = self.0.changes.subscribe();
        loop {
            if self.0.shut.load(Ordering::Relaxed) {
                return Err(PeerError::Shutdown);
            }
            let dial = {
                let mut entries = self.0.entries.lock();
                match entries.get(device).map(|e| &e.slot) {
                    Some(Slot::Live(live)) => return Ok(live.link.clone()),
                    Some(Slot::Backoff { until, failure }) => {
                        return Err(failure.error(device, *until));
                    }
                    Some(Slot::Dialing) => None,
                    Some(Slot::Idle) | None => {
                        if self.0.directory.get(device).is_none() {
                            return Err(PeerError::Unknown(device.to_owned()));
                        }
                        let epoch = self.0.next_epoch();
                        let entry = entries
                            .entry(device.to_owned())
                            .or_insert_with(|| self.0.entry(Slot::Idle));
                        entry.epoch = epoch;
                        entry.slot = Slot::Dialing;
                        Some(epoch)
                    }
                }
            };
            if let Some(epoch) = dial {
                self.0.changed();
                // On a task of its own, so a caller that gives up leaves no dial half done.
                tokio::spawn(self.clone().dialing(device.to_owned(), epoch, None));
            }
            changes
                .changed()
                .await
                .map_err(|_closed| PeerError::Shutdown)?;
        }
    }

    /// An RPC client to `device` that opens a stream of its own for each call.
    ///
    /// # Errors
    ///
    /// As [`Self::ready`].
    pub async fn client(&self, device: &str) -> Result<CarrierClient, PeerError> {
        let link = self.ready(device).await?;
        Ok(CarrierClient::new(Arc::new(PeerCarrier::new(link))))
    }

    /// Calls `method` on `device` over a stream of its own.
    ///
    /// # Errors
    ///
    /// As [`Self::ready`], then the call's.
    pub async fn call(
        &self,
        device: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PeerError> {
        let link = self.ready(device).await?;
        Ok(CarrierClient::new(Arc::new(PeerCarrier::new(link)))
            .call(method, params)
            .await?)
    }

    /// Subscribes to `method` on `device` over a stream that lives as long as the subscription.
    ///
    /// # Errors
    ///
    /// As [`Self::ready`], then the call's.
    pub async fn subscribe(
        &self,
        device: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<CarriedSubscription, PeerError> {
        let link = self.ready(device).await?;
        Ok(CarrierClient::new(Arc::new(PeerCarrier::new(link)))
            .subscribe(method, params)
            .await?)
    }

    /// Opens a stream to `device` with `head`.
    ///
    /// # Errors
    ///
    /// As [`Self::ready`], then the link's.
    pub async fn open(
        &self,
        device: &str,
        head: &StreamHead,
    ) -> Result<(StreamTx, StreamRx), PeerError> {
        Ok(self.ready(device).await?.open(head).await?)
    }

    /// Sends a datagram to `device` on `channel`, over its live link only (no dial).
    ///
    /// # Errors
    ///
    /// [`PeerError::Unknown`] with no live link; the link's.
    pub fn datagram(&self, device: &str, channel: u8, data: &[u8]) -> Result<(), PeerError> {
        let link = self
            .live(device)
            .ok_or_else(|| PeerError::Unknown(device.to_owned()))?;
        Ok(link.datagram(channel, data)?)
    }

    /// The live link to `device`, without dialling.
    #[must_use]
    pub fn live(&self, device: &str) -> Option<Link> {
        match &self.0.entries.lock().get(device)?.slot {
            Slot::Live(live) => Some(live.link.clone()),
            _ => None,
        }
    }

    /// Pings `device`'s link at the interactive rate (`true`) or the idle one.
    pub fn set_interactive(&self, device: &str, interactive: bool) {
        if let Some(Entry {
            slot: Slot::Live(live),
            ..
        }) = self.0.entries.lock().get(device)
        {
            live.interactive.send_replace(interactive);
        }
    }

    /// Closes `device`'s link and forgets its backoff: the next [`Self::ready`] dials at once.
    pub fn invalidate(&self, device: &str) {
        let removed = self.0.entries.lock().remove(device);
        if let Some(Entry {
            slot: Slot::Live(live),
            ..
        }) = &removed
        {
            live.link.close(close::NORMAL, b"invalidated");
        }
        if removed.is_some() {
            self.0.changed();
        }
    }

    /// The path may have changed (a wake, a new network): every backoff but a wrong build's
    /// ends now and its device is dialled again.
    pub fn reset(&self) {
        for entry in self.0.entries.lock().values_mut() {
            entry.redial.reset();
        }
        self.0.kick.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Cuts every backoff short ([`Self::reset`]) each time the OS reports a usable network
    /// path, from now on ([`crate::path`]). The watch holds the links weakly: once they are
    /// gone it does nothing.
    pub fn follow_path(&self) {
        let links = Arc::downgrade(&self.0);
        crate::path::watch(Box::new(move || {
            if let Some(inner) = links.upgrade() {
                Self(inner).reset();
            }
        }));
    }

    /// Closes every link and dials nothing more.
    pub fn shutdown(&self) {
        self.0.shut.store(true, Ordering::Relaxed);
        let entries = std::mem::take(&mut *self.0.entries.lock());
        for entry in entries.into_values() {
            if let Slot::Live(live) = entry.slot {
                live.link.close(close::NORMAL, b"shutting down");
            }
        }
        self.0.changed();
    }

    /// Where `device`'s link stands.
    #[must_use]
    pub fn state(&self, device: &str) -> LinkState {
        match self.0.entries.lock().get(device).map(|e| &e.slot) {
            None | Some(Slot::Idle) => LinkState::Idle,
            Some(Slot::Dialing) => LinkState::Dialing,
            Some(Slot::Live(_)) => LinkState::Live,
            Some(Slot::Backoff { until, failure }) => LinkState::Backoff {
                until: *until,
                reason: failure.reason(),
            },
        }
    }

    /// Every live link.
    #[must_use]
    pub fn links(&self) -> Vec<LinkInfo> {
        let entries = self.0.entries.lock();
        let mut links: Vec<LinkInfo> = entries
            .iter()
            .filter_map(|(device, entry)| match &entry.slot {
                Slot::Live(live) => Some(LinkInfo {
                    device: device.clone(),
                    peer: live.peer.clone(),
                    node: live.admitted.node.clone(),
                    name: live.admitted.name.trim_end_matches('.').to_owned(),
                    remote: live.remote,
                    dialed: live.dialed,
                    id: live.link.id(),
                    since: live.since,
                    rtt: live.link.rtt(),
                    path: self
                        .0
                        .directory
                        .path(&live.admitted.node)
                        .map(PathReport::from),
                    peer_path: live.peer_path.lock().clone(),
                    interactive: *live.interactive.borrow(),
                }),
                _ => None,
            })
            .collect();
        links.sort_by(|a, b| a.device.cmp(&b.device));
        links
    }

    /// The directory these links dial from: the engines the tailnet has.
    #[must_use]
    pub fn directory(&self) -> &Arc<PeerDirectory> {
        &self.0.directory
    }

    /// Changes as a counter that moves whenever a link comes, goes or starts dialling.
    #[must_use]
    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.0.changes.subscribe()
    }

    /// Takes in `session`, accepted or dialled. A second link to the same device keeps the one
    /// the smaller device-id dialled, closing the other with [`close::DUPLICATE`].
    pub fn adopt(&self, session: PeerSession) {
        let PeerSession { session, admitted } = session;
        let device = session.peer.device_id.clone();
        if self.0.shut.load(Ordering::Relaxed) {
            session.link.close(close::NORMAL, b"shutting down");
            return;
        }
        let mut entries = self.0.entries.lock();
        let entry = entries
            .entry(device.clone())
            .or_insert_with(|| self.0.entry(Slot::Idle));
        if let Slot::Live(old) = &entry.slot
            && !old.link.is_closed()
        {
            if !self.0.keeps_new(old.dialed, session.dialed, &device) {
                tracing::debug!(%device, "duplicate link closed");
                session.link.close(close::DUPLICATE, b"duplicate link");
                return;
            }
            old.link.close(close::DUPLICATE, b"duplicate link");
        }
        let epoch = self.0.next_epoch();
        entry.epoch = epoch;
        entry.redial.linked(Instant::now());
        let link = session.link.clone();
        let (interactive, interactive_rx) = watch::channel(false);
        let peer_path = Arc::new(Mutex::new(None));
        let silent = Arc::new(AtomicBool::new(false));
        let tasks = vec![
            tokio::spawn(control(
                self.clone(),
                Keepalive {
                    link: link.clone(),
                    node: admitted.node.clone(),
                    tx: session.control_tx,
                    rx: session.control_rx,
                    interactive: interactive_rx,
                    peer_path: Arc::clone(&peer_path),
                    silent: Arc::clone(&silent),
                },
            ))
            .abort_handle(),
            tokio::spawn(serve_streams(self.clone(), link.clone(), device.clone())).abort_handle(),
            tokio::spawn(serve_datagrams(self.clone(), link.clone(), device.clone()))
                .abort_handle(),
        ];
        entry.slot = Slot::Live(Box::new(Live {
            link: link.clone(),
            peer: session.peer,
            admitted,
            remote: session.remote,
            dialed: session.dialed,
            since: Instant::now(),
            interactive,
            peer_path,
            tasks,
        }));
        drop(entries);
        self.0.changed();
        tokio::spawn(self.clone().watch_closed(device, epoch, link, silent));
    }

    /// Waits for the link adopted at `epoch` to end, then gives up its slot and, when this end
    /// dialled it and it ended for a reason, redials.
    async fn watch_closed(self, device: String, epoch: u64, link: Link, silent: Arc<AtomicBool>) {
        // This end closing a silent link reads as a close of its own: it is a failure all the same.
        let error = link
            .closed()
            .await
            .or_else(|| silent.load(Ordering::Relaxed).then_some(NetError::Silent));
        let redial_after = {
            let mut entries = self.0.entries.lock();
            let Some(entry) = entries.get_mut(&device) else {
                return;
            };
            if entry.epoch != epoch {
                // Superseded (a duplicate replaced it, or it was invalidated): not ours to act on.
                return;
            }
            let Slot::Live(live) = &entry.slot else {
                return;
            };
            let dialed = live.dialed;
            let wanted = dialed
                && !self.0.shut.load(Ordering::Relaxed)
                && error
                    .as_ref()
                    .is_some_and(|e| !matches!(e, NetError::Duplicate | NetError::NotGranted));
            if wanted {
                let failure = error
                    .as_ref()
                    .map_or_else(|| Failure::Other("closed".to_owned()), Failure::of_net);
                let wait = self.0.wait(entry, &failure);
                entry.epoch = self.0.next_epoch();
                entry.slot = Slot::Backoff {
                    until: Instant::now() + wait,
                    failure,
                };
                Some((entry.epoch, wait))
            } else {
                entries.remove(&device);
                None
            }
        };
        self.0.changed();
        if let Some(e) = &error {
            tracing::info!(%device, error = %e, "link ended");
        }
        if let Some((epoch, wait)) = redial_after {
            self.dialing(device, epoch, Some(wait)).await;
        }
    }

    /// Dials `device` for the slot at `epoch`, after `first` when given, until it links, the
    /// slot is taken over, or the device is gone: the directory no longer has it and the node
    /// it was seen on has left the tailnet. A device dropped from the directory while its node
    /// is still there (a probe that met it falling asleep, an engine restarting) is backed off
    /// and looked up again, so the link comes back once a refresh finds it.
    async fn dialing(self, device: String, epoch: u64, first: Option<Duration>) {
        let mut kick = self.0.kick.subscribe();
        if let Some(wait) = first
            && !self.backoff(&device, epoch, wait, &mut kick).await
        {
            return;
        }
        loop {
            let result = match self.0.directory.get(&device) {
                Some(peer) => self.0.endpoint.dial(peer.addr).await,
                None => Err(PeerError::Unknown(device.clone())),
            };
            let result = result.and_then(|session| {
                if session.session.peer.device_id == device {
                    Ok(session)
                } else {
                    // The address now answers as another device: take that link, and look again.
                    self.0.directory.forget(&device);
                    let moved = session.session.peer.device_id.clone();
                    self.adopt(session);
                    Err(PeerError::Unknown(format!(
                        "{device} (its address answers as {moved})"
                    )))
                }
            });
            match result {
                Ok(session) => {
                    self.adopt(session);
                    return;
                }
                Err(PeerError::Net(NetError::Duplicate)) => {
                    // The peer keeps a link it dialled itself, which is on its way here.
                    tokio::time::sleep(HANDSHAKE_TIMEOUT).await;
                    if !self.is_current(&device, epoch, |slot| matches!(slot, Slot::Dialing)) {
                        return;
                    }
                }
                Err(e) => {
                    let gone = matches!(e, PeerError::Unknown(_))
                        && self.0.endpoint.node_of(&device).is_none();
                    let wait = {
                        let mut entries = self.0.entries.lock();
                        let Some(entry) = entries.get_mut(&device) else {
                            return;
                        };
                        if entry.epoch != epoch {
                            return;
                        }
                        if gone {
                            entries.remove(&device);
                            None
                        } else {
                            let failure = Failure::of(&e);
                            let wait = self.0.wait(entry, &failure);
                            entry.slot = Slot::Backoff {
                                until: Instant::now() + wait,
                                failure,
                            };
                            Some(wait)
                        }
                    };
                    self.0.changed();
                    tracing::debug!(%device, error = %e, "dial failed");
                    let Some(wait) = wait else { return };
                    if !self.backoff(&device, epoch, wait, &mut kick).await {
                        return;
                    }
                }
            }
        }
    }

    /// Sleeps out a backoff (cut short by [`Self::reset`] unless it is a wrong build's) and
    /// takes the slot back to dialling; `false` when the slot was taken over meanwhile.
    async fn backoff(
        &self,
        device: &str,
        epoch: u64,
        wait: Duration,
        kick: &mut watch::Receiver<u64>,
    ) -> bool {
        let wrong_build = self.is_current(device, epoch, |slot| {
            matches!(
                slot,
                Slot::Backoff {
                    failure: Failure::WrongBuild(_),
                    ..
                }
            )
        });
        kick.borrow_and_update();
        tokio::select! {
            biased;
            () = tokio::time::sleep(wait) => {}
            _ = kick.changed(), if !wrong_build => {}
        }
        let mut entries = self.0.entries.lock();
        let Some(entry) = entries.get_mut(device) else {
            return false;
        };
        if entry.epoch != epoch
            || !matches!(entry.slot, Slot::Backoff { .. })
            || self.0.shut.load(Ordering::Relaxed)
        {
            return false;
        }
        entry.slot = Slot::Dialing;
        drop(entries);
        self.0.changed();
        true
    }

    fn is_current(&self, device: &str, epoch: u64, slot: impl Fn(&Slot) -> bool) -> bool {
        self.0
            .entries
            .lock()
            .get(device)
            .is_some_and(|e| e.epoch == epoch && slot(&e.slot))
    }
}

impl Inner {
    fn entry(&self, slot: Slot) -> Entry {
        Entry {
            slot,
            epoch: self.next_epoch(),
            redial: Redial::default(),
        }
    }

    fn next_epoch(&self) -> u64 {
        self.epochs.fetch_add(1, Ordering::Relaxed)
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// A draw in `0..=1000` for the redial spread (xorshift64 from the seed).
    fn draw(&self) -> u32 {
        let mut x = self.draws.lock();
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        u32::try_from(*x % 1001).unwrap_or_default()
    }

    /// How long `entry` backs off after `failure`.
    fn wait(&self, entry: &mut Entry, failure: &Failure) -> Duration {
        let next = entry.redial.next_spread(Instant::now(), self.draw());
        match failure {
            Failure::WrongBuild(_) => redial::WRONG_BUILD,
            Failure::Other(_) => next,
        }
    }

    /// Whether a new link replaces the live one to `device`: the one the smaller device-id
    /// dialled stays; between two the same end dialled, the newer (its dialler gave up the
    /// older).
    fn keeps_new(&self, old_dialed: bool, new_dialed: bool, device: &str) -> bool {
        let me = self.endpoint.device_id();
        let by_smaller = |dialed: bool| if dialed { me < device } else { device < me };
        match (by_smaller(old_dialed), by_smaller(new_dialed)) {
            (true, false) => false,
            (false, true) | (true, true) | (false, false) => true,
        }
    }
}

/// What a link's control task holds.
struct Keepalive {
    link: Link,
    /// The peer's tailnet node, whose path each ping reports.
    node: String,
    tx: StreamTx,
    rx: StreamRx,
    interactive: watch::Receiver<bool>,
    peer_path: Arc<Mutex<Option<PathReport>>>,
    /// Set when this end closes the link for its silence.
    silent: Arc<AtomicBool>,
}

/// Keeps the link alive on its control stream, answers the peer's pings, and records the path
/// it reports. Closes the link with [`close::SILENT`] after [`SILENT_AFTER`] silent intervals.
async fn control(links: PeerLinks, keepalive: Keepalive) {
    let Keepalive {
        link,
        node,
        tx,
        mut rx,
        mut interactive,
        peer_path,
        silent,
    } = keepalive;
    rx.set_max_frame(MAX_CONTROL);
    let every = |interactive: bool| {
        if interactive {
            PING_INTERACTIVE
        } else {
            PING_IDLE
        }
    };
    let mut heard = Instant::now();
    let mut next_ping = Instant::now() + every(*interactive.borrow());
    let mut n = 0_u64;
    loop {
        let interval = every(*interactive.borrow());
        let silent_at = heard + interval * SILENT_AFTER;
        tokio::select! {
            biased;
            frame = rx.frame() => {
                let frame = match frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => {
                        // The control stream ends only with the link.
                        link.close(close::PROTOCOL, b"control stream ended");
                        return;
                    }
                };
                heard = Instant::now();
                match serde_json::from_slice::<Control>(&frame) {
                    Ok(Control::Ping { n, path }) => {
                        if path.is_some() {
                            *peer_path.lock() = path;
                        }
                        send(&tx, &Control::Pong { n });
                    }
                    Ok(Control::Pong { .. }) => {}
                    Err(_) => tracing::debug!("unreadable control frame"),
                }
            }
            () = tokio::time::sleep_until(next_ping) => {
                n = n.wrapping_add(1);
                let path = links.0.directory.path(&node).map(PathReport::from);
                send(&tx, &Control::Ping { n, path });
                next_ping = Instant::now() + interval;
            }
            () = tokio::time::sleep_until(silent_at) => {
                tracing::info!(peer = ?link.peer(), "silent link closed");
                silent.store(true, Ordering::Relaxed);
                link.close(close::SILENT, b"silent");
                return;
            }
            changed = interactive.changed() => {
                if changed.is_err() {
                    return;
                }
                // Going interactive pings now, so a dead link is found at the new rate.
                if *interactive.borrow() {
                    next_ping = Instant::now();
                }
            }
        }
    }
}

fn send(tx: &StreamTx, frame: &Control) {
    if let Ok(json) = serde_json::to_vec(frame) {
        let _ = tx.send_frame(&json);
    }
}

/// Serves each stream the peer opens, by its kind.
async fn serve_streams(links: PeerLinks, link: Link, device: String) {
    while let Ok((tx, mut rx)) = link.accept().await {
        let (config, device) = (links.0.config.clone(), device.clone());
        tokio::spawn(async move {
            let head = match tokio::time::timeout(HELLO_TIMEOUT, rx.head()).await {
                Ok(Ok(head)) => head,
                Ok(Err(_)) | Err(_) => {
                    tx.reset(close::PROTOCOL);
                    return;
                }
            };
            match (head.kind, &config.service, &config.handler) {
                (StreamKind::Control, _, _) => tx.reset(close::PROTOCOL),
                (StreamKind::Rpc, Some(service), _) => {
                    let (out, inbound) = crate::carrier::duplex(tx, rx);
                    onicode_rpc::serve_carried(Arc::clone(service), Some(device), out, inbound)
                        .await;
                }
                (StreamKind::Rpc, None, _) => tx.reset(close::NOT_GRANTED),
                (_, _, Some(handler)) => handler.stream(&device, head, tx, rx),
                (_, _, None) => tx.reset(close::NOT_GRANTED),
            }
        });
    }
}

/// Hands each datagram the peer sends to the handler.
async fn serve_datagrams(links: PeerLinks, link: Link, device: String) {
    let Some(handler) = links.0.config.handler.clone() else {
        return;
    };
    while let Ok((channel, data)) = link.recv_datagram().await {
        handler.datagram(&device, channel, data);
    }
}
