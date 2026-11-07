//! One engine: every hub over one store the engine opens itself, served as one
//! [`EngineService`] behind the [`Router`] that forwards what names another engine. The
//! `onicode-engine` process holds one and serves it on its socket ([`crate::host`]) and to its
//! peers on the tailnet ([`Local::join`]); tests hold one in process and speak the protocol to its
//! service. A viewer never touches the store.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use onicode_core::store::Store;
use tokio::sync::Notify;

use crate::accounts::{AccountsHub, AccountsService, AgentProbe, CliProbe};
use crate::alerts::{AlertsHub, AlertsService, SystemBanners};
use crate::app_access::{AppAccessHub, AppAccessService};
use crate::files::{FilesHub, FilesService};
use crate::git::{CheckoutHub, GitService};
use crate::machines::{MachinesHub, MachinesService};
use crate::outbox::Outbox;
use crate::peers::{Identity, Peers};
use crate::router::{Peering, Router};
use crate::service::{EngineGroup, EngineService};
use crate::session::{Runner, SessionHub, SessionService, Spawn};
use crate::settings::{SettingsHub, SettingsService};
use crate::settings_pages::SettingsPagesService;
use crate::terminal::{TerminalHub, TerminalService};
use crate::testing::Seed;
use crate::turn::Database;
use crate::work::{CliGh, CoreTrackers, Gh, Trackers, WorkHub, WorkService};
use crate::workspace::{Chores, WorkspaceHub, WorkspaceService};

/// The file the engine keeps its store in, inside its data directory.
pub const DATABASE: &str = "onicode.db";

/// How a [`Local`] runs: what a test replaces.
#[derive(Default)]
pub struct Options {
    /// Watch no disk: a listing or a status changes only through the engine's calls.
    pub quiet: bool,
    /// The background chores (titles, work items, branch names); `None` runs the providers'.
    pub chores: Option<Arc<dyn Chores>>,
    /// The provider doors the Agents group asks; `None` runs the provider CLIs.
    pub probe: Option<Arc<dyn AgentProbe>>,
    /// The Inbox's trackers; `None` runs their adapters.
    pub trackers: Option<Arc<dyn Trackers>>,
    /// The `gh` linked activity asks; `None` runs the CLI.
    pub gh: Option<Arc<dyn Gh>>,
    /// Runs each turn; `None` runs the provider (`turn::run_turn`). Tests script it.
    pub runner: Option<Runner>,
}

/// The schedulers an engine runs on its own clock, beaten every 30 s by whoever runs it (tests
/// in virtual time).
#[derive(Clone)]
pub struct Clock {
    workspace: Arc<WorkspaceHub>,
    session: Arc<SessionHub>,
}

impl Clock {
    /// One beat at `now` (Unix ms): the workspace's reminders, automation runs and Inbox claims,
    /// then the sessions' usage-limit resumes. Both run even when the other fails.
    pub fn tick(&self, now: u64) -> Result<(), String> {
        let workspace = self.workspace.tick(now);
        let session = self.session.resume_limits(now);
        workspace.and(session)
    }
}

/// One engine in this process.
pub struct Local {
    store: Database,
    pub session: Arc<SessionHub>,
    pub workspace: Arc<WorkspaceHub>,
    pub files: Arc<FilesHub>,
    pub git: Arc<CheckoutHub>,
    pub accounts: Arc<AccountsHub>,
    pub settings: Arc<SettingsHub>,
    pub work: Arc<WorkHub>,
    pub app_access: Arc<AppAccessHub>,
    /// What needs the owner, and the banners it posts.
    pub alerts: Arc<AlertsHub>,
    /// Every terminal's shell. A quiet engine starts none.
    pub terminal: Arc<TerminalHub>,
    /// The machines this engine knows (`WatchMachines`).
    pub machines: Arc<MachinesHub>,
    /// Who this engine is: its id and its machine's name.
    pub identity: Arc<Identity>,
    /// Every group, routed: what its viewers and peers reach.
    pub service: Arc<Router>,
    /// Told when a viewer stops the engine (`StopEngine`).
    stop: Arc<Notify>,
    /// The engine's place on the tailnet, once it joined, and the task that follows it into the
    /// machines list.
    peers: Mutex<Option<(Arc<Peers>, tokio::task::AbortHandle)>>,
}

impl Local {
    /// The engine over the store in `data_dir`, created if missing.
    pub fn open(data_dir: &Path, spawn: Arc<dyn Spawn>, options: Options) -> Result<Self, String> {
        let path = data_dir.join(DATABASE);
        let store = Store::open_monocode(&path)
            .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
        onicode_core::checkpoint::init(data_dir.join("checkpoints"));
        Self::over(Arc::new(Mutex::new(store)), spawn, options)
    }

    /// An engine over a store in memory: a test's, or a viewer's whose data cannot open.
    pub fn memory(spawn: Arc<dyn Spawn>, options: Options) -> Result<Self, String> {
        let store = Store::open(Path::new(":memory:"))?;
        Self::over(Arc::new(Mutex::new(store)), spawn, options)
    }

    fn over(store: Database, spawn: Arc<dyn Spawn>, options: Options) -> Result<Self, String> {
        let settings = SettingsHub::new(store.clone())?;
        let session = match options.runner {
            Some(runner) => SessionHub::with_runner(store.clone(), spawn.clone(), runner),
            None => SessionHub::new(store.clone(), spawn.clone()),
        };
        let identity = Identity::of(
            crate::turn::locked(&store)?
                .data_dir()
                .map(Path::to_path_buf)
                .as_deref(),
        );
        let machines = MachinesHub::new(identity.clone(), session.clone());
        let workspace = match options.chores {
            Some(chores) => WorkspaceHub::with_chores(store.clone(), spawn.clone(), chores),
            None => WorkspaceHub::new(store.clone(), spawn.clone()),
        };
        session.attach(workspace.clone());
        session.attach_settings(settings.clone());
        let (files, git) = if options.quiet {
            (
                FilesHub::quiet(store.clone(), spawn.clone()),
                CheckoutHub::quiet(store.clone(), spawn.clone()),
            )
        } else {
            (
                FilesHub::new(store.clone(), spawn.clone()),
                CheckoutHub::new(store.clone(), spawn.clone()),
            )
        };
        git.attach(workspace.clone());
        let probe = options.probe.unwrap_or_else(|| Arc::new(CliProbe));
        let accounts = AccountsHub::new(store.clone(), settings.clone(), probe, spawn.clone());
        session.attach_accounts(accounts.clone());
        let work = WorkHub::with(
            store.clone(),
            workspace.clone(),
            options.trackers.unwrap_or_else(|| Arc::new(CoreTrackers)),
            options.gh.unwrap_or_else(|| Arc::new(CliGh)),
            spawn.clone(),
        );
        let terminal = if options.quiet {
            TerminalHub::quiet(spawn.clone())
        } else {
            TerminalHub::new(spawn.clone())
        };
        let app_access = AppAccessHub::new(store.clone(), workspace.clone(), spawn);
        let alerts = AlertsHub::new(Arc::new(SystemBanners));
        let stop = Arc::new(Notify::new());
        let service = EngineService::new(
            SessionService::new(session.clone()),
            WorkspaceService::new(workspace.clone()),
            FilesService::new(files.clone()),
            GitService::new(git.clone()),
        )
        .with(Arc::new(AccountsService::new(accounts.clone())))
        .with(Arc::new(SettingsService::new(settings.clone())))
        .with(Arc::new(SettingsPagesService::new(git.clone())))
        .with(Arc::new(WorkService::new(work.clone())))
        .with(Arc::new(AppAccessService::new(app_access.clone())))
        .with(Arc::new(TerminalService::new(terminal.clone())))
        .with(Arc::new(MachinesService::new(machines.clone())))
        .with(Arc::new(AlertsService::new(alerts.clone())))
        .with(Arc::new(EngineGroup::new(
            session.clone(),
            identity.clone(),
            stop.clone(),
        )));
        let router = Router::new(
            Arc::new(service),
            identity.clone(),
            Outbox::new(store.clone()),
        );
        Ok(Self {
            store,
            session,
            workspace,
            files,
            git,
            accounts,
            settings,
            work,
            app_access,
            alerts,
            terminal,
            machines,
            identity,
            service: Arc::new(router),
            stop,
            peers: Mutex::new(None),
        })
    }

    /// Takes this engine's place on the tailnet through `peers`: its id becomes the tailnet's,
    /// what names another engine is forwarded over them, the outbox delivers, and the machines
    /// list follows them. Runs inside a tokio runtime. An engine joins once; `peers` built from
    /// [`Self::peer_service`].
    pub fn join(&self, peers: Peers) {
        let peers = Arc::new(peers);
        self.identity
            .set(peers.id().clone(), peers.name().to_owned());
        let peering: Arc<dyn Peering> = peers.clone();
        self.service.attach(&peering);
        let following = tokio::spawn(self.machines.clone().follow(Arc::downgrade(&peers)));
        let previous = self
            .peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace((peers, following.abort_handle()));
        if let Some((_, task)) = previous {
            task.abort();
        }
    }

    /// What the engine serves its peers: the same routed service its viewers reach.
    #[must_use]
    pub fn peer_service(&self) -> Arc<dyn onicode_rpc::RpcService> {
        self.service.clone()
    }

    /// The engine's place on the tailnet, once it joined.
    #[must_use]
    pub fn peers(&self) -> Option<Arc<Peers>> {
        self.peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(peers, _)| peers.clone())
    }

    /// Resolves once a viewer stopped the engine with `StopEngine`.
    pub async fn stopped(&self) {
        self.stop.notified().await;
    }

    /// The engine's schedulers, for the clock that beats them.
    #[must_use]
    pub fn clock(&self) -> Clock {
        Clock {
            workspace: self.workspace.clone(),
            session: self.session.clone(),
        }
    }

    /// One beat of the engine's schedulers at `now` (Unix ms), as [`Clock::tick`].
    pub fn tick(&self, now: u64) -> Result<(), String> {
        self.clock().tick(now)
    }

    /// The store's sessions, read only, without opening it for writing: `--check-data`.
    pub fn check_data(data_dir: &Path) -> Result<(PathBuf, usize), String> {
        let path = data_dir.join(DATABASE);
        let store = Store::read_monocode(&path)?;
        Ok((path.clone(), store.sessions()?.len()))
    }

    /// Removes the files of conversations deleted since the last sweep. Lists every
    /// conversation's folder, so it runs off the launch path.
    pub fn sweep(&self) -> Result<usize, String> {
        crate::turn::locked(&self.store)?.sweep_thread_dirs()
    }

    /// The directory the engine keeps its data in, `None` for one in memory.
    #[must_use]
    pub fn data_dir(&self) -> Option<PathBuf> {
        crate::turn::locked(&self.store)
            .ok()
            .and_then(|store| store.data_dir().map(Path::to_path_buf))
    }

    /// The test and fixture seam: writes and reads the store as typed calls, never handing the
    /// store out.
    #[must_use]
    pub fn seed(&self) -> Seed {
        Seed::new(self.store.clone())
    }

    /// Whether `other` is this engine.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.store, &other.store)
    }
}

impl Drop for Local {
    /// Leaves the tailnet with the engine: its links close, and the machines list stops following
    /// them.
    fn drop(&mut self) {
        if let Some((_, task)) = self
            .peers
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}
