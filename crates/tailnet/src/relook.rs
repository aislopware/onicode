//! This machine's daemon, looked up when first needed, again while none is found, and again
//! after it fails. An engine started at login ahead of Tailscale, or an App Store extension that
//! restarted on a new port and token, is found without a restart.
//!
//! Ported from the `Source::Machine` half of slopty's `slopty-net` `admission.rs` (MIT).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

use crate::{LocalApi, LocalApiError, Status, Tailnet, Watch, WhoIs};

/// How long a lookup that found no Tailscale stands before the next call looks again.
pub const RELOOK: Duration = Duration::from_secs(5);

type Find = Box<dyn Fn() -> Option<Arc<dyn Tailnet>> + Send + Sync>;

/// A [`Tailnet`] found by a lookup, and found again when it is missing or stops answering.
///
/// A call made while none is found fails with [`LocalApiError::Missing`]: a consumer that
/// admits peers refuses every tailnet address then (fail closed).
pub struct Relooking {
    find: Find,
    relook: Duration,
    lookup: Mutex<Lookup>,
}

#[derive(Default)]
struct Lookup {
    api: Option<Arc<dyn Tailnet>>,
    looked: Option<Instant>,
}

impl std::fmt::Debug for Relooking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relooking")
            .field("relook", &self.relook)
            .field("found", &self.lookup.lock().api.is_some())
            .finish_non_exhaustive()
    }
}

impl Relooking {
    /// This machine's Tailscale ([`LocalApi::find`]), looked up again at most every `RELOOK`
    /// while none is found.
    #[must_use]
    pub fn system() -> Self {
        Self::new(
            || LocalApi::find().map(|api| Arc::new(api) as Arc<dyn Tailnet>),
            RELOOK,
        )
    }

    /// `find` in place of this machine's lookup, asked again at most every `relook` while it
    /// finds none, and at once after the one it found fails.
    #[must_use]
    pub fn new(
        find: impl Fn() -> Option<Arc<dyn Tailnet>> + Send + Sync + 'static,
        relook: Duration,
    ) -> Self {
        Self {
            find: Box::new(find),
            relook,
            lookup: Mutex::default(),
        }
    }

    /// The tailnet as last found, looking again when none was and the wait is up.
    #[must_use]
    pub fn current(&self) -> Option<Arc<dyn Tailnet>> {
        let mut lookup = self.lookup.lock();
        let due = lookup.looked.is_none_or(|at| at.elapsed() >= self.relook);
        if lookup.api.is_none() && due {
            lookup.api = (self.find)();
            lookup.looked = Some(Instant::now());
        }
        lookup.api.clone()
    }

    /// The daemon did not answer as found: the next [`Self::current`] looks it up again.
    fn lost(&self) {
        *self.lookup.lock() = Lookup::default();
    }

    /// `call` on the tailnet found; once more on a fresh lookup when it fails.
    async fn with<T, F>(&self, call: impl Fn(Arc<dyn Tailnet>) -> F) -> Result<T, LocalApiError>
    where
        F: Future<Output = Result<T, LocalApiError>>,
    {
        let api = self.current().ok_or(LocalApiError::Missing)?;
        match call(api).await {
            Ok(answer) => Ok(answer),
            Err(e) => {
                tracing::info!(error = %e, "tailscale did not answer; looking it up again");
                self.lost();
                let again = self.current().ok_or(e)?;
                call(again).await
            }
        }
    }
}

#[async_trait::async_trait]
impl Tailnet for Relooking {
    async fn status(&self) -> Result<Status, LocalApiError> {
        self.with(|api| async move { api.status().await }).await
    }

    async fn whois(&self, from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError> {
        self.with(|api| async move { api.whois(from).await }).await
    }

    async fn watch(&self) -> Result<Watch, LocalApiError> {
        self.with(|api| async move { api.watch().await }).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::status::tests::STATUS;

    /// None running refuses; one that comes up later is found on a later call; one that stops
    /// answering is looked up again within the same call.
    #[tokio::test]
    async fn tailscale_is_looked_up_again_when_missing_or_failing() {
        static STAGE: AtomicUsize = AtomicUsize::new(0);
        let (stale, _) = crate::fake::daemon(|_| (401, "bad token".to_owned()))
            .await
            .unwrap();
        let (fresh, _) = crate::fake::daemon(|_| (200, STATUS.to_owned()))
            .await
            .unwrap();
        let found: Vec<Arc<dyn Tailnet>> = vec![Arc::new(stale), Arc::new(fresh)];
        let relooking = Relooking::new(
            move || {
                found
                    .get(STAGE.load(Ordering::SeqCst).checked_sub(1)?)
                    .cloned()
            },
            Duration::ZERO,
        );
        assert!(
            matches!(relooking.status().await, Err(LocalApiError::Missing)),
            "none running"
        );
        STAGE.store(1, Ordering::SeqCst);
        assert!(relooking.current().is_some(), "found once it runs");
        STAGE.store(2, Ordering::SeqCst);
        let status = relooking
            .status()
            .await
            .expect("the stale daemon failed, the fresh one answered");
        assert!(status.running());
    }
}
