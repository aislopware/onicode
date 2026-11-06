//! Replica-to-replica sync of one session doc over a `doc/{chatId}` stream.
//!
//! Any two replicas of a chat (host, viewer, or a third engine holding it as a mailbox) converge
//! by trading Loro version vectors: each side sends exactly the ops the other lacks, so a reconnect
//! can never strand a range. Wire, one frame each ([`Frame`]):
//!
//! 1. opener → `Open{live, vv}`
//! 2. responder → `State{vv}`, then its catch-up: the updates past the opener's vv, or its whole
//!    (shallow) snapshot when the opener's vv is behind its shallow root
//! 3. opener → its catch-up past the responder's vv, then `Ack{vv}`; responder → `Ack{vv}`
//! 4. live sessions keep going: each change is pushed as `Updates` past what the peer holds, and a
//!    side that imported something answers `Ack{vv}` on the next [`ACK_EVERY`] tick. A one-shot
//!    session ends after the two acks.
//!
//! A replica that receives a snapshot adopts it ([`Replica::adopt`]) and the session ends: the
//! caller reopens the doc and starts again.

use std::time::Duration;

use bytes::Bytes;
use loro::{ExportMode, ID, ImportStatus, LoroDoc, VersionVector};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::frames::{FrameError, FrameRx, FrameTx};

/// How long a replica that imported something waits before acking, so a burst acks once.
pub const ACK_EVERY: Duration = Duration::from_millis(120);

/// Loro bytes per `Updates` frame; a larger export is split and reassembled.
pub const CHUNK: usize = 256 * 1024;

/// The most one reassembled batch may hold.
pub const MAX_BATCH: usize = 256 << 20;

/// Bytes queued on the stream past which a live session waits before pushing more; the
/// changes made meanwhile go out as one export.
pub const SEND_WINDOW: usize = 1 << 20;

/// Stream reset code: the responder holds no replica of the chat.
pub const NOT_HELD: u32 = 0x444f_0001;

/// Frame bodies from this size are LZ4-compressed when that makes them smaller.
const COMPRESS_FROM: usize = 512;

/// Set on the tag byte when the body is compressed.
const COMPRESSED: u8 = 0x80;

/// The largest body a compressed frame may claim to inflate to.
const MAX_PLAIN: usize = CHUNK + 64 * 1024;

const TAG_OPEN: u8 = 1;
const TAG_STATE: u8 = 2;
const TAG_UPDATES: u8 = 3;
const TAG_ACK: u8 = 4;

const FLAG_MORE: u8 = 1;
const FLAG_SNAPSHOT: u8 = 2;

/// Why a session stopped short.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PeerDocError {
    /// The stream failed or closed mid-exchange.
    #[error(transparent)]
    Stream(#[from] FrameError),
    /// The peer sent something the protocol does not allow.
    #[error("protocol: {0}")]
    Protocol(&'static str),
    /// Loro refused to export or import.
    #[error("loro: {0}")]
    Loro(String),
    /// The local replica failed to apply what arrived.
    #[error("replica: {0}")]
    Replica(String),
}

/// How a session ended without error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// A one-shot session finished: both sides hold the union and acked it.
    Done,
    /// The peer closed the stream.
    Closed,
    /// The replica went away (evicted or deleted) or stopped publishing changes.
    Gone,
    /// A snapshot arrived and the replica adopted it; reopen before syncing again.
    Adopted,
}

/// One frame of the doc protocol. The first byte is the tag, with its top bit (`0x80`) set when the
/// body after it is an LZ4 block with its size prepended.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// The opener's first frame: `[live: u8][oplog vv]`.
    Open {
        /// Keep the session open and push changes both ways.
        live: bool,
        /// Everything the opener holds.
        vv: VersionVector,
    },
    /// The responder's first frame: `[oplog vv]`.
    State {
        /// Everything the responder holds.
        vv: VersionVector,
    },
    /// Loro bytes: `[flags: u8][bytes]`; flag 1 = more chunks of this batch follow, 2 = the
    /// batch is a snapshot to adopt.
    Updates {
        /// Another chunk of the same batch follows.
        more: bool,
        /// The batch is a snapshot, not updates.
        snapshot: bool,
        /// This chunk's Loro bytes.
        bytes: Vec<u8>,
    },
    /// What the sender holds now: `[oplog vv]`.
    Ack {
        /// The sender's oplog vv.
        vv: VersionVector,
    },
}

impl Frame {
    /// The frame's payload, compressed when that pays.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let (tag, body) = match self {
            Self::Open { live, vv } => {
                let mut body = vec![u8::from(*live)];
                body.extend_from_slice(&vv.encode());
                (TAG_OPEN, body)
            }
            Self::State { vv } => (TAG_STATE, vv.encode()),
            Self::Updates {
                more,
                snapshot,
                bytes,
            } => {
                let mut flags = 0;
                if *more {
                    flags |= FLAG_MORE;
                }
                if *snapshot {
                    flags |= FLAG_SNAPSHOT;
                }
                let mut body = Vec::with_capacity(bytes.len() + 1);
                body.push(flags);
                body.extend_from_slice(bytes);
                (TAG_UPDATES, body)
            }
            Self::Ack { vv } => (TAG_ACK, vv.encode()),
        };
        if body.len() >= COMPRESS_FROM {
            let packed = lz4_flex::compress_prepend_size(&body);
            if packed.len() < body.len() {
                let mut out = Vec::with_capacity(packed.len() + 1);
                out.push(tag | COMPRESSED);
                out.extend_from_slice(&packed);
                return out;
            }
        }
        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(tag);
        out.extend_from_slice(&body);
        out
    }

    /// Parses a payload.
    ///
    /// # Errors
    ///
    /// [`PeerDocError::Protocol`] for anything malformed, including a compressed body that would
    /// inflate past the frame limit.
    pub fn decode(payload: &[u8]) -> Result<Self, PeerDocError> {
        let (&tag, rest) = payload
            .split_first()
            .ok_or(PeerDocError::Protocol("empty frame"))?;
        let inflated;
        let body = if tag & COMPRESSED == 0 {
            rest
        } else {
            let (size, packed) = lz4_flex::block::uncompressed_size(rest)
                .map_err(|_| PeerDocError::Protocol("bad compressed frame"))?;
            if size > MAX_PLAIN {
                return Err(PeerDocError::Protocol("compressed frame too large"));
            }
            inflated = lz4_flex::decompress(packed, size)
                .map_err(|_| PeerDocError::Protocol("bad compressed frame"))?;
            &inflated[..]
        };
        let vv = |bytes: &[u8]| {
            VersionVector::decode(bytes).map_err(|_| PeerDocError::Protocol("bad version vector"))
        };
        match tag & !COMPRESSED {
            TAG_OPEN => {
                let (&live, vv_bytes) = body
                    .split_first()
                    .ok_or(PeerDocError::Protocol("short open"))?;
                Ok(Self::Open {
                    live: live != 0,
                    vv: vv(vv_bytes)?,
                })
            }
            TAG_STATE => Ok(Self::State { vv: vv(body)? }),
            TAG_UPDATES => {
                let (&flags, bytes) = body
                    .split_first()
                    .ok_or(PeerDocError::Protocol("short updates"))?;
                Ok(Self::Updates {
                    more: flags & FLAG_MORE != 0,
                    snapshot: flags & FLAG_SNAPSHOT != 0,
                    bytes: bytes.to_vec(),
                })
            }
            TAG_ACK => Ok(Self::Ack { vv: vv(body)? }),
            _ => Err(PeerDocError::Protocol("unknown frame")),
        }
    }
}

/// The local side of a session: one replica of the chat's doc.
pub trait Replica: Send + Sync {
    /// The doc as it is now (a shared reference), or `None` once the replica is gone.
    fn doc(&self) -> Option<LoroDoc>;

    /// Ticks after every change to the doc, local or imported.
    fn changes(&self) -> watch::Receiver<u64>;

    /// Imports updates from the peer; `catch_up` marks the batch that answers the handshake.
    ///
    /// # Errors
    ///
    /// The replica's reason, when the bytes do not apply.
    fn import(&self, bytes: &[u8], catch_up: bool) -> Result<ImportStatus, String>;

    /// Replaces the doc with a snapshot the peer sent because this replica was behind its shallow
    /// root. The replica re-queues its own unresolved commands onto the new doc.
    ///
    /// # Errors
    ///
    /// The replica's reason, when the snapshot does not load.
    fn adopt(&self, snapshot: &[u8]) -> Result<(), String>;

    /// `peer` holds everything in `vv`.
    fn acked(&self, peer: &str, vv: &VersionVector);
}

/// Opens a session from this replica's side: sends `Open`, then runs the exchange. `peer` names
/// the other device for [`Replica::acked`].
///
/// # Errors
///
/// The first failure; the doc keeps whatever was imported before it.
pub async fn open<R, Tx, Rx>(
    replica: &R,
    peer: &str,
    live: bool,
    tx: Tx,
    mut rx: Rx,
) -> Result<End, PeerDocError>
where
    R: Replica + ?Sized,
    Tx: FrameTx,
    Rx: FrameRx,
{
    let Some(doc) = replica.doc() else {
        return Ok(End::Gone);
    };
    let changes = replica.changes();
    send(
        &tx,
        &Frame::Open {
            live,
            vv: doc.oplog_vv(),
        },
    )?;
    let peer_vv = match recv(&mut rx).await? {
        None => return Ok(End::Closed),
        Some(Frame::State { vv }) => vv,
        Some(_) => return Err(PeerDocError::Protocol("expected state")),
    };
    let mut held = peer_vv.clone();
    let Some(batch) = recv_batch(&mut rx, replica, peer, &mut held).await? else {
        return Ok(End::Closed);
    };
    if apply(replica, &batch, true, &mut held)? {
        return Ok(End::Adopted);
    }
    if catch_up(&tx, &doc, &peer_vv, &mut held)? {
        return Ok(End::Closed);
    }
    send(&tx, &Frame::Ack { vv: doc.oplog_vv() })?;
    let session = Session {
        replica,
        peer,
        doc,
        changes,
        held,
        tx,
    };
    session.finish(live, rx).await
}

/// Answers a session the peer opened: reads its `Open`, then runs the exchange.
///
/// # Errors
///
/// The first failure; the doc keeps whatever was imported before it.
pub async fn serve<R, Tx, Rx>(
    replica: &R,
    peer: &str,
    tx: Tx,
    mut rx: Rx,
) -> Result<End, PeerDocError>
where
    R: Replica + ?Sized,
    Tx: FrameTx,
    Rx: FrameRx,
{
    let (live, peer_vv) = match recv(&mut rx).await? {
        None => return Ok(End::Closed),
        Some(Frame::Open { live, vv }) => (live, vv),
        Some(_) => return Err(PeerDocError::Protocol("expected open")),
    };
    let Some(doc) = replica.doc() else {
        return Ok(End::Gone);
    };
    let changes = replica.changes();
    send(&tx, &Frame::State { vv: doc.oplog_vv() })?;
    let mut held = peer_vv.clone();
    // A peer behind our shallow root adopts the snapshot and closes; nothing more comes back.
    if catch_up(&tx, &doc, &peer_vv, &mut held)? {
        return match recv(&mut rx).await {
            Ok(None) | Err(_) => Ok(End::Closed),
            Ok(Some(_)) => Err(PeerDocError::Protocol("frame after snapshot")),
        };
    }
    let Some(batch) = recv_batch(&mut rx, replica, peer, &mut held).await? else {
        return Ok(End::Closed);
    };
    if apply(replica, &batch, true, &mut held)? {
        return Ok(End::Adopted);
    }
    send(&tx, &Frame::Ack { vv: doc.oplog_vv() })?;
    let session = Session {
        replica,
        peer,
        doc,
        changes,
        held,
        tx,
    };
    session.finish(live, rx).await
}

/// One reassembled `Updates` batch.
struct Batch {
    bytes: Vec<u8>,
    snapshot: bool,
}

/// Collects chunks into batches; acks that arrive between them are handled on the way.
#[derive(Default)]
struct Assembly {
    bytes: Vec<u8>,
    snapshot: Option<bool>,
}

impl Assembly {
    /// Adds a chunk; the batch once its last chunk is in.
    fn push(
        &mut self,
        more: bool,
        snapshot: bool,
        bytes: &[u8],
    ) -> Result<Option<Batch>, PeerDocError> {
        if self.snapshot.is_some_and(|s| s != snapshot) {
            return Err(PeerDocError::Protocol("mixed batch"));
        }
        if self.bytes.len() + bytes.len() > MAX_BATCH {
            return Err(PeerDocError::Protocol("batch too large"));
        }
        self.snapshot = Some(snapshot);
        self.bytes.extend_from_slice(bytes);
        if more {
            return Ok(None);
        }
        self.snapshot = None;
        Ok(Some(Batch {
            bytes: std::mem::take(&mut self.bytes),
            snapshot,
        }))
    }
}

/// Reads frames until a whole batch is in; `None` when the stream ends first.
async fn recv_batch<R, Rx>(
    rx: &mut Rx,
    replica: &R,
    peer: &str,
    held: &mut VersionVector,
) -> Result<Option<Batch>, PeerDocError>
where
    R: Replica + ?Sized,
    Rx: FrameRx,
{
    let mut assembly = Assembly::default();
    loop {
        match recv(rx).await? {
            None => return Ok(None),
            Some(Frame::Updates {
                more,
                snapshot,
                bytes,
            }) => {
                if let Some(batch) = assembly.push(more, snapshot, &bytes)? {
                    return Ok(Some(batch));
                }
            }
            Some(Frame::Ack { vv }) => {
                held.merge(&vv);
                replica.acked(peer, &vv);
            }
            Some(_) => return Err(PeerDocError::Protocol("expected updates")),
        }
    }
}

/// Applies a batch; `true` when it was a snapshot the replica adopted.
fn apply<R: Replica + ?Sized>(
    replica: &R,
    batch: &Batch,
    catch_up: bool,
    held: &mut VersionVector,
) -> Result<bool, PeerDocError> {
    if batch.snapshot {
        replica.adopt(&batch.bytes).map_err(PeerDocError::Replica)?;
        return Ok(true);
    }
    if batch.bytes.is_empty() {
        return Ok(false);
    }
    let status = replica
        .import(&batch.bytes, catch_up)
        .map_err(PeerDocError::Replica)?;
    // The peer holds what it sent us, so pushing our changes never echoes it back.
    for (&peer, &(_, end)) in status.success.iter() {
        if end > 0 {
            held.try_update_last(ID::new(peer, end - 1));
        }
    }
    Ok(false)
}

/// Sends the peer what it lacks given `peer_vv`; `true` when that was a snapshot.
fn catch_up<Tx: FrameTx>(
    tx: &Tx,
    doc: &LoroDoc,
    peer_vv: &VersionVector,
    held: &mut VersionVector,
) -> Result<bool, PeerDocError> {
    let own = doc.oplog_vv();
    if doc.is_shallow() && !peer_vv.includes_vv(&doc.shallow_since_vv().to_vv()) {
        let snapshot = doc
            .export(ExportMode::Snapshot)
            .map_err(|e| PeerDocError::Loro(e.to_string()))?;
        send_batch(tx, &snapshot, true)?;
        held.merge(&own);
        return Ok(true);
    }
    if peer_vv.includes_vv(&own) {
        send_batch(tx, &[], false)?;
    } else {
        let updates = doc
            .export(ExportMode::updates(peer_vv))
            .map_err(|e| PeerDocError::Loro(e.to_string()))?;
        send_batch(tx, &updates, false)?;
    }
    held.merge(&own);
    Ok(false)
}

/// A session past its handshake.
struct Session<'a, R: ?Sized, Tx> {
    replica: &'a R,
    peer: &'a str,
    doc: LoroDoc,
    changes: watch::Receiver<u64>,
    /// What the peer holds, as far as this side knows: its vv, raised by what was sent to it,
    /// what it sent, and its acks.
    held: VersionVector,
    tx: Tx,
}

enum Event<Rx> {
    Frame(Rx, Result<Option<Bytes>, FrameError>),
    Changed(bool),
    Room,
    Tick,
}

async fn next_frame<Rx: FrameRx>(mut rx: Rx) -> (Rx, Result<Option<Bytes>, FrameError>) {
    let result = rx.recv().await;
    (rx, result)
}

impl<R: Replica + ?Sized, Tx: FrameTx> Session<'_, R, Tx> {
    /// Waits for the peer's ack (one-shot) or runs the live phase.
    async fn finish<Rx: FrameRx>(self, live: bool, mut rx: Rx) -> Result<End, PeerDocError> {
        if live {
            return self.run_live(rx).await;
        }
        match recv(&mut rx).await? {
            None => Ok(End::Closed),
            Some(Frame::Ack { vv }) => {
                self.replica.acked(self.peer, &vv);
                Ok(End::Done)
            }
            Some(_) => Err(PeerDocError::Protocol("expected ack")),
        }
    }

    /// The live phase: pushes local changes past what the peer holds, imports and acks theirs.
    async fn run_live<Rx: FrameRx>(mut self, rx: Rx) -> Result<End, PeerDocError> {
        let mut reading = Box::pin(next_frame(rx));
        let mut assembly = Assembly::default();
        let mut ack_at: Option<Instant> = None;
        // Something may have changed between the catch-up export and now.
        let mut outgoing = !self.held.includes_vv(&self.doc.oplog_vv());
        loop {
            let tick = async move {
                match ack_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let event = tokio::select! {
                (rx, result) = &mut reading => Event::Frame(rx, result),
                changed = self.changes.changed(), if !outgoing => Event::Changed(changed.is_ok()),
                () = self.tx.room(SEND_WINDOW), if outgoing => Event::Room,
                () = tick => Event::Tick,
            };
            match event {
                Event::Frame(rx, result) => {
                    reading.set(next_frame(rx));
                    let Some(frame) = result? else {
                        return Ok(End::Closed);
                    };
                    match Frame::decode(&frame)? {
                        Frame::Updates {
                            more,
                            snapshot,
                            bytes,
                        } => {
                            let Some(batch) = assembly.push(more, snapshot, &bytes)? else {
                                continue;
                            };
                            if apply(self.replica, &batch, false, &mut self.held)? {
                                return Ok(End::Adopted);
                            }
                            ack_at.get_or_insert_with(|| Instant::now() + ACK_EVERY);
                        }
                        Frame::Ack { vv } => {
                            self.held.merge(&vv);
                            self.replica.acked(self.peer, &vv);
                        }
                        Frame::Open { .. } | Frame::State { .. } => {
                            return Err(PeerDocError::Protocol("handshake frame in live phase"));
                        }
                    }
                }
                Event::Changed(false) => return Ok(End::Gone),
                Event::Changed(true) => {
                    outgoing = !self.held.includes_vv(&self.doc.oplog_vv());
                }
                Event::Room => {
                    outgoing = false;
                    let own = self.doc.oplog_vv();
                    if self.held.includes_vv(&own) {
                        continue;
                    }
                    let updates = self
                        .doc
                        .export(ExportMode::updates(&self.held))
                        .map_err(|e| PeerDocError::Loro(e.to_string()))?;
                    send_batch(&self.tx, &updates, false)?;
                    self.held.merge(&own);
                }
                Event::Tick => {
                    ack_at = None;
                    send(
                        &self.tx,
                        &Frame::Ack {
                            vv: self.doc.oplog_vv(),
                        },
                    )?;
                }
            }
        }
    }
}

fn send<Tx: FrameTx>(tx: &Tx, frame: &Frame) -> Result<(), PeerDocError> {
    Ok(tx.send(&frame.encode())?)
}

/// Sends `bytes` as one batch in [`CHUNK`]-sized frames; an empty batch is one empty frame.
fn send_batch<Tx: FrameTx>(tx: &Tx, bytes: &[u8], snapshot: bool) -> Result<(), PeerDocError> {
    if bytes.is_empty() {
        return send(
            tx,
            &Frame::Updates {
                more: false,
                snapshot,
                bytes: Vec::new(),
            },
        );
    }
    let mut chunks = bytes.chunks(CHUNK).peekable();
    while let Some(chunk) = chunks.next() {
        send(
            tx,
            &Frame::Updates {
                more: chunks.peek().is_some(),
                snapshot,
                bytes: chunk.to_vec(),
            },
        )?;
    }
    Ok(())
}

async fn recv<Rx: FrameRx>(rx: &mut Rx) -> Result<Option<Frame>, PeerDocError> {
    match rx.recv().await? {
        None => Ok(None),
        Some(payload) => Frame::decode(&payload).map(Some),
    }
}

#[cfg(test)]
#[path = "peer_doc/tests.rs"]
mod tests;
