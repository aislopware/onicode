//! What every stream opens with, and the frames after it.
//!
//! A stream's opener writes a [`StreamHead`] first: the [`StreamKind`] byte, then a big-endian
//! `u16` length and the stream's name (a file fetch's). Everything after is the kind's own:
//! control and RPC send `u32` big-endian length-prefixed frames ([`frame`]); a file sends raw
//! bytes. Datagrams start with a channel byte ([`channel`]) (slopty's framing, its transport
//! decisions).

use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};

use crate::NetError;

/// The longest stream name read.
pub const MAX_NAME: usize = 1024;
/// The longest frame a stream takes unless its kind says otherwise: an RPC reply is rarely more
/// than a few MB.
pub const MAX_FRAME: usize = 64 << 20;

/// What a stream carries, and so how it is sent (§3.2 of the transport design).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum StreamKind {
    /// The link's first stream, opened by the dialer: hellos, pings, path reports.
    Control = 0,
    /// One RPC call: the request, then the reply or a subscription's items.
    Rpc = 1,
    /// A file: a clipboard fetch.
    File = 6,
}

impl StreamKind {
    /// The kind its byte names.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Control,
            1 => Self::Rpc,
            6 => Self::File,
            _ => return None,
        })
    }

    /// The send priority the kind starts at: control and RPC at 0, ahead of datagrams; files
    /// behind them at −2.
    #[must_use]
    pub const fn priority(self) -> i32 {
        match self {
            Self::Control | Self::Rpc => 0,
            Self::File => -2,
        }
    }
}

/// The datagram channels: a datagram's first byte.
pub mod channel {
    /// Remote desktop, host to client: video, audio, cursor and clock
    /// (`onicode_desktop_media::wire`).
    pub const MEDIA: u8 = 2;
    /// Remote desktop, client to host: NACKs, refresh requests and clock reports.
    pub const FEEDBACK: u8 = 3;
    /// Remote desktop, client to host: the datagram copy of a numbered input
    /// (`onicode_desktop_link::copy`).
    pub const DESKTOP_INPUT: u8 = 4;
}

/// What a stream is, written by its opener before anything else.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamHead {
    /// What it carries.
    pub kind: StreamKind,
    /// Which one (a file fetch); empty for a kind with one per link or none.
    pub name: String,
}

impl StreamHead {
    /// A head of `kind` named `name`.
    #[must_use]
    pub fn new(kind: StreamKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }

    /// An RPC call's head.
    #[must_use]
    pub fn rpc() -> Self {
        Self::new(StreamKind::Rpc, String::new())
    }

    /// The head as it goes on the wire. A name past [`MAX_NAME`] is cut at a character
    /// boundary at or below it.
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut end = self.name.len().min(MAX_NAME);
        while !self.name.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let name = self.name.get(..end).unwrap_or_default();
        let mut out = BytesMut::with_capacity(3_usize.saturating_add(name.len()));
        out.put_u8(self.kind as u8);
        out.put_u16(u16::try_from(name.len()).unwrap_or(u16::MAX));
        out.put_slice(name.as_bytes());
        out.freeze()
    }

    /// Take a whole head off the front of `buf`; `None` until all of it is there.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] for an unknown kind, a name past [`MAX_NAME`] or one that is
    /// not UTF-8.
    pub fn take(buf: &mut BytesMut) -> Result<Option<Self>, NetError> {
        let Some(&byte) = buf.first() else {
            return Ok(None);
        };
        let kind = StreamKind::from_byte(byte).ok_or(NetError::Protocol("unknown stream kind"))?;
        let Some(&[hi, lo]) = buf.get(1..3) else {
            return Ok(None);
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        if len > MAX_NAME {
            return Err(NetError::Protocol("stream name too long"));
        }
        let end = 3_usize.saturating_add(len);
        let Some(name) = buf.get(3..end) else {
            return Ok(None);
        };
        let name = std::str::from_utf8(name)
            .map_err(|_utf8| NetError::Protocol("stream name not UTF-8"))?
            .to_owned();
        buf.advance(end);
        Ok(Some(Self { kind, name }))
    }
}

/// `payload` as one frame: its length as a big-endian `u32`, then the bytes.
///
/// # Errors
///
/// [`NetError::FrameTooLarge`] past [`MAX_FRAME`].
pub fn frame(payload: &[u8]) -> Result<Bytes, NetError> {
    if payload.len() > MAX_FRAME {
        return Err(NetError::FrameTooLarge(payload.len()));
    }
    let mut out = BytesMut::with_capacity(payload.len().saturating_add(4));
    out.put_u32(u32::try_from(payload.len()).unwrap_or(u32::MAX));
    out.put_slice(payload);
    Ok(out.freeze())
}

/// Take a whole frame off the front of `buf`; `None` until all of it is there.
///
/// # Errors
///
/// [`NetError::FrameTooLarge`] when the length says more than `max`.
pub fn take_frame(buf: &mut BytesMut, max: usize) -> Result<Option<Bytes>, NetError> {
    let Some(&[a, b, c, d]) = buf.get(..4) else {
        return Ok(None);
    };
    let len = usize::try_from(u32::from_be_bytes([a, b, c, d])).unwrap_or(usize::MAX);
    if len > max {
        return Err(NetError::FrameTooLarge(len));
    }
    if buf.len() < len.saturating_add(4) {
        return Ok(None);
    }
    buf.advance(4);
    Ok(Some(buf.split_to(len).freeze()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_reads_back_whole_and_waits_for_its_end() {
        let head = StreamHead::new(StreamKind::File, "fetch-1");
        let wire = head.encode();
        for cut in 0..wire.len() {
            let mut part = BytesMut::from(&wire[..cut]);
            assert_eq!(StreamHead::take(&mut part).unwrap(), None, "cut at {cut}");
        }
        let mut whole = BytesMut::from(&wire[..]);
        whole.extend_from_slice(b"rest");
        assert_eq!(StreamHead::take(&mut whole).unwrap(), Some(head));
        assert_eq!(&whole[..], b"rest", "what follows the head stays");
        let mut bad = BytesMut::from(&[9_u8, 0, 0][..]);
        assert!(StreamHead::take(&mut bad).is_err(), "an unknown kind");
        let mut long = BytesMut::from(&[1_u8, 0xff, 0xff][..]);
        assert!(
            StreamHead::take(&mut long).is_err(),
            "a name past the limit"
        );
        let cut = StreamHead::new(StreamKind::File, "é".repeat(MAX_NAME)).encode();
        assert!(cut.len() <= MAX_NAME + 3);
        assert!(
            StreamHead::take(&mut BytesMut::from(&cut[..]))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn frames_read_back_in_order_and_refuse_a_length_past_the_limit() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&frame(b"one").unwrap());
        buf.extend_from_slice(&frame(b"").unwrap());
        buf.extend_from_slice(&frame(b"three").unwrap()[..5]);
        assert_eq!(
            take_frame(&mut buf, MAX_FRAME).unwrap().as_deref(),
            Some(&b"one"[..])
        );
        assert_eq!(
            take_frame(&mut buf, MAX_FRAME).unwrap().as_deref(),
            Some(&b""[..])
        );
        assert_eq!(
            take_frame(&mut buf, MAX_FRAME).unwrap(),
            None,
            "half a frame waits"
        );
        let mut big = BytesMut::from(&frame(&[0; 100]).unwrap()[..]);
        assert!(matches!(
            take_frame(&mut big, 10),
            Err(NetError::FrameTooLarge(100))
        ));
        assert_eq!(StreamKind::File.priority(), -2);
        assert_eq!(
            StreamKind::from_byte(StreamKind::File as u8),
            Some(StreamKind::File)
        );
        assert_eq!(StreamKind::from_byte(2), None, "a retired kind");
    }
}
