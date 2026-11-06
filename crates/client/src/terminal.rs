//! Terminals on a chat's host: the engine's PTY sessions, carried by the hub every viewer runs
//! (`onicode_term_link::remote`, the desktop engine's too) on the peer link to the host:
//!
//! - **One stream per terminal.** Its output and input ride a `terminal/{id}` stream on the link,
//!   with a datagram copy of each key and each small echo so a lost packet costs no round trip. The
//!   link pings at the interactive rate while a terminal is attached.
//! - **No keystroke lost or doubled.** Input is numbered and held until the host acknowledges it; a
//!   stream reopened after a dropped link replays what's pending and the host applies each number
//!   once. Writes never wait on the network.
//! - **The stream outlives the network.** When the link drops, the stream is reopened as it comes
//!   back, resuming after the last output shown: no gap, no repeat (the host keeps a replay window
//!   and the shell outlives its viewers).
//!
//! The demo host has no links: its terminals go through its own RPCs.

use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_proto::TerminalEvent;
use onicode_rpc::{RpcError, methods};
use onicode_term_link::remote::Write;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{Client, ClientInner};
use crate::error::{ClientError, Result};
use crate::live::b64;

/// An open terminal and where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    pub id: String,
    pub device_id: String,
    pub cwd: String,
    /// Shell basename (`zsh`, `bash`, …).
    pub shell: String,
    /// The host answers the terminal's queries (device attributes, cursor
    /// position…) itself. A host from before that leaves them to the
    /// viewer, which then writes its emulator's answers back.
    pub host_replies: bool,
}

/// The host takes at most this much input per write (`MAX_INPUT_BYTES`).
const MAX_WRITE: usize = 64 * 1024;

/// What a link reports as it runs.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalUpdate {
    /// The output stream is (re)attached.
    Connected,
    /// PTY output, raw bytes (not necessarily valid UTF-8).
    Output(Vec<u8>),
    /// The shell exited; the stream is over.
    Exited { code: i32, signal: Option<String> },
    /// The host can't be reached; the next attempt is `retry_in_ms` away
    /// (or sooner: the network coming back, the app back in front).
    Reconnecting { retry_in_ms: u64 },
    /// The host no longer has this terminal (it exited long ago, or the
    /// host restarted).
    Gone,
}

/// Receives a link's updates on the client runtime.
pub trait TerminalSink: Send + Sync {
    fn update(&self, update: TerminalUpdate);
}

/// Where a terminal's writes go.
#[derive(Clone)]
enum Route {
    /// The hub, over the peer link to the host.
    Hub,
    /// The demo host's RPCs, one at a time in typed order.
    Demo(mpsc::UnboundedSender<Write>),
}

impl Route {
    fn new(inner: &Arc<ClientInner>, info: &TerminalInfo) -> Self {
        if inner.is_demo() {
            Self::Demo(demo_writer(Arc::downgrade(inner), info.clone()))
        } else {
            Self::Hub
        }
    }
}

/// Forwards a demo terminal's writes to the demo host, in order.
fn demo_writer(weak: Weak<ClientInner>, info: TerminalInfo) -> mpsc::UnboundedSender<Write> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    crate::runtime::shared().spawn(async move {
        while let Some(write) = rx.recv().await {
            let Some(inner) = weak.upgrade() else { return };
            let (method, params) = match write {
                Write::Input(bytes) => (
                    methods::WRITE_TERMINAL,
                    json!({ "terminalId": info.id, "data": b64::encode(&bytes) }),
                ),
                Write::Resize { cols, rows } => (
                    methods::RESIZE_TERMINAL,
                    json!({ "terminalId": info.id, "cols": cols, "rows": rows }),
                ),
            };
            let _ = inner.host_rpc(&info.device_id, method, params).await;
        }
    });
    tx
}

/// Queue `write` for the host, in order, never waiting on the network. Without links yet
/// (the app still joining the tailnet) there is no host to take it, and it is dropped.
fn send(inner: &ClientInner, route: &Route, info: &TerminalInfo, write: Write) {
    match route {
        Route::Demo(queue) => {
            let _ = queue.send(write);
        }
        Route::Hub => {
            let Some(links) = inner.peer_links.get() else {
                return;
            };
            // The key's datagram copy is sent from a task.
            let _runtime = crate::runtime::handle().enter();
            let _ = inner
                .remote_terminals
                .send(links, &info.device_id, &info.id, write);
        }
    }
}

fn write(inner: &ClientInner, route: &Route, info: &TerminalInfo, bytes: &[u8]) {
    for chunk in bytes.chunks(MAX_WRITE).filter(|c| !c.is_empty()) {
        send(inner, route, info, Write::Input(chunk.to_vec()));
    }
}

/// One attached terminal. Dropping it detaches (the shell keeps running on
/// the host); [`TerminalLink::close`] ends it — the close goes out even if
/// the link is dropped right after.
pub struct TerminalLink {
    info: TerminalInfo,
    inner: Weak<ClientInner>,
    route: Route,
    stream: CancellationToken,
}

/// Writes the terminal's own query answers back (hosts without host
/// replies). Doesn't keep the link alive.
#[derive(Clone)]
pub struct TerminalReplies {
    info: TerminalInfo,
    inner: Weak<ClientInner>,
    route: Route,
}

impl TerminalReplies {
    pub fn write(&self, bytes: Vec<u8>) {
        if let Some(inner) = self.inner.upgrade() {
            write(&inner, &self.route, &self.info, &bytes);
        }
    }
}

impl TerminalLink {
    pub fn info(&self) -> &TerminalInfo {
        &self.info
    }

    /// Bytes for the shell (typed keys, a paste). Queued in order.
    pub fn write(&self, bytes: Vec<u8>) {
        if let Some(inner) = self.inner.upgrade() {
            write(&inner, &self.route, &self.info, &bytes);
        }
    }

    /// The viewport's grid size changed.
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(inner) = self.inner.upgrade() {
            send(
                &inner,
                &self.route,
                &self.info,
                Write::Resize {
                    cols: cols.max(2),
                    rows: rows.max(1),
                },
            );
        }
    }

    /// End the shell on the host. The stream reports the exit when it lands.
    pub fn close(&self) {
        let (info, weak) = (self.info.clone(), self.inner.clone());
        crate::runtime::shared().spawn(async move {
            let params = json!({ "terminalId": info.id });
            for attempt in 0..5u64 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_millis(300 * attempt)).await;
                }
                let Some(inner) = weak.upgrade() else { return };
                match inner
                    .host_rpc(&info.device_id, methods::CLOSE_TERMINAL, params.clone())
                    .await
                {
                    Ok(_) | Err(ClientError::HostError(_) | ClientError::Unsupported(_)) => break,
                    Err(_) => {}
                }
            }
            if let Some(inner) = weak.upgrade() {
                inner.remote_terminals.ended(&info.device_id, &info.id);
            }
        });
    }

    /// For answering the terminal's queries when the host doesn't.
    pub fn replies(&self) -> TerminalReplies {
        TerminalReplies {
            info: self.info.clone(),
            inner: self.inner.clone(),
            route: self.route.clone(),
        }
    }
}

impl Drop for TerminalLink {
    /// Detach: the output stream stops (the shell keeps running on the host).
    fn drop(&mut self) {
        self.stream.cancel();
    }
}

impl Client {
    /// Open a shell for `chat_id` on its host, in the chat's working folder.
    /// The link to the host is dialled alongside, so the terminal's stream
    /// usually finds it up.
    pub async fn open_terminal(&self, chat_id: &str, cols: u16, rows: u16) -> Result<TerminalInfo> {
        let device_id = self.chat_host(chat_id)?;
        self.inner.warm_link(&device_id);
        let reply = self
            .inner
            .host_rpc(
                &device_id,
                methods::OPEN_TERMINAL,
                json!({ "chatId": chat_id, "cols": cols.max(2), "rows": rows.max(1) }),
            )
            .await?;
        let session: onicode_proto::TerminalSession = serde_json::from_value(reply)
            .map_err(|e| ClientError::HostError(format!("OpenTerminal: {e}")))?;
        Ok(TerminalInfo {
            id: session.id,
            device_id,
            cwd: session.cwd,
            shell: session.shell,
            host_replies: session.host_replies,
        })
    }

    /// Attach to an open terminal: replays everything after `after_seq`
    /// (0 = the host's whole replay window), then follows live output.
    pub fn attach_terminal(
        &self,
        info: TerminalInfo,
        after_seq: u64,
        sink: Arc<dyn TerminalSink>,
    ) -> TerminalLink {
        let stream = CancellationToken::new();
        self.inner.warm_link(&info.device_id);
        crate::runtime::shared().spawn(follow(
            Arc::downgrade(&self.inner),
            info.clone(),
            after_seq,
            sink,
            stream.clone(),
        ));
        TerminalLink {
            route: Route::new(&self.inner, &info),
            info,
            inner: Arc::downgrade(&self.inner),
            stream,
        }
    }

    /// The device hosting `chat_id`.
    pub(crate) fn chat_host(&self, chat_id: &str) -> Result<String> {
        self.inner
            .workspace
            .chat(chat_id)
            .map(|c| c.device_id)
            .ok_or_else(|| ClientError::NotFound(chat_id.to_owned()))
    }
}

fn rpc_error(error: ClientError) -> RpcError {
    match error {
        ClientError::HostError(message) => RpcError::Failed(message),
        ClientError::Unsupported(message) => RpcError::UnknownMethod(message),
        other => RpcError::Transport(other.to_string()),
    }
}

/// The terminal's events after `after_seq`: the hub's stream to the host, or the demo host's.
async fn subscribe(
    inner: Arc<ClientInner>,
    info: &TerminalInfo,
    after_seq: u64,
) -> std::result::Result<BoxStream<'static, serde_json::Value>, RpcError> {
    if inner.is_demo() {
        let stream = inner
            .host_subscribe(
                &info.device_id,
                methods::SUBSCRIBE_TERMINAL,
                json!({ "terminalId": info.id, "afterSeq": after_seq }),
            )
            .await
            .map_err(rpc_error)?;
        return Ok(futures::stream::unfold(stream, |mut stream| async move {
            stream.recv().await.map(|item| (item, stream))
        })
        .boxed());
    }
    let links = inner
        .peer_links
        .get()
        .cloned()
        .ok_or_else(|| RpcError::Transport("not on the tailnet yet".into()))?;
    let hub = inner.remote_terminals.clone();
    drop(inner);
    hub.subscribe(links, &info.device_id, &info.id, after_seq)
        .await
}

/// Follow the terminal, resubscribing from the last sequence whenever the
/// hub gives up on the host (0.4s → 5s, cut short by the network coming
/// back or the app returning to the foreground).
async fn follow(
    weak: Weak<ClientInner>,
    info: TerminalInfo,
    mut last_seq: u64,
    sink: Arc<dyn TerminalSink>,
    cancel: CancellationToken,
) {
    let mut backoff = Duration::from_millis(400);
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let wake = inner.links_wake.clone();
        let subscribed = tokio::select! {
            () = cancel.cancelled() => return,
            subscribed = subscribe(inner, &info, last_seq) => subscribed,
        };
        match subscribed {
            Ok(mut events) => {
                sink.update(TerminalUpdate::Connected);
                loop {
                    let item = tokio::select! {
                        () = cancel.cancelled() => return,
                        item = events.next() => item,
                    };
                    let Some(item) = item else { break };
                    backoff = Duration::from_millis(400);
                    match serde_json::from_value::<TerminalEvent>(item) {
                        Ok(TerminalEvent::Data { seq, data }) => {
                            if seq <= last_seq {
                                continue;
                            }
                            last_seq = seq;
                            if let Some(bytes) = b64::decode(&data) {
                                sink.update(TerminalUpdate::Output(bytes));
                            }
                        }
                        Ok(TerminalEvent::Exit {
                            exit_code, signal, ..
                        }) => {
                            sink.update(TerminalUpdate::Exited {
                                code: exit_code,
                                signal,
                            });
                            return;
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(RpcError::Failed(message)) if message.contains("Terminal not found") => {
                sink.update(TerminalUpdate::Gone);
                return;
            }
            Err(RpcError::UnknownMethod(_)) => {
                sink.update(TerminalUpdate::Gone);
                return;
            }
            Err(_) => {}
        }
        sink.update(TerminalUpdate::Reconnecting {
            retry_in_ms: backoff.as_millis() as u64,
        });
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(Duration::from_secs(5)),
            () = wake.notified() => backoff = Duration::from_millis(400),
        }
    }
}
