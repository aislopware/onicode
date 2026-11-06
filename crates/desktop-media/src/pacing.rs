//! When a decoded frame reaches the screen, and what that cost.
//!
//! The rule is **present on arrival**: a frame goes up on the first paint after it comes out of
//! the decoder, and nothing is ever queued for a later one. A queue would buy smoother spacing at
//! the price of a whole frame of latency on every frame, which is the wrong trade for a remote
//! desktop — the source is a screen, not a film, and a late picture is worse than an unevenly
//! spaced one. So the only decision left is what to do when the decoder is ahead of the display:
//! [`Pacer::offer`] replaces the frame waiting to be painted rather than lining up behind it, and
//! counts the one it dropped.
//!
//! The [`Pacer`] is also the instrument. It keeps a ring of the last [`RING`] presented frames and
//! reports, over that window, how long each took from the arrival of the datagram that completed
//! it to the display showing it ([`Pacer::shown`], from the window's presentation report, a
//! refresh or more after the paint), how far apart those were, and how often a paint showed the
//! same picture again ([`PacingStats::repeats`]) or the display never saw one at all
//! ([`PacingStats::skipped`]). Those two counters are the double-present / skipped-present
//! pattern; on a steady source matched to the display both stay near zero.
//!
//! Every frame is also timed from its capture on the host to the glass. On any link the
//! capture time is placed on this process's clock by a [`ClockSync`](crate::ClockSync), which
//! estimates the host's clock from probes the stream echoes ([`FrameStamp::captured`]). Where
//! the host's clock is this process's too (loopback: the mach host time the host stamps
//! captures with and [`Instant`] both read mach absolute time), a [`ClockAnchor`] given to
//! [`Pacer::share_clock`] converts exactly instead, which is how the estimate is checked. Inputs
//! the caller marks with [`Pacer::input_sent`] and [`Pacer::input_visible`] are timed from their
//! send to the first frame shown that has them ([`Pacer::glass`]).
//!
//! Everything here is pure: the caller supplies the clock ([`Clock`]), so the policy is testable
//! without a window.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::{ClockAnchor, ClockEstimate};

/// Presented frames kept for the percentiles: four seconds at 60 fps.
pub const RING: usize = 240;

/// The clock a [`Pacer`] reads. Real in the app, fake in tests.
pub trait Clock: std::fmt::Debug {
    /// Now.
    fn now(&self) -> Instant;
}

/// [`Instant::now`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// How a decoded frame got here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameStamp {
    /// Presentation timestamp, from the host's capture clock, widened past the wire's 32-bit
    /// wrap by [`CaptureClock`]; also the frame's identity and its order.
    pub pts_us: u64,
    /// Which picture this is out of the decoder, counted from the first. The pacer reads the
    /// gaps: a frame that never reached it (the newest-only channel between the decoder and
    /// the element overwrote it) leaves a hole here and nowhere else.
    pub decode_seq: u64,
    /// When the datagram that completed the frame arrived.
    pub arrived: Instant,
    /// When the decoder handed the picture back.
    pub decoded: Instant,
    /// When the host captured it, on this process's clock, as the link's
    /// [`ClockSync`](crate::ClockSync) placed the capture timestamp; `None` until a clock probe
    /// has come back.
    pub captured: Option<Captured>,
}

/// When the host captured a frame, on this process's clock, and how far off that may be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Captured {
    /// The moment.
    pub at: Instant,
    /// The most [`Self::at`] may be off, either way: the bound of the estimate that placed it,
    /// as it stood then, and the clocks' drift between the estimate's anchor and the capture.
    /// A later estimate's bound says nothing of a frame this one timed.
    pub within: Duration,
}

impl Captured {
    /// Where `estimate` places a capture stamped `pts_us`.
    #[must_use]
    pub fn by(estimate: &ClockEstimate, pts_us: u64) -> Option<Self> {
        let at = estimate.anchor.captured(pts_us)?;
        // How far the capture is from the anchor on the host's clock, as `captured` reads the
        // wire's low 32 bits: microseconds times parts per million is nanoseconds over 1000.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the wire's low 32 bits by design"
        )]
        let apart_us = (pts_us as u32).wrapping_sub(estimate.anchor.host_us as u32);
        let apart_us = u64::from(apart_us.cast_signed().unsigned_abs());
        let drift_ns = apart_us.saturating_mul(u64::from(estimate.drift_ppm.unsigned_abs())) / 1000;
        Some(Self {
            at,
            within: estimate
                .bound
                .saturating_add(Duration::from_nanos(drift_ns)),
        })
    }
}

/// Widens the wire's 32-bit capture timestamp into a monotonic one.
///
/// `FrameInfo::capture_ts_us` carries the low 32 bits of the host's microsecond clock, which
/// wraps every ~71.6 minutes. Widening it with `u64::from` would make the first frame after a
/// wrap compare *older* than the last one before it, and every ordering test downstream — the
/// pacer's, the decoder's parked arrivals — would reject fresh pictures until the truncated
/// value climbed back past the pre-wrap one, up to another ~71 minutes of frozen screen. This
/// counts the wraps instead: a step backwards of more than half the range is a wrap forward, a
/// step forwards of more than half is a straggler from before one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CaptureClock {
    /// Raw value of the last sample.
    last: Option<u32>,
    /// Whole wraps counted so far, already multiplied out.
    epoch: u64,
}

/// One turn of the 32-bit capture clock.
const WRAP: u64 = 1_u64 << 32;
/// Half of it: the largest step that still reads as "the same turn".
const HALF_WRAP: u32 = 1_u32 << 31;

impl CaptureClock {
    /// A clock that has seen nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            epoch: 0,
        }
    }

    /// Widen one raw timestamp. Call once per frame, in arrival order.
    pub fn widen(&mut self, raw: u32) -> u64 {
        if let Some(last) = self.last {
            if raw < last && last.wrapping_sub(raw) > HALF_WRAP {
                // Forward across the boundary.
                self.epoch = self.epoch.saturating_add(WRAP);
            } else if raw > last && raw.wrapping_sub(last) > HALF_WRAP {
                // A straggler from before the last wrap; put it back in its own turn.
                self.epoch = self.epoch.saturating_sub(WRAP);
            }
        }
        self.last = Some(raw);
        self.epoch.saturating_add(u64::from(raw))
    }
}

/// p50, p95 and the worst of one ring of durations.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Spread {
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// Worst.
    pub max: Duration,
    /// Samples in the ring.
    pub count: usize,
}

impl Spread {
    fn of(ring: &VecDeque<Duration>) -> Self {
        let mut sorted: Vec<Duration> = ring.iter().copied().collect();
        sorted.sort_unstable();
        Self {
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            max: sorted.last().copied().unwrap_or_default(),
            count: sorted.len(),
        }
    }
}

/// The two end-to-end timings, over the last [`RING`] samples of each.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct GlassStats {
    /// Capture on the host → the display showing the frame; empty until the clocks are
    /// shared ([`Pacer::share_clock`]) or estimated ([`FrameStamp::captured`]).
    pub capture: Spread,
    /// Input sent → the display showing the first frame that has it.
    pub input: Spread,
    /// Inputs sent that no shown frame has had yet.
    pub inputs_pending: usize,
}

/// An input the caller sent, waiting for the frame that shows it to reach the display.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SentInput {
    seq: u64,
    sent: Instant,
    /// The first decoded frame that has it; `None` until one is found.
    visible_from: Option<u64>,
}

/// Inputs in flight kept at most; an older one is dropped unmeasured.
const INPUTS_IN_FLIGHT: usize = 64;

/// What to do with a frame the decoder just produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Offer {
    /// Put it on screen and ask for a redraw.
    Present,
    /// Not newer than what is already up; drop it.
    Drop,
}

/// One presented frame, as the ring remembers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sample {
    /// Arrival of the completing datagram → the display showing it.
    latency: Duration,
    /// Arrival → the decoder handing the picture back.
    decode: Duration,
    /// Gap to the previous presented frame; `None` for the first one.
    interval: Option<Duration>,
}

/// The span a stream's frames a second are counted over ([`Pacer::rate`]).
pub const PAINT_WINDOW: Duration = Duration::from_secs(1);

/// Paints and misses remembered for [`Pacer::rate`]: a second at 240 Hz and then some.
const RATE_KEPT: usize = 512;

/// A stream's frames a second, as every readout of it says it: pictures this client painted
/// over the last [`PAINT_WINDOW`], and apart from them the frames that missed the display in it.
///
/// A frame dropped as late or replaced before a paint is never painted, so it is never in
/// [`Self::painted`]; it is in [`Self::missed`], which a readout shows as its own flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PaintRate {
    /// Pictures painted in the last [`PAINT_WINDOW`]: the stream's frames a second.
    pub painted: u32,
    /// Frames in it that never reached the display: dropped as late, or replaced before a
    /// paint (`PacingStats::late` and `PacingStats::skipped` over the same span).
    pub missed: u32,
}

/// What the ring says about the last [`RING`] frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PacingStats {
    /// Frames put on screen, for the lifetime of the stream.
    pub presented: u64,
    /// Decoded frames the display never saw, because the decoder ran ahead of it: both the ones
    /// the pacer replaced while they waited for a paint and the ones the newest-only channel
    /// between the decoder and the element overwrote before the pacer was ever offered them
    /// (counted from the gaps in [`FrameStamp::decode_seq`], which is the only place they leave
    /// a trace).
    pub skipped: u64,
    /// Paints that showed the picture already up, because no new frame was ready.
    pub repeats: u64,
    /// Frames dropped as not newer than what was already up (reordering, a stale retransmit).
    pub late: u64,
    /// Median arrival → shown over the ring.
    pub latency_p50: Duration,
    /// 95th percentile of the same.
    pub latency_p95: Duration,
    /// Worst in the ring.
    pub latency_max: Duration,
    /// Median arrival → decoded over the ring: the part of the latency the decoder owns.
    pub decode_p50: Duration,
    /// Median gap between shown frames.
    pub interval_p50: Duration,
    /// Mean absolute deviation of that gap: the cadence's own jitter. A double- or
    /// skipped-present on an otherwise steady source shows up here before it shows up anywhere
    /// else.
    pub interval_jitter: Duration,
    /// Frames the ring holds.
    pub window: usize,
}

/// Decides when a decoded frame goes on screen and measures what happened.
#[derive(Debug)]
pub struct Pacer<C: Clock = SystemClock> {
    clock: C,
    /// Offered, installed, not yet painted.
    pending: Option<FrameStamp>,
    /// Presentation timestamp of the picture on screen.
    shown: Option<u64>,
    /// When the picture on screen reached the display.
    shown_at: Option<Instant>,
    /// Decode sequence of the newest frame offered, whether it was taken or dropped.
    last_seq: Option<u64>,
    ring: VecDeque<Sample>,
    stats: PacingStats,
    /// The host's clock against this one, when they are the same clock.
    anchor: Option<ClockAnchor>,
    /// Capture → shown, over the last [`RING`] frames shown.
    captures: VecDeque<Duration>,
    /// Inputs sent and not yet shown, oldest first.
    inputs: VecDeque<SentInput>,
    /// Input sent → shown, over the last [`RING`] inputs.
    input_ring: VecDeque<Duration>,
    /// When pictures were painted, oldest first, for [`Self::rate`].
    paints: VecDeque<Instant>,
    /// When frames missed the display, and how many at once, for [`Self::rate`].
    misses: VecDeque<(Instant, u64)>,
}

impl Default for Pacer<SystemClock> {
    fn default() -> Self {
        Self::new(SystemClock)
    }
}

impl<C: Clock> Pacer<C> {
    /// A pacer reading `clock`.
    #[must_use]
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            pending: None,
            shown: None,
            shown_at: None,
            last_seq: None,
            ring: VecDeque::with_capacity(RING),
            stats: PacingStats::default(),
            anchor: None,
            captures: VecDeque::new(),
            inputs: VecDeque::new(),
            input_ring: VecDeque::new(),
            paints: VecDeque::new(),
            misses: VecDeque::new(),
        }
    }

    /// Frames painted over the last [`PAINT_WINDOW`], and those that missed the display in it.
    #[must_use]
    pub fn rate(&self) -> PaintRate {
        let now = self.clock.now();
        let since = now.checked_sub(PAINT_WINDOW);
        let within = |at: Instant| since.is_none_or(|since| at > since);
        let painted = self
            .paints
            .iter()
            .rev()
            .take_while(|at| within(**at))
            .count();
        let missed: u64 = self
            .misses
            .iter()
            .rev()
            .take_while(|(at, _)| within(*at))
            .map(|(_, n)| *n)
            .sum();
        PaintRate {
            painted: u32::try_from(painted).unwrap_or(u32::MAX),
            missed: u32::try_from(missed).unwrap_or(u32::MAX),
        }
    }

    /// `frames` missed the display now.
    fn missed(&mut self, frames: u64) {
        if frames == 0 {
            return;
        }
        if self.misses.len() >= RATE_KEPT {
            self.misses.pop_front();
        }
        self.misses.push_back((self.clock.now(), frames));
    }

    /// The host's capture clock is this process's clock too (loopback), read together at
    /// `anchor`: from here on every frame shown is timed from its capture through it, exactly,
    /// rather than through the estimate its stamp carries.
    pub const fn share_clock(&mut self, anchor: ClockAnchor) {
        self.anchor = Some(anchor);
    }

    /// Input `seq` left this client `at`. Sequence numbers count up.
    pub fn input_sent(&mut self, seq: u64, at: Instant) {
        if self.inputs.len() >= INPUTS_IN_FLIGHT {
            self.inputs.pop_front();
        }
        self.inputs.push_back(SentInput {
            seq,
            sent: at,
            visible_from: None,
        });
    }

    /// The decoded frame stamped `pts_us` is the first found to show every input up to `seq`:
    /// they are timed to the first frame at least that new that reaches the display.
    pub fn input_visible(&mut self, seq: u64, pts_us: u64) {
        for input in self
            .inputs
            .iter_mut()
            .filter(|i| i.seq <= seq && i.visible_from.is_none())
        {
            input.visible_from = Some(pts_us);
        }
    }

    /// Capture → glass and input → glass over their rings.
    #[must_use]
    pub fn glass(&self) -> GlassStats {
        GlassStats {
            capture: Spread::of(&self.captures),
            input: Spread::of(&self.input_ring),
            inputs_pending: self.inputs.len(),
        }
    }

    /// A decoded frame is available. `Present` means install it now and ask for a redraw;
    /// nothing is ever held back for a later paint.
    ///
    /// Frames the decoder produced but that never got here — the channel from the decoder keeps
    /// only the newest, so a burst finishing between two paints leaves only its last member —
    /// are counted from the gap in [`FrameStamp::decode_seq`]. They are skips like any other:
    /// the display never saw them.
    pub fn offer(&mut self, stamp: FrameStamp) -> Offer {
        if let Some(last) = self.last_seq {
            let lost_in_transit = stamp.decode_seq.saturating_sub(last).saturating_sub(1);
            self.stats.skipped = self.stats.skipped.saturating_add(lost_in_transit);
            self.missed(lost_in_transit);
        }
        // Even a frame that is dropped as late advances the sequence, or the frames the channel
        // swallowed behind it would be counted twice.
        self.last_seq = Some(stamp.decode_seq.max(self.last_seq.unwrap_or(0)));
        let newest = self.pending.map_or(self.shown, |p| Some(p.pts_us));
        if newest.is_some_and(|last| stamp.pts_us <= last) {
            self.stats.late = self.stats.late.saturating_add(1);
            self.missed(1);
            return Offer::Drop;
        }
        if self.pending.is_some() {
            // The one it replaces was installed but never painted.
            self.stats.skipped = self.stats.skipped.saturating_add(1);
            self.missed(1);
        }
        self.pending = Some(stamp);
        Offer::Present
    }

    /// The element painted: the frame this paint put up, to be passed to [`Self::shown`] when
    /// the display shows it, or `None` when the paint showed the picture already up (a repeat).
    /// Call once per paint.
    pub fn painted(&mut self) -> Option<FrameStamp> {
        let Some(stamp) = self.pending.take() else {
            self.stats.repeats = self.stats.repeats.saturating_add(1);
            return None;
        };
        self.shown = Some(stamp.pts_us);
        if self.paints.len() >= RATE_KEPT {
            self.paints.pop_front();
        }
        self.paints.push_back(self.clock.now());
        Some(stamp)
    }

    /// The frame `stamp` a paint put up reached the display `at`: the ring's clocks stop here,
    /// not at the paint, which runs a refresh or more earlier.
    pub fn shown(&mut self, stamp: FrameStamp, at: Instant) {
        let sample = Sample {
            latency: at.saturating_duration_since(stamp.arrived),
            decode: stamp.decoded.saturating_duration_since(stamp.arrived),
            interval: self.shown_at.map(|t| at.saturating_duration_since(t)),
        };
        if self.ring.len() >= RING {
            self.ring.pop_front();
        }
        self.ring.push_back(sample);
        self.shown_at = Some(at);
        self.stats.presented = self.stats.presented.saturating_add(1);
        let captured = self.anchor.map_or_else(
            || stamp.captured.map(|c| c.at),
            |a| a.captured(stamp.pts_us),
        );
        if let Some(captured) = captured {
            push_ring(&mut self.captures, at.saturating_duration_since(captured));
        }
        while let Some(input) = self.inputs.front().copied() {
            let Some(from) = input.visible_from else {
                break;
            };
            if from > stamp.pts_us {
                break;
            }
            self.inputs.pop_front();
            push_ring(
                &mut self.input_ring,
                at.saturating_duration_since(input.sent),
            );
        }
    }

    /// A picture put up never reached the display: a newer one replaced it first. It counts
    /// as skipped, as a picture replaced before a paint does.
    pub const fn unshown(&mut self) {
        self.stats.skipped = self.stats.skipped.saturating_add(1);
    }

    /// Age of the picture on screen: how long ago it reached the display.
    #[must_use]
    pub fn age(&self) -> Option<Duration> {
        self.shown_at
            .map(|t| self.clock.now().saturating_duration_since(t))
    }

    /// The counters plus the ring's percentiles.
    #[must_use]
    pub fn stats(&self) -> PacingStats {
        let mut latency: Vec<Duration> = self.ring.iter().map(|s| s.latency).collect();
        let mut decode: Vec<Duration> = self.ring.iter().map(|s| s.decode).collect();
        let mut interval: Vec<Duration> = self.ring.iter().filter_map(|s| s.interval).collect();
        latency.sort_unstable();
        decode.sort_unstable();
        interval.sort_unstable();
        let interval_p50 = percentile(&interval, 50);
        PacingStats {
            latency_p50: percentile(&latency, 50),
            latency_p95: percentile(&latency, 95),
            latency_max: latency.last().copied().unwrap_or_default(),
            decode_p50: percentile(&decode, 50),
            interval_p50,
            interval_jitter: mean_deviation(&interval, interval_p50),
            window: self.ring.len(),
            ..self.stats
        }
    }
}

/// Push onto a ring of at most [`RING`].
fn push_ring(ring: &mut VecDeque<Duration>, sample: Duration) {
    if ring.len() >= RING {
        ring.pop_front();
    }
    ring.push_back(sample);
}

/// The `p`th percentile of a sorted slice by nearest rank, or zero when it is empty.
///
/// The smallest value at least `p` % of the slice is no greater than. The one definition every
/// readout uses (the frame probe, the keystroke timings, the stream's pacing), so a p95 means
/// the same thing wherever it is printed.
#[must_use]
pub fn percentile(sorted: &[Duration], p: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = p.saturating_mul(sorted.len()).div_ceil(100).max(1);
    sorted
        .get(rank.saturating_sub(1))
        .copied()
        .unwrap_or_default()
}

/// Mean absolute deviation of `values` from `centre`.
fn mean_deviation(values: &[Duration], centre: Duration) -> Duration {
    let count = u32::try_from(values.len()).unwrap_or(u32::MAX);
    if count == 0 {
        return Duration::ZERO;
    }
    let total = values.iter().fold(Duration::ZERO, |acc, &v| {
        acc.saturating_add(v.abs_diff(centre))
    });
    total.checked_div(count).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A clock the test moves by hand.
    #[derive(Debug)]
    struct FakeClock {
        epoch: Instant,
        offset: Cell<Duration>,
        /// One per `stamp` call: every stamp stands for one decoder callback.
        seq: Cell<u64>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                epoch: Instant::now(),
                offset: Cell::new(Duration::ZERO),
                seq: Cell::new(0),
            }
        }

        /// The decode sequence of the next callback.
        fn next_seq(&self) -> u64 {
            let seq = self.seq.get();
            self.seq.set(seq.saturating_add(1));
            seq
        }

        /// Skip `count` callbacks, as the newest-only channel does when it overwrites them.
        fn swallow(&self, count: u64) {
            self.seq.set(self.seq.get().saturating_add(count));
        }

        fn advance(&self, by: Duration) {
            self.offset.set(self.offset.get().saturating_add(by));
        }

        fn now_instant(&self) -> Instant {
            self.at(self.offset.get())
        }

        fn at(&self, offset: Duration) -> Instant {
            self.epoch.checked_add(offset).expect("instant in range")
        }
    }

    impl Clock for &FakeClock {
        fn now(&self) -> Instant {
            self.at(self.offset.get())
        }
    }

    const FRAME: Duration = Duration::from_micros(16_667);
    const MS: Duration = Duration::from_millis(1);

    /// One arrival, one decode, one paint, `at` after the clock's epoch.
    fn stamp(clock: &FakeClock, index: u32, arrived: Duration, decode: Duration) -> FrameStamp {
        FrameStamp {
            pts_us: u64::from(index).saturating_mul(16_667),
            decode_seq: clock.next_seq(),
            arrived: clock.at(arrived),
            decoded: clock.at(arrived.saturating_add(decode)),
            captured: None,
        }
    }

    /// A paint that the display shows at once, as the tests' clock reads it.
    fn present(pacer: &mut Pacer<&FakeClock>, clock: &FakeClock) {
        if let Some(stamp) = pacer.painted() {
            pacer.shown(stamp, clock.now_instant());
        }
    }

    /// The clocks stop when the display shows the frame, not at the paint: a frame painted
    /// 4 ms after it arrived and shown a refresh later is a refresh and 4 ms late, and its
    /// picture ages from the glass.
    #[test]
    fn a_frame_is_timed_at_the_glass_not_the_paint() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        let arrived = FRAME;
        clock.advance(arrived.saturating_add(4 * MS));
        assert_eq!(pacer.offer(stamp(&clock, 0, arrived, MS)), Offer::Present);
        let painted = pacer.painted().expect("a new frame went up");
        assert_eq!(pacer.stats().presented, 0, "painted, not yet shown");
        // Once painted it is the picture up, shown or not: the same frame again is late.
        assert_eq!(pacer.offer(stamp(&clock, 0, arrived, MS)), Offer::Drop);
        clock.advance(FRAME);
        pacer.shown(painted, clock.now_instant());
        let stats = pacer.stats();
        assert_eq!(stats.presented, 1);
        assert_eq!(stats.latency_max, FRAME.saturating_add(4 * MS));
        assert_eq!(pacer.age(), Some(Duration::ZERO));
    }

    /// Nearest rank: the median of an even count is the lower middle, a p99 of 100 samples is
    /// the 99th and not the largest, and a single sample is every percentile.
    /// A picture put up and then replaced before the display showed it is skipped, and has no
    /// latency: only what reached the glass is timed.
    #[test]
    fn a_picture_replaced_before_the_glass_is_skipped() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        clock.advance(FRAME);
        assert_eq!(
            pacer.offer(stamp(&clock, 0, Duration::ZERO, MS)),
            Offer::Present
        );
        let _replaced = pacer.painted().expect("put up");
        assert_eq!(pacer.offer(stamp(&clock, 1, FRAME, MS)), Offer::Present);
        let shown = pacer.painted().expect("put up");
        pacer.unshown();
        clock.advance(FRAME);
        pacer.shown(shown, clock.now_instant());
        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.window), (1, 1, 1));
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<Duration> = (1..=100).map(|i| i * MS).collect();
        assert_eq!(percentile(&v, 50), 50 * MS);
        assert_eq!(percentile(&v, 95), 95 * MS);
        assert_eq!(percentile(&v, 99), 99 * MS);
        assert_eq!(percentile(&v, 100), 100 * MS);
        assert_eq!(percentile(&v[..4], 50), 2 * MS, "the lower middle of four");
        assert_eq!(percentile(&v[..1], 99), MS);
        assert_eq!(
            percentile(&v[..1], 0),
            MS,
            "rank 0 is the first, not before it"
        );
        assert_eq!(percentile(&[], 50), Duration::ZERO);
    }

    /// A steady 60 fps source painted at 60 Hz: every frame goes up on the paint that follows
    /// it, one interval apart, and nothing is skipped or repeated.
    #[test]
    fn a_steady_source_presents_every_frame_once() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        for i in 0..120 {
            let arrived = FRAME.saturating_mul(i);
            clock.advance(FRAME);
            assert_eq!(
                pacer.offer(stamp(&clock, i, arrived, 2 * MS)),
                Offer::Present
            );
            present(&mut pacer, &clock);
        }
        let stats = pacer.stats();
        assert_eq!(
            (stats.presented, stats.skipped, stats.repeats, stats.late),
            (120, 0, 0, 0)
        );
        assert_eq!(stats.window, RING.min(120));
        assert_eq!(stats.interval_p50, FRAME);
        assert_eq!(stats.interval_jitter, Duration::ZERO);
        // Arrival → present is the decode plus the wait for the paint; no extra frame of it.
        assert!(
            stats.latency_p95 < FRAME.saturating_add(2 * MS),
            "{stats:?}"
        );
        assert!(stats.latency_p95 > Duration::ZERO && stats.latency_p95 <= stats.latency_max);
        // The picture's age runs from the paint that put it up.
        assert_eq!(pacer.age(), Some(Duration::ZERO));
        clock.advance(MS);
        assert_eq!(pacer.age(), Some(MS));
    }

    /// Two frames between paints: the newer one is shown, the older is counted skipped and never
    /// queued for the next paint (which is what an extra frame of buffering would look like).
    #[test]
    fn a_frame_decoded_between_paints_replaces_the_one_waiting() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        clock.advance(FRAME);
        assert_eq!(
            pacer.offer(stamp(&clock, 0, Duration::ZERO, MS)),
            Offer::Present
        );
        present(&mut pacer, &clock);

        // Two decodes land before the next paint.
        assert_eq!(pacer.offer(stamp(&clock, 1, FRAME, MS)), Offer::Present);
        assert_eq!(
            pacer.offer(stamp(&clock, 2, FRAME + 8 * MS, MS)),
            Offer::Present
        );
        clock.advance(FRAME);
        present(&mut pacer, &clock);

        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.repeats), (2, 1, 0));
        // Two paints, two samples: the frame that was replaced never reached the ring.
        assert_eq!(stats.window, 2);
        // The second paint showed frame 2, so its latency is measured from *its* arrival — a
        // frame interval less eight milliseconds, which is under frame 0's whole interval.
        assert_eq!(stats.latency_max, FRAME);
        // The median of the two by nearest rank is the lower one.
        assert_eq!(stats.latency_p50, FRAME.saturating_sub(8 * MS));
        // Nothing is left over: the next paint has no new frame, so it repeats.
        present(&mut pacer, &clock);
        assert_eq!(pacer.stats().repeats, 1);
    }

    /// A source slower than the display repeats pictures instead of holding paints back, and a
    /// frame that is not newer than what is up is dropped rather than shown out of order.
    #[test]
    fn a_slow_source_repeats_and_a_late_frame_is_dropped() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        clock.advance(FRAME);
        pacer.offer(stamp(&clock, 4, Duration::ZERO, MS));
        present(&mut pacer, &clock);
        // Three paints with nothing new.
        for _ in 0..3 {
            clock.advance(FRAME);
            present(&mut pacer, &clock);
        }
        // A straggler from before the picture on screen.
        assert_eq!(pacer.offer(stamp(&clock, 3, 4 * FRAME, MS)), Offer::Drop);
        // …and the same frame again.
        assert_eq!(pacer.offer(stamp(&clock, 4, 4 * FRAME, MS)), Offer::Drop);
        clock.advance(FRAME);
        present(&mut pacer, &clock);

        let stats = pacer.stats();
        assert_eq!(
            (stats.presented, stats.skipped, stats.repeats, stats.late),
            (1, 0, 4, 2)
        );
        assert_eq!(
            stats.interval_p50,
            Duration::ZERO,
            "one frame has no interval"
        );
    }

    /// An uneven cadence (one paint missed in eight) shows up as interval jitter, which is what
    /// the overlay reads.
    #[test]
    fn a_missed_paint_shows_as_interval_jitter() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        let mut arrived = Duration::ZERO;
        for i in 0..64 {
            let step = if i % 8 == 7 { 2 * FRAME } else { FRAME };
            arrived = arrived.saturating_add(step);
            clock.advance(step);
            pacer.offer(stamp(&clock, i, arrived, MS));
            present(&mut pacer, &clock);
        }
        let stats = pacer.stats();
        assert_eq!(stats.interval_p50, FRAME);
        // One gap in eight is a frame long, so the mean deviation is about an eighth of one.
        let eighth = FRAME.checked_div(8).expect("nonzero");
        assert!(
            stats.interval_jitter > eighth.checked_div(2).expect("nonzero")
                && stats.interval_jitter < eighth.saturating_mul(2),
            "{:?} is not about {eighth:?}",
            stats.interval_jitter
        );
    }

    /// Pictures the newest-only channel swallowed before the element looked are skips too:
    /// the pacer reads them out of the gap in the decode sequence, which is the only trace
    /// they leave.
    #[test]
    fn frames_the_channel_overwrote_are_counted_as_skips() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        clock.advance(FRAME);
        assert_eq!(
            pacer.offer(stamp(&clock, 0, Duration::ZERO, MS)),
            Offer::Present
        );
        present(&mut pacer, &clock);

        // Three decoder callbacks finished between paints; the channel kept only the last.
        clock.swallow(3);
        clock.advance(FRAME);
        assert_eq!(pacer.offer(stamp(&clock, 4, 3 * FRAME, MS)), Offer::Present);
        present(&mut pacer, &clock);

        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.repeats), (2, 3, 0));
        assert_eq!(
            stats.window, 2,
            "only the two that were painted are in the ring"
        );

        // A frame dropped as late still advances the sequence, so the ones swallowed behind it
        // are not counted a second time by the frame after.
        clock.swallow(2);
        assert_eq!(pacer.offer(stamp(&clock, 1, 4 * FRAME, MS)), Offer::Drop);
        clock.advance(FRAME);
        assert_eq!(pacer.offer(stamp(&clock, 5, 4 * FRAME, MS)), Offer::Present);
        present(&mut pacer, &clock);
        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.late), (3, 5, 1));
    }

    /// The host's capture clock is 32 bits of microseconds and wraps every ~71.6 minutes. The
    /// widened stamp keeps climbing through it, so the pacer never mistakes the first frame of
    /// the new turn for a straggler and freeze the screen until the raw value catches up.
    #[test]
    fn the_capture_clock_survives_its_wrap() {
        let mut clock = CaptureClock::new();
        // A few frames before the boundary, 16 667 µs apart.
        let last_before = u32::MAX - 10_000;
        let before = clock.widen(last_before);
        assert_eq!(before, u64::from(last_before), "no wrap seen yet");

        // The next frame's raw stamp has wrapped past zero.
        let first_after = last_before.wrapping_add(16_667);
        assert!(first_after < last_before, "the raw stamp went backwards");
        let after = clock.widen(first_after);
        assert!(after > before, "{after} is not newer than {before}");
        assert_eq!(after.saturating_sub(before), 16_667);

        // It keeps counting from there, and a second wrap works the same.
        let mut raw = first_after;
        let mut widened = after;
        for _ in 0..600_000 {
            raw = raw.wrapping_add(16_667);
            let next = clock.widen(raw);
            assert_eq!(next.saturating_sub(widened), 16_667, "raw {raw}");
            widened = next;
        }
        assert!(
            widened > WRAP.saturating_mul(2),
            "two turns of the clock: {widened}"
        );

        // A straggler from before the boundary lands back in its own turn, below the frames
        // after it, which is what makes the pacer drop it as late rather than jump forward.
        let straggler = clock.widen(raw.wrapping_sub(50_000));
        assert!(
            straggler < widened,
            "{straggler} should be older than {widened}"
        );

        // The pacer built on it never freezes across the wrap.
        let fake = FakeClock::new();
        let mut pacer = Pacer::new(&fake);
        let mut clock = CaptureClock::new();
        let mut raw = u32::MAX - 33_334;
        for _ in 0..8 {
            fake.advance(FRAME);
            let pts = clock.widen(raw);
            let s = FrameStamp {
                pts_us: pts,
                decode_seq: fake.next_seq(),
                arrived: fake.now_instant(),
                decoded: fake.now_instant(),
                captured: None,
            };
            assert_eq!(pacer.offer(s), Offer::Present, "raw {raw} was refused");
            present(&mut pacer, &fake);
            raw = raw.wrapping_add(16_667);
        }
        assert_eq!(pacer.stats().late, 0);
        assert_eq!(pacer.stats().presented, 8);
    }

    /// The ring forgets: only the last `RING` frames are in the percentiles.
    #[test]
    fn a_step_back_within_a_turn_stays_in_it_and_half_a_turn_is_not_a_wrap() {
        let mut clock = CaptureClock::new();
        // Across the boundary once, so there is a turn to fall out of.
        clock.widen(u32::MAX - 10);
        assert_eq!(clock.widen(5), WRAP + 5);
        // A small step back (a reordered frame) is not a straggler from the previous turn.
        assert_eq!(clock.widen(1_000), WRAP + 1_000);
        assert_eq!(clock.widen(900), WRAP + 900);
        // Exactly half a turn back is the largest step that is still a step, not a wrap.
        let mut clock = CaptureClock::new();
        clock.widen(HALF_WRAP);
        assert_eq!(clock.widen(0), 0);
        // And exactly half a turn forward, in a later turn, is not a straggler either.
        let mut clock = CaptureClock::new();
        clock.widen(u32::MAX - 10);
        clock.widen(5);
        assert_eq!(clock.widen(HALF_WRAP + 5), WRAP + u64::from(HALF_WRAP) + 5);
    }

    #[test]
    fn the_ring_keeps_the_last_frames_only() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        for i in 0..(u32::try_from(RING).expect("small") + 40) {
            let arrived = FRAME.saturating_mul(i);
            // The first frames are slow, the last ones fast.
            let decode = if i < 100 { 50 * MS } else { MS };
            clock.advance(FRAME);
            pacer.offer(stamp(&clock, i, arrived, decode));
            present(&mut pacer, &clock);
        }
        let stats = pacer.stats();
        assert_eq!(stats.window, RING);
        assert_eq!(stats.presented, u64::try_from(RING).expect("small") + 40);
        assert_eq!(
            stats.decode_p50, MS,
            "the slow start has fallen out of the ring"
        );
    }

    /// A stream's frames a second are the pictures painted over the last second and nothing
    /// else: a frame dropped as late or replaced before a paint counts apart, a paint that
    /// showed the picture already up counts not at all, and a still stream reads nought a
    /// second after its last picture.
    #[test]
    fn the_rate_is_the_pictures_painted_in_the_last_second_and_misses_apart() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        assert_eq!(pacer.rate(), PaintRate::default());
        for index in 1..=120 {
            clock.advance(FRAME);
            let s = stamp(&clock, index, clock.offset.get(), MS);
            assert_eq!(pacer.offer(s), Offer::Present);
            present(&mut pacer, &clock);
        }
        assert_eq!(
            pacer.rate(),
            PaintRate {
                painted: 60,
                missed: 0
            },
            "60 Hz for two seconds"
        );

        let late = stamp(&clock, 100, clock.offset.get(), MS);
        assert_eq!(pacer.offer(late), Offer::Drop);
        let replaced = stamp(&clock, 121, clock.offset.get(), MS);
        assert_eq!(pacer.offer(replaced), Offer::Present);
        let newer = stamp(&clock, 122, clock.offset.get(), MS);
        assert_eq!(pacer.offer(newer), Offer::Present);
        present(&mut pacer, &clock);
        present(&mut pacer, &clock);
        assert_eq!(
            pacer.rate(),
            PaintRate {
                painted: 61,
                missed: 2
            },
            "one more painted, a repeat not counted, the late and the replaced apart"
        );

        clock.advance(PAINT_WINDOW);
        assert_eq!(
            pacer.rate(),
            PaintRate::default(),
            "still for a second: nothing painted"
        );
    }

    /// A capture placed by an estimate says how far off it may be: the estimate's bound as it
    /// stood, widened by the clocks' drift over the way from the anchor, either side of it.
    #[test]
    fn a_placed_capture_carries_its_estimates_bound_and_the_drift_since() {
        let clock = FakeClock::new();
        let estimate = ClockEstimate {
            anchor: ClockAnchor {
                at: clock.at(Duration::from_secs(10)),
                host_us: 50_000_000,
            },
            bound: 300 * Duration::from_micros(1),
            rtt: MS,
            drift_ppm: -40,
        };
        let at_anchor = Captured::by(&estimate, 50_000_000).unwrap();
        assert_eq!(
            at_anchor,
            Captured {
                at: estimate.anchor.at,
                within: estimate.bound
            }
        );
        // 2 s on at 40 ppm is 80 µs more, after the anchor or before it.
        let later = Captured::by(&estimate, 52_000_000).unwrap();
        assert_eq!(later.at, clock.at(Duration::from_secs(12)));
        assert_eq!(later.within, Duration::from_micros(380));
        let earlier = Captured::by(&estimate, 48_000_000).unwrap();
        assert_eq!(earlier.at, clock.at(Duration::from_secs(8)));
        assert_eq!(earlier.within, Duration::from_micros(380));
    }

    /// With the clocks shared, a frame is timed from its capture too: captured 5 ms before it
    /// arrived and shown a refresh after that, it took 5 ms and a refresh from capture to glass.
    /// Without an anchor it is timed from the capture its stamp's estimate places, and without
    /// either there is no capture timing at all, never a guess. The shared clock wins over an
    /// estimate, since it is exact.
    #[test]
    fn a_shared_clock_times_frames_from_their_capture() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        // The host's clock reads 7 000 000 µs at the fake clock's epoch.
        let anchor = ClockAnchor {
            at: clock.at(Duration::ZERO),
            host_us: 7_000_000,
        };
        let arrived = 20 * MS;
        let s = FrameStamp {
            pts_us: 7_000_000 + 15_000,
            decode_seq: clock.next_seq(),
            arrived: clock.at(arrived),
            decoded: clock.at(arrived + MS),
            captured: None,
        };
        clock.advance(arrived + FRAME);
        pacer.offer(s);
        present(&mut pacer, &clock);
        assert_eq!(
            pacer.glass().capture.count,
            0,
            "no anchor, no capture timing"
        );

        let mut shared = Pacer::new(&clock);
        shared.share_clock(anchor);
        // An estimate 2 ms off, which the exact anchor overrides.
        let estimated_at = Some(Captured {
            at: clock.at(17 * MS),
            within: MS,
        });
        shared.offer(FrameStamp {
            captured: estimated_at,
            ..s
        });
        present(&mut shared, &clock);
        let glass = shared.glass();
        assert_eq!(glass.capture.count, 1);
        assert_eq!(glass.capture.max, 5 * MS + FRAME);

        let mut estimated = Pacer::new(&clock);
        estimated.offer(FrameStamp {
            captured: estimated_at,
            ..s
        });
        present(&mut estimated, &clock);
        assert_eq!(
            estimated.glass().capture.max,
            3 * MS + FRAME,
            "from the estimate"
        );
    }

    /// An input is timed from its send to the first frame shown that has it. The frame found to
    /// show it may be replaced before a paint; the newer frame shown instead has it too, and
    /// that is where its clock stops. An input no frame has shown yet stays pending.
    #[test]
    fn an_input_is_timed_to_the_first_frame_shown_that_has_it() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        pacer.input_sent(1, clock.now_instant());
        pacer.input_sent(2, clock.now_instant() + 2 * MS);
        clock.advance(10 * MS);
        // Frame 1 comes back without it.
        pacer.offer(stamp(&clock, 1, 10 * MS, MS));
        present(&mut pacer, &clock);
        assert_eq!(pacer.glass().input.count, 0);
        // Frame 2 shows input 1 but is replaced by frame 3 before the paint.
        pacer.offer(stamp(&clock, 2, 20 * MS, MS));
        pacer.input_visible(1, 2 * 16_667);
        pacer.offer(stamp(&clock, 3, 22 * MS, MS));
        clock.advance(20 * MS);
        present(&mut pacer, &clock);
        let glass = pacer.glass();
        assert_eq!(glass.input.count, 1);
        assert_eq!(
            glass.input.max,
            30 * MS,
            "sent at 0, shown at 30 ms with frame 3"
        );
        assert_eq!(glass.inputs_pending, 1, "input 2 has not been seen");
        pacer.input_visible(2, 4 * 16_667);
        pacer.offer(stamp(&clock, 4, 40 * MS, MS));
        clock.advance(FRAME);
        present(&mut pacer, &clock);
        let glass = pacer.glass();
        assert_eq!((glass.input.count, glass.inputs_pending), (2, 0));
        assert_eq!(glass.input.p50, 30 * MS);
    }
}
