//! The workspace hub: what the sidebar, history and workspace views show of every session, kept
//! as one `WorkspaceState` numbered by a `Cursor {epoch, seq}`, with a ring of recent events for
//! watchers that come back and a bounded queue per watcher. Every change goes through the hub,
//! which writes the store and publishes the change under one lock, so the state never drifts from
//! the store and two racing calls apply in the order they took the lock.
//!
//! The hub also runs the workspace's schedulers on [`WorkspaceHub::tick`]: due reminders, due
//! and recovered automation runs, and retried Inbox event claims. `tick` takes the time it runs
//! at, so its tests run in virtual time. An automation run starts here, in the engine that holds
//! its project, with no viewer: the hub makes or takes its session and the session hub sends its
//! prompt (`SessionHub::start_automation`); the run records how its turn ended.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use futures::stream::BoxStream;
use onicode_core::accounts::Profile;
use onicode_core::agent::{AccessMode, Settings};
use onicode_core::automations::{Automation, AutomationRun, DueAutomationRun, Service};
use onicode_core::inbox::Item as InboxItem;
use onicode_core::session_groups::WorkItem;
use onicode_core::store::{Session, Store};
use onicode_protocol::workspace::{
    Archive, ClaimInboxEvents, CreateNote, CreateSession, Created, Delete, DeleteNote,
    GenerateTitle, MarkSeen, NameWorktree, Named, NewSession, Pin, Preview, PreviewSession,
    Project, ReminderChange, Rename, SaveGroups, SaveResponseNote, SearchResults, SearchSessions,
    SessionDefaults, SessionSummary, SetReminder, SetWorkItem, UpsertNote, WatchWorkspace,
    WorkspaceEvent, WorkspaceState,
};
use onicode_protocol::{Cursor, Item, OpId, Outcome, Rejection, SessionId};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::watch;
use uuid::Uuid;

use super::chores::{Chores, ProviderChores};
use super::queue::WorkspaceFeed;
pub(crate) use crate::feed::Limits;
use crate::feed::{self, Topic};
use crate::searches::Searches;
use crate::session::{SessionHub, Spawn};
use crate::turn::{Database, locked};

/// How long the ledger keeps a processed op (engine.md "Ids").
const OP_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Why a reminder at time 0 is refused.
pub(super) const NO_TIME: &str = "A reminder needs a time";
/// The ledger is pruned after every this many recorded ops.
const PRUNE_EVERY: u64 = 256;
/// Where Inbox items whose event claim failed wait for the next tick, across restarts.
const INBOX_RETRIES: &str = "native-automation-inbox-retries";
/// At most this many Inbox items wait for a retry; the most recently updated stay.
const MAX_RETRIES: usize = 500;

type Settled = Option<Result<String, String>>;

/// A title generation running for a session.
#[derive(Clone, Default)]
struct Titling {
    /// Set when the person renamed the session or a newer generation replaced this one: the
    /// generated title is dropped, and a pasted work item still links.
    title: Arc<AtomicBool>,
    /// Set when the session went: nothing more runs for it.
    gone: Arc<AtomicBool>,
}

impl Titling {
    fn is(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.title, &other.title)
    }
}

/// The automation scheduler's own state.
#[derive(Default)]
struct Automations {
    /// Opened on the first tick, over the store's database.
    service: Option<Service>,
    /// Runs an earlier engine left pending or running were handed out again.
    recovered: bool,
    /// Inbox items whose event claim must be retried, by key; read from the store once.
    retries: Option<BTreeMap<String, InboxItem>>,
}

struct Inner {
    loaded: bool,
    /// The workspace, its numbering, ring and watchers.
    topic: Topic<WorkspaceFeed>,
    /// Mutating calls still running, by op: a repeat waits for the first one's reply.
    inflight: HashMap<OpId, watch::Receiver<Settled>>,
    recorded: u64,
    /// Sessions with a turn running, and with a turn finished since a viewer showed them.
    busy: HashSet<i64>,
    unread: HashSet<i64>,
    /// Title generations running, by session: renaming the session drops its generated title,
    /// and deleting it stops the generation (monocode's conversation stopped both when it went).
    titles: HashMap<i64, Titling>,
    /// Worktree branch namings running, by session: deleting the session stops its own.
    namings: HashMap<i64, Arc<AtomicBool>>,
    automations: Automations,
    /// Sessions this run created (`CreateSession`) that no turn has started in yet: closed by
    /// their last viewer with nothing in them, they go.
    unprompted: HashSet<i64>,
}

pub struct WorkspaceHub {
    store: Database,
    spawn: Arc<dyn Spawn>,
    chores: Arc<dyn Chores>,
    /// When this engine run started (Unix ms): automation runs created before it and never
    /// finished are handed out again.
    started: u64,
    inner: Mutex<Inner>,
    /// The session searches running, by search id.
    searches: Searches,
    /// Sends an automation run's prompt; attached once the engine is built. Weak: the session
    /// hub holds this hub.
    session: OnceLock<Weak<SessionHub>>,
}

/// An automation run's session, made or taken and ready for its prompt.
pub(crate) struct Prepared {
    pub(crate) session: SessionId,
    pub(crate) prompt: String,
}

/// What a mutating call did: its reply, the outcome its receipt carries, and its events.
type Applied<R> = (R, Outcome, Vec<WorkspaceEvent>);

fn rejected<R>(reply: R, rejection: Rejection) -> Applied<R> {
    (reply, Outcome::Rejected(rejection), Vec::new())
}

fn ms(now: u64) -> i64 {
    i64::try_from(now).unwrap_or(i64::MAX)
}

impl WorkspaceHub {
    /// A hub over `store` whose background chores run on `spawn` with the real providers.
    pub fn new(store: Database, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::with_chores(store, spawn, Arc::new(ProviderChores))
    }

    /// A hub whose chores are `chores`: tests answer them without a provider.
    pub fn with_chores(
        store: Database,
        spawn: Arc<dyn Spawn>,
        chores: Arc<dyn Chores>,
    ) -> Arc<Self> {
        let now = onicode_core::time::now_ms();
        Self::build(store, spawn, chores, Limits::default(), now)
    }

    pub(crate) fn build(
        store: Database,
        spawn: Arc<dyn Spawn>,
        chores: Arc<dyn Chores>,
        limits: Limits,
        started: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            spawn,
            chores,
            started,
            inner: Mutex::new(Inner {
                loaded: false,
                // This run's epoch: a cursor from an earlier run never matches.
                topic: Topic::new(
                    started.wrapping_shl(16) | 1,
                    WorkspaceState::default(),
                    limits,
                ),
                inflight: HashMap::new(),
                recorded: 0,
                busy: HashSet::new(),
                unread: HashSet::new(),
                titles: HashMap::new(),
                namings: HashMap::new(),
                automations: Automations::default(),
                unprompted: HashSet::new(),
            }),
            searches: Searches::default(),
            session: OnceLock::new(),
        })
    }

    /// Starts automation runs through `session`, which sends their prompts. Lock order: the
    /// session hub's state, then this hub's; this hub calls it only with its own lock released.
    pub fn attach_session(&self, session: &Arc<SessionHub>) {
        let _ = self.session.set(Arc::downgrade(session));
    }

    /// Starts each claimed run in this engine, with no lock held. Without a session hub (a
    /// workspace on its own, in a test) the runs stay pending, and a later engine run hands them
    /// out again.
    pub(crate) fn start_runs(&self, due: Vec<DueAutomationRun>) {
        let Some(session) = self.session.get().and_then(Weak::upgrade) else {
            return;
        };
        for due in due {
            session.start_automation(due);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads the workspace from the store the first time anything needs it.
    fn ready(&self, inner: &mut Inner) -> Result<(), String> {
        if !inner.loaded {
            let state = load(&*locked(&self.store)?, &inner.busy, &inner.unread)?;
            inner.topic.load(state);
            inner.loaded = true;
        }
        Ok(())
    }

    /// The workspace as of now, and the cursor it is at.
    pub fn snapshot(&self) -> Result<(Cursor, WorkspaceState), String> {
        let mut inner = self.lock();
        self.ready(&mut inner)?;
        Ok((inner.topic.cursor(), inner.topic.state().clone()))
    }

    /// Runs a mutating call once per op: a repeat waits for the first one's reply, or gets it
    /// from the ledger. A call that failed is not recorded, so its retry runs again.
    async fn once<R, F>(&self, op: Option<OpId>, run: F) -> Result<R, String>
    where
        R: Serialize + DeserializeOwned,
        F: Future<Output = Result<R, String>>,
    {
        enum Start {
            Wait(watch::Receiver<Settled>),
            Done(String),
            Run(watch::Sender<Settled>),
        }
        let Some(op) = op else {
            return run.await;
        };
        let start = {
            let mut inner = self.lock();
            match inner.inflight.get(&op) {
                Some(waiting) => Start::Wait(waiting.clone()),
                None => match locked(&self.store)?.op_receipt(&op.to_string())? {
                    Some(receipt) => Start::Done(receipt),
                    None => {
                        let (resolve, waiting) = watch::channel(None);
                        inner.inflight.insert(op, waiting);
                        Start::Run(resolve)
                    }
                },
            }
        };
        let resolve = match start {
            Start::Wait(mut waiting) => {
                let settled = waiting
                    .wait_for(Option::is_some)
                    .await
                    .map(|settled| settled.clone())
                    .map_err(|_| "The engine stopped before this call settled".to_owned())?;
                let text = settled.ok_or("The engine stopped before this call settled")??;
                return serde_json::from_str(&text).map_err(|error| error.to_string());
            }
            Start::Done(receipt) => {
                return serde_json::from_str(&receipt).map_err(|error| error.to_string());
            }
            Start::Run(resolve) => resolve,
        };
        let result = run.await;
        let settled = match &result {
            Ok(reply) => serde_json::to_string(reply).map_err(|error| error.to_string()),
            Err(error) => Err(error.clone()),
        };
        {
            let mut inner = self.lock();
            if let Ok(text) = &settled {
                self.record(&mut inner, op, text);
            }
            inner.inflight.remove(&op);
        }
        resolve.send_replace(Some(settled));
        result
    }

    fn record(&self, inner: &mut Inner, op: OpId, reply: &str) {
        let now = onicode_core::time::now_ms();
        if let Ok(store) = locked(&self.store) {
            let _ = store.record_op(&op.to_string(), Uuid::nil(), reply, now);
            inner.recorded += 1;
            if inner.recorded.is_multiple_of(PRUNE_EVERY) {
                let _ = store.prune_ops(now.saturating_sub(OP_RETENTION_MS));
            }
        }
    }

    /// Runs `body` with the state and the store held, publishes its events and its receipt, and
    /// replies; once per op.
    async fn mutate<R, B>(&self, op: Option<OpId>, body: B) -> Result<R, String>
    where
        R: Serialize + DeserializeOwned,
        B: FnOnce(&mut Inner, &mut Store) -> Result<Applied<R>, String>,
    {
        self.once(op, async {
            let mut inner = self.lock();
            self.ready(&mut inner)?;
            let mut store = locked(&self.store)?;
            let (reply, outcome, mut events) = body(&mut inner, &mut store)?;
            drop(store);
            if let Some(op) = op {
                events.push(WorkspaceEvent::Receipt { op, outcome });
            }
            publish(&mut inner, events);
            Ok(reply)
        })
        .await
    }

    /// `CreateSession`: a session under the viewer's id, with its starting settings.
    pub async fn create_session(
        &self,
        op: Option<OpId>,
        body: CreateSession,
    ) -> Result<Created, String> {
        self.mutate(op, move |inner, store| {
            let CreateSession {
                session,
                provider,
                cwd,
                title,
                start,
            } = body;
            let mut stored = store.create(&provider, &cwd)?;
            let row = stored.id;
            let id = SessionId(store.adopt_session_uid(row, session.0)?);
            if let Some(title) = title {
                store.title(row, &title)?;
                stored.title = title;
            }
            let settings = match start {
                NewSession::Bare => None,
                NewSession::Settings(settings) => {
                    store.save_agent_settings(row, &settings)?;
                    Some(*settings)
                }
                NewSession::Defaults(defaults) => Some(start_with_defaults(store, row, defaults)?),
            };
            let summary = SessionSummary {
                id,
                key: persisted_key(store, row),
                worktree: settings.as_ref().and_then(|s| s.worktree_cwd.clone()),
                draft: false,
                automation: None,
                busy: false,
                unread: false,
                model: settings
                    .as_ref()
                    .and_then(|s| s.model.as_deref())
                    .and_then(|model| onicode_core::store::native_model(model, &provider)),
                session: stored,
            };
            inner.unprompted.insert(row);
            let mut events = vec![WorkspaceEvent::Session(Box::new(summary.clone()))];
            events.extend(new_project(inner, store, &summary.session.cwd));
            let created = Created {
                outcome: Outcome::Applied,
                session: Some(Box::new(summary)),
                settings: settings.map(Box::new),
            };
            Ok((created, Outcome::Applied, events))
        })
        .await
    }

    /// `Rename`. A rename without `expected` is the person's own: it cancels the session's title
    /// generation.
    pub async fn rename(&self, op: Option<OpId>, body: Rename) -> Result<Outcome, String> {
        self.mutate(op, move |inner, store| {
            let Some(mut summary) = known(inner, store, body.session) else {
                return Ok(gone());
            };
            match &body.expected {
                Some(expected) => {
                    if !store.replace_title(body.session, expected, &body.title)? {
                        return Ok(rejected(
                            Outcome::Rejected(Rejection::StaleRev),
                            Rejection::StaleRev,
                        ));
                    }
                }
                None => {
                    store.title(body.session, &body.title)?;
                    if let Some(titling) = inner.titles.get(&body.session) {
                        titling.title.store(true, Ordering::Relaxed);
                    }
                }
            }
            summary.session.title = body.title;
            Ok(applied(summary))
        })
        .await
    }

    /// `Archive`.
    pub async fn archive(&self, op: Option<OpId>, body: Archive) -> Result<Outcome, String> {
        self.mutate(op, move |inner, store| {
            let Some(mut summary) = known(inner, store, body.session) else {
                return Ok(gone());
            };
            store.set_archived(body.session, body.archived)?;
            summary.session.archived = body.archived;
            Ok(applied(summary))
        })
        .await
    }

    /// `Pin`.
    pub async fn pin(&self, op: Option<OpId>, body: Pin) -> Result<Outcome, String> {
        self.mutate(op, move |inner, store| {
            let summaries: Vec<_> = body
                .sessions
                .iter()
                .filter_map(|&row| known(inner, store, row))
                .collect();
            if summaries.is_empty() {
                return Ok(gone());
            }
            let rows: Vec<i64> = summaries.iter().map(|s| s.session.id).collect();
            store.set_pinned(&rows, body.pinned)?;
            let events = summaries
                .into_iter()
                .map(|mut summary| {
                    summary.session.pinned = body.pinned;
                    WorkspaceEvent::Session(Box::new(summary))
                })
                .collect();
            Ok((Outcome::Applied, Outcome::Applied, events))
        })
        .await
    }

    /// `Delete`: the session, its settings, reminder and folder entry, and its recorded turns.
    pub async fn delete(&self, op: Option<OpId>, body: Delete) -> Result<Outcome, String> {
        self.mutate(op, move |inner, store| {
            let row = body.session;
            if known(inner, store, row).is_none() {
                return Ok(gone());
            }
            let events = remove(inner, store, row)?;
            Ok((Outcome::Applied, Outcome::Applied, events))
        })
        .await
    }

    /// Runs `f` on the automations service, opened over the store's database the first time:
    /// the Work group edits the automations this hub schedules.
    pub(crate) fn with_automations<R>(
        &self,
        f: impl FnOnce(&mut Service) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut inner = self.lock();
        let store = locked(&self.store)?;
        f(service(&mut inner.automations, &store)?)
    }

    /// Session `row` was made or changed in the store by a path that does not go through the
    /// workspace's own calls: its summary is read again, and watchers get it if it differs.
    pub fn refresh_session(&self, row: i64) {
        let mut inner = self.lock();
        if !inner.loaded {
            return;
        }
        let Ok(store) = locked(&self.store) else {
            return;
        };
        let Some(summary) = read_summary(&store, &inner, row) else {
            return;
        };
        let project = new_project(&inner, &store, &summary.session.cwd);
        drop(store);
        if inner.topic.state().sessions.contains(&summary) {
            return;
        }
        let mut events = vec![WorkspaceEvent::Session(Box::new(summary))];
        events.extend(project);
        publish(&mut inner, events);
    }

    /// The last viewer of session `row` closed it. A session this run created that no turn
    /// started in goes, once the store shows nothing in it: no rows, and no saved draft, which is
    /// the person's (another viewer of this engine may have used it meanwhile). Every viewer
    /// sees it go as `Removed`.
    pub fn session_closed(&self, row: i64) {
        let mut inner = self.lock();
        if !inner.unprompted.contains(&row) || inner.busy.contains(&row) || !inner.loaded {
            return;
        }
        let Ok(mut store) = locked(&self.store) else {
            return;
        };
        let empty = store
            .load(row)
            .is_ok_and(|transcript| transcript.rows.is_empty())
            && store
                .draft_sessions()
                .is_ok_and(|drafts| !drafts.contains(&row));
        if !empty {
            // Used after all: it stays, and no later close removes it.
            inner.unprompted.remove(&row);
            return;
        }
        let events = remove(&mut inner, &mut store, row);
        drop(store);
        if let Ok(events) = events {
            publish(&mut inner, events);
        }
    }

    /// `SetWorkItem`.
    pub async fn set_work_item(
        &self,
        op: Option<OpId>,
        body: SetWorkItem,
    ) -> Result<Outcome, String> {
        self.mutate(op, move |inner, store| {
            let Some(mut summary) = known(inner, store, body.session) else {
                return Ok(gone());
            };
            if body.if_unlinked && summary.session.work_item.is_some() {
                return Ok(rejected(
                    Outcome::Rejected(Rejection::AlreadyResolved),
                    Rejection::AlreadyResolved,
                ));
            }
            store.set_work_item(body.session, body.item.as_ref())?;
            summary.session.work_item = body.item;
            Ok(applied(summary))
        })
        .await
    }

    /// `SetReminder`. A reminder at time 0 is refused, since the store keeps only real times.
    pub async fn set_reminder(
        &self,
        op: Option<OpId>,
        body: SetReminder,
    ) -> Result<Outcome, String> {
        self.mutate(op, move |_, store| {
            let outcome = match body.change {
                // The store keeps only real times; 0 is no time at all.
                ReminderChange::At(0) | ReminderChange::Snooze { due_at: 0, .. } => {
                    Outcome::Rejected(Rejection::Denied(NO_TIME.into()))
                }
                ReminderChange::At(due_at) => {
                    store.set_reminders(&body.sessions, due_at)?;
                    Outcome::Applied
                }
                ReminderChange::Snooze { expected, due_at } => {
                    let mut moved = false;
                    for &row in &body.sessions {
                        moved |= store.snooze_reminder_if(row, expected, due_at)?;
                    }
                    if moved {
                        Outcome::Applied
                    } else {
                        Outcome::Rejected(Rejection::StaleRev)
                    }
                }
                ReminderChange::Clear { expected } => {
                    store.clear_reminders(&body.sessions, expected)?;
                    Outcome::Applied
                }
            };
            let events = vec![WorkspaceEvent::Reminders(store.reminders()?)];
            Ok((outcome.clone(), outcome, events))
        })
        .await
    }

    /// `SaveGroups`.
    pub async fn save_groups(&self, op: Option<OpId>, body: SaveGroups) -> Result<Outcome, String> {
        self.mutate(op, move |_, store| {
            store.save_groups(&body.project, &body.groups)?;
            let events = vec![WorkspaceEvent::Project(Project {
                path: body.project,
                groups: body.groups,
            })];
            Ok((Outcome::Applied, Outcome::Applied, events))
        })
        .await
    }

    /// `MarkSeen`.
    pub async fn mark_seen(&self, op: Option<OpId>, body: MarkSeen) -> Result<Outcome, String> {
        self.mutate(op, move |inner, store| {
            if !inner.unread.remove(&body.session) {
                return Ok((Outcome::Applied, Outcome::Applied, Vec::new()));
            }
            let Some(mut summary) = known(inner, store, body.session) else {
                return Ok(gone());
            };
            summary.unread = false;
            Ok(applied(summary))
        })
        .await
    }

    /// `SearchSessions`, on the store's search reader when it has one, off the caller's thread.
    pub async fn search_sessions(&self, body: SearchSessions) -> Result<SearchResults, String> {
        let reader = locked(&self.store)?.search_reader()?;
        let store = self.store.clone();
        let running = self.searches.start(&body.search);
        let cancel = running.flag();
        let (reply, answer) = futures::channel::oneshot::channel();
        self.spawn.spawn(Box::pin(async move {
            let found = match reader {
                Some(mut reader) => reader.search_sessions(&body.query, cancel),
                None => {
                    locked(&store).and_then(|mut store| store.search_sessions(&body.query, cancel))
                }
            };
            let _ = reply.send(found);
        }));
        let (hits, truncated) = answer
            .await
            .map_err(|_| "The search stopped before it finished".to_owned())??;
        if running.stopped() {
            return Ok(SearchResults::default());
        }
        Ok(SearchResults { hits, truncated })
    }

    /// `StopSearch` for the session search running under `search`.
    pub fn stop_search(&self, search: &str) {
        self.searches.stop(search);
    }

    /// `GenerateTitle`: the seed stands in now; the generated title and a pasted work item follow
    /// as `Session` events.
    pub async fn generate_title(
        self: &Arc<Self>,
        op: Option<OpId>,
        body: GenerateTitle,
    ) -> Result<Outcome, String> {
        let hub = self.clone();
        self.mutate(op, move |inner, store| {
            let Some(mut summary) = known(inner, store, body.session) else {
                return Ok(gone());
            };
            let titled = store.replace_title(body.session, &body.placeholder, &body.seed)?;
            let mut events = Vec::new();
            if titled {
                summary.session.title.clone_from(&body.seed);
                events.push(WorkspaceEvent::Session(Box::new(summary)));
            }
            let titling = Titling::default();
            if let Some(previous) = inner.titles.insert(body.session, titling.clone()) {
                previous.title.store(true, Ordering::Relaxed);
            }
            let mut model = body.model.clone();
            if model.account.id != onicode_core::accounts::DEFAULT {
                model.account.root = store.data_dir().map(Path::to_path_buf);
            }
            let work = hub.clone();
            hub.spawn.spawn(Box::pin(async move {
                work.titled(body, model, titled, titling);
            }));
            Ok((Outcome::Applied, Outcome::Applied, events))
        })
        .await
    }

    /// Runs a title's generation and work item lookup, then applies what they found. A session
    /// deleted meanwhile gets neither, and no provider or `gh` runs for it after it went.
    fn titled(
        &self,
        body: GenerateTitle,
        model: onicode_core::session_title::TextModel,
        titled: bool,
        titling: Titling,
    ) {
        let cwd = Path::new(&body.cwd);
        let generated = if titled && !titling.title.load(Ordering::Relaxed) {
            self.chores
                .title(model, cwd, &body.message, titling.title.clone())
        } else {
            None
        };
        let linked = if titling.gone.load(Ordering::Relaxed) {
            None
        } else {
            self.chores.work_item(
                &body.message,
                cwd,
                generated.as_ref().and_then(|g| g.work_item.as_ref()),
            )
        };
        let mut inner = self.lock();
        if inner
            .titles
            .get(&body.session)
            .is_some_and(|running| running.is(&titling))
        {
            inner.titles.remove(&body.session);
        }
        if titling.gone.load(Ordering::Relaxed) {
            return;
        }
        let Ok(store) = locked(&self.store) else {
            return;
        };
        let Some(mut summary) = known(&inner, &store, body.session) else {
            return;
        };
        let mut changed = false;
        if !titling.title.load(Ordering::Relaxed)
            && let Some(generated) = generated
            && store
                .replace_title(body.session, &body.seed, &generated.title)
                .unwrap_or(false)
        {
            summary.session.title = generated.title;
            changed = true;
        }
        // A manual or Inbox link always wins.
        if let Some(item) = linked
            && summary.session.work_item.is_none()
            && store.set_work_item(body.session, Some(&item)).is_ok()
        {
            summary.session.work_item = Some(item);
            changed = true;
        }
        drop(store);
        if changed {
            publish(&mut inner, vec![WorkspaceEvent::Session(Box::new(summary))]);
        }
    }

    /// `NameWorktree`: renames a new checkout's temporary branch once its name is generated,
    /// unless the session went, left the checkout or the branch moved meanwhile.
    pub async fn name_worktree(
        self: &Arc<Self>,
        op: Option<OpId>,
        body: NameWorktree,
    ) -> Result<Named, String> {
        let hub = self.clone();
        self.once(op, async move {
            let mut model = body.model.clone();
            if model.account.id != onicode_core::accounts::DEFAULT {
                model.account.root = locked(&hub.store)?.data_dir().map(Path::to_path_buf);
            }
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(previous) = hub.lock().namings.insert(body.session, cancel.clone()) {
                previous.store(true, Ordering::Relaxed);
            }
            let (reply, answer) = futures::channel::oneshot::channel();
            let work = hub.clone();
            hub.spawn.spawn(Box::pin(async move {
                let named = work.named(&body, model, &cancel);
                let mut inner = work.lock();
                if inner
                    .namings
                    .get(&body.session)
                    .is_some_and(|running| Arc::ptr_eq(running, &cancel))
                {
                    inner.namings.remove(&body.session);
                }
                drop(inner);
                let _ = reply.send(named);
            }));
            Ok(Named {
                branch: answer.await.ok().flatten(),
            })
        })
        .await
    }

    fn named(
        &self,
        body: &NameWorktree,
        model: onicode_core::session_title::TextModel,
        cancel: &Arc<AtomicBool>,
    ) -> Option<String> {
        use onicode_core::git::{self, naming};
        let path = Path::new(&body.cwd);
        let expected = naming::temporary_branch(path, body.session)?;
        if cancel.load(Ordering::Relaxed)
            || git::snapshot(path).ok()?.current.as_deref() != Some(expected.as_str())
        {
            return None;
        }
        let branch = self
            .chores
            .branch(model, path, &body.message, cancel.clone())?;
        // A deleted session, or a checkout chosen while the name was generated, gets no late
        // name.
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let retained = locked(&self.store).ok()?.session_worktrees().ok()?;
        if retained.get(&body.session).and_then(Option::as_deref) != Some(body.cwd.as_str()) {
            return None;
        }
        naming::rename(Path::new(&body.root), path, &expected, &branch)
            .ok()?
            .current
    }

    /// `PreviewSession`.
    pub fn preview_session(&self, body: &PreviewSession) -> Result<Preview, String> {
        let preview = locked(&self.store)?.session_preview(body.session)?;
        Ok(Preview {
            model: preview.model,
            worktree: preview.worktree_cwd,
        })
    }

    /// `ListNotes`.
    pub fn list_notes(&self) -> Result<Vec<onicode_core::notes::Note>, String> {
        locked(&self.store)?.list_notes()
    }

    /// `CreateNote`: a new empty note under the viewer's id.
    pub async fn create_note(
        &self,
        op: Option<OpId>,
        body: CreateNote,
    ) -> Result<onicode_core::notes::Note, String> {
        self.mutate(op, move |_, store| {
            let note = store.create_note(&body.id, body.cwd)?;
            let events = vec![WorkspaceEvent::Notes(store.list_notes()?.len())];
            Ok((note, Outcome::Applied, events))
        })
        .await
    }

    /// `UpsertNote`.
    pub async fn upsert_note(
        &self,
        op: Option<OpId>,
        body: UpsertNote,
    ) -> Result<onicode_core::notes::Note, String> {
        self.mutate(op, move |_, store| {
            let note = store.upsert_note(&body.note)?;
            let events = vec![WorkspaceEvent::Notes(store.list_notes()?.len())];
            Ok((note, Outcome::Applied, events))
        })
        .await
    }

    /// `DeleteNote`.
    pub async fn delete_note(&self, op: Option<OpId>, body: DeleteNote) -> Result<Outcome, String> {
        self.mutate(op, move |_, store| {
            store.delete_note(&body.id)?;
            let events = vec![WorkspaceEvent::Notes(store.list_notes()?.len())];
            Ok((Outcome::Applied, Outcome::Applied, events))
        })
        .await
    }

    /// `SaveResponseNote`.
    pub async fn save_response_note(
        &self,
        op: Option<OpId>,
        body: SaveResponseNote,
    ) -> Result<Outcome, String> {
        self.mutate(op, move |_, store| {
            store.save_note(&body.note)?;
            let events = vec![WorkspaceEvent::Notes(store.list_notes()?.len())];
            Ok((Outcome::Applied, Outcome::Applied, events))
        })
        .await
    }

    /// The store half of starting a claimed run: its session (the reusable one when the
    /// automation keeps one that is free, else a new one in its project) given the
    /// automation's settings on `account`, linked and filed, and the run recorded as running.
    /// A run that cannot start is recorded as failed, with why.
    pub(crate) fn prepare_automation_run(
        &self,
        due: &DueAutomationRun,
        account: Profile,
    ) -> Result<Prepared, String> {
        let mut inner = self.lock();
        self.ready(&mut inner)?;
        let store = locked(&self.store)?;
        let now = ms(onicode_core::time::now_ms());
        let reuse = reusable(&inner, &store, &due.automation);
        let prepared =
            prepare_run(&store, due, reuse, account).and_then(|(stored, settings, persisted)| {
                let row = stored.id;
                let summary = SessionSummary {
                    id: SessionId(store.session_uid(row)?),
                    key: persisted_key(&store, row),
                    worktree: settings.worktree_cwd.clone(),
                    draft: false,
                    automation: Some(due.automation.id.clone()),
                    busy: inner.busy.contains(&row),
                    unread: inner.unread.contains(&row),
                    model: settings.model.as_deref().and_then(|model| {
                        onicode_core::store::native_model(model, &stored.provider)
                    }),
                    session: stored,
                };
                service(&mut inner.automations, &store)?.update_run(
                    &due.run.id,
                    "running",
                    Some(persisted),
                    None,
                    now,
                )?;
                Ok(summary)
            });
        let summary = match prepared {
            Ok(summary) => summary,
            Err(error) => {
                // A run that cannot start is over: record why.
                let _ = service(&mut inner.automations, &store).and_then(|service| {
                    service.update_run(&due.run.id, "failed", None, Some(error.clone()), now)
                });
                return Err(error);
            }
        };
        let row = summary.session.id;
        let session = summary.id;
        let mut events = vec![WorkspaceEvent::Session(Box::new(summary))];
        events.extend(new_project(&inner, &store, &due.automation.cwd));
        if !due.automation.session_folder_id.is_empty() {
            let mut groups = store.groups(&due.automation.cwd)?;
            groups.add_to_folder(&due.automation.session_folder_id, row);
            store.save_groups(&due.automation.cwd, &groups)?;
            events.push(WorkspaceEvent::Project(Project {
                path: due.automation.cwd.clone(),
                groups,
            }));
        }
        drop(store);
        publish(&mut inner, events);
        Ok(Prepared {
            session,
            prompt: due
                .run
                .prompt
                .clone()
                .unwrap_or_else(|| due.automation.prompt.clone()),
        })
    }

    /// Records how run `run` ended: `succeeded`, `failed` or `cancelled`, with why it failed.
    pub(crate) fn finish_automation_run(&self, run: &str, status: &str, error: Option<String>) {
        let mut inner = self.lock();
        let Ok(store) = locked(&self.store) else {
            return;
        };
        let now = ms(onicode_core::time::now_ms());
        let _ = service(&mut inner.automations, &store)
            .and_then(|service| service.update_run(run, status, None, error, now));
    }

    /// `ClaimInboxEvents`: claims the event automations the items trigger now and starts their
    /// runs; the items whose claim failed wait for the next tick.
    pub async fn claim_inbox_events(
        &self,
        op: Option<OpId>,
        body: ClaimInboxEvents,
    ) -> Result<Outcome, String> {
        let mut claimed = Vec::new();
        let outcome = self
            .mutate(op, |inner, store| {
                let now = onicode_core::time::now_ms();
                let waiting = retries(&mut inner.automations, store);
                for item in body.items {
                    waiting.insert(item.key(), item);
                }
                claimed = claim_events(&mut inner.automations, store, now)?;
                Ok((Outcome::Applied, Outcome::Applied, Vec::new()))
            })
            .await;
        self.start_runs(claimed);
        outcome
    }

    /// One beat of the schedulers at `now` (Unix ms): delivers due reminders, claims due and
    /// recovered automation runs and retries Inbox event claims, then starts the runs it
    /// claimed.
    pub fn tick(&self, now: u64) -> Result<(), String> {
        let due = {
            let mut inner = self.lock();
            self.ready(&mut inner)?;
            let mut store = locked(&self.store)?;
            let mut events = Vec::new();
            if !store.take_due_reminders(now)?.is_empty() {
                events.push(WorkspaceEvent::Reminders(store.reminders()?));
            }
            let mut due = Vec::new();
            let automations = &mut inner.automations;
            if !automations.recovered {
                due.extend(service(automations, &store)?.recover(ms(self.started), ms(now))?);
                automations.recovered = true;
            }
            due.extend(service(automations, &store)?.claim_due(ms(now))?);
            due.extend(claim_events(automations, &store, now)?);
            drop(store);
            publish(&mut inner, events);
            due
        };
        self.start_runs(due);
        Ok(())
    }

    /// Whether any enabled automation waits on Inbox events: the viewer's Inbox then keeps
    /// polling in the background.
    pub fn has_event_automations(&self) -> Result<bool, String> {
        let mut inner = self.lock();
        let store = locked(&self.store)?;
        Ok(service(&mut inner.automations, &store)?
            .list()?
            .iter()
            .any(|automation| {
                automation.enabled
                    && automation.triggers.as_ref().is_some_and(|triggers| {
                        triggers.iter().any(|trigger| trigger.kind != "time")
                    })
            }))
    }

    /// A turn started in session `row`.
    pub fn turn_started(&self, row: i64) {
        let mut inner = self.lock();
        // Prompted: it is the person's now, whoever closes it.
        inner.unprompted.remove(&row);
        if !inner.busy.insert(row) || !inner.loaded {
            return;
        }
        let Ok(store) = locked(&self.store) else {
            return;
        };
        let summary = known(&inner, &store, row);
        drop(store);
        if let Some(mut summary) = summary {
            summary.busy = true;
            publish(&mut inner, vec![WorkspaceEvent::Session(Box::new(summary))]);
        }
    }

    /// A turn ended in session `row`: its summary is read again (it has messages now, a newer
    /// time, perhaps a provider id) and it is unread.
    pub fn turn_finished(&self, row: i64) {
        let mut inner = self.lock();
        inner.busy.remove(&row);
        inner.unread.insert(row);
        if !inner.loaded {
            return;
        }
        let Ok(store) = locked(&self.store) else {
            return;
        };
        let summary = read_summary(&store, &inner, row);
        drop(store);
        if let Some(summary) = summary {
            publish(&mut inner, vec![WorkspaceEvent::Session(Box::new(summary))]);
        }
    }

    /// Session `row`'s queue was saved: its summary's draft mark follows the store.
    pub fn draft_saved(&self, row: i64, draft: bool) {
        let mut inner = self.lock();
        if !inner.loaded {
            return;
        }
        let Some(mut summary) = inner
            .topic
            .state()
            .sessions
            .iter()
            .find(|known| known.session.id == row && known.draft != draft)
            .cloned()
        else {
            return;
        };
        summary.draft = draft;
        publish(&mut inner, vec![WorkspaceEvent::Session(Box::new(summary))]);
    }

    /// `WatchWorkspace`: a snapshot (or, with a `since` the ring covers, only what was missed),
    /// then every event. After `Resync` the stream ends; resubscribe with the last cursor.
    pub fn watch(
        self: &Arc<Self>,
        params: WatchWorkspace,
    ) -> Result<BoxStream<'static, Item<WorkspaceState, WorkspaceEvent>>, String> {
        let (id, notify) = {
            let mut inner = self.lock();
            self.ready(&mut inner)?;
            inner.topic.join(params.since)
        };
        let (draining, leaving) = (self.clone(), self.clone());
        Ok(feed::stream(
            notify,
            move || draining.lock().topic.drain(id),
            move || {
                leaving.lock().topic.leave(id);
            },
        ))
    }
}

/// Numbers `events`, applies them to the state and the ring, and queues them for every watcher.
fn publish(inner: &mut Inner, events: Vec<WorkspaceEvent>) {
    inner.topic.publish(events);
}

fn applied(summary: SessionSummary) -> Applied<Outcome> {
    (
        Outcome::Applied,
        Outcome::Applied,
        vec![WorkspaceEvent::Session(Box::new(summary))],
    )
}

fn gone() -> Applied<Outcome> {
    rejected(Outcome::Rejected(Rejection::Gone), Rejection::Gone)
}

/// An id for a session the ledger cannot name (one gone from the store under a racing writer):
/// derived from its row, so it stays the same, and no session watch finds it.
fn row_uid(row: i64) -> Uuid {
    Uuid::from_u64_pair(0, row.cast_unsigned())
}

/// The session's summary as the state holds it, or as the store does when the state has not
/// seen it (a session another writer created).
fn known(inner: &Inner, store: &Store, row: i64) -> Option<SessionSummary> {
    inner
        .topic
        .state()
        .sessions
        .iter()
        .find(|known| known.session.id == row)
        .cloned()
        .or_else(|| read_summary(store, inner, row))
}

/// The session's summary as the store holds it now.
fn read_summary(store: &Store, inner: &Inner, row: i64) -> Option<SessionSummary> {
    let session = store
        .sessions()
        .ok()?
        .into_iter()
        .find(|session| session.id == row)?;
    let previous = inner
        .topic
        .state()
        .sessions
        .iter()
        .find(|known| known.session.id == row);
    let settings = store.agent_settings(row).ok().flatten();
    Some(SessionSummary {
        id: SessionId(store.session_uid(row).unwrap_or_else(|_| row_uid(row))),
        key: persisted_key(store, row),
        model: settings
            .as_ref()
            .and_then(|settings| settings.model.as_deref())
            .and_then(|model| onicode_core::store::native_model(model, &session.provider)),
        worktree: settings.and_then(|settings| settings.worktree_cwd),
        draft: store
            .draft_sessions()
            .is_ok_and(|drafts| drafts.contains(&row)),
        automation: previous.and_then(|known| known.automation.clone()),
        busy: inner.busy.contains(&row),
        unread: inner.unread.contains(&row),
        session,
    })
}

/// Deletes session `row` from the store: its settings, reminder, folder entries and recorded
/// turns. Returns the events that show it gone.
fn remove(inner: &mut Inner, store: &mut Store, row: i64) -> Result<Vec<WorkspaceEvent>, String> {
    store.delete(row)?;
    // Drop the session's recorded turns and their Git refs.
    if let Some(checkpoints) = onicode_core::checkpoint::store() {
        let _ = checkpoints.exclusive(|c| c.forget(&row.to_string()));
    }
    inner.busy.remove(&row);
    inner.unread.remove(&row);
    inner.unprompted.remove(&row);
    // Its title generation and branch naming stop: no provider work runs for a deleted chat.
    if let Some(titling) = inner.titles.remove(&row) {
        titling.title.store(true, Ordering::Relaxed);
        titling.gone.store(true, Ordering::Relaxed);
    }
    if let Some(naming) = inner.namings.remove(&row) {
        naming.store(true, Ordering::Relaxed);
    }
    let mut events = vec![WorkspaceEvent::Removed(row)];
    if inner
        .topic
        .state()
        .reminders
        .iter()
        .any(|reminder| reminder.session_id == row)
    {
        events.push(WorkspaceEvent::Reminders(store.reminders()?));
    }
    // Out of its folders; a folder it leaves empty goes too.
    for project in &inner.topic.state().projects {
        if project.groups.folder_of(row).is_some() {
            let mut groups = project.groups.clone();
            groups.remove_sessions(&[row]);
            store.save_groups(&project.path, &groups)?;
            events.push(WorkspaceEvent::Project(Project {
                path: project.path.clone(),
                groups,
            }));
        }
    }
    Ok(events)
}

/// The id the store keeps the session under for other tools.
fn persisted_key(store: &Store, row: i64) -> String {
    store
        .persisted_session_id(row)
        .unwrap_or_else(|_| row.to_string())
}

/// The project at `path`, when the state does not list it yet.
fn new_project(inner: &Inner, store: &Store, path: &str) -> Option<WorkspaceEvent> {
    if inner
        .topic
        .state()
        .projects
        .iter()
        .any(|known| known.path == path)
    {
        return None;
    }
    Some(WorkspaceEvent::Project(Project {
        path: path.to_owned(),
        groups: store.groups(path).unwrap_or_default(),
    }))
}

/// The workspace as the store holds it.
fn load(
    store: &Store,
    busy: &HashSet<i64>,
    unread: &HashSet<i64>,
) -> Result<WorkspaceState, String> {
    let stored = store.sessions()?;
    let metadata = store.session_sidebar_metadata(&[]);
    let automations = store.automation_session_ids().unwrap_or_default();
    let paths: BTreeSet<String> = stored.iter().map(|session| session.cwd.clone()).collect();
    let sessions = stored
        .into_iter()
        .map(|session| {
            let row = session.id;
            SessionSummary {
                id: SessionId(store.session_uid(row).unwrap_or_else(|_| row_uid(row))),
                key: persisted_key(store, row),
                worktree: metadata.worktrees.get(&row).cloned().flatten(),
                draft: metadata.drafts.contains(&row),
                automation: automations.get(&row).cloned(),
                busy: busy.contains(&row),
                unread: unread.contains(&row),
                model: metadata
                    .details
                    .get(&row)
                    .and_then(|details| details.model.clone()),
                session,
            }
        })
        .collect();
    let projects = paths
        .into_iter()
        .map(|path| Project {
            groups: store.groups(&path).unwrap_or_default(),
            path,
        })
        .collect();
    Ok(WorkspaceState {
        sessions,
        projects,
        reminders: store.reminders()?,
        notes: store.list_notes().map_or(0, |notes| notes.len()),
    })
}

/// A new session's settings from the viewer's choices, saved when they differ from a bare
/// session's.
fn start_with_defaults(
    store: &Store,
    row: i64,
    defaults: SessionDefaults,
) -> Result<Settings, String> {
    let SessionDefaults {
        model,
        account,
        access,
        effort,
        worktree_cwd,
    } = defaults;
    let settings = Settings {
        model,
        access,
        effort,
        account,
        worktree_cwd,
        ..Settings::default()
    };
    if settings.model.is_some()
        || settings.effort.is_some()
        || settings.worktree_cwd.is_some()
        || settings.account.id != onicode_core::accounts::DEFAULT
        || settings.access != AccessMode::default()
    {
        store.save_agent_settings(row, &settings)?;
    }
    Ok(settings)
}

/// The automations service, opened over the store's database the first time.
fn service<'a>(automations: &'a mut Automations, store: &Store) -> Result<&'a mut Service, String> {
    if automations.service.is_none() {
        automations.service = Some(Service::open(store.database_path())?);
    }
    automations
        .service
        .as_mut()
        .ok_or_else(|| "Automations are unavailable".to_owned())
}

/// The Inbox items waiting for a retry, read from the store the first time.
fn retries<'a>(
    automations: &'a mut Automations,
    store: &Store,
) -> &'a mut BTreeMap<String, InboxItem> {
    automations.retries.get_or_insert_with(|| {
        store
            .setting(INBOX_RETRIES)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Vec<InboxItem>>(&raw).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|item| (item.key(), item))
            .collect()
    })
}

/// Claims the event runs the waiting Inbox items trigger; what failed waits again, kept in the
/// store first so a restart retries it too.
fn claim_events(
    automations: &mut Automations,
    store: &Store,
    now: u64,
) -> Result<Vec<DueAutomationRun>, String> {
    let waiting = retries(automations, store);
    if waiting.is_empty() {
        return Ok(Vec::new());
    }
    if waiting.len() > MAX_RETRIES {
        let mut items: Vec<_> = std::mem::take(waiting).into_values().collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_ms()));
        *waiting = items
            .into_iter()
            .take(MAX_RETRIES)
            .map(|item| (item.key(), item))
            .collect();
    }
    let items: Vec<InboxItem> = waiting.values().cloned().collect();
    store.save_setting(
        INBOX_RETRIES,
        &serde_json::to_string(&items).map_err(|error| error.to_string())?,
    )?;
    let (runs, retry) = onicode_core::automations::claim_inbox_events_retry(
        service(automations, store)?,
        &items,
        ms(now),
    )?;
    let waiting = retries(automations, store);
    *waiting = retry.into_iter().map(|item| (item.key(), item)).collect();
    let left: Vec<&InboxItem> = waiting.values().collect();
    store.save_setting(
        INBOX_RETRIES,
        &serde_json::to_string(&left).map_err(|error| error.to_string())?,
    )?;
    Ok(runs)
}

/// The session a claimed run starts in: the reusable one, or a new one in the automation's
/// project, linked, titled and given the automation's settings on `account`. Returns it, its
/// settings and its persisted id.
fn prepare_run(
    store: &Store,
    due: &DueAutomationRun,
    reuse: Option<i64>,
    account: Profile,
) -> Result<(Session, Settings, String), String> {
    let automation = &due.automation;
    if onicode_core::provider::Kind::parse(&automation.harness).is_none() {
        return Err("The automation's agent is unavailable".into());
    }
    if !Path::new(&automation.cwd).is_dir() {
        return Err("The automation's project folder is unavailable".into());
    }
    if automation.workspace_mode == "existing" && !Path::new(&automation.worktree_cwd).is_dir() {
        return Err("The automation's worktree is unavailable".into());
    }
    let mut settings = Settings::default();
    let mut summary = if let Some(row) = reuse {
        if let Some(previous) = store.agent_settings(row)? {
            settings = previous;
        }
        store
            .sessions()?
            .into_iter()
            .find(|session| session.id == row)
            .ok_or("The reusable session no longer exists")?
    } else {
        let mut summary = store.create(&automation.harness, &automation.cwd)?;
        if due.run.trigger != "event" {
            summary.title.clone_from(&automation.name);
            store.title(summary.id, &summary.title)?;
        }
        summary
    };
    store.set_automation_id(summary.id, &automation.id)?;
    if let Some(item) = automation_work_item(&due.run) {
        store.set_work_item(summary.id, Some(&item))?;
        summary.work_item = Some(item);
    }
    // The engine sends the prompt under the session's stored settings.
    let settings = run_settings(automation, settings, account);
    store.save_agent_settings(summary.id, &settings)?;
    let persisted = store.persisted_session_id(summary.id)?;
    Ok((summary, settings, persisted))
}

/// `settings` as an automation's run takes them: its agent's model and model settings, its
/// access, and its checkout (a new worktree off `HEAD`, an existing one, or the project's), on
/// `account`, the one its project uses on this engine.
fn run_settings(automation: &Automation, mut settings: Settings, account: Profile) -> Settings {
    let kind = onicode_core::provider::Kind::from_id(&automation.harness);
    let choice = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| automation.model_settings.get(*key).cloned())
    };
    settings.account = account;
    settings.model = onicode_protocol::sessions::native_model(&automation.model, kind.id());
    settings
        .model_settings
        .clone_from(&automation.model_settings);
    settings.effort = choice(&[
        "effort",
        "reasoning",
        "reasoningEffort",
        "reasoning_effort",
        "variant",
    ]);
    settings.fast = choice(&["fast"]).is_some_and(|value| value == "true");
    settings.service_tier = choice(&["serviceTier", "service_tier"]);
    settings.thinking = choice(&["thinking"]).and_then(|value| value.parse().ok());
    settings.access = AccessMode::from_id(&automation.runtime_mode);
    let worktree = automation.workspace_mode == "worktree";
    settings.new_worktree = worktree;
    settings.worktree_base = worktree.then(|| "HEAD".into());
    settings.worktree_cwd =
        (automation.workspace_mode == "existing").then(|| automation.worktree_cwd.clone());
    settings.plan = false;
    settings.prepared_prompt = None;
    settings
}

/// The session `automation` keeps for its runs, when it is free for one: the same agent, not
/// archived, no turn running, nothing in its composer or queue, and in the checkout the
/// automation runs in.
fn reusable(inner: &Inner, store: &Store, automation: &Automation) -> Option<i64> {
    if !automation.reuse_session {
        return None;
    }
    let key = automation.last_session_id.as_deref()?;
    let summary = inner
        .topic
        .state()
        .sessions
        .iter()
        .find(|summary| summary.key == key)?;
    let row = summary.session.id;
    let same = |a: &str, b: &str| {
        onicode_protocol::files::path_key(a) == onicode_protocol::files::path_key(b)
    };
    let checkout = match automation.workspace_mode.as_str() {
        "current" => summary.worktree.is_none() && same(&summary.session.cwd, &automation.cwd),
        "existing" => summary
            .worktree
            .as_deref()
            .is_some_and(|cwd| same(cwd, &automation.worktree_cwd)),
        _ => false,
    };
    let free = summary.session.provider == automation.harness
        && !summary.session.archived
        && !summary.draft
        && !inner.busy.contains(&row)
        && checkout;
    let idle = store
        .follow_ups(row)
        .is_ok_and(|queue| queue.messages.is_empty() && queue.draft.is_none());
    (free && idle).then_some(row)
}

/// The pull request or issue a GitHub event run is about.
fn automation_work_item(run: &AutomationRun) -> Option<WorkItem> {
    if run.trigger != "event" || run.event_kind.as_deref() != Some("github") {
        return None;
    }
    let mut parts = run.event_key.as_deref()?.split(':');
    if parts.next()? != "github" {
        return None;
    }
    let kind = match parts.next()? {
        "pr" => "pull",
        "issue" => "issues",
        _ => return None,
    };
    let repo = parts.next()?;
    let number = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    onicode_core::session_groups::parse_github_work_item(&format!(
        "https://github.com/{repo}/{kind}/{number}"
    ))
}
