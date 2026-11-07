//! The Terminal hub: a [`Topic`] per terminal over its [`Host`], fed by a reader thread on the
//! PTY, written by a writer thread, and finished by a thread that waits for the shell.
//!
//! Every byte the PTY writes is fed to the host and published under the terminal's one lock, so a
//! watcher that joins gets a snapshot as of exactly the cursor it starts from and then the bytes
//! after it; the topic's ring is the output ring a returning watcher resumes from.
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::time::Duration;

use futures::stream::BoxStream;
use onicode_protocol::terminal::{
    Bytes, CloseTerminal, OpenTerminal, ReadForeground, ResizeTerminal, StopForeground,
    TerminalEvent, TerminalInput, TerminalState, TerminalSummary, WatchTerminal,
};
use onicode_protocol::{Item, Outcome, TerminalId};
use onicode_term::host::Host;
use onicode_term::pty::{self, NativePty, Process, Spawned, Spawner, Spec};
use onicode_term::{label, shell};

use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::session::Spawn;

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
            TerminalEvent::Output(_) | TerminalEvent::Clipboard(_) => {}
        }
    }

    fn weight(event: &TerminalEvent) -> usize {
        16 + match event {
            TerminalEvent::Output(bytes) => bytes.0.len(),
            TerminalEvent::Clipboard(text) => text.len(),
            TerminalEvent::Resized(_) | TerminalEvent::Exited { .. } => 0,
        }
    }

    fn absorb(pending: &mut Pending<Self>, event: TerminalEvent) -> bool {
        // Output joins the output just before it: one feed of both bytes is the same as two.
        match event {
            TerminalEvent::Output(more) => {
                let Some(at) = pending.rposition(|_| true) else {
                    return pending.append(TerminalEvent::Output(more));
                };
                match pending.event_mut(at) {
                    TerminalEvent::Output(bytes) => bytes.0.extend_from_slice(&more.0),
                    _ => return pending.append(TerminalEvent::Output(more)),
                }
                pending.reweigh(at);
                false
            }
            other => pending.append(other),
        }
    }
}

/// The shell a terminal in a directory runs, and the terminfo name it runs as.
type Shell = dyn Fn(&str) -> (Spec, &'static str) + Send + Sync;

/// One terminal: its host, its topic and the PTY behind them.
struct Live {
    host: Host,
    topic: Topic<TerminalFeed>,
    cwd: String,
    /// `None` once the shell ended or the terminal closed, or when nothing was spawned.
    process: Option<Box<dyn Process>>,
    /// The writer thread's queue; dropped when the shell ends.
    writer: Option<mpsc::Sender<Vec<u8>>>,
    exited: bool,
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
        self.topic.publish(events);
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
        let _ = self.host.take_effects();
        self.exited = true;
        self.writer = None;
        self.process = None;
        self.topic.publish(vec![
            TerminalEvent::Output(Bytes(note)),
            TerminalEvent::Exited { code },
        ]);
    }

    fn summary(&self, terminal: TerminalId) -> TerminalSummary {
        TerminalSummary {
            terminal,
            cwd: self.cwd.clone(),
            exited: self.exited,
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
        })
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
            };
            let live = Arc::new(Mutex::new(Live {
                host,
                topic: Topic::new(epoch, state, self.limits),
                cwd,
                process: None,
                writer: None,
                exited: false,
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

    /// `WatchTerminal`: the host's snapshot, then its output from there. After `Resync` the
    /// stream ends; resubscribe with the last cursor.
    pub fn watch(
        &self,
        params: WatchTerminal,
    ) -> Result<BoxStream<'static, Item<TerminalState, TerminalEvent>>, String> {
        let live = self.get(params.terminal)?;
        let (id, notify) = {
            let mut guard = lock(&live);
            let Live { host, topic, .. } = &mut *guard;
            topic.join_with(params.since, |state| TerminalState {
                screen: Bytes(host.snapshot().unwrap_or_default()),
                ..state.clone()
            })
        };
        let draining = live.clone();
        Ok(feed::stream(
            notify,
            move || lock(&draining).topic.drain(id),
            move || {
                lock(&live).topic.leave(id);
            },
        ))
    }

    /// `TerminalInput`: encoded from the host's modes now, and written.
    pub fn input(&self, params: TerminalInput) -> Result<Outcome, String> {
        let live = self.get(params.terminal)?;
        let live = lock(&live);
        let bytes = live.host.input(&params.input);
        if !bytes.is_empty() {
            live.write(bytes);
        }
        Ok(Outcome::Applied)
    }

    /// `ResizeTerminal`: the PTY, the host and every watcher take the new size, in that order in
    /// the stream.
    pub fn resize(&self, params: ResizeTerminal) -> Result<Outcome, String> {
        let live = self.get(params.terminal)?;
        let mut live = lock(&live);
        if live.host.size() != params.size {
            if let Some(process) = &live.process {
                process.resize(params.size);
            }
            live.host.resize(params.size);
            let _ = live.host.take_effects();
            live.topic
                .publish(vec![TerminalEvent::Resized(params.size)]);
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
