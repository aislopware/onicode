//! Registry replication over one `registry` stream between two replicas (engine ↔ engine, or a
//! thin client ↔ the engine it mirrors).
//!
//! Wire, one JSON frame each ([`Frame`]); the first byte is 0, or 1 when the JSON after it is an
//! LZ4 block with its size prepended (from 512 B, when that is smaller):
//!
//! 1. both sides → `{"t":"hello","vv":{origin: oseq}}`
//! 2. both sides → `{"t":"rows","rows":[..]}`… ending with one that carries `"vv"`: every row
//!    holding a write the peer's hello lacked
//! 3. live: each change → `{"t":"rows","rows":[..],"vv":{..}}` with the rows the peer lacks
//!
//! A receiver merges the rows ([`onicode_doc::merge_row`]) and raises its vv to a frame's `vv`,
//! which the sender only attaches once everything it holds up to that vv has been sent.

use std::time::Duration;

use onicode_doc::{RegistryDoc, RegistryRow, Vv};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::frames::{FrameError, FrameRx, FrameTx};

/// Frames from this size are LZ4-compressed when that makes them smaller.
const COMPRESS_FROM: usize = 512;

/// Serialized rows per frame, roughly; a catch-up past it is split.
const ROWS_PER_FRAME: usize = 512;

/// The largest frame body a compressed frame may claim to inflate to.
const MAX_PLAIN: usize = 64 << 20;

/// Bytes queued on the stream past which a live session waits before pushing more.
pub const SEND_WINDOW: usize = 1 << 20;

/// How long after a change a live session waits, so a burst of writes leaves as one frame.
pub const COALESCE: Duration = Duration::from_millis(20);

/// Why a session stopped short.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistryError {
    /// The stream failed or closed mid-exchange.
    #[error(transparent)]
    Stream(#[from] FrameError),
    /// The peer sent something the protocol does not allow.
    #[error("protocol: {0}")]
    Protocol(String),
}

/// How a session ended without error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The peer closed the stream.
    Closed,
    /// The replica stopped publishing changes.
    Gone,
}

/// One frame of the registry protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Frame {
    /// The sender's vv, first thing on the stream.
    Hello {
        /// Everything the sender holds.
        vv: Vv,
    },
    /// Rows the receiver lacks.
    Rows {
        /// The rows, whole.
        rows: Vec<RegistryRow>,
        /// Set once everything the sender holds up to it has been sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vv: Option<Vv>,
    },
}

impl Frame {
    /// The frame's payload, compressed when that pays.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Protocol`] if a row does not serialize.
    pub fn encode(&self) -> Result<Vec<u8>, RegistryError> {
        let json = serde_json::to_vec(self).map_err(|e| RegistryError::Protocol(e.to_string()))?;
        if json.len() >= COMPRESS_FROM {
            let packed = lz4_flex::compress_prepend_size(&json);
            if packed.len() < json.len() {
                let mut out = Vec::with_capacity(packed.len() + 1);
                out.push(1);
                out.extend_from_slice(&packed);
                return Ok(out);
            }
        }
        let mut out = Vec::with_capacity(json.len() + 1);
        out.push(0);
        out.extend_from_slice(&json);
        Ok(out)
    }

    /// Parses a payload.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Protocol`] for anything malformed.
    pub fn decode(payload: &[u8]) -> Result<Self, RegistryError> {
        let bad = |what: &str| RegistryError::Protocol(what.to_owned());
        let (&flag, body) = payload.split_first().ok_or_else(|| bad("empty frame"))?;
        let json = match flag {
            0 => body.to_vec(),
            1 => {
                let (size, packed) = lz4_flex::block::uncompressed_size(body)
                    .map_err(|_| bad("bad compressed frame"))?;
                if size > MAX_PLAIN {
                    return Err(bad("compressed frame too large"));
                }
                lz4_flex::decompress(packed, size).map_err(|_| bad("bad compressed frame"))?
            }
            _ => return Err(bad("unknown frame flag")),
        };
        serde_json::from_slice(&json).map_err(|e| RegistryError::Protocol(e.to_string()))
    }
}

/// The local side of a session: a registry replica several sessions share.
pub trait Replica: Send + Sync {
    /// Runs `f` on the doc under its lock.
    fn read<T>(&self, f: impl FnOnce(&RegistryDoc) -> T) -> T;

    /// Merges rows a peer sent and raises the vv; publishes any change to every session.
    fn merge(&self, rows: &[RegistryRow], vv: Option<&Vv>);

    /// Ticks after every change to the doc, local or merged.
    fn changes(&self) -> watch::Receiver<u64>;

    /// The peer's whole catch-up has been merged: this replica holds everything the peer held
    /// when the session opened.
    fn caught_up(&self);
}

/// Runs one session to its end. Both sides call this; neither needs to know who dialed.
///
/// # Errors
///
/// The first stream or protocol failure.
pub async fn run<R, Tx, Rx>(replica: &R, tx: Tx, mut rx: Rx) -> Result<End, RegistryError>
where
    R: Replica + ?Sized,
    Tx: FrameTx,
    Rx: FrameRx,
{
    let mut changes = replica.changes();
    send(
        &tx,
        &Frame::Hello {
            vv: replica.read(|doc| doc.vv().clone()),
        },
    )?;
    let mut held = match recv(&mut rx).await? {
        None => return Ok(End::Closed),
        Some(Frame::Hello { vv }) => vv,
        Some(Frame::Rows { .. }) => return Err(RegistryError::Protocol("expected hello".into())),
    };
    push(replica, &tx, &mut held, true)?;
    let mut caught_up = false;
    let mut outgoing = false;
    loop {
        tokio::select! {
            frame = recv(&mut rx) => {
                let Some(frame) = frame? else {
                    return Ok(End::Closed);
                };
                let Frame::Rows { rows, vv } = frame else {
                    return Err(RegistryError::Protocol("hello after hello".into()));
                };
                replica.merge(&rows, vv.as_ref());
                if let Some(vv) = vv {
                    raise(&mut held, &vv);
                    if !caught_up {
                        caught_up = true;
                        replica.caught_up();
                    }
                }
            }
            changed = changes.changed(), if !outgoing => {
                if changed.is_err() {
                    return Ok(End::Gone);
                }
                outgoing = true;
            }
            () = settle(&tx), if outgoing => {
                outgoing = false;
                push(replica, &tx, &mut held, false)?;
            }
        }
    }
}

/// Waits out [`COALESCE`] and the send window before a live push.
async fn settle<Tx: FrameTx>(tx: &Tx) {
    tokio::time::sleep(COALESCE).await;
    tx.room(SEND_WINDOW).await;
}

/// Sends every row `held` lacks, the last frame carrying this replica's vv, and raises `held` to
/// it. The catch-up (`always`) sends a frame even when nothing is missing, so the peer learns it
/// is caught up.
fn push<R: Replica + ?Sized, Tx: FrameTx>(
    replica: &R,
    tx: &Tx,
    held: &mut Vv,
    always: bool,
) -> Result<(), RegistryError> {
    let (rows, vv) = replica.read(|doc| (doc.rows_since(held), doc.vv().clone()));
    if !always && rows.is_empty() && vv == *held {
        return Ok(());
    }
    let mut chunks = rows.chunks(ROWS_PER_FRAME).peekable();
    if chunks.peek().is_none() {
        send(
            tx,
            &Frame::Rows {
                rows: Vec::new(),
                vv: Some(vv.clone()),
            },
        )?;
    }
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        send(
            tx,
            &Frame::Rows {
                rows: chunk.to_vec(),
                vv: last.then(|| vv.clone()),
            },
        )?;
    }
    raise(held, &vv);
    Ok(())
}

fn raise(held: &mut Vv, vv: &Vv) {
    for (origin, &oseq) in vv {
        let entry = held.entry(origin.clone()).or_default();
        *entry = (*entry).max(oseq);
    }
}

fn send<Tx: FrameTx>(tx: &Tx, frame: &Frame) -> Result<(), RegistryError> {
    Ok(tx.send(&frame.encode()?)?)
}

async fn recv<Rx: FrameRx>(rx: &mut Rx) -> Result<Option<Frame>, RegistryError> {
    match rx.recv().await? {
        None => Ok(None),
        Some(payload) => Frame::decode(&payload).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use onicode_doc::RegistryDoc;
    use onicode_proto::{Chat, ChatConfig, HarnessId, SandboxLevel};

    use super::*;
    use crate::frames::pipe::pipe;

    struct Mem {
        doc: Mutex<RegistryDoc>,
        changes: watch::Sender<u64>,
        caught_up: Mutex<u32>,
    }

    impl Mem {
        fn new(device: &str) -> Arc<Self> {
            Arc::new(Self {
                doc: Mutex::new(RegistryDoc::new(device)),
                changes: watch::Sender::new(0),
                caught_up: Mutex::new(0),
            })
        }

        fn write(&self, f: impl FnOnce(&mut RegistryDoc)) {
            f(&mut self.doc.lock().unwrap());
            self.changes.send_modify(|n| *n += 1);
        }

        fn chats(&self) -> Vec<Chat> {
            self.doc.lock().unwrap().read_chats().unwrap()
        }
    }

    impl Replica for Mem {
        fn read<T>(&self, f: impl FnOnce(&RegistryDoc) -> T) -> T {
            f(&self.doc.lock().unwrap())
        }

        fn merge(&self, rows: &[RegistryRow], vv: Option<&Vv>) {
            if self.doc.lock().unwrap().merge_remote(rows, vv) {
                self.changes.send_modify(|n| *n += 1);
            }
        }

        fn changes(&self) -> watch::Receiver<u64> {
            self.changes.subscribe()
        }

        fn caught_up(&self) {
            *self.caught_up.lock().unwrap() += 1;
        }
    }

    fn chat(id: &str, device: &str) -> Chat {
        Chat {
            id: id.into(),
            device_id: device.into(),
            title: Some(id.into()),
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: Some(ChatConfig {
                harness: HarnessId::Mock,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                access: Default::default(),
                plan: false,
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            harness_session_id: None,
            harness_session_cwd: None,
            harness_session_harness: None,
            parent_chat_id: None,
            child_kind: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
        }
    }

    /// Connects two replicas with a live session each way of one stream.
    fn link(
        a: &Arc<Mem>,
        b: &Arc<Mem>,
    ) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
        let (a_tx, b_rx) = pipe();
        let (b_tx, a_rx) = pipe();
        let spawn = |replica: Arc<Mem>, tx, rx| {
            tokio::spawn(async move {
                let _ = run(&*replica, tx, rx).await;
            })
        };
        (
            spawn(Arc::clone(a), a_tx, a_rx),
            spawn(Arc::clone(b), b_tx, b_rx),
        )
    }

    async fn wait_until(mut done: impl FnMut() -> bool) {
        for _ in 0..1_000 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition never held");
    }

    #[test]
    fn frames_round_trip_and_compress() {
        let mut doc = RegistryDoc::new("dev-a");
        for n in 0..50 {
            doc.upsert_chat(&chat(&format!("chat-{n}"), "dev-a"))
                .unwrap();
        }
        let frame = Frame::Rows {
            rows: doc.rows_since(&Vv::new()),
            vv: Some(doc.vv().clone()),
        };
        let wire = frame.encode().unwrap();
        assert_eq!(wire.first(), Some(&1), "rows compress");
        assert_eq!(Frame::decode(&wire).unwrap(), frame);
        let hello = Frame::Hello { vv: Vv::new() };
        assert_eq!(Frame::decode(&hello.encode().unwrap()).unwrap(), hello);
        assert!(Frame::decode(&[7]).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn sessions_catch_up_then_forward_live_writes_through_a_middle_replica() {
        let a = Mem::new("dev-a");
        let middle = Mem::new("dev-m");
        let c = Mem::new("dev-c");
        a.write(|doc| doc.upsert_chat(&chat("early", "dev-a")).unwrap());
        let ab = link(&a, &middle);
        let bc = link(&middle, &c);
        wait_until(|| c.chats().len() == 1).await;
        assert!(*a.caught_up.lock().unwrap() >= 1);

        a.write(|doc| doc.upsert_chat(&chat("live", "dev-a")).unwrap());
        c.write(|doc| {
            doc.rename_chat("early", "renamed at c").unwrap();
        });
        wait_until(|| {
            a.chats().len() == 2
                && c.chats().len() == 2
                && a.chats().first().and_then(|c| c.title.clone()).as_deref()
                    == Some("renamed at c")
        })
        .await;
        let vv = |m: &Mem| m.read(|doc| doc.vv().clone());
        wait_until(|| vv(&a) == vv(&middle) && vv(&middle) == vv(&c)).await;
        for task in [ab.0, ab.1, bc.0, bc.1] {
            task.abort();
        }
    }
}
