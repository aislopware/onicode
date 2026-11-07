//! A Mac running agents for its owner stays awake under a turn
//! (`docs/onicode/decisions/keep-awake.md`).
//!
//! While any of the engine's sessions has a turn open (working, or waiting on its owner) or one of
//! its terminals runs a program in the foreground, the engine holds one
//! `PreventUserIdleSystemSleep` assertion: the work is the reference count, so a second turn never
//! takes a second assertion. It lets go [`LINGER_MS`] after the last one ends, so back-to-back
//! turns hold one. Only idle system sleep is held off: the display still sleeps, and a closed lid
//! or the Sleep menu still sleeps the Mac. The engine setting `keep_awake_while_agents_run` (on by
//! default) turns it off.
//!
//! The decision is the pure [`KeepAwake`] on a clock the caller passes; the assertion sits behind
//! [`PowerAssertion`], so no test touches the machine's power state.
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_protocol::Item;
use onicode_protocol::terminal::{ProgramState, ReadForeground};
use onicode_protocol::workspace::{WatchWorkspace, WorkspaceEvent, WorkspaceState};

use crate::settings::SettingsHub;
use crate::terminal::TerminalHub;
use crate::workspace::WorkspaceHub;

mod power;
#[cfg(test)]
mod tests;

pub use power::system;

/// The name `pmset -g assertions` shows.
pub const REASON: &str = "Onicode: an agent or a terminal command is running";

/// How long the assertion outlives the last turn or command.
pub const LINGER_MS: u64 = 10_000;

/// How often the terminals and the setting are looked at again.
const POLL: Duration = Duration::from_secs(5);

/// What wants the Mac awake now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Load {
    /// The engine setting allows it.
    pub allowed: bool,
    /// Sessions with a turn open.
    pub turns: usize,
    /// Terminals running a program in the foreground.
    pub commands: usize,
}

impl Load {
    fn busy(self) -> bool {
        self.turns + self.commands > 0
    }
}

/// What the assertion must do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Take,
    Release,
}

/// The engine's one assertion, counted by what holds it.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeepAwake {
    held: bool,
    /// When the load went to nothing while held (Unix ms).
    idle_since: Option<u64>,
}

impl KeepAwake {
    /// Whether the assertion is held.
    #[must_use]
    pub fn held(&self) -> bool {
        self.held
    }

    /// When a held assertion goes if nothing else comes.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        self.idle_since.map(|since| since.saturating_add(LINGER_MS))
    }

    /// Takes in the load at `now`; what the assertion must do, if anything.
    pub fn step(&mut self, load: Load, now: u64) -> Option<Power> {
        if !load.allowed {
            self.idle_since = None;
            return std::mem::take(&mut self.held).then_some(Power::Release);
        }
        if load.busy() {
            self.idle_since = None;
            return (!std::mem::replace(&mut self.held, true)).then_some(Power::Take);
        }
        if !self.held {
            return None;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now.saturating_sub(since) >= LINGER_MS {
            self.held = false;
            self.idle_since = None;
            return Some(Power::Release);
        }
        None
    }

    /// The system refused the assertion: the next step asks again.
    pub fn refused(&mut self) {
        self.held = false;
        self.idle_since = None;
    }
}

/// The power assertion itself.
pub trait PowerAssertion: Send {
    /// Holds it; `false` when the system refused.
    fn take(&mut self, reason: &str) -> bool;
    /// Lets it go; nothing when none is held.
    fn release(&mut self);
}

/// [`KeepAwake`] acting on a [`PowerAssertion`].
pub struct Keeper<P: PowerAssertion> {
    awake: KeepAwake,
    power: P,
}

impl<P: PowerAssertion> Keeper<P> {
    /// Holding nothing yet.
    pub fn new(power: P) -> Self {
        Self {
            awake: KeepAwake::default(),
            power,
        }
    }

    /// Takes in the load at `now` and takes or releases the assertion as it says.
    pub fn apply(&mut self, load: Load, now: u64) {
        match self.awake.step(load, now) {
            Some(Power::Take) => {
                if !self.power.take(REASON) {
                    self.awake.refused();
                }
            }
            Some(Power::Release) => self.power.release(),
            None => {}
        }
    }

    /// When a held assertion is due to go.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        self.awake.deadline()
    }

    /// Whether the assertion is held.
    #[must_use]
    pub fn held(&self) -> bool {
        self.awake.held()
    }

    /// The assertion backend, for a test to read.
    pub fn power(&self) -> &P {
        &self.power
    }
}

impl<P: PowerAssertion> Drop for Keeper<P> {
    fn drop(&mut self) {
        self.power.release();
    }
}

/// The sessions with a turn open, as the workspace's watch says.
fn busy_rows(busy: &mut HashSet<i64>, item: Item<WorkspaceState, WorkspaceEvent>) {
    match item {
        Item::Snapshot { state, .. } => {
            *busy = state
                .sessions
                .iter()
                .filter(|summary| summary.busy)
                .map(|summary| summary.session.id)
                .collect();
        }
        Item::Events { events, .. } => {
            for event in events {
                match event {
                    WorkspaceEvent::Session(summary) if summary.busy => {
                        busy.insert(summary.session.id);
                    }
                    WorkspaceEvent::Session(summary) => {
                        busy.remove(&summary.session.id);
                    }
                    WorkspaceEvent::Removed(row) => {
                        busy.remove(&row);
                    }
                    _ => {}
                }
            }
        }
        Item::Resync => {}
    }
}

/// Terminals running a program in the foreground: a job other than the shell holds the terminal,
/// or its program says it works or waits on its owner.
async fn commands(terminals: &TerminalHub) -> usize {
    let mut count = 0;
    for summary in terminals.list() {
        if summary.exited {
            continue;
        }
        let reported = summary.status.as_ref().is_some_and(|status| {
            matches!(status.state, ProgramState::Working | ProgramState::Blocked)
        });
        let foreground = reported
            || terminals
                .read_foreground(ReadForeground {
                    terminal: summary.terminal,
                })
                .await
                .is_ok_and(|label| label.is_some());
        if foreground {
            count += 1;
        }
    }
    count
}

/// Holds the assertion through `power` while the engine works, until the future is dropped,
/// which lets it go. The engine process runs it beside its socket.
pub async fn run(
    workspace: Arc<WorkspaceHub>,
    terminals: Arc<TerminalHub>,
    settings: Arc<SettingsHub>,
    power: Box<dyn PowerAssertion>,
) {
    let mut keeper = Keeper::new(power);
    let mut busy = HashSet::new();
    let mut board: Option<BoxStream<'static, Item<WorkspaceState, WorkspaceEvent>>> = None;
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut commands_now = 0;
    loop {
        if board.is_none() {
            board = workspace.watch(WatchWorkspace { since: None }).ok();
        }
        let now = onicode_protocol::time::now_ms();
        let load = Load {
            allowed: settings.current().keep_awake_while_agents_run,
            turns: busy.len(),
            commands: commands_now,
        };
        keeper.apply(load, now);
        let linger = keeper
            .deadline()
            .map(|at| Duration::from_millis(at.saturating_sub(now)));
        tokio::select! {
            item = async {
                match board.as_mut() {
                    Some(items) => items.next().await,
                    None => std::future::pending().await,
                }
            } => match item {
                None | Some(Item::Resync) => board = None,
                Some(item) => busy_rows(&mut busy, item),
            },
            _ = poll.tick() => commands_now = commands(&terminals).await,
            () = async {
                match linger {
                    Some(wait) => tokio::time::sleep(wait).await,
                    None => std::future::pending().await,
                }
            } => {}
        }
    }
}

impl PowerAssertion for Box<dyn PowerAssertion> {
    fn take(&mut self, reason: &str) -> bool {
        (**self).take(reason)
    }

    fn release(&mut self) {
        (**self).release();
    }
}
