//! The app's link to its engine (engine.md, "Local feel", migration step 8). The engine is its
//! own process, `onicode-engine`, beside the app: the app reaches it on the socket in its data
//! dir, launching it when nothing answers ([`onicode_engine::link`]), and speaks the protocol to
//! it over one connection that every call and watch shares.
//!
//! The app's first frame never waits on the engine: the workspace and the engine settings are
//! mirrored from their watches into `viewer-cache.json`, and a launch reads them from there
//! ([`Engine::workspace_now`], [`Engine::settings_now`]); only a first launch, with nothing kept,
//! waits for the engine's answer. A watch whose connection drops resumes with its cursor on the
//! next one, and an engine that restarted meanwhile answers with a fresh snapshot. A call whose
//! connection drops is sent again under the same op.
//!
//! A local turn runs in the engine: the conversation hands it the composer's context with the
//! prompt (`Send.turn`), watches the session and folds what arrives back into one `turn::Update`
//! per beat. The turn outlives the window; the conversation's stop sends `Stop`. SSH and legacy
//! remote turns keep their own runners over [`Engine::legacy_store`]. The explorer, editor,
//! Changes, review, history and worktree views read and change files and checkouts through the
//! app's own engine ([`local`]).
//!
//! Every call and watch names the engine that serves it (`Routed.engine`): `None` for the app's
//! own, or another engine on the tailnet, which the app's own engine forwards to (engine.md,
//! "Step 9 as built"). [`Engine::on`] is the same connection aimed at another engine; the app never
//! dials one itself. The machines the app's engine knows are mirrored like the workspace, so a
//! turn for another engine's session knows at once whether that engine is reachable; while it is
//! not, the turn is refused with a status saying so and the conversation keeps what it last saw,
//! read-only. A mutating call made meanwhile waits in the app's engine's outbox.
//!
//! Tests run an engine in memory and call its service in place, on gpui's deterministic
//! executors (`Engine::memory`, built only for tests).
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError, Weak, mpsc};
use std::time::Duration;

use futures::future::{BoxFuture, Either, select};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use gpui::{App, BackgroundExecutor, Global, Task};
use onicode_engine::link::{SocketDoor, Timing};
use onicode_engine::session::{Expect, Viewer, apply};
use onicode_engine::turn::{Database, Update};
use onicode_protocol::agent::{Control, Decision, Settings};
use onicode_protocol::engine::{
    MachineEvent, MachineKind, MachineLink, Machines, ResolveSession, WatchMachines,
};
use onicode_protocol::engine_settings::{EngineSettings, EngineSettingsEvent, WatchEngineSettings};
use onicode_protocol::session::{
    self, Answer, Decide, KeepQuestionOpen, SessionEvent, SessionState, SteerNow, Stop,
    TurnContext, Verdict, WatchSession,
};
use onicode_protocol::sessions::Session;
use onicode_protocol::transcript::Row;
use onicode_protocol::usage::Limit;
use onicode_protocol::workspace::{WatchWorkspace, WorkspaceEvent, WorkspaceState};
use onicode_protocol::{
    Call, Cursor, EngineId, Item, MessageId, OpId, Outcome, Routed, SessionId, Watch,
};
#[cfg(test)]
use onicode_rpc::RpcService as _;
use onicode_rpc::RpcSubscription;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// The link's tokio thread: it serves the engine socket's one connection.
static RUNTIME: LazyLock<Option<tokio::runtime::Runtime>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("onicode-link")
        .enable_all()
        .build()
        .ok()
});

/// What the app keeps of the engine's workspace and settings, beside its data.
const VIEWER_CACHE: &str = "viewer-cache.json";
/// How long a first launch, with nothing kept, waits for the engine's workspace.
const FIRST_ANSWER: Duration = Duration::from_secs(10);
/// How long the mirror lets changes gather before it writes them down.
const CACHE_PAUSE: Duration = Duration::from_secs(2);
/// How long a watch the engine could not take waits before it asks again.
const RETRY: Duration = Duration::from_secs(1);

/// The app's own engine: the one the shell opened, which [`local`] serves.
struct LocalEngine(Engine);

impl Global for LocalEngine {}

/// The workspace, engine settings and machines as this viewer last saw them.
#[derive(Default, Serialize, Deserialize)]
struct Seen {
    workspace: Option<WorkspaceState>,
    settings: Option<EngineSettings>,
    machines: Option<Machines>,
}

/// [`Seen`], followed from the engine's watches and kept on disk, so a window's first frame
/// draws what the last one showed while the engine starts.
struct Mirror {
    seen: Mutex<Seen>,
    arrived: Condvar,
    dirty: AtomicBool,
    file: PathBuf,
}

impl Mirror {
    /// What the last run kept in `file`; nothing when it is missing or unreadable.
    fn load(file: PathBuf) -> Self {
        let seen = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            seen: Mutex::new(seen),
            arrived: Condvar::new(),
            dirty: AtomicBool::new(false),
            file,
        }
    }

    fn update(&self, change: impl FnOnce(&mut Seen)) {
        change(&mut self.seen.lock().unwrap_or_else(PoisonError::into_inner));
        self.dirty.store(true, Ordering::Release);
        self.arrived.notify_all();
    }

    /// What `pick` reads, waiting for the engine's first answer when nothing is kept yet.
    fn now<T>(&self, pick: impl Fn(&Seen) -> Option<T>) -> Option<T> {
        let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let (seen, _) = self
            .arrived
            .wait_timeout_while(seen, FIRST_ANSWER, |seen| pick(seen).is_none())
            .unwrap_or_else(PoisonError::into_inner);
        pick(&seen)
    }

    /// Writes what changed since the last save, whole, through a temporary file.
    fn save(&self) {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let bytes = {
            let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            serde_json::to_vec(&*seen)
        };
        let Ok(bytes) = bytes else {
            return;
        };
        let temporary = self.file.with_extension("json.tmp");
        if let Err(error) =
            std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, &self.file))
        {
            eprintln!("Cannot keep the workspace for the next launch: {error}");
        }
    }
}

fn fold_workspace(seen: &mut Seen, update: WorkspaceUpdate) {
    match update {
        WatchUpdate::Snapshot(state) => seen.workspace = Some(*state),
        WatchUpdate::Events(events) => {
            if let Some(state) = seen.workspace.as_mut() {
                for event in &events {
                    onicode_engine::workspace::apply(state, event);
                }
            }
        }
    }
}

fn fold_settings(seen: &mut Seen, update: WatchUpdate<EngineSettings, EngineSettingsEvent>) {
    match update {
        WatchUpdate::Snapshot(settings) => seen.settings = Some(*settings),
        WatchUpdate::Events(events) => {
            if let Some(settings) = seen.settings.as_mut() {
                for event in &events {
                    if let EngineSettingsEvent::Set(setting) = event {
                        settings.apply(setting);
                    }
                }
            }
        }
    }
}

fn fold_machines(seen: &mut Seen, update: WatchUpdate<Machines, MachineEvent>) {
    match update {
        WatchUpdate::Snapshot(machines) => seen.machines = Some(*machines),
        WatchUpdate::Events(events) => {
            if let Some(machines) = seen.machines.as_mut() {
                for event in &events {
                    machines.apply(event);
                }
            }
        }
    }
}

enum Link {
    /// The engine process, over its socket, served on the link's tokio thread.
    Socket {
        link: Arc<onicode_engine::link::Link>,
        mirror: Arc<Mirror>,
        dir: PathBuf,
        /// The SSH remote's and the phone host's own connection to the store.
        legacy: OnceLock<Database>,
    },
    /// An engine in memory, its service called in place: tests stay on gpui's deterministic
    /// executors.
    #[cfg(test)]
    Direct(Box<onicode_engine::Local>),
}

/// Turns of a test's engine run on gpui's background threads.
#[cfg(test)]
struct GpuiSpawn(BackgroundExecutor);

#[cfg(test)]
impl onicode_engine::session::Spawn for GpuiSpawn {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.spawn(work).detach();
    }
}

/// The chores of an app test's engine: no generated title or branch, and only the work item a
/// prompt's GitHub link names.
#[cfg(test)]
struct TestChores;

#[cfg(test)]
impl onicode_engine::workspace::Chores for TestChores {
    fn title(
        &self,
        _: onicode_protocol::session_title::TextModel,
        _: &std::path::Path,
        _: &str,
        _: Arc<AtomicBool>,
    ) -> Option<onicode_protocol::session_title::GeneratedTitle> {
        None
    }

    fn work_item(
        &self,
        message: &str,
        _: &std::path::Path,
        _: Option<&onicode_protocol::session_title::WorkItemHint>,
    ) -> Option<onicode_protocol::session_groups::WorkItem> {
        onicode_protocol::session_groups::parse_github_work_item(message)
    }

    fn branch(
        &self,
        _: onicode_protocol::session_title::TextModel,
        _: &std::path::Path,
        _: &str,
        _: Arc<AtomicBool>,
    ) -> Option<String> {
        None
    }
}

pub(crate) struct EngineClient {
    link: Link,
    executor: BackgroundExecutor,
    /// Each stored session's id, by the engine that stores it (`None`: the app's own) and its
    /// row there, as that engine named it; a session keeps its id.
    sessions: Mutex<HashMap<(Option<EngineId>, i64), SessionId>>,
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

/// A subscription's items. One whose connection closed under it ends with `Resync`: the watcher
/// resumes with its cursor on the next connection.
fn items<S, E>(subscription: RpcSubscription) -> BoxStream<'static, Item<S, E>>
where
    S: DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
{
    futures::stream::unfold(Some(subscription), |held| async move {
        let mut subscription = held?;
        loop {
            match subscription.recv().await {
                Some(value) => {
                    if let Ok(item) = serde_json::from_value(value) {
                        return Some((item, Some(subscription)));
                    }
                }
                None if subscription.lost() => return Some((Item::Resync, None)),
                None => return None,
            }
        }
    })
    .boxed()
}

/// A value of a test engine's stream, parsed; `None` skips one that is not an item.
#[cfg(test)]
fn parsed<T: DeserializeOwned>(value: serde_json::Value) -> futures::future::Ready<Option<T>> {
    futures::future::ready(serde_json::from_value(value).ok())
}

impl EngineClient {
    fn new(link: Link, cx: &App) -> Arc<Self> {
        Arc::new(Self {
            link,
            executor: cx.background_executor().clone(),
            sessions: Mutex::default(),
            #[cfg(test)]
            calls: Mutex::default(),
        })
    }

    /// Whether a watch the engine could not take is worth asking again: the engine may be
    /// starting, or restarting.
    fn reconnects(&self) -> bool {
        matches!(self.link, Link::Socket { .. })
    }

    /// The test engine itself.
    #[cfg(test)]
    fn direct(&self) -> &onicode_engine::Local {
        match &self.link {
            Link::Direct(local) => local,
            Link::Socket { .. } => panic!("a test's engine runs in memory"),
        }
    }

    /// Makes call `C` of the engine `target` names (`None`: the app's own), through the app's own
    /// engine.
    async fn call<C: Call>(
        &self,
        target: Option<&EngineId>,
        op: Option<OpId>,
        body: C::Params,
    ) -> Result<C::Reply, String> {
        let params = serde_json::to_value(Routed {
            engine: target.cloned(),
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
            Link::Socket { link, .. } => link.call(C::NAME, &params).await?,
            #[cfg(test)]
            Link::Direct(local) => match local
                .service
                .handle(C::NAME, params)
                .await
                .map_err(|error| error.to_string())?
            {
                onicode_rpc::RpcReply::Value(value) => value,
                onicode_rpc::RpcReply::Stream(_) => {
                    return Err(format!("{} answered with a stream", C::NAME));
                }
            },
        };
        serde_json::from_value(reply).map_err(|error| error.to_string())
    }

    /// Watches `W` on the engine `target` names, through the app's own engine.
    async fn watch<W: Watch>(
        &self,
        target: Option<&EngineId>,
        body: W::Params,
    ) -> Result<BoxStream<'static, Item<W::State, W::Event>>, String>
    where
        W::State: Send + 'static,
        W::Event: Send + 'static,
    {
        let params = serde_json::to_value(Routed {
            engine: target.cloned(),
            op: None,
            body,
        })
        .map_err(|error| error.to_string())?;
        match &self.link {
            Link::Socket { link, .. } => Ok(items(link.subscribe(W::NAME, &params).await?)),
            #[cfg(test)]
            Link::Direct(local) => match local
                .service
                .handle(W::NAME, params)
                .await
                .map_err(|error| error.to_string())?
            {
                onicode_rpc::RpcReply::Stream(values) => Ok(values.filter_map(parsed).boxed()),
                onicode_rpc::RpcReply::Value(_) => {
                    Err(format!("{} answered with a value", W::NAME))
                }
            },
        }
    }

    /// The machines the app's own engine knows, as last seen: kept across launches like the
    /// workspace, `None` before the engine first answered.
    fn machines_now(&self) -> Option<Machines> {
        match &self.link {
            Link::Socket { mirror, .. } => mirror
                .seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .machines
                .clone(),
            #[cfg(test)]
            Link::Direct(local) => Some(local.machines.current()),
        }
    }

    /// Whether the engine `target` names answers now: the app's own always does; another while
    /// the app's engine holds a live link to it.
    fn reachable(&self, target: Option<&EngineId>) -> bool {
        let Some(target) = target else {
            return true;
        };
        self.machines_now()
            .and_then(|machines| machines.get(target).cloned())
            .is_some_and(|machine| {
                matches!(machine.kind, MachineKind::This)
                    || matches!(machine.link, MachineLink::Here | MachineLink::Live)
            })
    }

    /// The id of the session stored at `row` in the engine `target` names.
    async fn session_id(&self, target: Option<&EngineId>, row: i64) -> Result<SessionId, String> {
        let key = (target.cloned(), row);
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return Ok(*session);
        }
        let session = match (&self.link, target) {
            #[cfg(test)]
            (Link::Direct(local), None) => local.session.session_id(row)?,
            _ => self
                .call::<ResolveSession>(target, None, ResolveSession { row })
                .await?
                .ok_or("Session no longer exists")?,
        };
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, session);
        Ok(session)
    }
}

/// Follows watch `W` into the mirror with `fold`, for as long as the app runs.
fn follow<W: Watch + 'static>(
    engine: &Arc<EngineClient>,
    mirror: &Arc<Mirror>,
    params: impl Fn(Option<Cursor>) -> W::Params + Send + 'static,
    fold: fn(&mut Seen, WatchUpdate<W::State, W::Event>),
) where
    W::Params: Send + 'static,
    W::State: Send + 'static,
    W::Event: Send + 'static,
{
    let (updates, received) = async_channel::unbounded();
    engine
        .executor
        .spawn(resume::<W>(engine.clone(), None, params, updates))
        .detach();
    let mirror = mirror.clone();
    engine
        .executor
        .spawn(async move {
            while let Ok(update) = received.recv().await {
                mirror.update(|seen| fold(seen, update));
            }
        })
        .detach();
}

/// Writes the mirror down a pause after it changes, until it goes.
fn keep(mirror: Weak<Mirror>, executor: BackgroundExecutor) {
    let timers = executor.clone();
    executor
        .spawn(async move {
            loop {
                timers.timer(CACHE_PAUSE).await;
                let Some(mirror) = mirror.upgrade() else {
                    return;
                };
                mirror.save();
            }
        })
        .detach();
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

    /// Whether the engine runs the turn: it outlives the conversation's view, and only an
    /// explicit stop ends it.
    pub(crate) fn is_engine(&self) -> bool {
        matches!(self, Self::Engine(_))
    }

    /// The prompt this view sent, for a turn the engine runs.
    pub(crate) fn message(&self) -> Option<MessageId> {
        match self {
            Self::Channel(_) => None,
            Self::Engine(engine) => engine.turn.message,
        }
    }
}

impl From<mpsc::Sender<Control>> for TurnControls {
    fn from(sender: mpsc::Sender<Control>) -> Self {
        Self::Channel(sender)
    }
}

/// What a turn's controls and its forwarder share.
struct Turn {
    /// The engine that stores the session and runs the turn (`None`: the app's own).
    target: Option<EngineId>,
    /// The session's row in that engine's store.
    row: i64,
    viewer: Mutex<Viewer>,
    /// Wakes the forwarder to hand the conversation what the viewer holds now.
    nudge: Notify,
    /// The conversation's stop: set, the controls' drop stops the engine's turn.
    cancel: Arc<AtomicBool>,
    /// The forwarder saw the turn end.
    finished: AtomicBool,
    /// The prompt this view sent for the turn (`None`: a turn the view joined).
    message: Option<MessageId>,
}

impl Turn {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

/// A Session call for a turn, made once its session's engine id is known.
type Ask = Box<
    dyn FnOnce(
            Arc<EngineClient>,
            Option<EngineId>,
            SessionId,
        ) -> BoxFuture<'static, Result<Outcome, String>>
        + Send,
>;

pub(crate) struct EngineControls {
    engine: Arc<EngineClient>,
    turn: Arc<Turn>,
}

impl EngineControls {
    fn send(&self, control: Control) {
        let op = OpId::new();
        let id = control.id;
        let ask: Ask = match control.decision {
            Decision::Steer(message) => {
                // The queued message is stored already: the engine takes it out of the queue by
                // its id, and the receipt names it delivered or failed.
                let Ok(queued) = message.id.parse::<MessageId>() else {
                    // Failed before it left: the conversation shows it now, not at the turn's
                    // next update.
                    lock(&self.turn.viewer).fail_steer(
                        message.id,
                        "This message has no id the engine knows".to_owned(),
                    );
                    self.turn.nudge.notify_one();
                    return;
                };
                lock(&self.turn.viewer).expect(op, Expect::Steer { id: message.id });
                Box::new(move |engine, target, session| {
                    let body = SteerNow {
                        session,
                        id: queued,
                    };
                    async move {
                        engine
                            .call::<SteerNow>(target.as_ref(), Some(op), body)
                            .await
                    }
                    .boxed()
                })
            }
            Decision::Answer(answers) => Box::new(move |engine, target, session| {
                let body = Answer {
                    session,
                    question: id,
                    answers,
                };
                async move { engine.call::<Answer>(target.as_ref(), Some(op), body).await }.boxed()
            }),
            Decision::KeepOpen => Box::new(move |engine, target, session| {
                let body = KeepQuestionOpen {
                    session,
                    question: id,
                };
                async move {
                    engine
                        .call::<KeepQuestionOpen>(target.as_ref(), Some(op), body)
                        .await
                }
                .boxed()
            }),
            decision => {
                let answer = match decision {
                    Decision::Allow => Verdict::Allow,
                    Decision::Session => Verdict::AllowForSession,
                    _ => Verdict::Deny,
                };
                Box::new(move |engine, target, session| {
                    let body = Decide {
                        session,
                        approval: id,
                        answer,
                    };
                    async move { engine.call::<Decide>(target.as_ref(), Some(op), body).await }
                        .boxed()
                })
            }
        };
        let engine = self.engine.clone();
        let (target, row) = (self.turn.target.clone(), self.turn.row);
        self.engine
            .executor
            .spawn(async move {
                // Applied or not, the turn's events say what became of it.
                if let Ok(session) = engine.session_id(target.as_ref(), row).await {
                    let _ = ask(engine, target, session).await;
                }
            })
            .detach();
    }
}

impl Drop for EngineControls {
    /// The conversation let its controls go. Stopped by the person, the engine's turn stops
    /// too; otherwise it runs on, past the conversation's view and the app's window.
    fn drop(&mut self) {
        if !self.turn.stopped() || self.turn.finished.load(Ordering::Acquire) {
            return;
        }
        let engine = self.engine.clone();
        let (target, row) = (self.turn.target.clone(), self.turn.row);
        self.engine
            .executor
            .spawn(async move {
                if let Ok(session) = engine.session_id(target.as_ref(), row).await {
                    let _ = engine
                        .call::<Stop>(target.as_ref(), Some(OpId::new()), Stop { session })
                        .await;
                }
            })
            .detach();
    }
}

/// Runs `prompt` as a turn of `summary` in `engine` (the engine it names: the app's own or one it
/// forwards to), under the composer's `settings`. Its updates arrive on `updates`, one per beat:
/// the conversation merges them as it merged the turn's own.
pub(crate) fn start_turn(
    engine: &Engine,
    summary: Session,
    prompt: String,
    mut settings: Settings,
    cancel: Arc<AtomicBool>,
    updates: async_channel::Sender<Update>,
    _: &mut App,
) -> TurnControls {
    let target = engine.target.clone();
    let engine = engine.client.clone();
    let message = MessageId::new();
    let op = OpId::new();
    let turn = Arc::new(Turn {
        target,
        row: summary.id,
        viewer: Mutex::new(Viewer::default()),
        nudge: Notify::new(),
        cancel,
        finished: AtomicBool::new(false),
        message: Some(message),
    });
    {
        let mut viewer = lock(&turn.viewer);
        viewer.echo(message);
        viewer.expect(
            op,
            Expect::Launch {
                queued: settings.mono_message_id.clone(),
            },
        );
    }
    // What the settings never save crosses beside them.
    let attachments = std::mem::take(&mut settings.attachments);
    let context = TurnContext {
        prepared_prompt: settings.prepared_prompt.take(),
        turn_model: settings.turn_model.take(),
        environment: std::mem::take(&mut settings.environment),
        queued: settings.mono_message_id.clone(),
        internal: settings.internal_message,
        summary,
        settings,
    };
    let send = move |session| session::Send {
        session,
        message,
        text: prompt,
        attachments,
        turn: Some(Box::new(context)),
    };
    let forwarder = engine.clone();
    let running = turn.clone();
    engine
        .executor
        .spawn(async move {
            let ended = forward(forwarder, &running, op, send, &updates).await;
            running.finished.store(true, Ordering::Release);
            if let Err(status) = ended
                && let Some(update) = refuse(&running.viewer, status)
            {
                let _ = updates.send(update).await;
            }
        })
        .detach();
    TurnControls::Engine(EngineControls { engine, turn })
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
    turn: &Turn,
    op: OpId,
    send: impl FnOnce(SessionId) -> session::Send + Send,
    updates: &async_channel::Sender<Update>,
) -> Result<(), String> {
    let target = turn.target.as_ref();
    if let Some(away) = target
        && !engine.reachable(target)
    {
        // The conversation keeps what it last saw of the session, read-only, under this status.
        return Err(format!(
            "{away} is offline: this session shows what it last saw until that machine is back"
        ));
    }
    let session = engine.session_id(target, turn.row).await?;
    // A turn stopped before it left is still sent: the engine keeps its prompt, and the stop
    // that follows once it is taken (`relay`, `Next::Sent`) ends it with the update that
    // settles the conversation.
    // Watch first: the turn's events follow the snapshot.
    let mut items = engine
        .watch::<WatchSession>(target, watching(session, None))
        .await?;
    let first = items.next().await.ok_or("The engine closed this session")?;
    absorb(&turn.viewer, first);
    let (refusal, refusals) = async_channel::bounded(1);
    let caller = engine.clone();
    let owner = turn.target.clone();
    let body = send(session);
    engine
        .executor
        .spawn(async move {
            // Applied arrives with the turn's first update, a denial with its last; only a call
            // the engine could not take leaves the turn unstarted.
            if let Err(error) = caller
                .call::<session::Send>(owner.as_ref(), Some(op), body)
                .await
            {
                let _ = refusal.send(error).await;
            }
        })
        .detach();
    relay(&engine, session, items, Some(refusals), turn, updates).await
}

/// Absorbs `item` into the turn's viewer, and says whether the turn finished. A snapshot from an
/// engine run other than `epoch`'s that holds no turn, once the turn started, finishes it: the
/// turn stopped with the engine that ran it.
fn take_in(
    viewer: &Mutex<Viewer>,
    item: Item<SessionState, SessionEvent>,
    epoch: Option<u64>,
    started: bool,
) -> bool {
    let restarted = started
        && matches!(&item, Item::Snapshot { at, state }
            if Some(at.epoch) != epoch && state.turn.is_none());
    let finished = absorb(viewer, item);
    if restarted && !finished {
        lock(viewer).refuse("The engine restarted, and this turn stopped with it".to_owned());
        return true;
    }
    finished
}

/// Folds the session's `items` into one update per beat until the turn finishes or the
/// conversation stops listening. `refused` names why the turn never started; the turn's nudge
/// hands on what the viewer holds without waiting for the session's next event.
async fn relay(
    engine: &EngineClient,
    session: SessionId,
    mut items: Items,
    mut refused: Option<async_channel::Receiver<String>>,
    turn: &Turn,
    updates: &async_channel::Sender<Update>,
) -> Result<(), String> {
    let viewer = &turn.viewer;
    let target = turn.target.as_ref();
    let rewatch = |viewer: &Mutex<Viewer>| watching(session, cursor(viewer));
    let epoch = cursor(viewer).map(|at| at.epoch);
    loop {
        let nudged = pin!(turn.nudge.notified());
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
                    items = engine
                        .watch::<WatchSession>(target, rewatch(viewer))
                        .await?;
                    continue;
                }
                Some(item) => take_in(viewer, item, epoch, refused.is_none()),
                None => return Err("The engine closed this session".into()),
            },
            Next::Refused(reason) => {
                // The refusal finishes the turn: its update is taken below.
                lock(viewer).refuse(reason);
                true
            }
            Next::Sent => {
                refused = None;
                if turn.stopped() {
                    // Stopped while the engine started it: a stop sent meanwhile found no turn.
                    let _ = engine
                        .call::<Stop>(target, Some(OpId::new()), Stop { session })
                        .await;
                }
                continue;
            }
            Next::Nudged => false,
        };
        // Whatever else is ready joins this beat's update.
        while !finished && let Some(Some(item)) = items.next().now_or_never() {
            if matches!(item, Item::Resync) {
                items = engine
                    .watch::<WatchSession>(target, rewatch(viewer))
                    .await?;
                continue;
            }
            finished = take_in(viewer, item, epoch, refused.is_none());
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

/// Follows the turn the engine runs now in the session stored at `row`, one this view did not
/// send: an automation's, another viewer's, or the engine's usage-limit resume. The first update
/// carries the rows from `from` on as the turn holds them; a turn that ended before the watch
/// joined arrives finished.
pub(crate) fn join_turn(
    engine: &Engine,
    row: i64,
    from: usize,
    cancel: Arc<AtomicBool>,
    updates: async_channel::Sender<Update>,
    _: &mut App,
) -> TurnControls {
    let target = engine.target.clone();
    let engine = engine.client.clone();
    let turn = Arc::new(Turn {
        target,
        row,
        viewer: Mutex::new(Viewer::default()),
        nudge: Notify::new(),
        cancel,
        finished: AtomicBool::new(false),
        message: None,
    });
    let follower = engine.clone();
    let running = turn.clone();
    engine
        .executor
        .spawn(async move {
            let ended = join(follower, &running, from, &updates).await;
            running.finished.store(true, Ordering::Release);
            if let Err(status) = ended
                && let Some(update) = refuse(&running.viewer, status)
            {
                let _ = updates.send(update).await;
            }
        })
        .detach();
    TurnControls::Engine(EngineControls { engine, turn })
}

/// Watches the session and forwards its running turn until it finishes or the conversation
/// stops listening. One turn's snapshot starts at that turn's prompt; a view holding less than
/// that reads the whole transcript instead.
async fn join(
    engine: Arc<EngineClient>,
    turn: &Turn,
    from: usize,
    updates: &async_channel::Sender<Update>,
) -> Result<(), String> {
    let target = turn.target.as_ref();
    let session = engine.session_id(target, turn.row).await?;
    let mut window = 1;
    loop {
        let watch = WatchSession {
            session,
            since: None,
            window_turns: window,
        };
        let mut items = engine.watch::<WatchSession>(target, watch).await?;
        let Some(Item::Snapshot { at, state }) = items.next().await else {
            return Err("The engine closed this session".into());
        };
        if from < state.first_row && window == 1 {
            window = usize::MAX;
            continue;
        }
        let opening = opening(&state, from);
        let finished = opening.finished;
        absorb(&turn.viewer, Item::Snapshot { at, state });
        if updates.send(opening).await.is_err() || finished {
            return Ok(());
        }
        return relay(&engine, session, items, None, turn, updates).await;
    }
}

/// A joined turn's first update: the rows from `from` on (from the snapshot's first row when
/// the view holds fewer) and the turn's state so far.
fn opening(state: &SessionState, from: usize) -> Update {
    let start = from.clamp(state.first_row, end(state));
    let turn = state.turn.as_ref();
    Update {
        start,
        rows: rows_from(state, start),
        status: state.status.clone(),
        provider_turn_id: turn.and_then(|turn| turn.provider_turn_id.clone()),
        turn_ready: turn.is_some_and(|turn| turn.ready),
        finished: turn.is_none_or(|turn| turn.finished),
        approvals: state.approvals.clone(),
        questions: state.questions.clone(),
        background: (!state.background.is_empty()).then(|| state.background.clone()),
        context: state.context,
        ..Update::default()
    }
}

/// Makes a call to `engine`, off the main thread. A mutating call carries the
/// viewer's `op`: the caller echoes its effect in the same frame and settles the echo with the
/// reply, and a retry under the same op gets the first reply.
pub(crate) fn call<C: Call + 'static>(
    engine: &Engine,
    op: Option<OpId>,
    body: C::Params,
    cx: &mut App,
) -> Task<Result<C::Reply, String>>
where
    C::Params: Send + 'static,
    C::Reply: Send + 'static,
{
    let (client, target) = (engine.client.clone(), engine.target.clone());
    cx.background_executor()
        .spawn(async move { client.call::<C>(target.as_ref(), op, body).await })
}

/// Makes a call naming the session stored at `row`, its engine id looked up off the main thread.
/// A mutating call carries the viewer's `op`, as [`call`]'s do.
pub(crate) fn session_call<C: Call + 'static>(
    engine: &Engine,
    row: i64,
    op: Option<OpId>,
    body: impl FnOnce(SessionId) -> C::Params + Send + 'static,
    cx: &mut App,
) -> Task<Result<C::Reply, String>>
where
    C::Params: Send + 'static,
    C::Reply: Send + 'static,
{
    let (client, target) = (engine.client.clone(), engine.target.clone());
    cx.background_executor().spawn(async move {
        let target = target.as_ref();
        let session = client.session_id(target, row).await?;
        client.call::<C>(target, op, body(session)).await
    })
}

/// An engine as the app reaches it, held by every view that reads or changes what it keeps:
/// cloned freely, and used where no `App` is at hand (a view's `Drop`, or a task it spawns). Its
/// calls and watches name `target`; the connection is always the app's own engine's.
#[derive(Clone)]
pub(crate) struct Engine {
    client: Arc<EngineClient>,
    /// The engine served: `None` for the app's own, else one it forwards to.
    target: Option<EngineId>,
}

/// Names `engine` the app's own: the shell's, which [`local`] serves.
pub(crate) fn set_local(engine: &Engine, cx: &mut App) {
    cx.set_global(LocalEngine(engine.clone()));
}

/// The app's own engine: the one this machine's files and checkouts are read and changed
/// through. A view made before any shell (a test's) gets an engine in memory.
pub(crate) fn local(cx: &mut App) -> Engine {
    if let Some(local) = cx.try_global::<LocalEngine>() {
        return local.0.clone();
    }
    #[cfg(test)]
    let engine = Engine::memory(cx);
    #[cfg(not(test))]
    let engine = match Engine::connect(&crate::data_dir(), cx) {
        Ok(engine) => engine,
        Err(error) => panic!("no engine can start: {error}"),
    };
    set_local(&engine, cx);
    engine
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
    /// The engine over `data_dir`: the `onicode-engine` beside this app, reached on its socket
    /// and launched when nothing answers. Returns at once: the engine is reached in the
    /// background, and the workspace and settings the last launch kept stand in until it answers.
    pub(crate) fn connect(data_dir: &Path, cx: &mut App) -> Result<Self, String> {
        let runtime = RUNTIME
            .as_ref()
            .ok_or("The engine link's thread could not start")?;
        let app =
            std::env::current_exe().map_err(|error| format!("Cannot find the app: {error}"))?;
        let door = SocketDoor::new(data_dir, &app);
        let link = onicode_engine::link::Link::new(
            Arc::new(door),
            runtime.handle().clone(),
            Timing::default(),
        );
        let mirror = Arc::new(Mirror::load(data_dir.join(VIEWER_CACHE)));
        let engine = EngineClient::new(
            Link::Socket {
                link,
                mirror: mirror.clone(),
                dir: data_dir.to_path_buf(),
                legacy: OnceLock::new(),
            },
            cx,
        );
        follow::<WatchWorkspace>(
            &engine,
            &mirror,
            |since| WatchWorkspace { since },
            fold_workspace,
        );
        follow::<WatchEngineSettings>(
            &engine,
            &mirror,
            |since| WatchEngineSettings { since },
            fold_settings,
        );
        follow::<WatchMachines>(
            &engine,
            &mirror,
            |since| WatchMachines { since },
            fold_machines,
        );
        keep(Arc::downgrade(&mirror), engine.executor.clone());
        Ok(Self::own(engine))
    }

    /// The app's own engine over `client`.
    fn own(client: Arc<EngineClient>) -> Self {
        Self {
            client,
            target: None,
        }
    }

    /// The engine `engine` names, reached through this one's connection: the app's own engine
    /// serves what names it and forwards the rest over the tailnet. Every call, watch and turn
    /// made through the result names it.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Machines view (step 12) opens peer sessions through it"
        )
    )]
    #[must_use]
    pub(crate) fn on(&self, engine: EngineId) -> Self {
        Self {
            client: self.client.clone(),
            target: Some(engine),
        }
    }

    /// An engine over a store in memory for a test: no disk watched, no provider CLI, `gh` or
    /// network asked, and its schedulers beat only through `tick`.
    #[cfg(test)]
    pub(crate) fn memory(cx: &mut App) -> Self {
        let options = onicode_engine::Options {
            chores: Some(Arc::new(TestChores)),
            ..onicode_engine::testing::quiet()
        };
        let spawn = Arc::new(GpuiSpawn(cx.background_executor().clone()));
        let local =
            onicode_engine::Local::memory(spawn, options).expect("an engine in memory opens");
        Self::own(EngineClient::new(Link::Direct(Box::new(local)), cx))
    }

    /// Writes and reads a test engine's store as typed calls.
    #[cfg(test)]
    pub(crate) fn seed(&self) -> onicode_engine::testing::Seed {
        self.client.direct().seed()
    }

    /// The store itself: the SSH remote and the phone host only; steps 10-11 delete them. The
    /// app's own connection to the engine's store, opened on first use.
    pub(crate) fn legacy_store(&self) -> Database {
        match &self.client.link {
            Link::Socket { dir, legacy, .. } => legacy
                .get_or_init(|| {
                    onicode_engine::Local::open_legacy_store(dir)
                        .unwrap_or_else(|error| panic!("no store can open: {error}"))
                })
                .clone(),
            #[cfg(test)]
            Link::Direct(local) => local.legacy_store(),
        }
    }

    /// The directory the engine keeps its data in, `None` for one in memory.
    pub(crate) fn data_dir(&self) -> Option<PathBuf> {
        match &self.client.link {
            Link::Socket { dir, .. } => Some(dir.clone()),
            #[cfg(test)]
            Link::Direct(local) => local.data_dir(),
        }
    }

    /// The workspace for a window's first frame: as the last launch kept it, or as the engine
    /// holds it now. Only a first launch, with nothing kept, waits for the engine. The shell's
    /// mirror follows it from there.
    pub(crate) fn workspace_now(&self) -> Result<WorkspaceState, String> {
        match &self.client.link {
            Link::Socket { mirror, .. } => mirror
                .now(|seen| seen.workspace.clone())
                .ok_or_else(|| "The engine has not answered yet".to_owned()),
            #[cfg(test)]
            Link::Direct(local) => local.workspace.snapshot().map(|(_, state)| state),
        }
    }

    /// The engine settings for a window's first frame, as [`Engine::workspace_now`]; the defaults
    /// when the engine has not answered. The shell's mirror follows them from there.
    pub(crate) fn settings_now(&self) -> EngineSettings {
        match &self.client.link {
            Link::Socket { mirror, .. } => {
                mirror.now(|seen| seen.settings.clone()).unwrap_or_default()
            }
            #[cfg(test)]
            Link::Direct(local) => local.settings.current(),
        }
    }

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
        let (client, target) = (self.client.clone(), self.target.clone());
        async move { client.call::<C>(target.as_ref(), op, body).await }
    }

    /// Watches `W` from the params `params` makes for a cursor: a snapshot, then every batch of
    /// events, resuming with its cursor whenever the engine drops it behind or the connection
    /// drops. Ends when the returned [`Watching`] is dropped, or when the engine stops serving
    /// the watch.
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
        let task = self.client.executor.spawn(resume::<W>(
            self.client.clone(),
            self.target.clone(),
            params,
            updates,
        ));
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
        let (engine, target) = (self.client.clone(), self.target.clone());
        self.client
            .executor
            .spawn(async move {
                let _ = engine
                    .call::<onicode_protocol::files::StopSearch>(
                        target.as_ref(),
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
        let (engine, target) = (self.client.clone(), self.target.clone());
        self.client
            .executor
            .spawn(async move {
                let target = target.as_ref();
                if let Ok(session) = engine.session_id(target, row).await {
                    let _ = engine.call::<C>(target, op, body(session)).await;
                }
            })
            .detach();
    }
}

/// How often a session event watcher checks whether its receiver is still there.
const LISTENER_CHECK: Duration = Duration::from_secs(1);

/// The events of the session stored at `row` from now on, without its rows: a watch with no
/// turns in its snapshot, resumed with its cursor whenever the engine drops it behind or the
/// connection drops. Ends when the receiver goes or the engine stops serving the session.
pub(crate) fn session_events(
    engine: &Engine,
    row: i64,
    _: &mut App,
) -> async_channel::Receiver<SessionEvent> {
    let (target, engine) = (engine.target.clone(), engine.client.clone());
    let (events, received) = async_channel::unbounded();
    let executor = engine.executor.clone();
    executor
        .clone()
        .spawn(async move {
            let target = target.as_ref();
            let Ok(session) = engine.session_id(target, row).await else {
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
                let mut items = match engine.watch::<WatchSession>(target, watch).await {
                    Ok(items) => items,
                    Err(_) if engine.reconnects() && !events.is_closed() => {
                        executor.timer(RETRY).await;
                        continue;
                    }
                    Err(_) => return,
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

/// What a session's turns watcher reports ([`session_turns`]).
#[derive(Debug)]
pub(crate) enum SessionTurn {
    /// A turn is running: its prompt's id when it carries one, so the view that sent it knows it.
    Started { message: Option<MessageId> },
    /// What changed while no turn ran: the rows from a transcript index on, and the usage limit
    /// when it moved (`Some(None)`: cleared).
    Between {
        rows: Option<(usize, Vec<Row>)>,
        usage_limit: Option<Option<Limit>>,
    },
}

/// The turns the engine runs in the session stored at `row`, and what changes between them:
/// a one-turn watch resumed with its cursor like [`session_events`]. Ends when the receiver goes
/// or the engine stops serving the session.
pub(crate) fn session_turns(
    engine: &Engine,
    row: i64,
    _: &mut App,
) -> async_channel::Receiver<SessionTurn> {
    let (target, engine) = (engine.target.clone(), engine.client.clone());
    let (turns, received) = async_channel::unbounded();
    let executor = engine.executor.clone();
    executor
        .clone()
        .spawn(async move {
            let target = target.as_ref();
            let Ok(session) = engine.session_id(target, row).await else {
                return;
            };
            let mut hearing = Hearing::default();
            // The watch a `Resync` ended, held until the next one has joined.
            let mut ended = None;
            loop {
                let watch = WatchSession {
                    session,
                    since: hearing.cursor,
                    window_turns: 1,
                };
                let mut items = match engine.watch::<WatchSession>(target, watch).await {
                    Ok(items) => items,
                    Err(_) if engine.reconnects() && !turns.is_closed() => {
                        executor.timer(RETRY).await;
                        continue;
                    }
                    Err(_) => return,
                };
                drop(ended.take());
                let mut resync = false;
                loop {
                    let item = match select(items.next(), executor.timer(LISTENER_CHECK)).await {
                        Either::Left((item, _)) => item,
                        Either::Right(((), _)) => {
                            if turns.is_closed() {
                                return;
                            }
                            continue;
                        }
                    };
                    let heard = match item {
                        Some(Item::Snapshot { at, state }) => hearing.snapshot(at, state),
                        Some(Item::Events { last, events, .. }) => hearing.events(last, &events),
                        Some(Item::Resync) => {
                            resync = true;
                            break;
                        }
                        None => break,
                    };
                    for turn in heard {
                        if turns.send(turn).await.is_err() {
                            return;
                        }
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

/// A turns watcher's mirror of the session.
#[derive(Default)]
struct Hearing {
    state: Option<SessionState>,
    cursor: Option<Cursor>,
    /// Where the next turn's prompt can start: the rows' end when the session last went idle,
    /// lowered by rows rewritten since.
    floor: usize,
}

impl Hearing {
    fn snapshot(&mut self, at: Cursor, state: SessionState) -> Vec<SessionTurn> {
        self.cursor = Some(at);
        let heard = if running(&state) {
            (!self.state.as_ref().is_some_and(running)).then(|| SessionTurn::Started {
                message: prompt_from(&state, state.first_row),
            })
        } else {
            self.floor = end(&state);
            match &self.state {
                // First sight: the live rows may hold what the store did not yet.
                None => Some(SessionTurn::Between {
                    rows: Some((state.first_row, rows_from(&state, state.first_row))),
                    usage_limit: None,
                }),
                // A gap the watch could not replay: what changed meanwhile.
                Some(before) => {
                    let rows =
                        first_change(before, &state).map(|start| (start, rows_from(&state, start)));
                    let usage_limit = (before.usage_limit != state.usage_limit)
                        .then(|| state.usage_limit.clone());
                    (rows.is_some() || usage_limit.is_some())
                        .then_some(SessionTurn::Between { rows, usage_limit })
                }
            }
        };
        self.state = Some(state);
        heard.into_iter().collect()
    }

    fn events(&mut self, last: u64, events: &[SessionEvent]) -> Vec<SessionTurn> {
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.seq = last;
        }
        let Some(state) = self.state.as_mut() else {
            return Vec::new();
        };
        let mut heard = Vec::new();
        let mut dirty: Option<usize> = None;
        let mut usage_limit: Option<Option<Limit>> = None;
        let mut started = false;
        for event in events {
            let was = running(state);
            if !was {
                match event {
                    SessionEvent::Rows { start, .. } | SessionEvent::Append { row: start, .. } => {
                        dirty = Some(dirty.map_or(*start, |at| at.min(*start)));
                        self.floor = self.floor.min(*start);
                    }
                    SessionEvent::UsageLimit(limit) => usage_limit = Some(limit.clone()),
                    _ => {}
                }
            }
            apply(state, event);
            match (was, running(state)) {
                (false, true) => {
                    if let Some(limit) = usage_limit.take() {
                        heard.push(SessionTurn::Between {
                            rows: None,
                            usage_limit: Some(limit),
                        });
                    }
                    // The rows the turn rewrites reach the view through the turn.
                    dirty = None;
                    started = true;
                }
                (true, false) => {
                    if std::mem::take(&mut started) {
                        heard.push(SessionTurn::Started {
                            message: prompt_from(state, self.floor),
                        });
                    }
                    self.floor = end(state);
                }
                _ => {}
            }
        }
        if started {
            heard.push(SessionTurn::Started {
                message: prompt_from(state, self.floor),
            });
        }
        if dirty.is_some() || usage_limit.is_some() {
            let rows = dirty.map(|start| {
                let start = start.max(state.first_row);
                (start, rows_from(state, start))
            });
            heard.push(SessionTurn::Between { rows, usage_limit });
        }
        heard
    }
}

fn running(state: &SessionState) -> bool {
    state.turn.as_ref().is_some_and(|turn| !turn.finished)
}

fn end(state: &SessionState) -> usize {
    state.first_row + state.rows.len()
}

/// The id on the last prompt from transcript index `floor` on.
fn prompt_from(state: &SessionState, floor: usize) -> Option<MessageId> {
    let skip = floor.saturating_sub(state.first_row);
    state
        .rows
        .iter()
        .skip(skip)
        .rev()
        .find(|row| row.first && row.is_prompt())
        .and_then(|row| row.id)
        .map(MessageId)
}

/// The rows from transcript index `start` on, without the live prompt ids.
fn rows_from(state: &SessionState, start: usize) -> Vec<Row> {
    let skip = start.saturating_sub(state.first_row).min(state.rows.len());
    state.rows[skip..]
        .iter()
        .cloned()
        .map(|mut row| {
            row.id = None;
            row
        })
        .collect()
}

fn row_at(state: &SessionState, at: usize) -> Option<&Row> {
    at.checked_sub(state.first_row)
        .and_then(|index| state.rows.get(index))
}

/// The first transcript index whose row differs between `before` and `now`.
fn first_change(before: &SessionState, now: &SessionState) -> Option<usize> {
    let start = before.first_row.max(now.first_row);
    (start..end(before).max(end(now))).find(|&at| row_at(before, at) != row_at(now, at))
}

/// What a workspace watcher receives: the whole workspace, or what changed since.
pub(crate) type WorkspaceUpdate = WatchUpdate<WorkspaceState, WorkspaceEvent>;

/// Feeds `updates` from the watch `params` names for a cursor: a snapshot, then every non-empty
/// batch of events, resubscribing with the last cursor after `Resync` (the engine dropped the
/// watcher behind, or the connection dropped; an engine that restarted answers with a fresh
/// snapshot). Ends when the receiver goes or a stream ends without `Resync` (the engine stopped
/// serving it).
async fn resume<W: Watch>(
    engine: Arc<EngineClient>,
    target: Option<EngineId>,
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
        let mut items = match engine.watch::<W>(target.as_ref(), params(cursor)).await {
            Ok(items) => items,
            // The engine is starting, or restarting: ask again while someone listens.
            Err(_) if engine.reconnects() && !updates.is_closed() => {
                engine.executor.timer(RETRY).await;
                continue;
            }
            Err(_) => return,
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

/// Watches `engine`'s workspace: a snapshot, then every batch of events, resuming with its
/// cursor whenever the engine drops it behind. Ends when the receiver goes.
pub(crate) fn watch_workspace(
    engine: &Engine,
    _: &mut App,
) -> async_channel::Receiver<WorkspaceUpdate> {
    let (target, engine) = (engine.target.clone(), engine.client.clone());
    let (updates, received) = async_channel::unbounded();
    engine
        .executor
        .clone()
        .spawn(resume::<WatchWorkspace>(
            engine,
            target,
            |since| WatchWorkspace { since },
            updates,
        ))
        .detach();
    received
}

/// Creates a session in `engine` at once, for a test to seed the workspace with.
#[cfg(test)]
pub(crate) fn create_for_test(engine: &Engine, provider: &str, cwd: &str, _: &mut App) -> Session {
    use onicode_protocol::workspace::{CreateSession, NewSession};
    let created = futures::executor::block_on(engine.client.direct().workspace.create_session(
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

/// One beat of `engine`'s schedulers at `now`, in a test's virtual time.
#[cfg(test)]
pub(crate) fn tick(engine: &Engine, now: u64, _: &mut App) -> Result<(), String> {
    engine.client.direct().tick(now)
}

/// Every call `engine` was asked, by name with its routed params.
#[cfg(test)]
pub(crate) fn calls_for_test(
    engine: &Engine,
    _: &mut App,
) -> Vec<(&'static str, serde_json::Value)> {
    engine
        .client
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use onicode_protocol::follow_up::Message;

    use super::*;

    fn turn(row: i64) -> Arc<Turn> {
        Arc::new(Turn {
            target: None,
            row,
            viewer: Mutex::new(Viewer::default()),
            nudge: Notify::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            finished: AtomicBool::new(false),
            message: None,
        })
    }

    #[gpui::test]
    fn a_steer_that_fails_before_it_leaves_shows_at_once(cx: &mut gpui::TestAppContext) {
        let engine = cx.update(Engine::memory).client;
        let session = SessionId::new();
        let turn = turn(1);
        let (updates, received) = async_channel::bounded(1);
        // The turn started, and its session sends nothing more.
        let (_refusal, refusals) = async_channel::bounded::<String>(1);
        let relayed = {
            let (engine, turn) = (engine.clone(), turn.clone());
            cx.executor().spawn(async move {
                let items: Items = futures::stream::pending().boxed();
                relay(&engine, session, items, Some(refusals), &turn, &updates).await
            })
        };
        cx.run_until_parked();
        assert!(received.try_recv().is_err(), "nothing to hand on yet");
        let controls = EngineControls { engine, turn };
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

    /// What names another engine goes to the app's own engine named for it: one it cannot reach
    /// answers that it is offline, and is not reachable, while the app's own engine, by no name
    /// or by its own, always is.
    #[gpui::test]
    fn another_engine_is_reached_through_the_apps_own(cx: &mut gpui::TestAppContext) {
        let engine = cx.update(Engine::memory);
        let own = engine.client.direct().identity.id();
        assert!(engine.client.reachable(None));
        assert!(engine.client.reachable(Some(&own)));
        let away = engine.on(EngineId("elsewhere".into()));
        assert!(!away.client.reachable(away.target.as_ref()));
        let asked = futures::executor::block_on(away.call::<onicode_protocol::engine::EngineInfo>(
            None,
            onicode_protocol::engine::EngineInfo {},
        ));
        let error = asked.expect_err("nothing reaches that engine");
        assert!(error.contains("offline"), "{error}");
        let calls = cx.update(|cx| calls_for_test(&engine, cx));
        let (name, params) = calls.last().expect("the call was made");
        assert_eq!(*name, "EngineInfo");
        assert_eq!(params["engine"], "elsewhere");
    }

    fn snapshot(epoch: u64, turn: Option<session::TurnState>) -> Item<SessionState, SessionEvent> {
        Item::Snapshot {
            at: Cursor { epoch, seq: 0 },
            state: SessionState {
                turn,
                ..SessionState::default()
            },
        }
    }

    /// An engine that restarted under a started turn answers with a snapshot of another epoch
    /// and no turn: the turn finishes, saying why. The same snapshot before the turn started, or
    /// from the same engine run, finishes nothing.
    #[test]
    fn a_turn_ends_when_its_engine_restarted() {
        let running = Some(session::TurnState {
            provider_turn_id: None,
            ready: true,
            finished: false,
        });
        let viewer = Mutex::new(Viewer::default());
        assert!(!take_in(&viewer, snapshot(7, running.clone()), None, false));
        assert!(
            !take_in(&viewer, snapshot(7, None), Some(7), true),
            "the same run"
        );
        assert!(
            !take_in(&viewer, snapshot(9, None), Some(7), false),
            "not started"
        );
        assert!(take_in(&viewer, snapshot(9, None), Some(7), true));
        let update = take(&viewer).expect("the end is handed on");
        assert!(update.finished);
        assert!(update.status.contains("restarted"), "{}", update.status);
        let restarted = Mutex::new(Viewer::default());
        assert!(!take_in(&restarted, snapshot(7, None), None, false));
        assert!(
            !take_in(&restarted, snapshot(9, running), Some(7), true),
            "a turn the new engine runs goes on"
        );
    }

    /// The mirror keeps what it saw across launches, and one with nothing kept reads what
    /// arrives.
    #[test]
    fn the_mirror_keeps_the_workspace_across_launches() {
        let dir = std::env::temp_dir().join(format!("onicode-mirror-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(VIEWER_CACHE);
        let mirror = Mirror::load(file.clone());
        mirror.update(|seen| {
            seen.settings = Some(EngineSettings {
                idle_agents: 3,
                ..EngineSettings::default()
            });
        });
        mirror.save();
        let next = Mirror::load(file);
        let settings = next.now(|seen| seen.settings.clone()).unwrap();
        assert_eq!(settings.idle_agents, 3);
        let empty = Mirror::load(dir.join("missing.json"));
        empty.update(|seen| seen.workspace = Some(WorkspaceState::default()));
        assert!(empty.now(|seen| seen.workspace.clone()).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
