//! Terminals — PTY sessions owned by this device (feature-inventory §3.4; port of
//! onicode's `terminals.ts` over `portable-pty`).
//!
//! - `open` spawns the user's login shell in the chat's cwd; `subscribe` replays a
//!   bounded 1MB window (resumable via `afterSeq`) then tails live output — sent
//!   as soon as it is read, paced at [`TERMINAL_OUTPUT_BATCH_MS`] only while it
//!   streams; data rides base64 (PTY bytes ≠ UTF-8).
//! - Live shells survive subscriber detach — a detached session is the user's
//!   running process, kept until its tab is explicitly closed or the engine exits.
//!   Only EXITED sessions expire (30min TTL on their inert replay buffers), and
//!   [`MAX_TERMINALS`] bounds leakage from renderers that lost their tab state.
//! - Ownership: M5 is single-user local — every IPC/relay caller is the device
//!   owner, so the per-user owner re-checks from onicode's Router land with real
//!   multi-account auth in M6.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::{mpsc, watch};

use onicode_doc::TERMINAL_OUTPUT_BATCH_MS;
use onicode_proto::{TerminalActivity, TerminalEvent, TerminalSession, TerminalStatus};

pub mod link;
mod program_status;
mod replies;
use program_status::{ProgramTracker, SUPPORT_REPLY};

use crate::{EngineError, new_id};

const MAX_TERMINALS: usize = 32;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_REPLAY_BYTES: usize = 1024 * 1024;
const EXITED_TTL: Duration = Duration::from_secs(30 * 60);
const REAPER_INTERVAL: Duration = Duration::from_secs(60);

struct LiveTerminal {
    chat_id: Option<String>,
    seen_at: Option<chrono::DateTime<chrono::Utc>>,
    programs: ProgramTracker,
    // Keep the private action script alive until the shell exits or the tab is closed.
    initial_script: Option<tempfile::NamedTempFile>,
    /// The shell's process id: something else in the PTY's foreground is a
    /// program running ([`Terminals::status`]).
    shell_pid: Option<u32>,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    /// Live streams, each flagged when its viewer answers terminal queries
    /// itself (an app from before host replies).
    subscribers: Vec<(mpsc::UnboundedSender<TerminalEvent>, bool)>,
    /// Answers terminal queries from beside the PTY ([`replies`]).
    replies: replies::HostReplies,
    replay: VecDeque<TerminalEvent>,
    replay_bytes: usize,
    seq: u64,
    last_active_at: std::time::Instant,
    exited: bool,
    /// Program-detection replies in the current window (rate-limited: see
    /// `pump_output`).
    support_replies: u32,
    support_window: std::time::Instant,
    /// Highest input seq applied per remote viewer, so input replayed after
    /// a path change (direct link ↔ relay) is applied exactly once.
    applied_inputs: HashMap<String, u64>,
}

/// Remote viewers whose input seqs one terminal remembers.
const MAX_INPUT_CLIENTS: usize = 16;

/// Program-detection replies one terminal writes per [`SUPPORT_WINDOW`].
/// Detection needs one per program launch; output that keeps asking faster
/// is answered no further until the window turns over.
const MAX_SUPPORT_REPLIES: u32 = 8;
const SUPPORT_WINDOW: Duration = Duration::from_secs(10);

impl LiveTerminal {
    /// Stamp a seq, append to the bounded replay window, and fan out to live
    /// subscribers. On `Exit` the subscriber senders are dropped so every
    /// attached stream ends after delivering the event.
    fn emit(&mut self, event: TerminalEvent) {
        self.last_active_at = std::time::Instant::now();
        let bytes = match &event {
            TerminalEvent::Data { data, .. } => data.len(),
            TerminalEvent::Exit { .. } => 16,
        };
        self.replay.push_back(event.clone());
        self.replay_bytes += bytes;
        while self.replay_bytes > MAX_REPLAY_BYTES && self.replay.len() > 1 {
            if let Some(dropped) = self.replay.pop_front() {
                self.replay_bytes -= match &dropped {
                    TerminalEvent::Data { data, .. } => data.len(),
                    TerminalEvent::Exit { .. } => 16,
                };
            }
        }
        self.subscribers
            .retain(|(tx, _)| tx.send(event.clone()).is_ok());
        if matches!(event, TerminalEvent::Exit { .. }) {
            self.initial_script.take();
            self.exited = true;
            self.subscribers.clear();
        }
    }

    /// The host answers queries unless a viewer that answers them itself is
    /// attached (two answers would read as typed input).
    fn host_answers(&self) -> bool {
        !self
            .subscribers
            .iter()
            .any(|(_, answers_itself)| *answers_itself)
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

struct TerminalsInner {
    sessions: Mutex<HashMap<String, Arc<Mutex<LiveTerminal>>>>,
    activity_tx: watch::Sender<Vec<(String, TerminalActivity)>>,
    /// Each terminal's chat, kept past the terminal's close: a server it
    /// backgrounded outlives it and still belongs to that chat's previews.
    chats: Mutex<VecDeque<(String, String)>>,
}

/// Terminals whose chat [`Terminals::chats`] still remembers.
const REMEMBERED_TERMINAL_CHATS: usize = 4096;

impl TerminalsInner {
    /// Called without a LiveTerminal lock; the graph's lock order is always
    /// sessions → terminal. Only status transitions, never ordinary output,
    /// rebuild this bounded projection.
    fn publish_activity(&self) {
        let mut chats = std::collections::BTreeMap::<String, TerminalActivity>::new();
        for (id, session) in lock(&self.sessions).iter() {
            let session = lock(session);
            let Some(chat) = &session.chat_id else {
                continue;
            };
            let Some((report, updated_at)) = session.programs.summary(session.seen_at) else {
                continue;
            };
            let activity = TerminalActivity {
                terminal_id: id.clone(),
                report,
                updated_at,
            };
            let replace = chats.get(chat).is_none_or(|old| {
                activity.report.state.attention_rank() < old.report.state.attention_rank()
                    || (activity.report.state.attention_rank() == old.report.state.attention_rank()
                        && (activity.updated_at, &activity.terminal_id)
                            > (old.updated_at, &old.terminal_id))
            });
            if replace {
                chats.insert(chat.clone(), activity);
            }
        }
        let snapshot: Vec<_> = chats.into_iter().collect();
        self.activity_tx.send_if_modified(|current| {
            if *current == snapshot {
                false
            } else {
                *current = snapshot;
                true
            }
        });
    }
}

impl Drop for TerminalsInner {
    fn drop(&mut self) {
        let sessions = self
            .sessions
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>();
        for session in sessions {
            dispose(&session, true);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct Terminals {
    inner: Arc<TerminalsInner>,
}

impl Default for Terminals {
    fn default() -> Self {
        Self::new()
    }
}

fn clamp_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols: cols.clamp(2, 500),
        rows: rows.clamp(1, 300),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The user's interactive shell: `$SHELL`, else the platform default.
fn selected_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/bash".into()
            }
        })
}

impl Terminals {
    /// Requires a tokio runtime (spawns the exited-session reaper).
    pub fn new() -> Self {
        let (activity_tx, _) = watch::channel(Vec::new());
        let terminals = Self {
            inner: Arc::new(TerminalsInner {
                sessions: Mutex::new(HashMap::new()),
                activity_tx,
                chats: Mutex::new(VecDeque::new()),
            }),
        };
        tokio::spawn(reaper_task(Arc::downgrade(&terminals.inner)));
        terminals
    }

    pub(crate) fn watch_activity(&self) -> watch::Receiver<Vec<(String, TerminalActivity)>> {
        self.inner.activity_tx.subscribe()
    }

    /// Seen results no longer compete with new results in other program
    /// records or terminals. Keep the records themselves for app inheritance.
    pub(crate) fn refresh_seen(&self, chats: &[onicode_proto::Chat]) {
        let mut changed = false;
        for session in lock(&self.inner.sessions).values() {
            let mut session = lock(session);
            let seen = session.chat_id.as_deref().and_then(|id| {
                chats
                    .iter()
                    .find(|chat| chat.id == id)
                    .and_then(|chat| chat.last_seen_at)
            });
            if session.seen_at != seen {
                session.seen_at = seen;
                changed = true;
            }
        }
        if changed {
            self.inner.publish_activity();
        }
    }

    /// Attach an engine-owned terminal to its chat after opening. Reports may
    /// arrive before this call; publish the current projection immediately.
    pub fn associate_chat(&self, terminal_id: &str, chat_id: &str) -> Result<(), EngineError> {
        let session = self.session(terminal_id)?;
        lock(&session).chat_id = Some(chat_id.to_owned());
        {
            let mut chats = lock(&self.inner.chats);
            chats.retain(|(terminal, _)| terminal != terminal_id);
            chats.push_back((terminal_id.to_owned(), chat_id.to_owned()));
            while chats.len() > REMEMBERED_TERMINAL_CHATS {
                chats.pop_front();
            }
        }
        self.inner.publish_activity();
        Ok(())
    }

    /// Terminal id → chat for every terminal opened for a chat, open or
    /// since closed (the newest [`REMEMBERED_TERMINAL_CHATS`]). Everything a
    /// terminal starts carries its id (see [`onicode_proto::ORIGIN_ENV`]).
    pub fn chats(&self) -> HashMap<String, String> {
        lock(&self.inner.chats).iter().cloned().collect()
    }

    /// Open a login shell in `cwd`. The PTY outlives every subscriber; it dies on
    /// [`Self::close`], shell exit + TTL, or engine shutdown.
    pub fn open(&self, cwd: &str, cols: u16, rows: u16) -> Result<TerminalSession, EngineError> {
        self.open_with_environment(cwd, cols, rows, &HashMap::new())
    }

    /// Open a login shell with host-resolved environment overrides.
    pub fn open_with_environment(
        &self,
        cwd: &str,
        cols: u16,
        rows: u16,
        environment: &HashMap<String, String>,
    ) -> Result<TerminalSession, EngineError> {
        self.open_with_shell_and_environment(cwd, cols, rows, None, environment)
    }

    /// Explicit shell override (tests use `/bin/sh`).
    pub fn open_with_shell(
        &self,
        cwd: &str,
        cols: u16,
        rows: u16,
        shell: Option<&str>,
    ) -> Result<TerminalSession, EngineError> {
        self.open_with_shell_and_environment(cwd, cols, rows, shell, &HashMap::new())
    }

    /// Explicit shell and environment override for focused integration tests.
    pub fn open_with_shell_and_environment(
        &self,
        cwd: &str,
        cols: u16,
        rows: u16,
        shell: Option<&str>,
        environment: &HashMap<String, String>,
    ) -> Result<TerminalSession, EngineError> {
        self.open_session(cwd, cols, rows, shell, environment, None)
    }

    /// Run an exact command in a fresh interactive login shell. On Unix, only a
    /// short bootstrap crosses the PTY's limited initial canonical line buffer.
    pub fn open_with_command(
        &self,
        cwd: &str,
        cols: u16,
        rows: u16,
        environment: &HashMap<String, String>,
        command: &str,
    ) -> Result<TerminalSession, EngineError> {
        self.open_session(cwd, cols, rows, None, environment, Some(command))
    }

    #[allow(clippy::too_many_arguments)]
    fn open_session(
        &self,
        cwd: &str,
        cols: u16,
        rows: u16,
        shell: Option<&str>,
        environment: &HashMap<String, String>,
        command: Option<&str>,
    ) -> Result<TerminalSession, EngineError> {
        if lock(&self.inner.sessions).len() >= MAX_TERMINALS {
            return Err(EngineError::Other(format!(
                "Too many open terminals (maximum {MAX_TERMINALS})"
            )));
        }
        if !std::fs::metadata(cwd).map(|m| m.is_dir()).unwrap_or(false) {
            return Err(EngineError::Other(
                "Session working directory is unavailable".into(),
            ));
        }

        let shell = shell.map(str::to_string).unwrap_or_else(selected_shell);
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| shell.clone());

        // Named before the shell starts, so everything it runs carries it.
        let id = new_id();
        let mut environment = environment.clone();
        environment.insert(
            onicode_proto::ORIGIN_ENV.to_owned(),
            onicode_proto::terminal_origin(&id),
        );
        let environment = &environment;

        let (initial_script, bootstrap) = if let Some(command) = command {
            let (suffix, source) = match shell_name.as_str() {
                "fish" => (".fish", "source \"$ONICODE_ACTION_SCRIPT\"\r"),
                _ => (".sh", ". \"$ONICODE_ACTION_SCRIPT\"\r"),
            };
            let mut script = tempfile::Builder::new()
                .prefix("onicode-action-")
                .suffix(suffix)
                .tempfile()?;
            script.write_all(command.as_bytes())?;
            script.flush()?;
            (Some(script), Some(source.to_string()))
        } else {
            (None, None)
        };

        let (master, mut child) = {
            let pty = native_pty_system();
            let pair = pty
                .openpty(clamp_size(cols, rows))
                .map_err(|e| EngineError::Other(format!("could not open a pty: {e}")))?;
            let mut cmd = CommandBuilder::new(&shell);
            cmd.arg("-l"); // login shell — the user's real PATH/profile
            cmd.cwd(cwd);
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("TERM_PROGRAM", "Onicode");
            for (name, value) in environment {
                cmd.env(name, value);
            }
            if let Some(script) = initial_script.as_ref() {
                cmd.env("ONICODE_ACTION_SCRIPT", script.path());
            }
            let child = pair
                .slave
                .spawn_command(cmd)
                .map_err(|e| EngineError::Other(format!("could not spawn {shell_name}: {e}")))?;
            drop(pair.slave);
            (pair.master, child)
        };
        let killer = child.clone_killer();
        let reader = master
            .try_clone_reader()
            .map_err(|e| EngineError::Other(format!("pty reader: {e}")))?;
        let writer = master
            .take_writer()
            .map_err(|e| EngineError::Other(format!("pty writer: {e}")))?;

        let session = Arc::new(Mutex::new(LiveTerminal {
            chat_id: None,
            seen_at: None,
            programs: ProgramTracker::default(),
            initial_script,
            shell_pid: child.process_id(),
            master: Some(master),
            writer: Some(writer),
            killer,
            subscribers: Vec::new(),
            replies: replies::HostReplies::new(
                clamp_size(cols, rows).cols,
                clamp_size(cols, rows).rows,
            ),
            replay: VecDeque::new(),
            replay_bytes: 0,
            seq: 0,
            last_active_at: std::time::Instant::now(),
            exited: false,
            support_replies: 0,
            support_window: std::time::Instant::now(),
            applied_inputs: HashMap::new(),
        }));
        lock(&self.inner.sessions).insert(id.clone(), session.clone());

        // Raw PTY bytes: blocking reader thread → batcher task.
        let (raw_tx, raw_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name(format!("pty-read-{id}"))
            .spawn(move || read_pty(reader, raw_tx))
            .map_err(|e| EngineError::Other(format!("pty reader thread: {e}")))?;
        let wait = tokio::task::spawn_blocking(move || child.wait());
        tokio::spawn(pump_output(
            Arc::downgrade(&session),
            Arc::downgrade(&self.inner),
            raw_rx,
            wait,
        ));

        if let Some(bootstrap) = bootstrap
            && let Err(err) = self.write_bytes(&id, bootstrap.as_bytes())
        {
            let _ = self.close(&id);
            return Err(err);
        }

        Ok(TerminalSession {
            id,
            cwd: cwd.to_string(),
            shell: shell_name,
            host_replies: true,
        })
    }

    fn session(&self, terminal_id: &str) -> Result<Arc<Mutex<LiveTerminal>>, EngineError> {
        lock(&self.inner.sessions)
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("Terminal not found".into()))
    }

    /// Replay (from `after_seq`, bounded 1MB window) then live tail. The stream
    /// ends after `Exit`; detaching (dropping the stream) leaves the PTY running.
    pub fn subscribe(
        &self,
        terminal_id: &str,
        after_seq: Option<u64>,
    ) -> Result<mpsc::UnboundedReceiver<TerminalEvent>, EngineError> {
        self.subscribe_viewer(terminal_id, after_seq, false)
    }

    /// [`Self::subscribe`] for a viewer; `answers_itself` marks an app from
    /// before host replies, which answers terminal queries from its own
    /// emulator — while one is attached the host stays silent.
    pub fn subscribe_viewer(
        &self,
        terminal_id: &str,
        after_seq: Option<u64>,
        answers_itself: bool,
    ) -> Result<mpsc::UnboundedReceiver<TerminalEvent>, EngineError> {
        let session = self.session(terminal_id)?;
        let mut session = lock(&session);
        session.last_active_at = std::time::Instant::now();
        let (tx, rx) = mpsc::unbounded_channel();
        let after = after_seq.unwrap_or(0);
        for event in &session.replay {
            let seq = match event {
                TerminalEvent::Data { seq, .. } | TerminalEvent::Exit { seq, .. } => *seq,
            };
            if seq > after {
                let _ = tx.send(event.clone());
            }
        }
        if !session.exited {
            session.subscribers.push((tx, answers_itself));
        }
        // On an exited session `tx` drops here: the stream ends after the replay.
        Ok(rx)
    }

    /// Write input bytes; `data` is base64 (matching `Data` events), with a plain
    /// UTF-8 fallback for lenient callers.
    pub fn write(&self, terminal_id: &str, data: &str) -> Result<(), EngineError> {
        let bytes = BASE64
            .decode(data)
            .unwrap_or_else(|_| data.as_bytes().to_vec());
        self.write_bytes(terminal_id, &bytes)
    }

    /// Write trusted host-side bytes without applying the RPC base64 decoder.
    pub fn write_bytes(&self, terminal_id: &str, bytes: &[u8]) -> Result<(), EngineError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(EngineError::Other("Terminal input is too large".into()));
        }
        let session = self.session(terminal_id)?;
        let mut session = lock(&session);
        if session.exited {
            return Err(EngineError::Other("Terminal has exited".into()));
        }
        session.last_active_at = std::time::Instant::now();
        let writer = session
            .writer
            .as_mut()
            .ok_or_else(|| EngineError::Other("Terminal has exited".into()))?;
        writer
            .write_all(bytes)
            .and_then(|_| writer.flush())
            .map_err(|e| EngineError::Other(format!("Terminal write failed: {e}")))
    }

    /// Write input a remote viewer numbered: applied only if `seq` is newer
    /// than the last one applied from `client`. Returns whether it was
    /// applied (a replay of already-applied input is acknowledged, not
    /// written twice).
    pub fn write_sequenced(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, EngineError> {
        {
            let session = self.session(terminal_id)?;
            let mut session = lock(&session);
            let applied = session.applied_inputs.get(client).copied().unwrap_or(0);
            if seq <= applied {
                return Ok(false);
            }
            if !session.applied_inputs.contains_key(client)
                && session.applied_inputs.len() >= MAX_INPUT_CLIENTS
                && let Some(oldest) = session.applied_inputs.keys().next().cloned()
            {
                session.applied_inputs.remove(&oldest);
            }
            session.applied_inputs.insert(client.to_owned(), seq);
        }
        self.write_bytes(terminal_id, bytes)?;
        Ok(true)
    }

    pub fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), EngineError> {
        let session = self.session(terminal_id)?;
        let mut session = lock(&session);
        session.last_active_at = std::time::Instant::now();
        if session.exited {
            return Ok(());
        }
        let size = clamp_size(cols, rows);
        session.replies.resize(size.cols, size.rows);
        let Some(master) = session.master.as_ref() else {
            return Ok(());
        };
        master
            .resize(size)
            .map_err(|e| EngineError::Other(format!("Terminal resize failed: {e}")))
    }

    /// Whether closing it would end a program (something other than the
    /// shell in its foreground), and whether its shell has exited: what a
    /// viewer asks before closing a terminal's tab.
    pub fn status(&self, terminal_id: &str) -> Result<TerminalStatus, EngineError> {
        let session = self.session(terminal_id)?;
        let live = lock(&session);
        if live.exited {
            return Ok(TerminalStatus {
                busy: false,
                exited: true,
            });
        }
        let busy = match (
            live.master.as_ref().and_then(|m| m.process_group_leader()),
            live.shell_pid,
        ) {
            (Some(leader), Some(shell)) => leader > 0 && leader as u32 != shell,
            _ => false,
        };
        Ok(TerminalStatus {
            busy,
            exited: false,
        })
    }

    /// Kill the shell (if still running) and drop the session + replay buffer.
    pub fn close(&self, terminal_id: &str) -> Result<(), EngineError> {
        let session = lock(&self.inner.sessions)
            .remove(terminal_id)
            .ok_or_else(|| EngineError::Other("Terminal not found".into()))?;
        self.inner.publish_activity();
        dispose(&session, true);
        Ok(())
    }

    /// Any live PTY (the reaper prunes exited ones) — restarts kill shells, so
    /// the auto-updater waits for none.
    pub fn any_open(&self) -> bool {
        !lock(&self.inner.sessions).is_empty()
    }

    /// Engine shutdown: kill every live shell.
    pub fn shutdown(&self) {
        let sessions: Vec<_> = lock(&self.inner.sessions).drain().map(|(_, s)| s).collect();
        self.inner.publish_activity();
        for session in sessions {
            dispose(&session, true);
        }
    }
}

fn dispose(session: &Arc<Mutex<LiveTerminal>>, kill: bool) {
    let mut session = lock(session);
    session.subscribers.clear();
    if kill
        && !session.exited
        && let Err(err) = session.killer.kill()
    {
        tracing::debug!(error = %err, "terminal kill failed (already exited?)");
    }
    session.initial_script.take();
}

/// Blocking PTY reader: forwards raw chunks until EOF. A closed PTY reads as an
/// error on some platforms (EIO on Linux once the shell exits) — both end the loop.
fn read_pty(mut reader: Box<dyn Read + Send>, tx: mpsc::UnboundedSender<Vec<u8>>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

/// Turns raw chunks into `Data` events: immediately after a quiet spell, at
/// most once per [`TERMINAL_OUTPUT_BATCH_MS`] while output keeps coming.
/// Child wait and output reads are driven concurrently: once the child is gone
/// we close the PTY handles, drain its final buffered output, then emit `Exit`.
/// Holds only a weak session handle so a closed terminal tears this task down.
async fn pump_output(
    session: Weak<Mutex<LiveTerminal>>,
    terminals: Weak<TerminalsInner>,
    mut raw_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut wait: tokio::task::JoinHandle<Result<portable_pty::ExitStatus, std::io::Error>>,
) {
    let batch = Duration::from_millis(TERMINAL_OUTPUT_BATCH_MS);
    let emit = |buffer: Vec<u8>| -> bool {
        let Some(session) = session.upgrade() else {
            return false;
        };
        let mut session = lock(&session);
        let update = session.programs.feed(&buffer);
        // Answer terminal queries now, beside the PTY: a viewer's answer
        // could arrive after the program stopped waiting.
        let answers = session.replies.feed(&buffer);
        if !answers.is_empty()
            && session.host_answers()
            && let Some(writer) = session.writer.as_mut()
            && let Err(error) = writer.write_all(&answers).and_then(|_| writer.flush())
        {
            tracing::debug!(%error, "terminal query answer failed");
        }
        // Answer from the PTY owner, not a viewport: detection must work while
        // detached and must not be replayed when another viewer attaches.
        // At most one per batch and a few per window: the write goes to the
        // PTY's input, which a program that never reads stdin fills, and this
        // runs under the session lock.
        if update.queries > 0 && session.support_window.elapsed() >= SUPPORT_WINDOW {
            session.support_window = std::time::Instant::now();
            session.support_replies = 0;
        }
        if update.queries > 0 && session.support_replies < MAX_SUPPORT_REPLIES {
            session.support_replies += 1;
            if let Some(writer) = session.writer.as_mut()
                && let Err(error) = writer.write_all(SUPPORT_REPLY).and_then(|_| writer.flush())
            {
                tracing::debug!(%error, "program status detection reply failed");
            }
        }
        let seq = session.next_seq();
        session.emit(TerminalEvent::Data {
            seq,
            data: BASE64.encode(&buffer),
        });
        drop(session);
        if update.changed
            && let Some(terminals) = terminals.upgrade()
        {
            terminals.publish_activity();
        }
        true
    };

    // Output that follows a quiet spell (a keystroke's echo) goes out the
    // moment it is read; only a continuous stream waits out the pacing
    // interval. Idle terminals need no timer.
    let flush = tokio::time::sleep(batch);
    tokio::pin!(flush);
    let mut buffer = Vec::new();
    let mut raw_open = true;
    let mut exit_code = None;
    let mut last_emit = tokio::time::Instant::now() - batch;

    while raw_open || exit_code.is_none() {
        tokio::select! {
            chunk = raw_rx.recv(), if raw_open => match chunk {
                Some(chunk) => {
                    let was_empty = buffer.is_empty();
                    buffer.extend_from_slice(&chunk);
                    // Whatever the reader already has queued rides along.
                    while let Ok(more) = raw_rx.try_recv() {
                        buffer.extend_from_slice(&more);
                    }
                    let now = tokio::time::Instant::now();
                    if now >= last_emit + batch {
                        last_emit = now;
                        if !emit(std::mem::take(&mut buffer)) {
                            return;
                        }
                    } else if was_empty {
                        flush.as_mut().reset(last_emit + batch);
                    }
                },
                None => raw_open = false,
            },
            result = &mut wait, if exit_code.is_none() => {
                exit_code = Some(match result {
                    Ok(Ok(status)) => status.exit_code() as i32,
                    Ok(Err(err)) => {
                        tracing::debug!(error = %err, "terminal wait failed");
                        -1
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "terminal wait task failed");
                        -1
                    }
                });

                let Some(session) = session.upgrade() else {
                    return;
                };
                {
                    let (master, writer) = {
                        let mut session = lock(&session);
                        (session.master.take(), session.writer.take())
                    };
                    let _ = tokio::task::spawn_blocking(move || {
                        drop(writer);
                        drop(master);
                    }).await;
                }
            }
            _ = &mut flush, if !buffer.is_empty() => {
                last_emit = tokio::time::Instant::now();
                if !emit(std::mem::take(&mut buffer)) {
                    return;
                }
            }
        }
    }

    if !buffer.is_empty() && !emit(buffer) {
        return;
    }
    if let Some(session) = session.upgrade() {
        let mut session = lock(&session);
        session.programs.end_command();
        let seq = session.next_seq();
        session.emit(TerminalEvent::Exit {
            seq,
            exit_code: exit_code.unwrap_or(-1),
            signal: None,
        });
        drop(session);
        if let Some(terminals) = terminals.upgrade() {
            terminals.publish_activity();
        }
    }
}

/// Live shells never expire on idleness — a detached session is the user's running
/// process. Only EXITED sessions are swept after [`EXITED_TTL`]: they're inert
/// replay buffers held so a returning viewer can show the tail + exit status.
async fn reaper_task(inner: Weak<TerminalsInner>) {
    let mut tick = tokio::time::interval(REAPER_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // consume the immediate first tick
    loop {
        tick.tick().await;
        let Some(inner) = inner.upgrade() else { break };
        let mut sessions = lock(&inner.sessions);
        sessions.retain(|_, session| {
            let session = lock(session);
            !(session.exited && session.last_active_at.elapsed() > EXITED_TTL)
        });
        drop(sessions);
        inner.publish_activity();
    }
}

#[cfg(test)]
mod initial_command_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn status_says_whether_a_program_is_running() {
        let root = tempfile::tempdir().unwrap();
        let terminals = Terminals::new();
        let session = terminals
            .open_with_shell(root.path().to_str().unwrap(), 80, 24, Some("/bin/sh"))
            .unwrap();
        let wait_for = |want: bool| {
            let terminals = &terminals;
            let id = session.id.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if terminals.status(&id).unwrap().busy == want {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .is_ok()
            }
        };
        assert!(wait_for(false).await, "an idle shell isn't busy");
        // The shell hands its child the terminal before the child restores
        // the default SIGINT and execs, so a ^C sent as soon as the shell
        // stops being the foreground can land in the shell's own handler and
        // be lost. The marker is written by a program the child has already
        // exec'd, whose SIGINT is the default: from then on ^C ends it.
        let started = root.path().join("started");
        terminals
            .write(&session.id, "sh -c ': > started; exec sleep 30'\n")
            .unwrap();
        assert!(wait_for(true).await, "a program in the foreground is");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the program started");
        terminals.write(&session.id, "\u{3}").unwrap();
        assert!(wait_for(false).await, "and isn't once it's interrupted");
        assert!(!terminals.status(&session.id).unwrap().exited);
        terminals.close(&session.id).unwrap();
        assert!(terminals.status(&session.id).is_err());
    }

    #[tokio::test]
    async fn long_command_survives_delayed_shell_startup_and_script_is_cleaned_up() {
        let root = tempfile::tempdir().unwrap();
        let terminals = Terminals::new();
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            // Deliberately leave the PTY in canonical mode while the bootstrap
            // is written, instead of relying on a scheduler-dependent race.
            let wrapper = root.path().join("slow-shell");
            std::fs::write(
                &wrapper,
                format!("#!/bin/sh\nsleep 0.2\nexec {shell} \"$@\"\n"),
            )
            .unwrap();
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = root.path().join("result");
            let payload = "a".repeat(6000);
            let command = format!("printf '%s' '{payload}' > result");
            let session = terminals
                .open_session(
                    root.path().to_str().unwrap(),
                    80,
                    24,
                    wrapper.to_str(),
                    &HashMap::new(),
                    Some(&command),
                )
                .unwrap();
            let script = lock(&terminals.session(&session.id).unwrap())
                .initial_script
                .as_ref()
                .unwrap()
                .path()
                .to_path_buf();
            assert_eq!(std::fs::read_to_string(&script).unwrap(), command);
            assert_eq!(
                std::fs::metadata(&script).unwrap().permissions().mode() & 0o077,
                0
            );
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if std::fs::read(&output).ok().as_deref() == Some(payload.as_bytes()) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("long command did not execute in {shell}"));
            terminals.close(&session.id).unwrap();
            assert!(
                !script.exists(),
                "closing the terminal must remove the script"
            );
            std::fs::remove_file(output).unwrap();
        }
        assert!(!terminals.any_open());
    }

    #[tokio::test]
    async fn initial_script_is_removed_on_shell_exit_and_spawn_failure() {
        let root = tempfile::tempdir().unwrap();
        let terminals = Terminals::new();
        let session = terminals
            .open_session(
                root.path().to_str().unwrap(),
                80,
                24,
                Some("/bin/sh"),
                &HashMap::new(),
                Some("sleep 0.1; exit 7"),
            )
            .unwrap();
        let script = lock(&terminals.session(&session.id).unwrap())
            .initial_script
            .as_ref()
            .unwrap()
            .path()
            .to_path_buf();
        let mut rx = terminals.subscribe(&session.id, None).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = rx.recv().await {
                if matches!(event, TerminalEvent::Exit { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(!script.exists());
        terminals.close(&session.id).unwrap();
        assert!(
            terminals
                .open_session(
                    root.path().to_str().unwrap(),
                    80,
                    24,
                    Some("/nonexistent/onicode-test-shell"),
                    &HashMap::new(),
                    Some("echo test"),
                )
                .is_err()
        );
        assert!(!terminals.any_open());
    }
}
