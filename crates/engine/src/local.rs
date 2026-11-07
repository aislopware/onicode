//! One engine: every hub over one store the engine opens itself, served as one
//! [`EngineService`]. The `onicode-engine` process holds one and serves it on its socket
//! ([`crate::host`]); tests hold one in process and speak the protocol to its service. A viewer
//! never touches the store.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use onicode_core::store::Store;
use tokio::sync::Notify;

use crate::accounts::{AccountsHub, AccountsService, AgentProbe, CliProbe};
use crate::app_access::{AppAccessHub, AppAccessService};
use crate::files::{FilesHub, FilesService};
use crate::git::{CheckoutHub, GitService};
use crate::service::{EngineGroup, EngineService};
use crate::session::{SessionHub, SessionService, Spawn};
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
    /// Every terminal's shell. A quiet engine starts none.
    pub terminal: Arc<TerminalHub>,
    pub service: Arc<EngineService>,
    /// Told when a viewer stops the engine (`StopEngine`).
    stop: Arc<Notify>,
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
        let session = SessionHub::new(store.clone(), spawn.clone());
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
        .with(Arc::new(EngineGroup::new(session.clone(), stop.clone())));
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
            terminal,
            service: Arc::new(service),
            stop,
        })
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

    /// The store itself, for the SSH remote and the phone host only: their runners still write it
    /// in the viewer's process until steps 10 and 11 delete them (engine.md, "Boundary").
    #[must_use]
    pub fn legacy_store(&self) -> Database {
        self.store.clone()
    }

    /// A connection of the viewer's own to the store in `data_dir`, for the SSH remote and the
    /// phone host only, whose runners write it from the app's process until steps 10 and 11
    /// delete them. SQLite serializes it with the engine's writer; the engine reads what they
    /// write from the store, as it did while both ran in one process.
    pub fn open_legacy_store(data_dir: &Path) -> Result<Database, String> {
        let path = data_dir.join(DATABASE);
        Store::open_monocode(&path)
            .map(|store| Arc::new(Mutex::new(store)))
            .map_err(|error| format!("Cannot open {}: {error}", path.display()))
    }

    /// Whether `other` is this engine.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.store, &other.store)
    }
}
