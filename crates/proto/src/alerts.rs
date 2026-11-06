//! Agent alerts: the moments a chat needs its owner, raised on each device the owner uses and
//! taken back everywhere once someone dealt with them (`docs/onicode/decisions/alerts.md`).
//!
//! Pure, so every device decides alike from the same replicated rows:
//! - [`Alerter`] turns registry rows into [`Change`]s: a raise after a short hold, a withdrawal
//!   once the cause ended (answered, seen or resumed on any device) or the chat is on screen here.
//! - [`Board`] keeps what should show, and waits briefly for an open question's words so its
//!   notification carries the answers it can take.
//! - [`command_for`] maps a notification action to what is sent to the chat's host.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::view::SESSION_STALE_MS;
use crate::{Chat, Session, SessionStatus, UserInputAnswer, UserInputQuestion};

/// How long a cause must hold before it is raised. A question answered by a rule, a run that fails
/// and is retried at once, or a burst of row updates comes and goes inside it unseen.
pub const HOLD_MS: i64 = 1_500;

/// How long a raised question waits for its words before it shows without them.
pub const QUESTION_WAIT_MS: i64 = 4_000;

/// The longest question text a notification carries.
const BODY_CHARS: usize = 180;

/// What an alert is about. One chat has at most one alert; a newer kind replaces the older.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AlertKind {
    /// The agent asks for approval or input.
    NeedsYou,
    /// The run failed.
    Failed,
    /// The chat's engine stopped answering mid-run: it slept, crashed or left the tailnet.
    Stalled,
    /// The turn finished.
    Finished,
}

/// What a chat's rows say now, as far as alerts care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Working,
    NeedsYou,
    Failed,
    Stalled,
}

/// The phase of a session row at `now`. Live states whose heartbeat stopped are [`Phase::Stalled`].
pub fn phase(session: &Session, now: DateTime<Utc>) -> Phase {
    let stale = now
        .signed_duration_since(session.updated_at)
        .num_milliseconds()
        > SESSION_STALE_MS;
    match session.activity_status() {
        SessionStatus::Idle => Phase::Idle,
        SessionStatus::Errored => Phase::Failed,
        SessionStatus::Working | SessionStatus::AwaitingInput if stale => Phase::Stalled,
        SessionStatus::Working => Phase::Working,
        SessionStatus::AwaitingInput => Phase::NeedsYou,
    }
}

/// The owner's alert settings on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertPrefs {
    pub enabled: bool,
    /// Raise nothing new while one of the app's windows has focus.
    pub background_only: bool,
}

impl Default for AlertPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            background_only: true,
        }
    }
}

/// Where the owner is, on this device, at one step.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub now: DateTime<Utc>,
    /// The chat on screen in a window that has focus, if any: it never alerts here.
    pub focused_chat: Option<&'a str>,
    /// One of the app's windows has focus (or the phone app is in front).
    pub app_in_front: bool,
    /// This device reaches the owner's engines, so a row gone stale means its engine went away.
    pub linked: bool,
    pub prefs: AlertPrefs,
}

/// An alert that passed its hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raised {
    pub chat_id: String,
    pub kind: AlertKind,
    pub title: String,
    pub raised_at: DateTime<Utc>,
}

/// What a step changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Show this, replacing whatever shows for the chat.
    Raise(Raised),
    /// Take back what shows for the chat.
    Withdraw { chat_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    phase: Phase,
    turn: Option<String>,
    fresh: bool,
    unseen: bool,
}

#[derive(Debug, Clone)]
struct Held {
    kind: AlertKind,
    since: DateTime<Utc>,
}

/// Whether the cause of a `kind` alert still stands.
fn stands(kind: AlertKind, row: &Row) -> bool {
    match kind {
        AlertKind::NeedsYou => row.phase == Phase::NeedsYou,
        AlertKind::Failed => row.phase == Phase::Failed && row.unseen,
        AlertKind::Stalled => row.phase == Phase::Stalled,
        AlertKind::Finished => row.phase == Phase::Idle && row.unseen,
    }
}

/// What moving from `prev` to `row` raises.
fn raised(prev: &Row, row: &Row, linked: bool) -> Option<AlertKind> {
    match row.phase {
        Phase::NeedsYou if prev.phase != Phase::NeedsYou => Some(AlertKind::NeedsYou),
        Phase::Failed if prev.phase != Phase::Failed => Some(AlertKind::Failed),
        // Only a heartbeat lost while this device is linked says the engine went away.
        Phase::Stalled if linked && matches!(prev.phase, Phase::Working | Phase::NeedsYou) => {
            Some(AlertKind::Stalled)
        }
        Phase::Idle if row.fresh && row.turn.is_some() && row.turn != prev.turn => {
            Some(AlertKind::Finished)
        }
        _ => None,
    }
}

/// The decision state of one device. Feed it every registry change and every
/// [`Self::next_deadline`]; it never shows anything itself.
#[derive(Debug, Default)]
pub struct Alerter {
    rows: HashMap<String, Row>,
    held: BTreeMap<String, Held>,
    live: BTreeMap<String, AlertKind>,
}

impl Alerter {
    /// The alerts showing now, by chat.
    pub fn live(&self) -> &BTreeMap<String, AlertKind> {
        &self.live
    }

    /// When a held cause is due to be raised.
    pub fn next_deadline(&self) -> Option<DateTime<Utc>> {
        self.held
            .values()
            .map(|held| held.since + TimeDelta::milliseconds(HOLD_MS))
            .min()
    }

    /// Take in the rows as they are at `cx.now`. A chat seen for the first time seeds silently,
    /// so a boot, a reconnect or a replay never rings.
    pub fn step(&mut self, chats: &[Chat], sessions: &[Session], cx: &Context<'_>) -> Vec<Change> {
        let by_chat: HashMap<&str, &Session> = sessions
            .iter()
            .map(|session| (session.chat_id.as_str(), session))
            .collect();
        let mut titles: HashMap<&str, &str> = HashMap::new();
        let mut changes = Vec::new();
        let mut present = Vec::new();
        for chat in chats {
            if !chat.is_top_level() || chat.archived {
                continue;
            }
            let Some(session) = by_chat.get(chat.id.as_str()) else {
                continue;
            };
            present.push(chat.id.as_str());
            titles.insert(
                chat.id.as_str(),
                chat.title.as_deref().unwrap_or("New session"),
            );
            let row = Row {
                phase: phase(session, cx.now),
                turn: session.last_completed_turn.clone(),
                fresh: cx
                    .now
                    .signed_duration_since(session.updated_at)
                    .num_milliseconds()
                    <= SESSION_STALE_MS,
                unseen: chat.unseen(),
            };
            let prev = self.rows.insert(chat.id.clone(), row.clone());
            if let Some(kind) = prev.and_then(|prev| raised(&prev, &row, cx.linked)) {
                self.held.insert(
                    chat.id.clone(),
                    Held {
                        kind,
                        since: cx.now,
                    },
                );
            }
            let looking = cx.focused_chat == Some(chat.id.as_str());
            if looking {
                self.held.remove(&chat.id);
            }
            if let Some(kind) = self.live.get(&chat.id).copied()
                && (looking || !cx.prefs.enabled || !stands(kind, &row))
            {
                self.live.remove(&chat.id);
                changes.push(Change::Withdraw {
                    chat_id: chat.id.clone(),
                });
            }
        }
        // Chats gone, archived or turned into side chats take their alerts with them.
        self.rows.retain(|id, _| present.contains(&id.as_str()));
        self.held.retain(|id, _| present.contains(&id.as_str()));
        let gone: Vec<String> = self
            .live
            .keys()
            .filter(|id| !present.contains(&id.as_str()))
            .cloned()
            .collect();
        for chat_id in gone {
            self.live.remove(&chat_id);
            changes.push(Change::Withdraw { chat_id });
        }
        let allowed = cx.prefs.enabled && !(cx.prefs.background_only && cx.app_in_front);
        let due: Vec<String> = self
            .held
            .iter()
            .filter(|(_, held)| {
                cx.now.signed_duration_since(held.since).num_milliseconds() >= HOLD_MS
            })
            .map(|(id, _)| id.clone())
            .collect();
        for chat_id in due {
            let Some(held) = self.held.remove(&chat_id) else {
                continue;
            };
            // A cause that ended inside the hold, or one held while the app was in front, is
            // dropped and never replayed.
            let stands_now = self
                .rows
                .get(&chat_id)
                .is_some_and(|row| stands(held.kind, row));
            if !allowed || !stands_now {
                continue;
            }
            let title = titles.get(chat_id.as_str()).copied().unwrap_or_default();
            self.live.insert(chat_id.clone(), held.kind);
            changes.push(Change::Raise(Raised {
                chat_id,
                kind: held.kind,
                title: title.to_owned(),
                raised_at: cx.now,
            }));
        }
        changes
    }
}

/// An action a notification offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AlertAction {
    Approve,
    Deny,
    /// Typed text: the answer to a question, or a message to the agent.
    Reply,
    Stop,
}

impl AlertAction {
    /// The action's identifier in a notification category.
    pub fn id(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
            Self::Reply => "reply",
            Self::Stop => "stop",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Approve, Self::Deny, Self::Reply, Self::Stop]
            .into_iter()
            .find(|action| action.id() == id)
    }

    /// The button's words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::Deny => "Deny",
            Self::Reply => "Reply",
            Self::Stop => "Stop",
        }
    }
}

/// The notification category for a set of actions: one per distinct set, so a platform registers
/// the few it needs once.
pub fn category(actions: &[AlertAction]) -> String {
    if actions.is_empty() {
        return "onicode.alert".to_owned();
    }
    let ids: Vec<&str> = actions.iter().map(|action| action.id()).collect();
    format!("onicode.alert.{}", ids.join("-"))
}

/// Every action set an alert can carry, for registering categories up front.
pub const ACTION_SETS: &[&[AlertAction]] = &[
    &[],
    &[
        AlertAction::Approve,
        AlertAction::Deny,
        AlertAction::Reply,
        AlertAction::Stop,
    ],
    &[AlertAction::Deny, AlertAction::Reply, AlertAction::Stop],
    &[AlertAction::Reply, AlertAction::Stop],
    &[AlertAction::Stop],
    &[AlertAction::Reply],
];

/// An open question, read from the chat's transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub request_id: String,
    pub questions: Vec<UserInputQuestion>,
}

/// What a quick answer sends back for a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answerable {
    pub request_id: String,
    pub question_id: String,
    /// The option that approves, when one-tap approval is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve: Option<String>,
    /// The option that declines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<String>,
}

/// Options that approve, and options that decline, as the drivers word them.
const APPROVING: &[&str] = &[
    "Yes",
    "Allow once",
    "Allow",
    "Approve",
    "Implement",
    "Accept",
];
const DECLINING: &[&str] = &["No", "Deny", "Reject", "Decline", "Keep planning"];

fn option_in<'a>(options: &'a [String], words: &[&str]) -> Option<&'a String> {
    words.iter().find_map(|word| {
        options
            .iter()
            .find(|option| option.trim().eq_ignore_ascii_case(word))
    })
}

/// What a notification can answer for `question`, and with which actions. Several questions, a
/// multi-select, or an approval that wants a deliberate act on its card answer only in the chat.
pub fn answers_for(question: &Question) -> (Option<Answerable>, Vec<AlertAction>) {
    let [only] = question.questions.as_slice() else {
        return (None, vec![AlertAction::Stop]);
    };
    if only.multi_select {
        return (None, vec![AlertAction::Stop]);
    }
    let approval = only.approval.clone().unwrap_or_default();
    if approval.requires_user_interaction {
        return (None, Vec::new());
    }
    let approve = option_in(&only.options, APPROVING).cloned();
    let deny = option_in(&only.options, DECLINING).cloned();
    let answerable = |approve: Option<String>| Answerable {
        request_id: question.request_id.clone(),
        question_id: only.id.clone(),
        approve,
        deny: deny.clone(),
    };
    match (&approve, &deny) {
        (Some(_), Some(_)) if approval.allows_quick_answer() => (
            Some(answerable(approve.clone())),
            vec![
                AlertAction::Approve,
                AlertAction::Deny,
                AlertAction::Reply,
                AlertAction::Stop,
            ],
        ),
        (_, Some(_)) => (
            Some(answerable(None)),
            vec![AlertAction::Deny, AlertAction::Reply, AlertAction::Stop],
        ),
        _ => (
            Some(answerable(None)),
            vec![AlertAction::Reply, AlertAction::Stop],
        ),
    }
}

/// One alert as a notification shows it. Presenters key it by `chat_id` and post it again only
/// when `raised_at` or its words changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAlert {
    pub chat_id: String,
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
    pub raised_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answerable>,
    #[serde(default)]
    pub actions: Vec<AlertAction>,
}

impl AgentAlert {
    /// The notification's identifier: one per chat, so a newer alert replaces the older.
    pub fn id(&self) -> &str {
        &self.chat_id
    }

    pub fn category(&self) -> String {
        category(&self.actions)
    }

    /// Only a question breaks through a Focus.
    pub fn urgent(&self) -> bool {
        self.kind == AlertKind::NeedsYou
    }
}

fn clip(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= BODY_CHARS {
        return text;
    }
    let mut clipped: String = text.chars().take(BODY_CHARS - 1).collect();
    clipped.push('…');
    clipped
}

/// The alert a raise shows, with the question when it is known.
pub fn present(raised: &Raised, question: Option<&Question>) -> AgentAlert {
    let (answer, actions) = match (raised.kind, question) {
        (AlertKind::NeedsYou, Some(question)) => answers_for(question),
        (AlertKind::NeedsYou, None) => (None, vec![AlertAction::Reply, AlertAction::Stop]),
        (AlertKind::Finished | AlertKind::Failed, _) => (None, vec![AlertAction::Reply]),
        (AlertKind::Stalled, _) => (None, Vec::new()),
    };
    let body = match raised.kind {
        AlertKind::NeedsYou => question
            .and_then(|question| question.questions.first())
            .map(|question| {
                if question.question.trim().is_empty() {
                    question.header.clone()
                } else {
                    question.question.clone()
                }
            })
            .map(|text| clip(&text))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "Waiting on your input".to_owned()),
        AlertKind::Failed => "Run failed".to_owned(),
        AlertKind::Stalled => "Its computer stopped answering mid-run".to_owned(),
        AlertKind::Finished => "Run finished".to_owned(),
    };
    AgentAlert {
        chat_id: raised.chat_id.clone(),
        kind: raised.kind,
        title: raised.title.clone(),
        body,
        raised_at: raised.raised_at,
        answer,
        actions,
    }
}

#[derive(Debug, Clone)]
struct Entry {
    raised: Raised,
    question: Option<Question>,
}

/// What should show, by chat. A raised question waits up to [`QUESTION_WAIT_MS`] for its words.
#[derive(Debug, Default)]
pub struct Board {
    entries: BTreeMap<String, Entry>,
}

impl Board {
    pub fn apply(&mut self, changes: Vec<Change>) {
        for change in changes {
            match change {
                Change::Raise(raised) => {
                    self.entries.insert(
                        raised.chat_id.clone(),
                        Entry {
                            raised,
                            question: None,
                        },
                    );
                }
                Change::Withdraw { chat_id } => {
                    self.entries.remove(&chat_id);
                }
            }
        }
    }

    /// Chats whose question's words are still wanted.
    pub fn wanting_question(&self) -> Vec<String> {
        self.entries
            .values()
            .filter(|entry| entry.raised.kind == AlertKind::NeedsYou && entry.question.is_none())
            .map(|entry| entry.raised.chat_id.clone())
            .collect()
    }

    /// The open question of `chat_id` was read. Returns whether it changed what shows.
    pub fn set_question(&mut self, chat_id: &str, question: Question) -> bool {
        match self.entries.get_mut(chat_id) {
            Some(entry)
                if entry.raised.kind == AlertKind::NeedsYou
                    && entry.question.as_ref() != Some(&question) =>
            {
                entry.question = Some(question);
                true
            }
            _ => false,
        }
    }

    /// When a question stops waiting for its words.
    pub fn next_deadline(&self) -> Option<DateTime<Utc>> {
        self.entries
            .values()
            .filter(|entry| entry.raised.kind == AlertKind::NeedsYou && entry.question.is_none())
            .map(|entry| entry.raised.raised_at + TimeDelta::milliseconds(QUESTION_WAIT_MS))
            .min()
    }

    /// The alerts to show at `now`, oldest first.
    pub fn shown(&self, now: DateTime<Utc>) -> Vec<AgentAlert> {
        let mut shown: Vec<AgentAlert> = self
            .entries
            .values()
            .filter(|entry| {
                entry.raised.kind != AlertKind::NeedsYou
                    || entry.question.is_some()
                    || now
                        .signed_duration_since(entry.raised.raised_at)
                        .num_milliseconds()
                        >= QUESTION_WAIT_MS
            })
            .map(|entry| present(&entry.raised, entry.question.as_ref()))
            .collect();
        shown.sort_by(|a, b| (a.raised_at, &a.chat_id).cmp(&(b.raised_at, &b.chat_id)));
        shown
    }
}

/// What an action sends to the chat's host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertCommand {
    /// Answer the open question (`RespondInput`).
    Answer {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
    /// A message to the agent: steered into a live turn, or sent as the next one.
    Message { text: String },
    /// Stop the live turn (`Interrupt`).
    Stop,
}

/// The command `action` on `alert` sends, if it offers that action. Typed text is required for
/// [`AlertAction::Reply`].
pub fn command_for(
    alert: &AgentAlert,
    action: AlertAction,
    text: Option<&str>,
) -> Option<AlertCommand> {
    if !alert.actions.contains(&action) {
        return None;
    }
    let answer = |label: String, answerable: &Answerable| AlertCommand::Answer {
        request_id: answerable.request_id.clone(),
        answers: vec![UserInputAnswer {
            question_id: answerable.question_id.clone(),
            labels: vec![label],
        }],
    };
    match action {
        AlertAction::Approve => {
            let answerable = alert.answer.as_ref()?;
            Some(answer(answerable.approve.clone()?, answerable))
        }
        AlertAction::Deny => {
            let answerable = alert.answer.as_ref()?;
            Some(answer(answerable.deny.clone()?, answerable))
        }
        AlertAction::Reply => {
            let text = text.map(str::trim).filter(|text| !text.is_empty())?;
            Some(match &alert.answer {
                Some(answerable) => answer(text.to_owned(), answerable),
                None => AlertCommand::Message {
                    text: text.to_owned(),
                },
            })
        }
        AlertAction::Stop => Some(AlertCommand::Stop),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::ApprovalContext;

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn ms(at: i64) -> DateTime<Utc> {
        t0() + TimeDelta::milliseconds(at)
    }

    fn chat(id: &str, parent: Option<&str>, seen_ms: Option<i64>, message_ms: i64) -> Chat {
        let mut chat: Chat = serde_json::from_value(serde_json::json!({
            "id": id,
            "title": format!("Chat {id}"),
            "deviceId": "studio",
            "archived": false,
            "cwd": null,
            "branch": null,
            "checkoutId": null,
            "config": null,
            "lastMessagePreview": null,
            "lastMessageAt": ms(message_ms),
            "createdAt": t0(),
        }))
        .expect("chat fixture");
        chat.parent_chat_id = parent.map(str::to_owned);
        chat.last_seen_at = seen_ms.map(ms);
        chat
    }

    fn session(id: &str, status: &str, updated_ms: i64, turn: Option<&str>) -> Session {
        serde_json::from_value(serde_json::json!({
            "chatId": id,
            "deviceId": "studio",
            "status": status,
            "updatedAt": ms(updated_ms),
            "lastCompletedTurn": turn,
            "startedAt": null,
        }))
        .expect("session fixture")
    }

    fn cx(now: i64) -> Context<'static> {
        Context {
            now: ms(now),
            focused_chat: None,
            app_in_front: false,
            linked: true,
            prefs: AlertPrefs::default(),
        }
    }

    fn raises(changes: &[Change]) -> Vec<(String, AlertKind)> {
        changes
            .iter()
            .filter_map(|change| match change {
                Change::Raise(raised) => Some((raised.chat_id.clone(), raised.kind)),
                Change::Withdraw { .. } => None,
            })
            .collect()
    }

    fn approval(options: &[&str], context: Option<ApprovalContext>) -> Question {
        Question {
            request_id: "req".into(),
            questions: vec![UserInputQuestion {
                id: "q1".into(),
                header: "Approve command".into(),
                question: "Claude wants to run `cargo test`. Allow it?".into(),
                options: options.iter().map(|o| (*o).to_owned()).collect(),
                multi_select: false,
                prefill: None,
                multiline: false,
                approval: context,
            }],
        }
    }

    #[test]
    fn a_question_rings_after_its_hold_and_goes_once_answered_anywhere() {
        let mut alerter = Alerter::default();
        let chats = [chat("a", None, Some(0), 0)];
        assert!(
            alerter
                .step(&chats, &[session("a", "working", 0, None)], &cx(0))
                .is_empty(),
            "first sight seeds"
        );
        let asking = [session("a", "awaitingInput", 1_000, None)];
        assert!(raises(&alerter.step(&chats, &asking, &cx(1_000))).is_empty());
        assert_eq!(alerter.next_deadline(), Some(ms(1_000 + HOLD_MS)));
        assert_eq!(
            raises(&alerter.step(&chats, &asking, &cx(1_000 + HOLD_MS))),
            vec![("a".to_owned(), AlertKind::NeedsYou)]
        );
        // Another device answered: the row runs again, and this device takes its alert back.
        let answered = [session("a", "working", 3_000, None)];
        assert_eq!(
            alerter.step(&chats, &answered, &cx(3_000)),
            vec![Change::Withdraw {
                chat_id: "a".into()
            }]
        );
        assert!(alerter.live().is_empty());
    }

    #[test]
    fn a_cause_that_ends_inside_its_hold_never_rings() {
        let mut alerter = Alerter::default();
        let chats = [chat("a", None, Some(0), 0)];
        alerter.step(&chats, &[session("a", "working", 0, None)], &cx(0));
        alerter.step(
            &chats,
            &[session("a", "awaitingInput", 100, None)],
            &cx(100),
        );
        let changes = alerter.step(&chats, &[session("a", "working", 900, None)], &cx(900));
        assert!(changes.is_empty());
        assert!(
            alerter
                .step(&chats, &[session("a", "working", 3_000, None)], &cx(3_000))
                .is_empty()
        );
    }

    #[test]
    fn the_chat_on_screen_never_rings_and_looking_takes_an_alert_back() {
        let mut alerter = Alerter::default();
        let chats = [chat("a", None, Some(0), 0)];
        let looking = Context {
            focused_chat: Some("a"),
            ..cx(0)
        };
        alerter.step(&chats, &[session("a", "working", 0, None)], &looking);
        let asking = [session("a", "awaitingInput", 0, None)];
        alerter.step(&chats, &asking, &looking);
        let later = Context {
            now: ms(HOLD_MS),
            ..looking
        };
        assert!(raises(&alerter.step(&chats, &asking, &later)).is_empty());

        let mut alerter = Alerter::default();
        alerter.step(&chats, &[session("a", "working", 0, None)], &cx(0));
        alerter.step(&chats, &asking, &cx(0));
        assert_eq!(
            raises(&alerter.step(&chats, &asking, &cx(HOLD_MS))).len(),
            1
        );
        let now_looking = Context {
            now: ms(HOLD_MS + 10),
            ..looking
        };
        assert_eq!(
            alerter.step(&chats, &asking, &now_looking),
            vec![Change::Withdraw {
                chat_id: "a".into()
            }]
        );
    }

    #[test]
    fn a_finished_turn_rings_until_the_chat_is_seen_anywhere() {
        let mut alerter = Alerter::default();
        let unseen = [chat("a", None, Some(0), 5_000)];
        alerter.step(&unseen, &[session("a", "working", 0, Some("t1"))], &cx(0));
        let done = [session("a", "idle", 5_000, Some("t2"))];
        alerter.step(&unseen, &done, &cx(5_000));
        assert_eq!(
            raises(&alerter.step(&unseen, &done, &cx(5_000 + HOLD_MS))),
            vec![("a".to_owned(), AlertKind::Finished)]
        );
        let seen = [chat("a", None, Some(9_000), 5_000)];
        assert_eq!(
            alerter.step(&seen, &done, &cx(9_000)),
            vec![Change::Withdraw {
                chat_id: "a".into()
            }]
        );
    }

    #[test]
    fn a_lost_heartbeat_stalls_only_while_this_device_is_linked() {
        let chats = [chat("a", None, Some(0), 0)];
        let working = [session("a", "working", 0, None)];
        let after = SESSION_STALE_MS + 1;
        let mut alerter = Alerter::default();
        alerter.step(&chats, &working, &cx(0));
        alerter.step(&chats, &working, &cx(after));
        assert_eq!(
            raises(&alerter.step(&chats, &working, &cx(after + HOLD_MS))),
            vec![("a".to_owned(), AlertKind::Stalled)]
        );

        let mut alerter = Alerter::default();
        let unlinked = |now| Context {
            linked: false,
            ..cx(now)
        };
        alerter.step(&chats, &working, &unlinked(0));
        alerter.step(&chats, &working, &unlinked(after));
        assert!(raises(&alerter.step(&chats, &working, &unlinked(after + HOLD_MS))).is_empty());
    }

    #[test]
    fn a_held_cause_is_dropped_while_the_app_is_in_front_and_not_replayed() {
        let mut alerter = Alerter::default();
        let chats = [chat("a", None, Some(0), 0)];
        alerter.step(&chats, &[session("a", "working", 0, None)], &cx(0));
        let failed = [session("a", "errored", 0, None)];
        let front = |now| Context {
            app_in_front: true,
            ..cx(now)
        };
        alerter.step(&chats, &failed, &front(0));
        assert!(alerter.step(&chats, &failed, &front(HOLD_MS)).is_empty());
        assert!(alerter.step(&chats, &failed, &cx(HOLD_MS * 3)).is_empty());
    }

    #[test]
    fn approvals_offer_one_tap_answers_only_when_allowed() {
        let (answer, actions) = answers_for(&approval(&["Yes", "No"], None));
        assert_eq!(
            actions,
            vec![
                AlertAction::Approve,
                AlertAction::Deny,
                AlertAction::Reply,
                AlertAction::Stop
            ]
        );
        let answer = answer.unwrap();
        assert_eq!(answer.approve.as_deref(), Some("Yes"));
        assert_eq!(answer.deny.as_deref(), Some("No"));

        let (answer, actions) = answers_for(&approval(
            &["Deny", "Allow once", "Stop"],
            Some(ApprovalContext {
                default_to_no: true,
                requires_user_interaction: false,
            }),
        ));
        assert_eq!(
            actions,
            vec![AlertAction::Deny, AlertAction::Reply, AlertAction::Stop]
        );
        assert_eq!(answer.unwrap().approve, None);

        let (answer, actions) = answers_for(&approval(
            &["Allow once", "Deny"],
            Some(ApprovalContext {
                default_to_no: false,
                requires_user_interaction: true,
            }),
        ));
        assert!(answer.is_none() && actions.is_empty());

        let (_, actions) = answers_for(&approval(&["Postgres", "SQLite"], None));
        assert_eq!(actions, vec![AlertAction::Reply, AlertAction::Stop]);
    }

    #[test]
    fn actions_map_to_what_the_host_takes() {
        let raised = Raised {
            chat_id: "a".into(),
            kind: AlertKind::NeedsYou,
            title: "Fix the build".into(),
            raised_at: t0(),
        };
        let alert = present(&raised, Some(&approval(&["Allow once", "Deny"], None)));
        assert_eq!(alert.body, "Claude wants to run `cargo test`. Allow it?");
        let answered = |label: &str| AlertCommand::Answer {
            request_id: "req".into(),
            answers: vec![UserInputAnswer {
                question_id: "q1".into(),
                labels: vec![label.into()],
            }],
        };
        assert_eq!(
            command_for(&alert, AlertAction::Approve, None),
            Some(answered("Allow once"))
        );
        assert_eq!(
            command_for(&alert, AlertAction::Deny, None),
            Some(answered("Deny"))
        );
        assert_eq!(
            command_for(&alert, AlertAction::Reply, Some("  use nextest  ")),
            Some(answered("use nextest"))
        );
        assert_eq!(command_for(&alert, AlertAction::Reply, Some(" ")), None);
        assert_eq!(
            command_for(&alert, AlertAction::Stop, None),
            Some(AlertCommand::Stop)
        );

        let finished = present(
            &Raised {
                kind: AlertKind::Finished,
                ..raised
            },
            None,
        );
        assert_eq!(command_for(&finished, AlertAction::Stop, None), None);
        assert_eq!(
            command_for(&finished, AlertAction::Reply, Some("ship it")),
            Some(AlertCommand::Message {
                text: "ship it".into()
            })
        );
    }

    #[test]
    fn a_question_waits_for_its_words_then_shows_without_them() {
        let mut board = Board::default();
        let raised = Raised {
            chat_id: "a".into(),
            kind: AlertKind::NeedsYou,
            title: "Fix the build".into(),
            raised_at: t0(),
        };
        board.apply(vec![Change::Raise(raised.clone())]);
        assert!(board.shown(t0()).is_empty());
        assert_eq!(board.wanting_question(), vec!["a".to_owned()]);
        assert_eq!(
            board.next_deadline(),
            Some(t0() + TimeDelta::milliseconds(QUESTION_WAIT_MS))
        );
        let late = board.shown(ms(QUESTION_WAIT_MS));
        assert_eq!(late.len(), 1);
        assert_eq!(late[0].body, "Waiting on your input");
        assert!(board.set_question("a", approval(&["Yes", "No"], None)));
        assert!(!board.set_question("a", approval(&["Yes", "No"], None)));
        assert_eq!(board.shown(t0())[0].actions.len(), 4);
        board.apply(vec![Change::Withdraw {
            chat_id: "a".into(),
        }]);
        assert!(board.shown(ms(QUESTION_WAIT_MS)).is_empty());
    }

    /// One step of a simulated fleet: each chat's row, where the owner looks, how much time passed.
    #[derive(Debug, Clone)]
    struct Frame {
        rows: Vec<(u8, bool, u8, bool)>,
        advance_ms: i64,
        focus: Option<usize>,
        app_in_front: bool,
        linked: bool,
        enabled: bool,
    }

    fn frame() -> impl Strategy<Value = Frame> {
        (
            proptest::collection::vec((0u8..4, any::<bool>(), 0u8..3, any::<bool>()), 3),
            prop_oneof![Just(0i64), 0i64..3_000, Just(SESSION_STALE_MS + 1)],
            proptest::option::of(0usize..3),
            any::<bool>(),
            prop_oneof![9 => Just(true), 1 => Just(false)],
            prop_oneof![9 => Just(true), 1 => Just(false)],
        )
            .prop_map(
                |(rows, advance_ms, focus, app_in_front, linked, enabled)| Frame {
                    rows,
                    advance_ms,
                    focus,
                    app_in_front,
                    linked,
                    enabled,
                },
            )
    }

    const IDS: [&str; 3] = ["a", "b", "side"];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Over any run of rows, focus and settings: what shows is exactly what the alerter says
        /// is live, every alert showing still has its cause, nothing rings for the chat on screen,
        /// for a side chat, on first sight, with alerts off, or twice for one step.
        #[test]
        fn alerts_follow_their_causes(frames in proptest::collection::vec(frame(), 1..40)) {
            let mut alerter = Alerter::default();
            let mut shown: BTreeMap<String, AlertKind> = BTreeMap::new();
            let mut now = 0i64;
            let mut beat = [0i64; 3];
            for (index, frame) in frames.iter().enumerate() {
                now += frame.advance_ms;
                let mut chats = Vec::new();
                let mut sessions = Vec::new();
                for (slot, (status, heartbeat, turn, seen)) in frame.rows.iter().enumerate() {
                    if *heartbeat {
                        beat[slot] = now;
                    }
                    let status = ["idle", "working", "awaitingInput", "errored"][*status as usize];
                    let parent = (IDS[slot] == "side").then_some("a");
                    let seen_at = if *seen { Some(now) } else { Some(now - 1) };
                    chats.push(chat(IDS[slot], parent, seen_at, now));
                    let turn = format!("t{turn}");
                    sessions.push(session(IDS[slot], status, beat[slot], Some(&turn)));
                }
                let focused = frame.focus.map(|slot| IDS[slot]);
                let context = Context {
                    now: ms(now),
                    focused_chat: focused,
                    app_in_front: frame.app_in_front,
                    linked: frame.linked,
                    prefs: AlertPrefs { enabled: frame.enabled, background_only: true },
                };
                let changes = alerter.step(&chats, &sessions, &context);
                for change in &changes {
                    match change {
                        Change::Raise(raised) => {
                            prop_assert!(index > 0, "first sight rang");
                            prop_assert!(raised.chat_id != "side", "a side chat rang");
                            prop_assert!(Some(raised.chat_id.as_str()) != focused, "the chat on screen rang");
                            prop_assert!(frame.enabled && !frame.app_in_front, "rang while held back");
                            shown.insert(raised.chat_id.clone(), raised.kind);
                        }
                        Change::Withdraw { chat_id } => {
                            prop_assert!(shown.remove(chat_id).is_some(), "withdrew what never showed");
                        }
                    }
                }
                prop_assert_eq!(&shown, alerter.live());
                for (chat_id, kind) in &shown {
                    prop_assert!(Some(chat_id.as_str()) != focused);
                    prop_assert!(frame.enabled);
                    let slot = IDS.iter().position(|id| id == chat_id).unwrap();
                    let row = Row {
                        phase: phase(&sessions[slot], ms(now)),
                        turn: None,
                        fresh: true,
                        unseen: chats[slot].unseen(),
                    };
                    prop_assert!(stands(*kind, &row), "{chat_id} shows {kind:?} past its cause");
                }
                prop_assert!(alerter.step(&chats, &sessions, &context).is_empty(), "a step repeated changed something");
            }
        }

        /// The board shows each raised alert once its words are in or its wait ran out, and
        /// nothing withdrawn.
        #[test]
        fn the_board_shows_what_was_raised_and_not_withdrawn(
            ops in proptest::collection::vec((0usize..3, any::<bool>(), 0usize..4, any::<bool>()), 1..30),
        ) {
            let kinds = [AlertKind::NeedsYou, AlertKind::Failed, AlertKind::Stalled, AlertKind::Finished];
            let mut board = Board::default();
            let mut model: BTreeMap<String, (AlertKind, bool)> = BTreeMap::new();
            for (slot, raise, kind, answered) in ops {
                let chat_id = IDS[slot].to_owned();
                if raise {
                    board.apply(vec![Change::Raise(Raised {
                        chat_id: chat_id.clone(),
                        kind: kinds[kind],
                        title: String::new(),
                        raised_at: t0(),
                    })]);
                    model.insert(chat_id.clone(), (kinds[kind], false));
                } else {
                    board.apply(vec![Change::Withdraw { chat_id: chat_id.clone() }]);
                    model.remove(&chat_id);
                }
                if answered && board.set_question(&chat_id, approval(&["Yes", "No"], None)) {
                    model.get_mut(&chat_id).unwrap().1 = true;
                }
            }
            let now: Vec<String> = board.shown(t0()).into_iter().map(|a| a.chat_id).collect();
            let expected: Vec<String> = model
                .iter()
                .filter(|(_, (kind, answered))| *kind != AlertKind::NeedsYou || *answered)
                .map(|(id, _)| id.clone())
                .collect();
            prop_assert_eq!(now, expected);
            let later = board.shown(ms(QUESTION_WAIT_MS)).len();
            prop_assert_eq!(later, model.len());
        }
    }
}
