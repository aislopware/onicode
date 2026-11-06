//! The first frame each way on the control stream, and the check that both ends speak one wire.
//!
//! After slopty's wire prefix (`slopty-net` `prefix.rs`, MIT), reduced to one generation
//! number: there is no backward compatibility, so two builds either share [`WIRE`] or refuse
//! each other at the hello. The end that reads another wire closes the connection with
//! [`close::WRONG_BUILD`] and its own build as the reason; the other end reads that close (or
//! the hello itself) as [`NetError::WrongBuild`], whichever comes first, so a person is told
//! which side is older instead of seeing a decode error.
//!
//! ```text
//! dialer    opens the control stream: StreamHead{Control} ‖ frame(Hello)  ─▶
//!                                                         ◀─  frame(Hello)   acceptor
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
pub use onicode_tailnet::Role;
use serde::{Deserialize, Serialize};

use crate::link::{Link, StreamRx, StreamTx};
use crate::stream::{StreamHead, StreamKind};
use crate::{NetError, WrongBuild, close};

/// The wire generation: bumped by any change a peer of the last generation could misread
/// (a frame's shape, a stream kind's protocol, a close code). Peers refuse every other.
pub const WIRE: u32 = 1;

/// How long the QUIC handshake may take: a tailnet path is milliseconds long, DERP a few
/// hundred at worst.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the peer has to open the control stream and say hello once connected.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// The capability a discovery probe's hello carries: it closes with [`close::PROBE`] once it
/// has read the answer, and the acceptor keeps no link for it.
pub const CAP_PROBE: &str = "probe";
/// The server name every dial gives; nothing checks it.
const SERVER_NAME: &str = "onicode";
/// The longest hello read.
const MAX_HELLO: usize = 64 << 10;

/// This build and its wire, as a person reads them: `0.2.136 wire 1`.
#[must_use]
pub fn this_build() -> String {
    format!("{} wire {WIRE}", env!("CARGO_PKG_VERSION"))
}

/// What each end says first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The wire generation ([`WIRE`]).
    pub wire: u32,
    /// The build, for the person told about a skew.
    pub build: String,
    /// The app identity (`{data_dir}/device-id`).
    #[serde(rename = "deviceId")]
    pub device_id: String,
    /// What this end is.
    pub role: Role,
    /// `macos`, `linux`, `ios`, …
    pub os: String,
    /// What this end can do beyond the wire's baseline.
    #[serde(default)]
    pub caps: Vec<String>,
}

/// The part of a hello every wire generation keeps, read when the rest does not parse.
#[derive(Deserialize)]
struct Skew {
    wire: u32,
    #[serde(default)]
    build: String,
}

impl Hello {
    /// This build's hello for `device_id` in `role`.
    #[must_use]
    pub fn new(device_id: impl Into<String>, role: Role) -> Self {
        Self {
            wire: WIRE,
            build: env!("CARGO_PKG_VERSION").to_owned(),
            device_id: device_id.into(),
            role,
            os: std::env::consts::OS.to_owned(),
            caps: Vec::new(),
        }
    }

    /// Whether it carries `cap`.
    #[must_use]
    pub fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c == cap)
    }

    /// Whether it is a discovery probe's ([`CAP_PROBE`]).
    #[must_use]
    pub fn is_probe(&self) -> bool {
        self.has(CAP_PROBE)
    }

    fn encode(&self) -> Result<Bytes, NetError> {
        serde_json::to_vec(self)
            .map(Bytes::from)
            .map_err(|_json| NetError::Protocol("hello does not encode"))
    }
}

/// A link whose hellos are exchanged.
#[derive(Debug)]
pub struct Session {
    /// The link.
    pub link: Link,
    /// What the peer said.
    pub peer: Hello,
    /// The control stream's sending half.
    pub control_tx: StreamTx,
    /// The control stream's receiving half, past the peer's hello.
    pub control_rx: StreamRx,
    /// The peer's address.
    pub remote: SocketAddr,
    /// Whether this end dialled.
    pub dialed: bool,
}

/// Dials `addr` and exchanges hellos.
///
/// # Errors
///
/// [`NetError::NoAnswer`], [`NetError::Refused`], [`NetError::NotGranted`],
/// [`NetError::WrongBuild`] as the peer answered; [`NetError::Connect`] otherwise.
pub async fn dial(
    endpoint: &noq::Endpoint,
    addr: SocketAddr,
    hello: &Hello,
) -> Result<Session, NetError> {
    dial_within(endpoint, addr, hello, HANDSHAKE_TIMEOUT).await
}

/// [`dial`] with the QUIC handshake held to `budget`.
///
/// # Errors
///
/// As [`dial`].
pub async fn dial_within(
    endpoint: &noq::Endpoint,
    addr: SocketAddr,
    hello: &Hello,
    budget: Duration,
) -> Result<Session, NetError> {
    let connecting = endpoint
        .connect(addr, SERVER_NAME)
        .map_err(|e| NetError::Connect(format!("{addr}: {e}")))?;
    let conn = tokio::time::timeout(budget, connecting)
        .await
        .map_err(|_elapsed| NetError::NoAnswer(addr.to_string()))?
        .map_err(|e| connect_error(addr, &e))?;
    let link = Link::new(conn);
    let (control_tx, mut control_rx) = link.open(&StreamHead::new(StreamKind::Control, "")).await?;
    control_tx.send_frame(&hello.encode()?)?;
    let peer = read_hello(&link, &mut control_rx).await?;
    Ok(Session {
        link,
        peer,
        control_tx,
        control_rx,
        remote: crate::transport::canonical(addr),
        dialed: true,
    })
}

/// Exchanges hellos on a connection just accepted (and admitted) from `remote`.
///
/// # Errors
///
/// [`NetError::Protocol`] when the peer's first stream is not the control stream,
/// [`NetError::WrongBuild`] on a skew, [`NetError::TimedOut`] for a peer that says nothing.
pub async fn accept(
    conn: noq::Connection,
    remote: SocketAddr,
    hello: &Hello,
) -> Result<Session, NetError> {
    let remote = crate::transport::canonical(remote);
    let link = Link::new(conn);
    let (control_tx, mut control_rx) =
        tokio::time::timeout(HELLO_TIMEOUT, link.accept())
            .await
            .map_err(|_elapsed| NetError::TimedOut("control stream"))??;
    let head = tokio::time::timeout(HELLO_TIMEOUT, control_rx.head())
        .await
        .map_err(|_elapsed| NetError::TimedOut("control stream head"))??;
    if head.kind != StreamKind::Control {
        link.close(close::PROTOCOL, b"control stream first");
        return Err(NetError::Protocol(
            "the first stream must be the control stream",
        ));
    }
    control_tx.send_frame(&hello.encode()?)?;
    let peer = read_hello(&link, &mut control_rx).await?;
    Ok(Session {
        link,
        peer,
        control_tx,
        control_rx,
        remote,
        dialed: false,
    })
}

/// Dials `addr` within `budget`, reads its hello and closes as a probe ([`close::PROBE`]).
///
/// # Errors
///
/// As [`dial`].
pub async fn probe(
    endpoint: &noq::Endpoint,
    addr: SocketAddr,
    hello: &Hello,
    budget: Duration,
) -> Result<Hello, NetError> {
    let mut hello = hello.clone();
    hello.caps.push(CAP_PROBE.to_owned());
    let session = tokio::time::timeout(budget, dial_within(endpoint, addr, &hello, budget))
        .await
        .map_err(|_elapsed| NetError::NoAnswer(addr.to_string()))??;
    session.link.close(close::PROBE, b"probe");
    Ok(session.peer)
}

/// Reads the peer's hello off the control stream; a wire other than [`WIRE`] closes the link
/// with [`close::WRONG_BUILD`].
async fn read_hello(link: &Link, rx: &mut StreamRx) -> Result<Hello, NetError> {
    rx.set_max_frame(MAX_HELLO);
    let frame = tokio::time::timeout(HELLO_TIMEOUT, rx.frame())
        .await
        .map_err(|_elapsed| NetError::TimedOut("hello"))??
        .ok_or(NetError::Closed)?;
    rx.set_max_frame(crate::stream::MAX_FRAME);
    match serde_json::from_slice::<Hello>(&frame) {
        Ok(peer) if peer.wire == WIRE => return Ok(peer),
        Ok(_) | Err(_) => {}
    }
    let peer = serde_json::from_slice::<Skew>(&frame).map_or_else(
        |_json| String::new(),
        |s| format!("{} wire {}", s.build, s.wire),
    );
    if peer.ends_with(&format!(" wire {WIRE}")) {
        link.close(close::PROTOCOL, b"unreadable hello");
        return Err(NetError::Protocol("unreadable hello"));
    }
    link.close(close::WRONG_BUILD, this_build().as_bytes());
    Err(WrongBuild {
        peer,
        this: this_build(),
    }
    .into())
}

/// What a failed QUIC handshake to `addr` means.
fn connect_error(addr: SocketAddr, e: &noq::ConnectionError) -> NetError {
    match (e, NetError::stream(e)) {
        (_, said @ (NetError::NotGranted | NetError::WrongBuild(_))) => said,
        (noq::ConnectionError::ConnectionClosed(close), _)
            if close.error_code == noq::TransportErrorCode::CONNECTION_REFUSED =>
        {
            NetError::Refused(addr.to_string())
        }
        (noq::ConnectionError::TimedOut, _) => NetError::NoAnswer(addr.to_string()),
        _ => NetError::Connect(format!("{addr}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_carries_the_wire_and_the_build() {
        let hello = Hello::new("dev-a", Role::Engine);
        assert_eq!(hello.wire, WIRE);
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["deviceId"], "dev-a");
        assert_eq!(json["role"], "engine");
        assert!(this_build().ends_with(&format!("wire {WIRE}")));
        let mut probe = hello;
        probe.caps.push(CAP_PROBE.to_owned());
        assert!(probe.is_probe());
    }
}
