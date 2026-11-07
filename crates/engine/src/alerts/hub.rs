//! The alerts hub: what the engine's own hubs say of each chat and terminal, turned into
//! [`Fact`]s for the [`Alerter`], and its decisions published as `WatchAlerts` on one [`Topic`].
//!
//! A chat's facts come from the workspace (its title, its marks, whether a turn runs) and, while a
//! turn runs, from a watch of its session (the approvals and questions it waits on, its status).
//! A turn ends when the workspace says so; the pump first drains the session's watch, so the
//! status the turn ended on is known. A turn that ended on "Ready" while it still waited on its
//! owner was stopped, and does not count as finished. "Seen" is the workspace's unread mark: a chat
//! on screen in a focused window of any viewer is marked seen there, so every device of the owner
//! agrees, and a chat whose mark clears stops alerting everywhere.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::stream::BoxStream;
use onicode_protocol::agent::{Approval, Question, QuestionOption};
use onicode_protocol::alerts::{
    self, Alert, AlertKind, AlertsEvent, AlertsState, Need, OnScreen, SetViewportFocus, Subject,
    WatchAlerts,
};
use onicode_protocol::provider::Kind;
use onicode_protocol::session::{SessionEvent, SessionState};
use onicode_protocol::terminal::{ProgramState, ProgramStatus, TerminalSummary};
use onicode_protocol::workspace::{SessionSummary, WorkspaceEvent, WorkspaceState};
use onicode_protocol::{Item, SessionId, TerminalId};
use tokio::sync::Notify;

use super::alerter::{Alerter, Change, Fact, Key, Phase, STALL_MS};
use super::banners::Banners;
use crate::feed::{self, Feed, Limits, Pending, Topic};

/// How long a viewport's report counts. Viewers repeat theirs every 5 s.
pub(crate) const REPORT_TTL_MS: u64 = 20_000;
/// The longest text an alert carries.
const TEXT_CHARS: usize = 240;
/// Options that approve, and options that decline, as the providers word them.
const APPROVING: [&str; 6] = [
    "Yes",
    "Allow once",
    "Allow",
    "Approve",
    "Implement",
    "Accept",
];
const DECLINING: [&str; 5] = ["No", "Deny", "Reject", "Decline", "Keep planning"];

pub(crate) struct AlertsFeed;

impl Feed for AlertsFeed {
    type Event = AlertsEvent;
    type State = AlertsState;

    fn apply(state: &mut AlertsState, event: &AlertsEvent) {
        state.apply(event);
    }

    fn weight(event: &AlertsEvent) -> usize {
        serde_json::to_vec(event).map_or(256, |bytes| bytes.len())
    }

    /// Either event replaces a pending one under the same id: each says all there is of it.
    fn absorb(pending: &mut Pending<Self>, event: AlertsEvent) -> bool {
        pending.replace_or_append(event, |next, held| event_id(next) == event_id(held))
    }
}

fn event_id(event: &AlertsEvent) -> &str {
    match event {
        AlertsEvent::Raised(alert) => &alert.id,
        AlertsEvent::Withdrawn(id) => id,
    }
}

/// The notification identifier of `key`'s alert.
pub(crate) fn alert_id(key: Key) -> String {
    match key {
        Key::Session(row) => alerts::session_alert_id(row),
        Key::Terminal(terminal) => alerts::terminal_alert_id(terminal),
    }
}

/// How a turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    None,
    Done,
    Failed,
    /// Stopped by its owner while it waited on them.
    Stopped,
}

/// A finishing `Status` as it came: its words, and whether the turn still waited on its owner.
#[derive(Clone, Debug)]
struct Finish {
    status: String,
    blocked: bool,
}

/// What the session's watch says of the running turn.
#[derive(Clone, Debug, Default)]
struct Run {
    open: bool,
    status: String,
    approvals: Vec<Approval>,
    questions: Vec<Question>,
    finish: Option<Finish>,
}

impl Run {
    fn apply(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Approval(approval) => {
                if !self.approvals.iter().any(|known| known.id == approval.id) {
                    self.approvals.push(approval.clone());
                }
            }
            SessionEvent::ApprovalResolved(id) => self.approvals.retain(|known| &known.id != id),
            SessionEvent::Question(question) => {
                if !self.questions.iter().any(|known| known.id == question.id) {
                    self.questions.push(question.clone());
                }
            }
            SessionEvent::QuestionResolved(id) => self.questions.retain(|known| &known.id != id),
            SessionEvent::Status { status, turn } => {
                self.status.clone_from(status);
                match turn {
                    Some(turn) if turn.finished => {
                        self.finish = Some(Finish {
                            status: status.clone(),
                            blocked: !self.approvals.is_empty() || !self.questions.is_empty(),
                        });
                        self.approvals.clear();
                        self.questions.clear();
                        self.open = false;
                    }
                    Some(_) => self.open = true,
                    None => {}
                }
            }
            _ => {}
        }
    }
}

/// One chat as alerts see it.
#[derive(Clone, Debug)]
struct Chat {
    session: SessionId,
    title: String,
    provider: String,
    cwd: String,
    /// Archived.
    side: bool,
    busy: bool,
    /// The workspace's unread mark, as last read.
    unread: bool,
    /// This hub asked the workspace to mark it seen, and the mark has not cleared yet.
    marking: bool,
    unseen: bool,
    run: Run,
    ended: u64,
    end: End,
    end_status: String,
    active_at: u64,
}

impl Chat {
    fn new(summary: &SessionSummary, now: u64) -> Self {
        let mut chat = Self {
            session: summary.id,
            title: String::new(),
            provider: String::new(),
            cwd: String::new(),
            side: false,
            busy: summary.busy,
            unread: summary.unread,
            marking: false,
            unseen: summary.unread,
            run: Run::default(),
            ended: 0,
            end: End::None,
            end_status: String::new(),
            active_at: now,
        };
        chat.describe(summary);
        chat
    }

    fn describe(&mut self, summary: &SessionSummary) {
        self.session = summary.id;
        self.title.clone_from(&summary.session.title);
        self.provider.clone_from(&summary.session.provider);
        self.cwd.clone_from(&summary.session.cwd);
        self.side = summary.session.archived;
    }

    fn fact(&self) -> Fact {
        let need = self
            .run
            .approvals
            .first()
            .map(|approval| format!("approval:{}", approval.id))
            .or_else(|| {
                self.run
                    .questions
                    .first()
                    .map(|question| format!("question:{}", question.id))
            });
        let phase = if need.is_some() {
            Phase::NeedsYou
        } else if self.busy || self.run.open {
            Phase::Working
        } else {
            match self.end {
                End::Done => Phase::Done,
                End::Failed => Phase::Failed,
                End::None | End::Stopped => Phase::Idle,
            }
        };
        Fact {
            phase,
            need,
            ended: self.ended,
            unseen: self.unseen,
            active_at: self.active_at,
            quiet: self.side,
        }
    }
}

/// One terminal as alerts see it.
#[derive(Clone, Debug)]
struct Term {
    cwd: String,
    status: Option<ProgramStatus>,
    ended: u64,
    unseen: bool,
    active_at: u64,
}

impl Term {
    fn fact(&self) -> Fact {
        let state = self.status.as_ref().map(|status| status.state);
        let phase = match state {
            None | Some(ProgramState::Idle) => Phase::Idle,
            Some(ProgramState::Working) => Phase::Working,
            Some(ProgramState::Blocked) => Phase::NeedsYou,
            Some(ProgramState::Done) => Phase::Done,
            Some(ProgramState::Error) => Phase::Failed,
        };
        let need = self
            .status
            .as_ref()
            .filter(|status| status.state == ProgramState::Blocked)
            .map(|status| {
                format!(
                    "{:?}|{}|{}",
                    status.kind,
                    status.title.as_deref().unwrap_or_default(),
                    status.message.as_deref().unwrap_or_default()
                )
            });
        Fact {
            phase,
            need,
            ended: self.ended,
            unseen: self.unseen,
            active_at: self.active_at,
            quiet: false,
        }
    }

    /// Takes in the program's status now.
    fn report(&mut self, status: Option<ProgramStatus>, now: u64) {
        let before = self.status.as_ref().map(|status| status.state);
        let after = status.as_ref().map(|status| status.state);
        if before != after {
            match after {
                Some(ProgramState::Done | ProgramState::Error) => {
                    self.ended += 1;
                    self.unseen = true;
                }
                Some(ProgramState::Working | ProgramState::Blocked) => self.unseen = false,
                _ => {}
            }
        }
        if self.status != status {
            self.active_at = now;
        }
        self.status = status;
    }
}

/// A viewport's last report.
#[derive(Clone, Copy, Debug)]
struct Report {
    at: u64,
    focused: bool,
    on_screen: Option<OnScreen>,
    presents: bool,
}

/// What the owner last said of the engine's own banners.
#[derive(Clone, Copy, Debug)]
struct Prefs {
    banners: bool,
    sound: bool,
}

/// What the pump must do for the workspace's news.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Wants {
    /// Turns ended: drain each session's watch (opening one if none), then
    /// [`AlertsHub::turn_ended`].
    pub(crate) ended: Vec<(i64, SessionId)>,
    /// Turns started: watch each session.
    pub(crate) watch: Vec<(i64, SessionId)>,
    /// Sessions gone: let their watches go.
    pub(crate) unwatch: Vec<i64>,
}

/// What a step leaves the pump.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Step {
    /// When to step again at the latest.
    pub(crate) next: Option<u64>,
    /// Chats to mark seen in the workspace.
    pub(crate) seen: Vec<i64>,
}

struct Inner {
    alerter: Alerter,
    chats: HashMap<i64, Chat>,
    terminals: HashMap<TerminalId, Term>,
    reports: HashMap<String, Report>,
    prefs: Prefs,
    seen: Vec<i64>,
    /// The alerts the engine posted as its own banners.
    posted: HashSet<String>,
    topic: Topic<AlertsFeed>,
}

impl Inner {
    fn observe(&mut self, row: i64, now: u64) {
        if let Some(chat) = self.chats.get(&row) {
            self.alerter.observe(Key::Session(row), chat.fact(), now);
        }
    }

    fn forget(&mut self, row: i64) {
        self.chats.remove(&row);
        self.alerter.forget(Key::Session(row));
    }

    /// A session's summary, new or changed.
    fn summary(&mut self, summary: &SessionSummary, now: u64, wants: &mut Wants) {
        let row = summary.session.id;
        if !self.chats.contains_key(&row) {
            let chat = Chat::new(summary, now);
            if chat.busy {
                wants.watch.push((row, chat.session));
            }
            self.alerter.seed(Key::Session(row), chat.fact());
            self.chats.insert(row, chat);
            return;
        }
        let Some(chat) = self.chats.get_mut(&row) else {
            return;
        };
        chat.describe(summary);
        let (was_busy, was_unread) = (chat.busy, chat.unread);
        chat.busy = summary.busy;
        chat.unread = summary.unread;
        if !summary.unread {
            chat.marking = false;
        }
        if summary.busy && !was_busy {
            chat.run = Run::default();
            chat.active_at = now;
            wants.watch.push((row, chat.session));
        }
        if was_unread && !summary.unread {
            // Seen on some device.
            chat.unseen = false;
        }
        // The workspace marks a session unread as each turn ends; a slow watcher may have had the
        // turn's start merged away, so a fresh mark ends a turn too.
        if !summary.busy && (was_busy || (summary.unread && !was_unread)) {
            wants.ended.push((row, chat.session));
        }
        self.observe(row, now);
    }

    /// Drops reports that went stale, then takes in what the fresh ones have on screen: those
    /// keys never alert, and showing a chat marks it seen.
    fn refocus(&mut self, now: u64) {
        self.reports
            .retain(|_, report| now.saturating_sub(report.at) < REPORT_TTL_MS);
        let looking: BTreeSet<Key> = self
            .reports
            .values()
            .filter(|report| report.focused)
            .filter_map(|report| report.on_screen)
            .map(|on_screen| match on_screen {
                OnScreen::Session(row) => Key::Session(row),
                OnScreen::Terminal(terminal) => Key::Terminal(terminal),
            })
            .collect();
        for key in &looking {
            match *key {
                Key::Session(row) => {
                    let Some(chat) = self.chats.get_mut(&row) else {
                        continue;
                    };
                    if chat.unread && !chat.marking {
                        chat.marking = true;
                        self.seen.push(row);
                    }
                    if chat.unseen {
                        chat.unseen = false;
                        self.observe(row, now);
                    }
                }
                Key::Terminal(terminal) => {
                    if let Some(term) = self.terminals.get_mut(&terminal)
                        && term.unseen
                    {
                        term.unseen = false;
                        let fact = term.fact();
                        self.alerter.observe(*key, fact, now);
                    }
                }
            }
        }
        self.alerter.look(looking);
    }

    fn presenting(&self) -> bool {
        self.reports.values().any(|report| report.presents)
    }

    fn alert(&self, key: Key, kind: AlertKind, now: u64) -> Option<Alert> {
        let (subject, title, body, project, need) = match key {
            Key::Session(row) => {
                let chat = self.chats.get(&row)?;
                let (need, body) = match kind {
                    AlertKind::NeedsYou => need_of(&chat.run),
                    AlertKind::Failed => (None, text_or(&chat.end_status, "Run failed")),
                    AlertKind::Finished => (
                        None,
                        format!("{} finished", Kind::from_id(&chat.provider).label()),
                    ),
                    AlertKind::Stalled => (
                        None,
                        format!("No word from the agent for {} minutes", STALL_MS / 60_000),
                    ),
                };
                (
                    Subject::Session {
                        session: chat.session,
                        row,
                    },
                    text_or(&chat.title, "New session"),
                    body,
                    Some(chat.cwd.clone()),
                    need,
                )
            }
            Key::Terminal(terminal) => {
                let term = self.terminals.get(&terminal)?;
                let status = term.status.as_ref();
                let named = status
                    .and_then(|status| status.title.as_deref())
                    .or_else(|| {
                        status
                            .and_then(|status| status.app.as_deref())
                            .map(app_name)
                    })
                    .unwrap_or_default();
                let folder = Path::new(&term.cwd)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let fallback = match kind {
                    AlertKind::NeedsYou => "Needs your input in a terminal",
                    AlertKind::Failed => "Failed in a terminal",
                    AlertKind::Finished => "Finished in a terminal",
                    AlertKind::Stalled => "Went quiet in a terminal",
                };
                let message = status
                    .and_then(|status| status.message.as_deref())
                    .unwrap_or_default();
                (
                    Subject::Terminal(terminal),
                    text_or(named, &text_or(&folder, "Terminal")),
                    text_or(message, fallback),
                    Some(term.cwd.clone()),
                    None,
                )
            }
        };
        let actions = alerts::actions_for(kind, &subject, need.as_ref());
        Some(Alert {
            id: alert_id(key),
            subject,
            kind,
            title,
            body,
            project,
            raised_at: now,
            need,
            actions,
        })
    }
}

/// The open approval or question a chat waits on, with the words its alert shows.
fn need_of(run: &Run) -> (Option<Need>, String) {
    if let Some(approval) = run.approvals.first() {
        let need = Need::Approval {
            approval: approval.id.clone(),
        };
        return (Some(need), clip(&format!("Approve: {}", approval.title)));
    }
    let Some(question) = run.questions.first() else {
        return (None, "Waiting on your input".to_owned());
    };
    let prompt = question
        .items
        .first()
        .map(|item| item.prompt.as_str())
        .filter(|prompt| !prompt.trim().is_empty())
        .unwrap_or(&question.title);
    let need = match question.items.as_slice() {
        [item] if !item.multiple => Some(Need::Question {
            question: question.id.clone(),
            approve: option_in(&item.options, &APPROVING),
            deny: option_in(&item.options, &DECLINING),
            custom: item.allow_custom,
        }),
        _ => None,
    };
    (need, text_or(prompt, "Waiting on your answer"))
}

fn option_in(options: &[QuestionOption], words: &[&str]) -> Option<String> {
    words.iter().find_map(|word| {
        options
            .iter()
            .find(|option| option.label.trim().eq_ignore_ascii_case(word))
            .map(|option| option.id.clone())
    })
}

/// What a terminal program calls itself, as people know it.
fn app_name(app: &str) -> &str {
    match app {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        other => other,
    }
}

/// `text` on one line and at most [`TEXT_CHARS`] long.
fn clip(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= TEXT_CHARS {
        return text;
    }
    let mut clipped: String = text.chars().take(TEXT_CHARS - 1).collect();
    clipped.push('…');
    clipped
}

/// `text` clipped, or `fallback` when it is blank.
fn text_or(text: &str, fallback: &str) -> String {
    let clipped = clip(text);
    if clipped.is_empty() {
        fallback.to_owned()
    } else {
        clipped
    }
}

/// The engine's alerts: fed by the pump from the engine's hubs and the viewports' reports, and
/// served as `WatchAlerts`. While no viewer presents them, it posts plain banners itself.
pub struct AlertsHub {
    inner: Mutex<Inner>,
    banners: Arc<dyn Banners>,
    /// Wakes the pump after a viewport's report.
    wake: Notify,
}

impl AlertsHub {
    /// A hub that posts its own banners through `banners`.
    #[must_use]
    pub fn new(banners: Arc<dyn Banners>) -> Arc<Self> {
        Self::with(banners, Limits::default())
    }

    pub(crate) fn with(banners: Arc<dyn Banners>, limits: Limits) -> Arc<Self> {
        let epoch = onicode_core::time::now_ms().wrapping_shl(16) | 7;
        Arc::new(Self {
            inner: Mutex::new(Inner {
                alerter: Alerter::default(),
                chats: HashMap::new(),
                terminals: HashMap::new(),
                reports: HashMap::new(),
                prefs: Prefs {
                    banners: true,
                    sound: true,
                },
                seen: Vec::new(),
                posted: HashSet::new(),
                topic: Topic::new(epoch, AlertsState::default(), limits),
            }),
            banners,
            wake: Notify::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The alerts showing now.
    #[must_use]
    pub fn current(&self) -> AlertsState {
        self.lock().topic.state().clone()
    }

    /// `WatchAlerts`: a snapshot (or, with a `since` the ring covers, only what was missed), then
    /// every event. After `Resync` the stream ends; resubscribe with the last cursor.
    pub fn watch(
        self: &Arc<Self>,
        params: &WatchAlerts,
    ) -> BoxStream<'static, Item<AlertsState, AlertsEvent>> {
        let (id, notify) = self.lock().topic.join(params.since);
        let (draining, leaving) = (self.clone(), self.clone());
        feed::stream(
            notify,
            move || draining.lock().topic.drain(id),
            move || {
                leaving.lock().topic.leave(id);
            },
        )
    }

    /// `SetViewportFocus` at `now`: the report counts for 20 s; the pump takes it in at once.
    pub fn set_focus(&self, report: SetViewportFocus, now: u64) {
        {
            let mut inner = self.lock();
            inner.prefs = Prefs {
                banners: report.banners,
                sound: report.sound,
            };
            if report.focused || report.presents {
                inner.reports.insert(
                    report.viewport,
                    Report {
                        at: now,
                        focused: report.focused,
                        on_screen: report.on_screen,
                        presents: report.presents,
                    },
                );
            } else {
                inner.reports.remove(&report.viewport);
            }
        }
        self.wake.notify_one();
    }

    /// Resolves after a viewport's report.
    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }

    /// An item of the workspace's watch at `now`.
    pub(crate) fn workspace(&self, item: Item<WorkspaceState, WorkspaceEvent>, now: u64) -> Wants {
        let mut wants = Wants::default();
        let mut inner = self.lock();
        match item {
            Item::Snapshot { state, .. } => {
                let rows: HashSet<i64> = state
                    .sessions
                    .iter()
                    .map(|summary| summary.session.id)
                    .collect();
                let gone: Vec<i64> = inner
                    .chats
                    .keys()
                    .filter(|row| !rows.contains(row))
                    .copied()
                    .collect();
                for row in gone {
                    inner.forget(row);
                    wants.unwatch.push(row);
                }
                for summary in &state.sessions {
                    inner.summary(summary, now, &mut wants);
                }
            }
            Item::Events { events, .. } => {
                for event in events {
                    match event {
                        WorkspaceEvent::Session(summary) => {
                            inner.summary(&summary, now, &mut wants);
                        }
                        WorkspaceEvent::Removed(row) => {
                            inner.forget(row);
                            wants.unwatch.push(row);
                        }
                        _ => {}
                    }
                }
            }
            Item::Resync => {}
        }
        wants
    }

    /// An item of session `row`'s watch at `now`.
    pub(crate) fn session(&self, row: i64, item: Item<SessionState, SessionEvent>, now: u64) {
        let mut inner = self.lock();
        let Some(chat) = inner.chats.get_mut(&row) else {
            return;
        };
        match item {
            Item::Snapshot { state, .. } => {
                chat.run.approvals = state.approvals;
                chat.run.questions = state.questions;
                chat.run.status = state.status;
                chat.run.open = state.turn.is_some_and(|turn| !turn.finished);
            }
            Item::Events { events, .. } => {
                for event in &events {
                    chat.run.apply(event);
                }
            }
            Item::Resync => return,
        }
        chat.active_at = now;
        inner.observe(row, now);
    }

    /// Session `row`'s turn ended at `now`, its watch drained. Whether the session runs another
    /// turn already, so its watch stays.
    pub(crate) fn turn_ended(&self, row: i64, now: u64) -> bool {
        let mut inner = self.lock();
        let Some(chat) = inner.chats.get_mut(&row) else {
            return false;
        };
        let finish = chat.run.finish.take().unwrap_or_else(|| Finish {
            status: chat.run.status.clone(),
            blocked: false,
        });
        let status = finish.status.trim();
        let failed = !status.is_empty() && !status.starts_with("Ready");
        chat.end = if failed {
            End::Failed
        } else if finish.blocked {
            End::Stopped
        } else {
            End::Done
        };
        chat.end_status = finish.status;
        chat.ended += 1;
        chat.unseen = true;
        chat.active_at = now;
        let keep = chat.busy;
        inner.observe(row, now);
        keep
    }

    /// Every terminal the engine hosts, as [`crate::terminal::TerminalHub::list`] says at `now`.
    /// A terminal not known before seeds silently from what it shows at the first look; after
    /// that (`fresh`), it is new and starts from nothing, so a program that blocks at once alerts.
    pub(crate) fn terminals(&self, list: &[TerminalSummary], fresh: bool, now: u64) {
        let mut inner = self.lock();
        let Inner {
            alerter, terminals, ..
        } = &mut *inner;
        let listed: HashSet<TerminalId> = list.iter().map(|summary| summary.terminal).collect();
        let gone: Vec<TerminalId> = terminals
            .keys()
            .filter(|terminal| !listed.contains(terminal))
            .copied()
            .collect();
        for terminal in gone {
            terminals.remove(&terminal);
            alerter.forget(Key::Terminal(terminal));
        }
        for summary in list {
            let key = Key::Terminal(summary.terminal);
            let status = if summary.exited {
                None
            } else {
                summary.status.clone()
            };
            match terminals.get_mut(&summary.terminal) {
                Some(term) => {
                    term.cwd.clone_from(&summary.cwd);
                    term.report(status, now);
                    alerter.observe(key, term.fact(), now);
                }
                None => {
                    let mut term = Term {
                        cwd: summary.cwd.clone(),
                        status: None,
                        ended: 0,
                        unseen: false,
                        active_at: now,
                    };
                    if fresh {
                        alerter.seed(key, term.fact());
                        term.report(status, now);
                        alerter.observe(key, term.fact(), now);
                    } else {
                        term.status = status;
                        alerter.seed(key, term.fact());
                    }
                    terminals.insert(summary.terminal, term);
                }
            }
        }
    }

    /// Everything due at `now`: stale reports go, what is on screen is taken in, and the
    /// alerter's raises and withdrawals are published, and posted as banners while no viewer
    /// presents.
    pub(crate) fn step(&self, now: u64) -> Step {
        let (posts, withdrawals, sound, step) = {
            let mut inner = self.lock();
            inner.refocus(now);
            let changes = inner.alerter.step(now);
            let own = !inner.presenting() && inner.prefs.banners;
            let mut events = Vec::new();
            let mut posts = Vec::new();
            let mut withdrawals = Vec::new();
            for change in changes {
                match change {
                    Change::Raise { key, kind } => {
                        let Some(alert) = inner.alert(key, kind, now) else {
                            continue;
                        };
                        if own {
                            inner.posted.insert(alert.id.clone());
                            posts.push(alert.clone());
                        }
                        events.push(AlertsEvent::Raised(Box::new(alert)));
                    }
                    Change::Withdraw { key } => {
                        let id = alert_id(key);
                        if inner.posted.remove(&id) {
                            withdrawals.push(id.clone());
                        }
                        events.push(AlertsEvent::Withdrawn(id));
                    }
                }
            }
            inner.topic.publish(events);
            let expiry = inner
                .reports
                .values()
                .map(|report| report.at.saturating_add(REPORT_TTL_MS))
                .min();
            let next = match (inner.alerter.next_deadline(), expiry) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let step = Step {
                next,
                seen: std::mem::take(&mut inner.seen),
            };
            (posts, withdrawals, inner.prefs.sound, step)
        };
        for alert in &posts {
            self.banners.post(alert, sound);
        }
        for id in &withdrawals {
            self.banners.withdraw(id);
        }
        step
    }
}
