//! The client's own tokio runtime.
//!
//! One small multi-thread runtime per process, created on first use and never
//! dropped: platform UI threads never block on it, and a client that shuts
//! down stops its *tasks* (cancellation token) rather than a runtime — dropping
//! a runtime from inside async context panics, and client restarts would
//! otherwise churn OS threads.
//!
//! Every public `async fn` in this crate spawns its work onto this runtime and
//! awaits the join handle, so the returned futures are executor-agnostic:
//! UniFFI's Swift/Kotlin executors (or any other) can poll them directly.
//!
//! A test lends the client its own runtime instead ([`lend`]): every task the
//! client spawns from the test's thread then runs there, so a current-thread
//! runtime on a paused clock drives the client, its links and its timers in
//! virtual time, and the shared runtime is never made.

use std::cell::RefCell;
use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Handle, Runtime};
use tokio::task::JoinHandle;

use crate::error::ClientError;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// The shared runtime (2 worker threads — sync is I/O bound; decode work is
/// per-changed-entry and small).
pub fn shared() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("onicode-client")
            .enable_all()
            .build()
            .expect("onicode-client runtime")
    })
}

thread_local! {
    /// The runtime lent to this thread, if one is ([`lend`]).
    static LENT: RefCell<Option<Handle>> = const { RefCell::new(None) };
}

/// The runtime the client's tasks go to: the one lent to this thread, else
/// [`shared`].
pub fn handle() -> Handle {
    LENT.with_borrow(Clone::clone)
        .unwrap_or_else(|| shared().handle().clone())
}

/// Spawn `fut` on [`handle`].
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle().spawn(fut)
}

/// Lend `runtime` to this thread until the answer is dropped: the client's
/// tasks spawned from this thread, and from those tasks when `runtime` runs
/// them here (a current-thread runtime), run on it rather than on [`shared`].
/// A test's own runtime, so a paused clock drives the client.
#[must_use = "the loan ends when the answer is dropped"]
pub fn lend(runtime: Handle) -> Lent {
    Lent {
        previous: LENT.replace(Some(runtime)),
    }
}

/// A runtime lent to this thread ([`lend`]); dropping it gives back whatever
/// was lent before.
#[derive(Debug)]
pub struct Lent {
    previous: Option<Handle>,
}

impl Drop for Lent {
    fn drop(&mut self) {
        LENT.set(self.previous.take());
    }
}

/// Run `fut` on the client runtime ([`handle`]) and await its result from any executor.
pub async fn run<F, T>(fut: F) -> Result<T, ClientError>
where
    F: Future<Output = Result<T, ClientError>> + Send + 'static,
    T: Send + 'static,
{
    match spawn(fut).await {
        Ok(result) => result,
        Err(err) if err.is_cancelled() => Err(ClientError::Closed),
        Err(err) => Err(ClientError::Internal(format!("task panicked: {err}"))),
    }
}
