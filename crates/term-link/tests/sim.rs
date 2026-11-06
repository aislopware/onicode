//! A viewer and a terminal's host on a simulated tailnet (`onicode_net_sim`), in virtual time:
//! keys reach the shell once and their echoes come back in order; a link lost mid-stream is
//! redialled and the stream resumes with no gap and no repeat; a lossy path costs time, never a
//! key or an event. Every run is seeded and prints its seed when it fails.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_net::Role;
use onicode_net::transport::endpoint_on;
use onicode_net_sim::shape::Link;
use onicode_net_sim::{Faults, Net, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{
    LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerLinks, Sockets, StreamHandler,
};
use onicode_proto::TerminalEvent;
use onicode_rpc::RpcError;
use onicode_term_link::link::{HostTerminals, TerminalService};
use onicode_term_link::remote::{RemoteTerminals, Write};
use tokio::sync::mpsc;

const USER: i64 = 3;
const HOST: &str = "dev-host";
const VIEWER: &str = "dev-viewer";
const TERMINAL: &str = "term-1";

struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

#[derive(Default)]
struct Shell {
    seq: u64,
    log: Vec<TerminalEvent>,
    subscribers: Vec<mpsc::UnboundedSender<TerminalEvent>>,
    applied: HashMap<String, u64>,
    typed: Vec<u8>,
}

impl Shell {
    fn emit(&mut self, bytes: &[u8]) {
        self.seq += 1;
        let event = TerminalEvent::Data {
            seq: self.seq,
            data: BASE64.encode(bytes),
        };
        self.log.push(event.clone());
        self.subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Applies input and echoes it, as a PTY in cooked mode would.
    fn apply(&mut self, client: &str, seq: u64, bytes: &[u8]) {
        self.applied.insert(client.to_owned(), seq);
        self.typed.extend_from_slice(bytes);
        self.emit(bytes);
    }
}

/// One terminal, `term-1`, that echoes what is typed into it.
#[derive(Clone, Default)]
struct Echo(Arc<Mutex<Shell>>);

impl Echo {
    fn shell(&self) -> std::sync::MutexGuard<'_, Shell> {
        self.0.lock().unwrap()
    }

    fn known(terminal_id: &str) -> Result<(), String> {
        if terminal_id == TERMINAL {
            Ok(())
        } else {
            Err("Terminal not found".into())
        }
    }
}

impl HostTerminals for Echo {
    fn subscribe(
        &self,
        terminal_id: &str,
        after_seq: u64,
    ) -> Result<mpsc::UnboundedReceiver<TerminalEvent>, String> {
        Self::known(terminal_id)?;
        let (tx, rx) = mpsc::unbounded_channel();
        let mut shell = self.shell();
        for event in &shell.log {
            let TerminalEvent::Data { seq, .. } = event else {
                continue;
            };
            if *seq > after_seq {
                tx.send(event.clone()).unwrap();
            }
        }
        shell.subscribers.push(tx);
        Ok(rx)
    }

    fn write_sequenced(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, String> {
        Self::known(terminal_id)?;
        let mut shell = self.shell();
        if seq <= shell.applied.get(client).copied().unwrap_or(0) {
            return Ok(false);
        }
        shell.apply(client, seq, bytes);
        Ok(true)
    }

    fn write_next(
        &self,
        terminal_id: &str,
        client: &str,
        seq: u64,
        bytes: &[u8],
    ) -> Result<bool, String> {
        Self::known(terminal_id)?;
        let mut shell = self.shell();
        if seq != shell.applied.get(client).copied().unwrap_or(0) + 1 {
            return Ok(false);
        }
        shell.apply(client, seq, bytes);
        Ok(true)
    }

    fn resize(&self, terminal_id: &str, _cols: u16, _rows: u16) -> Result<(), String> {
        Self::known(terminal_id)
    }
}

/// `device` on a node of its own, its links serving `handler`.
async fn join(
    tailnet: &SimTailnet,
    device: &str,
    seed: u64,
    handler: Arc<dyn StreamHandler>,
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
            service: None,
            handler: Some(handler),
            seed: Some(seed),
        },
    ));
    links.start();
    (links, directory)
}

/// A host serving `echo` (its links first) and a viewer's links and hub, on `tailnet`.
async fn pair(
    tailnet: &SimTailnet,
    seed: u64,
    echo: &Echo,
) -> (Arc<PeerLinks>, Arc<PeerLinks>, RemoteTerminals) {
    let service = Arc::new(TerminalService::new(
        echo.clone(),
        RemoteTerminals::default(),
    ));
    let (host_links, host_directory) = join(tailnet, HOST, seed, service.clone()).await;
    service.set_links(&host_links);
    let hub = RemoteTerminals::default();
    let (viewer_links, viewer_directory) =
        join(tailnet, VIEWER, seed + 1, Arc::new(hub.clone())).await;
    for directory in [host_directory, viewer_directory] {
        directory
            .refresh()
            .await
            .expect("the simulated tailnet answers");
    }
    (host_links, viewer_links, hub)
}

/// What the viewer has seen: each event's seq, and the output's bytes.
#[derive(Default)]
struct Seen {
    seqs: Vec<u64>,
    out: Vec<u8>,
}

impl Seen {
    fn assert_whole(&self) {
        let expected: Vec<u64> = (1..=self.seqs.len() as u64).collect();
        assert_eq!(self.seqs, expected, "every event once, in order");
    }
}

async fn read_until(
    stream: &mut BoxStream<'static, serde_json::Value>,
    seen: &mut Seen,
    done: impl Fn(&Seen) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(120), async {
        while !done(seen) {
            let item = stream.next().await.expect("the subscription stays up");
            match serde_json::from_value(item).unwrap() {
                TerminalEvent::Data { seq, data } => {
                    seen.seqs.push(seq);
                    seen.out.extend(BASE64.decode(data).unwrap());
                }
                TerminalEvent::Exit { .. } => panic!("the shell exited"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out; saw {:?}",
            String::from_utf8_lossy(&seen.out).into_owned()
        )
    });
}

fn key(hub: &RemoteTerminals, links: &PeerLinks, byte: u8) {
    hub.send(links, HOST, TERMINAL, Write::Input(vec![byte]))
        .expect("the terminal takes input");
}

/// Keys reach the shell once and their echoes come back in order; the link pings at the
/// interactive rate while the terminal is attached, and an unknown terminal is refused.
#[tokio::test(start_paused = true)]
async fn keys_echo_over_the_link_while_it_is_held_interactive() {
    const SEED: u64 = 61;
    let _seed = report_seed("keys_echo_over_the_link_while_it_is_held_interactive", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let echo = Echo::default();
    let (_host, links, hub) = pair(&tailnet, SEED, &echo).await;

    let refused = hub.subscribe(links.clone(), HOST, "nope", 0).await;
    assert!(
        matches!(&refused, Err(RpcError::Failed(message)) if message.contains("Terminal not found")),
        "{:?}",
        refused.err()
    );

    let mut stream = hub
        .subscribe(links.clone(), HOST, TERMINAL, 0)
        .await
        .expect("the host serves the terminal");
    assert!(hub.attached(HOST, TERMINAL));
    assert!(
        links.links()[0].interactive,
        "an attached terminal holds the link interactive"
    );
    for &byte in b"echo hi\r" {
        key(&hub, &links, byte);
    }
    let mut seen = Seen::default();
    read_until(&mut stream, &mut seen, |s| s.out.len() >= 8).await;
    assert_eq!(seen.out, b"echo hi\r");
    seen.assert_whole();
    assert_eq!(echo.shell().typed, b"echo hi\r", "each key applied once");

    drop(stream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!hub.attached(HOST, TERMINAL));
    assert!(
        !links.links()[0].interactive,
        "detached, the link idles again"
    );
}

/// The path goes dark long enough for the link to be declared dead: output the host writes
/// and keys typed meanwhile arrive once the viewer's link is redialled, after the last event it
/// showed: no gap, no repeat, every key applied once.
#[tokio::test(start_paused = true)]
async fn a_link_lost_mid_stream_resumes_with_no_gap_and_no_repeat() {
    const SEED: u64 = 62;
    let _seed = report_seed(
        "a_link_lost_mid_stream_resumes_with_no_gap_and_no_repeat",
        SEED,
    );
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let echo = Echo::default();
    let (_host, links, hub) = pair(&tailnet, SEED, &echo).await;
    let mut stream = hub
        .subscribe(links.clone(), HOST, TERMINAL, 0)
        .await
        .expect("the host serves the terminal");
    let first = links.links()[0].id;

    for line in 0..20 {
        echo.shell().emit(format!("line {line}\n").as_bytes());
    }
    let mut seen = Seen::default();
    read_until(&mut stream, &mut seen, |s| s.seqs.len() == 20).await;

    tailnet.net().blackout(Duration::from_secs(6));
    for line in 20..40 {
        echo.shell().emit(format!("line {line}\n").as_bytes());
    }
    for &byte in b"abc" {
        key(&hub, &links, byte);
    }
    read_until(&mut stream, &mut seen, |s| s.seqs.len() == 43).await;

    seen.assert_whole();
    let text = String::from_utf8_lossy(&seen.out).into_owned();
    for line in 0..40 {
        assert_eq!(text.matches(&format!("line {line}\n")).count(), 1, "{text}");
    }
    assert!(text.ends_with("line 39\nabc"), "{text}");
    assert_eq!(
        echo.shell().typed,
        b"abc",
        "keys typed into the dead link, applied once"
    );
    assert_ne!(links.links()[0].id, first, "the link was redialled");
    assert!(
        links.links()[0].interactive,
        "the new link is held interactive too"
    );
}

/// Over a path that loses one packet in ten, every key reaches the shell once and in order and
/// every echo comes back once and in order, whichever of a stream copy or a datagram copy lands
/// first.
#[tokio::test(start_paused = true)]
async fn a_lossy_path_costs_time_never_a_key_or_an_event() {
    for seed in [71_u64, 72, 73] {
        let _seed = report_seed("a_lossy_path_costs_time_never_a_key_or_an_event", seed);
        let lossy = Link {
            delay: Duration::from_millis(4),
            loss: 0.1,
            ..Link::CLEAR
        };
        let tailnet = SimTailnet::new(Faults::over(lossy), seed);
        let echo = Echo::default();
        let (_host, links, hub) = pair(&tailnet, seed, &echo).await;
        let mut stream = hub
            .subscribe(links.clone(), HOST, TERMINAL, 0)
            .await
            .expect("the host serves the terminal");
        let keys: Vec<u8> = (0..40_u8).map(|n| b'a' + n % 26).collect();
        for &byte in &keys {
            key(&hub, &links, byte);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let mut seen = Seen::default();
        read_until(&mut stream, &mut seen, |s| s.out.len() >= keys.len()).await;
        assert_eq!(seen.out, keys, "seed {seed}");
        seen.assert_whole();
        assert_eq!(echo.shell().typed, keys, "seed {seed}");
    }
}
