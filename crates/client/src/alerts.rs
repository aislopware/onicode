//! Agent alerts on a device without an engine (`docs/onicode/decisions/alerts.md`): the shared
//! [`Alerter`] over this device's mirror of the registry, stepped after every re-derivation and at
//! its own deadlines, and a [`Board`] that pairs a needs-you alert with the open question of its
//! session. The platform reads [`Client::alerts`] after [`ClientEvent::AlertsChanged`], posts
//! local notifications for them, and hands a button back to [`Client::act_on_alert`].
//!
//! Alerts are stepped while the app runs, in front or briefly behind; a suspended app steps
//! nothing, so nothing alerts until it runs again (and then a chat seen for the first time seeds
//! silently).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use onicode_proto::alerts::{
    self, AgentAlert, AlertAction, AlertCommand, AlertPrefs, Alerter, Board, Question,
};
use tokio::sync::Notify;

use crate::client::ClientInner;
use crate::connectivity::ConnectivityState;
use crate::error::{ClientError, Result};
use crate::events::ClientEvent;
use crate::session::{BusyPolicy, SendRequest};
use crate::{Client, lock};

/// How often a raised question is looked for while its session loads.
const QUESTION_POLL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct Alerts {
    state: Mutex<State>,
    wake: Arc<Notify>,
}

#[derive(Default)]
struct State {
    alerter: Alerter,
    board: Board,
    shown: Vec<AgentAlert>,
    /// The chat on screen while the app is in front.
    focus: Option<String>,
    in_front: bool,
    prefs: AlertPrefs,
}

impl Alerts {
    /// Step soon (the rows, the focus or the links changed).
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// When the next step is due without any change.
    fn deadline(&self, now: DateTime<Utc>) -> Option<Duration> {
        let state = lock(&self.state);
        let due = [state.alerter.next_deadline(), state.board.next_deadline()]
            .into_iter()
            .flatten()
            .min()
            .map(|at| (at - now).to_std().unwrap_or_default());
        let polling = (!state.board.wanting_question().is_empty()).then_some(QUESTION_POLL);
        [due, polling].into_iter().flatten().min()
    }
}

/// Steps the alerts for the client's life.
pub(crate) fn start(inner: &Arc<ClientInner>) {
    let weak = Arc::downgrade(inner);
    let cancel = inner.cancel.clone();
    let wake = inner.alerts.wake.clone();
    crate::runtime::spawn(async move {
        loop {
            let Some(deadline) = weak
                .upgrade()
                .map(|inner| inner.alerts.deadline(Utc::now()))
            else {
                return;
            };
            tokio::select! {
                () = cancel.cancelled() => return,
                () = wake.notified() => {}
                () = async {
                    match deadline {
                        Some(after) => tokio::time::sleep(after).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.step_alerts();
        }
    });
}

impl ClientInner {
    fn step_alerts(self: &Arc<Self>) {
        let now = Utc::now();
        let (rows, _) = self.workspace.state();
        let linked = !matches!(
            self.connectivity().state,
            ConnectivityState::Offline | ConnectivityState::Reconnecting
        );
        let wanting = {
            let mut state = lock(&self.alerts.state);
            let focus = state.focus.clone().filter(|_| state.in_front);
            let cx = alerts::Context {
                now,
                focused_chat: focus.as_deref(),
                app_in_front: state.in_front,
                linked,
                prefs: state.prefs,
            };
            let changes = state.alerter.step(&rows.chats, &rows.sessions, &cx);
            state.board.apply(changes);
            state.board.wanting_question()
        };
        // Outside the lock: opening a session may re-derive the workspace.
        let questions: Vec<(String, Question)> = wanting
            .into_iter()
            .filter_map(|chat_id| {
                let question = self.open_question(&chat_id)?;
                Some((chat_id, question))
            })
            .collect();
        let changed = {
            let mut state = lock(&self.alerts.state);
            for (chat_id, question) in questions {
                state.board.set_question(&chat_id, question);
            }
            let shown = state.board.shown(now);
            let changed = shown != state.shown;
            state.shown = shown;
            changed
        };
        if changed {
            self.events.alerts();
        }
    }

    /// The question `chat_id` waits on, opening its session to read it.
    fn open_question(self: &Arc<Self>, chat_id: &str) -> Option<Question> {
        let client = Client {
            inner: self.clone(),
        };
        let input = client
            .open_session(chat_id)
            .ok()?
            .composer()
            .open_input
            .clone()?;
        Some(Question {
            request_id: input.request_id,
            questions: input.questions,
        })
    }
}

impl Client {
    /// The agent alerts to show now, oldest first. Each one's `chat_id` is its notification id:
    /// a newer alert for a chat replaces the older.
    pub fn alerts(&self) -> Vec<AgentAlert> {
        lock(&self.inner.alerts.state).shown.clone()
    }

    /// What the owner is looking at: the chat on screen, and whether the app is in front. No
    /// alert is raised for that chat, and while the app is in front none is raised at all unless
    /// the preferences say otherwise.
    pub fn set_alert_focus(&self, chat_id: Option<String>, in_front: bool) {
        {
            let mut state = lock(&self.inner.alerts.state);
            state.focus = chat_id;
            state.in_front = in_front;
        }
        self.inner.alerts.wake();
    }

    pub fn set_alert_prefs(&self, prefs: AlertPrefs) {
        lock(&self.inner.alerts.state).prefs = prefs;
        self.inner.alerts.wake();
    }

    /// Run a notification's button: `alert` is the alert as JSON (what the platform stored with
    /// the notification), `action` its [`AlertAction`] id, `text` what was typed for Reply.
    pub fn act_on_alert(&self, alert: &str, action: &str, text: Option<String>) -> Result<()> {
        let alert: AgentAlert = serde_json::from_str(alert)
            .map_err(|error| ClientError::InvalidArgument(format!("alert: {error}")))?;
        let action = AlertAction::from_id(action)
            .ok_or_else(|| ClientError::InvalidArgument(format!("alert action {action}")))?;
        let command = alerts::command_for(&alert, action, text.as_deref()).ok_or_else(|| {
            ClientError::InvalidArgument(format!("{} takes no {}", alert.chat_id, action.id()))
        })?;
        let session = self.open_session(&alert.chat_id)?;
        match command {
            AlertCommand::Answer {
                request_id,
                answers,
            } => session.respond_input(&request_id, answers),
            AlertCommand::Stop => session.interrupt(),
            AlertCommand::Message { text } => session
                .send(SendRequest {
                    text,
                    busy: BusyPolicy::Steer,
                    ..Default::default()
                })
                .map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use onicode_proto::alerts::AlertKind;

    use super::*;
    use crate::config::{ClientConfig, Mode};

    fn demo() -> (tempfile::TempDir, Client) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = ClientConfig::new(dir.path());
        config.device_id = "ios-test".into();
        let client = Client::new(
            config,
            Mode::Demo(Default::default()),
            Arc::new(crate::events::NullListener),
        )
        .unwrap();
        (dir, client)
    }

    #[test]
    fn nothing_is_held_so_nothing_is_due() {
        assert_eq!(Alerts::default().deadline(Utc::now()), None);
    }

    #[test]
    fn the_rows_already_there_never_ring() {
        let (_dir, client) = demo();
        client.inner.step_alerts();
        client.inner.step_alerts();
        assert!(client.alerts().is_empty());
    }

    #[test]
    fn an_action_the_alert_does_not_carry_is_refused() {
        let (_dir, client) = demo();
        let alert = AgentAlert {
            chat_id: "nowhere".into(),
            kind: AlertKind::Finished,
            title: "Fix the build".into(),
            body: "Run finished".into(),
            raised_at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
            answer: None,
            actions: vec![AlertAction::Reply],
        };
        let json = serde_json::to_string(&alert).unwrap();
        assert!(matches!(
            client.act_on_alert(&json, "stop", None),
            Err(ClientError::InvalidArgument(_))
        ));
        assert!(matches!(
            client.act_on_alert(&json, "launch", None),
            Err(ClientError::InvalidArgument(_))
        ));
        assert!(matches!(
            client.act_on_alert("{}", "reply", Some("hi".into())),
            Err(ClientError::InvalidArgument(_))
        ));
        assert!(matches!(
            client.act_on_alert(&json, "reply", Some("hi".into())),
            Err(ClientError::NotFound(_))
        ));
    }
}
