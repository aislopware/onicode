//! The `xfer/...` heads and what follows them on a file stream.
//!
//! * `xfer/<id>/manifest`: a `u32` big-endian length, then the manifest as JSON.
//! * `xfer/<id>/<item>/<offset>/<len>`: the bytes of the item from `offset`, which is on a block,
//!   with a 32-byte BLAKE3 of each block after it (the last block ends at `offset + len`). A
//!   one-block file's digest is its BLAKE3. An item marked `zstd` in the manifest sends each block
//!   as frames of a `u32` big-endian length and a zstd frame of at most [`CHUNK`] bytes once
//!   decompressed, then the block's digest of the bytes before compression.
//! * `xfer/<id>/pack/<first>/<count>`: each item a `u64` big-endian length, its bytes and their
//!   BLAKE3; an item that is not a file is sent empty.

use async_trait::async_trait;
use bytes::{Buf as _, Bytes, BytesMut};

use crate::Error;

/// The head prefix of a transfer's streams.
pub const XFER: &str = "xfer/";
/// The head prefix of the clipboard's streams.
pub const CLIP: &str = "clip/";
/// A digest's bytes.
pub const DIGEST: usize = 32;
/// The most bytes read, compressed or sent at once.
pub const CHUNK: usize = 256 << 10;
/// The longest compressed frame taken: zstd's bound for a [`CHUNK`].
pub const MAX_ZFRAME: usize = CHUNK + (CHUNK >> 7) + 1024;
/// The longest manifest taken.
pub const MAX_MANIFEST: usize = 256 << 20;
/// The longest packed item taken.
pub const MAX_PACKED: u64 = 16 << 20;

/// What a stream asks of a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// The manifest.
    Manifest,
    /// A range of one item.
    Range {
        /// The item.
        item: u32,
        /// Where it starts.
        offset: u64,
        /// How long.
        len: u64,
    },
    /// Several items whole.
    Pack {
        /// The first item.
        first: u32,
        /// How many.
        count: u32,
    },
}

impl From<crate::plan::Unit> for Request {
    fn from(unit: crate::plan::Unit) -> Self {
        match unit {
            crate::plan::Unit::Range { item, offset, len } => Self::Range { item, offset, len },
            crate::plan::Unit::Pack { first, count } => Self::Pack { first, count },
        }
    }
}

/// The head that asks `request` of `transfer`.
#[must_use]
pub fn head(transfer: &str, request: &Request) -> String {
    match *request {
        Request::Manifest => format!("{XFER}{transfer}/manifest"),
        Request::Range { item, offset, len } => format!("{XFER}{transfer}/{item}/{offset}/{len}"),
        Request::Pack { first, count } => format!("{XFER}{transfer}/pack/{first}/{count}"),
    }
}

/// The transfer and the request a head names.
///
/// # Errors
///
/// [`Error::Refused`] for a head that does not parse.
pub fn parse(name: &str) -> Result<(&str, Request), Error> {
    let bad = || Error::Refused(format!("bad head {name:?}"));
    let rest = name.strip_prefix(XFER).ok_or_else(bad)?;
    let parts: Vec<&str> = rest.split('/').collect();
    let number = |s: &str| s.parse::<u64>().map_err(|_| bad());
    let index = |s: &str| s.parse::<u32>().map_err(|_| bad());
    match *parts.as_slice() {
        [id, "manifest"] if !id.is_empty() => Ok((id, Request::Manifest)),
        [id, "pack", first, count] if !id.is_empty() => Ok((
            id,
            Request::Pack {
                first: index(first)?,
                count: index(count)?,
            },
        )),
        [id, item, offset, len] if !id.is_empty() => Ok((
            id,
            Request::Range {
                item: index(item)?,
                offset: number(offset)?,
                len: number(len)?,
            },
        )),
        _ => Err(bad()),
    }
}

/// The blocks of a range: `(start, end)` of each, the first at `offset`. An empty range is one
/// empty block, so an empty file still has a digest.
#[must_use]
pub fn blocks(offset: u64, len: u64, block: u64) -> Vec<(u64, u64)> {
    let end = offset.saturating_add(len);
    let mut out = Vec::new();
    let mut at = offset;
    loop {
        let next = at.saturating_add(block.max(1)).min(end);
        out.push((at, next));
        at = next;
        if at >= end {
            return out;
        }
    }
}

/// How many blocks a file of `size` bytes has: at least one.
#[must_use]
pub const fn block_count(size: u64, block: u64) -> u64 {
    if size == 0 || block == 0 {
        1
    } else {
        size.div_ceil(block)
    }
}

/// The bytes of a stream, as they arrive.
#[async_trait]
pub trait Wire: Send {
    /// The next bytes; `None` at the end.
    ///
    /// # Errors
    ///
    /// The stream's.
    async fn chunk(&mut self) -> Result<Option<Bytes>, Error>;
}

/// Where a source writes a stream's bytes, holding it back while the link is behind.
#[async_trait]
pub trait Out: Send {
    /// Sends `data` in order.
    ///
    /// # Errors
    ///
    /// When the stream has ended.
    async fn send(&mut self, data: Bytes) -> Result<(), Error>;
}

/// A [`Wire`] read in pieces of a given size.
pub struct Reader {
    wire: Box<dyn Wire>,
    buf: BytesMut,
}

impl Reader {
    /// Reads `wire`.
    #[must_use]
    pub fn new(wire: Box<dyn Wire>) -> Self {
        Self {
            wire,
            buf: BytesMut::new(),
        }
    }

    async fn fill(&mut self) -> Result<(), Error> {
        match self.wire.chunk().await? {
            Some(chunk) => {
                self.buf.extend_from_slice(&chunk);
                Ok(())
            }
            None => Err(Error::Protocol("the stream ended early".into())),
        }
    }

    /// Exactly `n` bytes.
    ///
    /// # Errors
    ///
    /// The stream's, or [`Error::Protocol`] when it ends first.
    pub async fn exact(&mut self, n: usize) -> Result<Bytes, Error> {
        while self.buf.len() < n {
            self.fill().await?;
        }
        Ok(self.buf.split_to(n).freeze())
    }

    /// Between 1 and `max` bytes, as many as have come.
    ///
    /// # Errors
    ///
    /// As [`Self::exact`].
    pub async fn some(&mut self, max: usize) -> Result<Bytes, Error> {
        if self.buf.is_empty() {
            self.fill().await?;
        }
        let n = self.buf.len().min(max);
        Ok(self.buf.split_to(n).freeze())
    }

    /// A big-endian `u32`.
    ///
    /// # Errors
    ///
    /// As [`Self::exact`].
    pub async fn u32(&mut self) -> Result<u32, Error> {
        let mut bytes = self.exact(4).await?;
        Ok(bytes.get_u32())
    }

    /// A big-endian `u64`.
    ///
    /// # Errors
    ///
    /// As [`Self::exact`].
    pub async fn u64(&mut self) -> Result<u64, Error> {
        let mut bytes = self.exact(8).await?;
        Ok(bytes.get_u64())
    }

    /// A digest.
    ///
    /// # Errors
    ///
    /// As [`Self::exact`].
    pub async fn digest(&mut self) -> Result<[u8; DIGEST], Error> {
        let bytes = self.exact(DIGEST).await?;
        let mut out = [0_u8; DIGEST];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_read_back() {
        for request in [
            Request::Manifest,
            Request::Range {
                item: 7,
                offset: 16 << 20,
                len: 5,
            },
            Request::Pack { first: 3, count: 9 },
        ] {
            let name = head("t-1", &request);
            assert_eq!(parse(&name).unwrap(), ("t-1", request));
        }
        for bad in [
            "xfer/",
            "xfer//manifest",
            "xfer/t/1/2",
            "xfer/t/x/0/1",
            "clip/1/x",
            "xfer/t/pack/1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn blocks_cover_a_range() {
        assert_eq!(blocks(0, 0, 16), [(0, 0)]);
        assert_eq!(blocks(16, 40, 16), [(16, 32), (32, 48), (48, 56)]);
        assert_eq!(block_count(0, 16), 1);
        assert_eq!(block_count(33, 16), 3);
    }
}
