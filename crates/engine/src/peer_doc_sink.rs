//! An open chat as one side of a `doc/{chatId}` session ([`onicode_sync::peer_doc`]): imports land
//! through the handle's transcript lock, so the change plumbing (transcript watch, command drain,
//! snapshot debounce) treats them like any other change, and acks and snapshots go back to the
//! doc host, which owns the outbox and the forward jobs.

use std::sync::{Arc, Weak};

use loro::{ImportStatus, LoroDoc, VersionVector};
use onicode_sync::peer_doc::Replica;
use tokio::sync::watch;

use crate::doc_host::{ChatDocHandle, WeakDocHost};

/// One chat, synced with one peer device.
pub(crate) struct ChatReplica {
    host: WeakDocHost,
    /// Weak, so a session never keeps an evicted chat open.
    handle: Weak<ChatDocHandle>,
    /// The device on the other end.
    peer: String,
}

impl ChatReplica {
    pub(crate) fn new(host: WeakDocHost, handle: &Arc<ChatDocHandle>, peer: &str) -> Self {
        Self {
            host,
            handle: Arc::downgrade(handle),
            peer: peer.to_owned(),
        }
    }
}

impl Replica for ChatReplica {
    fn doc(&self) -> Option<LoroDoc> {
        self.handle
            .upgrade()
            .map(|handle| handle.doc().doc().clone())
    }

    fn changes(&self) -> watch::Receiver<u64> {
        match self.handle.upgrade() {
            Some(handle) => handle.subscribe_changes(),
            // Already closed: the receiver's sender is gone, so the session ends as `Gone`.
            None => watch::channel(0).1,
        }
    }

    fn import(&self, bytes: &[u8], catch_up: bool) -> Result<ImportStatus, String> {
        let handle = self.handle.upgrade().ok_or("the chat closed")?;
        // The handshake's batch is history the peer already had; live batches are news.
        let origin = if catch_up {
            crate::transcript_history::REPLAY_ORIGIN
        } else {
            ""
        };
        let status = handle
            .import_transcript(|| handle.doc().doc().import_with(bytes, origin))
            .map_err(|e| e.to_string())?;
        if let Some(host) = self.host.upgrade() {
            host.imported_from(&handle, &self.peer, &status);
        }
        Ok(status)
    }

    fn adopt(&self, snapshot: &[u8]) -> Result<(), String> {
        let handle = self.handle.upgrade().ok_or("the chat closed")?;
        let host = self.host.upgrade().ok_or("the engine is shutting down")?;
        host.adopt_snapshot(&handle, snapshot)
    }

    fn acked(&self, peer: &str, vv: &VersionVector) {
        if let (Some(host), Some(handle)) = (self.host.upgrade(), self.handle.upgrade()) {
            host.peer_acked(&handle, peer, vv);
        }
    }
}
