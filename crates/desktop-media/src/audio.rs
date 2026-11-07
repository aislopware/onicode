//! Audio datagrams: one Opus packet each.
//!
//! An Opus packet is 10 ms and about 120 bytes, and one datagram carries it behind the media
//! header, whose `frame` is the packet's sequence and whose `data_count` is 1. A lost packet is
//! not sent again: the client fades over the gap (`onicode_desktop_codec::audio::LossFade`).

use bytes::{BufMut as _, Bytes, BytesMut};
use zerocopy::IntoBytes as _;
use zerocopy::little_endian::{U16, U32};

use crate::wire::{HEADER_BYTES, Kind, MAX_PAYLOAD, MediaHeader, StreamId};

/// One Opus packet as a datagram; `None` when it is empty or does not fit.
#[must_use]
pub fn audio_datagram(stream: StreamId, seq: u32, send_ms_lo: u8, opus: &[u8]) -> Option<Bytes> {
    if opus.is_empty() || opus.len() > MAX_PAYLOAD {
        return None;
    }
    let header = MediaHeader {
        stream: U32::new(stream.0),
        frame: U32::new(seq),
        index: U16::new(0),
        data_count: U16::new(1),
        parity_count: 0,
        kind: Kind::Audio as u8,
        flags: 0,
        send_ms_lo,
    };
    let mut buf = BytesMut::with_capacity(HEADER_BYTES.saturating_add(opus.len()));
    buf.put_slice(header.as_bytes());
    buf.put_slice(opus);
    Some(buf.freeze())
}

/// An audio datagram's packet, from its header's `data_count` and its `payload`; `None` for a
/// count other than one or an empty packet.
#[must_use]
pub fn parse_audio(data_count: u16, payload: &Bytes) -> Option<Bytes> {
    (data_count == 1 && !payload.is_empty()).then(|| payload.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout, byte for byte: a wire format, so a change here is a wire change. The header
    /// counts the one packet, which takes the rest.
    #[test]
    fn the_layout_is_fixed() {
        let dg = audio_datagram(StreamId(7), 0x0102_0304, 0xab, &[0xaa, 0xbb]).unwrap();
        let header = [
            7,
            0,
            0,
            0,
            4,
            3,
            2,
            1,
            0,
            0,
            1,
            0,
            0,
            Kind::Audio as u8,
            0,
            0xab,
        ];
        assert_eq!(dg.get(..HEADER_BYTES), Some(&header[..]));
        assert_eq!(dg.get(HEADER_BYTES..), Some(&[0xaa, 0xbb][..]));
        let (parsed, _) = MediaHeader::parse(&dg).unwrap();
        assert_eq!(
            parse_audio(parsed.data_count.get(), &dg.slice(HEADER_BYTES..)),
            Some(Bytes::from_static(&[0xaa, 0xbb]))
        );
    }

    /// A packet that does not fit, or an empty one, is refused; so is a count other than one.
    #[test]
    fn what_does_not_fit_or_count_is_refused() {
        assert!(audio_datagram(StreamId(1), 5, 0, &[9; MAX_PAYLOAD]).is_some());
        assert!(audio_datagram(StreamId(1), 5, 0, &[9; MAX_PAYLOAD + 1]).is_none());
        assert!(audio_datagram(StreamId(1), 5, 0, &[]).is_none());
        let payload = Bytes::from_static(&[1, 2]);
        assert_eq!(parse_audio(0, &payload), None, "no packet counted");
        assert_eq!(parse_audio(2, &payload), None, "a count of two");
        assert_eq!(parse_audio(1, &Bytes::new()), None, "an empty packet");
        assert_eq!(parse_audio(1, &payload), Some(payload));
    }
}
