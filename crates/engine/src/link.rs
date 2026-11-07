//! A viewer's link to its machine's engine (engine.md, "Local feel"): one socket connection,
//! every call and watch multiplexed on it, connected or launched on first use and again whenever
//! it drops.
//!
//! Connecting dials the socket ([`Door::dial`]). Nothing answering starts the engine
//! ([`Door::launch`], once per attempt; a launch that loses the race for the instance lock exits,
//! and the dial reaches the winner) and polls until it answers. Every connection greets the
//! engine with `Hello`; an engine that is not the one this viewer would launch (another protocol,
//! or an older build than the one on disk) is retired and replaced. Concurrent callers share one
//! attempt.
//!
//! A call whose connection closes before its reply is sent again on the next one, with the same
//! params and so the same `OpId`: the engine's ledger answers a repeat with the first receipt. A
//! watch whose connection closes reads as lost ([`onicode_rpc::RpcSubscription::lost`]); the
//! viewer resubscribes with its cursor, and an engine that restarted meanwhile (a new epoch)
//! answers with a fresh snapshot.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use onicode_protocol::engine::Hello;
use onicode_protocol::{Call, PROTOCOL, Routed};
use onicode_rpc::ipc_auth::{read_token, token_path};
use onicode_rpc::{DialError, RpcClient, RpcError, RpcSubscription, connect_unix};
use tokio::runtime::Handle;

use crate::host::{InstanceLock, socket_path, stop_holder};

/// How a viewer reaches its engine. [`SocketDoor`] is the real one; tests script others.
pub trait Door: Send + Sync + 'static {
    /// Opens an authenticated connection. Polled on the link's runtime.
    fn dial(&self) -> BoxFuture<'static, Result<RpcClient, DialError>>;

    /// Starts an engine, detached, and returns at once.
    fn launch(&self) -> Result<(), String>;

    /// Whether the engine that greeted with `hello` is the one this viewer would launch.
    fn current(&self, hello: &Hello) -> bool;

    /// Stops the engine that is not current and waits until it let go, so [`Door::launch`] can
    /// start this viewer's.
    fn retire(&self) -> BoxFuture<'static, ()>;
}

/// How long connecting waits and how often it looks.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How long a launched engine may take to answer.
    pub start: Duration,
    /// The first pause between dials; each doubles, up to [`Timing::most`].
    pub first: Duration,
    /// The longest pause between dials.
    pub most: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            start: Duration::from_secs(5),
            first: Duration::from_millis(1),
            most: Duration::from_millis(50),
        }
    }
}

/// How many times a call is sent again after its connection closed under it.
const RETRIES: usize = 3;

/// How long a launched engine may stay absent before it is launched again (doubling after).
const RELAUNCH_AFTER: Duration = Duration::from_millis(500);

/// A viewer's link to its engine.
pub struct Link {
    door: Arc<dyn Door>,
    runtime: Handle,
    timing: Timing,
    current: Mutex<Option<Arc<RpcClient>>>,
    /// One connection attempt at a time; callers that arrive meanwhile take its result.
    connecting: tokio::sync::Mutex<()>,
}

/// The viewer's greeting.
fn greeting() -> serde_json::Value {
    let hello = Routed {
        engine: None,
        op: None,
        body: crate::service::hello(),
    };
    serde_json::to_value(hello).unwrap_or_default()
}

impl Link {
    /// A link through `door`, its connections served on `runtime`. Connects on first use.
    #[must_use]
    pub fn new(door: Arc<dyn Door>, runtime: Handle, timing: Timing) -> Arc<Self> {
        Arc::new(Self {
            door,
            runtime,
            timing,
            current: Mutex::new(None),
            connecting: tokio::sync::Mutex::new(()),
        })
    }

    fn live(&self) -> Option<Arc<RpcClient>> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|client| !client.is_closed())
            .cloned()
    }

    /// Lets `client` go if it is still the link's, so the next use connects again.
    fn forget(&self, client: &Arc<RpcClient>) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, client))
        {
            *current = None;
        }
    }

    /// The live connection, connecting (and launching the engine) when there is none.
    pub async fn client(self: &Arc<Self>) -> Result<Arc<RpcClient>, String> {
        if let Some(client) = self.live() {
            return Ok(client);
        }
        let link = self.clone();
        self.runtime
            .spawn(async move { link.reconnect().await })
            .await
            .map_err(|error| format!("The engine link stopped: {error}"))?
    }

    async fn reconnect(&self) -> Result<Arc<RpcClient>, String> {
        let _turn = self.connecting.lock().await;
        if let Some(client) = self.live() {
            return Ok(client);
        }
        let client = Arc::new(self.connect().await?);
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(client.clone());
        Ok(client)
    }

    async fn connect(&self) -> Result<RpcClient, String> {
        // When this connect last launched an engine, and how long it then waits before it
        // launches again: an engine can come up and die between two looks, unseen.
        let mut launched: Option<tokio::time::Instant> = None;
        let mut relaunch_after = RELAUNCH_AFTER;
        let mut retired = false;
        let mut deadline = tokio::time::Instant::now() + self.timing.start;
        let mut pause = self.timing.first;
        loop {
            match self.door.dial().await {
                Ok(client) => match client.call_ref(Hello::NAME, &greeting()).await {
                    Ok(reply) => {
                        let hello: Hello = serde_json::from_value(reply).map_err(|error| {
                            format!("The engine's greeting is garbled: {error}")
                        })?;
                        if self.door.current(&hello) {
                            return Ok(client);
                        }
                        if retired {
                            return Err(format!(
                                "The engine running here is another build (protocol {}, {}), \
                                 and it would not stop",
                                hello.protocol, hello.version
                            ));
                        }
                        drop(client);
                        retired = true;
                        self.door.retire().await;
                        self.door.launch()?;
                        launched = Some(tokio::time::Instant::now());
                        deadline = tokio::time::Instant::now() + self.timing.start;
                        pause = self.timing.first;
                    }
                    // The engine closed the connection mid-greeting: it is stopping, or it went
                    // down after it came up. Either way it is gone: the next look launches one.
                    Err(RpcError::Closed) => launched = None,
                    Err(error) => return Err(format!("The engine refused the greeting: {error}")),
                },
                Err(DialError::Absent(_)) if launched.is_none() => {
                    self.door.launch()?;
                    launched = Some(tokio::time::Instant::now());
                    deadline = tokio::time::Instant::now() + self.timing.start;
                }
                // Launched a while ago and still absent: it may have died as it started. Launch
                // again, less often each time; a second engine racing a slow first one loses its
                // lock and exits, so an extra launch costs nothing. The deadline stays.
                Err(DialError::Absent(_))
                    if launched.is_some_and(|at| at.elapsed() >= relaunch_after) =>
                {
                    self.door.launch()?;
                    launched = Some(tokio::time::Instant::now());
                    relaunch_after *= 2;
                }
                // Still starting, or rewriting its tokens: look again.
                Err(DialError::Absent(_) | DialError::Denied(_)) => {}
                Err(DialError::Failed(error)) => return Err(error),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("The engine did not start".into());
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(self.timing.most);
        }
    }

    /// A call, sent again on the next connection if its connection closes before the reply.
    pub async fn call(
        self: &Arc<Self>,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut retries = 0;
        loop {
            let client = self.client().await?;
            match client.call_ref(method, params).await {
                Err(RpcError::Closed) if retries < RETRIES => {
                    retries += 1;
                    self.forget(&client);
                }
                reply => return reply.map_err(|error| error.to_string()),
            }
        }
    }

    /// A typed call to the engine: `body` routed to it, under `op` when it mutates.
    pub async fn ask<C: Call>(
        self: &Arc<Self>,
        op: Option<onicode_protocol::OpId>,
        body: C::Params,
    ) -> Result<C::Reply, String> {
        let params = serde_json::to_value(Routed {
            engine: None,
            op,
            body,
        })
        .map_err(|error| error.to_string())?;
        let reply = self.call(C::NAME, &params).await?;
        serde_json::from_value(reply).map_err(|error| error.to_string())
    }

    /// A stream on the live connection. It ends when the engine ends it, or reads as lost when
    /// the connection closes; the caller resubscribes with its cursor.
    pub async fn subscribe(
        self: &Arc<Self>,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<RpcSubscription, String> {
        let mut retries = 0;
        loop {
            let client = self.client().await?;
            match client.subscribe_ref(method, params).await {
                Err(RpcError::Closed) if retries < RETRIES => {
                    retries += 1;
                    self.forget(&client);
                }
                subscription => return subscription.map_err(|error| error.to_string()),
            }
        }
    }
}

/// The real door: the socket in the data dir, and the `onicode-engine` beside the app.
pub struct SocketDoor {
    dir: PathBuf,
    /// The engine this viewer launches; `None` when none sits beside the app.
    engine: Option<PathBuf>,
    app: PathBuf,
}

impl SocketDoor {
    /// The door to the engine over `data_dir`, launched from beside the app at `app_exe`.
    #[must_use]
    pub fn new(data_dir: &Path, app_exe: &Path) -> Self {
        Self {
            dir: data_dir.to_path_buf(),
            engine: onicode_launch::engine_executable(app_exe),
            app: app_exe.to_path_buf(),
        }
    }

    /// The door that launches `engine` itself (the smoke test's).
    #[must_use]
    pub fn with_engine(data_dir: &Path, engine: &Path) -> Self {
        Self {
            dir: data_dir.to_path_buf(),
            engine: Some(engine.to_path_buf()),
            app: engine.to_path_buf(),
        }
    }
}

impl Door for SocketDoor {
    fn dial(&self) -> BoxFuture<'static, Result<RpcClient, DialError>> {
        let dir = self.dir.clone();
        async move {
            // Read at every dial: a restarted engine has published new tokens.
            let Ok(Some(token)) = read_token(&token_path(&dir)) else {
                return Err(DialError::Absent("no engine has started here".into()));
            };
            connect_unix(&socket_path(&dir), &token, None).await
        }
        .boxed()
    }

    fn launch(&self) -> Result<(), String> {
        let engine = self.engine.as_ref().ok_or_else(|| {
            format!(
                "No {} beside {}",
                onicode_launch::ENGINE_EXECUTABLES.join(" or "),
                self.app.display()
            )
        })?;
        let child = onicode_launch::launch(&self.dir, engine, &self.app)
            .map_err(|error| format!("Cannot start {}: {error}", engine.display()))?;
        onicode_launch::reap(child);
        Ok(())
    }

    fn current(&self, hello: &Hello) -> bool {
        if hello.protocol != PROTOCOL {
            return false;
        }
        // Without an engine of its own to prefer, a viewer takes the one that runs.
        let Some(engine) = &self.engine else {
            return true;
        };
        InstanceLock::holder(&self.dir).is_none_or(|holder| holder.runs(engine))
    }

    fn retire(&self) -> BoxFuture<'static, ()> {
        let dir = self.dir.clone();
        async move {
            let stopped =
                tokio::task::spawn_blocking(move || stop_holder(&dir, Duration::from_secs(3)))
                    .await;
            if !matches!(stopped, Ok(true)) {
                eprintln!("The engine of another build did not stop");
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
