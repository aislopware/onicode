//! A desktop session's clipboard end to end: a viewer's hub and a host's, each on a `Held` board,
//! joined by an in-memory link that hands each clip straight to the other end and serves each
//! fetch from it, on tokio's paused clock. Nothing here touches a real pasteboard.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use onicode_clipboard::types::{CONCEALED_TYPE, ORIGIN_TYPE};
use onicode_clipboard::{
    Board as _, Clip, ClipboardSettings, Clock, Fetch, Held, Hub, HubConfig, Item, Key, Line, POLL,
    PasteAccess, Served, Session,
};
use parking_lot::Mutex;

const TEXT: &str = "public.utf8-plain-text";
const PNG: &str = "public.png";

/// Epoch milliseconds the test sets, shared by both Macs.
#[derive(Default)]
struct Ticks(AtomicI64);

impl Clock for Ticks {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// One direction of the link: what its end says, and the bytes its end fetched.
#[derive(Default)]
struct FakeLine {
    other: OnceLock<Weak<Session>>,
    said: Mutex<Vec<Clip>>,
    fetched: AtomicU64,
}

impl FakeLine {
    fn other(&self) -> Option<Arc<Session>> {
        self.other.get().and_then(Weak::upgrade)
    }

    fn offers(&self) -> usize {
        self.said
            .lock()
            .iter()
            .filter(|clip| matches!(clip, Clip::Offer(_)))
            .count()
    }

    fn fetched(&self) -> u64 {
        self.fetched.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Line for FakeLine {
    fn say(&self, clip: Clip) -> bool {
        self.said.lock().push(clip.clone());
        let Some(other) = self.other() else {
            return false;
        };
        other.heard(clip);
        true
    }

    async fn fetch(&self, fetch: Fetch) -> Result<Vec<u8>, String> {
        let other = self.other().ok_or("the link is gone")?;
        match other.serve(&fetch) {
            Some(Served::Bytes(bytes)) => {
                self.fetched
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                Ok(bytes.to_vec())
            }
            Some(Served::Path(_)) | None => Err("refused".to_owned()),
        }
    }

    async fn fetch_file(&self, _fetch: Fetch, _into: &Path) -> Result<PathBuf, String> {
        Err("no files in this test".to_owned())
    }
}

/// One Mac: its board, its hub, and its end of the session.
struct End {
    board: Arc<Held>,
    hub: Hub,
    session: Arc<Session>,
    /// What this end says.
    line: Arc<FakeLine>,
}

struct Pair {
    viewer: End,
    host: End,
    clock: Arc<Ticks>,
    _dir: tempfile::TempDir,
}

impl Pair {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(Ticks(AtomicI64::new(1_000)));
        let runtime = tokio::runtime::Handle::current();
        let end = |device: &str, peer: &str| {
            let board = Arc::new(Held::default());
            let hub = Hub::start(
                HubConfig {
                    device: device.to_owned(),
                    board: Arc::clone(&board) as _,
                    clock: Arc::clone(&clock) as _,
                    access: Arc::new(|| PasteAccess::Ungated),
                    cache: dir.path().join(device),
                    settings: ClipboardSettings::default(),
                },
                &runtime,
            );
            let line = Arc::new(FakeLine::default());
            let session = Arc::new(hub.session(peer.to_owned(), Arc::clone(&line) as _));
            End {
                board,
                hub,
                session,
                line,
            }
        };
        let viewer = end("viewer", "host");
        let host = end("host", "viewer");
        viewer
            .line
            .other
            .set(Arc::downgrade(&host.session))
            .unwrap();
        host.line
            .other
            .set(Arc::downgrade(&viewer.session))
            .unwrap();
        Self {
            viewer,
            host,
            clock,
            _dir: dir,
        }
    }
}

/// Another app's copy on `board`: one item of `(type, bytes)`.
fn copy(board: &Held, reps: &[(&str, &[u8])]) {
    let item = Item::data(
        reps.iter()
            .map(|(kind, bytes)| ((*kind).to_owned(), bytes.to_vec()))
            .collect(),
    );
    board.write(&[item]).unwrap();
}

/// Lets the watchers look and the clips land: a few polls of the paused clock, until `done`.
async fn settle(done: impl Fn() -> bool) {
    for _ in 0..40 {
        if done() {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The session's clipboard both ways: text inline, a big picture fetched at once and written
/// whole, a concealed copy never, no echo after a write, and the newest copy on both Macs.
#[tokio::test(start_paused = true)]
async fn the_clipboard_follows_the_focused_window_both_ways() {
    let pair = Pair::new();
    let (viewer, host) = (&pair.viewer, &pair.host);

    // Unfocused, nothing moves.
    copy(&viewer.board, &[(TEXT, b"before focus")]);
    tokio::time::sleep(POLL * 4).await;
    assert_eq!(viewer.line.offers(), 0, "no window has focus");
    assert!(host.board.items().is_empty());

    // Focus: the host follows and says whether it may read unasked; the copy made meanwhile goes.
    assert!(viewer.session.focus(true));
    settle(|| host.board.data(0, TEXT).is_some()).await;
    assert!(
        host.line
            .said
            .lock()
            .contains(&Clip::Access(PasteAccess::Ungated)),
        "{:?}",
        host.line.said.lock()
    );
    assert_eq!(
        host.board.data(0, TEXT).as_deref(),
        Some(&b"before focus"[..])
    );

    // Text rides inline: written at once, no fetch, and never announced back.
    copy(&viewer.board, &[(TEXT, b"hello")]);
    settle(|| host.board.data(0, TEXT).as_deref() == Some(&b"hello"[..])).await;
    assert_eq!(host.board.data(0, TEXT).as_deref(), Some(&b"hello"[..]));
    assert_eq!(
        host.board.data(0, ORIGIN_TYPE).as_deref(),
        Some(&b"viewer"[..])
    );
    assert_eq!(host.line.fetched(), 0, "no fetch");
    tokio::time::sleep(POLL * 6).await;
    assert_eq!(host.line.offers(), 0, "no echo after the write");
    assert_eq!(viewer.line.offers(), 2);

    // A big picture on the host is fetched by the viewer at once, and written whole.
    let picture: Vec<u8> = (0..5_000_000_u32).map(|n| (n % 251) as u8).collect();
    copy(&host.board, &[(PNG, &picture), (TEXT, b"caption")]);
    settle(|| viewer.board.data(0, PNG).is_some()).await;
    assert_eq!(viewer.board.data(0, PNG), Some(picture));
    assert_eq!(viewer.board.data(0, TEXT).as_deref(), Some(&b"caption"[..]));
    assert_eq!(viewer.line.fetched(), 5_000_000, "the caption rode inline");
    tokio::time::sleep(POLL * 6).await;
    assert_eq!(viewer.line.fetched(), 5_000_000, "fetched once");
    assert_eq!(viewer.line.offers(), 2, "the viewer echoed nothing back");

    // A password manager's copy never leaves.
    let before = viewer.line.offers();
    copy(&viewer.board, &[(TEXT, b"hunter2"), (CONCEALED_TYPE, b"")]);
    tokio::time::sleep(POLL * 6).await;
    assert_eq!(viewer.line.offers(), before, "concealed: nothing announced");
    assert_ne!(host.board.data(0, TEXT).as_deref(), Some(&b"hunter2"[..]));

    // Two copies at once: the newest by (at, origin, seq) is on both Macs.
    pair.clock.0.fetch_add(1_000, Ordering::Relaxed);
    copy(&viewer.board, &[(TEXT, b"from the viewer")]);
    copy(&host.board, &[(TEXT, b"from the host")]);
    let agreed = || {
        let (v, h) = (
            viewer.hub.clipboard().newest(),
            host.hub.clipboard().newest(),
        );
        v.is_some() && v == h && viewer.board.data(0, TEXT) == host.board.data(0, TEXT)
    };
    settle(agreed).await;
    assert!(agreed());
    // Whichever look came first, both Macs hold the newest copy either of them announced: a tie
    // at one time goes to the larger origin, and a copy the other's newer offer landed on before
    // it was looked at is never announced.
    let announced: Vec<Key> = [&viewer.line, &host.line]
        .iter()
        .flat_map(|line| {
            line.said
                .lock()
                .iter()
                .filter_map(|clip| match clip {
                    Clip::Offer(offer) => Some(Key::of(offer)),
                    Clip::Focus { .. } | Clip::Access(_) => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let newest = announced.iter().max().cloned();
    assert_eq!(viewer.hub.clipboard().newest(), newest);
    let text = host.board.data(0, TEXT).unwrap();
    assert!(
        text == b"from the viewer" || text == b"from the host",
        "{}",
        String::from_utf8_lossy(&text)
    );
    assert_eq!(
        newest.map(|key| key.origin == "viewer"),
        Some(text == b"from the viewer")
    );

    // Blur: the host lets go, and copies stay where they are made.
    assert!(viewer.session.focus(false));
    tokio::time::sleep(POLL).await;
    let before = (viewer.line.offers(), host.line.offers());
    copy(&viewer.board, &[(TEXT, b"after blur")]);
    copy(&host.board, &[(TEXT, b"host after blur")]);
    tokio::time::sleep(POLL * 6).await;
    assert_eq!((viewer.line.offers(), host.line.offers()), before);
    assert_eq!(host.hub.clipboard().followed(), None);
}
