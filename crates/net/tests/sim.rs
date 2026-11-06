//! The link on a simulated network, in virtual time (`onicode_net_sim`): a key's echo beside a
//! flood, a flood that stops soon after Ctrl-C, loss and reordering, a Wi-Fi blackout ridden out
//! and one that ends the link, a seed that repeats its run to the packet, a lost key carried by
//! its datagram copy, packets that never pass the tailnet's MTU, and builds that refuse each
//! other's wire. Nothing reads a real clock, so a minute's outage takes milliseconds and every
//! number here is the same on every run.
//!
//! Ported from axolotty's `axquic` `tests/sim.rs` (MIT); its lanes are terminal streams here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use onicode_net::hello::{self, Hello, Role, Session};
use onicode_net::link::Link;
use onicode_net::stream::{StreamHead, StreamKind, channel};
use onicode_net::transport::{ECHO_PRIORITY, PATH_MTU, endpoint_on};
use onicode_net::{NetError, StreamTx, close};
use onicode_net_sim::{Faults, Net, report_seed, shape};
use tokio::sync::mpsc;
use tokio::time::Instant;

const KEYS: u32 = 120;
const KEY_EVERY: Duration = Duration::from_millis(30);
/// Flood lanes are this and up; a terminal's lanes are below.
const FLOOD_BASE: u32 = 100;
const KEY_LANE: u32 = 0;
const ECHO_LANE: u32 = 9;

fn at(host: u8) -> SocketAddr {
    SocketAddr::from(([100, 64, 0, host], 7433))
}

/// The person's Wi-Fi to the far Mac: 6 ms there and back, a few ms of jitter, 5 MB/s with
/// 50 ms of buffer at the bottleneck.
fn wifi() -> shape::Link {
    shape::Link {
        delay: Duration::from_millis(3),
        jitter: Duration::from_millis(3),
        loss: 0.0,
        rate: 5_000_000,
        queue: 250_000,
    }
}

#[derive(Default)]
struct Got {
    lanes: Mutex<HashMap<u32, Vec<u8>>>,
    /// Keys (lane 0) and echoes as the app applies them: key `n` only after key `n - 1`, from
    /// the stream or a datagram copy, whichever comes first; a copy ahead of its turn is
    /// dropped, as the app drops an output copy past its mirror's offset. When each was applied.
    applied: Mutex<HashMap<u32, Vec<Instant>>>,
    closed: Mutex<Option<Option<String>>>,
}

impl Got {
    fn arrive(&self, lane: u32, id: u32) {
        let mut applied = self.applied.lock().unwrap();
        let times = applied.entry(lane).or_default();
        if id as usize == times.len() {
            times.push(Instant::now());
        }
    }

    fn applied(&self, lane: u32) -> Vec<Instant> {
        self.applied
            .lock()
            .unwrap()
            .get(&lane)
            .cloned()
            .unwrap_or_default()
    }

    fn data(&self, lane: u32, bytes: &[u8]) {
        let mut lanes = self.lanes.lock().unwrap();
        let buf = lanes.entry(lane).or_default();
        let before = buf.len() / 4;
        buf.extend_from_slice(bytes);
        let units: Vec<u32> = if lane < FLOOD_BASE {
            buf[before * 4..]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|u| u32::from_be_bytes(*u))
                .collect()
        } else {
            Vec::new()
        };
        drop(lanes);
        for id in units {
            self.arrive(lane, id);
        }
    }

    fn copy(&self, lane: u32, bytes: &[u8]) {
        if let Ok(id) = bytes.try_into() {
            self.arrive(lane, u32::from_be_bytes(id));
        }
    }

    fn lane_len(&self, lane: u32) -> usize {
        self.lanes.lock().unwrap().get(&lane).map_or(0, Vec::len)
    }
}

/// One end's streams, a terminal stream per lane, opened by its first bytes.
#[derive(Clone)]
struct Side {
    link: Link,
    lanes: Arc<Mutex<HashMap<u32, Arc<StreamTx>>>>,
    opening: Arc<tokio::sync::Mutex<()>>,
}

impl Side {
    fn new(link: Link) -> Self {
        Self {
            link,
            lanes: Arc::default(),
            opening: Arc::default(),
        }
    }

    async fn tx(&self, lane: u32) -> Option<Arc<StreamTx>> {
        let found = self.lanes.lock().unwrap().get(&lane).cloned();
        if found.is_some() {
            return found;
        }
        let _opening = self.opening.lock().await;
        let found = self.lanes.lock().unwrap().get(&lane).cloned();
        if found.is_some() {
            return found;
        }
        let head = StreamHead::new(StreamKind::Terminal, lane.to_string());
        let (tx, _rx) = self.link.open(&head).await.ok()?;
        let tx = Arc::new(tx);
        self.lanes.lock().unwrap().insert(lane, Arc::clone(&tx));
        Some(tx)
    }

    /// Queues `data` on `lane` at `priority`.
    async fn send(&self, lane: u32, priority: i32, data: Bytes) {
        let Some(tx) = self.tx(lane).await else {
            return;
        };
        tx.set_priority(priority);
        tx.send(data);
    }

    /// A datagram copy of bytes sent on `lane`.
    fn copy(&self, lane: u32, data: &[u8]) {
        let mut d = lane.to_be_bytes().to_vec();
        d.extend_from_slice(data);
        self.link.copy(channel::TERMINAL_COPY, &d);
    }

    fn pending(&self, lane: u32) -> usize {
        self.lanes
            .lock()
            .unwrap()
            .get(&lane)
            .map_or(0, |tx| tx.pending())
    }

    fn close(&self) {
        self.link.close(close::NORMAL, b"");
    }

    /// Reads every stream the peer opens, its datagram copies and its end into `got`, a lane
    /// number on `notify` per arrival (`u32::MAX` for the end).
    fn start(&self, got: Arc<Got>, notify: mpsc::UnboundedSender<u32>) {
        let (link, g, n) = (self.link.clone(), Arc::clone(&got), notify.clone());
        tokio::spawn(async move {
            while let Ok((_tx, mut rx)) = link.accept().await {
                let (g, n) = (Arc::clone(&g), n.clone());
                tokio::spawn(async move {
                    let Ok(head) = rx.head().await else { return };
                    let Ok(lane) = head.name.parse::<u32>() else {
                        return;
                    };
                    while let Ok(Some(chunk)) = rx.chunk().await {
                        g.data(lane, &chunk);
                        let _ = n.send(lane);
                    }
                });
            }
        });
        let (link, g, n) = (self.link.clone(), Arc::clone(&got), notify.clone());
        tokio::spawn(async move {
            while let Ok((chan, d)) = link.recv_datagram().await {
                if chan != channel::TERMINAL_COPY || d.len() < 4 {
                    continue;
                }
                let lane = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
                g.copy(lane, &d[4..]);
                let _ = n.send(lane);
            }
        });
        let link = self.link.clone();
        tokio::spawn(async move {
            let reason = link.closed().await.map(|e| e.to_string());
            *got.closed.lock().unwrap() = Some(reason);
            let _ = notify.send(u32::MAX);
        });
    }
}

struct Pair {
    net: Net,
    client: Side,
    server: Side,
    c: Arc<Got>,
    s: Arc<Got>,
    s_rx: mpsc::UnboundedReceiver<u32>,
    c_rx: mpsc::UnboundedReceiver<u32>,
    _sessions: (Session, Session),
}

async fn sessions(
    net: &Net,
    seed: u64,
    client_hello: Hello,
) -> (Result<Session, NetError>, Result<Session, NetError>) {
    let server = endpoint_on(Box::new(net.bind(at(2)).unwrap()), true, Some(seed)).unwrap();
    let client = endpoint_on(Box::new(net.bind(at(3)).unwrap()), false, Some(seed ^ 1)).unwrap();
    let accept = tokio::spawn(async move {
        let incoming = server.accept().await.expect("an incoming connection");
        let remote = incoming.remote_address();
        let conn = incoming.await.expect("the handshake completes");
        hello::accept(conn, remote, &Hello::new("server", Role::Engine)).await
    });
    let dialed = hello::dial(&client, at(2), &client_hello).await;
    (dialed, accept.await.unwrap())
}

async fn pair(faults: Faults, seed: u64) -> Pair {
    let net = Net::new(faults, seed);
    let (c, s) = sessions(&net, seed, Hello::new("client", Role::Viewer)).await;
    let (c, s) = (
        c.expect("the dial succeeds"),
        s.expect("the accept succeeds"),
    );
    assert_eq!(c.peer.device_id, "server");
    assert_eq!(s.peer.device_id, "client");
    let (client, server) = (Side::new(c.link.clone()), Side::new(s.link.clone()));
    let (cg, sg) = (Arc::new(Got::default()), Arc::new(Got::default()));
    let (ctx, c_rx) = mpsc::unbounded_channel();
    let (stx, s_rx) = mpsc::unbounded_channel();
    client.start(Arc::clone(&cg), ctx);
    server.start(Arc::clone(&sg), stx);
    Pair {
        net,
        client,
        server,
        c: cg,
        s: sg,
        s_rx,
        c_rx,
        _sessions: (c, s),
    }
}

/// Keeps a terminal's worth of output queued on each of `lanes` flood lanes, as `cat` on a big
/// file does in each.
fn flood(side: &Side, lanes: u32) -> tokio::task::JoinHandle<()> {
    let side = side.clone();
    tokio::spawn(async move {
        let chunk = Bytes::from(vec![b'x'; 16 << 10]);
        loop {
            for lane in FLOOD_BASE..FLOOD_BASE + lanes {
                while side.pending(lane) < 256 << 10 {
                    side.send(lane, 0, chunk.clone()).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
}

/// A key every 30 ms, each echoed by the far side the moment it is applied, at
/// `echo_priority`; with `copies`, each key and echo also goes as a datagram copy. Returns each
/// key's time to its echo, as the app would apply it.
async fn type_keys(p: &mut Pair, echo_priority: i32, copies: bool) -> Vec<Duration> {
    let server = p.server.clone();
    let s = Arc::clone(&p.s);
    let mut s_rx = std::mem::replace(&mut p.s_rx, mpsc::unbounded_channel().1);
    // The far side: every key applied is echoed back on the terminal's lane, in order.
    let echoer = tokio::spawn(async move {
        let mut echoed = 0;
        while s_rx.recv().await.is_some() {
            let applied = s.applied(KEY_LANE).len() as u32;
            for id in echoed..applied {
                let echo = id.to_be_bytes();
                server
                    .send(ECHO_LANE, echo_priority, Bytes::copy_from_slice(&echo))
                    .await;
                if copies {
                    server.copy(ECHO_LANE, &echo);
                }
            }
            echoed = echoed.max(applied);
        }
    });
    let mut sent = Vec::new();
    for k in 0..KEYS {
        sent.push(Instant::now());
        p.client
            .send(
                KEY_LANE,
                ECHO_PRIORITY,
                Bytes::copy_from_slice(&k.to_be_bytes()),
            )
            .await;
        if copies {
            p.client.copy(KEY_LANE, &k.to_be_bytes());
        }
        tokio::time::sleep(KEY_EVERY).await;
    }
    let c = Arc::clone(&p.c);
    tokio::time::timeout(Duration::from_secs(30), async {
        while c.applied(ECHO_LANE).len() < KEYS as usize {
            p.c_rx.recv().await;
        }
    })
    .await
    .expect("every echo comes back");
    echoer.abort();
    let echoes = p.c.lanes.lock().unwrap()[&ECHO_LANE].clone();
    let order: Vec<u32> = echoes
        .chunks(4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(
        order,
        (0..KEYS).collect::<Vec<_>>(),
        "every key's echo on its stream, in order"
    );
    sent.iter()
        .zip(p.c.applied(ECHO_LANE))
        .map(|(s, t)| t - *s)
        .collect()
}

fn quantile(v: &[Duration], q: f64) -> Duration {
    let mut v = v.to_vec();
    v.sort();
    v[((v.len() as f64 * q) as usize).min(v.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn flooded(got: &Got) -> usize {
    got.lanes
        .lock()
        .unwrap()
        .iter()
        .filter(|(l, _)| **l >= FLOOD_BASE)
        .map(|(_, b)| b.len())
        .sum()
}

/// The seeds a run sweeps: one in the gate, `ONICODE_SIM_SEEDS=n` for a long run.
fn seeds(gate: u64) -> Vec<u64> {
    match std::env::var("ONICODE_SIM_SEEDS")
        .ok()
        .and_then(|n| n.parse::<u64>().ok())
    {
        Some(n) => (1..=n).collect(),
        None => vec![gate],
    }
}

/// Twelve terminals stream a build log on the far Mac while the person types in another: the
/// echo leaves ahead of all of them, so it waits on the path's queue, not behind twelve
/// packets of other terminals' output per hop. The same echo at the terminals' own priority is
/// the canary: it is shown slower in the same run, so the bound can fail.
#[tokio::test(start_paused = true)]
async fn an_echo_beside_a_flood_waits_on_the_path_not_the_flood() {
    const TERMINALS: u32 = 12;
    let mean = |v: &[Duration]| v.iter().sum::<Duration>() / v.len() as u32;
    let mut held = Vec::new();
    for seed in seeds(11) {
        let _seed = report_seed(
            "an_echo_beside_a_flood_waits_on_the_path_not_the_flood",
            seed,
        );
        let mut idle = pair(Faults::over(wifi()), seed).await;
        let alone = type_keys(&mut idle, ECHO_PRIORITY, false).await;
        idle.client.close();

        let run = async |priority: i32| {
            let mut p = pair(Faults::over(wifi()), seed).await;
            let f = flood(&p.server, TERMINALS);
            tokio::time::sleep(Duration::from_millis(500)).await; // the flood fills the path first
            let start = Instant::now();
            let echoes = type_keys(&mut p, priority, false).await;
            let rate = flooded(&p.c) as f64
                / 1e6
                / (start.elapsed() + Duration::from_millis(500)).as_secs_f64();
            let bandwidth = p.server.link.bandwidth();
            f.abort();
            p.client.close();
            (echoes, rate, p.net.counts(), bandwidth)
        };
        let (lifted, rate, counts, bandwidth) = run(ECHO_PRIORITY).await;
        let (level, _, _, _) = run(0).await;

        let q = |v: &[Duration], f| ms(quantile(v, f));
        println!(
            "seed {seed}: echo p50/p99/max ms idle {:.0}/{:.0}/{:.0}, flooded {:.0}/{:.0}/{:.0}, flooded level {:.0}/{:.0}/{:.0}; flood {rate:.2} MB/s (BBR reads {:.2}), overflowed {} lost {} of {}",
            q(&alone, 0.5),
            q(&alone, 0.99),
            q(&alone, 1.0),
            q(&lifted, 0.5),
            q(&lifted, 0.99),
            q(&lifted, 1.0),
            q(&level, 0.5),
            q(&level, 0.99),
            q(&level, 1.0),
            bandwidth.unwrap_or(0) as f64 / 1e6,
            counts.shaped.overflowed,
            counts.shaped.lost,
            counts.shaped.sent,
        );
        if std::env::var_os("ONICODE_SIM_REPORT").is_some() {
            continue;
        }
        assert!(
            rate > 4.0,
            "seed {seed}: the flood still has most of the 5 MB/s link ({rate:.2})"
        );
        // The app sizes how far a terminal may fall behind by this, so it has to be near the
        // truth. BBR's is a windowed maximum, and jitter bunches acknowledgements (axquic read
        // 5.3–6.1 over 20 seeds).
        let bandwidth = bandwidth.expect("measured while flooding") as f64 / 1e6;
        assert!(
            (4.5..=7.0).contains(&bandwidth),
            "seed {seed}: BBR reads the 5 MB/s link as {bandwidth:.2} MB/s"
        );
        // What the flood adds to an echo is the bottleneck's standing queue: BBR's bound on what
        // is in flight, about two round trips at this rate (15–17 ms over 30 seeds in axquic;
        // before the send window was fitted and an echo exempt from it, 100–450 ms in half).
        let added = quantile(&lifted, 1.0).saturating_sub(quantile(&alone, 1.0));
        assert!(
            added <= Duration::from_millis(20),
            "seed {seed}: the flood adds {added:?} to an echo"
        );
        assert!(
            counts.largest <= u64::from(PATH_MTU),
            "seed {seed}: a {}-byte datagram",
            counts.largest
        );
        held.push(mean(&level).saturating_sub(mean(&lifted)));
    }
    // The canary: the same echo at the terminals' priority waits its turn behind them. By how
    // much varies with the seed (0.3–3 ms on average over 100), so it is held over the sweep.
    if !held.is_empty() {
        let held = mean(&held);
        assert!(
            held >= Duration::from_millis(1),
            "unlifted, the echo waits its turn ({held:?})"
        );
    }
}

/// Ctrl-C on twelve flooding terminals: once the programs stop, what the link still holds
/// drains in the time its queued bytes take at the link's rate, and QUIC's own buffer adds at
/// most a few round trips to that. noq's default buffer, the canary, held seconds more.
#[tokio::test(start_paused = true)]
async fn after_a_flood_stops_the_screen_stops_soon_after() {
    const SEED: u64 = 11;
    let _seed = report_seed("after_a_flood_stops_the_screen_stops_soon_after", SEED);
    async fn drain(fitted: bool) -> (Duration, f64) {
        let mut p = pair(Faults::over(wifi()), SEED).await;
        if !fitted {
            p.server.link.keep_default_send_window();
        }
        let f = flood(&p.server, 12);
        tokio::time::sleep(Duration::from_secs(2)).await;
        f.abort();
        let stop = Instant::now();
        // What the app still had queued when the programs stopped: it drains at the link's rate.
        let queued = p.server.link.pending_total();
        let rate = flooded(&p.c) as f64 / 2.0;
        // The link is quiet once nothing has come for 100 ms.
        let mut last = (flooded(&p.c), Instant::now());
        while last.1.elapsed() < Duration::from_millis(100) {
            tokio::select! {
                biased;
                _ = p.c_rx.recv() => {}
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            let now = flooded(&p.c);
            if now != last.0 {
                last = (now, Instant::now());
            }
        }
        let extra = (last.1 - stop).saturating_sub(Duration::from_secs_f64(queued as f64 / rate));
        p.client.close();
        (extra, rate)
    }
    let (fitted, rate) = drain(true).await;
    let (default, _) = drain(false).await;
    println!(
        "past the app's queue, the link drained {fitted:?} fitted, {default:?} at noq's default; {:.2} MB/s",
        rate / 1e6
    );
    assert!(
        rate > 4.0e6,
        "fitting the buffer costs no throughput ({:.2} MB/s)",
        rate / 1e6
    );
    assert!(
        fitted <= Duration::from_millis(150),
        "QUIC's buffer held {fitted:?} of output"
    );
    assert!(
        default >= Duration::from_secs(1),
        "the canary: noq's default buffer holds seconds ({default:?})"
    );
}

/// Loss and reordering cost time, never bytes or order: every stream arrives whole.
#[tokio::test(start_paused = true)]
async fn loss_and_reordering_cost_time_not_bytes() {
    const SEED: u64 = 5;
    let _seed = report_seed("loss_and_reordering_cost_time_not_bytes", SEED);
    let mut faults = Faults::over(shape::Link {
        loss: 0.03,
        ..wifi()
    });
    faults.reorder = 0.02;
    faults.reorder_by = Duration::from_millis(8);
    let mut p = pair(faults, SEED).await;
    let lanes: Vec<u32> = vec![0, 3, 7, 1_000_003];
    // A kilobyte per message, so the run is a thousand-odd packets and the faults land.
    let message = |i: u32, l: u32| -> Vec<u8> { (i ^ l).to_be_bytes().repeat(256) };
    for i in 0..400_u32 {
        for &l in &lanes {
            p.server.send(l, 0, Bytes::from(message(i, l))).await;
        }
        if i % 16 == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    let c = Arc::clone(&p.c);
    tokio::time::timeout(Duration::from_secs(60), async {
        while lanes.iter().any(|l| c.lane_len(*l) < 400 * 1024) {
            p.c_rx.recv().await;
        }
    })
    .await
    .expect("everything arrives");
    for &l in &lanes {
        let got = p.c.lanes.lock().unwrap()[&l].clone();
        let want: Vec<u8> = (0..400_u32).flat_map(|i| message(i, l)).collect();
        assert_eq!(got, want, "lane {l}");
    }
    let counts = p.net.counts();
    assert!(
        counts.shaped.lost > 0 && counts.reordered > 0,
        "the run had loss and reordering: {counts:?}"
    );
    assert!(
        counts.largest <= u64::from(PATH_MTU),
        "no datagram past the tailnet's MTU: {counts:?}"
    );
}

/// Wi-Fi drops for ten seconds (a lid half-closed, a lift): the link is still there after,
/// and what was sent meanwhile arrives.
#[tokio::test(start_paused = true)]
async fn a_blackout_shorter_than_the_idle_timeout_is_ridden_out() {
    const SEED: u64 = 3;
    let _seed = report_seed(
        "a_blackout_shorter_than_the_idle_timeout_is_ridden_out",
        SEED,
    );
    let mut p = pair(Faults::over(wifi()), SEED).await;
    p.net.blackout(Duration::from_secs(10));
    p.server.send(5, 0, Bytes::from_static(b"during")).await;
    tokio::time::sleep(Duration::from_secs(12)).await;
    p.server.send(5, 0, Bytes::from_static(b"after!")).await;
    let c = Arc::clone(&p.c);
    tokio::time::timeout(Duration::from_secs(5), async {
        while c.lane_len(5) < 12 {
            p.c_rx.recv().await;
        }
    })
    .await
    .expect("both arrive once the path is back");
    assert_eq!(p.c.lanes.lock().unwrap()[&5], b"duringafter!");
    assert_eq!(*p.c.closed.lock().unwrap(), None, "still open");
}

/// A Mac gone for good (asleep, off the tailnet) ends the link within the idle timeout, so the
/// app says so and redials instead of waiting for ever.
#[tokio::test(start_paused = true)]
async fn a_peer_gone_for_good_ends_the_link() {
    const SEED: u64 = 4;
    let _seed = report_seed("a_peer_gone_for_good_ends_the_link", SEED);
    let mut p = pair(Faults::over(wifi()), SEED).await;
    let start = Instant::now();
    p.net.blackout(Duration::from_secs(600));
    let c = Arc::clone(&p.c);
    tokio::time::timeout(Duration::from_secs(120), async {
        while c.closed.lock().unwrap().is_none() {
            p.c_rx.recv().await;
        }
    })
    .await
    .expect("the link ends");
    let reason = p.c.closed.lock().unwrap().clone().unwrap();
    assert!(reason.is_some(), "an end nobody chose is an error");
    assert!(
        start.elapsed() <= Duration::from_secs(50),
        "{:?}",
        start.elapsed()
    );
}

/// The same seed is the same run, packet for packet; another seed is another run.
#[tokio::test(start_paused = true)]
async fn a_seed_repeats_its_run() {
    async fn run(seed: u64) -> (u64, Vec<Duration>) {
        let _seed = report_seed("a_seed_repeats_its_run", seed);
        let mut p = pair(
            Faults::over(shape::Link {
                loss: 0.01,
                ..wifi()
            }),
            seed,
        )
        .await;
        let f = flood(&p.server, 4);
        let echoes = type_keys(&mut p, ECHO_PRIORITY, true).await;
        f.abort();
        p.client.close();
        (p.net.digest(), echoes)
    }
    let a = run(21).await;
    let b = run(21).await;
    let c = run(22).await;
    assert_eq!(a, b);
    assert_ne!(a.0, c.0);
}

/// Wi-Fi loses a packet in a hundred, each way: a key or echo in that packet waits for QUIC to
/// notice and resend it, three round trips or so. Its datagram copy, a couple of milliseconds
/// behind it in a packet of its own, arrives instead. Without copies, the canary, the tail is
/// the retransmission's.
#[tokio::test(start_paused = true)]
async fn a_lost_key_or_echo_is_carried_by_its_copy() {
    let lossy = || {
        Faults::over(shape::Link {
            loss: 0.01,
            ..wifi()
        })
    };
    let (mut copied, mut alone, mut clean) = (Vec::new(), Vec::new(), Vec::new());
    // Ten seeds in the gate (1 200 keys); `ONICODE_SIM_SEEDS=n` sweeps n, a line each as it ends.
    let runs: Vec<u64> = if std::env::var_os("ONICODE_SIM_SEEDS").is_some() {
        seeds(0)
    } else {
        (1..=10).collect()
    };
    for seed in runs {
        let _seed = report_seed("a_lost_key_or_echo_is_carried_by_its_copy", seed);
        for (copies, out, faults) in [(true, &mut copied, lossy()), (false, &mut alone, lossy())] {
            let mut p = pair(faults, seed).await;
            out.extend(type_keys(&mut p, ECHO_PRIORITY, copies).await);
            p.client.close();
        }
        let mut p = pair(Faults::over(wifi()), seed).await;
        clean.extend(type_keys(&mut p, ECHO_PRIORITY, true).await);
        p.client.close();
        println!(
            "loss seed {seed}: copied p99 so far {:.0} ms",
            ms(quantile(&copied, 0.99))
        );
    }
    let q = |v: &[Duration], f| ms(quantile(v, f));
    println!(
        "1% loss each way, echo p50/p99/max ms: copied {:.0}/{:.0}/{:.0}, alone {:.0}/{:.0}/{:.0}; lossless {:.0}/{:.0}/{:.0} (n={})",
        q(&copied, 0.5),
        q(&copied, 0.99),
        q(&copied, 1.0),
        q(&alone, 0.5),
        q(&alone, 0.99),
        q(&alone, 1.0),
        q(&clean, 0.5),
        q(&clean, 0.99),
        q(&clean, 1.0),
        copied.len(),
    );
    // A copy costs one COPY_DELAY behind the packet it stands in for, and its own jitter.
    let added = quantile(&copied, 0.99).saturating_sub(quantile(&clean, 0.99));
    assert!(
        added <= Duration::from_millis(6),
        "copies leave {added:?} of loss in the p99"
    );
    assert_eq!(
        quantile(&copied, 0.5),
        quantile(&clean, 0.5),
        "copies cost the typical key nothing"
    );
    let canary = quantile(&alone, 0.99).saturating_sub(quantile(&clean, 0.99));
    assert!(
        canary >= Duration::from_millis(15),
        "without copies, loss shows in the p99 ({canary:?})"
    );
}

/// Two builds with different wires refuse each other at the hello, and each side is told the
/// other's build, never a decode error. Only the client's hello is from another build here, so
/// the client reads the server's hello as its own wire and learns of the skew from the server's
/// close (a real older build reads it at the hello, whichever comes first).
#[tokio::test(start_paused = true)]
async fn builds_with_different_wires_refuse_each_other_at_the_hello() {
    const SEED: u64 = 7;
    let _seed = report_seed(
        "builds_with_different_wires_refuse_each_other_at_the_hello",
        SEED,
    );
    let net = Net::new(Faults::over(wifi()), SEED);
    let mut newer = Hello::new("client", Role::Engine);
    newer.wire = hello::WIRE + 1;
    newer.build = "9.9.9".to_owned();
    let (dialed, accepted) = sessions(&net, SEED, newer).await;
    let Err(NetError::WrongBuild(at_server)) = accepted else {
        panic!("the acceptor refuses: {accepted:?}")
    };
    assert_eq!(at_server.peer, format!("9.9.9 wire {}", hello::WIRE + 1));
    let at_client = match dialed {
        Err(NetError::WrongBuild(wrong)) => wrong,
        Ok(session) => match session.link.closed().await {
            Some(NetError::WrongBuild(wrong)) => wrong,
            other => panic!("the dialer is told by the close: {other:?}"),
        },
        Err(other) => panic!("the dialer is told: {other:?}"),
    };
    assert_eq!(
        at_client.peer,
        hello::this_build(),
        "the server's build, from its close"
    );
}
