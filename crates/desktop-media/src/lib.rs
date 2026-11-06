//! Remote-desktop media transport policy, pure and fuzzable.
//!
//! The host turns each encoded video frame into datagrams with a fixed [`wire::MediaHeader`],
//! adds systematic Reed–Solomon parity per frame ([`Packetizer`]) and answers NACKs from a short
//! send history. The client puts the fragments back together, recovers from parity or
//! retransmission, delivers frames strictly in decode order and decides when to NACK, when to
//! give up on a frame and ask for an LTR refresh, and what to report back ([`Reassembler`]).
//! [`Redundancy`] turns receiver reports into a parity ratio, [`RateController`] into a bitrate
//! and a cadence, [`ClockSync`] places the host's capture clock on the client's, and [`Pacer`]
//! puts decoded pictures on the glass and times them. One stream is one display, or one stripe
//! of it.
//!
//! Nothing here touches sockets, codecs or clocks: callers pass `now` and ship the bytes. The
//! datagrams ride `onicode_net` links on the channels [`wire::channel`] names.
//!
//! Ported from slopty's `slopty-media` and the client's `pacing.rs` (MIT, same authors). Comments
//! cite slopty's records as (MEASUREMENTS, "entry") for `docs/MEASUREMENTS.md` and (video
//! decisions, "entry") for `docs/decisions/video.md`.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]

mod audio;
mod clock;
mod cursor;
mod heartbeat;
mod layers;
mod pacing;
mod packetize;
mod rate;
mod reassemble;
mod redundancy;
mod refine;
pub mod wire;

pub use audio::{
    AudioCopies, AudioPackets, MAX_AUDIO_COPIES, SoundCount, audio_datagram, parse_audio,
};
pub use clock::{CLOCK_WINDOW, ClockAnchor, ClockEstimate, ClockSync};
pub use cursor::{cursor_datagram, parse_cursor};
pub use heartbeat::{HEARTBEAT_AFTER, heartbeat_datagram};
pub use layers::LayerGate;
pub use pacing::{
    CaptureClock, Captured, Clock, FrameStamp, GlassStats, Offer, PAINT_WINDOW, Pacer, PacingStats,
    PaintRate, RING, Spread, SystemClock, percentile,
};
pub use packetize::{
    EncodedFrame, Layout, MAX_DATA_FRAGMENTS, MAX_PARITY_FRAGMENTS, MIN_PARITY_FRAGMENTS,
    Packetizer, SentFrame, layout,
};
pub use rate::{
    Cadence, ChromaGate, Decision, EncoderWatch, FULL_CHROMA_ENTER_BPS, FULL_CHROMA_HOLD,
    FULL_CHROMA_LEAVE_BPS, Fed, Pace, PathSample, RateController, Window as RateWindow,
    full_chroma_band, judge, slower_rung,
};
pub use reassemble::{
    Action, Config, FrameInfo, FrameOut, Ignored, Ingest, NackDelay, Reassembler, ReassemblerStats,
    STALL_GAP, StallAttribution,
};
pub use redundancy::Redundancy;
pub use refine::{IDLE_SHARE, MAX_REFINEMENTS, PERIODS_APART, Refine};

/// Errors from packetizing.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum MediaError {
    /// The frame has no bytes.
    #[error("empty frame")]
    Empty,
    /// The frame needs more fragments than a header can address.
    #[error("frame of {len} bytes needs more than {MAX_DATA_FRAGMENTS} fragments")]
    FrameTooLarge {
        /// Frame bytes.
        len: usize,
    },
    /// The Reed–Solomon engine refused the configuration.
    #[error("reed-solomon: {0}")]
    Fec(#[from] reed_solomon_simd::Error),
}
