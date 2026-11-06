//! Two engines on a simulated tailnet (`onicode_net_sim`), in virtual time: the destination
//! pulls a tree (a big file split over several streams, many small files packed, a compressible
//! file, a symlink, an empty folder and a bundle) through 2% loss and a blackout in the middle,
//! and lands the same bytes, modes and mtimes; a stream naming a transfer the source never
//! registered is reset. Every run is seeded and prints its seed when it fails.

mod common;

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use onicode_net::transport::endpoint_on;
use onicode_net::{NetError, Role, StreamHead, StreamKind};
use onicode_net_sim::shape::Link;
use onicode_net_sim::{Faults, Net, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{
    LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerLinks, Sockets, StreamHandler,
};
use onicode_proto::files::{
    ConflictPolicy, StartTransferRequest, TransferFrom, TransferState, TransferTo,
};
use onicode_rpc::{RpcError, RpcReply, RpcService, methods};
use onicode_transfer::store::Store;
use onicode_transfer::{Disk, FileStreams, LinksSlot, PeerOrigins, Served, Transfers, reset, wire};

const USER: i64 = 5;
const SOURCE: &str = "dev-source";
const DEST: &str = "dev-dest";
/// Longer than any copy here takes on the simulated tailnet, in virtual time.
const PATIENCE: Duration = Duration::from_secs(600);

struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// The source engine's `ServeTransfer`, as the engine's RPC dispatch answers it.
struct ServeRpc(Arc<Served>);

#[async_trait]
impl RpcService for ServeRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method != methods::SERVE_TRANSFER {
            return Err(RpcError::UnknownMethod(method.into()));
        }
        let request = onicode_rpc::parse_params(params)?;
        self.0
            .register_request(request)
            .map_err(|e| RpcError::Failed(e.to_string()))?;
        Ok(RpcReply::Value(serde_json::json!({})))
    }
}

/// `device` on a node of its own.
async fn join(
    tailnet: &SimTailnet,
    device: &str,
    seed: u64,
    service: Option<Arc<dyn RpcService>>,
    handler: Option<Arc<dyn StreamHandler>>,
) -> (Arc<PeerLinks>, Arc<PeerDirectory>) {
    let node = tailnet.add(NodeSpec::owned(device, USER));
    let api = tailnet.local_api(node);
    let endpoint = PeerEndpoint::bind(
        Arc::clone(&api),
        &SimSockets(tailnet.net().clone()),
        PeerConfig::new(device, Role::Engine).with_seed(seed),
    )
    .await
    .expect("the device binds its tailnet addresses");
    let directory = PeerDirectory::new(Arc::clone(&endpoint), api);
    let links = Arc::new(PeerLinks::new(
        endpoint,
        Arc::clone(&directory),
        LinksConfig {
            service,
            handler,
            seed: Some(seed),
        },
    ));
    links.start();
    (links, directory)
}

/// The source's registry and links, and the destination's links.
async fn pair(tailnet: &SimTailnet, seed: u64) -> (Arc<Served>, Arc<PeerLinks>, Arc<PeerLinks>) {
    let served = Arc::new(Served::new());
    let (source, source_directory) = join(
        tailnet,
        SOURCE,
        seed,
        Some(Arc::new(ServeRpc(Arc::clone(&served)))),
        Some(Arc::new(FileStreams::new(Arc::clone(&served)))),
    )
    .await;
    let (dest, dest_directory) = join(tailnet, DEST, seed + 1, None, None).await;
    for directory in [source_directory, dest_directory] {
        directory
            .refresh()
            .await
            .expect("the simulated tailnet answers");
    }
    (served, source, dest)
}

/// Waits, in virtual time, until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = tokio::time::Instant::now();
    while !done() {
        assert!(start.elapsed() < PATIENCE, "{what} took too long");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn transfers(dest: &Arc<PeerLinks>) -> Transfers {
    let slot: LinksSlot = Arc::new(RwLock::new(Some(Arc::clone(dest))));
    Transfers::new(
        Arc::new(PeerOrigins::new(DEST.into(), Arc::new(Served::new()), slot)),
        Arc::new(Disk),
        Store::none(),
        common::small_tuning(),
    )
}

fn copy(
    transfers: &Transfers,
    tree: &std::path::Path,
    dest: &std::path::Path,
    conflict: ConflictPolicy,
) -> String {
    transfers
        .start(StartTransferRequest {
            from: TransferFrom {
                device_id: SOURCE.into(),
                paths: vec![tree.to_string_lossy().into_owned()],
            },
            to: TransferTo {
                device_id: DEST.into(),
                dir: dest.to_string_lossy().into_owned(),
            },
            conflict,
        })
        .expect("the copy starts")
}

async fn finished(transfers: &Transfers, id: &str) {
    until("the copy", || {
        let update = transfers.get(id).expect("the job is listed");
        assert_ne!(
            update.state,
            TransferState::Failed,
            "the copy failed: {:?}",
            update.error
        );
        update.state == TransferState::Done
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn two_engines_copy_a_tree_through_loss_and_a_blackout() {
    for seed in [81_u64, 82] {
        let _seed = report_seed("two_engines_copy_a_tree_through_loss_and_a_blackout", seed);
        let lossy = Link {
            delay: Duration::from_millis(3),
            loss: 0.02,
            rate: 4_000_000,
            queue: 256 << 10,
            ..Link::CLEAR
        };
        let tailnet = SimTailnet::new(Faults::over(lossy), seed);
        let (_served, _source, dest_links) = pair(&tailnet, seed).await;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        let tree = common::build_tree(&src, seed, 120, 1 << 20);
        let transfers = transfers(&dest_links);

        let id = copy(&transfers, &tree, &dest, ConflictPolicy::Ask);
        until("the first bytes", || {
            transfers.get(&id).is_some_and(|u| u.bytes_done > 64 << 10)
        })
        .await;
        let at_blackout = transfers.get(&id).unwrap();
        assert!(
            at_blackout.bytes_done < at_blackout.bytes,
            "the blackout falls mid-copy"
        );
        tailnet.net().blackout(Duration::from_secs(4));
        finished(&transfers, &id).await;
        let done = transfers.get(&id).unwrap();
        assert_eq!(done.bytes_done, done.bytes);
        assert_eq!(done.items_done, done.items);
        common::assert_same_tree(&tree, &dest.join("tree"));

        // Again into the same folder: the name is taken, and "keep both" lands it beside.
        let again = copy(&transfers, &tree, &dest, ConflictPolicy::KeepBoth);
        finished(&transfers, &again).await;
        common::assert_same_tree(&tree, &dest.join("tree 2"));
        let mut names: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["tree", "tree 2"], "nothing partial is left");
    }
}

/// Reads `head` from the source and returns how the stream ended.
async fn read_all(links: &PeerLinks, head: &str) -> Result<usize, NetError> {
    let (_tx, mut rx) = links
        .open(SOURCE, &StreamHead::new(StreamKind::File, head))
        .await
        .expect("a stream opens");
    let mut read = 0;
    loop {
        match tokio::time::timeout(PATIENCE, rx.chunk()).await {
            Ok(Ok(Some(chunk))) => read += chunk.len(),
            Ok(Ok(None)) => return Ok(read),
            Ok(Err(error)) => return Err(error),
            Err(_) => panic!("{head}: no answer"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_stream_naming_an_unregistered_transfer_is_reset() {
    const SEED: u64 = 91;
    let _seed = report_seed("a_stream_naming_an_unregistered_transfer_is_reset", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let (served, _source, dest) = pair(&tailnet, SEED).await;
    let dir = tempfile::tempdir().unwrap();
    let tree = common::build_tree(dir.path(), SEED, 3, 1000);

    let unknown = u64::from(reset::UNKNOWN);
    for head in [
        "xfer/never-registered/manifest",
        "xfer/never-registered/0/0/10",
        "xfer/never-registered/pack/0/2",
        // No clipboard is set on the source yet.
        "clip/1/public.utf8-plain-text",
    ] {
        match read_all(&dest, head).await {
            Err(NetError::Reset(code)) => assert_eq!(code, unknown, "{head}"),
            other => panic!("{head}: {other:?}"),
        }
    }
    match read_all(&dest, "nonsense").await {
        Err(NetError::Reset(code)) => assert_eq!(code, u64::from(reset::BAD)),
        other => panic!("a bad head: {other:?}"),
    }

    // Registered, the same transfer is served; its ranges wait for the manifest.
    served
        .register("t-1", vec![tree], onicode_transfer::BLOCK)
        .unwrap();
    match read_all(&dest, "xfer/t-1/0/0/10").await {
        Err(NetError::Reset(code)) => assert_eq!(code, unknown, "a range before the manifest"),
        other => panic!("a range before the manifest: {other:?}"),
    }
    let manifest = read_all(&dest, &wire::head("t-1", &wire::Request::Manifest))
        .await
        .expect("a registered transfer's manifest");
    assert!(manifest > 4);
    served.forget("t-1");
    match read_all(&dest, "xfer/t-1/manifest").await {
        Err(NetError::Reset(code)) => assert_eq!(code, unknown, "a forgotten transfer"),
        other => panic!("a forgotten transfer: {other:?}"),
    }
}
