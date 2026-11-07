//! The UDP socket under every endpoint: noq-udp's socket state driven by tokio, as noq's own
//! `TokioRuntime` wraps one, plus two things that one cannot do.
//!
//! Ported from slopty's `slopty-net` `udp.rs` (MIT).
//!
//! * A send that goes out in part is finished, never dropped. Apple's batched `sendmsg_x` can take
//!   fewer datagrams than it is given, and noq's wrapper counted a batch sent once any of it was
//!   (quinn-rs/quinn#2748). Here the rest goes on the next try, even across a wait for the socket
//!   to have room.
//! * On macOS the socket takes the batched path ([`crate::transport::BATCHED_UDP_ENV`]), which
//!   noq's runtime never does. iOS never builds it: the calls are private API.

use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, ready};

use noq::udp::{RecvMeta, Transmit, UdpSocketState};
use noq::{AsyncUdpSocket, UdpSender};
use tokio::io::Interest;

/// `socket` as an endpoint's socket, on the batched path when `batched` asks for it on macOS.
///
/// # Errors
///
/// What configuring the socket or registering it with tokio returns.
pub(crate) fn wrap(
    socket: std::net::UdpSocket,
    batched: bool,
) -> io::Result<Box<dyn AsyncUdpSocket>> {
    Ok(Box::new(Socket(shared(socket, batched)?)))
}

fn shared(socket: std::net::UdpSocket, batched: bool) -> io::Result<Arc<Shared>> {
    let state = UdpSocketState::new((&socket).into())?;
    let batched = batched && enable_batching(&state);
    tracing::debug!(
        batched,
        receive_buffer = state.recv_buffer_size((&socket).into()).ok(),
        send_buffer = state.send_buffer_size((&socket).into()).ok(),
        "UDP socket"
    );
    let io = tokio::net::UdpSocket::from_std(socket)?;
    Ok(Arc::new(Shared {
        io,
        state,
        warned_oversize: AtomicBool::new(false),
    }))
}

#[cfg(target_os = "macos")]
fn enable_batching(state: &UdpSocketState) -> bool {
    state.set_apple_fast_path();
    true
}

#[cfg(target_os = "ios")]
const fn enable_batching(_state: &UdpSocketState) -> bool {
    false
}

struct Shared {
    io: tokio::net::UdpSocket,
    state: UdpSocketState,
    /// Whether this socket has said that a datagram was too large for the local interface.
    warned_oversize: AtomicBool,
}

impl Shared {
    /// A send the OS refused. `EMSGSIZE` means the interface under the socket (a VPN's tunnel,
    /// say) is narrower than the packets, which black-hole detection cannot see, since the
    /// datagram never left: said once per socket, where someone reading the log looks.
    fn refused(&self, e: &io::Error, transmit: &Transmit<'_>) {
        let size = transmit.segment_size.unwrap_or(transmit.contents.len());
        if e.raw_os_error() == Some(libc::EMSGSIZE)
            && !self.warned_oversize.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(%e, size, to = %transmit.destination, "UDP datagram too large for the interface");
        } else {
            tracing::trace!(%e, size, to = %transmit.destination, "UDP send failed");
        }
    }
}

/// The endpoint's socket.
struct Socket(Arc<Shared>);

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socket")
            .field("local", &self.0.io.local_addr())
            .finish_non_exhaustive()
    }
}

impl AsyncUdpSocket for Socket {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        Box::pin(Sender {
            shared: Arc::clone(&self.0),
            writable: None,
            partial: None,
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let shared = &*self.0;
        loop {
            ready!(shared.io.poll_recv_ready(cx))?;
            if let Ok(received) = shared.io.try_io(Interest::READABLE, || {
                shared.state.recv((&shared.io).into(), bufs, meta)
            }) {
                return Poll::Ready(Ok(received));
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.io.local_addr()
    }

    fn max_receive_segments(&self) -> NonZeroUsize {
        self.0.state.gro_segments()
    }

    fn may_fragment(&self) -> bool {
        self.0.state.may_fragment()
    }
}

/// A transmit that went out in part before the socket ran out of room: which one, and how many
/// of its datagrams went.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Partial {
    contents: usize,
    len: usize,
    destination: SocketAddr,
    sent: usize,
}

impl Partial {
    /// The datagrams of `transmit` already sent, when it is the one this records.
    fn sent_of(self, transmit: &Transmit<'_>) -> Option<usize> {
        let same = self.contents == transmit.contents.as_ptr().addr()
            && self.len == transmit.contents.len()
            && self.destination == transmit.destination;
        same.then_some(self.sent)
    }
}

type Writable = Pin<Box<dyn Future<Output = io::Result<()>> + Send + Sync>>;

/// One task's sender: its own wait for room, so every sender is woken, and the transmit it left
/// part-sent. noq retries a transmit that returned `Pending` with the same buffer.
struct Sender {
    shared: Arc<Shared>,
    writable: Option<Writable>,
    partial: Option<Partial>,
}

impl fmt::Debug for Sender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl UdpSender for Sender {
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let count = transmit.datagram_count();
        let mut sent = this
            .partial
            .take()
            .and_then(|p| p.sent_of(transmit))
            .unwrap_or(0);
        loop {
            let writable = this.writable.get_or_insert_with(|| {
                let shared = Arc::clone(&this.shared);
                Box::pin(async move { shared.io.writable().await })
            });
            let ready = match writable.as_mut().poll(cx) {
                Poll::Ready(ready) => ready,
                Poll::Pending => {
                    this.partial = (sent > 0).then(|| Partial {
                        contents: transmit.contents.as_ptr().addr(),
                        len: transmit.contents.len(),
                        destination: transmit.destination,
                        sent,
                    });
                    return Poll::Pending;
                }
            };
            this.writable = None;
            ready?;
            let shared = &*this.shared;
            let rest = transmit.advance(sent);
            match shared.io.try_io(Interest::WRITABLE, || {
                shared.state.try_send_partial((&shared.io).into(), &rest)
            }) {
                Ok(went) => {
                    sent = sent.saturating_add(went);
                    if sent >= count {
                        return Poll::Ready(Ok(()));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                // Too large for the path (an MTU probe) or refused by the network: lost, as
                // UDP loses a datagram, and recovered by QUIC. An error here would end the
                // endpoint.
                Err(e) => {
                    shared.refused(&e, transmit);
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }

    fn max_transmit_segments(&self) -> NonZeroUsize {
        self.shared.state.max_gso_segments()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use super::*;

    fn transmit(to: SocketAddr, contents: &[u8], segment: usize) -> Transmit<'_> {
        Transmit {
            destination: to,
            ecn: None,
            contents,
            segment_size: Some(segment),
            src_ip: None,
        }
    }

    /// Every datagram of `socket`'s longest transmit, twice over and three more, arrives once
    /// and in order. On Apple's batched path it goes as one transmit, which the batch cuts;
    /// anywhere else as noq would send it, in transmits of at most that many.
    async fn a_long_transmit_arrives_whole(batched: bool) {
        let receiver = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let to = receiver.local_addr().unwrap();
        let socket = wrap(
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            batched,
        )
        .unwrap();
        let mut sender = socket.create_sender();
        let segments = sender.max_transmit_segments().get();
        // macOS sends several datagrams in a call only on the batched path.
        assert_eq!(
            segments > 1,
            batched,
            "a transmit carries {segments} datagrams"
        );
        let datagrams = segments.saturating_mul(2).saturating_add(3);
        let contents: Vec<u8> = (0..datagrams)
            .flat_map(|i| [u8::try_from(i).unwrap(); 100])
            .collect();
        let chunk = if batched {
            contents.len()
        } else {
            segments.saturating_mul(100)
        };
        for part in contents.chunks(chunk) {
            let transmit = transmit(to, part, 100);
            std::future::poll_fn(|cx| sender.as_mut().poll_send(&transmit, cx))
                .await
                .unwrap();
        }
        let mut buf = [0_u8; 200];
        for i in 0..datagrams {
            let n = tokio::time::timeout(Duration::from_secs(1), receiver.recv(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("datagram {i} of {datagrams} never came"))
                .unwrap();
            assert_eq!(buf[..n], [u8::try_from(i).unwrap(); 100], "datagram {i}");
        }
    }

    #[tokio::test]
    async fn datagrams_arrive_whole_on_the_plain_path() {
        a_long_transmit_arrives_whole(false).await;
    }

    /// The batched path takes at most a batch a call, and a transmit longer than that goes on
    /// in the next.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_long_transmit_arrives_whole_on_the_batched_path() {
        a_long_transmit_arrives_whole(true).await;
    }

    /// How often a send over loopback went out in part or waited for room: 100 000 transmits of
    /// ten 1200-byte datagrams (noq's largest) to a socket nobody reads. Loopback hands each
    /// datagram to the receiver at once and drops it there when its buffer is full, so the send
    /// buffer never fills (slopty's MEASUREMENTS.md, "Partial sends").
    #[tokio::test]
    #[ignore = "diagnostic: cargo test -p onicode-net --lib loopback_short_sends -- --ignored --nocapture"]
    async fn loopback_short_sends() {
        let receiver = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let to = receiver.local_addr().unwrap();
        let shared = shared(
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            true,
        )
        .unwrap();
        let mut sender = Sender {
            shared,
            writable: None,
            partial: None,
        };
        let contents = vec![1_u8; 1200 * 10];
        let transmit = transmit(to, &contents, 1200);
        let (mut waited, mut cut) = (Vec::new(), 0);
        for n in 0..100_000 {
            std::future::poll_fn(|cx| {
                let sent = Pin::new(&mut sender).poll_send(&transmit, cx);
                if sent.is_pending() {
                    waited.push(n);
                    cut += usize::from(sender.partial.is_some());
                }
                sent
            })
            .await
            .unwrap();
        }
        // Only the first waits, while tokio learns the socket is writable.
        println!("transmits that waited for room: {waited:?}; of them cut short: {cut}");
    }

    /// A datagram larger than the interface carries is dropped as lost, the socket goes on, and
    /// the first such refusal is flagged for the warning.
    #[tokio::test]
    async fn a_datagram_too_large_for_the_interface_is_lost_not_fatal() {
        let receiver = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let to = receiver.local_addr().unwrap();
        let shared = shared(
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            true,
        )
        .unwrap();
        let mut sender = Sender {
            shared: Arc::clone(&shared),
            writable: None,
            partial: None,
        };
        let huge = vec![0_u8; 70_000];
        for _ in 0..2 {
            let transmit = transmit(to, &huge, huge.len());
            std::future::poll_fn(|cx| Pin::new(&mut sender).poll_send(&transmit, cx))
                .await
                .unwrap();
        }
        assert!(shared.warned_oversize.load(Ordering::Relaxed));
        let small = transmit(to, &[9; 100], 100);
        std::future::poll_fn(|cx| Pin::new(&mut sender).poll_send(&small, cx))
            .await
            .unwrap();
        let mut buf = [0_u8; 200];
        let n = tokio::time::timeout(Duration::from_secs(1), receiver.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(buf[..n], [9; 100]);
    }

    /// A transmit that went out in part is picked up where it stopped only when it comes back.
    #[test]
    fn a_partial_send_resumes_only_its_own_transmit() {
        let to = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        let contents = [0_u8; 300];
        let first = transmit(to, &contents, 100);
        let partial = Partial {
            contents: contents.as_ptr().addr(),
            len: 300,
            destination: to,
            sent: 2,
        };
        assert_eq!(partial.sent_of(&first), Some(2));
        assert_eq!(partial.sent_of(&transmit(to, &contents[..200], 100)), None);
        let elsewhere = SocketAddr::from((Ipv4Addr::LOCALHOST, 10));
        assert_eq!(partial.sent_of(&transmit(elsewhere, &contents, 100)), None);
        let other = [0_u8; 300];
        assert_eq!(partial.sent_of(&transmit(to, &other, 100)), None);
    }
}
