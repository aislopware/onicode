//! The small fixed-layout parsers: each input is one datagram (or one payload) as the receiving
//! side gets it, after the link's channel byte.

use onicode_desktop_media::parse_cursor;
use onicode_desktop_media::wire::{CURSOR_BYTES, FramePrefix, HEADER_BYTES, MediaHeader};

/// A client's first look at a media datagram: the fixed header, then the frame prefix a
/// reassembled body opens with.
pub fn media_header(data: &[u8]) {
    let parsed = MediaHeader::parse(data);
    assert_eq!(
        parsed.is_some(),
        data.len() >= HEADER_BYTES,
        "a header parsed from too few bytes"
    );
    if let Some((header, payload)) = parsed {
        assert_eq!(
            payload.len(),
            data.len().saturating_sub(HEADER_BYTES),
            "payload length"
        );
        let _kind = header.kind();
        let _parity = header.is_parity();
    }
    if let Some((_prefix, rest)) = FramePrefix::parse(data) {
        assert!(rest.len() < data.len(), "the prefix took no bytes");
    }
}

/// A client reading a cursor datagram's payload.
pub fn cursor(data: &[u8]) {
    assert_eq!(
        parse_cursor(data).is_some(),
        data.len() >= CURSOR_BYTES,
        "a cursor parsed from too few bytes, or not from enough"
    );
}
