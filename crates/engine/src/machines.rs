//! The machines this engine knows (engine.md, Engine group): itself, every engine the tailnet
//! has, and every device linked to it now, each with its link's state and path (direct, peer
//! relay, DERP), round trip, build and protocol, and how many agents it runs. Served as
//! `WatchMachines` on the generic `Topic`, and `EngineLoad` answers what a peer's list shows of
//! this engine. The list follows the links and the directory as they change, and refreshes round
//! trips and loads every [`REFRESH`] while someone watches.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use onicode_peer::{LinkState, PathReport};
use onicode_protocol::engine::{
    EngineLoad, LinkPath, Load, Machine, MachineEvent, MachineKind, MachineLink, Machines,
    WatchMachines,
};
use onicode_protocol::{Call, Cursor, EngineId, Item, PROTOCOL, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};
use tokio::time::Instant;

use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::peers::{Identity, Peers, protocol_of};
use crate::service::{Group, VERSION, routed};
use crate::session::SessionHub;

/// How often a watched list refreshes round trips and loads.
pub const REFRESH: Duration = Duration::from_secs(5);
/// How long a peer has to say its load.
const LOAD_BUDGET: Duration = Duration::from_secs(2);

/// The machines list as a feed: a machine's newer state supersedes its older one.
pub(crate) struct MachinesFeed;

impl Feed for MachinesFeed {
    type Event = MachineEvent;
    type State = Machines;

    fn apply(state: &mut Machines, event: &MachineEvent) {
        state.apply(event);
    }

    fn weight(event: &MachineEvent) -> usize {
        serde_json::to_vec(event).map_or(256, |bytes| bytes.len())
    }

    fn absorb(pending: &mut Pending<Self>, event: MachineEvent) -> bool {
        pending.replace_or_append(event, |newer, older| newer.engine() == older.engine())
    }
}

/// The list, its numbering and its watchers.
pub struct MachinesHub {
    topic: Mutex<Topic<MachinesFeed>>,
    identity: Arc<Identity>,
    session: Arc<SessionHub>,
    /// What each engine last said its load was.
    loads: Mutex<HashMap<String, u32>>,
}

impl MachinesHub {
    /// The list of this engine alone, until it joins the tailnet.
    pub(crate) fn new(identity: Arc<Identity>, session: Arc<SessionHub>) -> Arc<Self> {
        let epoch = onicode_protocol::time::now_ms().wrapping_shl(16);
        let hub = Arc::new(Self {
            topic: Mutex::new(Topic::new(epoch, Machines::default(), Limits::default())),
            identity,
            session,
            loads: Mutex::default(),
        });
        hub.show(vec![hub.this()]);
        hub
    }

    fn topic(&self) -> MutexGuard<'_, Topic<MachinesFeed>> {
        self.topic.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// This engine's own row.
    fn this(&self) -> Machine {
        Machine {
            engine: self.identity.id(),
            name: self.identity.name(),
            kind: MachineKind::This,
            link: MachineLink::Here,
            path: None,
            rtt_us: None,
            version: VERSION.into(),
            protocol: Some(PROTOCOL),
            owner: true,
            live_agents: Some(self.load().live_agents),
        }
    }

    /// This engine's load.
    #[must_use]
    pub fn load(&self) -> Load {
        Load {
            live_agents: u32::try_from(self.session.live_agents()).unwrap_or(u32::MAX),
        }
    }

    /// The list as it is now.
    #[must_use]
    pub fn current(&self) -> Machines {
        self.topic().state().clone()
    }

    /// Publishes what moves the list to `machines`: each machine that changed or came, and each
    /// one that went.
    fn show(&self, mut machines: Vec<Machine>) {
        machines.sort_by(|a, b| a.engine.cmp(&b.engine));
        machines.dedup_by(|a, b| a.engine == b.engine);
        let mut topic = self.topic();
        let held = topic.state();
        let mut events: Vec<MachineEvent> = held
            .machines
            .iter()
            .filter(|old| {
                machines
                    .binary_search_by(|new| new.engine.cmp(&old.engine))
                    .is_err()
            })
            .map(|old| MachineEvent::Gone(old.engine.clone()))
            .collect();
        events.extend(
            machines
                .into_iter()
                .filter(|new| held.get(&new.engine) != Some(new))
                .map(MachineEvent::Changed),
        );
        topic.publish(events);
    }

    /// `WatchMachines`: the list (this engine's own row fresh), then every change.
    pub fn watch(
        self: &Arc<Self>,
        since: Option<Cursor>,
    ) -> BoxStream<'static, Item<Machines, MachineEvent>> {
        let this = self.this();
        let (id, notify) = {
            let mut topic = self.topic();
            if topic.state().get(&this.engine) != Some(&this) {
                let stale: Vec<_> = topic
                    .state()
                    .machines
                    .iter()
                    .filter(|machine| {
                        machine.kind == MachineKind::This && machine.engine != this.engine
                    })
                    .map(|machine| MachineEvent::Gone(machine.engine.clone()))
                    .collect();
                topic.publish(stale);
                topic.publish(vec![MachineEvent::Changed(this)]);
            }
            topic.join(since)
        };
        let (draining, leaving) = (self.clone(), self.clone());
        feed::stream(
            notify,
            move || draining.topic().drain(id),
            move || {
                leaving.topic().leave(id);
            },
        )
    }

    /// The list as `peers` see the tailnet now.
    fn list(&self, peers: &Peers) -> Vec<Machine> {
        let links = peers.links();
        let directory = peers.directory();
        let loads = self
            .loads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let live: HashMap<String, onicode_peer::LinkInfo> = links
            .links()
            .into_iter()
            .map(|info| (info.device.clone(), info))
            .collect();
        let rtt = |info: Option<&onicode_peer::LinkInfo>| {
            // Whole milliseconds: a round trip that wobbles moves nothing.
            info.and_then(|info| info.rtt).map(|rtt| {
                u64::try_from(rtt.as_millis())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(1000)
            })
        };
        let mut list = vec![self.this()];
        for peer in directory.peers() {
            let info = live.get(&peer.device_id);
            let link = match links.state(&peer.device_id) {
                LinkState::Idle => MachineLink::Found,
                LinkState::Dialing => MachineLink::Dialing,
                LinkState::Live => MachineLink::Live,
                LinkState::Backoff { until, reason } => MachineLink::Offline {
                    reason,
                    retry_in_ms: u64::try_from(
                        until.saturating_duration_since(Instant::now()).as_millis(),
                    )
                    .unwrap_or(u64::MAX),
                },
            };
            let hello = info.map_or(&peer.hello, |info| &info.peer);
            list.push(Machine {
                engine: EngineId(peer.device_id.clone()),
                name: peer.name.clone(),
                kind: MachineKind::Engine,
                link,
                path: directory.path(&peer.node).map(PathReport::from).map(path),
                rtt_us: rtt(info),
                version: hello.build.clone(),
                protocol: protocol_of(hello),
                owner: peer.owner,
                live_agents: loads.get(&peer.device_id).copied(),
            });
        }
        for (device, info) in &live {
            if directory.get(device).is_some() {
                continue;
            }
            let engine = info.peer.role == onicode_net::Role::Engine;
            list.push(Machine {
                engine: EngineId(device.clone()),
                name: info.name.clone(),
                kind: if engine {
                    MachineKind::Engine
                } else {
                    MachineKind::Viewer
                },
                link: MachineLink::Live,
                path: info.path.clone().map(path),
                rtt_us: rtt(Some(info)),
                version: info.peer.build.clone(),
                protocol: protocol_of(&info.peer),
                owner: false,
                live_agents: if engine {
                    loads.get(device).copied()
                } else {
                    None
                },
            });
        }
        list
    }

    /// Asks every linked engine its load, each within [`LOAD_BUDGET`].
    async fn ask_loads(&self, peers: &Peers) {
        let params = serde_json::to_value(onicode_protocol::Routed {
            engine: None,
            op: None,
            body: EngineLoad {},
        })
        .unwrap_or_default();
        let engines: Vec<String> = peers
            .links()
            .links()
            .into_iter()
            .filter(|info| info.peer.role == onicode_net::Role::Engine)
            .map(|info| info.device)
            .collect();
        let asks = engines.into_iter().map(|engine| {
            let params = params.clone();
            async move {
                let answer = tokio::time::timeout(
                    LOAD_BUDGET,
                    peers.ask_live(&engine, EngineLoad::NAME, params),
                )
                .await
                .ok()
                .flatten()
                .and_then(|value| serde_json::from_value::<Load>(value).ok());
                (engine, answer)
            }
        });
        let answers = futures::future::join_all(asks).await;
        let mut loads = self.loads.lock().unwrap_or_else(PoisonError::into_inner);
        for (engine, answer) in answers {
            match answer {
                Some(load) => {
                    loads.insert(engine, load.live_agents);
                }
                None => {
                    loads.remove(&engine);
                }
            }
        }
    }

    /// Follows `peers` into the list until they go: at each change of the links or the
    /// directory, and every [`REFRESH`] while someone watches (round trips, loads).
    pub(crate) async fn follow(self: Arc<Self>, peers: Weak<Peers>) {
        let Some(held) = peers.upgrade() else {
            return;
        };
        let mut links = held.links().subscribe_changes();
        let mut found = held.directory().subscribe();
        drop(held);
        loop {
            let Some(peers) = peers.upgrade() else {
                return;
            };
            if self.topic().is_watched() {
                self.ask_loads(&peers).await;
            }
            self.show(self.list(&peers));
            drop(peers);
            tokio::select! {
                changed = links.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                changed = found.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(REFRESH) => {}
            }
        }
    }
}

fn path(report: PathReport) -> LinkPath {
    match report {
        PathReport::Direct(addr) => LinkPath::Direct(addr),
        PathReport::PeerRelay(relay) => LinkPath::PeerRelay(relay),
        PathReport::Derp(region) => LinkPath::Derp(region),
        PathReport::Idle => LinkPath::Idle,
    }
}

/// The machines group: `WatchMachines` and `EngineLoad`.
pub struct MachinesService {
    hub: Arc<MachinesHub>,
}

impl MachinesService {
    #[must_use]
    pub fn new(hub: Arc<MachinesHub>) -> Self {
        Self { hub }
    }
}

impl Group for MachinesService {
    fn serves(&self, method: &str) -> bool {
        method == WatchMachines::NAME || method == EngineLoad::NAME
    }
}

#[async_trait]
impl RpcService for MachinesService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            WatchMachines::NAME => {
                let (_, WatchMachines { since }) = routed(params)?;
                let items = self.hub.watch(since);
                Ok(RpcReply::Stream(Box::pin(futures::StreamExt::map(
                    items,
                    |item| serde_json::to_value(item).unwrap_or_default(),
                ))))
            }
            EngineLoad::NAME => {
                let (_, EngineLoad {}) = routed(params)?;
                RpcReply::value(&self.hub.load())
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn machine(engine: &str, rtt: u64) -> Machine {
        Machine {
            engine: EngineId(engine.into()),
            name: engine.into(),
            kind: MachineKind::Engine,
            link: MachineLink::Live,
            path: Some(LinkPath::Direct("100.64.0.2:7433".into())),
            rtt_us: Some(rtt),
            version: "1".into(),
            protocol: Some(PROTOCOL),
            owner: true,
            live_agents: None,
        }
    }

    proptest! {
        /// A watcher that falls behind holds one pending event per machine at most, and folding
        /// what it drains gives the list the host holds: merging never changes the result.
        #[test]
        fn merged_machine_events_fold_to_the_hosts_list(
            steps in proptest::collection::vec(
                (prop_oneof![Just("a"), Just("b"), Just("c")], proptest::option::of(0_u64..4)),
                0..40,
            ),
        ) {
            let mut topic: Topic<MachinesFeed> =
                Topic::new(1, Machines::default(), Limits::default());
            let (id, _) = topic.join(None);
            let Some(Some(Item::Snapshot { state: mut viewer, .. })) = topic.drain(id) else {
                return Err(TestCaseError::fail("a snapshot first"));
            };
            for (engine, rtt) in steps {
                let event = match rtt {
                    Some(rtt) => MachineEvent::Changed(machine(engine, rtt)),
                    None => MachineEvent::Gone(EngineId(engine.into())),
                };
                topic.publish(vec![event]);
            }
            if let Some(Some(Item::Events { events, .. })) = topic.drain(id) {
                prop_assert!(events.len() <= 3, "one pending event per machine: {:?}", events);
                for event in &events {
                    viewer.apply(event);
                }
            }
            prop_assert_eq!(&viewer, topic.state());
        }
    }
}
