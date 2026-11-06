//! One QUIC connection as onicode's link: kinded streams, each in order and none waiting on
//! another, datagram channels, and a send buffer fitted to the congestion window.
//!
//! Ported from axolotty's `axquic` `link.rs` (MIT), generalized from numbered lanes to the
//! stream kinds of the transport design (§3.2): every stream is bidirectional and opens with a
//! [`StreamHead`]; the dialer's first stream is the control stream.
//!
//! A terminal's output lost in one packet holds back that terminal alone, never another
//! stream's echo. Priorities come with each stream: one carrying an echo is raised to
//! [`crate::transport::ECHO_PRIORITY`] and leaves in the next packet, unpaced, and is never held
//! for send-buffer room a flood has taken (noq-proto patch 23).
//!
//! A key and its echo also go as a datagram copy ([`Link::copy`]), a couple of milliseconds
//! after the stream copy, in a packet of their own: when the stream's packet is lost, the copy
//! arrives a round trip or two before the retransmission would. The receiver keeps whichever
//! comes first (it deduplicates by sequence number).
//!
//! QUIC's send buffer is held to twice the congestion window ([`SendWindow`]): what a flood
//! queues past that waits in the stream's queue, where [`StreamTx::pending`] counts it, so the
//! app sees a terminal fall behind and skips ahead (a snapshot) rather than streaming seconds of
//! stale output after Ctrl-C.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use noq::{Connection, ConnectionError, PathId, RecvStream, SendStream, VarInt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::NetError;
use crate::stream::{MAX_FRAME, StreamHead, frame, take_frame};

/// The largest read taken off a stream at once.
const READ_CHUNK: usize = 64 << 10;

/// How long a datagram copy waits after its stream copy: long enough that noq has written the
/// stream's packet, so the two are never lost together (slopty's `echo::COPY_DELAY`).
pub const COPY_DELAY: Duration = Duration::from_millis(2);
/// Datagram bytes that may be queued ahead of a copy: a copy behind more would arrive after
/// the stream's own retransmission.
const COPY_QUEUED_AHEAD: usize = 2 * 1200;

/// The send buffer never shrinks below this: a few round trips of a fast tailnet path.
pub const SEND_WINDOW_FLOOR: u64 = 256 << 10;
/// How often a writing link fits its send buffer to the congestion window.
const SEND_WINDOW_EVERY: Duration = Duration::from_millis(20);
/// noq's default send buffer: eight times a stream's 1.25 MB receive window.
const NOQ_SEND_WINDOW: u64 = 10_000_000;

/// QUIC's send buffer, fitted to twice the congestion window while the link writes. noq's
/// default is 10 MB, eight round trips of a 100 Mbit/s path: on a 5 MB/s Wi-Fi link a flood of
/// terminals kept 2.7 s of output in it after the program stopped (`tests/sim.rs`). Twice the
/// window never holds back a byte the congestion controller would send.
struct SendWindow {
    conn: Connection,
    /// When it was last fitted.
    at: Mutex<Option<Instant>>,
    /// Off to measure against noq's default ([`Link::keep_default_send_window`]).
    tune: AtomicBool,
    /// BBR's estimate of the path's bandwidth, bytes per second, as of the last fit; 0 unknown.
    bandwidth: AtomicU64,
}

impl SendWindow {
    fn new(conn: Connection) -> Self {
        conn.set_send_window(SEND_WINDOW_FLOOR);
        Self {
            conn,
            at: Mutex::new(None),
            tune: AtomicBool::new(true),
            bandwidth: AtomicU64::new(0),
        }
    }

    /// Called as a stream writes; fits the buffer at most every [`SEND_WINDOW_EVERY`], so an
    /// idle link wakes for nothing.
    fn fit(&self) {
        let now = Instant::now();
        {
            let mut at = lock(&self.at);
            if at.is_some_and(|t| now.duration_since(t) < SEND_WINDOW_EVERY) {
                return;
            }
            *at = Some(now);
        }
        let Some(cc) = self.conn.congestion_state(PathId::ZERO) else {
            return;
        };
        let metrics = cc.metrics();
        if let Some(bits) = metrics.bandwidth_estimate {
            self.bandwidth.store(bits / 8, Ordering::Relaxed);
        }
        if self.tune.load(Ordering::Relaxed) {
            self.conn.set_send_window(
                metrics
                    .congestion_window
                    .saturating_mul(2)
                    .max(SEND_WINDOW_FLOOR),
            );
        }
    }
}

struct Inner {
    conn: Connection,
    window: Arc<SendWindow>,
    /// Bytes queued on every stream and not yet in QUIC's send buffer.
    total: Arc<AtomicUsize>,
}

/// One connection's streams and datagrams. Cheap to clone; every clone is the same link.
#[derive(Clone)]
pub struct Link(Arc<Inner>);

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("id", &self.id())
            .field("peer", &self.peer())
            .finish_non_exhaustive()
    }
}

impl Link {
    /// The link over `conn`, a connection just made or accepted.
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self(Arc::new(Inner {
            window: Arc::new(SendWindow::new(conn.clone())),
            conn,
            total: Arc::new(AtomicUsize::new(0)),
        }))
    }

    /// The connection under it.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.0.conn
    }

    /// The connection's id, unique among this endpoint's connections.
    #[must_use]
    pub fn id(&self) -> usize {
        self.0.conn.stable_id()
    }

    /// Opens a stream that starts with `head`, at the kind's priority. Takes no round trip:
    /// the peer sees the stream with its head.
    ///
    /// # Errors
    ///
    /// When the connection is gone.
    pub async fn open(&self, head: &StreamHead) -> Result<(StreamTx, StreamRx), NetError> {
        let (send, recv) = self
            .0
            .conn
            .open_bi()
            .await
            .map_err(|e| NetError::stream(&e))?;
        let tx = self.writer(send, head.kind.priority());
        tx.send(head.encode());
        Ok((tx, StreamRx::new(recv)))
    }

    /// The next stream the peer opens; its head is the first thing to read off it
    /// ([`StreamRx::head`]), best in a task of its own, so a peer that opens a stream and
    /// says nothing holds up no other.
    ///
    /// # Errors
    ///
    /// When the connection is gone.
    pub async fn accept(&self) -> Result<(StreamTx, StreamRx), NetError> {
        let (send, recv) = self
            .0
            .conn
            .accept_bi()
            .await
            .map_err(|e| NetError::stream(&e))?;
        Ok((self.writer(send, 0), StreamRx::new(recv)))
    }

    /// Sends `data` as a datagram on `channel`, now.
    ///
    /// # Errors
    ///
    /// [`NetError::DatagramTooLarge`] when the path takes no datagram that large; the
    /// connection's error when it is gone.
    pub fn datagram(&self, channel: u8, data: &[u8]) -> Result<(), NetError> {
        let datagram = self.datagram_bytes(channel, data)?;
        self.0
            .conn
            .send_datagram(datagram)
            .map_err(|e| NetError::stream(&e))
    }

    /// Sends `data`, already sent on a stream, again as a datagram on `channel` after
    /// [`COPY_DELAY`], if the path takes a datagram that large and hardly any are queued; else
    /// not at all. For a key and its echo: small, and worth a round trip.
    pub fn copy(&self, channel: u8, data: &[u8]) {
        let Ok(datagram) = self.datagram_bytes(channel, data) else {
            return;
        };
        let conn = self.0.conn.clone();
        tokio::spawn(async move {
            tokio::time::sleep(COPY_DELAY).await;
            let queued =
                crate::transport::DATAGRAM_BUFFER.saturating_sub(conn.datagram_send_buffer_space());
            if queued <= COPY_QUEUED_AHEAD {
                let _ = conn.send_datagram(datagram);
            }
        });
    }

    fn datagram_bytes(&self, channel: u8, data: &[u8]) -> Result<Bytes, NetError> {
        let len = data.len().saturating_add(1);
        if self.0.conn.max_datagram_size().is_none_or(|max| len > max) {
            return Err(NetError::DatagramTooLarge(len));
        }
        let mut d = BytesMut::with_capacity(len);
        d.extend_from_slice(&[channel]);
        d.extend_from_slice(data);
        Ok(d.freeze())
    }

    /// The next datagram: its channel and its bytes. Datagrams are unordered and may be lost
    /// or come twice; an empty one is skipped.
    ///
    /// # Errors
    ///
    /// When the connection is gone.
    pub async fn recv_datagram(&self) -> Result<(u8, Bytes), NetError> {
        loop {
            let mut d = self
                .0
                .conn
                .read_datagram()
                .await
                .map_err(|e| NetError::stream(&e))?;
            if d.is_empty() {
                continue;
            }
            let rest = d.split_off(1);
            return Ok((d[0], rest));
        }
    }

    /// Bytes queued on every stream and not yet taken by QUIC's send buffer.
    #[must_use]
    pub fn pending_total(&self) -> usize {
        self.0.total.load(Ordering::Relaxed)
    }

    /// The path's smoothed round trip.
    #[must_use]
    pub fn rtt(&self) -> Option<Duration> {
        self.0.conn.rtt(PathId::ZERO)
    }

    /// The peer's address as the path sees it.
    #[must_use]
    pub fn peer(&self) -> Option<SocketAddr> {
        let a = self.0.conn.path(PathId::ZERO)?.remote_address().ok()?;
        Some(crate::transport::canonical(a))
    }

    /// BBR's estimate of the path's bandwidth in bytes per second, as of the last write; `None`
    /// until a write has measured it. The app sizes how far a terminal may fall behind by it.
    #[must_use]
    pub fn bandwidth(&self) -> Option<u64> {
        Some(self.0.window.bandwidth.load(Ordering::Relaxed)).filter(|&b| b > 0)
    }

    /// Leaves QUIC's send buffer at noq's default from now on, to measure against the fitted one.
    pub fn keep_default_send_window(&self) {
        self.0.window.tune.store(false, Ordering::Relaxed);
        self.0.conn.set_send_window(NOQ_SEND_WINDOW);
    }

    /// Ends the link with `code` ([`crate::close`]) and `reason`. What is still queued is
    /// dropped.
    pub fn close(&self, code: u32, reason: &[u8]) {
        self.0.conn.close(VarInt::from_u32(code), reason);
    }

    /// Resolves once the link has ended: `None` when either side closed it on purpose with
    /// [`crate::close::NORMAL`], else what ended it.
    pub async fn closed(&self) -> Option<NetError> {
        match self.0.conn.closed().await {
            ConnectionError::LocallyClosed => None,
            ConnectionError::ApplicationClosed(c)
                if c.error_code == VarInt::from_u32(crate::close::NORMAL) =>
            {
                None
            }
            other => Some(NetError::stream(&other)),
        }
    }

    /// Whether the link has ended.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.conn.close_reason().is_some()
    }

    /// A queued writer over `stream`, at `priority`.
    fn writer(&self, mut stream: SendStream, priority: i32) -> StreamTx {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let total = Arc::clone(&self.0.total);
        let window = Arc::clone(&self.0.window);
        let _ = stream.set_priority(priority);
        let writer = StreamTx {
            tx,
            pending: Arc::clone(&pending),
            total: Arc::clone(&total),
            priority: Arc::new(AtomicI32::new(priority)),
        };
        tokio::spawn(async move {
            let mut end = End::Finish;
            while let Some(out) = rx.recv().await {
                match out {
                    Out::Priority(p) => {
                        let _ = stream.set_priority(p);
                    }
                    Out::Data(b) => {
                        let n = b.len();
                        window.fit();
                        let sent = stream.write_chunk(b).await;
                        pending.fetch_sub(n, Ordering::Relaxed);
                        total.fetch_sub(n, Ordering::Relaxed);
                        if sent.is_err() {
                            end = End::Gone;
                            break;
                        }
                    }
                    Out::Finish => break,
                    Out::Reset(code) => {
                        end = End::Reset(code);
                        break;
                    }
                }
            }
            // Whatever is still queued will never be written: it is no longer pending.
            rx.close();
            while let Some(out) = rx.recv().await {
                if let Out::Data(b) = out {
                    pending.fetch_sub(b.len(), Ordering::Relaxed);
                    total.fetch_sub(b.len(), Ordering::Relaxed);
                }
            }
            match end {
                End::Finish => {
                    let _ = stream.finish();
                }
                End::Reset(code) => {
                    let _ = stream.reset(VarInt::from_u32(code));
                }
                End::Gone => {}
            }
        });
        writer
    }
}

enum Out {
    Data(Bytes),
    Priority(i32),
    Finish,
    Reset(u32),
}

enum End {
    Finish,
    Reset(u32),
    Gone,
}

/// The sending half of a stream: a queue that never blocks, written to QUIC in order. Dropping
/// it finishes the stream once the queue is written.
#[derive(Debug)]
pub struct StreamTx {
    tx: mpsc::UnboundedSender<Out>,
    pending: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    priority: Arc<AtomicI32>,
}

impl std::fmt::Debug for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Data(b) => write!(f, "Data({})", b.len()),
            Self::Priority(p) => write!(f, "Priority({p})"),
            Self::Finish => f.write_str("Finish"),
            Self::Reset(c) => write!(f, "Reset({c})"),
        }
    }
}

impl StreamTx {
    /// Queues `data`; never blocks. Bytes on one stream leave in order. Nothing happens once
    /// the stream has ended.
    pub fn send(&self, data: Bytes) {
        if data.is_empty() {
            return;
        }
        let n = data.len();
        // Counted before it is queued, so the writer never takes off what was not yet added.
        self.pending.fetch_add(n, Ordering::Relaxed);
        self.total.fetch_add(n, Ordering::Relaxed);
        if self.tx.send(Out::Data(data)).is_err() {
            self.pending.fetch_sub(n, Ordering::Relaxed);
            self.total.fetch_sub(n, Ordering::Relaxed);
        }
    }

    /// Queues `payload` as one length-prefixed frame.
    ///
    /// # Errors
    ///
    /// [`NetError::FrameTooLarge`] past [`MAX_FRAME`]; [`NetError::Closed`] once the stream
    /// has ended.
    pub fn send_frame(&self, payload: &[u8]) -> Result<(), NetError> {
        if self.is_closed() {
            return Err(NetError::Closed);
        }
        self.send(frame(payload)?);
        Ok(())
    }

    /// Sends what is queued after this at `priority`: noq sends the highest first and
    /// round-robins equal ones a packet each.
    pub fn set_priority(&self, priority: i32) {
        if self.priority.swap(priority, Ordering::Relaxed) != priority {
            let _ = self.tx.send(Out::Priority(priority));
        }
    }

    /// Bytes queued and not yet taken by QUIC's send buffer.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// Whether the stream's writer has ended (finished, reset or lost).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Finishes the stream once what is queued is written.
    pub fn finish(self) {
        let _ = self.tx.send(Out::Finish);
    }

    /// Abandons the stream with `code` once the bytes before this are handed to QUIC; the
    /// peer reads a reset.
    pub fn reset(self, code: u32) {
        let _ = self.tx.send(Out::Reset(code));
    }
}

/// The receiving half of a stream, buffered.
#[derive(Debug)]
pub struct StreamRx {
    recv: RecvStream,
    buf: BytesMut,
    max_frame: usize,
}

impl StreamRx {
    fn new(recv: RecvStream) -> Self {
        Self {
            recv,
            buf: BytesMut::new(),
            max_frame: MAX_FRAME,
        }
    }

    /// Frames longer than `max` are refused from now on.
    pub const fn set_max_frame(&mut self, max: usize) {
        self.max_frame = max;
    }

    /// Reads the head the stream opens with.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] for a head that does not parse; [`NetError::Closed`] when the
    /// stream ends first.
    pub async fn head(&mut self) -> Result<StreamHead, NetError> {
        loop {
            if let Some(head) = StreamHead::take(&mut self.buf)? {
                return Ok(head);
            }
            if !self.fill().await? {
                return Err(NetError::Closed);
            }
        }
    }

    /// The next frame; `None` at a clean end of the stream between frames.
    ///
    /// # Errors
    ///
    /// [`NetError::FrameTooLarge`], [`NetError::Protocol`] for a stream that ends inside a
    /// frame, or the stream's error.
    pub async fn frame(&mut self) -> Result<Option<Bytes>, NetError> {
        loop {
            if let Some(frame) = take_frame(&mut self.buf, self.max_frame)? {
                return Ok(Some(frame));
            }
            if !self.fill().await? {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(NetError::Protocol("stream ended inside a frame"))
                };
            }
        }
    }

    /// The next bytes, buffered ones first; `None` at the end of the stream.
    ///
    /// # Errors
    ///
    /// The stream's error.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, NetError> {
        if !self.buf.is_empty() {
            return Ok(Some(self.buf.split().freeze()));
        }
        self.recv
            .read_chunk(READ_CHUNK)
            .await
            .map_err(|e| NetError::stream(&e))
    }

    /// Tells the peer to stop sending, with `code`.
    pub fn stop(&mut self, code: u32) {
        let _ = self.recv.stop(VarInt::from_u32(code));
    }

    /// Reads more into the buffer; `false` at the end of the stream.
    async fn fill(&mut self) -> Result<bool, NetError> {
        match self
            .recv
            .read_chunk(READ_CHUNK)
            .await
            .map_err(|e| NetError::stream(&e))?
        {
            Some(chunk) => {
                self.buf.extend_from_slice(&chunk);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
