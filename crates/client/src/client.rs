//! [`Client`] — the account-scoped root object.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use onicode_doc::RegistryDoc;
use onicode_proto::{Chat, ChatConfig, SidebarPinChange, SidebarSectionChange};
use tokio_util::sync::CancellationToken;

use crate::attachments::{self, AttachmentCache};
use crate::catalog::{self, HarnessInfo, ModelInfo};
use crate::config::{ClientConfig, Mode};
use crate::connectivity::{
    Connectivity, ConnectivityState, ConnectivityTracker, RawConnectivity, SendState,
};
use crate::demo::DemoHost;
use crate::error::{ClientError, Result};
use crate::events::{ClientEvent, ClientListener, EventPump};
use crate::live::LiveBackend;
use crate::rpc::{FolderListing, ProgressFn, RepoRef, capability};
use crate::session::{HostCapabilities, OutgoingAttachment, RoomState, SessionCore, SessionHandle};
use crate::workspace::{SearchHit, WorkspaceSnapshot, WorkspaceStore};
use crate::{lock, now_ms, read, write};

/// Attachment read cache budget.
const ATTACHMENT_CACHE_BYTES: usize = 48 * 1024 * 1024;
/// Time-driven re-derivation cadence (staleness, presence, send grace).
const TICK: Duration = Duration::from_secs(1);
/// Detached sessions kept warm (doc + room) before the least recently used
/// is evicted. On-screen sessions, streaming ones and ones with unadopted
/// sends are never evicted.
pub const WARM_SESSION_CAP: usize = 6;
/// Sessions `preload_sessions` warms (front page order).
pub const PRELOAD_CAP: usize = 4;

/// Where a new session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    /// In a project: host = the project's owning device, cwd = its folder
    /// (or `cwd` override, e.g. a picked worktree).
    Project { space_id: String },
    /// No project: runs in the chosen host's home folder (`~`).
    Projectless { device_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewSession {
    pub target: SessionTarget,
    pub config: Option<ChatConfig>,
    /// Branch to stamp from the first frame (picked ref).
    pub branch: Option<String>,
    /// Checkout override (an existing worktree's path).
    pub cwd: Option<String>,
    pub title: Option<String>,
}

pub(crate) enum Backend {
    Demo(Arc<DemoHost>),
    Live(Box<LiveBackend>),
}

pub(crate) struct ClientInner {
    pub(crate) config: ClientConfig,
    pub(crate) events: Arc<EventPump>,
    pub(crate) workspace: WorkspaceStore,
    pub(crate) attachment_cache: AttachmentCache,
    backend: OnceLock<Backend>,
    sessions: Mutex<HashMap<String, Arc<SessionCore>>>,
    pub(crate) cancel: CancellationToken,
    connectivity_tracker: Mutex<ConnectivityTracker>,
    connectivity: RwLock<Connectivity>,
    path_online: AtomicBool,
    foreground: AtomicBool,
    synced: AtomicBool,
    pub(crate) harness_catalogs: Mutex<HashMap<String, Vec<HarnessInfo>>>,
    /// Terminals on other devices, over the peer links (the desktop engine
    /// runs the same hub).
    pub(crate) remote_terminals: onicode_term_link::remote::RemoteTerminals,
    /// The links to the user's engines on the tailnet, once the app has
    /// joined it ([`Client::set_peer_links`]).
    pub(crate) peer_links: OnceLock<Arc<onicode_peer::PeerLinks>>,
    /// This device on the tailnet once it joined by itself ([`Client::join_tailnet`],
    /// [`Client::join_engines`]): its endpoint and discovery, and the engine list it reads
    /// when it has no `LocalAPI`.
    pub(crate) node: Mutex<Option<crate::join::Node>>,
    /// Notified when the links change (set, a new network path, the foreground):
    /// every wait for a link starts over.
    pub(crate) links_wake: Arc<tokio::sync::Notify>,
    /// When the app last went to the background.
    backgrounded_at: Mutex<Option<std::time::Instant>>,
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        // Released without `shutdown()`: stop every background task.
        self.cancel.cancel();
    }
}

impl ClientInner {
    pub(crate) fn backend(&self) -> &Backend {
        self.backend.get().expect("backend initialized")
    }

    pub(crate) fn demo(&self) -> Option<&Arc<DemoHost>> {
        match self.backend() {
            Backend::Demo(demo) => Some(demo),
            Backend::Live(_) => None,
        }
    }

    pub(crate) fn is_demo(&self) -> bool {
        self.demo().is_some()
    }

    pub(crate) fn live(&self) -> Option<&LiveBackend> {
        match self.backend.get()? {
            Backend::Live(live) => Some(live),
            Backend::Demo(_) => None,
        }
    }

    pub(crate) fn session_core(&self, chat_id: &str) -> Option<Arc<SessionCore>> {
        lock(&self.sessions).get(chat_id).cloned()
    }

    /// Forget `core` (its replica went stale), so the next open loads the chat afresh.
    pub(crate) fn forget_session(&self, core: &Arc<SessionCore>) {
        let mut sessions = lock(&self.sessions);
        if sessions
            .get(&core.chat_id)
            .is_some_and(|open| Arc::ptr_eq(open, core))
        {
            sessions.remove(&core.chat_id);
        }
    }

    pub(crate) fn cores(&self) -> Vec<Arc<SessionCore>> {
        lock(&self.sessions).values().cloned().collect()
    }

    pub(crate) fn network_online(&self) -> bool {
        self.path_online.load(Ordering::Acquire)
    }

    /// Re-derive the workspace snapshot; on change, notify and refresh every
    /// open composer (host presence / live status feed them).
    pub(crate) fn recompute_workspace(self: &Arc<Self>) {
        let send_states: HashMap<String, SendState> = self
            .cores()
            .iter()
            .filter_map(|core| core.send_state().map(|s| (core.chat_id.clone(), s)))
            .collect();
        let synced = self.synced.load(Ordering::Acquire);
        if let Some(revision) =
            self.workspace
                .recompute(&self.config.device_id, &send_states, synced)
        {
            self.events.workspace(revision);
            // Host presence / live status feed every open session's snapshot
            // (working flag) and composer; `refresh` is O(1) when no doc
            // event is pending.
            for core in self.cores() {
                core.refresh();
            }
        }
    }

    /// After any local registry write: push (live), re-derive. Demo's
    /// local-only replica already holds the write.
    pub(crate) fn after_registry_write(self: &Arc<Self>) {
        if let Backend::Live(live) = self.backend() {
            live.registry_written();
        }
        self.recompute_workspace();
    }

    /// Live: registry rows merged, or the mirror caught up.
    pub(crate) fn registry_changed(self: &Arc<Self>) {
        let Some(live) = self.live() else { return };
        let posture = live.registry_posture();
        if posture.synced {
            self.synced.store(true, Ordering::Release);
            // Initialize prefs / prune pins of deleted chats (authoritative
            // only once a server state applied — never on a cold replica).
            match self
                .workspace
                .mutate(|doc| doc.reconcile_sidebar_pins(true))
            {
                Ok(true) => live.registry_written(),
                Ok(false) => {}
                Err(err) => tracing::warn!(error = %err, "sidebar pin reconcile failed"),
            }
        }
        live.mark_registry_dirty();
        if let Some(presence) = live.presence() {
            self.workspace.replace_presence(presence);
        }
        self.recompute_connectivity();
        self.recompute_workspace();
        self.reconcile_change_request_watches();
        // New rows may bring a chat an open session lacked: attach its room.
        for core in self.cores() {
            core.ensure_room(self);
        }
    }

    /// Live: a link or the tailnet directory changed (or the tick came round).
    pub(crate) fn presence_changed(self: &Arc<Self>) {
        let Some(live) = self.live() else { return };
        if let Some(presence) = live.presence() {
            self.workspace.replace_presence(presence);
        }
        self.recompute_workspace();
    }

    pub(crate) fn connectivity_changed(self: &Arc<Self>) {
        self.recompute_connectivity();
    }

    /// PR-status streams for every active chat with a branch (per
    /// `(device, repo root, branch)`), only while foregrounded.
    pub(crate) fn reconcile_change_request_watches(self: &Arc<Self>) {
        let Some(live) = self.live() else { return };
        if !self.foreground.load(Ordering::Acquire) {
            live.hosts.stop_watches();
            return;
        }
        let (state, _) = self.workspace.state();
        let targets = state
            .chats
            .iter()
            .filter(|c| !c.archived && c.parent_chat_id.is_none())
            .filter_map(|c| {
                let source = c.source_context.as_ref()?;
                let branch = source.branch.trim();
                (!branch.is_empty() && !source.repo_root.is_empty()).then(|| {
                    crate::live::hosts::WatchKey {
                        device_id: c.device_id.clone(),
                        cwd: source.repo_root.clone(),
                        branch: branch.to_owned(),
                    }
                })
            })
            .collect();
        live.hosts.reconcile_watches(self, targets);
    }

    /// The chat owes its host something: rescan the owed chats, so a closed session reopens
    /// and carries it.
    pub(crate) fn nudge_host(self: &Arc<Self>) {
        if let Some(live) = self.live() {
            live.owed_wake.notify_one();
        }
    }

    pub(crate) fn resume_delivery(self: &Arc<Self>, chat_id: &str) -> Result<SessionHandle> {
        Client {
            inner: self.clone(),
        }
        .open_session(chat_id)
    }

    pub(crate) fn registry_write<R>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut RegistryDoc) -> std::result::Result<R, onicode_doc::DocError>,
    ) -> Result<R> {
        let result = self.workspace.mutate(f)?;
        self.after_registry_write();
        Ok(result)
    }

    pub(crate) fn mark_seen(self: &Arc<Self>, chat_id: &str) {
        let _ = self.registry_write(|doc| doc.set_chat_seen(chat_id, Utc::now()));
    }

    pub(crate) fn recompute_connectivity(self: &Arc<Self>) {
        let raw = match self.live() {
            None => RawConnectivity {
                path_offline: !self.network_online(),
                registry_connected: true,
                registry_retry_at_ms: None,
                last_failure: None,
                chat_rooms: Vec::new(),
            },
            Some(live) => {
                let posture = live.registry_posture();
                RawConnectivity {
                    path_offline: !self.network_online(),
                    registry_connected: posture.connected,
                    registry_retry_at_ms: posture.retry_at_ms,
                    last_failure: posture.last_failure,
                    chat_rooms: self
                        .cores()
                        .iter()
                        .filter_map(|core| {
                            let room = core.room()?;
                            Some((core.chat_id.clone(), room.connected(), room.retry_at_ms()))
                        })
                        .collect(),
                }
            }
        };
        let next = lock(&self.connectivity_tracker).compute(&raw, now_ms());
        let changed = {
            let mut current = write(&self.connectivity);
            if *current == next {
                false
            } else {
                *current = next.clone();
                true
            }
        };
        if changed {
            self.events.ordered(ClientEvent::ConnectivityChanged(next));
            for core in self.cores() {
                core.refresh();
            }
            if let Some(demo) = self.demo()
                && self.network_online()
            {
                demo.network_recovered();
            }
        }
    }

    pub(crate) fn connectivity(&self) -> Connectivity {
        read(&self.connectivity).clone()
    }

    /// Whether a send to this chat would queue rather than deliver promptly
    /// (legacy `chatDeliveryDegraded`): OS offline, the chat's room degraded,
    /// the registry down with no room, or the host presence-dark.
    pub(crate) fn chat_delivery_degraded(&self, chat_id: &str) -> bool {
        let connectivity = self.connectivity();
        if connectivity.state == ConnectivityState::Offline {
            return true;
        }
        if connectivity.degraded_chats.iter().any(|c| c == chat_id) {
            return true;
        }
        if !matches!(
            connectivity.state,
            ConnectivityState::Connected | ConnectivityState::Disabled
        ) {
            return true;
        }
        self.workspace
            .snapshot()
            .session(chat_id)
            .is_some_and(|row| !row.device_online)
    }

    pub(crate) fn room_state(&self, chat_id: &str) -> RoomState {
        let connectivity = self.connectivity();
        let degraded = connectivity.degraded_chats.iter().any(|c| c == chat_id);
        match self.session_core(chat_id).and_then(|core| core.room()) {
            Some(room) => RoomState {
                connected: room.connected(),
                retry_at_ms: room.retry_at_ms().or(connectivity.retry_at_ms),
                degraded,
            },
            None => RoomState {
                connected: connectivity.state != ConnectivityState::Offline,
                retry_at_ms: connectivity.retry_at_ms,
                degraded,
            },
        }
    }

    pub(crate) fn host_capabilities(
        &self,
        device_id: &str,
        harness: Option<&str>,
    ) -> HostCapabilities {
        let workspace = self.workspace.snapshot();
        let Some(device) = workspace.device(device_id) else {
            return HostCapabilities::default();
        };
        let has = |cap: &str| device.capabilities.iter().any(|c| c == cap);
        let mid_turn_steering = harness.and_then(|harness| {
            lock(&self.harness_catalogs)
                .get(device_id)
                .and_then(|list| list.iter().find(|h| h.id == harness))
                .and_then(HarnessInfo::mid_turn_steering)
        });
        HostCapabilities {
            message_queue: has(capability::MESSAGE_QUEUE_V1),
            queue_actions: has(capability::MESSAGE_QUEUE_ACTIONS_V1),
            queue_attachments: has(capability::MESSAGE_QUEUE_ATTACHMENTS_V1),
            clean_attachment_text: has(capability::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1),
            queue_edit_lease: has(capability::MESSAGE_QUEUE_EDIT_LEASE_V1),
            mid_turn_steering,
            provider_switch: has(capability::PROVIDER_SWITCH_V1),
        }
    }

    /// Mint `pending://` refs for picked images and keep their bytes local
    /// (the echo renders them immediately; the escort pushes them).
    pub(crate) fn stage_attachments(
        &self,
        chat_id: &str,
        device_id: &str,
        attachments: &[OutgoingAttachment],
    ) -> Result<Vec<String>> {
        if let Some(too_big) = attachments
            .iter()
            .find(|a| a.data.len() > attachments::MAX_ATTACHMENT_BYTES)
        {
            return Err(ClientError::InvalidArgument(format!(
                "{} is larger than 24 MB",
                too_big.name
            )));
        }
        attachments
            .iter()
            .map(|attachment| {
                let upload_id = crate::new_id();
                let name = attachments::upload_file_name(&attachment.name);
                let reference = attachments::pending_ref(&upload_id, &name);
                if let Some(live) = self.live() {
                    let meta = crate::live::escort::StashMeta {
                        upload_id: upload_id.clone(),
                        chat_id: chat_id.to_owned(),
                        host_device_id: device_id.to_owned(),
                        name: name.clone(),
                        created_at_ms: now_ms(),
                    };
                    live.escorts
                        .stash(&meta, &attachment.data)
                        .map_err(|e| ClientError::Storage(e.to_string()))?;
                }
                self.attachment_cache
                    .put(device_id, &reference, Arc::new(attachment.data.clone()));
                Ok(reference)
            })
            .collect()
    }

    /// A command/queue row was written: wake the host (and escort any
    /// staged attachment bytes).
    pub(crate) fn after_command(self: &Arc<Self>, core: &Arc<SessionCore>, has_attachments: bool) {
        match self.backend() {
            Backend::Demo(demo) => demo.on_command(&core.chat_id),
            Backend::Live(live) => {
                if has_attachments {
                    live.escorts.respawn_chat(self, &core.chat_id);
                }
                // The room pushes the outbox row itself; the scan keeps the session open
                // until the host acks it.
                live.owed_wake.notify_one();
            }
        }
    }

    /// Dial `device` in the background, so a stream to it finds the link up.
    pub(crate) fn warm_link(&self, device: &str) {
        let Some(links) = self.peer_links.get().cloned() else {
            return;
        };
        let device = device.to_owned();
        crate::runtime::shared().spawn(async move {
            if let Err(error) = links.ready(&device).await {
                tracing::debug!(%error, %device, "no link to warm");
            }
        });
    }

    /// The client's links to the user's engines: hosts, streams and terminals ride them from
    /// now on. Set once.
    pub(crate) fn set_peer_links(&self, links: Arc<onicode_peer::PeerLinks>) {
        if self.peer_links.set(links).is_err() {
            tracing::debug!("peer links were already set");
        }
        self.links_wake.notify_waiters();
    }

    /// Has the listed engines looked up again, if this device reads a list.
    pub(crate) fn poke_engines(&self) {
        if let Some(engines) = lock(&self.node).as_ref().and_then(|n| n.engines.as_ref()) {
            engines.poke();
        }
    }

    /// Joins the tailnet as `join` says, unless this client already holds links or is joining.
    fn join(self: &Arc<Self>, join: crate::join::Join) -> Result<()> {
        if self.is_demo() {
            return Err(ClientError::Unsupported(
                "the demo has no tailnet to join".into(),
            ));
        }
        let mut node = lock(&self.node);
        if node.is_some() || self.peer_links.get().is_some() {
            return Err(ClientError::InvalidArgument(
                "this client is already on the tailnet".into(),
            ));
        }
        *node = Some(crate::join::Node::start(self, join));
        Ok(())
    }

    pub(crate) async fn host_rpc(
        self: &Arc<Self>,
        device_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match self.backend() {
            Backend::Demo(demo) => demo.host_rpc(device_id, method, params).await,
            Backend::Live(live) => live.hosts.call(device_id, method, params).await,
        }
    }

    /// A host stream (terminal output, a subagent's transcript): a stream of
    /// its own over the host's link live, the demo host's channel offline.
    pub(crate) async fn host_subscribe(
        self: &Arc<Self>,
        device_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<crate::host_stream::HostStream> {
        match self.backend() {
            Backend::Demo(demo) => demo.host_subscribe(device_id, method, params).await,
            Backend::Live(live) => live
                .hosts
                .subscribe(device_id, method, params)
                .await
                .map(crate::host_stream::HostStream::Peer),
        }
    }

    /// Open the chat's stream again now (retry delivery).
    pub(crate) fn kick_room(&self, chat_id: &str) {
        if let Some(room) = self.session_core(chat_id).and_then(|c| c.room()) {
            room.kick();
        }
        if let Some(live) = self.live() {
            live.kick();
        }
    }

    /// Drop the least recently used detached, quiet sessions past the cap.
    pub(crate) fn evict_sessions(&self) {
        let mut sessions = lock(&self.sessions);
        let mut idle: Vec<(i64, String)> = sessions
            .values()
            .filter(|core| {
                !core.view_attached() && !core.has_pending_sends() && !core.snapshot().streaming
            })
            .map(|core| (core.touched_ms(), core.chat_id.clone()))
            .collect();
        if idle.len() <= WARM_SESSION_CAP {
            return;
        }
        idle.sort();
        let excess = idle.len() - WARM_SESSION_CAP;
        for (_, chat_id) in idle.into_iter().take(excess) {
            tracing::debug!(chat = %chat_id, "evicting warm session");
            sessions.remove(&chat_id);
        }
    }

    fn tick(self: &Arc<Self>) {
        if let Some(presence) = self.live().and_then(|live| live.presence()) {
            self.workspace.replace_presence(presence);
        }
        self.recompute_connectivity();
        self.recompute_workspace();
        for core in self.cores() {
            if core.has_pending_sends() {
                core.refresh();
            }
        }
    }
}

/// One device's view of the user's engines (or Demo mode). Cheap to clone.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<ClientInner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("device_id", &self.inner.config.device_id)
            .field("demo", &self.inner.is_demo())
            .finish()
    }
}

impl Client {
    /// Dedicated ephemeral call handle; never uses durable commands or RPC retries.
    pub async fn voice_transport(
        &self,
        device_id: &str,
    ) -> Result<Arc<onicode_voice_session::PeerTransport>> {
        let live = self.inner.live().ok_or_else(|| {
            ClientError::Unsupported("remote voice needs a computer on the tailnet".into())
        })?;
        live.hosts.voice_transport(device_id).await
    }

    /// Shutdown closes every call created by this client.
    pub fn voice_cancellation(&self) -> CancellationToken {
        self.inner.cancel.child_token()
    }

    /// Build and start. Never blocks on the network: live mode hydrates from
    /// `data_dir` and syncs once it is on the tailnet; Demo seeds its dataset.
    pub fn new(
        config: ClientConfig,
        mode: Mode,
        listener: Arc<dyn ClientListener>,
    ) -> Result<Self> {
        if config.device_id.trim().is_empty() {
            return Err(ClientError::InvalidArgument("device_id is required".into()));
        }
        let events = EventPump::new(listener);
        // Live: restore the registry replica + open the docs store first, so
        // the very first snapshot renders the cached workspace (instant).
        let (registry, store) = if mode.is_demo() {
            (RegistryDoc::new(config.device_id.clone()), None)
        } else {
            let (store, registry) = LiveBackend::open(&config.data_dir, &config.device_id)?;
            (registry, Some(store))
        };
        let inner = Arc::new(ClientInner {
            workspace: WorkspaceStore::new(registry),
            events: events.clone(),
            attachment_cache: AttachmentCache::new(ATTACHMENT_CACHE_BYTES),
            backend: OnceLock::new(),
            sessions: Mutex::new(HashMap::new()),
            cancel: CancellationToken::new(),
            connectivity_tracker: Mutex::new(ConnectivityTracker::default()),
            connectivity: RwLock::new(Connectivity {
                state: if mode.is_demo() {
                    ConnectivityState::Connected
                } else {
                    ConnectivityState::Reconnecting
                },
                ..Default::default()
            }),
            path_online: AtomicBool::new(true),
            foreground: AtomicBool::new(true),
            synced: AtomicBool::new(false),
            harness_catalogs: Mutex::new(HashMap::new()),
            remote_terminals: Default::default(),
            peer_links: OnceLock::new(),
            node: Mutex::new(None),
            links_wake: Arc::new(tokio::sync::Notify::new()),
            backgrounded_at: Mutex::new(None),
            config,
        });
        let backend = match &mode {
            Mode::Demo(options) => {
                let demo = DemoHost::new(&inner, options.clone());
                demo.seed(&inner)?;
                inner.synced.store(true, Ordering::Release);
                Backend::Demo(demo)
            }
            Mode::Live => Backend::Live(Box::new({
                catalog::DiskCatalog::new(&inner.config.data_dir).warm_labels();
                LiveBackend::new(
                    &inner,
                    store.ok_or_else(|| ClientError::Internal("docs store missing".into()))?,
                )
            })),
        };
        let _ = inner.backend.set(backend);
        events.start(inner.cancel.clone());
        inner.recompute_workspace();
        match inner.backend() {
            Backend::Demo(demo) => demo.start(&inner, inner.cancel.clone()),
            Backend::Live(live) => {
                live.start(&inner);
                inner.recompute_connectivity();
            }
        }
        let ticker = Arc::downgrade(&inner);
        let cancel = inner.cancel.clone();
        crate::runtime::shared().spawn(async move {
            let mut interval = tokio::time::interval(TICK);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = interval.tick() => {}
                }
                let Some(inner) = ticker.upgrade() else {
                    return;
                };
                if inner.foreground.load(Ordering::Acquire) {
                    inner.tick();
                }
            }
        });
        Ok(Self { inner })
    }

    pub fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    pub fn is_demo(&self) -> bool {
        self.inner.is_demo()
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    /// Stop every background task. Snapshots stay readable.
    pub fn shutdown(&self) {
        match self.inner.backend() {
            Backend::Demo(demo) => demo.stop(),
            Backend::Live(live) => {
                live.flush_registry(&self.inner);
                live.stop();
            }
        }
        self.inner.cancel.cancel();
        if let Some(links) = self.inner.peer_links.get() {
            links.shutdown();
        }
        lock(&self.inner.node).take();
        // Dropping the cores stops their rooms (each flushes its snapshot).
        lock(&self.inner.sessions).clear();
        self.inner.attachment_cache.clear();
    }

    // ── workspace reads ────────────────────────────────────────────────────

    /// O(1): the current derived workspace.
    pub fn workspace(&self) -> Arc<WorkspaceSnapshot> {
        self.inner.workspace.snapshot()
    }

    pub fn connectivity(&self) -> Connectivity {
        self.inner.connectivity()
    }

    /// The chat's run configuration (harness/model/effort/options/sandbox).
    pub fn session_config(&self, chat_id: &str) -> Option<ChatConfig> {
        self.inner.workspace.chat(chat_id).and_then(|c| c.config)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        self.workspace().search(query, limit)
    }

    // ── workspace writes ───────────────────────────────────────────────────

    /// Mint a chat row for its host. This device creates it, so its placement (cwd, space,
    /// branch) stands; the host writes its own fields once it runs.
    pub fn create_session(&self, new: NewSession) -> Result<String> {
        let (state, _) = self.inner.workspace.state();
        let id = crate::new_id();
        let now = Utc::now();
        let (device_id, space_id, cwd) = match &new.target {
            SessionTarget::Project { space_id } => {
                let space = state
                    .spaces
                    .iter()
                    .find(|s| &s.id == space_id)
                    .ok_or_else(|| ClientError::NotFound(space_id.clone()))?;
                (
                    space.device_id.clone(),
                    Some(space.id.clone()),
                    new.cwd.clone().unwrap_or_else(|| space.path.clone()),
                )
            }
            SessionTarget::Projectless { device_id } => {
                let host = state
                    .devices
                    .iter()
                    .find(|d| &d.id == device_id)
                    .ok_or_else(|| ClientError::NotFound(device_id.clone()))?;
                if matches!(host.platform.as_str(), "ios" | "android" | "ipados") {
                    return Err(ClientError::InvalidArgument(
                        "that device can't host sessions".into(),
                    ));
                }
                // Its own folder on the host, created when the first run
                // spawns; a host that predates scratch folders runs it in `~`.
                let cwd = if host
                    .capabilities
                    .iter()
                    .any(|c| c == crate::rpc::capability::PROJECTLESS_SCRATCH_V1)
                {
                    onicode_proto::projectless::scratch_cwd(&id, now)
                } else {
                    "~".to_owned()
                };
                (device_id.clone(), None, cwd)
            }
        };
        let chat = Chat {
            id,
            device_id,
            title: new.title.filter(|t| !t.trim().is_empty()),
            archived: false,
            cwd: Some(cwd),
            branch: new.branch,
            checkout_id: None,
            source_context: None,
            config: new.config,
            last_message_preview: None,
            last_message_at: None,
            created_at: now,
            harness_session_id: None,
            harness_session_cwd: None,
            harness_session_harness: None,
            space_id,
            last_seen_at: None,
            parent_chat_id: None,
            child_kind: None,
        };
        let id = chat.id.clone();
        self.inner.registry_write(|doc| doc.upsert_chat(&chat))?;
        Ok(id)
    }

    /// A fresh side chat under `parent_id`: same host, folder, checkout and
    /// agent settings as its parent, an empty conversation (the desktop's
    /// "New side chat"; agent-spawned side chats have the same shape).
    pub fn create_side_chat(&self, parent_id: &str) -> Result<String> {
        let parent = self
            .inner
            .workspace
            .chat(parent_id)
            .ok_or_else(|| ClientError::NotFound(parent_id.to_owned()))?;
        // One level deep: a side chat's own side chat hangs off the root.
        let root = parent
            .parent_chat_id
            .clone()
            .unwrap_or_else(|| parent.id.clone());
        let mut chat = parent;
        chat.id = crate::new_id();
        chat.parent_chat_id = Some(root);
        chat.title = None;
        chat.archived = false;
        chat.created_at = Utc::now();
        chat.last_message_at = None;
        chat.last_message_preview = None;
        chat.last_seen_at = None;
        chat.harness_session_id = None;
        chat.harness_session_cwd = None;
        chat.harness_session_harness = None;
        let id = chat.id.clone();
        self.inner.registry_write(|doc| doc.upsert_chat(&chat))?;
        Ok(id)
    }

    /// Fork `source_id` through its latest completed reply into a side chat
    /// (`ForkSideChat`, answered by the source's host, which copies the
    /// conversation). A side chat forks as a sibling, under the same parent.
    pub async fn fork_side_chat(&self, source_id: &str) -> Result<String> {
        let source = self
            .inner
            .workspace
            .chat(source_id)
            .ok_or_else(|| ClientError::NotFound(source_id.to_owned()))?;
        let parent = source
            .parent_chat_id
            .clone()
            .unwrap_or_else(|| source.id.clone());
        let chat_id = crate::new_id();
        let reply = self
            .inner
            .host_rpc(
                &source.device_id,
                onicode_rpc::methods::FORK_SIDE_CHAT,
                serde_json::json!({
                    "chatId": chat_id,
                    "sourceChatId": source.id,
                    "parentChatId": parent,
                }),
            )
            .await?;
        // The host writes the fork's row; it reaches this device through
        // registry sync on its own time. Hold the reply's copy now so the
        // fork opens at once (the same row converges when sync lands).
        match serde_json::from_value::<Chat>(reply) {
            Ok(chat) => {
                if self.inner.workspace.chat(&chat.id).is_none() {
                    self.inner.registry_write(|doc| doc.upsert_chat(&chat))?;
                }
                Ok(chat.id)
            }
            Err(_) => Ok(chat_id),
        }
    }

    fn chat_write(
        &self,
        chat_id: &str,
        f: impl FnOnce(&mut RegistryDoc) -> std::result::Result<bool, onicode_doc::DocError>,
    ) -> Result<()> {
        if self.inner.registry_write(f)? {
            Ok(())
        } else {
            Err(ClientError::NotFound(chat_id.to_owned()))
        }
    }

    pub fn archive_session(&self, chat_id: &str) -> Result<()> {
        self.chat_write(chat_id, |doc| doc.set_chat_archived(chat_id, true))
    }

    pub fn unarchive_session(&self, chat_id: &str) -> Result<()> {
        self.chat_write(chat_id, |doc| doc.set_chat_archived(chat_id, false))
    }

    pub fn rename_session(&self, chat_id: &str, title: &str) -> Result<()> {
        self.chat_write(chat_id, |doc| doc.rename_chat(chat_id, title.trim()))
    }

    pub fn mark_seen(&self, chat_id: &str) {
        self.inner.mark_seen(chat_id);
    }

    pub fn set_session_config(&self, chat_id: &str, config: &ChatConfig) -> Result<()> {
        // A provider change needs a host that switches: an older one would
        // resume the chat's stored session under the new provider.
        if let Some(chat) = self.inner.workspace.chat(chat_id)
            && chat
                .config
                .as_ref()
                .is_some_and(|current| current.harness != config.harness)
            && !self
                .inner
                .host_capabilities(&chat.device_id, None)
                .provider_switch
        {
            return Err(ClientError::Unsupported(
                "This chat's computer needs a newer Onicode to switch provider.".into(),
            ));
        }
        self.chat_write(chat_id, |doc| doc.set_chat_config(chat_id, config))
    }

    /// Remove the chat row (its host keeps the doc).
    pub fn delete_session(&self, chat_id: &str) -> Result<()> {
        self.chat_write(chat_id, |doc| doc.delete_chat(chat_id))?;
        self.forget_local_session(chat_id);
        Ok(())
    }

    /// Drop a deleted chat's open session and local snapshot.
    fn forget_local_session(&self, chat_id: &str) {
        let core = lock(&self.inner.sessions).remove(chat_id);
        // A view may still hold the session: close its room now (it flushes
        // once), so nothing re-saves the snapshot deleted below.
        if let Some(core) = &core {
            core.close_room();
        }
        drop(core);
        if let Some(live) = self.inner.live()
            && let Err(err) = live.store.delete_snapshot(chat_id)
        {
            tracing::debug!(error = %err, "local chat snapshot delete failed");
        }
    }

    /// Mid-session ref switch: retarget onto the ref's existing worktree (row
    /// writes only), else `git checkout` in the session's own folder.
    pub async fn switch_session_ref(&self, chat_id: &str, reference: RepoRef) -> Result<()> {
        let chat = self
            .inner
            .workspace
            .chat(chat_id)
            .ok_or_else(|| ClientError::NotFound(chat_id.to_owned()))?;
        let cwd = chat
            .cwd
            .clone()
            .ok_or_else(|| ClientError::InvalidArgument("session has no working folder".into()))?;
        if let Some(worktree) = reference.worktree_path.clone() {
            if worktree != cwd {
                self.inner.registry_write(|doc| {
                    doc.set_chat_cwd(chat_id, &worktree)?;
                    doc.set_chat_branch(chat_id, &reference.name)
                })?;
            }
            return Ok(());
        }
        self.switch_ref(&chat.device_id, &cwd, &reference.name)
            .await?;
        self.inner
            .registry_write(|doc| doc.set_chat_branch(chat_id, &reference.name))?;
        Ok(())
    }

    fn pin_change(&self, change: SidebarPinChange) -> Result<()> {
        if !self.workspace().pins_ready {
            return Err(ClientError::Unsupported("pins are not synced yet".into()));
        }
        self.inner
            .registry_write(|doc| doc.change_sidebar_pin(&change))
    }

    pub fn pin_session(&self, chat_id: &str) -> Result<()> {
        let after = self
            .inner
            .workspace
            .state()
            .1
            .and_then(|p| p.pinned_session_ids.last().cloned())
            .filter(|last| last != chat_id);
        self.pin_change(SidebarPinChange::Pin {
            session_id: chat_id.to_owned(),
            after,
            before: None,
        })
    }

    /// Pin between neighbours (either may be `None` at the ends).
    pub fn pin_session_at(
        &self,
        chat_id: &str,
        after: Option<String>,
        before: Option<String>,
    ) -> Result<()> {
        self.pin_change(SidebarPinChange::Pin {
            session_id: chat_id.to_owned(),
            after,
            before,
        })
    }

    pub fn unpin_session(&self, chat_id: &str) -> Result<()> {
        self.pin_change(SidebarPinChange::Unpin {
            session_id: chat_id.to_owned(),
        })
    }

    /// Reorder a pin between neighbours (either may be `None` at the ends).
    pub fn move_pin(
        &self,
        chat_id: &str,
        after: Option<String>,
        before: Option<String>,
    ) -> Result<()> {
        self.pin_change(SidebarPinChange::Move {
            session_id: chat_id.to_owned(),
            after,
            before,
        })
    }

    fn section_change(&self, change: SidebarSectionChange) -> Result<()> {
        self.pin_change(SidebarPinChange::Section { change })
    }

    pub fn create_section(&self, name: &str) -> Result<String> {
        let id = crate::new_id();
        self.section_change(SidebarSectionChange::Create {
            id: id.clone(),
            name: name.trim().to_owned(),
        })?;
        Ok(id)
    }

    pub fn rename_section(&self, section_id: &str, name: &str) -> Result<()> {
        self.section_change(SidebarSectionChange::Rename {
            id: section_id.to_owned(),
            name: name.trim().to_owned(),
        })
    }

    /// Delete a section (its sessions return to the recent list).
    pub fn delete_section(&self, section_id: &str) -> Result<()> {
        self.section_change(SidebarSectionChange::Delete {
            id: section_id.to_owned(),
        })
    }

    pub fn set_section_collapsed(&self, section_id: &str, collapsed: bool) -> Result<()> {
        self.section_change(SidebarSectionChange::Collapse {
            id: section_id.to_owned(),
            collapsed,
        })
    }

    /// Move a session into a section (`None` = back to the recent list).
    pub fn assign_section(&self, chat_id: &str, section_id: Option<String>) -> Result<()> {
        self.place_session(chat_id, section_id, None, None)
    }

    /// Move a session into a section between its members (either neighbour
    /// may be `None` at the ends; both `None` appends).
    pub fn place_session(
        &self,
        chat_id: &str,
        section_id: Option<String>,
        after: Option<String>,
        before: Option<String>,
    ) -> Result<()> {
        self.section_change(SidebarSectionChange::Assign {
            session_id: chat_id.to_owned(),
            section_id,
            after,
            before,
        })
    }

    /// Create a project on `device_id` (deduped on device + path). Live
    /// mode asks the owning host (`Mutate {createSpace}`) and falls back to
    /// a local row write when it is unreachable.
    pub async fn create_project(
        &self,
        device_id: &str,
        path: &str,
        git_detected: bool,
    ) -> Result<String> {
        let (state, _) = self.inner.workspace.state();
        if let Some(existing) = state
            .spaces
            .iter()
            .find(|s| s.device_id == device_id && s.path == path)
        {
            return Ok(existing.id.clone());
        }
        let space = onicode_proto::Space {
            id: crate::new_id(),
            device_id: device_id.to_owned(),
            path: path.to_owned(),
            name: None,
            git_detected,
            git_checked_at: None,
            checkout_id: None,
            repository_id: None,
            github_repo: None,
            icon: None,
            created_at: Utc::now(),
        };
        let id = space.id.clone();
        // The owning host writes the row itself (it knows git state and runs
        // the project's setup); an unreachable host gets a local row write
        // it will adopt later.
        if let Some(live) = self.inner.live() {
            let asked = live
                .hosts
                .call(
                    device_id,
                    onicode_rpc::methods::MUTATE,
                    serde_json::json!({
                        "op": "createSpace",
                        "spaceId": id,
                        "deviceId": device_id,
                        "path": path,
                        "gitDetected": git_detected,
                    }),
                )
                .await;
            match asked {
                Ok(_) => {
                    // Wait (briefly) for the host's row so the caller can
                    // start a session in it right away.
                    for _ in 0..50 {
                        if self
                            .inner
                            .workspace
                            .state()
                            .0
                            .spaces
                            .iter()
                            .any(|s| s.id == id)
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    return Ok(id);
                }
                Err(err) => {
                    tracing::info!(error = %err, "createSpace via host failed; writing locally")
                }
            }
        }
        self.inner.registry_write(|doc| doc.upsert_space(&space))?;
        Ok(id)
    }

    pub fn rename_project(&self, space_id: &str, name: Option<&str>) -> Result<()> {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        if self
            .inner
            .registry_write(|doc| doc.rename_space(space_id, name))?
        {
            Ok(())
        } else {
            Err(ClientError::NotFound(space_id.to_owned()))
        }
    }

    /// Delete a project and cascade its chats' rows.
    pub fn delete_project(&self, space_id: &str) -> Result<()> {
        let deleted = self
            .inner
            .registry_write(|doc| doc.delete_space(space_id))?;
        let mut sessions = lock(&self.inner.sessions);
        for chat_id in deleted.chat_ids {
            sessions.remove(&chat_id);
        }
        Ok(())
    }

    // ── sessions ───────────────────────────────────────────────────────────

    /// Open (or return the already-open) session. Instant: the local
    /// snapshot renders first; sync follows in the background.
    pub fn open_session(&self, chat_id: &str) -> Result<SessionHandle> {
        if let Some(core) = self.inner.session_core(chat_id) {
            return Ok(SessionHandle { core });
        }
        if self.inner.workspace.chat(chat_id).is_none() {
            return Err(ClientError::NotFound(chat_id.to_owned()));
        }
        let (doc, hydrated) = match self.inner.backend() {
            Backend::Demo(demo) => (demo.session_doc(chat_id)?, true),
            Backend::Live(live) => {
                let local = crate::live::room::load_local(&live.store, chat_id);
                (local.doc, local.had_content)
            }
        };
        let core = SessionCore::new(chat_id, &self.inner, doc);
        let core = {
            let mut sessions = lock(&self.inner.sessions);
            sessions
                .entry(chat_id.to_owned())
                .or_insert_with(|| core.clone())
                .clone()
        };
        if hydrated {
            core.set_hydrated();
        }
        core.ensure_room(&self.inner);
        core.touch();
        core.refresh();
        if let Some(demo) = self.inner.demo() {
            demo.session_opened(&core);
        }
        self.inner.evict_sessions();
        Ok(SessionHandle { core })
    }

    /// An already-open session, if any.
    pub fn session(&self, chat_id: &str) -> Option<SessionHandle> {
        self.inner
            .session_core(chat_id)
            .map(|core| SessionHandle { core })
    }

    /// The view closed. The session stays warm (streaming continues) until
    /// the eviction policy reclaims it.
    pub fn close_session(&self, chat_id: &str) {
        if let Some(handle) = self.session(chat_id) {
            handle.set_view_attached(false);
        }
        self.inner.evict_sessions();
    }

    // ── host RPC ───────────────────────────────────────────────────────────

    /// Harness catalog of an execution device: live (cached to disk), else
    /// the last cached list, else the static fallback. Cached in memory for
    /// capability gating (mid-turn steering).
    pub async fn list_harnesses(&self, device_id: &str) -> Vec<HarnessInfo> {
        let list = match self.inner.backend() {
            Backend::Demo(demo) => {
                crate::providers::with_demo_toggles(true, demo.list_harnesses(device_id).await)
            }
            Backend::Live(live) => {
                let cache = catalog::DiskCatalog::new(&self.inner.config.data_dir);
                let reply = live
                    .hosts
                    .call(
                        device_id,
                        onicode_rpc::methods::LIST_HARNESSES,
                        serde_json::json!({}),
                    )
                    .await
                    .and_then(|v| {
                        serde_json::from_value::<Vec<HarnessInfo>>(v)
                            .map_err(|e| ClientError::HostError(e.to_string()))
                    });
                match reply {
                    Ok(list) => {
                        let list: Vec<HarnessInfo> =
                            list.into_iter().filter(|h| h.id != "mock").collect();
                        cache.put_harnesses(device_id, &list);
                        list
                    }
                    Err(err) => {
                        tracing::debug!(device = %device_id, error = %err, "ListHarnesses failed; using cache");
                        cache
                            .harnesses(device_id)
                            .unwrap_or_else(catalog::fallback_harnesses)
                    }
                }
            }
        };
        lock(&self.inner.harness_catalogs).insert(device_id.to_owned(), list.clone());
        self.inner.recompute_workspace();
        for core in self.inner.cores() {
            core.recompute_composer(&self.inner);
        }
        list
    }

    /// Model catalog for `harness` on `device_id` (normalized live reply,
    /// else the cached one, else the curated static list).
    pub async fn list_models(&self, device_id: &str, harness: &str) -> Vec<ModelInfo> {
        match self.inner.backend() {
            Backend::Demo(demo) => demo.list_models(harness).await,
            Backend::Live(live) => {
                let cache = catalog::DiskCatalog::new(&self.inner.config.data_dir);
                let reply = live
                    .hosts
                    .call(
                        device_id,
                        onicode_rpc::methods::LIST_MODELS,
                        serde_json::json!({ "harness": harness }),
                    )
                    .await
                    .and_then(|v| {
                        serde_json::from_value::<Vec<ModelInfo>>(v)
                            .map_err(|e| ClientError::HostError(e.to_string()))
                    });
                match reply {
                    Ok(list) if !list.is_empty() => {
                        let list = catalog::normalize_models(harness, list);
                        cache.put_models(device_id, harness, &list);
                        // Rows and chips pick up newly learned model labels.
                        self.inner.recompute_workspace();
                        list
                    }
                    _ => match cache.models(device_id, harness) {
                        Some(list) => {
                            catalog::learn_labels(harness, &list);
                            list
                        }
                        None => catalog::fallback_models(harness),
                    },
                }
            }
        }
    }

    pub async fn list_refs(&self, device_id: &str, repo_path: &str) -> Result<Vec<RepoRef>> {
        match self.inner.backend() {
            Backend::Demo(demo) => demo.list_refs(repo_path).await,
            Backend::Live(live) => {
                let value = live
                    .hosts
                    .call(
                        device_id,
                        onicode_rpc::methods::LIST_REFS,
                        serde_json::json!({ "repoPath": repo_path }),
                    )
                    .await?;
                serde_json::from_value(value).map_err(|e| ClientError::HostError(e.to_string()))
            }
        }
    }

    /// Workspace file search for composer `@` mentions (host `SearchFiles`,
    /// gitignore-aware; an empty query returns the chat's featured files).
    pub async fn search_files(
        &self,
        device_id: &str,
        chat_id: Option<String>,
        space_id: Option<String>,
        query: &str,
    ) -> Result<Vec<onicode_proto::FileSearchMatch>> {
        match self.inner.backend() {
            Backend::Demo(_) => {
                const FILES: &[&str] = &[
                    "crates/mobile/src/layout/mod.rs",
                    "crates/mobile/src/layout/markdown.rs",
                    "crates/mobile/src/layout/rows.rs",
                    "crates/text/src/layout.rs",
                    "crates/text/src/prepare.rs",
                    "crates/markdown/src/parser.rs",
                    "apps/ios/Onicode/Transcript/TranscriptListView.swift",
                    "apps/ios/Onicode/Composer/ComposerBar.swift",
                    "docs/mobile-rewrite.md",
                    "README.md",
                ];
                let q = query.to_lowercase();
                Ok(FILES
                    .iter()
                    .filter(|p| q.is_empty() || p.to_lowercase().contains(&q))
                    .map(|p| onicode_proto::FileSearchMatch {
                        path: (*p).to_owned(),
                        is_dir: false,
                    })
                    .collect())
            }
            Backend::Live(live) => {
                let value = live
                    .hosts
                    .call(
                        device_id,
                        onicode_rpc::methods::SEARCH_FILES,
                        serde_json::json!({ "query": query, "chatId": chat_id, "spaceId": space_id }),
                    )
                    .await?;
                serde_json::from_value(value).map_err(|e| ClientError::HostError(e.to_string()))
            }
        }
    }

    /// Browse folders on a device (`None` = its home folder).
    pub async fn list_folders(
        &self,
        device_id: &str,
        path: Option<String>,
    ) -> Result<FolderListing> {
        match self.inner.backend() {
            Backend::Demo(demo) => demo.list_folders(device_id, path).await,
            Backend::Live(live) => {
                let params = match path.filter(|p| !p.is_empty()) {
                    Some(path) => serde_json::json!({ "path": path }),
                    None => serde_json::json!({}),
                };
                let value = live
                    .hosts
                    .call(device_id, onicode_rpc::methods::LIST_FOLDERS, params)
                    .await?;
                serde_json::from_value(value).map_err(|e| ClientError::HostError(e.to_string()))
            }
        }
    }

    /// `git checkout <ref>` in `repo_path` on the device.
    pub async fn switch_ref(&self, device_id: &str, repo_path: &str, ref_name: &str) -> Result<()> {
        match self.inner.backend() {
            Backend::Demo(demo) => demo.switch_ref(repo_path, ref_name).await,
            Backend::Live(live) => live
                .hosts
                .call(
                    device_id,
                    onicode_rpc::methods::SWITCH_REF,
                    serde_json::json!({ "repoPath": repo_path, "refName": ref_name }),
                )
                .await
                .map(|_| ()),
        }
    }

    /// Create a worktree for `branch` (an existing ref); returns its path.
    pub async fn create_worktree(
        &self,
        device_id: &str,
        space_id: &str,
        repo_path: &str,
        branch: &str,
    ) -> Result<String> {
        match self.inner.backend() {
            Backend::Demo(demo) => demo.create_worktree(repo_path, branch).await,
            Backend::Live(live) => {
                let mut params = serde_json::json!({ "repoPath": repo_path, "branch": branch });
                if !space_id.is_empty() {
                    params["spaceId"] = serde_json::Value::String(space_id.to_owned());
                }
                let value = live
                    .hosts
                    .call(device_id, onicode_rpc::methods::CREATE_WORKTREE, params)
                    .await?;
                value
                    .get("path")
                    .and_then(|p| p.as_str())
                    .map(str::to_owned)
                    .ok_or_else(|| ClientError::HostError("CreateWorktree returned no path".into()))
            }
        }
    }

    /// Chunked upload of one file to the host; returns its durable path there.
    pub async fn upload_attachment(
        &self,
        device_id: &str,
        name: &str,
        data: Vec<u8>,
        progress: Option<ProgressFn>,
    ) -> Result<String> {
        if data.len() > attachments::MAX_ATTACHMENT_BYTES {
            return Err(ClientError::InvalidArgument(format!(
                "{name} is larger than 24 MB"
            )));
        }
        match self.inner.backend() {
            Backend::Demo(demo) => {
                demo.upload(&self.inner, device_id, name, data, progress)
                    .await
            }
            Backend::Live(live) => {
                let upload_id = crate::new_id();
                let file_name = attachments::upload_file_name(name);
                let path = live
                    .hosts
                    .upload(device_id, &upload_id, &file_name, &data, progress)
                    .await?;
                self.inner
                    .attachment_cache
                    .put(device_id, &path, Arc::new(data));
                Ok(path)
            }
        }
    }

    /// Attachment bytes (LRU-cached). Accepts host paths and this device's
    /// own `pending://` refs.
    pub async fn read_attachment(&self, device_id: &str, path: &str) -> Result<Arc<Vec<u8>>> {
        if let Some(hit) = self.inner.attachment_cache.get(device_id, path) {
            return Ok(hit);
        }
        let bytes = match self.inner.backend() {
            Backend::Demo(demo) => demo.read_attachment(path)?,
            Backend::Live(live) => Arc::new(live.hosts.read_attachment(device_id, path).await?),
        };
        self.inner
            .attachment_cache
            .put(device_id, path, bytes.clone());
        Ok(bytes)
    }

    // ── lifecycle ──────────────────────────────────────────────────────────

    /// OS network path (NWPathMonitor): `false` only for a definitive
    /// "unsatisfied" — ambiguity stays online.
    pub fn set_network_online(&self, online: bool) {
        let was = self.inner.path_online.swap(online, Ordering::AcqRel);
        if was != online {
            self.inner.recompute_connectivity();
            if online && let Some(live) = self.inner.live() {
                live.kick();
                self.kick_rooms();
            }
            if online {
                self.network_path_changed();
            }
        }
    }

    /// The OS network path changed (another interface, a new address): the
    /// links' paths may be gone, so every backoff ends, they redial now and the
    /// listed engines are looked up again.
    pub fn network_path_changed(&self) {
        if let Some(links) = self.inner.peer_links.get() {
            links.reset();
        }
        self.inner.poke_engines();
        self.inner.links_wake.notify_waiters();
    }

    /// Dial the device hosting `chat_id`, so a terminal opened on it next
    /// finds its link up (a terminal screen opening).
    pub fn warm_link(&self, chat_id: &str) {
        if let Ok(device) = self.chat_host(chat_id) {
            self.inner.warm_link(&device);
        }
    }

    /// Hand the client its links to the user's engines on the tailnet: every
    /// host call, stream and terminal rides them; until then they wait. Set
    /// once, here or by joining ([`Self::join_tailnet`], [`Self::join_engines`]).
    pub fn set_peer_links(&self, links: Arc<onicode_peer::PeerLinks>) {
        self.inner.set_peer_links(links);
    }

    /// Join the tailnet this machine's Tailscale daemon reports (macOS, Linux): find the
    /// user's engines on it and link to each. Returns at once; until Tailscale is up the
    /// client keeps trying, and host calls answer [`ClientError::HostUnavailable`].
    ///
    /// # Errors
    ///
    /// [`ClientError::Unsupported`] in the demo; [`ClientError::InvalidArgument`] when the
    /// client is already on the tailnet.
    pub fn join_tailnet(&self) -> Result<()> {
        self.inner.join(crate::join::Join {
            tailnet: Arc::new(onicode_tailnet::Relooking::system()),
            sockets: Arc::new(onicode_peer::SystemSockets),
            config: self.peer_config(),
            engines: None,
            follow_path: true,
        })
    }

    /// Join the tailnet through the engines the person listed (`MagicDNS` names or tailnet
    /// addresses), on a device with no Tailscale daemon to read (iOS: the Tailscale app keeps
    /// it to itself). Called again, it lists `hosts` in place of the engines before and looks
    /// them up now. Returns at once, like [`Self::join_tailnet`].
    ///
    /// # Errors
    ///
    /// [`ClientError::Unsupported`] in the demo; [`ClientError::InvalidArgument`] when the
    /// client joined some other way.
    pub fn join_engines(&self, hosts: Vec<String>) -> Result<()> {
        let listed = lock(&self.inner.node)
            .as_ref()
            .and_then(|node| node.engines.clone());
        if let Some(engines) = listed {
            engines.set_hosts(hosts);
            return Ok(());
        }
        let engines = onicode_peer::EngineList::system(hosts);
        self.inner.join(crate::join::Join {
            tailnet: engines.clone(),
            sockets: Arc::new(onicode_peer::SystemSockets),
            config: self.peer_config(),
            engines: Some(engines),
            follow_path: true,
        })
    }

    /// Join `tailnet`, binding through `sockets` as `config` says: a simulated tailnet in
    /// tests. Returns at once, like [`Self::join_tailnet`].
    ///
    /// # Errors
    ///
    /// As [`Self::join_tailnet`].
    pub fn join_on(
        &self,
        tailnet: Arc<dyn onicode_tailnet::Tailnet>,
        sockets: Arc<dyn onicode_peer::Sockets>,
        config: onicode_peer::PeerConfig,
    ) -> Result<()> {
        self.inner.join(crate::join::Join {
            tailnet,
            sockets,
            config,
            engines: None,
            follow_path: false,
        })
    }

    /// The engines listed for [`Self::join_engines`], as cleaned up; empty when the client
    /// reads no list.
    pub fn engine_hosts(&self) -> Vec<String> {
        lock(&self.inner.node)
            .as_ref()
            .and_then(|node| node.engines.as_ref())
            .map(|engines| engines.hosts())
            .unwrap_or_default()
    }

    /// Whether the client holds its links to the tailnet yet.
    pub fn on_tailnet(&self) -> bool {
        self.inner.peer_links.get().is_some()
    }

    /// This device on the tailnet: a viewer, by its app identity, on a port of its own so an
    /// engine on the same machine keeps the engines' port.
    fn peer_config(&self) -> onicode_peer::PeerConfig {
        onicode_peer::PeerConfig::new(
            self.inner.config.device_id.clone(),
            onicode_net::Role::Viewer,
        )
        .on_ephemeral_port()
    }

    /// Takes what the hosts send on the client's links: the datagram copies
    /// of terminal output (its links' handler).
    pub fn peer_handler(&self) -> Arc<dyn onicode_peer::StreamHandler> {
        Arc::new(self.inner.remote_terminals.clone())
    }

    /// App returned to the foreground: resume ticking, look up the listed
    /// engines again, mirror the registry and every open room now, restart PR
    /// watches.
    pub fn on_foreground(&self) {
        self.inner.foreground.store(true, Ordering::Release);
        let away = lock(&self.inner.backgrounded_at)
            .take()
            .map(|at| at.elapsed())
            .unwrap_or_default();
        if !away.is_zero()
            && let Some(links) = self.inner.peer_links.get()
        {
            links.reset();
        }
        self.inner.poke_engines();
        self.inner.links_wake.notify_waiters();
        if let Some(live) = self.inner.live() {
            live.kick();
            self.kick_rooms();
            self.inner.reconcile_change_request_watches();
        }
        self.inner.tick();
    }

    /// App is backgrounding: persist registry + docs now; pause time-driven
    /// work and PR streams.
    pub fn on_background(&self) {
        self.inner.foreground.store(false, Ordering::Release);
        *lock(&self.inner.backgrounded_at) = Some(std::time::Instant::now());
        if let Some(live) = self.inner.live() {
            live.hosts.stop_watches();
            live.flush_registry(&self.inner);
            for core in self.inner.cores() {
                core.flush();
            }
        }
    }

    fn kick_rooms(&self) {
        for core in self.inner.cores() {
            if let Some(room) = core.room() {
                room.kick();
            }
        }
    }

    /// Warm the most relevant sessions (front page order: pinned, sections,
    /// recent), up to [`PRELOAD_CAP`]. Opening is instant (local snapshot);
    /// live rooms dial behind the client's dial cap.
    pub fn preload_sessions(&self) {
        let workspace = self.workspace();
        let front = &workspace.front;
        let candidates = front
            .pinned
            .iter()
            .chain(front.sections.iter().flat_map(|s| s.sessions.iter()))
            .chain(front.recent.iter());
        for row in candidates.take(PRELOAD_CAP) {
            if self.inner.session_core(&row.id).is_none() {
                let _ = self.open_session(&row.id);
            }
        }
    }

    /// Ids of the sessions currently open (warm), most recently used first.
    pub fn open_session_ids(&self) -> Vec<String> {
        let mut cores = self.inner.cores();
        cores.sort_by_key(|c| std::cmp::Reverse(c.touched_ms()));
        cores.iter().map(|c| c.chat_id.clone()).collect()
    }
}

/// Seed helpers shared with the demo host.
pub(crate) fn ms(at: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_millis_opt(at)
        .single()
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_renames_land_in_the_local_replica() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = ClientConfig::new(dir.path());
        config.device_id = "ios-test".into();
        let client = Client::new(
            config,
            Mode::Demo(Default::default()),
            Arc::new(crate::events::NullListener),
        )
        .unwrap();
        let chat_id = client
            .inner
            .workspace
            .mutate(|doc| doc.read_chats())
            .unwrap()
            .into_iter()
            .find(|c| c.parent_chat_id.is_none())
            .expect("seeded chat")
            .id;
        for i in 0..20 {
            client
                .rename_session(&chat_id, &format!("title {i}"))
                .unwrap();
        }
        let title = client
            .inner
            .workspace
            .mutate(|doc| doc.chat(&chat_id).unwrap().unwrap().title);
        assert_eq!(title.as_deref(), Some("title 19"));
    }
}
