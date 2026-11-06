//! Client side: put fragments back together, recover, deliver in decode order, and decide when
//! to NACK and when to give up and ask for a refresh.
//!
//! State machine per stream:
//!
//! * **Need keyframe** (start): only an IDR starts delivery.
//! * **Need frame *n***: frames leave in order. A frame with missing fragments gets a NACK after a
//!   short silence, retried at most [`Config::nack_retries`] times, and declared lost after the
//!   deadline those retries imply, counted from its latest fragment — but only while the link is
//!   *flowing*: a retry needs something to have arrived since the last NACK, and the deadline only
//!   counts when newer datagrams are still coming in. A path that stalls outright (Wi-Fi delay
//!   bursts of 100–300 ms hold every packet and then release them all) is waited out up to
//!   [`Config::max_hold`], because a refresh could not get through it either and everything usually
//!   arrives once it clears. When it clears, every pending frame's NACK clock restarts: the NACK
//!   sent into the stall only left with the release, so its answer is a round trip away from *now*,
//!   not from when it was written.
//! * **Need refresh** (after a loss): the host is asked for a refresh from the last good frame; the
//!   next IDR or LTR-refresh frame restarts delivery and everything older is dropped.
//!
//! A frame nothing refers to (the encoder's temporal layer 1, [`FrameInfo::discardable`]) is never
//! worth a refresh. It is asked for once, and only while no later frame is complete; once a later
//! frame is complete, or its deadline passes, it is skipped and delivery goes on with the next
//! frame. Its own datagrams say what it is, and so do the next frame's, so a frame that lost every
//! fragment is skipped too.
//!
//! Frames whose fragments all arrive are delivered without any parity work; parity is only
//! decoded when data fragments are missing and enough parity arrived.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use reed_solomon_simd::ReedSolomonDecoder;

use crate::cursor::parse_cursor;
use crate::wire::{
    CursorUpdate, FramePrefix, HEADER_BYTES, Kind, MAX_PAYLOAD, MediaHeader, ReceiverReport,
    StreamId, flags,
};

/// Shortest silence on a stream that counts as a stall (see [`Config::stall_gap`]); the host
/// heartbeats at half this while its source is quiet.
pub const STALL_GAP: Duration = Duration::from_millis(50);

/// How far apart two `send_ms_lo` stamps can be before the byte's wrap makes the difference
/// meaningless. A silence this long is a stall by any reading, so the ambiguity costs nothing.
const SEND_STAMP_RANGE: Duration = Duration::from_millis(256);

/// How far a stamp difference may read *past* the silence it is supposed to explain before it
/// is better read as a stamp that went backwards.
///
/// The difference is congruent to the host's real interval modulo [`SEND_STAMP_RANGE`], so
/// within one range there are two readings: `d` and `d − 256 ms`. The reading nearer the
/// arrival gap is the one meant, and the midpoint of the range is where they swap. Below it,
/// the host accounts for the whole silence and a few milliseconds more, which is what the
/// stamp's millisecond truncation and the wait between building a datagram and sending it look
/// like. Above it, the datagram overtook its predecessor and the difference is not a host
/// interval at all.
const STAMP_SLACK: Duration = match SEND_STAMP_RANGE.checked_div(2) {
    Some(half) => half,
    None => SEND_STAMP_RANGE,
};

/// Acknowledged tokens kept for the next report; older ones are superseded by newer ones.
const MAX_PENDING_ACKS: usize = 16;

/// The default [`Config::tick_period`]: half [`STALL_GAP`], so the receiver looks twice per gap.
const HALF_STALL_GAP: Duration = match STALL_GAP.checked_div(2) {
    Some(half) => half,
    None => STALL_GAP,
};

/// How long to wait on an incomplete frame before asking for its missing fragments, as a
/// function of the round trip.
///
/// The delay only has to outlast the spread of one frame's fragments on the wire: they leave the
/// host back to back, so silence after them means loss rather than pacing. That spread tracks the
/// path's own delay variation, which in turn tracks its round trip, so the delay is a fraction of
/// the RTT — the same reordering tolerance TCP RACK uses (`min_rtt / 4`). The bounds matter more
/// than the fraction: without the floor a loopback link (RTT ≈ 0.5 ms) would NACK on scheduling
/// noise, and without the ceiling a slow path would hold a repairable frame far past the point
/// where the retransmission could still be shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NackDelay {
    /// Never shorter than this, whatever the round trip.
    pub min: Duration,
    /// Never longer than this.
    pub max: Duration,
    /// The delay is `rtt / divisor` before the bounds apply. Zero means "always [`Self::min`]".
    pub divisor: u32,
}

impl NackDelay {
    /// The delay for a measured round trip.
    #[must_use]
    pub fn for_rtt(self, rtt: Duration) -> Duration {
        rtt.checked_div(self.divisor)
            .unwrap_or(self.min)
            .clamp(self.min, self.max)
    }
}

impl Default for NackDelay {
    fn default() -> Self {
        Self {
            min: Duration::from_millis(1),
            max: Duration::from_millis(20),
            divisor: 4,
        }
    }
}

/// Timing and bounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// Silence after the last fragment of an incomplete frame before a NACK goes out, derived
    /// from the round trip (see [`NackDelay`]).
    pub nack_delay: NackDelay,
    /// NACK attempts per frame before it is given up.
    pub nack_retries: u8,
    /// Slack added to the loss deadline on top of the NACK round trips.
    pub grace: Duration,
    /// Most frames tracked at once; the oldest is dropped beyond this.
    pub max_pending: usize,
    /// How often a refresh request is repeated while no refresh frame arrives (plus two RTTs).
    /// Each unanswered repeat doubles the wait, up to [`Self::refresh_repeat_max`], so a target
    /// that produces no frames at all (a display that does not change) is not asked eight times a
    /// second.
    pub refresh_repeat: Duration,
    /// Longest wait between refresh repeats.
    pub refresh_repeat_max: Duration,
    /// Unanswered refresh repeats before the receiver stops asking altogether, until something
    /// arrives on the stream again. The host's
    /// [`SourceState`](crate::wire::SourceState) hint is the real answer to a target
    /// that produces nothing; this is the fallback for a host too old or too silent to send
    /// one, so it is generous: with the doubling backoff it spans about 17 s of asking.
    pub refresh_max_repeats: u32,
    /// Longest an incomplete frame is held while nothing at all arrives on the stream (a
    /// stalled link); past this it is lost even though the deadline logic would keep waiting.
    pub max_hold: Duration,
    /// Shortest silence on the stream that counts as a stall when it ends (the effective gap is
    /// the larger of this and one NACK round trip). Must exceed a normal inter-frame gap, or
    /// every frame would restart the pending deadlines.
    pub stall_gap: Duration,
    /// How often the owner promises to call [`Reassembler::tick`]. Silence the receiver slept
    /// through past this is its own: a task that is not scheduled cannot tell a link that held
    /// datagrams from a socket it did not read, and charging its own load to the link is what
    /// makes a busy machine look like a congested network.
    ///
    /// Must be shorter than [`Self::stall_gap`], for the same reason the host beats twice per
    /// gap: a receiver that looks exactly as often as the thing it is looking for is long
    /// cannot resolve it, and a gap it slept through entirely would still read as a full
    /// stall's worth of link silence.
    pub tick_period: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nack_delay: NackDelay::default(),
            nack_retries: 2,
            grace: Duration::from_millis(10),
            max_pending: 64,
            refresh_repeat: Duration::from_millis(100),
            refresh_repeat_max: Duration::from_secs(2),
            refresh_max_repeats: 12,
            max_hold: Duration::from_millis(500),
            stall_gap: STALL_GAP,
            tick_period: HALF_STALL_GAP,
        }
    }
}

/// What a delivered frame is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameInfo {
    /// Frame number.
    pub frame: u32,
    /// IDR frame.
    pub keyframe: bool,
    /// LTR token to acknowledge once decoded.
    pub ltr_token: Option<u64>,
    /// Recovery frame from an acknowledged LTR.
    pub ltr_refresh: bool,
    /// Nothing later refers to this frame: a decoder that cannot use it needs no refresh.
    pub discardable: bool,
    /// Host capture timestamp, microseconds (low 32 bits).
    pub capture_ts_us: u32,
    /// The stripes coded from this capture, bit `i` for stripe `i`; zero for one picture
    /// ([`crate::wire::FramePrefix::stripes`]).
    pub stripes: u8,
    /// The build of the host's sessions that coded it
    /// ([`crate::wire::FramePrefix::build`]).
    pub build: u8,
    /// Needed parity or a retransmission.
    pub recovered: bool,
}

/// A complete frame in decode order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameOut {
    /// Metadata.
    pub info: FrameInfo,
    /// The bitstream: a view into the reassembled fragments, not a copy of them.
    pub data: Bytes,
    /// Time from the first fragment's arrival to delivery.
    pub hold: Duration,
    /// When the fragment that completed the frame arrived. This, not the delivery instant, is
    /// where the client's arrival → present clock starts: everything after it is the receiver's
    /// own doing.
    pub arrived: Instant,
}

/// Why a datagram was dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ignored {
    /// Header or payload did not make sense.
    Malformed,
    /// Another stream.
    Foreign,
    /// Older than what has been delivered or skipped.
    Stale,
    /// Already had it.
    Duplicate,
}

/// Result of feeding one datagram.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ingest {
    /// A video fragment was stored; call [`Reassembler::next_frame`] for anything now ready.
    Video,
    /// An audio packet.
    Audio {
        /// Packet sequence.
        seq: u32,
        /// Opus bytes.
        payload: Bytes,
        /// The packets before it the datagram carried again, nearest first
        /// ([`crate::audio_datagram`]).
        earlier: [Option<Bytes>; crate::MAX_AUDIO_COPIES],
    },
    /// A cursor move.
    Cursor {
        /// Update sequence; keep the highest.
        seq: u32,
        /// Position.
        update: CursorUpdate,
    },
    /// The host had nothing to send: the link moved, the stall clock restarted, nothing else.
    Heartbeat,
    /// Dropped.
    Ignored(Ignored),
}

/// Something to tell the host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Retransmit fragments (`fragments` empty: the whole frame).
    Nack {
        /// Frame.
        frame: u32,
        /// Missing data fragment indices.
        fragments: Vec<u16>,
    },
    /// A frame is lost for good; refresh from an acknowledged LTR (or send an IDR).
    RequestRefresh {
        /// Highest frame delivered.
        last_good_frame: u32,
        /// Only a keyframe will do: the stream has not started, or the decoder lost its session
        /// and with it every reference an LTR refresh would be predicted from.
        keyframe: bool,
    },
}

/// What the host's send stamps made of a silence in arrivals.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stamp {
    /// The host waited this long between sending the two datagrams; the rest of the silence is
    /// the link's.
    Host(Duration),
    /// The host's own interval covers the whole silence (and reads a little longer than it,
    /// within [`STAMP_SLACK`]): nothing is left for the link.
    Covered,
    /// Nothing to compare against: the stream just started, or a retransmission (which carries
    /// its original frame's stamp) cleared the pair.
    Absent,
    /// The silence outran the stamp byte's 256 ms range, so the difference between two stamps
    /// could name any of several intervals.
    Wrapped,
    /// The stamp read longer than the silence by more than [`STAMP_SLACK`], by this much: the
    /// datagram overtook its predecessor, and the unsigned subtraction wrapped.
    Backwards(Duration),
}

/// Where every silence past the stall threshold went.
///
/// A stall is meant to mean "the link held datagrams", and the host's send stamps are what
/// tells that from "the host had nothing to send" — with the receiver's own scheduling as the
/// third possibility. These counters say which of the three each silence was, so a stall count
/// can be read rather than guessed at. The first three are forgiven, `in_flight` is the one the
/// stamps prove was the link's, and the `stamp_*` three are charged to the link only because no
/// reading was available.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StallAttribution {
    /// Silences the stamps explained: the host itself was quiet. Not charged.
    pub host_quiet: u64,
    /// Silences the host's interval covered outright, give or take the stamp's own slack (see
    /// `STAMP_SLACK`). Not charged.
    pub host_covered: u64,
    /// Silences the receiver slept through, so it never observed the link at all. Not charged.
    pub receiver_dozed: u64,
    /// Silences the stamps prove were spent in flight. Charged.
    pub in_flight: u64,
    /// Silences longer than the stamp's 256 ms range. Charged for want of a reading.
    pub stamp_wrapped: u64,
    /// Silences whose stamp overtook its predecessor. Charged for want of a reading.
    pub stamp_backwards: u64,
    /// Silences with no stamp to compare against. Charged for want of a reading.
    pub stamp_absent: u64,
    /// Silences that began with no frame pending — nothing the link could have been holding.
    pub while_idle: u64,
    /// Longest silence seen, milliseconds.
    pub gap_ms_max: u64,
    /// Longest stretch of a silence the receiver's own loop slept through, milliseconds.
    pub dozed_ms_max: u64,
    /// Silences a heartbeat ended.
    pub ended_heartbeat: u64,
    /// Silences a video fragment ended.
    pub ended_video: u64,
    /// Silences a cursor update or an audio packet ended.
    pub ended_other: u64,
}

/// Lifetime counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReassemblerStats {
    /// Frames delivered.
    pub frames_ok: u64,
    /// Delivered frames that needed parity.
    pub frames_fec: u64,
    /// Delivered frames that needed a retransmission.
    pub frames_retransmit: u64,
    /// Frames given up on.
    pub frames_lost: u64,
    /// Frames nothing refers to that were skipped rather than repaired: no refresh, no wait.
    pub frames_skipped: u64,
    /// Fragments that never arrived (counted when a frame resolves).
    pub datagrams_lost: u64,
    /// Data fragments the host cut frames into (counted once per frame seen).
    pub data_shards: u64,
    /// Parity fragments it added to them: the ratio is the parity the host settled on.
    pub parity_shards: u64,
    /// NACKs sent.
    pub nacks: u64,
    /// Refreshes asked for: one each time the stream comes to need a picture to restart from,
    /// however often that one is asked again.
    pub refreshes: u64,
    /// A refresh asked again while it went unanswered ([`Config::refresh_repeat`]).
    pub refresh_repeats: u64,
    /// Stalls that released (silence past the stall gap, then datagrams again).
    pub stalls: u64,
    /// Time spent stalled, in milliseconds (released stalls plus the one in progress, if any).
    pub stalled_ms: u64,
    /// Where every silence past the stall gap went, stalls and forgiven silences alike.
    pub silences: StallAttribution,
}

struct Partial {
    data_count: usize,
    parity_count: usize,
    shard_bytes: usize,
    flags: u8,
    shards: Vec<Option<Bytes>>,
    have_data: usize,
    have_total: usize,
    first_seen: Instant,
    last_seen: Instant,
    nacks: u8,
    nacked_at: Option<Instant>,
    retransmitted: bool,
    /// Nothing later refers to it: its own flags said so, or the next frame's did.
    discardable: bool,
}

impl Partial {
    fn new(header: &MediaHeader, shard_bytes: usize, now: Instant, discardable: bool) -> Self {
        let data_count = usize::from(header.data_count.get());
        let parity_count = usize::from(header.parity_count);
        Self {
            data_count,
            parity_count,
            shard_bytes,
            flags: header.flags,
            shards: vec![None; data_count.saturating_add(parity_count)],
            have_data: 0,
            have_total: 0,
            first_seen: now,
            last_seen: now,
            nacks: 0,
            nacked_at: None,
            retransmitted: false,
            discardable: discardable || header.flags & flags::DISCARDABLE != 0,
        }
    }

    const fn complete(&self) -> bool {
        self.have_data == self.data_count
            || (self.parity_count > 0 && self.have_total >= self.data_count)
    }

    fn missing_data(&self) -> Vec<u16> {
        self.shards
            .iter()
            .take(self.data_count)
            .enumerate()
            .filter(|(_i, s)| s.is_none())
            .map(|(i, _s)| u16::try_from(i).unwrap_or(u16::MAX))
            .collect()
    }

    /// Data fragments that never arrived. Parity still in flight when the frame completes is
    /// not counted: it may yet arrive, and losing it costs nothing.
    const fn missing_data_count(&self) -> u64 {
        self.data_count.saturating_sub(self.have_data) as u64
    }

    const fn restartable(&self, accept_refresh: bool) -> bool {
        self.flags & flags::KEYFRAME != 0
            || (accept_refresh && self.flags & flags::LTR_REFRESH != 0)
    }
}

enum Slot {
    /// Nothing of this frame arrived yet, but a later frame did.
    Unknown {
        first_seen: Instant,
        nacks: u8,
        nacked_at: Option<Instant>,
        /// The next frame said nothing refers to this one.
        discardable: bool,
    },
    Partial(Partial),
    Complete {
        out: FrameOut,
        first_seen: Instant,
    },
    /// A frame nothing refers to, given up on without a refresh: delivery steps over it.
    Skipped,
}

impl Slot {
    const fn unknown(now: Instant) -> Self {
        Self::Unknown {
            first_seen: now,
            nacks: 0,
            nacked_at: None,
            discardable: false,
        }
    }

    /// Nothing more will come of it: delivered-ready or stepped over.
    const fn settled(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Skipped)
    }

    /// Still waited for, and nothing later refers to it.
    const fn discardable(&self) -> bool {
        match self {
            Self::Unknown { discardable, .. } => *discardable,
            Self::Partial(p) => p.discardable,
            Self::Complete { .. } | Self::Skipped => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    Keyframe,
    Refresh,
    Frame(u32),
}

#[derive(Default)]
struct Window {
    frames_ok: u32,
    frames_fec: u32,
    frames_lost: u32,
    datagrams_lost: u32,
    holds_ns: Vec<u64>,
    /// Silence charged to this window (see `Reassembler::charge_stall`).
    stalled: Duration,
    /// Stalls that released in this window.
    stalls: u16,
}

/// Reassembles one stream's video datagrams and drives its loss policy.
pub struct Reassembler {
    stream: StreamId,
    cfg: Config,
    epoch: Instant,
    frames: BTreeMap<u32, Slot>,
    need: Need,
    last_good: Option<u32>,
    /// The frames given up on lately, at most [`Config::max_pending`]. Their fragments still in
    /// flight are stale whatever the receiver waits for: while it waits for a keyframe or a
    /// refresh nothing else marks them so, and each one opened its frame again, to be lost again
    /// and asked for again.
    given_up: VecDeque<u32>,
    ready: VecDeque<FrameOut>,
    decoder: Option<ReedSolomonDecoder>,
    actions: Vec<Action>,
    refresh_requested_at: Option<Instant>,
    /// Unanswered refresh repeats in a row since the last datagram of any kind: the give-up
    /// count ([`Config::refresh_max_repeats`]).
    refresh_repeats: u32,
    /// Repeats of the refresh being asked for: the backoff's exponent. Only an answer, or the
    /// host saying its source draws again, starts it over. A datagram does not, since frames
    /// predicted from before the loss keep coming while the answer is made, and a backoff they
    /// reset asked again every 100 ms for as long as the answer took.
    refresh_backoff: u32,
    /// What the host last said its capture target is doing. `false` means the target has drawn
    /// nothing at all, so no refresh can produce a frame and asking is pure noise.
    source_live: bool,
    stats: ReassemblerStats,
    window: Window,
    jitter_us: i64,
    last_arrival: Option<(u64, u32)>,
    /// When the last datagram of this stream (of any kind) came in.
    arrived_at: Instant,
    /// Silence before this instant has already been charged to a report window.
    stall_charged_to: Instant,
    /// A video datagram of this stream has arrived at least once; before that, silence is the
    /// stream starting up (the cursor arrives before the first frame), not a stall.
    any_arrived: bool,
    /// A heartbeat said the host sent a picture before any of the stream's video came: the
    /// first picture was lost whole. Until then a new stream asks for nothing, since the host
    /// is still making the keyframe that starts it, and a question would only have it make a
    /// second (`heartbeat_datagram` names the next frame the host will send).
    first_lost: bool,
    /// The host's send stamp on the last datagram that carried a fresh one, so the silence the
    /// host made itself can be told from the silence the link made (see `link_gap`).
    last_send_ms_lo: Option<u8>,
    /// When the receiver's own loop last ran — a `tick` or an `ingest`, since either is proof
    /// it was scheduled — so a silence it slept through is not charged to the link (see `dozed`).
    last_tick_at: Instant,
    /// Sleep already banked against the silence in progress. A receiver that wakes after a long
    /// doze runs whatever its executor polls first, and a ready timer is as likely as the socket:
    /// if `tick` went first it would move `last_tick_at` to now and the ingest a moment later
    /// would find nothing slept through and charge the whole gap to the link. `tick` banks the
    /// stretch here instead, and the arrival that ends the silence spends it.
    dozed_since_arrival: Duration,
    /// Where the banked sleep begins, so the part of it that could fall inside the host's own
    /// silence can be told from the part that could not (see `doze_beyond_host`).
    dozed_from: Option<Instant>,
    /// How much of the bank has already been forgiven by a charge. A report every 50 ms would
    /// otherwise spend the same sleep again in every window, and a stall that stays on would
    /// read as healthy from the second report onwards.
    dozed_spent: Duration,
    /// Round trip the last `tick` was given; sizes the gap that counts as a stall.
    last_rtt: Duration,
    /// [`Config::nack_delay`] evaluated for `last_rtt`, so `stalled` and `resume` (which have no
    /// round trip to hand) use the same number `tick` does.
    nack_delay: Duration,
    last_host_ts_us: u32,
    /// Tokens of decoded frames not yet in a report, oldest first.
    ltr_acks: VecDeque<u64>,
    /// The newest token acknowledged, repeated in every report that delivered a frame (see
    /// [`Reassembler::take_report`]).
    newest_ack: Option<u64>,
}

impl std::fmt::Debug for Reassembler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reassembler")
            .field("stream", &self.stream)
            .field("need", &self.need)
            .field("pending", &self.frames.len())
            .field("ready", &self.ready.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Reassembler {
    /// A reassembler for one stream. `now` anchors relative timestamps.
    #[must_use]
    pub fn new(stream: StreamId, cfg: Config, now: Instant) -> Self {
        let last_rtt = Duration::from_millis(20);
        Self {
            stream,
            cfg,
            epoch: now,
            frames: BTreeMap::new(),
            need: Need::Keyframe,
            last_good: None,
            given_up: VecDeque::new(),
            ready: VecDeque::new(),
            decoder: None,
            actions: Vec::new(),
            refresh_requested_at: Some(now),
            refresh_repeats: 0,
            refresh_backoff: 0,
            source_live: true,
            stats: ReassemblerStats::default(),
            window: Window::default(),
            jitter_us: 0,
            last_arrival: None,
            arrived_at: now,
            stall_charged_to: now,
            any_arrived: false,
            first_lost: false,
            last_send_ms_lo: None,
            last_tick_at: now,
            dozed_since_arrival: Duration::ZERO,
            dozed_from: None,
            dozed_spent: Duration::ZERO,
            last_rtt,
            nack_delay: cfg.nack_delay.for_rtt(last_rtt),
            last_host_ts_us: 0,
            ltr_acks: VecDeque::new(),
            newest_ack: None,
        }
    }

    /// Lifetime counters.
    #[must_use]
    pub const fn stats(&self) -> ReassemblerStats {
        self.stats
    }

    /// True while waiting for an IDR or refresh frame.
    #[must_use]
    pub const fn awaiting_refresh(&self) -> bool {
        !matches!(self.need, Need::Frame(_))
    }

    /// The host said whether its capture target is producing pictures.
    ///
    /// While it is not, refresh requests stop: a display that has not drawn has nothing to refresh
    /// from, and asking every backoff period for as long as the item is open is the storm this
    /// exists to prevent. Going live restarts the backoff, so the first refresh after the display
    /// draws goes out immediately.
    ///
    /// The statement also outranks arriving video until it is taken back. The host says this
    /// once per change, on the control stream, while frames travel as datagrams: without that
    /// ordering a fragment sent before the window went away would put the receiver back to live
    /// behind the host's back, and since every datagram (heartbeats included) restarts the
    /// refresh cap, one lost frame after that would ask for a refresh forever.
    pub const fn set_source_live(&mut self, live: bool) {
        if self.source_live != live {
            self.source_live = live;
            if live {
                self.refresh_repeats = 0;
                self.refresh_backoff = 0;
                self.refresh_requested_at = None;
            }
        }
    }

    /// Whether the host's target is producing pictures, as last reported.
    #[must_use]
    pub const fn source_live(&self) -> bool {
        self.source_live
    }

    /// Frames complete and waiting to be taken plus frames still being assembled.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.ready.len().saturating_add(self.frames.len())
    }

    /// The next frame in decode order, if one is ready.
    pub fn next_frame(&mut self) -> Option<FrameOut> {
        self.ready.pop_front()
    }

    /// Record that a frame carrying `token` was decoded; the next report acknowledges it.
    ///
    /// Call it from the decoder's output, never on submission: a token acknowledged for a frame
    /// the decoder then failed names a reference the client does not hold, and the host's
    /// next refresh would be predicted from it.
    pub fn ack_ltr(&mut self, token: u64) {
        if self.ltr_acks.len() >= MAX_PENDING_ACKS {
            self.ltr_acks.pop_front();
        }
        self.ltr_acks.push_back(token);
        self.newest_ack = Some(token);
    }

    /// The decoder could not use what it was given: ask for a picture it can decode, and drop
    /// every frame until one comes. `keyframe` says the decoder lost its session, so a refresh
    /// predicted from a long-term reference cannot help and only an IDR restarts it.
    ///
    /// A receiver already waiting for what this asks for asks nothing more here; its repeats run
    /// on the usual backoff, so a decoder failing on every frame cannot turn into a request per
    /// frame. One waiting for a refresh when the session goes asks for a keyframe at once.
    pub fn force_refresh(&mut self, now: Instant, keyframe: bool) {
        // A wait for a refresh that becomes a wait for a keyframe asks again at once: what was
        // asked for is no longer something the decoder can use.
        let waiting = self.awaiting_refresh() && (!keyframe || self.need == Need::Keyframe);
        self.need = if keyframe || self.need == Need::Keyframe {
            Need::Keyframe
        } else {
            Need::Refresh
        };
        self.ready.clear();
        if keyframe {
            // References the lost session decoded are not held by the next one.
            self.ltr_acks.clear();
            self.newest_ack = None;
        }
        if !waiting {
            self.refresh_requested_at = None;
            self.refresh_backoff = 0;
            self.request_refresh(now, false);
        }
    }

    /// A report that could not be sent: its counts and acknowledgements go into the next one,
    /// so the host neither misses a loss nor a reference it may predict from.
    pub fn take_back(&mut self, report: &ReceiverReport) {
        let tokens = report
            .acked_ltr
            .iter()
            .take(usize::from(report.acked_ltr_len));
        for &token in tokens.rev() {
            if !self.ltr_acks.contains(&token) {
                self.ltr_acks.push_front(token);
            }
        }
        while self.ltr_acks.len() > MAX_PENDING_ACKS {
            self.ltr_acks.pop_front();
        }
        let window = &mut self.window;
        window.frames_ok = window.frames_ok.saturating_add(report.frames_ok);
        window.frames_fec = window.frames_fec.saturating_add(report.frames_fec);
        window.frames_lost = window.frames_lost.saturating_add(report.frames_lost);
        window.datagrams_lost = window.datagrams_lost.saturating_add(report.datagrams_lost);
        window.stalled = window
            .stalled
            .saturating_add(Duration::from_millis(u64::from(report.stalled_ms)));
        window.stalls = window.stalls.saturating_add(report.stalls);
    }

    /// Feed one datagram.
    pub fn ingest(&mut self, datagram: &Bytes, now: Instant) -> Ingest {
        let Some((header, _payload)) = MediaHeader::parse(datagram) else {
            return Ingest::Ignored(Ignored::Malformed);
        };
        if header.stream.get() != self.stream.0 {
            return Ingest::Ignored(Ignored::Foreign);
        }
        let gap = now.saturating_duration_since(self.arrived_at);
        let stamp = self.read_stamp(header, gap);
        let host_gap = Self::host_share(stamp, gap);
        let dozed = self.doze_beyond_host(now, host_gap);
        let unexplained = gap.saturating_sub(host_gap);
        let link_gap = unexplained.saturating_sub(dozed);
        if self.any_arrived && gap >= self.stall_threshold() {
            self.attribute(gap, stamp, dozed, header.kind());
        }
        // Deadlines restart on any stretch in which nothing could have arrived, whether the
        // link held it or the receiver was not awake to read it: a NACK written before either
        // is answered no sooner than a round trip from now.
        if self.any_arrived && unexplained >= self.stall_threshold() {
            self.resume(now, gap);
        }
        if self.any_arrived && link_gap >= self.stall_threshold() {
            tracing::debug!(
                stream = %self.stream,
                ?gap,
                ?host_gap,
                ?dozed,
                ?link_gap,
                ?stamp,
                ended_by = ?header.kind(),
                pending = self.frames.len(),
                "stall released: the link held datagrams the host had already sent"
            );
            self.charge_stall(now, host_gap);
            self.window.stalls = self.window.stalls.saturating_add(1);
            self.stats.stalls = self.stats.stalls.saturating_add(1);
        }
        // The stamp and `arrived_at` have to name the same datagram, or the next gap subtracts a
        // host interval that spans a different pair. A retransmission advances the arrival but
        // carries its original frame's stamp, so it clears the pair instead of updating it: the
        // next datagram is measured against no stamp at all, which is the pessimistic reading.
        self.last_send_ms_lo = (header.flags & flags::RETRANSMIT == 0).then_some(header.send_ms_lo);
        self.arrived_at = now;
        // The silence is over and its account is settled: the next one starts from this arrival
        // with nothing banked and the loop's mark here, not wherever the last `tick` left it.
        self.dozed_since_arrival = Duration::ZERO;
        self.dozed_from = None;
        self.dozed_spent = Duration::ZERO;
        self.last_tick_at = self.last_tick_at.max(now);
        // Anything on the stream — a heartbeat included — proves the host is still there, so the
        // refresh cap starts over; only a video fragment proves the *source* is drawing.
        self.refresh_repeats = 0;
        let header = *header;
        let payload = datagram.slice(HEADER_BYTES..);
        match header.kind() {
            None => Ingest::Ignored(Ignored::Malformed),
            Some(Kind::Audio) => {
                let seq = header.frame.get();
                match crate::parse_audio(header.data_count.get(), &payload) {
                    Some(packets) => Ingest::Audio {
                        seq,
                        payload: packets.packet,
                        earlier: packets.earlier,
                    },
                    None => Ingest::Ignored(Ignored::Malformed),
                }
            }
            // A beat names the next frame the host will send: past zero with none of the
            // stream's video seen, the first picture was lost whole, and is asked for at once.
            Some(Kind::Heartbeat) => {
                if !self.any_arrived && !self.first_lost && header.frame.get() != 0 {
                    self.first_lost = true;
                    self.refresh_requested_at = None;
                }
                Ingest::Heartbeat
            }
            // A clock echo is the client's to read before this; here it only says the link is up.
            Some(Kind::Clock) => Ingest::Heartbeat,
            Some(Kind::Cursor) => {
                parse_cursor(&payload).map_or(Ingest::Ignored(Ignored::Malformed), |update| {
                    Ingest::Cursor {
                        seq: header.frame.get(),
                        update,
                    }
                })
            }
            Some(Kind::VideoData | Kind::VideoParity) => self.ingest_video(&header, payload, now),
        }
    }

    fn ingest_video(&mut self, header: &MediaHeader, payload: Bytes, now: Instant) -> Ingest {
        // A picture never changes what the host said about its source: one that arrives after
        // the source was called idle was captured before it stopped and proves nothing about
        // now, and only the host's word makes it live again (`set_source_live`).
        self.any_arrived = true;
        let frame = header.frame.get();
        let data_count = usize::from(header.data_count.get());
        let total = data_count.saturating_add(usize::from(header.parity_count));
        let index = usize::from(header.index.get());
        let valid = data_count > 0
            && index < total
            && !payload.is_empty()
            && payload.len().is_multiple_of(2)
            && payload.len() <= MAX_PAYLOAD
            && header.is_parity() == (index >= data_count);
        if !valid {
            return Ingest::Ignored(Ignored::Malformed);
        }
        // A frame already delivered is stale whatever is needed now. Waiting on a keyframe or a
        // refresh, a repeat or a retransmission of one delivered before made it whole again,
        // and it was delivered a second time, behind a newer one (fuzz, `reassemble`).
        if self.given_up.contains(&frame) || self.last_good.is_some_and(|good| frame <= good) {
            return Ingest::Ignored(Ignored::Stale);
        }
        if let Need::Frame(next) = self.need {
            if frame < next {
                return Ingest::Ignored(Ignored::Stale);
            }
            let gap = usize::try_from(frame.wrapping_sub(next)).unwrap_or(usize::MAX);
            if gap > self.cfg.max_pending {
                // A whole second of frames vanished: no point NACKing each one.
                self.stats.frames_lost = self.stats.frames_lost.saturating_add(gap as u64);
                self.window.frames_lost = self
                    .window
                    .frames_lost
                    .saturating_add(u32::try_from(gap).unwrap_or(u32::MAX));
                self.frames.retain(|f, _| *f >= frame);
                self.enter_refresh(now);
            } else {
                for missing in next..frame {
                    self.frames
                        .entry(missing)
                        .or_insert_with(|| Slot::unknown(now));
                }
            }
        }
        if header.flags & flags::PREV_DISCARDABLE != 0
            && let Some(previous) = frame.checked_sub(1)
        {
            match self.frames.get_mut(&previous) {
                Some(Slot::Unknown { discardable, .. }) => *discardable = true,
                Some(Slot::Partial(p)) => p.discardable = true,
                Some(Slot::Complete { .. } | Slot::Skipped) | None => {}
            }
        }

        let slot = self
            .frames
            .entry(frame)
            .or_insert_with(|| Slot::unknown(now));
        if let Slot::Unknown { discardable, .. } = *slot {
            *slot = Slot::Partial(Partial::new(header, payload.len(), now, discardable));
            self.stats.data_shards = self
                .stats
                .data_shards
                .saturating_add(u64::try_from(data_count).unwrap_or(0));
            self.stats.parity_shards = self
                .stats
                .parity_shards
                .saturating_add(u64::try_from(total.saturating_sub(data_count)).unwrap_or(0));
        }
        let Slot::Partial(partial) = slot else {
            return Ingest::Ignored(Ignored::Duplicate);
        };
        if partial.data_count != data_count
            || partial.shards.len() != total
            || partial.shard_bytes != payload.len()
        {
            return Ingest::Ignored(Ignored::Malformed);
        }
        let Some(cell) = partial.shards.get_mut(index) else {
            return Ingest::Ignored(Ignored::Malformed);
        };
        if cell.is_some() {
            return Ingest::Ignored(Ignored::Duplicate);
        }
        *cell = Some(payload);
        partial.have_total = partial.have_total.saturating_add(1);
        if index < data_count {
            partial.have_data = partial.have_data.saturating_add(1);
        }
        partial.last_seen = now;
        if header.flags & flags::RETRANSMIT != 0 {
            partial.retransmitted = true;
        }
        if partial.complete() {
            self.finish(frame, now);
        }
        self.drain(now);
        self.evict(now);
        Ingest::Video
    }

    /// Move a complete partial into a `Complete` slot, or count it lost if it is corrupt.
    fn finish(&mut self, frame: u32, now: Instant) {
        let Some(Slot::Partial(partial)) = self.frames.remove(&frame) else {
            return;
        };
        let first_seen = partial.first_seen;
        let missing = partial.missing_data_count();
        match assemble(&mut self.decoder, frame, &partial, now) {
            Some(out) => {
                if out.info.recovered {
                    self.window.frames_fec = self.window.frames_fec.saturating_add(1);
                    if partial.have_data < partial.data_count {
                        self.stats.frames_fec = self.stats.frames_fec.saturating_add(1);
                    }
                    if partial.retransmitted {
                        self.stats.frames_retransmit =
                            self.stats.frames_retransmit.saturating_add(1);
                    }
                }
                self.count_lost_datagrams(missing);
                self.frames
                    .insert(frame, Slot::Complete { out, first_seen });
            }
            None => self.lose(frame, missing, now),
        }
    }

    fn count_lost_datagrams(&mut self, missing: u64) {
        self.stats.datagrams_lost = self.stats.datagrams_lost.saturating_add(missing);
        self.window.datagrams_lost = self
            .window
            .datagrams_lost
            .saturating_add(u32::try_from(missing).unwrap_or(u32::MAX));
    }

    /// Give up on `frame`: skip it when nothing refers to it, otherwise lose it and ask for a
    /// refresh.
    fn lose(&mut self, frame: u32, missing: u64, now: Instant) {
        if self.frames.get(&frame).is_some_and(Slot::discardable) {
            self.skip(frame, missing);
            return;
        }
        self.frames.remove(&frame);
        self.give_up(frame);
        self.stats.frames_lost = self.stats.frames_lost.saturating_add(1);
        self.window.frames_lost = self.window.frames_lost.saturating_add(1);
        self.count_lost_datagrams(missing);
        match self.need {
            Need::Frame(_) => self.enter_refresh(now),
            Need::Keyframe | Need::Refresh => self.request_refresh(now, true),
        }
    }

    /// Step over `frame`, which nothing refers to: no refresh, and delivery goes on past it.
    fn skip(&mut self, frame: u32, missing: u64) {
        tracing::debug!(frame, missing, "frame nothing refers to skipped");
        self.frames.insert(frame, Slot::Skipped);
        self.give_up(frame);
        self.stats.frames_skipped = self.stats.frames_skipped.saturating_add(1);
        self.count_lost_datagrams(missing);
    }

    /// Nothing more of `frame` is wanted.
    fn give_up(&mut self, frame: u32) {
        if self.given_up.len() >= self.cfg.max_pending.max(1) {
            self.given_up.pop_front();
        }
        self.given_up.push_back(frame);
    }

    fn enter_refresh(&mut self, now: Instant) {
        self.need = Need::Refresh;
        self.refresh_requested_at = None;
        self.refresh_backoff = 0;
        self.request_refresh(now, false);
    }

    /// Ask the host for a picture to restart from; `again` when it is the one already asked
    /// for, unanswered.
    fn request_refresh(&mut self, now: Instant, again: bool) {
        tracing::debug!(
            stream = %self.stream,
            need = ?self.need,
            arrived = self.any_arrived,
            again,
            backoff = self.refresh_backoff,
            "a picture asked for"
        );
        self.refresh_requested_at = Some(now);
        if again {
            self.stats.refresh_repeats = self.stats.refresh_repeats.saturating_add(1);
        } else {
            self.stats.refreshes = self.stats.refreshes.saturating_add(1);
        }
        self.actions.push(Action::RequestRefresh {
            last_good_frame: self.last_good.unwrap_or(0),
            keyframe: self.need == Need::Keyframe,
        });
    }

    /// Deliver everything that is in order (or restarts the stream).
    fn drain(&mut self, now: Instant) {
        loop {
            let next = match self.need {
                Need::Frame(n) => match self.frames.get(&n) {
                    Some(Slot::Complete { .. }) => n,
                    Some(Slot::Skipped) => {
                        self.frames.remove(&n);
                        self.need = Need::Frame(n.wrapping_add(1));
                        continue;
                    }
                    // A later picture is ready: showing it beats waiting for one nothing needs.
                    Some(slot) if slot.discardable() && self.complete_after(n) => {
                        let missing = match slot {
                            Slot::Partial(p) => p.missing_data_count(),
                            _ => 1,
                        };
                        self.skip(n, missing);
                        continue;
                    }
                    _ => break,
                },
                Need::Keyframe | Need::Refresh => {
                    let accept_refresh = matches!(self.need, Need::Refresh);
                    let found = self.frames.iter().find_map(|(f, slot)| match slot {
                        Slot::Complete { out, .. }
                            if out.info.keyframe || (accept_refresh && out.info.ltr_refresh) =>
                        {
                            Some(*f)
                        }
                        _ => None,
                    });
                    let Some(f) = found else { break };
                    self.frames.retain(|k, _| *k >= f);
                    self.refresh_requested_at = None;
                    self.refresh_backoff = 0;
                    f
                }
            };
            let Some(Slot::Complete {
                mut out,
                first_seen,
            }) = self.frames.remove(&next)
            else {
                break;
            };
            out.hold = now.saturating_duration_since(first_seen);
            self.record_delivery(&out, now);
            self.ready.push_back(out);
            self.last_good = Some(next);
            self.need = Need::Frame(next.wrapping_add(1));
        }
    }

    /// Whether a frame after `frame` is complete.
    fn complete_after(&self, frame: u32) -> bool {
        self.frames
            .range(frame.saturating_add(1)..)
            .any(|(_, slot)| matches!(slot, Slot::Complete { .. }))
    }

    fn record_delivery(&mut self, out: &FrameOut, now: Instant) {
        self.stats.frames_ok = self.stats.frames_ok.saturating_add(1);
        self.window.frames_ok = self.window.frames_ok.saturating_add(1);
        self.window
            .holds_ns
            .push(u64::try_from(out.hold.as_nanos()).unwrap_or(u64::MAX));
        self.last_host_ts_us = out.info.capture_ts_us;
        let arrival_us = u64::try_from(now.saturating_duration_since(self.epoch).as_micros())
            .unwrap_or(u64::MAX);
        if let Some((prev_arrival, prev_ts)) = self.last_arrival {
            // RFC 3550 interarrival jitter, on the host's capture clock.
            let transit =
                i64::try_from(arrival_us.saturating_sub(prev_arrival)).unwrap_or(i64::MAX);
            let sent = i64::from(out.info.capture_ts_us.wrapping_sub(prev_ts));
            let d = i64::try_from(transit.saturating_sub(sent).unsigned_abs()).unwrap_or(i64::MAX);
            self.jitter_us = self
                .jitter_us
                .saturating_add(d.saturating_sub(self.jitter_us) / 16);
        }
        self.last_arrival = Some((arrival_us, out.info.capture_ts_us));
    }

    /// Drop the oldest frames beyond `max_pending`.
    fn evict(&mut self, now: Instant) {
        while self.frames.len() > self.cfg.max_pending {
            let Some((&frame, slot)) = self.frames.first_key_value() else {
                break;
            };
            let missing = match slot {
                Slot::Complete { .. } => {
                    self.frames.remove(&frame);
                    continue;
                }
                Slot::Skipped => {
                    self.frames.remove(&frame);
                    if self.need == Need::Frame(frame) {
                        self.need = Need::Frame(frame.wrapping_add(1));
                    }
                    continue;
                }
                Slot::Unknown { .. } => 1,
                Slot::Partial(p) => p.missing_data_count(),
            };
            self.lose(frame, missing, now);
        }
    }

    /// Silence on the stream that counts as a stall: one NACK round trip, at least
    /// [`Config::stall_gap`].
    fn stall_threshold(&self) -> Duration {
        self.last_rtt
            .saturating_add(self.nack_delay)
            .max(self.cfg.stall_gap)
    }

    /// The NACK delay in force, as derived from the last round trip the policy was given.
    #[must_use]
    pub const fn nack_delay(&self) -> Duration {
        self.nack_delay
    }

    /// How much of a `gap` in arrivals the host made itself, read off its own send clock.
    ///
    /// Every datagram carries the low byte of the host's millisecond clock, so the difference
    /// between two stamps is how long the host waited between sending them. A source with
    /// nothing to draw waits; a link that holds datagrams and releases them together does not,
    /// and its stamps come out bunched. Subtracting the host's share is what tells the two
    /// apart — the heartbeat is the same statement in datagram form, and this reads it even
    /// when the beat that would have carried it was late.
    ///
    /// Unreadable when the stamp cannot be trusted: no previous stamp, a gap past the
    /// byte's range, a retransmission (which carries the original frame's stamp), or a stamp
    /// that reads as *older* than the one before it. The caller then charges the whole gap to
    /// the link, which is the pessimistic reading this had before stamps existed.
    ///
    /// The last case is what a reordered or delayed datagram looks like: the subtraction is
    /// unsigned and wraps, so a stamp 10 ms behind its predecessor reads as 246 ms ahead and
    /// would forgive a real stall. There is no bit that says which it is, but a host interval
    /// longer than the arrival gap it is supposed to explain is not evidence of anything —
    /// the host cannot have spent longer not sending than the receiver spent not receiving.
    fn read_stamp(&self, header: &MediaHeader, gap: Duration) -> Stamp {
        if header.flags & flags::RETRANSMIT != 0 {
            return Stamp::Absent;
        }
        let Some(previous) = self.last_send_ms_lo else {
            return Stamp::Absent;
        };
        if gap >= SEND_STAMP_RANGE {
            return Stamp::Wrapped;
        }
        let host_gap = Duration::from_millis(u64::from(header.send_ms_lo.wrapping_sub(previous)));
        match host_gap.checked_sub(gap).filter(|by| !by.is_zero()) {
            None => Stamp::Host(host_gap),
            Some(by) if by <= STAMP_SLACK => Stamp::Covered,
            Some(by) => Stamp::Backwards(by),
        }
    }

    /// The host's share of a silence, as the stamps read it. Zero when they cannot be read,
    /// which is the pessimistic reading: the whole silence goes to the link.
    const fn host_share(stamp: Stamp, gap: Duration) -> Duration {
        match stamp {
            Stamp::Host(host) => host,
            Stamp::Covered => gap,
            Stamp::Absent | Stamp::Wrapped | Stamp::Backwards(_) => Duration::ZERO,
        }
    }

    /// How much of a silence ending at `arrival` the receiver's own loop slept through: the
    /// stretch since its last [`Self::tick`] beyond the cadence [`Config::tick_period`]
    /// promises. A receiver that is not scheduled sees the same thing a held link produces —
    /// nothing, then everything — so this part of a silence is evidence about the machine, not
    /// about the network, and is subtracted before anything is charged.
    fn dozed(&self, arrival: Instant) -> Duration {
        self.doze(arrival).0
    }

    /// The sleep in this silence as of `arrival`: how much, and when the first of it began.
    /// The banked stretches and the unbanked tail are disjoint and in order, so the total is
    /// their sum and the start is the earliest of them.
    fn doze(&self, arrival: Instant) -> (Duration, Option<Instant>) {
        let tail = self.dozed_since_tick(arrival);
        let tail_from = (!tail.is_zero()).then(|| self.doze_tail_from());
        (
            self.dozed_since_arrival.saturating_add(tail),
            self.dozed_from.or(tail_from),
        )
    }

    /// Where the sleep since the last `tick` begins: one cadence period after that tick, which
    /// is the moment the loop broke its promise.
    fn doze_tail_from(&self) -> Instant {
        self.last_tick_at
            .checked_add(self.cfg.tick_period)
            .unwrap_or(self.last_tick_at)
    }

    /// The sleep that cannot have been the host's, given the host's own share of the silence.
    ///
    /// The two shares are not disjoint: the host's silence runs from the last arrival for
    /// `host_gap`, and the receiver may have slept through part of exactly that stretch.
    /// Forgiving their sum would excuse the overlap twice and let a real hold through — a host
    /// quiet for 100 ms, then a link holding the next datagram for 100 ms, with 75 ms of sleep
    /// inside the host's half, would read as 25 ms of link time instead of 100. Only the sleep
    /// past the end of the host's silence is credited.
    fn doze_beyond_host(&self, arrival: Instant, host_gap: Duration) -> Duration {
        let (total, from) = self.doze(arrival);
        let Some(from) = from else {
            return Duration::ZERO;
        };
        let host_until = self
            .arrived_at
            .checked_add(host_gap)
            .unwrap_or(self.arrived_at);
        total.saturating_sub(host_until.saturating_duration_since(from))
    }

    /// The part of [`Self::dozed`] not yet banked by [`Self::tick`]: the stretch since the last
    /// one beyond the cadence [`Config::tick_period`] promises.
    fn dozed_since_tick(&self, arrival: Instant) -> Duration {
        arrival
            .saturating_duration_since(self.last_tick_at)
            .saturating_sub(self.cfg.tick_period)
    }

    /// File a silence past the stall threshold under what the stamps made of it, so the stall
    /// count can be read back as a cause rather than a number (see [`StallAttribution`]).
    fn attribute(&mut self, gap: Duration, stamp: Stamp, dozed: Duration, kind: Option<Kind>) {
        let threshold = self.stall_threshold();
        let host = Self::host_share(stamp, gap);
        let pending = self.frames.values().any(|s| !s.settled());
        let a = &mut self.stats.silences;
        a.gap_ms_max = a
            .gap_ms_max
            .max(u64::try_from(gap.as_millis()).unwrap_or(u64::MAX));
        a.dozed_ms_max = a
            .dozed_ms_max
            .max(u64::try_from(dozed.as_millis()).unwrap_or(u64::MAX));
        if !pending {
            a.while_idle = a.while_idle.saturating_add(1);
        }
        let counter = if gap.saturating_sub(host) < threshold {
            match stamp {
                Stamp::Covered => &mut a.host_covered,
                _ => &mut a.host_quiet,
            }
        } else if gap.saturating_sub(host).saturating_sub(dozed) < threshold {
            &mut a.receiver_dozed
        } else {
            match stamp {
                Stamp::Host(_) | Stamp::Covered => &mut a.in_flight,
                Stamp::Absent => &mut a.stamp_absent,
                Stamp::Wrapped => &mut a.stamp_wrapped,
                Stamp::Backwards(_) => &mut a.stamp_backwards,
            }
        };
        *counter = counter.saturating_add(1);
        let ended = match kind {
            Some(Kind::Heartbeat) => &mut a.ended_heartbeat,
            Some(Kind::VideoData | Kind::VideoParity) => &mut a.ended_video,
            _ => &mut a.ended_other,
        };
        *ended = ended.saturating_add(1);
    }

    /// Whether nothing has arrived for a stall's worth of time as of `now` (never before the
    /// stream's first video datagram: that wait is the host starting the stream, never while
    /// the host says its source is idle — silence from a display that is not drawing is the
    /// source's, not the link's — and never for the stretch the receiver itself slept through).
    #[must_use]
    pub fn stalled(&self, now: Instant) -> bool {
        self.any_arrived
            && self.source_live
            && now
                .saturating_duration_since(self.arrived_at)
                .saturating_sub(self.dozed(now))
                >= self.stall_threshold()
    }

    /// Charge the silence since the last datagram (the part not yet charged, less the part the
    /// host spent not sending) to the current report window, so a stall is reported whether it
    /// released or is still on.
    fn charge_stall(&mut self, now: Instant, host_gap: Duration) {
        let from = self.arrived_at.max(self.stall_charged_to);
        let stretch = now.saturating_duration_since(from);
        // Spend the sleep as it is forgiven. The bank belongs to the whole silence, but a
        // charge only covers the stretch since the last one, so crediting the full bank every
        // time would forgive the same sleep in every report window and a stall that stays on
        // would go quiet after its first one.
        let doze = self
            .doze_beyond_host(now, host_gap)
            .saturating_sub(self.dozed_spent)
            .min(stretch.saturating_sub(host_gap));
        self.dozed_spent = self.dozed_spent.saturating_add(doze);
        let silence = stretch.saturating_sub(host_gap).saturating_sub(doze);
        self.window.stalled = self.window.stalled.saturating_add(silence);
        self.stats.stalled_ms = self
            .stats
            .stalled_ms
            .saturating_add(u64::try_from(silence.as_millis()).unwrap_or(0));
        self.stall_charged_to = now;
    }

    /// The link moved again after a `gap` with nothing on it: restart every pending frame's
    /// NACK clock, so the answer to a NACK that was stuck in the stall gets its round trip.
    fn resume(&mut self, now: Instant, gap: Duration) {
        if self.frames.values().all(Slot::settled) {
            return;
        }
        tracing::debug!(
            ?gap,
            pending = self.frames.len(),
            "link resumed; deadlines restart"
        );
        let fresh = now.checked_sub(self.nack_delay).unwrap_or(now);
        for slot in self.frames.values_mut() {
            match slot {
                Slot::Complete { .. } | Slot::Skipped => {}
                Slot::Unknown {
                    first_seen,
                    nacks,
                    nacked_at,
                    ..
                } => {
                    *first_seen = fresh;
                    *nacked_at = (*nacks > 0).then_some(now);
                }
                Slot::Partial(p) => {
                    p.first_seen = fresh;
                    p.last_seen = fresh;
                    p.nacked_at = (p.nacks > 0).then_some(now);
                }
            }
        }
    }

    /// Time-driven policy: NACKs, loss deadlines, refresh repeats. Call every few milliseconds
    /// and after ingesting a batch. `rtt` is the current round-trip estimate.
    pub fn tick(&mut self, now: Instant, rtt: Duration) -> Vec<Action> {
        // Bank the sleep before moving the mark, or the arrival that ends this silence would
        // find a tick that just ran and read the whole gap as the link's.
        let slept = self.dozed_since_tick(now);
        if !slept.is_zero() {
            let from = self.doze_tail_from();
            self.dozed_from.get_or_insert(from);
            self.dozed_since_arrival = self.dozed_since_arrival.saturating_add(slept);
        }
        self.last_tick_at = self.last_tick_at.max(now);
        self.last_rtt = rtt;
        self.nack_delay = self.cfg.nack_delay.for_rtt(rtt);
        let nack_delay = self.nack_delay;
        let retry_gap = rtt.saturating_add(nack_delay);
        let deadline = nack_delay
            .saturating_add(retry_gap.saturating_mul(u32::from(self.cfg.nack_retries)))
            .saturating_add(self.cfg.grace);
        let in_order = matches!(self.need, Need::Frame(_));
        let accept_refresh = matches!(self.need, Need::Refresh);
        let retries = self.cfg.nack_retries;
        let max_hold = self.cfg.max_hold;
        let arrived_at = self.arrived_at;
        // Newer datagrams still coming in: a fragment that is missing was dropped, not delayed.
        let flowing = now.saturating_duration_since(arrived_at) < deadline;
        let retry_due = |nacked_at: Option<Instant>| {
            nacked_at
                .is_none_or(|t| now.saturating_duration_since(t) >= retry_gap && arrived_at > t)
        };

        // A frame nothing refers to is asked for once, and not at all once a later picture is
        // ready to show in its place.
        let newest_complete = self
            .frames
            .iter()
            .rev()
            .find_map(|(&f, slot)| matches!(slot, Slot::Complete { .. }).then_some(f));
        let worth_asking = |frame: u32, discardable: bool, tries: u8| {
            !discardable || (tries == 0 && newest_complete.is_none_or(|newest| newest < frame))
        };

        let mut lost: Vec<(u32, u64)> = Vec::new();
        let mut nacks: Vec<Action> = Vec::new();
        for (&frame, slot) in &mut self.frames {
            match slot {
                Slot::Complete { .. } | Slot::Skipped => {}
                Slot::Unknown {
                    first_seen,
                    nacks: tries,
                    nacked_at,
                    discardable,
                } => {
                    let age = now.saturating_duration_since(*first_seen);
                    if age >= max_hold || (age >= deadline && flowing) {
                        tracing::debug!(
                            frame,
                            ?age,
                            tries,
                            ?rtt,
                            flowing,
                            "frame lost: nothing arrived"
                        );
                        lost.push((frame, 1));
                    } else if in_order
                        && *tries < retries
                        && worth_asking(frame, *discardable, *tries)
                        && age >= nack_delay
                        && retry_due(*nacked_at)
                    {
                        tracing::debug!(frame, ?age, try_ = *tries, "nack whole frame");
                        nacks.push(Action::Nack {
                            frame,
                            fragments: Vec::new(),
                        });
                        *tries = tries.saturating_add(1);
                        *nacked_at = Some(now);
                    }
                }
                Slot::Partial(p) => {
                    // Counted from the frame's latest fragment, not its first: fragments still
                    // coming in are in flight, however long the frame takes to cross. A 160 kB
                    // keyframe leaving a connection whose window is still opening takes longer
                    // than the deadline, and was given up on half-way across (MEASUREMENTS,
                    // "the first keyframe given up on while it arrives").
                    let age = now.saturating_duration_since(p.first_seen);
                    let quiet = now.saturating_duration_since(p.last_seen);
                    if quiet >= max_hold || (quiet >= deadline && flowing) {
                        tracing::debug!(
                            frame,
                            ?age,
                            ?quiet,
                            tries = p.nacks,
                            missing = p.missing_data_count(),
                            of = p.data_count,
                            parity = p.parity_count,
                            retransmitted = p.retransmitted,
                            ?rtt,
                            flowing,
                            "frame lost"
                        );
                        lost.push((frame, p.missing_data_count()));
                    } else if (in_order || p.restartable(accept_refresh))
                        && p.nacks < retries
                        && worth_asking(frame, p.discardable, p.nacks)
                        && quiet >= nack_delay
                        && retry_due(p.nacked_at)
                    {
                        let fragments = p.missing_data();
                        tracing::debug!(
                            frame,
                            ?age,
                            try_ = p.nacks,
                            missing = fragments.len(),
                            of = p.data_count,
                            "nack"
                        );
                        nacks.push(Action::Nack { frame, fragments });
                        p.nacks = p.nacks.saturating_add(1);
                        p.nacked_at = Some(now);
                    }
                }
            }
        }
        self.stats.nacks = self.stats.nacks.saturating_add(nacks.len() as u64);
        self.actions.append(&mut nacks);
        for (frame, missing) in lost {
            self.lose(frame, missing, now);
        }
        // A frame that would restart delivery, still coming in and not given up on, is the
        // answer on its way: asked again, the host made a second keyframe behind the first
        // while QUIC was still draining it (MEASUREMENTS, "a refresh asked for while the
        // keyframe is encoded").
        let answer_arriving = self
            .frames
            .values()
            .any(|slot| matches!(slot, Slot::Partial(p) if p.restartable(accept_refresh)));
        // Before the stream's first video, only the host's word that a picture went out
        // makes one worth asking for (`first_lost`).
        if !in_order
            && !answer_arriving
            && (self.any_arrived || self.first_lost)
            && self.source_live
            && self.refresh_repeats < self.cfg.refresh_max_repeats
        {
            let backoff = self
                .cfg
                .refresh_repeat
                .saturating_mul(1_u32 << self.refresh_backoff.min(16))
                .min(self.cfg.refresh_repeat_max);
            let repeat = backoff.saturating_add(rtt.saturating_mul(2));
            // Nothing asked yet (a first picture lost whole, or a source drawing again) is a
            // refresh of its own; anything else is the one asked for, asked again.
            let again = match self.refresh_requested_at {
                None => Some(false),
                Some(asked) => (now.saturating_duration_since(asked) >= repeat).then_some(true),
            };
            if let Some(again) = again {
                self.refresh_repeats = self.refresh_repeats.saturating_add(1);
                self.refresh_backoff = self.refresh_backoff.saturating_add(1);
                self.request_refresh(now, again);
            }
        }
        self.drain(now);
        std::mem::take(&mut self.actions)
    }

    /// Build the periodic receiver report and reset the window counters. `late_frames` comes
    /// from the presenter, which alone knows about vsync; `now` charges a stall still in
    /// progress to this window.
    pub fn take_report(&mut self, now: Instant, late_frames: u32) -> ReceiverReport {
        if self.stalled(now) {
            // Still in it: no datagram has arrived to say how much of it was the host's, so all
            // of it is charged bar the receiver's own sleep, which needs no datagram to settle.
            // The stamp settles the rest of the account when the silence ends.
            self.charge_stall(now, Duration::ZERO);
        }
        let window = std::mem::take(&mut self.window);
        let mut holds = window.holds_ns;
        holds.sort_unstable();
        let percentile = |p: usize| -> Duration {
            let index = holds.len().saturating_mul(p) / 100;
            let index = index.min(holds.len().saturating_sub(1));
            Duration::from_nanos(holds.get(index).copied().unwrap_or(0))
        };
        let mut acked_ltr = [0_u64; 4];
        let mut acked_ltr_len: u8 = 0;
        for slot in &mut acked_ltr {
            let Some(token) = self.ltr_acks.pop_front() else {
                break;
            };
            *slot = token;
            acked_ltr_len = acked_ltr_len.saturating_add(1);
        }
        // Repeated while frames flow, so a report that never reached the host costs it one
        // report period and not the reference. Only then: the host holds acknowledgements
        // until its next encode, and a still source would pile up one per report.
        if acked_ltr_len == 0
            && window.frames_ok > 0
            && let Some(token) = self.newest_ack
        {
            acked_ltr[0] = token;
            acked_ltr_len = 1;
        }
        ReceiverReport {
            frames_ok: window.frames_ok,
            frames_fec: window.frames_fec,
            frames_lost: window.frames_lost,
            datagrams_lost: window.datagrams_lost,
            last_host_send_ts_us: self.last_host_ts_us,
            hold_p50: percentile(50),
            hold_p95: percentile(95),
            owd_jitter: Duration::from_micros(u64::try_from(self.jitter_us).unwrap_or(0)),
            queue_depth: u8::try_from(self.queue_depth()).unwrap_or(u8::MAX),
            late_frames,
            acked_ltr,
            acked_ltr_len,
            stalled_ms: u16::try_from(window.stalled.as_millis()).unwrap_or(u16::MAX),
            stalls: window.stalls,
        }
    }
}

/// Concatenate (recovering with parity when needed) and strip the prefix. `None` when the
/// prefix is corrupt or the engine refuses the shards.
fn assemble(
    decoder: &mut Option<ReedSolomonDecoder>,
    frame: u32,
    partial: &Partial,
    arrived: Instant,
) -> Option<FrameOut> {
    let data_count = partial.data_count;
    let mut body = Vec::with_capacity(data_count.saturating_mul(partial.shard_bytes));
    let fec = partial.have_data < data_count;
    if fec {
        let dec = match decoder.as_mut() {
            Some(dec) => {
                dec.reset(data_count, partial.parity_count, partial.shard_bytes)
                    .ok()?;
                dec
            }
            None => decoder.insert(
                ReedSolomonDecoder::new(data_count, partial.parity_count, partial.shard_bytes)
                    .ok()?,
            ),
        };
        for (i, shard) in partial.shards.iter().enumerate() {
            let Some(bytes) = shard else { continue };
            if i < data_count {
                dec.add_original_shard(i, bytes).ok()?;
            } else {
                dec.add_recovery_shard(i.saturating_sub(data_count), bytes)
                    .ok()?;
            }
        }
        let restored = dec.decode().ok()?;
        for (i, shard) in partial.shards.iter().take(data_count).enumerate() {
            match shard {
                Some(bytes) => body.extend_from_slice(bytes),
                None => body.extend_from_slice(restored.restored_original(i)?),
            }
        }
    } else {
        for shard in partial.shards.iter().take(data_count) {
            body.extend_from_slice(shard.as_ref()?);
        }
    }
    let (prefix, rest) = FramePrefix::parse(&body)?;
    let len = usize::try_from(prefix.len.get()).ok()?;
    let start = body.len().checked_sub(rest.len())?;
    let end = start.checked_add(len).filter(|&end| end <= body.len())?;
    let prefix = *prefix;
    let data = Bytes::from(body).slice(start..end);
    let f = partial.flags;
    Some(FrameOut {
        info: FrameInfo {
            frame,
            keyframe: f & flags::KEYFRAME != 0,
            ltr_token: (f & flags::LTR != 0).then_some(prefix.ltr_token.get()),
            ltr_refresh: f & flags::LTR_REFRESH != 0,
            discardable: partial.discardable,
            capture_ts_us: prefix.capture_ts_us.get(),
            stripes: prefix.stripes,
            build: prefix.build,
            recovered: fec || partial.retransmitted,
        },
        data,
        hold: Duration::ZERO,
        arrived,
    })
}

#[cfg(test)]
mod nack_delay_tests {
    use super::*;

    /// A fake RTT source: what the transport would have reported on each of these links.
    fn links() -> [(&'static str, Duration); 5] {
        [
            ("loopback", Duration::from_micros(500)),
            ("lan", Duration::from_millis(2)),
            ("wifi", Duration::from_millis(12)),
            ("mesh", Duration::from_millis(40)),
            ("satellite", Duration::from_millis(600)),
        ]
    }

    /// The delay follows the round trip between its bounds: a loopback link repairs in a
    /// millisecond, a 40 ms link waits 10 ms rather than asking again for fragments that are
    /// still in flight, and nothing waits longer than the ceiling.
    #[test]
    fn the_delay_tracks_the_round_trip_between_its_bounds() {
        let policy = NackDelay::default();
        let got: Vec<(&str, u64)> = links()
            .into_iter()
            .map(|(name, rtt)| {
                let delay = policy.for_rtt(rtt);
                (name, u64::try_from(delay.as_micros()).unwrap_or(u64::MAX))
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("loopback", 1_000),
                ("lan", 1_000),
                ("wifi", 3_000),
                ("mesh", 10_000),
                ("satellite", 20_000),
            ]
        );
        // Monotonic, and always inside the bounds.
        let mut previous = Duration::ZERO;
        for ms in 0..1_000 {
            let delay = policy.for_rtt(Duration::from_millis(ms));
            assert!(
                delay >= policy.min && delay <= policy.max,
                "{ms} ms → {delay:?}"
            );
            assert!(delay >= previous, "{ms} ms went backwards");
            previous = delay;
        }
        // A divisor of zero degenerates to the floor rather than dividing by zero.
        assert_eq!(
            NackDelay {
                divisor: 0,
                ..policy
            }
            .for_rtt(Duration::from_millis(40)),
            policy.min
        );
    }

    /// `tick` re-derives the delay from the round trip it is given, and everything that reads it
    /// without one (the stall threshold) sees the same number.
    #[test]
    fn a_tick_moves_the_delay_and_the_stall_threshold_with_it() {
        let now = Instant::now();
        let mut rx = Reassembler::new(StreamId(1), Config::default(), now);
        assert_eq!(
            rx.nack_delay(),
            Duration::from_millis(5),
            "assumed 20 ms round trip"
        );

        let _quiet = rx.tick(now, Duration::from_micros(600));
        assert_eq!(rx.nack_delay(), Duration::from_millis(1));
        // Nothing has arrived yet, so the stall floor still governs.
        assert_eq!(rx.stall_threshold(), STALL_GAP);

        let _quiet = rx.tick(now, Duration::from_millis(120));
        assert_eq!(rx.nack_delay(), Duration::from_millis(20));
        assert_eq!(rx.stall_threshold(), Duration::from_millis(140));
    }
}
