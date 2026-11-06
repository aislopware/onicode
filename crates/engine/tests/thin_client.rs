//! The thin client (a phone) joining an engine by its listed address, on a simulated tailnet:
//! `onicode_peer::EngineList` stands in for the Tailscale app's tailnet, and everything the
//! client does with the engine rides the link it dials. Seeded, with a clear network
//! (`support/tailnet.rs`).
//!
//! The test lends the client its runtime (`onicode_client::runtime::lend`), so the engine, the
//! client and the network all run on one current-thread runtime on a paused clock: finding the
//! engine, the RPC and the registry's mirror happen in virtual time and repeat with the seed.
//! The terminal is a real shell, whose waiter holds tokio's blocking pool for as long as it
//! runs and so stops the paused clock from moving on its own; that phase moves the clock by
//! hand with real time ([`on_real_time`]), bounded by [`REAL_PATIENCE`].

#[path = "support/tailnet.rs"]
mod tailnet;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_client::events::NullListener;
use onicode_client::{Client, ClientConfig, Mode, TerminalSink, TerminalUpdate};
use onicode_engine::{EngineCore, HarnessRegistry};
use onicode_harness::{Harness, HarnessError, RunControls};
use onicode_net::Role;
use onicode_net::transport::endpoint_on;
use onicode_net_sim::Net;
use onicode_net_sim::tailnet::ipv4;
use onicode_peer::{EngineList, PeerConfig, Sockets};
use onicode_proto::{AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};
use tokio::sync::mpsc;

const SEED: u64 = 0x7417;
/// How long the client may take to find the engine, mirror its registry or hear its shell, in
/// virtual time.
const PATIENCE: Duration = Duration::from_secs(20);
/// How long the real shell may take to start, echo and stop, in real time.
const REAL_PATIENCE: Duration = Duration::from_secs(20);
/// How far the clock moves for each such stretch of real time while the shell runs.
const STEP: Duration = Duration::from_millis(1);
const CHAT: &str = "chat-thin";

/// A harness the engine lists under a name only it knows, so a listing the client got from
/// the engine tells itself apart from the client's cached or built-in one.
struct Listed;

#[async_trait]
impl Harness for Listed {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }

    fn display_name(&self) -> &str {
        "Listed by the engine"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }

    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::empty().boxed())
    }
}

/// The phone's sockets on the simulated network.
struct PhoneSockets(Net);

impl Sockets for PhoneSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// What the client's terminal link reports, as a channel.
struct Updates(mpsc::UnboundedSender<TerminalUpdate>);

impl TerminalSink for Updates {
    fn update(&self, update: TerminalUpdate) {
        let _ = self.0.send(update);
    }
}

/// Polls `ready` until it holds, failing with `what` after [`PATIENCE`].
async fn eventually(what: &str, mut ready: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while !ready().await {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Runs `fut` while a real process holds the paused clock (a blocking task stops it from
/// advancing on its own): the clock moves [`STEP`] for each [`STEP`] of real time, failing with
/// `what` after [`REAL_PATIENCE`].
async fn on_real_time<F: Future>(what: &str, fut: F) -> F::Output {
    let (beat, mut beats) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while beat.send(()).is_ok() {
            std::thread::sleep(STEP);
        }
    });
    let started = std::time::Instant::now();
    let mut fut = std::pin::pin!(fut);
    loop {
        tokio::select! {
            biased;
            out = &mut fut => return out,
            Some(()) = beats.recv() => {
                assert!(started.elapsed() < REAL_PATIENCE, "{what}");
                tokio::time::advance(STEP).await;
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_thin_client_reaches_a_listed_engine_over_its_link() {
    let _seed =
        onicode_net_sim::report_seed("a_thin_client_reaches_a_listed_engine_over_its_link", SEED);
    // Every task the client spawns runs here, on the paused clock.
    let _lent = onicode_client::runtime::lend(tokio::runtime::Handle::current());
    let dirs = tempfile::tempdir().expect("tempdir");
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(Listed));
    let core = EngineCore::assemble(
        &dirs.path().join("engine"),
        Arc::new(registry),
        HarnessId::Codex,
    )
    .expect("engine assembles");
    core.workspace
        .create_chat(CHAT, None, Some(&core.device_id), None, None)
        .expect("the engine hosts a chat");
    let (mesh, joined) = tailnet::mesh(&[&core], SEED).await;
    let engine = joined[0].0;

    // The phone, on a machine of its own, lists the engine by its tailnet address.
    let phone = mesh.node();
    let client = Client::new(
        ClientConfig::new(dirs.path().join("phone")),
        Mode::Live,
        Arc::new(NullListener),
    )
    .expect("the client opens");
    client
        .join_on(
            EngineList::at(vec![ipv4(engine).to_string()], vec![ipv4(phone)]),
            Arc::new(PhoneSockets(mesh.sim.net().clone())),
            PeerConfig::new(client.device_id(), Role::Viewer).with_seed(SEED + 1),
        )
        .expect("the client joins");
    eventually("the client never bound on the tailnet", async || {
        client.on_tailnet()
    })
    .await;

    // An RPC: the engine's own harness listing, not a cached or built-in one.
    eventually("ListHarnesses never reached the engine", async || {
        client
            .list_harnesses(&core.device_id)
            .await
            .iter()
            .any(|h| h.label == "Listed by the engine")
    })
    .await;

    // A subscription: the client mirrors the engine's registry over a stream of the link.
    eventually("the engine's chat never reached the client", async || {
        client
            .workspace()
            .session(CHAT)
            .is_some_and(|row| row.device_id == core.device_id)
    })
    .await;

    // A terminal: opened on the engine, typed into and heard back over the link. From here a
    // real shell runs, so the clock moves with real time.
    let info = on_real_time(
        "the engine never opened a terminal",
        client.open_terminal(CHAT, 80, 24),
    )
    .await
    .expect("the engine opens a terminal");
    assert_eq!(info.device_id, core.device_id);
    assert!(
        core.terminals.status(&info.id).is_ok(),
        "the shell runs on the engine"
    );
    let (tx, mut updates) = mpsc::unbounded_channel();
    let link = client.attach_terminal(info, 0, Arc::new(Updates(tx)));
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut heard = Vec::new();
    let mut typed = false;
    while !String::from_utf8_lossy(&heard).contains("thin-42") {
        let update = on_real_time(
            "the shell's output never came",
            tokio::time::timeout_at(deadline, updates.recv()),
        )
        .await
        .expect("the shell's output before the deadline")
        .expect("the link stays attached");
        match update {
            TerminalUpdate::Connected if !typed => {
                link.write(b"echo thin-$((40+2))\n".to_vec());
                typed = true;
            }
            TerminalUpdate::Output(bytes) => heard.extend(bytes),
            TerminalUpdate::Exited { .. } | TerminalUpdate::Gone => {
                panic!("the shell ended early: {}", String::from_utf8_lossy(&heard))
            }
            TerminalUpdate::Connected | TerminalUpdate::Reconnecting { .. } => {}
        }
    }
    link.close();

    client.shutdown();
    on_real_time("the engine never shut down", core.shutdown()).await;
}
