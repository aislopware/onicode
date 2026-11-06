//! Files between peers, as if they were local (`docs/onicode/decisions/one-machine.md`).
//!
//! The destination engine owns a job and pulls from the source engine over
//! `StreamKind::File` streams on the peer link it already has:
//!
//! * [`manifest`]: the source walks what it was asked for: trees (bundles too), symlinks as
//!   symlinks, mode, mtime and the [`meta`] AppleDouble of each item.
//! * [`serve`]: the source side: [`serve::Served`] holds the transfers registered through
//!   `ServeTransfer`, and [`serve::FileStreams`] answers `xfer/...` heads for them only (and hands
//!   `clip/...` heads to the clipboard).
//! * [`wire`]: the heads and framing. A range carries a 32-byte BLAKE3 after every block of
//!   [`BLOCK`] bytes, so a file arriving on four streams at once is checked block by block, and a
//!   one-block file's digest is its BLAKE3.
//! * [`plan`]: what is in flight: 8 files at once, a lone big file in ranges over 4 streams, small
//!   files packed back to back.
//! * [`pull`]: the destination side of one run: partial files in `.onicode-partial-<id>`, resume
//!   from the blocks on disk, conflicts, metadata, the atomic rename into place.
//! * [`transfers`]: the jobs: persisted, retried past dropped links and restarts, paused,
//!   cancelled, and reported at most every 100 ms.
//!
//! The I/O sits behind [`source::Source`] and [`sink::Sink`], so tests run on a local source,
//! a flaky one, or the simulated tailnet.

#![deny(unsafe_code)]

pub mod conflict;
pub mod fsops;
pub mod job;
pub mod manifest;
#[allow(unsafe_code)]
pub mod meta;
pub mod plan;
pub mod pull;
pub mod serve;
#[allow(unsafe_code)]
pub mod sink;
pub mod source;
pub mod spans;
pub mod store;
pub mod transfers;
pub mod wire;

use std::time::Duration;

use serde::{Deserialize, Serialize};
pub use serve::{ClipStreams, FileStreams, Served};
pub use sink::{Disk, Sink};
pub use source::{LinkSource, LinksSlot, LocalSource, Origins, PeerOrigins, Source};
pub use transfers::Transfers;

/// Bytes between digests on a range; a range starts on a block.
pub const BLOCK: u64 = 16 << 20;
/// Files under this many bytes ride in packs.
pub const SMALL: u64 = 256 << 10;
/// A lone file over this many bytes is split into ranges.
pub const SPLIT: u64 = 64 << 20;
/// How often a job's progress is reported, at most.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// The staging folder inside the destination folder is this and the job's id.
pub const PARTIAL_PREFIX: &str = ".onicode-partial-";

/// The scheduler's numbers. Only `block` is on the wire (`ServeTransfer` names it); the rest
/// are the puller's own. Tests shrink them to move a "big" file in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tuning {
    /// Bytes between digests.
    pub block: u64,
    /// A split file's range: a whole number of blocks.
    pub range: u64,
    /// Files under this ride in packs.
    pub small: u64,
    /// A lone file over this is split.
    pub split: u64,
    /// Units (files, ranges, packs) in flight.
    pub files: usize,
    /// Ranges of one file in flight.
    pub ranges: usize,
    /// The most bytes one pack carries.
    pub pack_bytes: u64,
    /// The most items one pack carries.
    pub pack_items: u32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            block: BLOCK,
            range: BLOCK,
            small: SMALL,
            split: SPLIT,
            files: 8,
            ranges: 4,
            pack_bytes: 8 << 20,
            pack_items: 1024,
        }
    }
}

impl Tuning {
    /// Whether the numbers make sense together.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.block >= 1024
            && self.range >= self.block
            && self.range.is_multiple_of(self.block)
            && self.small <= self.block
            && self.files >= 1
            && self.ranges >= 1
            && self.pack_items >= 1
    }
}

/// The reset codes a source answers a stream with.
pub mod reset {
    /// The head names no transfer registered here (or it was forgotten): register again.
    pub const UNKNOWN: u32 = 0x7801;
    /// The item changed or vanished since the manifest: fetch the manifest again.
    pub const CHANGED: u32 = 0x7802;
    /// The head does not parse, or asks for something not served.
    pub const BAD: u32 = 0x7803;
}

/// What a transfer can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Local disk I/O.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The link or the source dropped out; try again.
    #[error("link: {0}")]
    Link(String),
    /// The source does not know the transfer: register it again.
    #[error("the source no longer serves this transfer")]
    Unknown,
    /// An item changed at the source: fetch the manifest again.
    #[error("an item changed at the source")]
    Changed,
    /// The peer sent something malformed.
    #[error("protocol: {0}")]
    Protocol(String),
    /// A block's digest did not match the bytes: fetch it again.
    #[error("item {0} arrived damaged")]
    Digest(u32),
    /// The request cannot be served.
    #[error("{0}")]
    Refused(String),
    /// Paused or cancelled.
    #[error("stopped")]
    Stopped,
}

impl Error {
    /// Whether trying again later may succeed.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(self, Self::Link(_))
    }

    /// The reset code a source answers a stream with for this.
    #[must_use]
    pub const fn reset_code(&self) -> u32 {
        match self {
            Self::Unknown => reset::UNKNOWN,
            Self::Refused(_) | Self::Protocol(_) => reset::BAD,
            _ => reset::CHANGED,
        }
    }
}

impl From<onicode_net::NetError> for Error {
    fn from(error: onicode_net::NetError) -> Self {
        match error {
            onicode_net::NetError::Reset(code) if code == u64::from(reset::UNKNOWN) => {
                Self::Unknown
            }
            onicode_net::NetError::Reset(code) if code == u64::from(reset::CHANGED) => {
                Self::Changed
            }
            onicode_net::NetError::Reset(code) if code == u64::from(reset::BAD) => {
                Self::Refused("the source refused the request".into())
            }
            other => Self::Link(other.to_string()),
        }
    }
}

impl From<onicode_peer::PeerError> for Error {
    fn from(error: onicode_peer::PeerError) -> Self {
        use onicode_rpc::RpcError;
        match error {
            onicode_peer::PeerError::Net(net) => net.into(),
            onicode_peer::PeerError::Rpc(
                RpcError::Failed(message)
                | RpcError::BadParams(message)
                | RpcError::UnknownMethod(message)
                | RpcError::Capability(message),
            ) => Self::Refused(message),
            other => Self::Link(other.to_string()),
        }
    }
}

/// Runs `work` on the blocking pool.
///
/// # Errors
///
/// `work`'s error, or [`Error::Io`] if the task panicked.
pub(crate) async fn blocking<T, F>(work: F) -> Result<T, Error>
where
    F: FnOnce() -> Result<T, Error> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|join| Error::Io(std::io::Error::other(join.to_string())))?
}

/// Milliseconds since the epoch of `time`; 0 before it.
pub(crate) fn millis(time: std::time::SystemTime) -> i64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
