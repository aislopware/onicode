//! Cursor position datagrams: one tiny unreliable packet per pointer move.

use bytes::{BufMut as _, Bytes, BytesMut};
use zerocopy::little_endian::{I32, U16, U32};
use zerocopy::{FromBytes as _, IntoBytes as _};

use crate::wire::{CURSOR_BYTES, CursorUpdate, HEADER_BYTES, Kind, MediaHeader, StreamId};

/// Build a cursor datagram. `seq` orders updates; the receiver keeps the highest.
#[must_use]
pub fn cursor_datagram(
    stream: StreamId,
    seq: u32,
    send_ms_lo: u8,
    x: i32,
    y: i32,
    visible: bool,
) -> Bytes {
    let header = MediaHeader {
        stream: U32::new(stream.0),
        frame: U32::new(seq),
        index: U16::new(0),
        data_count: U16::new(1),
        parity_count: 0,
        kind: Kind::Cursor as u8,
        flags: 0,
        send_ms_lo,
    };
    let body = CursorUpdate {
        x: I32::new(x),
        y: I32::new(y),
        visible: u8::from(visible),
        reserved: [0; 3],
    };
    let mut buf = BytesMut::with_capacity(HEADER_BYTES.saturating_add(CURSOR_BYTES));
    buf.put_slice(header.as_bytes());
    buf.put_slice(body.as_bytes());
    buf.freeze()
}

/// Parse the payload of a [`Kind::Cursor`] datagram.
#[must_use]
pub fn parse_cursor(payload: &[u8]) -> Option<CursorUpdate> {
    CursorUpdate::read_from_prefix(payload)
        .ok()
        .map(|(update, _rest)| update)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip() {
        let dg = cursor_datagram(StreamId(9), 77, 3, -5, 1200, true);
        let (header, payload) = MediaHeader::parse(&dg).unwrap();
        assert_eq!(header.kind(), Some(Kind::Cursor));
        assert_eq!(header.frame.get(), 77);
        let cursor = parse_cursor(payload).unwrap();
        assert_eq!(
            (cursor.x.get(), cursor.y.get(), cursor.visible),
            (-5, 1200, 1)
        );
        assert!(parse_cursor(&payload[..CURSOR_BYTES - 1]).is_none());
    }
}
