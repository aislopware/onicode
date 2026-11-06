//! Terminals on other devices, as a viewer sees them: each subscription is a `terminal/{id}`
//! stream ([`crate::link`]) on the peer link to the terminal's host.
//!
//! - A stream that dies with its link is reopened at once, as the link comes back, resuming after
//!   the last event delivered: no gap, no repeat. Only a host that refuses (the terminal is gone)
//!   or a link that stays down past [`REATTACH_FOR`] ends the subscription.
//! - Writes and resizes return at once and keep typed order. Input is numbered and held until the
//!   host acknowledges it; a new stream replays what is unacknowledged first, and the host applies
//!   each number once, so a dropped link neither loses nor repeats a keystroke.
//! - Each key also goes as a datagram copy, and the host's echo comes back as one
//!   ([`TerminalCopy`]); a copy counts only when its number is the next one owed.
//! - The link pings at the interactive rate while any terminal on it is attached.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_net::stream::channel;
use onicode_net::{StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_peer::{PeerLinks, StreamHandler};
use onicode_proto::TerminalEvent;
use onicode_rpc::RpcError;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::link::{self, AttachError, Attached, COPY_MAX, Outgoing, TerminalCopy};

/// How long a subscription whose link died keeps trying to reattach before it ends (the viewer
/// then resubscribes on its own schedule).
pub const REATTACH_FOR: Duration = Duration::from_secs(60);
/// The first wait between reattach attempts, doubling to [`REATTACH_MAX`]; a link coming back
/// cuts it short.
const REATTACH_MIN: Duration = Duration::from_millis(250);
const REATTACH_MAX: Duration = Duration::from_secs(4);
/// Recently closed or exited terminals remembered, so writes to them fail like a local write
/// would instead of vanishing into a queue.
const ENDED_MEMORY: usize = 256;
/// Unacknowledged input kept for replay on the next stream. Past this the oldest is dropped: a
/// link down that long has bigger problems, and the bound keeps a dead link from holding a paste
/// forever.
const MAX_UNACKED_BYTES: usize = 256 * 1024;

type Key = (String, String);

/// What a viewer asks of a remote terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Bytes for the shell.
    Input(Vec<u8>),
    /// The viewport's grid.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
}

#[derive(Default)]
struct Entry {
    /// Writers of attached streams, each tagged with the subscription that owns it (two views of
    /// one terminal each attach their own).
    writers: Vec<(u64, mpsc::UnboundedSender<Outgoing>)>,
    /// Where each subscription takes the echo copies.
    copies: Vec<(u64, mpsc::UnboundedSender<(u64, Bytes)>)>,
    /// Last input seq handed out. Never reset while this viewer runs: the host drops any seq it
    /// has already applied from this client.
    next_input: u64,
    /// Input the host hasn't acknowledged yet, oldest first.
    unacked: VecDeque<(u64, Vec<u8>)>,
    unacked_bytes: usize,
    /// The last size asked for, sent again on every new stream.
    size: Option<(u16, u16)>,
}

impl Entry {
    fn writer(&mut self) -> Option<&mpsc::UnboundedSender<Outgoing>> {
        self.writers.retain(|(_, tx)| !tx.is_closed());
        self.writers.first().map(|(_, tx)| tx)
    }

    fn ack(&mut self, seq: u64) {
        while self.unacked.front().is_some_and(|(s, _)| *s <= seq) {
            if let Some((_, bytes)) = self.unacked.pop_front() {
                self.unacked_bytes = self.unacked_bytes.saturating_sub(bytes.len());
            }
        }
    }

    /// What a new stream carries before anything typed from now on.
    fn replay(&self) -> Vec<Outgoing> {
        let size = self
            .size
            .map(|(cols, rows)| Outgoing::Resize { cols, rows });
        self.unacked
            .iter()
            .map(|(seq, bytes)| Outgoing::Input {
                seq: *seq,
                bytes: bytes.clone(),
            })
            .chain(size)
            .collect()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The viewer's side of every terminal on another device. Cheap to clone; every clone is the
/// same hub. Hand it to the links as their [`StreamHandler`] (or through
/// [`crate::link::TerminalService`] on an engine) so echo copies reach it.
#[derive(Clone)]
pub struct RemoteTerminals {
    /// This viewer's identity; input seqs are per client.
    client: Arc<str>,
    entries: Arc<Mutex<HashMap<Key, Entry>>>,
    ended: Arc<Mutex<VecDeque<Key>>>,
    pumps: Arc<AtomicU64>,
    /// Attached subscriptions per device, to hold its link at the interactive ping rate.
    interactive: Arc<Mutex<HashMap<String, usize>>>,
}

impl Default for RemoteTerminals {
    fn default() -> Self {
        Self {
            client: uuid::Uuid::new_v4().to_string().into(),
            entries: Arc::default(),
            ended: Arc::default(),
            pumps: Arc::default(),
            interactive: Arc::default(),
        }
    }
}

impl RemoteTerminals {
    /// Queue `write` for the terminal `terminal_id` on `target`; never waits on the network.
    /// With no stream attached, input waits for the next one and the size is sent on it.
    ///
    /// # Errors
    ///
    /// Only for a terminal this viewer saw close or exit.
    pub fn send(
        &self,
        links: &PeerLinks,
        target: &str,
        terminal_id: &str,
        write: Write,
    ) -> Result<(), RpcError> {
        let key = (target.to_owned(), terminal_id.to_owned());
        if lock(&self.ended).contains(&key) {
            return Err(RpcError::Failed("Terminal has exited".into()));
        }
        let mut entries = lock(&self.entries);
        let entry = entries.entry(key).or_default();
        let out = match write {
            Write::Input(bytes) => {
                entry.next_input = entry.next_input.saturating_add(1);
                let seq = entry.next_input;
                entry.unacked_bytes = entry.unacked_bytes.saturating_add(bytes.len());
                entry.unacked.push_back((seq, bytes.clone()));
                while entry.unacked_bytes > MAX_UNACKED_BYTES && entry.unacked.len() > 1 {
                    if let Some((_, dropped)) = entry.unacked.pop_front() {
                        entry.unacked_bytes = entry.unacked_bytes.saturating_sub(dropped.len());
                    }
                }
                Outgoing::Input { seq, bytes }
            }
            Write::Resize { cols, rows } => {
                entry.size = Some((cols, rows));
                Outgoing::Resize { cols, rows }
            }
        };
        let Some(writer) = entry.writer() else {
            return Ok(());
        };
        let _ = writer.send(out.clone());
        if let Outgoing::Input { seq, bytes } = out
            && bytes.len() <= COPY_MAX
            && let Some(link) = links.live(target)
        {
            let copy = TerminalCopy::Input {
                terminal: terminal_id.to_owned(),
                client: self.client.to_string(),
                seq,
                bytes: Bytes::from(bytes),
            };
            link.copy(channel::TERMINAL_COPY, &copy.encode());
        }
        Ok(())
    }

    fn ack(&self, key: &Key, seq: u64) {
        if let Some(entry) = lock(&self.entries).get_mut(key) {
            entry.ack(seq);
        }
    }

    /// The terminal closed or exited: detach its streams, refuse further writes.
    pub fn ended(&self, target: &str, terminal_id: &str) {
        let key = (target.to_owned(), terminal_id.to_owned());
        lock(&self.entries).remove(&key);
        let mut ended = lock(&self.ended);
        if !ended.contains(&key) {
            if ended.len() == ENDED_MEMORY {
                ended.pop_front();
            }
            ended.push_back(key);
        }
    }

    /// An echo copy `device` sent: to every subscription of that terminal, which keeps it only
    /// when it is the next event owed.
    pub fn copied(&self, device: &str, copy: TerminalCopy) {
        let TerminalCopy::Data {
            terminal,
            seq,
            bytes,
        } = copy
        else {
            return;
        };
        let key = (device.to_owned(), terminal);
        if let Some(entry) = lock(&self.entries).get_mut(&key) {
            entry
                .copies
                .retain(|(_, tx)| tx.send((seq, bytes.clone())).is_ok());
        }
    }

    /// Hands a new stream's writer the replay, then makes it the one writes take.
    fn attach_writer(
        &self,
        key: &Key,
        pump: u64,
        writer: mpsc::UnboundedSender<Outgoing>,
        copies: mpsc::UnboundedSender<(u64, Bytes)>,
    ) {
        if lock(&self.ended).contains(key) {
            return;
        }
        let mut entries = lock(&self.entries);
        let entry = entries.entry(key.clone()).or_default();
        // Replay first, on the new stream itself: it carries them ahead of anything typed from
        // now on.
        for out in entry.replay() {
            let _ = writer.send(out);
        }
        entry.writers.retain(|(owner, _)| *owner != pump);
        entry.writers.push((pump, writer));
        entry.copies.retain(|(owner, _)| *owner != pump);
        entry.copies.push((pump, copies));
    }

    /// Takes the subscription's writer away. The entry stays: its input numbering must never
    /// restart while this viewer runs.
    fn detach_writer(&self, key: &Key, pump: u64) {
        if let Some(entry) = lock(&self.entries).get_mut(key) {
            entry.writers.retain(|(owner, _)| *owner != pump);
            entry.copies.retain(|(owner, _)| *owner != pump);
        }
    }

    /// Counts one more (`true`) or one fewer attached subscription to `device`, holding its link
    /// at the interactive ping rate while any is.
    fn interactive(&self, links: &PeerLinks, device: &str, attached: bool) {
        let mut counts = lock(&self.interactive);
        let count = counts.entry(device.to_owned()).or_default();
        *count = if attached {
            count.saturating_add(1)
        } else {
            count.saturating_sub(1)
        };
        let now = *count > 0;
        if !now {
            counts.remove(device);
        }
        links.set_interactive(device, now);
    }

    /// Whether a stream carries this terminal's writes now (tests, diagnostics).
    #[doc(hidden)]
    #[must_use]
    pub fn attached(&self, target: &str, terminal_id: &str) -> bool {
        lock(&self.entries)
            .get_mut(&(target.to_owned(), terminal_id.to_owned()))
            .is_some_and(|entry| entry.writer().is_some())
    }

    /// The terminal's events after `after_seq` (each a [`TerminalEvent`] as JSON), over a stream
    /// to `target` that is reopened whenever its link drops.
    ///
    /// # Errors
    ///
    /// [`RpcError::Failed`] when the host refuses (an unknown terminal); [`RpcError::Transport`]
    /// when the first stream cannot be opened.
    pub async fn subscribe(
        &self,
        links: Arc<PeerLinks>,
        target: &str,
        terminal_id: &str,
        after_seq: u64,
    ) -> Result<BoxStream<'static, serde_json::Value>, RpcError> {
        let key = (target.to_owned(), terminal_id.to_owned());
        let first =
            open(&links, &key, after_seq, &self.client)
                .await
                .map_err(|error| match error {
                    AttachError::Refused(message) => RpcError::Failed(message),
                    AttachError::Lost(message) => RpcError::Transport(message),
                })?;
        let (out, events) = mpsc::unbounded_channel();
        let id = self.pumps.fetch_add(1, Ordering::Relaxed);
        // Writes issued right after this returns must already take the stream.
        let (copies, copied) = mpsc::unbounded_channel();
        self.attach_writer(&key, id, first.outgoing.clone(), copies);
        self.interactive(&links, target, true);
        tokio::spawn(
            Pump {
                id,
                hub: self.clone(),
                key,
                links,
                out,
                last_seq: after_seq,
            }
            .run(first, copied),
        );
        Ok(futures::stream::unfold(events, |mut events| async move {
            events.recv().await.map(|event| (event, events))
        })
        .boxed())
    }
}

impl StreamHandler for RemoteTerminals {
    /// A viewer serves no terminal streams.
    fn stream(&self, _device: &str, _head: StreamHead, tx: StreamTx, _rx: StreamRx) {
        tx.reset(close::NOT_GRANTED);
    }

    fn datagram(&self, device: &str, channel: u8, data: Bytes) {
        if channel == channel::TERMINAL_COPY
            && let Some(copy) = TerminalCopy::decode(&data)
        {
            self.copied(device, copy);
        }
    }
}

/// Opens a `terminal/{id}` stream to the key's device and attaches over it.
async fn open(
    links: &PeerLinks,
    (target, terminal_id): &Key,
    after_seq: u64,
    client: &str,
) -> Result<Attached, AttachError> {
    let (tx, rx) = links
        .open(
            target,
            &StreamHead::new(StreamKind::Terminal, terminal_id.as_str()),
        )
        .await
        .map_err(|error| AttachError::Lost(error.to_string()))?;
    link::attach(tx, rx, after_seq, client).await
}

/// Why a stream stopped delivering.
enum Ended {
    /// The terminal exited, the host refused, or the viewer went away.
    Done,
    /// The stream or its link died: reattach after the last seq.
    Lost,
}

struct Pump {
    /// Owner tag for this subscription's writer.
    id: u64,
    hub: RemoteTerminals,
    key: Key,
    links: Arc<PeerLinks>,
    out: mpsc::UnboundedSender<serde_json::Value>,
    last_seq: u64,
}

impl Pump {
    async fn run(
        mut self,
        mut attached: Attached,
        mut copies: mpsc::UnboundedReceiver<(u64, Bytes)>,
    ) {
        loop {
            if matches!(self.pump(attached, &mut copies).await, Ended::Done) {
                break;
            }
            let Some(next) = self.reattach().await else {
                break;
            };
            attached = next;
            let (tx, rx) = mpsc::unbounded_channel();
            copies = rx;
            self.hub
                .attach_writer(&self.key, self.id, attached.outgoing.clone(), tx);
            // A new link starts at the idle rate.
            self.links.set_interactive(&self.key.0, true);
        }
        self.hub.detach_writer(&self.key, self.id);
        self.hub.interactive(&self.links, &self.key.0, false);
    }

    /// A new stream after the last one died, as soon as the link allows; `None` when the host
    /// refuses, the viewer went away, or the link stayed down past [`REATTACH_FOR`].
    async fn reattach(&self) -> Option<Attached> {
        let deadline = Instant::now() + REATTACH_FOR;
        let mut wait = REATTACH_MIN;
        let mut changes = self.links.subscribe_changes();
        loop {
            if self.out.is_closed() {
                return None;
            }
            changes.borrow_and_update();
            match open(&self.links, &self.key, self.last_seq, &self.hub.client).await {
                Ok(attached) => return Some(attached),
                Err(AttachError::Refused(message)) => {
                    tracing::debug!(%message, "terminal reattach refused");
                    return None;
                }
                Err(AttachError::Lost(message)) => {
                    tracing::debug!(%message, "terminal reattach failed");
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::select! {
                () = tokio::time::sleep(wait) => wait = (wait * 2).min(REATTACH_MAX),
                _ = changes.changed() => {}
                () = self.out.closed() => return None,
            }
        }
    }

    /// Deliver one event; `false` once the stream is over.
    fn deliver(&mut self, event: &TerminalEvent) -> bool {
        let (seq, exit) = match event {
            TerminalEvent::Data { seq, .. } => (*seq, false),
            TerminalEvent::Exit { seq, .. } => (*seq, true),
        };
        if seq <= self.last_seq {
            return true; // already delivered, by a copy or the previous stream
        }
        let Ok(value) = serde_json::to_value(event) else {
            return true;
        };
        self.last_seq = seq;
        if exit {
            self.hub.ended(&self.key.0, &self.key.1);
        }
        self.out.send(value).is_ok() && !exit
    }

    async fn pump(
        &mut self,
        attached: Attached,
        copies: &mut mpsc::UnboundedReceiver<(u64, Bytes)>,
    ) -> Ended {
        let Attached {
            outgoing: _outgoing,
            mut events,
            mut acks,
        } = attached;
        let ended = loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else { break Ended::Lost };
                    if !self.deliver(&event) {
                        break Ended::Done;
                    }
                }
                Some(seq) = acks.recv() => self.hub.ack(&self.key, seq),
                Some((seq, bytes)) = copies.recv() => {
                    // A copy only ever fills the very next place.
                    if seq == self.last_seq.saturating_add(1) {
                        let event = TerminalEvent::Data { seq, data: BASE64.encode(&bytes) };
                        if !self.deliver(&event) {
                            break Ended::Done;
                        }
                    }
                }
                () = self.out.closed() => break Ended::Done,
            }
        };
        self.hub.detach_writer(&self.key, self.id);
        ended
    }
}
