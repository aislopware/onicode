//! The encode engines every stream of this desktopd shares, and a focused stream's claim on them.
//!
//! Each stream codes on its own VideoToolbox session, and the sessions share the Mac's hardware
//! encode engines: two on an M1 Max, where six 1080p streams at 60 fill both. Past that each
//! stream's encoder watch sees its frames come back late or its captures replaced, and steps its
//! own rung down ([`onicode_desktop_media::EncoderWatch`]), so every stream ends up at about 44
//! frames a second, the one the person is working in with the rest (MEASUREMENTS.md, "several
//! streams on the encode engines").
//!
//! A client says which of its streams has its keyboard
//! ([`onicode_desktop_proto::screen::ScreenRequest::Focused`]). When a focused stream's watch would
//! step its rung down, the streams nobody focuses step theirs down instead (`Engines::contended`)
//! and hold there for `GIVE_WAY_US`. The watch windows that follow still hold frames queued
//! before the others gave way, so the next give-way waits `SETTLE_US` (and two of the focused
//! stream's own windows, which the stream counts). Only when none of them has a rung left does
//! the focused stream pay itself. The hold ends early when no stream is focused any more.
//!
//! Holding the others' captures back while a focused capture goes into its encoder was tried and
//! dropped. An engine codes its sessions' frames in the order they arrive, but over three rounds
//! each way the hold changed the focused stream's p95 by less than the rounds differed from
//! each other. It also cost the other streams 1–3 ms a frame and a rung (MEASUREMENTS.md, "the
//! focused stream when the engines are full").

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

/// How long a stream that gave way holds its lower ceiling. The focused stream's watch cannot
/// see whether the engines would now fit the others at their old rate (it codes as fast as it
/// did alone), so the others try again after this long, and give way again within a window if
/// they still do not fit.
pub(super) const GIVE_WAY_US: u64 = 10_000_000;

/// How long after the others gave way a focused stream's encoder falling behind asks nothing
/// more of them: its watch's windows still count the frames queued behind theirs before they
/// stepped down. Two windows at 60 frames a second.
pub(super) const SETTLE_US: u64 = 1_000_000;

/// How long no session of this desktopd may have coded anything before the engines count as
/// idle ([`Engines::quiet`]): past a still picture's last refinements, and past any frame a
/// session still holds at 60 frames a second.
pub(super) const QUIET_US: u64 = 1_000_000;

/// A call into a session under way ([`Engines::enter`]): the engines are not idle while one is,
/// however long ago its frame went in. Dropped when the call returns, so a call that never
/// returns keeps them busy for good.
pub(super) struct Inside<'a>(&'a Engines);

impl Drop for Inside<'_> {
    fn drop(&mut self) {
        self.0.inside.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What a stream lends the others.
pub(super) trait Contender: Send + Sync {
    /// A client has this stream's tile focused.
    fn focused(&self) -> bool;
    /// Step the ceiling down a rung for a focused stream and hold it there until `until_us`
    /// (`0` ends a hold); whether there was a rung to give.
    fn give_way(&self, until_us: u64) -> bool;
}

/// What became of a focused stream's claim on the others' rate ([`Engines::contended`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Claim {
    /// The streams nobody focuses stepped down a rung.
    Gave,
    /// They gave way less than [`SETTLE_US`] ago: nothing more is asked, and the focused stream
    /// keeps its rate while the queue from before drains.
    Settling,
    /// None had a rung left: the focused stream steps down itself.
    Spent,
}

/// desktopd's live streams, as their encoders contend for the engines.
pub(super) struct Engines {
    streams: Mutex<Vec<Weak<dyn Contender>>>,
    /// `host_now_us()` of the last give-way, `0` before one.
    gave_at_us: AtomicU64,
    /// The clock's time a frame last went into or came out of any session, `0` before the
    /// first.
    busy_us: AtomicU64,
    /// Calls into a session under way now, those given up on included ([`Inside`]).
    inside: AtomicUsize,
}

/// This process's engines: one desktopd, one Mac.
pub(super) static ENGINES: Engines = Engines::new();

impl Engines {
    const fn new() -> Self {
        Self {
            streams: Mutex::new(Vec::new()),
            gave_at_us: AtomicU64::new(0),
            busy_us: AtomicU64::new(0),
            inside: AtomicUsize::new(0),
        }
    }

    /// A frame went into a session, or came out of one, at `now_us`.
    pub(super) fn busy(&self, now_us: u64) {
        self.busy_us.fetch_max(now_us.max(1), Ordering::Relaxed);
    }

    /// A call into a session starts: the engines are busy until the guard is dropped.
    pub(super) fn enter(&self) -> Inside<'_> {
        self.inside.fetch_add(1, Ordering::Relaxed);
        Inside(self)
    }

    /// Whether no session has coded anything for [`QUIET_US`] by `now_us` and no call into one
    /// is under way: never before the first frame went in, while a new stream's first keyframe
    /// is still to be coded.
    ///
    /// A frame stamps the clock only as it goes in and comes out, so a call that has not
    /// returned for a second looks like a second of nothing. On a hosted virtual Mac an encode
    /// has stayed inside VideoToolbox for 170 s, and timing the engines then would put three
    /// more sessions on an encoder that is not answering (`docs/decisions/video.md`, "The stripe
    /// timing waits for the encoder to answer").
    pub(super) fn quiet(&self, now_us: u64) -> bool {
        let busy = self.busy_us.load(Ordering::Relaxed);
        busy != 0
            && now_us.saturating_sub(busy) >= QUIET_US
            && self.inside.load(Ordering::Relaxed) == 0
    }

    /// A stream opened; it leaves by being dropped.
    pub(super) fn join(&self, stream: Weak<dyn Contender>) {
        let mut streams = self.streams.lock();
        streams.retain(|s| s.strong_count() > 0);
        streams.push(stream);
    }

    /// The live streams, outside the lock: giving way takes each stream's own locks.
    fn live(&self) -> Vec<Arc<dyn Contender>> {
        let mut streams = self.streams.lock();
        streams.retain(|s| s.strong_count() > 0);
        streams.iter().filter_map(Weak::upgrade).collect()
    }

    /// A focused stream's encoder fell behind at `now_us`: every stream nobody focuses gives
    /// way a rung, unless they did less than [`SETTLE_US`] ago.
    pub(super) fn contended(&self, now_us: u64) -> Claim {
        let gave_at = self.gave_at_us.load(Ordering::Relaxed);
        if gave_at != 0 && now_us < gave_at.saturating_add(SETTLE_US) {
            return Claim::Settling;
        }
        let until = now_us.saturating_add(GIVE_WAY_US);
        let mut gave = false;
        for stream in self.live().iter().filter(|s| !s.focused()) {
            gave |= stream.give_way(until);
        }
        if !gave {
            return Claim::Spent;
        }
        self.gave_at_us.store(now_us.max(1), Ordering::Relaxed);
        Claim::Gave
    }

    /// A stream lost its focus, or closed with it. When no stream has one any more, the streams
    /// holding a lower ceiling for it may climb back at once, and the next focus asks at once.
    pub(super) fn unfocused(&self) {
        let live = self.live();
        if live.iter().any(|s| s.focused()) {
            return;
        }
        self.gave_at_us.store(0, Ordering::Relaxed);
        for stream in &live {
            stream.give_way(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU16};

    use super::*;

    /// The engines are idle only once a frame has gone in and nothing has gone in or come out
    /// for [`QUIET_US`]; a frame out of any session starts the wait again.
    #[test]
    fn the_engines_are_quiet_a_second_after_the_last_frame() {
        let engines = Engines::new();
        assert!(
            !engines.quiet(5_000_000),
            "nothing coded yet: a first keyframe is to come"
        );
        engines.busy(5_000_000);
        assert!(!engines.quiet(5_999_999));
        assert!(engines.quiet(6_000_000));
        engines.busy(4_900_000);
        assert!(
            engines.quiet(6_000_000),
            "an earlier stamp from another stream moves nothing"
        );
        engines.busy(6_100_000);
        assert!(!engines.quiet(6_200_000), "a frame came out again");
        assert!(engines.quiet(7_100_000));
    }

    /// A call into a session keeps the engines busy for as long as it is under way, however long
    /// ago its frame went in, and they are idle again once every call has returned.
    #[test]
    fn a_call_that_has_not_returned_keeps_the_engines_busy() {
        let engines = Engines::new();
        engines.busy(5_000_000);
        let first = engines.enter();
        let second = engines.enter();
        assert!(
            !engines.quiet(60_000_000),
            "a call under way for a minute is not idle"
        );
        drop(first);
        assert!(!engines.quiet(60_000_000), "one still under way");
        drop(second);
        assert!(engines.quiet(60_000_000));
    }

    /// A stream on the ladder 60, 30, 15.
    struct Stream {
        focused: AtomicBool,
        ceiling: AtomicU16,
        until: AtomicU64,
    }

    impl Stream {
        fn new(focused: bool) -> Arc<Self> {
            Arc::new(Self {
                focused: AtomicBool::new(focused),
                ceiling: AtomicU16::new(60),
                until: AtomicU64::new(0),
            })
        }
    }

    impl Contender for Stream {
        fn focused(&self) -> bool {
            self.focused.load(Ordering::Relaxed)
        }

        fn give_way(&self, until_us: u64) -> bool {
            self.until.store(until_us, Ordering::Relaxed);
            if until_us == 0 {
                return false;
            }
            let ceiling = self.ceiling.load(Ordering::Relaxed);
            let slower = onicode_desktop_media::slower_rung(ceiling);
            self.ceiling.store(slower, Ordering::Relaxed);
            slower < ceiling
        }
    }

    fn join(engines: &Engines, stream: &Arc<Stream>) {
        let weak = Arc::downgrade(stream);
        engines.join(weak);
    }

    fn ceilings(streams: &[&Arc<Stream>]) -> Vec<u16> {
        streams
            .iter()
            .map(|s| s.ceiling.load(Ordering::Relaxed))
            .collect()
    }

    /// The streams nobody focuses give way a rung at a time and hold it; within a settling
    /// second nothing more is asked; once they have none left the focused stream is told to pay
    /// itself, and a closed stream is not asked.
    #[test]
    fn the_unfocused_streams_give_way_until_they_have_no_rung_left() {
        let engines = Engines::new();
        let (focused, a, b, closed) = (
            Stream::new(true),
            Stream::new(false),
            Stream::new(false),
            Stream::new(false),
        );
        for stream in [&focused, &a, &b, &closed] {
            join(&engines, stream);
        }
        drop(closed);
        assert_eq!(engines.contended(1), Claim::Gave);
        assert_eq!(ceilings(&[&focused, &a, &b]), [60, 30, 30]);
        assert_eq!(a.until.load(Ordering::Relaxed), 1 + GIVE_WAY_US);
        assert_eq!(engines.contended(1 + SETTLE_US - 1), Claim::Settling);
        assert_eq!(
            ceilings(&[&focused, &a, &b]),
            [60, 30, 30],
            "not a second rung yet"
        );
        assert_eq!(engines.contended(1 + SETTLE_US), Claim::Gave);
        assert_eq!(ceilings(&[&focused, &a, &b]), [60, 15, 15]);
        assert_eq!(
            engines.contended(1 + 2 * SETTLE_US),
            Claim::Spent,
            "no rung left"
        );
        assert_eq!(engines.streams.lock().len(), 3, "the closed stream left");
    }

    /// Losing the last focus ends every hold and the settling at once; another focus keeps them.
    #[test]
    fn the_hold_ends_when_nothing_is_focused() {
        let engines = Engines::new();
        let (first, second, other) = (Stream::new(true), Stream::new(true), Stream::new(false));
        for stream in [&first, &second, &other] {
            join(&engines, stream);
        }
        assert_eq!(engines.contended(1), Claim::Gave);
        first.focused.store(false, Ordering::Relaxed);
        engines.unfocused();
        assert_ne!(
            other.until.load(Ordering::Relaxed),
            0,
            "the second client still focuses"
        );
        second.focused.store(false, Ordering::Relaxed);
        engines.unfocused();
        assert_eq!(other.until.load(Ordering::Relaxed), 0);
        second.focused.store(true, Ordering::Relaxed);
        assert_eq!(
            engines.contended(2),
            Claim::Gave,
            "a new focus asks at once"
        );
    }
}
