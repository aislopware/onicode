//! The two lanes Git runs in (t3code #32162): a foreground lane for what a person waits on (their
//! calls and the status watch's refreshes) and a small background lane for work nobody waits on
//! (pull-request discovery and the watch's periodic repair reads). The caller picks the lane.
//! Each lane has its own permits, so a background flood can hold at most its own two: a
//! foreground call waits only for a foreground slot.
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Git commands a person waits on that run at once.
pub(crate) const FOREGROUND_PERMITS: usize = 8;
/// Background Git commands that run at once.
pub(crate) const BACKGROUND_PERMITS: usize = 2;

/// Which lane a Git command runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// A person waits on it.
    Foreground,
    /// Nobody waits on it.
    Background,
}

pub(crate) struct Lanes {
    foreground: Arc<Semaphore>,
    background: Arc<Semaphore>,
}

impl Default for Lanes {
    fn default() -> Self {
        Self::new(FOREGROUND_PERMITS, BACKGROUND_PERMITS)
    }
}

impl Lanes {
    pub(crate) fn new(foreground: usize, background: usize) -> Self {
        Self {
            foreground: Arc::new(Semaphore::new(foreground)),
            background: Arc::new(Semaphore::new(background)),
        }
    }

    /// A slot in `lane`, once one is free; first come, first served within the lane.
    pub(crate) async fn enter(&self, lane: Lane) -> Result<OwnedSemaphorePermit, String> {
        let semaphore = match lane {
            Lane::Foreground => &self.foreground,
            Lane::Background => &self.background,
        };
        semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Git's lanes closed".to_owned())
    }
}
