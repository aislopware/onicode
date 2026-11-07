//! Engines, an app and a phone on a simulated tailnet (engine.md "Tests → Simulation"), in
//! virtual time over `onicode-net-sim`: real engines (`Local`) with a scripted provider, joined
//! through `onicode-peer` exactly as the process joins the real tailnet, an app speaking to its own
//! engine in memory, and a phone dialling an engine directly. Every run is seeded and prints its
//! seed when it fails. Nothing here touches the real tailnet.
//!
//! * A forwarded watch rides out a duplicate-link race: one snapshot, every event once, in order.
//! * A host that restarts mid-turn answers the forwarded watch with a fresh snapshot (a new epoch),
//!   and a repeated op gets its first receipt.
//! * A call for a host that is asleep waits in the outbox and takes effect exactly once when the
//!   host returns; its repeats wait with it.
//! * A slow phone is pushed to `Resync` by the host while the Mac, through its engine, stays live.
//! * Round trips against the latency baseline: a remote receipt in 1 RTT + 1 ms, and a resume after
//!   a link drop with the cursor in the ring in 1 RTT, with no snapshot.
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use onicode_engine::peers::{self, Peers};
use onicode_engine::session::{Runner, Spawn, TurnJob};
use onicode_engine::turn::Update;
use onicode_engine::workspace::Chores;
use onicode_engine::{Local, Options};
use onicode_net::Role;
use onicode_net::transport::{PORT, endpoint_on};
use onicode_net_sim::tailnet::{ipv4, stable_id};
use onicode_net_sim::{Faults, Link, Net, NodeId, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{
    LinkState, LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerLinks, Sockets,
};
use onicode_protocol::engine::{EngineInfo, WatchMachines};
use onicode_protocol::session::{self, SessionEvent, SessionState, WatchSession};
use onicode_protocol::session_groups::WorkItem;
use onicode_protocol::session_title::{GeneratedTitle, TextModel, WorkItemHint};
use onicode_protocol::transcript::Row;
use onicode_protocol::workspace::{CreateSession, NewSession};
use onicode_protocol::{Call, Cursor, EngineId, Item, MessageId, OpId, Routed, SessionId, Watch};
use onicode_rpc::{RpcClient, RpcSubscription, memory_client};
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

const USER: i64 = 2;

/// The simulated network's sockets.
struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<onicode_net::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// The engine's work on the test's runtime, in virtual time.
struct TokioSpawn;

impl Spawn for TokioSpawn {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        tokio::spawn(work);
    }
}

/// No generated titles, work items or branch names.
struct NoChores;

impl Chores for NoChores {
    fn title(&self, _: TextModel, _: &Path, _: &str, _: Arc<AtomicBool>) -> Option<GeneratedTitle> {
        None
    }

    fn work_item(&self, _: &str, _: &Path, _: Option<&WorkItemHint>) -> Option<WorkItem> {
        None
    }

    fn branch(&self, _: TextModel, _: &Path, _: &str, _: Arc<AtomicBool>) -> Option<String> {
        None
    }
}

type Jobs = Arc<Mutex<Vec<TurnJob>>>;

/// One engine on a node: its turns run by the test.
struct Engine {
    local: Arc<Local>,
    jobs: Jobs,
}

/// An engine over `dir` on `node`, joined to the tailnet. A node whose last engine just went may
/// still hold its port for the drain: the join is tried again until it binds.
async fn engine(tailnet: &SimTailnet, node: NodeId, dir: &Path, seed: u64) -> Engine {
    let jobs = Jobs::default();
    let held = jobs.clone();
    let runner: Runner = Arc::new(move |job| held.lock().unwrap().push(job));
    let options = Options {
        chores: Some(Arc::new(NoChores)),
        runner: Some(runner),
        ..onicode_engine::testing::quiet()
    };
    let local = Arc::new(Local::open(dir, Arc::new(TokioSpawn), options).unwrap());
    let sockets = SimSockets(tailnet.net().clone());
    for _ in 0..30 {
        match Peers::start_on(
            tailnet.local_api(node),
            &sockets,
            Some(seed),
            local.peer_service(),
        )
        .await
        {
            Ok(peers) => {
                local.join(peers);
                assert_eq!(local.identity.id(), EngineId(stable_id(node)));
                return Engine { local, jobs };
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    panic!("the engine never joined the tailnet");
}

impl Engine {
    fn id(&self) -> EngineId {
        self.local.identity.id()
    }

    fn peers(&self) -> Arc<Peers> {
        self.local.peers().expect("joined")
    }

    async fn find(&self) {
        self.peers().directory().refresh().await.unwrap();
    }

    /// A new session in this engine's store: its row and id.
    fn session(&self) -> (i64, SessionId) {
        let row = self.local.seed().create("codex", "/project").unwrap().id;
        (row, self.local.session.session_id(row).unwrap())
    }

    fn turns(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    /// The running turn's next update.
    fn publish(&self, update: Update) {
        let jobs = self.jobs.lock().unwrap();
        jobs.last()
            .expect("a running turn")
            .updates
            .try_send(update)
            .unwrap();
    }

    /// The host's own view of `session`: a fresh watcher's snapshot.
    async fn state(&self, session: SessionId) -> SessionState {
        use futures::StreamExt;
        let mut items = self
            .local
            .session
            .watch(WatchSession {
                session,
                since: None,
                window_turns: usize::MAX,
            })
            .unwrap();
        match items.next().await {
            Some(Item::Snapshot { state, .. }) => state,
            other => panic!("a snapshot first, not {other:?}"),
        }
    }
}

fn routed<T: Serialize>(engine: Option<&EngineId>, op: Option<OpId>, body: T) -> Value {
    serde_json::to_value(Routed {
        engine: engine.cloned(),
        op,
        body,
    })
    .unwrap()
}

/// The app: a connection to its own engine, in memory.
fn app(engine: &Engine) -> Arc<RpcClient> {
    Arc::new(memory_client(engine.local.service.clone()))
}

/// Waits (in virtual time) until `done`.
async fn until(within: Duration, done: impl Fn() -> bool) {
    tokio::time::timeout(within, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("it came to pass in time");
}

/// Sends a prompt to `session` on `host` through the app's engine and answers the turn's start,
/// so the send's receipt comes.
async fn start_turn(app: &Arc<RpcClient>, host: &Engine, session: SessionId, op: OpId) -> Value {
    let (app, engine) = (app.clone(), host.id());
    let body = session::Send {
        session,
        message: MessageId::new(),
        text: "go".into(),
        attachments: Vec::new(),
        turn: None,
    };
    let call = tokio::spawn(async move {
        app.call(session::Send::NAME, routed(Some(&engine), Some(op), body))
            .await
    });
    let turns = host.turns();
    until(Duration::from_secs(10), || host.turns() > turns).await;
    host.publish(Update {
        status: "Working".into(),
        ..Update::default()
    });
    call.await.unwrap().unwrap()
}

/// Row `at` of the scripted transcript, `bytes` long.
fn row(at: usize, bytes: usize) -> Row {
    Row {
        role: "assistant".into(),
        text: format!("{at:08}").repeat(bytes.div_ceil(8).max(1)),
        first: true,
        sent_at: Some(at as u64),
        id: None,
    }
}

fn grow(at: usize, bytes: usize) -> Update {
    Update {
        start: at,
        rows: vec![row(at, bytes)],
        status: "Working".into(),
        ..Update::default()
    }
}

/// A watcher of one session, as the app or the phone holds it: it checks every item's numbering
/// and folds what arrives into the state it shows.
struct Viewer {
    items: Box<dyn Source>,
    cursor: Option<Cursor>,
    snapshots: Vec<Cursor>,
    resyncs: usize,
    state: SessionState,
}

trait Source: Send {
    fn next(&mut self) -> BoxFuture<'_, Option<Value>>;
}

impl Source for RpcSubscription {
    fn next(&mut self) -> BoxFuture<'_, Option<Value>> {
        Box::pin(self.recv())
    }
}

impl Source for onicode_peer::PeerSubscription {
    fn next(&mut self) -> BoxFuture<'_, Option<Value>> {
        Box::pin(self.recv())
    }
}

fn watching(session: SessionId) -> WatchSession {
    WatchSession {
        session,
        since: None,
        window_turns: usize::MAX,
    }
}

impl Viewer {
    fn new(items: Box<dyn Source>) -> Self {
        Self {
            items,
            cursor: None,
            snapshots: Vec::new(),
            resyncs: 0,
            state: SessionState::default(),
        }
    }

    /// The app's watch of `session` on `host`, through its own engine.
    async fn through(app: &RpcClient, host: &EngineId, session: SessionId) -> Self {
        let items = app
            .subscribe_scoped(
                WatchSession::NAME,
                routed(Some(host), None, watching(session)),
            )
            .await
            .unwrap();
        Self::new(Box::new(items))
    }

    /// The next item within `within`, checked and folded; `None` when none came or the watch
    /// ended.
    async fn next(&mut self, within: Duration) -> Option<Item<SessionState, SessionEvent>> {
        let value = tokio::time::timeout(within, self.items.next())
            .await
            .ok()??;
        let item: Item<SessionState, SessionEvent> = serde_json::from_value(value).unwrap();
        match &item {
            Item::Snapshot { at, state } => {
                self.cursor = Some(*at);
                self.snapshots.push(*at);
                self.state = state.clone();
            }
            Item::Events {
                first,
                last,
                events,
            } => {
                let cursor = self.cursor.as_mut().expect("events after a snapshot");
                assert_eq!(*first, cursor.seq + 1, "no gap and no repeat");
                assert!(*last + 1 >= *first);
                cursor.seq = *last;
                for event in events {
                    onicode_engine::session::apply(&mut self.state, event);
                }
            }
            Item::Resync => self.resyncs += 1,
        }
        Some(item)
    }

    /// Reads until the state holds `rows` rows.
    async fn until_rows(&mut self, rows: usize, within: Duration) {
        let deadline = Instant::now() + within;
        while self.state.rows.len() < rows {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                self.next(left).await.is_some(),
                "only {} of {rows} rows came",
                self.state.rows.len()
            );
        }
    }
}

fn texts(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|row| row.text.as_str()).collect()
}

/// A watch forwarded by the app's engine survives the two engines dialling each other at once:
/// the link it rode loses to the host's dial, and it subscribes again on the kept link with its
/// cursor. The viewer sees one snapshot and every event once, in order, and ends where the host
/// is.
#[tokio::test(start_paused = true)]
async fn a_forwarded_watch_rides_out_a_duplicate_link_race() {
    for seed in [51_u64, 52, 53] {
        let _seed = report_seed("a_forwarded_watch_rides_out_a_duplicate_link_race", seed);
        let tailnet = SimTailnet::new(
            Faults::over(Link {
                delay: Duration::from_millis(3),
                ..Link::CLEAR
            }),
            seed,
        );
        let one = tailnet.add(NodeSpec::owned("one", USER));
        let two = tailnet.add(NodeSpec::owned("two", USER));
        // The app's engine has the larger id, so the host's dial is the link both ends keep and
        // the one the watch rode is retired.
        let (mac, host) = if stable_id(one) > stable_id(two) {
            (one, two)
        } else {
            (two, one)
        };
        let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = engine(&tailnet, mac, dirs.0.path(), seed).await;
        let b = engine(&tailnet, host, dirs.1.path(), seed + 1).await;
        a.find().await;
        b.find().await;
        let (_, session) = b.session();
        let app = app(&a);
        let mut viewer = Viewer::through(&app, &b.id(), session).await;
        assert!(matches!(
            viewer.next(Duration::from_secs(5)).await,
            Some(Item::Snapshot { .. })
        ));
        start_turn(&app, &b, session, OpId::new()).await;

        let dialed = b
            .peers()
            .endpoint()
            .dial(SocketAddr::new(ipv4(mac), PORT))
            .await
            .unwrap();
        b.peers().links().adopt(dialed);
        for at in 0..60 {
            b.publish(grow(at, 64));
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        viewer.until_rows(60, Duration::from_secs(30)).await;

        let kept = a.peers().links().links();
        assert_eq!(kept.len(), 1, "seed {seed}: one link");
        assert!(!kept[0].dialed, "seed {seed}: the host's dial is kept");
        assert_eq!(viewer.snapshots.len(), 1, "seed {seed}: no second snapshot");
        assert_eq!(viewer.resyncs, 0);
        let host_state = b.state(session).await;
        assert_eq!(texts(&viewer.state.rows), texts(&host_state.rows));
    }
}

/// A host that restarts mid-turn comes back under a new epoch: the forwarded watch resubscribes
/// with the cursor it held and gets a fresh snapshot of what the store kept, and a send repeated
/// under its op gets the receipt the first one got, with no second turn.
#[tokio::test(start_paused = true)]
async fn a_host_restart_mid_turn_gives_a_fresh_snapshot() {
    const SEED: u64 = 61;
    let _seed = report_seed("a_host_restart_mid_turn_gives_a_fresh_snapshot", SEED);
    let tailnet = SimTailnet::new(
        Faults::over(Link {
            delay: Duration::from_millis(5),
            ..Link::CLEAR
        }),
        SEED,
    );
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let host = tailnet.add(NodeSpec::owned("laptop", USER));
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = engine(&tailnet, mac, dirs.0.path(), SEED).await;
    let b = engine(&tailnet, host, dirs.1.path(), SEED + 1).await;
    a.find().await;
    let (row_id, session) = b.session();
    let app = app(&a);
    let mut viewer = Viewer::through(&app, &b.id(), session).await;
    viewer.next(Duration::from_secs(5)).await.unwrap();
    let op = OpId::new();
    let receipt = start_turn(&app, &b, session, op).await;
    for at in 0..3 {
        let update = grow(at, 32);
        b.local.seed().write_rows(row_id, at, &update.rows).unwrap();
        b.publish(update);
    }
    viewer.until_rows(3, Duration::from_secs(5)).await;
    let first = viewer.snapshots[0];

    // The host's process ends mid-turn: its turn never speaks again, and nothing it held is
    // written after.
    let Engine {
        local,
        jobs: _stuck_turn,
    } = b;
    drop(local);
    tokio::time::sleep(Duration::from_secs(10)).await;
    let b = engine(&tailnet, host, dirs.1.path(), SEED + 2).await;

    let snapshot = loop {
        match viewer.next(Duration::from_secs(120)).await {
            Some(Item::Snapshot { at, state }) => break (at, state),
            Some(_) => {}
            None => panic!("the watch never came back"),
        }
    };
    assert_ne!(snapshot.0.epoch, first.epoch, "a new epoch");
    assert_eq!(viewer.snapshots.len(), 2);
    assert_eq!(texts(&snapshot.1.rows), texts(&b.state(session).await.rows));
    let kept = b.local.seed().rows(row_id).unwrap();
    assert_eq!(texts(&snapshot.1.rows), texts(&kept), "what the store kept");

    let body = session::Send {
        session,
        message: MessageId::new(),
        text: "go".into(),
        attachments: Vec::new(),
        turn: None,
    };
    let again = app
        .call(session::Send::NAME, routed(Some(&b.id()), Some(op), body))
        .await
        .unwrap();
    assert_eq!(again, receipt, "the first receipt");
    assert_eq!(b.turns(), 0, "no second turn");
}

/// A call made while its host sleeps waits in the forwarding engine's outbox, and a repeat of it
/// waits with it. Once the host wakes, the call takes effect there exactly once and both callers
/// get its receipt; a later repeat gets the same receipt and changes nothing.
#[tokio::test(start_paused = true)]
async fn the_outbox_delivers_exactly_once_after_the_host_returns() {
    const SEED: u64 = 71;
    let _seed = report_seed(
        "the_outbox_delivers_exactly_once_after_the_host_returns",
        SEED,
    );
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let host = tailnet.add(NodeSpec::owned("laptop", USER));
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = engine(&tailnet, mac, dirs.0.path(), SEED).await;
    let b = engine(&tailnet, host, dirs.1.path(), SEED + 1).await;
    a.find().await;
    let before = b.local.seed().sessions().unwrap().len();
    tailnet.set_asleep(host, true);

    let app = app(&a);
    let op = OpId::new();
    let created = SessionId::new();
    let create = |app: Arc<RpcClient>, engine: EngineId| {
        let body = CreateSession {
            session: created,
            provider: "codex".into(),
            cwd: "/project".into(),
            title: None,
            start: NewSession::Bare,
            started_by: None,
        };
        tokio::spawn(async move {
            app.call(CreateSession::NAME, routed(Some(&engine), Some(op), body))
                .await
        })
    };
    let first = create(app.clone(), b.id());
    let repeat = create(app.clone(), b.id());
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(!first.is_finished() && !repeat.is_finished(), "they wait");
    assert_eq!(b.local.seed().sessions().unwrap().len(), before);

    tailnet.set_asleep(host, false);
    let first = tokio::time::timeout(Duration::from_secs(300), first)
        .await
        .expect("delivered once the host is back")
        .unwrap()
        .unwrap();
    let repeat = repeat.await.unwrap().unwrap();
    assert_eq!(first, repeat);
    let later = create(app.clone(), b.id()).await.unwrap().unwrap();
    assert_eq!(later, first, "the first receipt");
    let sessions = b.local.seed().sessions().unwrap();
    assert_eq!(sessions.len(), before + 1, "it took effect exactly once");
    let made = sessions.iter().map(|s| s.id).max().unwrap();
    assert_eq!(b.local.session.session_id(made).unwrap(), created);
}

/// The phone dials the host directly and stops reading; the Mac watches through its engine and
/// keeps up. The host's turn writes 40 MB, more than the phone's QUIC window, the carrier's backlog
/// and the host's queue hold together (about 21 MiB): the phone is
/// dropped to `Resync` by the host's backpressure, while the Mac sees every event, in order,
/// with no `Resync`.
#[tokio::test(start_paused = true)]
async fn a_slow_phone_is_pushed_to_resync_while_the_mac_stays_live() {
    const SEED: u64 = 81;
    const UPDATES: usize = 5000;
    let _seed = report_seed(
        "a_slow_phone_is_pushed_to_resync_while_the_mac_stays_live",
        SEED,
    );
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let host = tailnet.add(NodeSpec::owned("laptop", USER));
    let phone = tailnet.add(NodeSpec::owned("phone", USER).on("ios"));
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = engine(&tailnet, mac, dirs.0.path(), SEED).await;
    let b = engine(&tailnet, host, dirs.1.path(), SEED + 1).await;
    a.find().await;
    let (_, session) = b.session();

    // The phone: a viewer that serves nothing, saying the protocol it speaks.
    let api = tailnet.local_api(phone);
    let mut config = PeerConfig::new(stable_id(phone), Role::Viewer).with_seed(SEED + 2);
    config.caps = peers::caps();
    let sockets = SimSockets(tailnet.net().clone());
    let endpoint = PeerEndpoint::bind(api.clone(), &sockets, config)
        .await
        .unwrap();
    let directory = PeerDirectory::new(endpoint.clone(), api);
    let phone_links = PeerLinks::new(
        endpoint,
        directory.clone(),
        LinksConfig {
            seed: Some(SEED + 2),
            ..LinksConfig::default()
        },
    );
    phone_links.start();
    directory.refresh().await.unwrap();
    let phone_items = phone_links
        .subscribe(
            &b.id().0,
            WatchSession::NAME,
            routed(None, None, watching(session)),
        )
        .await
        .unwrap();
    let mut phone_viewer = Viewer::new(Box::new(phone_items));
    assert!(matches!(
        phone_viewer.next(Duration::from_secs(5)).await,
        Some(Item::Snapshot { .. })
    ));

    let app = app(&a);
    let mut mac_viewer = Viewer::through(&app, &b.id(), session).await;
    mac_viewer.next(Duration::from_secs(5)).await.unwrap();
    start_turn(&app, &b, session, OpId::new()).await;
    let reading = tokio::spawn(async move {
        mac_viewer
            .until_rows(UPDATES, Duration::from_secs(600))
            .await;
        mac_viewer
    });
    for at in 0..UPDATES {
        b.publish(grow(at, 8192));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let mac_viewer = reading.await.unwrap();
    assert_eq!(mac_viewer.resyncs, 0, "the Mac stays live");
    assert_eq!(mac_viewer.snapshots.len(), 1);

    let mut resynced = false;
    while let Some(item) = phone_viewer.next(Duration::from_secs(60)).await {
        if matches!(item, Item::Resync) {
            resynced = true;
            break;
        }
    }
    assert!(resynced, "the slow phone is dropped to Resync");
}

/// The latency baseline (engine.md "Local feel"), counted in round trips over a 40 ms path: a
/// mutating call's receipt from a remote engine in 1 RTT + 1 ms, and a forwarded watch that comes
/// back after its link dropped, its cursor still in the host's ring, in 1 RTT from the link's
/// return, with no snapshot.
#[tokio::test(start_paused = true)]
async fn remote_receipts_and_resumes_cost_one_round_trip() {
    const SEED: u64 = 91;
    const RTT: Duration = Duration::from_millis(40);
    let slack = Duration::from_millis(1);
    let _seed = report_seed("remote_receipts_and_resumes_cost_one_round_trip", SEED);
    let tailnet = SimTailnet::new(
        Faults::over(Link {
            delay: RTT / 2,
            ..Link::CLEAR
        }),
        SEED,
    );
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let host = tailnet.add(NodeSpec::owned("laptop", USER));
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = engine(&tailnet, mac, dirs.0.path(), SEED).await;
    let b = engine(&tailnet, host, dirs.1.path(), SEED + 1).await;
    a.find().await;
    let app = app(&a);
    // Warm: the link is up.
    app.call(EngineInfo::NAME, routed(Some(&b.id()), None, EngineInfo {}))
        .await
        .unwrap();

    let body = CreateSession {
        session: SessionId::new(),
        provider: "codex".into(),
        cwd: "/project".into(),
        title: None,
        start: NewSession::Bare,
        started_by: None,
    };
    let asked = Instant::now();
    app.call(
        CreateSession::NAME,
        routed(Some(&b.id()), Some(OpId::new()), body),
    )
    .await
    .unwrap();
    let receipt = asked.elapsed();
    assert!(receipt <= RTT + slack, "receipt in {receipt:?}");

    let (_, session) = b.session();
    let mut viewer = Viewer::through(&app, &b.id(), session).await;
    viewer.next(Duration::from_secs(5)).await.unwrap();
    // A running turn holds the session live at the host, so its ring outlives the link.
    start_turn(&app, &b, session, OpId::new()).await;
    b.publish(grow(0, 64));
    viewer.until_rows(1, Duration::from_secs(5)).await;

    let links = a.peers().links().clone();
    let device = b.id().0;
    tailnet.net().blackout(Duration::from_secs(45));
    until(Duration::from_secs(60), || {
        links.state(&device) != LinkState::Live
    })
    .await;
    let mut changes = links.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(300), async {
        while links.state(&device) != LinkState::Live {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the link comes back");
    let back = Instant::now();
    let item = viewer
        .next(Duration::from_secs(5))
        .await
        .expect("the watch resumes");
    let resume = back.elapsed();
    assert!(matches!(item, Item::Events { .. }), "no snapshot: {item:?}");
    assert_eq!(viewer.snapshots.len(), 1);
    assert!(resume <= RTT + slack, "resumed in {resume:?}");

    // The app's engine lists the host, linked, with its path and round trip.
    let mut machines = app
        .subscribe_scoped(
            WatchMachines::NAME,
            routed(None, None, WatchMachines { since: None }),
        )
        .await
        .unwrap();
    let mut state = <WatchMachines as Watch>::State::default();
    let listed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let value = machines.recv().await.expect("the list stays open");
            let item: Item<<WatchMachines as Watch>::State, <WatchMachines as Watch>::Event> =
                serde_json::from_value(value).unwrap();
            match item {
                Item::Snapshot { state: list, .. } => state = list,
                Item::Events { events, .. } => {
                    for event in &events {
                        state.apply(event);
                    }
                }
                Item::Resync => panic!("the list never falls behind here"),
            }
            if let Some(host) = state.get(&b.id())
                && host.link == onicode_protocol::engine::MachineLink::Live
            {
                break host.clone();
            }
        }
    })
    .await
    .expect("the host is listed, linked");
    assert!(
        !listed.version.is_empty() && listed.protocol.is_some(),
        "{listed:?}"
    );
    assert!(state.get(&a.id()).is_some(), "and this engine");
}
