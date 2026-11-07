//! The app's link to its engine (engine.md, migration steps 3 and 4). A local turn runs in the
//! engine's `SessionHub`: the conversation stages the turn's in-process context, watches the
//! session and sends the prompt over `onicode-rpc`'s in-memory transport, and a `Viewer` folds what
//! arrives back into one `turn::Update` per beat. SSH and legacy remote turns keep their own
//! runners. The sidebar, history and workspace views read the engine's `WatchWorkspace` through
//! [`watch_workspace`] and change it with [`call`]; the engine's own clock drives its reminder and
//! automation schedulers. The explorer, editor, Changes, review, history and worktree views read
//! and change files and checkouts through the app's own engine ([`local`]): its calls, and its
//! `WatchDir` and `WatchGit` watches through [`Engine::watch`]. The terminal dock's terminals are
//! the app's own engine's too: its shells run there, and each view mirrors its `WatchTerminal`.
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, mpsc};

use futures::future::{BoxFuture, Either, select};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use gpui::{App, BackgroundExecutor, Global, Task};
use onicode_core::agent::{Control, Decision, Settings};
use onicode_core::store::Session;
use onicode_engine::EngineService;
use onicode_engine::files::{FilesHub, FilesService};
use onicode_engine::git::{CheckoutHub, GitService};
use onicode_engine::session::{
    Expect, SessionHub, SessionService, Spawn, Staged, StagedTurn, Viewer,
};
use onicode_engine::terminal::{TerminalHub, TerminalService};
use onicode_engine::turn::{Database, Update};
use onicode_engine::workspace::{WorkspaceHub, WorkspaceService};
use onicode_protocol::session::{
    self, Answer, Decide, KeepQuestionOpen, SessionEvent, SessionState, SteerNow, Verdict,
    WatchSession,
};
use onicode_protocol::workspace::{WatchWorkspace, WorkspaceEvent, WorkspaceState};
use onicode_protocol::{Call, Cursor, Item, MessageId, OpId, Outcome, Routed, SessionId, Watch};
use onicode_rpc::{RpcClient, RpcReply, RpcService};
use tokio::sync::Notify;

/// The engine's tokio thread: it serves the in-memory transport.
static RUNTIME: LazyLock<Option<tokio::runtime::Runtime>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("onicode-engine")
        .enable_all()
        .build()
        .ok()
});

/// The engines this app started, one per store.
#[derive(Default)]
struct Engines(Vec<Arc<EngineClient>>);

impl Global for Engines {}

/// The store of the app's own engine: the one the shell opened.
struct LocalStore(Database);

impl Global for LocalStore {}

/// Turns run on gpui's background threads, where the app ran them before the engine.
struct GpuiSpawn(BackgroundExecutor);

impl Spawn for GpuiSpawn {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.spawn(work).detach();
    }
}

enum Link {
    /// Over `onicode-rpc`'s in-memory transport, served on the engine's tokio thread.
    Rpc(Arc<RpcClient>),
    /// The service's own dispatch, called in place: tests stay on gpui's deterministic
    /// executors, and so does an app whose tokio thread could not start.
    Direct(Arc<EngineService>),
}

/// The chores of an app test's engine: no generated title or branch, and only the work item a
/// prompt's GitHub link names.
#[cfg(test)]
struct TestChores;

#[cfg(test)]
impl onicode_engine::workspace::Chores for TestChores {
    fn title(
        &self,
        _: onicode_core::session_title::TextModel,
        _: &std::path::Path,
        _: &str,
        _: Arc<AtomicBool>,
    ) -> Option<onicode_core::session_title::GeneratedTitle> {
        None
    }

    fn work_item(
        &self,
        message: &str,
        _: &std::path::Path,
        _: Option<&onicode_core::session_title::WorkItemHint>,
    ) -> Option<onicode_core::session_groups::WorkItem> {
        onicode_core::session_groups::parse_github_work_item(message)
    }

    fn branch(
        &self,
        _: onicode_core::session_title::TextModel,
        _: &std::path::Path,
        _: &str,
        _: Arc<AtomicBool>,
    ) -> Option<String> {
        None
    }
}

/// How often the engine's schedulers run: due reminders and automation runs.
const SCHEDULER_BEAT: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) struct EngineClient {
    store: Database,
    hub: Arc<SessionHub>,
    workspace: Arc<WorkspaceHub>,
    /// Held for the engine's lifetime: the Files and Git groups' hubs, served through `link`.
    _files: Arc<FilesHub>,
    _git: Arc<CheckoutHub>,
    /// The Terminal group's hub: every terminal's shell, served through `link`.
    _terminals: Arc<TerminalHub>,
    link: Link,
    executor: BackgroundExecutor,
    /// Every call made, by name with its routed params, for a test to assert on.
    #[cfg(test)]
    calls: Mutex<Vec<(&'static str, serde_json::Value)>>,
}

fn lock(viewer: &Mutex<Viewer>) -> MutexGuard<'_, Viewer> {
    viewer.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `Viewer::absorb`, the lock never held across an await.
fn absorb(viewer: &Mutex<Viewer>, item: Item<SessionState, SessionEvent>) -> bool {
    lock(viewer).absorb(item)
}

fn cursor(viewer: &Mutex<Viewer>) -> Option<onicode_protocol::Cursor> {
    lock(viewer).cursor()
}

fn take(viewer: &Mutex<Viewer>) -> Option<Update> {
    lock(viewer).take()
}

fn refuse(viewer: &Mutex<Viewer>, reason: String) -> Option<Update> {
    let mut viewer = lock(viewer);
    viewer.refuse(reason);
    viewer.take()
}

impl EngineClient {
    /// The engine serving `store`, started on first use.
    fn get(store: &Database, cx: &mut App) -> Arc<Self> {
        if let Some(engine) = cx.try_global::<Engines>().and_then(|engines| {
            engines
                .0
                .iter()
                .find(|engine| Arc::ptr_eq(&engine.store, store))
                .cloned()
        }) {
            return engine;
        }
        let executor = cx.background_executor().clone();
        let hub = SessionHub::new(store.clone(), Arc::new(GpuiSpawn(executor.clone())));
        let spawn: Arc<dyn Spawn> = Arc::new(GpuiSpawn(executor.clone()));
        // Tests answer the chores in place: no provider CLI, `gh` or network.
        #[cfg(test)]
        let workspace =
            WorkspaceHub::with_chores(store.clone(), spawn.clone(), Arc::new(TestChores));
        #[cfg(not(test))]
        let workspace = WorkspaceHub::new(store.clone(), spawn.clone());
        hub.attach(workspace.clone());
        // Tests start no shell: a terminal view in a test runs without one.
        #[cfg(test)]
        let terminals = TerminalHub::quiet(spawn.clone());
        #[cfg(not(test))]
        let terminals = TerminalHub::new(spawn.clone());
        // Tests watch no disk: a listing or a status changes only through the engine's calls.
        #[cfg(test)]
        let (files, git) = (
            FilesHub::quiet(store.clone(), spawn.clone()),
            CheckoutHub::quiet(store.clone(), spawn),
        );
        #[cfg(not(test))]
        let (files, git) = (
            FilesHub::new(store.clone(), spawn.clone()),
            CheckoutHub::new(store.clone(), spawn),
        );
        git.attach(workspace.clone());
        let service = Arc::new(EngineService::new(
            SessionService::new(hub.clone()),
            WorkspaceService::new(workspace.clone()),
            FilesService::new(files.clone()),
            GitService::new(git.clone()),
            TerminalService::new(terminals.clone()),
        ));
        let runtime = if cfg!(test) { None } else { RUNTIME.as_ref() };
        let link = match runtime {
            Some(runtime) => {
                let _entered = runtime.enter();
                Link::Rpc(Arc::new(onicode_rpc::memory_client(service)))
            }
            None => Link::Direct(service),
        };
        // The schedulers' clock: a beat now for what came due while the app was closed, then
        // every 30 seconds. Tests beat in virtual time instead (`tick`).
        if !cfg!(test) {
            let clock = workspace.clone();
            let timers = executor.clone();
            executor
                .spawn(async move {
                    loop {
                        if let Err(error) = clock.tick(onicode_core::time::now_ms()) {
                            eprintln!("The engine's schedulers could not run: {error}");
                        }
                        timers.timer(SCHEDULER_BEAT).await;
                    }
                })
                .detach();
        }
        let engine = Arc::new(Self {
            store: store.clone(),
            hub,
            workspace,
            _files: files,
            _git: git,
            _terminals: terminals,
            link,
            executor,
            #[cfg(test)]
            calls: Mutex::default(),
        });
        cx.default_global::<Engines>().0.push(engine.clone());
        engine
    }

    async fn call<C: Call>(&self, op: Option<OpId>, body: C::Params) -> Result<C::Reply, String> {
        let params = serde_json::to_value(Routed {
            engine: None,
            op,
            body,
        })
        .map_err(|error| error.to_string())?;
        #[cfg(test)]
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((C::NAME, params.clone()));
        let reply = match &self.link {
            Link::Rpc(client) => client
                .call(C::NAME, params)
                .await
                .map_err(|error| error.to_string())?,
            Link::Direct(service) => match service
                .handle(C::NAME, params)
                .await
                .map_err(|error| error.to_string())?
            {
                RpcReply::Value(value) => value,
                RpcReply::Stream(_) => return Err(format!("{} answered with a stream", C::NAME)),
            },
        };
        serde_json::from_value(reply).map_err(|error| error.to_string())
    }

    async fn watch<W: Watch>(
        &self,
        body: W::Params,
    ) -> Result<BoxStream<'static, Item<W::State, W::Event>>, String>
    where
        W::State: Send + 'static,
        W::Event: Send + 'static,
    {
        let params = serde_json::to_value(Routed {
            engine: None,
            op: None,
            body,
        })
        .map_err(|error| error.to_string())?;
        let values = match &self.link {
            Link::Rpc(client) => {
                let subscription = client
                    .subscribe_scoped(W::NAME, params)
                    .await
                    .map_err(|error| error.to_string())?;
                futures::stream::unfold(subscription, |mut subscription| async move {
                    let value = subscription.recv().await?;
                    Some((value, subscription))
                })
                .boxed()
            }
            Link::Direct(service) => match service
                .handle(W::NAME, params)
                .await
                .map_err(|error| error.to_string())?
            {
                RpcReply::Stream(values) => values,
                RpcReply::Value(_) => {
                    return Err(format!("{} answered with a value", W::NAME));
                }
            },
        };
        Ok(values
            .filter_map(|value| futures::future::ready(serde_json::from_value(value).ok()))
            .boxed())
    }
}

/// How the conversation hands a running turn its approvals, answers and steers.
pub(crate) enum TurnControls {
    /// A runner that reads them itself: SSH and legacy remote turns.
    Channel(mpsc::Sender<Control>),
    /// The engine's turn, answered through the Session calls.
    Engine(EngineControls),
}

impl TurnControls {
    /// Hands `control` to the turn. A channel whose runner is gone refuses it; the engine answers
    /// one it cannot apply with a receipt instead.
    pub(crate) fn send(&self, control: Control) -> Result<(), Box<mpsc::SendError<Control>>> {
        match self {
            Self::Channel(sender) => sender.send(control).map_err(Box::new),
            Self::Engine(engine) => {
                engine.send(control);
                Ok(())
            }
        }
    }
}

impl From<mpsc::Sender<Control>> for TurnControls {
    fn from(sender: mpsc::Sender<Control>) -> Self {
        Self::Channel(sender)
    }
}

pub(crate) struct EngineControls {
    engine: Arc<EngineClient>,
    session: SessionId,
    viewer: Arc<Mutex<Viewer>>,
    /// Wakes the turn's forwarder to hand the conversation what the viewer holds now.
    nudge: Arc<Notify>,
}

impl EngineControls {
    fn send(&self, control: Control) {
        let session = self.session;
        let op = OpId::new();
        let engine = self.engine.clone();
        let id = control.id;
        let call: BoxFuture<'static, Result<Outcome, String>> = match control.decision {
            Decision::Steer(message) => {
                // The queued message is stored already: the engine takes it out of the queue by
                // its id, and the receipt names it delivered or failed.
                let Ok(queued) = message.id.parse::<MessageId>() else {
                    // Failed before it left: the conversation shows it now, not at the turn's
                    // next update.
                    lock(&self.viewer).fail_steer(
                        message.id,
                        "This message has no id the engine knows".to_owned(),
                    );
                    self.nudge.notify_one();
                    return;
                };
                lock(&self.viewer).expect(op, Expect::Steer { id: message.id });
                let body = SteerNow {
                    session,
                    id: queued,
                };
                async move { engine.call::<SteerNow>(Some(op), body).await }.boxed()
            }
            Decision::Answer(answers) => {
                let body = Answer {
                    session,
                    question: id,
                    answers,
                };
                async move { engine.call::<Answer>(Some(op), body).await }.boxed()
            }
            Decision::KeepOpen => {
                let body = KeepQuestionOpen {
                    session,
                    question: id,
                };
                async move { engine.call::<KeepQuestionOpen>(Some(op), body).await }.boxed()
            }
            decision => {
                let answer = match decision {
                    Decision::Allow => Verdict::Allow,
                    Decision::Session => Verdict::AllowForSession,
                    _ => Verdict::Deny,
                };
                let body = Decide {
                    session,
                    approval: id,
                    answer,
                };
                async move { engine.call::<Decide>(Some(op), body).await }.boxed()
            }
        };
        self.engine
            .executor
            .spawn(async move {
                // Applied or not, the turn's events say what became of it.
                let _ = call.await;
            })
            .detach();
    }
}

/// Runs `prompt` as a turn of `summary` in the engine serving `store`. Its updates arrive on
/// `updates`, one per beat: the conversation merges them as it merged the turn's own.
pub(crate) fn start_turn(
    store: &Database,
    summary: Session,
    prompt: String,
    settings: Settings,
    cancel: Arc<AtomicBool>,
    updates: async_channel::Sender<Update>,
    cx: &mut App,
) -> TurnControls {
    let engine = EngineClient::get(store, cx);
    let session = match engine.hub.session_id(summary.id) {
        Ok(session) => session,
        Err(status) => {
            let _ = updates.try_send(Update {
                status,
                finished: true,
                ..Update::default()
            });
            return mpsc::channel::<Control>().0.into();
        }
    };
    let message = MessageId::new();
    let op = OpId::new();
    let viewer = Arc::new(Mutex::new(Viewer::default()));
    {
        let mut viewer = lock(&viewer);
        viewer.echo(message);
        viewer.expect(
            op,
            Expect::Launch {
                queued: settings.mono_message_id.clone(),
            },
        );
    }
    let send = session::Send {
        session,
        message,
        text: prompt.clone(),
        attachments: settings.attachments.clone(),
    };
    engine.hub.stage(
        message,
        Staged::Turn(Box::new(StagedTurn {
            summary,
            prompt,
            settings,
            cancel,
        })),
    );
    let forwarder = engine.clone();
    let held = viewer.clone();
    let nudge = Arc::new(Notify::new());
    let nudged = nudge.clone();
    engine
        .executor
        .spawn(async move {
            let turn = forward(forwarder, session, op, send, &held, &nudged, &updates).await;
            if let Err(status) = turn
                && let Some(update) = refuse(&held, status)
            {
                let _ = updates.send(update).await;
            }
        })
        .detach();
    TurnControls::Engine(EngineControls {
        engine,
        session,
        viewer,
        nudge,
    })
}

enum Next {
    Item(Box<Option<Item<SessionState, SessionEvent>>>),
    Refused(String),
    Sent,
    /// The conversation's side changed what the viewer holds (a steer that failed before it
    /// left): hand it on now.
    Nudged,
}

/// The session's items as a turn's forwarder reads them.
type Items = BoxStream<'static, Item<SessionState, SessionEvent>>;

fn watching(session: SessionId, since: Option<Cursor>) -> WatchSession {
    WatchSession {
        session,
        since,
        window_turns: 1,
    }
}

/// Watches the session, sends the turn and forwards what arrives until the turn finishes or the
/// conversation stops listening.
async fn forward(
    engine: Arc<EngineClient>,
    session: SessionId,
    op: OpId,
    send: session::Send,
    viewer: &Mutex<Viewer>,
    nudge: &Notify,
    updates: &async_channel::Sender<Update>,
) -> Result<(), String> {
    // Watch first: the turn's events follow the snapshot.
    let mut items = engine
        .watch::<WatchSession>(watching(session, None))
        .await?;
    let first = items.next().await.ok_or("The engine closed this session")?;
    absorb(viewer, first);
    let (refusal, refusals) = async_channel::bounded(1);
    let caller = engine.clone();
    engine
        .executor
        .spawn(async move {
            // Applied arrives with the turn's first update, a denial with its last; only a call
            // the engine could not take leaves the turn unstarted.
            if let Err(error) = caller.call::<session::Send>(Some(op), send).await {
                let _ = refusal.send(error).await;
            }
        })
        .detach();
    relay(
        &engine,
        session,
        items,
        Some(refusals),
        viewer,
        nudge,
        updates,
    )
    .await
}

/// Folds the session's `items` into one update per beat until the turn finishes or the
/// conversation stops listening. `refused` names why the turn never started; a `nudge` hands on
/// what the viewer holds without waiting for the session's next event.
async fn relay(
    engine: &EngineClient,
    session: SessionId,
    mut items: Items,
    mut refused: Option<async_channel::Receiver<String>>,
    viewer: &Mutex<Viewer>,
    nudge: &Notify,
    updates: &async_channel::Sender<Update>,
) -> Result<(), String> {
    let rewatch = |viewer: &Mutex<Viewer>| watching(session, cursor(viewer));
    loop {
        let nudged = pin!(nudge.notified());
        let beat = select(items.next(), nudged);
        let next = match &refused {
            Some(receiver) => match select(beat, pin!(receiver.recv())).await {
                Either::Left((Either::Left((item, _)), _)) => Next::Item(Box::new(item)),
                Either::Left((Either::Right(((), _)), _)) => Next::Nudged,
                Either::Right((Ok(reason), _)) => Next::Refused(reason),
                Either::Right((Err(_), _)) => Next::Sent,
            },
            None => match beat.await {
                Either::Left((item, _)) => Next::Item(Box::new(item)),
                Either::Right(((), _)) => Next::Nudged,
            },
        };
        let mut finished = match next {
            Next::Item(item) => match *item {
                Some(Item::Resync) => {
                    items = engine.watch::<WatchSession>(rewatch(viewer)).await?;
                    continue;
                }
                Some(item) => absorb(viewer, item),
                None => return Err("The engine closed this session".into()),
            },
            Next::Refused(reason) => {
                // The refusal finishes the turn: its update is taken below.
                lock(viewer).refuse(reason);
                true
            }
            Next::Sent => {
                refused = None;
                continue;
            }
            Next::Nudged => false,
        };
        // Whatever else is ready joins this beat's update.
        while !finished && let Some(Some(item)) = items.next().now_or_never() {
            if matches!(item, Item::Resync) {
                items = engine.watch::<WatchSession>(rewatch(viewer)).await?;
                continue;
            }
            finished = absorb(viewer, item);
        }
        if let Some(update) = take(viewer)
            && updates.send(update).await.is_err()
        {
            // The conversation closed; the turn runs on and saves its rows.
            return Ok(());
        }
        if finished {
            return Ok(());
        }
    }
}

/// Makes a call to the engine serving `store`, off the main thread. A mutating call carries the
/// viewer's `op`: the caller echoes its effect in the same frame and settles the echo with the
/// reply, and a retry under the same op gets the first reply.
pub(crate) fn call<C: Call + 'static>(
    store: &Database,
    op: Option<OpId>,
    body: C::Params,
    cx: &mut App,
) -> Task<Result<C::Reply, String>>
where
    C::Params: Send + 'static,
    C::Reply: Send + 'static,
{
    let engine = EngineClient::get(store, cx);
    cx.background_executor()
        .spawn(async move { engine.call::<C>(op, body).await })
}

/// Makes a call naming the session stored at `row`, its engine id looked up off the main thread.
/// A mutating call carries the viewer's `op`, as [`call`]'s do.
pub(crate) fn session_call<C: Call + 'static>(
    store: &Database,
    row: i64,
    op: Option<OpId>,
    body: impl FnOnce(SessionId) -> C::Params + Send + 'static,
    cx: &mut App,
) -> Task<Result<C::Reply, String>>
where
    C::Params: Send + 'static,
    C::Reply: Send + 'static,
{
    let engine = EngineClient::get(store, cx);
    cx.background_executor().spawn(async move {
        let session = engine.hub.session_id(row)?;
        engine.call::<C>(op, body(session)).await
    })
}

/// The engine serving a store, held by a view for the calls it makes where no `App` is at hand:
/// its `Drop`, or a task it spawns.
#[derive(Clone)]
pub(crate) struct Engine(Arc<EngineClient>);

/// The engine serving `store`.
pub(crate) fn engine(store: &Database, cx: &mut App) -> Engine {
    Engine(EngineClient::get(store, cx))
}

/// Names `store` as the app's own engine's: the shell's store, which [`local`] serves.
pub(crate) fn set_local(store: &Database, cx: &mut App) {
    cx.set_global(LocalStore(store.clone()));
}

/// The app's own engine: the one this machine's files and checkouts are read and changed
/// through. A view made before any shell (a test's) gets an engine over an in-memory store.
pub(crate) fn local(cx: &mut App) -> Engine {
    let store = match cx.try_global::<LocalStore>() {
        Some(local) => local.0.clone(),
        None => {
            let opened = onicode_core::store::Store::open(std::path::Path::new(":memory:"))
                .map(|store| Arc::new(Mutex::new(store)));
            match opened {
                Ok(store) => {
                    set_local(&store, cx);
                    store
                }
                // An in-memory store that cannot open leaves the first engine there is.
                Err(error) => match cx
                    .try_global::<Engines>()
                    .and_then(|engines| engines.0.first().cloned())
                {
                    Some(engine) => return Engine(engine),
                    None => panic!("no engine can start: {error}"),
                },
            }
        }
    };
    Engine(EngineClient::get(&store, cx))
}

/// What a watcher receives: the whole state, or what changed since.
pub(crate) enum WatchUpdate<S, E> {
    Snapshot(Box<S>),
    Events(Vec<E>),
}

/// A watch the app holds: its updates, until it is dropped, which lets the engine's watch go.
pub(crate) struct Watching<S, E> {
    pub(crate) updates: async_channel::Receiver<WatchUpdate<S, E>>,
    _task: Task<()>,
}

impl Engine {
    /// Makes a call, as a future the caller awaits wherever it likes (a view's task, or the
    /// background executor). A mutating call carries the viewer's `op`: the caller echoes its
    /// effect in the same frame and settles the echo with the reply, and a retry under the same
    /// op gets the first reply.
    pub(crate) fn call<C: Call + 'static>(
        &self,
        op: Option<OpId>,
        body: C::Params,
    ) -> impl Future<Output = Result<C::Reply, String>> + Send + 'static + use<C>
    where
        C::Params: Send + 'static,
        C::Reply: Send + 'static,
    {
        let engine = self.0.clone();
        async move { engine.call::<C>(op, body).await }
    }

    /// Watches `W` from the params `params` makes for a cursor: a snapshot, then every batch of
    /// events, resuming with its cursor whenever the engine drops it behind. Ends when the
    /// returned [`Watching`] is dropped, or when the engine stops serving the watch.
    pub(crate) fn watch<W: Watch + 'static>(
        &self,
        params: impl Fn(Option<Cursor>) -> W::Params + Send + 'static,
    ) -> Watching<W::State, W::Event>
    where
        W::Params: Send + 'static,
        W::State: Send + 'static,
        W::Event: Send + 'static,
    {
        let (updates, received) = async_channel::unbounded();
        let task = self
            .0
            .executor
            .spawn(resume::<W>(self.0.clone(), params, updates));
        Watching {
            updates: received,
            _task: task,
        }
    }

    /// Stops the engine's search run under `search` (`SearchPaths`, `SearchContent`,
    /// `SearchSessions`), as monocode's cancel flag did when its view closed or its query moved
    /// on. Sent off the main thread and let go: a stop that arrives before its search stops it as
    /// it starts.
    pub(crate) fn stop_search(&self, search: String) {
        let engine = self.0.clone();
        self.0
            .executor
            .spawn(async move {
                let _ = engine
                    .call::<onicode_protocol::files::StopSearch>(
                        None,
                        onicode_protocol::files::StopSearch { search },
                    )
                    .await;
            })
            .detach();
    }

    /// Makes a call naming the session stored at `row`, off the main thread, and lets it go:
    /// what it came to reaches the views through the watches.
    pub(crate) fn session_send<C: Call + 'static>(
        &self,
        row: i64,
        op: Option<OpId>,
        body: impl FnOnce(SessionId) -> C::Params + Send + 'static,
    ) where
        C::Params: Send + 'static,
        C::Reply: Send + 'static,
    {
        let engine = self.0.clone();
        self.0
            .executor
            .spawn(async move {
                if let Ok(session) = engine.hub.session_id(row) {
                    let _ = engine.call::<C>(op, body(session)).await;
                }
            })
            .detach();
    }
}

/// How often a session event watcher checks whether its receiver is still there.
const LISTENER_CHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// The events of the session stored at `row` from now on, without its rows: a watch with no
/// turns in its snapshot, resumed with its cursor whenever the engine drops it behind. Ends when
/// the receiver goes or the engine stops serving the session.
pub(crate) fn session_events(
    store: &Database,
    row: i64,
    cx: &mut App,
) -> async_channel::Receiver<SessionEvent> {
    let engine = EngineClient::get(store, cx);
    let (events, received) = async_channel::unbounded();
    let executor = engine.executor.clone();
    executor
        .clone()
        .spawn(async move {
            let Ok(session) = engine.hub.session_id(row) else {
                return;
            };
            let mut since: Option<Cursor> = None;
            // The watch a `Resync` ended, held until the next one has joined, so the session
            // stays live between them instead of reloading from the store.
            let mut ended = None;
            loop {
                let watch = WatchSession {
                    session,
                    since,
                    window_turns: 0,
                };
                let Ok(mut items) = engine.watch::<WatchSession>(watch).await else {
                    return;
                };
                drop(ended.take());
                let mut resync = false;
                loop {
                    // The watch holds the session open: let it go soon after the listener does.
                    let item = match select(items.next(), executor.timer(LISTENER_CHECK)).await {
                        Either::Left((item, _)) => item,
                        Either::Right(((), _)) => {
                            if events.is_closed() {
                                return;
                            }
                            continue;
                        }
                    };
                    match item {
                        Some(Item::Snapshot { at, .. }) => since = Some(at),
                        Some(Item::Events {
                            last,
                            events: batch,
                            ..
                        }) => {
                            if let Some(cursor) = since.as_mut() {
                                cursor.seq = last;
                            }
                            for event in batch {
                                if events.send(event).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Some(Item::Resync) => {
                            resync = true;
                            break;
                        }
                        None => break,
                    }
                }
                if !resync {
                    return;
                }
                ended = Some(items);
            }
        })
        .detach();
    received
}

/// What a workspace watcher receives: the whole workspace, or what changed since.
pub(crate) type WorkspaceUpdate = WatchUpdate<WorkspaceState, WorkspaceEvent>;

/// Feeds `updates` from the watch `params` names for a cursor: a snapshot, then every non-empty
/// batch of events, resubscribing with the last cursor after `Resync`. Ends when the receiver goes
/// or a stream ends without `Resync` (the engine stopped serving it).
async fn resume<W: Watch>(
    engine: Arc<EngineClient>,
    params: impl Fn(Option<Cursor>) -> W::Params,
    updates: async_channel::Sender<WatchUpdate<W::State, W::Event>>,
) where
    W::State: Send + 'static,
    W::Event: Send + 'static,
{
    let mut cursor: Option<Cursor> = None;
    // The watch a `Resync` ended, held until the next one has joined, so what it watches stays
    // live between them instead of reloading.
    let mut ended = None;
    loop {
        let Ok(mut items) = engine.watch::<W>(params(cursor)).await else {
            return;
        };
        drop(ended.take());
        let mut resync = false;
        while let Some(item) = items.next().await {
            let update = match item {
                Item::Snapshot { at, state } => {
                    cursor = Some(at);
                    WatchUpdate::Snapshot(Box::new(state))
                }
                Item::Events { last, events, .. } => {
                    if let Some(cursor) = cursor.as_mut() {
                        cursor.seq = last;
                    }
                    if events.is_empty() {
                        continue;
                    }
                    WatchUpdate::Events(events)
                }
                Item::Resync => {
                    resync = true;
                    break;
                }
            };
            if updates.send(update).await.is_err() {
                return;
            }
        }
        if !resync {
            return;
        }
        ended = Some(items);
    }
}

/// Watches the workspace of the engine serving `store`: a snapshot, then every batch of events,
/// resuming with its cursor whenever the engine drops it behind. Ends when the receiver goes.
pub(crate) fn watch_workspace(
    store: &Database,
    cx: &mut App,
) -> async_channel::Receiver<WorkspaceUpdate> {
    let engine = EngineClient::get(store, cx);
    let (updates, received) = async_channel::unbounded();
    engine
        .executor
        .clone()
        .spawn(resume::<WatchWorkspace>(
            engine,
            |since| WatchWorkspace { since },
            updates,
        ))
        .detach();
    received
}

/// Whether an enabled automation of the engine serving `store` waits on Inbox events, so the
/// Inbox keeps polling in the background for it.
pub(crate) fn has_event_automations(store: &Database, cx: &mut App) -> Task<bool> {
    let engine = EngineClient::get(store, cx);
    cx.background_executor()
        .spawn(async move { engine.workspace.has_event_automations().unwrap_or(false) })
}

/// The automations of the engine serving `store`, for the Automations view to list and edit.
/// Their runs are the engine's: it claims them, and the shell starts them through
/// `PrepareAutomationRun`. Editing them moves into the engine with its settings (engine.md,
/// migration step 6).
pub(crate) fn automations(
    store: &Database,
    cx: &mut App,
) -> Task<Result<onicode_core::automations::Service, String>> {
    let store = store.clone();
    cx.background_executor().spawn(async move {
        let path = onicode_engine::turn::locked(&store)?
            .database_path()
            .to_path_buf();
        onicode_core::automations::Service::open(&path)
    })
}

/// Creates a session in the engine serving `store` at once, for a test to seed the workspace
/// with.
#[cfg(test)]
pub(crate) fn create_for_test(
    store: &Database,
    provider: &str,
    cwd: &str,
    cx: &mut App,
) -> Session {
    use onicode_protocol::workspace::{CreateSession, NewSession};
    let workspace = EngineClient::get(store, cx).workspace.clone();
    let created = futures::executor::block_on(workspace.create_session(
        Some(OpId::new()),
        CreateSession {
            session: SessionId::new(),
            provider: provider.into(),
            cwd: cwd.into(),
            title: None,
            start: NewSession::Bare,
            started_by: None,
        },
    ))
    .expect("the engine creates the session");
    created.session.expect("a created session").session
}

/// Tells the engine serving `store` that session `row` changed in the store behind it (the app
/// bridge's launches), so its watchers see it.
pub(crate) fn refresh_session(store: &Database, row: i64, cx: &mut App) {
    let engine = EngineClient::get(store, cx);
    cx.background_executor()
        .spawn(async move { engine.workspace.refresh_session(row) })
        .detach();
}

/// One beat of the engine's schedulers at `now`, in a test's virtual time.
#[cfg(test)]
pub(crate) fn tick(store: &Database, now: u64, cx: &mut App) -> Result<(), String> {
    EngineClient::get(store, cx).workspace.tick(now)
}

/// Every call the engine serving `store` was asked, by name with its routed params.
#[cfg(test)]
pub(crate) fn calls_for_test(
    store: &Database,
    cx: &mut App,
) -> Vec<(&'static str, serde_json::Value)> {
    EngineClient::get(store, cx)
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use onicode_core::follow_up::Message;
    use onicode_core::store::Store;

    use super::*;

    #[gpui::test]
    fn a_steer_that_fails_before_it_leaves_shows_at_once(cx: &mut gpui::TestAppContext) {
        let store: Database = Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()));
        let engine = cx.update(|cx| EngineClient::get(&store, cx));
        let session = SessionId::new();
        let viewer = Arc::new(Mutex::new(Viewer::default()));
        let nudge = Arc::new(Notify::new());
        let (updates, received) = async_channel::bounded(1);
        // The turn started, and its session sends nothing more.
        let (_refusal, refusals) = async_channel::bounded::<String>(1);
        let relayed = {
            let (engine, viewer, nudge) = (engine.clone(), viewer.clone(), nudge.clone());
            cx.executor().spawn(async move {
                let items: Items = futures::stream::pending().boxed();
                relay(
                    &engine,
                    session,
                    items,
                    Some(refusals),
                    &viewer,
                    &nudge,
                    &updates,
                )
                .await
            })
        };
        cx.run_until_parked();
        assert!(received.try_recv().is_err(), "nothing to hand on yet");
        let controls = EngineControls {
            engine,
            session,
            viewer,
            nudge,
        };
        controls.send(Control {
            id: "steer".into(),
            decision: Decision::Steer(Message {
                id: "not-a-message-id".into(),
                text: "Look at the build".into(),
                ..Message::default()
            }),
        });
        cx.run_until_parked();
        let update = received
            .try_recv()
            .expect("the failure reaches the conversation without a session event");
        assert_eq!(
            update.follow_up_errors,
            vec![(
                "not-a-message-id".to_owned(),
                "This message has no id the engine knows".to_owned()
            )]
        );
        assert!(!update.finished, "the turn runs on");
        drop(relayed);
    }
}
