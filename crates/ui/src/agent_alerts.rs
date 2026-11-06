//! Agent alerts on this Mac (`docs/onicode/decisions/alerts.md`). The engine decides them and
//! publishes them on `WatchAlerts` into [`AppState::agent_alerts`]; this shows each through
//! `onicode_notify::center` with its buttons, takes down what the engine drops, and runs what the
//! owner picks over the RPC the composer uses.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{App, Entity};
use onicode_doc::SessionCommandPayload;
use onicode_notify::center::{self, Action, Category, Note, Response, Style};
use onicode_proto::alerts::{self, AgentAlert, AlertAction, AlertCommand};
use onicode_rpc::methods;

use crate::state::{AppState, EngineHandle};

/// How long an action that launched the app waits for the engine.
const ENGINE_WAIT: Duration = Duration::from_secs(30);
const ENGINE_POLL: Duration = Duration::from_millis(250);

static PRESENTING: AtomicBool = AtomicBool::new(false);

/// Whether this app shows the agent alerts (it is an app bundle and holds the center). Until it
/// says so in its focus reports, the engine posts plain banners itself.
pub fn presenting() -> bool {
    PRESENTING.load(Ordering::SeqCst)
}

/// Take the notification center and follow `state`'s alerts. Call while the app finishes
/// launching.
pub fn init(state: Entity<AppState>, cx: &mut App) {
    // The center answers on its own thread; hop to the app through a channel.
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Response>();
    let installed = center::install(&categories(), move |response| {
        let _sent = tx.unbounded_send(response);
    });
    PRESENTING.store(installed, Ordering::SeqCst);
    if !installed {
        return;
    }
    let mut shown = BTreeMap::new();
    cx.observe(&state, move |state, cx| {
        let (post, withdraw) = sync(&mut shown, &state.read(cx).agent_alerts);
        center::withdraw(&withdraw);
        for alert in &post {
            if let Some(note) = note(alert) {
                center::post(&note);
            }
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Some(response) = rx.next().await {
            cx.update(|cx| respond(response, &state, cx));
        }
    })
    .detach();
}

/// One category per action set the alerts use.
fn categories() -> Vec<Category> {
    alerts::ACTION_SETS
        .iter()
        .map(|actions| Category {
            id: alerts::category(actions),
            actions: actions
                .iter()
                .map(|&action| Action {
                    id: action.id().to_owned(),
                    title: action.label().to_owned(),
                    style: match action {
                        AlertAction::Reply => Style::Reply {
                            button: "Send".to_owned(),
                            placeholder: "Reply to the agent".to_owned(),
                        },
                        AlertAction::Stop => Style::Destructive,
                        AlertAction::Approve | AlertAction::Deny => Style::Plain,
                    },
                })
                .collect(),
        })
        .collect()
}

/// Bring `shown` to `wanted`: the alerts to post (new or changed) and the ids to take down.
fn sync(
    shown: &mut BTreeMap<String, AgentAlert>,
    wanted: &[AgentAlert],
) -> (Vec<AgentAlert>, Vec<String>) {
    let withdraw: Vec<String> = shown
        .keys()
        .filter(|id| !wanted.iter().any(|alert| alert.id() == id.as_str()))
        .cloned()
        .collect();
    for id in &withdraw {
        shown.remove(id);
    }
    let mut post = Vec::new();
    for alert in wanted {
        if shown.get(alert.id()) != Some(alert) {
            shown.insert(alert.id().to_owned(), alert.clone());
            post.push(alert.clone());
        }
    }
    (post, withdraw)
}

fn note(alert: &AgentAlert) -> Option<Note> {
    Some(Note {
        id: alert.id().to_owned(),
        thread: alert.chat_id.clone(),
        title: alert.title.clone(),
        body: alert.body.clone(),
        category: alert.category(),
        info: serde_json::to_string(alert).ok()?,
        time_sensitive: alert.urgent(),
    })
}

/// A click opens the chat; a button runs its command.
fn respond(response: Response, state: &Entity<AppState>, cx: &mut App) {
    let Some(alert) = response
        .info
        .as_deref()
        .and_then(|info| serde_json::from_str::<AgentAlert>(info).ok())
    else {
        return;
    };
    let Some(action) = response.action.as_deref() else {
        crate::open_notification_target(alert.chat_id, state, cx);
        return;
    };
    let Some((method, params)) = AlertAction::from_id(action)
        .and_then(|action| alerts::command_for(&alert, action, response.text.as_deref()))
        .and_then(|command| request(&alert.chat_id, command))
    else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let Some(engine) = engine(&state, cx).await else {
            tracing::warn!(method, "no engine for an alert's action");
            return;
        };
        if let Err(error) = engine.client().call(method, params).await {
            tracing::warn!(method, %error, "an alert's action failed");
        }
    })
    .detach();
}

/// The engine, waiting a while for it after a launch.
async fn engine(state: &Entity<AppState>, cx: &mut gpui::AsyncApp) -> Option<EngineHandle> {
    let tries = ENGINE_WAIT.as_millis() / ENGINE_POLL.as_millis();
    for _ in 0..tries {
        if let Some(engine) = cx.update(|cx| state.read(cx).engine().cloned()) {
            return Some(engine);
        }
        cx.background_executor().timer(ENGINE_POLL).await;
    }
    None
}

/// The RPC that carries `command` for `chat_id`.
fn request(chat_id: &str, command: AlertCommand) -> Option<(&'static str, serde_json::Value)> {
    let queue_command = |command: SessionCommandPayload| {
        let command = serde_json::to_value(&command).ok()?;
        Some((
            methods::QUEUE_COMMAND,
            serde_json::json!({ "chatId": chat_id, "command": command }),
        ))
    };
    match command {
        AlertCommand::Answer {
            request_id,
            answers,
        } => queue_command(SessionCommandPayload::RespondInput {
            request_id,
            answers,
        }),
        AlertCommand::Stop => queue_command(SessionCommandPayload::Interrupt {}),
        // Steered into a live turn where the agent takes that, else sent as the next turn.
        AlertCommand::Message { text } => Some((
            methods::QUEUE_MESSAGE,
            serde_json::json!({
                "chatId": chat_id,
                "text": text,
                "attachments": [],
                "holdForTurnEnd": false,
            }),
        )),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use onicode_proto::alerts::AlertKind;

    use super::*;

    fn alert(chat: &str, body: &str) -> AgentAlert {
        AgentAlert {
            chat_id: chat.to_owned(),
            kind: AlertKind::Finished,
            title: "Fix the build".to_owned(),
            body: body.to_owned(),
            raised_at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
            answer: None,
            actions: vec![AlertAction::Reply],
        }
    }

    #[test]
    fn sync_posts_new_and_changed_alerts_once_and_withdraws_dropped_ones() {
        let mut shown = BTreeMap::new();
        let (post, withdraw) = sync(&mut shown, &[alert("a", "one"), alert("b", "one")]);
        assert_eq!(post.len(), 2);
        assert!(withdraw.is_empty());

        let (post, withdraw) = sync(&mut shown, &[alert("a", "one"), alert("b", "one")]);
        assert!(post.is_empty() && withdraw.is_empty(), "nothing moved");

        let (post, withdraw) = sync(&mut shown, &[alert("a", "two")]);
        assert_eq!(post, vec![alert("a", "two")]);
        assert_eq!(withdraw, vec!["b".to_owned()]);
        assert_eq!(shown.len(), 1);
    }

    #[test]
    fn every_action_set_has_a_category_and_reply_takes_text() {
        let categories = categories();
        assert_eq!(categories.len(), alerts::ACTION_SETS.len());
        for (category, actions) in categories.iter().zip(alerts::ACTION_SETS) {
            assert_eq!(category.id, alerts::category(actions));
            assert_eq!(category.actions.len(), actions.len());
        }
        let reply = categories
            .iter()
            .flat_map(|category| &category.actions)
            .find(|action| action.id == AlertAction::Reply.id())
            .unwrap();
        assert!(matches!(reply.style, Style::Reply { .. }));
    }

    #[test]
    fn commands_ride_the_composers_rpcs() {
        let (method, params) = request("c", AlertCommand::Stop).unwrap();
        assert_eq!(method, methods::QUEUE_COMMAND);
        assert_eq!(params["chatId"], "c");
        assert_eq!(params["command"]["kind"], "interrupt");

        let (method, params) = request(
            "c",
            AlertCommand::Answer {
                request_id: "r".to_owned(),
                answers: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(method, methods::QUEUE_COMMAND);
        assert_eq!(params["command"]["kind"], "respondInput");
        assert_eq!(params["command"]["requestId"], "r");

        let (method, params) = request(
            "c",
            AlertCommand::Message {
                text: "use the other branch".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(method, methods::QUEUE_MESSAGE);
        assert_eq!(params["holdForTurnEnd"], false);
        assert_eq!(params["text"], "use the other branch");
    }

    #[test]
    fn a_note_carries_the_alert_back() {
        let alert = alert("a", "Run finished");
        let note = note(&alert).unwrap();
        assert_eq!(note.id, "a");
        assert_eq!(note.category, alert.category());
        assert!(!note.time_sensitive);
        let back: AgentAlert = serde_json::from_str(&note.info).unwrap();
        assert_eq!(back, alert);
    }
}
