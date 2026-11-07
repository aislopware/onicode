//! Agent alerts on this Mac (`docs/onicode/decisions/alerts.md`): the engine decides them and
//! serves them as `WatchAlerts`; this posts them through the notification center in monocode's
//! look, with their buttons, and makes each button's call to the engine. The shell tells it, on
//! every poll, what the window shows and the owner's notification preferences; it reports that to
//! the engine with `SetViewportFocus`, so the engine never alerts for the chat on screen, marks it
//! seen, and leaves the posting to the app while the app runs.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::App;
use onicode_protocol::OpId;
use onicode_protocol::alerts::{
    self, Alert, AlertCommand, AlertKind, AlertsEvent, AlertsState, OnScreen, SetViewportFocus,
    WatchAlerts,
};
use onicode_protocol::session::{self, Answer, Decide, Stop};

use super::delivery::{self, Permission};
use super::platform::{self, AlertResponse};
use crate::engine_client::{Engine, WatchUpdate, Watching};

/// How often the report is repeated while nothing changes; the engine forgets one after 20 s.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// What the shell knows at a poll.
pub(crate) struct Policy<'a> {
    /// Settings → General → Notifications.
    pub(crate) enabled: bool,
    pub(crate) sound: bool,
    pub(crate) permission: Permission,
    pub(crate) window_focused: bool,
    /// The chat on screen in the window, by row.
    pub(crate) on_screen: Option<i64>,
    /// Whether the project at a path allows a category ("agentInput", "agentFinished") now:
    /// monocode's project mutes.
    pub(crate) allows: &'a dyn Fn(&str, &str) -> bool,
}

/// The app's side of the engine's alerts.
pub(crate) struct AgentAlerts {
    engine: Engine,
    watching: Watching<AlertsState, AlertsEvent>,
    /// What the engine shows, by id.
    alerts: BTreeMap<String, Alert>,
    /// What this app took in, by id, with the raise it was: each raise is posted (or let pass)
    /// once, and a suppressed one is never replayed.
    handled: BTreeMap<String, u64>,
    viewport: String,
    reported: Option<(SetViewportFocus, Instant)>,
}

impl AgentAlerts {
    /// Watches `engine`'s alerts from now on.
    pub(crate) fn new(engine: &Engine) -> Self {
        Self {
            engine: engine.clone(),
            watching: engine.watch::<WatchAlerts>(|since| WatchAlerts { since }),
            alerts: BTreeMap::new(),
            handled: BTreeMap::new(),
            viewport: OpId::new().to_string(),
            reported: None,
        }
    }

    /// One poll: reports what the window shows, takes in the engine's alerts, posts what is new
    /// and withdraws what went, and runs the buttons pressed since.
    pub(crate) fn sync(&mut self, policy: &Policy<'_>, cx: &mut App) {
        self.report(policy, cx);
        while let Ok(update) = self.watching.updates.try_recv() {
            match update {
                WatchUpdate::Snapshot(state) => {
                    self.alerts = state
                        .alerts
                        .into_iter()
                        .map(|alert| (alert.id.clone(), alert))
                        .collect();
                }
                WatchUpdate::Events(events) => {
                    for event in events {
                        match event {
                            AlertsEvent::Raised(alert) => {
                                self.alerts.insert(alert.id.clone(), *alert);
                            }
                            AlertsEvent::Withdrawn(id) => {
                                self.alerts.remove(&id);
                            }
                        }
                    }
                }
            }
        }
        self.present(policy);
        for response in platform::drain_alert_responses() {
            self.act(&response, cx);
        }
    }

    /// Tells the engine what the window shows when that changed, and every [`HEARTBEAT`].
    fn report(&mut self, policy: &Policy<'_>, cx: &mut App) {
        let report = SetViewportFocus {
            viewport: self.viewport.clone(),
            focused: policy.window_focused,
            on_screen: policy.on_screen.map(OnScreen::Session),
            presents: policy.permission != Permission::Unsupported,
            banners: policy.enabled,
            sound: policy.sound,
        };
        let due = self
            .reported
            .as_ref()
            .is_none_or(|(last, at)| *last != report || at.elapsed() >= HEARTBEAT);
        if !due {
            return;
        }
        self.reported = Some((report.clone(), Instant::now()));
        cx.background_executor()
            .spawn(self.engine.call::<SetViewportFocus>(None, report))
            .detach();
    }

    fn present(&mut self, policy: &Policy<'_>) {
        let gone: Vec<String> = self
            .handled
            .keys()
            .filter(|id| !self.alerts.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.handled.remove(&id);
            platform::dismiss(&id);
        }
        for alert in self.alerts.values() {
            if self.handled.get(&alert.id) == Some(&alert.raised_at) {
                continue;
            }
            self.handled.insert(alert.id.clone(), alert.raised_at);
            let category = match alert.kind {
                AlertKind::NeedsYou => "agentInput",
                AlertKind::Failed | AlertKind::Finished | AlertKind::Stalled => "agentFinished",
            };
            let allowed = alert
                .project
                .as_deref()
                .is_none_or(|project| (policy.allows)(project, category));
            let visible = alert.row().is_some_and(|row| policy.on_screen == Some(row));
            if allowed
                && delivery::should_notify(
                    policy.enabled,
                    policy.permission,
                    policy.window_focused,
                    visible,
                )
            {
                platform::show_alert(alert, policy.sound);
            }
        }
    }

    /// Makes the call a button stands for, under a fresh op; what it came to reaches every
    /// viewer through the engine's watches, and the engine withdraws the alert once its cause
    /// ends.
    fn act(&self, response: &AlertResponse, cx: &mut App) {
        let Some(alert) = self.alerts.get(&response.id) else {
            return;
        };
        let Some(command) = alerts::command(alert, response.action, response.text.as_deref())
        else {
            return;
        };
        let op = Some(OpId::new());
        let executor = cx.background_executor();
        match command {
            AlertCommand::Decide(body) => {
                executor
                    .spawn(self.engine.call::<Decide>(op, body))
                    .detach();
            }
            AlertCommand::Answer(body) => {
                executor
                    .spawn(self.engine.call::<Answer>(op, body))
                    .detach();
            }
            AlertCommand::Stop(body) => {
                executor.spawn(self.engine.call::<Stop>(op, body)).detach();
            }
            AlertCommand::Send(body) => {
                executor
                    .spawn(self.engine.call::<session::Send>(op, body))
                    .detach();
            }
        }
    }
}
