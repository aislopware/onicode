//! The link's connect-or-launch and resume: a seeded model over a scripted engine in virtual
//! time, and real engines on real sockets for restarts and racing viewers.
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use onicode_protocol::engine::{EngineInfo, StopEngine};
use onicode_protocol::workspace::{
    CreateSession, NewSession, WatchWorkspace, WorkspaceEvent, WorkspaceState,
};
use onicode_protocol::{Cursor, Item, OpId, Outcome, SessionId, Watch};
use onicode_rpc::{RpcReply, RpcService, serve_connection};
use proptest::prelude::*;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use super::*;
use crate::host::Host;
use crate::local::Local;
use crate::session::ThreadSpawn;
use crate::testing::quiet;

/// A scripted engine's state that outlives its runs, as the store does: the ops it applied and
/// the receipt each got.
#[derive(Default)]
struct Ledger {
    receipts: Mutex<HashMap<OpId, u64>>,
    /// Every time an op arrived, first or repeat.
    arrivals: AtomicUsize,
}

/// The scripted engine's service: `Hello`, and `Apply`, which applies its op once and answers a
/// repeat with the first receipt, after a delay in which a crash can lose the reply.
struct Applying {
    ledger: Arc<Ledger>,
    reply_after: Duration,
}

#[async_trait]
impl RpcService for Applying {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == Hello::NAME {
            return RpcReply::value(&crate::service::hello());
        }
        let routed: Routed<u64> = onicode_rpc::parse_params(params)?;
        let op = routed.op.ok_or(RpcError::BadParams("no op".into()))?;
        self.ledger.arrivals.fetch_add(1, Ordering::Relaxed);
        let receipt = {
            let mut receipts = self.ledger.receipts.lock().unwrap();
            let next = u64::try_from(receipts.len()).unwrap();
            *receipts.entry(op).or_insert(next)
        };
        tokio::time::sleep(self.reply_after).await;
        RpcReply::value(&receipt)
    }
}

/// A door to the scripted engine: it runs once launched and `start` has passed, and a crash
/// ends every connection it serves.
struct Sim {
    ledger: Arc<Ledger>,
    start: Duration,
    reply_after: Duration,
    running: Arc<Mutex<bool>>,
    starting: Arc<Mutex<bool>>,
    served: Mutex<Vec<AbortHandle>>,
    launches: AtomicUsize,
}

impl Sim {
    fn crash(&self) {
        *self.running.lock().unwrap() = false;
        for server in self.served.lock().unwrap().drain(..) {
            server.abort();
        }
    }
}

struct SimDoor(Arc<Sim>);

impl Door for SimDoor {
    fn dial(&self) -> BoxFuture<'static, Result<RpcClient, DialError>> {
        let sim = self.0.clone();
        async move {
            if !*sim.running.lock().unwrap() {
                return Err(DialError::Absent("not running".into()));
            }
            let (client_out, server_in) = mpsc::channel::<String>(16);
            let (server_out, client_in) = mpsc::channel::<String>(16);
            let service = Arc::new(Applying {
                ledger: sim.ledger.clone(),
                reply_after: sim.reply_after,
            });
            let server = tokio::spawn(serve_connection(service, server_out, server_in));
            sim.served.lock().unwrap().push(server.abort_handle());
            Ok(RpcClient::new(client_out, client_in))
        }
        .boxed()
    }

    fn launch(&self) -> Result<(), String> {
        self.0.launches.fetch_add(1, Ordering::Relaxed);
        let mut starting = self.0.starting.lock().unwrap();
        if *starting || *self.0.running.lock().unwrap() {
            return Ok(());
        }
        *starting = true;
        let (running, starting, start) = (
            self.0.running.clone(),
            self.0.starting.clone(),
            self.0.start,
        );
        tokio::spawn(async move {
            tokio::time::sleep(start).await;
            *running.lock().unwrap() = true;
            *starting.lock().unwrap() = false;
        });
        Ok(())
    }

    fn current(&self, _: &Hello) -> bool {
        true
    }

    fn retire(&self) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }
}

/// A seeded run: `callers` calls start at seeded times while the engine crashes at seeded times.
/// Every caller gets the first receipt of its op, every op is applied once however often it
/// arrived, and each outage costs at most one launch however many callers raced into it.
fn run_seeded(seed: u64) {
    let mut state = seed | 1;
    let mut next = |below: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % below
    };
    let callers = 1 + next(8);
    let crashes: Vec<u64> = (0..next(3)).map(|_| 5 + next(120)).collect();
    let sim = Arc::new(Sim {
        ledger: Arc::default(),
        start: Duration::from_millis(1 + next(30)),
        reply_after: Duration::from_millis(next(12)),
        running: Arc::default(),
        starting: Arc::default(),
        served: Mutex::default(),
        launches: AtomicUsize::new(0),
    });
    let starts: Vec<u64> = (0..callers).map(|_| next(80)).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    runtime.block_on(async {
        let link = Link::new(
            Arc::new(SimDoor(sim.clone())),
            Handle::current(),
            Timing::default(),
        );
        let calls: Vec<_> = starts
            .iter()
            .map(|start| {
                let (link, start) = (link.clone(), Duration::from_millis(*start));
                tokio::spawn(async move {
                    tokio::time::sleep(start).await;
                    let op = OpId::new();
                    let params = serde_json::to_value(Routed {
                        engine: None,
                        op: Some(op),
                        body: 0_u64,
                    })
                    .unwrap();
                    (op, link.call("Apply", &params).await)
                })
            })
            .collect();
        for at in &crashes {
            let (sim, at) = (sim.clone(), Duration::from_millis(*at));
            tokio::spawn(async move {
                tokio::time::sleep(at).await;
                sim.crash();
            });
        }
        for call in calls {
            let (op, reply) = call.await.unwrap();
            let reply = reply.unwrap_or_else(|error| panic!("seed {seed}: {error}"));
            let first = sim.ledger.receipts.lock().unwrap()[&op];
            assert_eq!(reply, serde_json::json!(first), "seed {seed}");
        }
    });
    let applied = sim.ledger.receipts.lock().unwrap().len();
    assert_eq!(applied, usize::try_from(callers).unwrap(), "seed {seed}");
    assert!(sim.ledger.arrivals.load(Ordering::Relaxed) >= applied);
    let launches = sim.launches.load(Ordering::Relaxed);
    assert!(
        (1..=1 + crashes.len()).contains(&launches),
        "seed {seed}: {launches} launches for {} crashes",
        crashes.len()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn calls_survive_crashes_and_launch_once_per_outage(seed in any::<u64>()) {
        run_seeded(seed);
    }
}

/// Racing callers on a cold machine launch one engine.
#[test]
fn racing_first_calls_launch_once() {
    run_seeded(0x5eed);
}

/// A door to real engines in `dir`, each launched on a thread of its own as a separate process
/// would be, counting the engines that took the lock.
struct HostDoor {
    dir: PathBuf,
    bound: Arc<AtomicUsize>,
    launches: AtomicUsize,
    engines: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl HostDoor {
    fn new(dir: &Path, bound: Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            dir: dir.to_path_buf(),
            bound,
            launches: AtomicUsize::new(0),
            engines: Mutex::default(),
        })
    }

    fn join(&self) {
        for engine in self.engines.lock().unwrap().drain(..) {
            engine.join().unwrap();
        }
    }
}

struct Doorway(Arc<HostDoor>);

impl Door for Doorway {
    fn dial(&self) -> BoxFuture<'static, Result<RpcClient, DialError>> {
        SocketDoor::with_engine(&self.0.dir, Path::new("/no-engine")).dial()
    }

    fn launch(&self) -> Result<(), String> {
        self.0.launches.fetch_add(1, Ordering::Relaxed);
        let (dir, bound) = (self.0.dir.clone(), self.0.bound.clone());
        let engine = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let open = |dir: &Path| Local::open(dir, Arc::new(ThreadSpawn), quiet());
                if let Some(host) = Host::bind(&dir, open).unwrap() {
                    bound.fetch_add(1, Ordering::Relaxed);
                    host.serve(std::future::pending()).await;
                }
            });
        });
        self.0.engines.lock().unwrap().push(engine);
        Ok(())
    }

    fn current(&self, hello: &Hello) -> bool {
        hello.protocol == PROTOCOL
    }

    fn retire(&self) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }
}

fn routed<T: serde::Serialize>(body: T) -> serde_json::Value {
    serde_json::to_value(Routed {
        engine: None,
        op: None,
        body,
    })
    .unwrap()
}

async fn next_item(items: &mut RpcSubscription) -> Option<Item<WorkspaceState, WorkspaceEvent>> {
    let value = items.recv().await?;
    Some(serde_json::from_value(value).unwrap())
}

fn create(dir: &Path) -> CreateSession {
    CreateSession {
        session: SessionId::new(),
        provider: "codex".into(),
        cwd: dir.display().to_string(),
        title: None,
        start: NewSession::Bare,
        started_by: None,
        scope: Vec::new(),
    }
}

/// Reads `items` until the receipt of `op`, checking each batch starts right after `cursor`.
async fn until_receipt(items: &mut RpcSubscription, cursor: &mut Cursor, op: OpId) {
    loop {
        match next_item(items).await {
            Some(Item::Events {
                first,
                last,
                events,
            }) => {
                assert_eq!(first, cursor.seq + 1, "no gap and no repeat");
                cursor.seq = last;
                let receipt = events.iter().any(|event| {
                    matches!(event, WorkspaceEvent::Receipt { op: seen, .. } if *seen == op)
                });
                if receipt {
                    return;
                }
            }
            other => panic!("expected events, got {other:?}"),
        }
    }
}

/// The engine stops mid-watch. The watch reads as lost; resubscribing with its cursor launches a
/// new engine, whose new epoch answers with a fresh snapshot holding what the old one wrote, and
/// its events follow that snapshot with no gap and no repeat.
#[test]
fn an_engine_restart_mid_watch_resumes_with_a_fresh_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let door = HostDoor::new(dir.path(), Arc::default());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let link = Link::new(
            Arc::new(Doorway(door.clone())),
            Handle::current(),
            Timing::default(),
        );
        let watch = |since| routed(WatchWorkspace { since });
        let mut items = link
            .subscribe(WatchWorkspace::NAME, &watch(None))
            .await
            .unwrap();
        let Some(Item::Snapshot { at, .. }) = next_item(&mut items).await else {
            panic!("a watch starts from a snapshot");
        };
        let mut cursor = at;
        let first = create(dir.path());
        let session = first.session;
        let op = OpId::new();
        let created = link.ask::<CreateSession>(Some(op), first).await.unwrap();
        assert_eq!(created.outcome, Outcome::Applied);
        until_receipt(&mut items, &mut cursor, op).await;

        link.ask::<StopEngine>(None, StopEngine {}).await.unwrap();
        while items.recv().await.is_some() {}
        assert!(items.lost(), "the engine went; the watch did not end");
        door.join();
        // Epochs are counted from the engine's start in milliseconds.
        tokio::time::sleep(Duration::from_millis(5)).await;

        let mut items = link
            .subscribe(WatchWorkspace::NAME, &watch(Some(cursor)))
            .await
            .unwrap();
        let Some(Item::Snapshot { at, state }) = next_item(&mut items).await else {
            panic!("a cursor from the stopped engine gets a fresh snapshot");
        };
        assert_ne!(at.epoch, cursor.epoch);
        assert!(state.sessions.iter().any(|summary| summary.id == session));
        let mut cursor = at;
        let op = OpId::new();
        link.ask::<CreateSession>(Some(op), create(dir.path()))
            .await
            .unwrap();
        until_receipt(&mut items, &mut cursor, op).await;
        assert_eq!(door.launches.load(Ordering::Relaxed), 2);
        assert_eq!(door.bound.load(Ordering::Relaxed), 2);
        link.ask::<StopEngine>(None, StopEngine {}).await.unwrap();
    });
    door.join();
}

/// Two app launches race on a cold machine: whatever each launches, one engine takes the lock,
/// both reach it, and what one creates the other sees.
#[test]
fn two_app_launches_share_one_engine() {
    let dir = tempfile::tempdir().unwrap();
    let bound = Arc::new(AtomicUsize::new(0));
    let (first, second) = (
        HostDoor::new(dir.path(), bound.clone()),
        HostDoor::new(dir.path(), bound.clone()),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let a = Link::new(
            Arc::new(Doorway(first.clone())),
            Handle::current(),
            Timing::default(),
        );
        let b = Link::new(
            Arc::new(Doorway(second.clone())),
            Handle::current(),
            Timing::default(),
        );
        let (seen_a, seen_b) = tokio::join!(
            a.ask::<EngineInfo>(None, EngineInfo {}),
            b.ask::<EngineInfo>(None, EngineInfo {})
        );
        assert_eq!(seen_a.unwrap(), seen_b.unwrap());
        let created = create(dir.path());
        let session = created.session;
        a.ask::<CreateSession>(Some(OpId::new()), created)
            .await
            .unwrap();
        let mut items = b
            .subscribe(
                WatchWorkspace::NAME,
                &routed(WatchWorkspace { since: None }),
            )
            .await
            .unwrap();
        let Some(Item::Snapshot { state, .. }) = next_item(&mut items).await else {
            panic!("a watch starts from a snapshot");
        };
        assert!(state.sessions.iter().any(|summary| summary.id == session));
        a.ask::<StopEngine>(None, StopEngine {}).await.unwrap();
    });
    first.join();
    second.join();
    assert_eq!(bound.load(Ordering::Relaxed), 1);
}
