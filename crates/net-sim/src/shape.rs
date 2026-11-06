//! The link model: when a packet may leave a shaped link, and whether it leaves at all.
//!
//! Ported from axolotty's `axquic` `tests/shape/mod.rs`, itself slopty's `slopty-shape` (MIT).
//!
//! Time here is elapsed time since the run began, not a clock: the arithmetic stays total, and a
//! test can name an instant as a number of milliseconds. Every draw comes from a seed, so a run
//! repeats exactly; a measurement that cannot be repeated cannot be compared. [`crate::sim`]
//! runs this model in an in-memory network on tokio's clock.

use std::time::Duration;

/// A link's faults, as the shaper applies them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Link {
    /// One-way delay added to every packet.
    pub delay: Duration,
    /// Added on top of `delay`, spread over `0..jitter`. Packets still leave in the order they
    /// arrived: one drawing a small jitter behind one that drew a large jitter follows it out
    /// rather than passing it. QUIC reads reordering as loss, so a shaper that reorders would
    /// make every run look congested before the link had done anything.
    pub jitter: Duration,
    /// Share of packets dropped outright, 0.0 to 1.0.
    pub loss: f32,
    /// Bytes per second the link carries. Zero means no limit.
    pub rate: u64,
    /// Bytes that may wait for the rate limit. A packet arriving at a full queue is dropped,
    /// which is what a real bottleneck does and what a congestion controller is watching for.
    pub queue: u64,
}

impl Link {
    /// A link that carries everything, at once.
    pub const CLEAR: Self = Self {
        delay: Duration::ZERO,
        jitter: Duration::ZERO,
        loss: 0.0,
        rate: 0,
        queue: 0,
    };
}

/// What the shaper does with one packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fate {
    /// Send it once this much time has passed since the run began.
    At(Duration),
    /// Drop it: the link lost it, or the queue was full.
    Drop,
}

/// What a run carried and what it did not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Packets that went out.
    pub sent: u64,
    /// Packets the loss rate took.
    pub lost: u64,
    /// Packets that arrived at a full queue.
    pub overflowed: u64,
    /// Bytes that went out.
    pub bytes: u64,
}

/// One direction of a shaped link.
///
/// The bottleneck is the queue in front of it. `drains_at` is when the last byte already
/// accepted finishes leaving, so a packet's own transmission starts no earlier than that, and
/// the bytes still waiting are the gap between that and now — which is what [`Link::queue`]
/// bounds.
#[derive(Debug)]
pub struct Shaper {
    link: Link,
    /// When the queue empties if nothing more arrives. Never behind the clock once
    /// [`Shaper::admit`] has returned.
    drains_at: Duration,
    rng: Rng,
    tally: Tally,
}

impl Shaper {
    /// A shaper for `link`, drawing from `seed`.
    #[must_use]
    pub const fn new(link: Link, seed: u64) -> Self {
        Self {
            link,
            drains_at: Duration::ZERO,
            rng: Rng::new(seed),
            tally: Tally::new(),
        }
    }

    /// What this run has carried and dropped.
    #[must_use]
    pub const fn tally(&self) -> Tally {
        self.tally
    }

    /// Carry `rate` bytes per second from now on, the way a Wi-Fi or LTE link changes its rate
    /// under a sender. What is already queued keeps the times it was given; the queue's limit
    /// stays in bytes, so it holds longer at a lower rate, as a router's buffer does.
    pub const fn set_rate(&mut self, rate: u64) {
        self.link.rate = rate;
    }

    /// The link this shaper applies now.
    #[must_use]
    pub const fn link(&self) -> Link {
        self.link
    }

    /// Apply `link` from the next packet on, as a path that moved (direct to DERP) does. What
    /// is already queued keeps its times.
    pub const fn set_link(&mut self, link: Link) {
        self.link = link;
    }

    /// How long a packet arriving `now` would wait in the queue before its own transmission:
    /// the bottleneck's standing queue, in time.
    #[must_use]
    pub const fn queue_delay(&self, now: Duration) -> Duration {
        self.drains_at.saturating_sub(now)
    }

    /// Decide a packet of `len` bytes arriving `now` into the run.
    pub fn admit(&mut self, len: u64, now: Duration) -> Fate {
        if self.link.loss > 0.0 && self.rng.unit() < self.link.loss {
            self.tally.lost = self.tally.lost.saturating_add(1);
            return Fate::Drop;
        }
        // The queue drains while nothing arrives, but never into the past: a link left idle for
        // a minute does not owe the next packet a minute of free transmission.
        self.drains_at = self.drains_at.max(now);
        let leaves = self.drains_at;
        if self.link.rate > 0 {
            if leaves.saturating_sub(now) > self.queue_time() {
                self.tally.overflowed = self.tally.overflowed.saturating_add(1);
                return Fate::Drop;
            }
            self.drains_at = self.drains_at.saturating_add(self.carry(len));
        }
        self.tally.sent = self.tally.sent.saturating_add(1);
        self.tally.bytes = self.tally.bytes.saturating_add(len);
        Fate::At(
            leaves
                .saturating_add(self.link.delay)
                .saturating_add(self.jitter()),
        )
    }

    /// How long `bytes` take to leave at the link's rate.
    fn carry(&self, bytes: u64) -> Duration {
        Duration::from_nanos(
            bytes
                .saturating_mul(1_000_000_000)
                .checked_div(self.link.rate)
                .unwrap_or(0),
        )
    }

    /// How long the queue may hold, in time rather than bytes: the rate turns one into the
    /// other, and time is what the rest of the model is in.
    fn queue_time(&self) -> Duration {
        self.carry(self.link.queue)
    }

    /// A draw from `0..jitter`, or zero when there is none.
    fn jitter(&mut self) -> Duration {
        if self.link.jitter.is_zero() {
            return Duration::ZERO;
        }
        self.link.jitter.mul_f32(self.rng.unit())
    }
}

/// `xorshift64*`: a seeded generator, so a shaped run repeats byte for byte.
#[derive(Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    /// Seeded. Zero is a fixed point of the shift, so it becomes something else.
    pub(crate) const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    /// The next draw in `0.0..1.0`, to sixteen bits.
    pub(crate) fn unit(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let bits = x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 48;
        f32::from(u16::try_from(bits).unwrap_or(u16::MAX)) / 65_536.0
    }
}

impl Tally {
    /// Nothing carried yet.
    pub(crate) const fn new() -> Self {
        Self {
            sent: 0,
            lost: 0,
            overflowed: 0,
            bytes: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Milliseconds into the run.
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A link with a rate and nothing else, so the queue maths stands alone.
    const fn rate_only(rate: u64, queue: u64) -> Link {
        Link {
            rate,
            queue,
            ..Link::CLEAR
        }
    }

    #[test]
    fn a_clear_link_sends_everything_the_moment_it_arrives() {
        let mut shaper = Shaper::new(Link::CLEAR, 1);
        for step in 0..100_u64 {
            assert_eq!(shaper.admit(1_200, ms(step)), Fate::At(ms(step)));
        }
        assert_eq!(
            shaper.tally(),
            Tally {
                sent: 100,
                lost: 0,
                overflowed: 0,
                bytes: 120_000
            }
        );
    }

    #[test]
    fn delay_moves_every_packet_by_the_same_amount() {
        let mut shaper = Shaper::new(
            Link {
                delay: ms(40),
                ..Link::CLEAR
            },
            1,
        );
        assert_eq!(shaper.admit(100, Duration::ZERO), Fate::At(ms(40)));
        assert_eq!(shaper.admit(100, ms(5_000)), Fate::At(ms(5_040)));
    }

    #[test]
    fn a_bottleneck_queues_a_burst_and_drops_what_will_not_fit() {
        // 100 kB/s with a 10 kB queue: a 1 kB packet takes 10 ms, so ten of them fill it.
        let mut shaper = Shaper::new(rate_only(100_000, 10_000), 1);
        let fates: Vec<Fate> = std::iter::repeat_with(|| shaper.admit(1_000, Duration::ZERO))
            .take(20)
            .collect();
        let sent = fates.iter().filter(|f| matches!(**f, Fate::At(_))).count();
        assert_eq!(
            sent, 11,
            "the one in transmission plus a full queue behind it"
        );
        assert_eq!(shaper.tally().overflowed, 9);
        assert_eq!(
            shaper.queue_delay(Duration::ZERO),
            ms(110),
            "eleven packets to carry"
        );
        assert_eq!(
            shaper.queue_delay(ms(60)),
            ms(50),
            "and it drains at the rate"
        );
        assert_eq!(
            shaper.queue_delay(ms(500)),
            Duration::ZERO,
            "an empty queue waits for nothing"
        );
        // And they leave one transmission apart, in the order they arrived.
        assert_eq!(fates.first(), Some(&Fate::At(Duration::ZERO)));
        assert_eq!(fates.get(10), Some(&Fate::At(ms(100))));
    }

    #[test]
    fn a_queue_that_has_had_time_to_drain_takes_a_burst_again() {
        let mut shaper = Shaper::new(rate_only(100_000, 10_000), 1);
        for _packet in 0..11_u32 {
            assert!(matches!(shaper.admit(1_000, Duration::ZERO), Fate::At(_)));
        }
        assert_eq!(shaper.admit(1_000, Duration::ZERO), Fate::Drop);
        // A second later the queue is long empty, and the next packet leaves at once.
        assert_eq!(shaper.admit(1_000, ms(1_000)), Fate::At(ms(1_000)));
    }

    #[test]
    fn a_rate_change_applies_to_the_next_packet() {
        let mut shaper = Shaper::new(rate_only(100_000, 100_000), 1);
        assert_eq!(
            shaper.admit(1_000, Duration::ZERO),
            Fate::At(Duration::ZERO)
        );
        shaper.set_rate(50_000);
        // The first packet still leaves in 10 ms; the next takes 20 ms behind it.
        assert_eq!(shaper.admit(1_000, Duration::ZERO), Fate::At(ms(10)));
        assert_eq!(shaper.queue_delay(Duration::ZERO), ms(30));
    }

    #[test]
    fn loss_takes_about_its_share_and_repeats_with_the_seed() {
        let link = Link {
            loss: 0.2,
            ..Link::CLEAR
        };
        let run = || {
            let mut shaper = Shaper::new(link, 7);
            let fates: Vec<Fate> = std::iter::repeat_with(|| shaper.admit(1_200, Duration::ZERO))
                .take(2_000)
                .collect();
            (fates, shaper.tally())
        };
        let (first, tally) = run();
        let (second, _) = run();
        assert_eq!(first, second, "the same seed is the same run");
        // A fifth of 2000 is 400, and a fair sample lands well inside a quarter of that.
        assert!((300..500).contains(&tally.lost), "{tally:?}");
        assert_eq!(tally.sent + tally.lost, 2_000);
    }

    #[test]
    fn jitter_spreads_inside_its_bound_and_never_beyond_it() {
        let link = Link {
            delay: ms(10),
            jitter: ms(5),
            ..Link::CLEAR
        };
        let mut shaper = Shaper::new(link, 3);
        let mut seen_high = false;
        for _packet in 0..500_u32 {
            let Fate::At(at) = shaper.admit(1_200, Duration::ZERO) else {
                panic!("a link with no loss and no rate drops nothing")
            };
            assert!(at >= ms(10), "never early: {at:?}");
            assert!(at < ms(15), "never past the bound: {at:?}");
            seen_high |= at > ms(13);
        }
        assert!(seen_high, "the draw reaches the top of its range");
    }
}
