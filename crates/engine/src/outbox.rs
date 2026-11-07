//! The forwarding engine's outbox (engine.md, "Authority"): a mutating call for an engine that
//! cannot be reached is kept in the store's `outbox` table and delivered once that engine is back,
//! in the order the calls were made, across restarts of either engine. The caller's reply waits
//! for the delivery. A delivery whose answer is lost with its link goes again under the same op,
//! which the host's ledger answers with its first receipt: each call takes effect exactly once.
//!
//! A call for an engine with calls already waiting joins the queue behind them, so a call made
//! after the engine came back never overtakes one made while it was away.
//!
//! Two runs of the forwarding engine can share the store for a moment (a restart while the earlier
//! run still delivers), and either may deliver what the other put in. A caller whose call left the
//! outbox without its run delivering it sends it again under the same op once nothing waits ahead
//! of it: the host's ledger answers with the receipt of the delivery that took effect.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use onicode_core::store::OutboxEntry;
use onicode_protocol::{EngineId, OpId};
use onicode_rpc::RpcError;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::router::{Hop, Peering};
use crate::turn::{Database, locked};

/// How long a delivery whose link went before its answer waits before it looks again.
const AGAIN: Duration = Duration::from_millis(50);

type Reply = Result<Value, String>;

/// A call whose caller waits, kept beside its outbox entry.
struct Waiting {
    engine: String,
    method: String,
    params: Value,
    callers: Vec<oneshot::Sender<Reply>>,
}

/// One delivery: the op, its method and its params.
type Delivery = (String, String, Value);

/// The outbox over the engine's store.
pub(crate) struct Outbox {
    store: Database,
    /// Callers waiting for an op's delivery, by op.
    waiters: Mutex<HashMap<String, Waiting>>,
    /// The engines a delivery runs for now.
    running: Mutex<HashSet<String>>,
    peering: Mutex<Option<Weak<dyn Peering>>>,
    /// The runtime deliveries run on, from [`Self::start`].
    runtime: OnceLock<tokio::runtime::Handle>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Outbox {
    pub(crate) fn new(store: Database) -> Arc<Self> {
        Arc::new(Self {
            store,
            waiters: Mutex::default(),
            running: Mutex::default(),
            peering: Mutex::new(None),
            runtime: OnceLock::new(),
        })
    }

    /// Delivers through `peering` from now on, starting with what earlier runs left. Runs inside
    /// a tokio runtime.
    pub(crate) fn start(self: &Arc<Self>, peering: &Arc<dyn Peering>) {
        *lock(&self.peering) = Some(Arc::downgrade(peering));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let _ = self.runtime.set(runtime);
        }
        match locked(&self.store).and_then(|store| store.outbox_engines()) {
            Ok(engines) => {
                for engine in engines {
                    self.kick(&EngineId(engine));
                }
            }
            Err(error) => eprintln!("Cannot read the outbox: {error}"),
        }
    }

    fn peering(&self) -> Option<Arc<dyn Peering>> {
        lock(&self.peering).as_ref().and_then(Weak::upgrade)
    }

    fn pending(&self, engine: &EngineId) -> Vec<OutboxEntry> {
        match locked(&self.store).and_then(|store| store.outbox_for(&engine.0)) {
            Ok(pending) => pending,
            Err(error) => {
                eprintln!("Cannot read the outbox: {error}");
                Vec::new()
            }
        }
    }

    /// Sends the mutating call `op` to `engine`: straight through when nothing waits for that
    /// engine and it answers, else through the outbox. Resolves with the host's reply, however
    /// long the engine stays away.
    pub(crate) async fn send(
        self: &Arc<Self>,
        peering: Option<Arc<dyn Peering>>,
        engine: &EngineId,
        op: OpId,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let key = op.to_string();
        let held = locked(&self.store)
            .and_then(|store| store.outbox_holds(&key))
            .map_err(RpcError::Failed)?;
        if !held
            && self.pending(engine).is_empty()
            && let Some(peering) = &peering
        {
            match peering.call(engine, method, params.clone()).await {
                Ok(reply) => return Ok(reply),
                Err(Hop::Refused(error)) => return Err(error),
                Err(Hop::Unreachable(_)) => {}
            }
        }
        // Waiting before the entry goes in, so its delivery cannot be missed.
        let (tx, rx) = oneshot::channel();
        lock(&self.waiters)
            .entry(key.clone())
            .or_insert_with(|| Waiting {
                engine: engine.0.clone(),
                method: method.to_owned(),
                params: params.clone(),
                callers: Vec::new(),
            })
            .callers
            .push(tx);
        let entry = OutboxEntry {
            op: key,
            engine: engine.0.clone(),
            method: method.to_owned(),
            params: params.to_string(),
            at: onicode_protocol::time::now_ms(),
        };
        locked(&self.store)
            .and_then(|store| store.outbox_put(&entry))
            .map_err(RpcError::Failed)?;
        self.kick(engine);
        match rx.await {
            Ok(reply) => reply.map_err(RpcError::Failed),
            Err(_) => Err(RpcError::Failed("The outbox stopped".into())),
        }
    }

    /// Starts `engine`'s delivery unless one runs: only once [`Self::start`] gave a runtime.
    fn kick(self: &Arc<Self>, engine: &EngineId) {
        let Some(runtime) = self.runtime.get() else {
            return;
        };
        if !lock(&self.running).insert(engine.0.clone()) {
            return;
        }
        runtime.spawn(self.clone().deliver(engine.clone()));
    }

    /// What goes to `engine` next: its oldest entry, else a call of this run's callers that
    /// another run delivered (gone from the store, its caller still waiting).
    fn next(&self, engine: &EngineId) -> Option<Delivery> {
        if let Some(entry) = self.pending(engine).into_iter().next() {
            let params = serde_json::from_str(&entry.params).unwrap_or(Value::Null);
            return Some((entry.op, entry.method, params));
        }
        let waiters = lock(&self.waiters);
        let store = locked(&self.store).ok()?;
        waiters
            .iter()
            .filter(|(op, waiting)| {
                waiting.engine == engine.0 && store.outbox_holds(op).is_ok_and(|held| !held)
            })
            .map(|(op, waiting)| (op.clone(), waiting.method.clone(), waiting.params.clone()))
            .next()
    }

    /// Delivers what waits for `engine`, oldest first, each once it is reachable, until nothing
    /// waits.
    async fn deliver(self: Arc<Self>, engine: EngineId) {
        loop {
            let Some((op, method, params)) = self.next(&engine) else {
                // A call put in while this one looked is seen under the same lock its kick takes.
                let mut running = lock(&self.running);
                if self.next(&engine).is_none() {
                    running.remove(&engine.0);
                    return;
                }
                continue;
            };
            let Some(peering) = self.peering() else {
                lock(&self.running).remove(&engine.0);
                return;
            };
            peering.reached(&engine).await;
            match peering.call(&engine, &method, params).await {
                Ok(reply) => self.settle(&op, Ok(reply)),
                Err(Hop::Refused(error)) => self.settle(&op, Err(error.to_string())),
                Err(Hop::Unreachable(_)) => tokio::time::sleep(AGAIN).await,
            }
        }
    }

    /// `op` was answered: out of the outbox, and its reply to whoever waits.
    fn settle(&self, op: &str, reply: Reply) {
        if let Err(error) = locked(&self.store).and_then(|store| store.outbox_remove(op)) {
            eprintln!("Cannot settle an outbox entry: {error}");
        }
        let waiting = lock(&self.waiters).remove(op);
        for caller in waiting.into_iter().flat_map(|waiting| waiting.callers) {
            let _ = caller.send(reply.clone());
        }
    }
}

#[cfg(test)]
mod tests;
