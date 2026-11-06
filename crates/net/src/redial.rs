//! When a dropped link is dialled again.
//!
//! Ported from slopty's `slopty-net` `redial.rs` (MIT), with onicode's limits (4 s for the first
//! minute, then up to 60 s; a peer on another build asked again after ten minutes).
//!
//! Every peer link follows the one rule: a quarter second after a drop,
//! doubling per failure to [`MAX`], and back to the start once a link has held for [`STEADY`].
//! A peer still not back after [`PATIENCE`] is dialled less and less often, doubling to
//! [`CEILING`], so a laptop whose link is dead does not wake its radio every four seconds for
//! good. Every wait is spread ([`SPREAD_PERMILLE`]) so links that dropped together do not dial
//! together: within the first minute only ever earlier than the schedule, so a peer that comes
//! back is still dialled within [`MAX`]. Something that says the path may have changed (a resume, a
//! new network path) starts the schedule again ([`Redial::reset`]).
//! A peer on a different build is the exception: it is asked again after [`WRONG_BUILD`].

use std::hash::{BuildHasher as _, Hasher as _};
use std::time::Duration;

use tokio::time::Instant;

/// The first redial after a drop waits this long.
pub const FIRST: Duration = Duration::from_millis(250);
/// Redials back off to this at most for the first [`PATIENCE`], so a peer that comes back soon
/// is dialled within it.
pub const MAX: Duration = Duration::from_secs(4);
/// How long a run of failures keeps to [`MAX`] before it backs off further.
pub const PATIENCE: Duration = Duration::from_secs(60);
/// Redials past [`PATIENCE`] back off to this at most.
pub const CEILING: Duration = Duration::from_secs(60);
/// How far a wait is spread, in thousandths of it: up to a fifth earlier within [`PATIENCE`], up
/// to a fifth either way past it.
pub const SPREAD_PERMILLE: u32 = 200;
/// A link that held this long was healthy: its drop starts the backoff again from [`FIRST`].
pub const STEADY: Duration = Duration::from_secs(10);
/// A peer on a different build ([`crate::WrongBuild`]) is dialled again only this long after.
///
/// Unless something says it changed: it changes only when someone updates it, and a dial
/// before then costs both ends a handshake that can only be refused.
pub const WRONG_BUILD: Duration = Duration::from_secs(600);

/// The wait before redial number `failures` (0 for the first after a drop) within the first
/// [`PATIENCE`], before its spread: 250 ms doubling to [`MAX`].
#[must_use]
pub fn delay(failures: u32) -> Duration {
    FIRST.saturating_mul(1_u32 << failures.min(8)).min(MAX)
}

/// The wait before redial number `past` (0 for the first) once failures have gone on for
/// [`PATIENCE`], before its spread: [`MAX`] doubling from the first to [`CEILING`].
#[must_use]
pub fn patient_delay(past: u32) -> Duration {
    MAX.saturating_mul(2_u32 << past.min(8)).min(CEILING)
}

/// `wait` spread by `unit` thousandths (any value in `0..=1000`): within [`PATIENCE`]
/// (`patient` false) between a fifth earlier and the wait itself, past it a fifth either way.
#[must_use]
pub fn spread(wait: Duration, unit: u32, patient: bool) -> Duration {
    let unit = unit.min(1000);
    let share = |of: u32| {
        of.saturating_mul(unit)
            .checked_div(1000)
            .unwrap_or_default()
    };
    let permille = if patient {
        1000_u32
            .saturating_sub(SPREAD_PERMILLE)
            .saturating_add(share(SPREAD_PERMILLE.saturating_mul(2)))
    } else {
        1000_u32.saturating_sub(share(SPREAD_PERMILLE))
    };
    wait.saturating_mul(permille)
        .checked_div(1000)
        .unwrap_or(wait)
}

/// A fresh draw in `0..=1000`: the standard library's random hash keys, which differ for every
/// [`std::collections::hash_map::RandomState`], hashed with the clock.
pub fn unit() -> u32 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let draw = hasher.finish().checked_rem(1001).unwrap_or_default();
    u32::try_from(draw).unwrap_or_default()
}

/// The wall clock ran this much further than the monotonic one over a wait: the machine
/// slept through it (the monotonic clock stands still in sleep on macOS and Linux alike).
pub const SLEPT_AFTER: Duration = Duration::from_secs(2);
/// How long a [`wait`] sleeps at a time between looks at the two clocks.
const WAIT_STEP: Duration = Duration::from_secs(1);

/// Whether the machine slept over a step the monotonic clock measured `mono` long, in which
/// the wall clock moved from `before` to `after`.
fn slept(before: std::time::SystemTime, after: std::time::SystemTime, mono: Duration) -> bool {
    after
        .duration_since(before)
        .is_ok_and(|wall| wall.saturating_sub(mono) >= SLEPT_AFTER)
}

/// Wait `wait` before a redial, or less once the machine wakes from sleep in it: `true` then.
///
/// The caller dials at once with its schedule [`Redial::reset`], since a wake is the moment a
/// path is most likely back. A daemon has no resume of its own to say so; this is it.
pub async fn wait(wait: Duration) -> bool {
    let mut left = wait;
    while !left.is_zero() {
        let (wall, mono) = (std::time::SystemTime::now(), tokio::time::Instant::now());
        tokio::time::sleep(left.min(WAIT_STEP)).await;
        let took = mono.elapsed();
        if slept(wall, std::time::SystemTime::now(), took) {
            return true;
        }
        left = left.saturating_sub(took);
    }
    false
}

/// One link's backoff. A link that drops soon after it came up (a peer that accepts and then
/// fails) keeps backing off instead of being redialled at once forever.
#[derive(Clone, Copy, Debug, Default)]
pub struct Redial {
    /// Failures within the first [`PATIENCE`] of the run.
    failures: u32,
    /// Failures past it.
    past: u32,
    /// When the current run of failures began.
    since: Option<Instant>,
    linked: Option<Instant>,
}

impl Redial {
    /// The link came up at `now`.
    pub const fn linked(&mut self, now: Instant) {
        self.linked = Some(now);
    }

    /// The path may have changed (a resume, a new network path, the peer said to be back): the
    /// next redial waits [`FIRST`] again.
    pub const fn reset(&mut self) {
        self.failures = 0;
        self.past = 0;
        self.since = None;
    }

    /// The link dropped, or a dial failed, at `now`: how long to wait before the next dial.
    pub fn next(&mut self, now: Instant) -> Duration {
        self.next_spread(now, unit())
    }

    /// [`Self::next`] with the spread's draw given (`0..=1000`), for a test.
    pub fn next_spread(&mut self, now: Instant, unit: u32) -> Duration {
        if self
            .linked
            .take()
            .is_some_and(|up| now.saturating_duration_since(up) >= STEADY)
        {
            self.reset();
        }
        let since = *self.since.get_or_insert(now);
        let patient = now.saturating_duration_since(since) >= PATIENCE;
        let wait = if patient {
            let wait = patient_delay(self.past);
            self.past = self.past.saturating_add(1);
            wait
        } else {
            let wait = delay(self.failures);
            self.failures = self.failures.saturating_add(1);
            wait
        };
        spread(wait, unit, patient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(waits: &[Duration]) -> Vec<u128> {
        waits.iter().map(Duration::as_millis).collect()
    }

    /// A step whose wall clock ran seconds past the monotonic one slept; one that kept pace,
    /// or whose wall clock was set back, did not.
    #[test]
    fn a_wait_tells_a_machine_that_slept() {
        let before = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let step = Duration::from_secs(1);
        assert!(!slept(before, before + step, step), "kept pace");
        assert!(
            !slept(before, before + step + Duration::from_millis(900), step),
            "drift"
        );
        assert!(
            slept(before, before + Duration::from_secs(3_600), step),
            "an hour asleep"
        );
        assert!(
            !slept(before, before - Duration::from_secs(60), step),
            "set back"
        );
    }

    /// A wait with no sleep in it lasts its whole length and says no wake.
    #[tokio::test]
    async fn a_wait_without_a_sleep_lasts_its_length() {
        let start = tokio::time::Instant::now();
        assert!(!wait(Duration::from_millis(30)).await);
        assert!(start.elapsed() >= Duration::from_millis(30));
    }

    /// Links that all dropped at once (a peer restarted) do not all dial it at once:
    /// their first redials spread over a fifth of the wait, and so do the ones after.
    #[test]
    fn many_links_dropped_together_do_not_redial_together() {
        let start = Instant::now();
        let mut links = vec![Redial::default(); 200];
        for round in 0..4 {
            let mut waits: Vec<Duration> = links.iter_mut().map(|r| r.next(start)).collect();
            waits.sort_unstable();
            let (first, last) = (waits[0], waits[waits.len() - 1]);
            let scheduled = delay(round);
            assert!(last <= scheduled, "never later than the schedule: {last:?}");
            assert!(
                last.saturating_sub(first) >= scheduled / 10,
                "round {round}: spread over {:?} of {scheduled:?}",
                last.saturating_sub(first)
            );
            waits.dedup();
            assert!(
                waits.len() > 80,
                "round {round}: {} distinct of 200",
                waits.len()
            );
        }
    }

    #[test]
    fn redials_back_off_from_a_quarter_second_to_four_then_to_sixty() {
        let delays: Vec<u128> = (0..6).map(|n| delay(n).as_millis()).collect();
        assert_eq!(delays, [250, 500, 1000, 2000, 4000, 4000]);
        assert_eq!(delay(u32::MAX), MAX);
        let patient: Vec<u128> = (0..6).map(|n| patient_delay(n).as_millis()).collect();
        assert_eq!(patient, [8000, 16000, 32000, 60000, 60000, 60000]);
        assert_eq!(patient_delay(u32::MAX), CEILING);
    }

    /// The first minute of failures keeps to 250 ms doubling to 4 s; past it the waits double
    /// to 60 s; a reset (a resume, a path change) starts it all again at 250 ms.
    #[test]
    fn a_dead_link_is_dialled_every_four_seconds_for_a_minute_then_less() {
        let start = Instant::now();
        let mut redial = Redial::default();
        let mut now = start;
        let mut waits = Vec::new();
        while now.duration_since(start) < PATIENCE + Duration::from_secs(120) {
            let wait = redial.next_spread(now, 0);
            waits.push(wait);
            now += wait;
        }
        let first_minute = waits.iter().take_while(|w| **w <= MAX).count();
        assert_eq!(ms(&waits[..5]), [250, 500, 1000, 2000, 4000]);
        assert!(
            waits[..first_minute].iter().sum::<Duration>() >= PATIENCE.saturating_sub(MAX),
            "every wait of the first minute is at most four seconds"
        );
        // The earliest draw: a fifth before 8 s doubling to 60 s.
        assert_eq!(
            ms(&waits[first_minute..first_minute + 5]),
            [6400, 12800, 25600, 48000, 48000]
        );
        redial.reset();
        assert_eq!(
            redial.next_spread(now, 0),
            FIRST,
            "a resume starts the schedule again"
        );
    }

    /// Within the first minute a wait is only ever earlier than the schedule, by up to a fifth;
    /// past it, a fifth either way.
    #[test]
    fn redials_are_spread_never_past_four_seconds_in_the_first_minute() {
        assert_eq!(spread(MAX, 0, false), MAX);
        assert_eq!(spread(MAX, 1000, false), Duration::from_millis(3200));
        assert_eq!(spread(CEILING, 0, true), Duration::from_secs(48));
        assert_eq!(spread(CEILING, 1000, true), Duration::from_secs(72));
        assert_eq!(spread(CEILING, 500, true), CEILING);
        let draws: Vec<u32> = std::iter::repeat_with(unit).take(64).collect();
        assert!(draws.iter().all(|u| *u <= 1000));
        assert!(draws.windows(2).any(|w| w[0] != w[1]), "draws differ");
        let mut redial = Redial::default();
        let start = Instant::now();
        assert!(
            (0..40).all(|_| redial.next(start) <= MAX),
            "never later than four seconds"
        );
    }

    #[test]
    fn a_steady_link_starts_the_backoff_again_and_a_flapping_one_does_not() {
        let start = Instant::now();
        let mut redial = Redial::default();
        let failed: Vec<u128> = std::iter::repeat_with(|| redial.next_spread(start, 0).as_millis())
            .take(4)
            .collect();
        assert_eq!(failed, [250, 500, 1000, 2000], "dials that fail back off");

        redial.linked(start);
        assert_eq!(
            redial.next_spread(start + STEADY / 2, 0),
            MAX,
            "a link that flapped keeps backing off"
        );

        redial.linked(start);
        assert_eq!(
            redial.next_spread(start + STEADY, 0),
            FIRST,
            "a link that held starts again"
        );
        assert_eq!(
            redial.next_spread(start + STEADY, 0),
            delay(1),
            "and a failed dial after it doubles"
        );
    }
}
