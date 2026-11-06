//! Signing in to an agent from the ACP registry from another device, end to
//! end: the agent (tests/fixtures/fake-registry-login.py, added on device B
//! as the registry would add it) runs its own sign-in on B, its browser
//! captured; B's login reports the page and publishes the page's loopback
//! callback for device A alone; A forwards the same port over a tunnel on its
//! peer link to B (two preview services on a simulated tailnet); A's "browser"
//! lands the redirect, which reaches the agent on B, and the sign-in finishes.
//! Unix: the recording browser is a shell script.

#![cfg(unix)]

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use onicode_engine::peers::{Peers, StreamRouter};
use onicode_engine::{AgentAccounts, AgentAccountsConfig};
use onicode_net::transport::endpoint_on;
use onicode_net::{Role, StreamKind};
use onicode_net_sim::{Faults, Net, NodeSpec, SimTailnet};
use onicode_peer::{PeerConfig, Sockets};
use onicode_preview::{PreviewService, Scope};
use onicode_proto::{AgentLoginStatus, HarnessId, RegistryAgent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// A preview service as `device`, on a machine of its own on `tailnet`, serving tunnels to
/// its peers.
async fn service(
    tailnet: &SimTailnet,
    dir: &std::path::Path,
    device: &str,
    seed: u64,
) -> (PreviewService, Peers) {
    let service = PreviewService::new(
        dir.join(format!("{device}.json")),
        device.into(),
        device.into(),
    )
    .unwrap();
    service.start(Arc::new(Scope::default)).await;
    let streams = Arc::new(StreamRouter::default());
    streams.serve(StreamKind::Tunnel, service.tunnels());
    let node = tailnet.add(NodeSpec::owned(device, 1));
    let peers = Peers::start_on(
        tailnet.local_api(node),
        &SimSockets(tailnet.net().clone()),
        PeerConfig::new(device, Role::Engine).with_seed(seed),
        None,
        Some(streams),
    )
    .await
    .expect("the device joins the simulated tailnet");
    service.set_links(peers.links.clone());
    (service, peers)
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-registry-login.py")
}

/// Device B's agents: the fake agent added there, as the registry would add
/// it (one folder for both tests: the environment is the process's).
fn added() -> RegistryAgent {
    static AGENTS: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = AGENTS.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("fake-login")).unwrap();
        let record = serde_json::json!({
            "id": "fake-login", "name": "Fake Login", "version": "1.0.0", "kind": "binary",
            "program": fixture(), "args": [], "env": {}, "verified": true
        });
        std::fs::write(dir.path().join("fake-login/agent.json"), record.to_string()).unwrap();
        // SAFETY: set once, before either test reads it.
        unsafe { std::env::set_var("ONICODE_AGENTS_DIR", dir.path()) };
        dir
    });
    let _ = dir;
    let agent = RegistryAgent::new("fake-login").unwrap();
    assert!(onicode_harness::acp_registry::installed(agent));
    agent
}

fn has_python() -> bool {
    std::process::Command::new("python3")
        .args(["-c", "pass"])
        .status()
        .is_ok_and(|s| s.success())
}

/// B runs the sign-in for A: its page reported, its loopback callback
/// published for A alone; then the browser's redirect lands (over the P2P
/// tunnel from A's loopback, or, `tunnel` off, straight at the port B
/// published, as B's end of the tunnel dials it) and the sign-in finishes.
async fn sign_in(tunnel: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let agent = added();

    let tailnet = SimTailnet::new(Faults::CLEAR, 90);
    let (host, host_peers) = service(&tailnet, dir.path(), "device-b", 90).await;
    let (requester, requester_peers) = service(&tailnet, dir.path(), "device-a", 91).await;
    for peers in [&host_peers, &requester_peers] {
        peers
            .directory
            .refresh()
            .await
            .expect("the simulated tailnet answers");
    }
    let accounts = AgentAccounts::with_callback_routes(
        AgentAccountsConfig::isolated(&dir.path().join("accounts")),
        host.callback_routes(),
    );

    // A asks B to sign the agent in: B runs it, the page comes with a poll.
    let start = accounts
        .start_login_for(HarnessId::Registry(agent), Some("device-a"))
        .await
        .expect("the sign-in starts");
    let mut page = None;
    for _ in 0..100 {
        let poll = accounts.poll_login(&start.login_id).await.unwrap();
        assert_eq!(poll.status, AgentLoginStatus::Pending, "{poll:?}");
        if let (Some(url), Some(port)) = (poll.url.clone(), poll.callback_port) {
            page = Some((url, port));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (url, port) = page.expect("the sign-in's page, with its loopback callback");
    assert!(
        url.starts_with("https://auth.example.com/authorize"),
        "{url}"
    );
    // Published for A alone.
    assert!(host.callback_routes().is_registered(&start.login_id));
    assert!(
        host.callback_routes()
            .port_for(&start.login_id, "device-c")
            .is_err()
    );

    // A forwards a port of its own (the same number in production; both
    // run on this machine here) and its browser lands the redirect.
    let published = host
        .callback_routes()
        .port_for(&start.login_id, "device-a")
        .unwrap();
    assert_eq!(published, port);
    let local = if tunnel {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = probe.local_addr().unwrap().port();
        drop(probe);
        assert_ne!(local, port);
        requester
            .open_login_tunnel(&start.login_id, "device-b", local, Duration::from_secs(60))
            .await
            .unwrap();
        local
    } else {
        published
    };
    let mut browser = TcpStream::connect(("127.0.0.1", local)).await.unwrap();
    browser
        .write_all(b"GET /callback?code=abc&state=s HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(40), browser.read_to_string(&mut reply))
        .await
        .expect("the callback answers in time")
        .unwrap();
    assert!(reply.ends_with("signed in"), "{reply:?}");

    // The agent finished its sign-in; the login says so.
    let mut done = false;
    for _ in 0..100 {
        let poll = accounts.poll_login(&start.login_id).await.unwrap();
        match poll.status {
            AgentLoginStatus::Done => {
                done = true;
                break;
            }
            AgentLoginStatus::Error => panic!("sign-in failed: {poll:?}"),
            AgentLoginStatus::Pending => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    assert!(done, "the sign-in finishes");
    requester.close_login_tunnel(&start.login_id);
    host.shutdown().await;
    requester.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registry_agents_sign_in_publishes_its_callback_for_the_requester_and_finishes() {
    if has_python() {
        sign_in(false).await;
    }
}

/// The whole way, over a tunnel on the peer link between two preview services.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registry_agents_sign_in_finishes_through_the_requesters_browser() {
    if has_python() {
        sign_in(true).await;
    }
}
