//! Live mode: what syncs behind [`crate::Client`].
//!
//! - registry: mirrored from one engine over a `registry` peer stream ([`onicode_sync::registry`]):
//!   the engine's rows come in, this device's writes go out, and the engine replicates them on;
//!   persisted in the docs store, sidebar pins reconciled once caught up. Presence is the tailnet:
//!   an engine is online while the client holds a live link to it or the directory's last probe
//!   found it.
//! - rooms ([`room`]): each open chat follows its host over a `doc/{chatId}` peer stream;
//!   [`room::spawn_owed`] reopens chats that still owe their host after a restart;
//! - hosts ([`hosts`]): host RPCs, chunked uploads / attachment reads, PR-status watches;
//! - escorts ([`escort`]): `pending://` attachment bytes pushed to the host.
//!
//! Every loop here retries forever with bounded backoff that link events (the links set, a
//! path change, the foreground) cut short.

pub(crate) mod escort;
pub(crate) mod hosts;
pub(crate) mod room;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use onicode_doc::{REGISTRY_DOC_ID, RegistryDoc, RegistryRow, Vv};
use onicode_net::{Role, StreamHead, StreamKind};
use onicode_peer::PeerLinks;
use onicode_sync::DocsStore;
use onicode_sync::registry::{self, End, Replica};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::client::ClientInner;
use crate::error::{ClientError, Result};
use crate::{lock, now_ms};

/// Registry snapshot debounce.
const REGISTRY_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
/// How long opening the registry stream may take before the engine counts as away.
const REGISTRY_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// The first wait before mirroring again; doubles to [`REGISTRY_RETRY_CAP`].
const REGISTRY_RETRY_BASE: Duration = Duration::from_millis(500);
const REGISTRY_RETRY_CAP: Duration = Duration::from_secs(16);

/// Sleep `wait` (jittered), cut short when the links change (set, a new network path, the
/// foreground). `false` when cancelled.
pub(crate) async fn wait_backoff(
    cancel: &CancellationToken,
    links_wake: &Notify,
    wait: Duration,
) -> bool {
    let jitter = Duration::from_millis((now_ms().unsigned_abs() % 250) + 1);
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(wait + jitter) => true,
        () = links_wake.notified() => true,
    }
}

/// Raw registry posture sampled by the connectivity recompute.
#[derive(Debug, Clone, Default)]
pub(crate) struct RegistryPosture {
    pub connected: bool,
    pub synced: bool,
    pub retry_at_ms: Option<i64>,
    pub last_failure: Option<String>,
}

/// Where the registry mirror stands.
#[derive(Default)]
struct RegistryState {
    /// A session with the mirrored engine is running.
    connected: AtomicBool,
    /// An engine's whole registry has been merged this process.
    synced: AtomicBool,
    /// When the next attempt is due, while no engine answers.
    retry_at_ms: AtomicI64,
    failure: Mutex<Option<String>>,
}

/// The client's registry as one side of a `registry` session with the engine it mirrors.
struct ClientRegistry {
    inner: Weak<ClientInner>,
    doc: Arc<Mutex<RegistryDoc>>,
    changes: Arc<watch::Sender<u64>>,
    state: Arc<RegistryState>,
}

impl Replica for ClientRegistry {
    fn read<T>(&self, f: impl FnOnce(&RegistryDoc) -> T) -> T {
        f(&lock(&self.doc))
    }

    fn merge(&self, rows: &[RegistryRow], vv: Option<&Vv>) {
        let changed = {
            let mut doc = lock(&self.doc);
            let before = doc.vv().clone();
            doc.merge_remote(rows, vv) || *doc.vv() != before
        };
        if !changed {
            return;
        }
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
        // A raised vv alone must still reach the snapshot.
        if let Some(inner) = self.inner.upgrade() {
            inner.registry_changed();
        }
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn caught_up(&self) {
        self.state.synced.store(true, Ordering::Release);
        if let Some(inner) = self.inner.upgrade() {
            inner.registry_changed();
        }
    }
}

/// The engine to mirror: one with a live link, else one the directory found.
fn mirror(links: &PeerLinks) -> Option<String> {
    links
        .links()
        .into_iter()
        .find(|link| link.peer.role == Role::Engine)
        .map(|link| link.device)
        .or_else(|| {
            let mut peers = links.directory().peers();
            peers.sort_by(|a, b| a.device_id.cmp(&b.device_id));
            peers.into_iter().next().map(|peer| peer.device_id)
        })
}

pub(crate) struct LiveBackend {
    pub(crate) store: Arc<DocsStore>,
    inner: Weak<ClientInner>,
    registry: Arc<RegistryState>,
    /// Ticks on every local registry write: the mirror session pushes it.
    registry_changes: Arc<watch::Sender<u64>>,
    /// Cuts the mirror's backoff short.
    registry_kick: Arc<Notify>,
    registry_save: Arc<Notify>,
    registry_save_gate: Mutex<()>,
    registry_dirty: Arc<AtomicBool>,
    pub(crate) hosts: hosts::Hosts,
    pub(crate) escorts: escort::Escorts,
    /// Rescans the chats that owe their host something ([`room::spawn_owed`]).
    pub(crate) owed_wake: Arc<Notify>,
    cancel: CancellationToken,
}

impl LiveBackend {
    /// Open the docs store and restore the registry replica (instant, local).
    pub(crate) fn open(
        data_dir: &std::path::Path,
        device_id: &str,
    ) -> Result<(DocsStore, RegistryDoc)> {
        std::fs::create_dir_all(data_dir).map_err(|e| ClientError::Storage(e.to_string()))?;
        let store = DocsStore::open(data_dir).map_err(|e| ClientError::Storage(e.to_string()))?;
        let registry = match store.load_snapshot(REGISTRY_DOC_ID) {
            Ok(Some(bytes)) => RegistryDoc::from_bytes(&bytes, device_id).unwrap_or_else(|err| {
                tracing::warn!(error = %err, "registry snapshot unreadable; starting fresh");
                RegistryDoc::new(device_id.to_owned())
            }),
            _ => RegistryDoc::new(device_id.to_owned()),
        };
        Ok((store, registry))
    }

    pub(crate) fn new(inner: &Arc<ClientInner>, store: DocsStore) -> Self {
        Self {
            hosts: hosts::Hosts::new(inner),
            escorts: escort::Escorts::new(&inner.config.data_dir),
            owed_wake: Arc::new(Notify::new()),
            store: Arc::new(store),
            inner: Arc::downgrade(inner),
            registry: Arc::default(),
            registry_changes: Arc::new(watch::channel(0).0),
            registry_kick: Arc::new(Notify::new()),
            registry_save: Arc::new(Notify::new()),
            registry_save_gate: Mutex::new(()),
            registry_dirty: Arc::new(AtomicBool::new(false)),
            cancel: inner.cancel.child_token(),
        }
    }

    /// Spawn the registry mirror, the registry saver, the presence watch, the escorts and the
    /// owed-chat scan.
    pub(crate) fn start(&self, inner: &Arc<ClientInner>) {
        self.spawn_registry(inner);
        self.spawn_presence(inner);
        self.spawn_registry_saver(inner);
        self.escorts.respawn(inner);
        room::spawn_owed(inner, self.owed_wake.clone(), self.cancel.clone());
    }

    pub(crate) fn stop(&self) {
        self.cancel.cancel();
        self.hosts.shutdown();
        self.flush_registry_now_with(None);
    }

    pub(crate) fn registry_posture(&self) -> RegistryPosture {
        let at = self.registry.retry_at_ms.load(Ordering::Acquire);
        RegistryPosture {
            connected: self.registry.connected.load(Ordering::Acquire),
            synced: self.registry.synced.load(Ordering::Acquire),
            retry_at_ms: (at > 0).then_some(at),
            last_failure: lock(&self.registry.failure).clone(),
        }
    }

    /// The devices online now: those the client holds a live link to, and the engines the
    /// directory's last probe found. `None` before the links are set.
    pub(crate) fn presence(&self) -> Option<HashMap<String, i64>> {
        let inner = self.inner.upgrade()?;
        let links = inner.peer_links.get()?;
        let now = now_ms();
        let linked = links.links().into_iter().map(|link| link.device);
        let found = links
            .directory()
            .peers()
            .into_iter()
            .map(|peer| peer.device_id);
        Some(linked.chain(found).map(|device| (device, now)).collect())
    }

    /// Local registry write landed: push now, persist soon.
    pub(crate) fn registry_written(&self) {
        self.registry_changes
            .send_modify(|n| *n = n.wrapping_add(1));
        self.mark_registry_dirty();
    }

    pub(crate) fn mark_registry_dirty(&self) {
        self.registry_dirty.store(true, Ordering::Release);
        self.registry_save.notify_one();
    }

    /// Foreground / network restored: mirror the registry again now.
    pub(crate) fn kick(&self) {
        self.registry_kick.notify_one();
        self.owed_wake.notify_one();
    }

    fn flush_registry_now_with(&self, inner: Option<&ClientInner>) {
        if !self.registry_dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(inner) = inner else { return };
        if let Err(err) = self.persist_registry(inner) {
            self.registry_dirty.store(true, Ordering::Release);
            tracing::warn!(error = %err, "registry snapshot save failed");
        }
    }

    /// Serialize export and save, so a debounced older export cannot overwrite
    /// a new chat row persisted before its first command.
    pub(crate) fn persist_registry(&self, inner: &ClientInner) -> Result<()> {
        let _gate = lock(&self.registry_save_gate);
        let bytes = inner.workspace.mutate(|doc| doc.to_bytes())?;
        self.store
            .save_snapshot(REGISTRY_DOC_ID, &bytes)
            .map_err(|err| ClientError::Storage(err.to_string()))
    }

    /// Persist the registry now (backgrounding).
    pub(crate) fn flush_registry(&self, inner: &ClientInner) {
        self.flush_registry_now_with(Some(inner));
    }

    fn spawn_registry_saver(&self, inner: &Arc<ClientInner>) {
        let weak = Arc::downgrade(inner);
        let cancel = self.cancel.clone();
        let wake = self.registry_save.clone();
        let dirty = self.registry_dirty.clone();
        crate::runtime::spawn(async move {
            loop {
                // Register before checking so a notify can't slip by.
                let wait = wake.notified();
                if !dirty.load(Ordering::Acquire) {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = wait => {}
                    }
                }
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = tokio::time::sleep(REGISTRY_SAVE_DEBOUNCE) => {}
                }
                let Some(inner) = weak.upgrade() else { return };
                let weak_inner = Arc::downgrade(&inner);
                drop(inner);
                let _ = tokio::task::spawn_blocking(move || {
                    if let Some(inner) = weak_inner.upgrade()
                        && let Some(live) = inner.live()
                    {
                        live.flush_registry(&inner);
                    }
                })
                .await;
                if cancel.is_cancelled() {
                    return;
                }
            }
        });
    }

    /// Republishes presence whenever a link or the directory changes.
    fn spawn_presence(&self, inner: &Arc<ClientInner>) {
        let weak = Arc::downgrade(inner);
        let cancel = self.cancel.clone();
        let links_set = inner.links_wake.clone();
        crate::runtime::spawn(async move {
            let links = loop {
                let set = links_set.notified();
                let Some(inner) = weak.upgrade() else { return };
                if let Some(links) = inner.peer_links.get() {
                    break links.clone();
                }
                drop(inner);
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = set => {}
                }
            };
            let mut link_changes = links.subscribe_changes();
            let mut found = links.directory().subscribe();
            drop(links);
            loop {
                let Some(inner) = weak.upgrade() else { return };
                inner.presence_changed();
                drop(inner);
                tokio::select! {
                    () = cancel.cancelled() => return,
                    changed = link_changes.changed() => if changed.is_err() { return },
                    changed = found.changed() => if changed.is_err() { return },
                }
            }
        });
    }

    /// Mirrors the registry from one engine for the client's whole life: one session at a time,
    /// opened again (on another engine if need be) whenever it ends.
    fn spawn_registry(&self, inner: &Arc<ClientInner>) {
        let replica = ClientRegistry {
            inner: Arc::downgrade(inner),
            doc: inner.workspace.doc().clone(),
            changes: self.registry_changes.clone(),
            state: self.registry.clone(),
        };
        let weak = Arc::downgrade(inner);
        let cancel = self.cancel.clone();
        let kick = self.registry_kick.clone();
        let links_set = inner.links_wake.clone();
        crate::runtime::spawn(async move {
            let state = replica.state.clone();
            let head = StreamHead::new(StreamKind::Registry, "");
            let mut retry = REGISTRY_RETRY_BASE;
            loop {
                let woken = links_set.notified();
                let kicked = kick.notified();
                let Some(inner) = weak.upgrade() else { return };
                let links = inner.peer_links.get().cloned();
                drop(inner);
                let mut link_changes = links.as_ref().map(|links| links.subscribe_changes());
                let target = links.as_ref().and_then(|links| mirror(links));
                let failure = match (links, target) {
                    (Some(links), Some(engine)) => {
                        match tokio::time::timeout(
                            REGISTRY_OPEN_TIMEOUT,
                            links.open(&engine, &head),
                        )
                        .await
                        {
                            Ok(Ok((tx, rx))) => {
                                state.connected.store(true, Ordering::Release);
                                state.retry_at_ms.store(0, Ordering::Release);
                                *lock(&state.failure) = None;
                                connectivity_changed(&weak);
                                let end = tokio::select! {
                                    () = cancel.cancelled() => return,
                                    end = registry::run(&replica, tx, rx) => end,
                                };
                                state.connected.store(false, Ordering::Release);
                                match end {
                                    Ok(End::Gone) => return,
                                    Ok(End::Closed) => {
                                        retry = REGISTRY_RETRY_BASE;
                                        None
                                    }
                                    Err(error) => Some(error.to_string()),
                                }
                            }
                            Ok(Err(error)) => Some(error.to_string()),
                            Err(_elapsed) => Some(format!("{engine} did not answer")),
                        }
                    }
                    (Some(_), None) => Some("no engine found on the tailnet yet".to_owned()),
                    // Not on the tailnet yet: nothing to retry until the links are set.
                    (None, _) => None,
                };
                if let Some(failure) = failure {
                    *lock(&state.failure) = Some(failure);
                    let wait = i64::try_from(retry.as_millis()).unwrap_or(i64::MAX);
                    state
                        .retry_at_ms
                        .store(now_ms().saturating_add(wait), Ordering::Release);
                }
                connectivity_changed(&weak);
                let changed = async {
                    match link_changes.as_mut() {
                        Some(changes) => {
                            let _ = changes.changed().await;
                        }
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(retry) => {}
                    () = woken => retry = REGISTRY_RETRY_BASE,
                    () = kicked => retry = REGISTRY_RETRY_BASE,
                    () = changed => {}
                }
                retry = (retry * 2).min(REGISTRY_RETRY_CAP);
            }
        });
    }
}

fn connectivity_changed(weak: &Weak<ClientInner>) {
    if let Some(inner) = weak.upgrade() {
        inner.connectivity_changed();
    }
}

/// Minimal standard base64 (chunked uploads / attachment reads).
pub(crate) mod b64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(crate) fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    pub(crate) fn decode(input: &str) -> Option<Vec<u8>> {
        fn value(c: u8) -> Option<u32> {
            Some(match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a' + 26) as u32,
                b'0'..=b'9' => (c - b'0' + 52) as u32,
                b'+' | b'-' => 62,
                b'/' | b'_' => 63,
                _ => return None,
            })
        }
        let mut out = Vec::with_capacity(input.len() / 4 * 3);
        let mut buffer = 0u32;
        let mut bits = 0u32;
        for &c in input.as_bytes() {
            if c == b'=' {
                break;
            }
            if c.is_ascii_whitespace() {
                continue;
            }
            buffer = (buffer << 6) | value(c)?;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        Some(out)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn round_trips() {
            for len in 0..20 {
                let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
                assert_eq!(super::decode(&super::encode(&bytes)).unwrap(), bytes);
            }
            assert_eq!(super::encode(b"hello"), "aGVsbG8=");
        }
    }
}
