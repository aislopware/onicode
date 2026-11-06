//! The display holder: the process that owns the displays made for clients.
//!
//! So a desktopd's restart, crash or update moves no window (`docs/decisions/video.md`, "The
//! displays made for clients outlive desktopd"; `.research/vdisplay-holder-2026-10-11.md`).
//!
//! A `CGVirtualDisplay` ends with the process that made it, `exec` included, and macOS then
//! moves its windows to a physical display. So desktopd makes none itself: it asks the
//! holder, a mode of its own binary in a process group of its own, over a Unix socket
//! ([`Remote`], a [`Factory`]). desktopd's [`super::sized::Registry`] keeps its leases,
//! lingers and enforcement as before; only the displays live in the holder ([`Holder`]). A
//! desktopd that starts takes the holder's displays as lingering for their clients
//! ([`super::sized::Displays::adopt`]), so a client back within the linger has its display,
//! windows where they were.
//!
//! The holder keeps its list on disk. One that has to go (a holder of another wire, which the
//! desktopd has quit, or a crash) is followed by one that makes each display again under the same
//! identity at once, and macOS brings the windows back to it: measured 0.28 s to 30 s after,
//! every time. It ends, its displays with it, once no desktopd has been linked for
//! [`ALONE_FOR`]: a desktopd restarted by launchd is back within seconds, and one that is not
//! back by then was stopped or removed.

pub mod wire;

use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use onicode_desktop_proto::screen::DisplayKey;
use onicode_desktop_vdisplay::{DisplayError, Enforced, Plan};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::time::Instant;

use self::wire::{Answer, Ask, HeldDisplay, WireError, WirePlan};
use crate::sized::{Factory, Main};

/// What desktopd's binary is started with to be the holder: then its socket and its list.
pub const HOLDER_ARG: &str = "--display-holder";

/// How long a holder waits for a desktopd before it ends, its displays with it.
pub const ALONE_FOR: Duration = Duration::from_mins(1);

/// When a desktopd that started asks the holder again for what it holds: by then a holder that
/// had to go has made its list again ([`restore`], up to `RESTORE_TRIES` seconds).
pub const ADOPT_AGAIN_AFTER: Duration = Duration::from_secs(20);

/// How long a desktopd waits for one answer of the holder.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// How many times, a second apart, a holder makes a kept display again while macOS still
/// holds its identity for the holder before it.
const RESTORE_TRIES: u32 = 16;

/// The wire's fingerprint: a holder answering another is quit and started anew.
#[must_use]
pub fn fingerprint() -> u64 {
    // FNV-1a over the wire's source: any change to it is another wire.
    include_bytes!("holder/wire.rs")
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// A display in the holder's list on disk.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Kept {
    key: DisplayKey,
    plan: WirePlan,
}

/// The displays the holder whose list is `store` held when it last wrote it.
#[must_use]
pub fn kept(store: &Path) -> Vec<(DisplayKey, Plan)> {
    let read = std::fs::read(store).ok();
    let kept: Vec<Kept> = read
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    kept.into_iter()
        .map(|k| (k.key, Plan::from(k.plan)))
        .collect()
}

/// The holder's displays, on its main thread.
pub struct Holder<F: Factory> {
    factory: F,
    displays: HashMap<DisplayKey, (F::Display, Plan)>,
    store: PathBuf,
}

impl<F: Factory> std::fmt::Debug for Holder<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Holder")
            .field("displays", &self.displays.len())
            .finish_non_exhaustive()
    }
}

impl<F: Factory> Holder<F> {
    /// No display yet; `factory` makes them, and the list is kept at `store`.
    pub fn new(factory: F, store: PathBuf) -> Self {
        Self {
            factory,
            displays: HashMap::new(),
            store,
        }
    }

    /// Whether it holds no display.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.displays.is_empty()
    }

    /// Make `key`'s display again from the list, unless it holds it already.
    ///
    /// # Errors
    ///
    /// Why it could not be made: [`DisplayError::Refused`] while macOS still holds the identity.
    pub fn restore(&mut self, key: DisplayKey, plan: &Plan) -> Result<(), DisplayError> {
        if self.displays.contains_key(&key) {
            return Ok(());
        }
        let display = self.factory.create(key, plan)?;
        let id = self.factory.id(&display);
        tracing::info!(id, "virtual display made again");
        self.displays.insert(key, (display, *plan));
        Ok(())
    }

    /// End every display, and the list: nobody will take them back.
    pub fn release_all(&mut self) {
        for (_key, (display, _plan)) in self.displays.drain() {
            self.factory.release(display);
        }
        self.keep();
    }

    /// The answer to `ask`. [`Ask::Quit`] is answered [`Answer::Done`] and left to the caller.
    pub fn answer(&mut self, ask: Ask) -> Answer {
        let failed = |e: DisplayError| Answer::Failed(e.into());
        match ask {
            Ask::Hello => Answer::Hello {
                fingerprint: fingerprint(),
            },
            Ask::Create { key, plan } => match self.create(key, &Plan::from(plan)) {
                Ok(id) => Answer::Made { id },
                Err(e) => failed(e),
            },
            Ask::Resize { key, plan } => {
                let plan = Plan::from(plan);
                let Some((display, held)) = self.displays.get_mut(&key) else {
                    return Answer::Failed(WireError::Unknown);
                };
                match self.factory.resize(display, &plan) {
                    Ok(()) => {
                        *held = plan;
                        self.keep();
                        Answer::Done
                    }
                    Err(e) => failed(e),
                }
            }
            Ask::Enforce { key } => match self.displays.get(&key) {
                Some((display, _plan)) => match self.factory.enforce(display) {
                    Ok(enforced) => Answer::Enforced(enforced.into()),
                    Err(e) => failed(e),
                },
                None => Answer::Failed(WireError::Unknown),
            },
            Ask::Release { key } => {
                if let Some((display, _plan)) = self.displays.remove(&key) {
                    let id = self.factory.id(&display);
                    tracing::info!(id, "virtual display ended");
                    self.factory.release(display);
                    self.keep();
                }
                Answer::Done
            }
            Ask::Held => Answer::Held(
                self.displays
                    .iter()
                    .map(|(key, (display, plan))| HeldDisplay {
                        key: *key,
                        plan: WirePlan::from(plan),
                        id: self.factory.id(display),
                    })
                    .collect(),
            ),
            Ask::Quit => Answer::Done,
        }
    }

    /// `key`'s display in `plan`'s mode: the one held, changed in place or made anew when it
    /// outgrew it, else a new one.
    fn create(&mut self, key: DisplayKey, plan: &Plan) -> Result<u32, DisplayError> {
        if let Some((display, held)) = self.displays.get_mut(&key) {
            if held.mode == plan.mode {
                return Ok(self.factory.id(display));
            }
            match self.factory.resize(display, plan) {
                Ok(()) => {
                    *held = *plan;
                    let id = self.factory.id(display);
                    self.keep();
                    return Ok(id);
                }
                Err(DisplayError::Outgrown { .. }) => {
                    if let Some((old, _plan)) = self.displays.remove(&key) {
                        self.factory.release(old);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        let display = self.factory.create(key, plan)?;
        let id = self.factory.id(&display);
        tracing::info!(id, mode = ?plan.mode, "virtual display made");
        self.displays.insert(key, (display, *plan));
        self.keep();
        Ok(id)
    }

    /// Write the list, whole or not at all; none when it holds nothing.
    fn keep(&self) {
        let written = if self.displays.is_empty() {
            match std::fs::remove_file(&self.store) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                gone => gone,
            }
        } else {
            let kept: Vec<Kept> = self
                .displays
                .iter()
                .map(|(key, (_display, plan))| Kept {
                    key: *key,
                    plan: WirePlan::from(plan),
                })
                .collect();
            write_whole(&self.store, &kept)
        };
        if let Err(e) = written {
            tracing::warn!(error = %e, "the held displays' list was not written");
        }
    }
}

/// Write `value` at `path` whole or not at all, through a sibling renamed into place.
fn write_whole(path: &Path, value: &impl serde::Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let staging = path.with_extension("part");
    std::fs::write(&staging, bytes)?;
    std::fs::rename(&staging, path)
}

/// Run `job` on the holder's main thread and wait for its answer; `None` when it was dropped.
async fn on_main<F: Factory, R: Send + 'static>(
    main: &Arc<dyn Main<Holder<F>>>,
    job: impl FnOnce(&mut Holder<F>) -> R + Send + 'static,
) -> Option<R> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    main.run(Box::new(move |holder| {
        let _gone = tx.send(job(holder));
    }));
    rx.await.ok()
}

/// Make every display in `store`'s list again, each tried `RESTORE_TRIES` times a second
/// apart while macOS still holds its identity for the holder before.
pub async fn restore<F: Factory>(main: Arc<dyn Main<Holder<F>>>, store: &Path) {
    for (key, plan) in kept(store) {
        for _ in 0..RESTORE_TRIES {
            match on_main(&main, move |holder| holder.restore(key, &plan)).await {
                Some(Err(DisplayError::Refused)) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "a kept display was not made again");
                    break;
                }
                Some(Ok(())) | None => break,
            }
        }
    }
}

/// Why [`serve`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// A desktopd asked it to quit: the next holder makes the displays again.
    Quit,
    /// No desktopd was linked for the time [`serve`] was given.
    Alone,
}

/// Serve desktopds until one says [`Ask::Quit`] or none has been linked for `alone_for`.
///
/// desktopd on `first` is the one that started it; then each that dials `listener`. Every ask
/// is answered on the main thread. The holder's own is [`ALONE_FOR`].
pub async fn serve<F: Factory>(
    main: Arc<dyn Main<Holder<F>>>,
    first: Option<tokio::net::UnixStream>,
    listener: Option<tokio::net::UnixListener>,
    alone_for: Duration,
) -> Ended {
    let quit = Arc::new(tokio::sync::Notify::new());
    let mut links = tokio::task::JoinSet::new();
    if let Some(stream) = first {
        links.spawn(link(Arc::clone(&main), stream, Arc::clone(&quit)));
    }
    let mut alone_since: Option<Instant> = None;
    loop {
        let alone = if links.is_empty() {
            let since = *alone_since.get_or_insert_with(Instant::now);
            let left = alone_for.saturating_sub(since.elapsed());
            if left.is_zero() {
                return Ended::Alone;
            }
            Some(left)
        } else {
            alone_since = None;
            None
        };
        tokio::select! {
            accepted = accept(listener.as_ref()) => match accepted {
                Ok(stream) => {
                    alone_since = None;
                    links.spawn(link(Arc::clone(&main), stream, Arc::clone(&quit)));
                }
                Err(e) => tracing::warn!(error = %e, "a desktopd's link to the holder"),
            },
            Some(_ended) = links.join_next(), if !links.is_empty() => {}
            () = quit.notified() => return Ended::Quit,
            () = tokio::time::sleep(alone.unwrap_or_default()), if alone.is_some() => {}
        }
    }
}

/// The next desktopd to dial `listener`; never, with none.
async fn accept(
    listener: Option<&tokio::net::UnixListener>,
) -> std::io::Result<tokio::net::UnixStream> {
    match listener {
        Some(listener) => listener.accept().await.map(|(stream, _addr)| stream),
        None => std::future::pending().await,
    }
}

/// Answer one desktopd's asks, a line each, until it goes or says [`Ask::Quit`].
async fn link<F: Factory>(
    main: Arc<dyn Main<Holder<F>>>,
    stream: tokio::net::UnixStream,
    quit: Arc<tokio::sync::Notify>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let answer = match serde_json::from_str::<Ask>(&line) {
            Ok(Ask::Quit) => {
                let _said = say(&mut write, &Answer::Done).await;
                quit.notify_one();
                return;
            }
            Ok(ask) => on_main(&main, move |holder| holder.answer(ask))
                .await
                .unwrap_or_else(|| {
                    Answer::Failed(WireError::Unavailable("the holder is ending".to_owned()))
                }),
            Err(e) => Answer::Failed(WireError::Unavailable(format!("an unreadable ask: {e}"))),
        };
        if say(&mut write, &answer).await.is_err() {
            return;
        }
    }
}

async fn say(write: &mut tokio::net::unix::OwnedWriteHalf, answer: &Answer) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(answer).map_err(std::io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await
}

/// What a desktopd's [`Remote`] needs the holder for, which decides whether one is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// The displays it holds, at desktopd's start: one is started only to make a kept list
    /// again, after a holder that crashed.
    Adopt,
    /// A display a client asked for: one is started when none answers.
    Make,
    /// A new holder, the one answering having been asked to quit.
    Fresh,
}

/// How a desktopd reaches the holder: a connected stream, for what it is needed for.
pub type Dial = Box<dyn FnMut(Need) -> std::io::Result<std::os::unix::net::UnixStream> + Send>;

/// A display the holder holds for this desktopd: its key and id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeldBy {
    key: DisplayKey,
    id: u32,
}

/// The holder as desktopd's [`Factory`].
///
/// Each display is made, changed and ended there, over its socket, from desktopd's main
/// thread. A display dropped rather than released stays in the holder, for the next desktopd to
/// adopt.
pub struct Remote {
    dial: Dial,
    link: Option<std::io::BufReader<std::os::unix::net::UnixStream>>,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("linked", &self.link.is_some())
            .finish_non_exhaustive()
    }
}

impl Remote {
    /// The holder, reached through `dial`.
    #[must_use]
    pub fn new(dial: Dial) -> Self {
        Self { dial, link: None }
    }

    /// No holder yet: every ask fails until [`Self::set_dial`] says how one is reached.
    #[must_use]
    pub fn unlinked() -> Self {
        Self::new(Box::new(|_need| {
            Err(std::io::Error::other("no display holder is set"))
        }))
    }

    /// Reach the holder through `dial` from now on.
    pub fn set_dial(&mut self, dial: Dial) {
        self.dial = dial;
        self.link = None;
    }

    /// Have the holder end, if one answers, and its displays with it: nothing will take them
    /// back. A holder is started anew when a display is next asked for.
    pub fn quit(&mut self) {
        if let Some(mut link) = self.link.take() {
            let _done = exchange(&mut link, &Ask::Quit);
        }
    }

    /// A link to a holder of this wire, made for `need` unless there is one.
    fn linked(
        &mut self,
        need: Need,
    ) -> std::io::Result<&mut std::io::BufReader<std::os::unix::net::UnixStream>> {
        if self.link.is_none() {
            let mut link = self.handshake(need)?;
            if link.1 != fingerprint() {
                tracing::info!("the display holder speaks another wire: it quits for a new one");
                let _done = exchange(&mut link.0, &Ask::Quit);
                link = self.handshake(Need::Fresh)?;
                if link.1 != fingerprint() {
                    return Err(std::io::Error::other("a new holder speaks another wire"));
                }
            }
            self.link = Some(link.0);
        }
        self.link
            .as_mut()
            .ok_or_else(|| std::io::Error::other("no link"))
    }

    /// A stream to a holder reached for `need`, and the wire it says it speaks.
    fn handshake(
        &mut self,
        need: Need,
    ) -> std::io::Result<(std::io::BufReader<std::os::unix::net::UnixStream>, u64)> {
        let stream = (self.dial)(need)?;
        stream.set_read_timeout(Some(ANSWER_WITHIN))?;
        stream.set_write_timeout(Some(ANSWER_WITHIN))?;
        let mut link = std::io::BufReader::new(stream);
        match exchange(&mut link, &Ask::Hello)? {
            Answer::Hello { fingerprint } => Ok((link, fingerprint)),
            other => Err(std::io::Error::other(format!(
                "a holder answered hello with {other:?}"
            ))),
        }
    }

    /// The holder's answer to `ask`, reached for `need`. A link that fails is dropped, and the
    /// next ask dials again.
    fn ask(&mut self, ask: &Ask, need: Need) -> Result<Answer, DisplayError> {
        let answered = self.linked(need).and_then(|link| exchange(link, ask));
        answered.map_err(|e| {
            self.link = None;
            DisplayError::Unavailable(format!("the display holder: {e}"))
        })
    }
}

/// Say `ask` on `link` and read the answer.
fn exchange(
    link: &mut std::io::BufReader<std::os::unix::net::UnixStream>,
    ask: &Ask,
) -> std::io::Result<Answer> {
    let mut line = serde_json::to_vec(ask).map_err(std::io::Error::other)?;
    line.push(b'\n');
    link.get_mut().write_all(&line)?;
    let mut answer = String::new();
    if link.read_line(&mut answer)? == 0 {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    serde_json::from_str(&answer).map_err(std::io::Error::other)
}

/// A display error for an answer that does not fit the ask.
fn unexpected(answer: &Answer) -> DisplayError {
    DisplayError::Unavailable(format!("the display holder answered {answer:?}"))
}

impl Factory for Remote {
    type Display = HeldBy;

    fn create(&mut self, key: DisplayKey, plan: &Plan) -> Result<HeldBy, DisplayError> {
        match self.ask(
            &Ask::Create {
                key,
                plan: WirePlan::from(plan),
            },
            Need::Make,
        )? {
            Answer::Made { id } => Ok(HeldBy { key, id }),
            Answer::Failed(e) => Err(e.into()),
            other => Err(unexpected(&other)),
        }
    }

    fn resize(&mut self, display: &mut HeldBy, plan: &Plan) -> Result<(), DisplayError> {
        let key = display.key;
        match self.ask(
            &Ask::Resize {
                key,
                plan: WirePlan::from(plan),
            },
            Need::Make,
        )? {
            Answer::Done => Ok(()),
            Answer::Failed(e) => Err(e.into()),
            other => Err(unexpected(&other)),
        }
    }

    fn enforce(&mut self, display: &HeldBy) -> Result<Enforced, DisplayError> {
        match self.ask(&Ask::Enforce { key: display.key }, Need::Make)? {
            Answer::Enforced(enforced) => Ok(enforced.into()),
            Answer::Failed(e) => Err(e.into()),
            other => Err(unexpected(&other)),
        }
    }

    fn id(&self, display: &HeldBy) -> u32 {
        display.id
    }

    fn release(&mut self, display: HeldBy) {
        if let Err(e) = self.ask(&Ask::Release { key: display.key }, Need::Make) {
            tracing::warn!(error = %e, "a virtual display was not ended");
        }
    }

    fn held(&mut self) -> Vec<(DisplayKey, Plan, HeldBy)> {
        match self.ask(&Ask::Held, Need::Adopt) {
            Ok(Answer::Held(held)) => held
                .into_iter()
                .map(|h| {
                    (
                        h.key,
                        Plan::from(h.plan),
                        HeldBy {
                            key: h.key,
                            id: h.id,
                        },
                    )
                })
                .collect(),
            Ok(other) => {
                tracing::warn!(?other, "the display holder did not say what it holds");
                Vec::new()
            }
            // No holder, and nothing kept for one to make again.
            Err(_none) => Vec::new(),
        }
    }
}

/// How a desktopd reaches the holder at `socket` that keeps its list at `store`.
///
/// One is started from `program` (this desktopd's binary, [`start`]) when a display is asked for
/// and none answers, and at desktopd's start only when the list holds displays to make again.
pub fn spawning(program: PathBuf, socket: PathBuf, store: PathBuf) -> Dial {
    Box::new(move |need| {
        let started = || start(&program, &socket, &store);
        match need {
            Need::Fresh => started(),
            Need::Make => std::os::unix::net::UnixStream::connect(&socket).or_else(|_| started()),
            Need::Adopt => std::os::unix::net::UnixStream::connect(&socket).or_else(|e| {
                if kept(&store).is_empty() {
                    Err(e)
                } else {
                    started()
                }
            }),
        }
    })
}

/// Start a holder from `program` (this desktopd's binary), serving `socket` and keeping its list
/// at `store`, in a process group of its own so that it outlives desktopd; the stream to it.
///
/// desktopd's end of a socket pair is its standard input, so the first link needs no wait
/// for the holder to listen. A thread waits on it, reaping it should it end first.
///
/// # Errors
///
/// When it could not be started.
pub fn start(
    program: &Path,
    socket: &Path,
    store: &Path,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::process::CommandExt as _;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    let mut holder = std::process::Command::new(program)
        .arg(HOLDER_ARG)
        .arg(socket)
        .arg(store)
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            theirs,
        )))
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    tracing::info!(pid = holder.id(), "display holder started");
    std::thread::Builder::new()
        .name("onicode-display-holder".to_owned())
        .spawn(move || {
            let _ended = holder.wait();
        })?;
    Ok(ours)
}

/// The holder's link to desktopd that started it, on its standard input ([`start`]).
///
/// # Errors
///
/// When standard input is no socket.
pub fn first_link() -> std::io::Result<tokio::net::UnixStream> {
    use std::os::fd::AsFd as _;
    let stdin = std::io::stdin().as_fd().try_clone_to_owned()?;
    let stream = std::os::unix::net::UnixStream::from(stdin);
    stream.peer_addr()?;
    stream.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(stream)
}

/// The holder's socket at `socket`, taken over from any holder before.
///
/// # Errors
///
/// When it could not be bound.
pub fn listen(socket: &Path) -> std::io::Result<tokio::net::UnixListener> {
    match std::fs::remove_file(socket) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    tokio::net::UnixListener::bind(socket)
}

#[cfg(test)]
mod tests;
