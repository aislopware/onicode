//! The registry between engines, and to the thin clients that mirror one: one `registry` stream
//! (kind [`StreamKind::Registry`]) per pair of devices, synced by per-origin version vectors
//! ([`onicode_sync::registry`]).
//!
//! - Serving: any peer may open one; the session runs until the stream ends.
//! - Keeping: this engine dials every engine the tailnet directory finds, and opens the stream on
//!   each engine link it dialled, so each pair of engines runs exactly one session (a pair's
//!   duplicate links resolve to one dialler). A session that ends on a live link is opened again on
//!   the next change, or after [`RETRY`].

use std::collections::HashMap;
use std::time::Duration;

use onicode_net::{Role, StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_peer::{LinkState, StreamHandler};
use onicode_sync::registry;
use tokio::task::JoinHandle;

use crate::workspace_host::{WeakWorkspaceHost, WorkspaceHost};

/// How long a session that ended on a live link waits before it is opened again, when nothing
/// else changes first.
const RETRY: Duration = Duration::from_secs(30);

/// Serves the `registry` streams peers open (see the module docs).
pub(crate) struct RegistryServer(pub(crate) WeakWorkspaceHost);

impl StreamHandler for RegistryServer {
    fn stream(&self, device: &str, _head: StreamHead, tx: StreamTx, rx: StreamRx) {
        match self.0.upgrade() {
            Some(host) if !host.local_only() => {
                tokio::spawn(session(host, device.to_owned(), tx, rx));
            }
            _ => tx.reset(close::NOT_GRANTED),
        }
    }
}

/// Runs one session with `device` to its end.
async fn session(host: WorkspaceHost, device: String, tx: StreamTx, rx: StreamRx) {
    let replica = host.replica_for(&device);
    // The replica holds the registry; the host handle is not needed past this.
    drop(host);
    match registry::run(&replica, tx, rx).await {
        Ok(end) => tracing::debug!(%device, ?end, "registry session ended"),
        Err(error) => tracing::debug!(%device, %error, "registry session failed"),
    }
}

/// Keeps a session open with every engine (see the module docs). Ends with the host or its
/// links.
pub(crate) async fn keep(weak: WeakWorkspaceHost) {
    let Some(links) = weak.upgrade().and_then(|host| host.links()) else {
        return;
    };
    let directory = links.directory().clone();
    let mut link_changes = links.subscribe_changes();
    let mut found = directory.subscribe();
    // device → (link id, the session on it)
    let mut sessions: HashMap<String, (usize, JoinHandle<()>)> = HashMap::new();
    loop {
        let Some(host) = weak.upgrade() else { return };
        let own = host.device_id().to_owned();
        drop(host);
        for peer in directory.peers() {
            if peer.device_id != own
                && peer.hello.role == Role::Engine
                && links.state(&peer.device_id) == LinkState::Idle
            {
                let links = links.clone();
                tokio::spawn(async move {
                    // The dialler keeps redialling the link from here on.
                    if let Err(error) = links.ready(&peer.device_id).await {
                        tracing::debug!(device = %peer.device_id, %error, "registry: dial failed");
                    }
                });
            }
        }
        let live = links.links();
        sessions.retain(|device, (id, task)| {
            let kept = live
                .iter()
                .any(|link| link.device == *device && link.id == *id);
            if !kept {
                task.abort();
            }
            kept && !task.is_finished()
        });
        for link in live {
            if !link.dialed || link.peer.role != Role::Engine || sessions.contains_key(&link.device)
            {
                continue;
            }
            let Some(host) = weak.upgrade() else { return };
            let opener = links.clone();
            let device = link.device.clone();
            let task = tokio::spawn(async move {
                let head = StreamHead::new(StreamKind::Registry, String::new());
                match opener.open(&device, &head).await {
                    Ok((tx, rx)) => session(host, device, tx, rx).await,
                    Err(error) => tracing::debug!(%device, %error, "registry: open failed"),
                }
            });
            sessions.insert(link.device, (link.id, task));
        }
        tokio::select! {
            changed = link_changes.changed() => if changed.is_err() { break },
            changed = found.changed() => if changed.is_err() { break },
            () = tokio::time::sleep(RETRY) => {}
        }
    }
    for (_, task) in sessions.into_values() {
        task.abort();
    }
}
