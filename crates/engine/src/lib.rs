//! onicode-engine — the headless backend: sessions engine, doc host + command executor,
//! run journal + crash recovery, and the IPC RPC server.
//!
//! It serves its clients over the localhost IPC port, and the user's other engines and
//! viewers over QUIC links on the tailnet ([`peers`]).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
pub use onicode_proto::{EngineInfo, HarnessId};
use onicode_rpc::{RpcError, RpcReply, RpcService, methods};
use onicode_sync::DocsStore;

pub mod acp_registry;
pub mod agent_accounts;

/// Which build this engine is: its git commit and when its executable was
/// built (the file's modification time, in seconds), as
/// `"<commit>.<seconds>"`. Read once, as the engine starts: a binary replaced
/// on disk while it runs leaves its id as it was, so a client can compare it
/// with `onicode-engine --build-id` of the binary there now.
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let built = std::env::current_exe()
            .and_then(std::fs::metadata)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        format!("{}.{built}", env!("ONICODE_GIT_COMMIT"))
    })
}
mod change_request_store;
pub mod change_requests;
mod chat_persistence;
pub mod clipboard;
pub mod deferred;
pub mod desktop;
pub mod desktop_notify;
pub mod diff_sync;
pub mod doc_host;
mod doc_server;
mod fs_watch;
pub mod harness_updates;
pub(crate) mod home_index;
pub mod instance_lock;
pub mod keep_awake;
pub mod managed;
mod model_catalogs;
pub mod paths;
mod peer_doc_sink;
pub mod peers;
pub mod profile;
pub mod project_actions;
pub mod registry;
mod registry_server;
pub mod remote_terminals;
pub mod repos;
pub mod rpc;
pub mod run_journal;
pub mod sessions;
pub mod source_control;
pub mod spaces;
pub mod terminals;
pub mod titles;
pub mod tool_output;
mod transcript_history;
pub mod transfers;
mod update_trial;
pub mod uploads;
pub mod voice;
pub mod workspace_files;
pub mod workspace_host;
pub mod workspace_search;
mod worktree_settings;

pub use agent_accounts::{AgentAccounts, AgentAccountsConfig};
pub use change_requests::{ChangeRequestCacheKey, CheckoutChangeRequests};
pub use diff_sync::{
    CheckoutDiffSync, DiffFileTextPair, DiffSnapshot, TreeSnapshot, TurnSnapshot,
    capture_commit_diff, capture_diff, capture_diff_against, capture_turn_diff,
    discard_working_tree, merge_base, read_diff_file_text, snapshot_tree, working_diff_base,
};
pub use doc_host::{ChatDocHandle, DocHost, DocHostConfig};
pub use instance_lock::InstanceLock;
pub use onicode_preview::Scope as PreviewScope;
pub use profile::EngineProfile;
pub use project_actions::ProjectActionsStore;
pub use registry::{HarnessDescriptor, HarnessRegistry, default_registry};
pub use repos::{CheckoutIdentity, Repos, worktree_branch_from_title};
pub use rpc::EngineRpc;
pub use run_journal::{JournalError, RunJournal};
pub use sessions::{JournaledEvent, SessionsEngine, SteerOutcome};
pub use source_control::{
    BranchHeadContext, ChangeRequestError, ChangeRequestProvider, ChangeRequestResolution,
    ChangeRequestResolver, CheckoutChangeRequestLookup, CheckoutSourceContext, GitHubCli,
    GitRemote, parse_git_remote,
};
pub use spaces::SpacesSync;
pub use terminals::Terminals;
pub use titles::TitleGenerator;
pub use uploads::{AttachmentChunk, Uploads};
pub use workspace_files::WorkspaceFiles;
pub use workspace_host::{WorkspaceHost, WorkspaceHostConfig};
pub use workspace_search::WorkspaceSearch;

pub(crate) const LEGACY_UNKNOWN_DEVICE_NAME: &str = "unknown-device";

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("doc: {0}")]
    Doc(#[from] onicode_doc::DocError),
    #[error("journal: {0}")]
    Journal(#[from] run_journal::JournalError),
    #[error("store: {0}")]
    Store(#[from] onicode_sync::StoreError),
    #[error("harness: {0}")]
    Harness(#[from] onicode_harness::HarnessError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Epoch millis now — the doc/journal timestamp base.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Data directory (default `~/.onicode`, dev `~/.onicode-dev`).
    pub data_dir: PathBuf,
    /// Localhost IPC port for the UI.
    pub ipc_port: u16,
    /// Harness for doc-command runs on chats without a workspace `config` row.
    pub default_harness: HarnessId,
    /// Base URL of the public release feed (releases and harness compatibility).
    pub update_feed: String,
    /// Join the tailnet: serve this engine to the user's other engines and reach theirs.
    /// Off for tests and fixtures, which must never show up on a real tailnet.
    pub peers: bool,
}

/// The assembled engine core — also constructible without the IPC server for tests
/// and the in-process (headed) mode.
pub struct EngineCore {
    pub sessions: SessionsEngine,
    pub doc_host: DocHost,
    pub workspace: WorkspaceHost,
    pub registry: Arc<HarnessRegistry>,
    pub repos: Repos,
    pub workspace_files: WorkspaceFiles,
    pub terminals: Terminals,
    /// Terminals on other devices this engine attaches to, over the peer links.
    pub remote_terminals: remote_terminals::RemoteTerminals,
    /// Serves this engine's terminals to peers (needs the links for its datagram copies).
    terminal_service: Arc<terminals::link::TerminalService<Terminals>>,
    pub project_actions: ProjectActionsStore,
    pub previews: onicode_preview::PreviewService,
    pub change_requests: CheckoutChangeRequests,
    pub diff_sync: CheckoutDiffSync,
    pub spaces_sync: SpacesSync,
    pub uploads: Uploads,
    pub agent_accounts: AgentAccounts,
    pub harness_updates: harness_updates::HarnessUpdateCoordinator,
    pub device_id: String,
    /// Which app windows are focused and the chat each shows (alerts hold back for them).
    pub viewport_focus: desktop_notify::ViewportFocus,
    /// The agent alerts that should show now (`WatchAlerts`; desktop engines only).
    pub alerts: desktop_notify::AlertFeed,
    /// Links to the user's other engines on the tailnet, once it is up: `targetDeviceId`
    /// routing and every stream to a peer.
    links: std::sync::Mutex<Option<Arc<onicode_peer::PeerLinks>>>,
    /// Who serves each kind of stream peers open to this engine (see [`peers::StreamRouter`]).
    streams: Arc<peers::StreamRouter>,
    /// Files between this engine and its peers, and the `File` stream handler.
    pub transfers: transfers::EngineTransfers,
    /// The clipboard shared with this owner's other engines (`one-machine.md`).
    pub clipboard: Arc<clipboard::ClipboardHost>,
    /// Release checker (attached by [`Engine::assemble_runtime`]) — the
    /// UpdateStatus stream + ApplyUpdate.
    updater: std::sync::Mutex<Option<onicode_update::Updater>>,
    /// Exclusive data-dir lock — held for the engine's lifetime (single-instance).
    _instance_lock: InstanceLock,
}

impl EngineCore {
    /// Open the profile under `data_dir`, wire sessions ⇄ doc host ⇄ workspace host, and
    /// recover stale journals from a previous crash.
    pub fn assemble(
        data_dir: &Path,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
    ) -> Result<Self, EngineError> {
        Self::assemble_with_profile(EngineProfile::local(data_dir)?, registry, default_harness)
    }

    /// Assemble the engine against one resolved profile.
    pub fn assemble_with_profile(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        // Single-instance guard: two engines on one data dir would race the
        // SQLite snapshots + journals. Taken before any store opens or the IPC
        // port binds; held (and kernel-released on crash) for the engine's life.
        let lock = InstanceLock::acquire(data_dir)?;
        Self::assemble_with_profile_locked(profile, registry, default_harness, lock)
    }

    /// Assemble against a pre-acquired [`InstanceLock`]. The headed app takes
    /// the lock before binding the IPC port so the listener owner and the
    /// data-dir owner cannot diverge when several viewports bootstrap at once.
    pub fn assemble_with_profile_locked(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        lock: InstanceLock,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        update_trial::refuse_mid_update(data_dir)?;
        let device_id = load_or_create_device_id(data_dir)?;
        // This device's harness enablement (Settings → Providers) rides the
        // engine data dir — per-device, like the CLI installs it gates.
        registry.load_prefs(data_dir);
        let store = Arc::new(DocsStore::open(profile.store_root())?);
        let journal = Arc::new(RunJournal::open(profile.store_root().join("journals"))?);
        let sessions = SessionsEngine::new(device_id.clone(), journal, registry.clone());
        let doc_host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: device_id.clone(),
                default_harness,
            },
        );
        let workspace = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: device_id.clone(),
                device_name: local_device_name(&device_id),
                platform: std::env::consts::OS.to_string(),
            },
        )?;
        doc_host.set_workspace(workspace.clone());
        doc_host.set_sessions(sessions.clone());
        let viewport_focus = desktop_notify::ViewportFocus::default();
        sessions.set_doc_host(doc_host.clone());
        // A new version on trial may yet be undone, and recovering clears the
        // journals' marks of what to resume, which a rollback doesn't restore.
        // It waits for the commit (`update_trial`), with all other new work.
        if onicode_update::transaction::in_trial() {
            doc_host.hold_work();
        } else {
            match sessions.recover_stale() {
                Ok(0) => {}
                Ok(recovered) => tracing::info!(recovered, "stale sessions recovered on boot"),
                Err(err) => tracing::error!(error = %err, "stale-session recovery failed"),
            }
        }
        let repos = Repos::new(data_dir, &device_id);
        sessions.set_scratch_root(repos.scratch_root());
        doc_host.set_repos(repos.clone());
        let change_requests = CheckoutChangeRequests::start(repos.clone(), &device_id);
        let workspace_files =
            WorkspaceFiles::new(repos.clone(), workspace.clone(), device_id.clone());
        let terminals = Terminals::new();
        sessions.track_terminal_activity(terminals.clone(), workspace.clone());
        let project_actions = ProjectActionsStore::open(profile.store_root())?;
        doc_host.set_project_action_runtime(project_actions.clone(), terminals.clone());
        let previews = onicode_preview::PreviewService::new(
            profile.store_root().join("previews.json"),
            device_id.clone(),
            local_device_name(&device_id),
        )
        .map_err(|e| EngineError::Other(e.to_string()))?;
        // Peers open terminals and preview tunnels to this engine as streams on their links;
        // input and echo copies ride datagrams beside them.
        let remote_terminals = remote_terminals::RemoteTerminals::default();
        let terminal_service = Arc::new(terminals::link::TerminalService::new(
            terminals.clone(),
            remote_terminals.clone(),
        ));
        let streams = Arc::new(peers::StreamRouter::default());
        streams.serve(onicode_net::StreamKind::Terminal, terminal_service.clone());
        streams.serve_datagrams(
            onicode_net::stream::channel::TERMINAL_COPY,
            terminal_service.clone(),
        );
        streams.serve(onicode_net::StreamKind::Tunnel, previews.tunnels());
        streams.serve(onicode_net::StreamKind::Doc, doc_host.doc_server());
        streams.serve(
            onicode_net::StreamKind::Registry,
            workspace.registry_server(),
        );
        // Transfers pull over `File` streams (`xfer/...`); the clipboard's `clip/...` heads
        // share them through `transfers.set_clip`.
        let transfers = transfers::EngineTransfers::open(data_dir, &device_id);
        streams.serve(onicode_net::StreamKind::File, transfers.file_streams());
        let clipboard = clipboard::ClipboardHost::new(data_dir, &device_id);
        transfers.set_clip(clipboard.clone());
        let uploads = Uploads::from_root(profile.uploads_root());
        // Queued-attachment support: the doc host resolves `pending://` refs
        // against this store and pushes staged bytes to remote hosts.
        doc_host.set_uploads(uploads.clone());
        let agent_accounts_config = AgentAccountsConfig::detect(data_dir);
        sessions.set_generated_images(
            uploads.clone(),
            agent_accounts_config.codex_home.join("generated_images"),
        );
        // Logins started from another device publish their callback port to
        // the P2P service, which serves it to that device alone.
        let agent_accounts =
            AgentAccounts::with_callback_routes(agent_accounts_config, previews.callback_routes());
        registry.set_account_environment(Arc::new(agent_accounts::MuseRunEnvironment(
            agent_accounts.clone(),
        )));
        // Agents added from the ACP registry (this engine's own, under its
        // data dir): harnesses before the update coordinator lists them.
        acp_registry::start(data_dir, &registry);
        let harness_updates =
            harness_updates::HarnessUpdateCoordinator::new(data_dir, registry.clone());
        harness_updates.start();
        sessions.set_titles(TitleGenerator::new(
            workspace.clone(),
            registry.clone(),
            repos.clone(),
        ));
        let diff_sync = CheckoutDiffSync::start(repos.clone(), workspace.clone(), &device_id);
        // Turn starts snapshot the checkout tree — the "Latest turn" diff base.
        let turn_diff = diff_sync.clone();
        sessions.set_turn_listener(Arc::new(move |chat_id, cwd| {
            turn_diff.note_turn_start(chat_id, cwd);
        }));
        let spaces_sync = SpacesSync::start(repos.clone(), workspace.clone(), &device_id);
        Ok(Self {
            sessions,
            doc_host,
            workspace,
            registry,
            repos,
            workspace_files,
            terminals,
            remote_terminals,
            terminal_service,
            project_actions,
            previews,
            change_requests,
            diff_sync,
            spaces_sync,
            uploads,
            agent_accounts,
            harness_updates,
            device_id,
            links: std::sync::Mutex::new(None),
            streams,
            transfers,
            clipboard,
            updater: std::sync::Mutex::new(None),
            viewport_focus,
            alerts: desktop_notify::AlertFeed::default(),
            _instance_lock: lock,
        })
    }

    /// Attach the links to the user's other engines: `targetDeviceId` routing,
    /// [`Self::dial_device`], session docs and the registry, terminals and previews.
    pub fn set_links(&self, links: Arc<onicode_peer::PeerLinks>) {
        self.doc_host.set_links(links.clone());
        self.workspace.set_links(links.clone());
        self.terminal_service.set_links(&links);
        self.previews.set_links(links.clone());
        self.transfers.set_links(links.clone());
        self.clipboard.set_links(links.clone());
        *self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(links);
    }

    /// Where the engine's parts register for the streams peers open to it.
    pub fn streams(&self) -> &Arc<peers::StreamRouter> {
        &self.streams
    }

    pub fn links(&self) -> Option<Arc<onicode_peer::PeerLinks>> {
        self.links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Attach the release checker (before building the RPC service).
    pub fn set_updater(&self, updater: onicode_update::Updater) {
        *self
            .updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(updater);
    }

    pub fn updater(&self) -> Option<onicode_update::Updater> {
        self.updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// An RPC client to another device's engine over its tailnet link (the router's dial
    /// seam), dialled if there is none.
    pub async fn dial_device(
        &self,
        device_id: &str,
    ) -> Result<onicode_rpc::CarrierClient, EngineError> {
        let links = self
            .links()
            .ok_or_else(|| EngineError::Other("peer links unavailable (offline)".into()))?;
        links
            .client(device_id)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))
    }

    pub fn rpc_service(&self) -> Arc<EngineRpc> {
        let mut rpc = EngineRpc::new(
            self.sessions.clone(),
            self.doc_host.clone(),
            self.workspace.clone(),
            self.registry.clone(),
            self.repos.clone(),
            self.workspace_files.clone(),
            self.terminals.clone(),
            self.project_actions.clone(),
            self.change_requests.clone(),
            self.diff_sync.clone(),
            self.uploads.clone(),
            self.agent_accounts.clone(),
        )
        .with_previews(self.previews.clone())
        .with_remote_terminals(self.remote_terminals.clone())
        .with_harness_updates(self.harness_updates.clone())
        .with_viewport_focus(self.viewport_focus.clone())
        .with_alerts(self.alerts.clone())
        .with_transfers(self.transfers.clone())
        .with_clipboard(self.clipboard.clone());
        if let Some(links) = self.links() {
            rpc = rpc.with_links(links);
        }
        if let Some(updater) = self.updater() {
            rpc = rpc.with_updater(updater);
        }
        let rpc = Arc::new(rpc);
        let service: std::sync::Weak<dyn onicode_rpc::RpcService> =
            Arc::downgrade(&rpc) as std::sync::Weak<dyn onicode_rpc::RpcService>;
        self.clipboard.set_service(service);
        rpc
    }

    /// Graceful teardown: settle live runs (streaming entries stamped `aborted`),
    /// kill live PTYs, stamp our workspace `lastSeenAt`, and flush every open doc
    /// snapshot. Each phase's time goes to the log, so a slow quit names its cause.
    pub async fn shutdown(&self) {
        let mut phases = ShutdownPhases::new();
        self.previews.shutdown().await;
        phases.end("previews");
        self.harness_updates.shutdown().await;
        phases.end("harness_updates");
        // A run interruption transitions its chat to Idle, and Idle normally
        // releases the next queued row. Freeze first so quitting never starts
        // recovered work while the engine is being torn down.
        self.doc_host.pause_all_queues();
        self.sessions.shutdown().await;
        phases.end("sessions");
        self.terminals.shutdown();
        phases.end("terminals");
        self.transfers.shutdown();
        phases.end("transfers");
        self.clipboard.shutdown();
        phases.end("clipboard");
        self.sessions.stop_activity_projection().await;
        phases.end("activity_projection");
        self.agent_accounts.shutdown();
        self.change_requests.shutdown();
        phases.end("accounts_and_change_requests");
        // Stop polling releases before flushing.
        let updater = self
            .updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(updater) = updater {
            updater.shutdown().await;
        }
        phases.end("updater");
        self.diff_sync.shutdown().await;
        phases.end("diff_sync");
        self.workspace_files.shutdown().await;
        phases.end("workspace_files");
        self.spaces_sync.shutdown().await;
        phases.end("spaces_sync");
        self.doc_host.shutdown_workers().await;
        phases.end("doc_workers");
        self.doc_host.flush_all();
        phases.end("doc_flush");
        self.workspace.shutdown();
        phases.end("workspace");
        // Break the sessions ⇄ doc-host retain cycle so the replaced graph can
        // actually be freed once the last handle drops.
        self.sessions.clear_doc_host();
        phases.finish();
    }
}

/// Times the sequential phases of [`EngineCore::shutdown`]: each at debug,
/// and one info line with the total and the slowest phase.
struct ShutdownPhases {
    started: std::time::Instant,
    last: std::time::Instant,
    slowest: (&'static str, std::time::Duration),
}

impl ShutdownPhases {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last: now,
            slowest: ("none", std::time::Duration::ZERO),
        }
    }

    fn end(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        let took = now.duration_since(self.last);
        self.last = now;
        tracing::debug!(
            phase,
            took_ms = took.as_millis() as u64,
            "engine shutdown phase"
        );
        if took > self.slowest.1 {
            self.slowest = (phase, took);
        }
    }

    fn finish(self) {
        tracing::info!(
            total_ms = self.started.elapsed().as_millis() as u64,
            slowest = self.slowest.0,
            slowest_ms = self.slowest.1.as_millis() as u64,
            "engine shut down"
        );
    }
}

pub struct Engine {
    pub config: EngineConfig,
}

/// A fully assembled engine plus its place on the tailnet, whose lifetime keeps this device
/// reachable. Used by both the headless server and the headed in-process engine so their
/// boot paths cannot diverge.
pub struct EngineRuntime {
    core: EngineCore,
    peers: std::sync::Mutex<Option<peers::Peers>>,
    /// Desktop banners and alerts (desktop engines only), and the keep-awake watch.
    watchers: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// IPC-only lifecycle control owned by `onicode headless`. The regular
/// [`EngineRpc`] deliberately does not expose this method, so a viewport
/// attached to another headed process cannot shut down that process's engine.
struct HeadlessRpc {
    inner: Arc<dyn RpcService>,
    stop_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

#[async_trait]
impl RpcService for HeadlessRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method != methods::STOP_ENGINE {
            return self.inner.handle(method, params).await;
        }

        let stop_tx = self.stop_tx.clone();
        // Let the unary success frame reach the client before `Engine::run`
        // aborts the IPC server and drains the runtime.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = stop_tx.send(());
        });
        RpcReply::value(&serde_json::json!({ "ok": true }))
    }
}

impl EngineRuntime {
    pub fn core(&self) -> &EngineCore {
        &self.core
    }

    pub async fn shutdown(&self) {
        self.stop_watchers();
        // Off the tailnet before draining: no peer call may start work meanwhile.
        self.peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.core.shutdown().await;
    }

    fn stop_watchers(&self) {
        for task in self
            .watchers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            task.abort();
        }
    }
}

impl Drop for EngineRuntime {
    fn drop(&mut self) {
        self.stop_watchers();
        self.peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    /// Resolve the one-shot identity served before profile stores are available.
    pub fn engine_info(config: &EngineConfig) -> Result<EngineInfo, EngineError> {
        std::fs::create_dir_all(&config.data_dir)?;
        Ok(EngineInfo {
            device_id: load_or_create_device_id(&config.data_dir)?,
            cursor_sdk_version: Some(onicode_harness::CursorHarness::sdk_version().into()),
            capabilities: {
                let mut capabilities = onicode_proto::capabilities::current();
                if desktop_notify::enabled() {
                    capabilities
                        .push(onicode_proto::capabilities::DESKTOP_NOTIFICATIONS_V1.to_string());
                }
                capabilities
            },
            build: Some(crate::build_id().to_owned()),
        })
    }

    /// Open the profile and join the tailnet (when `config.peers`).
    pub async fn assemble_runtime(
        config: &EngineConfig,
        profile: EngineProfile,
    ) -> anyhow::Result<EngineRuntime> {
        Self::assemble_runtime_inner(config, profile, None).await
    }

    /// Like [`Self::assemble_runtime`], but against an [`InstanceLock`] the
    /// caller already holds on the profile's device root (headed bootstrap
    /// acquires it before binding IPC).
    pub async fn assemble_runtime_with_lock(
        config: &EngineConfig,
        profile: EngineProfile,
        lock: InstanceLock,
    ) -> anyhow::Result<EngineRuntime> {
        Self::assemble_runtime_inner(config, profile, Some(lock)).await
    }

    async fn assemble_runtime_inner(
        config: &EngineConfig,
        profile: EngineProfile,
        lock: Option<InstanceLock>,
    ) -> anyhow::Result<EngineRuntime> {
        let core = match lock {
            Some(lock) => EngineCore::assemble_with_profile_locked(
                profile,
                Arc::new(default_registry()),
                config.default_harness,
                lock,
            )?,
            None => EngineCore::assemble_with_profile(
                profile,
                Arc::new(default_registry()),
                config.default_harness,
            )?,
        };
        let preview_workspace = core.workspace.clone();
        let preview_device = core.device_id.clone();
        let preview_terminals = core.terminals.clone();
        let projects = Arc::new(move || onicode_preview::Scope {
            chats: preview_workspace
                .read_chats()
                .unwrap_or_default()
                .into_iter()
                .filter(|chat| chat.device_id == preview_device)
                .filter_map(|chat| Some((chat.id, std::path::PathBuf::from(chat.cwd?))))
                .collect(),
            terminals: preview_terminals.chats(),
            reserved_ports: onicode_harness::agent_ports::reserved(),
        });
        core.previews.start(projects).await;
        // Release checker: polls {feed}/releases hourly (wall clock); headless
        // installs with ONICODE_AUTO_UPDATE=1 apply + restart themselves — gated
        // on quiescence so a restart never lands under a live run or open PTY.
        let quiescent: onicode_update::QuiescentCheck = {
            let sessions = core.sessions.clone();
            let terminals = core.terminals.clone();
            Arc::new(move || !sessions.any_active() && !terminals.any_open())
        };
        core.harness_updates.set_compatibility_feed(format!(
            "{}/releases/harness-compatibility.json",
            config.update_feed.trim_end_matches('/')
        ));
        // The release channel a macOS app install follows lives in the data
        // dir (update-channel.json), so this report follows the app's choice.
        onicode_update::channel::set_data_dir(&config.data_dir);
        let updater = onicode_update::Updater::spawn(config.update_feed.clone(), Some(quiescent));
        core.set_updater(updater);
        tracing::info!(device_id = %core.device_id, "engine core assembled");
        // Managed ACP adapters install in the background at boot (agents
        // whose CLI is present but whose adapter isn't yet), so a first chat
        // never waits on — or dies inside — an npm run.
        onicode_harness::acp::prewarm_managed_adapters();

        let peers = if config.peers {
            match peers::Peers::start(&core.device_id, core.rpc_service(), core.streams().clone())
                .await
            {
                Ok(peers) => {
                    core.set_links(peers.links.clone());
                    // Only a real engine on a tailnet watches this Mac's pasteboard.
                    core.clipboard.start_general();
                    Some(peers)
                }
                Err(error) => {
                    tracing::info!(%error, "not on a tailnet: this engine serves its own clients only");
                    None
                }
            }
        } else {
            None
        };

        let notifier = desktop_notify::enabled()
            .then(|| desktop_notify::spawn(&core, config.data_dir.clone()));
        let keep_awake =
            keep_awake::enabled().then(|| keep_awake::spawn(&core, config.data_dir.clone()));
        Ok(EngineRuntime {
            core,
            peers: std::sync::Mutex::new(peers),
            watchers: std::sync::Mutex::new(notifier.into_iter().chain(keep_awake).collect()),
        })
    }

    /// Run until ctrl-c, SIGTERM or `StopEngine`: the profile's engine, its IPC server, and
    /// its links to the user's other engines on the tailnet.
    pub async fn run(self) -> anyhow::Result<()> {
        let config = self.config;
        tracing::info!(data_dir = %config.data_dir.display(), "engine starting");
        keep_awake::enable();

        std::fs::create_dir_all(&config.data_dir)?;
        update_trial::boot(&config.data_dir)?;
        let profile = EngineProfile::local(&config.data_dir)?;
        let runtime = Self::assemble_runtime(&config, profile).await?;
        // The desktop app or `onicode update` may install a newer binary under
        // a running service; restart into it once no run or terminal is live.
        if let Some(updater) = runtime.core().updater() {
            updater.restart_when_superseded();
        }

        // A daemon exists to serve this port, so a bind failure is fatal here —
        // unlike the headed app, which can still work over its in-process
        // transport (see `serve_ipc`).
        let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel();
        let service: Arc<dyn RpcService> = Arc::new(HeadlessRpc {
            inner: runtime.core().rpc_service(),
            stop_tx,
        });
        let server = serve_ipc(config.ipc_port, &config.data_dir, service).await?;
        // Only a port this process actually serves goes to agents: the
        // injected MCP server must dial back into THIS engine.
        runtime.core().sessions.set_ipc_port(config.ipc_port);
        runtime
            .core()
            .sessions
            .set_ipc_token_file(onicode_rpc::ipc_auth::agent_token_path(&config.data_dir));
        update_trial::prove_and_commit(runtime.core(), &config);

        tokio::select! {
            result = shutdown_signal() => result?,
            requested = stop_rx.recv() => {
                if requested.is_some() {
                    tracing::info!("headless shutdown requested over IPC");
                }
            }
        }
        tracing::info!("shutting down");
        server.abort();
        runtime.shutdown().await;
        Ok(())
    }
}

/// Ctrl-C or SIGTERM. systemd/launchd stop (and the auto-updater's service
/// restart) deliver SIGTERM — without catching it the daemon dies mid-write
/// and every stop takes the crash-recovery path instead of the graceful drain.
async fn shutdown_signal() -> std::io::Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = sigterm.recv() => Ok(()),
    }
}

/// Serve the typed RPC on the localhost IPC port.
///
/// Both engines call this: the headless daemon, and the headed app's embedded
/// engine. That second case is the point — an embedded engine that keeps the
/// port to itself forces anyone wanting a second viewport (the terminal app) to
/// stop the desktop app, start a daemon, and start it again in the right order.
/// Serving here means any viewport can just attach.
///
/// Localhost only, exactly as before: this widens *which process* can serve the
/// port, not who can reach it.
///
/// Every call mints a fresh bearer at `{data_dir}/ipc-token` (0600) that each
/// handshake must present ([`onicode_rpc::ipc_auth`]); viewports and the CLI
/// read it at dial time. Agents' `onicode mcp` servers get the agent-scoped
/// one (`{data_dir}/ipc-token-agent`), which reaches only their tools'
/// methods ([`onicode_rpc::agent_allowed`]). The caller owns `data_dir` (it holds the
/// [`InstanceLock`]), so rotating the file before binding cannot strand
/// another engine's clients — and a client that sees the port open always
/// finds the new token already published.
pub async fn serve_ipc(
    port: u16,
    data_dir: &Path,
    service: std::sync::Arc<dyn onicode_rpc::RpcService>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let tokens = onicode_rpc::ipc_auth::issue_tokens(data_dir)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "IPC server listening");
    Ok(tokio::spawn(onicode_rpc::serve_ws_listener_scoped(
        listener, service, tokens,
    )))
}

/// Best-effort human name for this device's registry row.
fn local_device_name(device_id: &str) -> String {
    select_local_device_name(
        [
            std::env::var("ONICODE_DEVICE_NAME").ok(),
            native_friendly_device_name(),
            std::env::var("HOSTNAME").ok(),
            gethostname::gethostname().into_string().ok(),
            std::fs::read_to_string("/etc/hostname").ok(),
        ],
        device_id,
        std::env::consts::OS,
    )
}

fn select_local_device_name(
    candidates: impl IntoIterator<Item = Option<String>>,
    device_id: &str,
    platform: &str,
) -> String {
    candidates
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let platform = match platform {
                "macos" => "macOS",
                "linux" => "Linux",
                _ => "Local",
            };
            let short_id: String = device_id.chars().take(8).collect();
            format!("{platform} device {short_id}")
        })
}

#[cfg(target_os = "macos")]
fn native_friendly_device_name() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn native_friendly_device_name() -> Option<String> {
    None
}

/// Stable per-installation device id, persisted at `{data_dir}/device-id`.
fn load_or_create_device_id(data_dir: &Path) -> Result<String, EngineError> {
    std::fs::create_dir_all(data_dir)?;
    // EngineInfo is resolved before the lifetime InstanceLock is acquired, so
    // identity creation and legacy repair need their own short critical section.
    // The OS releases this lock after a crash; unlike a create_new lockfile it
    // cannot strand an installation permanently.
    let _identity_lock = DeviceIdentityLock::acquire(data_dir)?;
    let path = data_dir.join("device-id");
    let recovering_empty = match std::fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => return Ok(id.trim().to_string()),
        // Older releases used truncate+write. A crash between those operations
        // left a zero-byte file that is safe to replace with a fresh identity.
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };

    let id = new_id();
    let temp_path = data_dir.join(format!(
        ".device-id.tmp-{}-{}",
        std::process::id(),
        new_id()
    ));
    let write_result = (|| -> Result<(), EngineError> {
        let mut temp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        temp.write_all(id.as_bytes())?;
        temp.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    // Fresh installs use create-if-absent. Legacy empty files need an atomic
    // same-directory replacement.
    let publish_result = if recovering_empty {
        match std::fs::read_to_string(&path) {
            Ok(id) if !id.trim().is_empty() => {
                let _ = std::fs::remove_file(&temp_path);
                return Ok(id.trim().to_string());
            }
            Ok(_) => replace_empty_device_id(&temp_path, &path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::hard_link(&temp_path, &path)
            }
            Err(err) => Err(err),
        }
    } else {
        std::fs::hard_link(&temp_path, &path)
    };
    let _ = std::fs::remove_file(&temp_path);
    match publish_result {
        Ok(()) => Ok(id),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let winner = std::fs::read_to_string(&path)?;
            if winner.trim().is_empty() {
                Err(EngineError::Other(format!(
                    "invalid device identity {}: file is empty",
                    path.display()
                )))
            } else {
                Ok(winner.trim().to_string())
            }
        }
        Err(err) => Err(err.into()),
    }
}

struct DeviceIdentityLock {
    _file: std::fs::File,
}

impl DeviceIdentityLock {
    fn acquire(data_dir: &Path) -> Result<Self, EngineError> {
        use std::os::unix::io::AsRawFd;

        let path = data_dir.join("device-id.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        let file = options.open(&path)?;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINTR) {
                return Err(err.into());
            }
        }

        Ok(Self { _file: file })
    }
}

fn replace_empty_device_id(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

#[cfg(test)]
mod device_name_tests {
    use super::select_local_device_name;

    fn name(candidates: &[Option<&str>], device_id: &str, platform: &str) -> String {
        select_local_device_name(
            candidates
                .iter()
                .map(|candidate| candidate.map(str::to_string)),
            device_id,
            platform,
        )
    }

    #[test]
    fn explicit_override_wins_and_is_trimmed() {
        assert_eq!(
            name(
                &[Some("  Studio Mac  "), Some("system-host")],
                "17bc0aa2-rest",
                "macos"
            ),
            "Studio Mac"
        );
    }

    #[test]
    fn native_friendly_name_wins_over_hostnames() {
        assert_eq!(
            name(
                &[
                    None,
                    Some("MacBook Pro de Jose"),
                    None,
                    Some("MacBook-Pro.local"),
                ],
                "17bc0aa2-rest",
                "macos"
            ),
            "MacBook Pro de Jose"
        );
    }

    #[test]
    fn blank_candidates_are_ignored() {
        assert_eq!(
            name(
                &[Some("  "), None, Some("\n"), Some("linux-box")],
                "17bc0aa2-rest",
                "linux"
            ),
            "linux-box"
        );
    }

    #[test]
    fn final_fallback_is_platform_specific_and_distinct() {
        assert_eq!(
            name(&[None, Some(" ")], "17bc0aa2-rest", "linux"),
            "Linux device 17bc0aa2"
        );
    }
}
