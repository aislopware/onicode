use std::{
    cmp::min,
    io::{ErrorKind, IoSliceMut},
    net::{Ipv4Addr, Ipv6Addr, UdpSocket},
};

use criterion::{Criterion, criterion_group, criterion_main};
use tokio::{io::Interest, runtime::Runtime};

use noq_udp::{BATCH_SIZE, RecvMeta, Transmit, UdpSocketState};

pub fn criterion_benchmark(c: &mut Criterion) {
    const TOTAL_BYTES: usize = 10 * 1024 * 1024;
    const SEGMENT_SIZE: usize = 1280;

    let rt = Runtime::new().unwrap();
    let _guard = rt.enter();

    // Without the batched path a send carries one datagram and a receive takes one; with it
    // (`fast-apple-datapath`) both go `BATCH_SIZE` at a time through `sendmsg_x`/`recvmsg_x`.
    for batched in [
        false,
        #[cfg(feature = "fast-apple-datapath")]
        true,
    ] {
        let (send_state, send_socket) = new_socket(batched);
        let (recv_state, recv_socket) = new_socket(batched);
        let dst_addr = recv_socket.local_addr().unwrap();

        let mut group = c.benchmark_group(format!("batched_{batched}"));
        group.throughput(criterion::Throughput::Bytes(TOTAL_BYTES as u64));

        let segments = send_state.max_gso_segments();
        let msg = vec![0xAB; min(MAX_DATAGRAM_SIZE, SEGMENT_SIZE * segments.get())];
        let transmit = Transmit {
            destination: dst_addr,
            ecn: None,
            contents: &msg,
            segment_size: Some(SEGMENT_SIZE),
            src_ip: None,
        };
        let batch_size = if batched { BATCH_SIZE } else { 1 };

        group.bench_function("throughput", |b| {
            b.to_async(&rt).iter(|| async {
                let mut receive_buffers = vec![vec![0; SEGMENT_SIZE]; batch_size];
                let mut receive_slices = receive_buffers
                    .iter_mut()
                    .map(|buf| IoSliceMut::new(buf))
                    .collect::<Vec<_>>();
                let mut meta = vec![RecvMeta::default(); batch_size];

                let mut sent: usize = 0;
                let mut received: usize = 0;
                while sent < TOTAL_BYTES {
                    send_socket.writable().await.unwrap();
                    send_socket
                        .try_io(Interest::WRITABLE, || {
                            send_state.try_send((&send_socket).into(), &transmit)
                        })
                        .unwrap();
                    sent += transmit.contents.len();

                    while received < sent {
                        recv_socket.readable().await.unwrap();
                        let n = match recv_socket.try_io(Interest::READABLE, || {
                            recv_state.recv((&recv_socket).into(), &mut receive_slices, &mut meta)
                        }) {
                            Ok(n) => n,
                            // recv.readable() can lead to false positives. Try again.
                            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
                            e => e.unwrap(),
                        };
                        received += meta.iter().map(|m| m.len).take(n).sum::<usize>();
                    }
                }
            })
        });
    }
}

fn new_socket(batched: bool) -> (UdpSocketState, tokio::net::UdpSocket) {
    let socket = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)))
        .unwrap();
    let state = UdpSocketState::new((&socket).into()).unwrap();
    #[cfg(feature = "fast-apple-datapath")]
    if batched {
        state.set_apple_fast_path();
    }
    #[cfg(not(feature = "fast-apple-datapath"))]
    let _ = batched;

    (state, tokio::net::UdpSocket::from_std(socket).unwrap())
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

const MAX_IP_UDP_HEADER_SIZE: usize = 48;
const MAX_DATAGRAM_SIZE: usize = u16::MAX as usize - MAX_IP_UDP_HEADER_SIZE;
