use std::{
    io::{self, IoSlice, IoSliceMut},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket},
    os::fd::AsRawFd,
    slice,
};

use noq_udp::{EcnCodepoint, RecvMeta, Transmit, UdpSocketState};
use socket2::{MsgHdr, Socket};

#[test]
fn basic() {
    let send = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap();
    let recv = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap();
    let dst_addr = recv.local_addr().unwrap();
    test_send_recv(
        &send.into(),
        &recv.into(),
        Transmit {
            destination: dst_addr,
            ecn: None,
            contents: b"hello",
            segment_size: None,
            src_ip: None,
        },
    );
}

#[test]
fn basic_src_ip() {
    let send = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap();
    let recv = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap();
    let src_ip = send.local_addr().unwrap().ip();
    let dst_addr = recv.local_addr().unwrap();
    test_send_recv(
        &send.into(),
        &recv.into(),
        Transmit {
            destination: dst_addr,
            ecn: None,
            contents: b"hello",
            segment_size: None,
            src_ip: Some(src_ip),
        },
    );
}

#[test]
fn ecn_v6() {
    let send = Socket::from(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap());
    let recv = Socket::from(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap());
    for codepoint in [EcnCodepoint::Ect0, EcnCodepoint::Ect1] {
        test_send_recv(
            &send,
            &recv,
            Transmit {
                destination: recv.local_addr().unwrap().as_socket().unwrap(),
                ecn: Some(codepoint),
                contents: b"hello",
                segment_size: None,
                src_ip: None,
            },
        );
    }
}

#[test]
fn ecn_v4() {
    let send = Socket::from(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
    let recv = Socket::from(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
    for codepoint in [EcnCodepoint::Ect0, EcnCodepoint::Ect1] {
        test_send_recv(
            &send,
            &recv,
            Transmit {
                destination: recv.local_addr().unwrap().as_socket().unwrap(),
                ecn: Some(codepoint),
                contents: b"hello",
                segment_size: None,
                src_ip: None,
            },
        );
    }
}

#[test]
fn ecn_v6_dualstack() {
    let recv = Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    recv.set_only_v6(false).unwrap();
    // We must use the unspecified address here, rather than a local address, to support dual-stack
    // mode
    recv.bind(&socket2::SockAddr::from(
        "[::]:0".parse::<SocketAddr>().unwrap(),
    ))
    .unwrap();
    let recv_v6 = SocketAddr::V6(SocketAddrV6::new(
        Ipv6Addr::LOCALHOST,
        recv.local_addr().unwrap().as_socket().unwrap().port(),
        0,
        0,
    ));
    let recv_v4 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, recv_v6.port()));
    for (src, dst) in [
        (SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0), recv_v6),
        (SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), recv_v4),
    ] {
        dbg!(src, dst);
        let send = UdpSocket::bind(src).unwrap();
        let send = Socket::from(send);
        for codepoint in [EcnCodepoint::Ect0, EcnCodepoint::Ect1] {
            test_send_recv(
                &send,
                &recv,
                Transmit {
                    destination: dst,
                    ecn: Some(codepoint),
                    contents: b"hello",
                    segment_size: None,
                    src_ip: None,
                },
            );
        }
    }
}

#[test]
fn ecn_v4_mapped_v6() {
    let send = Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    send.set_only_v6(false).unwrap();
    send.bind(&socket2::SockAddr::from(
        "[::]:0".parse::<SocketAddr>().unwrap(),
    ))
    .unwrap();

    let recv = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let recv = Socket::from(recv);
    let recv_v4_mapped_v6 = SocketAddr::V6(SocketAddrV6::new(
        Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
        recv.local_addr().unwrap().as_socket().unwrap().port(),
        0,
        0,
    ));

    for codepoint in [EcnCodepoint::Ect0, EcnCodepoint::Ect1] {
        test_send_recv(
            &send,
            &recv,
            Transmit {
                destination: recv_v4_mapped_v6,
                ecn: Some(codepoint),
                contents: b"hello",
                segment_size: None,
                src_ip: None,
            },
        );
    }
}

#[test]
fn socket_buffers() {
    const BUFFER_SIZE: usize = 123456;

    let send = Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    let recv = Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    for sock in [&send, &recv] {
        sock.bind(&socket2::SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            0,
        )))
        .unwrap();

        let socket_state = UdpSocketState::new(sock.into()).expect("created socket state");

        // Change the send buffer size.
        let buffer_before = socket_state.send_buffer_size(sock.into()).unwrap();
        assert_ne!(
            buffer_before,
            BUFFER_SIZE,
            "make sure buffer is not already desired size"
        );
        socket_state
            .set_send_buffer_size(sock.into(), BUFFER_SIZE)
            .expect("set send buffer size {buffer_before} -> {BUFFER_SIZE}");
        let buffer_after = socket_state.send_buffer_size(sock.into()).unwrap();
        assert_eq!(
            buffer_after,
            BUFFER_SIZE,
            "setting send buffer size to {BUFFER_SIZE} resulted in {buffer_before} -> {buffer_after}",
        );

        // Change the receive buffer size.
        let buffer_before = socket_state.recv_buffer_size(sock.into()).unwrap();
        socket_state
            .set_recv_buffer_size(sock.into(), BUFFER_SIZE)
            .expect("set recv buffer size {buffer_before} -> {BUFFER_SIZE}");
        let buffer_after = socket_state.recv_buffer_size(sock.into()).unwrap();
        assert_eq!(
            buffer_after,
            BUFFER_SIZE,
            "setting recv buffer size to {BUFFER_SIZE} resulted in {buffer_before} -> {buffer_after}",
        );
    }

    test_send_recv(
        &send,
        &recv,
        Transmit {
            destination: recv.local_addr().unwrap().as_socket().unwrap(),
            ecn: None,
            contents: b"hello",
            segment_size: None,
            src_ip: None,
        },
    );
}

fn test_send_recv(send: &Socket, recv: &Socket, transmit: Transmit<'_>) {
    let send_state = UdpSocketState::new(send.into()).unwrap();
    let recv_state = UdpSocketState::new(recv.into()).unwrap();

    // Reverse non-blocking flag set by `UdpSocketState` to make the test non-racy
    recv.set_nonblocking(false).unwrap();

    if let Err(err) = send_state.try_send(send.into(), &transmit) {
        panic!("send failed: {err:?}");
    }

    let mut buf = [0; u16::MAX as usize];
    let mut meta = RecvMeta::default();
    let segment_size = transmit.segment_size.unwrap_or(transmit.contents.len());
    let expected_datagrams = transmit.contents.len() / segment_size;
    let mut datagrams = 0;
    while datagrams < expected_datagrams {
        let n = recv_state
            .recv(
                recv.into(),
                &mut [IoSliceMut::new(&mut buf)],
                slice::from_mut(&mut meta),
            )
            .unwrap();
        assert_eq!(n, 1);
        let segments = meta.len / meta.stride;
        for i in 0..segments {
            assert_eq!(
                &buf[(i * meta.stride)..((i + 1) * meta.stride)],
                &transmit.contents
                    [(datagrams + i) * segment_size..(datagrams + i + 1) * segment_size]
            );
        }
        datagrams += segments;

        assert_eq!(
            meta.addr.port(),
            send.local_addr().unwrap().as_socket().unwrap().port()
        );
        let send_v6 = send.local_addr().unwrap().as_socket().unwrap().is_ipv6();
        let recv_v6 = recv.local_addr().unwrap().as_socket().unwrap().is_ipv6();
        let mut addresses = vec![meta.addr.ip()];
        if let Some(addr) = meta.dst_ip {
            addresses.push(addr);
        }
        for addr in addresses {
            match (send_v6, recv_v6) {
                (_, false) => assert_eq!(addr, Ipv4Addr::LOCALHOST),
                // IPv6-mapped IPv4 addrs; canonicalize to IPv6-mapped for robustness.
                (false, true) => {
                    assert_eq!(ip_to_v6_mapped(addr), Ipv4Addr::LOCALHOST.to_ipv6_mapped())
                }
                (true, true) => assert!(
                    addr == Ipv6Addr::LOCALHOST || addr == Ipv4Addr::LOCALHOST.to_ipv6_mapped()
                ),
            }
        }

        assert_eq!(meta.ecn, transmit.ecn);
    }
    assert_eq!(datagrams, expected_datagrams);
}

fn ip_to_v6_mapped(x: IpAddr) -> IpAddr {
    match x {
        IpAddr::V4(x) => IpAddr::V6(x.to_ipv6_mapped()),
        IpAddr::V6(_) => x,
    }
}

/// Test Apple fast datapath enable/disable functionality.
///
/// This test verifies that:
/// 1. `max_gso_segments()` returns 1 by default (fast path disabled)
/// 2. After calling `set_apple_fast_path()`, `max_gso_segments()` returns `BATCH_SIZE`
/// 3. Send/recv still works correctly with the fast path enabled
#[test]
#[cfg(feature = "fast-apple-datapath")]
fn apple_fast_datapath() {
    let send = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let recv = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dst_addr = recv.local_addr().unwrap();

    let send_state = UdpSocketState::new((&send).into()).unwrap();
    let recv_state = UdpSocketState::new((&recv).into()).unwrap();

    // Initially, fast path should be disabled and max_gso_segments should be 1
    assert!(
        !send_state.is_apple_fast_path_enabled(),
        "fast path should be disabled initially"
    );
    assert_eq!(
        send_state.max_gso_segments().get(),
        1,
        "max_gso_segments should be 1 before enabling fast path"
    );

    send_state.set_apple_fast_path();
    recv_state.set_apple_fast_path();

    // After enabling, fast path should be enabled and max_gso_segments should be BATCH_SIZE
    assert!(
        send_state.is_apple_fast_path_enabled(),
        "fast path should be enabled after calling set_apple_fast_path()"
    );
    assert_eq!(
        send_state.max_gso_segments().get(),
        noq_udp::BATCH_SIZE,
        "max_gso_segments should be BATCH_SIZE after enabling fast path"
    );

    // Verify send/recv still works with fast path enabled
    recv.set_nonblocking(false).unwrap();

    const SEGMENT_SIZE: usize = 128;
    let segments = send_state.max_gso_segments().get();
    let msg = vec![0xAB; SEGMENT_SIZE * segments];

    send_state
        .try_send(
            (&send).into(),
            &Transmit {
                destination: dst_addr,
                ecn: None,
                contents: &msg,
                segment_size: Some(SEGMENT_SIZE),
                src_ip: None,
            },
        )
        .unwrap();

    // Receive all segments
    let mut buf = [0u8; u16::MAX as usize];
    let mut total_received = 0;
    while total_received < segments {
        let mut meta = RecvMeta::default();
        let n = recv_state
            .recv(
                (&recv).into(),
                &mut [IoSliceMut::new(&mut buf)],
                slice::from_mut(&mut meta),
            )
            .unwrap();
        assert_eq!(n, 1);
        let received_segments = meta.len / meta.stride;
        for i in 0..received_segments {
            assert_eq!(
                &buf[i * meta.stride..(i + 1) * meta.stride],
                &msg[(total_received + i) * SEGMENT_SIZE..(total_received + i + 1) * SEGMENT_SIZE],
                "segment {} content mismatch",
                total_received + i
            );
        }
        total_received += received_segments;
    }
    assert_eq!(total_received, segments, "should receive all segments");
}

#[test]
fn sndbuf_cmsg_boundary() {
    let send = Socket::from(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap());
    let recv = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let dst = recv.local_addr().unwrap();
    send.set_nonblocking(true).unwrap();

    let _ = send.set_send_buffer_size(9216);
    let sndbuf = send.send_buffer_size().unwrap();
    let cmsg_len = tclass_cmsg_space();

    let payload = vec![0u8; sndbuf];
    for attempt in 1..=3 {
        match raw_send_with_tclass_cmsg(&send, dst, &payload) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => wait_writable(&send),
            other => panic!(
                "expected macOS SO_SNDBUF/cmsg kernel bug to reproduce on attempt {attempt}, got {other:?}"
            ),
        }
    }

    let state = UdpSocketState::new((&send).into()).unwrap();
    assert_min_sndbuf_enforced(&state, &send, dst, cmsg_len);
}

/// Asserts `UdpSocketState`'s `SO_SNDBUF` floor holds on `sendmsg_x`.
#[test]
#[cfg(feature = "fast-apple-datapath")]
fn sndbuf_cmsg_boundary_fast_path() {
    let send = Socket::from(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap());
    let recv = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let dst = recv.local_addr().unwrap();
    send.set_nonblocking(true).unwrap();

    let state = UdpSocketState::new((&send).into()).unwrap();
    state.set_apple_fast_path();
    assert_min_sndbuf_enforced(&state, &send, dst, tclass_cmsg_space());
}

/// Asserts `UdpSocketState`'s `SO_SNDBUF` floor holds on `send`.
fn assert_min_sndbuf_enforced(
    state: &UdpSocketState,
    send: &Socket,
    dst: SocketAddr,
    cmsg_len: usize,
) {
    let _ = state.set_send_buffer_size(send.into(), 1);
    let sndbuf = state.send_buffer_size(send.into()).unwrap();
    assert!(sndbuf >= 65535 + cmsg_len);

    let transmit = Transmit {
        destination: dst,
        ecn: Some(EcnCodepoint::Ect0),
        contents: &vec![0u8; 60_000],
        segment_size: None,
        src_ip: None,
    };
    match state.try_send(send.into(), &transmit) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EMSGSIZE) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            panic!("regressed: permanently stuck EWOULDBLOCK sending a large datagram")
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}

/// Sends `payload` with an `IPV6_TCLASS` cmsg attached via `Socket::sendmsg`.
fn raw_send_with_tclass_cmsg(sock: &Socket, dst: SocketAddr, payload: &[u8]) -> io::Result<()> {
    let addr = socket2::SockAddr::from(dst);
    let iov = [IoSlice::new(payload)];

    let mut cbuf = AlignedCmsgBuf([0u8; 32]); // room for one aligned c_int cmsg
    let mut scratch: libc::msghdr = unsafe { std::mem::zeroed() };
    scratch.msg_control = cbuf.0.as_mut_ptr() as *mut _;
    scratch.msg_controllen = tclass_cmsg_space() as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&scratch);
        (*cmsg).cmsg_level = libc::IPPROTO_IPV6;
        (*cmsg).cmsg_type = libc::IPV6_TCLASS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<libc::c_int>() as _) as _;
        std::ptr::write(
            libc::CMSG_DATA(cmsg) as *mut libc::c_int,
            EcnCodepoint::Ect0 as libc::c_int,
        );
    }

    let msg = MsgHdr::new()
        .with_addr(&addr)
        .with_buffers(&iov)
        .with_control(&cbuf.0[..tclass_cmsg_space()]);
    sock.sendmsg(&msg, 0).map(|_| ())
}

/// Blocks (via `poll()`) until `sock` reports `POLLOUT`, or panics after 2s.
fn wait_writable(sock: &Socket) {
    let mut pfd = libc::pollfd {
        fd: sock.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut pfd, 1, 2000) };
    assert!(n != 0, "poll() itself timed out waiting for POLLOUT");
}

fn tclass_cmsg_space() -> usize {
    unsafe { libc::CMSG_SPACE(size_of::<libc::c_int>() as _) as usize }
}

#[repr(align(8))]
struct AlignedCmsgBuf([u8; 32]);

/// A transmit longer than one `sendmsg_x` batch goes out a batch at a time: each call reports
/// how many datagrams it sent, and [`Transmit::advance`] leaves the rest, so nothing is dropped
/// that the socket did not refuse (quinn-rs/quinn#2748).
#[test]
#[cfg(feature = "fast-apple-datapath")]
fn apple_fast_datapath_reports_a_partial_send() {
    let send = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let recv = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let send_state = UdpSocketState::new((&send).into()).unwrap();
    send_state.set_apple_fast_path();
    const SEGMENT_SIZE: usize = 128;
    let segments = noq_udp::BATCH_SIZE + 5;
    let msg: Vec<u8> = (0..segments)
        .flat_map(|i| [i as u8; SEGMENT_SIZE])
        .collect();
    let transmit = Transmit {
        destination: recv.local_addr().unwrap(),
        ecn: None,
        contents: &msg,
        segment_size: Some(SEGMENT_SIZE),
        src_ip: None,
    };
    assert_eq!(transmit.datagram_count(), segments);
    let first = send_state
        .try_send_partial((&send).into(), &transmit)
        .unwrap();
    assert_eq!(first, noq_udp::BATCH_SIZE);
    let rest = transmit.advance(first);
    assert_eq!(rest.datagram_count(), 5);
    assert_eq!(
        send_state.try_send_partial((&send).into(), &rest).unwrap(),
        5
    );
    recv.set_nonblocking(false).unwrap();
    let mut buf = [0u8; SEGMENT_SIZE];
    for i in 0..segments {
        assert_eq!(recv.recv(&mut buf).unwrap(), SEGMENT_SIZE);
        assert_eq!(buf, [i as u8; SEGMENT_SIZE], "datagram {i} out of order");
    }
}

/// Without the fast path every call sends the whole transmit.
#[test]
fn a_whole_send_reports_every_datagram() {
    let send = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let recv = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let state = UdpSocketState::new((&send).into()).unwrap();
    let transmit = Transmit {
        destination: recv.local_addr().unwrap(),
        ecn: None,
        contents: &[7; 300],
        segment_size: None,
        src_ip: None,
    };
    assert_eq!(
        state.try_send_partial((&send).into(), &transmit).unwrap(),
        1
    );
    let mut buf = [0u8; 400];
    recv.set_nonblocking(false).unwrap();
    assert_eq!(recv.recv(&mut buf).unwrap(), 300);
}

/// macOS answers a non-blocking `sendmsg` or `sendmsg_x` whose payload fits `SO_SNDBUF` but
/// whose payload and `cmsg` together do not with `EWOULDBLOCK`, for good (quinn-rs/quinn#2727).
/// The socket state keeps `SO_SNDBUF` above any datagram and turns every size that would still
/// land there into `EMSGSIZE`. Every size across the edge of the effective buffer is sent or
/// refused, never left blocking, on both paths.
#[test]
fn no_datagram_size_blocks_for_good_at_the_send_buffer_edge() {
    for fast in [false, true] {
        let send = Socket::from(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap());
        let recv = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let state = UdpSocketState::new((&send).into()).unwrap();
        #[cfg(feature = "fast-apple-datapath")]
        if fast {
            state.set_apple_fast_path();
        }
        #[cfg(not(feature = "fast-apple-datapath"))]
        if fast {
            continue;
        }
        let _ = state.set_send_buffer_size((&send).into(), 1);
        let sndbuf = state.send_buffer_size((&send).into()).unwrap();
        assert!(sndbuf > 65535, "SO_SNDBUF {sndbuf} under the floor");
        let payload = vec![0u8; sndbuf + 1];
        let mut refused = 0;
        // A `cmsg` is at most a few dozen bytes, so the bug's region lies inside this sweep.
        for len in sndbuf - 64..=sndbuf + 1 {
            let transmit = Transmit {
                destination: recv.local_addr().unwrap(),
                ecn: Some(EcnCodepoint::Ect0),
                contents: &payload[..len],
                segment_size: None,
                src_ip: None,
            };
            match state.try_send((&send).into(), &transmit) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(libc::EMSGSIZE) => refused += 1,
                Err(e) => {
                    panic!("fast path {fast}: a {len}-byte datagram, SO_SNDBUF {sndbuf}: {e}")
                }
            }
        }
        assert!(
            refused > 0,
            "fast path {fast}: nothing past SO_SNDBUF {sndbuf} was refused"
        );
    }
}
