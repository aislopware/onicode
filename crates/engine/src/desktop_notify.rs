//! Desktop banners and agent alerts, decided by the engine so they keep coming with the app's
//! window closed.
//!
//! Only desktop engines run this (the app's background engine and its in-process fallback call
//! [`enable`]); a headless server never does.
//!
//! - Agent alerts (`docs/onicode/decisions/alerts.md`): the shared [`onicode_proto::alerts`]
//!   machine over every engine's session rows, with the chat on screen from `SetViewportFocus`. The
//!   engine publishes what should show on [`AlertFeed`] (`WatchAlerts`); an attached app presents
//!   it with actions. With no app presenting, the engine posts plain banners itself.
//! - Connection loss and agent updates stay engine banners.
//! - Preferences come from the app's `ui-settings.json` (read on each step, so changes apply at
//!   once): `notificationsEnabled`, `notificationsBackgroundOnly`, `agentUpdateNotifications`.
//!
//! The app keeps the chimes; it uses [`SessionNotificationState`] and
//! [`ConnectivityNotificationState`] for them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use onicode_proto::alerts::{self, AgentAlert, AlertPrefs, Alerter, Board, Question};
use onicode_proto::view::{Indicator, effective_indicator};
use onicode_proto::{Chat, ConnectivityState, HarnessUpdatePhase, HarnessUpdateStatus, Session};
use tokio::sync::watch;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// This process shows desktop banners: engines assembled from now on post
/// them (and advertise [`onicode_proto::capabilities::DESKTOP_NOTIFICATIONS_V1`]).
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// What a transition calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ping {
    /// A run finished.
    Done,
    /// An agent is waiting on an answer.
    Request,
    /// A run failed, or the connection went away.
    Attention,
}

/// Notification baseline is separate from the visual activity indicator:
/// going idle can mean cancellation, expiry, or an internal handoff.
#[derive(Debug, Clone)]
pub struct SessionNotificationState {
    indicator: Indicator,
    last_completed_turn: Option<String>,
    fresh: bool,
}

impl SessionNotificationState {
    pub fn new(session: &Session, now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            indicator: effective_indicator(Some(session), now),
            last_completed_turn: session.last_completed_turn.clone(),
            fresh: now
                .signed_duration_since(session.updated_at)
                .num_milliseconds()
                <= onicode_proto::view::SESSION_STALE_MS,
        }
    }

    /// Call after saving the new baseline, including when delivery is pending
    /// or outputs are disabled. Suppressed pings must never be replayed later.
    pub fn ping_since(&self, prev: &Self, send_pending: bool) -> Option<Ping> {
        if self.indicator == Indicator::Errored && prev.indicator != Indicator::Errored {
            return Some(Ping::Attention);
        }
        if self.indicator == Indicator::AwaitingInput && prev.indicator != Indicator::AwaitingInput
        {
            return Some(Ping::Request);
        }
        if !send_pending
            && self.fresh
            && self.last_completed_turn.is_some()
            && self.last_completed_turn != prev.last_completed_turn
        {
            return Some(Ping::Done);
        }
        None
    }
}

/// The engine already holds raw transport degradation for four seconds before
/// exposing `Offline` or `Reconnecting`. Notify once when that durable state is
/// first crossed; booting into an outage is seeded silently.
pub fn connectivity_ping_since(
    current: ConnectivityState,
    previous: ConnectivityState,
) -> Option<Ping> {
    let degraded = matches!(
        current,
        ConnectivityState::Offline | ConnectivityState::Reconnecting
    );
    let was_degraded = matches!(
        previous,
        ConnectivityState::Offline | ConnectivityState::Reconnecting
    );
    (degraded && !was_degraded).then_some(Ping::Attention)
}

/// A fresh observer can see `Connected` while the four-second degradation
/// grace is still measuring an outage that predates it. Arm alerts only
/// after a longer healthy observation, or after recovery from a boot-time
/// outage. Runtime replacement resets the observation flag.
#[derive(Debug, Default)]
pub struct ConnectivityNotificationState {
    previous: Option<ConnectivityState>,
    first_observed_at: Option<Instant>,
    armed: bool,
}

impl ConnectivityNotificationState {
    const STARTUP_QUIET: Duration = Duration::from_secs(5);

    pub fn update(
        &mut self,
        current: ConnectivityState,
        observed: bool,
        now: Instant,
    ) -> Option<Ping> {
        if !observed {
            *self = Self::default();
            return None;
        }
        let first = *self.first_observed_at.get_or_insert(now);
        let previous = self.previous.replace(current);
        if !self.armed {
            if now.duration_since(first) >= Self::STARTUP_QUIET {
                self.armed = true;
                return previous.and_then(|previous| connectivity_ping_since(current, previous));
            }
            return None;
        }
        previous.and_then(|previous| connectivity_ping_since(current, previous))
    }
}

/// One viewport's last report.
#[derive(Debug, Clone)]
struct Report {
    at: Instant,
    focused: bool,
    chat: Option<String>,
    presents: bool,
}

#[derive(Default)]
struct FocusInner {
    reports: Mutex<HashMap<String, Report>>,
    changed: tokio::sync::Notify,
}

/// Where the owner is in the app's windows, as their viewports report: which window has focus,
/// the chat it shows, and whether the app presents agent alerts itself. A report goes stale after
/// [`Self::TTL`] (viewports repeat it), so a viewport that died never holds anything back for good.
#[derive(Clone, Default)]
pub struct ViewportFocus(Arc<FocusInner>);

impl ViewportFocus {
    pub const TTL: Duration = Duration::from_secs(45);

    /// A viewport's report. `presents: false` with `focused: false` is a viewport going away.
    pub fn report(&self, viewport: &str, focused: bool, chat: Option<String>, presents: bool) {
        self.report_at(viewport, focused, chat, presents, Instant::now());
    }

    fn report_at(
        &self,
        viewport: &str,
        focused: bool,
        chat: Option<String>,
        presents: bool,
        now: Instant,
    ) {
        {
            let mut reports = self
                .0
                .reports
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if focused || presents {
                reports.insert(
                    viewport.to_owned(),
                    Report {
                        at: now,
                        focused,
                        chat,
                        presents,
                    },
                );
            } else {
                reports.remove(viewport);
            }
        }
        self.0.changed.notify_one();
    }

    /// Wakes after any report.
    async fn changed(&self) {
        self.0.changed.notified().await;
    }

    fn fresh_at(&self, now: Instant) -> Vec<Report> {
        let mut reports = self
            .0
            .reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reports.retain(|_, report| now.saturating_duration_since(report.at) < Self::TTL);
        reports.values().cloned().collect()
    }

    /// Some app window is focused.
    pub fn any(&self) -> bool {
        self.any_at(Instant::now())
    }

    fn any_at(&self, now: Instant) -> bool {
        self.fresh_at(now).iter().any(|report| report.focused)
    }

    /// The chat on screen in the focused window.
    pub fn focused_chat(&self) -> Option<String> {
        self.fresh_at(Instant::now())
            .into_iter()
            .filter(|report| report.focused)
            .max_by_key(|report| report.at)
            .and_then(|report| report.chat)
    }

    /// An attached app presents agent alerts, so the engine posts none of its own.
    pub fn presenting(&self) -> bool {
        self.presenting_at(Instant::now())
    }

    fn presenting_at(&self, now: Instant) -> bool {
        self.fresh_at(now).iter().any(|report| report.presents)
    }
}

/// What agent alerts should show now, for `WatchAlerts`.
#[derive(Clone)]
pub struct AlertFeed(Arc<watch::Sender<Vec<AgentAlert>>>);

impl Default for AlertFeed {
    fn default() -> Self {
        Self(Arc::new(watch::channel(Vec::new()).0))
    }
}

impl AlertFeed {
    pub fn watch(&self) -> watch::Receiver<Vec<AgentAlert>> {
        self.0.subscribe()
    }

    fn publish(&self, alerts: Vec<AgentAlert>) {
        self.0.send_if_modified(|current| {
            if *current == alerts {
                false
            } else {
                *current = alerts;
                true
            }
        });
    }
}

/// The app's banner preferences, from `{data_dir}/ui-settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Prefs {
    enabled: bool,
    background_only: bool,
    agent_updates: bool,
}

impl Prefs {
    fn read(data_dir: &Path) -> Self {
        let value = std::fs::read(data_dir.join("ui-settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        Self::from_value(value.as_ref())
    }

    fn from_value(value: Option<&serde_json::Value>) -> Self {
        let flag = |key: &str| {
            value
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        };
        Self {
            enabled: flag("notificationsEnabled"),
            background_only: flag("notificationsBackgroundOnly"),
            agent_updates: flag("agentUpdateNotifications"),
        }
    }
}

const VERSIONLESS: &str = "versionless";

fn versionless_key(device: &str, harness: onicode_proto::HarnessId) -> String {
    format!("{device}:{harness:?}:{VERSIONLESS}")
}

/// One banner per release: versioned releases deduplicate by version,
/// versionless availability until the agent reports current again.
fn update_key(device: &str, status: &HarnessUpdateStatus) -> Option<String> {
    if status.phase != HarnessUpdatePhase::Available {
        return None;
    }
    Some(match status.latest_version.as_deref() {
        Some(version) => format!("{device}:{:?}:{version}", status.harness),
        None => versionless_key(device, status.harness),
    })
}

/// The open question a chat's transcript ends on: an unresolved input part on its last assistant
/// entry, as the composer's question panel reads it.
pub fn open_question(entries: &[onicode_doc::SessionMessageEntry]) -> Option<Question> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.role == onicode_doc::MessageRole::Assistant)
        .and_then(|entry| {
            entry.parts.iter().find_map(|part| match part {
                onicode_doc::MessagePart::Input {
                    request_id,
                    questions,
                    resolved: false,
                    ..
                } if !questions.is_empty() => Some(Question {
                    request_id: request_id.clone(),
                    questions: questions.clone(),
                }),
                _ => None,
            })
        })
}

type Post = Box<dyn Fn(&str, &str, Option<&str>) + Send>;
type ReadQuestion = Box<dyn Fn(&str) -> Option<Question> + Send>;

/// How often a raised question's words are looked for while its doc syncs.
const QUESTION_POLL: chrono::TimeDelta = chrono::TimeDelta::milliseconds(250);

/// The decision state, separate from the watches so it can be driven by tests.
struct Notifier {
    data_dir: PathBuf,
    device: String,
    focus: ViewportFocus,
    post: Post,
    read_question: ReadQuestion,
    feed: AlertFeed,
    alerter: Alerter,
    board: Board,
    /// What showed, by chat, so a raise is posted once: by the app, or by this engine while no
    /// app presents.
    posted: HashMap<String, chrono::DateTime<chrono::Utc>>,
    linked: bool,
    connectivity: ConnectivityNotificationState,
    updates_seen: HashSet<String>,
}

impl Notifier {
    fn new(
        data_dir: PathBuf,
        device: String,
        focus: ViewportFocus,
        feed: AlertFeed,
        post: Post,
        read_question: ReadQuestion,
    ) -> Self {
        Self {
            data_dir,
            device,
            focus,
            post,
            read_question,
            feed,
            alerter: Alerter::default(),
            board: Board::default(),
            posted: HashMap::new(),
            linked: true,
            connectivity: ConnectivityNotificationState::default(),
            updates_seen: HashSet::new(),
        }
    }

    /// Whether a banner may show now (and its preferences, read fresh).
    fn allowed(&self) -> Option<Prefs> {
        let prefs = Prefs::read(&self.data_dir);
        (prefs.enabled && !(prefs.background_only && self.focus.any())).then_some(prefs)
    }

    /// Take in every engine's session rows (or a focus change, or a due deadline).
    fn sessions(
        &mut self,
        sessions: &[Session],
        chats: &[Chat],
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let prefs = Prefs::read(&self.data_dir);
        let focused_chat = self.focus.focused_chat();
        let cx = alerts::Context {
            now,
            focused_chat: focused_chat.as_deref(),
            app_in_front: self.focus.any(),
            linked: self.linked,
            prefs: AlertPrefs {
                enabled: prefs.enabled,
                background_only: prefs.background_only,
            },
        };
        let changes = self.alerter.step(chats, sessions, &cx);
        self.board.apply(changes);
        for chat_id in self.board.wanting_question() {
            if let Some(question) = (self.read_question)(&chat_id) {
                self.board.set_question(&chat_id, question);
            }
        }
        let shown = self.board.shown(now);
        let presenting = self.focus.presenting();
        for alert in &shown {
            if self.posted.get(&alert.chat_id) == Some(&alert.raised_at) {
                continue;
            }
            self.posted.insert(alert.chat_id.clone(), alert.raised_at);
            if !presenting {
                tracing::info!(chat = %alert.chat_id, kind = ?alert.kind, "desktop banner");
                (self.post)(&alert.title, &alert.body, Some(&alert.chat_id));
            }
        }
        self.posted
            .retain(|chat_id, _| shown.iter().any(|alert| &alert.chat_id == chat_id));
        self.feed.publish(shown);
    }

    /// When the alerts next need a step without any row changing.
    fn alert_deadline(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let polling = (!self.board.wanting_question().is_empty()).then(|| now + QUESTION_POLL);
        [
            self.alerter.next_deadline(),
            self.board.next_deadline(),
            polling,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn connectivity(&mut self, state: ConnectivityState, now: Instant) {
        self.linked = !matches!(
            state,
            ConnectivityState::Offline | ConnectivityState::Reconnecting
        );
        if self.connectivity.update(state, true, now).is_none() || self.allowed().is_none() {
            return;
        }
        let body = match state {
            ConnectivityState::Offline => "Your device is offline",
            _ => "Onicode is trying to reconnect",
        };
        (self.post)("Connection unavailable", body, None);
    }

    /// New releases not bannered yet (a confirmed current/updated status
    /// re-arms versionless availability).
    fn unseen_updates(&mut self, statuses: &[HarnessUpdateStatus]) -> Vec<String> {
        for status in statuses {
            if matches!(
                status.phase,
                HarnessUpdatePhase::Current | HarnessUpdatePhase::Updated
            ) {
                self.updates_seen
                    .remove(&versionless_key(&self.device, status.harness));
            }
        }
        statuses
            .iter()
            .filter_map(|status| update_key(&self.device, status))
            .filter(|key| !self.updates_seen.contains(key))
            .collect()
    }

    /// Release discoveries finish independently, so the caller debounces and
    /// this delivers one aggregate banner for everything new.
    fn updates(&mut self, statuses: &[HarnessUpdateStatus]) {
        let new = self.unseen_updates(statuses);
        if new.is_empty() {
            return;
        }
        let count = new.len();
        self.updates_seen.extend(new);
        if !self.allowed().is_some_and(|prefs| prefs.agent_updates) {
            return;
        }
        let body = if count == 1 {
            "A coding agent update is ready"
        } else {
            "Coding agent updates are ready"
        };
        (self.post)(
            &format!(
                "{count} agent update{} available",
                if count == 1 { "" } else { "s" }
            ),
            body,
            Some(onicode_notify::AGENT_UPDATES_TARGET),
        );
    }
}

/// `at` as a Tokio deadline.
fn tokio_deadline(at: chrono::DateTime<chrono::Utc>) -> tokio::time::Instant {
    let wait = (at - chrono::Utc::now()).to_std().unwrap_or_default();
    tokio::time::Instant::now() + wait
}

/// Watch every engine's sessions, the viewports, connectivity and agent updates; publish agent
/// alerts and post banners until the runtime shuts down.
pub(crate) fn spawn(core: &crate::EngineCore, data_dir: PathBuf) -> tokio::task::JoinHandle<()> {
    let mut sessions = core
        .workspace
        .merged_sessions_watch(core.sessions.watch_sessions());
    let mut chats = core.workspace.watch_chats();
    let mut connectivity = core.doc_host.watch_connectivity();
    let mut updates = core.harness_updates.watch();
    let focus = core.viewport_focus.clone();
    let docs = core.doc_host.clone();
    let mut notifier = Notifier::new(
        data_dir,
        core.device_id.clone(),
        focus.clone(),
        core.alerts.clone(),
        Box::new(onicode_notify::post),
        Box::new(move |chat_id| {
            let handle = docs.open(chat_id).ok()?;
            let entries = handle.doc().read_entries().ok()?;
            open_question(&entries)
        }),
    );
    tokio::spawn(async move {
        const UPDATE_DEBOUNCE: Duration = Duration::from_secs(1);
        // Seed every baseline silently.
        notifier.connectivity(connectivity.borrow_and_update().state, Instant::now());
        notifier.sessions(
            &sessions.borrow_and_update(),
            &chats.borrow_and_update(),
            chrono::Utc::now(),
        );
        let mut updates_due: Option<tokio::time::Instant> =
            Some(tokio::time::Instant::now() + UPDATE_DEBOUNCE);
        loop {
            let alert_due = notifier
                .alert_deadline(chrono::Utc::now())
                .map(tokio_deadline);
            tokio::select! {
                changed = sessions.changed() => {
                    if changed.is_err() { break; }
                    let sessions = sessions.borrow_and_update().clone();
                    notifier.sessions(&sessions, &chats.borrow(), chrono::Utc::now());
                }
                changed = chats.changed() => {
                    if changed.is_err() { break; }
                    let chats = chats.borrow_and_update().clone();
                    notifier.sessions(&sessions.borrow(), &chats, chrono::Utc::now());
                }
                () = focus.changed() => {
                    let sessions = sessions.borrow().clone();
                    notifier.sessions(&sessions, &chats.borrow(), chrono::Utc::now());
                }
                () = async {
                    match alert_due {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let sessions = sessions.borrow().clone();
                    notifier.sessions(&sessions, &chats.borrow(), chrono::Utc::now());
                }
                changed = connectivity.changed() => {
                    if changed.is_err() { break; }
                    let state = connectivity.borrow_and_update().state;
                    notifier.connectivity(state, Instant::now());
                }
                changed = updates.changed() => {
                    if changed.is_err() { break; }
                    updates.borrow_and_update();
                    updates_due.get_or_insert(tokio::time::Instant::now() + UPDATE_DEBOUNCE);
                }
                () = async {
                    match updates_due {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    updates_due = None;
                    let statuses = updates.borrow().clone();
                    notifier.updates(&statuses);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn baseline(indicator: Indicator, turn: Option<&str>) -> SessionNotificationState {
        SessionNotificationState {
            indicator,
            last_completed_turn: turn.map(str::to_owned),
            fresh: true,
        }
    }

    #[test]
    fn interrupted_and_expired_activity_never_ping() {
        let working = baseline(Indicator::Working, Some("old"));
        let idle = baseline(Indicator::None, Some("old"));
        assert_eq!(idle.ping_since(&working, false), None);
        // An older host without explicit completion metadata is silent too.
        assert_eq!(
            baseline(Indicator::None, None).ping_since(&baseline(Indicator::Working, None), false),
            None
        );
    }

    #[test]
    fn a_run_error_pings_once_and_never_masquerades_as_completion() {
        let working = baseline(Indicator::Working, Some("old"));
        let errored = baseline(Indicator::Errored, Some("failed"));
        assert_eq!(errored.ping_since(&working, false), Some(Ping::Attention));
        assert_eq!(errored.ping_since(&errored, false), None);
    }

    #[test]
    fn durable_connectivity_degradation_pings_once_per_outage() {
        use onicode_proto::ConnectivityState as State;

        assert_eq!(
            connectivity_ping_since(State::Connected, State::Disabled),
            None
        );
        assert_eq!(
            connectivity_ping_since(State::Reconnecting, State::Connected),
            Some(Ping::Attention)
        );
        assert_eq!(
            connectivity_ping_since(State::Offline, State::Reconnecting),
            None
        );
        assert_eq!(
            connectivity_ping_since(State::Connected, State::Offline),
            None
        );
        assert_eq!(
            connectivity_ping_since(State::Offline, State::Connected),
            Some(Ping::Attention)
        );
    }

    #[test]
    fn ordinary_queue_completions_survive_coalesced_working_states() {
        let first = baseline(Indicator::Working, None);
        let second = baseline(Indicator::Working, Some("first"));
        assert_eq!(second.ping_since(&first, false), Some(Ping::Done));
        assert_eq!(second.ping_since(&second, false), None);
        let last = baseline(Indicator::None, Some("second"));
        assert_eq!(last.ping_since(&second, false), Some(Ping::Done));
        assert_eq!(last.ping_since(&last, false), None);
    }

    #[test]
    fn pending_send_consumes_completion_but_preserves_input_requests() {
        let before = baseline(Indicator::Working, None);
        let settled = baseline(Indicator::None, Some("first"));
        assert_eq!(settled.ping_since(&before, true), None);
        assert_eq!(settled.ping_since(&settled, false), None);
        let question = baseline(Indicator::AwaitingInput, Some("first"));
        assert_eq!(question.ping_since(&settled, true), Some(Ping::Request));
        assert_eq!(question.ping_since(&question, false), None);
    }

    #[test]
    fn stale_completion_is_consumed_without_replaying_on_a_heartbeat() {
        let before = baseline(Indicator::Working, None);
        let mut stale = baseline(Indicator::None, Some("old"));
        stale.fresh = false;
        assert_eq!(stale.ping_since(&before, false), None);
        let refreshed = baseline(Indicator::Working, Some("old"));
        assert_eq!(refreshed.ping_since(&stale, false), None);
    }

    #[test]
    fn connectivity_boot_outages_seed_silently_then_later_outages_alert() {
        use onicode_proto::ConnectivityState as State;
        let t0 = Instant::now();

        // Warm daemon: the first authoritative snapshot is already degraded.
        let mut warm = ConnectivityNotificationState::default();
        assert_eq!(warm.update(State::Disabled, false, t0), None);
        assert_eq!(warm.update(State::Offline, true, t0), None);
        assert_eq!(
            warm.update(State::Connected, true, t0 + Duration::from_secs(6)),
            None
        );
        assert_eq!(
            warm.update(State::Offline, true, t0 + Duration::from_secs(7)),
            Some(Ping::Attention)
        );

        // Cold daemon: the engine's grace initially masks the existing outage.
        let mut cold = ConnectivityNotificationState::default();
        assert_eq!(cold.update(State::Connected, true, t0), None);
        assert_eq!(
            cold.update(State::Offline, true, t0 + Duration::from_secs(4)),
            None
        );
        assert_eq!(
            cold.update(State::Connected, true, t0 + Duration::from_secs(5)),
            None
        );
        assert_eq!(
            cold.update(State::Reconnecting, true, t0 + Duration::from_secs(6)),
            Some(Ping::Attention)
        );

        // A quiet healthy boot may not publish another frame until the first
        // genuine outage; elapsed time arms that transition itself.
        let mut healthy = ConnectivityNotificationState::default();
        assert_eq!(healthy.update(State::Connected, true, t0), None);
        assert_eq!(
            healthy.update(State::Offline, true, t0 + Duration::from_secs(6)),
            Some(Ping::Attention)
        );

        // Replacing the runtime returns to bootstrap semantics even if the
        // prior runtime had already armed alerts.
        assert_eq!(
            healthy.update(State::Disabled, false, t0 + Duration::from_secs(7)),
            None
        );
        assert_eq!(
            healthy.update(State::Offline, true, t0 + Duration::from_secs(7)),
            None
        );
    }

    #[test]
    fn focus_reports_expire_and_clear() {
        let focus = ViewportFocus::default();
        let now = Instant::now();
        assert!(!focus.any_at(now));
        focus.report_at("window", true, Some("a".into()), true, now);
        assert!(focus.any_at(now + Duration::from_secs(10)));
        assert!(focus.presenting_at(now + Duration::from_secs(10)));
        // A viewport that died focused stops holding banners back.
        assert!(!focus.any_at(now + ViewportFocus::TTL + Duration::from_secs(1)));
        assert!(!focus.presenting_at(now + ViewportFocus::TTL + Duration::from_secs(1)));
        // Unfocused but still presenting: nothing held back, nothing posted twice.
        focus.report_at("window", false, None, true, now);
        assert!(!focus.any_at(now));
        assert!(focus.presenting_at(now));
        // Going away.
        focus.report_at("window", false, None, false, now);
        assert!(!focus.presenting_at(now));
    }

    #[test]
    fn preferences_default_on_and_read_the_app_settings() {
        assert_eq!(
            Prefs::from_value(None),
            Prefs {
                enabled: true,
                background_only: true,
                agent_updates: true
            }
        );
        let value = serde_json::json!({
            "notificationsEnabled": false,
            "notificationsBackgroundOnly": false,
            "theme": "dark",
        });
        assert_eq!(
            Prefs::from_value(Some(&value)),
            Prefs {
                enabled: false,
                background_only: false,
                agent_updates: true
            }
        );
    }

    type Posted = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

    fn notifier(dir: &Path) -> (Notifier, Posted, ViewportFocus, AlertFeed) {
        notifier_asking(dir, None)
    }

    fn notifier_asking(
        dir: &Path,
        question: Option<Question>,
    ) -> (Notifier, Posted, ViewportFocus, AlertFeed) {
        let posted: Posted = Arc::default();
        let sink = posted.clone();
        let focus = ViewportFocus::default();
        let feed = AlertFeed::default();
        let notifier = Notifier::new(
            dir.to_path_buf(),
            "mac".into(),
            focus.clone(),
            feed.clone(),
            Box::new(move |title, body, target| {
                sink.lock()
                    .unwrap()
                    .push((title.into(), body.into(), target.map(str::to_owned)));
            }),
            Box::new(move |_| question.clone()),
        );
        (notifier, posted, focus, feed)
    }

    fn session_in(
        chat_id: &str,
        status: &str,
        turn: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Session {
        serde_json::from_value(serde_json::json!({
            "chatId": chat_id,
            "deviceId": "mac",
            "status": status,
            "updatedAt": now,
            "lastCompletedTurn": turn,
        }))
        .expect("session fixture")
    }

    fn session(chat_id: &str, turn: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Session {
        session_in(chat_id, "idle", turn, now)
    }

    fn chat(id: &str, title: &str, parent: Option<&str>) -> Chat {
        let mut chat: Chat = serde_json::from_value(serde_json::json!({
            "id": id,
            "title": title,
            "deviceId": "mac",
            "archived": false,
            "cwd": null,
            "branch": null,
            "checkoutId": null,
            "config": null,
            "lastMessagePreview": null,
            "lastMessageAt": chrono::Utc::now(),
            "createdAt": chrono::Utc::now(),
        }))
        .expect("chat fixture");
        chat.parent_chat_id = parent.map(str::to_owned);
        chat
    }

    fn after(now: chrono::DateTime<chrono::Utc>, ms: i64) -> chrono::DateTime<chrono::Utc> {
        now + chrono::TimeDelta::milliseconds(ms)
    }

    #[test]
    fn finished_runs_banner_top_level_chats_unless_focused_or_off() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notifier, posted, focus, _) = notifier(dir.path());
        let now = chrono::Utc::now();
        let chats = [
            chat("a", "Fix the build", None),
            chat("side", "Side", Some("a")),
        ];
        // First sight seeds silently.
        notifier.sessions(
            &[
                session("a", Some("t1"), now),
                session("side", Some("s1"), now),
            ],
            &chats,
            now,
        );
        assert!(posted.lock().unwrap().is_empty());
        // A finished run on the top-level chat banners after its hold; the side chat doesn't.
        let done = [
            session("a", Some("t2"), now),
            session("side", Some("s2"), now),
        ];
        notifier.sessions(&done, &chats, now);
        assert!(posted.lock().unwrap().is_empty(), "held");
        notifier.sessions(&done, &chats, after(now, alerts::HOLD_MS));
        assert_eq!(
            *posted.lock().unwrap(),
            vec![(
                "Fix the build".into(),
                "Run finished".into(),
                Some("a".into())
            )]
        );
        // A focused window holds it back (background-only is the default).
        focus.report("window", true, None, false);
        notifier.sessions(&[session("a", Some("t3"), now)], &chats, now);
        notifier.sessions(
            &[session("a", Some("t3"), now)],
            &chats,
            after(now, alerts::HOLD_MS * 2),
        );
        assert_eq!(posted.lock().unwrap().len(), 1);
        // Banners off: nothing, focused or not.
        focus.report("window", false, None, false);
        std::fs::write(
            dir.path().join("ui-settings.json"),
            r#"{"notificationsEnabled": false}"#,
        )
        .unwrap();
        notifier.sessions(&[session("a", Some("t4"), now)], &chats, now);
        notifier.sessions(
            &[session("a", Some("t4"), now)],
            &chats,
            after(now, alerts::HOLD_MS * 3),
        );
        assert_eq!(posted.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_presenting_app_takes_the_alerts_and_the_engine_posts_none() {
        let dir = tempfile::tempdir().unwrap();
        let question = Question {
            request_id: "req".into(),
            questions: vec![onicode_proto::UserInputQuestion {
                id: "q".into(),
                header: "Approve command".into(),
                question: "Codex wants to run `make`. Allow it?".into(),
                options: vec!["Yes".into(), "No".into()],
                multi_select: false,
                prefill: None,
                multiline: false,
                approval: None,
            }],
        };
        let (mut notifier, posted, focus, feed) = notifier_asking(dir.path(), Some(question));
        let alerts = feed.watch();
        focus.report("window", false, None, true);
        let now = chrono::Utc::now();
        let chats = [chat("a", "Fix the build", None)];
        notifier.sessions(&[session_in("a", "working", None, now)], &chats, now);
        let asking = [session_in("a", "awaitingInput", None, now)];
        notifier.sessions(&asking, &chats, now);
        notifier.sessions(&asking, &chats, after(now, alerts::HOLD_MS));
        let shown = alerts.borrow().clone();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].body, "Codex wants to run `make`. Allow it?");
        assert_eq!(
            shown[0].answer.as_ref().and_then(|a| a.approve.as_deref()),
            Some("Yes")
        );
        assert!(posted.lock().unwrap().is_empty(), "the app presents it");
        // Answered on the phone: the row runs again and the alert leaves the feed.
        let resumed = after(now, alerts::HOLD_MS + 500);
        notifier.sessions(
            &[session_in("a", "working", None, resumed)],
            &chats,
            resumed,
        );
        assert!(alerts.borrow().is_empty());
    }

    #[test]
    fn the_open_question_is_the_last_assistant_entrys_unresolved_input() {
        use onicode_doc::{MessagePart, MessageRole, SessionMessageEntry};

        let entry = |role: MessageRole, parts: Vec<MessagePart>| SessionMessageEntry {
            id: "entry".into(),
            role,
            parts,
            created_at: 0,
            device_id: "mac".into(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        };
        let input = |resolved: bool| MessagePart::Input {
            id: "in-req".into(),
            request_id: "req".into(),
            questions: vec![onicode_proto::UserInputQuestion {
                id: "q".into(),
                header: String::new(),
                question: "Which database?".into(),
                options: vec!["Postgres".into(), "SQLite".into()],
                multi_select: false,
                prefill: None,
                multiline: false,
                approval: None,
            }],
            resolved,
            answers: None,
        };
        let open = [
            entry(MessageRole::Assistant, vec![input(false)]),
            entry(MessageRole::User, Vec::new()),
        ];
        assert_eq!(
            open_question(&open).map(|q| q.request_id),
            Some("req".into())
        );
        assert_eq!(
            open_question(&[entry(MessageRole::Assistant, vec![input(true)])]),
            None
        );
        assert_eq!(
            open_question(&[
                entry(MessageRole::Assistant, vec![input(false)]),
                entry(MessageRole::Assistant, Vec::new()),
            ]),
            None,
            "a newer reply supersedes the question"
        );
    }

    #[test]
    fn agent_updates_banner_once_per_release() {
        let dir = tempfile::tempdir().unwrap();
        let (mut notifier, posted, _) = notifier(dir.path());
        let status = |phase: &str, version: Option<&str>| -> HarnessUpdateStatus {
            serde_json::from_value(serde_json::json!({
                "harness": "codex",
                "phase": phase,
                "latestVersion": version,
            }))
            .expect("update fixture")
        };
        notifier.updates(&[status("available", Some("1.2"))]);
        notifier.updates(&[status("available", Some("1.2"))]);
        assert_eq!(posted.lock().unwrap().len(), 1);
        assert_eq!(
            posted.lock().unwrap()[0].2.as_deref(),
            Some(onicode_notify::AGENT_UPDATES_TARGET)
        );
        // Versionless availability re-arms after the agent reports current.
        notifier.updates(&[status("available", None)]);
        notifier.updates(&[status("current", None)]);
        notifier.updates(&[status("available", None)]);
        assert_eq!(posted.lock().unwrap().len(), 3);
    }
}
