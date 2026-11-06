//! Heartbeat datagrams: "the link is up, the source is quiet".
//!
//! The receiver tells a stalled link from a quiet source by silence alone: nothing for a stall
//! gap means a stall. A display that does not change produces no frames for seconds, and even a
//! busy capture has holes (ScreenCaptureKit warms up after the first frame, skips a frame now
//! and then). So the host sends a header-only datagram whenever nothing else left for
//! [`HEARTBEAT_AFTER`], half the stall gap, and the receiver's stall clock only runs on silence
//! from the link.

use std::time::Duration;

use bytes::{BufMut as _, Bytes, BytesMut};
use zerocopy::IntoBytes as _;
use zerocopy::little_endian::{U16, U32};

use crate::reassemble::STALL_GAP;
use crate::wire::{HEADER_BYTES, Kind, MediaHeader, StreamId};

/// Silence on the sender after which a heartbeat goes out: half of [`STALL_GAP`], so two may
/// be late before the receiver counts a stall.
pub const HEARTBEAT_AFTER: Duration = match STALL_GAP.checked_div(2) {
    Some(half) => half,
    None => STALL_GAP,
};

/// Build a heartbeat datagram, naming `next_frame`: the number of the next video frame the
/// stream (its top stripe, when it is striped) will send, so zero until its first picture.
///
/// A receiver that has seen none of the stream's video learns from it whether the first picture
/// is still being made or was lost whole on the link, and asks for another only then.
#[must_use]
pub fn heartbeat_datagram(stream: StreamId, next_frame: u32, send_ms_lo: u8) -> Bytes {
    let header = MediaHeader {
        stream: U32::new(stream.0),
        frame: U32::new(next_frame),
        index: U16::new(0),
        data_count: U16::new(0),
        parity_count: 0,
        kind: Kind::Heartbeat as u8,
        flags: 0,
        send_ms_lo,
    };
    let mut buf = BytesMut::with_capacity(HEADER_BYTES);
    buf.put_slice(header.as_bytes());
    buf.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_is_a_bare_header_and_beats_twice_per_stall_gap() {
        let dg = heartbeat_datagram(StreamId(9), 3, 0x11);
        assert_eq!(dg.len(), HEADER_BYTES);
        let (header, payload) = MediaHeader::parse(&dg).unwrap();
        assert_eq!(
            (header.kind(), header.frame.get(), payload.len()),
            (Some(Kind::Heartbeat), 3, 0)
        );
        assert_eq!(HEARTBEAT_AFTER * 2, STALL_GAP);
    }
}
