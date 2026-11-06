//! The remote-desktop media wire: datagram channels, the media header, the frame prefix, clock
//! echoes, cursor moves and the client's feedback.
//!
//! Every layout is fixed and little-endian. `onicode_net::Link` puts a datagram's channel byte
//! in front of what is here and takes it off again, so nothing here carries one. A change to any
//! byte here is a wire change: `tests::the_wire_is_pinned` holds the canonical encoding, and a
//! change updates it and bumps [`VERSION`] in the same commit.

use std::fmt;
use std::time::Duration;

use bytes::{BufMut as _, Bytes, BytesMut};
use zerocopy::little_endian::{I32, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// The wire's version, carried when a desktop stream opens; bumped with every change to the
/// pinned encoding.
pub const VERSION: u8 = 1;

/// The datagram channels this wire uses: a datagram's first byte, as `onicode_net::Link` sends
/// and reads it. Channel 1 is the terminal copies'.
pub mod channel {
    /// Host to client: video fragments and parity, audio, cursor moves, heartbeats and clock
    /// echoes, told apart by [`super::Kind`]. `onicode_net`'s `channel::MEDIA`.
    pub const MEDIA: u8 = 2;
    /// Client to host: NACKs, refresh requests, clock probes and reports ([`super::Feedback`]).
    pub const FEEDBACK: u8 = 3;
}

/// Every packet's UDP payload on the tailnet (`onicode_net::transport::PATH_MTU`).
pub const PATH_MTU: usize = 1232;

/// What QUIC takes of a packet around one datagram frame, at most, as noq counts it for
/// `max_datagram_size`: a flags byte, a 20-byte connection id, a 4-byte packet number, the
/// 16-byte tag and the frame's 9-byte type and length.
pub const QUIC_OVERHEAD: usize = 1 + 20 + 4 + 16 + 9;

/// Largest datagram this wire sends, after the link's channel byte: what a 1232-byte packet
/// carries whatever the connection id. A path that falls back to QUIC's 1200 carries less; the
/// sender reads the link's own budget before every frame (`Packetizer::set_max_datagram`).
pub const MAX_DATAGRAM: usize = PATH_MTU - QUIC_OVERHEAD - 1;

/// Bytes of [`MediaHeader`].
pub const HEADER_BYTES: usize = 16;

/// Bytes available for payload after the header, rounded down to even: parity shards are cut
/// to an even size, and every fragment of a frame carries as many bytes as its shards.
pub const MAX_PAYLOAD: usize = (MAX_DATAGRAM - HEADER_BYTES) & !1;

/// One media stream of a link: a display's picture (or one stripe of it), or its sound.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct StreamId(pub u32);

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stream#{}", self.0)
    }
}

/// The stream the host's sound comes on. A host numbers its picture streams from 1, so no
/// picture is this one.
pub const SOUND: StreamId = StreamId(0);

/// Media datagram kinds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    /// A fragment of an encoded video frame.
    VideoData = 0,
    /// A Reed–Solomon parity shard for a video frame.
    VideoParity = 1,
    /// One Opus packet.
    Audio = 2,
    /// Cursor position update.
    Cursor = 3,
    /// Nothing to show: the source is quiet, the link is not. Header only, sent when no other
    /// datagram left for a while, so the receiver's stall clock runs on silence from the link
    /// alone. Its `frame` is the next video frame the stream will send, so a receiver that has
    /// none yet can tell a first picture still being made from one lost.
    Heartbeat = 4,
    /// The answer to a clock probe ([`Feedback::Clock`]): a [`ClockEcho`].
    Clock = 5,
}

impl Kind {
    /// Parse.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::VideoData),
            1 => Some(Self::VideoParity),
            2 => Some(Self::Audio),
            3 => Some(Self::Cursor),
            4 => Some(Self::Heartbeat),
            5 => Some(Self::Clock),
            _ => None,
        }
    }
}

/// Header flag bits.
pub mod flags {
    /// The frame is an IDR / keyframe.
    pub const KEYFRAME: u8 = 1 << 0;
    /// The frame is a long-term reference; the receiver must ack its token.
    pub const LTR: u8 = 1 << 1;
    /// The frame was encoded as an LTR refresh (recovery frame).
    pub const LTR_REFRESH: u8 = 1 << 2;
    /// This datagram is a retransmission (NACK reply).
    pub const RETRANSMIT: u8 = 1 << 3;
    /// Nothing later refers to this frame (a temporal layer-1 frame).
    ///
    /// A receiver that cannot repair it skips it instead of asking for a refresh. Set on every
    /// datagram of the frame, data and parity; never on a keyframe, an LTR refresh or a
    /// long-term reference.
    pub const DISCARDABLE: u8 = 1 << 4;
    /// The frame numbered one below this one was [`DISCARDABLE`]. Set on every datagram of the
    /// frame after it, so a receiver that lost that frame whole still knows it may skip it.
    pub const PREV_DISCARDABLE: u8 = 1 << 5;
}

/// The fixed header at the front of every media datagram.
#[derive(
    Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, PartialEq, Eq,
)]
#[repr(C)]
pub struct MediaHeader {
    /// Stream this belongs to ([`StreamId`]).
    pub stream: U32,
    /// Frame number within the stream (video), packet number (audio), update seq (cursor), or
    /// the next video frame the stream will send (heartbeat).
    pub frame: U32,
    /// Fragment index within the frame. For parity, `data_count + parity index`.
    pub index: U16,
    /// Number of data fragments in the frame.
    pub data_count: U16,
    /// Number of parity fragments in the frame.
    pub parity_count: u8,
    /// [`Kind`] as `u8`.
    pub kind: u8,
    /// [`flags`].
    pub flags: u8,
    /// Host send timestamp, low 8 bits of milliseconds (wraps; read for gaps between sends,
    /// never for absolute latency).
    pub send_ms_lo: u8,
}

impl MediaHeader {
    /// A header for datagram `frame` of `kind` on `stream`, with no fragments counted.
    #[must_use]
    pub const fn new(stream: StreamId, frame: u32, kind: Kind, send_ms_lo: u8) -> Self {
        Self {
            stream: U32::new(stream.0),
            frame: U32::new(frame),
            index: U16::new(0),
            data_count: U16::new(0),
            parity_count: 0,
            kind: kind as u8,
            flags: 0,
            send_ms_lo,
        }
    }

    /// Parse the front of a datagram; `None` if it is too short.
    #[must_use]
    pub fn parse(datagram: &[u8]) -> Option<(&Self, &[u8])> {
        Self::ref_from_prefix(datagram).ok()
    }

    /// The kind, if valid.
    #[must_use]
    pub const fn kind(&self) -> Option<Kind> {
        Kind::from_u8(self.kind)
    }

    /// True when this datagram is a parity shard.
    #[must_use]
    pub const fn is_parity(&self) -> bool {
        self.kind == Kind::VideoParity as u8
    }
}

const _: () = assert!(
    size_of::<MediaHeader>() == HEADER_BYTES,
    "header layout drifted"
);

/// Bytes of [`FramePrefix`].
pub const FRAME_PREFIX_BYTES: usize = 20;

/// Per-frame metadata carried *inside* the fragmented payload, ahead of the encoded bitstream.
///
/// It is protected by the frame's parity like the bitstream, so a recovered frame recovers its
/// metadata too. The fragmented body is `FramePrefix ‖ bitstream ‖ zero padding`; every fragment
/// of a frame (data and parity) carries the same number of payload bytes.
#[derive(
    Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, PartialEq, Eq,
)]
#[repr(C)]
pub struct FramePrefix {
    /// Bitstream bytes that follow; anything after them is padding.
    pub len: U32,
    /// Host capture timestamp, microseconds on the host's monotonic clock (low 32 bits).
    pub capture_ts_us: U32,
    /// The long-term-reference token to acknowledge when [`flags::LTR`] is set; zero otherwise.
    pub ltr_token: U64,
    /// For a striped stream, the stripes coded from this capture, bit `i` for stripe `i`; zero
    /// for a stream coded as one picture. A receiver shows a capture once every stripe it names
    /// has decoded, and a stripe it does not name keeps the picture it has: a refresh or a
    /// refinement codes only the stripe that needs it.
    pub stripes: u8,
    /// The host's build of the stream's sessions that coded this frame, counted on from 0 and
    /// wrapping. A resize, a chroma switch or the stripes turning on or off builds the sessions
    /// anew, and only stripes of one build fit together.
    pub build: u8,
    /// Zero.
    pub reserved: [u8; 2],
}

impl FramePrefix {
    /// Split a reassembled body into its prefix and the bytes after it.
    #[must_use]
    pub fn parse(body: &[u8]) -> Option<(&Self, &[u8])> {
        Self::ref_from_prefix(body).ok()
    }
}

const _: () = assert!(
    size_of::<FramePrefix>() == FRAME_PREFIX_BYTES,
    "prefix layout drifted"
);

/// Bytes of [`ClockEcho`].
pub const CLOCK_ECHO_BYTES: usize = 24;

/// Payload of a [`Kind::Clock`] datagram: one round trip's four timestamps, less the fourth,
/// which the client reads when it arrives.
///
/// The host's two readings are on the clock its capture timestamps are on
/// ([`FramePrefix::capture_ts_us`], all 64 bits of it), so the client can estimate that clock's
/// offset from its own the way NTP does: the offset is `((received − sent) + (echoed − arrived))
/// / 2` and the round trip `(arrived − sent) − (echoed − received)`, and the true offset lies
/// within half that round trip of the estimate.
#[derive(
    Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, PartialEq, Eq,
)]
#[repr(C)]
pub struct ClockEcho {
    /// The probe's `sent_us`, as the client wrote it.
    pub sent: U64,
    /// The host's capture clock when the probe arrived, microseconds.
    pub received: U64,
    /// The host's capture clock when this echo left, microseconds.
    pub echoed: U64,
}

impl ClockEcho {
    /// An echo of the probe stamped `sent_us`, which arrived at `received_us` and leaves at
    /// `echoed_us` on the host's capture clock.
    #[must_use]
    pub const fn new(sent_us: u64, received_us: u64, echoed_us: u64) -> Self {
        Self {
            sent: U64::new(sent_us),
            received: U64::new(received_us),
            echoed: U64::new(echoed_us),
        }
    }

    /// Read the payload of a [`Kind::Clock`] datagram; `None` when it is short.
    #[must_use]
    pub fn parse(payload: &[u8]) -> Option<&Self> {
        Self::ref_from_prefix(payload).ok().map(|(echo, _)| echo)
    }

    /// The [`Kind::Clock`] datagram carrying this echo on `stream`, stamped `send_ms_lo` as
    /// every datagram of the stream is ([`MediaHeader::send_ms_lo`]).
    #[must_use]
    pub fn datagram(&self, stream: StreamId, send_ms_lo: u8) -> Bytes {
        let header = MediaHeader::new(stream, 0, Kind::Clock, send_ms_lo);
        let mut out = BytesMut::with_capacity(HEADER_BYTES.saturating_add(CLOCK_ECHO_BYTES));
        out.put_slice(header.as_bytes());
        out.put_slice(self.as_bytes());
        out.freeze()
    }
}

const _: () = assert!(
    size_of::<ClockEcho>() == CLOCK_ECHO_BYTES,
    "clock echo layout drifted"
);

/// Bytes of [`CursorUpdate`].
pub const CURSOR_BYTES: usize = 12;

/// Payload of a [`Kind::Cursor`] datagram: pointer position in stream pixels.
#[derive(
    Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, PartialEq, Eq,
)]
#[repr(C)]
pub struct CursorUpdate {
    /// X in stream pixels; negative when left of the display.
    pub x: I32,
    /// Y in stream pixels.
    pub y: I32,
    /// 1 when the pointer should be drawn.
    pub visible: u8,
    /// Zero.
    pub reserved: [u8; 3],
}

const _: () = assert!(
    size_of::<CursorUpdate>() == CURSOR_BYTES,
    "cursor layout drifted"
);

/// Video codec.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum VideoCodec {
    /// HEVC: Main, 8-bit 4:2:0, or Main 4:4:4 10 for a [`Chroma::Full`] stream.
    Hevc = 0,
    /// H.264 High (fallback only).
    H264 = 1,
}

impl VideoCodec {
    /// Parse.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Hevc),
            1 => Some(Self::H264),
            _ => None,
        }
    }
}

/// How much colour a video stream carries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum Chroma {
    /// 4:2:0, one colour sample per 2×2 pixels: HEVC Main or H.264 High, fed full-range NV12.
    #[default]
    Subsampled = 0,
    /// 4:4:4, colour at every pixel: HEVC Main 4:4:4 10, HEVC only, captured as full-range
    /// 10-bit bi-planar 4:4:4 (`xf44`).
    Full = 1,
}

impl Chroma {
    /// Parse.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Subsampled),
            1 => Some(Self::Full),
            _ => None,
        }
    }
}

/// What the host's bitrate controller made of its last decision window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RateVerdict {
    /// Loss, queueing or hold time: the target was cut.
    Cut = 0,
    /// The window held a stall: the target is frozen, the window's loss discarded.
    Stall = 1,
    /// Nothing wrong, nothing to grow into yet (cooldown after a cut, or the ceiling).
    Steady = 2,
    /// Clean: the target grew.
    Grow = 3,
}

impl RateVerdict {
    /// Parse.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Cut),
            1 => Some(Self::Stall),
            2 => Some(Self::Steady),
            3 => Some(Self::Grow),
            _ => None,
        }
    }
}

/// Whether a stream's display is producing pictures, and when it cannot, why.
///
/// A display that does not change yields no frames, and the client cannot tell that from
/// frames being lost; without the hint it would ask for a refresh for as long as the stream is
/// open. A locked Mac, or one whose screens another session has, shows nothing until someone at
/// the Mac changes that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum SourceState {
    /// Open and capturing, but the display has produced no frame yet. Nothing to refresh from.
    Idle = 0,
    /// The display has produced a frame; pictures are on the way.
    Live = 1,
    /// The host's Mac is locked. Nothing to refresh from.
    Locked = 2,
    /// The host's session is off the Mac's screens (login window, another user). Nothing to
    /// refresh from until it is back.
    Away = 3,
}

impl SourceState {
    /// Parse.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Idle),
            1 => Some(Self::Live),
            2 => Some(Self::Locked),
            3 => Some(Self::Away),
            _ => None,
        }
    }
}

/// Receiver-side telemetry, all relative so no clock sync is assumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReceiverReport {
    /// Frames received complete or recovered in the window.
    pub frames_ok: u32,
    /// Frames recovered by FEC.
    pub frames_fec: u32,
    /// Frames lost.
    pub frames_lost: u32,
    /// Datagrams lost (by sequence gaps).
    pub datagrams_lost: u32,
    /// Highest host capture timestamp seen (host clock, echoed).
    pub last_host_send_ts_us: u32,
    /// Client-side hold time between arrival and present, p50. Microseconds on the wire.
    pub hold_p50: Duration,
    /// Client-side hold time, p95. Microseconds on the wire.
    pub hold_p95: Duration,
    /// One-way-delay jitter estimate. Microseconds on the wire.
    pub owd_jitter: Duration,
    /// Present queue depth.
    pub queue_depth: u8,
    /// Frames presented late (missed their intended vsync).
    pub late_frames: u32,
    /// Acknowledged LTR tokens since the last report.
    pub acked_ltr: [u64; 4],
    /// Number of valid entries in `acked_ltr`.
    pub acked_ltr_len: u8,
    /// Milliseconds of the window during which nothing at all arrived on the stream past the
    /// reassembler's stall gap (a stall in progress at report time counts up to now). Loss
    /// counted while this is non-zero is the link holding packets, not dropping them.
    pub stalled_ms: u16,
    /// Stalls that released in the window (packets held, then delivered together).
    pub stalls: u16,
}

/// Bytes of a [`ReceiverReport`] on the wire.
pub const REPORT_BYTES: usize = 74;

/// [`ReceiverReport`] as the wire lays it out.
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
struct ReportBytes {
    frames_ok: U32,
    frames_fec: U32,
    frames_lost: U32,
    datagrams_lost: U32,
    last_host_send_ts_us: U32,
    hold_p50_us: U32,
    hold_p95_us: U32,
    owd_jitter_us: U32,
    late_frames: U32,
    acked_ltr: [U64; 4],
    stalled_ms: U16,
    stalls: U16,
    queue_depth: u8,
    acked_ltr_len: u8,
}

const _: () = assert!(
    size_of::<ReportBytes>() == REPORT_BYTES,
    "report layout drifted"
);

/// `d` in whole microseconds, saturating at the field's top.
fn micros(d: Duration) -> U32 {
    U32::new(u32::try_from(d.as_micros()).unwrap_or(u32::MAX))
}

impl ReceiverReport {
    fn to_wire(self) -> ReportBytes {
        ReportBytes {
            frames_ok: U32::new(self.frames_ok),
            frames_fec: U32::new(self.frames_fec),
            frames_lost: U32::new(self.frames_lost),
            datagrams_lost: U32::new(self.datagrams_lost),
            last_host_send_ts_us: U32::new(self.last_host_send_ts_us),
            hold_p50_us: micros(self.hold_p50),
            hold_p95_us: micros(self.hold_p95),
            owd_jitter_us: micros(self.owd_jitter),
            late_frames: U32::new(self.late_frames),
            acked_ltr: self.acked_ltr.map(U64::new),
            stalled_ms: U16::new(self.stalled_ms),
            stalls: U16::new(self.stalls),
            queue_depth: self.queue_depth,
            acked_ltr_len: self.acked_ltr_len,
        }
    }

    fn from_wire(w: &ReportBytes) -> Self {
        let us = |v: U32| Duration::from_micros(u64::from(v.get()));
        Self {
            frames_ok: w.frames_ok.get(),
            frames_fec: w.frames_fec.get(),
            frames_lost: w.frames_lost.get(),
            datagrams_lost: w.datagrams_lost.get(),
            last_host_send_ts_us: w.last_host_send_ts_us.get(),
            hold_p50: us(w.hold_p50_us),
            hold_p95: us(w.hold_p95_us),
            owd_jitter: us(w.owd_jitter_us),
            queue_depth: w.queue_depth,
            late_frames: w.late_frames.get(),
            acked_ltr: w.acked_ltr.map(U64::get),
            acked_ltr_len: w.acked_ltr_len,
            stalled_ms: w.stalled_ms.get(),
            stalls: w.stalls.get(),
        }
    }
}

/// What the client's one player of a host's sound ([`SOUND`]) heard since its last report.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SoundReport {
    /// Audio packets that arrived in the window.
    pub received: u16,
    /// Audio packets missing from the sequence in the window, whether or not a copy an audio
    /// datagram carried recovered them: the loss the host sizes those copies by.
    pub lost: u16,
}

/// Most fragments one NACK names: what fits in a datagram behind its stream and frame.
pub const MAX_NACK_FRAGMENTS: usize = (MAX_DATAGRAM - 9) / 2;

/// Client to host on [`channel::FEEDBACK`], one per datagram.
///
/// Datagrams, not the control stream: one lost packet on an ordered stream would hold every
/// later NACK until QUIC's loss timer fired, by which time the receiver has given up and asked
/// for a refresh. Each stands alone; the receiver's own retries are the reliability, and a
/// report lost is folded into the next one's window.
///
/// The first byte names the variant; the fields follow little-endian, fixed size, except a
/// NACK's fragments, which run to the end.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Feedback {
    /// Ask for retransmission of specific fragments (inside the playout window).
    Nack {
        /// Stream.
        stream: StreamId,
        /// Frame.
        frame: u32,
        /// Missing data fragment indices; empty means "every fragment" (nothing of the frame
        /// arrived, so the client does not know how many there are). At most
        /// [`MAX_NACK_FRAGMENTS`] go on the wire; the retries ask for the rest.
        fragments: Vec<u16>,
    },
    /// The client lost a frame it could not recover; the host should refresh from an acked LTR.
    Refresh {
        /// Stream.
        stream: StreamId,
        /// Highest frame fully decoded.
        last_good_frame: u32,
        /// The client holds no reference to predict from (its decoder session was lost, or the
        /// stream has not started): only an IDR can be decoded.
        keyframe: bool,
    },
    /// What time the host's capture clock reads: answered at once with a [`Kind::Clock`]
    /// datagram on the stream carrying `sent_us` back beside the host's own readings.
    Clock {
        /// Stream.
        stream: StreamId,
        /// When the probe left, on the client's clock, microseconds from an epoch of its own;
        /// the host only echoes it.
        sent_us: u64,
    },
    /// A picture stream's receiver report, every 50 ms.
    Report {
        /// Stream.
        stream: StreamId,
        /// The report.
        report: ReceiverReport,
    },
    /// The sound player's report.
    Sound(SoundReport),
}

impl Feedback {
    const CLOCK: u8 = 2;
    const NACK: u8 = 0;
    const REFRESH: u8 = 1;
    const REPORT: u8 = 3;
    const SOUND: u8 = 4;

    /// The datagram's bytes, after the channel byte.
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut out = BytesMut::with_capacity(16);
        match self {
            Self::Nack {
                stream,
                frame,
                fragments,
            } => {
                out.reserve(fragments.len().min(MAX_NACK_FRAGMENTS).saturating_mul(2));
                out.put_u8(Self::NACK);
                out.put_u32_le(stream.0);
                out.put_u32_le(*frame);
                for &fragment in fragments.iter().take(MAX_NACK_FRAGMENTS) {
                    out.put_u16_le(fragment);
                }
            }
            Self::Refresh {
                stream,
                last_good_frame,
                keyframe,
            } => {
                out.put_u8(Self::REFRESH);
                out.put_u32_le(stream.0);
                out.put_u32_le(*last_good_frame);
                out.put_u8(u8::from(*keyframe));
            }
            Self::Clock { stream, sent_us } => {
                out.put_u8(Self::CLOCK);
                out.put_u32_le(stream.0);
                out.put_u64_le(*sent_us);
            }
            Self::Report { stream, report } => {
                out.reserve(REPORT_BYTES);
                out.put_u8(Self::REPORT);
                out.put_u32_le(stream.0);
                out.put_slice(report.to_wire().as_bytes());
            }
            Self::Sound(sound) => {
                out.put_u8(Self::SOUND);
                out.put_u16_le(sound.received);
                out.put_u16_le(sound.lost);
            }
        }
        out.freeze()
    }

    /// Read a feedback datagram's bytes (after the channel byte); `None` for anything that is
    /// not exactly one of [`Self::encode`]'s.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&tag, rest) = bytes.split_first()?;
        match tag {
            Self::NACK => {
                let (stream, rest) = split_stream(rest)?;
                let (frame, rest) = U32::read_from_prefix(rest).ok()?;
                let list = <[U16]>::ref_from_bytes(rest).ok()?;
                if list.len() > MAX_NACK_FRAGMENTS {
                    return None;
                }
                let fragments = list.iter().copied().map(U16::get).collect();
                Some(Self::Nack {
                    stream,
                    frame: frame.get(),
                    fragments,
                })
            }
            Self::REFRESH => {
                let (stream, rest) = split_stream(rest)?;
                let (frame, rest) = U32::read_from_prefix(rest).ok()?;
                let keyframe = match rest {
                    [0] => false,
                    [1] => true,
                    _ => return None,
                };
                Some(Self::Refresh {
                    stream,
                    last_good_frame: frame.get(),
                    keyframe,
                })
            }
            Self::CLOCK => {
                let (stream, rest) = split_stream(rest)?;
                let sent = U64::read_from_bytes(rest).ok()?;
                Some(Self::Clock {
                    stream,
                    sent_us: sent.get(),
                })
            }
            Self::REPORT => {
                let (stream, rest) = split_stream(rest)?;
                let report = ReportBytes::ref_from_bytes(rest).ok()?;
                Some(Self::Report {
                    stream,
                    report: ReceiverReport::from_wire(report),
                })
            }
            Self::SOUND => {
                let &[r0, r1, l0, l1] = rest else {
                    return None;
                };
                let received = u16::from_le_bytes([r0, r1]);
                let lost = u16::from_le_bytes([l0, l1]);
                Some(Self::Sound(SoundReport { received, lost }))
            }
            _ => None,
        }
    }
}

/// The stream id at the front of `bytes`, and the bytes after it.
fn split_stream(bytes: &[u8]) -> Option<(StreamId, &[u8])> {
    let (id, rest) = U32::read_from_prefix(bytes).ok()?;
    Some((StreamId(id.get()), rest))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
                s.push(char::from_digit(u32::from(b >> 4), 16).unwrap());
                s.push(char::from_digit(u32::from(b & 0xf), 16).unwrap());
                s
            })
    }

    fn report() -> ReceiverReport {
        ReceiverReport {
            frames_ok: 0x0102_0304,
            frames_fec: 5,
            frames_lost: 6,
            datagrams_lost: 7,
            last_host_send_ts_us: 0x0809_0a0b,
            hold_p50: Duration::from_micros(0x0c0d),
            hold_p95: Duration::from_micros(0x0e0f),
            owd_jitter: Duration::from_micros(0x10),
            queue_depth: 0x11,
            late_frames: 0x12,
            acked_ltr: [0x13, 0x14, 0, 0],
            acked_ltr_len: 2,
            stalled_ms: 0x1516,
            stalls: 0x17,
        }
    }

    /// One of every wire layout and constant, in a fixed order.
    fn canonical() -> Vec<u8> {
        let mut out = vec![VERSION, channel::MEDIA, channel::FEEDBACK];
        out.extend_from_slice(&u16::try_from(MAX_DATAGRAM).unwrap().to_le_bytes());
        out.extend(
            [
                HEADER_BYTES,
                FRAME_PREFIX_BYTES,
                CLOCK_ECHO_BYTES,
                CURSOR_BYTES,
                REPORT_BYTES,
            ]
            .map(|n| u8::try_from(n).unwrap()),
        );
        let mut header =
            MediaHeader::new(StreamId(0x0102_0304), 0x0506_0708, Kind::VideoParity, 0xab);
        header.index = U16::new(0x090a);
        header.data_count = U16::new(0x0b0c);
        header.parity_count = 0x0d;
        header.flags = flags::KEYFRAME | flags::LTR | flags::LTR_REFRESH;
        out.extend_from_slice(header.as_bytes());
        out.extend([
            flags::RETRANSMIT,
            flags::DISCARDABLE,
            flags::PREV_DISCARDABLE,
            Kind::VideoData as u8,
            Kind::Audio as u8,
            Kind::Cursor as u8,
            Kind::Heartbeat as u8,
            Kind::Clock as u8,
            VideoCodec::Hevc as u8,
            VideoCodec::H264 as u8,
            Chroma::Subsampled as u8,
            Chroma::Full as u8,
            RateVerdict::Cut as u8,
            RateVerdict::Stall as u8,
            RateVerdict::Steady as u8,
            RateVerdict::Grow as u8,
            SourceState::Idle as u8,
            SourceState::Live as u8,
            SourceState::Locked as u8,
            SourceState::Away as u8,
        ]);
        let prefix = FramePrefix {
            len: U32::new(0x0102_0304),
            capture_ts_us: U32::new(5),
            ltr_token: U64::new(0x0807_0605_0403_0201),
            stripes: 0b10,
            build: 9,
            reserved: [0; 2],
        };
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(&ClockEcho::new(1, 2, 3).datagram(StreamId(4), 5));
        let cursor = CursorUpdate {
            x: I32::new(-1),
            y: I32::new(2),
            visible: 1,
            reserved: [0; 3],
        };
        out.extend_from_slice(cursor.as_bytes());
        for feedback in [
            Feedback::Nack {
                stream: StreamId(1),
                frame: 2,
                fragments: vec![3, 0x0405],
            },
            Feedback::Refresh {
                stream: StreamId(1),
                last_good_frame: 2,
                keyframe: true,
            },
            Feedback::Clock {
                stream: StreamId(1),
                sent_us: 0x0102_0304_0506_0708,
            },
            Feedback::Report {
                stream: StreamId(1),
                report: report(),
            },
            Feedback::Sound(SoundReport {
                received: 0x0102,
                lost: 3,
            }),
        ] {
            out.extend_from_slice(&feedback.encode());
        }
        out
    }

    /// The wire, byte for byte. A change here is a wire change: update the bytes and bump
    /// [`VERSION`] in the same commit.
    #[test]
    fn the_wire_is_pinned() {
        let pinned = concat!(
            // version, channels, MAX_DATAGRAM, the five layout sizes
            "010203",
            "9d04",
            "1014180c4a",
            // media header
            "04030201",
            "08070605",
            "0a09",
            "0c0b",
            "0d",
            "01",
            "07",
            "ab",
            // flags, kinds, codec, chroma, verdicts, source states
            "081020",
            "0002030405",
            "0001",
            "0001",
            "00010203",
            "00010203",
            // frame prefix
            "04030201",
            "05000000",
            "0102030405060708",
            "02",
            "09",
            "0000",
            // clock echo datagram: header, then sent, received, echoed
            "04000000",
            "00000000",
            "0000",
            "0000",
            "00",
            "05",
            "00",
            "05",
            "0100000000000000",
            "0200000000000000",
            "0300000000000000",
            // cursor
            "ffffffff",
            "02000000",
            "01",
            "000000",
            // nack, refresh, clock probe
            "00",
            "01000000",
            "02000000",
            "0300",
            "0504",
            "01",
            "01000000",
            "02000000",
            "01",
            "02",
            "01000000",
            "0807060504030201",
            // report
            "03",
            "01000000",
            "04030201",
            "05000000",
            "06000000",
            "07000000",
            "0b0a0908",
            "0d0c0000",
            "0f0e0000",
            "10000000",
            "12000000",
            "1300000000000000",
            "1400000000000000",
            "0000000000000000",
            "0000000000000000",
            "1615",
            "1700",
            "11",
            "02",
            // sound
            "04",
            "0201",
            "0300",
        );
        assert_eq!(hex(&canonical()), pinned);
    }

    #[test]
    fn the_budget_fits_the_link() {
        assert_eq!(PATH_MTU, usize::from(onicode_net::transport::PATH_MTU));
        assert_eq!(channel::MEDIA, onicode_net::stream::channel::MEDIA);
        assert_eq!(channel::FEEDBACK, onicode_net::stream::channel::FEEDBACK);
        assert!(MAX_PAYLOAD.is_multiple_of(2) && MAX_PAYLOAD + HEADER_BYTES <= MAX_DATAGRAM);
        let nack = Feedback::Nack {
            stream: StreamId(1),
            frame: 1,
            fragments: vec![7; MAX_NACK_FRAGMENTS + 10],
        };
        assert!(nack.encode().len() <= MAX_DATAGRAM, "a NACK is cut to fit");
    }

    #[test]
    fn header_round_trips_through_bytes() {
        let mut h = MediaHeader::new(StreamId(7), 123_456, Kind::VideoParity, 0xab);
        h.index = U16::new(3);
        h.data_count = U16::new(10);
        h.parity_count = 2;
        h.flags = flags::KEYFRAME | flags::LTR;
        let mut buf = vec![0_u8; MAX_DATAGRAM];
        buf[..HEADER_BYTES].copy_from_slice(h.as_bytes());
        buf[HEADER_BYTES] = 0xee;
        let (parsed, payload) = MediaHeader::parse(&buf).unwrap();
        assert_eq!(parsed, &h);
        assert_eq!(payload.len(), MAX_DATAGRAM - HEADER_BYTES);
        assert_eq!(payload[0], 0xee);
        assert!(parsed.is_parity());
        assert_eq!(parsed.kind(), Some(Kind::VideoParity));
        assert!(
            MediaHeader::parse(&buf[..HEADER_BYTES - 1]).is_none(),
            "short"
        );
    }

    #[test]
    fn a_clock_echo_is_a_header_and_three_readings() {
        let echo = ClockEcho::new(1, 0x0102_0304_0506_0708, 0x0102_0304_0506_0709);
        let datagram = echo.datagram(StreamId(9), 0xab);
        assert_eq!(datagram.len(), HEADER_BYTES + CLOCK_ECHO_BYTES);
        let (header, payload) = MediaHeader::parse(&datagram).unwrap();
        assert_eq!(
            (header.kind(), header.stream.get(), header.send_ms_lo),
            (Some(Kind::Clock), 9, 0xab)
        );
        assert_eq!(ClockEcho::parse(payload), Some(&echo));
        assert_eq!(
            ClockEcho::parse(&payload[..CLOCK_ECHO_BYTES - 1]),
            None,
            "short"
        );
    }

    /// Every feedback reads back as written, and a byte short or over is refused.
    #[test]
    fn feedback_round_trips_and_refuses_the_rest() {
        let all = [
            Feedback::Nack {
                stream: StreamId(3),
                frame: 9,
                fragments: vec![],
            },
            Feedback::Nack {
                stream: StreamId(3),
                frame: 9,
                fragments: vec![1, 2, 500],
            },
            Feedback::Refresh {
                stream: StreamId(3),
                last_good_frame: 8,
                keyframe: false,
            },
            Feedback::Clock {
                stream: StreamId(3),
                sent_us: u64::MAX,
            },
            Feedback::Report {
                stream: StreamId(3),
                report: report(),
            },
            Feedback::Sound(SoundReport {
                received: 4,
                lost: 1,
            }),
        ];
        for feedback in all {
            let bytes = feedback.encode();
            assert_eq!(Feedback::decode(&bytes), Some(feedback.clone()));
            let mut over = bytes.to_vec();
            over.push(0);
            assert_eq!(Feedback::decode(&over), None, "a byte over: {feedback:?}");
            assert_eq!(
                Feedback::decode(&bytes[..bytes.len() - 1]),
                None,
                "short: {feedback:?}"
            );
        }
        assert_eq!(Feedback::decode(&[]), None);
        assert_eq!(
            Feedback::decode(&[9, 0, 0, 0, 0]),
            None,
            "an unknown variant"
        );
        assert_eq!(
            Feedback::decode(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 2]),
            None,
            "keyframe is 0 or 1"
        );
    }
}
