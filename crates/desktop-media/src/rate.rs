//! Adaptive video bitrate, driven by the receiver's reports and the QUIC path.
//!
//! The client asks for a ceiling (`Quality::bitrate_bps`); the host sends at whatever the path
//! sustains below it. Every [`DECIDE_EVERY`] reports (about half a second at the client's
//! 50 ms cadence) the window is judged by [`judge`], a pure function of the summed reports:
//!
//! * **Stall** — any report in the window said the link stalled (nothing arrived for a stall gap;
//!   packets held, then released together): freeze the target. Loss counted during a stall is the
//!   receiver giving up on frames the link is still holding, and the release fills the present
//!   queue for a moment; neither means the path is short of bandwidth, and sending less does not
//!   clear a Wi-Fi stall. The window is discarded (no cut, no grow, cooldown untouched) and the
//!   queue and hold figures of the next [`SETTLE_REPORTS`] reports are not counted, so the burst
//!   the release produces is not judged either. Loss in those reports still counts: a stall
//!   followed by real loss cuts once.
//! * **Overuse** — the client's present queue ≥ 3 frames or its hold p95 above 60 ms (frames
//!   waiting for their missing tail), more than 2 % of the window's frames lost after parity and
//!   NACK had their turn, or datagram loss above 10 %: cut to 75 % and hold for [`COOLDOWN`]
//!   decisions.
//! * **Clean** — datagram loss ≤ 0.5 %, no frame lost, queue ≤ 1, hold p95 ≤ 25 ms: grow by an
//!   eighth (at least [`STEP_MIN_BPS`]) towards the ceiling.
//! * Otherwise stay.
//!
//! Datagram loss between the clean and the heavy line holds rather than cuts. Parity is sized
//! from that same loss (`Redundancy`) and repairs it without a round trip, so on a path that
//! drops at random (Wi-Fi, a tailnet hop) the loss says nothing about the rate: 3 % of i.i.d.
//! loss cut a stream to the floor and halved its cadence for no frame saved (MEASUREMENTS,
//! "capture to the glass"). What congestion does that random loss does not is queue, hold
//! frames, outrun the parity, or drop heavily, and each of those still cuts. This is GCC's
//! hold band between 2 and 10 % with the lower line moved to the loss the viewer sees
//! (video decisions, "Repaired loss holds the rate").
//!
//! The policy's value (`wanted`) and the target the encoder gets are two numbers: the target is
//! `wanted` capped at 90 % of the selected QUIC path's `cwnd × 8 / rtt`, in every state
//! including a stall, because datagrams are congestion-controlled and sending past the window
//! only queues datagrams on the host that go stale before they are sent. The path is sampled
//! with every report and the window keeps the *widest* sample: BBR shrinks the congestion
//! window to four packets for 200 ms every few seconds to re-measure the round trip
//! (`ProbeRTT`), and a cap read in those 200 ms would cut a loopback stream to a few Mbit/s
//! (MEASUREMENTS, "start-up on a cold connection"). The cap only shadows
//! `wanted`: a stall that shrinks the window drags the target down for as long as the window
//! is small and the target springs back when it recovers, instead of growing back an eighth
//! at a time. A cut is taken from the target actually sent; a clean window under the cap does
//! not grow `wanted` (nothing was learned about rates above the cap).
//!
//! The target then decides the frame rate as well, through [`Cadence`]: a bitrate is spent on
//! however many frames the encoder is handed, so a collapsed path at 60 fps produces sixty
//! smeared frames a second instead of fifteen readable ones. The ladder is the client's ceiling
//! then 60, 30 and 15, and it moves on bytes per frame rather than on the bitrate itself, so a
//! ceiling of 30 and a ceiling of 120 behave the same way.

use std::time::Duration;

use crate::wire::{Chroma, RateVerdict, ReceiverReport};

/// Reports per decision.
pub(crate) const DECIDE_EVERY: u32 = 10;
/// Decisions to wait after a cut before growing again.
pub(crate) const COOLDOWN: u32 = 4;
/// Reports after a stall whose queue and hold figures are ignored (the release burst).
pub(crate) const SETTLE_REPORTS: u32 = 2;
/// Floor for any target; below this the picture is unreadable anyway.
pub(crate) const MIN_BPS: u32 = 1_000_000;
/// Where a stream starts when the ceiling is higher: a mesh path sustains this, a LAN grows
/// out of it in a few seconds.
pub(crate) const START_BPS: u32 = 12_000_000;
/// Smallest growth step.
pub(crate) const STEP_MIN_BPS: u32 = 500_000;

/// GCC's decrease line. Past it parity costs a quarter of the rate and more, and a smaller
/// picture buys more than further parity does.
const HEAVY_LOSS_PERMILLE: u32 = 100;
const UNREPAIRED_LOSS_PERMILLE: u32 = 20;
const CLEAN_LOSS_PERMILLE: u32 = 5;
const OVERUSE_QUEUE: u8 = 3;
const CLEAN_QUEUE: u8 = 1;
const OVERUSE_HOLD_MS: u128 = 60;
const CLEAN_HOLD_MS: u128 = 25;

/// What the transport knows about the selected path when a report arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PathSample {
    /// Smoothed round trip.
    pub rtt: Duration,
    /// Congestion window in bytes.
    pub cwnd: u64,
}

impl PathSample {
    /// Throughput the window allows, bits per second; `None` when the sample is empty.
    #[must_use]
    pub fn window_bps(self) -> Option<u64> {
        let rtt_us = u64::try_from(self.rtt.as_micros()).unwrap_or(u64::MAX);
        if rtt_us == 0 || self.cwnd == 0 {
            return None;
        }
        self.cwnd
            .saturating_mul(8)
            .saturating_mul(1_000_000)
            .checked_div(rtt_us)
    }
}

/// Bytes a frame needs before the cadence stays where it is; under this the ladder drops a rung.
///
/// A bitrate is a budget per second, and the encoder spends it on however many frames it is given.
/// At the 1 Mbit/s floor, 60 fps leaves 2 KB a frame: every frame is smeared and none of them is
/// worth the bandwidth it took. Halving the cadence doubles what each surviving frame gets.
pub(crate) const CADENCE_DROP_BYTES: u32 = 8 * 1024;
/// Bytes a frame must have to spare before the cadence climbs a rung, so the ladder does not flap
/// around one threshold: half again as much as leaving demands.
pub(crate) const CADENCE_RAISE_BYTES: u32 = 12 * 1024;
/// `rung` when it sits under `ceiling`, and the ceiling itself when it does not: a client that
/// asked for 30 never hears about 60, and one that asked for 10 stays at 10.
const fn under(ceiling: u16, rung: u16) -> u16 {
    if rung < ceiling { rung } else { ceiling }
}

/// The rungs highest first: the client's ceiling, then 60, 30 and 15. 15 fps is the slowest ever
/// sent — below it a moving window reads as a slideshow rather than a slow picture — and 60 is
/// there for the ceilings above it, so a 120 fps stream has somewhere to go that is not a quarter
/// of what it asked for. A repeat means that rung is at or above the ceiling; the search below
/// takes the fastest that qualifies, so a repeat costs nothing.
const fn rungs(ceiling: u16) -> [u16; 4] {
    [
        ceiling,
        under(ceiling, 60),
        under(ceiling, 30),
        under(ceiling, 15),
    ]
}

/// Whether each frame gets `bytes` at `target_bps` and `fps`.
const fn affords(fps: u16, target_bps: u32, bytes: u32) -> bool {
    match (target_bps / 8).checked_div(fps as u32) {
        Some(per_frame) => per_frame >= bytes,
        None => false,
    }
}

/// The fastest rung whose frames each get `bytes` at `target_bps`; the slowest when none does,
/// because something has to be sent.
const fn fastest_rung(ceiling: u16, target_bps: u32, bytes: u32) -> u16 {
    let [top, high, mid, low] = rungs(ceiling);
    let mut best = low;
    if mid > best && affords(mid, target_bps, bytes) {
        best = mid;
    }
    if high > best && affords(high, target_bps, bytes) {
        best = high;
    }
    if top > best && affords(top, target_bps, bytes) {
        best = top;
    }
    best
}

/// How many frames a second are worth encoding at the bitrate now in force.
///
/// Capture keeps running at the ceiling, so a change on screen is still noticed within one display
/// beat; this only decides how many of those captures reach the encoder. Two consequences to keep
/// in view: the held-bytes budget in the host's frame guard is `2 / fps` of *time*, so a slower
/// cadence lets QUIC hold proportionally longer before a frame is refused, and frames arriving
/// 67 ms apart would read as stalls to the receiver were the host not already heartbeating every
/// 25 ms of silence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cadence {
    ceiling: u16,
    current: u16,
}

impl Cadence {
    /// At the client's ceiling, where every stream starts.
    #[must_use]
    pub const fn new(ceiling_fps: u16) -> Self {
        Self {
            ceiling: ceiling_fps,
            current: ceiling_fps,
        }
    }

    /// Pick the ladder back up at the rung already in force.
    #[must_use]
    pub const fn resume(ceiling_fps: u16, current_fps: u16) -> Self {
        Self {
            ceiling: ceiling_fps,
            current: current_fps,
        }
    }

    /// The rung in force.
    #[must_use]
    pub const fn fps(self) -> u16 {
        self.current
    }

    /// Move to the rung `target_bps` affords; `None` when that is the rung already in force.
    pub const fn update(&mut self, target_bps: u32) -> Option<u16> {
        let keep = fastest_rung(self.ceiling, target_bps, CADENCE_DROP_BYTES);
        let raise = fastest_rung(self.ceiling, target_bps, CADENCE_RAISE_BYTES);
        let next = if keep < self.current {
            keep
        } else if raise > self.current {
            raise
        } else {
            return None;
        };
        self.current = next;
        Some(next)
    }
}

/// How long a frame may spend in the encoder, in periods of the rung in force, before it counts as
/// late. The hardware encoder's own latency is about one 120 fps period at 1080p and stays there
/// at 120 fps; frames that queue inside it come back later and later, 80 ms at 3024 × 1964
/// (MEASUREMENTS, "120 fps against 60").
pub(crate) const ENCODER_LATE_PERIODS: u64 = 3;
/// Late frames in a row that say the encoder cannot keep the rung: a tenth of a second at 120
/// fps. A lone slow frame (the first keyframe, a rebuild) is not a run.
pub(crate) const ENCODER_LATE_RUN: u32 = 12;

/// The rung under `fps` on the ladder (60, 30, 15); `fps` itself at the bottom.
#[must_use]
pub const fn slower_rung(fps: u16) -> u16 {
    if fps > 60 {
        60
    } else if fps > 30 {
        30
    } else if fps > 15 {
        15
    } else {
        fps
    }
}

/// Captures due at the rung that the encoder is judged over: a quarter of a second at 120, half
/// a second at 60. Long enough that the beat's jitter averages out, short enough that a rung
/// the encoder cannot feed stands for a fraction of a second.
pub(crate) const ENCODER_WINDOW: u32 = 30;

/// How the rate the encoder was fed compares with the rung, in eighths: under seven eighths of
/// the rung, the rung follows the rate. A window that ran a little short (one slow frame, the
/// beat's jitter against the gate) keeps the rung.
const ENCODER_SHORT_EIGHTHS: u64 = 7;

/// How the rate the encoder can code compares with the rung, in sevenths: over eight sevenths of
/// it, the ceiling rises to that rate. The mirror of [`ENCODER_SHORT_EIGHTHS`], so that one
/// window's noise moves the ceiling neither way, and one slow window (another process on the
/// media engine) does not hold it down for the rest of the session.
const ENCODER_ROOM_SEVENTHS: u64 = 8;

/// Whether the encoder keeps up with the rung in force.
///
/// A rung the link carries can still be one the encoder cannot: a frame a period is only a
/// frame a period if the encoder turns one out that fast. It falls short in one of two ways.
///
/// A session that queues inside VideoToolbox returns every frame later than the one before, so
/// the picture falls behind by the queue, tens of milliseconds, for as long as the rung stands:
/// a run of frames back late ([`Self::returned`]) drops the ceiling to the next rung
/// (video decisions, "The stream follows the screen's refresh").
///
/// A session that codes each frame inside the submit (sides that are multiples of 16) queues
/// nothing: the host's mailbox in front of it replaces a capture the encoder is still busy
/// for, so frames are lost rather than late, and nothing comes back late to count. At 3024 ×
/// 1968 it codes a frame in 15 ms, 66 a second, and a 120 rung was fed 66 of its 120 due
/// captures while the gate, the congestion guard and the encoder's own rate control all
/// budgeted each frame at a 120th of the rate. Here each window of `ENCODER_WINDOW` due
/// captures is weighed: the share of them the encoder took ([`Self::fed`] against
/// [`Self::superseded`]), capped at one frame per encode time. That is the rate the encoder is
/// fed, and a ceiling for the rung when it falls well under it. The same window lifts that
/// ceiling again when the encoder codes well over the rung, so a slow stretch (another stream or
/// process on the media engine) costs the rung only while it lasts
/// (video decisions, "Stream sides padded to 16").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EncoderWatch {
    /// Late frames in a row.
    late: u32,
    /// The rung the window is weighing; a new rung starts a new window.
    rung: u16,
    /// Due captures in the window that reached the encoder.
    taken: u32,
    /// Due captures in the window that a newer capture replaced while the encoder was busy.
    lost: u32,
    /// Encode time of the window's frames back, keyframes aside, and how many there were.
    encode_us: u64,
    encoded: u32,
    /// Slots of the rung that keyframes spent in the encoder this window: the captures the
    /// mailbox replaced in them are a keyframe's cost, not a rung the encoder cannot feed.
    excused: u32,
}

/// A window's verdict ([`EncoderWatch::fed`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fed {
    /// Frames a second the encoder was fed: what each of its frames is budgeted from.
    pub fps: u16,
    /// Where the rung's ceiling moves: down to what the encoder was fed when that fell well
    /// short of the rung, or up to what it can code when that is well over the rung.
    pub ceiling: Option<u16>,
}

impl EncoderWatch {
    const DEFAULT: Self = Self {
        late: 0,
        rung: 0,
        taken: 0,
        lost: 0,
        encode_us: 0,
        encoded: 0,
        excused: 0,
    };

    /// Weigh the window at the rung `fps`: a new rung starts a new one.
    const fn weigh(&mut self, fps: u16) {
        if fps != self.rung {
            *self = Self {
                late: self.late,
                rung: fps,
                ..Self::DEFAULT
            };
        }
    }

    /// A frame came back from the encoder `encode_us` after it went in, at `fps`. The ceiling
    /// the encoder can keep when this frame ends a run of twelve late ones: the
    /// rung under `fps`. The run starts again from there.
    ///
    /// A keyframe's time is not a frame's: a session's first one takes 50–130 ms at 3024 × 1964,
    /// and counted it would cap a window at 52 frames a second. Nor are the captures the mailbox
    /// replaced while it was coded: the slots it spent are excused from the window's losses
    /// ([`Self::fed`]). Charged, a first keyframe of 494 ms (a cold encoder, at 756 × 492)
    /// left one capture of a 60 fps window taken and set the ceiling at 4 frames a second, held
    /// for the 7.5 s a window takes at that rung (MEASUREMENTS, "a keyframe charged to the
    /// rung").
    pub const fn returned(&mut self, encode_us: u64, fps: u16, keyframe: bool) -> Option<u16> {
        if keyframe {
            #[expect(clippy::cast_possible_truncation, reason = "clamped to a u32 first")]
            let slots = match encode_us.checked_div(period_us(fps)) {
                Some(slots) if slots < u32::MAX as u64 => slots as u32,
                Some(_) | None => u32::MAX,
            };
            self.excused = self.excused.saturating_add(slots);
        } else {
            self.encode_us = self.encode_us.saturating_add(encode_us);
            self.encoded = self.encoded.saturating_add(1);
        }
        if encode_us <= period_us(fps).saturating_mul(ENCODER_LATE_PERIODS) {
            self.late = 0;
            return None;
        }
        self.late = self.late.saturating_add(1);
        if self.late < ENCODER_LATE_RUN {
            return None;
        }
        self.late = 0;
        let slower = slower_rung(fps);
        if slower < fps { Some(slower) } else { None }
    }

    /// A slot of the rung `fps` went unfilled: a due capture never reached the encoder, as a
    /// newer one due only in the slot after replaced it while the encoder was busy. A capture
    /// replaced within its own slot is no loss, and the caller does not tell it.
    pub const fn superseded(&mut self, fps: u16) {
        self.weigh(fps);
        self.lost = self.lost.saturating_add(1);
    }

    /// A capture went into the encoder at `fps`. The window's verdict once it holds
    /// `ENCODER_WINDOW` due captures, the ones replaced in a keyframe's slots not counted; the
    /// next window starts with the next capture.
    pub const fn fed(&mut self, fps: u16) -> Option<Fed> {
        self.weigh(fps);
        self.taken = self.taken.saturating_add(1);
        let due = self
            .taken
            .saturating_add(self.lost.saturating_sub(self.excused));
        if due < ENCODER_WINDOW {
            return None;
        }
        let rung = fps as u64;
        let delivered = match rung
            .saturating_mul(self.taken as u64)
            .checked_div(due as u64)
        {
            Some(fps) => fps,
            None => rung,
        };
        let capacity = match 1_000_000_u64
            .saturating_mul(self.encoded as u64)
            .checked_div(self.encode_us)
        {
            Some(fps) => fps,
            None => rung,
        };
        let fed = if delivered < capacity {
            delivered
        } else {
            capacity
        };
        let fed = if fed < rung { fed } else { rung };
        let short = fed.saturating_mul(8) < rung.saturating_mul(ENCODER_SHORT_EIGHTHS);
        let room = self.encoded > 0
            && capacity.saturating_mul(7) > rung.saturating_mul(ENCODER_ROOM_SEVENTHS);
        *self = Self {
            late: self.late,
            rung: fps,
            ..Self::DEFAULT
        };
        #[expect(clippy::cast_possible_truncation, reason = "at most the rung, a u16")]
        let fed = if fed == 0 { 1 } else { fed as u16 };
        let ceiling = if short {
            Some(fed)
        } else if room {
            #[expect(clippy::cast_possible_truncation, reason = "clamped to a u16 first")]
            Some(if capacity < u16::MAX as u64 {
                capacity as u16
            } else {
                u16::MAX
            })
        } else {
            None
        };
        Some(Fed { fps: fed, ceiling })
    }
}

/// One frame's period at `fps`, microseconds; a second at 0.
const fn period_us(fps: u16) -> u64 {
    match 1_000_000_u64.checked_div(fps as u64) {
        Some(us) => us,
        None => 1_000_000,
    }
}

/// The cadence gate: which captures reach the encoder at the rung in force.
///
/// Captures land on the display's beat, not on the rung's. A gate measured from the last encoded
/// frame turns a 60 fps rung on a 75 Hz display (captures 13.3 ms apart) into 37.5 fps: 13.3 ms is
/// short of the period, 26.7 ms clears it, and the 10 ms by which it overshoots is thrown away.
/// This keeps the rung's own schedule instead. Each frame claims the next slot, and a capture that
/// lands late in its slot leaves the lateness as credit for the one after, so the 60 fps rung on
/// that display sends four captures in five. The credit is at most one period: a capture more
/// than a period past its slot ends a pause, and the schedule restarts from it. An eighth of the
/// period early still counts as due, since captures jitter either way around the beat
/// (MEASUREMENTS, "capture on a 75 Hz display").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pace {
    /// The next slot, microseconds on the capture clock; 0 before any frame.
    next_us: u64,
}

impl Pace {
    /// Pick the schedule back up at the slot `next_us` (what [`Self::next_us`] returned).
    #[must_use]
    pub const fn resume(next_us: u64) -> Self {
        Self { next_us }
    }

    /// The next slot, to store and [`Self::resume`] from.
    #[must_use]
    pub const fn next_us(self) -> u64 {
        self.next_us
    }

    /// The earliest a capture passes the gate at `fps`.
    #[must_use]
    pub const fn due_at(self, fps: u16) -> u64 {
        self.next_us.saturating_sub(period_us(fps) / 8)
    }

    /// Whether a capture taken at `at_us` is due at `fps`.
    #[must_use]
    pub const fn due(self, at_us: u64, fps: u16) -> bool {
        at_us >= self.due_at(fps)
    }

    /// A frame taken at `at_us` went to the encoder at `fps`, due or not (a keyframe is never
    /// held back): it claims the slot, and the next is one period on. A frame more than a period
    /// past its slot ends a pause, and the next slot is a period after it.
    pub const fn sent(&mut self, at_us: u64, fps: u16) {
        let period = period_us(fps);
        let slot = self.next_us.saturating_add(period);
        self.next_us = if at_us > slot {
            at_us.saturating_add(period)
        } else {
            slot
        };
    }
}

/// One decision window: the reports since the last decision, summed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Window {
    /// Datagrams the client counted lost.
    pub lost: u32,
    /// Datagrams the host sent.
    pub sent: u32,
    /// Frames the client delivered, whole or repaired.
    pub frames_ok: u32,
    /// Frames the client gave up on: what parity and NACK could not repair.
    pub frames_lost: u32,
    /// Deepest present queue reported.
    pub queue_max: u8,
    /// Longest hold p95 reported.
    pub hold_max: Duration,
    /// Milliseconds the link was stalled, summed over the reports.
    pub stalled_ms: u32,
    /// Stalls that released.
    pub stalls: u32,
    /// The widest path sample of the window (highest `cwnd × 8 / rtt`).
    pub path: Option<PathSample>,
}

impl Window {
    /// Fold one report in. `settling` means the report follows a stall closely: its queue and
    /// hold figures are the release burst and are not counted.
    pub fn add(&mut self, report: &ReceiverReport, datagrams_sent: u32, settling: bool) {
        self.lost = self.lost.saturating_add(report.datagrams_lost);
        self.sent = self.sent.saturating_add(datagrams_sent);
        self.frames_ok = self.frames_ok.saturating_add(report.frames_ok);
        self.frames_lost = self.frames_lost.saturating_add(report.frames_lost);
        self.stalled_ms = self.stalled_ms.saturating_add(u32::from(report.stalled_ms));
        self.stalls = self.stalls.saturating_add(u32::from(report.stalls));
        if !settling {
            self.queue_max = self.queue_max.max(report.queue_depth);
            if report.hold_p95 > self.hold_max {
                self.hold_max = report.hold_p95;
            }
        }
    }

    /// Keep `sample` if it allows more than the window's current path sample.
    pub fn add_path(&mut self, sample: Option<PathSample>) {
        let Some(sample) = sample else { return };
        let wider = match (
            self.path.and_then(PathSample::window_bps),
            sample.window_bps(),
        ) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(have), Some(new)) => new > have,
        };
        if wider {
            self.path = Some(sample);
        }
    }

    /// Whether the link stalled at any point in the window.
    #[must_use]
    pub const fn stalled(&self) -> bool {
        self.stalled_ms > 0 || self.stalls > 0
    }

    /// Lost datagrams per thousand sent.
    #[must_use]
    pub const fn loss_permille(&self) -> u32 {
        let sent = if self.sent > self.lost {
            self.sent
        } else {
            self.lost
        };
        match self.lost.saturating_mul(1000).checked_div(sent) {
            Some(permille) => permille,
            None => 0,
        }
    }

    /// Frames lost after repair per thousand the client resolved.
    #[must_use]
    pub const fn unrepaired_permille(&self) -> u32 {
        let resolved = self.frames_ok.saturating_add(self.frames_lost);
        match self.frames_lost.saturating_mul(1000).checked_div(resolved) {
            Some(permille) => permille,
            None => 0,
        }
    }
}

/// The policy: what one window asks of the target. `cooling` is whether a recent cut still
/// holds growth back.
#[must_use]
pub const fn judge(window: &Window, cooling: bool) -> RateVerdict {
    if window.stalled() {
        return RateVerdict::Stall;
    }
    let loss = window.loss_permille();
    let hold_ms = window.hold_max.as_millis();
    let overuse = loss > HEAVY_LOSS_PERMILLE
        || window.unrepaired_permille() > UNREPAIRED_LOSS_PERMILLE
        || window.queue_max >= OVERUSE_QUEUE
        || hold_ms > OVERUSE_HOLD_MS;
    let clean = loss <= CLEAN_LOSS_PERMILLE
        && window.frames_lost == 0
        && window.queue_max <= CLEAN_QUEUE
        && hold_ms <= CLEAN_HOLD_MS;
    if overuse {
        RateVerdict::Cut
    } else if clean && !cooling {
        RateVerdict::Grow
    } else {
        RateVerdict::Steady
    }
}

/// The outcome of one decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decision {
    /// What the window asked for.
    pub verdict: RateVerdict,
    /// The target after the decision (and the cwnd cap), bits per second.
    pub target_bps: u32,
    /// Whether the target differs from before the decision.
    pub changed: bool,
    /// The cwnd cap, not the policy, is what holds the target where it is.
    pub capped: bool,
    /// The window that was judged.
    pub window: Window,
}

/// Per-stream bitrate controller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RateController {
    max_bps: u32,
    /// The policy's value: what the path has earned, cap aside.
    wanted_bps: u32,
    /// What the encoder is asked for: `wanted_bps` under the cwnd cap.
    target_bps: u32,
    reports: u32,
    cooldown: u32,
    /// Reports left whose queue and hold are ignored after a stall.
    settle: u32,
    window: Window,
}

impl RateController {
    /// Start below `max_bps` (the client's ceiling) and grow into it.
    #[must_use]
    pub fn new(max_bps: u32) -> Self {
        let max_bps = max_bps.max(MIN_BPS);
        Self {
            max_bps,
            wanted_bps: max_bps.min(START_BPS),
            target_bps: max_bps.min(START_BPS),
            reports: 0,
            cooldown: 0,
            settle: 0,
            window: Window::default(),
        }
    }

    /// Current target.
    #[must_use]
    pub const fn target_bps(self) -> u32 {
        self.target_bps
    }

    /// The client's ceiling.
    #[must_use]
    pub const fn max_bps(self) -> u32 {
        self.max_bps
    }

    /// A new ceiling from the client: clamp the target under it.
    pub fn set_max(&mut self, max_bps: u32) {
        self.max_bps = max_bps.max(MIN_BPS);
        self.wanted_bps = self.wanted_bps.min(self.max_bps);
        self.target_bps = self.target_bps.min(self.max_bps);
    }

    /// Fold in one report. Returns the decision every `DECIDE_EVERY` reports.
    pub fn on_report(
        &mut self,
        report: &ReceiverReport,
        datagrams_sent: u32,
        path: Option<PathSample>,
    ) -> Option<Decision> {
        let settling = self.settle > 0;
        self.settle = self.settle.saturating_sub(1);
        if report.stalled_ms > 0 || report.stalls > 0 {
            self.settle = SETTLE_REPORTS;
        }
        self.window.add(report, datagrams_sent, settling);
        self.window.add_path(path);
        self.reports = self.reports.saturating_add(1);
        if self.reports < DECIDE_EVERY {
            return None;
        }
        let decision = self.decide();
        self.reports = 0;
        self.window = Window::default();
        Some(decision)
    }

    fn decide(&mut self) -> Decision {
        let path = self.window.path;
        let verdict = judge(&self.window, self.cooldown > 0);
        let before = self.target_bps;
        let under_cap = self.target_bps < self.wanted_bps;
        match verdict {
            RateVerdict::Cut => {
                self.wanted_bps = before.saturating_mul(3) / 4;
                self.cooldown = COOLDOWN;
            }
            RateVerdict::Grow if !under_cap => {
                self.wanted_bps = before.saturating_add((before / 8).max(STEP_MIN_BPS));
            }
            RateVerdict::Grow | RateVerdict::Stall => {}
            RateVerdict::Steady => self.cooldown = self.cooldown.saturating_sub(1),
        }
        self.wanted_bps = self.wanted_bps.clamp(MIN_BPS, self.max_bps);
        let cap = path
            .and_then(PathSample::window_bps)
            .map(|window| u32::try_from(window.saturating_mul(9) / 10).unwrap_or(u32::MAX));
        self.target_bps = cap
            .map_or(self.wanted_bps, |cap| cap.min(self.wanted_bps))
            .max(MIN_BPS);
        Decision {
            verdict,
            target_bps: self.target_bps,
            changed: self.target_bps != before,
            capped: self.target_bps < self.wanted_bps,
            window: self.window,
        }
    }
}

/// Where a stream that asked for 4:4:4 takes it at the reference size.
///
/// The rate at which the 10-bit 4:4:4 stream matches 4:2:0's luma. At 1080p60 on scrolling text
/// 4:2:0 saturates at 6.9 Mbit/s (53.2 dB luma, chroma pinned at its 29 dB ceiling); 4:4:4 is 47.5
/// dB luma at that rate and 55.3 dB with 52.8 dB chroma at 11.1, crossing 4:2:0's luma near 10
/// (MEASUREMENTS, "4:4:4 HEVC on the low-latency encoder").
pub const FULL_CHROMA_ENTER_BPS: u32 = 10_000_000;
/// Where a 4:4:4 stream falls back to 4:2:0 at the reference size.
///
/// Between this and
/// [`FULL_CHROMA_ENTER_BPS`] 4:4:4 gives up at most a few dB of luma for some 15 dB of chroma,
/// so a stream already there holds; under it 4:2:0 is the sharper picture. A cut is to 75 %,
/// so one cut from the enter line lands under this one, and the band is wider than the
/// eighth a clean window grows by.
pub const FULL_CHROMA_LEAVE_BPS: u32 = 8_000_000;
/// Decisions a stream that fell back waits before it may take 4:4:4 again (about five
/// seconds): every switch is a new encoder session and a keyframe.
pub const FULL_CHROMA_HOLD: u32 = 10;
/// Pixels of the picture the thresholds were measured on, 1920×1080.
const REFERENCE_PIXELS: f64 = 1920.0 * 1080.0;
/// How the thresholds grow with the picture: the rate a stream saturates at grew as the
/// pixels to the 0.62 (4:4:4) and 0.66 (4:2:0) between 1080p and 5K on the same content, so
/// two thirds, which errs towards 4:2:0.
const SIZE_EXPONENT: f64 = 2.0 / 3.0;

/// The rates a `width`×`height` stream takes 4:4:4 at and falls back from, bits per second.
#[must_use]
pub fn full_chroma_band(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(width) * f64::from(height) / REFERENCE_PIXELS).powf(SIZE_EXPONENT);
    let at = |bps: u32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped"
        )]
        let scaled = (f64::from(bps) * scale)
            .round()
            .clamp(f64::from(MIN_BPS), f64::from(u32::MAX)) as u32;
        scaled
    };
    (at(FULL_CHROMA_ENTER_BPS), at(FULL_CHROMA_LEAVE_BPS))
}

/// Whether a stream carries 4:4:4 or 4:2:0 right now (video decisions, "Full chroma
/// follows the rate").
///
/// 4:4:4 is only ever what the client asked for, and only while the rate target is high enough
/// for it to beat 4:2:0 at the stream's size ([`full_chroma_band`]). It is taken at the enter
/// line and left under the lower leave line, and a stream that fell back waits
/// [`FULL_CHROMA_HOLD`] decisions before it takes 4:4:4 again, so a rate hovering near the
/// line costs one keyframe, not one a decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChromaGate {
    asked: Chroma,
    current: Chroma,
    enter_bps: u32,
    leave_bps: u32,
    hold: u32,
}

impl ChromaGate {
    /// A stream of `size` asking for `asked`, opening at `target_bps`.
    #[must_use]
    pub fn new(asked: Chroma, size: (u32, u32), target_bps: u32) -> Self {
        let mut gate = Self {
            asked,
            current: Chroma::Subsampled,
            enter_bps: 0,
            leave_bps: 0,
            hold: 0,
        };
        gate.ask(asked, size, target_bps);
        gate
    }

    /// The chroma in force.
    #[must_use]
    pub const fn chroma(self) -> Chroma {
        self.current
    }

    /// What the client asked for (4:2:0 again after a [`Self::refuse`]).
    #[must_use]
    pub const fn asked(self) -> Chroma {
        self.asked
    }

    /// The rates this stream takes 4:4:4 at and falls back from, bits per second.
    #[must_use]
    pub const fn band(self) -> (u32, u32) {
        (self.enter_bps, self.leave_bps)
    }

    /// The client asked again, or the stream is to change size: decide at once, from the band
    /// for `size`. A new ask or a new size drops the hold, since a new encoder session is being
    /// built either way and the switch costs nothing extra; the same ask at the same size
    /// (a client repeating its quality) keeps it.
    pub fn ask(&mut self, asked: Chroma, size: (u32, u32), target_bps: u32) -> Chroma {
        let band = full_chroma_band(size.0, size.1);
        if asked != self.asked || band != self.band() {
            self.hold = 0;
        }
        (self.enter_bps, self.leave_bps) = band;
        self.asked = asked;
        self.current = match (asked, self.current) {
            (Chroma::Full, Chroma::Full) if target_bps >= self.leave_bps => Chroma::Full,
            (Chroma::Full, Chroma::Full) => {
                self.hold = FULL_CHROMA_HOLD;
                Chroma::Subsampled
            }
            (Chroma::Full, Chroma::Subsampled)
                if self.hold == 0 && target_bps >= self.enter_bps =>
            {
                Chroma::Full
            }
            (Chroma::Subsampled, _) | (Chroma::Full, Chroma::Subsampled) => Chroma::Subsampled,
        };
        self.current
    }

    /// Follow a rate decision; the new chroma when it changes.
    pub fn update(&mut self, target_bps: u32) -> Option<Chroma> {
        self.hold = self.hold.saturating_sub(1);
        let next = match self.current {
            Chroma::Full if target_bps < self.leave_bps => {
                self.hold = FULL_CHROMA_HOLD;
                Chroma::Subsampled
            }
            Chroma::Subsampled
                if self.asked == Chroma::Full && self.hold == 0 && target_bps >= self.enter_bps =>
            {
                Chroma::Full
            }
            Chroma::Full | Chroma::Subsampled => return None,
        };
        self.current = next;
        Some(next)
    }

    /// A 4:4:4 session could not be had here: 4:2:0 until the client asks again.
    pub const fn refuse(&mut self) {
        self.asked = Chroma::Subsampled;
        self.current = Chroma::Subsampled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: ReceiverReport = ReceiverReport {
        // A 60 fps stream at the 50 ms report cadence.
        frames_ok: 3,
        frames_fec: 0,
        frames_lost: 0,
        datagrams_lost: 0,
        last_host_send_ts_us: 0,
        hold_p50: Duration::ZERO,
        hold_p95: Duration::ZERO,
        owd_jitter: Duration::ZERO,
        queue_depth: 0,
        late_frames: 0,
        acked_ltr: [0; 4],
        acked_ltr_len: 0,
        stalled_ms: 0,
        stalls: 0,
    };
    /// Loss that outran the parity: a quarter of the frames never arrived whole.
    const LOSSY: ReceiverReport = ReceiverReport {
        datagrams_lost: 30,
        frames_ok: 3,
        frames_lost: 1,
        ..CLEAN
    };
    /// 3 % of the datagrams dropped at random, and every frame they hit repaired by parity.
    const REPAIRED: ReceiverReport = ReceiverReport {
        datagrams_lost: 9,
        frames_fec: 1,
        ..CLEAN
    };
    /// A stall released in this report, and the frames given up during it count as lost.
    const STALL: ReceiverReport = ReceiverReport {
        datagrams_lost: 30,
        stalled_ms: 180,
        stalls: 1,
        ..CLEAN
    };
    /// Right after the release: the present queue is full of the burst.
    const BURST: ReceiverReport = ReceiverReport {
        queue_depth: 4,
        hold_p95: Duration::from_millis(90),
        ..CLEAN
    };

    /// One decision window of identical reports.
    fn run(
        c: &mut RateController,
        report: &ReceiverReport,
        sent: u32,
        path: Option<PathSample>,
    ) -> Decision {
        let mut last = None;
        for _ in 0..DECIDE_EVERY {
            if let Some(d) = c.on_report(report, sent, path) {
                last = Some(d);
            }
        }
        last.expect("one decision per DECIDE_EVERY reports")
    }

    /// Feed report sequences (each entry one full decision window) and collect the targets
    /// and verdicts they produce.
    fn trajectory(windows: &[&ReceiverReport]) -> Vec<(RateVerdict, u32)> {
        let mut c = RateController::new(30_000_000);
        windows
            .iter()
            .map(|r| run(&mut c, r, 300, None))
            .map(|d| (d.verdict, d.target_bps))
            .collect()
    }

    #[test]
    fn the_policy_is_a_pure_function_of_the_window() {
        let clean = Window {
            sent: 300,
            ..Window::default()
        };
        assert_eq!(judge(&clean, false), RateVerdict::Grow);
        assert_eq!(
            judge(&clean, true),
            RateVerdict::Steady,
            "cooling: no growth"
        );
        let lossy = Window {
            lost: 40,
            sent: 300,
            ..Window::default()
        };
        assert_eq!(judge(&lossy, false), RateVerdict::Cut);
        assert_eq!(
            judge(
                &Window {
                    stalled_ms: 120,
                    ..lossy
                },
                false
            ),
            RateVerdict::Stall
        );
        assert_eq!(
            judge(&Window { stalls: 1, ..lossy }, true),
            RateVerdict::Stall
        );
        let queued = Window {
            queue_max: 3,
            sent: 300,
            ..Window::default()
        };
        assert_eq!(judge(&queued, false), RateVerdict::Cut);
        let held = Window {
            hold_max: Duration::from_millis(80),
            sent: 300,
            ..Window::default()
        };
        assert_eq!(judge(&held, false), RateVerdict::Cut);
        let meh = Window {
            lost: 3,
            sent: 300,
            ..Window::default()
        };
        assert_eq!(
            judge(&meh, false),
            RateVerdict::Steady,
            "1 %: neither clean nor overuse"
        );
        assert_eq!(Window::default().loss_permille(), 0);
        assert_eq!(
            Window {
                lost: 5,
                sent: 0,
                ..Window::default()
            }
            .loss_permille(),
            1000
        );
    }

    #[test]
    fn report_sequences_and_their_target_trajectories() {
        use RateVerdict::{Cut, Grow, Stall, Steady};
        let start = START_BPS;
        let grown = start + start / 8;
        let cut = start / 4 * 3;
        // Clean → grow.
        assert_eq!(
            trajectory(&[&CLEAN, &CLEAN]),
            [(Grow, grown), (Grow, grown + grown / 8)]
        );
        // Loss while flowing → cut, then the cooldown holds.
        assert_eq!(
            trajectory(&[&LOSSY, &CLEAN, &CLEAN, &CLEAN, &CLEAN, &CLEAN]),
            [
                (Cut, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Grow, cut + cut / 8)
            ]
        );
        // Stall (with the loss the give-ups produce) → hold, then clean → grow straight away.
        assert_eq!(
            trajectory(&[&STALL, &STALL, &CLEAN]),
            [(Stall, start), (Stall, start), (Grow, grown)]
        );
        // Stall followed by real loss → one cut.
        assert_eq!(
            trajectory(&[&STALL, &LOSSY, &CLEAN]),
            [(Stall, start), (Cut, cut), (Steady, cut)]
        );
        // A cut, then a stall in the cooldown: the stall neither cuts again nor ends the cooldown.
        assert_eq!(
            trajectory(&[&LOSSY, &STALL, &CLEAN, &CLEAN, &CLEAN, &CLEAN, &CLEAN]),
            [
                (Cut, cut),
                (Stall, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Grow, cut + cut / 8)
            ]
        );
    }

    #[test]
    fn the_release_burst_after_a_stall_is_not_judged() {
        let mut c = RateController::new(30_000_000);
        // A window: eight clean reports, the stall releases in the ninth, the burst fills the
        // queue in the tenth.
        for _ in 0..8 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        assert_eq!(c.on_report(&STALL, 300, None), None);
        let d = c.on_report(&BURST, 300, None).expect("decision");
        assert_eq!((d.verdict, d.target_bps), (RateVerdict::Stall, START_BPS));
        // The next window starts with the burst still draining, then is clean: it grows.
        assert_eq!(c.on_report(&BURST, 300, None), None);
        for _ in 0..8 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        let d = c.on_report(&CLEAN, 300, None).expect("decision");
        assert_eq!(
            d.verdict,
            RateVerdict::Grow,
            "the burst report after a stall is not counted"
        );
        // The same burst report without a stall before it is overuse.
        let mut c = RateController::new(30_000_000);
        assert_eq!(run(&mut c, &BURST, 300, None).verdict, RateVerdict::Cut);
        // Loss in a settling report still counts.
        let mut c = RateController::new(30_000_000);
        for _ in 0..9 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        assert_eq!(
            c.on_report(&STALL, 300, None).map(|d| d.verdict),
            Some(RateVerdict::Stall)
        );
        // Two of the window's 30 frames lost for good: over the 2 % line on its own.
        let burst_loss = ReceiverReport {
            datagrams_lost: 100,
            frames_ok: 1,
            frames_lost: 2,
            ..CLEAN
        };
        assert_eq!(c.on_report(&burst_loss, 300, None), None);
        for _ in 0..8 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        let d = c.on_report(&CLEAN, 300, None).expect("decision");
        assert_eq!(d.verdict, RateVerdict::Cut, "real loss right after a stall");
    }

    /// The window's folds: the hold keeps its maximum, the path keeps the wider sample and
    /// ignores an equal or empty one, and a sample with no time or no window is no throughput.
    #[test]
    fn a_window_keeps_the_longest_hold_and_the_widest_path() {
        assert_eq!(
            PathSample {
                rtt: Duration::ZERO,
                cwnd: 10
            }
            .window_bps(),
            None
        );
        assert_eq!(
            PathSample {
                rtt: Duration::from_millis(1),
                cwnd: 0
            }
            .window_bps(),
            None
        );
        let mut w = Window::default();
        w.add(
            &ReceiverReport {
                hold_p95: Duration::from_millis(30),
                ..CLEAN
            },
            10,
            false,
        );
        w.add(
            &ReceiverReport {
                hold_p95: Duration::from_millis(20),
                ..CLEAN
            },
            10,
            false,
        );
        assert_eq!((w.hold_max, w.sent), (Duration::from_millis(30), 20));
        let first = PathSample {
            rtt: Duration::from_millis(10),
            cwnd: 100_000,
        };
        let equal = PathSample {
            rtt: Duration::from_millis(20),
            cwnd: 200_000,
        };
        let wider = PathSample {
            rtt: Duration::from_millis(10),
            cwnd: 200_000,
        };
        w.add_path(Some(first));
        w.add_path(Some(equal));
        assert_eq!(
            w.path,
            Some(first),
            "an equal window does not replace the first"
        );
        w.add_path(Some(wider));
        w.add_path(None);
        assert_eq!(w.path, Some(wider));
    }

    /// The overuse lines are exclusive: loss or a hold of exactly the figure is not yet a cut.
    #[test]
    fn the_overuse_lines_are_exclusive() {
        let at = |lost: u32, frames_lost: u32, hold_ms: u64| Window {
            lost,
            sent: 1000,
            frames_ok: 1000_u32.saturating_sub(frames_lost),
            frames_lost,
            hold_max: Duration::from_millis(hold_ms),
            ..Window::default()
        };
        assert_ne!(
            judge(&at(HEAVY_LOSS_PERMILLE, 0, 0), false),
            RateVerdict::Cut
        );
        assert_eq!(
            judge(&at(HEAVY_LOSS_PERMILLE + 1, 0, 0), false),
            RateVerdict::Cut
        );
        assert_ne!(
            judge(&at(0, UNREPAIRED_LOSS_PERMILLE, 0), false),
            RateVerdict::Cut
        );
        assert_eq!(
            judge(&at(0, UNREPAIRED_LOSS_PERMILLE + 1, 0), false),
            RateVerdict::Cut
        );
        let hold_line = u64::try_from(OVERUSE_HOLD_MS).unwrap();
        assert_ne!(judge(&at(0, 0, hold_line), false), RateVerdict::Cut);
        assert_eq!(judge(&at(0, 0, hold_line + 1), false), RateVerdict::Cut);
    }

    /// A window of `lost` datagrams in a thousand and `frames_lost` frames in thirty, with the
    /// present queue and hold p95 the client reported.
    fn window(lost: u32, frames_lost: u32, queue_max: u8, hold_ms: u64) -> Window {
        Window {
            lost,
            sent: 1000,
            frames_ok: 30_u32.saturating_sub(frames_lost),
            frames_lost,
            queue_max,
            hold_max: Duration::from_millis(hold_ms),
            ..Window::default()
        }
    }

    /// Random loss that parity repairs, at 3 % (the tailnet-shaped link) and 8 %, neither cuts
    /// nor grows, whatever the hold short of the overuse line. A run of such windows keeps the
    /// rate where it was instead of walking it to the floor.
    #[test]
    fn repaired_random_loss_holds_the_rate() {
        for lost in [30, 80] {
            for hold_ms in [5, 25, 45] {
                let w = window(lost, 0, 1, hold_ms);
                assert_eq!(judge(&w, false), RateVerdict::Steady, "{w:?}");
            }
        }
        let run = trajectory(&[&REPAIRED; 12]);
        assert!(
            run.iter()
                .all(|&(v, bps)| (v, bps) == (RateVerdict::Steady, START_BPS)),
            "{run:?}"
        );
        // Under the clean line it still grows.
        assert_eq!(judge(&window(5, 0, 1, 25), false), RateVerdict::Grow);
    }

    /// Loss is let off only while nothing else looks like a queue: the same 3 % cuts once the
    /// hold climbs over its line or the present queue backs up.
    #[test]
    fn repaired_loss_with_a_growing_hold_or_queue_cuts() {
        let climbing: Vec<_> = [20, 40, 70]
            .map(|hold_ms| judge(&window(30, 0, 1, hold_ms), false))
            .into();
        assert_eq!(
            climbing,
            [RateVerdict::Steady, RateVerdict::Steady, RateVerdict::Cut]
        );
        assert_eq!(judge(&window(30, 0, 3, 20), false), RateVerdict::Cut);
    }

    /// Loss cuts when the viewer would see it or when it is heavy: frames parity and NACK could
    /// not save, even with little datagram loss, or datagram loss past GCC's 10 % line even
    /// with every frame repaired. One lost frame under the line only stops the growth.
    #[test]
    fn unrepaired_or_heavy_loss_cuts() {
        assert_eq!(
            judge(&window(10, 1, 0, 0), false),
            RateVerdict::Cut,
            "1 of 30 frames"
        );
        assert_eq!(
            judge(&window(120, 0, 0, 0), false),
            RateVerdict::Cut,
            "12 %, all repaired"
        );
        let one_in_sixty = Window {
            frames_ok: 59,
            frames_lost: 1,
            sent: 1000,
            ..Window::default()
        };
        assert_eq!(judge(&one_in_sixty, false), RateVerdict::Steady);
        assert_eq!(one_in_sixty.unrepaired_permille(), 16);
        assert_eq!(Window::default().unrepaired_permille(), 0);
    }

    /// Either stall figure alone starts the settling, and a report with neither never does:
    /// the burst after a clean run is judged.
    #[test]
    fn either_stall_figure_alone_settles_the_next_reports() {
        let only_count = ReceiverReport { stalls: 1, ..CLEAN };
        let only_time = ReceiverReport {
            stalled_ms: 50,
            ..CLEAN
        };
        for stall in [only_count, only_time] {
            let mut c = RateController::new(30_000_000);
            for _ in 0..8 {
                assert_eq!(c.on_report(&CLEAN, 300, None), None);
            }
            assert_eq!(c.on_report(&stall, 300, None), None);
            let d = c.on_report(&BURST, 300, None).expect("decision");
            assert_eq!(d.verdict, RateVerdict::Stall, "{stall:?}");
            assert_eq!(
                d.window.queue_max, 0,
                "the burst was not counted after {stall:?}"
            );
        }
        let mut c = RateController::new(30_000_000);
        for _ in 0..9 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        let d = c.on_report(&BURST, 300, None).expect("decision");
        assert_eq!(
            (d.verdict, d.window.queue_max),
            (RateVerdict::Cut, 4),
            "nothing to settle"
        );
    }

    #[test]
    fn grows_into_the_ceiling_on_a_clean_path() {
        let mut c = RateController::new(30_000_000);
        assert_eq!(c.target_bps(), START_BPS);
        let mut steps = 0;
        while c.target_bps() < 30_000_000 {
            assert!(run(&mut c, &CLEAN, 300, None).changed);
            steps += 1;
            assert!(steps < 20, "never reached the ceiling");
        }
        let d = run(&mut c, &CLEAN, 300, None);
        assert_eq!(
            (d.verdict, d.changed),
            (RateVerdict::Grow, false),
            "stays at the ceiling"
        );
    }

    #[test]
    fn congestion_window_caps_the_target_in_every_state() {
        // 13 KB window over 10 ms: ~10.4 Mbit/s, 90 % of that is the cap.
        let path = PathSample {
            rtt: Duration::from_millis(10),
            cwnd: 13_000,
        };
        let cap = u32::try_from(path.window_bps().expect("window") * 9 / 10).expect("fits");
        for report in [&CLEAN, &STALL, &LOSSY] {
            let mut c = RateController::new(30_000_000);
            let d = run(&mut c, report, 300, Some(path));
            assert!(
                d.target_bps <= cap,
                "{:?} → {} over the cap {cap}",
                d.verdict,
                d.target_bps
            );
            assert!(d.capped || d.verdict == RateVerdict::Cut, "{d:?}");
        }
        let mut c = RateController::new(30_000_000);
        assert_eq!(run(&mut c, &CLEAN, 300, Some(path)).target_bps, cap);
        assert_eq!(PathSample::default().window_bps(), None);
    }

    /// The cap shadows the policy's value: a stall that shrinks the window pulls the target
    /// down only while the window is small, and clean windows under the cap teach nothing.
    #[test]
    fn the_cap_shadows_the_wanted_rate_and_lets_go() {
        let small = Some(PathSample {
            rtt: Duration::from_millis(30),
            cwnd: 9_000,
        });
        let big = Some(PathSample {
            rtt: Duration::from_millis(10),
            cwnd: 1_000_000,
        });
        let mut c = RateController::new(30_000_000);
        let d = run(&mut c, &STALL, 300, small);
        assert_eq!((d.verdict, d.capped), (RateVerdict::Stall, true));
        assert!(
            d.target_bps < 3_000_000,
            "under the 2.4 Mbit/s window: {}",
            d.target_bps
        );
        let d = run(&mut c, &STALL, 300, big);
        assert_eq!(
            (d.verdict, d.target_bps, d.capped),
            (RateVerdict::Stall, START_BPS, false)
        );
        // Clean under a small cap: the target sits at the cap and wanted does not grow.
        assert_eq!(run(&mut c, &CLEAN, 300, small).verdict, RateVerdict::Grow);
        assert_eq!(
            run(&mut c, &CLEAN, 300, big).target_bps,
            START_BPS + START_BPS / 8
        );
        // A cut is taken from the rate that was actually sent.
        let capped = run(&mut c, &CLEAN, 300, small).target_bps;
        let d = run(&mut c, &LOSSY, 300, big);
        assert_eq!(d.target_bps, capped / 4 * 3);
    }

    /// Four of the ten reports in a window see BBR's `ProbeRTT` window (four packets); the
    /// other six see the real one. The cap follows the widest sample.
    #[test]
    fn a_probe_rtt_dip_inside_the_window_does_not_cap_the_target() {
        let wide = Some(PathSample {
            rtt: Duration::from_millis(1),
            cwnd: 80_000,
        });
        let dip = Some(PathSample {
            rtt: Duration::from_millis(5),
            cwnd: 5_808,
        });
        let mut c = RateController::new(30_000_000);
        let mut last = None;
        for i in 0..DECIDE_EVERY {
            let path = if (3..7).contains(&i) { dip } else { wide };
            if let Some(d) = c.on_report(&CLEAN, 300, path) {
                last = Some(d);
            }
        }
        let d = last.expect("decision");
        assert_eq!(
            (d.verdict, d.capped, d.target_bps),
            (RateVerdict::Grow, false, START_BPS + START_BPS / 8)
        );
        // A window that only ever saw the dip is capped by it.
        let mut c = RateController::new(30_000_000);
        let d = run(&mut c, &CLEAN, 300, dip);
        assert!(d.capped && d.target_bps < START_BPS, "{d:?}");
        let mut w = Window::default();
        w.add_path(None);
        assert_eq!(w.path, None);
        w.add_path(dip);
        w.add_path(Some(PathSample::default()));
        assert_eq!(w.path, dip, "an empty sample never replaces a real one");
    }

    #[test]
    fn never_below_the_floor_or_above_the_ceiling() {
        let mut c = RateController::new(500_000);
        assert_eq!(c.max_bps(), MIN_BPS);
        assert_eq!(c.target_bps(), MIN_BPS);
        let worst = ReceiverReport {
            datagrams_lost: 100,
            ..CLEAN
        };
        let d = run(&mut c, &worst, 300, None);
        assert_eq!(
            (d.verdict, d.changed),
            (RateVerdict::Cut, false),
            "already at the floor"
        );
        c.set_max(2_000_000);
        for _ in 0..COOLDOWN + 4 {
            run(&mut c, &CLEAN, 300, None);
        }
        assert_eq!(c.target_bps(), 2_000_000);
    }

    #[test]
    fn the_cadence_drops_a_rung_when_a_frame_can_no_longer_hold_8_kb() {
        let mut c = Cadence::new(60);
        // 4 Mbit/s is 8.3 KB a frame at 60: thin, and still the fastest rung that clears the bar.
        assert_eq!(c.update(4_000_000), None);
        assert_eq!(c.fps(), 60);
        // 3 Mbit/s is 6.2 KB at 60 and 12.5 KB at 30.
        assert_eq!(c.update(3_000_000), Some(30));
        // 1.5 Mbit/s is 6.2 KB at 30 and 12.5 KB at 15.
        assert_eq!(c.update(1_500_000), Some(15));
        // The floor holds: MIN_BPS still leaves 8.3 KB at 15, and nothing slower is ever sent.
        assert_eq!(c.update(MIN_BPS), None);
        assert_eq!(c.fps(), 15);
    }

    #[test]
    fn climbing_back_costs_more_than_leaving_so_the_ladder_does_not_flap() {
        let mut c = Cadence::resume(60, 30);
        // The rate that took the stream off 60 does not put it back: 4 Mbit/s clears 8 KB at 60
        // but not the 12 KB a climb asks for.
        assert_eq!(c.update(4_000_000), None);
        assert_eq!(c.update(5_800_000), None, "12 KB × 60 × 8 is 5.9 Mbit/s");
        assert_eq!(c.update(6_000_000), Some(60));
        // And the same from the bottom rung, which may skip 30 outright when the path recovers.
        let mut c = Cadence::resume(60, 15);
        assert_eq!(c.update(3_000_000), Some(30));
        let mut c = Cadence::resume(60, 15);
        assert_eq!(c.update(20_000_000), Some(60));
    }

    #[test]
    fn the_ladder_never_passes_the_ceiling_the_client_asked_for() {
        let mut c = Cadence::new(30);
        assert_eq!(c.update(30_000_000), None, "already at the ceiling");
        assert_eq!(
            c.update(2_000_000),
            None,
            "8.3 KB a frame at 30 still clears the bar"
        );
        assert_eq!(c.update(1_500_000), Some(15));
        assert_eq!(c.update(30_000_000), Some(30));
        // A ceiling already under the floor is left alone rather than raised to 15.
        let mut c = Cadence::new(10);
        assert_eq!(c.update(MIN_BPS), None);
        assert_eq!(c.update(30_000_000), None);
        assert_eq!(c.fps(), 10);
        // A ceiling above every rung steps down through them rather than to the floor.
        let mut c = Cadence::new(120);
        assert_eq!(
            c.update(6_000_000),
            Some(60),
            "12 KB × 120 × 8 is 11.8 Mbit/s"
        );
        assert_eq!(c.update(3_000_000), Some(30));
        assert_eq!(c.update(20_000_000), Some(120));
    }

    /// A link that cannot carry 120 takes the stream to 60 before any picture gets thinner:
    /// 120 stands while a frame gets 8 KB, which the measured 1080p text scroll holds at the
    /// same PSNR as 60 does with 14 KB, and below that the next rung is 60, not 30.
    #[test]
    fn a_link_short_of_120_drops_to_60_first() {
        let mut c = Cadence::new(120);
        assert_eq!(
            c.update(8_000_000),
            None,
            "8.1 KB a frame at 120 holds the rung"
        );
        assert_eq!(
            c.update(7_500_000),
            Some(60),
            "7.6 KB at 120; 15.3 KB at 60"
        );
        assert_eq!(
            c.update(11_000_000),
            None,
            "11.2 KB at 120 is under the climb's 12"
        );
        assert_eq!(c.update(12_000_000), Some(120));
    }

    /// The encoder is let off a rung only after a run of frames queueing inside it, never for
    /// one slow frame; then it steps down one rung at a time, and the bottom rung stays.
    #[test]
    fn an_encoder_that_falls_behind_takes_the_ceiling_down_a_rung() {
        let period_120 = period_us(120);
        let late = period_120 * ENCODER_LATE_PERIODS + 1;
        let almost = |fps| {
            let mut w = EncoderWatch::default();
            for _ in 1..ENCODER_LATE_RUN {
                assert_eq!(w.returned(1_000_000, fps, false), None);
            }
            w
        };
        let mut w = EncoderWatch::default();
        assert_eq!(
            w.returned(97_000, 120, true),
            None,
            "a lone slow first frame"
        );
        assert_eq!(
            w.returned(8_000, 120, false),
            None,
            "back in time: the run is over"
        );
        assert_eq!(w.late, 0);
        for _ in 1..ENCODER_LATE_RUN {
            assert_eq!(w.returned(late, 120, false), None);
        }
        assert_eq!(
            w.returned(80_000, 120, false),
            Some(60),
            "80 ms at 3024 × 1964 and 120 fps"
        );
        assert_eq!(w.late, 0, "the run starts again at the new rung");
        // 25 ms is late at 120 and on time at 60.
        let mut w = almost(60);
        assert_eq!(
            w.returned(late, 60, false),
            None,
            "three 120 fps periods: 1.5 at 60"
        );
        assert_eq!(w.late, 0);
        for _ in 0..ENCODER_LATE_RUN {
            let _step = w.returned(60_000, 60, false);
        }
        assert_eq!(w.late, 0, "a run at 60 went to 30 and started over");
        let mut w = almost(15);
        assert_eq!(
            w.returned(1_000_000, 15, false),
            None,
            "nothing under the bottom rung"
        );
        assert_eq!(
            [slower_rung(120), slower_rung(75), slower_rung(60)],
            [60, 60, 30]
        );
        assert_eq!(
            [slower_rung(30), slower_rung(15), slower_rung(10)],
            [15, 15, 10]
        );
    }

    /// A window of an encoder fed at `fps` on a beat `beat_us` apart, each frame coded in
    /// `encode_us` inside the submit with a one-frame mailbox in front of it: the watch's verdict
    /// when the window closes. Every capture is due at the rung (the rung is the beat or above).
    fn fed_through_the_mailbox(beat_us: u64, encode_us: u64, fps: u16) -> Fed {
        fed_after_a_keyframe(beat_us, None, encode_us, fps)
    }

    /// [`fed_through_the_mailbox`], the window's first frame a keyframe coded in `keyframe_us`
    /// when there is one.
    fn fed_after_a_keyframe(
        beat_us: u64,
        keyframe_us: Option<u64>,
        encode_us: u64,
        fps: u16,
    ) -> Fed {
        let mut w = EncoderWatch::default();
        let (mut busy_until, mut waiting, mut first) = (0_u64, false, keyframe_us);
        let mut code = |w: &mut EncoderWatch| {
            let took = first.take();
            let _late = w.returned(took.unwrap_or(encode_us), fps, took.is_some());
            took.unwrap_or(encode_us)
        };
        for i in 0_u64..1_000 {
            let at = i.saturating_mul(beat_us);
            // The encoder takes the capture left for it as soon as it is free.
            if waiting && busy_until <= at {
                if let Some(verdict) = w.fed(fps) {
                    return verdict;
                }
                busy_until = busy_until.saturating_add(code(&mut w));
                waiting = false;
            }
            if at >= busy_until {
                if let Some(verdict) = w.fed(fps) {
                    return verdict;
                }
                busy_until = at.saturating_add(code(&mut w));
                continue;
            }
            if waiting {
                w.superseded(fps);
            }
            waiting = true;
        }
        panic!("no verdict");
    }

    /// An encoder that turns a frame out in 15 ms cannot feed a 120 rung however the link does:
    /// the mailbox replaces half of the due captures while it is busy, and nothing comes back
    /// late (15 ms is under three 120 fps periods) for the late watch to count. The window says
    /// the encoder was fed one frame per encode time, 66, and the rung follows. At 60 it keeps up,
    /// and a 4K encoder at 23 ms is capped at its own 43.
    #[test]
    fn a_rung_the_encoder_cannot_feed_follows_what_it_was_fed() {
        let at_120 = fed_through_the_mailbox(8_333, 15_000, 120);
        assert_eq!(
            at_120,
            Fed {
                fps: 66,
                ceiling: Some(66)
            },
            "3024 × 1968 on a 120 Hz beat"
        );
        let at_60 = fed_through_the_mailbox(16_667, 15_200, 60);
        assert_eq!(
            at_60,
            Fed {
                fps: 60,
                ceiling: None
            },
            "the same encoder at 60 keeps up"
        );
        let four_k = fed_through_the_mailbox(16_667, 23_000, 60);
        assert_eq!(four_k.ceiling, Some(four_k.fps), "{four_k:?}");
        assert!(
            (40..=43).contains(&four_k.fps),
            "4K at 23 ms a frame: {four_k:?}"
        );
    }

    /// The captures the mailbox replaces while a keyframe is coded are the keyframe's, not the
    /// rung's: a session's first keyframe that takes 494 ms (a cold encoder) leaves the 60 rung
    /// standing for an encoder that codes its frames in 2 ms, where it had cut the ceiling to 4
    /// frames a second; and an encoder slow at every frame is still capped at what it codes.
    #[test]
    fn a_slow_keyframe_costs_the_rung_nothing() {
        let beat = 16_667;
        let cold = fed_after_a_keyframe(beat, Some(494_000), 2_000, 60);
        assert_eq!(
            cold,
            Fed {
                fps: 60,
                ceiling: Some(500)
            },
            "a cold first keyframe: room"
        );
        let cold = fed_after_a_keyframe(beat, Some(494_000), 15_200, 60);
        assert_eq!(
            cold,
            Fed {
                fps: 60,
                ceiling: None
            },
            "a cold first keyframe"
        );
        let idr = fed_after_a_keyframe(beat, Some(130_000), 15_200, 60);
        assert_eq!(
            idr,
            Fed {
                fps: 60,
                ceiling: None
            },
            "an IDR at 3024 × 1964"
        );
        let four_k = fed_after_a_keyframe(beat, Some(130_000), 23_000, 60);
        assert_eq!(four_k.ceiling, Some(four_k.fps), "{four_k:?}");
        assert!(
            (40..=43).contains(&four_k.fps),
            "4K at 23 ms a frame: {four_k:?}"
        );
    }

    /// Frames that come seldom (a still screen, typing) lose nothing in the mailbox, so the rung
    /// stands as long as the encoder could code it; a slow encoder caps it all the same, and a
    /// keyframe's time is left out of that. A new rung starts a new window.
    #[test]
    fn a_seldom_fed_encoder_is_capped_only_by_its_encode_time() {
        let window = |encode_us, keyframes| {
            let mut w = EncoderWatch::default();
            let mut verdict = None;
            for i in 0..ENCODER_WINDOW {
                verdict = w.fed(60);
                let _late = w.returned(
                    if i < keyframes { 130_000 } else { encode_us },
                    60,
                    i < keyframes,
                );
            }
            verdict.expect("a verdict")
        };
        assert_eq!(
            window(15_200, 0),
            Fed {
                fps: 60,
                ceiling: None
            }
        );
        assert_eq!(
            window(15_200, 2),
            Fed {
                fps: 60,
                ceiling: None
            },
            "two first keyframes"
        );
        assert_eq!(
            window(23_000, 0),
            Fed {
                fps: 43,
                ceiling: Some(43)
            },
            "4K"
        );
        assert_eq!(
            window(17_500, 0),
            Fed {
                fps: 57,
                ceiling: None
            },
            "an eighth short at most"
        );

        let mut w = EncoderWatch::default();
        assert_eq!(w.fed(120), None);
        w.superseded(120);
        w.superseded(120);
        assert_eq!(w.fed(60), None, "the rung moved: the 120 window is gone");
        assert_eq!((w.taken, w.lost), (1, 0));
    }

    /// A ceiling a slow stretch brought down comes back up once the encoder codes well over it:
    /// a window of 18 ms frames took a 66 rung to 55, and the same session at its 15.2 ms codes
    /// 65 a second. Within an eighth of the rung either way it stays where it is.
    #[test]
    fn a_ceiling_the_encoder_outgrew_rises_to_what_it_codes() {
        let window = |fps, encode_us| {
            let mut w = EncoderWatch::default();
            let mut verdict = None;
            for _ in 0..ENCODER_WINDOW {
                let _late = w.returned(encode_us, fps, false);
                verdict = w.fed(fps);
            }
            verdict.expect("a verdict")
        };
        assert_eq!(
            window(66, 18_000),
            Fed {
                fps: 55,
                ceiling: Some(55)
            },
            "a slow stretch"
        );
        assert_eq!(
            window(55, 15_200),
            Fed {
                fps: 55,
                ceiling: Some(65)
            },
            "back to its own"
        );
        assert_eq!(
            window(60, 15_200),
            Fed {
                fps: 60,
                ceiling: None
            },
            "65 is within an eighth"
        );
        assert_eq!(
            window(60, 12_000),
            Fed {
                fps: 60,
                ceiling: Some(83)
            },
            "a smaller picture"
        );
    }

    /// The encoded frames a steady stream of captures `beat_us` apart makes through the gate at
    /// `fps` over `frames` captures, with the largest gap between two of them.
    fn through_the_gate(beat_us: u64, jitter_us: &[i64], fps: u16, frames: u64) -> (u64, u64) {
        let mut pace = Pace::default();
        let (mut sent, mut last, mut widest) = (0_u64, None::<u64>, 0_u64);
        for i in 0..frames {
            let wobble = usize::try_from(i).unwrap_or(0).checked_rem(jitter_us.len());
            let wobble = wobble.and_then(|w| jitter_us.get(w)).copied().unwrap_or(0);
            let at = i
                .saturating_mul(beat_us)
                .saturating_add(1_000_000)
                .saturating_add_signed(wobble);
            if pace.due(at, fps) {
                pace.sent(at, fps);
                sent = sent.saturating_add(1);
                if let Some(last) = last {
                    widest = widest.max(at.saturating_sub(last));
                }
                last = Some(at);
            }
        }
        (sent, widest)
    }

    #[test]
    fn a_rung_under_the_display_rate_keeps_its_rate_on_the_displays_beat() {
        // 75 Hz: four captures in five at 60 fps, never two beats apart for more than one gap.
        let beat = 13_333;
        let (sent, widest) = through_the_gate(beat, &[0], 60, 750);
        assert_eq!(sent, 600, "a second of 75 Hz captures makes 60 frames");
        assert!(
            widest <= beat.saturating_mul(2).saturating_add(1),
            "{widest}"
        );
        // At the display's rate every capture goes, and so it does with the beat's jitter.
        assert_eq!(through_the_gate(beat, &[0], 75, 750).0, 750);
        assert_eq!(
            through_the_gate(beat, &[-300, 250, 0, 400, -150], 75, 750).0,
            750
        );
        // 60 Hz at 30 and at 15: every second and every fourth capture, exactly.
        let beat = 16_667;
        assert_eq!(through_the_gate(beat, &[0], 30, 600).0, 300);
        assert_eq!(through_the_gate(beat, &[-200, 300, 100], 30, 600).0, 300);
        assert_eq!(through_the_gate(beat, &[0], 15, 600).0, 150);
        // 120 Hz at 60, and 75 Hz at 30: the rung, not the beat, sets the rate.
        assert_eq!(through_the_gate(8_333, &[0], 60, 1_200).0, 600);
        assert_eq!(through_the_gate(13_333, &[0], 30, 750).0, 300);
    }

    #[test]
    fn a_pause_restarts_the_schedule_from_the_capture_that_ends_it() {
        let mut pace = Pace::default();
        assert!(pace.due(1_000_000, 60), "the first capture is due");
        pace.sent(1_000_000, 60);
        assert!(!pace.due(1_013_333, 60), "a beat later is too soon at 60");
        assert!(
            pace.due(1_014_584, 60),
            "an eighth of the period early is due"
        );
        // A second of nothing, then a second of captures on a 75 Hz beat: the capture that ends
        // the pause goes, the next beat is too soon, and from there four in five go.
        let start = 2_000_000;
        let beats: Vec<u64> = (0..75_u64)
            .map(|i| i.saturating_mul(13_333).saturating_add(start))
            .collect();
        let mut sent = Vec::new();
        for &at in &beats {
            if pace.due(at, 60) {
                pace.sent(at, 60);
                sent.push(at);
            }
        }
        assert_eq!(
            (sent.first(), sent.get(1)),
            (beats.first(), beats.get(2)),
            "{sent:?}"
        );
        assert_eq!(sent.len(), 60, "{sent:?}");
        // A frame sent though not due (a keyframe) claims a slot all the same.
        let mut pace = Pace::default();
        pace.sent(1_000_000, 30);
        pace.sent(1_005_000, 30);
        assert!(
            !pace.due(1_060_000, 30),
            "the keyframe took the slot at 1 033 333"
        );
        assert!(pace.due(1_062_500, 30));
        assert_eq!(Pace::resume(pace.next_us()), pace);
        // A zero cadence is one frame a second, not a division trap.
        let mut pace = Pace::default();
        pace.sent(5_000_000, 0);
        assert!(!pace.due(5_500_000, 0));
        assert!(pace.due(6_000_000, 0));
    }

    const HD: (u32, u32) = (1920, 1080);

    /// The band is the measured one at 1080p, grows with the picture slower than its pixels,
    /// and never falls under the rate floor.
    #[test]
    fn the_full_chroma_band_is_measured_at_1080p_and_scales_with_the_picture() {
        assert_eq!(full_chroma_band(1920, 1080), (10_000_000, 8_000_000));
        let (enter_5k, leave_5k) = full_chroma_band(5120, 2880);
        // 7.1× the pixels: about 3.7× the rate, where 4:4:4 at 5K spent 36 Mbit/s against
        // 4:2:0's 25.
        assert!((36_000_000..38_000_000).contains(&enter_5k), "{enter_5k}");
        assert!(leave_5k < enter_5k);
        let (enter_720, _) = full_chroma_band(1280, 720);
        assert!(
            enter_720 < 10_000_000 && enter_720 > 5_000_000,
            "{enter_720}"
        );
        assert_eq!(full_chroma_band(2, 2), (MIN_BPS, MIN_BPS), "floored");
    }

    /// 4:4:4 is only what the client asked for, and only above the enter line.
    #[test]
    fn full_chroma_is_asked_for_and_earned() {
        let full = |target| ChromaGate::new(Chroma::Full, HD, target).chroma();
        assert_eq!(
            full(START_BPS),
            Chroma::Full,
            "a 1080p stream opens at 12 Mbit/s"
        );
        assert_eq!(full(9_000_000), Chroma::Subsampled, "under the enter line");
        assert_eq!(
            ChromaGate::new(Chroma::Subsampled, HD, 30_000_000).chroma(),
            Chroma::Subsampled,
            "never unasked"
        );
        assert_eq!(
            ChromaGate::new(Chroma::Full, (5120, 2880), START_BPS).chroma(),
            Chroma::Subsampled,
            "a 5K stream has not earned it at 12 Mbit/s"
        );
        let mut gate = ChromaGate::new(Chroma::Subsampled, HD, 30_000_000);
        for _ in 0..20 {
            assert_eq!(
                gate.update(30_000_000),
                None,
                "never unasked, whatever the rate"
            );
        }
    }

    /// A 4:4:4 stream holds inside the band and leaves under it; one that left waits out the
    /// hold before it comes back, even with the rate back over the line.
    #[test]
    fn full_chroma_has_hysteresis_and_a_hold() {
        let mut gate = ChromaGate::new(Chroma::Full, HD, START_BPS);
        assert_eq!(
            gate.update(9_000_000),
            None,
            "inside the band a 4:4:4 stream holds"
        );
        assert_eq!(gate.update(8_000_000), None, "the leave line itself holds");
        assert_eq!(gate.update(7_999_999), Some(Chroma::Subsampled));
        assert_eq!(
            gate.update(9_500_000),
            None,
            "inside the band a 4:2:0 stream holds too"
        );
        for decision in 2..FULL_CHROMA_HOLD {
            assert_eq!(gate.update(20_000_000), None, "held at decision {decision}");
        }
        assert_eq!(
            gate.update(20_000_000),
            Some(Chroma::Full),
            "back on the hold's last decision"
        );
    }

    /// A rate that swings across the whole band every decision switches once per hold, not
    /// once per decision: each switch is a keyframe.
    #[test]
    fn a_swinging_rate_does_not_flap_the_chroma() {
        let mut gate = ChromaGate::new(Chroma::Full, HD, START_BPS);
        let switches = (0..110_u32)
            .filter_map(|i| {
                gate.update(if i.is_multiple_of(2) {
                    7_000_000
                } else {
                    11_000_000
                })
            })
            .count();
        assert!(switches <= 20, "{switches} switches in 110 decisions");
        // The rate controller's own swing: a cut from just over the line lands under the leave
        // line, and growing back takes the cooldown plus two clean windows.
        let mut rate = RateController::new(30_000_000);
        let mut gate = ChromaGate::new(Chroma::Full, HD, rate.target_bps());
        let cut = ReceiverReport {
            frames_lost: 3,
            ..CLEAN
        };
        let mut switched = Vec::new();
        for i in 0..200_u32 {
            // Loss whenever the target is over 10.5 Mbit/s: the link's capacity.
            let report = if rate.target_bps() > 10_500_000 {
                cut
            } else {
                CLEAN
            };
            let decision = (0..DECIDE_EVERY).find_map(|_| rate.on_report(&report, 10, None));
            if let Some(chroma) = decision.and_then(|d| gate.update(d.target_bps)) {
                switched.push((i, chroma));
            }
        }
        // The sawtooth crosses the leave line about every 33 s (67 decisions), and comes back
        // once the hold is out and the rate is over the enter line again.
        assert!(
            !switched.is_empty(),
            "a cut from 11.4 Mbit/s lands under the leave line"
        );
        assert!(switched.len() <= 6, "{switched:?}");
        assert!(
            switched
                .windows(2)
                .all(|w| w[1].0 - w[0].0 > FULL_CHROMA_HOLD),
            "{switched:?}"
        );
    }

    /// Asking again decides at once from the band, and a refusal holds until the next ask.
    #[test]
    fn an_ask_decides_at_once_and_a_refusal_holds() {
        let mut gate = ChromaGate::new(Chroma::Full, HD, START_BPS);
        assert_eq!(
            gate.ask(Chroma::Full, HD, 9_000_000),
            Chroma::Full,
            "in the band, stays"
        );
        assert_eq!(
            gate.ask(Chroma::Subsampled, HD, 30_000_000),
            Chroma::Subsampled
        );
        assert_eq!(
            gate.ask(Chroma::Full, HD, 9_000_000),
            Chroma::Subsampled,
            "not earned"
        );
        assert_eq!(gate.ask(Chroma::Full, HD, 10_000_000), Chroma::Full);
        assert_eq!(
            gate.ask(Chroma::Full, (3840, 2160), 10_000_000),
            Chroma::Subsampled,
            "a bigger picture needs more for it"
        );
        // A client repeating its quality during a hold does not lift it.
        assert_eq!(gate.ask(Chroma::Full, HD, 10_000_000), Chroma::Full);
        assert_eq!(
            gate.ask(Chroma::Full, HD, 7_000_000),
            Chroma::Subsampled,
            "fell back"
        );
        assert_eq!(
            gate.ask(Chroma::Full, HD, 20_000_000),
            Chroma::Subsampled,
            "held"
        );
        assert_eq!(
            gate.ask(Chroma::Full, (1280, 720), 20_000_000),
            Chroma::Full,
            "resized"
        );
        gate.refuse();
        assert_eq!(gate.chroma(), Chroma::Subsampled);
        assert_eq!(gate.update(30_000_000), None, "refused until asked again");
        assert_eq!(gate.ask(Chroma::Full, HD, 30_000_000), Chroma::Full);
    }
}
