//! Uniform interface to send and receive UDP packets with advanced features useful for QUIC
//!
//! This crate exposes kernel UDP stack features which are required for an efficient and
//! conformant QUIC implementation. As of this writing, these are not available in std or major
//! async runtimes, and their niche character and complexity are a barrier to adding them. Hence, a
//! dedicated crate.
//!
//! onicode's copy runs on macOS and iOS alone (27.2 and later, Apple silicon). Exposed features
//! include:
//!
//! - Batched sends and receives through `sendmsg_x`/`recvmsg_x` on macOS (the
//!   `fast-apple-datapath` feature), reducing system calls per datagram.
//! - Reporting the exact destination address of received packets and specifying explicit source
//!   addresses for sent packets, allowing responses to be sent from the address that the peer
//!   expects when there are multiple possibilities. This is common when bound to a wildcard address
//!   in IPv6 due to [RFC 8981] temporary addresses.
//! - [Explicit Congestion Notification], which is required by QUIC to prevent packet loss and
//!   reduce latency on congested links when supported by the network path.
//! - Disabled IP-layer fragmentation, which allows the true physical MTU to be detected and reduces
//!   risk of QUIC packet loss.
//!
//! [RFC 8981]: https://www.rfc-editor.org/rfc/rfc8981.html
//! [Explicit Congestion Notification]: https://www.rfc-editor.org/rfc/rfc3168.html
#![warn(unreachable_pub)]
#![warn(clippy::use_self)]

use std::{
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    os::fd::AsFd,
    sync::Mutex,
    time::{Duration, Instant},
};

#[cfg(feature = "fast-apple-datapath")]
mod apple_fast;

mod cmsg;

#[path = "unix.rs"]
mod imp;

#[allow(unused_imports, unused_macros)]
mod log {
    #[cfg(all(feature = "log", not(feature = "tracing-log")))]
    pub(crate) use log::{debug, error, info, trace, warn};

    #[cfg(feature = "tracing-log")]
    pub(crate) use tracing::{debug, error, info, trace, warn};

    #[cfg(not(any(feature = "log", feature = "tracing-log")))]
    mod no_op {
        macro_rules! trace    ( ($($tt:tt)*) => {{}} );
        macro_rules! debug    ( ($($tt:tt)*) => {{}} );
        macro_rules! info     ( ($($tt:tt)*) => {{}} );
        macro_rules! log_warn ( ($($tt:tt)*) => {{}} );
        macro_rules! error    ( ($($tt:tt)*) => {{}} );

        pub(crate) use {debug, error, info, log_warn as warn, trace};
    }

    #[cfg(not(any(feature = "log", feature = "tracing-log")))]
    pub(crate) use no_op::*;
}

pub use imp::UdpSocketState;

/// Number of UDP packets to send/receive at a time
pub const BATCH_SIZE: usize = imp::BATCH_SIZE;

/// Metadata for a single buffer filled with bytes received from the network
///
/// This associated buffer can contain one or more datagrams, see [`stride`].
///
/// [`stride`]: RecvMeta::stride
#[derive(Debug, Copy, Clone)]
#[non_exhaustive]
pub struct RecvMeta {
    /// The source address of the datagram(s) contained in the buffer
    pub addr: SocketAddr,
    /// The number of bytes the associated buffer has
    pub len: usize,
    /// The size of a single datagram in the associated buffer
    ///
    /// Apple platforms have no receive offload, so every buffer holds one datagram and this
    /// equals [`len`].
    ///
    /// [`len`]: RecvMeta::len
    pub stride: usize,
    /// The Explicit Congestion Notification bits for the datagram(s) in the buffer
    pub ecn: Option<EcnCodepoint>,
    /// The destination IP address which was encoded in this datagram
    pub dst_ip: Option<IpAddr>,
    /// The interface index of the interface on which the datagram was received
    ///
    /// Populated for IPv6 (`IPV6_PKTINFO`); IPv4's `IP_RECVDSTADDR` carries no index.
    pub interface_index: Option<u32>,
}

impl Default for RecvMeta {
    /// Constructs a value with arbitrary fields, intended to be overwritten
    fn default() -> Self {
        Self {
            addr: SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
            len: 0,
            stride: 0,
            ecn: None,
            dst_ip: None,
            interface_index: None,
        }
    }
}

/// An outgoing packet
#[derive(Debug, Clone)]
pub struct Transmit<'a> {
    /// The socket this datagram should be sent to
    pub destination: SocketAddr,
    /// Explicit congestion notification bits to set on the packet
    pub ecn: Option<EcnCodepoint>,
    /// Contents of the datagram
    pub contents: &'a [u8],
    /// The segment size if this transmission contains multiple datagrams.
    /// This is `None` if the transmit only contains a single datagram
    pub segment_size: Option<usize>,
    /// Optional source IP address for the datagram
    pub src_ip: Option<IpAddr>,
}

impl<'a> Transmit<'a> {
    /// The datagrams this transmit carries: its contents cut every `segment_size` bytes.
    pub fn datagram_count(&self) -> usize {
        match self.segment_size {
            Some(size) if size > 0 => self.contents.len().div_ceil(size).max(1),
            _ => 1,
        }
    }

    /// The transmit without its first `datagrams` datagrams, on the same contents: what is left
    /// to send after a partial send.
    pub fn advance(&self, datagrams: usize) -> Self {
        let skipped = match self.segment_size {
            Some(size) if size > 0 => datagrams.saturating_mul(size),
            _ if datagrams == 0 => 0,
            _ => self.contents.len(),
        };
        Self {
            destination: self.destination,
            ecn: self.ecn,
            contents: &self.contents[skipped.min(self.contents.len())..],
            segment_size: self.segment_size,
            src_ip: self.src_ip,
        }
    }
}

/// Returns true if the given I/O error corresponds to a message size error.
///
/// Useful, for example, after invoking `std::io::Error::last_os_error()`
/// to check if the last OS error was a message size error (`EMSGSIZE`).
pub fn is_msg_size_err(err: &io::Error) -> bool {
    err.raw_os_error() == Some(libc::EMSGSIZE)
}

/// Log at most 1 IO error per minute
const IO_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Logs a warning message when sendmsg fails
///
/// Logging will only be performed if at least [`IO_ERROR_LOG_INTERVAL`]
/// has elapsed since the last error was logged.
#[cfg(any(feature = "tracing-log", feature = "log"))]
fn log_sendmsg_error(
    last_send_error: &Mutex<Instant>,
    err: impl core::fmt::Debug,
    transmit: &Transmit<'_>,
) {
    let now = Instant::now();
    let last_send_error = &mut *last_send_error.lock().expect("poisend lock");
    if now.saturating_duration_since(*last_send_error) > IO_ERROR_LOG_INTERVAL {
        *last_send_error = now;
        log::debug!(
            "sendmsg error: {:?}, Transmit: {{ destination: {:?}, src_ip: {:?}, ecn: {:?}, len: {:?}, segment_size: {:?} }}",
            err,
            transmit.destination,
            transmit.src_ip,
            transmit.ecn,
            transmit.contents.len(),
            transmit.segment_size
        );
    }
}

// No-op
#[cfg(not(any(feature = "tracing-log", feature = "log")))]
fn log_sendmsg_error(_: &Mutex<Instant>, _: impl core::fmt::Debug, _: &Transmit<'_>) {}

/// A borrowed UDP socket, constructible via `From<T: AsFd>`.
// Wrapper around socket2 to avoid making it a public dependency and incurring stability risk
pub struct UdpSockRef<'a>(socket2::SockRef<'a>);

impl<'s, S> From<&'s S> for UdpSockRef<'s>
where
    S: AsFd,
{
    fn from(socket: &'s S) -> Self {
        Self(socket.into())
    }
}

/// Explicit congestion notification codepoint
#[repr(u8)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum EcnCodepoint {
    /// The ECT(0) codepoint, indicating that an endpoint is ECN-capable
    Ect0 = 0b10,
    /// The ECT(1) codepoint, indicating that an endpoint is ECN-capable
    Ect1 = 0b01,
    /// The CE codepoint, signalling that congestion was experienced
    Ce = 0b11,
}

impl EcnCodepoint {
    /// Create new object from the given bits
    pub fn from_bits(x: u8) -> Option<Self> {
        use EcnCodepoint::*;
        Some(match x & 0b11 {
            0b10 => Ect0,
            0b01 => Ect1,
            0b11 => Ce,
            _ => {
                return None;
            }
        })
    }
}
