//! The congestion controller every connection runs: one of noq's, its window held to twice the
//! path's measured bandwidth-delay product, and its bytes in flight readable beside the window.
//!
//! Ported from axolotty's `axquic` `congestion.rs`, itself slopty's `slopty-net` (MIT); the
//! measurements named below are slopty's.
//!
//! noq-proto 1.3.0's BBR3 grew its window without bound under slopty's traffic ("BBR3's window
//! under bursty video"). Video leaves in one burst per frame and the connection goes idle between
//! frames, and each restart from idle kept the bytes counted in BBR's ACK-aggregation interval
//! while moving its start to the present, so the window climbed at the video's rate. The
//! vendored noq clears the count (`vendor/noq-proto/SLOPTY.md`, patch 2), and unbounded its
//! window now stays near this bound ("noq's BBR3 against the draft"). The bound
//! stays for what that fix does not reach: an app-limited flow can stay in `Startup` for good,
//! pacing at the rate its initial window implies (2.77 × 38 400 B per millisecond, far above any
//! link), and then nothing but the bound holds the bytes in flight to the path. A burst would
//! land in the bottleneck's queue, where an echo written after it waits and nothing on this host
//! can reorder it.
//!
//! The bound is measured here, not read from BBR3, whose model is private and is what went
//! wrong: the fastest the peer has acknowledged data over a round trip in the last ten seconds,
//! times the smallest round trip in that time, twice over. That is the window BBR3 means to keep
//! (`cwnd_gain` 2), without the runaway allowance. Once any burst has crossed the bottleneck at
//! its rate, the bound is at least twice the product and costs no throughput; before that it
//! doubles each round trip, as a slow start does.
//!
//! The bound needs a controller that drains the queue now and then, as BBR3's `ProbeRTT` does,
//! so that a fresh round-trip minimum comes in. Cubic never drains a queue it has filled, and
//! after ten seconds of one the minimum is the queued round trip and the bound rises with it:
//! the rate-drop run filled a 100 ms queue under bounded Cubic as under unbounded BBR3.

use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use noq::congestion::{Bbr3, Controller, ControllerFactory, ControllerMetrics};
use noq::{Connection, PathId};
use noq_proto::RttEstimator;

/// Bandwidth-delay products the window may hold.
pub const CEILING_BDPS: f64 = 2.0;
/// The bound never goes below this many packets: the pacer releases ten at once, and a window
/// shorter than a burst would stall it on a path whose round trip rounds to nothing.
const FLOOR_PACKETS: u64 = 16;
/// A round-trip minimum or a delivery-rate maximum stands for this span and the one after it,
/// five to ten seconds: long enough to outlast a quiet spell between bursts, short enough to
/// follow a path that changed (BBR's own round-trip filter is 10 s).
const FILTER_HALF: Duration = Duration::from_secs(5);
/// The shortest span a delivery-rate sample covers. One round trip at least, so a clump of ACKs
/// released together does not read as a fast link.
const MIN_SAMPLE_SPAN: Duration = Duration::from_millis(1);

/// A noq controller with its window held to [`CEILING_BDPS`] of the measured path, and what noq
/// told it about the bytes in flight.
#[derive(Debug)]
pub struct Bounded {
    inner: Box<dyn Controller>,
    /// Bandwidth-delay products the window may hold; `None` leaves the inner window alone.
    ceiling: Option<f64>,
    /// Ack-eliciting bytes sent and neither acknowledged nor declared lost, as of the last ACK
    /// batch and every packet sent since.
    in_flight: u64,
    delivery: Delivery,
    min_rtt: Recent<Duration>,
    mtu: u16,
}

impl Bounded {
    fn new(inner: Box<dyn Controller>, ceiling: Option<f64>, mtu: u16) -> Self {
        Self {
            inner,
            ceiling,
            in_flight: 0,
            delivery: Delivery::default(),
            min_rtt: Recent::new(Ord::min),
            mtu,
        }
    }

    /// The bound on the window, once there is a round trip and a delivery rate to size it by.
    fn ceiling(&self) -> Option<u64> {
        let bdps = self.ceiling?;
        let rate = self.delivery.max.get()?;
        let min_rtt = self.min_rtt.get()?;
        let ceiling = (rate as f64 * min_rtt.as_secs_f64() * bdps) as u64;
        Some(ceiling.max(FLOOR_PACKETS.saturating_mul(u64::from(self.mtu))))
    }
}

impl Clone for Bounded {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone_box(),
            ceiling: self.ceiling,
            in_flight: self.in_flight,
            delivery: self.delivery.clone(),
            min_rtt: self.min_rtt,
            mtu: self.mtu,
        }
    }
}

impl Controller for Bounded {
    fn on_sent(&mut self, now: Instant, bytes: u64, largest_number: u64) {
        self.inner.on_sent(now, bytes, largest_number);
    }

    fn on_packet_sent(&mut self, now: Instant, bytes: u16, number: u64) {
        self.in_flight = self.in_flight.saturating_add(u64::from(bytes));
        self.inner.on_packet_sent(now, bytes, number);
    }

    fn on_cwnd_limited(&mut self) {
        // Only when the inner window was what bound: a send the ceiling held back says nothing
        // about the window BBR's probing reasons about.
        if self.in_flight.saturating_add(u64::from(self.mtu)) >= self.inner.window() {
            self.inner.on_cwnd_limited();
        }
    }

    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        number: u64,
        app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.min_rtt
            .update(now, now.saturating_duration_since(sent));
        self.delivery.acked = self.delivery.acked.saturating_add(bytes);
        self.inner
            .on_ack(now, sent, bytes, number, app_limited, rtt);
    }

    fn on_end_acks(
        &mut self,
        now: Instant,
        in_flight: u64,
        app_limited: bool,
        largest_packet_num_acked: Option<u64>,
    ) {
        self.in_flight = in_flight;
        if largest_packet_num_acked.is_some() {
            let span = self.min_rtt.get().unwrap_or_default().max(MIN_SAMPLE_SPAN);
            self.delivery.sample(now, span);
        }
        self.inner
            .on_end_acks(now, in_flight, app_limited, largest_packet_num_acked);
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        is_ecn: bool,
        lost_bytes: u64,
        largest_lost_number: u64,
    ) {
        self.inner.on_congestion_event(
            now,
            sent,
            is_persistent_congestion,
            is_ecn,
            lost_bytes,
            largest_lost_number,
        );
    }

    fn on_packet_lost(&mut self, lost_bytes: u16, number: u64, now: Instant) {
        self.inner.on_packet_lost(lost_bytes, number, now);
    }

    fn on_spurious_congestion_event(&mut self) {
        self.inner.on_spurious_congestion_event();
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.mtu = new_mtu;
        self.inner.on_mtu_update(new_mtu);
    }

    fn on_ack_frequency_update(
        &mut self,
        ack_eliciting_threshold: u64,
        requested_max_ack_delay: Duration,
    ) {
        self.inner
            .on_ack_frequency_update(ack_eliciting_threshold, requested_max_ack_delay);
    }

    fn window(&self) -> u64 {
        let window = self.inner.window();
        self.ceiling().map_or(window, |ceiling| window.min(ceiling))
    }

    fn metrics(&self) -> ControllerMetrics {
        let mut metrics = self.inner.metrics();
        metrics.congestion_window = self.window();
        metrics
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        self.inner.initial_window()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// The fastest the peer has acknowledged data over a span of at least a round trip.
#[derive(Clone, Debug)]
struct Delivery {
    /// Bytes acknowledged since the connection began.
    acked: u64,
    /// `(when, acked)` at recent ACK batches, oldest first, reaching back one span.
    history: VecDeque<(Instant, u64)>,
    /// Bytes per second.
    max: Recent<u64>,
}

impl Default for Delivery {
    fn default() -> Self {
        Self {
            acked: 0,
            history: VecDeque::new(),
            max: Recent::new(Ord::max),
        }
    }
}

impl Delivery {
    /// Close an ACK batch at `now`: measure from the latest batch at least `span` ago.
    fn sample(&mut self, now: Instant, span: Duration) {
        self.history.push_back((now, self.acked));
        let old_enough = |at: Instant| now.saturating_duration_since(at) >= span;
        while self.history.get(1).is_some_and(|&(at, _)| old_enough(at)) {
            self.history.pop_front();
        }
        let Some(&(from, acked)) = self.history.front() else {
            return;
        };
        if !old_enough(from) {
            return;
        }
        let elapsed = now.saturating_duration_since(from).as_nanos();
        let bytes = u128::from(self.acked.saturating_sub(acked));
        if let Some(rate) = bytes.saturating_mul(1_000_000_000).checked_div(elapsed) {
            self.max
                .update(now, u64::try_from(rate).unwrap_or(u64::MAX));
        }
    }
}

/// The best sample of the last five to ten seconds, by `better`: the best of this
/// [`FILTER_HALF`] and the one before it.
#[derive(Clone, Copy, Debug)]
struct Recent<T> {
    better: fn(T, T) -> T,
    current: Option<T>,
    previous: Option<T>,
    since: Option<Instant>,
}

impl<T: Copy> Recent<T> {
    const fn new(better: fn(T, T) -> T) -> Self {
        Self {
            better,
            current: None,
            previous: None,
            since: None,
        }
    }

    fn update(&mut self, now: Instant, sample: T) {
        if self
            .since
            .is_none_or(|since| now.saturating_duration_since(since) >= FILTER_HALF)
        {
            self.previous = self.current.take();
            self.since = Some(now);
        }
        self.current = Some(
            self.current
                .map_or(sample, |current| (self.better)(current, sample)),
        );
    }

    fn get(&self) -> Option<T> {
        match (self.current, self.previous) {
            (Some(a), Some(b)) => Some((self.better)(a, b)),
            (a, b) => a.or(b),
        }
    }
}

/// Builds a [`Bounded`] around whatever `inner` builds.
pub struct BoundedFactory {
    inner: Arc<dyn ControllerFactory + Send + Sync>,
    ceiling: Option<f64>,
}

impl BoundedFactory {
    /// Wrap `inner`, its window held to `ceiling` bandwidth-delay products; `None` only observes.
    #[must_use]
    pub fn new(inner: Arc<dyn ControllerFactory + Send + Sync>, ceiling: Option<f64>) -> Self {
        Self { inner, ceiling }
    }
}

impl std::fmt::Debug for BoundedFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedFactory")
            .field("ceiling", &self.ceiling)
            .finish_non_exhaustive()
    }
}

impl ControllerFactory for BoundedFactory {
    fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
        let inner = Arc::clone(&self.inner).build(now, current_mtu);
        Box::new(Bounded::new(inner, self.ceiling, current_mtu))
    }
}

/// The congestion picture of a connection's path at one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The congestion window in force, bytes.
    pub cwnd: u64,
    /// The inner controller's own window, before the ceiling.
    pub inner_cwnd: u64,
    /// Ack-eliciting bytes on the wire.
    pub in_flight: u64,
    /// The controller's own pacing rate in bytes per second; `None` for a window-paced one.
    pub pacing_rate: Option<u64>,
    /// The fastest delivery of the last ten seconds, bytes per second.
    pub delivery_rate: Option<u64>,
    /// The smallest round trip of the last ten seconds.
    pub min_rtt: Option<Duration>,
    /// BBR3's own `BBR.min_rtt`, which sizes its window: `None` under another controller.
    pub model_min_rtt: Option<Duration>,
    /// BBR3's state by the draft's name (`Startup`, `ProbeBW_UP`, `ProbeRTT`, …): `None` under
    /// another controller.
    pub model_state: Option<&'static str>,
}

/// The controller's picture of `conn`'s path, `None` once the path is gone.
///
/// This clones the controller under the connection's lock: a diagnostic, not a hot-path read.
#[must_use]
pub fn snapshot(conn: &Connection) -> Option<Snapshot> {
    let controller = conn.congestion_state(PathId::ZERO)?;
    let bounded = controller.into_any().downcast::<Bounded>().ok()?;
    let bounded_window = bounded.window();
    let Bounded {
        inner,
        in_flight,
        delivery,
        min_rtt,
        ..
    } = *bounded;
    let (cwnd, inner_cwnd) = (bounded_window, inner.window());
    let pacing_rate = inner.metrics().pacing_rate;
    let model = inner.into_any().downcast::<Bbr3>().ok();
    let model_min_rtt = model.as_ref().map(|bbr| bbr.min_rtt());
    let model_state = model.as_ref().map(|bbr| bbr.state_name());
    Some(Snapshot {
        cwnd,
        inner_cwnd,
        in_flight,
        pacing_rate,
        delivery_rate: delivery.max.get(),
        min_rtt: min_rtt.get(),
        model_min_rtt,
        model_state,
    })
}

/// The inner controller's whole state as its `Debug` prints it: every field of noq's model,
/// and for BBR3 every packet it still tracks. For tracing a run, not for a live connection.
#[must_use]
pub fn debug_state(conn: &Connection) -> Option<String> {
    let controller = conn.congestion_state(PathId::ZERO)?;
    let bounded = controller.into_any().downcast::<Bounded>().ok()?;
    Some(format!("{:?}", bounded.inner))
}
