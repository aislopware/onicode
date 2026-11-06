//! The datagram copy of a numbered request ([`crate::order`]).
//!
//! The payload is the [`Numbered`] request's JSON, the same bytes the control stream's frame
//! carries, sent on [`CHANNEL`] with `onicode_net::Link::copy` a couple of milliseconds after
//! the stream copy, so the two are never lost in one packet. A copy that does not fit a
//! datagram (a long text) is not sent: the stream copy carries it.

use bytes::Bytes;

use crate::order::Numbered;

/// The datagram channel of input copies, client to desktopd: `onicode_net::stream::channel::
/// DESKTOP_INPUT`.
pub const CHANNEL: u8 = 4;

/// The datagram's payload, after the channel byte.
#[must_use]
pub fn encode(numbered: &Numbered) -> Bytes {
    crate::encode(numbered)
}

/// A copy read back; `None` for anything else.
#[must_use]
pub fn decode(payload: &[u8]) -> Option<Numbered> {
    crate::decode(payload).ok()
}

#[cfg(test)]
mod tests {
    use onicode_desktop_proto::StreamId;
    use onicode_desktop_proto::screen::{ScreenInput, ScreenRequest};

    use super::*;
    use crate::order::Numberer;

    #[test]
    fn a_copy_reads_back_and_garbage_does_not() {
        let numbered = Numberer::default()
            .number(ScreenRequest::Input {
                stream: StreamId(3),
                input: ScreenInput::Move { x: 10.5, y: 4.0 },
            })
            .unwrap();
        assert_eq!(decode(&encode(&numbered)), Some(numbered));
        assert_eq!(decode(b"\x00\x01"), None);
    }
}
