//! Media datagrams fanned out by stream.
//!
//! The link's datagram reader hands every `MEDIA` datagram here ([`ScreenRouter::route_many`],
//! a read's worth under one lock). A stream a task has attached gets them on its queue; a
//! stream nobody has attached yet keeps a short backlog, since a stream's first datagrams
//! usually beat its `Opened` on the control stream; a stream let go of is remembered for a
//! while ([`TOMBSTONES`]), so what is still in flight for it is dropped, not backlogged for an
//! attach that will not come. A stripe's media stream goes to its stream's task, which tells
//! the two apart.
//!
//! Ported from slopty's client `screen.rs` router (MIT, same authors), with its seeded loss
//! injection for tests.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use bytes::Bytes;
use onicode_desktop_media::wire::{MediaHeader, StreamId};
use onicode_desktop_proto::screen::Stripe;
use parking_lot::Mutex;
use tokio::sync::mpsc;

/// Datagrams buffered per attached stream before its task must drain them.
pub const STREAM_DEPTH: usize = 2048;
/// Datagrams kept for a stream nobody has attached yet.
pub const PENDING_DEPTH: usize = 512;
/// Streams that may wait unattached at once; past this many, the one heard from longest ago
/// lets its backlog go.
pub const PENDING_STREAMS: usize = 8;
/// Detached streams remembered. Stream ids count up per link, so the oldest can be forgotten
/// once this many newer ones have gone.
pub const TOMBSTONES: usize = 64;

/// Seed of the loss-injection sequence: a run at a given drop rate repeats exactly.
const LOSS_SEED: u64 = 0x2545_f491_4f6c_dd1d;

/// A datagram and when the link handed it over: the stream task may be busy when it lands, and
/// the reassembler's stall clock must not charge that wait to the link.
pub type Arrival = (Instant, Bytes);

/// Fans incoming datagrams out to per-stream queues. Cheap to clone; clones share the routes.
#[derive(Clone, Debug)]
pub struct ScreenRouter {
    inner: Arc<Mutex<Routes>>,
    /// Drop this many datagrams per thousand, from a fixed seed: tests only.
    drop_permille: Arc<AtomicU32>,
    lcg: Arc<AtomicU64>,
}

impl Default for ScreenRouter {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            drop_permille: Arc::new(AtomicU32::new(0)),
            lcg: Arc::new(AtomicU64::new(LOSS_SEED)),
        }
    }
}

#[derive(Debug, Default)]
struct Routes {
    attached: HashMap<StreamId, Route>,
    pending: HashMap<StreamId, Backlog>,
    /// Streams let go of, newest last ([`TOMBSTONES`] at most).
    detached: VecDeque<StreamId>,
}

/// An attached stream's queue, and the count of what did not fit in it.
#[derive(Debug)]
struct Route {
    tx: mpsc::Sender<Arrival>,
    dropped: Arc<AtomicU64>,
}

/// What a stream nobody has attached yet brought, and how much of it the bound let go.
#[derive(Debug, Default)]
struct Backlog {
    arrivals: VecDeque<Arrival>,
    dropped: u64,
}

/// An attached stream's datagrams, as the router hands them over.
#[derive(Debug)]
pub struct Attached {
    /// The queue.
    pub datagrams: mpsc::Receiver<Arrival>,
    /// Datagrams of the stream dropped before its task read them: a full queue, or a backlog
    /// past its bound before the stream attached.
    pub dropped: Arc<AtomicU64>,
}

impl Routes {
    fn deliver(&mut self, stream: StreamId, arrival: Arrival) {
        if let Some(route) = self.attached.get(&stream) {
            // A full queue means the stream task is behind: dropping is right, and counting it
            // tells that apart from a datagram the link lost.
            if let Err(mpsc::error::TrySendError::Full(_)) = route.tx.try_send(arrival) {
                route.dropped.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if self.detached.contains(&stream) {
            return;
        }
        if !self.pending.contains_key(&stream) && self.pending.len() >= PENDING_STREAMS {
            let stalest = self
                .pending
                .iter()
                .min_by_key(|(_, backlog)| backlog.arrivals.back().map(|(at, _)| *at))
                .map(|(id, _)| *id);
            if let Some(id) = stalest {
                self.pending.remove(&id);
            }
        }
        let backlog = self.pending.entry(stream).or_default();
        if backlog.arrivals.len() >= PENDING_DEPTH {
            backlog.arrivals.pop_front();
            backlog.dropped = backlog.dropped.saturating_add(1);
        }
        backlog.arrivals.push_back(arrival);
    }
}

impl ScreenRouter {
    /// An empty router.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A router that drops `drop_permille` of the datagrams handed to it, from a fixed seed.
    #[must_use]
    pub fn with_loss(drop_permille: u32) -> Self {
        let router = Self::default();
        router.set_loss(drop_permille);
        router
    }

    /// Drop this many datagrams per thousand from now on, and restart the sequence so a run at
    /// a given rate always sees the same losses. Tests only.
    pub fn set_loss(&self, drop_permille: u32) {
        let permille = drop_permille.min(1000);
        self.lcg.store(LOSS_SEED, Ordering::Relaxed);
        self.drop_permille.store(permille, Ordering::Relaxed);
        if permille > 0 {
            tracing::warn!(drop_permille = permille, "media loss injection is on");
        }
    }

    /// Whether loss injection says to drop this datagram (a 64-bit LCG).
    fn inject_loss(&self) -> bool {
        let permille = self.drop_permille.load(Ordering::Relaxed);
        if permille == 0 {
            return false;
        }
        let step = |x: u64| {
            x.wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407)
        };
        let prev = self
            .lcg
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |x| Some(step(x)))
            .unwrap_or(0);
        let roll = u32::try_from((step(prev) >> 33) % 1000).unwrap_or(0);
        roll < permille
    }

    /// Deliver one datagram that arrived at `now`.
    pub fn route(&self, datagram: Bytes, now: Instant) {
        self.route_many([datagram], now);
    }

    /// Deliver datagrams that arrived together at `now`, in order, under one lock.
    pub fn route_many(&self, datagrams: impl IntoIterator<Item = Bytes>, now: Instant) {
        let mut routes = None;
        for datagram in datagrams {
            if self.inject_loss() {
                continue;
            }
            let Some((header, _payload)) = MediaHeader::parse(&datagram) else {
                continue;
            };
            let (stream, _stripe) = Stripe::stream_of(StreamId(header.stream.get()));
            routes
                .get_or_insert_with(|| self.inner.lock())
                .deliver(stream, (now, datagram));
        }
    }

    /// Start receiving `stream`'s datagrams, backlog first.
    #[must_use]
    pub fn attach(&self, stream: StreamId) -> Attached {
        let (tx, datagrams) = mpsc::channel(STREAM_DEPTH);
        let mut routes = self.inner.lock();
        routes.detached.retain(|id| *id != stream);
        let mut dropped = 0_u64;
        if let Some(backlog) = routes.pending.remove(&stream) {
            dropped = backlog.dropped;
            for datagram in backlog.arrivals {
                if tx.try_send(datagram).is_err() {
                    dropped = dropped.saturating_add(1);
                }
            }
        }
        let dropped = Arc::new(AtomicU64::new(dropped));
        routes.attached.insert(
            stream,
            Route {
                tx,
                dropped: Arc::clone(&dropped),
            },
        );
        drop(routes);
        Attached { datagrams, dropped }
    }

    /// Stop routing `stream`: what is in flight for it from here on is dropped.
    pub fn detach(&self, stream: StreamId) {
        let mut routes = self.inner.lock();
        routes.attached.remove(&stream);
        routes.pending.remove(&stream);
        if !routes.detached.contains(&stream) {
            if routes.detached.len() >= TOMBSTONES {
                routes.detached.pop_front();
            }
            routes.detached.push_back(stream);
        }
    }

    /// Streams attached now.
    #[cfg(test)]
    pub(crate) fn attached(&self) -> usize {
        self.inner.lock().attached.len()
    }

    /// Forget every stream: the link went, and the next one numbers its streams anew.
    pub fn clear(&self) {
        let mut routes = self.inner.lock();
        routes.attached.clear();
        routes.pending.clear();
        routes.detached.clear();
    }
}

#[cfg(test)]
mod tests {
    use onicode_desktop_media::heartbeat_datagram;

    use super::*;

    /// A header-only datagram on `stream`, numbered `frame`.
    fn header_bytes(stream: u32, frame: u32) -> Bytes {
        heartbeat_datagram(StreamId(stream), frame, 0)
    }

    #[test]
    fn a_backlog_waits_for_its_attach_and_a_detached_stream_drops() {
        let router = ScreenRouter::new();
        let now = Instant::now();
        router.route(header_bytes(1, 0), now);
        router.route(header_bytes(1, 1), now);
        let mut attached = router.attach(StreamId(1));
        assert_eq!(attached.datagrams.try_recv().unwrap().1, header_bytes(1, 0));
        assert_eq!(attached.datagrams.try_recv().unwrap().1, header_bytes(1, 1));
        router.detach(StreamId(1));
        router.route(header_bytes(1, 2), now);
        let mut again = router.attach(StreamId(1));
        assert!(
            again.datagrams.try_recv().is_err(),
            "nothing kept while detached"
        );
    }

    #[test]
    fn a_lower_stripe_goes_to_its_stream() {
        let router = ScreenRouter::new();
        let mut attached = router.attach(StreamId(5));
        let lower = Stripe::media_of(StreamId(5), 1);
        router.route(header_bytes(lower.0, 0), Instant::now());
        assert_eq!(
            attached.datagrams.try_recv().unwrap().1,
            header_bytes(lower.0, 0)
        );
    }

    #[test]
    fn the_oldest_waiting_stream_goes_past_the_bound() {
        let router = ScreenRouter::new();
        let start = Instant::now();
        for stream in 1..=u32::try_from(PENDING_STREAMS).unwrap() + 1 {
            let at = start + std::time::Duration::from_millis(u64::from(stream));
            router.route(header_bytes(stream, 0), at);
        }
        let mut first = router.attach(StreamId(1));
        assert!(first.datagrams.try_recv().is_err(), "stream 1 was let go");
        let mut last = router.attach(StreamId(u32::try_from(PENDING_STREAMS).unwrap() + 1));
        assert!(last.datagrams.try_recv().is_ok());
    }

    #[test]
    fn injected_loss_repeats_from_its_seed() {
        let run = || {
            let router = ScreenRouter::with_loss(300);
            let mut attached = router.attach(StreamId(1));
            for frame in 0..1000 {
                router.route(header_bytes(1, frame), Instant::now());
            }
            let mut kept = Vec::new();
            while let Ok((_, d)) = attached.datagrams.try_recv() {
                kept.push(d);
            }
            kept
        };
        let first = run();
        assert_eq!(first, run(), "the same losses every run");
        assert!((600..800).contains(&first.len()), "{}", first.len());
    }
}
