//! The clipboard between Macs, on `Held` boards and an in-memory network of peers, on tokio's
//! paused clock: what moves, when, and what never does. Each pair follows the other, as a focused
//! desktop window and its host do. Nothing here touches a real pasteboard.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use onicode_clipboard::types::{
    AUTO_GENERATED_TYPE, CONCEALED_TYPE, FILE_URL_TYPE, ORIGIN_TYPE, REMOTE_CLIPBOARD_TYPE,
    TRANSIENT_TYPE,
};
use onicode_clipboard::{
    Board as _, Clipboard, ClipboardOffer, ClipboardSettings, Clock, Config, FETCH_DEADLINE, Held,
    Inline, Item, PasteAccess, Peers, Received,
};
use parking_lot::Mutex;
use proptest::prelude::*;

const TEXT: &str = "public.utf8-plain-text";
const PNG: &str = "public.png";

/// Epoch milliseconds the test sets.
#[derive(Default)]
struct Ticks(AtomicI64);

impl Clock for Ticks {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// The peers of every node: each device's clipboard by id, and what moved between them.
#[derive(Default)]
struct Net {
    clips: Mutex<HashMap<String, Clipboard>>,
    /// Bytes fetched for pastes.
    moved: AtomicU64,
    /// Files copied by the stand-in file streams.
    files: AtomicUsize,
}

impl Net {
    fn moved(&self) -> u64 {
        self.moved.load(Ordering::Relaxed)
    }
}

struct NetPeers(Arc<Net>);

#[async_trait]
impl Peers for NetPeers {
    async fn fetch(
        &self,
        origin: &str,
        seq: u64,
        item: usize,
        uti: &str,
    ) -> Result<Vec<u8>, String> {
        let clip = self.0.clips.lock().get(origin).cloned();
        let Some(clip) = clip else {
            // A device that left the tailnet: the dial never answers.
            return std::future::pending().await;
        };
        let uti = uti.to_owned();
        let bytes = tokio::task::spawn_blocking(move || clip.serve(seq, item, &uti))
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "the origin holds it no more".to_owned())?;
        self.0
            .moved
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    async fn fetch_file(
        &self,
        origin: &str,
        seq: u64,
        item: usize,
        into: &Path,
    ) -> Result<PathBuf, String> {
        let clip = self.0.clips.lock().get(origin).cloned();
        let Some(clip) = clip else {
            return std::future::pending().await;
        };
        let from = tokio::task::spawn_blocking(move || clip.serve_file(seq, item))
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "the origin holds it no more".to_owned())?;
        let name = from.file_name().ok_or("no name")?;
        let landed = into.join(name);
        let copied = std::fs::copy(&from, &landed).map_err(|e| e.to_string())?;
        self.0.moved.fetch_add(copied, Ordering::Relaxed);
        self.0.files.fetch_add(1, Ordering::Relaxed);
        Ok(landed)
    }
}

struct Node {
    clip: Clipboard,
    board: Arc<Held>,
    cache: PathBuf,
}

struct Mesh {
    nodes: Vec<Node>,
    net: Arc<Net>,
    clock: Arc<Ticks>,
    access: Arc<Mutex<PasteAccess>>,
    _dir: tempfile::TempDir,
}

impl Mesh {
    fn new(devices: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let net = Arc::new(Net::default());
        let clock = Arc::new(Ticks(AtomicI64::new(1_000)));
        let access = Arc::new(Mutex::new(PasteAccess::Ungated));
        let nodes = devices
            .iter()
            .map(|device| {
                let board = Arc::new(Held::default());
                let cache = dir.path().join(device);
                let reads = Arc::clone(&access);
                let clip = Clipboard::new(
                    Config {
                        device: (*device).to_owned(),
                        board: Arc::clone(&board) as _,
                        peers: Arc::new(NetPeers(Arc::clone(&net))),
                        clock: Arc::clone(&clock) as _,
                        access: Arc::new(move || *reads.lock()),
                        cache: cache.clone(),
                        settings: ClipboardSettings::default(),
                    },
                    tokio::runtime::Handle::current(),
                );
                net.clips.lock().insert((*device).to_owned(), clip.clone());
                Node { clip, board, cache }
            })
            .collect();
        Self {
            nodes,
            net,
            clock,
            access,
            _dir: dir,
        }
    }

    fn node(&self, n: usize) -> &Node {
        &self.nodes[n]
    }

    /// Nodes `n` and `m` follow each other, as a focused window and its host do.
    fn pair(&self, n: usize, m: usize) {
        let device = |k: usize| self.node(k).clip.device().to_owned();
        self.node(n).clip.follow(Some(device(m)));
        self.node(m).clip.follow(Some(device(n)));
    }
}

/// Another app's copy on `board`: one item per entry, each `(type, bytes)`.
fn copy(board: &Held, items: &[&[(&str, &[u8])]]) {
    let items: Vec<Item> = items
        .iter()
        .map(|reps| {
            Item::data(
                reps.iter()
                    .map(|(kind, bytes)| ((*kind).to_owned(), bytes.to_vec()))
                    .collect(),
            )
        })
        .collect();
    board.write(&items, None).unwrap();
}

/// A paste on `board`, as an app reads it: from a thread of its own, which waits on a promise
/// while the runtime fetches.
async fn paste(board: &Arc<Held>, item: usize, kind: &str) -> Option<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let board = Arc::clone(board);
    let kind = kind.to_owned();
    std::thread::spawn(move || {
        let _sent = tx.send(board.data(item, &kind));
    });
    rx.await.unwrap()
}

/// The next offer `sub` sees within a second, if any.
async fn next(
    sub: &mut futures::stream::BoxStream<'static, ClipboardOffer>,
) -> Option<ClipboardOffer> {
    tokio::time::timeout(Duration::from_secs(1), sub.next())
        .await
        .ok()
        .flatten()
}

/// Text copied on one Mac is pasted on the other from the offer alone: no fetch, and nothing
/// is announced back.
#[tokio::test(start_paused = true)]
async fn text_copied_on_one_pastes_on_another_from_the_offer() {
    let mesh = Mesh::new(&["a", "b"]);
    let (a, b) = (mesh.node(0), mesh.node(1));
    mesh.pair(0, 1);
    let mut to_b = a.clip.subscribe(Some("b".into()));
    let mut to_a = b.clip.subscribe(Some("a".into()));

    copy(
        &a.board,
        &[&[(TEXT, b"hello"), ("dyn.ah62d4rv4gu8y", b"x")]],
    );
    let offer = a.clip.look().expect("the copy is announced");
    assert_eq!(next(&mut to_b).await.as_ref(), Some(&*offer));
    let types = &offer.items[0].types;
    assert_eq!(types.len(), 1, "a dynamic type stays home: {types:?}");
    assert_eq!(types[0].size, 5);
    assert!(types[0].inline.is_some(), "small enough to ride inline");

    assert_eq!(b.clip.receive((*offer).clone()), Received::Applied);
    assert_eq!(
        b.board.promised(),
        vec![Vec::<String>::new()],
        "nothing promised"
    );
    assert_eq!(
        paste(&b.board, 0, TEXT).await.as_deref(),
        Some(&b"hello"[..])
    );
    assert_eq!(b.board.data(0, ORIGIN_TYPE).as_deref(), Some(&b"a"[..]));
    assert_eq!(mesh.net.moved(), 0, "the paste took no fetch");

    assert_eq!(b.clip.look(), None, "never announced back");
    assert_eq!(next(&mut to_a).await, None);
    assert_eq!(b.clip.receive((*offer).clone()), Received::Stale);
    assert_eq!(
        a.clip.receive((*offer).clone()),
        Received::Ignored,
        "its own"
    );
}

/// A big picture is promised, not sent: no byte moves until a paste asks, and then all of it.
#[tokio::test(start_paused = true)]
async fn a_big_picture_moves_only_when_pasted() {
    let mesh = Mesh::new(&["a", "b"]);
    let (a, b) = (mesh.node(0), mesh.node(1));
    mesh.pair(0, 1);
    let picture: Vec<u8> = (0..5_000_000_u32).map(|n| (n % 251) as u8).collect();
    copy(&a.board, &[&[(PNG, &picture), (TEXT, b"caption")]]);
    let offer = a.clip.look().unwrap();
    let png = &offer.items[0].types[0];
    assert_eq!((png.size, png.inline.is_none()), (5_000_000, true));

    assert_eq!(b.clip.receive((*offer).clone()), Received::Applied);
    assert_eq!(b.board.promised(), vec![vec![PNG.to_owned()]]);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(mesh.net.moved(), 0, "no byte before the paste");

    assert_eq!(
        paste(&b.board, 0, TEXT).await.as_deref(),
        Some(&b"caption"[..])
    );
    assert_eq!(mesh.net.moved(), 0, "the caption rode inline");
    assert_eq!(paste(&b.board, 0, PNG).await, Some(picture.clone()));
    assert_eq!(mesh.net.moved(), 5_000_000);
    assert_eq!(paste(&b.board, 0, PNG).await, Some(picture), "kept");
    assert_eq!(mesh.net.moved(), 5_000_000, "fetched once");
    assert!(!b.clip.shutdown(), "every promise kept: nothing to clear");
}

/// A password manager's copy, a transient one and Universal Clipboard's never leave; an
/// auto-generated one leaves only when the settings opt in; the other Mac cannot plant a marker.
#[tokio::test(start_paused = true)]
async fn private_copies_never_leave() {
    let mesh = Mesh::new(&["a", "b"]);
    let (a, b) = (mesh.node(0), mesh.node(1));
    mesh.pair(0, 1);
    let mut to_b = a.clip.subscribe(Some("b".into()));
    for marked in [
        vec![vec![(TEXT, &b"hunter2"[..]), (CONCEALED_TYPE, &b""[..])]],
        vec![
            vec![(TEXT, &b"one-time"[..])],
            vec![(TRANSIENT_TYPE, &b""[..])],
        ],
        vec![vec![
            (TEXT, &b"from a phone"[..]),
            (REMOTE_CLIPBOARD_TYPE, &b""[..]),
        ]],
        vec![vec![(TEXT, &b"auto"[..]), (AUTO_GENERATED_TYPE, &b""[..])]],
    ] {
        let items: Vec<&[(&str, &[u8])]> = marked.iter().map(Vec::as_slice).collect();
        copy(&a.board, &items);
        assert_eq!(a.clip.look(), None, "{marked:?}");
    }
    assert_eq!(next(&mut to_b).await, None, "nothing announced");
    assert_eq!(a.clip.serve(0, 0, TEXT), None, "nothing served");

    a.clip.set_settings(ClipboardSettings {
        enabled: true,
        auto_generated: true,
    });
    copy(&a.board, &[&[(TEXT, b"auto"), (AUTO_GENERATED_TYPE, b"")]]);
    let offer = a.clip.look().expect("opted in");
    assert!(offer.items[0].types.iter().all(|t| t.uti == TEXT));

    let mut planted = (*offer).clone();
    planted.items[0]
        .types
        .push(onicode_clipboard::ClipboardType {
            uti: CONCEALED_TYPE.into(),
            size: 0,
            inline: Some(Inline(Vec::new())),
            file_size: None,
        });
    assert_eq!(b.clip.receive(planted), Received::Applied);
    assert_eq!(
        b.board.items(),
        vec![vec![ORIGIN_TYPE.to_owned(), TEXT.to_owned()]]
    );
}

/// Without "Paste from Other Apps" an offer carries its types alone, and the origin reads the
/// bytes only when the other Mac pastes; denied, the clipboard stays home.
#[tokio::test(start_paused = true)]
async fn an_origin_that_may_not_read_unasked_sends_types_alone() {
    let mesh = Mesh::new(&["a", "b"]);
    let (a, b) = (mesh.node(0), mesh.node(1));
    mesh.pair(0, 1);
    *mesh.access.lock() = PasteAccess::Ask;
    copy(&a.board, &[&[(TEXT, b"asked")]]);
    let offer = a.clip.look().unwrap();
    let text = &offer.items[0].types[0];
    assert_eq!((text.size, text.inline.as_ref()), (0, None));
    assert_eq!(b.clip.receive((*offer).clone()), Received::Applied);
    assert_eq!(
        paste(&b.board, 0, TEXT).await.as_deref(),
        Some(&b"asked"[..])
    );
    assert_eq!(mesh.net.moved(), 5);

    *mesh.access.lock() = PasteAccess::Denied;
    copy(&a.board, &[&[(TEXT, b"kept home")]]);
    assert_eq!(a.clip.look(), None);
}

/// While a window follows a session, only that device's copies apply and this Mac's go only to
/// it and to readers here; with nobody followed, nothing applies or goes, and the next session
/// followed gets the newest copy.
#[tokio::test(start_paused = true)]
async fn a_followed_session_has_the_clipboard_both_ways() {
    let mesh = Mesh::new(&["a", "b", "c"]);
    let (a, b, c) = (mesh.node(0), mesh.node(1), mesh.node(2));
    let mut to_b = a.clip.subscribe(Some("b".into()));
    let mut to_c = a.clip.subscribe(Some("c".into()));
    let mut to_app = a.clip.subscribe(None);
    mesh.pair(0, 1);
    c.clip.follow(Some("a".into()));

    copy(&c.board, &[&[(TEXT, b"from c")]]);
    let from_c = c.clip.look().unwrap();
    assert_eq!(a.clip.receive((*from_c).clone()), Received::Ignored);
    mesh.clock.0.fetch_add(10, Ordering::Relaxed);
    copy(&b.board, &[&[(TEXT, b"from b")]]);
    let from_b = b.clip.look().unwrap();
    assert_eq!(a.clip.receive((*from_b).clone()), Received::Applied);
    assert_eq!(a.board.data(0, TEXT).as_deref(), Some(&b"from b"[..]));

    mesh.clock.0.fetch_add(10, Ordering::Relaxed);
    copy(&a.board, &[&[(TEXT, b"from a")]]);
    let from_a = a.clip.look().unwrap();
    assert_eq!(next(&mut to_b).await.as_ref(), Some(&*from_a));
    assert_eq!(
        next(&mut to_app).await.as_ref(),
        Some(&*from_a),
        "a reader here sees it"
    );
    assert_eq!(next(&mut to_c).await, None, "c is not followed");

    a.clip.unfollow("c");
    assert_eq!(
        a.clip.followed().as_deref(),
        Some("b"),
        "another window's blur"
    );
    a.clip.unfollow("b");
    assert_eq!(a.clip.followed(), None);
    assert_eq!(next(&mut to_c).await, None, "nobody followed: nothing goes");
    assert_eq!(
        a.clip.receive((*from_b).clone()),
        Received::Ignored,
        "nothing applies"
    );
    a.clip.follow(Some("c".into()));
    assert_eq!(
        next(&mut to_c).await.as_ref(),
        Some(&*from_a),
        "the newest copy, once c is followed"
    );
    assert_eq!(c.clip.receive((*from_a).clone()), Received::Applied);
}

/// A paste whose origin left fails within the deadline, and the board keeps the offer as it
/// was: another paste fails the same way, a newer copy still lands, and a quit clears an
/// unkept promise.
#[tokio::test(start_paused = true)]
async fn a_paste_from_a_vanished_origin_fails_in_time() {
    let mesh = Mesh::new(&["a", "b", "c"]);
    let (a, b, c) = (mesh.node(0), mesh.node(1), mesh.node(2));
    mesh.pair(0, 1);
    copy(&a.board, &[&[(PNG, &vec![1; 100_000])]]);
    let offer = a.clip.look().unwrap();
    assert_eq!(b.clip.receive((*offer).clone()), Received::Applied);
    mesh.net.clips.lock().remove("a");

    let began = tokio::time::Instant::now();
    assert_eq!(paste(&b.board, 0, PNG).await, None);
    let took = began.elapsed();
    assert!(
        took >= FETCH_DEADLINE && took < FETCH_DEADLINE + Duration::from_secs(1),
        "{took:?}"
    );
    assert_eq!(
        b.board.items(),
        vec![vec![ORIGIN_TYPE.to_owned(), PNG.to_owned()]]
    );
    assert_eq!(
        b.board.promised(),
        vec![vec![PNG.to_owned()]],
        "still promised"
    );
    assert_eq!(paste(&b.board, 0, PNG).await, None);
    assert_eq!(b.clip.look(), None, "the failed paste is no copy");
    assert_eq!(b.clip.newest(), Some(onicode_clipboard::Key::of(&offer)));

    assert!(b.clip.shutdown(), "a quit clears the unkept promise");
    assert!(b.board.items().is_empty());

    mesh.clock.0.fetch_add(10, Ordering::Relaxed);
    mesh.pair(1, 2);
    copy(&c.board, &[&[(TEXT, b"later")]]);
    let later = c.clip.look().unwrap();
    assert_eq!(b.clip.receive((*later).clone()), Received::Applied);
}

/// A small copied file lands in the cache before any paste, and the paste names the local copy;
/// the cache keeps only the newest offer's files.
#[tokio::test(start_paused = true)]
async fn a_small_file_is_fetched_ahead_and_pasted_from_the_cache() {
    let mesh = Mesh::new(&["a", "b"]);
    let (a, b) = (mesh.node(0), mesh.node(1));
    mesh.pair(0, 1);
    let source = tempfile::tempdir().unwrap();
    let file = source.path().join("notes 1.txt");
    std::fs::write(&file, b"remember").unwrap();
    let url = onicode_clipboard::files::url_of_path(&file);
    copy(&a.board, &[&[(FILE_URL_TYPE, url.as_bytes())]]);
    let offer = a.clip.look().unwrap();
    assert_eq!(offer.items[0].types[0].file_size, Some(8));

    assert_eq!(b.clip.receive((*offer).clone()), Received::Applied);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(mesh.net.files.load(Ordering::Relaxed), 1, "fetched ahead");
    let pasted = paste(&b.board, 0, FILE_URL_TYPE).await.unwrap();
    let local = onicode_clipboard::files::path_of_url(&pasted).unwrap();
    assert!(local.starts_with(&b.cache), "{}", local.display());
    assert_eq!(std::fs::read(&local).unwrap(), b"remember");
    assert_eq!(mesh.net.files.load(Ordering::Relaxed), 1, "fetched once");

    mesh.clock.0.fetch_add(10, Ordering::Relaxed);
    copy(&a.board, &[&[(TEXT, b"next")]]);
    let next_offer = a.clip.look().unwrap();
    assert_eq!(b.clip.receive((*next_offer).clone()), Received::Applied);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!local.exists(), "no history is kept");
}

/// The board is looked at only while someone listens.
#[tokio::test(start_paused = true)]
async fn the_board_is_watched_only_while_someone_listens() {
    let mesh = Mesh::new(&["a"]);
    let a = mesh.node(0);
    a.clip.follow(Some("b".into()));
    let watcher = tokio::spawn(a.clip.watcher());
    copy(&a.board, &[&[(TEXT, b"unheard")]]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(a.clip.announced(), None, "nobody listens");

    let mut sub = a.clip.subscribe(Some("b".into()));
    let offer = next(&mut sub).await.expect("watched once listened to");
    assert_eq!(offer.origin, "a");
    drop(sub);
    tokio::time::sleep(Duration::from_secs(1)).await;
    copy(&a.board, &[&[(TEXT, b"unheard again")]]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(a.clip.announced().map(|o| o.seq), Some(offer.seq));
    watcher.abort();
}

/// One step of a window and its host copying and hearing each other in any order.
#[derive(Clone, Debug)]
enum Step {
    /// The person copies on a node.
    Copy(usize),
    /// A node's watcher looks, its clock this far ahead of the shared one.
    Look { node: usize, skew: i64 },
    /// A node hears the other's newest offer.
    Hear { to: usize },
    /// Time passes.
    Wait(i64),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..2_usize).prop_map(Step::Copy),
        (0..2_usize, -5..5_i64).prop_map(|(node, skew)| Step::Look { node, skew }),
        (0..2_usize).prop_map(|to| Step::Hear { to }),
        (0..4_i64).prop_map(Step::Wait),
    ]
}

/// Runs `steps`, then lets both Macs look and hear each other until nothing changes: every offer
/// is a copy made on its origin (no echo), and both boards end on the newest offer by
/// `(at, origin, seq)`.
fn converge(steps: &[Step]) -> Result<(), TestCaseError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let mesh = Mesh::new(&["n0", "n1"]);
    mesh.pair(0, 1);
    let mut copies = 0_u32;
    let mut outbox: Vec<Option<ClipboardOffer>> = vec![None; 2];
    let mut announced: Vec<ClipboardOffer> = Vec::new();
    let look =
        |n: usize, outbox: &mut Vec<Option<ClipboardOffer>>, all: &mut Vec<ClipboardOffer>| {
            if let Some(offer) = mesh.node(n).clip.look() {
                outbox[n] = Some((*offer).clone());
                all.push((*offer).clone());
            }
        };
    for step in steps {
        match *step {
            Step::Copy(node) => {
                copies += 1;
                let text = format!("n{node}-{copies}");
                copy(&mesh.node(node).board, &[&[(TEXT, text.as_bytes())]]);
            }
            Step::Look { node, skew } => {
                mesh.clock.0.fetch_add(skew, Ordering::Relaxed);
                look(node, &mut outbox, &mut announced);
                mesh.clock.0.fetch_sub(skew, Ordering::Relaxed);
            }
            Step::Hear { to } => {
                if let Some(offer) = outbox[1 - to].clone() {
                    let _ = mesh.node(to).clip.receive(offer);
                }
            }
            Step::Wait(ms) => {
                mesh.clock.0.fetch_add(ms, Ordering::Relaxed);
            }
        }
    }
    for n in 0..2 {
        look(n, &mut outbox, &mut announced);
    }
    for _ in 0..4 {
        for offer in outbox.clone().into_iter().flatten() {
            for to in 0..2 {
                let _ = mesh.node(to).clip.receive(offer.clone());
            }
        }
        for n in 0..2 {
            look(n, &mut outbox, &mut announced);
        }
    }
    for offer in &announced {
        let text = offer.items[0].types[0]
            .inline
            .clone()
            .map(|inline| inline.0)
            .unwrap_or_default();
        let text = String::from_utf8(text).unwrap();
        prop_assert!(
            text.starts_with(&format!("{}-", offer.origin)),
            "{} announced {text}: an echo",
            offer.origin
        );
    }
    let Some(newest) = announced
        .iter()
        .max_by_key(|o| onicode_clipboard::Key::of(o))
    else {
        return Ok(());
    };
    let want = newest.items[0].types[0]
        .inline
        .clone()
        .map(|inline| inline.0);
    for n in 0..2 {
        let node = mesh.node(n);
        prop_assert_eq!(node.clip.newest(), Some(onicode_clipboard::Key::of(newest)));
        prop_assert_eq!(node.board.data(0, TEXT), want.clone(), "node {}", n);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, rng_seed: proptest::test_runner::RngSeed::Fixed(0x0c11_b0a2), ..ProptestConfig::default() })]

    #[test]
    fn a_window_and_its_host_copying_at_once_converge_without_echo(steps in proptest::collection::vec(step(), 0..40)) {
        converge(&steps)?;
    }
}
