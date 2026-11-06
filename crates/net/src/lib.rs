//! onicode's transport: plaintext QUIC between peers over the tailnet.
//!
//! Tailscale finds the path, crosses NATs, encrypts and authenticates; this layer only carries
//! onicode's streams with the least delay it can, and admits a peer by what the tailnet says of
//! it. The QUIC stack is n0's noq with slopty's and axolotty's patches (`vendor/`), and the
//! tuning is theirs, every value measured there (ported from axolotty's `axquic` and slopty's
//! `slopty-net`).
//!
//! * [`crypto`]: the null QUIC crypto provider, QUIC version `ONI1`.
//! * [`transport`]: the transport config every connection runs, and binding an endpoint.
//! * [`congestion`]: BBR3 held to twice the measured bandwidth-delay product.
//! * [`link`]: one connection as onicode's link: kinded streams, datagram channels and the
//!   send window fitted to the congestion window.
//! * [`stream`]: the head every stream opens with, and the frames after it.
//! * [`hello`]: the first frame each way on the control stream, and the build-skew check.
//! * [`admission`]: who may connect, by Tailscale's whois, failing closed.
//! * [`redial`]: when a dropped link is dialled again.
//! * `udp`: the UDP socket under every endpoint, which finishes a send that went out in part.
//!
//! One QUIC connection per device pair carries a control stream (bidirectional, opened by the
//! dialer), one bidirectional stream per RPC call, document, terminal, tunnel or file, and
//! datagrams for keystroke and echo copies.

#![deny(unsafe_code)]

pub mod admission;
pub mod congestion;
pub mod crypto;
pub mod hello;
pub mod link;
pub mod redial;
pub mod stream;
pub mod transport;
#[cfg(unix)]
mod udp;

use std::sync::OnceLock;

pub use hello::{Hello, Role, Session, WIRE};
pub use link::{Link, StreamRx, StreamTx};
pub use noq::{Connection, Endpoint};
pub use stream::{StreamHead, StreamKind};

/// The codes a link is closed with, as its peer reads them ([`NetError::from_close`]).
pub mod close {
    /// Closed on purpose, with nothing wrong.
    pub const NORMAL: u32 = 0;
    /// The peer broke the protocol (no hello, a bad stream head).
    pub const PROTOCOL: u32 = 1;
    /// The peers run builds with different wires; the reason names the closer's build.
    pub const WRONG_BUILD: u32 = 2;
    /// The tailnet grants the peer no role here.
    pub const NOT_GRANTED: u32 = 3;
    /// Another link between the same two devices is kept (the smaller device-id's dial).
    pub const DUPLICATE: u32 = 4;
    /// A discovery probe, done once it has read the peer's hello.
    pub const PROBE: u32 = 5;
    /// The hello claimed a device-id another tailnet node holds.
    pub const DEVICE_MISMATCH: u32 = 6;
    /// The peer stopped answering on the control stream.
    pub const SILENT: u32 = 7;
}

/// Transport errors.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Binding a socket failed.
    #[error("bind {addr}: {source}")]
    Bind {
        /// The address it was bound to.
        addr: String,
        /// The OS's error.
        #[source]
        source: std::io::Error,
    },
    /// Connecting failed, for a reason none of the variants below names.
    #[error("connect: {0}")]
    Connect(String),
    /// The address did not answer the handshake in time: asleep, off the tailnet, or nothing
    /// listens there.
    #[error("{0}: no answer")]
    NoAnswer(String),
    /// The peer refused this device before the handshake (QUIC's `CONNECTION_REFUSED`).
    #[error("{0}: turned this device away")]
    Refused(String),
    /// The peer's tailnet grants this device no role there ([`close::NOT_GRANTED`]).
    #[error("not granted by the tailnet policy")]
    NotGranted,
    /// The peer runs a build with another wire ([`close::WRONG_BUILD`]).
    #[error(transparent)]
    WrongBuild(#[from] WrongBuild),
    /// Another link to the same device is kept ([`close::DUPLICATE`]).
    #[error("superseded by another link to the same device")]
    Duplicate,
    /// The hello's device-id belongs to another node ([`close::DEVICE_MISMATCH`]).
    #[error("the device-id belongs to another tailnet node")]
    DeviceMismatch,
    /// The peer stopped answering ([`close::SILENT`]).
    #[error("the peer stopped answering")]
    Silent,
    /// A QUIC stream or the connection failed.
    #[error("stream: {0}")]
    Stream(String),
    /// The peer closed the stream or connection.
    #[error("closed")]
    Closed,
    /// The peer reset the stream with this code.
    #[error("stream reset by the peer: code {0}")]
    Reset(u64),
    /// Something waited on the peer longer than it may.
    #[error("{0} timed out")]
    TimedOut(&'static str),
    /// The peer sent something not expected here.
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// A frame longer than the stream's limit.
    #[error("a frame of {0} bytes is past the limit")]
    FrameTooLarge(usize),
    /// The path takes no datagram that large, or none at all.
    #[error("the path takes no datagram of {0} bytes")]
    DatagramTooLarge(usize),
}

/// A peer whose wire differs from this build's: refused at the hello, never redialled into a
/// decode error.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WrongBuild {
    /// The peer's build and wire as it said them (`0.2.136 wire 1`); empty when it said
    /// nothing readable.
    pub peer: String,
    /// This build and wire, in the same form.
    pub this: String,
}

impl std::fmt::Display for WrongBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let peer = if self.peer.is_empty() {
            "an unreadable one"
        } else {
            &self.peer
        };
        write!(
            f,
            "the peer runs a different build ({peer}); this is {}",
            self.this
        )
    }
}

impl std::error::Error for WrongBuild {}

impl NetError {
    /// What a connection error says, with onicode's close codes read: a peer's close with
    /// [`close::WRONG_BUILD`] anywhere in the chain is [`NetError::WrongBuild`], naming the
    /// build its reason carries, and so on for the other codes.
    #[must_use]
    pub fn stream(e: &(dyn std::error::Error + 'static)) -> Self {
        if let Some(noq::ReadError::Reset(code)) = e.downcast_ref() {
            return Self::Reset(code.into_inner());
        }
        if let Some(close) = closed_with(e)
            && let Some(known) = Self::from_close(close)
        {
            return known;
        }
        let mut text = e.to_string();
        let mut source = e.source();
        while let Some(s) = source {
            let part = s.to_string();
            if !text.contains(&part) {
                text.push_str(": ");
                text.push_str(&part);
            }
            source = s.source();
        }
        Self::Stream(text)
    }

    /// The error a peer's close with one of onicode's codes means; `None` for any other.
    #[must_use]
    pub fn from_close(close: &noq::ApplicationClose) -> Option<Self> {
        let code = u32::try_from(close.error_code.into_inner()).ok()?;
        Some(match code {
            close::WRONG_BUILD => Self::WrongBuild(WrongBuild {
                peer: String::from_utf8_lossy(&close.reason).into_owned(),
                this: hello::this_build(),
            }),
            close::NOT_GRANTED => Self::NotGranted,
            close::DUPLICATE => Self::Duplicate,
            close::DEVICE_MISMATCH => Self::DeviceMismatch,
            close::SILENT => Self::Silent,
            _ => return None,
        })
    }

    /// Whether the failure is about the peer's build, so a redial waits until it changes.
    #[must_use]
    pub const fn is_wrong_build(&self) -> bool {
        matches!(self, Self::WrongBuild(_))
    }
}

/// How the peer closed the connection, found anywhere in `e`'s source chain.
fn closed_with<'e>(e: &'e (dyn std::error::Error + 'static)) -> Option<&'e noq::ApplicationClose> {
    let mut at = Some(e);
    while let Some(err) = at {
        if let Some(noq::ConnectionError::ApplicationClosed(close)) = err.downcast_ref() {
            return Some(close);
        }
        at = err.source();
    }
    None
}

/// The process's QUIC runtime: two threads at user-interactive QoS, so a keystroke is never
/// queued behind a background thread on a busy Mac (slopty: worker p99 11–569 → 0.7–11 ms
/// under an all-core load).
///
/// # Panics
///
/// When the OS refuses the runtime's threads at the first call.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("onicode-quic")
            .on_thread_start(interactive)
            .enable_all()
            .build()
            .expect("the QUIC runtime starts")
    })
}

#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
fn interactive() {
    // SAFETY: sets the calling thread's own QoS class; no pointers involved.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

#[cfg(not(target_vendor = "apple"))]
const fn interactive() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("connection lost")]
    struct LostTo(#[source] noq::ConnectionError);

    fn closed_for(code: u32, reason: &'static [u8]) -> LostTo {
        let close = noq::ApplicationClose {
            error_code: noq::VarInt::from_u32(code),
            reason: bytes::Bytes::from_static(reason),
        };
        LostTo(noq::ConnectionError::ApplicationClosed(close))
    }

    /// Each of onicode's close codes reads as its own error wherever it sits in the chain; a
    /// clean close is a stream error with its text.
    #[test]
    fn close_codes_read_as_their_errors() {
        let e = NetError::stream(&closed_for(close::WRONG_BUILD, b"0.1.0 wire 0"));
        let NetError::WrongBuild(wrong) = &e else {
            panic!("{e:?}")
        };
        assert_eq!(wrong.peer, "0.1.0 wire 0");
        assert!(
            e.to_string().contains("different build (0.1.0 wire 0)"),
            "{e}"
        );
        assert!(matches!(
            NetError::stream(&closed_for(close::NOT_GRANTED, b"")),
            NetError::NotGranted
        ));
        assert!(matches!(
            NetError::stream(&closed_for(close::DUPLICATE, b"").0),
            NetError::Duplicate
        ));
        assert!(matches!(
            NetError::stream(&closed_for(close::SILENT, b"")),
            NetError::Silent
        ));
        let normal = NetError::stream(&closed_for(close::NORMAL, b""));
        assert!(
            matches!(&normal, NetError::Stream(text) if text.contains("closed by peer")),
            "{normal}"
        );
        let reset = noq::ReadError::Reset(noq::VarInt::from_u32(2));
        assert!(matches!(NetError::stream(&reset), NetError::Reset(2)));
    }
}
