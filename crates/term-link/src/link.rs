//! One terminal's I/O over one QUIC stream on the peer link (`StreamKind::Terminal`, named by
//! the terminal's id), opened by the viewer to the host.
//!
//! Each stream frame ([`onicode_net::stream::frame`]) is `kind u8 ‖ payload`:
//!
//! - viewer → host: `ATTACH {afterSeq u64, client utf8}` (first frame, exactly once), `INPUT {seq
//!   u64, bytes}`, `RESIZE {cols u16, rows u16}`, `PING {nonce u64}`;
//! - host → viewer: `DATA {seq u64, bytes}`, `EXIT {seq u64, code i32}`, `ERROR utf8`, `PONG {nonce
//!   u64}`, `ACK {inputSeq u64}`.
//!
//! The host's side is an ordinary terminal subscription ([`HostTerminals`]), so replay and
//! `afterSeq` resume behave as they do locally. Input is numbered per viewer and acknowledged
//! once applied: whatever a dying link swallowed is replayed on the next stream, and the host
//! applies it exactly once. Liveness is the link's own (`onicode_peer`'s pings): a dead link
//! ends every stream on it.
//!
//! Keys and their echoes are also sent as datagram copies on [`channel::TERMINAL_COPY`]
//! ([`TerminalCopy`]), a couple of milliseconds after the stream copy: when the stream's packet
//! is lost, the copy lands a round trip early. Each end keeps a copy only when its sequence
//! number is the very next one it owes, so a copy never reorders or repeats anything.

use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::{BufMut as _, Bytes, BytesMut};
use onicode_net::stream::channel;
use onicode_net::transport::ECHO_PRIORITY;
use onicode_net::{StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_peer::{PeerLinks, StreamHandler};
use onicode_proto::TerminalEvent;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::remote::RemoteTerminals;

/// A device's own terminals, as the link's host side serves them.
pub trait HostTerminals: Clone + Send + Sync + 'static {
    /// Events after `after_seq` (the replay window), then the live tail; the channel ends after
    /// `Exit`.
    ///
    /// # Errors
    ///
    /// The terminal is unknown here.
    fn subscribe(
        &self,
        terminal_id: &str,
        after_seq: u64,
    ) -> Result<mpsc::UnboundedReceiver<TerminalEvent>, String>;

    /// Apply input numbered `seq` by `client`, once (a replay of a seq already applied is
    /// dropped). `Ok(false)` = a duplicate.
    ///
    /// # Errors
    ///
    /// The terminal is unknown or has exited, or the write failed.
    fn write_sequenced(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, String>;

    /// Apply input `seq` by `client` only when it is the next one owed (a datagram copy may come
    /// ahead of earlier input still on its stream). `Ok(false)` = not applied.
    ///
    /// # Errors
    ///
    /// As [`Self::write_sequenced`].
    fn write_next(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, String>;

    /// Resize the terminal's grid.
    ///
    /// # Errors
    ///
    /// The terminal is unknown, or the PTY refused.
    fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), String>;
}

const ATTACH: u8 = 1;
const INPUT: u8 = 2;
const RESIZE: u8 = 3;
const PING: u8 = 4;
const DATA: u8 = 16;
const EXIT: u8 = 17;
const ERROR: u8 = 18;
const PONG: u8 = 19;
const ACK: u8 = 20;

/// Largest frame either side accepts. Output batches are bounded by the engine's pacing, input
/// by its own limit; this only stops runaways.
const MAX_FRAME: usize = 16 << 20;
/// The host waits this long for `ATTACH`, the viewer as long for the host's first answer.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);
/// Output the host lets queue for a viewer before it cuts it: a viewer that far behind resumes
/// from the replay window instead of reading seconds of stale output.
const HOST_BACKLOG: usize = 4 << 20;
/// Output this soon after a key is its echo: sent ahead of other streams, and copied.
const ECHO_WINDOW: Duration = Duration::from_millis(100);
/// Terminal bytes a datagram copy carries at most; longer input or output rides its stream only.
pub const COPY_MAX: usize = 1024;

fn send(tx: &StreamTx, kind: u8, payload: &[u8]) -> bool {
    let mut frame = Vec::with_capacity(payload.len().saturating_add(1));
    frame.push(kind);
    frame.extend_from_slice(payload);
    tx.send_frame(&frame).is_ok()
}

fn u64_at(payload: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        payload.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn encode_event(event: &TerminalEvent) -> (u8, Vec<u8>) {
    match event {
        TerminalEvent::Data { seq, data } => {
            let bytes = BASE64.decode(data).unwrap_or_default();
            let mut payload = Vec::with_capacity(bytes.len().saturating_add(8));
            payload.extend_from_slice(&seq.to_be_bytes());
            payload.extend_from_slice(&bytes);
            (DATA, payload)
        }
        TerminalEvent::Exit { seq, exit_code, .. } => {
            let mut payload = seq.to_be_bytes().to_vec();
            payload.extend_from_slice(&exit_code.to_be_bytes());
            (EXIT, payload)
        }
    }
}

fn decode_event(kind: u8, payload: &[u8]) -> Option<TerminalEvent> {
    let seq = u64_at(payload, 0)?;
    match kind {
        DATA => Some(TerminalEvent::Data {
            seq,
            data: BASE64.encode(payload.get(8..)?),
        }),
        EXIT => Some(TerminalEvent::Exit {
            seq,
            exit_code: i32::from_be_bytes(payload.get(8..12)?.try_into().ok()?),
            signal: None,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Datagram copies
// ---------------------------------------------------------------------------

/// A datagram copy of a key or of its echo, on [`channel::TERMINAL_COPY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalCopy {
    /// Input `seq` that viewer `client` typed into `terminal`.
    Input {
        /// The terminal's id.
        terminal: String,
        /// The viewer that numbered it.
        client: String,
        /// Its number.
        seq: u64,
        /// The bytes.
        bytes: Bytes,
    },
    /// Output `seq` of `terminal`.
    Data {
        /// The terminal's id.
        terminal: String,
        /// Its number.
        seq: u64,
        /// The bytes.
        bytes: Bytes,
    },
}

fn put_str(out: &mut BytesMut, text: &str) {
    out.put_u16(u16::try_from(text.len()).unwrap_or(u16::MAX));
    out.put_slice(
        text.as_bytes()
            .get(..usize::from(u16::MAX))
            .unwrap_or(text.as_bytes()),
    );
}

fn take_str(data: &[u8], at: &mut usize) -> Option<String> {
    let len = usize::from(u16::from_be_bytes(
        data.get(*at..at.checked_add(2)?)?.try_into().ok()?,
    ));
    let start = at.checked_add(2)?;
    let end = start.checked_add(len)?;
    let text = std::str::from_utf8(data.get(start..end)?).ok()?.to_owned();
    *at = end;
    Some(text)
}

impl TerminalCopy {
    /// The copy as it goes in a datagram (after the channel byte).
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut out = BytesMut::new();
        match self {
            Self::Input {
                terminal,
                client,
                seq,
                bytes,
            } => {
                out.put_u8(INPUT);
                put_str(&mut out, terminal);
                put_str(&mut out, client);
                out.put_u64(*seq);
                out.put_slice(bytes);
            }
            Self::Data {
                terminal,
                seq,
                bytes,
            } => {
                out.put_u8(DATA);
                put_str(&mut out, terminal);
                out.put_u64(*seq);
                out.put_slice(bytes);
            }
        }
        out.freeze()
    }

    /// The copy a datagram carries; `None` for anything else.
    #[must_use]
    pub fn decode(data: &Bytes) -> Option<Self> {
        let (&kind, _) = data.split_first()?;
        let mut at = 1;
        let terminal = take_str(data, &mut at)?;
        match kind {
            INPUT => {
                let client = take_str(data, &mut at)?;
                let seq = u64_at(data, at)?;
                Some(Self::Input {
                    terminal,
                    client,
                    seq,
                    bytes: data.slice(at.checked_add(8)?..),
                })
            }
            DATA => {
                let seq = u64_at(data, at)?;
                Some(Self::Data {
                    terminal,
                    seq,
                    bytes: data.slice(at.checked_add(8)?..),
                })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

/// Serve one viewer's stream to `terminal_id` until either side ends it. `copies` is the link
/// the stream rides, for the echoes' datagram copies.
pub async fn serve<T: HostTerminals>(
    terminals: T,
    terminal_id: String,
    tx: StreamTx,
    mut rx: StreamRx,
    copies: Option<onicode_net::Link>,
) {
    rx.set_max_frame(MAX_FRAME);
    let attach = match tokio::time::timeout(ATTACH_TIMEOUT, rx.frame()).await {
        Ok(Ok(Some(frame))) => match frame.split_first() {
            Some((&ATTACH, payload)) => u64_at(payload, 0).map(|after| {
                (
                    after,
                    String::from_utf8_lossy(payload.get(8..).unwrap_or_default()).into_owned(),
                )
            }),
            _ => None,
        },
        _ => None,
    };
    let Some((after_seq, client)) = attach.filter(|(_, client)| !client.is_empty()) else {
        send(&tx, ERROR, b"expected attach");
        tx.finish();
        return;
    };
    let mut events = match terminals.subscribe(&terminal_id, after_seq) {
        Ok(events) => events,
        Err(err) => {
            send(&tx, ERROR, err.as_bytes());
            tx.finish();
            return;
        }
    };
    let mut echo_until: Option<Instant> = None;
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { break };
                let exit = matches!(event, TerminalEvent::Exit { .. });
                let (kind, payload) = encode_event(&event);
                let echo = kind == DATA
                    && payload.len() <= COPY_MAX.saturating_add(8)
                    && echo_until.is_some_and(|until| Instant::now() < until);
                tx.set_priority(if echo { ECHO_PRIORITY } else { StreamKind::Terminal.priority() });
                if !send(&tx, kind, &payload) || exit {
                    break;
                }
                if echo
                    && let (Some(link), TerminalEvent::Data { seq, .. }) = (&copies, &event)
                {
                    let copy = TerminalCopy::Data {
                        terminal: terminal_id.clone(),
                        seq: *seq,
                        bytes: Bytes::copy_from_slice(payload.get(8..).unwrap_or_default()),
                    };
                    link.copy(channel::TERMINAL_COPY, &copy.encode());
                }
                if tx.pending() > HOST_BACKLOG {
                    tracing::debug!(terminal = %terminal_id, "terminal viewer fell too far behind");
                    tx.reset(close::NORMAL);
                    return;
                }
            }
            frame = rx.frame() => {
                let Ok(Some(frame)) = frame else { break };
                let Some((&kind, payload)) = frame.split_first() else { break };
                match kind {
                    INPUT if payload.len() >= 8 => {
                        let seq = u64_at(payload, 0).unwrap_or_default();
                        let bytes = payload.get(8..).unwrap_or_default();
                        if let Err(err) = terminals.write_sequenced(&terminal_id, &client, seq, bytes) {
                            tracing::debug!(error = %err, "terminal link input failed");
                        }
                        echo_until = Some(Instant::now() + ECHO_WINDOW);
                        if !send(&tx, ACK, &seq.to_be_bytes()) {
                            break;
                        }
                    }
                    RESIZE if payload.len() == 4 => {
                        if let Err(err) = terminals.resize(
                            &terminal_id,
                            u16::from_be_bytes([payload[0], payload[1]]),
                            u16::from_be_bytes([payload[2], payload[3]]),
                        ) {
                            tracing::debug!(error = %err, "terminal link resize failed");
                        }
                    }
                    PING if payload.len() == 8 => {
                        if !send(&tx, PONG, payload) {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
    tx.finish();
}

/// Serves this device's terminals to the viewers that open `terminal/{id}` streams to it, and
/// takes the datagram copies on [`channel::TERMINAL_COPY`] both ways: keys for the terminals
/// here, echoes for `hub`'s viewers. Register it for [`StreamKind::Terminal`] and for the copy
/// channel.
pub struct TerminalService<T> {
    terminals: T,
    hub: RemoteTerminals,
    /// The links the streams ride, for the echoes' copies; weak, since the links hold this.
    links: RwLock<Weak<PeerLinks>>,
}

impl<T: HostTerminals> TerminalService<T> {
    /// Serves `terminals` and hands echo copies to `hub`.
    #[must_use]
    pub fn new(terminals: T, hub: RemoteTerminals) -> Self {
        Self {
            terminals,
            hub,
            links: RwLock::new(Weak::new()),
        }
    }

    /// The links the streams ride, once they are up.
    pub fn set_links(&self, links: &Arc<PeerLinks>) {
        *self
            .links
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(links);
    }

    fn links(&self) -> Option<Arc<PeerLinks>> {
        self.links
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .upgrade()
    }
}

impl<T: HostTerminals> StreamHandler for TerminalService<T> {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        if head.kind != StreamKind::Terminal || head.name.is_empty() {
            tx.reset(close::NOT_GRANTED);
            return;
        }
        let copies = self.links().and_then(|links| links.live(device));
        tokio::spawn(serve(self.terminals.clone(), head.name, tx, rx, copies));
    }

    fn datagram(&self, device: &str, channel: u8, data: Bytes) {
        if channel != channel::TERMINAL_COPY {
            return;
        }
        match TerminalCopy::decode(&data) {
            Some(TerminalCopy::Input {
                terminal,
                client,
                seq,
                bytes,
            }) => {
                if let Err(err) = self.terminals.write_next(&terminal, &client, seq, &bytes) {
                    tracing::debug!(error = %err, "terminal input copy not applied");
                }
            }
            Some(copy @ TerminalCopy::Data { .. }) => self.hub.copied(device, copy),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Viewer
// ---------------------------------------------------------------------------

/// What the viewer sends over an attached stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// Input numbered by the viewer (see the module docs).
    Input {
        /// Its number.
        seq: u64,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// The viewport's grid.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
}

/// The viewer's end of an attached stream. Dropping every sender detaches.
pub struct Attached {
    /// Input and resizes for the host, in order.
    pub outgoing: mpsc::UnboundedSender<Outgoing>,
    /// Host events in order. Ends after `Exit`, or when the stream or its link dies; then the
    /// caller resumes after its last seq on a new stream.
    pub events: mpsc::UnboundedReceiver<TerminalEvent>,
    /// Highest input seq the host has applied, as it acknowledges them.
    pub acks: mpsc::UnboundedReceiver<u64>,
}

/// Why an attach failed.
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    /// The host refused it (an unknown terminal, a viewer it does not serve): trying again
    /// cannot help.
    #[error("{0}")]
    Refused(String),
    /// The stream or its link failed first.
    #[error("terminal stream lost: {0}")]
    Lost(String),
}

/// Handles one host frame on the viewer's side; `false` once the stream is over.
fn take_host_frame(
    kind: u8,
    payload: &[u8],
    events: &mpsc::UnboundedSender<TerminalEvent>,
    acks: &mpsc::UnboundedSender<u64>,
) -> bool {
    match kind {
        PONG => true,
        ACK => {
            if let Some(seq) = u64_at(payload, 0) {
                let _ = acks.send(seq);
            }
            true
        }
        ERROR => {
            tracing::debug!(error = %String::from_utf8_lossy(payload), "terminal stream ended by host");
            false
        }
        _ => decode_event(kind, payload).is_some_and(|event| {
            let exit = matches!(event, TerminalEvent::Exit { .. });
            events.send(event).is_ok() && !exit
        }),
    }
}

/// Attach to the host's terminal over a stream just opened to it, as viewer `client`, resuming
/// after `after_seq`.
///
/// # Errors
///
/// [`AttachError::Refused`] when the host refuses; [`AttachError::Lost`] when the stream fails
/// before the host answers.
pub async fn attach(
    tx: StreamTx,
    mut rx: StreamRx,
    after_seq: u64,
    client: &str,
) -> Result<Attached, AttachError> {
    rx.set_max_frame(MAX_FRAME);
    // What the viewer sends is keys: it leaves ahead of other streams.
    tx.set_priority(ECHO_PRIORITY);
    let mut hello = after_seq.to_be_bytes().to_vec();
    hello.extend_from_slice(client.as_bytes());
    // The first frame back says whether the host took the attach: a refusal is an ERROR, a
    // live terminal at least answers this ping.
    if !send(&tx, ATTACH, &hello) || !send(&tx, PING, &0_u64.to_be_bytes()) {
        return Err(AttachError::Lost("stream closed".into()));
    }
    let first = match tokio::time::timeout(ATTACH_TIMEOUT, rx.frame()).await {
        Err(_) => return Err(AttachError::Lost("no answer".into())),
        Ok(Err(err)) => return Err(AttachError::Lost(err.to_string())),
        Ok(Ok(None)) => return Err(AttachError::Lost("stream ended".into())),
        Ok(Ok(Some(frame))) => frame,
    };
    let Some((&kind, payload)) = first.split_first() else {
        return Err(AttachError::Lost("empty frame".into()));
    };
    if kind == ERROR {
        return Err(AttachError::Refused(
            String::from_utf8_lossy(payload).into_owned(),
        ));
    }

    let (outgoing, mut to_host) = mpsc::unbounded_channel::<Outgoing>();
    let (to_viewer, events) = mpsc::unbounded_channel();
    let (ack_tx, acks) = mpsc::unbounded_channel();
    let open = take_host_frame(kind, payload, &to_viewer, &ack_tx);
    tokio::spawn(async move {
        if open {
            loop {
                tokio::select! {
                    frame = rx.frame() => {
                        let Ok(Some(frame)) = frame else { break };
                        let Some((&kind, payload)) = frame.split_first() else { break };
                        if !take_host_frame(kind, payload, &to_viewer, &ack_tx) {
                            break;
                        }
                    }
                    out = to_host.recv() => {
                        let sent = match out {
                            Some(Outgoing::Input { seq, bytes }) => {
                                let mut payload = Vec::with_capacity(bytes.len().saturating_add(8));
                                payload.extend_from_slice(&seq.to_be_bytes());
                                payload.extend_from_slice(&bytes);
                                send(&tx, INPUT, &payload)
                            }
                            Some(Outgoing::Resize { cols, rows }) => {
                                let mut payload = cols.to_be_bytes().to_vec();
                                payload.extend_from_slice(&rows.to_be_bytes());
                                send(&tx, RESIZE, &payload)
                            }
                            None => break,
                        };
                        if !sent {
                            break;
                        }
                    }
                    () = to_viewer.closed() => break,
                }
            }
        }
        rx.stop(close::NORMAL);
        tx.finish();
    });
    Ok(Attached {
        outgoing,
        events,
        acks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_read_back_whole_and_refuse_anything_short() {
        let input = TerminalCopy::Input {
            terminal: "term-1".into(),
            client: "viewer".into(),
            seq: 7,
            bytes: Bytes::from_static(b"ls\r"),
        };
        let data = TerminalCopy::Data {
            terminal: "term-1".into(),
            seq: 9,
            bytes: Bytes::from_static(b"ls\r\n"),
        };
        for (copy, carried) in [(input, 3), (data, 4)] {
            let wire = copy.encode();
            assert_eq!(TerminalCopy::decode(&wire), Some(copy));
            // Cut anywhere before its bytes, it reads as nothing.
            for cut in 0..wire.len() - carried {
                let short = wire.slice(..cut);
                if let Some(read) = TerminalCopy::decode(&short) {
                    panic!("a copy cut at {cut} read as {read:?}");
                }
            }
        }
        assert_eq!(TerminalCopy::decode(&Bytes::from_static(&[9, 0, 0])), None);
    }

    #[test]
    fn events_read_back_as_they_were_sent() {
        let data = TerminalEvent::Data {
            seq: 3,
            data: BASE64.encode(b"hi"),
        };
        let exit = TerminalEvent::Exit {
            seq: 4,
            exit_code: -2,
            signal: None,
        };
        for event in [data, exit] {
            let (kind, payload) = encode_event(&event);
            assert_eq!(decode_event(kind, &payload), Some(event));
        }
    }
}
