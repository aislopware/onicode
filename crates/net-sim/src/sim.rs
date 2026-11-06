//! An in-memory network for noq endpoints, on tokio's clock.
//!
//! Ported from axolotty's `axquic` `tests/shape/sim.rs`, itself slopty's (MIT). onicode adds a
//! link per pair of addresses (a path that moves to DERP), hosts taken off the network (a Mac
//! asleep), and per-pair delivery counts.
//!
//! [`Net`] carries datagrams between the [`Socket`]s bound on it, each direction through its
//! own [`Shaper`] drawn from the run's seed, so it has every fault the relay has (delay, jitter,
//! loss, rate, queue) plus three a relay between two real sockets cannot give:
//!
//! * blackouts, where nothing crosses either way and whatever was in flight is lost;
//! * NAT rebinding, where the address a socket's peers see it at changes mid-flow;
//! * reordering, off unless [`Faults::reorder`] asks for it;
//! * a pair of hosts with a link of its own ([`Net::set_path`]), and a host off the network
//!   ([`Net::set_down`]).
//!
//! A send takes a batch of datagrams, as the real sockets' batched sends and GSO do, so noq
//! builds its packets as it does on them.
//!
//! Nothing here reads a real clock or does I/O. Under `#[tokio::test(start_paused = true)]`
//! tokio's clock jumps to the next packet or timer whenever every task waits, and noq's
//! `TokioRuntime` reads that same clock, so a whole QUIC connection runs in virtual time: a
//! minute's blackout passes in milliseconds, and a seed repeats its run. Like tokio's timers,
//! a delivery lands on a whole millisecond.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use noq::udp::{RecvMeta, Transmit};
use noq::{AsyncUdpSocket, UdpSender};
use parking_lot::Mutex;
use tokio::time::{Instant, Sleep};

use crate::shape::{Fate, Link, Rng, Shaper, Tally};

/// What the network does to every packet, each direction apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Faults {
    /// The relay's link model, applied to each direction between two sockets.
    pub link: Link,
    /// Share of the packets the link carries that are held back by [`Self::reorder_by`] and
    /// arrive after packets sent behind them, 0.0 to 1.0.
    pub reorder: f32,
    /// How long a reordered packet is held past its time.
    pub reorder_by: Duration,
}

impl Faults {
    /// A network that carries everything, at once, in order.
    pub const CLEAR: Self = Self {
        link: Link::CLEAR,
        reorder: 0.0,
        reorder_by: Duration::ZERO,
    };

    /// `link` each way, in order.
    #[must_use]
    pub const fn over(link: Link) -> Self {
        Self {
            link,
            reorder: 0.0,
            reorder_by: Duration::ZERO,
        }
    }
}

/// What the network has done with the packets sent on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// The link model's decisions, summed over every direction.
    pub shaped: Tally,
    /// Packets sent or due during a blackout.
    pub blacked_out: u64,
    /// Packets held back out of their turn.
    pub reordered: u64,
    /// Packets sent to an address nothing is bound at, or due at a NAT mapping that is gone.
    pub unroutable: u64,
    /// Packets a socket received.
    pub delivered: u64,
    /// Packets dropped because their sender or receiver was off the network.
    pub down: u64,
    /// The largest datagram sent, in bytes.
    pub largest: u64,
}

/// An in-memory network. Cloning it shares it.
#[derive(Clone)]
pub struct Net {
    fabric: Arc<Mutex<Fabric>>,
}

impl fmt::Debug for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fabric = self.fabric.lock();
        f.debug_struct("Net")
            .field("faults", &fabric.faults)
            .field("seed", &fabric.seed)
            .field("counts", &fabric.counts())
            .finish_non_exhaustive()
    }
}

impl Net {
    /// A network with `faults` on every direction, drawing from `seed`. Made on the runtime
    /// that drives it, since its epoch is that runtime's now.
    #[must_use]
    pub fn new(faults: Faults, seed: u64) -> Self {
        let fabric = Fabric {
            epoch: Instant::now(),
            faults,
            seed,
            ends: Vec::new(),
            routes: BTreeMap::new(),
            directions: BTreeMap::new(),
            blackouts: Vec::new(),
            paths: BTreeMap::new(),
            down: BTreeSet::new(),
            pairs: BTreeMap::new(),
            counts: Counts::default(),
            digest: FNV_OFFSET,
            sent: 0,
        };
        Self {
            fabric: Arc::new(Mutex::new(fabric)),
        }
    }

    /// A socket at `addr`, reached there until [`Self::rebind`] moves it.
    ///
    /// # Errors
    ///
    /// `AddrInUse` if a socket already answers at `addr`.
    pub fn bind(&self, addr: SocketAddr) -> io::Result<Socket> {
        let mut fabric = self.fabric.lock();
        let addr = canonical(addr);
        if fabric.routes.contains_key(&addr) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{addr} is bound"),
            ));
        }
        let end = fabric.ends.len();
        fabric.ends.push(End {
            local: addr,
            public: addr,
            inbox: BinaryHeap::new(),
            waker: None,
        });
        fabric.routes.insert(addr, end);
        Ok(Socket {
            net: self.clone(),
            end,
            local: addr,
            epoch: fabric.epoch,
            timer: None,
        })
    }

    /// NAT rebinding: the socket bound at `local` is seen by its peers at `public` from now on.
    /// The old mapping is gone, so what is still addressed to it is lost, and the socket
    /// itself notices nothing.
    ///
    /// # Errors
    ///
    /// `NotFound` if nothing is bound at `local`; `AddrInUse` if a socket answers at `public`.
    pub fn rebind(&self, local: SocketAddr, public: SocketAddr) -> io::Result<()> {
        let (local, public) = (canonical(local), canonical(public));
        let mut fabric = self.fabric.lock();
        if fabric.routes.contains_key(&public) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{public} is bound"),
            ));
        }
        let Some(index) = fabric.ends.iter().position(|end| end.local == local) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("nothing at {local}"),
            ));
        };
        let old = fabric
            .ends
            .get_mut(index)
            .map(|end| std::mem::replace(&mut end.public, public));
        if let Some(old) = old {
            fabric.routes.remove(&old);
        }
        fabric.routes.insert(public, index);
        drop(fabric);
        Ok(())
    }

    /// Nothing crosses in either direction for `length` from now, and a packet due in that
    /// time is lost with the rest.
    pub fn blackout(&self, length: Duration) {
        let mut fabric = self.fabric.lock();
        let start = fabric.elapsed();
        fabric.blackouts.push(start..start.saturating_add(length));
    }

    /// Packets between the hosts at `a` and `b` (each way) run over `faults` from now on, in
    /// place of the network's own: a path that moved from direct to DERP, or back.
    pub fn set_path(&self, a: IpAddr, b: IpAddr, faults: Faults) {
        let (a, b) = (a.to_canonical(), b.to_canonical());
        let mut fabric = self.fabric.lock();
        fabric.paths.insert((a, b), faults);
        fabric.paths.insert((b, a), faults);
    }

    /// The host at `ip` is off the network (`true`) or back on it: nothing it sends leaves and
    /// nothing reaches it, as for a machine asleep or off the tailnet.
    pub fn set_down(&self, ip: IpAddr, down: bool) {
        let ip = ip.to_canonical();
        let mut fabric = self.fabric.lock();
        if down {
            fabric.down.insert(ip);
        } else {
            fabric.down.remove(&ip);
        }
    }

    /// Packets the host at `from` delivered to the host at `to` so far.
    #[must_use]
    pub fn delivered(&self, from: IpAddr, to: IpAddr) -> u64 {
        let fabric = self.fabric.lock();
        fabric
            .pairs
            .get(&(from.to_canonical(), to.to_canonical()))
            .copied()
            .unwrap_or(0)
    }

    /// How long the network has run, on tokio's clock.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.fabric.lock().elapsed()
    }

    /// What the network has done so far.
    #[must_use]
    pub fn counts(&self) -> Counts {
        self.fabric.lock().counts()
    }

    /// A hash of every delivery so far: when, from which port, to which port and how many
    /// bytes. Two runs that delivered the same packets at the same times have the same digest;
    /// the bytes themselves are left out, since connection IDs and nonces are random.
    #[must_use]
    pub fn digest(&self) -> u64 {
        self.fabric.lock().digest
    }
}

/// The network's state, behind the one lock every socket and sender shares.
#[derive(Debug)]
struct Fabric {
    /// Time zero of the run: every packet's time is measured from here.
    epoch: Instant,
    faults: Faults,
    seed: u64,
    /// Every socket bound, by the order it was bound in.
    ends: Vec<End>,
    /// Which socket answers at each address, as its peers see it.
    routes: BTreeMap<SocketAddr, usize>,
    /// Each direction's link, by sending and receiving socket, made at its first packet.
    directions: BTreeMap<(usize, usize), Direction>,
    blackouts: Vec<Range<Duration>>,
    /// Pairs of hosts with a link of their own, each direction by sending and receiving host.
    paths: BTreeMap<(IpAddr, IpAddr), Faults>,
    /// Hosts off the network.
    down: BTreeSet<IpAddr>,
    /// Deliveries by sending and receiving host.
    pairs: BTreeMap<(IpAddr, IpAddr), u64>,
    /// The counts not kept by the shapers.
    counts: Counts,
    digest: u64,
    /// Packets sent so far, which breaks ties between packets due at the same time.
    sent: u64,
}

/// One bound socket's side of the network.
#[derive(Debug)]
struct End {
    /// The address it was bound at, which is what it believes it is.
    local: SocketAddr,
    /// Where its peers reach it: `local` until a NAT rebinding.
    public: SocketAddr,
    /// Packets on their way to it, the first due on top.
    inbox: BinaryHeap<Reverse<Pending>>,
    /// Its receiver, waiting for a packet.
    waker: Option<Waker>,
}

/// One direction between two sockets.
#[derive(Debug)]
struct Direction {
    shaper: Shaper,
    /// Draws the reordering, apart from the shaper's draws so turning it on leaves the loss
    /// pattern as it was.
    rng: Rng,
    /// When the last packet that kept its turn arrives: none behind it may arrive sooner.
    in_order: Duration,
}

/// A packet on its way.
#[derive(Debug)]
struct Pending {
    /// When it arrives, into the run.
    at: Duration,
    seq: u64,
    from: SocketAddr,
    /// The address it was sent to, which must still reach its socket when it arrives.
    to: SocketAddr,
    data: Vec<u8>,
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Pending {}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.at, self.seq).cmp(&(other.at, other.seq))
    }
}

impl Fabric {
    fn elapsed(&self) -> Duration {
        Instant::now().saturating_duration_since(self.epoch)
    }

    fn dark(&self, at: Duration) -> bool {
        self.blackouts.iter().any(|window| window.contains(&at))
    }

    fn counts(&self) -> Counts {
        let shaped =
            self.directions
                .values()
                .map(|d| d.shaper.tally())
                .fold(Tally::new(), |sum, t| Tally {
                    sent: sum.sent.saturating_add(t.sent),
                    lost: sum.lost.saturating_add(t.lost),
                    overflowed: sum.overflowed.saturating_add(t.overflowed),
                    bytes: sum.bytes.saturating_add(t.bytes),
                });
        Counts {
            shaped,
            ..self.counts
        }
    }

    /// Put each datagram of `transmit`, sent by socket `from`, on its way.
    fn send(&mut self, from: usize, transmit: &Transmit<'_>) {
        let size = transmit
            .segment_size
            .unwrap_or(transmit.contents.len())
            .max(1);
        for datagram in transmit.contents.chunks(size) {
            self.send_one(from, canonical(transmit.destination), datagram);
        }
    }

    fn send_one(&mut self, from: usize, to_addr: SocketAddr, datagram: &[u8]) {
        let now = self.elapsed();
        let (Some(&to), Some(sender)) = (self.routes.get(&to_addr), self.ends.get(from)) else {
            self.counts.unroutable = self.counts.unroutable.saturating_add(1);
            return;
        };
        let from_addr = sender.public;
        let from_ip = sender.local.ip();
        let to_ip = self.ends.get(to).map_or(to_addr.ip(), |end| end.local.ip());
        self.counts.largest = self.counts.largest.max(datagram.len() as u64);
        if self.dark(now) {
            self.counts.blacked_out = self.counts.blacked_out.saturating_add(1);
            return;
        }
        if self.down.contains(&from_ip) || self.down.contains(&to_ip) {
            self.counts.down = self.counts.down.saturating_add(1);
            return;
        }
        let faults = self
            .paths
            .get(&(from_ip, to_ip))
            .copied()
            .unwrap_or(self.faults);
        let seed = self.seed;
        let direction = self
            .directions
            .entry((from, to))
            .or_insert_with(|| Direction::new(faults.link, direction_seed(seed, from, to)));
        if direction.shaper.link() != faults.link {
            direction.shaper.set_link(faults.link);
        }
        let fate = direction.shaper.admit(datagram.len() as u64, now);
        let Fate::At(due) = fate else { return };
        let at = if faults.reorder > 0.0 && direction.rng.unit() < faults.reorder {
            self.counts.reordered = self.counts.reordered.saturating_add(1);
            due.saturating_add(faults.reorder_by)
        } else {
            direction.in_order = direction.in_order.max(due);
            direction.in_order
        };
        let seq = self.sent;
        self.sent = self.sent.saturating_add(1);
        let Some(end) = self.ends.get_mut(to) else {
            return;
        };
        let pending = Pending {
            at,
            seq,
            from: from_addr,
            to: to_addr,
            data: datagram.to_vec(),
        };
        end.inbox.push(Reverse(pending));
        if let Some(waker) = end.waker.take() {
            waker.wake();
        }
    }

    /// Fill `bufs` with socket `end`'s packets that are due; how many it took.
    fn receive(&mut self, end: usize, bufs: &mut [IoSliceMut<'_>], meta: &mut [RecvMeta]) -> usize {
        let now = self.elapsed();
        let mut taken = 0_usize;
        for (buf, meta) in bufs.iter_mut().zip(meta.iter_mut()) {
            let Some(packet) = self.take_due(end, now) else {
                break;
            };
            let len = packet.data.len().min(buf.len());
            if let (Some(into), Some(from)) = (buf.get_mut(..len), packet.data.get(..len)) {
                into.copy_from_slice(from);
            }
            let local = self.ends.get(end).map(|e| e.local.ip());
            // `RecvMeta` is non-exhaustive: filled field by field over its default.
            *meta = RecvMeta::default();
            meta.addr = packet.from;
            meta.len = len;
            meta.stride = len;
            meta.dst_ip = local;
            self.record(end, &packet);
            taken = taken.saturating_add(1);
        }
        taken
    }

    /// The next packet due at `end` by `now` that survives the trip.
    fn take_due(&mut self, end: usize, now: Duration) -> Option<Pending> {
        loop {
            let inbox = &mut self.ends.get_mut(end)?.inbox;
            if inbox.peek().is_none_or(|Reverse(next)| next.at > now) {
                return None;
            }
            let Reverse(packet) = inbox.pop()?;
            let here = self.ends.get(end).map(|e| e.local.ip());
            if self.dark(packet.at) {
                self.counts.blacked_out = self.counts.blacked_out.saturating_add(1);
            } else if here.is_some_and(|ip| self.down.contains(&ip))
                || self.down.contains(&packet.from.ip())
            {
                self.counts.down = self.counts.down.saturating_add(1);
            } else if self.routes.get(&packet.to) != Some(&end) {
                self.counts.unroutable = self.counts.unroutable.saturating_add(1);
            } else {
                return Some(packet);
            }
        }
    }

    /// Count a delivery and fold it into the digest.
    fn record(&mut self, end: usize, packet: &Pending) {
        self.counts.delivered = self.counts.delivered.saturating_add(1);
        if let Some(here) = self.ends.get(end).map(|e| e.local.ip()) {
            let pair = self.pairs.entry((packet.from.ip(), here)).or_default();
            *pair = pair.saturating_add(1);
        }
        let at = u64::try_from(packet.at.as_nanos()).unwrap_or(u64::MAX);
        let len = packet.data.len() as u64;
        for word in [
            at,
            u64::from(packet.from.port()),
            u64::from(packet.to.port()),
            len,
        ] {
            self.digest = fnv(self.digest, &word.to_le_bytes());
        }
    }

    /// Wait for socket `end`'s next packet: its waker is kept for a sender to wake, and the
    /// time the first packet already on its way is due is returned.
    fn park(&mut self, end: usize, waker: &Waker) -> Option<Duration> {
        let end = self.ends.get_mut(end)?;
        end.waker = Some(waker.clone());
        end.inbox.peek().map(|Reverse(next)| next.at)
    }
}

impl Direction {
    const fn new(link: Link, seed: u64) -> Self {
        Self {
            shaper: Shaper::new(link, seed),
            rng: Rng::new(seed.rotate_left(29)),
            in_order: Duration::ZERO,
        }
    }
}

/// Each direction's own seed, so the two ways of one path draw apart and a third socket does
/// not shift the draws between the first two.
const fn direction_seed(seed: u64, from: usize, to: usize) -> u64 {
    let (from, to) = (from as u64, to as u64);
    seed ^ from.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ to.wrapping_add(1).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
}

/// Datagrams one send may carry: what macOS's batched `sendmsg_x` takes (`noq_udp::BATCH_SIZE`
/// there), which onicode's endpoints use (`onicode_net::udp`); Linux's GSO takes more. Every
/// platform's sim batches alike, so a seed runs the same everywhere. A batch's datagrams after
/// the first are cut to the first one's size, which splits a burst of frames across packets as
/// the real sockets do; with one datagram a send, no seed could split the client's
/// `NEW_CONNECTION_ID` frames (slopty's transport decisions, "A lone `NEW_CONNECTION_ID` is no
/// stateless reset"; noq-proto patch 21).
const BATCH: NonZeroUsize = match NonZeroUsize::new(32) {
    Some(batch) => batch,
    None => NonZeroUsize::MIN,
};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a over `bytes`, from `hash`.
fn fnv(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// `addr` with an IPv4-mapped IPv6 address turned back into IPv4, so a dual-stack peer's
/// address and a plain one meet in the routes.
const fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// A socket on a [`Net`]: noq's [`AsyncUdpSocket`], for
/// `noq::Endpoint::new_with_abstract_socket`.
#[derive(Debug)]
pub struct Socket {
    net: Net,
    end: usize,
    local: SocketAddr,
    epoch: Instant,
    /// Wakes the receiver when the first packet on its way is due. Made at the first wait,
    /// since a timer can only be made on the runtime.
    timer: Option<Pin<Box<Sleep>>>,
}

/// A closed socket frees its address, as the OS frees a port: a process restarted on the
/// same machine binds it again.
impl Drop for Socket {
    fn drop(&mut self) {
        let mut fabric = self.net.fabric.lock();
        let Some(public) = fabric.ends.get(self.end).map(|end| end.public) else {
            return;
        };
        if fabric.routes.get(&public) == Some(&self.end) {
            fabric.routes.remove(&public);
        }
        if let Some(end) = fabric.ends.get_mut(self.end) {
            // No longer this address's socket, for `rebind`'s lookup by address.
            end.local = SocketAddr::from(([0, 0, 0, 0], 0));
            end.inbox.clear();
        }
    }
}

impl AsyncUdpSocket for Socket {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        Box::pin(Sender {
            net: self.net.clone(),
            end: self.end,
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let next = {
                let mut fabric = self.net.fabric.lock();
                let taken = fabric.receive(self.end, bufs, meta);
                if taken > 0 {
                    return Poll::Ready(Ok(taken));
                }
                fabric.park(self.end, cx.waker())
            };
            let Some(due) = next else {
                return Poll::Pending;
            };
            let deadline = self.epoch.checked_add(due).unwrap_or(self.epoch);
            let timer = self
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
            timer.as_mut().reset(deadline);
            if timer.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    /// Nothing here fragments, so noq may probe for a larger MTU.
    fn may_fragment(&self) -> bool {
        false
    }
}

/// Sends for one [`Socket`]. The network takes every packet at once: a full queue is the link
/// model's to drop, as a router's is.
#[derive(Debug)]
struct Sender {
    net: Net,
    end: usize,
}

impl UdpSender for Sender {
    fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        _cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.net.fabric.lock().send(self.end, transmit);
        Poll::Ready(Ok(()))
    }

    /// A batch, as the real sockets take one: noq then builds a send as it does on them, where
    /// each datagram after the first is cut to the first one's size.
    fn max_transmit_segments(&self) -> NonZeroUsize {
        BATCH
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::net::Ipv4Addr;

    use super::*;

    fn at(host: u8, port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(192, 0, 2, host), port))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Send `data` from `socket` to `to`.
    fn send(socket: &Socket, to: SocketAddr, data: &[u8]) {
        let transmit = Transmit {
            destination: to,
            ecn: None,
            contents: data,
            segment_size: None,
            src_ip: None,
        };
        let mut sender = socket.create_sender();
        let waker = Waker::noop();
        let sent = sender
            .as_mut()
            .poll_send(&transmit, &mut Context::from_waker(waker));
        assert!(
            matches!(sent, Poll::Ready(Ok(()))),
            "the network takes every packet"
        );
    }

    /// The next datagram at `socket`, its sender and when it came.
    async fn recv(socket: &mut Socket, net: &Net) -> (Vec<u8>, SocketAddr, Duration) {
        let mut buf = vec![0_u8; 2_048];
        let mut meta = [RecvMeta::default()];
        let got = poll_fn(|cx| {
            let mut bufs = [IoSliceMut::new(&mut buf)];
            socket.poll_recv(cx, &mut bufs, &mut meta)
        })
        .await
        .unwrap();
        assert_eq!(got, 1);
        let [meta] = meta;
        (buf[..meta.len].to_vec(), meta.addr, net.elapsed())
    }

    /// Whether a datagram is waiting or on its way within `limit`.
    async fn arrives_within(socket: &mut Socket, net: &Net, limit: Duration) -> bool {
        tokio::time::timeout(limit, recv(socket, net)).await.is_ok()
    }

    #[tokio::test(start_paused = true)]
    async fn a_packet_takes_the_links_delay_on_the_virtual_clock() {
        let net = Net::new(
            Faults::over(Link {
                delay: ms(40),
                ..Link::CLEAR
            }),
            1,
        );
        let a = net.bind(at(1, 1)).unwrap();
        let mut b = net.bind(at(2, 2)).unwrap();
        send(&a, at(2, 2), b"hello");
        let (data, from, when) = recv(&mut b, &net).await;
        assert_eq!(
            (data.as_slice(), from, when),
            (&b"hello"[..], at(1, 1), ms(40))
        );
        assert!(net.bind(at(2, 2)).is_err(), "an address is bound once");
        send(&a, at(9, 9), b"nobody");
        assert_eq!(net.counts().unroutable, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_blackout_loses_what_is_sent_and_what_is_in_flight() {
        let net = Net::new(
            Faults::over(Link {
                delay: ms(10),
                ..Link::CLEAR
            }),
            1,
        );
        let a = net.bind(at(1, 1)).unwrap();
        let mut b = net.bind(at(2, 2)).unwrap();
        send(&a, at(2, 2), b"in flight");
        net.blackout(ms(100));
        send(&a, at(2, 2), b"into the dark");
        assert!(
            !arrives_within(&mut b, &net, ms(200)).await,
            "nothing crosses a blackout"
        );
        assert_eq!(net.counts().blacked_out, 2);
        send(&a, at(2, 2), b"after");
        assert_eq!(recv(&mut b, &net).await.0, b"after");
    }

    #[tokio::test(start_paused = true)]
    async fn a_rebound_socket_is_seen_at_its_new_address_and_the_old_one_is_gone() {
        let net = Net::new(Faults::CLEAR, 1);
        let mut client = net.bind(at(1, 1)).unwrap();
        let mut server = net.bind(at(2, 2)).unwrap();
        send(&server, at(1, 1), b"to the old mapping");
        net.rebind(at(1, 1), at(3, 3)).unwrap();
        assert!(
            !arrives_within(&mut client, &net, ms(10)).await,
            "the old mapping is gone"
        );
        send(&client, at(2, 2), b"from the new one");
        let (_, from, _) = recv(&mut server, &net).await;
        assert_eq!(from, at(3, 3));
        assert_eq!(
            client.local_addr().unwrap(),
            at(1, 1),
            "the socket notices nothing"
        );
        send(&server, at(3, 3), b"back");
        assert_eq!(recv(&mut client, &net).await.0, b"back");
    }

    #[tokio::test(start_paused = true)]
    async fn packets_keep_their_turn_unless_reordering_is_asked_for() {
        let jittery = Link {
            delay: ms(5),
            jitter: ms(20),
            ..Link::CLEAR
        };
        for (reorder, overtaken) in [(0.0, false), (0.3, true)] {
            let faults = Faults {
                link: jittery,
                reorder,
                reorder_by: ms(30),
            };
            let net = Net::new(faults, 5);
            let a = net.bind(at(1, 1)).unwrap();
            let mut b = net.bind(at(2, 2)).unwrap();
            for seq in 0..50_u8 {
                send(&a, at(2, 2), &[seq]);
            }
            let mut order = Vec::new();
            for _packet in 0..50 {
                order.push(recv(&mut b, &net).await.0[0]);
            }
            let sorted = order.windows(2).all(|w| w[0] < w[1]);
            assert_eq!(!sorted, overtaken, "reorder {reorder}: {order:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_seed_repeats_its_run_and_another_seed_does_not() {
        let run = async |seed| {
            let link = Link {
                delay: ms(5),
                jitter: ms(3),
                loss: 0.3,
                ..Link::CLEAR
            };
            let net = Net::new(Faults::over(link), seed);
            let a = net.bind(at(1, 1)).unwrap();
            let mut b = net.bind(at(2, 2)).unwrap();
            for seq in 0..100_u8 {
                send(&a, at(2, 2), &[seq]);
            }
            while arrives_within(&mut b, &net, ms(50)).await {}
            (net.digest(), net.counts())
        };
        let (first, counts) = run(7).await;
        assert_eq!(run(7).await, (first, counts));
        assert_ne!(run(8).await.0, first);
        assert_eq!(counts.shaped.sent + counts.shaped.lost, 100);
        assert_eq!(counts.delivered, counts.shaped.sent);
    }
}
