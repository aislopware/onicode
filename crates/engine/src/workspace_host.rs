//! `WorkspaceHost` owns the per-user workspace **registry** (docs/registry-sync.md): its local
//! snapshot, the `registry` stream sessions that replicate it engine to engine over the tailnet
//! (`registry_server`), this device's own row, and the typed watch channels the
//! WatchChats/WatchDevices/WatchSessions/WatchSidebarPreferences streams are fed from.
//!
//! Writer discipline: this host writes its own device row, its own session-status rows and rows
//! for chats it hosts; peers' merges refuse owner-only fields from anyone else. Renames and
//! archives are LWW sets accepted from any device (the Mutate surface).
//!
//! Presence: a device is online while this engine holds a live link to it. `lastSeenAt` is a row
//! write on boot and shutdown only; the overlay stamps live peers on each publish, so staying
//! online never grows the registry.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use chrono::Utc;
use onicode_doc::{DeletedSpace, REGISTRY_DOC_ID, RegistryDoc, RegistryRow, Vv};
use onicode_peer::{PeerLinks, StreamHandler};
use onicode_proto::{
    Chat, ChatConfig, Device, HarnessId, Session, SidebarPreferencesState, Space, projectless,
};
use onicode_sync::DocsStore;
use tokio::sync::watch;

use crate::{EngineError, now_ms};

/// How often the device lists republish, so live peers' `lastSeenAt` keeps moving and a gone
/// peer's badge goes stale.
const PRESENCE_INTERVAL_MS: u64 = 15_000;
/// Debounce window for local snapshot saves after a change.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;

/// A new projectless chat's cwd: the scratch folder the creating client
/// already picked, else one named for the chat. Older clients send `~` (or
/// nothing); new chats never run in the host's home.
fn new_projectless_cwd(
    chat_id: &str,
    requested: Option<&str>,
    created_at: chrono::DateTime<Utc>,
) -> String {
    match requested.map(str::trim) {
        Some(cwd) if projectless::is_scratch_cwd(cwd) => cwd.to_string(),
        _ => projectless::scratch_cwd(chat_id, created_at),
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceHostConfig {
    pub device_id: String,
    /// Human name for this device's registry row (hostname by default).
    pub device_name: String,
    /// `std::env::consts::OS`-style platform string.
    pub platform: String,
}

struct WorkspaceHostInner {
    store: Arc<DocsStore>,
    config: WorkspaceHostConfig,
    reg: Arc<Mutex<RegistryDoc>>,
    chats_tx: watch::Sender<Vec<Chat>>,
    devices_tx: watch::Sender<Vec<Device>>,
    sessions_tx: watch::Sender<Vec<Session>>,
    spaces_tx: watch::Sender<Vec<Space>>,
    sidebar_preferences_tx: watch::Sender<SidebarPreferencesState>,
    /// Bumped on every registry change (local write or merged rows): drives republish, the
    /// snapshot debounce in `workspace_task`, and every `registry` session's live push.
    changed_tx: watch::Sender<u64>,
    /// When this engine last held a live link to each device (ms): the presence overlay.
    presence_seen: Mutex<HashMap<String, i64>>,
    /// Chats whose row this host claimed for a command that outran their
    /// `createChat` ([`WorkspaceHost::claim_chat`]).
    claimed: Mutex<HashSet<String>>,
    /// The engine's peer links, once the tailnet is up.
    links: OnceLock<Arc<PeerLinks>>,
    /// Peers whose whole registry this replica merged this process.
    caught_up: Mutex<HashSet<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A chat row's stored harness-native session (see
/// [`WorkspaceHost::chat_harness_session`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHarnessSession {
    /// Empty = the "do not resume" tombstone.
    pub session_id: String,
    pub cwd: Option<String>,
    /// The harness that minted `session_id`. `None` on rows that predate
    /// the tag.
    pub harness: Option<HarnessId>,
}

#[derive(Clone)]
pub struct WorkspaceHost {
    inner: Arc<WorkspaceHostInner>,
}

/// A [`WorkspaceHost`] that does not keep it alive: the stream handler holds this, since the
/// peer links that call it live as long as the engine.
#[derive(Clone)]
pub(crate) struct WeakWorkspaceHost(Weak<WorkspaceHostInner>);

impl WeakWorkspaceHost {
    pub(crate) fn upgrade(&self) -> Option<WorkspaceHost> {
        self.0.upgrade().map(|inner| WorkspaceHost { inner })
    }
}

/// The registry as one side of a `registry` session with one peer.
pub(crate) struct PeerReplica {
    inner: Arc<WorkspaceHostInner>,
    peer: String,
}

impl onicode_sync::registry::Replica for PeerReplica {
    fn read<T>(&self, f: impl FnOnce(&RegistryDoc) -> T) -> T {
        f(&lock(&self.inner.reg))
    }

    fn merge(&self, rows: &[RegistryRow], vv: Option<&Vv>) {
        let changed = {
            let mut doc = lock(&self.inner.reg);
            let before = doc.vv().clone();
            doc.merge_remote(rows, vv) || *doc.vv() != before
        };
        // A raised vv alone must still reach the snapshot and the other sessions.
        if changed {
            self.inner.bump_changed();
        }
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.inner.changed_tx.subscribe()
    }

    fn caught_up(&self) {
        lock(&self.inner.caught_up).insert(self.peer.clone());
        self.inner.publish();
    }
}

impl WorkspaceHost {
    /// Loads (or inits) the registry, upserts this device's row and starts the change-driven
    /// task. Replication starts with [`Self::set_links`].
    pub fn open(store: Arc<DocsStore>, config: WorkspaceHostConfig) -> Result<Self, EngineError> {
        let mut doc = match store.load_snapshot(REGISTRY_DOC_ID)? {
            Some(bytes) => RegistryDoc::from_bytes(&bytes, &config.device_id)
                .map_err(|e| EngineError::Other(format!("registry snapshot load failed: {e}")))?,
            None => RegistryDoc::new(&config.device_id),
        };
        // Boot: upsert our own device row. A user-set name (RenameDevice is LWW from
        // any device) survives restarts. The old fallback sentinel is repaired with
        // the platform-resolved name because it was never a user-selected name.
        let now = Utc::now();
        let existing = doc
            .read_devices()?
            .into_iter()
            .find(|d| d.id == config.device_id);
        doc.upsert_device(&Device {
            id: config.device_id.clone(),
            name: device_name_on_boot(
                existing.as_ref().map(|device| device.name.as_str()),
                &config.device_name,
            ),
            platform: config.platform.clone(),
            last_seen_at: Some(now),
            // First registration stamps `createdAt`; restarts keep the original
            // (the Devices page "Added …" fragment).
            created_at: existing.and_then(|d| d.created_at).or(Some(now)),
            // Every boot restamps the running binary's version (fleet staleness
            // on the Devices page; workspace version — same for every crate).
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            cursor_sdk_version: Some(onicode_harness::CursorHarness::sdk_version().into()),
            capabilities: onicode_proto::capabilities::current(),
        })?;

        let state = doc.read_all()?;
        let (chats_tx, _) = watch::channel(state.chats);
        let (devices_tx, _) = watch::channel(state.devices);
        let (sessions_tx, _) = watch::channel(state.sessions);
        let (spaces_tx, _) = watch::channel(state.spaces);
        let preferences = doc.sidebar_preferences();
        let (sidebar_preferences_tx, _) = watch::channel(SidebarPreferencesState {
            revision: 0,
            synced: false,
            initialized: preferences.is_some(),
            sections: preferences
                .as_ref()
                .map(|p| p.sections.clone())
                .unwrap_or_default(),
            pinned_session_ids: preferences
                .map(|preferences| preferences.pinned_session_ids)
                .unwrap_or_default(),
        });
        let (changed_tx, changed_rx) = watch::channel(0u64);

        let host = Self {
            inner: Arc::new(WorkspaceHostInner {
                store,
                config,
                reg: Arc::new(Mutex::new(doc)),
                chats_tx,
                devices_tx,
                sessions_tx,
                spaces_tx,
                sidebar_preferences_tx,
                changed_tx,
                presence_seen: Mutex::default(),
                claimed: Mutex::default(),
                links: OnceLock::new(),
                caught_up: Mutex::default(),
            }),
        };
        // Reclaim the free pages of a bloated store off the boot path. The store gates
        // the VACUUM on its own thresholds, so ordinary boots only checkpoint.
        let store = host.inner.store.clone();
        tokio::task::spawn_blocking(move || store.reclaim_free_space());
        tokio::spawn(workspace_task(Arc::downgrade(&host.inner), changed_rx));
        Ok(host)
    }

    /// Replicates the registry with the engines `links` reach, and derives presence from the
    /// live links. Call once, when the tailnet is up.
    pub fn set_links(&self, links: Arc<PeerLinks>) {
        if self.inner.links.set(links.clone()).is_err() {
            return;
        }
        tokio::spawn(crate::registry_server::keep(self.downgrade()));
        let weak = Arc::downgrade(&self.inner);
        let mut changes = links.subscribe_changes();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let Some(inner) = weak.upgrade() else { return };
                inner.publish_lists(true);
            }
        });
    }

    /// Serves the `registry` streams peers open.
    pub fn registry_server(&self) -> Arc<dyn StreamHandler> {
        Arc::new(crate::registry_server::RegistryServer(self.downgrade()))
    }

    pub(crate) fn downgrade(&self) -> WeakWorkspaceHost {
        WeakWorkspaceHost(Arc::downgrade(&self.inner))
    }

    pub(crate) fn links(&self) -> Option<Arc<PeerLinks>> {
        self.inner.links.get().cloned()
    }

    /// This registry as one side of a `registry` session with `peer`.
    pub(crate) fn replica_for(&self, peer: &str) -> PeerReplica {
        PeerReplica {
            inner: self.inner.clone(),
            peer: peer.to_owned(),
        }
    }

    /// Test seam: a foreign device row as if it arrived by replication, so written as that
    /// device (a device row is its own device's to write).
    #[doc(hidden)]
    pub fn upsert_device_row(&self, device: &Device) {
        if let Err(err) =
            self.mutate(|doc| doc.write_as(&device.id, |doc| doc.upsert_device(device)))
        {
            tracing::warn!(error = %err, "device row upsert failed");
        }
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    /// Whether this replica's rows can be taken as the whole truth: it merged some peer's whole
    /// registry this process, or no other device ever wrote to it.
    /// Destructive repair (the orphan sweep) gates on this: a replica that has heard from no
    /// peer may be arbitrarily behind, and a row absent from it is no evidence of a deletion.
    pub fn registry_synced(&self) -> bool {
        self.inner.registry_synced()
    }

    /// The peers whose whole registry this replica merged this process, sorted.
    pub fn registry_peers(&self) -> Vec<String> {
        let mut peers: Vec<String> = lock(&self.inner.caught_up).iter().cloned().collect();
        peers.sort();
        peers
    }

    // ── registry access helpers ─────────────────────────────────────────────

    /// Run a mutation under the registry lock, then wake the publish/persist
    /// task and every `registry` session, which pushes the write to its peer.
    fn mutate<R>(&self, f: impl FnOnce(&mut RegistryDoc) -> R) -> R {
        let result = f(&mut lock(&self.inner.reg));
        self.inner.bump_changed();
        result
    }

    fn read<R>(&self, f: impl FnOnce(&RegistryDoc) -> R) -> R {
        f(&lock(&self.inner.reg))
    }

    /// The chat row as currently known (overlay view).
    pub fn chat(&self, chat_id: &str) -> Result<Option<Chat>, EngineError> {
        Ok(self.read(|doc| doc.chat(chat_id))?)
    }

    /// Whether `chat_id`'s row was deleted (a tombstone, not an unknown id).
    pub fn chat_deleted(&self, chat_id: &str) -> bool {
        self.read(|doc| doc.chat_deleted(chat_id))
    }

    /// Whether `chat_id`'s parent is unknown here yet: this host claimed its
    /// row for a command that arrived first ([`Self::claim_chat`]), and its
    /// `createChat` hasn't landed ([`RegistryDoc::chat_parent_recorded`]).
    pub fn parent_unknown(&self, chat_id: &str) -> bool {
        lock(&self.inner.claimed).contains(chat_id)
            && !self.read(|doc| doc.chat_parent_recorded(chat_id))
    }

    /// The space row as currently known (overlay view).
    pub fn space(&self, space_id: &str) -> Result<Option<Space>, EngineError> {
        Ok(self.read(|doc| doc.space(space_id))?)
    }

    pub fn read_chats(&self) -> Result<Vec<Chat>, EngineError> {
        Ok(self.read(|doc| doc.read_chats())?)
    }

    pub fn read_devices(&self) -> Result<Vec<Device>, EngineError> {
        Ok(self.read(|doc| doc.read_devices())?)
    }

    pub fn read_sessions(&self) -> Result<Vec<Session>, EngineError> {
        Ok(self.read(|doc| doc.read_sessions())?)
    }

    pub fn change_sidebar_pin(
        &self,
        change: &onicode_proto::SidebarPinChange,
    ) -> Result<(), EngineError> {
        let synced = self.registry_synced();
        self.mutate(|doc| {
            if !synced && doc.sidebar_preferences().is_none() {
                return Err(EngineError::Other("Pins are still syncing".into()));
            }
            Ok(doc.change_sidebar_pin(change)?)
        })?;
        if matches!(
            change,
            onicode_proto::SidebarPinChange::Section {
                change: onicode_proto::SidebarSectionChange::Import { .. }
            }
        ) {
            // The UI removes its legacy copy only after this acknowledgement.
            self.inner.persist_snapshot()?;
        }
        Ok(())
    }

    // ── watches (WatchChats / WatchDevices / merged WatchSessions) ──────────

    pub fn watch_chats(&self) -> watch::Receiver<Vec<Chat>> {
        self.inner.chats_tx.subscribe()
    }

    pub fn watch_devices(&self) -> watch::Receiver<Vec<Device>> {
        self.inner.devices_tx.subscribe()
    }

    /// Raw workspace session-status rows (all devices').
    pub fn watch_session_rows(&self) -> watch::Receiver<Vec<Session>> {
        self.inner.sessions_tx.subscribe()
    }

    pub fn watch_spaces(&self) -> watch::Receiver<Vec<Space>> {
        self.inner.spaces_tx.subscribe()
    }

    pub fn watch_sidebar_preferences(&self) -> watch::Receiver<SidebarPreferencesState> {
        self.inner.sidebar_preferences_tx.subscribe()
    }

    /// Mutation acknowledgements and watches share the same ordered revision.
    pub fn sidebar_preferences_snapshot(&self) -> SidebarPreferencesState {
        let synced = self.registry_synced();
        let doc = lock(&self.inner.reg);
        self.inner.publish_sidebar_preferences(&doc, synced);
        self.inner.sidebar_preferences_tx.borrow().clone()
    }

    /// WatchSessions source: remote devices' rows from the registry merged with
    /// this engine's live status watch (the local view is fresher for our own runs).
    pub fn merged_sessions_watch(
        &self,
        local: watch::Receiver<Vec<Session>>,
    ) -> watch::Receiver<Vec<Session>> {
        let mut rows = self.watch_session_rows();
        let mut local = local;
        let device_id = self.inner.config.device_id.clone();
        let (tx, rx) = watch::channel(merge_sessions(&device_id, &rows.borrow(), &local.borrow()));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = rows.changed() => if changed.is_err() { break },
                    changed = local.changed() => if changed.is_err() { break },
                }
                let merged = merge_sessions(
                    &device_id,
                    &rows.borrow_and_update(),
                    &local.borrow_and_update(),
                );
                if tx.send(merged).is_err() {
                    break; // no receivers left
                }
            }
        });
        rx
    }

    // ── chat ownership ──────────────────────────────────────────────────────

    /// Writer discipline: the chat's host is its row's `deviceId`. Unknown chats
    /// are claimable — the first run command claims them via [`Self::claim_chat`].
    pub fn is_host(&self, chat_id: &str) -> bool {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(Some(chat)) => chat.device_id == self.inner.config.device_id,
            Ok(None) => true,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                true
            }
        }
    }

    /// Claim-on-first-command: create the chat row under OUR device id when a run
    /// command arrives for a chat with no row yet. No-op when the row exists.
    ///
    /// The claim is a PARTIAL row write (identity/cwd/space only): the command
    /// plane is nudged and outruns the registry channel, so the client's
    /// `createChat` for the same chat routinely arrives AFTER the claim with
    /// older clocks — fields the claim never wrote (`config`, `title`) must
    /// still land then.
    ///
    /// Resolve project cwd claims to a space. A projectless target (`~` from
    /// older clients, or a scratch folder) mints no project when the run
    /// outruns createChat on the registry channel; the new row gets its own
    /// scratch folder either way.
    pub fn claim_chat(&self, chat_id: &str, cwd: Option<&str>) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            return Ok(());
        }
        let now = Utc::now();
        let (cwd, space_id) = match cwd {
            Some(cwd) if projectless::is_projectless_cwd(Some(cwd)) => {
                (Some(new_projectless_cwd(chat_id, Some(cwd), now)), None)
            }
            Some(cwd) => (Some(cwd.to_string()), Some(self.space_for_path(cwd)?)),
            None => (None, None),
        };
        self.mutate(|doc| doc.claim_chat(chat_id, cwd.as_deref(), space_id.as_deref(), now));
        lock(&self.inner.claimed).insert(chat_id.to_owned());
        Ok(())
    }

    /// An own-device space whose path matches, else one at the path's parent
    /// checkout root, else a freshly created one at that root.
    ///
    /// A linked-worktree cwd resolves to the checkout root FIRST: claiming at
    /// the worktree path itself minted a phantom sidebar space named after the
    /// worktree folder ("clever-ember") next to the project's real space.
    fn space_for_path(&self, path: &str) -> Result<String, EngineError> {
        let device_id = &self.inner.config.device_id;
        let spaces = self.read(|doc| doc.read_spaces())?;
        if let Some(space) = spaces
            .iter()
            .find(|s| s.device_id == *device_id && s.path == path)
        {
            return Ok(space.id.clone());
        }
        let root = linked_worktree_root(std::path::Path::new(path));
        if let Some(root) = root.as_deref()
            && let Some(space) = spaces.iter().find(|s| {
                s.device_id == *device_id
                    && (s.path == root || same_file::is_same_file(&s.path, root).unwrap_or(false))
            })
        {
            return Ok(space.id.clone());
        }
        let space = Space {
            id: crate::new_id(),
            device_id: device_id.clone(),
            path: root.unwrap_or_else(|| path.to_string()),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            repository_id: None,
            github_repo: None,
            icon: None,
            created_at: Utc::now(),
        };
        self.mutate(|doc| doc.upsert_space(&space))?;
        Ok(space.id)
    }

    /// The chat's configured harness/model row, when present (RunRequest harness
    /// selection; callers fall back to the engine default).
    pub fn chat_config(&self, chat_id: &str) -> Option<ChatConfig> {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(chat) => chat.and_then(|c| c.config),
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                None
            }
        }
    }

    // ── host-side row writes ────────────────────────────────────────────────

    /// Sidebar freshness on message persist: preview = first 120 chars of the last
    /// message's text. Claims the row first so a pre-workspace chat gains one.
    pub fn note_message(&self, chat_id: &str, text: &str) {
        // Annotation tokens carry their element as hex; previews read the label.
        let text = onicode_proto::annotation::annotation_display(text);
        let preview: String = text.chars().take(120).collect();
        let result = self.claim_chat(chat_id, None).and_then(|_| {
            self.mutate(|doc| doc.set_chat_last_message(chat_id, &preview, Utc::now()))
                .map_err(EngineError::from)
        });
        if let Err(err) = result {
            tracing::warn!(chat = %chat_id, error = %err, "registry last-message write failed");
        }
    }

    /// Resume continuity: stamp the chat row with the harness-native session id
    /// of its latest run and the cwd it was created under. An empty `session_id`
    /// tombstones the row ("do not resume" after a rejected resume). Best-effort:
    /// a missing chat row (claim happens on first command) just returns.
    ///
    /// `harness` names the provider that minted `session_id`: an id is only
    /// meaningful to that harness, and resume checks the tag.
    pub fn set_chat_harness_session(
        &self,
        chat_id: &str,
        session_id: &str,
        cwd: &str,
        harness: Option<HarnessId>,
    ) {
        let slug = harness.map(HarnessId::as_str);
        match self.mutate(|doc| doc.set_chat_harness_session(chat_id, session_id, cwd, slug)) {
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry harness-session write failed");
            }
        }
    }

    /// The chat row's stored harness session, if stamped. The empty-string
    /// tombstone passes through — callers must treat it as "explicitly no
    /// resume" (and must NOT fall back to older sources).
    pub fn chat_harness_session(&self, chat_id: &str) -> Option<StoredHarnessSession> {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(chat) => {
                let chat = chat?;
                let id = chat.harness_session_id?;
                Some(StoredHarnessSession {
                    session_id: id,
                    cwd: chat.harness_session_cwd,
                    harness: chat
                        .harness_session_harness
                        .as_deref()
                        .and_then(HarnessId::from_slug),
                })
            }
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                None
            }
        }
    }

    /// Session-status row upsert (sessions engine transitions land here too, in
    /// addition to the local watch channel).
    pub fn record_session(&self, session: &Session) {
        if let Err(err) = self.mutate(|doc| doc.upsert_session(session)) {
            tracing::warn!(chat = %session.chat_id, error = %err, "registry session write failed");
        }
    }

    /// Test seam: `session` as its owner wrote it after the chat moved there, as if it arrived
    /// by replication (a session row is its owner's to write, and its owner never changes).
    #[doc(hidden)]
    pub fn record_foreign_session(&self, session: &Session) {
        let result = self.mutate(|doc| {
            doc.delete_session(&session.chat_id);
            doc.write_as(&session.device_id, |doc| doc.upsert_session(session))
        });
        if let Err(err) = result {
            tracing::warn!(chat = %session.chat_id, error = %err, "registry session write failed");
        }
    }

    // ── Mutate surface (LWW writes accepted from any device) ────────────────

    /// Create a chat, usually *in a project*: the project fixes the host device
    /// and base cwd (`cwd` override = an isolated-worktree path). With no
    /// `space_id` the chat is project-less: `device_id` picks the host and the
    /// cwd defaults to the chat's own scratch folder (expanded and created
    /// host-side when the run spawns).
    pub fn create_chat(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
    ) -> Result<(), EngineError> {
        self.create_chat_with_parent(chat_id, space_id, device_id, config, cwd, None)
    }

    /// [`create_chat`](Self::create_chat) recording the creating chat
    /// (`parentChatId`) — the Onicode MCP's orchestration link.
    pub fn create_chat_with_parent(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
        parent_chat_id: Option<String>,
    ) -> Result<(), EngineError> {
        self.create_chat_of_kind(
            chat_id,
            space_id,
            device_id,
            config,
            cwd,
            parent_chat_id.map(|parent| (parent, None)),
        )
    }

    /// [`create_chat_with_parent`](Self::create_chat_with_parent) recording
    /// what the child is to its parent (a side chat or a subagent):
    /// `parent` is the parent chat's id and the child's kind.
    pub fn create_chat_of_kind(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
        parent: Option<(String, Option<onicode_proto::ChildKind>)>,
    ) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            return Ok(()); // idempotent: optimistic client retries never duplicate
        }
        let space = match space_id {
            Some(space_id) => match self.read(|doc| doc.space(space_id))? {
                Some(space) => Some(space),
                None => return Err(EngineError::Other(format!("no such space: {space_id}"))),
            },
            None => None,
        };
        let host_device = match (&space, device_id) {
            (Some(space), _) => space.device_id.clone(),
            (None, Some(device_id)) => device_id.to_string(),
            (None, None) => {
                return Err(EngineError::Other(
                    "createChat needs a spaceId or a deviceId".into(),
                ));
            }
        };
        let created_at = Utc::now();
        let cwd = match &space {
            Some(space) => cwd.unwrap_or_else(|| space.path.clone()),
            None if projectless::is_projectless_cwd(cwd.as_deref()) => {
                if self.hosts_scratch_folders(&host_device)? {
                    new_projectless_cwd(chat_id, cwd.as_deref(), created_at)
                } else {
                    "~".to_string()
                }
            }
            None => cwd.unwrap_or_default(),
        };
        let (parent, child_kind) = match parent {
            Some((parent, kind)) if !parent.trim().is_empty() => (Some(parent), kind),
            _ => (None, None),
        };
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                id: chat_id.to_string(),
                device_id: host_device.clone(),
                title: None,
                archived: false,
                cwd: Some(cwd),
                branch: None,
                checkout_id: None,
                source_context: None,
                config,
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                harness_session_harness: None,
                space_id: space.as_ref().map(|s| s.id.clone()),
                last_seen_at: None,
                // (What a child is to its parent: only a child has a kind.)
                child_kind,
                parent_chat_id: parent,
            })
        })?;
        Ok(())
    }

    /// Whether `device_id` creates scratch folders for projectless chats: this
    /// engine does; a peer must advertise it (older hosts only expand `~`).
    fn hosts_scratch_folders(&self, device_id: &str) -> Result<bool, EngineError> {
        if device_id == self.inner.config.device_id {
            return Ok(true);
        }
        Ok(self.read_devices()?.iter().any(|device| {
            device.id == device_id
                && device
                    .capabilities
                    .iter()
                    .any(|c| c == onicode_proto::capabilities::PROJECTLESS_SCRATCH_V1)
        }))
    }

    // ── spaces (Mutate surface + owner stamps) ──────────────────────────────

    /// Create a space (any device). Idempotent by id; a live duplicate of the
    /// same `(deviceId, path)` is a no-op backstop (the UI reuses via
    /// WatchSpaces). `git_detected` is seeded from the picker's FolderEntry;
    /// the owning device's SpacesSync re-verifies.
    pub fn create_space(
        &self,
        space_id: &str,
        device_id: &str,
        path: &str,
        name: Option<String>,
        git_detected: bool,
    ) -> Result<(), EngineError> {
        let spaces = self.read(|doc| doc.read_spaces())?;
        if spaces
            .iter()
            .any(|s| s.id == space_id || (s.device_id == device_id && s.path == path))
        {
            return Ok(());
        }
        self.mutate(|doc| {
            doc.upsert_space(&Space {
                id: space_id.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                name,
                git_detected,
                git_checked_at: None,
                checkout_id: None,
                repository_id: None,
                github_repo: None,
                icon: None,
                created_at: Utc::now(),
            })
        })?;
        Ok(())
    }

    pub fn rename_space(&self, space_id: &str, name: Option<&str>) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_space(space_id, name))?)
    }

    pub fn set_space_icon(
        &self,
        space_id: &str,
        icon: Option<&onicode_proto::SpaceIcon>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_space_icon(space_id, icon))?)
    }

    /// Hard-delete a space and its chats (registry cascade — one atomic batch).
    /// The caller (rpc layer) tears down live runs / doc-host handles for the
    /// returned chat ids.
    pub fn delete_space(&self, space_id: &str) -> Result<DeletedSpace, EngineError> {
        Ok(self.mutate(|doc| doc.delete_space(space_id))?)
    }

    /// Synced seen marker (any device; LWW + monotonic guard in the doc layer).
    pub fn mark_chat_seen(
        &self,
        chat_id: &str,
        at: chrono::DateTime<Utc>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_seen(chat_id, at))?)
    }

    /// Owner-only git stamp (SpacesSync). Refuses rows owned by another device.
    pub fn set_space_git(
        &self,
        space_id: &str,
        detected: bool,
        checkout_id: Option<&str>,
        repository_id: Option<&str>,
        github_repo: Option<&str>,
    ) -> Result<bool, EngineError> {
        match self.read(|doc| doc.space(space_id))? {
            Some(space) if space.device_id == self.inner.config.device_id => {
                Ok(self.mutate(|doc| {
                    doc.set_space_git(
                        space_id,
                        detected,
                        checkout_id,
                        repository_id,
                        github_repo,
                        Utc::now(),
                    )
                })?)
            }
            Some(space) => {
                tracing::warn!(
                    space = %space_id, owner = %space.device_id,
                    "refusing git stamp on space owned by another device"
                );
                Ok(false)
            }
            None => Ok(false),
        }
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, EngineError> {
        Ok(self.read(|doc| doc.read_spaces())?)
    }

    pub fn rename_chat(&self, chat_id: &str, title: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_chat(chat_id, title))?)
    }

    /// Backdate a chat's activity timestamps (epoch ms). Returns false when
    /// the chat doesn't exist.
    pub fn set_chat_activity(
        &self,
        chat_id: &str,
        last_message_at: Option<i64>,
        created_at: Option<i64>,
    ) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        if let Some(ms) = last_message_at {
            chat.last_message_at = chrono::DateTime::<Utc>::from_timestamp_millis(ms);
        }
        if let Some(ms) = created_at
            && let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
        {
            chat.created_at = at;
        }
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Re-home a chat to another device (tooling/seeds; a future device
    /// migration flow will drive this). Returns false when the chat doesn't
    /// exist.
    pub fn set_chat_host(&self, chat_id: &str, device_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rehome_chat(chat_id, device_id))?)
    }

    /// Upsert a chat row copied verbatim from another profile (local→synced
    /// import). Same write path as every live mutation, so the row persists
    /// and replicates like any other.
    pub fn import_chat_row(&self, chat: &Chat) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_chat(chat))?)
    }

    /// Upsert a space row copied verbatim from another profile (local→synced
    /// import).
    pub fn import_space_row(&self, space: &Space) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_space(space))?)
    }

    pub fn set_chat_archived(&self, chat_id: &str, archived: bool) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_archived(chat_id, archived))?)
    }

    /// LWW full-config replace on the chat row (onicode `SetChatConfig` — the
    /// composer's mid-session model/reasoning/options changes). Returns false
    /// when the chat doesn't exist.
    pub fn set_chat_config(&self, chat_id: &str, config: &ChatConfig) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_config(chat_id, config))?)
    }

    /// Tombstone: removes the chats (and session-status) row; the per-chat session
    /// doc remains untouched.
    pub fn delete_chat(&self, chat_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.delete_chat(chat_id))?)
    }

    pub fn rename_device(&self, device_id: &str, name: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_device(device_id, name))?)
    }

    // ── git metadata (diff-sync host writes) ────────────────────────────────

    /// HEAD-watcher reconciliation: the branch checked out at the chat's cwd.
    pub fn set_chat_branch(&self, chat_id: &str, branch: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_branch(chat_id, branch))?)
    }

    pub fn set_chat_source_context(
        &self,
        chat_id: &str,
        context: &onicode_proto::ConversationSourceContext,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_source_context(chat_id, context))?)
    }

    /// Retarget a chat onto another folder (mid-session switch to an existing
    /// worktree). Resume is cwd-scoped — the next run there starts fresh.
    pub fn set_chat_cwd(&self, chat_id: &str, cwd: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_cwd(chat_id, cwd))?)
    }

    /// Canonical checkout identity for the chat's cwd (diff grouping key).
    pub fn set_chat_checkout(&self, chat_id: &str, checkout_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_checkout(chat_id, checkout_id))?)
    }

    // ── persistence / teardown ──────────────────────────────────────────────

    /// Persist the snapshot now (shutdown path; bypasses the debounce).
    pub fn flush(&self) {
        self.inner.save_snapshot();
    }

    /// Shutdown: stamp our `lastSeenAt` (the only periodic-ish row write besides
    /// boot) and flush the snapshot.
    pub fn shutdown(&self) {
        let now = Utc::now();
        let device_id = self.inner.config.device_id.clone();
        if let Err(err) = self.mutate(|doc| doc.set_device_last_seen(&device_id, now)) {
            tracing::warn!(error = %err, "device lastSeenAt stamp failed");
        }
        self.inner.save_snapshot();
    }
}

impl WorkspaceHostInner {
    fn bump_changed(&self) {
        self.changed_tx.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn publish(&self) {
        self.publish_lists(false);
    }

    fn publish_lists(&self, clock_tick: bool) {
        let registry_synced = self.registry_synced();
        let snapshot = {
            let mut doc = lock(&self.reg);
            match doc.reconcile_sidebar_pins(registry_synced) {
                Ok(true) => {
                    // Persist and replicate the cleanup like a user write.
                    self.bump_changed();
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "sidebar pin cleanup failed"),
            }
            self.publish_sidebar_preferences(&doc, registry_synced);
            doc.read_all()
        };
        match snapshot {
            Ok(mut state) => {
                self.overlay_presence(&mut state.devices, clock_tick);
                // Retain the latest value even with no subscribers, but don't
                // wake every list for unrelated registry/presence changes.
                publish_if_changed(&self.chats_tx, state.chats);
                // The periodic presence tick also drives time-based expiry.
                // Unrelated registry writes need no synthetic device event.
                if clock_tick {
                    self.devices_tx.send_replace(state.devices);
                } else {
                    publish_if_changed(&self.devices_tx, state.devices);
                }
                publish_if_changed(&self.sessions_tx, state.sessions);
                publish_if_changed(&self.spaces_tx, state.spaces);
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry read failed");
            }
        }
    }

    /// Called under the registry lock so publications cannot overtake one another.
    fn publish_sidebar_preferences(&self, doc: &RegistryDoc, synced: bool) {
        let preferences = doc.sidebar_preferences();
        let initialized = preferences.is_some();
        let sections = preferences
            .as_ref()
            .map(|p| p.sections.clone())
            .unwrap_or_default();
        let pins = preferences
            .map(|p| p.pinned_session_ids)
            .unwrap_or_default();
        self.sidebar_preferences_tx.send_if_modified(|current| {
            // Readiness is sticky for this host. A caller that sampled stats
            // before another publisher acquired the lock must not regress it.
            let synced = synced || current.synced;
            if current.synced == synced
                && current.initialized == initialized
                && current.pinned_session_ids == pins
                && current.sections == sections
            {
                return false;
            }
            *current = SidebarPreferencesState {
                revision: current.revision + 1,
                synced,
                initialized,
                pinned_session_ids: pins,
                sections,
            };
            true
        });
    }

    /// Stamps each device's `lastSeenAt` with the last time this engine held a live link to it.
    /// The row is written on boot and shutdown only, so without this every peer would look
    /// offline soon after its boot. `refresh` re-reads the links (the presence tick, a link
    /// change); other publishes reuse what was seen, so unrelated writes leave the list quiet.
    fn overlay_presence(&self, devices: &mut [Device], refresh: bool) {
        let mut seen = lock(&self.presence_seen);
        if refresh && let Some(links) = self.links.get() {
            let now = now_ms();
            for link in links.links() {
                seen.insert(link.device, now);
            }
        }
        for device in devices.iter_mut() {
            let Some(at) = seen
                .get(&device.id)
                .and_then(|&ms| chrono::DateTime::<Utc>::from_timestamp_millis(ms))
            else {
                continue;
            };
            if device.last_seen_at.is_none_or(|prev| prev < at) {
                device.last_seen_at = Some(at);
            }
        }
    }

    fn registry_synced(&self) -> bool {
        if !lock(&self.caught_up).is_empty() {
            return true;
        }
        lock(&self.reg)
            .read_devices()
            .is_ok_and(|devices| devices.iter().all(|d| d.id == self.config.device_id))
    }

    fn save_snapshot(&self) {
        if let Err(error) = self.persist_snapshot() {
            tracing::warn!(%error, "registry snapshot save failed");
        }
    }

    /// Returns the snapshot's size in bytes.
    fn persist_snapshot(&self) -> Result<usize, EngineError> {
        // Keep export and disk write serialized: an older background snapshot
        // must not overwrite an acknowledged migration's durable snapshot.
        let doc = lock(&self.reg);
        let bytes = doc.to_bytes()?;
        self.store
            .save_snapshot(REGISTRY_DOC_ID, &bytes)
            .map_err(|error| {
                EngineError::Other(format!("registry snapshot save failed: {error}"))
            })?;
        Ok(bytes.len())
    }
}

/// The parent checkout root of a linked git worktree: `<path>/.git` is a FILE
/// containing `gitdir: <root>/.git/worktrees/<name>`. `None` for a primary
/// checkout (`.git` is a directory), a non-repo folder, or any other layout
/// (bare-repo worktrees have no `<root>` working copy to attribute to). Pure
/// fs reads — no git subprocess; this runs on the synchronous claim path.
pub(crate) fn linked_worktree_root(path: &std::path::Path) -> Option<String> {
    let gitfile = path.join(".git");
    if !std::fs::metadata(&gitfile).ok()?.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&gitfile).ok()?;
    let target = content
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let mut target = std::path::PathBuf::from(target);
    if target.is_relative() {
        // Rare (`worktree.useRelativePaths`); canonicalize resolves the
        // `../..` hops against the real filesystem.
        target = std::fs::canonicalize(path.join(target)).ok()?;
    }
    let worktrees = target.parent()?;
    let dot_git = worktrees.parent()?;
    if worktrees.file_name()? != "worktrees" || dot_git.file_name()? != ".git" {
        return None;
    }
    Some(dot_git.parent()?.to_string_lossy().into_owned())
}

/// Local live statuses win for this device's chats; every other device's rows come
/// from the registry. Sorted by chat id (stable stream output).
fn merge_sessions(device_id: &str, rows: &[Session], local: &[Session]) -> Vec<Session> {
    let mut merged: std::collections::HashMap<String, Session> = rows
        .iter()
        .filter(|s| s.device_id != device_id)
        .map(|s| (s.chat_id.clone(), s.clone()))
        .collect();
    for session in local {
        merged.insert(session.chat_id.clone(), session.clone());
    }
    let mut list: Vec<Session> = merged.into_values().collect();
    list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    list
}

/// Background task: reacts to registry changes (local writes and merged
/// rows) by re-publishing the watch channels and debouncing snapshots,
/// and refreshes presence every [`PRESENCE_INTERVAL_MS`]. Holds only a weak
/// handle so a dropped host tears the task down.
async fn workspace_task(weak: Weak<WorkspaceHostInner>, mut changed_rx: watch::Receiver<u64>) {
    let mut presence =
        tokio::time::interval(std::time::Duration::from_millis(PRESENCE_INTERVAL_MS));
    presence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    presence.tick().await; // consume the immediate first tick
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // host (and its change sender) is gone
                }
                let Some(inner) = weak.upgrade() else { break };
                inner.publish();
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                let Some(inner) = weak.upgrade() else { break };
                if let Err(error) = tokio::task::spawn_blocking(move || inner.save_snapshot()).await {
                    tracing::warn!(%error, "registry snapshot worker failed");
                }
            }
            _ = presence.tick() => {
                let Some(inner) = weak.upgrade() else { break };
                // Live peers' `lastSeenAt` moves on, and a gone peer's goes stale for the
                // watchers (the UI online dot, "host offline" hints).
                inner.publish_lists(true);
            }
        }
    }
}

fn publish_if_changed<T: PartialEq>(sender: &watch::Sender<T>, value: T) {
    sender.send_if_modified(|current| {
        if *current == value {
            false
        } else {
            *current = value;
            true
        }
    });
}

fn device_name_on_boot(existing_name: Option<&str>, detected_name: &str) -> String {
    existing_name
        .filter(|name| {
            let name = name.trim();
            !name.is_empty() && name != crate::LEGACY_UNKNOWN_DEVICE_NAME
        })
        .unwrap_or(detected_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{device_name_on_boot, linked_worktree_root};

    #[tokio::test]
    async fn presence_publish_keeps_unchanged_lists_quiet_and_late_subscribers_current() {
        use super::*;

        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
            },
        )
        .unwrap();
        let chats = host.watch_chats();
        let sessions = host.watch_session_rows();
        let mut spaces = host.watch_spaces();
        let devices = host.watch_devices();

        host.inner.publish();
        assert!(!chats.has_changed().unwrap());
        assert!(!sessions.has_changed().unwrap());
        assert!(!spaces.has_changed().unwrap());
        assert!(!devices.has_changed().unwrap());
        host.inner.publish_lists(true);
        assert!(
            devices.has_changed().unwrap(),
            "presence expiry still ticks"
        );
        assert!(!chats.has_changed().unwrap());
        assert!(!sessions.has_changed().unwrap());
        assert!(!spaces.has_changed().unwrap());

        host.create_space("space", "test-device", "/project", None, false)
            .unwrap();
        host.inner.publish();
        assert!(
            spaces.has_changed().unwrap(),
            "real changes must wake readers"
        );
        assert_eq!(spaces.borrow_and_update()[0].id, "space");
        assert!(!chats.has_changed().unwrap());
        assert!(!sessions.has_changed().unwrap());
        drop(spaces);

        host.rename_space("space", Some("Renamed")).unwrap();
        host.inner.publish();
        assert_eq!(
            host.watch_spaces().borrow()[0].name.as_deref(),
            Some("Renamed")
        );
    }

    /// A claimed row doesn't know its parent until the chat's `createChat`
    /// lands, which records `parentChatId` even when it has none; a row this
    /// host never claimed always knows it.
    #[tokio::test]
    async fn a_claimed_chat_knows_its_parent_once_its_create_lands() {
        use super::*;

        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
            },
        )
        .unwrap();
        host.create_chat("created", None, Some("test-device"), None, None)
            .unwrap();
        assert!(!host.parent_unknown("created"));

        host.claim_chat("claimed", Some("/tmp")).unwrap();
        assert!(host.parent_unknown("claimed"));
        let mut chat = host.chat("claimed").unwrap().expect("the claimed row");
        chat.parent_chat_id = None;
        host.mutate(|doc| doc.upsert_chat(&chat)).unwrap();
        assert!(!host.parent_unknown("claimed"));
    }

    #[tokio::test]
    async fn sidebar_preferences_watch_preserves_explicit_empty_state() {
        use super::*;

        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "test-device".into(),
                device_name: "Test device".into(),
                platform: "macos".into(),
            },
        )
        .unwrap();
        let mut preferences = host.watch_sidebar_preferences();
        assert!(!preferences.borrow().initialized);

        host.change_sidebar_pin(&onicode_proto::SidebarPinChange::Unpin {
            session_id: "absent".into(),
        })
        .unwrap();
        host.inner.publish();
        assert!(preferences.has_changed().unwrap());
        let state = preferences.borrow_and_update();
        assert!(state.initialized);
        assert!(state.synced, "no other device wrote to this registry");
        assert!(state.pinned_session_ids.is_empty());
        let first_revision = state.revision;
        drop(state);
        let acknowledgement = host.sidebar_preferences_snapshot();
        assert_eq!(acknowledgement.revision, first_revision);
        assert!(
            !preferences.has_changed().unwrap(),
            "unchanged acknowledgements must not churn watches"
        );
        host.create_chat("cached", None, Some("test-device"), None, None)
            .unwrap();
        host.change_sidebar_pin(&onicode_proto::SidebarPinChange::Pin {
            session_id: "cached".into(),
            after: None,
            before: None,
        })
        .unwrap();
        let acknowledgement = host.sidebar_preferences_snapshot();
        assert!(acknowledgement.revision > first_revision);
        assert_eq!(*preferences.borrow(), acknowledgement);
        host.change_sidebar_pin(&onicode_proto::SidebarPinChange::Section {
            change: onicode_proto::SidebarSectionChange::Create {
                id: "focus".into(),
                name: "Focus".into(),
            },
        })
        .unwrap();
        let sections = host.sidebar_preferences_snapshot();
        assert!(sections.revision > acknowledgement.revision);
        assert_eq!(sections.sections[0].name, "Focus");
        assert_eq!(*preferences.borrow(), sections);
        host.change_sidebar_pin(&onicode_proto::SidebarPinChange::Section {
            change: onicode_proto::SidebarSectionChange::Collapse {
                id: "focus".into(),
                collapsed: true,
            },
        })
        .unwrap();
        let collapsed = host.sidebar_preferences_snapshot();
        assert!(collapsed.revision > sections.revision);
        assert!(collapsed.sections[0].collapsed);
        assert_eq!(*preferences.borrow(), collapsed);
    }

    #[test]
    fn boot_repairs_the_legacy_unknown_device_sentinel() {
        assert_eq!(
            device_name_on_boot(Some("unknown-device"), "MacBook Pro"),
            "MacBook Pro"
        );
    }

    #[test]
    fn boot_preserves_a_user_selected_device_name() {
        assert_eq!(
            device_name_on_boot(Some("Work laptop"), "MacBook Pro"),
            "Work laptop"
        );
    }

    #[test]
    fn linked_worktree_resolves_to_the_checkout_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let wt = dir.path().join("clever-ember");
        std::fs::create_dir_all(root.join(".git").join("worktrees").join("clever-ember")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!(
                "gitdir: {}\n",
                root.join(".git/worktrees/clever-ember").display()
            ),
        )
        .unwrap();
        assert_eq!(
            linked_worktree_root(&wt).as_deref(),
            Some(root.to_str().unwrap())
        );
    }

    #[test]
    fn primary_checkouts_and_plain_folders_resolve_to_none() {
        let dir = tempfile::tempdir().unwrap();
        // Primary checkout: `.git` is a directory.
        let primary = dir.path().join("primary");
        std::fs::create_dir_all(primary.join(".git")).unwrap();
        assert_eq!(linked_worktree_root(&primary), None);
        // Not a repo at all.
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(linked_worktree_root(&plain), None);
        // A `.git` file pointing somewhere that is not `<root>/.git/worktrees/<name>`.
        let odd = dir.path().join("odd");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        assert_eq!(linked_worktree_root(&odd), None);
    }
}
