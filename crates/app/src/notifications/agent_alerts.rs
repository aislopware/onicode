//! Agent alerts on this Mac (`docs/onicode/decisions/alerts.md`): each engine decides its own
//! and serves them as `WatchAlerts`; this posts them through the notification center in
//! monocode's look, with their buttons, and makes each button's call to the engine that raised
//! it. The app's own engine is watched, and so is every other engine whose projects the window
//! shows, so an agent working on another Mac reaches the Mac in front of its owner as one here
//! does. The shell tells this, on every poll, what the window shows and the owner's notification
//! preferences; it reports that to each engine with `SetViewportFocus`, so the engine never alerts
//! for the chat on screen, marks it seen, and leaves the posting to the app while the app runs.
//! Project mutes are not reported: each engine keeps its projects' own and holds back what they
//! mute before any viewer hears of it (`shell/project_notifications.rs`).
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::App;
use onicode_protocol::alerts::{
    self, Alert, AlertCommand, AlertsEvent, AlertsState, OnScreen, SetViewportFocus, Subject,
    WatchAlerts,
};
use onicode_protocol::session::{self, Answer, Decide, Stop};
use onicode_protocol::{EngineId, OpId, SessionKey};

use super::delivery::{self, Permission};
use super::platform::{self, AlertResponse};
use crate::engine_client::{Engine, WatchUpdate, Watching};

/// How often the report is repeated while nothing changes; the engine forgets one after 20 s.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// How long a button pressed before its engine's alerts arrived waits for them: a press that
/// launched the app lands before the first snapshot.
const BUTTON_WAIT: Duration = Duration::from_secs(30);

/// What the shell knows at a poll.
pub(crate) struct Policy {
    /// Settings → General → Notifications.
    pub(crate) enabled: bool,
    pub(crate) sound: bool,
    pub(crate) permission: Permission,
    pub(crate) window_focused: bool,
    /// The chat on screen in the window: the engine that holds it (`None`, the app's own) and
    /// the row that engine keeps it at.
    pub(crate) on_screen: Option<(Option<EngineId>, i64)>,
    /// Every other engine whose projects the window shows.
    pub(crate) peers: Vec<EngineId>,
}

/// The identifier this Mac posts `alert` of the engine `target` under: a chat on another engine
/// by the row the app names it ([`crate::engine_client::peer_row`]), so a click opens it here;
/// anything else as the engine named it.
fn shown_id(target: Option<&EngineId>, alert: &Alert) -> String {
    match (target, alert.subject) {
        (Some(engine), Subject::Session { session, .. }) => {
            alerts::session_alert_id(crate::engine_client::peer_row(&SessionKey {
                engine: engine.clone(),
                session,
            }))
        }
        _ => alert.id.clone(),
    }
}

/// One engine's alerts, as this app watches them.
struct Watched {
    engine: Engine,
    watching: Watching<AlertsState, AlertsEvent>,
    /// What the engine shows, by the engine's ids.
    alerts: BTreeMap<String, Alert>,
    /// The engine's first snapshot arrived.
    primed: bool,
    reported: Option<(SetViewportFocus, Instant)>,
}

impl Watched {
    fn new(engine: Engine) -> Self {
        Self {
            watching: engine.watch::<WatchAlerts>(|since| WatchAlerts { since }),
            engine,
            alerts: BTreeMap::new(),
            primed: false,
            reported: None,
        }
    }

    /// Takes in what the engine's watch brought since the last poll.
    fn take_in(&mut self) {
        while let Ok(update) = self.watching.updates.try_recv() {
            match update {
                WatchUpdate::Snapshot(state) => {
                    self.primed = true;
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
    }

    /// Tells the engine what the window shows, in its rows and paths, when that changed, and
    /// every [`HEARTBEAT`].
    fn report(&mut self, viewport: &str, policy: &Policy, cx: &mut App) {
        let target = self.engine.target();
        let on_screen = policy
            .on_screen
            .as_ref()
            .filter(|(engine, _)| engine.as_ref() == target)
            .map(|(_, row)| OnScreen::Session(*row));
        let report = SetViewportFocus {
            viewport: viewport.to_owned(),
            focused: policy.window_focused,
            on_screen,
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
}

/// The app's side of the engines' alerts.
pub(crate) struct AgentAlerts {
    /// The app's own engine's, first, then each other engine's the window shows projects of.
    engines: Vec<Watched>,
    /// What this app took in, by the id it posts under, with the raise it was: each raise is
    /// posted (or let pass) once, and a suppressed one is never replayed.
    handled: BTreeMap<String, u64>,
    /// Buttons pressed for alerts not known yet, while an engine's first snapshot is still on
    /// its way, with when each was taken in ([`BUTTON_WAIT`]).
    waiting: Vec<(AlertResponse, Instant)>,
    viewport: String,
}

impl AgentAlerts {
    /// Watches `engine`'s alerts from now on.
    pub(crate) fn new(engine: &Engine) -> Self {
        Self {
            engines: vec![Watched::new(engine.clone())],
            handled: BTreeMap::new(),
            waiting: Vec::new(),
            viewport: OpId::new().to_string(),
        }
    }

    /// One poll: follows the engines the window shows projects of, reports what the window
    /// shows to each, takes in their alerts, posts what is new and withdraws what went, and runs
    /// the buttons pressed since.
    pub(crate) fn sync(&mut self, policy: &Policy, cx: &mut App) {
        self.follow(&policy.peers);
        for watched in &mut self.engines {
            watched.report(&self.viewport, policy, cx);
            watched.take_in();
        }
        self.present(policy);
        let mut responses = std::mem::take(&mut self.waiting);
        let now = Instant::now();
        responses.extend(
            platform::drain_alert_responses()
                .into_iter()
                .map(|response| (response, now)),
        );
        let unprimed = self.engines.iter().any(|watched| !watched.primed);
        for (response, at) in responses {
            if !self.act(&response, cx) && unprimed && at.elapsed() < BUTTON_WAIT {
                self.waiting.push((response, at));
            }
        }
    }

    /// What to send as the app quits: each engine reported to hears at once that this viewport
    /// neither shows nor posts anything, so its own banners take over now rather than once the
    /// last report ages out 20 s later.
    pub(crate) fn closing(&mut self) -> impl Future<Output = ()> + 'static + use<> {
        let calls: Vec<_> = self
            .engines
            .iter_mut()
            .filter_map(|watched| {
                let (last, _) = watched.reported.take()?;
                let report = SetViewportFocus {
                    focused: false,
                    on_screen: None,
                    presents: false,
                    ..last
                };
                Some(watched.engine.call::<SetViewportFocus>(None, report))
            })
            .collect();
        async move {
            futures::future::join_all(calls).await;
        }
    }

    /// Watches each engine of `peers` not watched yet, and lets go of the others' watches: what
    /// they showed goes in [`Self::present`].
    fn follow(&mut self, peers: &[EngineId]) {
        self.engines.retain(|watched| {
            watched
                .engine
                .target()
                .is_none_or(|target| peers.contains(target))
        });
        let Some(own) = self.engines.first().map(|watched| watched.engine.clone()) else {
            return;
        };
        for peer in peers {
            let watched = self
                .engines
                .iter()
                .any(|watched| watched.engine.target() == Some(peer));
            if !watched {
                self.engines.push(Watched::new(own.on(peer.clone())));
            }
        }
    }

    fn present(&mut self, policy: &Policy) {
        let shown: BTreeMap<String, (Option<&EngineId>, &Alert)> = self
            .engines
            .iter()
            .flat_map(|watched| {
                let target = watched.engine.target();
                watched
                    .alerts
                    .values()
                    .map(move |alert| (shown_id(target, alert), (target, alert)))
            })
            .collect();
        let gone: Vec<String> = self
            .handled
            .keys()
            .filter(|id| !shown.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            self.handled.remove(&id);
            platform::dismiss(&id);
        }
        for (id, (target, alert)) in &shown {
            if self.handled.get(id) == Some(&alert.raised_at) {
                continue;
            }
            self.handled.insert(id.clone(), alert.raised_at);
            // The engine already held back what the project mutes say.
            let visible = alert.row().is_some_and(|row| {
                policy
                    .on_screen
                    .as_ref()
                    .is_some_and(|(engine, held)| engine.as_ref() == *target && *held == row)
            });
            if delivery::should_notify(
                policy.enabled,
                policy.permission,
                policy.window_focused,
                visible,
            ) {
                let mut posted = (*alert).clone();
                posted.id.clone_from(id);
                platform::show_alert(&posted, policy.sound);
            }
        }
    }

    /// Makes the call a button stands for on the engine that raised the alert, under a fresh
    /// op; what it came to reaches every viewer through that engine's watches, and the engine
    /// withdraws the alert once its cause ends. `false` when no engine shows the alert.
    fn act(&self, response: &AlertResponse, cx: &mut App) -> bool {
        let Some((engine, alert)) = self.engines.iter().find_map(|watched| {
            let target = watched.engine.target();
            watched
                .alerts
                .values()
                .find(|alert| shown_id(target, alert) == response.id)
                .map(|alert| (&watched.engine, alert))
        }) else {
            return false;
        };
        let Some(command) = alerts::command(alert, response.action, response.text.as_deref())
        else {
            return true;
        };
        let op = Some(OpId::new());
        let executor = cx.background_executor();
        match command {
            AlertCommand::Decide(body) => {
                executor.spawn(engine.call::<Decide>(op, body)).detach();
            }
            AlertCommand::Answer(body) => {
                executor.spawn(engine.call::<Answer>(op, body)).detach();
            }
            AlertCommand::Stop(body) => {
                executor.spawn(engine.call::<Stop>(op, body)).detach();
            }
            AlertCommand::Send(body) => {
                executor
                    .spawn(engine.call::<session::Send>(op, body))
                    .detach();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use onicode_protocol::alerts::AlertKind;

    use super::*;

    fn studio() -> EngineId {
        EngineId("nStudio".into())
    }

    #[test]
    fn a_chat_on_another_engine_is_posted_under_the_apps_row_for_it() {
        let session = onicode_protocol::SessionId::new();
        let alert = Alert {
            id: alerts::session_alert_id(7),
            subject: Subject::Session { session, row: 7 },
            kind: AlertKind::Finished,
            title: "Fix the flaky test".into(),
            body: "Claude finished".into(),
            project: Some("/Users/me/api".into()),
            raised_at: 1,
            need: None,
            actions: Vec::new(),
        };
        assert_eq!(shown_id(None, &alert), alert.id);
        let row = crate::engine_client::peer_row(&SessionKey {
            engine: studio(),
            session,
        });
        let id = shown_id(Some(&studio()), &alert);
        assert_eq!(id, alerts::session_alert_id(row));
        assert_eq!(
            delivery::Target::from_tag(&id),
            Some(delivery::Target::Session(row)),
            "a click opens the chat as the app names it"
        );
    }
}
