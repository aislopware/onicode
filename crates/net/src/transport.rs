//! The transport config every connection runs and the UDP socket it runs on.
//!
//! Ported from axolotty's `axquic` `transport.rs`, itself after slopty's `slopty-net::endpoint`
//! (MIT), where each value was measured.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use noq::{AckFrequencyConfig, Endpoint, IdleTimeout, TransportConfig, VarInt};

/// The UDP port every engine listens on, on each of its tailnet addresses.
pub const PORT: u16 = 7433;
/// QUIC's own keep-alive: a resting link carries a ping and its ACK this often, so NAT bindings
/// live and a restarted peer's stateless reset arrives. Interactive links ping faster on their
/// control stream (`onicode-peer`), so a resting mesh of laptops does not wake its radios every
/// second.
pub const KEEP_ALIVE: Duration = Duration::from_secs(10);
/// Silence after which a link is dropped. Generous: a laptop's radio gap is ridden out by QUIC
/// migration, and the control stream's own liveness notices a dead peer sooner.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// Longest the peer may sit on an ACK (QUIC's default is 25 ms): the window turns at the
/// path's round trip, and a lone echo is acknowledged before the next key.
const MAX_ACK_DELAY: Duration = Duration::from_millis(2);
/// The round trip assumed before the first sample: every path here is a LAN or a tailnet a few
/// milliseconds long, not RFC 9002's unknown Internet path of 333 ms.
const INITIAL_RTT: Duration = Duration::from_millis(5);
/// Every packet's UDP payload: 1280, Tailscale's TUN MTU and the least IPv6 carries, less 48
/// bytes of IPv6 and UDP header. Nothing to discover on a tailnet, so discovery is off: its
/// probes past the tunnel's MTU only look like loss (tailscale#2633).
pub const PATH_MTU: u16 = 1232;
/// What every QUIC path must carry, where black-hole detection falls back to.
pub const MIN_MTU: u16 = 1200;
/// The socket's receive buffer; macOS's default (768 KiB) overflows when a burst lands while
/// the endpoint's task is late.
const RECEIVE_BUFFER: usize = 4 << 20;
/// Datagram buffers (the echo copies are small; this bounds a burst of them). noq drops the
/// oldest datagram when full.
pub const DATAGRAM_BUFFER: usize = 1 << 20;
/// Initial congestion window in packets: a LAN or a tailnet affords Chromium's burst, and the
/// first screenful of a terminal is tens of kilobytes.
const INITIAL_WINDOW_PACKETS: u64 = 32;
/// Streams each way at once: an RPC call each, a terminal each, a tunnel's connections (a
/// browser opens six per origin), a file each.
pub const MAX_STREAMS: u32 = 1024;

/// A stream's receive window. quinn's default (1.25 MB) is 100 Mbit/s at 100 ms; a tailnet path
/// between Macs is a LAN or a few milliseconds of WAN, often 1 to 10 Gbit/s. 16 MiB covers the
/// bandwidth-delay product of 10 Gbit/s at 13 ms (or 1 Gbit/s at 130 ms), so one file stream can
/// fill the path alone. To be measured on the office Macs
/// (`docs/onicode/decisions/one-machine.md`).
pub const STREAM_WINDOW: u32 = 16 << 20;
/// The connection's receive window: four streams' worth, the ranges of a lone big file, so they
/// never wait on each other's credit. Credit, not memory: nothing is held until bytes arrive
/// unread.
pub const CONNECTION_WINDOW: u32 = 64 << 20;
/// QUIC's send buffer before the link first fits it (`link::SendWindow` then holds it to twice
/// the congestion window, which past a fast path's BDP already lets every byte BBR would send).
pub const SEND_WINDOW: u64 = 64 << 20;

/// Lowest send priority that goes ahead of queued datagrams: control, RPC, docs, registry and
/// terminal streams.
pub const AHEAD_OF_DATAGRAMS: i32 = 0;
/// A stream while it carries an echo: it leaves in the next packet, ahead of every other
/// stream, unpaced, and past the send window (noq-proto patches 8 and 23; slopty: p99 beside
/// six flooding sessions 30–73 → 11–25 ms).
pub const ECHO_PRIORITY: i32 = 1;

/// Set to `0`, a macOS endpoint sends and receives one datagram a call instead of through
/// Apple's batched `sendmsg_x` and `recvmsg_x` (to measure against the plain path).
pub const BATCHED_UDP_ENV: &str = "ONICODE_BATCHED_UDP";

/// The transport config every connection runs. Congestion control is BBR3 held to twice the
/// measured bandwidth-delay product (`congestion`): Cubic cut its window to a few packets on
/// Wi-Fi's real losses, and unbounded BBR3 filled the bottleneck's queue ahead of an echo.
/// `seed` draws BBR3's probes, so a simulated run repeats.
#[must_use]
pub fn transport_config(seed: Option<u64>) -> TransportConfig {
    let mut acks = AckFrequencyConfig::default();
    acks.max_ack_delay(Some(MAX_ACK_DELAY));
    let mut bbr = noq::congestion::Bbr3Config::default();
    bbr.initial_window(INITIAL_WINDOW_PACKETS * u64::from(MIN_MTU));
    bbr.probe_rng_seed(seed.map(spread_seed));
    let mut config = TransportConfig::default();
    config
        .congestion_controller_factory(Arc::new(crate::congestion::BoundedFactory::new(
            Arc::new(bbr),
            Some(crate::congestion::CEILING_BDPS),
        )))
        .ack_frequency_config(Some(acks))
        .initial_rtt(INITIAL_RTT)
        .initial_mtu(PATH_MTU)
        .min_mtu(MIN_MTU)
        .mtu_discovery_config(None)
        .max_idle_timeout(IdleTimeout::try_from(IDLE_TIMEOUT).ok())
        .keep_alive_interval(Some(KEEP_ALIVE))
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER)
        .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
        .receive_window(VarInt::from_u32(CONNECTION_WINDOW))
        .send_window(SEND_WINDOW)
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_STREAMS))
        .stream_priority_before_datagrams(Some(AHEAD_OF_DATAGRAMS))
        .stream_priority_unpaced(Some(ECHO_PRIORITY));
    config
}

/// Seeds noq's own draws from one number, so a seeded run repeats.
#[must_use]
pub fn spread_seed<const N: usize>(seed: u64) -> [u8; N] {
    let mut bytes = [0_u8; N];
    for (chunk, word) in bytes.chunks_mut(8).zip(0_u64..) {
        let mixed = seed ^ word.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for (byte, from) in chunk.iter_mut().zip(mixed.to_le_bytes()) {
            *byte = from;
        }
    }
    bytes
}

/// A QUIC endpoint on `local`; with `server` it also accepts. Must run inside a runtime.
///
/// # Errors
///
/// What binding or configuring the socket returns.
pub fn bind(local: SocketAddr, server: bool) -> std::io::Result<Endpoint> {
    let socket = udp_socket(local)?;
    socket.bind(&local.into())?;
    endpoint_on_udp(socket.into(), server, None)
}

/// An endpoint on a bound OS socket (one a caller has bound to an interface, say). On macOS it
/// takes Apple's batched datapath unless [`BATCHED_UDP_ENV`] is `0`.
///
/// # Errors
///
/// What registering the socket with the runtime returns.
pub fn endpoint_on_udp(
    socket: std::net::UdpSocket,
    server: bool,
    seed: Option<u64>,
) -> std::io::Result<Endpoint> {
    socket.set_nonblocking(true)?;
    #[cfg(unix)]
    let socket = crate::udp::wrap(socket, batched_udp())?;
    #[cfg(not(unix))]
    let socket = {
        use noq::Runtime as _;
        noq::TokioRuntime.wrap_udp_socket(socket)?
    };
    endpoint_on(socket, server, seed)
}

/// Whether a macOS socket takes the batched path, as the environment said at the first ask.
fn batched_udp() -> bool {
    static BATCHED: OnceLock<bool> = OnceLock::new();
    *BATCHED.get_or_init(|| {
        let on = std::env::var(BATCHED_UDP_ENV).map_or(true, |v| v.trim() != "0");
        tracing::debug!(batched = on, "UDP datapath");
        on
    })
}

/// An endpoint on any socket noq can drive: the OS's, or a simulated network's in tests.
/// `seed` draws every random choice noq makes, so a simulated run repeats.
///
/// # Errors
///
/// What noq returns for the socket.
pub fn endpoint_on(
    socket: Box<dyn noq::AsyncUdpSocket>,
    server: bool,
    seed: Option<u64>,
) -> std::io::Result<Endpoint> {
    let transport = Arc::new(transport_config(seed));
    let mut endpoint_config = crate::crypto::endpoint_config();
    endpoint_config.rng_seed(seed.map(spread_seed));
    let server_config = server.then(|| {
        let mut config = crate::crypto::server_config();
        config.transport_config(Arc::clone(&transport));
        config
    });
    let endpoint = Endpoint::new_with_abstract_socket(
        endpoint_config,
        server_config,
        socket,
        Arc::new(noq::TokioRuntime),
    )?;
    let mut client = crate::crypto::client_config();
    client.transport_config(transport);
    endpoint.set_default_client_config(client);
    Ok(endpoint)
}

/// An unbound UDP socket for `local`, dual-stack when `local` is the IPv6 wildcard, with a
/// 4 MiB receive buffer. The caller binds it (after binding it to an interface, if it does).
///
/// # Errors
///
/// What creating or configuring the socket returns.
pub fn udp_socket(local: SocketAddr) -> std::io::Result<socket2::Socket> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(local),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if local.is_ipv6() && local.ip().is_unspecified() {
        socket.set_only_v6(false)?;
    }
    // The OS clamps what it grants to kern.ipc.maxsockbuf and says nothing; less is still a link.
    let _ = socket.set_recv_buffer_size(RECEIVE_BUFFER);
    Ok(socket)
}

/// `addr` with an IPv4-mapped IPv6 address turned back into IPv4.
#[must_use]
pub const fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}
