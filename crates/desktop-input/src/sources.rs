//! The host's keyboard input source, matched to the client whose tile has the keyboard.
//!
//! A key goes by its position, and the host's layout makes the character, so the host
//! types what the client means only under the client's input source. A stream asks for it
//! when its tile takes the keyboard (`ScreenInput::KeyboardSource`); the host selects it
//! and answers whether it did. Several clients may ask: the last to ask wins, as the person
//! typing last is the one looking. When a claim goes (the tile idle, the stream ended), the
//! source of the latest claim still held comes back, and once none holds, the one the host
//! had before the first, with every source the host turned on for a claim turned off again
//! (`docs/decisions/input.md`, "Keys go by position"). A source the person at the host picked
//! by hand is theirs: nothing is selected over it, and it is never turned off.
//!
//! [`Claims`] is the pure bookkeeping, keyed by [`Claimant`], a token each stream draws once:
//! stream ids are numbered per connection, and a client that reconnects numbers its streams
//! from one again, so no id a client names can tell two streams apart. [`Sources`] runs it where
//! Text Input Sources Services may be called ([`Tis`], the main queue on macOS), in the order
//! the streams asked, and publishes the source the host is under as the platform reports each
//! switch ([`Sources::heard`]): a stream whose source was taken by another hears it there and
//! tells its client, and an answer waits for its own switch to be heard rather than a guessed
//! delay. While any claim holds, the host's own source is kept on disk
//! ([`Sources::keep_at`]), so a run that ends without letting go brings it back at the next
//! start.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{oneshot, watch};

use crate::chords::{ChordTable, KeyLayout};

/// The most an answer waits for its switch to be heard.
///
/// The switch reaches every app, the host included, as `HIToolbox`'s distributed
/// notification, so the host hearing it is the sign the target app is reading under it; this
/// only bounds a notification that never comes.
pub const SETTLE_MOST: Duration = Duration::from_millis(100);

/// Who asks for an input source: one stream, by a token no other stream of this process ever
/// draws.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Claimant(u64);

impl Claimant {
    /// A token of its own.
    #[must_use]
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// What to do once a claim went: select one source, then turn some off.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Restore {
    /// The source to select, when it changes.
    pub select: Option<String>,
    /// Sources the host turned on for claims, to turn off once none asks.
    pub disable: Vec<String>,
    /// The source the claims last selected: `select` goes only while the host is still
    /// under it, since any other is one the person picked by hand.
    pub ours: Option<String>,
}

/// What the host keeps on disk while a claim holds: its own source, the sources it turned
/// on, and the one it last selected.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Kept {
    /// The source the host had before the first claim.
    pub original: Option<String>,
    /// Sources the host turned on for claims.
    pub enabled: Vec<String>,
    /// The source the claims last selected.
    pub selected: Option<String>,
}

impl Kept {
    /// One line each: `original <id>`, `selected <id>`, then `enabled <id>` for each.
    #[must_use]
    pub fn to_text(&self) -> String {
        let original = self.original.iter().map(|id| format!("original {id}\n"));
        let selected = self.selected.iter().map(|id| format!("selected {id}\n"));
        let enabled = self.enabled.iter().map(|id| format!("enabled {id}\n"));
        original.chain(selected).chain(enabled).collect()
    }

    /// Read back what [`Self::to_text`] wrote; a line it does not know is skipped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut kept = Self::default();
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("original", id)) if !id.is_empty() => kept.original = Some(id.to_owned()),
                Some(("selected", id)) if !id.is_empty() => kept.selected = Some(id.to_owned()),
                Some(("enabled", id)) if !id.is_empty() => kept.enabled.push(id.to_owned()),
                _ => {}
            }
        }
        kept
    }

    /// What brings the host back to how it was.
    #[must_use]
    pub fn restore(self) -> Restore {
        Restore {
            select: self.original,
            disable: self.enabled,
            ours: self.selected,
        }
    }
}

/// Which claimants asked for which input source, newest last, what the host had first, what
/// it turned on for them, and what it last selected.
#[derive(Debug, Default)]
pub struct Claims {
    asked: Vec<(Claimant, String)>,
    /// The host's own source, read when the first claimant asked; `None` before.
    original: Option<String>,
    enabled: Vec<String>,
    selected: Option<String>,
}

impl Claims {
    /// No claimant has asked.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            asked: Vec::new(),
            original: None,
            enabled: Vec::new(),
            selected: None,
        }
    }

    /// `who` asks for `source` while the host has `current`: the source to select now.
    pub fn claim(&mut self, who: Claimant, source: String, current: Option<String>) -> String {
        if self.asked.is_empty() {
            self.original = current;
        }
        self.asked.retain(|(c, _)| *c != who);
        self.asked.push((who, source.clone()));
        source
    }

    /// The claims selected `source`, turning it on first when `enabled`. Once none holds,
    /// nothing is recorded: the host's own source is its own again.
    pub fn selected(&mut self, source: String, enabled: bool) {
        if self.asked.is_empty() {
            return;
        }
        if enabled && !self.enabled.contains(&source) {
            self.enabled.push(source.clone());
        }
        self.selected = Some(source);
    }

    /// `who` let go, or its claim failed: what to select and turn off now.
    pub fn release(&mut self, who: Claimant) -> Restore {
        let was_latest = self.asked.last().is_some_and(|(c, _)| *c == who);
        let before = self.asked.len();
        self.asked.retain(|(c, _)| *c != who);
        if self.asked.len() == before {
            return Restore::default();
        }
        match self.asked.last() {
            None => self.release_all(),
            Some((_, source)) if was_latest => Restore {
                select: Some(source.clone()),
                disable: Vec::new(),
                ours: self.selected.clone(),
            },
            Some(_) => Restore::default(),
        }
    }

    /// Every claim goes (the host stops): the host's own source back, and what it turned on
    /// off.
    pub fn release_all(&mut self) -> Restore {
        self.asked.clear();
        Restore {
            select: self.original.take(),
            disable: std::mem::take(&mut self.enabled),
            ours: self.selected.take(),
        }
    }

    /// What to keep on disk: `None` once no claim holds.
    #[must_use]
    pub fn kept(&self) -> Option<Kept> {
        (!self.asked.is_empty()).then(|| Kept {
            original: self.original.clone(),
            enabled: self.enabled.clone(),
            selected: self.selected.clone(),
        })
    }

    /// The source `who` asked for, while its claim holds.
    #[must_use]
    pub fn of(&self, who: Claimant) -> Option<&str> {
        self.asked
            .iter()
            .find(|(c, _)| *c == who)
            .map(|(_, source)| source.as_str())
    }
}

/// Text Input Sources Services, or a stand-in: where the claims are run and what they call.
pub trait Tis: Send + Sync + 'static {
    /// Run `job` where TIS may be called, after every job handed over before it.
    fn on_main(&self, job: Box<dyn FnOnce() + Send>);
    /// The selected keyboard input source.
    fn current(&self) -> Option<String>;
    /// Select `id`; whether it had to be turned on first.
    ///
    /// # Errors
    ///
    /// Why it was not selected.
    fn select(&self, id: &str) -> Result<bool, String>;
    /// Turn `id` off.
    fn disable(&self, id: &str);
    /// What the current keyboard layout types where, for a chord's character
    /// ([`crate::chords`]); `None` where no layout is read, which leaves the one read before.
    fn layout(&self) -> Option<ChordTable> {
        None
    }
}

/// A platform with no input sources to select: every claim is refused, and the client composes.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unsupported;

impl Tis for Unsupported {
    fn on_main(&self, job: Box<dyn FnOnce() + Send>) {
        job();
    }

    fn current(&self) -> Option<String> {
        None
    }

    fn select(&self, _id: &str) -> Result<bool, String> {
        Err("no input sources here".to_owned())
    }

    fn disable(&self, _id: &str) {}
}

/// How a claim was answered on the main queue.
enum Claimed {
    /// The host was under the source already: nothing changed.
    Current,
    /// Selected now: the answer waits for the switch to be heard, on a receiver that has
    /// seen everything heard before the switch.
    Switched(watch::Receiver<Option<String>>),
    /// Not selected; the client composes.
    Refused,
}

/// The host's input-source claims, shared by every stream on every connection.
#[derive(Clone)]
pub struct Sources(Arc<Inner>);

struct Inner {
    tis: Box<dyn Tis>,
    claims: parking_lot::Mutex<Claims>,
    /// The source the host is under, as the platform last reported a switch.
    heard: watch::Sender<Option<String>>,
    /// The platform reports every switch ([`Sources::hearing`]); without, a selection made here
    /// counts as heard at once.
    hearing: AtomicBool,
    /// Where the host's own source is kept while a claim holds, and what was written there.
    kept: parking_lot::Mutex<(Option<PathBuf>, Option<Kept>)>,
    /// The layout a chord's character is looked up in, read again at each switch heard.
    layout: KeyLayout,
}

impl std::fmt::Debug for Sources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources")
            .field("claims", &*self.0.claims.lock())
            .finish_non_exhaustive()
    }
}

/// One stream's claim on the host's input source. Dropped, however the stream ends (a panic
/// included), it queues its release behind every ask it made.
#[derive(Debug)]
pub struct Claim {
    sources: Sources,
    who: Claimant,
}

impl Claim {
    /// Ask for `source`: see [`Sources::claim`].
    pub fn ask(&self, source: String) -> impl Future<Output = bool> + Send + 'static {
        self.sources.claim(self.who, source)
    }

    /// Give the source back while the stream goes on (the tile is idle); asking again claims
    /// it anew.
    pub fn release(&self) {
        self.sources.release(self.who);
    }

    /// The source the host is under, as each switch is heard: see [`Sources::subscribe`].
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.sources.subscribe()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.sources.release(self.who);
    }
}

impl Sources {
    /// Claims run through `tis`.
    pub fn new(tis: impl Tis) -> Self {
        Self(Arc::new(Inner {
            tis: Box::new(tis),
            claims: parking_lot::Mutex::new(Claims::new()),
            heard: watch::Sender::new(None),
            hearing: AtomicBool::new(false),
            kept: parking_lot::Mutex::new((None, None)),
            layout: KeyLayout::system(),
        }))
    }

    /// This machine's input sources: `HIToolbox` on the main queue on macOS, none elsewhere.
    #[must_use]
    pub fn system() -> Self {
        #[cfg(target_os = "macos")]
        return Self::new(mac::Hitoolbox);
        #[cfg(not(target_os = "macos"))]
        Self::new(Unsupported)
    }

    /// A claim for a new stream, released when dropped.
    #[must_use]
    pub fn claimant(&self) -> Claim {
        Claim {
            sources: self.clone(),
            who: Claimant::next(),
        }
    }

    /// Keep the host's own source at `path` while a claim holds, and first bring back what a
    /// run that ended without letting go left there, unless the person picked another since.
    pub fn keep_at(&self, path: PathBuf) {
        let inner = Arc::clone(&self.0);
        self.0.tis.on_main(Box::new(move || {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let left = Kept::parse(&text);
                tracing::info!(?left, "a run ended with a client's input source: restoring");
                inner.restore(left.restore());
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(error = %e, path = %path.display(), "input source kept");
                }
            }
            *inner.kept.lock() = (Some(path), None);
        }));
    }

    /// The platform reports every switch through [`Self::heard`] from now on.
    pub fn hearing(&self) {
        self.0.hearing.store(true, Ordering::Relaxed);
    }

    /// The host switched to `current` (the platform's notification). Every report counts,
    /// the same source again included: an answer waits for one made after its switch. Called
    /// where TIS may be called (the notification's own thread, the main one on macOS): the
    /// layout a chord's character is looked up in is read again here.
    pub fn heard(&self, current: Option<String>) {
        if let Some(table) = self.0.tis.layout() {
            self.0.layout.set(table);
        }
        self.0.heard.send_replace(current);
    }

    /// The source the host is under, as each switch is heard.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.0.heard.subscribe()
    }

    /// `who` asks for `source`. The ask is queued now, behind every ask and release before it;
    /// the answer, whether the host types under it, comes at once when it was already
    /// selected, and otherwise once a switch to it is heard after the selection (at most
    /// [`SETTLE_MOST`] after it).
    pub fn claim(
        &self,
        who: Claimant,
        source: String,
    ) -> impl Future<Output = bool> + Send + 'static {
        let (answer, answered) = oneshot::channel();
        let inner = Arc::clone(&self.0);
        let want = source.clone();
        self.0.tis.on_main(Box::new(move || {
            let _gone = answer.send(inner.claim(who, source));
        }));
        async move {
            let mut heard = match answered.await.unwrap_or(Claimed::Refused) {
                Claimed::Current => return true,
                Claimed::Refused => return false,
                Claimed::Switched(heard) => heard,
            };
            let settled = async {
                while heard.changed().await.is_ok() {
                    if heard.borrow_and_update().as_deref() == Some(want.as_str()) {
                        return;
                    }
                }
            };
            if tokio::time::timeout(SETTLE_MOST, settled).await.is_err() {
                tracing::debug!(source = want, "input source switch not heard; answering");
            }
            true
        }
    }

    /// `who` let go: the source it asked for goes, the one before comes back. Queued behind
    /// every ask before it.
    pub fn release(&self, who: Claimant) {
        let inner = Arc::clone(&self.0);
        self.0.tis.on_main(Box::new(move || {
            let back = inner.claims.lock().release(who);
            inner.restore(back);
            inner.keep();
        }));
    }

    /// Every claim goes (the host stops): its own source back, and what it turned on off.
    /// Done when the future is.
    pub fn release_all(&self) -> impl Future<Output = ()> + Send + 'static {
        let (done, finished) = oneshot::channel();
        let inner = Arc::clone(&self.0);
        self.0.tis.on_main(Box::new(move || {
            inner.release_all();
            let _gone = done.send(());
        }));
        async move {
            let _answered = finished.await;
        }
    }
}

impl Inner {
    /// Let go of every claim, on the main queue: the host's own source back, and what the
    /// claims turned on off.
    fn release_all(&self) {
        let back = self.claims.lock().release_all();
        self.restore(back);
        self.keep();
    }

    /// Run `who`'s ask, on the main queue.
    fn claim(&self, who: Claimant, source: String) -> Claimed {
        let current = self.tis.current();
        let mut claims = self.claims.lock();
        let want = claims.claim(who, source, current.clone());
        if current.as_deref() == Some(want.as_str()) {
            drop(claims);
            self.keep();
            return Claimed::Current;
        }
        match self.tis.select(&want) {
            Ok(enabled) => {
                claims.selected(want.clone(), enabled);
                drop(claims);
                // Subscribed after the selection: only a report made since counts.
                let heard = self.heard.subscribe();
                self.selected(want);
                self.keep();
                Claimed::Switched(heard)
            }
            Err(e) => {
                tracing::info!(source = want, error = %e, "keyboard input source not taken");
                let back = claims.release(who);
                drop(claims);
                self.restore(back);
                self.keep();
                Claimed::Refused
            }
        }
    }

    /// Select and turn off what a release says, on the main queue. The select goes only while
    /// the host is under the source the claims last selected; a source the person picked by
    /// hand stays, and the one the host is under is never turned off.
    fn restore(&self, back: Restore) {
        if let Some(source) = back.select
            && back.ours.is_some()
            && self.tis.current() == back.ours
        {
            match self.tis.select(&source) {
                Ok(enabled) => {
                    self.claims.lock().selected(source.clone(), enabled);
                    self.selected(source);
                }
                Err(e) => tracing::info!(source, error = %e, "keyboard input source not restored"),
            }
        }
        let current = self.tis.current();
        for source in back.disable {
            if current.as_deref() != Some(source.as_str()) {
                self.tis.disable(&source);
            }
        }
    }

    /// `source` was selected here: heard at once when the platform reports no switches.
    fn selected(&self, source: String) {
        if !self.hearing.load(Ordering::Relaxed) {
            self.heard.send_replace(Some(source));
        }
    }

    /// Write what the claims keep, or remove it once none holds, when it changed.
    fn keep(&self) {
        let now = self.claims.lock().kept();
        let (path, written) = self.kept.lock().clone();
        let Some(path) = path else { return };
        if written == now {
            return;
        }
        let done = match &now {
            Some(k) => std::fs::write(&path, k.to_text()),
            None => std::fs::remove_file(&path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            }),
        };
        match done {
            Ok(()) => self.kept.lock().1 = now,
            Err(e) => tracing::warn!(error = %e, path = %path.display(), "input source kept"),
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use dispatch2::DispatchQueue;

    use super::{ChordTable, Tis};
    use crate::input_source;

    /// Text Input Sources Services, which asserts it is called on the main queue.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Hitoolbox;

    impl Tis for Hitoolbox {
        fn on_main(&self, job: Box<dyn FnOnce() + Send>) {
            DispatchQueue::main().exec_async(job);
        }

        fn current(&self) -> Option<String> {
            input_source::current()
        }

        fn select(&self, id: &str) -> Result<bool, String> {
            let enabled = input_source::select(id).map_err(|e| e.to_string())?;
            if input_source::current().as_deref() != Some(id) {
                return Err("selected, yet another source is current".to_owned());
            }
            Ok(enabled)
        }

        fn disable(&self, id: &str) {
            if let Err(e) = input_source::disable(id) {
                tracing::info!(source = id, error = %e, "keyboard input source left on");
            }
        }

        fn layout(&self) -> Option<ChordTable> {
            input_source::layout_keys().map(ChordTable::from_keys)
        }
    }
}

#[cfg(test)]
mod tests;
