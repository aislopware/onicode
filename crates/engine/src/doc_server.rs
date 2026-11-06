//! Session docs between engines, and to the thin clients that drive one: a `doc/{chatId}` stream
//! (kind [`StreamKind::Doc`], named by the chat) per chat and pair of devices, synced by Loro
//! version vectors ([`onicode_sync::peer_doc`]).
//!
//! - Serving: a peer may open a stream for any chat this engine's registry has a row for; the
//!   engine opens the chat and answers. A chat it has no row for is reset with [`NOT_HELD`]: it
//!   cannot tell who hosts it, so it must not take commands for it.
//! - Following: while a chat another engine hosts is open here, a live session to the host keeps
//!   the two in step. While the host is away, whatever this replica owes it is handed one-shot to
//!   the other engines it can reach, which hold it as mailboxes.
//! - Forwarding: when links change, chats that owe a now-live host something (an unacked outbox row
//!   or a [`FORWARD`] job) are opened, so following delivers it.

use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::time::Duration;

use onicode_net::{Role, StreamHead, StreamKind, StreamRx, StreamTx};
use onicode_peer::{PeerLinks, StreamHandler};
use onicode_sync::FORWARD;
use onicode_sync::peer_doc::{self, End, NOT_HELD};

use crate::doc_host::{ChatDocHandle, ChatHost, DocHost, WeakDocHost};
use crate::peer_doc_sink::ChatReplica;

/// How long opening a stream to a peer may take before it counts as away.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// The first wait before following a host again; doubles to [`RETRY_CAP`].
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_CAP: Duration = Duration::from_secs(30);

/// Serves the doc streams peers open (see the module docs).
pub(crate) struct DocServer(pub(crate) WeakDocHost);

impl StreamHandler for DocServer {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        let Some(host) = self.0.upgrade() else {
            tx.reset(NOT_HELD);
            return;
        };
        let task = serve(host.clone(), device.to_owned(), head.name, tx, rx);
        host.spawn_worker(task);
    }
}

async fn serve(host: DocHost, device: String, chat_id: String, tx: StreamTx, rx: StreamRx) {
    let known = host
        .workspace()
        .is_none_or(|ws| ws.chat(&chat_id).ok().flatten().is_some());
    if chat_id.is_empty() || !known {
        tx.reset(NOT_HELD);
        return;
    }
    let handle = match host.open(&chat_id) {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(chat = %chat_id, %device, %error, "doc stream: open failed");
            tx.reset(NOT_HELD);
            return;
        }
    };
    let replica = ChatReplica::new(host.downgrade(), &handle, &device);
    // Held for the session: a peer following this chat keeps it open here.
    let end = peer_doc::serve(&replica, &device, tx, rx).await;
    drop(handle);
    match end {
        Ok(End::Adopted) => reopen(&host, &chat_id),
        Ok(_) => {}
        Err(error) => tracing::debug!(chat = %chat_id, %device, %error, "doc stream ended"),
    }
}

/// The chat's doc was replaced by a snapshot: open the new one, which follows on its own.
fn reopen(host: &DocHost, chat_id: &str) {
    if let Err(error) = host.open(chat_id) {
        tracing::warn!(chat = %chat_id, %error, "reopen after adopting a snapshot failed");
    }
}

/// Clears the handle's following mark when the follower stops, whatever the reason.
struct Following(Weak<ChatDocHandle>);

impl Drop for Following {
    fn drop(&mut self) {
        if let Some(handle) = self.0.upgrade() {
            handle.set_following(false);
        }
    }
}

/// Keeps one open chat in step with its host while it stays open (see the module docs).
pub(crate) async fn follow(weak: WeakDocHost, handle: Weak<ChatDocHandle>) {
    let _following = Following(handle.clone());
    let mut retry = RETRY_BASE;
    loop {
        let Some(host) = weak.upgrade() else { return };
        let Some(links) = host.links() else { return };
        let Some(open) = handle.upgrade() else { return };
        let chat_id = open.chat_id().to_owned();
        let kick = open.kick();
        let mut link_changes = links.subscribe_changes();
        let mut chats = host.workspace().map(|ws| ws.watch_chats());
        let chat_host = host.chat_host(&chat_id);
        drop(open);
        drop(host);
        match chat_host {
            // Others come to the host.
            ChatHost::Here => return,
            // Nothing names a host yet: wait for the registry.
            ChatHost::Unknown => {}
            ChatHost::Peer(target) => {
                let head = StreamHead::new(StreamKind::Doc, chat_id.clone());
                match tokio::time::timeout(OPEN_TIMEOUT, links.open(&target, &head)).await {
                    Ok(Ok((tx, rx))) => {
                        let Some(open) = handle.upgrade() else { return };
                        let replica = ChatReplica::new(weak.clone(), &open, &target);
                        open.set_host_live(true);
                        drop(open);
                        let end = peer_doc::open(&replica, &target, true, tx, rx).await;
                        if let Some(open) = handle.upgrade() {
                            open.set_host_live(false);
                        }
                        match end {
                            Ok(End::Adopted) => {
                                if let Some(host) = weak.upgrade() {
                                    reopen(&host, &chat_id);
                                }
                                return;
                            }
                            Ok(End::Gone) => return,
                            // The link went, or the host closed: start over when things change.
                            Ok(End::Closed | End::Done) => retry = RETRY_BASE,
                            Err(error) => {
                                tracing::debug!(chat = %chat_id, host = %target, %error,
                                    "following the host stopped");
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(chat = %chat_id, host = %target, %error, "host unreachable");
                        if mailboxes(&weak, &handle, &links, &target, &chat_id).await {
                            return;
                        }
                    }
                    Err(_elapsed) => {
                        if mailboxes(&weak, &handle, &links, &target, &chat_id).await {
                            return;
                        }
                    }
                }
            }
        }
        let chats_changed = async {
            match chats.as_mut() {
                Some(chats) => {
                    let _ = chats.changed().await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = tokio::time::sleep(retry) => {}
            _ = link_changes.changed() => {}
            () = kick.notified() => retry = RETRY_BASE,
            () = chats_changed => {}
        }
        retry = (retry * 2).min(RETRY_CAP);
    }
}

/// Hands what this replica owes its away host to every other engine it reaches, one-shot;
/// `true` once the chat was replaced by an adopted snapshot (and reopened).
async fn mailboxes(
    weak: &WeakDocHost,
    handle: &Weak<ChatDocHandle>,
    links: &Arc<PeerLinks>,
    target: &str,
    chat_id: &str,
) -> bool {
    let Some(host) = weak.upgrade() else {
        return false;
    };
    if !host.owes_host(chat_id) {
        return false;
    }
    let own = host.device_id().to_owned();
    drop(host);
    let head = StreamHead::new(StreamKind::Doc, chat_id.to_owned());
    let peers: Vec<String> = links
        .links()
        .into_iter()
        .filter(|link| {
            link.peer.role == Role::Engine && link.device != target && link.device != own
        })
        .map(|link| link.device)
        .collect();
    for peer in peers {
        let Ok(Ok((tx, rx))) = tokio::time::timeout(OPEN_TIMEOUT, links.open(&peer, &head)).await
        else {
            continue;
        };
        let Some(open) = handle.upgrade() else {
            return false;
        };
        let replica = ChatReplica::new(weak.clone(), &open, &peer);
        drop(open);
        match peer_doc::open(&replica, &peer, false, tx, rx).await {
            Ok(End::Adopted) => {
                if let Some(host) = weak.upgrade() {
                    reopen(&host, chat_id);
                }
                return true;
            }
            Ok(_) => tracing::debug!(chat = %chat_id, mailbox = %peer, "left with a mailbox"),
            Err(error) => {
                tracing::debug!(chat = %chat_id, mailbox = %peer, %error, "mailbox refused");
            }
        }
    }
    false
}

/// Whenever links change, opens the chats that owe a now-live host something (see the module
/// docs). Ends with the doc host or its links.
pub(crate) async fn forward(weak: WeakDocHost) {
    let Some(links) = weak.upgrade().and_then(|host| host.links()) else {
        return;
    };
    let mut changes = links.subscribe_changes();
    loop {
        let live: HashSet<String> = links.links().into_iter().map(|link| link.device).collect();
        if !live.is_empty() {
            let Some(host) = weak.upgrade() else { return };
            let mut after = String::new();
            loop {
                let page = match host.store().pending_forwards(&after, 64) {
                    Ok(page) => page,
                    Err(error) => {
                        tracing::warn!(%error, "pending forwards unreadable");
                        break;
                    }
                };
                let Some(last) = page.last().cloned() else {
                    break;
                };
                after = last;
                for chat_id in page {
                    let ChatHost::Peer(target) = host.chat_host(&chat_id) else {
                        continue;
                    };
                    if !live.contains(&target) {
                        continue;
                    }
                    match host.open(&chat_id) {
                        Ok(handle) => handle.kick().notify_one(),
                        Err(error) => {
                            tracing::warn!(chat = %chat_id, %error, kind = FORWARD,
                                "opening a chat to forward failed");
                        }
                    }
                }
            }
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}
