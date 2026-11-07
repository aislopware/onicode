//! The `onicode-engine` process (engine.md, "Process", migration step 8): one engine per machine,
//! outliving the window, serving every group on a Unix socket in its data dir.
//!
//! Start: the data dir is made the owner's alone (0700), the instance lock taken (another engine
//! holding it ends this one quietly: a race of launches leaves one engine), the store and hubs
//! opened, fresh tokens published, a socket a crashed engine left removed, and `engine.sock`
//! bound. Each connection passes the socket's gates (`onicode_rpc::serve_unix`: the same uid,
//! then a token) and then this module's policy (`Admitted`): an agent's token reaches only the
//! calls its protocol marks as an agent's, and until a connection has greeted the engine with its
//! `PROTOCOL` only the frozen calls are answered. The engine beats its own schedulers and sweeps
//! deleted conversations; `StopEngine`, `SIGTERM` or `SIGINT` stop it, removing the socket before
//! the lock goes, and its agents die with it. The process also joins the tailnet
//! ([`Host::on_tailnet`], unless `ONICODE_PEERS=0`): as soon as Tailscale is up it serves its peers
//! and forwards what its viewers send other engines (`peers.rs`, `router.rs`).
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use onicode_protocol::engine::{EngineInfo, Hello, UpdateEngine};
use onicode_protocol::{Call, METHODS, PROTOCOL, Routed};
use onicode_rpc::ipc_auth::IpcTokens;
use onicode_rpc::{Admission, Caller, Gate, RpcError, RpcReply, RpcService, Scope};
use serde::Deserialize;
use tokio::net::UnixListener;

mod lock;
#[cfg(test)]
mod tests;

pub use lock::{Holder, InstanceLock, LOCK, build_stamp, stop_holder};

use crate::local::{Local, Options};
use crate::router::Router;
use crate::session::ThreadSpawn;

/// The socket inside the data dir.
pub const SOCKET: &str = "engine.sock";
/// How often the engine beats its schedulers.
pub const BEAT: Duration = Duration::from_secs(30);
/// How long a stopping engine keeps serving, so the reply to `StopEngine` reaches its viewer.
const STOP_GRACE: Duration = Duration::from_millis(20);

/// `{data_dir}/engine.sock`.
#[must_use]
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOCKET)
}

/// Creates `dir` if missing and makes it the owner's alone (0700): the socket, the tokens and
/// the store sit in it.
pub fn prepare_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// The calls answered before a connection greets the engine with its own `PROTOCOL`: their shapes
/// never change, so a viewer on another protocol can still say "Needs update".
pub(crate) const FROZEN: [&str; 3] = [Hello::NAME, EngineInfo::NAME, UpdateEngine::NAME];

/// The calls an agent's scoped token may make (`Call::AGENT`).
static AGENT_CALLS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    METHODS
        .iter()
        .filter(|method| method.agent)
        .map(|method| method.name)
        .collect()
});

/// One admitted connection's view of the engine.
struct Admitted {
    service: Arc<Router>,
    /// The chat an agent's server says it serves, for an agent's connection.
    agent: Option<Option<String>>,
    /// The connection said `Hello` with this engine's `PROTOCOL`.
    greeted: AtomicBool,
}

impl Admitted {
    fn new(service: Arc<Router>, admission: Admission) -> Self {
        Self {
            service,
            agent: (admission.scope == Scope::Agent).then_some(admission.chat),
            greeted: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl RpcService for Admitted {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if self.agent.is_some() && !AGENT_CALLS.contains(method) {
            eprintln!("Refused an agent's call outside its scope: {method}");
            return Err(RpcError::Failed(format!(
                "{method} isn't open to agents: only the person can do that, in Onicode"
            )));
        }
        if method == Hello::NAME {
            let peer = Routed::<Hello>::deserialize(&params)
                .map_err(|error| RpcError::BadParams(error.to_string()))?;
            if peer.body.protocol == PROTOCOL {
                self.greeted.store(true, Ordering::Release);
            }
        } else if !self.greeted.load(Ordering::Acquire) && !FROZEN.contains(&method) {
            return Err(RpcError::Failed(format!(
                "{method}: greet this engine first; it speaks protocol {PROTOCOL}"
            )));
        }
        match &self.agent {
            Some(chat) => {
                let caller = Caller::Agent { chat: chat.clone() };
                onicode_rpc::with_caller(caller, self.service.handle(method, params)).await
            }
            None => self.service.handle(method, params).await,
        }
    }
}

/// An engine bound to its data dir: lock held, store open, socket bound.
pub struct Host {
    dir: PathBuf,
    lock: InstanceLock,
    local: Arc<Local>,
    listener: UnixListener,
    tokens: IpcTokens,
    uid: u32,
    beat: Option<Duration>,
    /// Join the tailnet while serving.
    tailnet: bool,
}

impl Host {
    /// Takes `dir`'s instance lock, opens the engine with `open` and binds its socket; `None`
    /// when another engine holds the lock. Runs inside a tokio runtime (the socket's reactor).
    pub fn bind(
        dir: &Path,
        open: impl FnOnce(&Path) -> Result<Local, String>,
    ) -> Result<Option<Self>, String> {
        prepare_dir(dir).map_err(|error| format!("Cannot prepare {}: {error}", dir.display()))?;
        let Some(mut lock) = InstanceLock::acquire(dir).map_err(|error| {
            format!("Cannot take the engine lock in {}: {error}", dir.display())
        })?
        else {
            return Ok(None);
        };
        let exe = std::env::current_exe().unwrap_or_default();
        lock.record(&Holder::this(&exe))
            .map_err(|error| format!("Cannot write the engine lock: {error}"))?;
        let local = open(dir)?;
        let tokens = onicode_rpc::ipc_auth::issue_tokens(dir)
            .map_err(|error| format!("Cannot publish the engine's tokens: {error}"))?;
        // The lock is ours, so a socket file here is one a crashed engine left.
        let socket = socket_path(dir);
        match std::fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot remove {}: {error}", socket.display())),
        }
        let listener = UnixListener::bind(&socket)
            .map_err(|error| format!("Cannot bind {}: {error}", socket.display()))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("Cannot protect {}: {error}", socket.display()))?;
        }
        // SAFETY: `geteuid` cannot fail and reads no memory.
        let uid = unsafe { libc::geteuid() };
        Ok(Some(Self {
            dir: dir.to_path_buf(),
            lock,
            local: Arc::new(local),
            listener,
            tokens,
            uid,
            beat: None,
            tailnet: false,
        }))
    }

    /// This host, beating its schedulers every `beat`: at once, for what came due while no
    /// engine ran, then on that period. Unbeaten (a test's), the schedulers run only when told.
    #[must_use]
    pub fn beating(mut self, beat: Duration) -> Self {
        self.beat = Some(beat);
        self
    }

    /// This host, joining the tailnet as soon as Tailscale is up. Off it (a test's host), the
    /// engine serves only its socket, and calls for other engines wait in its outbox.
    #[must_use]
    pub fn on_tailnet(mut self) -> Self {
        self.tailnet = true;
        self
    }

    /// The engine this host serves.
    #[must_use]
    pub fn local(&self) -> &Arc<Local> {
        &self.local
    }

    /// Serves until a viewer stops the engine or `until` resolves, then closes every
    /// connection, removes the socket file and lets the lock go, in that order.
    pub async fn serve(self, until: impl Future<Output = ()>) {
        let Self {
            dir,
            lock,
            local,
            listener,
            tokens,
            uid,
            beat,
            tailnet,
        } = self;
        let service = local.service.clone();
        let serving = onicode_rpc::serve_unix(listener, Gate { tokens, uid }, move |admission| {
            Arc::new(Admitted::new(service.clone(), admission)) as Arc<dyn RpcService>
        });
        let sweeper = local.clone();
        drop(tokio::task::spawn_blocking(move || {
            if let Err(error) = sweeper.sweep() {
                eprintln!("Cannot sweep deleted conversations: {error}");
            }
        }));
        let clock = local.clock();
        let beating = async move {
            let Some(beat) = beat else {
                return std::future::pending().await;
            };
            let mut interval = tokio::time::interval(beat);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let clock = clock.clone();
                let beat = tokio::task::spawn_blocking(move || {
                    clock.tick(onicode_protocol::time::now_ms())
                });
                if let Ok(Err(error)) = beat.await {
                    eprintln!("The engine's schedulers could not run: {error}");
                }
            }
        };
        let joining = local.clone();
        let peering_dir = dir.clone();
        let peering = async move {
            if tailnet {
                let peers = crate::peers::Peers::join(&peering_dir, joining.peer_service()).await;
                joining.join(peers);
            }
            std::future::pending::<()>().await;
        };
        let alerting = crate::alerts::pump(
            local.alerts.clone(),
            local.workspace.clone(),
            local.session.clone(),
            local.terminal.clone(),
        );
        let keeping_awake = crate::keep_awake::run(
            local.workspace.clone(),
            local.terminal.clone(),
            local.settings.clone(),
            crate::keep_awake::system(),
        );
        let stopped = async {
            local.stopped().await;
            tokio::time::sleep(STOP_GRACE).await;
        };
        tokio::select! {
            () = serving => {}
            () = beating => {}
            () = peering => {}
            () = alerting => {}
            () = keeping_awake => {}
            () = stopped => {}
            () = until => {}
        }
        if let Err(error) = std::fs::remove_file(socket_path(&dir))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Cannot remove the engine's socket: {error}");
        }
        drop(lock);
    }
}

/// Resolves on `SIGTERM` or `SIGINT`.
async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut interrupt)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = interrupt.recv() => {}
    }
}

/// The `onicode-engine` process over `dir`: serves until stopped, then stops its agents. Another
/// engine already serving `dir` is success: the viewer that launched this one reaches that one.
pub fn run(dir: &Path) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("onicode-engine")
        .enable_all()
        .build()
        .map_err(|error| format!("Cannot start the engine's runtime: {error}"))?;
    let served = runtime.block_on(async {
        let open = |dir: &Path| Local::open(dir, Arc::new(ThreadSpawn), Options::default());
        match Host::bind(dir, open)? {
            Some(host) => {
                let host = host.beating(BEAT);
                let host = if crate::peers::enabled() {
                    host.on_tailnet()
                } else {
                    host
                };
                host.serve(terminated()).await;
            }
            None => eprintln!("Another engine serves {}", dir.display()),
        }
        Ok(())
    });
    // Its agents die with it (engine.md, "Process").
    onicode_core::process::shutdown_all();
    served
}
