//! The processed-op ledger for the Files and Git groups (engine.md "Ids"): a mutating call runs
//! once per [`OpId`], and a repeat gets the first reply, from the call still running or from the
//! store's `ops` table, which keeps it for 7 days. A call that failed is not recorded, so its
//! retry runs again.
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, MutexGuard, PoisonError};

use onicode_protocol::OpId;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::watch;
use uuid::Uuid;

use crate::turn::{Database, locked};

/// How long the ledger keeps a processed op.
const OP_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The ledger is pruned after every this many recorded ops.
const PRUNE_EVERY: u64 = 256;

type Settled = Option<Result<String, String>>;

#[derive(Default)]
struct Inner {
    /// Calls still running, by op: a repeat waits for the first one's reply.
    inflight: HashMap<OpId, watch::Receiver<Settled>>,
    recorded: u64,
}

pub(crate) struct Ledger {
    store: Database,
    inner: Mutex<Inner>,
}

enum Start {
    Wait(watch::Receiver<Settled>),
    Done(String),
    Run(watch::Sender<Settled>),
}

impl Ledger {
    pub(crate) fn new(store: Database) -> Self {
        Self {
            store,
            inner: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `run` once per op: a repeat waits for the first run's reply, or reads it from the
    /// store. Without an op, `run` simply runs.
    pub(crate) async fn once<R, F>(&self, op: Option<OpId>, run: F) -> Result<R, String>
    where
        R: Serialize + DeserializeOwned,
        F: Future<Output = Result<R, String>>,
    {
        let Some(op) = op else {
            return run.await;
        };
        let start = {
            let mut inner = self.lock();
            match inner.inflight.get(&op) {
                Some(waiting) => Start::Wait(waiting.clone()),
                None => match locked(&self.store)?.op_receipt(&op.to_string())? {
                    Some(receipt) => Start::Done(receipt),
                    None => {
                        let (resolve, waiting) = watch::channel(None);
                        inner.inflight.insert(op, waiting);
                        Start::Run(resolve)
                    }
                },
            }
        };
        let resolve = match start {
            Start::Wait(mut waiting) => {
                let settled = waiting
                    .wait_for(Option::is_some)
                    .await
                    .map(|settled| settled.clone())
                    .map_err(|_| "The engine stopped before this call settled".to_owned())?;
                let text = settled.ok_or("The engine stopped before this call settled")??;
                return serde_json::from_str(&text).map_err(|error| error.to_string());
            }
            Start::Done(receipt) => {
                return serde_json::from_str(&receipt).map_err(|error| error.to_string());
            }
            Start::Run(resolve) => resolve,
        };
        let result = run.await;
        let settled = match &result {
            Ok(reply) => serde_json::to_string(reply).map_err(|error| error.to_string()),
            Err(error) => Err(error.clone()),
        };
        {
            let mut inner = self.lock();
            if let Ok(text) = &settled {
                self.record(&mut inner, op, text);
            }
            inner.inflight.remove(&op);
        }
        resolve.send_replace(Some(settled));
        result
    }

    fn record(&self, inner: &mut Inner, op: OpId, reply: &str) {
        let now = onicode_core::time::now_ms();
        if let Ok(store) = locked(&self.store) {
            let _ = store.record_op(&op.to_string(), Uuid::nil(), reply, now);
            inner.recorded += 1;
            if inner.recorded.is_multiple_of(PRUNE_EVERY) {
                let _ = store.prune_ops(now.saturating_sub(OP_RETENTION_MS));
            }
        }
    }
}
