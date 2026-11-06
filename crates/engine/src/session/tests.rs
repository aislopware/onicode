//! The hub against its reference (engine.md "Tests → Session model"). A scripted provider stands
//! in for the turn: the test publishes each `turn::Update` itself and runs the hub's work on a
//! `LocalPool`, so every interleaving is the one the script names. Whatever the hub splits,
//! numbers, queues, merges, rings and resyncs, every watcher must end up showing the
//! `SessionModel`, and a snapshot plus the events after it must be the later snapshot.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::{LocalPool, block_on};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::task::LocalSpawnExt;
use futures::{FutureExt, StreamExt};
use onicode_core::agent::{Approval, Decision, Question, Settings};
use onicode_core::context::Usage;
use onicode_core::provider::RewindError;
use onicode_core::store::Store;
use onicode_core::transcript::{Row, Transcript};
use onicode_protocol::session::{
    self, Answer, Btw, BtwAnswer, BtwThread, BtwThreads, Configure, Decide, EditLastTurn,
    EditedTurn, QueueChange, QueueEdit, ReadBtw, ReadSession, RecallLastTurn, SaveBtw, SaveQueue,
    SendNow, SessionEvent, SessionState, SteerNow, Stop, Verdict, WatchSession,
};
use onicode_protocol::{Cursor, Item, MessageId, OpId, Outcome, Rejection, SessionId};
use proptest::prelude::*;

use super::asides::{Aside, Asides, Sink};
use super::hub::{Limits, Runner, SessionHub, Spawn, TurnJob};
use super::model::SessionModel;
use super::queue::WatchQueue;
use super::reduce::apply;
use super::split::split;
use super::{Expect, Viewer};
use crate::turn::{Database, Update, locked};

type Work = Arc<Mutex<Vec<BoxFuture<'static, ()>>>>;
type Jobs = Arc<Mutex<Vec<TurnJob>>>;
type Reply<T> = Rc<RefCell<Option<Result<T, String>>>>;
type Slot = Reply<Outcome>;
type Stream = BoxStream<'static, Item<SessionState, SessionEvent>>;

/// The hub's work, held until the test runs it.
struct Queued(Work);

impl Spawn for Queued {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.lock().unwrap().push(work);
    }
}

/// A runner that hands each turn to the test.
fn scripted(jobs: &Jobs) -> Runner {
    let jobs = jobs.clone();
    Arc::new(move |job| jobs.lock().unwrap().push(job))
}

/// The provider's asides, answered by the script: every rewind binds the provider session
/// `rewound`, and every by-the-way answer is `ANSWER` in two pieces.
#[derive(Default)]
struct Scripted {
    rewinds: AtomicUsize,
    asks: AtomicUsize,
}

const ANSWER: [&str; 2] = ["The answer", " is 42."];

impl Asides for Scripted {
    fn rewind(&self, aside: Aside, _: Option<&str>) -> Result<String, RewindError> {
        assert!(aside.prompt.is_empty(), "a rewind sends no prompt");
        self.rewinds.fetch_add(1, Ordering::Relaxed);
        Ok("rewound".into())
    }

    fn ask(&self, aside: Aside, _: Arc<AtomicBool>, text: Sink<'_>) -> Result<(), String> {
        assert!(aside.settings.text_only, "an aside never runs tools");
        self.asks.fetch_add(1, Ordering::Relaxed);
        for piece in ANSWER {
            text(piece.into())?;
        }
        Ok(())
    }
}

struct Harness {
    pool: LocalPool,
    work: Work,
    jobs: Jobs,
    asides: Arc<Scripted>,
    hub: Arc<SessionHub>,
    store: Database,
    id: i64,
    session: SessionId,
}

impl Harness {
    fn new(limits: Limits) -> Self {
        Self::of("codex", limits)
    }

    /// A hub over a store holding one session of `provider`.
    fn of(provider: &str, limits: Limits) -> Self {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let id = store.create(provider, "/project").unwrap().id;
        Self::over(Arc::new(Mutex::new(store)), id, limits)
    }

    /// A hub over `store`, as after a restart.
    fn over(store: Database, id: i64, limits: Limits) -> Self {
        let work = Work::default();
        let jobs = Jobs::default();
        let asides = Arc::new(Scripted::default());
        let hub = SessionHub::build(
            store.clone(),
            Arc::new(Queued(work.clone())),
            scripted(&jobs),
            asides.clone(),
            limits,
        );
        let session = hub.session_id(id).unwrap();
        Self {
            pool: LocalPool::new(),
            work,
            jobs,
            asides,
            hub,
            store,
            id,
            session,
        }
    }

    /// Runs the hub's work until nothing more can happen.
    fn settle(&mut self) {
        loop {
            let work = std::mem::take(&mut *self.work.lock().unwrap());
            let idle = work.is_empty();
            for work in work {
                self.pool.spawner().spawn_local(work).unwrap();
            }
            self.pool.run_until_stalled();
            if idle && self.work.lock().unwrap().is_empty() {
                return;
            }
        }
    }

    /// Sends `text` as `message`; its receipt lands in the slot.
    fn send(&mut self, op: Option<OpId>, message: MessageId, text: &str) -> Slot {
        let slot = Slot::default();
        let held = slot.clone();
        let hub = self.hub.clone();
        let body = session::Send {
            session: self.session,
            message,
            text: text.into(),
            attachments: Vec::new(),
        };
        self.pool
            .spawner()
            .spawn_local(async move {
                *held.borrow_mut() = Some(hub.send(op, body).await);
            })
            .unwrap();
        self.settle();
        slot
    }

    /// Runs `call` on the test's pool until nothing more can happen; its reply lands in the
    /// slot.
    fn call<T: 'static>(
        &mut self,
        call: impl Future<Output = Result<T, String>> + 'static,
    ) -> Reply<T> {
        let slot = Reply::<T>::default();
        let held = slot.clone();
        self.pool
            .spawner()
            .spawn_local(async move {
                *held.borrow_mut() = Some(call.await);
            })
            .unwrap();
        self.settle();
        slot
    }

    /// Queues `text` in the stored queue, as a steer, under a new id.
    fn queue(&self, text: &str) -> MessageId {
        let id = MessageId::new();
        let added = block_on(self.hub.queue_edit(
            None,
            QueueEdit {
                session: self.session,
                id,
                base_rev: self.stored_queue().revision,
                edit: QueueChange::Add {
                    text: text.into(),
                    attachments: Vec::new(),
                    steer: true,
                },
            },
        ));
        assert_eq!(added, Ok(Outcome::Applied));
        id
    }

    fn stored_queue(&self) -> onicode_core::follow_up::State {
        locked(&self.store).unwrap().follow_ups(self.id).unwrap()
    }

    /// The running turn publishes `update`, saved as a provider's turn saves it; a finished
    /// update ends the turn.
    fn publish(&mut self, update: Update) {
        persist(&self.store, self.id, &update);
        let finished = update.finished;
        {
            let jobs = self.jobs.lock().unwrap();
            let job = jobs.last().expect("a running turn");
            job.updates.try_send(update).unwrap();
        }
        if finished {
            self.jobs.lock().unwrap().clear();
        }
        self.settle();
    }

    /// The host's whole state: a fresh watcher's snapshot.
    fn state(&self) -> SessionState {
        let mut stream = self
            .hub
            .watch(WatchSession {
                session: self.session,
                since: None,
                window_turns: usize::MAX,
            })
            .unwrap();
        match stream.next().now_or_never() {
            Some(Some(Item::Snapshot { state, .. })) => state,
            other => panic!("a fresh watcher starts from a snapshot, not {other:?}"),
        }
    }
}

/// What a provider's turn saves of `update`: what a reload reads back.
fn persist(store: &Database, id: i64, update: &Update) {
    let mut store = locked(store).unwrap();
    store.write_rows(id, update.start, &update.rows).unwrap();
    if let Some(title) = &update.title {
        store.title(id, title).unwrap();
    }
    if let Some(provider_id) = &update.provider_id {
        store.provider_id(id, provider_id).unwrap();
    }
    if let Some(usage) = &update.context {
        store.save_context_usage(id, usage).unwrap();
    }
}

/// A watcher that checks every item's numbering and resubscribes after `Resync`.
struct Client {
    window: usize,
    stream: Option<Stream>,
    state: Option<SessionState>,
    cursor: Option<Cursor>,
}

impl Client {
    fn join(harness: &Harness, window: usize) -> Self {
        let mut client = Self {
            window,
            stream: None,
            state: None,
            cursor: None,
        };
        client.pump(harness);
        client
    }

    /// Takes every ready item.
    fn pump(&mut self, harness: &Harness) {
        loop {
            if self.stream.is_none() {
                self.stream = Some(
                    harness
                        .hub
                        .watch(WatchSession {
                            session: harness.session,
                            since: self.cursor,
                            window_turns: self.window,
                        })
                        .unwrap(),
                );
            }
            let Some(stream) = self.stream.as_mut() else {
                return;
            };
            let Some(item) = stream.next().now_or_never() else {
                return;
            };
            match item {
                None | Some(Item::Resync) => self.stream = None,
                Some(item) => self.absorb(item),
            }
        }
    }

    fn absorb(&mut self, item: Item<SessionState, SessionEvent>) {
        match item {
            Item::Snapshot { at, state } => {
                self.cursor = Some(at);
                self.state = Some(state);
            }
            Item::Events {
                first,
                last,
                events,
            } => {
                let cursor = self.cursor.as_mut().expect("events before any snapshot");
                assert_eq!(first, cursor.seq + 1, "a gap or a repeat");
                assert!(last + 1 >= first, "events numbered backwards");
                assert!(
                    events.len() as u64 <= last + 1 - first,
                    "more events than numbers"
                );
                cursor.seq = last;
                let state = self.state.as_mut().expect("events before any snapshot");
                for event in &events {
                    apply(state, event);
                }
            }
            Item::Resync => unreachable!("handled by pump"),
        }
    }

    /// Checks what this watcher shows against the model and the host.
    fn check(&self, model: &SessionModel, full: &SessionState) {
        let Some(seen) = &self.state else {
            return;
        };
        if let Some(why) = model.differs(seen) {
            panic!("a watcher differs from the model: {why}");
        }
        let from = seen
            .first_row
            .checked_sub(full.first_row)
            .filter(|from| *from <= full.rows.len())
            .expect("a watcher's window inside the host's rows");
        let mut cut = full.clone();
        cut.first_row = seen.first_row;
        cut.rows.drain(..from);
        assert_eq!(
            serde_json::to_value(seen).unwrap(),
            serde_json::to_value(&cut).unwrap(),
            "a snapshot and the events after it are the later snapshot"
        );
    }
}

const WORDS: [&str; 4] = ["a", "bc", "def", "\u{e9}t\u{e9}"];
const STATUSES: [&str; 4] = ["Working", "Thinking", "Running a command", "Waiting"];

/// Row `at` of the scripted transcript: its role, first flag and time fixed by its place, as a
/// stored row's are.
fn row(at: usize, text: &str) -> Row {
    Row {
        role: if at.is_multiple_of(3) {
            "user"
        } else {
            "assistant"
        }
        .into(),
        text: text.into(),
        first: true,
        sent_at: Some(at as u64),
        id: None,
    }
}

fn approval(n: u8) -> Approval {
    Approval {
        id: format!("a{n}"),
        title: format!("Run command {n}"),
        detail: String::new(),
        can_remember: n.is_multiple_of(2),
    }
}

fn question(n: u8) -> Question {
    Question {
        id: format!("q{n}"),
        title: format!("Question {n}"),
        items: Vec::new(),
        auto_resolve_at: None,
    }
}

#[derive(Clone, Debug)]
struct Fields {
    title: Option<u8>,
    model: Option<u8>,
    worktree: Option<u8>,
    provider_id: Option<u8>,
    context: Option<u8>,
    approval: Option<u8>,
    resolve: Option<u8>,
    question: Option<u8>,
    answer: Option<u8>,
    turn_id: Option<u8>,
    ready: bool,
    status: u8,
}

/// One update, as seeds read against the rows it follows.
#[derive(Clone, Debug)]
struct Change {
    start: usize,
    grow: usize,
    /// Per row from `start`: keep, grow by a word, or replace.
    texts: Vec<(u8, u8)>,
    fields: Fields,
}

impl Change {
    fn update(&self, model: &SessionModel, finished: bool) -> Update {
        let len = model.rows.len();
        let start = self.start % (len + 1);
        let rows = (start..len + self.grow)
            .enumerate()
            .map(|(at, index)| {
                let before = model.rows.get(index).map_or("", |row| row.text.as_str());
                let (how, word) = self.texts.get(at).copied().unwrap_or((0, 0));
                let word = WORDS[usize::from(word) % WORDS.len()];
                let text = match how {
                    0 => before.to_owned(),
                    1 => format!("{before}{word}"),
                    _ => word.repeat(2),
                };
                row(index, &text)
            })
            .collect();
        let fields = &self.fields;
        Update {
            start,
            rows,
            title: fields.title.map(|n| format!("Title {n}")),
            model: fields.model.map(|n| format!("model-{n}")),
            worktree_cwd: fields.worktree.map(|n| format!("/worktrees/{n}")),
            provider_id: fields.provider_id.map(|n| format!("thread-{n}")),
            context: fields.context.map(|n| Usage {
                used: u64::from(n) * 1000,
                window: Some(200_000),
            }),
            approvals: fields.approval.map(approval).into_iter().collect(),
            approvals_resolved: fields
                .resolve
                .map(|n| format!("a{n}"))
                .into_iter()
                .collect(),
            questions: fields.question.map(question).into_iter().collect(),
            questions_resolved: fields.answer.map(|n| format!("q{n}")).into_iter().collect(),
            provider_turn_id: fields.turn_id.map(|n| format!("turn-{n}")),
            turn_ready: fields.ready,
            status: if finished {
                "Done".into()
            } else {
                STATUSES[usize::from(fields.status) % STATUSES.len()].into()
            },
            finished,
            ..Update::default()
        }
    }
}

fn some() -> impl Strategy<Value = Option<u8>> {
    prop::option::of(0..4u8)
}

fn change() -> impl Strategy<Value = Change> {
    let fields = (
        (some(), some(), some(), some()),
        (some(), some(), some(), some(), some()),
        (some(), any::<bool>(), 0..4u8),
    )
        .prop_map(
            |(
                (title, model, worktree, provider_id),
                (context, approval, resolve, question, answer),
                (turn_id, ready, status),
            )| Fields {
                title,
                model,
                worktree,
                provider_id,
                context,
                approval,
                resolve,
                question,
                answer,
                turn_id,
                ready,
                status,
            },
        );
    (
        any::<usize>(),
        0..3usize,
        prop::collection::vec((0..3u8, 0..4u8), 0..6),
        fields,
    )
        .prop_map(|(start, grow, texts, fields)| Change {
            start,
            grow,
            texts,
            fields,
        })
}

#[derive(Clone, Debug)]
enum Step {
    /// The running turn (started if none is) publishes an update.
    Update(Change),
    /// The running turn (started if none is) publishes its last update.
    Finish(Change),
    /// A watcher joins with this window of turns.
    Join(usize),
    Leave(usize),
    Pump(usize),
    /// A watcher drops its stream and comes back with its cursor.
    Rejoin(usize),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        5 => change().prop_map(Step::Update),
        1 => change().prop_map(Step::Finish),
        1 => (0..4usize).prop_map(|window| Step::Join(if window == 3 { usize::MAX } else { window })),
        1 => any::<usize>().prop_map(Step::Leave),
        2 => any::<usize>().prop_map(Step::Pump),
        1 => any::<usize>().prop_map(Step::Rejoin),
    ]
}

/// Runs `steps` against a hub with `limits`, checking every watcher as it catches up.
fn run_script(limits: Limits, steps: &[Step]) {
    let mut harness = Harness::new(limits);
    // Keeps the session live, so it stays the model's.
    let mut keeper = Client::join(&harness, usize::MAX);
    let mut model = SessionModel::of(keeper.state.as_ref().unwrap());
    let mut clients: Vec<Client> = Vec::new();
    let mut launch: Option<(Slot, bool)> = None;
    for step in steps {
        match step {
            Step::Update(change) | Step::Finish(change) => {
                let finished = matches!(step, Step::Finish(_));
                let (slot, first) = launch.get_or_insert_with(|| {
                    let slot = harness.send(Some(OpId::new()), MessageId::new(), "go");
                    assert!(
                        slot.borrow().is_none(),
                        "a launch's receipt waits for the turn's first update"
                    );
                    (slot, true)
                });
                let slot = slot.clone();
                let first = std::mem::take(first);
                let update = change.update(&model, finished);
                let status = update.status.clone();
                model.update(&update);
                harness.publish(update);
                if first {
                    let expected = if finished {
                        Outcome::Rejected(Rejection::Denied(status))
                    } else {
                        Outcome::Applied
                    };
                    assert_eq!(*slot.borrow(), Some(Ok(expected)));
                }
                if finished {
                    launch = None;
                }
            }
            Step::Join(window) => {
                let client = Client::join(&harness, *window);
                client.check(&model, &harness.state());
                clients.push(client);
            }
            Step::Leave(at) => {
                if !clients.is_empty() {
                    let at = at % clients.len();
                    clients.remove(at);
                }
            }
            Step::Pump(at) => {
                if !clients.is_empty() {
                    let at = at % clients.len();
                    clients[at].pump(&harness);
                    clients[at].check(&model, &harness.state());
                }
            }
            Step::Rejoin(at) => {
                if !clients.is_empty() {
                    let at = at % clients.len();
                    clients[at].stream = None;
                    clients[at].pump(&harness);
                    clients[at].check(&model, &harness.state());
                }
            }
        }
        let full = harness.state();
        if let Some(why) = model.differs(&full) {
            panic!("the host differs from the model: {why}");
        }
        keeper.pump(&harness);
        keeper.check(&model, &full);
    }
    let full = harness.state();
    for client in &mut clients {
        client.pump(&harness);
        client.check(&model, &full);
    }
}

fn drain_into(queue: &mut WatchQueue, mirror: &mut Option<SessionState>, next: &mut u64) {
    while let Some(item) = queue.drain() {
        match item {
            Item::Snapshot { state, .. } => *mirror = Some(state),
            Item::Events {
                first,
                last,
                events,
            } => {
                assert_eq!(first, *next, "a gap or a repeat");
                let held = mirror.as_mut().expect("the snapshot comes first");
                for event in &events {
                    apply(held, event);
                }
                *next = last + 1;
            }
            Item::Resync => panic!("an unbounded queue never resyncs"),
        }
    }
}

proptest! {
    #[test]
    fn split_events_build_what_the_updates_do(
        script in prop::collection::vec((change(), any::<bool>()), 1..40),
    ) {
        let mut state = SessionState::default();
        let mut model = SessionModel::of(&state);
        for (change, finished) in &script {
            let update = change.update(&model, *finished);
            model.update(&update);
            for event in split(&state, update, &BTreeMap::new()) {
                apply(&mut state, &event);
            }
            prop_assert_eq!(model.differs(&state), None);
        }
    }

    #[test]
    fn queue_merges_leave_the_fold_unchanged(
        script in prop::collection::vec((change(), any::<bool>(), any::<bool>()), 1..60),
    ) {
        let mut host = SessionState::default();
        let mut model = SessionModel::of(&host);
        let mut queue = WatchQueue::snapshot(
            Item::Snapshot {
                at: Cursor { epoch: 1, seq: 0 },
                state: host.clone(),
            },
            0,
            (usize::MAX, usize::MAX),
        );
        let mut seq = 0;
        let mut mirror = None;
        let mut next = 1;
        for (change, finished, drain) in &script {
            let update = change.update(&model, *finished);
            model.update(&update);
            let events = split(&host, update, &BTreeMap::new());
            for event in &events {
                apply(&mut host, event);
            }
            let first = seq + 1;
            seq += events.len() as u64;
            queue.push(first, events);
            if *drain {
                drain_into(&mut queue, &mut mirror, &mut next);
            }
        }
        drain_into(&mut queue, &mut mirror, &mut next);
        prop_assert_eq!(next, seq + 1);
        prop_assert_eq!(
            serde_json::to_value(mirror.unwrap()).unwrap(),
            serde_json::to_value(&host).unwrap()
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Watchers join, leave, fall behind (tiny queues drop them to `Resync`) and come back with
    /// cursors the tiny ring may no longer cover; each one, caught up, shows the model.
    #[test]
    fn every_watcher_converges_on_the_model(
        ring in 1usize..48,
        batches in 1usize..4,
        steps in prop::collection::vec(step(), 1..60),
    ) {
        run_script(
            Limits {
                ring: (ring, usize::MAX),
                queue: (batches, usize::MAX),
            },
            &steps,
        );
    }
}

#[test]
fn a_repeated_op_gets_the_first_receipt_and_no_second_turn() {
    let mut harness = Harness::new(Limits::default());
    let mut watcher = Client::join(&harness, usize::MAX);
    let op = OpId::new();
    let message = MessageId::new();
    let first = harness.send(Some(op), message, "go");
    let again = harness.send(Some(op), message, "go");
    assert_eq!(harness.jobs.lock().unwrap().len(), 1, "one turn for one op");
    harness.publish(Update {
        rows: vec![row(0, "go"), row(1, "")],
        status: "Working".into(),
        ..Update::default()
    });
    assert_eq!(*first.borrow(), Some(Ok(Outcome::Applied)));
    assert_eq!(*again.borrow(), Some(Ok(Outcome::Applied)));
    harness.publish(Update {
        start: 1,
        rows: vec![row(1, "Hello")],
        status: "Done".into(),
        finished: true,
        ..Update::default()
    });
    let late = harness.send(Some(op), message, "go");
    assert_eq!(*late.borrow(), Some(Ok(Outcome::Applied)));
    assert!(
        harness.jobs.lock().unwrap().is_empty(),
        "a repeat after the turn starts nothing"
    );
    watcher.pump(&harness);
    let state = watcher.state.as_ref().unwrap();
    assert_eq!(state.rows.iter().filter(|row| row.is_prompt()).count(), 1);
    assert_eq!(state.rows[0].id, Some(message.0));

    // The ledger outlives the hub.
    let mut restarted = Harness::over(harness.store.clone(), harness.id, Limits::default());
    let repeat = restarted.send(Some(op), message, "go");
    assert_eq!(*repeat.borrow(), Some(Ok(Outcome::Applied)));
    assert!(restarted.jobs.lock().unwrap().is_empty());
}

fn drain_viewer(stream: &mut Stream, viewer: &mut Viewer) -> bool {
    let mut finished = false;
    while let Some(Some(item)) = stream.next().now_or_never() {
        finished |= viewer.absorb(item);
    }
    finished
}

#[test]
fn an_echo_is_replaced_in_place_by_the_hosts_row() {
    let mut harness = Harness::new(Limits::default());
    let mut stream = harness
        .hub
        .watch(WatchSession {
            session: harness.session,
            since: None,
            window_turns: 1,
        })
        .unwrap();
    let mut viewer = Viewer::default();
    let message = MessageId::new();
    let op = OpId::new();
    viewer.echo(message);
    viewer.expect(op, Expect::Launch { queued: None });
    drain_viewer(&mut stream, &mut viewer);
    assert_eq!(viewer.pending_echoes(), [message]);

    let receipt = harness.send(Some(op), message, "hello");
    harness.publish(Update {
        rows: vec![row(0, "hello"), row(1, "")],
        status: "Working".into(),
        ..Update::default()
    });
    assert!(!drain_viewer(&mut stream, &mut viewer));
    assert_eq!(*receipt.borrow(), Some(Ok(Outcome::Applied)));
    assert!(viewer.pending_echoes().is_empty());
    let rows = &viewer.state().rows;
    assert_eq!(
        rows.len(),
        2,
        "the host's row replaced the echo, not joined it"
    );
    assert_eq!(rows[0].id, Some(message.0));
    let update = viewer.take().expect("a status arrived");
    assert_eq!(update.start, 0);
    assert_eq!(update.rows.len(), 2);
    assert!(update.rows.iter().all(|row| row.id.is_none()));
    assert!(viewer.take().is_none(), "nothing new since");

    harness.publish(Update {
        start: 1,
        rows: vec![row(1, "Hi there")],
        status: "Done".into(),
        finished: true,
        ..Update::default()
    });
    assert!(drain_viewer(&mut stream, &mut viewer));
    let update = viewer.take().expect("the last update");
    assert!(update.finished);
    assert_eq!(update.start, 1);
    assert_eq!(update.rows, [row(1, "Hi there")]);
    assert_eq!(viewer.state().rows.len(), 2);
}

#[test]
fn two_answers_to_one_approval_apply_once() {
    let mut harness = Harness::new(Limits::default());
    let mut watcher = Client::join(&harness, usize::MAX);
    let _launch = harness.send(None, MessageId::new(), "go");
    harness.publish(Update {
        rows: vec![row(0, "go")],
        approvals: vec![approval(1)],
        questions: vec![question(2)],
        status: "Waiting".into(),
        ..Update::default()
    });
    let session = harness.session;
    let decide = |op, answer| {
        block_on(harness.hub.decide(
            Some(op),
            Decide {
                session,
                approval: "a1".into(),
                answer,
            },
        ))
    };
    let first = OpId::new();
    assert_eq!(decide(first, Verdict::Allow), Ok(Outcome::Applied));
    assert_eq!(
        decide(OpId::new(), Verdict::Deny),
        Ok(Outcome::Rejected(Rejection::AlreadyResolved))
    );
    assert_eq!(
        decide(first, Verdict::Allow),
        Ok(Outcome::Applied),
        "a repeat gets the first receipt"
    );
    let answer = |op| {
        block_on(harness.hub.answer(
            Some(op),
            Answer {
                session,
                question: "q2".into(),
                answers: vec![vec!["Yes".into()]],
            },
        ))
    };
    assert_eq!(answer(OpId::new()), Ok(Outcome::Applied));
    assert_eq!(
        answer(OpId::new()),
        Ok(Outcome::Rejected(Rejection::AlreadyResolved))
    );
    {
        let jobs = harness.jobs.lock().unwrap();
        let controls = &jobs.last().unwrap().controls;
        let allowed = controls.try_recv().expect("the decision reached the turn");
        assert_eq!(allowed.id, "a1");
        assert!(matches!(allowed.decision, Decision::Allow));
        let answered = controls.try_recv().expect("the answer reached the turn");
        assert_eq!(answered.id, "q2");
        assert!(matches!(answered.decision, Decision::Answer(_)));
        assert!(controls.try_recv().is_err(), "each only once");
    }
    watcher.pump(&harness);
    let state = watcher.state.as_ref().unwrap();
    assert!(state.approvals.is_empty());
    assert!(state.questions.is_empty());

    assert_eq!(
        block_on(harness.hub.stop(None, Stop { session })),
        Ok(Outcome::Applied)
    );
    let jobs = harness.jobs.lock().unwrap();
    assert!(jobs.last().unwrap().cancel.load(Ordering::Relaxed));
}

#[test]
fn a_queue_edit_against_an_older_revision_is_stale() {
    let harness = Harness::new(Limits::default());
    let edit = |id, base_rev, edit| {
        block_on(harness.hub.queue_edit(
            Some(OpId::new()),
            QueueEdit {
                session: harness.session,
                id,
                base_rev,
                edit,
            },
        ))
    };
    let add = |text: &str| QueueChange::Add {
        text: text.into(),
        attachments: Vec::new(),
        steer: false,
    };
    let rev = locked(&harness.store)
        .unwrap()
        .follow_ups(harness.id)
        .unwrap()
        .revision;
    let first = MessageId::new();
    assert_eq!(edit(first, rev, add("one")), Ok(Outcome::Applied));
    assert_eq!(
        edit(MessageId::new(), rev, add("two")),
        Ok(Outcome::Rejected(Rejection::StaleRev))
    );
    assert_eq!(
        edit(MessageId::new(), rev + 1, QueueChange::Remove),
        Ok(Outcome::Rejected(Rejection::Gone))
    );
    assert_eq!(
        edit(first, rev + 1, QueueChange::Remove),
        Ok(Outcome::Applied)
    );
    let queue = locked(&harness.store)
        .unwrap()
        .follow_ups(harness.id)
        .unwrap();
    assert!(queue.messages.is_empty());
    assert_eq!(queue.revision, rev + 2);
}

#[test]
fn a_cursor_the_ring_covers_resumes_with_only_what_was_missed() {
    let mut harness = Harness::new(Limits::default());
    let _keeper = Client::join(&harness, usize::MAX);
    let mut client = Client::join(&harness, usize::MAX);
    let cursor = client.cursor.unwrap();
    client.stream = None;
    let _launch = harness.send(None, MessageId::new(), "go");
    harness.publish(Update {
        rows: vec![row(0, "go")],
        status: "Working".into(),
        ..Update::default()
    });
    harness.publish(Update {
        start: 1,
        rows: vec![row(1, "Hello")],
        status: "Done".into(),
        finished: true,
        ..Update::default()
    });
    let resume = |since| {
        harness
            .hub
            .watch(WatchSession {
                session: harness.session,
                since: Some(since),
                window_turns: usize::MAX,
            })
            .unwrap()
            .next()
            .now_or_never()
    };
    let head = match resume(cursor) {
        Some(Some(Item::Events {
            first,
            last,
            events,
        })) => {
            assert_eq!(first, cursor.seq + 1);
            assert!(last >= first && !events.is_empty());
            last
        }
        other => panic!("a covered cursor resumes with events, not {other:?}"),
    };
    let caught_up = Cursor {
        epoch: cursor.epoch,
        seq: head,
    };
    match resume(caught_up) {
        Some(Some(Item::Events {
            first,
            last,
            events,
        })) => {
            assert_eq!((first, last), (head + 1, head));
            assert!(events.is_empty(), "nothing was missed");
        }
        other => panic!("a current cursor gets an empty acknowledgement, not {other:?}"),
    }
}

#[test]
fn a_cursor_from_before_the_session_left_gets_a_snapshot() {
    let mut harness = Harness::new(Limits::default());
    let mut client = Client::join(&harness, usize::MAX);
    let _launch = harness.send(None, MessageId::new(), "go");
    harness.publish(Update {
        rows: vec![row(0, "go"), row(1, "Hello")],
        status: "Done".into(),
        finished: true,
        ..Update::default()
    });
    client.pump(&harness);
    let cursor = client.cursor.unwrap();
    // The last watcher leaves an idle session: it goes, and comes back under a new epoch.
    client.stream = None;
    let mut back = harness
        .hub
        .watch(WatchSession {
            session: harness.session,
            since: Some(cursor),
            window_turns: usize::MAX,
        })
        .unwrap();
    match back.next().now_or_never() {
        Some(Some(Item::Snapshot { at, state })) => {
            assert_ne!(at.epoch, cursor.epoch);
            assert_eq!(state.rows, [row(0, "go"), row(1, "Hello")]);
        }
        other => panic!("a stale epoch gets a snapshot, not {other:?}"),
    }
}

/// Spawns the hub's work on the test's tokio runtime.
struct Tokio(tokio::runtime::Handle);

impl Spawn for Tokio {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        drop(self.0.spawn(work));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_service_serves_a_turn_over_rpc() {
    use onicode_protocol::{Call, Routed, Watch};

    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let id = store.create("codex", "/project").unwrap().id;
    let jobs = Jobs::default();
    let hub = SessionHub::with_runner(
        Arc::new(Mutex::new(store)),
        Arc::new(Tokio(tokio::runtime::Handle::current())),
        scripted(&jobs),
    );
    let session = hub.session_id(id).unwrap();
    let client = Arc::new(onicode_rpc::memory_client(Arc::new(
        super::SessionService::new(hub),
    )));
    let routed = |op, body| {
        serde_json::to_value(Routed {
            engine: None,
            op,
            body,
        })
        .unwrap()
    };
    let mut items = client
        .subscribe_scoped(
            WatchSession::NAME,
            serde_json::to_value(Routed {
                engine: None,
                op: None,
                body: WatchSession {
                    session,
                    since: None,
                    window_turns: 1,
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let next = async |items: &mut onicode_rpc::RpcSubscription| {
        serde_json::from_value::<Item<SessionState, SessionEvent>>(items.recv().await.unwrap())
            .unwrap()
    };
    let Item::Snapshot { state, .. } = next(&mut items).await else {
        panic!("a watch starts from a snapshot");
    };
    assert!(state.rows.is_empty());

    let message = MessageId::new();
    let sent = tokio::spawn({
        let client = client.clone();
        let params = routed(
            Some(OpId::new()),
            session::Send {
                session,
                message,
                text: "go".into(),
                attachments: Vec::new(),
            },
        );
        async move { client.call(session::Send::NAME, params).await }
    });
    let mut waited = 0;
    while jobs.lock().unwrap().is_empty() {
        waited += 1;
        assert!(waited < 10_000, "the turn never started");
        tokio::task::yield_now().await;
    }
    {
        let jobs = jobs.lock().unwrap();
        let job = &jobs[0];
        job.updates
            .try_send(Update {
                rows: vec![row(0, "go"), row(1, "")],
                status: "Working".into(),
                ..Update::default()
            })
            .unwrap();
        job.updates
            .try_send(Update {
                start: 1,
                rows: vec![row(1, "Hello")],
                status: "Done".into(),
                finished: true,
                ..Update::default()
            })
            .unwrap();
    }
    jobs.lock().unwrap().clear();
    let receipt: Outcome = serde_json::from_value(sent.await.unwrap().unwrap()).unwrap();
    assert_eq!(receipt, Outcome::Applied);

    let mut seen = state;
    loop {
        let Item::Events { events, .. } = next(&mut items).await else {
            panic!("a current watcher gets events");
        };
        let mut finished = false;
        for event in &events {
            apply(&mut seen, event);
            finished |= matches!(
                event,
                SessionEvent::Status { turn: Some(turn), .. } if turn.finished
            );
        }
        if finished {
            break;
        }
    }
    assert_eq!(seen.rows.len(), 2);
    assert_eq!(seen.rows[0].id, Some(message.0));
    assert_eq!(seen.rows[1].text, "Hello");
    assert_eq!(seen.status, "Done");

    let unknown = client
        .call("session.nothing", serde_json::Value::Null)
        .await;
    assert!(unknown.is_err());
}

/// The real turn runner against a mock provider: the hub's seam is the turn's.
#[cfg(unix)]
#[test]
fn a_real_turn_runs_through_the_hub() {
    use std::os::unix::fs::PermissionsExt;

    use onicode_core::agent::TurnInfo;

    use super::{Staged, StagedTurn, ThreadSpawn};
    use crate::turn::{TurnInput, run_turn_using};

    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("mock-provider");
    std::fs::write(
        &binary,
        r#"#!/usr/bin/env python3
import json, sys
def send(value):
    print(json.dumps(value), flush=True)
for line in sys.stdin:
    value = json.loads(line)
    method = value.get('method')
    if method == 'initialize':
        send({'id': value['id'], 'result': {}})
    elif method == 'thread/start':
        send({'id': value['id'], 'result': {'thread': {'id': 'thread'}}})
    elif method == 'turn/start':
        send({'id': value['id'], 'result': {'turn': {'id': 'turn'}}})
        send({'method': 'item/agentMessage/delta', 'params': {'itemId': 'answer', 'delta': 'Hello'}})
        send({'method': 'turn/completed', 'params': {'turn': {'status': 'completed'}}})
        break
"#,
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::open_monocode(&dir.path().join("sessions.db")).unwrap();
    let summary = store.create("codex", dir.path().to_str().unwrap()).unwrap();
    let id = summary.id;
    let store: Database = Arc::new(Mutex::new(store));
    let runner: Runner = Arc::new(move |job: TurnJob| {
        let binary = binary.clone();
        run_turn_using(
            TurnInput {
                summary: job.summary,
                prompt: job.prompt,
                settings: job.settings,
            },
            job.controls,
            job.store,
            job.cancel,
            job.updates,
            move |_| Some(binary),
        );
    });
    let hub = SessionHub::with_runner(store, Arc::new(ThreadSpawn), runner);
    let session = hub.session_id(id).unwrap();
    let mut items = hub
        .watch(WatchSession {
            session,
            since: None,
            window_turns: 1,
        })
        .unwrap();
    let Some(Item::Snapshot { state, .. }) = block_on(items.next()) else {
        panic!("a watch starts from a snapshot");
    };
    let message = MessageId::new();
    hub.stage(
        message,
        Staged::Turn(Box::new(StagedTurn {
            summary,
            prompt: "Say hello".into(),
            settings: Settings {
                text_only: true,
                turn_model: Some(TurnInfo {
                    provider: "codex".into(),
                    model: "Chosen Model".into(),
                    ..TurnInfo::default()
                }),
                ..Settings::default()
            },
            cancel: Arc::new(AtomicBool::new(false)),
        })),
    );
    let receipt = block_on(hub.send(
        Some(OpId::new()),
        session::Send {
            session,
            message,
            text: "Say hello".into(),
            attachments: Vec::new(),
        },
    ));
    assert_eq!(receipt, Ok(Outcome::Applied));
    let mut seen = state;
    let status = loop {
        match block_on(items.next()) {
            Some(Item::Events { events, .. }) => {
                let mut finished = None;
                for event in &events {
                    apply(&mut seen, event);
                    if let SessionEvent::Status {
                        status,
                        turn: Some(turn),
                    } = event
                        && turn.finished
                    {
                        finished = Some(status.clone());
                    }
                }
                if let Some(status) = finished {
                    break status;
                }
            }
            other => panic!("a live watcher gets events, not {other:?}"),
        }
    };
    assert!(!status.is_empty());
    let prompt = seen
        .rows
        .iter()
        .find(|row| row.is_prompt())
        .expect("the prompt row");
    assert_eq!(prompt.id, Some(message.0));
    assert!(
        seen.rows
            .iter()
            .any(|row| row.role == "assistant" && row.text.contains("Hello")),
        "the provider's answer: {:?}",
        seen.rows
    );
}

/// Starts a turn and has it take steers.
fn running(harness: &mut Harness) {
    let _launch = harness.send(None, MessageId::new(), "go");
    harness.publish(Update {
        rows: vec![row(0, "go")],
        status: "Working".into(),
        turn_ready: true,
        ..Update::default()
    });
}

/// Every control the running turn was handed since the last look.
fn controls(harness: &Harness) -> Vec<onicode_core::agent::Control> {
    let jobs = harness.jobs.lock().unwrap();
    let job = jobs.last().expect("a running turn");
    std::iter::from_fn(|| job.controls.try_recv().ok()).collect()
}

#[test]
fn steer_now_moves_a_queued_message_into_the_turn_once() {
    let mut harness = Harness::new(Limits::default());
    let mut watcher = Client::join(&harness, usize::MAX);
    running(&mut harness);
    let id = harness.queue("also this");
    let op = OpId::new();
    let steer = |harness: &mut Harness| {
        let hub = harness.hub.clone();
        let body = SteerNow {
            session: harness.session,
            id,
        };
        harness.call(async move { hub.steer_now(Some(op), body).await })
    };
    let first = steer(&mut harness);
    let again = steer(&mut harness);
    let handed = controls(&harness);
    assert_eq!(handed.len(), 1, "one steer for one op");
    assert_eq!(handed[0].id, id.to_string());
    assert!(matches!(&handed[0].decision, Decision::Steer(message) if message.text == "also this"));
    assert!(harness.stored_queue().messages.is_empty());
    assert_eq!(*first.borrow(), None, "the receipt waits for the provider");

    harness.publish(Update {
        start: 1,
        follow_up_delivered: vec![id.to_string()],
        status: "Working".into(),
        ..Update::default()
    });
    assert_eq!(*first.borrow(), Some(Ok(Outcome::Applied)));
    assert_eq!(*again.borrow(), Some(Ok(Outcome::Applied)));
    let late = steer(&mut harness);
    assert_eq!(*late.borrow(), Some(Ok(Outcome::Applied)));
    assert!(controls(&harness).is_empty(), "a repeat steers nothing");
    watcher.pump(&harness);
    assert!(watcher.state.as_ref().unwrap().queue.messages.is_empty());

    let gone = steer_now_of(&mut harness, MessageId::new());
    assert_eq!(*gone.borrow(), Some(Ok(Outcome::Rejected(Rejection::Gone))));
}

fn steer_now_of(harness: &mut Harness, id: MessageId) -> Slot {
    let hub = harness.hub.clone();
    let body = SteerNow {
        session: harness.session,
        id,
    };
    harness.call(async move { hub.steer_now(Some(OpId::new()), body).await })
}

#[test]
fn a_refused_steer_now_returns_to_the_queue_paused_on_the_error() {
    let mut harness = Harness::new(Limits::default());
    running(&mut harness);
    let kept = harness.queue("later");
    let id = harness.queue("now");
    let steered = steer_now_of(&mut harness, id);
    assert_eq!(controls(&harness).len(), 1);
    harness.publish(Update {
        start: 1,
        follow_up_errors: vec![(id.to_string(), "Busy".into())],
        status: "Working".into(),
        ..Update::default()
    });
    assert_eq!(
        *steered.borrow(),
        Some(Ok(Outcome::Rejected(Rejection::Denied("Busy".into()))))
    );
    let queue = harness.stored_queue();
    assert!(queue.paused);
    let ids: Vec<_> = queue.messages.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, [id.to_string(), kept.to_string()], "back at the head");
    assert_eq!(queue.messages[0].error.as_deref(), Some("Busy"));
    assert_eq!(harness.state().queue.messages.len(), 2);
}

#[test]
fn send_now_stops_the_turn_and_sends_the_message_when_it_ends() {
    let mut harness = Harness::new(Limits::default());
    let mut watcher = Client::join(&harness, usize::MAX);
    running(&mut harness);
    let id = harness.queue("next");
    let op = OpId::new();
    let send_now = |harness: &mut Harness| {
        let hub = harness.hub.clone();
        let body = SendNow {
            session: harness.session,
            id,
        };
        harness.call(async move { hub.send_now(Some(op), body).await })
    };
    let first = send_now(&mut harness);
    let again = send_now(&mut harness);
    {
        let jobs = harness.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(
            jobs[0].cancel.load(Ordering::Relaxed),
            "the running turn stops"
        );
    }
    assert!(harness.stored_queue().messages.is_empty());
    let other = harness.queue("other");
    let refused = {
        let hub = harness.hub.clone();
        let body = SendNow {
            session: harness.session,
            id: other,
        };
        harness.call(async move { hub.send_now(Some(OpId::new()), body).await })
    };
    assert!(matches!(
        *refused.borrow(),
        Some(Ok(Outcome::Rejected(Rejection::Denied(_))))
    ));
    assert_eq!(
        harness.stored_queue().messages.len(),
        1,
        "a refusal keeps it"
    );

    harness.publish(Update {
        start: 1,
        status: "Stopped".into(),
        finished: true,
        ..Update::default()
    });
    {
        let jobs = harness.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1, "the message follows as the next turn");
        assert_eq!(jobs[0].prompt, "next");
        assert_eq!(jobs[0].settings.mono_message_id, Some(id.to_string()));
    }
    assert_eq!(*first.borrow(), None);
    harness.publish(Update {
        start: 1,
        rows: vec![row(1, "")],
        delivered: Some(id.to_string()),
        status: "Working".into(),
        ..Update::default()
    });
    assert_eq!(*first.borrow(), Some(Ok(Outcome::Applied)));
    assert_eq!(*again.borrow(), Some(Ok(Outcome::Applied)));
    harness.publish(Update {
        start: 1,
        rows: vec![row(1, "Done")],
        status: "Done".into(),
        finished: true,
        ..Update::default()
    });
    let late = send_now(&mut harness);
    assert_eq!(*late.borrow(), Some(Ok(Outcome::Applied)));
    assert!(
        harness.jobs.lock().unwrap().is_empty(),
        "a repeat sends nothing"
    );
    watcher.pump(&harness);
    let queue = &watcher.state.as_ref().unwrap().queue;
    assert_eq!(queue.messages.len(), 1);
    assert_eq!(queue.messages[0].id, other.to_string());
}

#[test]
fn send_now_with_no_turn_running_sends_at_once() {
    let mut harness = Harness::new(Limits::default());
    let id = harness.queue("hello");
    let hub = harness.hub.clone();
    let body = SendNow {
        session: harness.session,
        id,
    };
    let sent = harness.call(async move { hub.send_now(None, body).await });
    {
        let jobs = harness.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].prompt, "hello");
    }
    harness.publish(Update {
        rows: vec![row(0, "hello")],
        delivered: Some(id.to_string()),
        status: "Working".into(),
        ..Update::default()
    });
    assert_eq!(*sent.borrow(), Some(Ok(Outcome::Applied)));
    let state = harness.state();
    assert_eq!(
        state.rows[0].id,
        Some(id.0),
        "the prompt keeps the message's id"
    );
}

/// A `pi` session whose last turn the store holds: a prompt and its answer.
fn answered() -> Harness {
    let harness = Harness::of("pi", Limits::default());
    let mut transcript = Transcript::default();
    transcript.begin("user");
    transcript.append("Original");
    transcript.begin("assistant");
    transcript.append("Answer");
    let mut store = locked(&harness.store).unwrap();
    store.provider_id(harness.id, "thread").unwrap();
    store.write_rows(harness.id, 0, &transcript.rows).unwrap();
    drop(store);
    harness
}

fn edit(harness: &mut Harness, op: OpId) -> Reply<EditedTurn> {
    let hub = harness.hub.clone();
    let body = EditLastTurn {
        session: harness.session,
        attachments: Vec::new(),
        model: None,
    };
    harness.call(async move { hub.edit_last_turn(Some(op), body).await })
}

#[test]
fn an_edit_rewinds_the_provider_once_then_drops_the_turn() {
    let mut harness = answered();
    let mut watcher = Client::join(&harness, usize::MAX);
    let recalled = harness
        .hub
        .recall_last_turn(&RecallLastTurn {
            session: harness.session,
            turn: None,
        })
        .unwrap();
    assert_eq!((recalled.start, recalled.prompt.as_str()), (0, "Original"));
    assert_eq!(recalled.provider_id, "thread");
    assert_eq!(recalled.rows.len(), 2);

    let op = OpId::new();
    let edited = edit(&mut harness, op);
    let applied = EditedTurn {
        outcome: Outcome::Applied,
        provider_id: Some("rewound".into()),
    };
    assert_eq!(*edited.borrow(), Some(Ok(applied.clone())));
    assert_eq!(harness.asides.rewinds.load(Ordering::Relaxed), 1);
    assert!(
        locked(&harness.store)
            .unwrap()
            .load(harness.id)
            .unwrap()
            .rows
            .is_empty()
    );
    watcher.pump(&harness);
    let state = watcher.state.as_ref().unwrap();
    assert!(state.rows.is_empty(), "watchers lose the turn too");
    assert_eq!(state.provider_id.as_deref(), Some("rewound"));

    let again = edit(&mut harness, op);
    assert_eq!(
        *again.borrow(),
        Some(Ok(applied)),
        "a repeat gets the first reply"
    );
    assert_eq!(
        harness.asides.rewinds.load(Ordering::Relaxed),
        1,
        "and rewinds nothing"
    );
    let unrecalled = edit(&mut harness, OpId::new());
    assert_eq!(
        *unrecalled.borrow(),
        Some(Ok(EditedTurn {
            outcome: Outcome::Rejected(Rejection::Gone),
            provider_id: None,
        }))
    );
}

#[test]
fn an_edit_of_a_changed_transcript_rewinds_nothing() {
    let mut harness = answered();
    harness
        .hub
        .recall_last_turn(&RecallLastTurn {
            session: harness.session,
            turn: None,
        })
        .unwrap();
    locked(&harness.store)
        .unwrap()
        .write_rows(harness.id, 2, &[row(2, "More")])
        .unwrap();
    let edited = edit(&mut harness, OpId::new());
    assert!(matches!(
        &*edited.borrow(),
        Some(Ok(EditedTurn {
            outcome: Outcome::Rejected(Rejection::Denied(_)),
            provider_id: None,
        }))
    ));
    assert_eq!(harness.asides.rewinds.load(Ordering::Relaxed), 0);
    assert_eq!(
        locked(&harness.store)
            .unwrap()
            .load(harness.id)
            .unwrap()
            .rows
            .len(),
        3
    );
}

fn question_thread() -> BtwThread {
    BtwThread {
        id: "side".into(),
        provider: "codex".into(),
        cwd: "/project".into(),
        snapshot: "user: go".into(),
        rows: vec![Row {
            role: "user".into(),
            text: "Why?".into(),
            first: true,
            sent_at: None,
            id: None,
        }],
        status: "running".into(),
        ..BtwThread::default()
    }
}

#[test]
fn a_btw_answer_streams_to_watchers_and_stays_with_its_thread() {
    let mut harness = Harness::new(Limits::default());
    let mut stream = harness
        .hub
        .watch(WatchSession {
            session: harness.session,
            since: None,
            window_turns: 0,
        })
        .unwrap();
    let op = OpId::new();
    let ask = |harness: &mut Harness| {
        let hub = harness.hub.clone();
        let body = Btw {
            session: harness.session,
            thread: question_thread(),
        };
        harness.call(async move { hub.btw(Some(op), body).await })
    };
    let answer = ask(&mut harness);
    assert_eq!(
        *answer.borrow(),
        Some(Ok(BtwAnswer {
            outcome: Outcome::Applied,
            text: Some(ANSWER.concat()),
        }))
    );
    let mut streamed = String::new();
    while let Some(Some(item)) = stream.next().now_or_never() {
        if let Item::Events { events, .. } = item {
            for event in events {
                if let SessionEvent::BtwText { thread, text } = event {
                    assert_eq!(thread, "side");
                    streamed.push_str(&text);
                }
            }
        }
    }
    assert_eq!(streamed, ANSWER.concat());

    let kept = harness
        .hub
        .read_btw(&ReadBtw {
            session: harness.session,
        })
        .unwrap();
    let thread = &kept.threads[0];
    assert_eq!(thread.status, "completed");
    assert_eq!(thread.rows.len(), 2);
    assert_eq!(thread.rows[1].text, ANSWER.concat());

    let again = ask(&mut harness);
    assert_eq!(
        *again.borrow(),
        Some(Ok(BtwAnswer {
            outcome: Outcome::Applied,
            text: None,
        })),
        "a repeat gets the first outcome"
    );
    assert_eq!(
        harness.asides.asks.load(Ordering::Relaxed),
        1,
        "and asks nothing"
    );
}

#[test]
fn btw_saves_land_only_when_newer_and_keep_nothing_else() {
    let harness = Harness::new(Limits::default());
    let save = |revision, op| {
        block_on(harness.hub.save_btw(
            Some(op),
            SaveBtw {
                session: harness.session,
                threads: BtwThreads {
                    revision,
                    threads: vec![question_thread()],
                },
            },
        ))
    };
    let op = OpId::new();
    assert_eq!(save(5, op), Ok(Outcome::Applied));
    assert_eq!(
        save(5, op),
        Ok(Outcome::Applied),
        "a repeat gets the first receipt"
    );
    assert_eq!(
        save(5, OpId::new()),
        Ok(Outcome::Rejected(Rejection::StaleRev))
    );
    assert_eq!(
        save(4, OpId::new()),
        Ok(Outcome::Rejected(Rejection::StaleRev))
    );
    let read = harness
        .hub
        .read_btw(&ReadBtw {
            session: harness.session,
        })
        .unwrap();
    assert_eq!(read.revision, 5);
    assert_eq!(read.threads, [question_thread()]);
}

#[test]
fn a_saved_queue_and_settings_read_back_and_older_settings_are_stale() {
    let mut harness = Harness::new(Limits::default());
    let mut watcher = Client::join(&harness, usize::MAX);
    let mut queue = onicode_core::follow_up::State {
        revision: 3,
        ..Default::default()
    };
    queue.messages.push(onicode_core::follow_up::Message {
        id: MessageId::new().to_string(),
        text: "queued".into(),
        ..Default::default()
    });
    let hub = harness.hub.clone();
    let body = SaveQueue {
        session: harness.session,
        queue: queue.clone(),
    };
    let saved = harness.call(async move { hub.save_queue(Some(OpId::new()), body).await });
    let saved = saved.borrow_mut().take().unwrap().unwrap();
    assert_eq!(saved.messages, queue.messages);

    let configure = |revision, model: &str| {
        block_on(harness.hub.configure(
            Some(OpId::new()),
            Configure {
                session: harness.session,
                provider: "codex".into(),
                settings: Settings {
                    revision,
                    model: Some(model.into()),
                    ..Settings::default()
                },
            },
        ))
    };
    assert_eq!(configure(2, "newer"), Ok(Outcome::Applied));
    assert_eq!(
        configure(1, "older"),
        Ok(Outcome::Rejected(Rejection::StaleRev))
    );
    let record = harness
        .hub
        .read_session(&ReadSession {
            session: harness.session,
        })
        .unwrap();
    assert_eq!(record.queue.revision, 3);
    assert_eq!(record.queue.messages, queue.messages);
    assert_eq!(
        record
            .settings
            .and_then(|settings| settings.model)
            .as_deref(),
        Some("newer")
    );
    assert_eq!(record.edit_recovery, None);
    watcher.pump(&harness);
    let state = watcher.state.as_ref().unwrap();
    assert_eq!(state.queue_rev, 3);
    assert_eq!(state.settings.model.as_deref(), Some("newer"));
}
