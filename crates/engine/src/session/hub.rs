//! The hub: every live session's state, ring, watchers and running turn, and the commands that
//! drive it. A session goes live when it is watched or sent to, and leaves once nothing watches
//! it and no turn runs; it comes back under a new epoch, so no old cursor matches.
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use onicode_core::agent::{Control, Decision, Settings};
use onicode_core::follow_up::Message;
use onicode_core::store::{Session, Store};
use onicode_protocol::session::{
    self, Answer, Configure, Decide, KeepQuestionOpen, QueueChange, QueueEdit, ReadTranscript,
    SessionEvent, SessionState, Steer, Stop, TranscriptPage, Verdict, WatchSession,
};
use onicode_protocol::{Cursor, Item, MessageId, OpId, Outcome, Rejection, SessionId};
use tokio::sync::{Notify, watch};

use super::queue::{QUEUE_BATCHES, QUEUE_BYTES, WatchQueue};
use super::reduce::apply;
use super::ring::{RING_BYTES, RING_EVENTS, Ring};
use super::split;
use crate::turn::{Database, Update, locked};

/// How long the ledger keeps a processed op (engine.md "Ids").
const OP_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The ledger is pruned after every this many recorded ops, and when a hub starts.
const PRUNE_EVERY: u64 = 256;

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

/// A `Send` as the app prepared it in process: the turn context the protocol does not carry
/// (`Settings`' transport-only fields: the prepared prompt, the turn's model identity, the
/// environment, the queued message's id) and the app's stop flag. It goes when the composer,
/// the catalog and the queue move into the engine.
pub struct StagedTurn {
    pub summary: Session,
    pub prompt: String,
    pub settings: Settings,
    pub cancel: Arc<AtomicBool>,
}

/// What a viewer in process hands the hub under a `MessageId` before the call that names it.
pub enum Staged {
    Turn(Box<StagedTurn>),
    /// A `Steer`'s queued message, whole.
    Steer(Message),
}

/// The hub's bounds: the ring's and each watcher queue's, in events or batches and in bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) ring: (usize, usize),
    pub(crate) queue: (usize, usize),
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ring: (RING_EVENTS, RING_BYTES),
            queue: (QUEUE_BATCHES, QUEUE_BYTES),
        }
    }
}

/// A mutating call waiting on its receipt.
struct Pending {
    op: Option<OpId>,
    message: MessageId,
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
}

struct Watcher {
    queue: WatchQueue,
    notify: Arc<Notify>,
}

struct Live {
    id: i64,
    epoch: u64,
    seq: u64,
    state: SessionState,
    ring: Ring,
    watchers: HashMap<u64, Watcher>,
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
    staged: HashMap<MessageId, Staged>,
    ops: Ops,
    next_epoch: u64,
    next_watcher: u64,
    next_turn: u64,
}

enum Done {
    Now(Outcome),
    Later(watch::Receiver<Option<Outcome>>),
}

pub struct SessionHub {
    store: Database,
    spawn: Arc<dyn Spawn>,
    runner: Runner,
    limits: Limits,
    inner: Mutex<Inner>,
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
        Self::build(store, spawn, runner, Limits::default())
    }

    pub(crate) fn build(
        store: Database,
        spawn: Arc<dyn Spawn>,
        runner: Runner,
        limits: Limits,
    ) -> Arc<Self> {
        let now = onicode_core::time::now_ms();
        if let Ok(store) = locked(&store) {
            let _ = store.prune_ops(now.saturating_sub(OP_RETENTION_MS));
        }
        Arc::new(Self {
            store,
            spawn,
            runner,
            limits,
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                staged: HashMap::new(),
                ops: Ops::default(),
                // This run's epochs: a cursor from an earlier run never matches.
                next_epoch: now.wrapping_shl(16),
                next_watcher: 0,
                next_turn: 0,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The protocol id of the store's session `id`.
    pub fn session_id(&self, id: i64) -> Result<SessionId, String> {
        locked(&self.store)?.session_uid(id).map(SessionId)
    }

    /// Hands the hub what goes with the call naming `message`.
    pub fn stage(&self, message: MessageId, staged: Staged) {
        self.lock().staged.insert(message, staged);
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
                    epoch: *next_epoch,
                    seq: 0,
                    state,
                    ring: Ring::new(self.limits.ring.0, self.limits.ring.1),
                    watchers: HashMap::new(),
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
        split::rows(&live.state, 0, rows, &mut events);
        if fresh.title != live.state.title {
            events.push(SessionEvent::Title(fresh.title));
        }
        if fresh.provider_id != live.state.provider_id
            && let Some(id) = fresh.provider_id
        {
            events.push(SessionEvent::ProviderId(id));
        }
        if serde_json::to_value(&fresh.settings).ok()
            != serde_json::to_value(&live.state.settings).ok()
        {
            events.push(SessionEvent::Settings(Box::new(fresh.settings)));
        }
        if fresh.queue_rev != live.state.queue_rev {
            events.push(SessionEvent::Queue {
                rev: fresh.queue_rev,
                state: fresh.queue,
            });
        }
        if fresh.context != live.state.context
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
        self.once(op, |inner| self.launch(inner, op, body)).await
    }

    fn launch(
        self: &Arc<Self>,
        inner: &mut Inner,
        op: Option<OpId>,
        body: session::Send,
    ) -> Result<Done, String> {
        let staged = inner.staged.remove(&body.message);
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
        let (summary, prompt, settings, cancel) = match staged {
            Some(Staged::Turn(turn)) => {
                let StagedTurn {
                    summary,
                    prompt,
                    settings,
                    cancel,
                } = *turn;
                (summary, prompt, settings, cancel)
            }
            Some(Staged::Steer(_)) => return Err("This message was staged as a steer".into()),
            None => {
                let store = locked(&self.store)?;
                let summary = store
                    .sessions()?
                    .into_iter()
                    .find(|summary| summary.id == live.id)
                    .ok_or("Session no longer exists")?;
                let mut settings = store.agent_settings(live.id)?.unwrap_or_default();
                settings.attachments = body.attachments;
                (
                    summary,
                    body.text,
                    settings,
                    Arc::new(AtomicBool::new(false)),
                )
            }
        };
        let (resolve, waiting) = watch::channel(None);
        if let Some(op) = op {
            ops.inflight.insert(op, waiting.clone());
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
            }),
            started: false,
            steers: HashMap::new(),
        });
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
        }));
        Ok(Done::Later(waiting))
    }

    /// One update of turn `turn_id`: its events, the receipts it settles, and, when it is the
    /// last, the end of the turn.
    fn apply_update(&self, session: SessionId, turn_id: u64, mut update: Update) {
        let mut inner = self.lock();
        let Inner { sessions, ops, .. } = &mut *inner;
        let Some(live) = sessions.get_mut(&session) else {
            return;
        };
        let Some(turn) = live.turn.as_mut().filter(|turn| turn.id == turn_id) else {
            return;
        };
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
        let end = live.state.first_row + live.state.rows.len();
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
        for (id, error) in failed {
            if let Some(steer) = turn.steers.remove(&id) {
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
            for (_, steer) in turn.steers.drain() {
                let outcome = Outcome::Rejected(Rejection::Gone);
                receipts.extend(self.settle(ops, session, steer, outcome));
            }
        }
        let mut events = split::split(&live.state, update, &live.prompts);
        // The `Status` stays last: a watcher sees the receipts of a turn before its end.
        let status = events.pop();
        events.extend(receipts);
        if finished && started {
            events.extend(self.turn_finished(live.id));
        }
        events.extend(status);
        publish(live, events);
        if finished {
            live.turn = None;
            let end = live.state.first_row + live.state.rows.len();
            live.prompts.retain(|at, _| *at < end);
            if live.watchers.is_empty() {
                sessions.remove(&session);
            }
        }
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
    fn turn_closed(&self, session: SessionId, turn_id: u64) {
        let end = {
            let inner = self.lock();
            inner
                .sessions
                .get(&session)
                .filter(|live| live.turn.as_ref().is_some_and(|turn| turn.id == turn_id))
                .map(|live| live.state.first_row + live.state.rows.len())
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
            let staged = inner.staged.remove(&body.message);
            let Inner { sessions, ops, .. } = inner;
            let message = match staged {
                Some(Staged::Steer(message)) => message,
                _ => Message {
                    id: body.message.to_string(),
                    text: body.text,
                    attachments: body.attachments,
                    sent_at: Some(onicode_core::time::now_ms()),
                    steer: true,
                    ..Message::default()
                },
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
            let Inner { sessions, ops, .. } = inner;
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
                Some(held) if held.state.approvals.iter().any(|known| known.id == id) => {
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
                        && held.state.questions.iter().any(|known| known.id == id) =>
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
                Some(held) if held.state.questions.iter().any(|known| known.id == id) => {
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
                Some(held) if held.state.questions.iter().any(|q| q.id == body.question) => {
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

    /// `Configure`: replaces the session's agent settings.
    pub async fn configure(&self, op: Option<OpId>, body: Configure) -> Result<Outcome, String> {
        self.once(op, |inner| {
            let Inner { sessions, ops, .. } = inner;
            let store = locked(&self.store)?;
            let outcome = match store.session_by_uid(body.session.0)? {
                Some(id) => {
                    store.save_agent_settings(id, &body.settings)?;
                    Outcome::Applied
                }
                None => Outcome::Rejected(Rejection::Gone),
            };
            drop(store);
            let events = if outcome == Outcome::Applied {
                vec![SessionEvent::Settings(Box::new(body.settings))]
            } else {
                Vec::new()
            };
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
        let notify = Arc::new(Notify::new());
        let id = {
            let mut inner = self.lock();
            let Inner {
                sessions,
                next_epoch,
                next_watcher,
                ..
            } = &mut *inner;
            let live = self.live(sessions, next_epoch, params.session)?;
            let resumed = params
                .since
                .filter(|since| since.epoch == live.epoch)
                .and_then(|since| {
                    live.ring
                        .since(since.seq, live.seq)
                        .map(|events| (since.seq, events))
                });
            let queue = match resumed {
                Some((since, events)) => {
                    WatchQueue::resume(since, live.seq, events, self.limits.queue)
                }
                None => WatchQueue::snapshot(
                    Item::Snapshot {
                        at: Cursor {
                            epoch: live.epoch,
                            seq: live.seq,
                        },
                        state: snapshot(&live.state, params.window_turns),
                    },
                    live.seq,
                    self.limits.queue,
                ),
            };
            *next_watcher += 1;
            live.watchers.insert(
                *next_watcher,
                Watcher {
                    queue,
                    notify: notify.clone(),
                },
            );
            *next_watcher
        };
        let guard = WatchGuard {
            hub: self.clone(),
            session: params.session,
            id,
        };
        Ok(
            futures::stream::unfold(Some((guard, notify)), |held| async move {
                let (guard, notify) = held?;
                loop {
                    match guard.hub.drain(guard.session, guard.id)? {
                        Some(Item::Resync) => return Some((Item::Resync, None)),
                        Some(item) => return Some((item, Some((guard, notify)))),
                        None => notify.notified().await,
                    }
                }
            })
            .boxed(),
        )
    }

    /// The watcher's next item: `None` when it is gone, `Some(None)` when nothing is ready.
    fn drain(
        &self,
        session: SessionId,
        id: u64,
    ) -> Option<Option<Item<SessionState, SessionEvent>>> {
        let mut inner = self.lock();
        let watcher = inner.sessions.get_mut(&session)?.watchers.get_mut(&id)?;
        Some(watcher.queue.drain())
    }

    fn unwatch(&self, session: SessionId, id: u64) {
        let mut inner = self.lock();
        let idle = inner.sessions.get_mut(&session).is_some_and(|live| {
            live.watchers.remove(&id);
            live.watchers.is_empty() && live.turn.is_none()
        });
        if idle {
            inner.sessions.remove(&session);
        }
    }
}

struct WatchGuard {
    hub: Arc<SessionHub>,
    session: SessionId,
    id: u64,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.hub.unwatch(self.session, self.id);
    }
}

/// Numbers `events`, applies them to the state and the ring, and queues them for every watcher.
fn publish(live: &mut Live, events: Vec<SessionEvent>) {
    if events.is_empty() {
        return;
    }
    let first = live.seq + 1;
    for event in &events {
        live.seq += 1;
        apply(&mut live.state, event);
        live.ring.push(live.seq, event.clone());
    }
    for watcher in live.watchers.values_mut() {
        watcher.queue.push(first, events.clone());
        watcher.notify.notify_one();
    }
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
