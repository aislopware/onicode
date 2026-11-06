//! The host's capture clock on the client's: one moment read on both ([`ClockAnchor`]), and
//! the estimate of it from the clock probes a stream echoes ([`ClockSync`]), NTP's way, on any
//! link (video decisions, "Capture to glass on any link").
//!
//! Pure: the caller hands in every time, so the estimator is tested against a simulated link
//! and fuzzed with the readings a peer may send.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One moment read on both the host's capture clock and this process's.
///
/// Exact where they are the same clock: the host time clock capture stamps read
/// and [`Instant`] are both mach absolute time on one machine, so on loopback a capture
/// timestamp converts exactly. On any other link a [`ClockSync`] estimates one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClockAnchor {
    /// The moment on this process's clock.
    pub at: Instant,
    /// The same moment on the host's capture clock, microseconds.
    pub host_us: u64,
}

impl ClockAnchor {
    /// When a frame stamped `pts_us` was captured, on this process's clock. Only the low 32 bits
    /// of the stamp are the wire's, so the stamp is read as the nearest moment to the anchor with
    /// those bits: within 35 minutes of it either way.
    #[must_use]
    pub fn captured(&self, pts_us: u64) -> Option<Instant> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the wire's low 32 bits by design"
        )]
        let (pts, anchor) = (pts_us as u32, self.host_us as u32);
        let ahead = pts.wrapping_sub(anchor).cast_signed();
        let by = Duration::from_micros(u64::from(ahead.unsigned_abs()));
        if ahead >= 0 {
            self.at.checked_add(by)
        } else {
            self.at.checked_sub(by)
        }
    }
}

/// How much shorter than the host's hold inside it a round trip may read: each end reads
/// whole microseconds, truncated.
const STAMP_SLACK_US: u64 = 2;

/// How long the probes a [`ClockSync`] fits its estimate to are kept.
///
/// Long enough to hold probes that met an empty path on a busy link, and to see two clocks
/// drift apart: quartz on two Macs differs by tens of parts per million, a millisecond a
/// minute at worst, so the estimate fits the drift instead of averaging over it.
pub const CLOCK_WINDOW: Duration = Duration::from_secs(30);

/// Of the probes in the window, those whose round trip is within this of the fastest one's
/// (or within an eighth of the fastest, when that is more) are the ones the estimate is fitted
/// to: a round trip that sat in a queue on one leg says little about the clock and much about
/// the queue.
const NEAR_FLOOR: Duration = Duration::from_micros(200);

/// Slices of [`CLOCK_WINDOW`] that give one probe each to the fit.
const CLOCK_SLICES: f64 = 15.0;

/// The fit estimates drift only over probes this far apart; closer, the drift is lost in the
/// noise of the offsets and the estimate holds the offset still.
const DRIFT_SPAN: Duration = Duration::from_secs(8);

/// No two clocks worth measuring drift further apart than this, in parts per million; a fit
/// that says more is fitting noise, and is held to it.
const MAX_DRIFT_PPM: f64 = 500.0;

/// Beyond what a probe's round trip and the estimate's bound allow, a probe must be this far
/// off the estimate before it counts against it.
const JUMP_SLACK: Duration = Duration::from_millis(1);

/// Probes in a row that disagree with the estimate, and with it by more than their round trip
/// allows, after which the clock is taken to have jumped and the estimate starts over from
/// them. One could be a probe answered by a host whose clock was mid-step; two in a row that
/// agree with each other are the new clock.
const JUMP_AFTER: usize = 2;

/// Probes kept at most, whatever their age: a burst of echoes cannot grow the window.
const PROBES_KEPT: usize = 512;

/// The host's capture clock placed on this process's, and how far it may be off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClockEstimate {
    /// The newest probe's moment on both clocks, as the fit places them.
    pub anchor: ClockAnchor,
    /// The most the anchor can be off, either way, while the clocks drift at a steady rate
    /// over [`CLOCK_WINDOW`]. Each probe pins the host's clock to within half its round trip
    /// of the offset it saw (no more can be known without assuming the path symmetric), so
    /// this is how far the fitted line strays from what the newest probe pins, or what the
    /// fitted probes at either end pin carried forward to the anchor, whichever is less.
    ///
    /// The anchor converts at one to one, so a frame captured a while after it is off by the
    /// drift over that while as well: [`drift_ppm`](Self::drift_ppm) of it, tens of
    /// microseconds between probes.
    pub bound: Duration,
    /// The fastest round trip in the window.
    pub rtt: Duration,
    /// How fast the host's clock runs against this one, parts per million; zero until the
    /// probes span long enough to tell.
    pub drift_ppm: i32,
}

/// One probe's round trip, in microseconds since the [`ClockSync`]'s epoch.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ClockSample {
    /// The midpoint of the probe's send and its echo's arrival, on this process's clock.
    at: f64,
    /// Host clock minus this one at `at`: `((received − sent) + (echoed − arrived)) / 2`.
    offset: f64,
    /// The round trip less the host's time holding the probe.
    delay: f64,
}

/// A line through the probes: the offset at `at` and its change per microsecond.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Fit {
    at: f64,
    offset: f64,
    drift: f64,
    /// Half the slowest round trip among the probes fitted: how far a probe may sit off the line
    /// and still be explained by its path.
    spread: f64,
    floor: f64,
    /// The first and the last probe fitted.
    ends: (ClockSample, ClockSample),
}

impl Fit {
    fn offset_at(&self, at: f64) -> f64 {
        self.drift.mul_add(at - self.at, self.offset)
    }

    /// The most the line can be off at `sample`'s moment: its miss there, and half the round
    /// trip within which `sample` pins the true offset.
    fn miss(&self, sample: &ClockSample) -> f64 {
        (self.offset_at(sample.at) - sample.offset).abs() + sample.delay / 2.0
    }

    /// The most the line can be off at `newest`'s moment, with both the line and the true
    /// offset straight over the window: the error is then straight too, so it is pinned by
    /// `newest` alone, or by the fitted ends' errors carried to it (either way: an echo that
    /// overtook another can be older than the last probe fitted).
    fn bound_at(&self, newest: &ClockSample) -> f64 {
        let (first, last) = &self.ends;
        let span = last.at - first.at;
        let carried = if span > 0.0 {
            let by = (newest.at - last.at).abs() / span;
            (self.miss(first) + self.miss(last)).mul_add(by, self.miss(last))
        } else {
            f64::INFINITY
        };
        self.miss(newest).min(carried)
    }
}

/// Places the host's capture clock on this process's from clock probes, NTP's way, on any
/// link (video decisions, "Capture to glass on any link").
///
/// Each probe's echo gives an offset between the clocks that is off by at most half its round
/// trip, since the path's two legs are not known to be equal. So the probes kept are those that
/// met the emptiest path in the last [`CLOCK_WINDOW`] (near the fastest round trip, the fastest
/// of each slice of the window), and a line through their offsets gives the offset now and how
/// fast the clocks drift apart. A probe the line cannot explain even at the full width of its own
/// round trip means a clock stepped (a Mac that slept stops its host clock); two of those in a
/// row that agree with each other replace the window. Pure: the caller hands in the times.
#[derive(Clone, Debug)]
pub struct ClockSync {
    epoch: Instant,
    samples: VecDeque<ClockSample>,
    /// Probes in a row the estimate could not explain.
    strikes: Vec<ClockSample>,
    fit: Option<Fit>,
    /// Probes taken, and the clock steps seen, for the lifetime of the stream.
    probes: u64,
    jumps: u64,
}

impl ClockSync {
    /// Probes stamped from `epoch`, this process's clock.
    #[must_use]
    pub const fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            samples: VecDeque::new(),
            strikes: Vec::new(),
            fit: None,
            probes: 0,
            jumps: 0,
        }
    }

    /// The `sent_us` of a probe leaving `at`.
    #[must_use]
    pub fn stamp(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_micros()).unwrap_or(u64::MAX)
    }

    /// The echo of the probe stamped `sent_us` arrived `arrived`; the host's clock read
    /// `received_us` when the probe came and `echoed_us` when the echo left. An echo that
    /// cannot be one is ignored: it arrived before it was sent, left the host before the probe
    /// came, or the host held it longer than the whole round trip took. That last one would
    /// read as a probe that met an empty path, the one the fit trusts most, with an offset off by
    /// half the difference.
    #[expect(
        clippy::cast_precision_loss,
        reason = "microseconds stay far below 2^53"
    )]
    pub fn observe(&mut self, sent_us: u64, received_us: u64, echoed_us: u64, arrived: Instant) {
        let arrived_us = self.stamp(arrived);
        if arrived_us < sent_us || echoed_us < received_us {
            return;
        }
        let held = echoed_us.saturating_sub(received_us);
        if held
            > arrived_us
                .saturating_sub(sent_us)
                .saturating_add(STAMP_SLACK_US)
        {
            return;
        }
        let (sent, arrived) = (sent_us as f64, arrived_us as f64);
        let (received, echoed) = (received_us as f64, echoed_us as f64);
        let sample = ClockSample {
            at: f64::midpoint(sent, arrived),
            offset: f64::midpoint(received - sent, echoed - arrived),
            delay: ((arrived - sent) - (echoed - received)).max(0.0),
        };
        self.probes = self.probes.saturating_add(1);
        let window = CLOCK_WINDOW.as_secs_f64() * 1e6;
        while self
            .samples
            .front()
            .is_some_and(|s| sample.at - s.at > window)
            || self.samples.len() >= PROBES_KEPT
        {
            self.samples.pop_front();
        }
        if self.explains(&sample) {
            self.strikes.clear();
            self.samples.push_back(sample);
        } else {
            self.strikes.push(sample);
            if self.strikes.len() >= JUMP_AFTER && agree(&self.strikes) {
                self.jumps = self.jumps.saturating_add(1);
                self.samples = self.strikes.drain(..).collect();
            } else if self.strikes.len() >= JUMP_AFTER {
                // They disagree among themselves too: keep only the newest as a candidate.
                let newest = self.strikes.pop();
                self.strikes.clear();
                self.strikes.extend(newest);
            }
        }
        self.fit = fit(&self.samples);
    }

    /// Whether the estimate so far can explain `sample`: its offset lies within half its round
    /// trip, the estimate's own bound and [`JUMP_SLACK`] of the line. With no estimate, any can.
    fn explains(&self, sample: &ClockSample) -> bool {
        self.fit.is_none_or(|fit| {
            let slack = JUMP_SLACK.as_secs_f64() * 1e6;
            (sample.offset - fit.offset_at(sample.at)).abs()
                <= sample.delay / 2.0 + fit.spread + slack
        })
    }

    /// The estimate, once a probe has come back.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded microseconds, clamped to their type's range first"
    )]
    pub fn estimate(&self) -> Option<ClockEstimate> {
        let fit = self.fit?;
        let newest = self.samples.back()?;
        let at = self
            .epoch
            .checked_add(Duration::from_micros(newest.at.max(0.0) as u64))?;
        let host = (newest.at + fit.offset_at(newest.at)).round();
        if !(0.0..1.8e19).contains(&host) {
            return None;
        }
        let micros = |us: f64| Duration::from_micros(us.max(0.0).round() as u64);
        Some(ClockEstimate {
            anchor: ClockAnchor {
                at,
                host_us: host as u64,
            },
            bound: micros(fit.bound_at(newest)),
            rtt: micros(fit.floor),
            drift_ppm: (fit.drift * 1e6).round() as i32,
        })
    }

    /// Probes taken so far, and how many times a clock was seen to step.
    #[must_use]
    pub const fn counts(&self) -> (u64, u64) {
        (self.probes, self.jumps)
    }
}

/// Whether every sample in `samples` explains every other: the offsets overlap within their
/// round trips and [`JUMP_SLACK`].
fn agree(samples: &[ClockSample]) -> bool {
    let slack = JUMP_SLACK.as_secs_f64() * 1e6;
    samples.iter().all(|a| {
        samples
            .iter()
            .all(|b| (a.offset - b.offset).abs() <= f64::midpoint(a.delay, b.delay) + slack)
    })
}

/// A line through the probes that met the emptiest path, or `None` for no probes.
///
/// The window is cut into [`CLOCK_SLICES`], and each slice gives its fastest probe, so the line
/// rests on points spread over the whole window rather than on a burst of them. Of those, the
/// ones within [`NEAR_FLOOR`] (or an eighth of the floor, on a slower path) of the fastest
/// round trip are fitted by least squares, each weighted by how close to the floor it came. The
/// slope is fitted only when they span [`DRIFT_SPAN`], and never past [`MAX_DRIFT_PPM`].
fn fit(samples: &VecDeque<ClockSample>) -> Option<Fit> {
    let floor = samples.iter().map(|s| s.delay).reduce(f64::min)?;
    let near = (NEAR_FLOOR.as_secs_f64() * 1e6).max(floor / 8.0);
    let slice = CLOCK_WINDOW.as_secs_f64() * 1e6 / CLOCK_SLICES;
    let first_at = samples.front()?.at;
    let slice_of = |at: f64| ((at - first_at) / slice).floor();
    let mut kept: Vec<ClockSample> = Vec::new();
    for sample in samples.iter().filter(|s| s.delay <= floor + near) {
        let same = kept
            .last()
            .is_some_and(|k| slice_of(k.at).total_cmp(&slice_of(sample.at)).is_eq());
        match kept.last_mut() {
            Some(last) if same => {
                if sample.delay < last.delay {
                    *last = *sample;
                }
            }
            _ => kept.push(*sample),
        }
    }
    // A probe on the floor weighs as much as one a tenth of the band above it, no more: the
    // floor's own probe is not exact either.
    let weight = |s: &ClockSample| (near / 10.0) / (s.delay - floor + near / 10.0);
    let total = kept.iter().map(weight).sum::<f64>();
    let mean_at = kept.iter().map(|s| weight(s) * s.at).sum::<f64>() / total;
    let mean_offset = kept.iter().map(|s| weight(s) * s.offset).sum::<f64>() / total;
    let (first, last) = (kept.first()?.at, kept.last()?.at);
    let drift = if last - first >= DRIFT_SPAN.as_secs_f64() * 1e6 {
        let spread = kept
            .iter()
            .map(|s| weight(s) * (s.at - mean_at).powi(2))
            .sum::<f64>();
        let covary = kept
            .iter()
            .map(|s| weight(s) * (s.at - mean_at) * (s.offset - mean_offset))
            .sum::<f64>();
        (covary / spread).clamp(-MAX_DRIFT_PPM / 1e6, MAX_DRIFT_PPM / 1e6)
    } else {
        0.0
    };
    let spread = kept.iter().map(|s| s.delay).reduce(f64::max)? / 2.0;
    let ends = (*kept.first()?, *kept.last()?);
    Some(Fit {
        at: mean_at,
        offset: mean_offset,
        drift,
        spread,
        floor,
        ends,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// The anchor reads only the wire's low 32 bits, so a stamp from just past a wrap of the
    /// host's clock still converts to a moment right after the anchor, not 71 minutes off.
    #[test]
    fn the_anchor_reads_the_nearest_moment_across_a_wrap() {
        let at = Instant::now();
        let anchor = ClockAnchor {
            at,
            host_us: (1_u64 << 32) - 1_000,
        };
        let captured = anchor.captured((1_u64 << 32) + 2_000).expect("in range");
        assert_eq!(captured.saturating_duration_since(at), 3 * MS);
        let before = anchor.captured((1_u64 << 32) - 4_000).expect("in range");
        assert_eq!(at.saturating_duration_since(before), 3 * MS);
    }

    /// A link for clock probes: the host's clock runs `drift_ppm` fast from `start_us`, and
    /// may step; each leg of a probe takes its base plus jitter from a fixed sequence.
    struct Link {
        epoch: Instant,
        start_us: f64,
        drift_ppm: f64,
        step_us: f64,
        up_us: f64,
        down_us: f64,
        jitter_us: f64,
        seed: u64,
    }

    impl Link {
        fn new(up_us: f64, down_us: f64, jitter_us: f64) -> Self {
            Self {
                epoch: Instant::now(),
                start_us: 123_456_789.0,
                drift_ppm: 0.0,
                step_us: 0.0,
                up_us,
                down_us,
                jitter_us,
                seed: 0x9e37_79b9_7f4a_7c15,
            }
        }

        /// The host's clock when this one reads `t_us` from the epoch.
        fn host(&self, t_us: f64) -> f64 {
            (self.drift_ppm / 1e6).mul_add(t_us, self.start_us + t_us + self.step_us)
        }

        /// Queueing on one leg: mostly a little, now and then a lot (xorshift, cubed).
        fn jitter(&mut self) -> f64 {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            #[expect(clippy::cast_precision_loss, reason = "a fraction")]
            let unit = (self.seed >> 11) as f64 / (1_u64 << 53) as f64;
            self.jitter_us * unit.powi(3)
        }

        /// Send a probe `t_us` after the epoch and hand its echo to `sync`.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test µs"
        )]
        fn probe(&mut self, sync: &mut ClockSync, t_us: f64) {
            let up = self.up_us + self.jitter();
            let down = self.down_us + self.jitter();
            let received = self.host(t_us + up);
            let echoed = self.host(t_us + up + 40.0);
            let arrived = t_us + up + 40.0 + down;
            let at = self
                .epoch
                .checked_add(Duration::from_micros(arrived as u64))
                .unwrap();
            sync.observe(t_us as u64, received as u64, echoed as u64, at);
        }

        /// How far the estimate's anchor is from the host's true clock, microseconds.
        #[expect(clippy::cast_precision_loss, reason = "test µs")]
        fn error_us(&self, estimate: &ClockEstimate) -> f64 {
            let at = estimate.anchor.at.duration_since(self.epoch).as_micros() as f64;
            estimate.anchor.host_us as f64 - self.host(at)
        }
    }

    fn sync_for(link: &Link) -> ClockSync {
        ClockSync::new(link.epoch)
    }

    /// Four probes a second for `secs`, starting `from_s` after the epoch.
    #[expect(clippy::cast_precision_loss, reason = "test µs")]
    fn run(link: &mut Link, sync: &mut ClockSync, from_s: u64, secs: u64) {
        for i in from_s.saturating_mul(4)..from_s.saturating_add(secs).saturating_mul(4) {
            link.probe(sync, i as f64 * 250_000.0);
        }
    }

    /// Loopback: one clock, a round trip of a fifth of a millisecond. The estimate is the
    /// shared clock to within the round trip, and says so.
    #[test]
    fn on_loopback_the_estimate_is_the_shared_clock() {
        let mut link = Link::new(100.0, 100.0, 50.0);
        let mut sync = sync_for(&link);
        assert_eq!(sync.estimate(), None, "no probe, no estimate");
        run(&mut link, &mut sync, 0, 5);
        let estimate = sync.estimate().expect("probes came back");
        assert!(link.error_us(&estimate).abs() <= 20.0, "{estimate:?}");
        assert!(estimate.bound <= Duration::from_micros(150), "{estimate:?}");
        assert!(estimate.rtt >= Duration::from_micros(200), "{estimate:?}");
    }

    /// The tailnet to another Mac: 5 ms each way with up to 30 ms of queueing on either leg,
    /// and a host clock from 80 ppm slow to 150 ppm fast, over six queueing sequences. The
    /// probes that met an empty path carry the estimate: within 0.15 ms of the truth after a
    /// minute (66 µs at worst when written), with the drift found within 15 ppm, and a bound it
    /// states that holds from the first probes on without claiming more than it can know.
    #[test]
    fn the_estimate_finds_a_drifting_clock_through_a_jittery_link() {
        for seed in [1_u64, 7, 42, 99, 12_345, 0xdead_beef] {
            for ppm in [-80_i32, 0, 40, 150] {
                let mut link = Link::new(5_000.0, 5_000.0, 30_000.0);
                link.seed = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
                link.drift_ppm = f64::from(ppm);
                let mut sync = sync_for(&link);
                run(&mut link, &mut sync, 0, 2);
                let early = sync.estimate().expect("probes came back");
                assert!(
                    link.error_us(&early).abs() <= early.bound.as_secs_f64() * 1e6,
                    "{early:?}"
                );
                run(&mut link, &mut sync, 2, 58);
                let estimate = sync.estimate().expect("probes came back");
                let error = link.error_us(&estimate);
                let case = format!("seed {seed}, {ppm} ppm: {error:.0} µs off, {estimate:?}");
                assert!(error.abs() <= 150.0, "{case}");
                assert!(error.abs() <= estimate.bound.as_secs_f64() * 1e6, "{case}");
                assert!((estimate.drift_ppm - ppm).abs() <= 15, "{case}");
                // Half the fastest round trip, and what the fitted ends carried forward add.
                let most = estimate.rtt / 2 + Duration::from_millis(3);
                assert!(estimate.bound < most, "{case}");
            }
        }
    }

    /// The bound an estimate states holds after every probe, not only once the fit settles: on
    /// loopback, through queueing, on a slow path, one way slower than the other, and for a
    /// clock drifting past what the fit will follow, where the line misses and says so.
    #[test]
    fn the_stated_bound_holds_after_every_probe() {
        let paths = [
            (100.0, 100.0, 50.0),
            (5_000.0, 5_000.0, 30_000.0),
            (40_000.0, 40_000.0, 60_000.0),
            (2_000.0, 8_000.0, 1_000.0),
        ];
        for (up, down, jitter) in paths {
            for seed in [1_u64, 42, 0xdead_beef] {
                for ppm in [-80_i32, 150, 800] {
                    let mut link = Link::new(up, down, jitter);
                    link.seed = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
                    link.drift_ppm = f64::from(ppm);
                    let mut sync = sync_for(&link);
                    for i in 0..240_u32 {
                        link.probe(&mut sync, f64::from(i) * 250_000.0);
                        let estimate = sync.estimate().expect("probes came back");
                        let error = link.error_us(&estimate);
                        assert!(
                            error.abs() <= estimate.bound.as_secs_f64().mul_add(1e6, 1.0),
                            "{up}/{down} ± {jitter}, seed {seed}, {ppm} ppm, probe {i}: \
                             {error:.0} µs off, {estimate:?}"
                        );
                    }
                }
            }
        }
    }

    /// A path slower one way than the other cannot be told from a clock offset: the estimate is
    /// off by half the difference, and the bound it states still covers that.
    #[test]
    fn an_asymmetric_path_stays_inside_the_bound() {
        let mut link = Link::new(2_000.0, 8_000.0, 1_000.0);
        let mut sync = sync_for(&link);
        run(&mut link, &mut sync, 0, 20);
        let estimate = sync.estimate().expect("probes came back");
        let error = link.error_us(&estimate);
        assert!(
            (error.abs() - 3_000.0).abs() <= 200.0,
            "{error}: half of 6 ms, off one way"
        );
        assert!(
            error.abs() <= estimate.bound.as_secs_f64() * 1e6,
            "{estimate:?}"
        );
    }

    /// The host's Mac slept: its host clock stood still for three seconds, so it now reads
    /// three seconds behind. One probe that disagrees is not trusted on its own; the second in
    /// a row that agrees with it replaces the window, and the estimate follows the new clock.
    #[test]
    fn a_clock_that_steps_is_followed_after_two_probes() {
        let mut link = Link::new(3_000.0, 3_000.0, 2_000.0);
        let mut sync = sync_for(&link);
        run(&mut link, &mut sync, 0, 10);
        assert_eq!(sync.counts().1, 0);
        link.step_us = -3_000_000.0;
        link.probe(&mut sync, 10_000_000.0);
        let held = sync.estimate().expect("still one");
        assert!(
            link.error_us(&held) > 2_900_000.0,
            "one probe is not a step yet: {held:?}"
        );
        link.probe(&mut sync, 10_250_000.0);
        let followed = sync.estimate().expect("still one");
        assert_eq!(sync.counts().1, 1, "one step");
        assert!(link.error_us(&followed).abs() <= followed.bound.as_secs_f64() * 1e6);
        run(&mut link, &mut sync, 11, 10);
        let settled = sync.estimate().expect("still one");
        assert!(link.error_us(&settled).abs() <= 500.0, "{settled:?}");
    }

    /// A probe held in a queue for a long time still agrees with the estimate within its own
    /// round trip, so it neither counts as a step nor moves the estimate; an echo that cannot
    /// be one is ignored.
    #[test]
    fn a_slow_probe_or_an_impossible_echo_changes_nothing() {
        let mut link = Link::new(1_000.0, 1_000.0, 100.0);
        let mut sync = sync_for(&link);
        run(&mut link, &mut sync, 0, 4);
        let before = sync.estimate().expect("probes came back");
        link.up_us = 400_000.0;
        link.probe(&mut sync, 4_000_000.0);
        link.up_us = 1_000.0;
        let after = sync.estimate().expect("probes came back");
        assert_eq!(sync.counts().1, 0, "not a step");
        assert!(
            link.error_us(&after).abs() <= 100.0,
            "{before:?} → {after:?}"
        );
        let probes = sync.counts().0;
        sync.observe(9_000_000, 1, 2, link.epoch + Duration::from_secs(8));
        sync.observe(4_000_000, 20, 10, link.epoch + Duration::from_secs(5));
        assert_eq!(
            sync.counts().0,
            probes,
            "arrived before it left, or left before it came"
        );
        // Back 10 µs after it left, from a host that says it held it 30.
        let arrived = link.epoch + Duration::from_micros(9_000_010);
        sync.observe(9_000_000, 50_000_000, 50_000_030, arrived);
        assert_eq!(sync.counts().0, probes, "held longer than the round trip");
        sync.observe(9_000_000, 50_000_000, 50_000_012, arrived);
        assert_eq!(
            sync.counts().0,
            probes + 1,
            "a hold a truncated stamp longer is one"
        );
    }
}
