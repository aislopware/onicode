//! The searches a hub runs, by the id the viewer gave each (`SearchPaths`, `SearchContent`,
//! `SearchSessions`): a newer search under the same id stops the older one, and `StopSearch` stops
//! one when its view closes or its query moves on, as monocode's cancel flag did. A stop that
//! arrives before its search (the two travel apart) is remembered, so the search stops as it
//! starts; viewers mint a fresh id for each search they may stop, so a remembered stop never
//! catches a later search.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// How many stops of searches not running are remembered, newest kept.
const REMEMBERED: usize = 256;

#[derive(Default)]
struct Inner {
    running: HashMap<String, Arc<AtomicBool>>,
    stopped: VecDeque<String>,
}

#[derive(Default)]
pub(crate) struct Searches {
    inner: Mutex<Inner>,
}

/// A search's place in [`Searches`]: its stop flag, let go when dropped.
pub(crate) struct Running<'a> {
    searches: &'a Searches,
    id: String,
    flag: Arc<AtomicBool>,
}

impl Running<'_> {
    /// The flag the search checks as it runs.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }

    /// Whether the search was stopped.
    pub(crate) fn stopped(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut inner = self.searches.lock();
        if inner
            .running
            .get(&self.id)
            .is_some_and(|known| Arc::ptr_eq(known, &self.flag))
        {
            inner.running.remove(&self.id);
        }
    }
}

impl Searches {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A search starting under `id`: it stops an older one under the same id, and starts stopped
    /// when a stop for it came first. An empty id is never stopped.
    pub(crate) fn start(&self, id: &str) -> Running<'_> {
        let flag = Arc::new(AtomicBool::new(false));
        if !id.is_empty() {
            let mut inner = self.lock();
            if let Some(at) = inner.stopped.iter().position(|stopped| stopped == id) {
                inner.stopped.remove(at);
                flag.store(true, Ordering::Release);
            }
            if let Some(older) = inner.running.insert(id.to_owned(), flag.clone()) {
                older.store(true, Ordering::Release);
            }
        }
        Running {
            searches: self,
            id: id.to_owned(),
            flag,
        }
    }

    /// Stops the search running under `id`, or the next one to start under it.
    pub(crate) fn stop(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        let mut inner = self.lock();
        if let Some(running) = inner.running.remove(id) {
            running.store(true, Ordering::Release);
            return;
        }
        if inner.stopped.len() == REMEMBERED {
            inner.stopped.pop_front();
        }
        inner.stopped.push_back(id.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_reaches_the_search_before_or_after_it_starts() {
        let searches = Searches::default();
        let running = searches.start("a");
        assert!(!running.stopped());
        searches.stop("a");
        assert!(running.stopped(), "a running search stops");
        drop(running);

        searches.stop("b");
        let late = searches.start("b");
        assert!(
            late.stopped(),
            "a stop that came first stops it as it starts"
        );
        drop(late);
        assert!(
            !searches.start("b").stopped(),
            "a remembered stop is spent once"
        );

        let first = searches.start("c");
        let second = searches.start("c");
        assert!(
            first.stopped(),
            "a newer search under the id stops the older"
        );
        assert!(!second.stopped());
        drop(first);
        searches.stop("c");
        assert!(
            second.stopped(),
            "the older one's end leaves the newer stoppable"
        );

        searches.stop("");
        assert!(
            !searches.start("").stopped(),
            "an empty id is never stopped"
        );
    }
}
