//! The pump: feeds the [`AlertsHub`] from the engine's own hubs and steps it on its deadlines.
//! It watches the workspace, each session while a turn runs in it, every terminal's program
//! status and the engine's settings (for the projects' notification preferences), and marks seen
//! in the workspace what a focused viewport shows.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use onicode_protocol::engine_settings::{EngineSettings, EngineSettingsEvent, WatchEngineSettings};
use onicode_protocol::session::{SessionEvent, SessionState, WatchSession};
use onicode_protocol::workspace::{MarkSeen, WatchWorkspace, WorkspaceEvent, WorkspaceState};
use onicode_protocol::{Item, SessionId};
use tokio::sync::broadcast::error::RecvError;

use super::hub::{AlertsHub, Wants};
use crate::session::SessionHub;
use crate::settings::SettingsHub;
use crate::terminal::TerminalHub;
use crate::workspace::WorkspaceHub;

type SessionItems = BoxStream<'static, Item<SessionState, SessionEvent>>;
type WorkspaceItems = BoxStream<'static, Item<WorkspaceState, WorkspaceEvent>>;
type SettingsItems = BoxStream<'static, Item<EngineSettings, EngineSettingsEvent>>;

/// The longest the pump sleeps with nothing due.
const IDLE: Duration = Duration::from_secs(60);
/// How long the pump waits before asking again for a watch the engine could not serve.
const RETRY: Duration = Duration::from_secs(1);
/// How often the terminals are listed again, for ones that closed.
const RECONCILE: Duration = Duration::from_secs(30);

fn now() -> u64 {
    onicode_protocol::time::now_ms()
}

/// The sessions watched while their turns run, by row.
#[derive(Default)]
struct Sessions {
    watched: BTreeMap<i64, SessionItems>,
    /// Where the next poll starts, so a chatty session never starves the others.
    turn: usize,
}

impl Sessions {
    fn open(&mut self, hub: &Arc<SessionHub>, row: i64, session: SessionId) {
        if self.watched.contains_key(&row) {
            return;
        }
        let params = WatchSession {
            session,
            since: None,
            window_turns: 0,
        };
        if let Ok(items) = hub.watch(params) {
            self.watched.insert(row, items);
        }
    }

    /// A fresh watch of `row` after `Resync`, joined before the old one goes, so the session
    /// stays live between them.
    fn renew(&mut self, hub: &Arc<SessionHub>, row: i64, session: Option<SessionId>) {
        let Some(session) = session else {
            self.watched.remove(&row);
            return;
        };
        let params = WatchSession {
            session,
            since: None,
            window_turns: 0,
        };
        match hub.watch(params) {
            Ok(items) => {
                let old = self.watched.insert(row, items);
                drop(old);
            }
            Err(_) => {
                self.watched.remove(&row);
            }
        }
    }

    /// The next item of any watched session.
    fn next(&mut self) -> impl Future<Output = (i64, Option<Item<SessionState, SessionEvent>>)> {
        futures::future::poll_fn(move |cx| {
            let rows: Vec<i64> = self.watched.keys().copied().collect();
            if rows.is_empty() {
                return Poll::Pending;
            }
            self.turn = self.turn.wrapping_add(1);
            let start = self.turn % rows.len();
            for row in rows.iter().cycle().skip(start).take(rows.len()) {
                if let Some(items) = self.watched.get_mut(row)
                    && let Poll::Ready(item) = items.poll_next_unpin(cx)
                {
                    return Poll::Ready((*row, item));
                }
            }
            Poll::Pending
        })
    }
}

async fn next_of<T>(items: &mut Option<BoxStream<'static, T>>) -> Option<T> {
    match items {
        Some(items) => items.next().await,
        None => std::future::pending().await,
    }
}

/// Does what the workspace's news asks: drains and ends the turns that ended (in that order, so
/// the status a turn ended on is known), lets go of sessions that went, watches turns that began.
fn apply(hub: &AlertsHub, sessions: &Arc<SessionHub>, watched: &mut Sessions, wants: Wants) {
    for (row, session) in wants.ended {
        watched.open(sessions, row, session);
        if let Some(items) = watched.watched.get_mut(&row) {
            // Everything the session published before its turn ended is queued already.
            while let Some(Some(item)) = items.next().now_or_never() {
                if matches!(item, Item::Resync) {
                    break;
                }
                hub.session(row, item, now());
            }
        }
        if !hub.turn_ended(row, now()) {
            watched.watched.remove(&row);
        }
    }
    for row in wants.unwatch {
        watched.watched.remove(&row);
    }
    for (row, session) in wants.watch {
        watched.open(sessions, row, session);
    }
}

/// Feeds `hub` from `workspace`, `sessions`, `terminals` and `settings` until the terminal hub
/// goes. The engine process runs it beside its socket.
pub async fn pump(
    hub: Arc<AlertsHub>,
    workspace: Arc<WorkspaceHub>,
    sessions: Arc<SessionHub>,
    terminals: Arc<TerminalHub>,
    settings: Arc<SettingsHub>,
) {
    let mut statuses = terminals.statuses();
    // The stored mutes hold before the first step, so none rings after a restart.
    let mut rules: Option<SettingsItems> = Some(follow_settings(&hub, &settings));
    hub.terminals(&terminals.list(), false, now());
    let mut board: Option<WorkspaceItems> = None;
    let mut watched = Sessions::default();
    let mut ids: BTreeMap<i64, SessionId> = BTreeMap::new();
    let mut reconcile = tokio::time::interval(RECONCILE);
    reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if board.is_none() {
            match workspace.watch(WatchWorkspace { since: None }) {
                Ok(items) => board = Some(items),
                Err(_) => {
                    tokio::time::sleep(RETRY).await;
                    continue;
                }
            }
        }
        let step = hub.step(now());
        for row in step.seen {
            let _ = workspace.mark_seen(None, MarkSeen { session: row }).await;
        }
        let wait = step
            .next
            .map_or(IDLE, |at| Duration::from_millis(at.saturating_sub(now())))
            .min(IDLE);
        tokio::select! {
            item = next_of(&mut board) => match item {
                // A fresh snapshot reconciles whatever was missed.
                None | Some(Item::Resync) => board = None,
                Some(item) => {
                    let wants = hub.workspace(item, now());
                    for (row, session) in wants.watch.iter().chain(&wants.ended) {
                        ids.insert(*row, *session);
                    }
                    for row in &wants.unwatch {
                        ids.remove(row);
                    }
                    apply(&hub, &sessions, &mut watched, wants);
                }
            },
            (row, item) = watched.next() => match item {
                None | Some(Item::Resync) => watched.renew(&sessions, row, ids.get(&row).copied()),
                Some(item) => hub.session(row, item, now()),
            },
            change = statuses.recv() => match change {
                Ok(_) | Err(RecvError::Lagged(_)) => hub.terminals(&terminals.list(), true, now()),
                Err(RecvError::Closed) => return,
            },
            item = next_of(&mut rules) => match item {
                None | Some(Item::Resync) => rules = Some(follow_settings(&hub, &settings)),
                Some(_) => hub.set_projects(settings.current().project_notifications),
            },
            _ = reconcile.tick() => hub.terminals(&terminals.list(), true, now()),
            () = hub.woken() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// A watch of the engine's settings, with the projects' notification preferences handed to `hub`
/// as they stand when it starts: each item after it hands them over again.
pub(super) fn follow_settings(hub: &AlertsHub, settings: &Arc<SettingsHub>) -> SettingsItems {
    let items = settings.watch(&WatchEngineSettings { since: None });
    hub.set_projects(settings.current().project_notifications);
    items
}
