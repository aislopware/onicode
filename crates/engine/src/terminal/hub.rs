//! The Terminal hub: a [`Topic`] per terminal over its [`Host`], fed by a reader thread on the
//! PTY, written by a writer thread, and finished by a thread that waits for the shell.
//!
//! Every byte the PTY writes is fed to the host and published under the terminal's one lock, so a
//! watcher that joins gets a snapshot as of exactly the cursor it starts from and then the bytes
//! after it; the topic's ring is the output ring a returning watcher resumes from. A watcher gets
//! its snapshot in two items, the renderable prefix and then the history behind it, so a far
//! viewer paints before a long scrollback has crossed the link.
//!
//! The PTY takes the size of the viewer that typed last ([`viewers`]). The program's OSC 7501
//! status rides the topic as [`TerminalEvent::Status`], and every change of any terminal's goes
//! out on [`TerminalHub::statuses`] for whatever alerts on it.
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_protocol::terminal::{
    Bytes, CloseTerminal, OpenTerminal, ProgramStatus, ReadForeground, ResizeTerminal,
    StopForeground, TerminalEvent, TerminalInput, TerminalSize, TerminalState, TerminalSummary,
    WatchTerminal,
};
use onicode_protocol::{Item, Outcome, TerminalId};
use onicode_term::host::Host;
use onicode_term::pty::{self, NativePty, Process, Spawned, Spawner, Spec};
use onicode_term::{label, shell, snapshot};
use tokio::sync::broadcast;

use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::session::Spawn;

mod viewers;

use viewers::Viewers;

/// Status changes a slow [`TerminalHub::statuses`] subscriber may fall behind by before it skips
/// to the newest.
const STATUS_BACKLOG: usize = 64;

/// One terminal's program status changed: its id and its most urgent record now, or none.
pub type StatusChange = (TerminalId, Option<ProgramStatus>);

/// What the PTY is read in.
const READ_BYTES: usize = 64 << 10;

/// How long the shell's last output may trail its exit before the exit note goes in.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(50);

/// A terminal as a watch's state: its size and whether it ended. The screen itself is the host's
/// snapshot, cut for each watcher that joins.
pub(crate) struct TerminalFeed;

impl Feed for TerminalFeed {
    type Event = TerminalEvent;
    type State = TerminalState;

    fn apply(state: &mut TerminalState, event: &TerminalEvent) {
        match event {
            TerminalEvent::Resized(size) => state.size = *size,
            TerminalEvent::Exited { .. } => state.exited = true,
            TerminalEvent::Status(status) => state.status.clone_from(status),
            TerminalEvent::Output(_) | TerminalEvent::Clipboard(_) | TerminalEvent::History(_) => {}
        }
    }

    fn weight(event: &TerminalEvent) -> usize {
        16 + match event {
            TerminalEvent::Output(bytes) | TerminalEvent::History(bytes) => bytes.0.len(),
            TerminalEvent::Clipboard(text) => text.len(),
            TerminalEvent::Status(status) => status.as_ref().map_or(0, |status| {
                [&status.app, &status.title, &status.message]
                    .iter()
                    .map(|text| text.as_ref().map_or(0, String::len))
                    .sum::<usize>()
            }),
            TerminalEvent::Resized(_) | TerminalEvent::Exited { .. } => 0,
        }
    }

    fn absorb(pending: &mut Pending<Self>, event: TerminalEvent) -> bool {
        // Output joins the output just before it: one feed of both bytes is the same as two. A
        // status replaces the status just before it.
        let Some(at) = pending.rposition(|_| true) else {
            return pending.append(event);
        };
        let merged = match (pending.event_mut(at), &event) {
            (TerminalEvent::Output(bytes), TerminalEvent::Output(more)) => {
                bytes.0.extend_from_slice(&more.0);
                true
            }
            (TerminalEvent::Status(status), TerminalEvent::Status(newer)) => {
                status.clone_from(newer);
                true
            }
            _ => false,
        };
        if !merged {
            return pending.append(event);
        }
        pending.reweigh(at);
        false
    }
}

/// The shell a terminal in a directory runs, and the terminfo name it runs as.
type Shell = dyn Fn(&str) -> (Spec, &'static str) + Send + Sync;

/// One terminal: its host, its topic and the PTY behind them.
struct Live {
    id: TerminalId,
    host: Host,
    topic: Topic<TerminalFeed>,
    cwd: String,
    /// `None` once the shell ended or the terminal closed, or when nothing was spawned.
    process: Option<Box<dyn Process>>,
    /// The writer thread's queue; dropped when the shell ends.
    writer: Option<mpsc::Sender<Vec<u8>>>,
    exited: bool,
    viewers: Viewers,
    statuses: broadcast::Sender<StatusChange>,
}

impl Live {
    /// Output from the PTY: fed to the host, its replies written back, and published.
    fn output(&mut self, bytes: &[u8]) {
        self.host.feed(bytes);
        let effects = self.host.take_effects();
        if !effects.pty.is_empty() {
            self.write(effects.pty);
        }
        let mut events = vec![TerminalEvent::Output(Bytes(bytes.to_vec()))];
        events.extend(effects.clipboard.into_iter().map(TerminalEvent::Clipboard));
        events.extend(effects.status.map(|status| self.status_changed(status)));
        self.topic.publish(events);
    }

    /// Tells the hub's status subscribers, and makes the event watchers get.
    fn status_changed(&self, status: Option<ProgramStatus>) -> TerminalEvent {
        // Nobody subscribed is no error.
        let _ = self.statuses.send((self.id, status.clone()));
        TerminalEvent::Status(status)
    }

    /// The PTY, the host and every watcher take `size`, in that order in the stream.
    fn apply_size(&mut self, size: TerminalSize) {
        if self.host.size() == size {
            return;
        }
        if let Some(process) = &self.process {
            process.resize(size);
        }
        self.host.resize(size);
        let _ = self.host.take_effects();
        self.topic.publish(vec![TerminalEvent::Resized(size)]);
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(writer) = &self.writer {
            let _ = writer.send(bytes);
        }
    }

    /// The shell ended: monocode's note goes into the grid, and the terminal stays.
    fn finish(&mut self, code: Option<i32>) {
        if self.exited {
            return;
        }
        let code_note = code.map(|code| format!(" ({code})")).unwrap_or_default();
        let note = format!("\r\n[process exited{code_note}]").into_bytes();
        self.host.feed(&note);
        // Its program's working, blocked and idle records end with it.
        self.host.exited();
        let status = self.host.take_effects().status;
        self.exited = true;
        self.writer = None;
        self.process = None;
        let mut events = vec![TerminalEvent::Output(Bytes(note))];
        events.extend(status.map(|status| self.status_changed(status)));
        events.push(TerminalEvent::Exited { code });
        self.topic.publish(events);
    }

    fn summary(&self, terminal: TerminalId) -> TerminalSummary {
        TerminalSummary {
            terminal,
            cwd: self.cwd.clone(),
            exited: self.exited,
            status: self.host.status(),
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // The hangup a closing shell expects, for it and its job.
        if let Some(process) = self.process.take() {
            process.hangup();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Inner {
    terminals: HashMap<TerminalId, Arc<Mutex<Live>>>,
    /// Topics made this run: each takes a fresh epoch.
    topics: u64,
}

pub struct TerminalHub {
    /// `None` spawns nothing: a terminal without a shell, for an app's tests.
    spawner: Option<Arc<dyn Spawner>>,
    shell: Arc<Shell>,
    spawn: Arc<dyn Spawn>,
    limits: Limits,
    /// When this engine run started (Unix ms): topic epochs are numbered from it.
    started: u64,
    inner: Mutex<Inner>,
    statuses: broadcast::Sender<StatusChange>,
}

impl TerminalHub {
    /// Login shells on the machine's PTYs.
    pub fn new(spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::build(
            Some(Arc::new(NativePty)),
            Arc::new(|cwd: &str| shell::spec(cwd, onicode_core::scm::terminal_search_path())),
            spawn,
            Limits::default(),
            onicode_core::time::now_ms(),
        )
    }

    /// Terminals without a shell behind them: what they show is only what the engine writes.
    /// For an app's tests, which must never start a shell.
    pub fn quiet(spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::build(
            None,
            Arc::new(|cwd: &str| {
                let spec = Spec {
                    cwd: cwd.into(),
                    ..Spec::default()
                };
                (spec, shell::FALLBACK_TERM)
            }),
            spawn,
            Limits::default(),
            onicode_core::time::now_ms(),
        )
    }

    pub(crate) fn build(
        spawner: Option<Arc<dyn Spawner>>,
        shell: Arc<Shell>,
        spawn: Arc<dyn Spawn>,
        limits: Limits,
        started: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            spawner,
            shell,
            spawn,
            limits,
            started,
            inner: Mutex::new(Inner::default()),
            statuses: broadcast::channel(STATUS_BACKLOG).0,
        })
    }

    /// Every terminal's program status changes from now on, for the alerts a needs-input report
    /// raises. A subscriber that falls more than a few dozen changes behind skips to the newest
    /// (`RecvError::Lagged`); [`Self::list`] has every terminal's status now.
    pub fn statuses(&self) -> broadcast::Receiver<StatusChange> {
        self.statuses.subscribe()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get(&self, terminal: TerminalId) -> Result<Arc<Mutex<Live>>, String> {
        self.lock()
            .terminals
            .get(&terminal)
            .cloned()
            .ok_or_else(|| "This terminal is closed".to_owned())
    }

    /// Runs `work` on the hub's spawner and waits for it.
    async fn blocking<T, W>(&self, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let (reply, answer) = futures::channel::oneshot::channel();
        self.spawn.spawn(Box::pin(async move {
            let _ = reply.send(work());
        }));
        answer
            .await
            .map_err(|_| "The engine stopped before this call finished".to_owned())?
    }

    /// `OpenTerminal`: a login shell on a new PTY. An id already open answers its summary.
    /// Building the shell's environment writes files and spawning forks, so both run off the
    /// caller's thread.
    pub async fn open(self: &Arc<Self>, params: OpenTerminal) -> Result<TerminalSummary, String> {
        if let Ok(live) = self.get(params.terminal) {
            return Ok(lock(&live).summary(params.terminal));
        }
        let hub = self.clone();
        self.blocking(move || Ok(hub.start(params))).await
    }

    fn start(&self, params: OpenTerminal) -> TerminalSummary {
        let OpenTerminal {
            terminal,
            viewer,
            cwd,
            size,
            colors,
        } = params;
        let (spec, term) = (self.shell)(&cwd);
        let mut host = Host::new(size, colors, term);
        let spawned = self.spawner.as_ref().and_then(|spawner| {
            spawner
                .spawn(&spec, size)
                .map_err(|error| {
                    // TerminalView prints why, and the terminal stays open on it.
                    let note = format!(
                        "\x1b[31mCould not start the shell ({}): {error}\x1b[0m",
                        spec.program
                    );
                    host.feed(note.as_bytes());
                    let _ = host.take_effects();
                })
                .ok()
        });
        let live = {
            let mut inner = self.lock();
            if let Some(open) = inner.terminals.get(&terminal) {
                // Another open of the same id won the race.
                if let Some(spawned) = spawned {
                    spawned.process.hangup();
                }
                return lock(open).summary(terminal);
            }
            inner.topics += 1;
            let epoch = self.started.wrapping_shl(20).wrapping_add(inner.topics);
            let state = TerminalState {
                size,
                screen: Bytes::default(),
                exited: false,
                status: None,
            };
            let mut viewers = Viewers::default();
            // The opener's grid is the PTY's.
            let _ = viewers.sized(viewer, size);
            let live = Arc::new(Mutex::new(Live {
                id: terminal,
                host,
                topic: Topic::new(epoch, state, self.limits),
                cwd,
                process: None,
                writer: None,
                exited: false,
                viewers,
                statuses: self.statuses.clone(),
            }));
            inner.terminals.insert(terminal, live.clone());
            live
        };
        if let Some(spawned) = spawned {
            run(&live, spawned);
        }
        lock(&live).summary(terminal)
    }

    /// `ListTerminals`: every terminal this engine hosts.
    pub fn list(&self) -> Vec<TerminalSummary> {
        let terminals: Vec<_> = self
            .lock()
            .terminals
            .iter()
            .map(|(id, live)| (*id, live.clone()))
            .collect();
        let mut list: Vec<_> = terminals
            .into_iter()
            .map(|(id, live)| lock(&live).summary(id))
            .collect();
        list.sort_by_key(|summary| summary.terminal);
        list
    }

    /// `WatchTerminal`: the host's snapshot, its renderable prefix and then its history, then
    /// its output from there. After `Resync` the stream ends; resubscribe with the last cursor.
    /// While the watch lasts, its viewer's size counts toward the PTY's.
    pub fn watch(
        &self,
        params: WatchTerminal,
    ) -> Result<BoxStream<'static, Item<TerminalState, TerminalEvent>>, String> {
        let WatchTerminal {
            terminal,
            viewer,
            since,
        } = params;
        let live = self.get(terminal)?;
        let (id, notify) = {
            let mut guard = lock(&live);
            let Live {
                host,
                topic,
                viewers,
                ..
            } = &mut *guard;
            viewers.joined(viewer);
            topic.join_with(since, |state| TerminalState {
                screen: Bytes(host.snapshot().unwrap_or_default()),
                ..state.clone()
            })
        };
        let draining = live.clone();
        let items = feed::stream(
            notify,
            move || lock(&draining).topic.drain(id),
            move || {
                let mut live = lock(&live);
                live.topic.leave(id);
                if let Some(size) = live.viewers.left(viewer) {
                    live.apply_size(size);
                }
            },
        );
        Ok(items
            .flat_map(|item| futures::stream::iter(split_snapshot(item)))
            .boxed())
    }

    /// `TerminalInput`: its viewer becomes the typist, so the PTY takes its size first; then the
    /// input is encoded from the host's modes now, and written.
    pub fn input(&self, params: TerminalInput) -> Result<Outcome, String> {
        let live = self.get(params.terminal)?;
        let mut live = lock(&live);
        if let Some(size) = live.viewers.typed(params.viewer) {
            live.apply_size(size);
        }
        let bytes = live.host.input(&params.input);
        if !bytes.is_empty() {
            live.write(bytes);
        }
        Ok(Outcome::Applied)
    }

    /// `ResizeTerminal`: the viewer's grid, which the PTY, the host and every watcher take, in
    /// that order in the stream, when the PTY follows that viewer.
    pub fn resize(&self, params: ResizeTerminal) -> Result<Outcome, String> {
        let live = self.get(params.terminal)?;
        let mut live = lock(&live);
        if let Some(size) = live.viewers.sized(params.viewer, params.size) {
            live.apply_size(size);
        }
        Ok(Outcome::Applied)
    }

    /// `CloseTerminal`: hangs up the shell and its job, and forgets the terminal.
    pub fn close(&self, params: CloseTerminal) -> Result<Outcome, String> {
        let removed = self.lock().terminals.remove(&params.terminal);
        if let Some(live) = removed {
            let mut live = lock(&live);
            if let Some(process) = live.process.take() {
                process.hangup();
            }
            live.writer = None;
            if !live.exited {
                live.exited = true;
                live.topic
                    .publish(vec![TerminalEvent::Exited { code: None }]);
            }
        }
        Ok(Outcome::Applied)
    }

    /// `ReadForeground`: the label of the command holding the terminal, unless it is the shell.
    pub async fn read_foreground(&self, params: ReadForeground) -> Result<Option<String>, String> {
        let live = self.get(params.terminal)?;
        let group = lock(&live)
            .process
            .as_ref()
            .and_then(|process| process.foreground());
        match group {
            Some(group) => self.blocking(move || Ok(label::process_label(group))).await,
            None => Ok(None),
        }
    }

    /// `StopForeground`: hangs up the job holding the terminal, leaving the shell at its prompt.
    pub fn stop_foreground(&self, params: StopForeground) -> Result<Outcome, String> {
        let live = self.get(params.terminal)?;
        let group = lock(&live)
            .process
            .as_ref()
            .and_then(|process| process.foreground());
        if let Some(group) = group {
            pty::hang_up_group(group);
        }
        Ok(Outcome::Applied)
    }
}

/// A snapshot item as two: the snapshot through READY, which paints, and its history in an
/// `Events` item that covers no numbers, so the cursor, the ring and resuming see one snapshot.
fn split_snapshot(
    item: Item<TerminalState, TerminalEvent>,
) -> Vec<Item<TerminalState, TerminalEvent>> {
    let Item::Snapshot { at, mut state } = item else {
        return vec![item];
    };
    if state.screen.0.is_empty() {
        return vec![Item::Snapshot { at, state }];
    }
    let (prefix, history) = snapshot::split(std::mem::take(&mut state.screen.0));
    state.screen = Bytes(prefix);
    vec![
        Item::Snapshot { at, state },
        Item::Events {
            first: at.seq + 1,
            last: at.seq,
            events: vec![TerminalEvent::History(Bytes(history))],
        },
    ]
}

/// Starts the threads that read the PTY, write it and wait for the shell. They hold the terminal
/// weakly: a closed terminal lets them end.
fn run(live: &Arc<Mutex<Live>>, spawned: Spawned) {
    let Spawned {
        process,
        mut output,
        input,
        wait,
    } = spawned;
    let (writes, queued) = mpsc::channel::<Vec<u8>>();
    {
        let mut live = lock(live);
        live.process = Some(process);
        live.writer = Some(writes);
    }
    let writer = std::thread::Builder::new()
        .name("onicode-terminal-write".into())
        .spawn(move || {
            let mut input = input;
            for bytes in queued {
                if input
                    .write_all(&bytes)
                    .and_then(|()| input.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
    let (drained, drained_wait) = mpsc::channel::<()>();
    let reading = Arc::downgrade(live);
    let reader = std::thread::Builder::new()
        .name("onicode-terminal-read".into())
        .spawn(move || {
            let mut buf = vec![0; READ_BYTES];
            loop {
                match output.read(&mut buf) {
                    Ok(0) => break,
                    Ok(read) => {
                        let Some(live) = reading.upgrade() else {
                            break;
                        };
                        if let Some(bytes) = buf.get(..read) {
                            lock(&live).output(bytes);
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = drained.send(());
        });
    let waiting: Weak<Mutex<Live>> = Arc::downgrade(live);
    let waiter = std::thread::Builder::new()
        .name("onicode-terminal-wait".into())
        .spawn(move || {
            let code = wait();
            // Output still in the PTY lands before the note; a job that keeps the PTY open does
            // not hold it back.
            let _ = drained_wait.recv_timeout(DRAIN_AFTER_EXIT);
            if let Some(live) = waiting.upgrade() {
                lock(&live).finish(code);
            }
        });
    if writer.is_err() || reader.is_err() || waiter.is_err() {
        // A terminal without its threads is dead: end its shell rather than leave it hung.
        let mut live = lock(live);
        if let Some(process) = live.process.take() {
            process.hangup();
        }
        live.finish(None);
    }
}

/// The size a terminal was opened or last resized at.
#[cfg(test)]
pub(super) fn size_of(
    hub: &TerminalHub,
    terminal: TerminalId,
) -> Option<onicode_protocol::terminal::TerminalSize> {
    hub.get(terminal).ok().map(|live| lock(&live).host.size())
}
