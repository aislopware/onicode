//! The app's link to its engine (engine.md, migration step 3). A local turn runs in the engine's
//! `SessionHub`: the conversation stages the turn's in-process context, watches the session and
//! sends the prompt over `onicode-rpc`'s in-memory transport, and a `Viewer` folds what arrives
//! back into one `turn::Update` per beat. SSH and legacy remote turns keep their own runners.
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, mpsc};

use futures::future::{BoxFuture, Either, select};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use gpui::{App, BackgroundExecutor, Global};
use onicode_core::agent::{Control, Decision, Settings};
use onicode_core::store::Session;
use onicode_engine::session::{
    Expect, SessionHub, SessionService, Spawn, Staged, StagedTurn, Viewer,
};
use onicode_engine::turn::{Database, Update};
use onicode_protocol::session::{
    self, Answer, Decide, KeepQuestionOpen, SessionEvent, SessionState, Steer, Verdict,
    WatchSession,
};
use onicode_protocol::{Call, Item, MessageId, OpId, Outcome, Routed, SessionId, Watch};
use onicode_rpc::{RpcClient, RpcReply, RpcService};

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
    Direct(Arc<SessionService>),
}

pub(crate) struct EngineClient {
    store: Database,
    hub: Arc<SessionHub>,
    link: Link,
    executor: BackgroundExecutor,
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
        let service = Arc::new(SessionService::new(hub.clone()));
        let runtime = if cfg!(test) { None } else { RUNTIME.as_ref() };
        let link = match runtime {
            Some(runtime) => {
                let _entered = runtime.enter();
                Link::Rpc(Arc::new(onicode_rpc::memory_client(service)))
            }
            None => Link::Direct(service),
        };
        let engine = Arc::new(Self {
            store: store.clone(),
            hub,
            link,
            executor,
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

    async fn watch(
        &self,
        body: WatchSession,
    ) -> Result<BoxStream<'static, Item<SessionState, SessionEvent>>, String> {
        let params = serde_json::to_value(Routed {
            engine: None,
            op: None,
            body,
        })
        .map_err(|error| error.to_string())?;
        let values = match &self.link {
            Link::Rpc(client) => {
                let subscription = client
                    .subscribe_scoped(WatchSession::NAME, params)
                    .await
                    .map_err(|error| error.to_string())?;
                futures::stream::unfold(subscription, |mut subscription| async move {
                    let value = subscription.recv().await?;
                    Some((value, subscription))
                })
                .boxed()
            }
            Link::Direct(service) => match service
                .handle(WatchSession::NAME, params)
                .await
                .map_err(|error| error.to_string())?
            {
                RpcReply::Stream(values) => values,
                RpcReply::Value(_) => {
                    return Err(format!("{} answered with a value", WatchSession::NAME));
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
}

impl EngineControls {
    fn send(&self, control: Control) {
        let session = self.session;
        let op = OpId::new();
        let engine = self.engine.clone();
        let id = control.id;
        let call: BoxFuture<'static, Result<Outcome, String>> = match control.decision {
            Decision::Steer(message) => {
                // The message travels whole, staged under the steer's id; its receipt names it
                // delivered or failed.
                let staged = MessageId::new();
                lock(&self.viewer).expect(
                    op,
                    Expect::Steer {
                        id: message.id.clone(),
                    },
                );
                let body = Steer {
                    session,
                    message: staged,
                    text: message.text.clone(),
                    attachments: message.attachments.clone(),
                };
                engine.hub.stage(staged, Staged::Steer(message));
                async move { engine.call::<Steer>(Some(op), body).await }.boxed()
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
    engine
        .executor
        .spawn(async move {
            if let Err(status) = forward(forwarder, session, op, send, &held, &updates).await
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
    })
}

enum Next {
    Item(Box<Option<Item<SessionState, SessionEvent>>>),
    Refused(String),
    Sent,
}

/// Watches the session, sends the turn and forwards what arrives until the turn finishes or the
/// conversation stops listening.
async fn forward(
    engine: Arc<EngineClient>,
    session: SessionId,
    op: OpId,
    send: session::Send,
    viewer: &Mutex<Viewer>,
    updates: &async_channel::Sender<Update>,
) -> Result<(), String> {
    let watch = |since| WatchSession {
        session,
        since,
        window_turns: 1,
    };
    // Watch first: the turn's events follow the snapshot.
    let mut items = engine.watch(watch(None)).await?;
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
    let mut refused = Some(refusals);
    loop {
        let next = match &refused {
            Some(receiver) => match select(items.next(), pin!(receiver.recv())).await {
                Either::Left((item, _)) => Next::Item(Box::new(item)),
                Either::Right((Ok(reason), _)) => Next::Refused(reason),
                Either::Right((Err(_), _)) => Next::Sent,
            },
            None => Next::Item(Box::new(items.next().await)),
        };
        let mut finished = match next {
            Next::Item(item) => match *item {
                Some(Item::Resync) => {
                    items = engine.watch(watch(cursor(viewer))).await?;
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
        };
        // Whatever else is ready joins this beat's update.
        while !finished && let Some(Some(item)) = items.next().now_or_never() {
            if matches!(item, Item::Resync) {
                items = engine.watch(watch(cursor(viewer))).await?;
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
