//! The clipboard over desktop sessions: one [`Hub`] per Mac (its [`Clipboard`] and the sessions
//! it can fetch through), one [`Session`] per desktop link.
//!
//! A session follows the other end while the viewer's window has focus. The viewer says so
//! ([`Session::focus`], which tells the host with [`Clip::Focus`]); the host hears it
//! ([`Session::heard`]) and tells back whether it may read its pasteboard without a prompt
//! ([`Clip::Access`]), again whenever that changes: until it may, the host's copies stay there.
//! While it follows, each end's copies go to the other as offers, and the other end's offers are
//! taken here, newest wins: what did not ride inline is fetched at once through the session the
//! offer came on ([`Line`]); a fetch from the other end is served only for the offer this session
//! sent ([`Session::serve`]).
//!
//! [`Line`] is the seam to the link: `crate::link::LinkLine` on QUIC, an in-memory pair in tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt as _;
use onicode_desktop_proto::clipboard::{Clip, Fetch, PasteAccess};
use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::board::Board;
use crate::sync::{Access, Clipboard, ClipboardSettings, Clock, Config, Peers, Received};

/// How often a following host reads its pasteboard access again, to tell the viewer a change:
/// set to Allow, the copies there start to travel within this.
pub const ACCESS_EVERY: Duration = Duration::from_secs(2);

/// One desktop link, as the clipboard reaches the other end.
#[async_trait]
pub trait Line: Send + Sync + 'static {
    /// Sends `clip` on the session's control stream; `false` once the link is gone.
    fn say(&self, clip: Clip) -> bool;

    /// The bytes `fetch` (a [`Fetch::Type`]) names, from the other end.
    ///
    /// # Errors
    ///
    /// The link is gone, the other end refused, or past [`crate::SEND_MAX`].
    async fn fetch(&self, fetch: Fetch) -> Result<Vec<u8>, String>;

    /// The file `fetch` (a [`Fetch::Files`]) names, copied from the other end into
    /// `into` under its own name; its local path once all of it is there.
    ///
    /// # Errors
    ///
    /// The link is gone, the other end refused, or the copy failed.
    async fn fetch_file(&self, fetch: Fetch, into: &Path) -> Result<PathBuf, String>;
}

/// The sessions a hub can fetch through, by the device at their other end. A device linked twice
/// (two windows on one Mac) fetches through the newest.
/// A session's line, with the number it was linked under so the newest wins.
type Linked = (u64, Weak<dyn Line>);

#[derive(Default)]
struct Lines {
    by_device: Mutex<HashMap<String, Linked>>,
    next: AtomicU64,
}

impl Lines {
    fn line(&self, origin: &str) -> Result<Arc<dyn Line>, String> {
        self.by_device
            .lock()
            .get(origin)
            .and_then(|(_, line)| line.upgrade())
            .ok_or_else(|| format!("no desktop session with {origin}"))
    }

    fn add(&self, device: &str, line: &Arc<dyn Line>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.by_device
            .lock()
            .insert(device.to_owned(), (id, Arc::downgrade(line)));
        id
    }

    fn remove(&self, device: &str, id: u64) {
        let mut lines = self.by_device.lock();
        if lines.get(device).is_some_and(|(kept, _)| *kept == id) {
            lines.remove(device);
        }
    }
}

#[async_trait]
impl Peers for Lines {
    async fn fetch(
        &self,
        origin: &str,
        seq: u64,
        item: usize,
        uti: &str,
    ) -> Result<Vec<u8>, String> {
        let line = self.line(origin)?;
        line.fetch(Fetch::Type {
            seq,
            item,
            uti: uti.to_owned(),
        })
        .await
    }

    async fn fetch_file(
        &self,
        origin: &str,
        seq: u64,
        item: usize,
        into: &Path,
    ) -> Result<PathBuf, String> {
        let line = self.line(origin)?;
        line.fetch_file(Fetch::Files { seq, item }, into).await
    }
}

/// What a [`Hub`] is made of.
pub struct HubConfig {
    /// This Mac's device id, as the other ends know it from its hello.
    pub device: String,
    /// The pasteboard.
    pub board: Arc<dyn Board>,
    /// Stamps offers.
    pub clock: Arc<dyn Clock>,
    /// Whether the board may be read unasked.
    pub access: Access,
    /// Where received files land.
    pub cache: PathBuf,
    /// The settings it starts with.
    pub settings: ClipboardSettings,
}

/// One Mac's clipboard and the desktop sessions it follows. Cheap to clone.
#[derive(Clone)]
pub struct Hub {
    clipboard: Clipboard,
    lines: Arc<Lines>,
    runtime: Handle,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("device", &self.clipboard.device())
            .finish_non_exhaustive()
    }
}

impl Hub {
    /// A hub over `config`, its watcher and fetches on `runtime`. The watcher runs until the
    /// last clone of the hub is dropped, looking at the board only while a session follows.
    #[must_use]
    pub fn start(config: HubConfig, runtime: &Handle) -> Self {
        let lines = Arc::new(Lines::default());
        let clipboard = Clipboard::new(
            Config {
                device: config.device,
                board: config.board,
                peers: Arc::clone(&lines) as Arc<dyn Peers>,
                clock: config.clock,
                access: config.access,
                cache: config.cache,
                settings: config.settings,
            },
            runtime.clone(),
        );
        drop(runtime.spawn(clipboard.watcher()));
        Self {
            clipboard,
            lines,
            runtime: runtime.clone(),
        }
    }

    /// The clipboard.
    #[must_use]
    pub const fn clipboard(&self) -> &Clipboard {
        &self.clipboard
    }

    /// A session with `peer` (the other end's device id) over `line`, until it is dropped.
    #[must_use]
    pub fn session(&self, peer: String, line: Arc<dyn Line>) -> Session {
        let id = self.lines.add(&peer, &line);
        Session {
            hub: self.clone(),
            peer,
            line,
            id,
            shared: Arc::new(Shared::default()),
        }
    }
}

/// What a session's tasks share with it.
#[derive(Default)]
struct Shared {
    /// The newest offer sent on this session: the one a fetch from the other end may name.
    offered: Mutex<Option<u64>>,
    following: Mutex<Option<JoinHandle<()>>>,
}

/// The clipboard on one desktop link. Dropping it stops following and fetching through it.
pub struct Session {
    hub: Hub,
    peer: String,
    line: Arc<dyn Line>,
    id: u64,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// The other end's device id.
    #[must_use]
    pub fn peer(&self) -> &str {
        &self.peer
    }

    /// This end's window showing the other end gained or lost focus: follow it, or let go, and
    /// tell the other end. `false` once the link is gone.
    pub fn focus(&self, focused: bool) -> bool {
        self.follow(focused, false);
        self.line.say(Clip::Focus { focused })
    }

    /// A clip from the other end.
    pub fn heard(&self, clip: Clip) {
        match clip {
            Clip::Focus { focused } => self.follow(focused, true),
            Clip::Offer(offer) => {
                if offer.origin != self.peer {
                    tracing::debug!(origin = %offer.origin, peer = %self.peer, "an offer in another's name: dropped");
                    return;
                }
                let clipboard = self.hub.clipboard.clone();
                // Writing may wait on the pasteboard server: off the runtime's threads.
                let _task = self.hub.runtime.spawn_blocking(move || {
                    let received = clipboard.receive(offer);
                    if !matches!(received, Received::Applied | Received::Fetching) {
                        tracing::debug!(?received, "a clipboard offer not applied");
                    }
                });
            }
            // The viewer reads it from the session's state; nothing to do with it here.
            Clip::Access(_) => {}
        }
    }

    /// The bytes or the path a fetch from the other end names: only for the offer this session
    /// sent, and only while it is this Mac's newest.
    #[must_use]
    pub fn serve(&self, fetch: &Fetch) -> Option<Served> {
        let offered = *self.shared.offered.lock();
        match fetch {
            Fetch::Type { seq, item, uti } if offered == Some(*seq) => self
                .hub
                .clipboard
                .serve(*seq, *item, uti)
                .map(Served::Bytes),
            Fetch::Files { seq, item } if offered == Some(*seq) => {
                self.hub.clipboard.serve_file(*seq, *item).map(Served::Path)
            }
            _ => None,
        }
    }

    /// Start following the other end, or stop. A host that follows because the viewer said so
    /// (`told`) tells it its pasteboard access, now and on every change.
    fn follow(&self, on: bool, told: bool) {
        let clipboard = &self.hub.clipboard;
        let mut following = self.shared.following.lock();
        if let Some(task) = following.take() {
            task.abort();
        }
        if !on {
            clipboard.unfollow(&self.peer);
            return;
        }
        clipboard.follow(Some(self.peer.clone()));
        let mut offers = clipboard.subscribe(Some(self.peer.clone()));
        let line = Arc::clone(&self.line);
        let shared = Arc::downgrade(&self.shared);
        let access = told.then(|| clipboard.clone());
        *following = Some(self.hub.runtime.spawn(async move {
            let mut told_access: Option<PasteAccess> = None;
            let mut every = tokio::time::interval(ACCESS_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    offer = offers.next() => {
                        let Some(offer) = offer else { return };
                        let Some(shared) = shared.upgrade() else { return };
                        *shared.offered.lock() = Some(offer.seq);
                        if !line.say(Clip::Offer(offer)) {
                            return;
                        }
                    }
                    _tick = every.tick(), if access.is_some() => {
                        let Some(clipboard) = access.clone() else { continue };
                        let Ok(now) = tokio::task::spawn_blocking(move || clipboard.access()).await
                        else {
                            return;
                        };
                        if told_access != Some(now) {
                            told_access = Some(now);
                            if !line.say(Clip::Access(now)) {
                                return;
                            }
                        }
                    }
                }
            }
        }));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.shared.following.lock().take() {
            task.abort();
        }
        self.hub.clipboard.unfollow(&self.peer);
        self.hub.lines.remove(&self.peer, self.id);
    }
}

/// What a fetch from the other end gets.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Served {
    /// A type's bytes.
    Bytes(Bytes),
    /// The local path of a copied file, to send as entries.
    Path(PathBuf),
}
