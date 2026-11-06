//! One chat's replica on the thin client, synced with the chat's host over a `doc/{chatId}`
//! peer stream by Loro version vectors ([`onicode_sync::peer_doc`]), as the engines do:
//!
//! - every local write lands in the store's outbox before anything else sees it, and stays
//!   "sending" until the HOST acks a version that holds it;
//! - while the host is away, what the replica owes it is handed one-shot to the other engines the
//!   links reach, which hold it as mailboxes and forward it when the host is back;
//! - snapshots are saved debounced; a replica behind the host's shallow root adopts the host's
//!   snapshot and the session reopens on it;
//! - [`spawn_owed`] reopens, after a restart or a link change, every chat that still owes its host
//!   something, until the host has acked it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use loro::{ImportStatus, LoroDoc, VersionVector};
use onicode_doc::{SessionCommandStatus, SessionDoc};
use onicode_net::{Role, StreamHead, StreamKind};
use onicode_peer::PeerLinks;
use onicode_sync::peer_doc::{self, End, Replica};
use onicode_sync::{DocsStore, FORWARD};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::client::ClientInner;
use crate::lock;

/// Snapshot flush cadence while changes stream in.
const SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// How long opening a stream to a peer may take before it counts as away.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// The first wait before following the host again; doubles to [`RETRY_CAP`].
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_CAP: Duration = Duration::from_secs(30);
/// Chats [`spawn_owed`] keeps open at once.
const MAX_OWED: usize = 8;

/// Loaded local state for a chat (instant open).
pub(crate) struct LocalDoc {
    pub doc: SessionDoc,
    /// A snapshot or outbox existed: content is on screen immediately.
    pub had_content: bool,
}

/// A chat doc from disk: its snapshot plus the own writes no host has acked yet.
pub(crate) fn load_local(store: &DocsStore, chat_id: &str) -> LocalDoc {
    let raw = LoroDoc::new();
    let mut had_content = false;
    match store.load_snapshot(chat_id) {
        Ok(Some(bytes)) => match raw.import(&bytes) {
            Ok(_) => had_content = true,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "chat snapshot unreadable; starting fresh");
            }
        },
        Ok(None) => {}
        Err(err) => tracing::warn!(chat = %chat_id, error = %err, "chat snapshot load failed"),
    }
    match store.outbox(chat_id) {
        Ok(rows) => {
            for row in rows {
                if raw.import(&row.bytes).is_ok() {
                    had_content = true;
                }
            }
        }
        Err(err) => tracing::warn!(chat = %chat_id, error = %err, "chat outbox unreadable"),
    }
    LocalDoc {
        doc: SessionDoc::from_doc(raw),
        had_content,
    }
}

/// Debounced snapshot writer. Retired once the chat adopted a snapshot, so this copy never
/// overwrites it.
pub(crate) struct Persister {
    doc: Weak<SessionDoc>,
    store: Arc<DocsStore>,
    chat_id: String,
    generation: AtomicU64,
    saved: AtomicU64,
    urgent: AtomicBool,
    retired: AtomicBool,
    write: Mutex<()>,
    wake: Arc<Notify>,
}

impl Persister {
    fn new(doc: &Arc<SessionDoc>, store: Arc<DocsStore>, chat_id: &str) -> Arc<Self> {
        Arc::new(Self {
            doc: Arc::downgrade(doc),
            store,
            chat_id: chat_id.to_owned(),
            generation: AtomicU64::new(0),
            saved: AtomicU64::new(0),
            urgent: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            write: Mutex::new(()),
            wake: Arc::new(Notify::new()),
        })
    }

    fn start(self: &Arc<Self>, cancel: CancellationToken) {
        let weak = Arc::downgrade(self);
        let wake = self.wake.clone();
        crate::runtime::spawn(async move {
            loop {
                let notified = wake.notified();
                tokio::select! {
                    () = cancel.cancelled() => {
                        if let Some(this) = weak.upgrade() {
                            let _ = tokio::task::spawn_blocking(move || this.flush()).await;
                        }
                        return;
                    }
                    () = notified => {}
                }
                let Some(this) = weak.upgrade() else { return };
                if !this.urgent.swap(false, Ordering::AcqRel) {
                    drop(this);
                    tokio::time::sleep(SAVE_INTERVAL).await;
                }
                let Some(this) = weak.upgrade() else { return };
                let _ = tokio::task::spawn_blocking(move || this.flush()).await;
            }
        });
    }

    fn dirty(&self, urgent: bool) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if urgent {
            self.urgent.store(true, Ordering::Release);
        }
        self.wake.notify_one();
    }

    fn retire(&self) {
        let _serial = lock(&self.write);
        self.retired.store(true, Ordering::Release);
    }

    /// Write now if anything changed since the last save (blocking I/O).
    pub(crate) fn flush(&self) {
        let _serial = lock(&self.write);
        if self.retired.load(Ordering::Acquire) {
            return;
        }
        let generation = self.generation.load(Ordering::Acquire);
        if generation == self.saved.load(Ordering::Acquire) {
            return;
        }
        let Some(doc) = self.doc.upgrade() else {
            return;
        };
        let bytes = match doc.export_snapshot() {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "chat snapshot export failed");
                return;
            }
        };
        match self.store.save_snapshot(&self.chat_id, &bytes) {
            Ok(()) => self.saved.store(generation, Ordering::Release),
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "chat snapshot save failed");
            }
        }
    }
}

/// Connection posture (feeds connectivity and the composer strip).
#[derive(Default)]
pub(crate) struct RoomStatus {
    /// A live session with the host is running.
    pub connected: AtomicBool,
    /// The host's state has been merged at least once (hydration).
    pub caught_up: AtomicBool,
    /// When the next attempt to reach the host is due, while it is away.
    pub retry_at_ms: AtomicI64,
    pub last_failure: Mutex<Option<String>>,
}

pub(crate) struct RoomDeps {
    /// This device.
    pub device_id: String,
    pub store: Arc<DocsStore>,
    /// The chat's host as the registry has it now.
    pub host: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// The links to the user's engines, once the app has joined the tailnet.
    pub links: Arc<dyn Fn() -> Option<Arc<PeerLinks>> + Send + Sync>,
    /// Moves when the links are set, the path changes or the app comes to the foreground.
    pub wake: Arc<Notify>,
    /// A change landed from a peer (schedule a coalesced session refresh).
    pub on_applied: Arc<dyn Fn() + Send + Sync>,
    /// Connection posture or the outbox changed (connectivity, composer).
    pub on_status: Arc<dyn Fn() + Send + Sync>,
    /// The chat adopted a snapshot: this replica is stale; reopen it.
    pub on_adopted: Arc<dyn Fn() + Send + Sync>,
    /// The client's lifetime: shutdown stops the room even while a view holds its session.
    pub cancel: CancellationToken,
}

/// One chat's replica, following its host. Dropping it stops the session and flushes the doc.
pub(crate) struct Room {
    pub(crate) chat_id: String,
    pub(crate) status: Arc<RoomStatus>,
    pub(crate) persister: Arc<Persister>,
    store: Arc<DocsStore>,
    kick: Arc<Notify>,
    cancel: CancellationToken,
    _changes: Arc<watch::Sender<u64>>,
    _local_updates: loro::Subscription,
    _root: loro::Subscription,
}

impl Room {
    /// Start recording local writes and following the host.
    pub(crate) fn start(doc: &Arc<SessionDoc>, chat_id: &str, deps: RoomDeps) -> Arc<Self> {
        let persister = Persister::new(doc, deps.store.clone(), chat_id);
        let cancel = deps.cancel.child_token();
        persister.start(cancel.clone());

        let store = deps.store.clone();
        let chat = chat_id.to_owned();
        let on_status = deps.on_status.clone();
        let local_updates = doc
            .doc()
            .subscribe_local_update(Box::new(move |bytes: &Vec<u8>| {
                // The blob names its own end, so the doc is not re-entered from its hook.
                let enqueued = LoroDoc::decode_import_blob_meta(bytes, false)
                    .map_err(|e| e.to_string())
                    .and_then(|meta| {
                        store
                            .enqueue_outbox(&chat, bytes, &meta.partial_end_vv)
                            .map_err(|e| e.to_string())
                    });
                if let Err(error) = enqueued {
                    tracing::error!(%chat, %error, "outbox write failed");
                }
                on_status();
                true
            }));
        let changes = Arc::new(watch::channel(0u64).0);
        let tick = changes.clone();
        let dirty = persister.clone();
        let root = doc.doc().subscribe_root(Arc::new(move |_event| {
            dirty.dirty(false);
            tick.send_modify(|n| *n = n.wrapping_add(1));
        }));

        let room = Arc::new(Self {
            chat_id: chat_id.to_owned(),
            status: Arc::new(RoomStatus::default()),
            persister,
            store: deps.store.clone(),
            kick: Arc::new(Notify::new()),
            cancel,
            _changes: changes.clone(),
            _local_updates: local_updates,
            _root: root,
        });
        let replica = ViewerReplica {
            doc: Arc::downgrade(doc),
            chat_id: chat_id.to_owned(),
            device_id: deps.device_id.clone(),
            host: deps.host.clone(),
            store: deps.store.clone(),
            changes: Arc::downgrade(&changes),
            status: room.status.clone(),
            persister: room.persister.clone(),
            on_applied: deps.on_applied.clone(),
            on_status: deps.on_status.clone(),
        };
        crate::runtime::spawn(follow(
            Arc::downgrade(&room),
            replica,
            deps,
            room.kick.clone(),
            room.cancel.clone(),
        ));
        room
    }

    /// Liveness hint (view attached, foreground, retry delivery): reach the host now.
    pub(crate) fn kick(&self) {
        self.kick.notify_one();
    }

    pub(crate) fn connected(&self) -> bool {
        self.status.connected.load(Ordering::Acquire)
    }

    /// Has own writes the host has not acked (never evict the session meanwhile).
    pub(crate) fn has_pending_pushes(&self) -> bool {
        self.store.has_outbox(&self.chat_id).unwrap_or(true)
    }

    pub(crate) fn retry_at_ms(&self) -> Option<i64> {
        let at = self.status.retry_at_ms.load(Ordering::Acquire);
        (at > 0).then_some(at)
    }

    pub(crate) fn flush(&self) {
        self.persister.flush();
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.persister.flush();
    }
}

/// Follows the host while the room lives (see the module docs).
async fn follow(
    room: Weak<Room>,
    replica: ViewerReplica,
    deps: RoomDeps,
    kick: Arc<Notify>,
    cancel: CancellationToken,
) {
    let status = replica.status.clone();
    let head = StreamHead::new(StreamKind::Doc, replica.chat_id.clone());
    let mut retry = RETRY_BASE;
    loop {
        if room.strong_count() == 0 || cancel.is_cancelled() {
            return;
        }
        let wake = deps.wake.notified();
        let links = (deps.links)();
        let mut link_changes = links.as_ref().map(|links| links.subscribe_changes());
        if let (Some(links), Some(host)) = (links, (deps.host)()) {
            match tokio::time::timeout(OPEN_TIMEOUT, links.open(&host, &head)).await {
                Ok(Ok((tx, rx))) => {
                    status.retry_at_ms.store(0, Ordering::Release);
                    status.connected.store(true, Ordering::Release);
                    *lock(&status.last_failure) = None;
                    (deps.on_status)();
                    let end = tokio::select! {
                        () = cancel.cancelled() => return,
                        end = peer_doc::open(&replica, &host, true, tx, rx) => end,
                    };
                    status.connected.store(false, Ordering::Release);
                    (deps.on_status)();
                    match end {
                        Ok(End::Adopted) => {
                            (deps.on_adopted)();
                            return;
                        }
                        Ok(End::Gone) => return,
                        Ok(End::Closed | End::Done) => retry = RETRY_BASE,
                        Err(error) => *lock(&status.last_failure) = Some(error.to_string()),
                    }
                }
                Ok(Err(error)) => {
                    *lock(&status.last_failure) = Some(error.to_string());
                    if mailboxes(&replica, &links, &host).await {
                        (deps.on_adopted)();
                        return;
                    }
                }
                Err(_elapsed) => {
                    *lock(&status.last_failure) = Some("the host did not answer".into());
                    if mailboxes(&replica, &links, &host).await {
                        (deps.on_adopted)();
                        return;
                    }
                }
            }
        }
        status.retry_at_ms.store(
            crate::now_ms() + retry.as_millis() as i64,
            Ordering::Release,
        );
        (deps.on_status)();
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
            () = wake => retry = RETRY_BASE,
            () = kick.notified() => retry = RETRY_BASE,
            () = changed => {}
        }
        retry = (retry * 2).min(RETRY_CAP);
    }
}

/// Hands what this replica owes its away host to every other engine the links reach,
/// one-shot; `true` once the chat adopted a snapshot.
async fn mailboxes(replica: &ViewerReplica, links: &Arc<PeerLinks>, host: &str) -> bool {
    if !replica.owes_host() {
        return false;
    }
    let head = StreamHead::new(StreamKind::Doc, replica.chat_id.clone());
    let peers: Vec<String> = links
        .links()
        .into_iter()
        .filter(|link| link.peer.role == Role::Engine && link.device != host)
        .map(|link| link.device)
        .collect();
    for peer in peers {
        let Ok(Ok((tx, rx))) = tokio::time::timeout(OPEN_TIMEOUT, links.open(&peer, &head)).await
        else {
            continue;
        };
        match peer_doc::open(replica, &peer, false, tx, rx).await {
            Ok(End::Adopted) => return true,
            Ok(_) => {
                tracing::debug!(chat = %replica.chat_id, mailbox = %peer, "left with a mailbox")
            }
            Err(error) => {
                tracing::debug!(chat = %replica.chat_id, mailbox = %peer, %error, "mailbox refused");
            }
        }
    }
    false
}

/// The client's replica as one side of a `doc/{chatId}` session.
struct ViewerReplica {
    doc: Weak<SessionDoc>,
    chat_id: String,
    device_id: String,
    host: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    store: Arc<DocsStore>,
    changes: Weak<watch::Sender<u64>>,
    status: Arc<RoomStatus>,
    persister: Arc<Persister>,
    on_applied: Arc<dyn Fn() + Send + Sync>,
    on_status: Arc<dyn Fn() + Send + Sync>,
}

impl ViewerReplica {
    /// Unacked own writes, or a forward owed since adopting a snapshot.
    fn owes_host(&self) -> bool {
        self.store.has_outbox(&self.chat_id).unwrap_or(true)
            || !matches!(
                self.store.sync_job_version(&self.chat_id, FORWARD),
                Ok(None)
            )
    }

    fn hydrated(&self) {
        if !self.status.caught_up.swap(true, Ordering::AcqRel) {
            (self.on_applied)();
        }
    }
}

impl Replica for ViewerReplica {
    fn doc(&self) -> Option<LoroDoc> {
        self.doc.upgrade().map(|doc| doc.doc().clone())
    }

    fn changes(&self) -> watch::Receiver<u64> {
        match self.changes.upgrade() {
            Some(changes) => changes.subscribe(),
            // Already closed: the receiver's sender is gone, so the session ends as `Gone`.
            None => watch::channel(0).1,
        }
    }

    fn import(&self, bytes: &[u8], catch_up: bool) -> Result<ImportStatus, String> {
        let doc = self.doc.upgrade().ok_or("the chat closed")?;
        let status = doc.doc().import(bytes).map_err(|e| e.to_string())?;
        self.persister.dirty(catch_up);
        if catch_up {
            self.hydrated();
        }
        (self.on_applied)();
        Ok(status)
    }

    fn adopt(&self, snapshot: &[u8]) -> Result<(), String> {
        let doc = self.doc.upgrade().ok_or("the chat closed")?;
        let raw = LoroDoc::from_snapshot(snapshot).map_err(|e| e.to_string())?;
        for row in self
            .store
            .outbox(&self.chat_id)
            .map_err(|e| e.to_string())?
        {
            // Ops resting on history the snapshot trimmed cannot apply; the commands below
            // still carry the sends among them.
            if let Err(error) = raw.import(&row.bytes) {
                tracing::debug!(chat = %self.chat_id, %error, "outbox row does not apply to the adopted doc");
            }
        }
        let adopted = SessionDoc::from_doc(raw);
        let present: HashSet<String> = adopted
            .read_commands()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|command| command.id)
            .collect();
        for command in doc.read_commands().unwrap_or_default() {
            if command.status == SessionCommandStatus::Pending
                && command.issued_by == self.device_id
                && !present.contains(&command.id)
            {
                adopted.queue_command(&command).map_err(|e| e.to_string())?;
            }
        }
        let bytes = adopted.export_snapshot().map_err(|e| e.to_string())?;
        self.persister.retire();
        self.store
            .adopt_snapshot(&self.chat_id, &bytes)
            .map_err(|e| e.to_string())?;
        tracing::info!(chat = %self.chat_id, "adopted the host's snapshot");
        Ok(())
    }

    fn acked(&self, peer: &str, vv: &VersionVector) {
        self.hydrated();
        if (self.host)().as_deref() != Some(peer) {
            // A mailbox holding our writes is not the host executing them.
            return;
        }
        // Read the job first: a forward owed after this point keeps a newer version.
        let job = self.store.sync_job_version(&self.chat_id, FORWARD);
        if let Err(error) = self.store.retire_outbox(&self.chat_id, vv) {
            tracing::warn!(chat = %self.chat_id, %error, "outbox retire failed");
        }
        if let (Ok(Some(version)), Some(doc)) = (job, self.doc.upgrade())
            && vv.includes_vv(&doc.doc().oplog_vv())
            && let Err(error) = self
                .store
                .complete_sync_job(&self.chat_id, FORWARD, version)
        {
            tracing::warn!(chat = %self.chat_id, %error, "forward job completion failed");
        }
        (self.on_status)();
    }
}

/// Keeps open every chat that owes its host something (unacked writes, or a forward owed since
/// adopting a snapshot) until the host has acked it: after a restart nothing else would open
/// them. Rescans when `wake` moves, the links change, or every [`RETRY_CAP`].
pub(crate) fn spawn_owed(inner: &Arc<ClientInner>, wake: Arc<Notify>, cancel: CancellationToken) {
    let weak = Arc::downgrade(inner);
    crate::runtime::spawn(async move {
        let mut held: HashMap<String, crate::SessionHandle> = HashMap::new();
        loop {
            let notified = wake.notified();
            let Some(inner) = weak.upgrade() else { return };
            let Some(live) = inner.live() else { return };
            held.retain(|chat, session| {
                // A session dropped for adopting a snapshot is stale: let go, so it reopens.
                let current = inner
                    .session_core(chat)
                    .is_some_and(|open| Arc::ptr_eq(&open, &session.core));
                current
                    && (live.store.has_outbox(chat).unwrap_or(true)
                        || !matches!(live.store.sync_job_version(chat, FORWARD), Ok(None)))
            });
            let mut after = String::new();
            while held.len() < MAX_OWED {
                let page = match live.store.pending_forwards(&after, 64) {
                    Ok(page) => page,
                    Err(err) => {
                        tracing::warn!(%err, "owed chats unreadable");
                        break;
                    }
                };
                let Some(last) = page.last().cloned() else {
                    break;
                };
                after = last;
                for chat in page {
                    if held.len() >= MAX_OWED {
                        break;
                    }
                    if held.contains_key(&chat) || inner.workspace.chat(&chat).is_none() {
                        continue;
                    }
                    match inner.resume_delivery(&chat) {
                        Ok(session) => {
                            held.insert(chat, session);
                        }
                        Err(err) => tracing::debug!(%chat, %err, "owed chat did not open"),
                    }
                }
            }
            let mut link_changes = inner
                .peer_links
                .get()
                .map(|links| links.subscribe_changes());
            drop(inner);
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
                () = notified => {}
                () = changed => {}
                () = tokio::time::sleep(RETRY_CAP) => {}
            }
        }
    });
}
