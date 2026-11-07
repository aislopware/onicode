//! The hub: every live session's state, ring, watchers and running turn, and the commands that
//! drive it. A session goes live when it is watched or sent to, and leaves once nothing watches
//! it and no turn runs; it comes back under a new epoch, so no old cursor matches.
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use onicode_core::agent::{AccessMode, Control, Decision, Handoff, Settings};
use onicode_core::follow_up::{self, Message, StateExt as _};
use onicode_core::provider::{Event, Kind};
use onicode_core::store::{EditSnapshot, Session, Store};
use onicode_core::tool::{ToolCall, ToolState};
use onicode_core::transcript::{Row, Transcript};
use onicode_protocol::session::{
    self, Answer, Btw, BtwAnswer, BtwThread, BtwThreads, Compact, Compacted, Configure, Decide,
    EditLastTurn, EditedTurn, Fork, KeepQuestionOpen, QueueChange, QueueEdit, ReadBtw, ReadSession,
    ReadTranscript, RecallLastTurn, RecalledTurn, ResetThread, Rewind, SaveBtw, SaveQueue, SendNow,
    SessionEvent, SessionRecord, SessionState, Steer, SteerNow, Stop, StopBtw, SwitchProvider,
    TranscriptPage, TurnContext, Verdict, WatchSession,
};
use onicode_protocol::workspace::{CreateSession, NewSession};
use onicode_protocol::{Item, MessageId, OpId, Outcome, Rejection, SessionId};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

pub(crate) mod resume;

use super::asides::{Aside, Asides, ProviderAsides, btw_prompt};
use super::queue::SessionFeed;
use super::split;
use crate::accounts::AccountsHub;
pub(crate) use crate::feed::Limits;
use crate::feed::{self, Topic};
use crate::ledger::Ledger;
use crate::settings::SettingsHub;
use crate::turn::{Database, Update, locked};
use crate::workspace::WorkspaceHub;

/// How long the ledger keeps a processed op (engine.md "Ids").
const OP_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The ledger is pruned after every this many recorded ops, and when a hub starts.
const PRUNE_EVERY: u64 = 256;
/// A by-the-way answer stops growing past this many bytes.
const BTW_BYTES: usize = 256 * 1024;

/// Where the hub runs a turn and the task that drains its updates.
pub trait Spawn: Send + Sync + 'static {
    fn spawn(&self, work: BoxFuture<'static, ()>);
}

/// A thread for each piece of work: the hub's spawner outside the app.
pub struct ThreadSpawn;

impl Spawn for ThreadSpawn {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        // A thread that cannot start drops its work; the turn's channel then closes and the hub
        // finishes the turn with an error.
        let _ = std::thread::Builder::new()
            .name("onicode-session".into())
            .spawn(move || futures::executor::block_on(work));
    }
}

/// One turn for a [`Runner`]: what `turn::run_turn` takes.
pub struct TurnJob {
    pub summary: Session,
    pub prompt: String,
    pub settings: Settings,
    pub controls: mpsc::Receiver<Control>,
    pub store: Database,
    pub cancel: Arc<AtomicBool>,
    /// Unbounded: a turn never waits on a viewer.
    pub updates: async_channel::Sender<Update>,
}

/// Runs a turn to its end on the calling thread, publishing into `job.updates`.
pub type Runner = Arc<dyn Fn(TurnJob) + Send + Sync>;

/// A mutating call waiting on its receipt.
struct Pending {
    op: Option<OpId>,
    message: MessageId,
    resolve: watch::Sender<Option<Outcome>>,
    /// A `SteerNow`'s message: back to the queue's head if the provider refuses it.
    queued: Option<Box<Message>>,
}

/// A `SendNow` waiting for the running turn to end.
struct Next {
    op: Option<OpId>,
    id: MessageId,
    message: Box<Message>,
    resolve: watch::Sender<Option<Outcome>>,
}

struct Turn {
    /// Tells a closed update channel of this turn from one of a later turn.
    id: u64,
    controls: Option<mpsc::Sender<Control>>,
    cancel: Arc<AtomicBool>,
    /// The queued message the turn launched (`Settings::mono_message_id`).
    queued: Option<String>,
    launch: Option<Pending>,
    /// The launch was applied: the turn ran and has a receipt to report.
    started: bool,
    /// Steers handed to the provider, by queued message id, until it accepts or fails them.
    steers: HashMap<String, Pending>,
    /// The message `SendNow` launches once this turn ends.
    next: Option<Next>,
    /// The engine's usage-limit resume (`resume.rs`).
    resume: bool,
}

struct Live {
    id: i64,
    /// The session, its numbering, ring and watchers.
    topic: Topic<SessionFeed>,
    /// Prompt rows' ids by transcript index.
    prompts: BTreeMap<usize, uuid::Uuid>,
    turn: Option<Turn>,
    /// Loaded from the store and not run since: a send needs no reload.
    fresh: bool,
}

#[derive(Default)]
struct Ops {
    inflight: HashMap<OpId, watch::Receiver<Option<Outcome>>>,
    recorded: u64,
}

struct Inner {
    sessions: HashMap<SessionId, Live>,
    ops: Ops,
    next_epoch: u64,
    next_turn: u64,
    /// Last turns recalled for an edit (`RecallLastTurn`), until `EditLastTurn` takes them.
    recalled: HashMap<SessionId, EditSnapshot>,
    /// Sessions whose last turn is being rewound: no turn starts in them meanwhile.
    rewinding: HashSet<SessionId>,
    /// Running by-the-way answers, by session and thread.
    btw: HashMap<(SessionId, String), Arc<AtomicBool>>,
    /// Running compactions, by session: `Stop` stops one, and no turn starts meanwhile.
    compacting: HashMap<SessionId, Arc<AtomicBool>>,
    /// The usage-limit resumes in flight, as the store keeps them; `None` until this run's first
    /// beat recovered the ones an earlier run left.
    resumes: Option<resume::Resumes>,
}

/// A session's by-the-way threads as the store keeps them (`btw:{row}`): with the desktop app's
/// original of an imported thread, which viewers never see.
#[derive(Default, Serialize, Deserialize)]
struct SavedBtw {
    revision: u64,
    threads: Vec<SavedThread>,
}

#[derive(Serialize, Deserialize)]
struct SavedThread {
    #[serde(flatten)]
    thread: BtwThread,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desktop: Option<serde_json::Value>,
}

enum Done {
    Now(Outcome),
    Later(watch::Receiver<Option<Outcome>>),
}

pub struct SessionHub {
    store: Database,
    spawn: Arc<dyn Spawn>,
    runner: Runner,
    asides: Arc<dyn Asides>,
    limits: Limits,
    inner: Mutex<Inner>,
    /// Told when a turn starts and ends, for the sessions' marks.
    workspace: OnceLock<Arc<WorkspaceHub>>,
    /// Read as a turn starts: whether Claude Code runs the person's hooks.
    settings: OnceLock<Arc<SettingsHub>>,
    /// Told when a turn ends, for the usage its stream reported.
    accounts: OnceLock<Arc<AccountsHub>>,
    /// The ops of the calls whose reply is not a receipt (`Compact`, `Fork`).
    ledger: Ledger,
}

fn run(job: TurnJob) {
    crate::turn::run_turn(
        job.summary,
        job.prompt,
        job.settings,
        job.controls,
        job.store,
        job.cancel,
        job.updates,
    );
}

impl SessionHub {
    /// A hub over `store` that runs turns with `turn::run_turn` on `spawn`.
    pub fn new(store: Database, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::with_runner(store, spawn, Arc::new(run))
    }

    /// A hub whose turns run on `runner`: tests hand it a scripted provider.
    pub fn with_runner(store: Database, spawn: Arc<dyn Spawn>, runner: Runner) -> Arc<Self> {
        Self::build(
            store,
            spawn,
            runner,
            Arc::new(ProviderAsides),
            Limits::default(),
        )
    }

    pub(crate) fn build(
        store: Database,
        spawn: Arc<dyn Spawn>,
        runner: Runner,
        asides: Arc<dyn Asides>,
        limits: Limits,
    ) -> Arc<Self> {
        let now = onicode_core::time::now_ms();
        if let Ok(store) = locked(&store) {
            let _ = store.prune_ops(now.saturating_sub(OP_RETENTION_MS));
        }
        Arc::new(Self {
            spawn,
            runner,
            asides,
            limits,
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                ops: Ops::default(),
                // This run's epochs: a cursor from an earlier run never matches.
                next_epoch: now.wrapping_shl(16),
                next_turn: 0,
                recalled: HashMap::new(),
                rewinding: HashSet::new(),
                btw: HashMap::new(),
                compacting: HashMap::new(),
                resumes: None,
            }),
            workspace: OnceLock::new(),
            settings: OnceLock::new(),
            accounts: OnceLock::new(),
            ledger: Ledger::new(store.clone()),
            store,
        })
    }

    /// Tells `workspace` when each turn starts and ends. Lock order: a hub's state, then the
    /// workspace's, then the store.
    pub fn attach(&self, workspace: Arc<WorkspaceHub>) {
        let _ = self.workspace.set(workspace);
    }

    /// Reads the engine settings as each turn starts. Lock order: the hub's state, then the
    /// settings'.
    pub fn attach_settings(&self, settings: Arc<SettingsHub>) {
        let _ = self.settings.set(settings);
    }

    /// Tells `accounts` when each turn ends.
    pub fn attach_accounts(&self, accounts: Arc<AccountsHub>) {
        let _ = self.accounts.set(accounts);
    }

    /// Settings as the engine runs them: the account's home, and Claude Code's hooks as the
    /// engine settings say.
    fn engine_settings(&self, store: &Store, settings: &mut Settings) {
        if settings.account.id != onicode_core::accounts::DEFAULT && settings.account.root.is_none()
        {
            settings.account.root = store.data_dir().map(Path::to_path_buf);
        }
        if let Some(hub) = self.settings.get() {
            settings.disable_claude_hooks = !hub.current().claude_hooks;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many sessions have a turn running now: the engine's live agents, as the machines list
    /// shows them.
    pub fn live_agents(&self) -> usize {
        self.lock()
            .sessions
            .values()
            .filter(|live| live.turn.is_some())
            .count()
    }

    /// The protocol id of the store's session `id`.
    pub fn session_id(&self, id: i64) -> Result<SessionId, String> {
        locked(&self.store)?.session_uid(id).map(SessionId)
    }

    /// The session's live state, loading it from the store when it is not live.
    fn live<'a>(
        &self,
        sessions: &'a mut HashMap<SessionId, Live>,
        next_epoch: &mut u64,
        session: SessionId,
    ) -> Result<&'a mut Live, String> {
        match sessions.entry(session) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let store = locked(&self.store)?;
                let id = store
                    .session_by_uid(session.0)?
                    .ok_or("Session no longer exists")?;
                let state = load_state(&store, id)?;
                drop(store);
                *next_epoch += 1;
                Ok(entry.insert(Live {
                    id,
                    topic: Topic::new(*next_epoch, state, self.limits),
                    prompts: BTreeMap::new(),
                    turn: None,
                    fresh: true,
                }))
            }
        }
    }

    /// Catches a live session up with what was stored since it was loaded.
    fn refresh(&self, live: &mut Live) -> Result<(), String> {
        let fresh = load_state(&*locked(&self.store)?, live.id)?;
        let mut events = Vec::new();
        let mut rows = fresh.rows;
        split::stamp(&mut rows, 0, &live.prompts);
        split::rows(live.topic.state(), 0, rows, &mut events);
        if fresh.title != live.topic.state().title {
            events.push(SessionEvent::Title(fresh.title));
        }
        if fresh.provider_id != live.topic.state().provider_id {
            events.push(SessionEvent::ProviderId(fresh.provider_id));
        }
        if serde_json::to_value(&fresh.settings).ok()
            != serde_json::to_value(&live.topic.state().settings).ok()
        {
            events.push(SessionEvent::Settings(Box::new(fresh.settings)));
        }
        if fresh.queue_rev != live.topic.state().queue_rev {
            events.push(SessionEvent::Queue {
                rev: fresh.queue_rev,
                state: fresh.queue,
            });
        }
        if fresh.context != live.topic.state().context
            && let Some(usage) = fresh.context
        {
            events.push(SessionEvent::Context(usage));
        }
        publish(live, events);
        Ok(())
    }

    /// The receipt the ledger holds for `op`.
    fn receipt(&self, op: OpId) -> Result<Option<Outcome>, String> {
        Ok(locked(&self.store)?
            .op_receipt(&op.to_string())?
            .and_then(|receipt| serde_json::from_str(&receipt).ok()))
    }

    fn record(&self, ops: &mut Ops, session: SessionId, op: OpId, outcome: &Outcome) {
        let Ok(receipt) = serde_json::to_string(outcome) else {
            return;
        };
        let now = onicode_core::time::now_ms();
        if let Ok(store) = locked(&self.store) {
            let _ = store.record_op(&op.to_string(), session.0, &receipt, now);
            ops.recorded += 1;
            if ops.recorded.is_multiple_of(PRUNE_EVERY) {
                let _ = store.prune_ops(now.saturating_sub(OP_RETENTION_MS));
            }
        }
    }

    /// Settles a waiting call: answers it, records it and returns its receipt event.
    fn settle(
        &self,
        ops: &mut Ops,
        session: SessionId,
        pending: Pending,
        outcome: Outcome,
    ) -> Option<SessionEvent> {
        pending.resolve.send_replace(Some(outcome.clone()));
        let op = pending.op?;
        ops.inflight.remove(&op);
        self.record(ops, session, op, &outcome);
        Some(SessionEvent::Receipt { op, outcome })
    }

    /// Settles a call at once: records it and publishes `events` and its receipt.
    fn settled(
        &self,
        live: Option<&mut Live>,
        ops: &mut Ops,
        session: SessionId,
        op: Option<OpId>,
        outcome: Outcome,
        mut events: Vec<SessionEvent>,
    ) -> Outcome {
        if let Some(op) = op {
            self.record(ops, session, op, &outcome);
            events.push(SessionEvent::Receipt {
                op,
                outcome: outcome.clone(),
            });
        }
        if let Some(live) = live {
            publish(live, events);
        }
        outcome
    }

    /// Runs a mutating call once per op: a repeat waits for or returns the first one's receipt.
    async fn once(
        &self,
        op: Option<OpId>,
        run: impl FnOnce(&mut Inner) -> Result<Done, String>,
    ) -> Result<Outcome, String> {
        let done = {
            let mut inner = self.lock();
            let repeated = match op {
                Some(op) => match inner.ops.inflight.get(&op) {
                    Some(waiting) => Some(Done::Later(waiting.clone())),
                    None => self.receipt(op)?.map(Done::Now),
                },
                None => None,
            };
            match repeated {
                Some(done) => done,
                None => run(&mut inner)?,
            }
        };
        match done {
            Done::Now(outcome) => Ok(outcome),
            Done::Later(mut waiting) => {
                let outcome = waiting
                    .wait_for(Option::is_some)
                    .await
                    .map(|outcome| outcome.clone())
                    .map_err(|_| "The session closed before this call settled".to_owned())?;
                Ok(outcome.unwrap_or(Outcome::Applied))
            }
        }
    }

    /// `Send`: starts a turn. Its receipt arrives with the turn's first update, which holds the
    /// prompt under the call's `MessageId`.
    pub async fn send(
        self: &Arc<Self>,
        op: Option<OpId>,
        body: session::Send,
    ) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let (resolve, waiting) = watch::channel(None);
            self.launch(inner, op, body, &mut Some(resolve))?;
            Ok(Done::Later(waiting))
        })
        .await
    }

    /// Starts a turn whose receipt `resolve` answers. `resolve` is taken only once the turn
    /// starts: a launch that fails leaves it to the caller.
    fn launch(
        self: &Arc<Self>,
        inner: &mut Inner,
        op: Option<OpId>,
        mut body: session::Send,
        resolve: &mut Option<watch::Sender<Option<Outcome>>>,
    ) -> Result<(), String> {
        if inner.rewinding.contains(&body.session) {
            return Err("The last turn is being edited".into());
        }
        if inner.compacting.contains_key(&body.session) {
            return Err("Wait for the context compaction to finish".into());
        }
        let Inner {
            sessions,
            ops,
            next_epoch,
            next_turn,
            ..
        } = inner;
        let live = self.live(sessions, next_epoch, body.session)?;
        if live.turn.is_some() {
            return Err("A turn is already running in this session".into());
        }
        if !live.fresh {
            self.refresh(live)?;
        }
        live.fresh = false;
        let (summary, mut settings) = {
            let store = locked(&self.store)?;
            let (summary, mut settings) = match body.turn.take() {
                Some(turn) => {
                    let TurnContext {
                        summary,
                        mut settings,
                        prepared_prompt,
                        turn_model,
                        environment,
                        queued,
                        internal,
                    } = *turn;
                    if summary.id != live.id {
                        return Err("The turn names another session".into());
                    }
                    settings.prepared_prompt = prepared_prompt;
                    settings.turn_model = turn_model;
                    settings.environment = environment;
                    settings.mono_message_id = queued;
                    settings.internal_message = internal;
                    (summary, settings)
                }
                None => {
                    let summary = store
                        .sessions()?
                        .into_iter()
                        .find(|summary| summary.id == live.id)
                        .ok_or("Session no longer exists")?;
                    (summary, store.agent_settings(live.id)?.unwrap_or_default())
                }
            };
            self.engine_settings(&store, &mut settings);
            (summary, settings)
        };
        settings.attachments = std::mem::take(&mut body.attachments);
        let prompt = std::mem::take(&mut body.text);
        // The engine's own stop flag: `Stop` sets it, whichever viewer sends it.
        let cancel = Arc::new(AtomicBool::new(false));
        let ended = (
            Kind::from_id(&summary.provider),
            settings.account.id.clone(),
        );
        // The turn continues the conversation past any usage limit it was held at, armed or not.
        settings.usage_limit = None;
        if resolve.is_none() {
            return Err("This call already started a turn".into());
        }
        if let Some(lifted) = self.lift_limit(live.id)? {
            publish(live, vec![lifted]);
        }
        let resolve = resolve.take().ok_or("This call already started a turn")?;
        if let Some(op) = op {
            ops.inflight.insert(op, resolve.subscribe());
        }
        let (controls, control_rx) = mpsc::channel();
        let (updates, received) = async_channel::unbounded();
        *next_turn += 1;
        let turn_id = *next_turn;
        live.turn = Some(Turn {
            id: turn_id,
            controls: Some(controls),
            cancel: cancel.clone(),
            queued: settings.mono_message_id.clone(),
            launch: Some(Pending {
                op,
                message: body.message,
                resolve,
                queued: None,
            }),
            started: false,
            steers: HashMap::new(),
            next: None,
            resume: false,
        });
        if let Some(workspace) = self.workspace.get() {
            workspace.turn_started(live.id);
        }
        let job = TurnJob {
            summary,
            prompt,
            settings,
            controls: control_rx,
            store: self.store.clone(),
            cancel,
            updates,
        };
        let runner = self.runner.clone();
        self.spawn.spawn(Box::pin(async move { runner(job) }));
        let hub = self.clone();
        let session = body.session;
        self.spawn.spawn(Box::pin(async move {
            while let Ok(update) = received.recv().await {
                hub.apply_update(session, turn_id, update);
            }
            hub.turn_closed(session, turn_id);
            if let Some(accounts) = hub.accounts.get() {
                accounts.turn_ended(ended.0, &ended.1);
            }
        }));
        Ok(())
    }

    /// One update of turn `turn_id`: its events, the receipts it settles, and, when it is the
    /// last, the end of the turn.
    fn apply_update(self: &Arc<Self>, session: SessionId, turn_id: u64, mut update: Update) {
        let mut inner = self.lock();
        let Inner {
            sessions,
            ops,
            resumes,
            ..
        } = &mut *inner;
        let Some(live) = sessions.get_mut(&session) else {
            return;
        };
        let Some(turn) = live.turn.as_mut().filter(|turn| turn.id == turn_id) else {
            return;
        };
        let (resuming, cancelled) = (turn.resume, turn.cancel.load(Ordering::Relaxed));
        if resuming && resume::spoke(&update) {
            self.resume_spoke(resumes, live.id);
        }
        // A turn that ends at a usage limit leaves the engine holding it.
        let reported = update.usage_limit.is_some();
        let limited = (update.finished && !cancelled)
            .then(|| {
                update
                    .usage_limit
                    .clone()
                    .or_else(|| onicode_core::usage::Limit::from_error(&update.status))
            })
            .flatten();
        let delivered = std::mem::take(&mut update.follow_up_delivered);
        let failed = std::mem::take(&mut update.follow_up_errors);
        let finished = update.finished;
        // A queued message's turn names it once its prompt is saved; any other turn's first
        // update before the last holds its prompt.
        let launched = turn.launch.is_some()
            && match &turn.queued {
                Some(queued) => update.delivered.as_ref() == Some(queued),
                None => !finished,
            };
        // Name the prompts this update brings: the launch's, then each accepted steer's.
        let end = live.topic.state().first_row + live.topic.state().rows.len();
        let mut ids = Vec::new();
        if launched && let Some(launch) = &turn.launch {
            ids.push(launch.message);
        }
        ids.extend(
            delivered
                .iter()
                .filter_map(|id| turn.steers.get(id))
                .map(|steer| steer.message),
        );
        let fresh_prompts: Vec<usize> = update
            .rows
            .iter()
            .enumerate()
            .map(|(at, row)| (update.start + at, row))
            .filter(|(at, row)| *at >= end && row.first && row.is_prompt())
            .map(|(at, _)| at)
            .collect();
        for (at, id) in fresh_prompts.into_iter().zip(ids) {
            live.prompts.insert(at, id.0);
        }
        let mut receipts = Vec::new();
        if launched && let Some(launch) = turn.launch.take() {
            turn.started = true;
            receipts.extend(self.settle(ops, session, launch, Outcome::Applied));
        }
        for id in &delivered {
            if let Some(steer) = turn.steers.remove(id) {
                receipts.extend(self.settle(ops, session, steer, Outcome::Applied));
            }
        }
        // `SteerNow`'s messages the provider refused go back to the queue's head, paused on the
        // error; ones the turn outlived go back to be sent next.
        let mut requeued = Vec::new();
        for (id, error) in failed {
            if let Some(mut steer) = turn.steers.remove(&id) {
                if let Some(message) = steer.queued.take() {
                    requeued.push((message, Some(error.clone())));
                }
                let outcome = Outcome::Rejected(Rejection::Denied(error));
                receipts.extend(self.settle(ops, session, steer, outcome));
            }
        }
        let mut started = turn.started;
        if finished {
            if let Some(launch) = turn.launch.take() {
                let outcome = Outcome::Rejected(Rejection::Denied(update.status.clone()));
                receipts.extend(self.settle(ops, session, launch, outcome));
                started = false;
            }
            for (_, mut steer) in turn.steers.drain() {
                if let Some(message) = steer.queued.take() {
                    requeued.push((message, None));
                }
                let outcome = Outcome::Rejected(Rejection::Gone);
                receipts.extend(self.settle(ops, session, steer, outcome));
            }
        }
        let mut events = split::split(live.topic.state(), update, &live.prompts);
        // The `Status` stays last: a watcher sees the receipts of a turn before its end.
        let status = events.pop();
        for (message, error) in requeued {
            events.extend(self.requeue(live.id, *message, error));
        }
        events.extend(receipts);
        if finished && started {
            events.extend(self.turn_finished(live.id));
        }
        if finished {
            if resuming {
                let reason = match &status {
                    Some(SessionEvent::Status { status, .. }) => status.as_str(),
                    _ => "",
                };
                events.extend(self.resume_ended(resumes, live.id, started, cancelled, reason));
            }
            if let Some(limit) = limited {
                events.extend(self.hold_limit(live.id, limit, reported));
            }
        }
        events.extend(status);
        publish(live, events);
        if finished {
            if let Some(workspace) = self.workspace.get() {
                workspace.turn_finished(live.id);
            }
            let row = live.id;
            let next = live.turn.take().and_then(|turn| turn.next);
            let end = live.topic.state().first_row + live.topic.state().rows.len();
            live.prompts.retain(|at, _| *at < end);
            if !live.topic.is_watched() && next.is_none() {
                sessions.remove(&session);
            }
            if let Some(next) = next {
                self.launch_next(&mut inner, session, row, next);
            }
        }
    }

    /// Launches a `SendNow`'s message once the turn it waited on ended. A launch that fails
    /// puts the message back at the queue's head, paused on the error.
    fn launch_next(self: &Arc<Self>, inner: &mut Inner, session: SessionId, row: i64, next: Next) {
        let Next {
            op,
            id,
            message,
            resolve,
        } = next;
        let mut resolve = Some(resolve);
        let launched = self.queued_turn(row, &message).and_then(|turn| {
            let body = session::Send {
                session,
                message: id,
                text: message.text.clone(),
                attachments: message.attachments.clone(),
                turn: Some(Box::new(turn)),
            };
            self.launch(inner, op, body, &mut resolve)
        });
        let Err(error) = launched else {
            return;
        };
        let outcome = Outcome::Rejected(Rejection::Denied(error.clone()));
        let events = self
            .requeue(row, *message, Some(error))
            .into_iter()
            .collect();
        if let Some(resolve) = resolve {
            self.resolved(inner, session, op, resolve, outcome, events);
        }
    }

    /// The turn that sends the queued `message`, as the app's queue sends one: its own mode,
    /// attachments and prepared prompt over the session's stored settings.
    fn queued_turn(&self, row: i64, message: &Message) -> Result<TurnContext, String> {
        let store = locked(&self.store)?;
        let summary = store
            .sessions()?
            .into_iter()
            .find(|summary| summary.id == row)
            .ok_or("Session no longer exists")?;
        let mut settings = store.agent_settings(row)?.unwrap_or_default();
        if settings.account.id != onicode_core::accounts::DEFAULT {
            settings.account.root = store.data_dir().map(Path::to_path_buf);
        }
        drop(store);
        let mode = message.mode.as_deref();
        settings.orchestrator = mode == Some("orchestrator");
        settings.plan = mode == Some("plan") || settings.orchestrator;
        settings.operator = mode == Some("operator");
        settings.operator_enabled |= settings.operator;
        Ok(TurnContext {
            summary,
            settings,
            prepared_prompt: message.prepared_prompt.clone(),
            queued: Some(message.id.clone()),
            ..TurnContext::default()
        })
    }

    /// Takes the message `id` out of the stored queue, publishing the queue without it.
    fn unqueue(
        &self,
        inner: &mut Inner,
        session: SessionId,
        row: i64,
        id: MessageId,
    ) -> Result<Option<Message>, String> {
        let store = locked(&self.store)?;
        let mut queue = store.follow_ups(row)?;
        let id = id.to_string();
        let Some(at) = queue.messages.iter().position(|message| message.id == id) else {
            return Ok(None);
        };
        let message = queue.messages.remove(at);
        queue.revision += 1;
        store.save_follow_ups(row, &queue)?;
        drop(store);
        if let Some(live) = inner.sessions.get_mut(&session) {
            publish(
                live,
                vec![SessionEvent::Queue {
                    rev: queue.revision,
                    state: queue,
                }],
            );
        }
        Ok(Some(message))
    }

    /// Puts a message taken out of the queue back at its head; with `error`, the queue pauses
    /// on it. The queue event, unless the message is already back.
    fn requeue(
        &self,
        row: i64,
        mut message: Message,
        error: Option<String>,
    ) -> Option<SessionEvent> {
        let store = locked(&self.store).ok()?;
        let mut queue = store.follow_ups(row).ok()?;
        if queue.messages.iter().any(|queued| queued.id == message.id) {
            return None;
        }
        queue.paused |= error.is_some();
        message.error = error;
        queue.messages.insert(0, message);
        queue.revision += 1;
        store.save_follow_ups(row, &queue).ok()?;
        Some(SessionEvent::Queue {
            rev: queue.revision,
            state: queue,
        })
    }

    /// Settles a call at once with `outcome`, publishing `events` and its receipt.
    fn answered(
        &self,
        inner: &mut Inner,
        session: SessionId,
        op: Option<OpId>,
        outcome: Outcome,
        events: Vec<SessionEvent>,
    ) -> Done {
        let Inner { sessions, ops, .. } = inner;
        Done::Now(self.settled(
            sessions.get_mut(&session),
            ops,
            session,
            op,
            outcome,
            events,
        ))
    }

    /// Settles a call that waited on work outside the lock: answers its waiters, records it and
    /// publishes `events` and its receipt.
    fn resolved(
        &self,
        inner: &mut Inner,
        session: SessionId,
        op: Option<OpId>,
        resolve: watch::Sender<Option<Outcome>>,
        outcome: Outcome,
        events: Vec<SessionEvent>,
    ) {
        resolve.send_replace(Some(outcome.clone()));
        if let Some(op) = op {
            inner.ops.inflight.remove(&op);
        }
        self.answered(inner, session, op, outcome, events);
    }

    /// The receipt the finished turn saved.
    fn turn_finished(&self, id: i64) -> Option<SessionEvent> {
        let settings = locked(&self.store).ok()?.agent_settings(id).ok()??;
        let (turn, info) = settings
            .turns
            .into_iter()
            .filter(|(_, info)| info.joined_turn.is_none())
            .max_by_key(|(turn, _)| *turn)?;
        Some(SessionEvent::TurnFinished { turn, info })
    }

    /// Turn `turn_id`'s updates ended. Its last update ends it; one that never came (the runner
    /// panicked or never started) still ends it, with an error.
    fn turn_closed(self: &Arc<Self>, session: SessionId, turn_id: u64) {
        let end = {
            let inner = self.lock();
            inner
                .sessions
                .get(&session)
                .filter(|live| live.turn.as_ref().is_some_and(|turn| turn.id == turn_id))
                .map(|live| live.topic.state().first_row + live.topic.state().rows.len())
        };
        if let Some(end) = end {
            self.apply_update(
                session,
                turn_id,
                Update {
                    start: end,
                    status: "The turn ended without finishing".into(),
                    finished: true,
                    ..Update::default()
                },
            );
        }
    }

    /// `Steer`: hands a message to the running turn. Its receipt arrives when the provider
    /// accepts it (with the update that holds its prompt) or fails it.
    pub async fn steer(&self, op: Option<OpId>, body: Steer) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner { sessions, ops, .. } = inner;
            let message = Message {
                id: body.message.to_string(),
                text: body.text,
                attachments: body.attachments,
                sent_at: Some(onicode_core::time::now_ms()),
                steer: true,
                ..Message::default()
            };
            let mut live = sessions.get_mut(&body.session);
            let id = message.id.clone();
            let sent = live
                .as_ref()
                .and_then(|live| live.turn.as_ref())
                .and_then(|turn| turn.controls.as_ref())
                .map(|controls| {
                    controls
                        .send(Control {
                            id: id.clone(),
                            decision: Decision::Steer(message),
                        })
                        .is_ok()
                });
            let outcome = match sent {
                None => Outcome::Rejected(Rejection::Gone),
                Some(false) => Outcome::Rejected(Rejection::Denied(
                    "The provider disconnected before accepting this message".into(),
                )),
                Some(true) => {
                    let (resolve, waiting) = watch::channel(None);
                    if let Some(op) = op {
                        ops.inflight.insert(op, waiting.clone());
                    }
                    if let Some(turn) = live.as_mut().and_then(|live| live.turn.as_mut()) {
                        turn.steers.insert(
                            id,
                            Pending {
                                op,
                                message: body.message,
                                resolve,
                                queued: None,
                            },
                        );
                    }
                    return Ok(Done::Later(waiting));
                }
            };
            Ok(Done::Now(self.settled(
                live,
                ops,
                body.session,
                op,
                outcome,
                Vec::new(),
            )))
        })
        .await
    }

    /// `Stop`: cancels the running turn; its last update follows.
    pub async fn stop(&self, op: Option<OpId>, body: Stop) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner {
                sessions,
                ops,
                compacting,
                ..
            } = inner;
            if let Some(compaction) = compacting.get(&body.session) {
                compaction.store(true, Ordering::Relaxed);
            }
            let mut live = sessions.get_mut(&body.session);
            if let Some(turn) = live.as_mut().and_then(|live| live.turn.as_mut()) {
                turn.cancel.store(true, Ordering::Relaxed);
                turn.controls = None;
            }
            Ok(Done::Now(self.settled(
                live,
                ops,
                body.session,
                op,
                Outcome::Applied,
                Vec::new(),
            )))
        })
        .await
    }

    /// `Decide`: answers an approval, or with `Deny` dismisses a question. The first answer to
    /// reach the host wins; a later one is `AlreadyResolved`.
    pub async fn decide(&self, op: Option<OpId>, body: Decide) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner { sessions, ops, .. } = inner;
            let live = sessions.get_mut(&body.session);
            let id = body.approval;
            let (outcome, events) = match live.as_deref() {
                None => (Outcome::Rejected(Rejection::Gone), Vec::new()),
                Some(held)
                    if held
                        .topic
                        .state()
                        .approvals
                        .iter()
                        .any(|known| known.id == id) =>
                {
                    let decision = match body.answer {
                        Verdict::Allow => Decision::Allow,
                        Verdict::AllowForSession => Decision::Session,
                        Verdict::Deny => Decision::Deny,
                    };
                    let outcome = control(held, &id, decision);
                    (outcome, vec![SessionEvent::ApprovalResolved(id)])
                }
                Some(held)
                    if body.answer == Verdict::Deny
                        && held
                            .topic
                            .state()
                            .questions
                            .iter()
                            .any(|known| known.id == id) =>
                {
                    let outcome = control(held, &id, Decision::Deny);
                    (outcome, vec![SessionEvent::QuestionResolved(id)])
                }
                Some(_) => (Outcome::Rejected(Rejection::AlreadyResolved), Vec::new()),
            };
            let events = if outcome == Outcome::Applied {
                events
            } else {
                Vec::new()
            };
            Ok(Done::Now(self.settled(
                live,
                ops,
                body.session,
                op,
                outcome,
                events,
            )))
        })
        .await
    }

    /// `Answer`: answers a question, once.
    pub async fn answer(&self, op: Option<OpId>, body: Answer) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner { sessions, ops, .. } = inner;
            let live = sessions.get_mut(&body.session);
            let id = body.question;
            let (outcome, events) = match live.as_deref() {
                None => (Outcome::Rejected(Rejection::Gone), Vec::new()),
                Some(held)
                    if held
                        .topic
                        .state()
                        .questions
                        .iter()
                        .any(|known| known.id == id) =>
                {
                    let outcome = control(held, &id, Decision::Answer(body.answers));
                    let events = if outcome == Outcome::Applied {
                        vec![SessionEvent::QuestionResolved(id)]
                    } else {
                        Vec::new()
                    };
                    (outcome, events)
                }
                Some(_) => (Outcome::Rejected(Rejection::AlreadyResolved), Vec::new()),
            };
            Ok(Done::Now(self.settled(
                live,
                ops,
                body.session,
                op,
                outcome,
                events,
            )))
        })
        .await
    }

    /// `KeepQuestionOpen`: keeps a question open past its automatic resolution.
    pub async fn keep_question_open(
        &self,
        op: Option<OpId>,
        body: KeepQuestionOpen,
    ) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner { sessions, ops, .. } = inner;
            let live = sessions.get_mut(&body.session);
            let outcome = match live.as_deref() {
                None => Outcome::Rejected(Rejection::Gone),
                Some(held)
                    if held
                        .topic
                        .state()
                        .questions
                        .iter()
                        .any(|q| q.id == body.question) =>
                {
                    control(held, &body.question, Decision::KeepOpen)
                }
                Some(_) => Outcome::Rejected(Rejection::AlreadyResolved),
            };
            Ok(Done::Now(self.settled(
                live,
                ops,
                body.session,
                op,
                outcome,
                Vec::new(),
            )))
        })
        .await
    }

    /// `QueueEdit`: one edit of the stored queue, made against the revision it saw.
    pub async fn queue_edit(&self, op: Option<OpId>, body: QueueEdit) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner { sessions, ops, .. } = inner;
            let store = locked(&self.store)?;
            let Some(id) = store.session_by_uid(body.session.0)? else {
                drop(store);
                return Ok(Done::Now(self.settled(
                    None,
                    ops,
                    body.session,
                    op,
                    Outcome::Rejected(Rejection::Gone),
                    Vec::new(),
                )));
            };
            let mut queue = store.follow_ups(id)?;
            let outcome = edit_queue(&mut queue, &body);
            let mut events = Vec::new();
            if outcome == Outcome::Applied {
                queue.revision += 1;
                store.save_follow_ups(id, &queue)?;
                events.push(SessionEvent::Queue {
                    rev: queue.revision,
                    state: queue,
                });
            }
            drop(store);
            let live = sessions.get_mut(&body.session);
            Ok(Done::Now(self.settled(
                live,
                ops,
                body.session,
                op,
                outcome,
                events,
            )))
        })
        .await
    }

    /// `Configure`: replaces the session's provider and agent settings, unless the stored
    /// settings are newer. The usage limit stays the engine's: a viewer only arms or disarms the
    /// one it holds (`resume::viewer_limit`).
    pub async fn configure(&self, op: Option<OpId>, body: Configure) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Configure {
                session,
                provider,
                mut settings,
            } = body;
            let store = locked(&self.store)?;
            let outcome = match store.session_by_uid(session.0)? {
                Some(id) => match store.agent_settings(id)? {
                    Some(saved) if saved.revision > settings.revision => {
                        Outcome::Rejected(Rejection::StaleRev)
                    }
                    saved => {
                        settings.usage_limit = resume::viewer_limit(
                            saved.and_then(|saved| saved.usage_limit),
                            settings.usage_limit.take(),
                        );
                        store.configure_agent(id, &provider, &settings)?;
                        Outcome::Applied
                    }
                },
                None => Outcome::Rejected(Rejection::Gone),
            };
            drop(store);
            let events = if outcome == Outcome::Applied {
                vec![
                    SessionEvent::UsageLimit(settings.usage_limit.clone()),
                    SessionEvent::Settings(Box::new(settings)),
                ]
            } else {
                Vec::new()
            };
            Ok(self.answered(inner, session, op, outcome, events))
        })
        .await
    }

    /// `ReadSession`: what the store holds of the session besides its rows.
    pub fn read_session(&self, body: &ReadSession) -> Result<SessionRecord, String> {
        let store = locked(&self.store)?;
        let id = store
            .session_by_uid(body.session.0)?
            .ok_or("Session no longer exists")?;
        Ok(SessionRecord {
            settings: store.agent_settings(id)?.map(Box::new),
            context: store.context_usage(id)?,
            queue: store.follow_ups(id)?,
            edit_recovery: store.edit_recovery_reason(id)?,
        })
    }

    /// `SaveQueue`: replaces the stored queue whole. The attachments are copied before the hub
    /// is locked; a repeat copies nothing new.
    pub async fn save_queue(
        &self,
        op: Option<OpId>,
        body: SaveQueue,
    ) -> Result<follow_up::State, String> {
        let (row, directory) = {
            let store = locked(&self.store)?;
            let row = store
                .session_by_uid(body.session.0)?
                .ok_or("Session no longer exists")?;
            (row, store.data_dir().map(Path::to_path_buf))
        };
        let mut queue = body.queue;
        queue.retain_attachments(row, directory.as_deref())?;
        let outcome = self
            .once(op, |inner| {
                locked(&self.store)?.save_follow_ups(row, &queue)?;
                let events = vec![SessionEvent::Queue {
                    rev: queue.revision,
                    state: queue.clone(),
                }];
                Ok(self.answered(inner, body.session, op, Outcome::Applied, events))
            })
            .await?;
        match outcome {
            Outcome::Applied => {
                // The sidebar marks a session that holds a saved draft.
                if let Some(workspace) = self.workspace.get() {
                    workspace.draft_saved(row, queue.draft.is_some());
                }
                Ok(queue)
            }
            Outcome::Rejected(rejection) => Err(format!("The queue was not saved: {rejection:?}")),
        }
    }

    /// `SendNow`: takes the queued message out of the stored queue and sends it as the next
    /// turn, stopping the running one first.
    pub async fn send_now(
        self: &Arc<Self>,
        op: Option<OpId>,
        body: SendNow,
    ) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let (row, running, following) = {
                let Inner {
                    sessions,
                    next_epoch,
                    ..
                } = &mut *inner;
                let live = self.live(sessions, next_epoch, body.session)?;
                let turn = live.turn.as_ref();
                (
                    live.id,
                    turn.is_some(),
                    turn.is_some_and(|turn| turn.next.is_some()),
                )
            };
            if following {
                let outcome = Outcome::Rejected(Rejection::Denied(
                    "Another message is already being sent now".into(),
                ));
                return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
            }
            let Some(message) = self.unqueue(inner, body.session, row, body.id)? else {
                let outcome = Outcome::Rejected(Rejection::Gone);
                return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
            };
            let (resolve, waiting) = watch::channel(None);
            if !running {
                let mut resolve = Some(resolve);
                let launched = self.queued_turn(row, &message).and_then(|turn| {
                    let send = session::Send {
                        session: body.session,
                        message: body.id,
                        text: message.text.clone(),
                        attachments: message.attachments.clone(),
                        turn: Some(Box::new(turn)),
                    };
                    self.launch(inner, op, send, &mut resolve)
                });
                if let Err(error) = launched {
                    let events = self.requeue(row, message, Some(error.clone()));
                    if let Some(live) = inner.sessions.get_mut(&body.session) {
                        publish(live, events.into_iter().collect());
                    }
                    return Err(error);
                }
                return Ok(Done::Later(waiting));
            }
            let Inner { sessions, ops, .. } = inner;
            let Some(turn) = sessions
                .get_mut(&body.session)
                .and_then(|live| live.turn.as_mut())
            else {
                return Err("The running turn ended before this message could follow it".into());
            };
            turn.cancel.store(true, Ordering::Relaxed);
            turn.controls = None;
            if let Some(op) = op {
                ops.inflight.insert(op, waiting.clone());
            }
            turn.next = Some(Next {
                op,
                id: body.id,
                message: Box::new(message),
                resolve,
            });
            Ok(Done::Later(waiting))
        })
        .await
    }

    /// `SteerNow`: takes the queued message out of the stored queue and steers it into the
    /// running turn. Its receipt arrives as a `Steer`'s does; a refusal puts the message back.
    pub async fn steer_now(&self, op: Option<OpId>, body: SteerNow) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let running = inner.sessions.get(&body.session).and_then(|live| {
                let controls = live.turn.as_ref()?.controls.clone()?;
                Some((live.id, controls))
            });
            let Some((row, controls)) = running else {
                let outcome = Outcome::Rejected(Rejection::Gone);
                return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
            };
            let Some(mut message) = self.unqueue(inner, body.session, row, body.id)? else {
                let outcome = Outcome::Rejected(Rejection::Gone);
                return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
            };
            message.steer = true;
            message.error = None;
            let id = message.id.clone();
            let control = Control {
                id: id.clone(),
                decision: Decision::Steer(message.clone()),
            };
            if controls.send(control).is_err() {
                let error = "The provider disconnected before accepting this message".to_owned();
                let events = self.requeue(row, message, Some(error.clone()));
                let outcome = Outcome::Rejected(Rejection::Denied(error));
                return Ok(self.answered(
                    inner,
                    body.session,
                    op,
                    outcome,
                    events.into_iter().collect(),
                ));
            }
            let (resolve, waiting) = watch::channel(None);
            if let Some(op) = op {
                inner.ops.inflight.insert(op, waiting.clone());
            }
            if let Some(turn) = inner
                .sessions
                .get_mut(&body.session)
                .and_then(|live| live.turn.as_mut())
            {
                turn.steers.insert(
                    id,
                    Pending {
                        op,
                        message: body.id,
                        resolve,
                        queued: Some(Box::new(message)),
                    },
                );
            }
            Ok(Done::Later(waiting))
        })
        .await
    }

    /// `RecallLastTurn`: holds the session's last turn for an edit and replies with it.
    pub fn recall_last_turn(&self, body: &RecallLastTurn) -> Result<RecalledTurn, String> {
        let mut inner = self.lock();
        if inner
            .sessions
            .get(&body.session)
            .is_some_and(|live| live.turn.is_some())
        {
            return Err("Wait for the running turn to finish before editing it".into());
        }
        let store = locked(&self.store)?;
        let row = store
            .session_by_uid(body.session.0)?
            .ok_or("Session no longer exists")?;
        let rows = store.load(row)?.rows;
        let edit = store.prepare_edit_last_turn(row, &rows, body.turn.as_deref())?;
        drop(store);
        let recalled = RecalledTurn {
            start: edit.start,
            prompt: edit.prompt.clone(),
            attachments: edit.attachments.clone(),
            provider: edit.provider().to_owned(),
            provider_id: edit.provider_id.clone(),
            account_id: edit.account_id.clone(),
            cwd: edit.cwd.clone(),
            rows: edit.rows.clone(),
        };
        inner.recalled.insert(body.session, edit);
        Ok(recalled)
    }

    /// `EditLastTurn`: rewinds the recalled turn in the provider, then, only once the provider
    /// confirms, drops it from the transcript. No turn starts in the session meanwhile.
    pub async fn edit_last_turn(
        self: &Arc<Self>,
        op: Option<OpId>,
        body: EditLastTurn,
    ) -> Result<EditedTurn, String> {
        let session = body.session;
        let outcome = self
            .once(op, |inner| {
                let busy = inner
                    .sessions
                    .get(&session)
                    .is_some_and(|live| live.turn.is_some());
                let edit = if busy {
                    None
                } else {
                    inner.recalled.remove(&session)
                };
                let row = match edit {
                    Some(_) => locked(&self.store)?.session_by_uid(session.0)?,
                    None => None,
                };
                let (Some(edit), Some(row)) = (edit, row) else {
                    let outcome = if busy {
                        Outcome::Rejected(Rejection::Denied(
                            "Wait for the running turn to finish before editing it".into(),
                        ))
                    } else {
                        Outcome::Rejected(Rejection::Gone)
                    };
                    return Ok(self.answered(inner, session, op, outcome, Vec::new()));
                };
                inner.rewinding.insert(session);
                let (resolve, waiting) = watch::channel(None);
                if let Some(op) = op {
                    inner.ops.inflight.insert(op, waiting.clone());
                }
                let hub = self.clone();
                self.spawn.spawn(Box::pin(async move {
                    let outcome = hub.rewind(row, &edit, &body);
                    let mut inner = hub.lock();
                    inner.rewinding.remove(&session);
                    if outcome == Outcome::Applied
                        && let Some(live) = inner.sessions.get_mut(&session)
                    {
                        // The transcript lost its last turn and the provider binding moved.
                        let _ = hub.refresh(live);
                        let end = live.topic.state().first_row + live.topic.state().rows.len();
                        live.prompts.retain(|at, _| *at < end);
                    }
                    hub.resolved(&mut inner, session, op, resolve, outcome, Vec::new());
                }));
                Ok(Done::Later(waiting))
            })
            .await?;
        let provider_id = match outcome {
            Outcome::Applied => {
                let store = locked(&self.store)?;
                match store.session_by_uid(session.0)? {
                    Some(row) => store
                        .sessions()?
                        .into_iter()
                        .find(|summary| summary.id == row)
                        .and_then(|summary| summary.provider_id),
                    None => None,
                }
            }
            Outcome::Rejected(_) => None,
        };
        Ok(EditedTurn {
            outcome,
            provider_id,
        })
    }

    /// The provider half of an edit, off the hub's lock: checks the edit again, rewinds the
    /// provider, and writes the shorter transcript only after the provider confirmed.
    fn rewind(&self, row: i64, edit: &EditSnapshot, body: &EditLastTurn) -> Outcome {
        let denied = |error: String| Outcome::Rejected(Rejection::Denied(error));
        let aside = match self.rewind_request(row, edit, body) {
            Ok(aside) => aside,
            Err(error) => return denied(error),
        };
        let binding = match self.asides.rewind(aside, edit.provider_turn_id.as_deref()) {
            Ok(binding) => binding,
            Err(error) if error.provider_may_have_changed => {
                if let Err(marking) =
                    locked(&self.store).and_then(|store| store.mark_edit_recovery_required(row))
                {
                    return denied(marking);
                }
                return denied(format!("Provider recovery required: {error}"));
            }
            Err(error) => return denied(error.to_string()),
        };
        let mut store = match locked(&self.store) {
            Ok(store) => store,
            Err(error) => return denied(error),
        };
        match store.finish_edit_last_turn(row, edit, &binding) {
            Ok(()) => Outcome::Applied,
            Err(error) => denied(format!(
                "Provider recovery required: Provider rewound, but local history changed: {error}"
            )),
        }
    }

    fn rewind_request(
        &self,
        row: i64,
        edit: &EditSnapshot,
        body: &EditLastTurn,
    ) -> Result<Aside, String> {
        onicode_core::composer::validate_attachments(&body.attachments)?;
        let store = locked(&self.store)?;
        store.validate_edit_last_turn(row, edit)?;
        let mut settings = store.agent_settings(row)?.unwrap_or_default();
        if settings.account.id != onicode_core::accounts::DEFAULT {
            settings.account.root = store.data_dir().map(Path::to_path_buf);
        }
        Ok(Aside {
            kind: Kind::from_id(edit.provider()),
            cwd: edit.cwd.clone().into(),
            prompt: String::new(),
            provider_id: Some(edit.provider_id.clone()),
            model: body.model.clone(),
            settings,
        })
    }

    /// `Rewind`: drops every turn from `turn` on; the session leaves its provider thread and its
    /// next prompt carries the kept turns as a handoff brief.
    pub async fn rewind_turns(&self, op: Option<OpId>, body: Rewind) -> Result<Outcome, String> {
        let session = body.session;
        let mut rewound = None;
        let outcome = self
            .once(op, |inner| {
                let busy = inner.rewinding.contains(&session)
                    || inner.compacting.contains_key(&session)
                    || inner
                        .sessions
                        .get(&session)
                        .is_some_and(|live| live.turn.is_some());
                if busy {
                    let outcome = Outcome::Rejected(Rejection::Denied(
                        "Wait for the running turn to finish before rewinding".into(),
                    ));
                    return Ok(self.answered(inner, session, op, outcome, Vec::new()));
                }
                let row = {
                    let store = locked(&self.store)?;
                    match store.session_by_uid(session.0)? {
                        Some(row)
                            if store
                                .load(row)?
                                .rows
                                .get(body.turn)
                                .is_some_and(|prompt| prompt.role == "user") =>
                        {
                            store.rewind_to(row, body.turn)?;
                            Some(row)
                        }
                        _ => None,
                    }
                };
                let Some(row) = row else {
                    let outcome = Outcome::Rejected(Rejection::Gone);
                    return Ok(self.answered(inner, session, op, outcome, Vec::new()));
                };
                inner.recalled.remove(&session);
                if let Some(live) = inner.sessions.get_mut(&session) {
                    self.refresh(live)?;
                    live.prompts.retain(|at, _| *at < body.turn);
                }
                rewound = Some(row);
                Ok(self.answered(inner, session, op, Outcome::Applied, Vec::new()))
            })
            .await?;
        if let (Some(row), Some(workspace)) = (rewound, self.workspace.get()) {
            workspace.refresh_session(row);
        }
        Ok(outcome)
    }

    /// `Fork`: a new session `into` holding this one's turns before `turn`, created through the
    /// workspace so every viewer sees it arrive.
    pub async fn fork(&self, op: Option<OpId>, body: Fork) -> Result<Outcome, String> {
        let workspace = self
            .workspace
            .get()
            .cloned()
            .ok_or("This engine keeps no workspace")?;
        let source = {
            let store = locked(&self.store)?;
            match store.session_by_uid(body.session.0)? {
                None => None,
                Some(row) => {
                    let summary = store
                        .sessions()?
                        .into_iter()
                        .find(|summary| summary.id == row)
                        .ok_or("Session no longer exists")?;
                    let rows = store.load(row)?.rows;
                    let settings = store.agent_settings(row)?.unwrap_or_default();
                    Some((summary, rows, settings))
                }
            }
        };
        let Some((summary, mut rows, mut settings)) = source else {
            return Ok(Outcome::Rejected(Rejection::Gone));
        };
        if rows
            .get(body.turn)
            .is_some_and(|prompt| prompt.role != "user")
        {
            return Ok(Outcome::Rejected(Rejection::Gone));
        }
        rows.truncate(body.turn);
        settings.turns.retain(|&row, _| row < rows.len());
        settings.pending_provider_switch = None;
        settings.mono_message_id = None;
        settings.handoff = (!rows.is_empty()).then(|| Handoff {
            from: summary.provider.clone(),
            brief: onicode_core::response::handoff(&rows),
        });
        let created = workspace
            .create_session(
                op,
                CreateSession {
                    session: body.into,
                    provider: summary.provider.clone(),
                    cwd: summary.cwd.clone(),
                    title: Some(summary.title.clone()),
                    start: NewSession::Settings(Box::new(settings)),
                    started_by: None,
                    scope: Vec::new(),
                },
            )
            .await?;
        if created.outcome != Outcome::Applied {
            return Ok(created.outcome);
        }
        let row = {
            let mut store = locked(&self.store)?;
            let row = store
                .session_by_uid(body.into.0)?
                .ok_or("The forked session no longer exists")?;
            if store.load(row)?.rows.is_empty() {
                store.write_rows(row, 0, &rows)?;
            }
            row
        };
        workspace.refresh_session(row);
        Ok(Outcome::Applied)
    }

    /// `SwitchProvider`: the stored provider (its thread let go when it changes), the thread to
    /// bind, and the context meter to keep.
    pub async fn switch_provider(
        &self,
        op: Option<OpId>,
        body: SwitchProvider,
    ) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let SwitchProvider {
                session,
                provider,
                thread,
                context,
            } = body;
            if inner
                .sessions
                .get(&session)
                .is_some_and(|live| live.turn.is_some())
            {
                let outcome = Outcome::Rejected(Rejection::Denied(
                    "Wait for the running turn to finish before switching provider".into(),
                ));
                return Ok(self.answered(inner, session, op, outcome, Vec::new()));
            }
            let store = locked(&self.store)?;
            let Some(row) = store.session_by_uid(session.0)? else {
                drop(store);
                let outcome = Outcome::Rejected(Rejection::Gone);
                return Ok(self.answered(inner, session, op, outcome, Vec::new()));
            };
            store.change_provider(row, &provider)?;
            if let Some(thread) = &thread {
                store.provider_id(row, thread)?;
            }
            if let Some(context) = &context {
                store.save_context_usage(row, context)?;
            }
            drop(store);
            if let Some(live) = inner.sessions.get_mut(&session) {
                self.refresh(live)?;
            }
            Ok(self.answered(inner, session, op, Outcome::Applied, Vec::new()))
        })
        .await
    }

    /// `ResetThread`: lets the session's provider thread go if it is still `expected`.
    pub async fn reset_thread(
        &self,
        op: Option<OpId>,
        body: ResetThread,
    ) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let store = locked(&self.store)?;
            let outcome = match store.session_by_uid(body.session.0)? {
                Some(row) => {
                    store.reset_provider_id_if(row, body.expected.as_deref())?;
                    Outcome::Applied
                }
                None => Outcome::Rejected(Rejection::Gone),
            };
            drop(store);
            if let Some(live) = inner.sessions.get_mut(&body.session) {
                self.refresh(live)?;
            }
            Ok(self.answered(inner, body.session, op, outcome, Vec::new()))
        })
        .await
    }

    /// `Compact`: the provider's own compaction of the thread's context, recorded as a
    /// `contextCompaction` tool row. No turn starts meanwhile; `Stop` stops it.
    pub async fn compact(
        self: &Arc<Self>,
        op: Option<OpId>,
        body: Compact,
    ) -> Result<Compacted, String> {
        let hub = self.clone();
        self.ledger
            .once(op, async move { hub.compact_once(body).await })
            .await
    }

    async fn compact_once(self: &Arc<Self>, body: Compact) -> Result<Compacted, String> {
        let Compact {
            session,
            at,
            tool,
            model,
            mut settings,
        } = body;
        let (row, summary, mut context) = {
            let store = locked(&self.store)?;
            let row = store
                .session_by_uid(session.0)?
                .ok_or("Session no longer exists")?;
            let summary = store
                .sessions()?
                .into_iter()
                .find(|summary| summary.id == row)
                .ok_or("Session no longer exists")?;
            self.engine_settings(&store, &mut settings);
            (row, summary, store.context_usage(row)?.unwrap_or_default())
        };
        let kind = Kind::from_id(&summary.provider);
        if summary.provider_id.is_none() {
            return Err("Send a message before compacting this chat.".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut inner = self.lock();
            let busy = inner.rewinding.contains(&session)
                || inner.compacting.contains_key(&session)
                || inner
                    .sessions
                    .get(&session)
                    .is_some_and(|live| live.turn.is_some());
            if busy {
                return Err(
                    "Wait for the current turn and queued messages before compacting context."
                        .into(),
                );
            }
            inner.compacting.insert(session, cancel.clone());
        }
        let mut call = ToolCall::new(&tool, "contextCompaction");
        let mut compacted = Row {
            role: "tool".into(),
            text: serde_json::to_string(&call).map_err(|error| error.to_string())?,
            first: true,
            sent_at: Some(onicode_core::time::now_ms()),
            id: None,
        };
        let written = locked(&self.store)
            .and_then(|mut store| store.write_rows(row, at, std::slice::from_ref(&compacted)));
        self.caught_up(session);
        let account = settings.account.id.clone();
        let aside = Aside {
            kind,
            cwd: settings
                .worktree_cwd
                .clone()
                .unwrap_or_else(|| summary.cwd.clone())
                .into(),
            prompt: String::new(),
            provider_id: summary.provider_id.clone(),
            model,
            settings,
        };
        let (reply, answer) = futures::channel::oneshot::channel();
        let (asides, stop) = (self.asides.clone(), cancel.clone());
        let started = context;
        self.spawn.spawn(Box::pin(async move {
            let mut context = started;
            let mut commands = None;
            let result = written.and_then(|()| {
                asides.compact(aside, stop, &mut |events| {
                    for event in events {
                        match event {
                            Event::Context { used, window } => context.merge(used, window),
                            Event::Commands(discovered) => commands = Some(discovered),
                            _ => {}
                        }
                    }
                    Ok(())
                })
            });
            let _ = reply.send((result, context, commands));
        }));
        let stopped = || "The engine stopped before this call finished".to_owned();
        let (result, merged, commands) = answer
            .await
            .unwrap_or_else(|_| (Err(stopped()), context, None));
        context = merged;
        let result = result.and_then(|()| {
            let store = locked(&self.store)?;
            // Only while the session still runs on the provider and account it compacted.
            if store.agent_settings(row)?.unwrap_or_default().account.id == account
                && store
                    .sessions()?
                    .iter()
                    .any(|current| current.id == row && current.provider == summary.provider)
            {
                store.save_context_usage(row, &context)?;
            }
            Ok(())
        });
        call.state = if cancel.load(Ordering::Relaxed) {
            ToolState::Cancelled
        } else if result.is_ok() {
            ToolState::Completed
        } else {
            ToolState::Failed
        };
        call.output = result.as_ref().err().cloned().unwrap_or_default();
        compacted.text = serde_json::to_string(&call).map_err(|error| error.to_string())?;
        let saved = locked(&self.store)
            .and_then(|mut store| store.write_rows(row, at, std::slice::from_ref(&compacted)));
        self.lock().compacting.remove(&session);
        self.caught_up(session);
        Ok(Compacted {
            row: compacted,
            context,
            commands,
            error: result.and(saved).err(),
        })
    }

    /// Catches a live session up with the store after a write outside a turn.
    fn caught_up(&self, session: SessionId) {
        let mut inner = self.lock();
        if let Some(live) = inner.sessions.get_mut(&session) {
            let _ = self.refresh(live);
        }
    }

    /// `ReadBtw`: the session's by-the-way threads.
    pub fn read_btw(&self, body: &ReadBtw) -> Result<BtwThreads, String> {
        let store = locked(&self.store)?;
        let row = store
            .session_by_uid(body.session.0)?
            .ok_or("Session no longer exists")?;
        let saved = stored_btw(&store, row)?;
        Ok(BtwThreads {
            revision: saved.revision,
            threads: saved.threads.into_iter().map(|kept| kept.thread).collect(),
        })
    }

    /// `SaveBtw`: keeps the threads unless the stored ones are as new; an imported thread keeps
    /// its desktop original.
    pub async fn save_btw(&self, op: Option<OpId>, body: SaveBtw) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let store = locked(&self.store)?;
            let Some(row) = store.session_by_uid(body.session.0)? else {
                drop(store);
                let outcome = Outcome::Rejected(Rejection::Gone);
                return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
            };
            // A stored list that no longer reads is replaced.
            let held = stored_btw(&store, row).unwrap_or_default();
            if held.revision >= body.threads.revision {
                drop(store);
                let outcome = Outcome::Rejected(Rejection::StaleRev);
                return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
            }
            let mut originals: HashMap<String, serde_json::Value> = held
                .threads
                .into_iter()
                .filter_map(|kept| Some((kept.thread.id, kept.desktop?)))
                .collect();
            let saved = SavedBtw {
                revision: body.threads.revision,
                threads: body
                    .threads
                    .threads
                    .into_iter()
                    .map(|thread| SavedThread {
                        desktop: originals.remove(&thread.id),
                        thread,
                    })
                    .collect(),
            };
            let text = serde_json::to_string(&saved).map_err(|error| error.to_string())?;
            store.save_setting(&btw_key(row), &text)?;
            drop(store);
            Ok(self.answered(inner, body.session, op, Outcome::Applied, Vec::new()))
        })
        .await
    }

    /// `Btw`: answers a by-the-way thread on its own provider, read-only and text-only. The
    /// answer streams to watchers, is kept with the thread when it ends, and comes in the reply.
    pub async fn btw(self: &Arc<Self>, op: Option<OpId>, body: Btw) -> Result<BtwAnswer, String> {
        let answer: Arc<Mutex<Option<String>>> = Arc::default();
        let slot = answer.clone();
        let outcome = self
            .once(op, |inner| {
                let key = (body.session, body.thread.id.clone());
                if inner.btw.contains_key(&key) {
                    let outcome = Outcome::Rejected(Rejection::Denied(
                        "This side question is already being answered".into(),
                    ));
                    return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
                }
                let Some(row) = locked(&self.store)?.session_by_uid(body.session.0)? else {
                    let outcome = Outcome::Rejected(Rejection::Gone);
                    return Ok(self.answered(inner, body.session, op, outcome, Vec::new()));
                };
                let cancel = Arc::new(AtomicBool::new(false));
                inner.btw.insert(key.clone(), cancel.clone());
                let (resolve, waiting) = watch::channel(None);
                if let Some(op) = op {
                    inner.ops.inflight.insert(op, waiting.clone());
                }
                let hub = self.clone();
                self.spawn.spawn(Box::pin(async move {
                    let (outcome, text) = hub.answer_btw(row, &body, cancel);
                    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
                    let mut inner = hub.lock();
                    inner.btw.remove(&key);
                    hub.resolved(&mut inner, body.session, op, resolve, outcome, Vec::new());
                }));
                Ok(Done::Later(waiting))
            })
            .await?;
        let text = answer.lock().unwrap_or_else(PoisonError::into_inner).take();
        Ok(BtwAnswer { outcome, text })
    }

    /// Runs a by-the-way answer to its end, off the hub's lock: its outcome and its text.
    fn answer_btw(&self, row: i64, body: &Btw, cancel: Arc<AtomicBool>) -> (Outcome, String) {
        let thread = &body.thread;
        let kind = Kind::from_id(&thread.provider);
        let mut text = String::new();
        let result = self.btw_settings(kind, thread).and_then(|settings| {
            let aside = Aside {
                kind,
                cwd: thread.cwd.clone().into(),
                prompt: btw_prompt(thread),
                provider_id: None,
                model: thread.model.clone(),
                settings,
            };
            let stop = cancel.clone();
            self.asides
                .ask(aside, cancel, &mut |piece: String| -> Result<(), String> {
                    if text.len() > BTW_BYTES {
                        stop.store(true, Ordering::Relaxed);
                        return Ok(());
                    }
                    text.push_str(&piece);
                    self.btw_text(body.session, &thread.id, piece);
                    Ok(())
                })
        });
        let (outcome, status, error) = match result {
            Ok(()) => (Outcome::Applied, "completed", None),
            Err(error) if error == "Stopped" => {
                (Outcome::Rejected(Rejection::Denied(error)), "stopped", None)
            }
            Err(error) => (
                Outcome::Rejected(Rejection::Denied(error.clone())),
                "error",
                Some(error),
            ),
        };
        self.keep_btw(row, thread, &text, status, error);
        (outcome, text)
    }

    /// A by-the-way answer's settings: read-only and text-only, on the thread's account.
    fn btw_settings(&self, kind: Kind, thread: &BtwThread) -> Result<Settings, String> {
        let mut settings = Settings {
            account: thread.account.clone(),
            effort: thread.effort.clone(),
            model_settings: thread.model_settings.clone(),
            text_only: true,
            disable_claude_hooks: true,
            access: AccessMode::ReadOnly,
            ..Settings::default()
        };
        if settings.account.id != onicode_core::accounts::DEFAULT {
            let store = locked(&self.store)?;
            if !store
                .provider_accounts()?
                .exists(kind, &settings.account.id)
            {
                return Err("This provider account was removed. Choose another account before retrying the side question.".into());
            }
            settings.account.root = store.data_dir().map(Path::to_path_buf);
        }
        Ok(settings)
    }

    /// Hands a piece of a by-the-way answer to the session's watchers.
    fn btw_text(&self, session: SessionId, thread: &str, text: String) {
        let mut inner = self.lock();
        if let Some(live) = inner.sessions.get_mut(&session) {
            publish(
                live,
                vec![SessionEvent::BtwText {
                    thread: thread.to_owned(),
                    text,
                }],
            );
        }
    }

    /// Keeps a finished answer with its thread, at the stored revision, so a viewer's own save
    /// of the thread that follows still lands.
    fn keep_btw(
        &self,
        row: i64,
        thread: &BtwThread,
        answer: &str,
        status: &str,
        error: Option<String>,
    ) {
        let Ok(store) = locked(&self.store) else {
            return;
        };
        let Ok(mut saved) = stored_btw(&store, row) else {
            return;
        };
        let mut answered = thread.clone();
        let mut transcript = Transcript {
            rows: std::mem::take(&mut answered.rows),
        };
        transcript.begin("assistant");
        transcript.append(answer);
        answered.rows = transcript.rows;
        answered.status = status.into();
        answered.error = error;
        match saved
            .threads
            .iter_mut()
            .find(|kept| kept.thread.id == thread.id)
        {
            Some(kept) => kept.thread = answered,
            None => saved.threads.push(SavedThread {
                thread: answered,
                desktop: None,
            }),
        }
        if let Ok(text) = serde_json::to_string(&saved) {
            let _ = store.save_setting(&btw_key(row), &text);
        }
    }

    /// `StopBtw`: stops a running by-the-way answer; its `Btw` call then replies "Stopped".
    pub async fn stop_btw(&self, op: Option<OpId>, body: StopBtw) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let outcome = match inner.btw.get(&(body.session, body.thread)) {
                Some(cancel) => {
                    cancel.store(true, Ordering::Relaxed);
                    Outcome::Applied
                }
                None => Outcome::Rejected(Rejection::Gone),
            };
            Ok(self.answered(inner, body.session, op, outcome, Vec::new()))
        })
        .await
    }

    /// `ReadTranscript`: `turns` turns of the stored transcript before `before_turn`.
    pub fn read_transcript(&self, body: &ReadTranscript) -> Result<TranscriptPage, String> {
        let store = locked(&self.store)?;
        let id = store
            .session_by_uid(body.session.0)?
            .ok_or("Session no longer exists")?;
        let rows = store.load(id)?.rows;
        drop(store);
        let prompts = prompt_rows(&rows);
        let end_turn = body.before_turn.unwrap_or(prompts.len()).min(prompts.len());
        let first_turn = end_turn.saturating_sub(body.turns);
        // Rows before the first prompt open the first page.
        let first_row = if first_turn == 0 {
            0
        } else {
            prompts[first_turn]
        };
        let end_row = prompts.get(end_turn).copied().unwrap_or(rows.len());
        Ok(TranscriptPage {
            first_row,
            rows: rows[first_row..end_row.max(first_row)].to_vec(),
            more: first_turn > 0,
        })
    }

    /// `WatchSession`: a snapshot (or, with a `since` the ring covers, only what was missed),
    /// then every event. After `Resync` the stream ends; resubscribe with the last cursor.
    pub fn watch(
        self: &Arc<Self>,
        params: WatchSession,
    ) -> Result<BoxStream<'static, Item<SessionState, SessionEvent>>, String> {
        let (epoch, id, notify) = {
            let mut inner = self.lock();
            let Inner {
                sessions,
                next_epoch,
                ..
            } = &mut *inner;
            let live = self.live(sessions, next_epoch, params.session)?;
            let window = params.window_turns;
            let (id, notify) = live
                .topic
                .join_with(params.since, |state| snapshot(state, window));
            (live.topic.cursor().epoch, id, notify)
        };
        let (draining, leaving) = (self.clone(), self.clone());
        let session = params.session;
        Ok(feed::stream(
            notify,
            move || draining.drain(session, epoch, id),
            move || leaving.unwatch(session, epoch, id),
        ))
    }

    /// The session's topic of `epoch`, if it is still the live one: a session that went and came
    /// back numbers its watchers afresh.
    fn held(inner: &mut Inner, session: SessionId, epoch: u64) -> Option<&mut Live> {
        inner
            .sessions
            .get_mut(&session)
            .filter(|live| live.topic.cursor().epoch == epoch)
    }

    /// The watcher's next item: `None` when it is gone, `Some(None)` when nothing is ready.
    fn drain(
        &self,
        session: SessionId,
        epoch: u64,
        id: u64,
    ) -> Option<Option<Item<SessionState, SessionEvent>>> {
        let mut inner = self.lock();
        Self::held(&mut inner, session, epoch)?.topic.drain(id)
    }

    fn unwatch(&self, session: SessionId, epoch: u64, id: u64) {
        let closed = {
            let mut inner = self.lock();
            let idle = Self::held(&mut inner, session, epoch)
                .is_some_and(|live| live.topic.leave(id) && live.turn.is_none());
            if idle {
                inner.sessions.remove(&session).map(|live| live.id)
            } else {
                None
            }
        };
        // Its last viewer closed it: a session nobody prompted, with nothing in it, goes.
        if let Some(row) = closed
            && let Some(workspace) = self.workspace.get()
        {
            workspace.session_closed(row);
        }
    }
}

/// Numbers `events`, applies them to the state and the ring, and queues them for every watcher.
fn publish(live: &mut Live, events: Vec<SessionEvent>) {
    live.topic.publish(events);
}

/// Hands `decision` on `id` to the running turn.
fn control(live: &Live, id: &str, decision: Decision) -> Outcome {
    let sent = live
        .turn
        .as_ref()
        .and_then(|turn| turn.controls.as_ref())
        .is_some_and(|controls| {
            controls
                .send(Control {
                    id: id.to_owned(),
                    decision,
                })
                .is_ok()
        });
    if sent {
        Outcome::Applied
    } else {
        Outcome::Rejected(Rejection::Gone)
    }
}

fn prompt_rows(rows: &[onicode_core::transcript::Row]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.first && row.is_prompt())
        .map(|(at, _)| at)
        .collect()
}

/// Where the store keeps a session's by-the-way threads.
fn btw_key(row: i64) -> String {
    format!("btw:{row}")
}

/// The session's by-the-way threads as stored, or imported from the desktop app's when none
/// were saved yet.
fn stored_btw(store: &Store, row: i64) -> Result<SavedBtw, String> {
    if let Some(value) = store.setting(&btw_key(row))? {
        return serde_json::from_str(&value)
            .map_err(|error| format!("Could not restore side questions: {error}"));
    }
    let accounts = store.provider_accounts()?;
    let threads = store
        .desktop_side_threads(row)?
        .into_iter()
        .map(|thread| {
            let kind = Kind::from_id(&thread.provider);
            let account = onicode_core::accounts::Profile {
                id: accounts.selected(kind, &thread.cwd),
                root: None,
            };
            let effort = [
                "effort",
                "reasoning",
                "reasoningEffort",
                "reasoning_effort",
                "variant",
            ]
            .into_iter()
            .find_map(|key| thread.model_settings.get(key).cloned());
            SavedThread {
                thread: BtwThread {
                    id: thread.id,
                    source_turn: thread.source_turn,
                    snapshot: thread.snapshot,
                    provider: thread.provider,
                    model: thread.model,
                    cwd: thread.cwd,
                    account,
                    effort,
                    model_settings: thread.model_settings,
                    rows: thread.rows,
                    draft: String::new(),
                    status: match thread.status.as_str() {
                        "ready" => "completed",
                        "running" => "stopped",
                        status => status,
                    }
                    .into(),
                    error: thread.error,
                },
                desktop: Some(thread.original),
            }
        })
        .collect();
    Ok(SavedBtw {
        revision: 0,
        threads,
    })
}

/// The session as the store holds it, before any turn of this live run.
fn load_state(store: &Store, id: i64) -> Result<SessionState, String> {
    let summary = store
        .sessions()?
        .into_iter()
        .find(|summary| summary.id == id)
        .ok_or("Session no longer exists")?;
    let settings = store.agent_settings(id)?.unwrap_or_default();
    let queue = store.follow_ups(id)?;
    Ok(SessionState {
        first_row: 0,
        rows: store.load(id)?.rows,
        status: "Ready".into(),
        turn: None,
        approvals: Vec::new(),
        questions: Vec::new(),
        queue_rev: queue.revision,
        queue,
        model: settings.model.clone(),
        worktree: settings.worktree_cwd.clone(),
        usage_limit: settings.usage_limit.clone(),
        handoff: settings.handoff.clone(),
        settings,
        title: summary.title,
        provider_id: summary.provider_id,
        context: store.context_usage(id)?,
        background: Vec::new(),
        commands: Vec::new(),
    })
}

/// `state` with only its last `window_turns` turns of rows.
fn snapshot(state: &SessionState, window_turns: usize) -> SessionState {
    let prompts = prompt_rows(&state.rows);
    let from = match window_turns {
        0 => state.rows.len(),
        turns if turns >= prompts.len() => 0,
        turns => prompts[prompts.len() - turns],
    };
    SessionState {
        first_row: state.first_row + from,
        rows: state.rows[from..].to_vec(),
        status: state.status.clone(),
        turn: state.turn.clone(),
        approvals: state.approvals.clone(),
        questions: state.questions.clone(),
        queue_rev: state.queue_rev,
        queue: state.queue.clone(),
        settings: state.settings.clone(),
        title: state.title.clone(),
        provider_id: state.provider_id.clone(),
        model: state.model.clone(),
        worktree: state.worktree.clone(),
        context: state.context,
        usage_limit: state.usage_limit.clone(),
        background: state.background.clone(),
        commands: state.commands.clone(),
        handoff: state.handoff.clone(),
    }
}

/// Applies one queue edit, checking the revision it was made against.
fn edit_queue(queue: &mut onicode_core::follow_up::State, edit: &QueueEdit) -> Outcome {
    if queue.revision != edit.base_rev {
        return Outcome::Rejected(Rejection::StaleRev);
    }
    let id = edit.id.to_string();
    let at = queue.messages.iter().position(|message| message.id == id);
    match (&edit.edit, at) {
        (QueueChange::Add { .. }, Some(_)) => {
            Outcome::Rejected(Rejection::Denied("This message is already queued".into()))
        }
        (
            QueueChange::Add {
                text,
                attachments,
                steer,
            },
            None,
        ) => {
            queue.messages.push(Message {
                id,
                text: text.clone(),
                attachments: attachments.clone(),
                steer: *steer,
                sent_at: Some(onicode_core::time::now_ms()),
                ..Message::default()
            });
            Outcome::Applied
        }
        (_, None) => Outcome::Rejected(Rejection::Gone),
        (QueueChange::Text { text, attachments }, Some(at)) => {
            let message = &mut queue.messages[at];
            message.text.clone_from(text);
            message.attachments.clone_from(attachments);
            message.error = None;
            Outcome::Applied
        }
        (QueueChange::Move { to }, Some(at)) => {
            let message = queue.messages.remove(at);
            let to = (*to).min(queue.messages.len());
            queue.messages.insert(to, message);
            Outcome::Applied
        }
        (QueueChange::Remove, Some(at)) => {
            queue.messages.remove(at);
            Outcome::Applied
        }
    }
}
