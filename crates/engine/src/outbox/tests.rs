//! The outbox against its model (engine.md "Tests"): seeded runs of calls made while their engines
//! come and go, replies lost with their links, and the forwarding engine starting again over its
//! store while its earlier run still delivers. However they interleave, every call takes effect at
//! its host exactly once, in the order it was first made, every caller gets the host's receipt,
//! and the outbox ends empty.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use onicode_core::store::Store;
use proptest::prelude::*;
use tokio::sync::watch;

use super::*;

const ENGINES: [&str; 2] = ["laptop", "vps"];
const OPS: usize = 6;

/// One host engine as the fake tailnet reaches it: whether it is reachable, its ledger, and
/// what took effect there, in order.
struct Host {
    reachable: watch::Sender<bool>,
    ledger: HashMap<String, Value>,
    applied: Vec<String>,
    /// The next call that takes effect loses its reply with its link.
    lose_next: bool,
}

/// The tailnet of the model: hosts that apply each op once and answer repeats from their ledger.
struct Fake {
    hosts: Mutex<HashMap<String, Host>>,
}

impl Fake {
    fn new() -> Self {
        let hosts = ENGINES
            .iter()
            .map(|engine| {
                (
                    (*engine).to_owned(),
                    Host {
                        reachable: watch::channel(true).0,
                        ledger: HashMap::new(),
                        applied: Vec::new(),
                        lose_next: false,
                    },
                )
            })
            .collect();
        Self {
            hosts: Mutex::new(hosts),
        }
    }

    fn with<T>(&self, engine: &str, act: impl FnOnce(&mut Host) -> T) -> T {
        act(self.hosts.lock().unwrap().get_mut(engine).unwrap())
    }
}

#[async_trait]
impl Peering for Fake {
    async fn call(&self, engine: &EngineId, _: &str, params: Value) -> Result<Value, Hop> {
        self.with(&engine.0, |host| {
            if !*host.reachable.borrow() {
                return Err(Hop::Unreachable("asleep".into()));
            }
            let op = params["op"].as_str().unwrap().to_owned();
            let receipt = host
                .ledger
                .entry(op.clone())
                .or_insert_with(|| {
                    host.applied.push(op.clone());
                    serde_json::json!({ "applied": op })
                })
                .clone();
            if std::mem::take(&mut host.lose_next) {
                return Err(Hop::Unreachable("the reply was lost".into()));
            }
            Ok(receipt)
        })
    }

    async fn watch(
        &self,
        _: &EngineId,
        _: &str,
        _: Value,
    ) -> Result<BoxStream<'static, Value>, Hop> {
        Err(Hop::Unreachable("the model watches nothing".into()))
    }

    async fn reached(&self, engine: &EngineId) {
        let mut reachable = self.with(&engine.0, |host| host.reachable.subscribe());
        let _ = reachable.wait_for(|reachable| *reachable).await;
    }

    fn protocol(&self, _: &str) -> Option<u32> {
        Some(onicode_protocol::PROTOCOL)
    }
}

#[derive(Clone, Debug)]
enum Action {
    /// The app makes op `n` (for engine `n % 2`), again if it made it before.
    Call(usize),
    /// Engine `n` goes away or comes back.
    Flip(usize),
    /// Engine `n`'s next applied call loses its reply.
    Lose(usize),
    /// The forwarding engine starts again over its store.
    Restart,
    /// Time passes.
    Wait(u64),
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => (0..OPS).prop_map(Action::Call),
        2 => (0..ENGINES.len()).prop_map(Action::Flip),
        1 => (0..ENGINES.len()).prop_map(Action::Lose),
        1 => Just(Action::Restart),
        2 => (1_u64..200).prop_map(Action::Wait),
    ]
}

fn op(n: usize) -> OpId {
    OpId(uuid::Uuid::from_u128(
        0x0190_0000_0000_7000_8000_0000_0000_0000 + n as u128,
    ))
}

fn engine_of(n: usize) -> EngineId {
    EngineId(ENGINES[n % ENGINES.len()].to_owned())
}

fn open(path: &std::path::Path) -> Database {
    Arc::new(std::sync::Mutex::new(Store::open(path).unwrap()))
}

fn run(actions: Vec<Action>) -> Result<(), TestCaseError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forwarder.db");
    runtime.block_on(async move {
        let fake = Arc::new(Fake::new());
        let peering: Arc<dyn Peering> = fake.clone();
        let mut outboxes = vec![Outbox::new(open(&path))];
        outboxes[0].start(&peering);
        let mut calls = Vec::new();
        let mut first_made: HashMap<String, Vec<String>> = HashMap::new();
        let mut made = HashSet::new();
        for action in actions {
            match action {
                Action::Call(n) => {
                    let (op, engine) = (op(n), engine_of(n));
                    if made.insert(op) {
                        first_made
                            .entry(engine.0.clone())
                            .or_default()
                            .push(op.to_string());
                    }
                    let outbox = outboxes.last().unwrap().clone();
                    let peering = peering.clone();
                    let params = serde_json::json!({ "engine": null, "op": op, "body": n });
                    calls.push((
                        op,
                        tokio::spawn(async move {
                            outbox
                                .send(Some(peering), &engine, op, "Rename", params)
                                .await
                        }),
                    ));
                }
                Action::Flip(n) => fake.with(ENGINES[n], |host| {
                    host.reachable
                        .send_modify(|reachable| *reachable = !*reachable);
                }),
                Action::Lose(n) => fake.with(ENGINES[n], |host| host.lose_next = true),
                Action::Restart => {
                    let outbox = Outbox::new(open(&path));
                    outbox.start(&peering);
                    outboxes.push(outbox);
                }
                Action::Wait(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            }
            tokio::task::yield_now().await;
        }
        for engine in ENGINES {
            fake.with(engine, |host| host.reachable.send_replace(true));
        }
        for (op, call) in calls {
            let reply = tokio::time::timeout(Duration::from_secs(60), call)
                .await
                .map_err(|_| TestCaseError::fail(format!("{op} never answered")))?
                .unwrap()
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(reply, serde_json::json!({ "applied": op.to_string() }));
        }
        for engine in ENGINES {
            let applied = fake.with(engine, |host| host.applied.clone());
            let expected = first_made.remove(engine).unwrap_or_default();
            prop_assert_eq!(
                &applied,
                &expected,
                "{} took each op once, in order",
                engine
            );
        }
        let store = Store::open(&path).unwrap();
        prop_assert_eq!(store.outbox_engines().unwrap(), Vec::<String>::new());
        Ok(())
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn every_call_takes_effect_once_in_order(actions in prop::collection::vec(action(), 1..40)) {
        run(actions)?;
    }
}

/// A call made while its engine is away waits in the outbox, survives the forwarder starting
/// again, and is delivered once the engine is back; a repeat of it waits with it.
#[test]
fn a_call_for_an_absent_engine_waits_and_arrives_once() {
    run(vec![
        Action::Flip(0),
        Action::Call(0),
        Action::Call(2),
        Action::Call(0),
        Action::Restart,
        Action::Wait(500),
        Action::Call(4),
        Action::Flip(0),
        Action::Wait(10),
    ])
    .unwrap();
}
