//! The Alerts group (`docs/onicode/decisions/alerts.md`): the moments an agent needs its owner,
//! decided by the engine that runs it and served to every viewer as [`WatchAlerts`]. A viewer
//! says what it has on screen and whether it presents the alerts itself with
//! [`SetViewportFocus`]; until one presents them, the engine posts plain banners of its own.
//!
//! An [`Alert`] carries what its notification shows and the [`AlertAction`]s it offers; [`command`]
//! turns an action into the Session call that does it, so every viewer answers alike.
use serde::{Deserialize, Serialize};

use crate::session::{self, Answer, Decide, Stop, Verdict};
use crate::{Call, Cursor, MessageId, Outcome, SessionId, TerminalId, Watch};

/// What an alert is about. One chat or terminal has at most one alert; a newer one replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlertKind {
    /// The agent asks for an approval or an answer.
    NeedsYou,
    /// The run failed, and nobody has looked since.
    Failed,
    /// The run finished, and nobody has looked since.
    Finished,
    /// The run went silent mid-turn for a long while.
    Stalled,
}

/// Whose alert it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Subject {
    /// A chat, by its id and the engine's row id the Workspace group names it by.
    Session { session: SessionId, row: i64 },
    /// A program in a terminal that reported its status (OSC 7501).
    Terminal(TerminalId),
}

/// What the owner is asked, when a notification can answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Need {
    /// A permission prompt.
    Approval { approval: String },
    /// A question with one item to choose or type.
    Question {
        question: String,
        /// The option that approves, by id.
        approve: Option<String>,
        /// The option that declines, by id; without one, Deny dismisses the question.
        deny: Option<String>,
        /// The item takes typed words.
        custom: bool,
    },
}

/// A button a notification offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlertAction {
    Approve,
    Deny,
    /// Typed words: the answer to the question, or a message to the agent.
    Reply,
    Stop,
}

impl AlertAction {
    /// Every action, in the order a notification shows them.
    pub const ALL: [Self; 4] = [Self::Approve, Self::Deny, Self::Reply, Self::Stop];

    /// The action's identifier in a notification category.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
            Self::Reply => "reply",
            Self::Stop => "stop",
        }
    }

    /// The action an identifier names.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// The button's words.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::Deny => "Deny",
            Self::Reply => "Reply",
            Self::Stop => "Stop",
        }
    }
}

/// Every set of actions an alert can carry, for a platform to register its categories up front.
pub const ACTION_SETS: [&[AlertAction]; 7] = [
    &[],
    &[AlertAction::Stop],
    &[AlertAction::Reply],
    &[AlertAction::Deny, AlertAction::Stop],
    &[AlertAction::Deny, AlertAction::Reply, AlertAction::Stop],
    &[AlertAction::Approve, AlertAction::Deny, AlertAction::Stop],
    &[
        AlertAction::Approve,
        AlertAction::Deny,
        AlertAction::Reply,
        AlertAction::Stop,
    ],
];

/// The notification category of a set of actions: one per set in [`ACTION_SETS`].
#[must_use]
pub fn category(actions: &[AlertAction]) -> String {
    if actions.is_empty() {
        return "onicode.alert".to_owned();
    }
    let ids: Vec<&str> = actions.iter().map(|action| action.id()).collect();
    format!("onicode.alert.{}", ids.join("-"))
}

/// The actions an alert of `kind` offers. A question with several items, or one that takes
/// several choices, is answered in the chat; a terminal's program is answered in its terminal.
#[must_use]
pub fn actions_for(kind: AlertKind, subject: &Subject, need: Option<&Need>) -> Vec<AlertAction> {
    if matches!(subject, Subject::Terminal(_)) {
        return Vec::new();
    }
    match (kind, need) {
        (AlertKind::NeedsYou, Some(Need::Approval { .. })) => {
            vec![AlertAction::Approve, AlertAction::Deny, AlertAction::Stop]
        }
        (
            AlertKind::NeedsYou,
            Some(Need::Question {
                approve, custom, ..
            }),
        ) => AlertAction::ALL
            .into_iter()
            .filter(|action| match action {
                AlertAction::Approve => approve.is_some(),
                AlertAction::Reply => *custom,
                AlertAction::Deny | AlertAction::Stop => true,
            })
            .collect(),
        (AlertKind::NeedsYou, None) | (AlertKind::Stalled, _) => vec![AlertAction::Stop],
        (AlertKind::Finished | AlertKind::Failed, _) => vec![AlertAction::Reply],
    }
}

/// One alert as a notification shows it. Posting another with the same `id` replaces it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    /// The notification's identifier: one per chat or terminal.
    pub id: String,
    pub subject: Subject,
    pub kind: AlertKind,
    /// The chat's title, or what the terminal's program calls itself.
    pub title: String,
    /// Plain text. A terminal program's words are its own and untrusted.
    pub body: String,
    /// The folder it runs in, which a viewer's project mutes go by.
    pub project: Option<String>,
    /// Unix ms.
    pub raised_at: u64,
    pub need: Option<Need>,
    pub actions: Vec<AlertAction>,
}

impl Alert {
    /// The notification category its actions register under.
    #[must_use]
    pub fn category(&self) -> String {
        category(&self.actions)
    }

    /// The chat's row, for a viewer that names sessions by row.
    #[must_use]
    pub fn row(&self) -> Option<i64> {
        match self.subject {
            Subject::Session { row, .. } => Some(row),
            Subject::Terminal(_) => None,
        }
    }
}

/// The identifier of a chat's alert: `onicode-alert:{row}`.
#[must_use]
pub fn session_alert_id(row: i64) -> String {
    format!("onicode-alert:{row}")
}

/// The identifier of a terminal's alert.
#[must_use]
pub fn terminal_alert_id(terminal: TerminalId) -> String {
    format!("onicode-alert-terminal:{terminal}")
}

/// The chat row a [`session_alert_id`] names.
#[must_use]
pub fn alert_row(id: &str) -> Option<i64> {
    id.strip_prefix("onicode-alert:")?.parse().ok()
}

/// What an action sends to the chat's engine: one Session call.
#[derive(Clone, Debug)]
pub enum AlertCommand {
    Decide(Decide),
    Answer(Answer),
    Stop(Stop),
    /// A message: it starts the next turn under the session's own settings.
    Send(session::Send),
}

/// The call `action` on `alert` makes, if the alert offers it. [`AlertAction::Reply`] needs
/// non-blank `text`.
#[must_use]
pub fn command(alert: &Alert, action: AlertAction, text: Option<&str>) -> Option<AlertCommand> {
    if !alert.actions.contains(&action) {
        return None;
    }
    let Subject::Session { session, .. } = alert.subject else {
        return None;
    };
    let text = text.map(str::trim).filter(|text| !text.is_empty());
    let answer = |question: &str, answer: String| {
        AlertCommand::Answer(Answer {
            session,
            question: question.to_owned(),
            answers: vec![vec![answer]],
        })
    };
    let decide = |approval: &str, verdict: Verdict| {
        AlertCommand::Decide(Decide {
            session,
            approval: approval.to_owned(),
            answer: verdict,
        })
    };
    match (action, &alert.need) {
        (AlertAction::Stop, _) => Some(AlertCommand::Stop(Stop { session })),
        (AlertAction::Approve, Some(Need::Approval { approval })) => {
            Some(decide(approval, Verdict::Allow))
        }
        (AlertAction::Deny, Some(Need::Approval { approval })) => {
            Some(decide(approval, Verdict::Deny))
        }
        (
            AlertAction::Approve,
            Some(Need::Question {
                question, approve, ..
            }),
        ) => Some(answer(question, approve.clone()?)),
        (AlertAction::Deny, Some(Need::Question { question, deny, .. })) => Some(match deny {
            Some(deny) => answer(question, deny.clone()),
            // A Deny dismisses a question that has no declining option.
            None => decide(question, Verdict::Deny),
        }),
        (
            AlertAction::Reply,
            Some(Need::Question {
                question,
                custom: true,
                ..
            }),
        ) => Some(answer(question, text?.to_owned())),
        (AlertAction::Reply, Some(Need::Question { custom: false, .. })) => None,
        (AlertAction::Reply, _) => Some(AlertCommand::Send(session::Send {
            session,
            message: MessageId::new(),
            text: text?.to_owned(),
            attachments: Vec::new(),
            turn: None,
        })),
        (AlertAction::Approve | AlertAction::Deny, None) => None,
    }
}

/// The alerts showing now, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertsState {
    pub alerts: Vec<Alert>,
}

impl AlertsState {
    /// Applies `event`, on the host and in every viewer's mirror alike.
    pub fn apply(&mut self, event: &AlertsEvent) {
        match event {
            AlertsEvent::Raised(alert) => {
                self.alerts.retain(|known| known.id != alert.id);
                self.alerts.push(Alert::clone(alert));
                self.alerts
                    .sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
            }
            AlertsEvent::Withdrawn(id) => self.alerts.retain(|known| &known.id != id),
        }
    }
}

/// One change to the alerts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertsEvent {
    /// Show this, replacing whatever shows under its id.
    Raised(Box<Alert>),
    /// Take back what shows under this id: its cause ended, or someone looked.
    Withdrawn(String),
}

/// Watch the engine's alerts: a snapshot (or, with a `since` the engine's ring still covers, only
/// the events after it), then every event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchAlerts {
    pub since: Option<Cursor>,
}

impl Watch for WatchAlerts {
    type Event = AlertsEvent;
    type Params = Self;
    type State = AlertsState;

    const NAME: &'static str = "WatchAlerts";
}

/// What a viewport has on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OnScreen {
    /// A chat, by the engine's row id.
    Session(i64),
    Terminal(TerminalId),
}

/// A viewport's report: whether one of its windows has focus, what it shows there, and whether
/// the viewer presents the alerts itself. The engine forgets a report 20 s after it came, so a
/// viewer repeats it while it lives (every 5 s) and a viewer that went away holds nothing back.
/// A chat on screen in a focused window never alerts, and showing it marks it seen for every
/// device. While no viewer presents, the engine posts plain banners itself, as the last report's
/// `banners` and `sound` say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetViewportFocus {
    /// The viewport's own id, minted once per run.
    pub viewport: String,
    pub focused: bool,
    pub on_screen: Option<OnScreen>,
    /// The viewer posts the alerts it watches, with their actions.
    pub presents: bool,
    /// The owner wants agent banners on this machine.
    pub banners: bool,
    /// Banners play a sound.
    pub sound: bool,
}

impl Call for SetViewportFocus {
    type Params = Self;
    type Reply = Outcome;

    const NAME: &'static str = "SetViewportFocus";
}
