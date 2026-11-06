//! The clipboard that follows the owner: announce a copy, fetch it on paste.
//!
//! [`Clipboard`] is one engine's side, pure behind a [`Board`], [`Peers`] and a [`Clock`]:
//!
//! - **Watching.** While someone listens ([`Clipboard::subscribe`]), the board's `changeCount` is
//!   looked at every [`POLL`]. A change becomes an offer: its items' types and sizes, the small
//!   ones inline. Nothing leaves that carries a privacy marker, that Universal Clipboard brought,
//!   or that onicode wrote itself (the origin marker, and the count its own write left).
//! - **Applying.** A peer's offer that is newer by `(at, origin, seq)` than everything applied or
//!   announced here is written at once: inline types as bytes, the rest as promises a paste fetches
//!   from the origin, files through the transfer engine.
//! - **Serving.** A peer that pastes asks the origin for one type of one item of the offer it holds
//!   ([`Clipboard::serve`]); only what the offer announced, and only while the board still holds
//!   it.
//! - **Following.** While a remote desktop viewer has focus, the clipboard follows that session
//!   both ways ([`Clipboard::follow`]).

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures::stream::BoxStream;
use onicode_proto::clipboard::{
    ClipboardItem, ClipboardOffer, ClipboardSettings, ClipboardType, INLINE_MAX,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use crate::board::{Board, Capped, Item, Provide};
use crate::bridge::wait;
use crate::files::{Files, file_size};
use crate::types::{
    AUTO_GENERATED_TYPE, CONCEALED_TYPE, FILE_URL_TYPE, ORIGIN_TYPE, REMOTE_CLIPBOARD_TYPE,
    TRANSIENT_TYPE, carried,
};

/// How often the board is looked at while someone listens: a copy reaches a peer within this
/// and a link's delay, and a look at an unchanged board is one cheap call.
pub const POLL: Duration = Duration::from_millis(150);
/// How long a paste waits for a type's bytes from its origin. Past it the paste gets nothing
/// and the board stays as it was, so a peer that left never hangs an app.
pub const FETCH_DEADLINE: Duration = Duration::from_secs(3);
/// How long a paste of files waits for the transfer engine: about a gigabyte on a fast tailnet.
pub const FILE_DEADLINE: Duration = Duration::from_secs(120);
/// Files of a received offer are fetched at once, before any paste, when each is a regular
/// file of known size and together they are no more than this.
///
/// A paste of files blocks the app that pastes until they land, and Finder shows nothing while
/// it waits; most copied files (documents, screenshots, small archives) are well under this, so
/// prefetching them makes the paste as instant as a local one. It costs at most this much of
/// the link and of the cache per copy that is never pasted, and the cache keeps only the newest
/// offer. Bigger copies, and folders (whose size needs a walk), wait for the paste.
pub const PREFETCH_MAX: u64 = 16 << 20;
/// The most bytes one fetch takes in.
pub const FETCH_MAX: usize = 512 << 20;

/// The ordering of offers: the newest `(at, origin, seq)` wins. `seq` breaks a tie between two
/// copies of one device in one millisecond.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Key {
    /// When it was copied, epoch milliseconds, never behind what its origin had seen.
    pub at: i64,
    /// The device it was copied on.
    pub origin: String,
    /// The origin's count of offers.
    pub seq: u64,
}

impl Key {
    /// `offer`'s key.
    #[must_use]
    pub fn of(offer: &ClipboardOffer) -> Self {
        Self {
            at: offer.at,
            origin: offer.origin_device.clone(),
            seq: offer.origin_seq,
        }
    }
}

/// Whether this engine may read the pasteboard unasked (macOS 15.4+'s "Paste from Other Apps").
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PasteAccess {
    /// No pasteboard privacy here: off macOS, or a board of onicode's own.
    #[default]
    Ungated,
    /// Never asked yet: a read would prompt, and the app is not yet listed in System Settings.
    NotAsked,
    /// Listed and set to ask: a read would prompt.
    Ask,
    /// Listed and allowed: reads never prompt.
    Allowed,
    /// Listed and denied: reads fail, and the clipboard stops following.
    Denied,
}

impl PasteAccess {
    /// Whether a copy's bytes may be read when it is announced, for its sizes and the inline
    /// fast path. Otherwise an offer carries types alone and the origin reads only when a peer
    /// pastes, so nobody is prompted on a Mac they are not looking at until they paste.
    #[must_use]
    pub const fn reads_freely(self) -> bool {
        matches!(self, Self::Ungated | Self::Allowed)
    }

    /// Whether the watcher stops.
    #[must_use]
    pub const fn stops(self) -> bool {
        matches!(self, Self::Denied)
    }

    /// Why the clipboard is held back, in words for the grants panel.
    #[must_use]
    pub const fn why(self) -> Option<&'static str> {
        match self {
            Self::Ungated | Self::Allowed => None,
            Self::NotAsked | Self::Ask => Some(
                "Copies here go to your other devices once one pastes, and this Mac asks first.",
            ),
            Self::Denied => {
                Some("Paste from Other Apps is off for Onicode, so copies on this Mac stay here.")
            }
        }
    }
}

/// Epoch milliseconds, for an offer's `at`.
pub trait Clock: Send + Sync + 'static {
    /// Now.
    fn now_ms(&self) -> i64;
}

/// The system's clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
            })
    }
}

/// The owner's other engines, as a paste on this one reaches them.
#[async_trait]
pub trait Peers: Send + Sync + 'static {
    /// The bytes of type `uti` of item `item` of `origin`'s offer `seq`.
    ///
    /// # Errors
    ///
    /// The origin is gone, or no longer holds the offer.
    async fn fetch(
        &self,
        origin: &str,
        seq: u64,
        item: usize,
        uti: &str,
    ) -> Result<Vec<u8>, String>;

    /// Copies `paths` on `origin` into the local folder `into`, each under its own name, and
    /// returns once they are all there.
    ///
    /// # Errors
    ///
    /// The transfer failed or was cancelled.
    async fn fetch_files(
        &self,
        origin: &str,
        paths: &[String],
        into: &std::path::Path,
    ) -> Result<(), String>;
}

/// Reads [`PasteAccess`] for the board, cheaply enough to ask on every look.
pub type Access = Arc<dyn Fn() -> PasteAccess + Send + Sync>;

/// What a [`Clipboard`] works with.
pub struct Config {
    /// This engine's device id.
    pub device: String,
    /// The pasteboard.
    pub board: Arc<dyn Board>,
    /// The other engines.
    pub peers: Arc<dyn Peers>,
    /// Stamps offers.
    pub clock: Arc<dyn Clock>,
    /// Whether the board may be read unasked.
    pub access: Access,
    /// Where received files land, one folder per offer: `~/Library/Caches/onicode/clipboard`.
    pub cache: PathBuf,
    /// The settings it starts with.
    pub settings: ClipboardSettings,
}

/// What became of a peer's offer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Received {
    /// It is on the board.
    Applied,
    /// Something newer was applied or announced here.
    Stale,
    /// Not taken: sync is off, it is this device's own, another device is followed, or it holds
    /// nothing.
    Ignored,
    /// The board refused the write.
    Failed,
}

/// One engine's clipboard. Cheap to clone.
#[derive(Clone)]
pub struct Clipboard(Arc<Inner>);

struct Inner {
    device: String,
    board: Arc<dyn Board>,
    peers: Arc<dyn Peers>,
    clock: Arc<dyn Clock>,
    access: Access,
    cache: PathBuf,
    runtime: Handle,
    state: Mutex<State>,
    /// The newest offer announced here.
    offers: watch::Sender<Option<Arc<ClipboardOffer>>>,
    /// The device whose session has focus.
    follow: watch::Sender<Option<String>>,
    settings: watch::Sender<ClipboardSettings>,
    /// How many subscriptions are open.
    listeners: watch::Sender<usize>,
}

#[derive(Default)]
struct State {
    /// The count last looked at.
    seen: Option<isize>,
    /// The count this engine's own last write left.
    own: Option<isize>,
    /// The last offer's sequence number.
    seq: u64,
    /// The newest offer applied or announced.
    newest: Option<Key>,
    /// What the newest announced offer is on the board.
    announced: Option<Announced>,
    /// The last peer's offer written.
    written: Option<Written>,
}

/// An announced offer as it sits on the board.
struct Announced {
    seq: u64,
    count: isize,
    /// Per offer item: its index on the board and the types offered.
    items: Vec<(usize, Vec<String>)>,
}

/// A peer's offer as written here.
struct Written {
    count: isize,
    /// Promises not yet kept.
    unkept: Arc<AtomicUsize>,
}

impl Clipboard {
    /// A clipboard over `config`, its fetches run on `runtime`.
    #[must_use]
    pub fn new(config: Config, runtime: Handle) -> Self {
        // Sequence numbers start at the clock, so they keep rising across restarts.
        let seq = u64::try_from(config.clock.now_ms()).unwrap_or(0);
        Self(Arc::new(Inner {
            device: config.device,
            board: config.board,
            peers: config.peers,
            clock: config.clock,
            access: config.access,
            cache: config.cache,
            runtime,
            state: Mutex::new(State {
                seq,
                ..State::default()
            }),
            offers: watch::Sender::new(None),
            follow: watch::Sender::new(None),
            settings: watch::Sender::new(config.settings),
            listeners: watch::Sender::new(0),
        }))
    }

    /// This engine's device id.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.0.device
    }

    /// The settings now.
    #[must_use]
    pub fn settings(&self) -> ClipboardSettings {
        *self.0.settings.borrow()
    }

    /// Changes the settings.
    pub fn set_settings(&self, settings: ClipboardSettings) {
        self.0.settings.send_replace(settings);
    }

    /// Whether the board may be read unasked, as the access check says now.
    #[must_use]
    pub fn access(&self) -> PasteAccess {
        (self.0.access)()
    }

    /// While `device`'s session has focus, only its offers apply here and this engine's go only
    /// to it; `None` follows every device again.
    pub fn follow(&self, device: Option<String>) {
        self.0.follow.send_if_modified(|followed| {
            let changed = *followed != device;
            *followed = device;
            changed
        });
    }

    /// Stops following `device`, if it is the one followed: a viewer losing focus after another
    /// gained it leaves the newer one followed.
    pub fn unfollow(&self, device: &str) {
        self.0.follow.send_if_modified(|followed| {
            let ours = followed.as_deref() == Some(device);
            if ours {
                *followed = None;
            }
            ours
        });
    }

    /// The device followed, if any.
    #[must_use]
    pub fn followed(&self) -> Option<String> {
        self.0.follow.borrow().clone()
    }

    /// The newest offer applied or announced here.
    #[must_use]
    pub fn newest(&self) -> Option<Key> {
        self.0.state.lock().newest.clone()
    }

    /// The newest offer announced here.
    #[must_use]
    pub fn announced(&self) -> Option<Arc<ClipboardOffer>> {
        self.0.offers.borrow().clone()
    }

    /// This engine's offers, as `subscriber` (a peer's device id; `None` for this device's own
    /// app) may see them: the newest at once, then each new one. While another device is
    /// followed, a peer that is not it sees nothing. The board is watched while any
    /// subscription is open.
    pub fn subscribe(&self, subscriber: Option<String>) -> BoxStream<'static, ClipboardOffer> {
        let start = Subscription {
            offers: self.0.offers.subscribe(),
            follow: self.0.follow.subscribe(),
            subscriber,
            sent: None,
            _listening: Listening::new(&self.0),
        };
        Box::pin(futures::stream::unfold(start, |mut sub| async move {
            let offer = sub.next().await?;
            Some((offer, sub))
        }))
    }

    /// Watches the board every [`POLL`] while sync is on and someone listens, until the
    /// clipboard is dropped. Each look runs on a blocking thread: a board's calls may wait on
    /// the pasteboard server.
    pub fn watcher(&self) -> impl Future<Output = ()> + Send + 'static {
        let weak = Arc::downgrade(&self.0);
        let mut listeners = self.0.listeners.subscribe();
        let mut settings = self.0.settings.subscribe();
        async move {
            let mut tick = tokio::time::interval(POLL);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                loop {
                    let idle = *listeners.borrow_and_update() == 0
                        || !settings.borrow_and_update().enabled;
                    if !idle {
                        break;
                    }
                    let woke = tokio::select! {
                        changed = listeners.changed() => changed,
                        changed = settings.changed() => changed,
                    };
                    if woke.is_err() {
                        return;
                    }
                    tick.reset();
                }
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let clipboard = Self(inner);
                if tokio::task::spawn_blocking(move || clipboard.look())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }

    /// Looks at the board once: a new offer when it changed by someone else's copy and holds
    /// something that may leave, announced to every subscription.
    pub fn look(&self) -> Option<Arc<ClipboardOffer>> {
        let inner = &*self.0;
        let settings = self.settings();
        if !settings.enabled {
            return None;
        }
        let access = (inner.access)();
        if access.stops() {
            return None;
        }
        let count = inner.board.change_count();
        {
            let mut state = inner.state.lock();
            if state.seen == Some(count) {
                return None;
            }
            state.seen = Some(count);
            if state.own == Some(count) {
                return None;
            }
        }
        let read = read_board(&*inner.board, settings, access.reads_freely())?;
        let mut state = inner.state.lock();
        // The board moved while it was read (a peer's offer landed, or another copy): the next
        // look reads it again, unless it was this engine's own write.
        if inner.board.change_count() != count {
            state.seen = None;
            return None;
        }
        state.seq = state.seq.saturating_add(1);
        let floor = state
            .newest
            .as_ref()
            .map_or(i64::MIN, |newest| newest.at.saturating_add(1));
        let offer = Arc::new(ClipboardOffer {
            origin_device: inner.device.clone(),
            origin_seq: state.seq,
            at: inner.clock.now_ms().max(floor),
            items: read.items,
            concealed: false,
        });
        state.newest = Some(Key::of(&offer));
        state.announced = Some(Announced {
            seq: state.seq,
            count,
            items: read.index,
        });
        drop(state);
        inner.offers.send_replace(Some(Arc::clone(&offer)));
        tracing::debug!(
            seq = offer.origin_seq,
            items = offer.items.len(),
            "clipboard announced"
        );
        Some(offer)
    }

    /// Takes a peer's offer: written to the board when it is newer than everything applied or
    /// announced here and the followed device (if any) made it.
    pub fn receive(&self, offer: ClipboardOffer) -> Received {
        let inner = &*self.0;
        if !self.settings().enabled
            || offer.concealed
            || offer.origin_device == inner.device
            || offer.items.is_empty()
        {
            return Received::Ignored;
        }
        if let Some(followed) = inner.follow.borrow().as_deref()
            && followed != offer.origin_device
        {
            return Received::Ignored;
        }
        let key = Key::of(&offer);
        let offer = Arc::new(offer);
        let plan = Plan::of(&offer);
        let unkept = Arc::new(AtomicUsize::new(plan.promised));
        let dir = inner
            .cache
            .join(format!("{}-{}", offer.origin_device, offer.origin_seq));
        let files = Arc::new(Files {
            peers: Arc::clone(&inner.peers),
            origin: offer.origin_device.clone(),
            seq: offer.origin_seq,
            dir: dir.clone(),
            sources: plan.files,
            done: tokio::sync::OnceCell::new(),
        });
        let provide = (plan.promised > 0).then(|| {
            provider(
                Arc::downgrade(&self.0),
                Arc::clone(&offer),
                Arc::clone(&files),
                Arc::clone(&unkept),
            )
        });
        let mut state = inner.state.lock();
        if state.newest.as_ref().is_some_and(|newest| key <= *newest) {
            return Received::Stale;
        }
        // Written under the state lock, so a look in between cannot take it for a copy.
        let Some(count) = inner.board.write(&plan.items, provide) else {
            return Received::Failed;
        };
        state.own = Some(count);
        state.seen = Some(count);
        state.newest = Some(key);
        state.announced = None;
        state.written = Some(Written { count, unkept });
        drop(state);
        tracing::debug!(origin = %offer.origin_device, seq = offer.origin_seq, "clipboard applied");
        self.keep_only(dir);
        if prefetches(&offer) {
            let _task = inner.runtime.spawn(async move {
                let _url = Arc::clone(&files).url(0).await;
            });
        }
        Received::Applied
    }

    /// The bytes of type `uti` of item `item` of this engine's offer `seq`, for a peer that
    /// pastes it; `None` when the offer is not the board's any more or did not offer that.
    /// Reads the board: call it off the runtime's threads.
    #[must_use]
    pub fn serve(&self, seq: u64, item: usize, uti: &str) -> Option<Vec<u8>> {
        let inner = &*self.0;
        let (count, on_board) = {
            let state = inner.state.lock();
            let announced = state.announced.as_ref().filter(|a| a.seq == seq)?;
            let (on_board, types) = announced.items.get(item)?;
            if !types.iter().any(|t| t == uti) {
                return None;
            }
            (announced.count, *on_board)
        };
        if inner.board.change_count() != count {
            return None;
        }
        let bytes = inner.board.data(on_board, uti)?;
        // The copy moved on while it was read.
        (inner.board.change_count() == count).then_some(bytes)
    }

    /// Clears the board if it still holds a peer's offer with promises not kept, so the system
    /// does not ask for every one of them as the process quits. `true` when it cleared.
    pub fn shutdown(&self) -> bool {
        let state = self.0.state.lock();
        let Some(written) = state.written.as_ref() else {
            return false;
        };
        if written.unkept.load(Ordering::Acquire) == 0 {
            return false;
        }
        self.0.board.clear_if(written.count).is_some()
    }

    /// Empties the file cache but for `keep`: no history is kept.
    fn keep_only(&self, keep: PathBuf) {
        let cache = self.0.cache.clone();
        let _task = self.0.runtime.spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&cache) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path != keep
                    && let Err(error) = std::fs::remove_dir_all(&path)
                {
                    tracing::debug!(path = %path.display(), %error, "clipboard cache entry kept");
                }
            }
        });
    }
}

/// Counts the open subscriptions while it lives.
struct Listening(Weak<Inner>);

impl Listening {
    fn new(inner: &Arc<Inner>) -> Self {
        inner.listeners.send_modify(|n| *n = n.saturating_add(1));
        Self(Arc::downgrade(inner))
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            inner.listeners.send_modify(|n| *n = n.saturating_sub(1));
        }
    }
}

struct Subscription {
    offers: watch::Receiver<Option<Arc<ClipboardOffer>>>,
    follow: watch::Receiver<Option<String>>,
    subscriber: Option<String>,
    sent: Option<Key>,
    _listening: Listening,
}

impl Subscription {
    async fn next(&mut self) -> Option<ClipboardOffer> {
        loop {
            let offer = self.offers.borrow_and_update().clone();
            let followed = self.follow.borrow_and_update().clone();
            if let Some(offer) = offer
                && visible(followed.as_deref(), self.subscriber.as_deref())
            {
                let key = Key::of(&offer);
                if self.sent.as_ref() != Some(&key) {
                    self.sent = Some(key);
                    return Some((*offer).clone());
                }
            }
            tokio::select! {
                changed = self.offers.changed() => changed.ok()?,
                changed = self.follow.changed() => changed.ok()?,
            }
        }
    }
}

/// Whether `subscriber` sees this engine's offers while `followed` is followed: everyone when
/// nobody is, else the followed device and this device's own app.
fn visible(followed: Option<&str>, subscriber: Option<&str>) -> bool {
    match (followed, subscriber) {
        (None, _) | (Some(_), None) => true,
        (Some(followed), Some(subscriber)) => followed == subscriber,
    }
}

/// An offer's items as read off the board.
#[derive(Default)]
struct Read {
    items: Vec<ClipboardItem>,
    /// Per offer item: its index on the board and the types offered.
    index: Vec<(usize, Vec<String>)>,
}

/// What may leave of the board's contents; `None` when nothing may.
///
/// A privacy marker keeps the whole copy home, whichever item it sits on: a password manager
/// may write it as an item of its own. `free` reads each type's bytes for its size and the
/// inline fast path; otherwise only the types are read, which never prompts.
fn read_board(board: &dyn Board, settings: ClipboardSettings, free: bool) -> Option<Read> {
    let all = board.items();
    let marked = |kind: &str| all.iter().flatten().any(|t| t == kind);
    if marked(ORIGIN_TYPE)
        || marked(REMOTE_CLIPBOARD_TYPE)
        || marked(CONCEALED_TYPE)
        || marked(TRANSIENT_TYPE)
        || (!settings.auto_generated && marked(AUTO_GENERATED_TYPE))
    {
        return None;
    }
    let mut read = Read::default();
    for (n, kinds) in all.iter().enumerate() {
        let mut types = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for uti in kinds.iter().filter(|t| carried(t)) {
            if names.contains(uti) {
                continue;
            }
            let mut offered = ClipboardType {
                uti: uti.clone(),
                size: 0,
                inline: None,
                file_size: None,
            };
            if free {
                match board.data_within(n, uti, INLINE_MAX) {
                    Some(Capped::Data(bytes)) => {
                        offered.size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        if uti == FILE_URL_TYPE {
                            offered.file_size = file_size(&bytes);
                        }
                        offered.inline = Some(STANDARD.encode(&bytes));
                    }
                    Some(Capped::TooBig(size)) => offered.size = size,
                    None => continue,
                }
            }
            names.push(uti.clone());
            types.push(offered);
        }
        if !types.is_empty() {
            read.items.push(ClipboardItem { types });
            read.index.push((n, names));
        }
    }
    (!read.items.is_empty()).then_some(read)
}

/// A peer's offer as it is written here.
struct Plan {
    items: Vec<Item>,
    /// How many types are promised.
    promised: usize,
    /// Per offer item, its file ([`Files::sources`]).
    files: Vec<Option<Option<Vec<u8>>>>,
}

impl Plan {
    fn of(offer: &ClipboardOffer) -> Self {
        let mut plan = Self {
            items: Vec::with_capacity(offer.items.len()),
            promised: 0,
            files: Vec::with_capacity(offer.items.len()),
        };
        for item in &offer.items {
            let mut data = vec![(
                ORIGIN_TYPE.to_owned(),
                offer.origin_device.clone().into_bytes(),
            )];
            let mut promised: Vec<String> = Vec::new();
            let mut file = None;
            // A peer cannot plant a marker: only types that travel are written.
            for kind in item.types.iter().filter(|t| carried(&t.uti)) {
                if promised.contains(&kind.uti) || data.iter().any(|(t, _)| *t == kind.uti) {
                    continue;
                }
                let inline = kind
                    .inline
                    .as_deref()
                    .and_then(|text| STANDARD.decode(text).ok());
                if kind.uti == FILE_URL_TYPE {
                    // The origin's path means nothing here: the file is fetched on paste.
                    file = Some(inline);
                    promised.push(kind.uti.clone());
                } else if let Some(bytes) = inline {
                    data.push((kind.uti.clone(), bytes));
                } else {
                    promised.push(kind.uti.clone());
                }
            }
            plan.promised = plan.promised.saturating_add(promised.len());
            plan.items.push(Item { data, promised });
            plan.files.push(file);
        }
        plan
    }
}

/// Whether `offer`'s files are fetched before any paste ([`PREFETCH_MAX`]).
fn prefetches(offer: &ClipboardOffer) -> bool {
    let mut total = 0_u64;
    let mut any = false;
    for kind in offer
        .items
        .iter()
        .flat_map(|item| &item.types)
        .filter(|t| t.uti == FILE_URL_TYPE)
    {
        let Some(size) = kind.file_size else {
            return false;
        };
        any = true;
        total = total.saturating_add(size);
    }
    any && total <= PREFETCH_MAX
}

/// The promises of a written offer: each type fetched from its origin when pasted, files
/// through the transfer engine, within their deadlines.
fn provider(
    clipboard: Weak<Inner>,
    offer: Arc<ClipboardOffer>,
    files: Arc<Files>,
    unkept: Arc<AtomicUsize>,
) -> Provide {
    Arc::new(move |item, uti| {
        let inner = clipboard.upgrade()?;
        let got = if uti == FILE_URL_TYPE {
            let files = Arc::clone(&files);
            wait(&inner.runtime, FILE_DEADLINE, files.url(item))
        } else {
            let peers = Arc::clone(&inner.peers);
            let offer = Arc::clone(&offer);
            let uti = uti.to_owned();
            wait(&inner.runtime, FETCH_DEADLINE, async move {
                match peers
                    .fetch(&offer.origin_device, offer.origin_seq, item, &uti)
                    .await
                {
                    Ok(bytes) => Some(bytes),
                    Err(error) => {
                        tracing::debug!(origin = %offer.origin_device, %uti, %error, "clipboard fetch failed");
                        None
                    }
                }
            })
        };
        if got.is_some() {
            let _kept = unkept.try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
        }
        got
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_by_time_then_origin_then_seq() {
        let key = |at, origin: &str, seq| Key {
            at,
            origin: origin.to_owned(),
            seq,
        };
        assert!(key(2, "a", 1) > key(1, "z", 9));
        assert!(key(1, "b", 1) > key(1, "a", 9));
        assert!(key(1, "a", 2) > key(1, "a", 1));
    }

    #[test]
    fn a_followed_session_sees_alone_and_this_app_always() {
        assert!(visible(None, Some("b")));
        assert!(visible(Some("b"), Some("b")));
        assert!(!visible(Some("b"), Some("c")));
        assert!(visible(Some("b"), None));
    }

    #[test]
    fn small_regular_files_prefetch_and_folders_wait() {
        let offer = |sizes: &[Option<u64>]| ClipboardOffer {
            origin_device: "a".into(),
            origin_seq: 1,
            at: 1,
            items: sizes
                .iter()
                .map(|&file_size| ClipboardItem {
                    types: vec![ClipboardType {
                        uti: FILE_URL_TYPE.into(),
                        size: 20,
                        inline: None,
                        file_size,
                    }],
                })
                .collect(),
            concealed: false,
        };
        assert!(prefetches(&offer(&[Some(1 << 20), Some(2 << 20)])));
        assert!(!prefetches(&offer(&[Some(PREFETCH_MAX), Some(1)])));
        assert!(!prefetches(&offer(&[Some(1), None])), "a folder");
        assert!(!prefetches(&offer(&[])), "no files");
    }
}
