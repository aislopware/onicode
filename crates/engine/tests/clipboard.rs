//! The clipboard between two engines over a simulated tailnet, on tokio's paused clock: each
//! follows the other's `WatchClipboard`, text lands from the offer alone, a big picture crosses
//! on a `clip/` file stream only when pasted, and a paste whose origin dropped off fails in
//! time. The boards are `Held`: nothing here touches a real pasteboard.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use onicode_clipboard::types::ORIGIN_TYPE;
use onicode_clipboard::{Board as _, FETCH_DEADLINE, Held, Item, PasteAccess};
use onicode_engine::clipboard::ClipboardHost;
use onicode_engine::peers::{Peers, StreamRouter};
use onicode_net::transport::endpoint_on;
use onicode_net::{Role, StreamHead, StreamKind, StreamRx, StreamTx};
use onicode_net_sim::tailnet::{ipv4, ipv6};
use onicode_net_sim::{Faults, Net, NodeId, NodeSpec, SimTailnet};
use onicode_peer::{PeerConfig, Sockets, StreamHandler};
use onicode_rpc::{RpcError, RpcReply, RpcService};

const SEED: u64 = 0xc11b;
const TEXT: &str = "public.utf8-plain-text";
const PNG: &str = "public.png";

struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// The clipboard's part of an engine's RPC surface.
struct ClipRpc(Arc<ClipboardHost>);

#[async_trait]
impl RpcService for ClipRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        self.0.handle(method, params).await
    }
}

/// The clipboard's part of an engine's file streams.
struct ClipStreams(Arc<ClipboardHost>);

impl StreamHandler for ClipStreams {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        self.0.serve_stream(device, &head.name, tx, rx);
    }
}

struct Engine {
    node: NodeId,
    host: Arc<ClipboardHost>,
    board: Arc<Held>,
    peers: Peers,
    _dir: tempfile::TempDir,
}

async fn engine(tailnet: &SimTailnet, device: &str, seed: u64) -> Engine {
    let dir = tempfile::tempdir().unwrap();
    let host = ClipboardHost::new(dir.path(), device);
    let board = Arc::new(Held::default());
    host.start(
        Arc::clone(&board) as _,
        Arc::new(|| PasteAccess::Ungated),
        dir.path().join("cache"),
    );
    let streams = Arc::new(StreamRouter::default());
    streams.serve(StreamKind::File, Arc::new(ClipStreams(Arc::clone(&host))));
    let node = tailnet.add(NodeSpec::owned(device, 1));
    let peers = Peers::start_on(
        tailnet.local_api(node),
        &SimSockets(tailnet.net().clone()),
        PeerConfig::new(device, Role::Engine).with_seed(seed),
        Some(Arc::new(ClipRpc(Arc::clone(&host)))),
        Some(streams),
    )
    .await
    .unwrap();
    host.set_links(Arc::clone(&peers.links));
    Engine {
        node,
        host,
        board,
        peers,
        _dir: dir,
    }
}

/// Packets `from` sent `to` so far, on either address family.
fn packets(net: &Net, from: NodeId, to: NodeId) -> u64 {
    net.delivered(ipv4(from), ipv4(to)) + net.delivered(ipv6(from), ipv6(to))
}

/// Waits up to a minute of the simulated clock for `done`.
async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..600 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("never happened");
}

/// A paste on `board` from a thread of its own, as an app's.
async fn paste(board: &Arc<Held>, kind: &str) -> Option<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let board = Arc::clone(board);
    let kind = kind.to_owned();
    std::thread::spawn(move || {
        let _sent = tx.send(board.data(0, &kind));
    });
    rx.await.unwrap()
}

fn copy(board: &Held, kind: &str, bytes: &[u8]) {
    board
        .write(&[Item::data(vec![(kind.to_owned(), bytes.to_vec())])], None)
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_clipboard_follows_the_owner_across_the_tailnet() {
    let _seed =
        onicode_net_sim::report_seed("the_clipboard_follows_the_owner_across_the_tailnet", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let a = engine(&tailnet, "dev-a", SEED).await;
    let b = engine(&tailnet, "dev-b", SEED + 1).await;
    for side in [&a, &b] {
        side.peers.directory.refresh().await.unwrap();
    }

    // Text: from the offer alone.
    copy(&a.board, TEXT, b"across");
    until(|| b.board.data(0, TEXT).is_some()).await;
    assert_eq!(b.board.data(0, TEXT).as_deref(), Some(&b"across"[..]));
    assert_eq!(b.board.data(0, ORIGIN_TYPE).as_deref(), Some(&b"dev-a"[..]));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        a.board.data(0, TEXT).as_deref(),
        Some(&b"across"[..]),
        "no echo comes back to overwrite it"
    );
    assert_eq!(
        a.board.data(0, ORIGIN_TYPE),
        None,
        "a still holds its own copy"
    );

    // A big picture: promised, and moved only by the paste.
    let picture: Vec<u8> = (0..5_000_000_u32).map(|n| (n % 251) as u8).collect();
    copy(&a.board, PNG, &picture);
    until(|| b.board.promised() == vec![vec![PNG.to_owned()]]).await;
    let before = packets(tailnet.net(), a.node, b.node);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let idle = packets(tailnet.net(), a.node, b.node) - before;
    assert!(idle < 500, "{idle} packets before any paste");
    assert_eq!(paste(&b.board, PNG).await, Some(picture));
    let pasted = packets(tailnet.net(), a.node, b.node) - before;
    assert!(pasted > 3_000, "only {pasted} packets for 5 MB");

    // The origin drops off: the paste fails within its deadline, the offer stays as it was.
    copy(&a.board, PNG, &vec![9; 1_000_000]);
    until(|| b.board.promised() == vec![vec![PNG.to_owned()]]).await;
    for ip in [ipv4(a.node), ipv6(a.node)] {
        tailnet.net().set_down(ip, true);
    }
    let began = tokio::time::Instant::now();
    assert_eq!(paste(&b.board, PNG).await, None);
    let took = began.elapsed();
    assert!(
        took >= FETCH_DEADLINE && took < FETCH_DEADLINE + Duration::from_secs(1),
        "{took:?}"
    );
    assert_eq!(b.board.promised(), vec![vec![PNG.to_owned()]]);
    b.host.shutdown();
    assert!(
        b.board.items().is_empty(),
        "the quit cleared the unkept promise"
    );
    a.host.shutdown();
}
