//! Engines on a simulated tailnet (`onicode_net_sim::SimTailnet`), in virtual time: who
//! discovery finds, who admission lets in and what a refusal costs, a daemon that is down,
//! refusing or stuck, `Online` flaps under a live link, two engines dialling each other at
//! once, a link redialled after its peer slept, and RPC over the link. Every run is seeded
//! and prints its seed when it fails.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::{Engine, engine, engine_with, found};
use onicode_net::hello::{self, Hello};
use onicode_net::transport::{PORT, endpoint_on};
use onicode_net::{NetError, Role};
use onicode_net_sim::tailnet::{SLOW_API, ipv4};
use onicode_net_sim::{ApiFault, Faults, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{LinkState, LinksConfig, PeerError};
use onicode_rpc::{RpcError, RpcReply, RpcService};
use onicode_tailnet::{LocalApiError, Relooking, Status, Tailnet, Watch, WhoIs};
use tokio::time::Instant;

const USER: i64 = 2;

fn at(node: onicode_net_sim::NodeId) -> SocketAddr {
    SocketAddr::new(ipv4(node), PORT)
}

/// Waits (in virtual time, at most `within`) until `done` holds of `engine`'s links.
async fn until(engine: &Engine, within: Duration, done: impl Fn(&Engine) -> bool) -> bool {
    let mut changes = engine.links.subscribe_changes();
    tokio::time::timeout(within, async {
        while !done(engine) {
            tokio::select! {
                _ = changes.changed() => {}
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    })
    .await
    .is_ok()
}

/// Discovery finds exactly this user's own engines the control plane calls online: not one
/// asleep, not a phone, not another user's, not a tagged node with no grant.
#[tokio::test(start_paused = true)]
async fn discovery_finds_exactly_the_users_own_online_engines() {
    const SEED: u64 = 31;
    let _seed = report_seed("discovery_finds_exactly_the_users_own_online_engines", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
    let studio = tailnet.add(NodeSpec::owned("studio", USER));
    let phone = tailnet.add(NodeSpec::owned("phone", USER).on("iOS"));
    let stranger = tailnet.add(NodeSpec::owned("other", 7));
    let vps = tailnet.add(NodeSpec::tagged("vps", "tag:onicode"));
    let engines = [
        engine(&tailnet, mac, "dev-mac", SEED).await,
        engine(&tailnet, laptop, "dev-laptop", SEED + 1).await,
        engine(&tailnet, studio, "dev-studio", SEED + 2).await,
        engine(&tailnet, phone, "dev-phone", SEED + 3).await,
        engine(&tailnet, stranger, "dev-other", SEED + 4).await,
        engine(&tailnet, vps, "dev-vps", SEED + 5).await,
    ];
    tailnet.set_asleep(studio, true);

    let peers = engines[0].directory.refresh().await.unwrap();
    let devices: Vec<&str> = peers.iter().map(|p| p.device_id.as_str()).collect();
    assert_eq!(devices, ["dev-laptop"]);
    let laptop_peer = &peers[0];
    assert_eq!(laptop_peer.addr, at(laptop));
    assert_eq!(
        laptop_peer.node,
        onicode_net_sim::tailnet::stable_id(laptop)
    );
    assert!(laptop_peer.owner);
    assert_eq!(laptop_peer.name, "laptop.sim.ts.net");
    // A probe leaves no link at either end.
    assert!(engines[0].links.links().is_empty() && engines[1].links.links().is_empty());

    tailnet.set_asleep(studio, false);
    let woke = engines[0].watch();
    let mut changes = engines[0].directory.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        while found(&engines[0]) != ["dev-laptop", "dev-studio"] {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("a node that wakes is found from the daemon's notification");
    woke.abort();
}

/// An address on the tailnet that no node holds is refused before its handshake: it costs
/// this engine one packet, the refusal, and the caller learns it was refused.
#[tokio::test(start_paused = true)]
async fn a_caller_the_tailnet_does_not_know_costs_one_packet() {
    const SEED: u64 = 32;
    let _seed = report_seed("a_caller_the_tailnet_does_not_know_costs_one_packet", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let _engine = engine(&tailnet, mac, "dev-mac", SEED).await;

    let ghost = SocketAddr::from(([100, 64, 0, 200], PORT));
    let socket = tailnet.net().bind(ghost).unwrap();
    let endpoint = endpoint_on(Box::new(socket), false, Some(SEED)).unwrap();
    let dialed = hello::dial(&endpoint, at(mac), &Hello::new("ghost", Role::Engine)).await;
    assert!(
        matches!(dialed, Err(NetError::Refused(_))),
        "{:?}",
        dialed.map(|s| s.peer)
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        tailnet.net().delivered(ipv4(mac), ghost.ip()),
        1,
        "one packet back: the refusal"
    );
}

/// A node the tailnet names but grants nothing here (another user's machine) is told so once
/// its handshake is done, and is never adopted.
#[tokio::test(start_paused = true)]
async fn a_node_with_no_grant_is_told_so() {
    const SEED: u64 = 33;
    let _seed = report_seed("a_node_with_no_grant_is_told_so", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let stranger = tailnet.add(NodeSpec::owned("other", 7));
    let mac_engine = engine(&tailnet, mac, "dev-mac", SEED).await;
    // The stranger's own side lets the mac in, so only the mac's check stands between them.
    tailnet.grant(mac, stranger, &["engine"]);

    let sockets = common::SimSockets(tailnet.net().clone());
    let stranger_api = tailnet.local_api(stranger);
    let config = onicode_peer::PeerConfig::new("dev-other", Role::Engine).with_seed(SEED + 1);
    let stranger_end = onicode_peer::PeerEndpoint::bind(stranger_api, &sockets, config)
        .await
        .unwrap();
    let dialed = stranger_end.dial(at(mac)).await;
    assert!(
        matches!(dialed, Err(PeerError::Net(NetError::NotGranted))),
        "{dialed:?}"
    );
    assert!(mac_engine.links.links().is_empty());
}

/// A tagged node (a VPS) is an engine here only once a grant says so; the grant arriving is a
/// change the daemon reports, and discovery finds it from that alone.
#[tokio::test(start_paused = true)]
async fn a_tagged_node_is_an_engine_only_with_a_grant() {
    const SEED: u64 = 34;
    let _seed = report_seed("a_tagged_node_is_an_engine_only_with_a_grant", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let vps = tailnet.add(NodeSpec::tagged("vps", "tag:onicode"));
    let mac_engine = engine(&tailnet, mac, "dev-mac", SEED).await;
    let vps_engine = engine(&tailnet, vps, "dev-vps", SEED + 1).await;
    // Both watch, so a grant arriving clears what each had read of the other.
    let watching = [mac_engine.watch(), vps_engine.watch()];
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(found(&mac_engine).is_empty(), "no grant, no engine");
    assert!(
        matches!(
            vps_engine.endpoint.dial(at(mac)).await,
            Err(PeerError::NotGranted(_))
        ),
        "nor a dial out"
    );

    // Each side's policy lets the other in.
    tailnet.grant(vps, mac, &["engine"]);
    tailnet.grant(mac, vps, &["engine"]);
    let mut changes = mac_engine.directory.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        while found(&mac_engine) != ["dev-vps"] {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the granted VPS is found");
    let peer = mac_engine.directory.get("dev-vps").unwrap();
    assert!(!peer.owner);
    let link = mac_engine.links.ready("dev-vps").await.unwrap();
    assert!(!link.is_closed());
    for w in watching {
        w.abort();
    }
}

/// With the daemon down, nobody new gets in, and discovery says why instead of finding nothing.
#[tokio::test(start_paused = true)]
async fn a_local_api_that_is_down_fails_closed() {
    const SEED: u64 = 35;
    let _seed = report_seed("a_local_api_that_is_down_fails_closed", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
    let mac_engine = engine(&tailnet, mac, "dev-mac", SEED).await;
    let laptop_engine = engine(&tailnet, laptop, "dev-laptop", SEED + 1).await;

    tailnet.set_api(mac, ApiFault::Down);
    let dialed = laptop_engine.endpoint.dial(at(mac)).await;
    assert!(
        matches!(dialed, Err(PeerError::Net(NetError::Refused(_)))),
        "{dialed:?}"
    );
    assert!(matches!(
        mac_engine.directory.refresh().await,
        Err(PeerError::Tailnet(LocalApiError::Io(_)))
    ));
    assert!(mac_engine.links.links().is_empty());
    assert!(matches!(
        mac_engine.links.ready("dev-laptop").await,
        Err(PeerError::Unknown(_))
    ));

    tailnet.set_api(mac, ApiFault::Up);
    let dialed = laptop_engine
        .endpoint
        .dial(at(mac))
        .await
        .expect("let in once the daemon answers");
    assert_eq!(dialed.session.peer.device_id, "dev-mac");
}

/// A daemon that fails as it is found: the stand-in for a refusing (401) or stuck one.
struct Failing(ApiFault);

impl Failing {
    async fn fail<T>(&self) -> Result<T, LocalApiError> {
        match self.0 {
            ApiFault::Slow => {
                tokio::time::sleep(SLOW_API).await;
                Err(LocalApiError::Timeout)
            }
            ApiFault::Unauthorized => Err(LocalApiError::Status {
                code: 401,
                body: "bad token".to_owned(),
            }),
            ApiFault::Down | ApiFault::Up => Err(LocalApiError::Missing),
        }
    }
}

#[async_trait]
impl Tailnet for Failing {
    async fn status(&self) -> Result<Status, LocalApiError> {
        self.fail().await
    }

    async fn whois(&self, _from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError> {
        self.fail().await
    }

    async fn watch(&self) -> Result<Watch, LocalApiError> {
        self.fail().await
    }
}

/// The daemon found first refuses the token (an App Store extension back on a new port) or
/// never answers: the same call looks it up again and goes on with the one found then.
#[tokio::test(start_paused = true)]
async fn a_refusing_or_stuck_local_api_is_looked_up_again() {
    const SEED: u64 = 36;
    let _seed = report_seed("a_refusing_or_stuck_local_api_is_looked_up_again", SEED);
    for (i, fault) in [ApiFault::Unauthorized, ApiFault::Slow]
        .into_iter()
        .enumerate()
    {
        let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
        let mac = tailnet.add(NodeSpec::owned("mac", USER));
        let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
        let _laptop_engine = engine(&tailnet, laptop, "dev-laptop", SEED + 1).await;
        let finds = Arc::new(AtomicUsize::new(0));
        let (sim, count) = (tailnet.clone(), Arc::clone(&finds));
        let relooking: Arc<dyn Tailnet> = Arc::new(Relooking::new(
            move || {
                let n = count.fetch_add(1, Ordering::Relaxed);
                Some(if n == 0 {
                    Arc::new(Failing(fault)) as Arc<dyn Tailnet>
                } else {
                    sim.local_api(mac)
                })
            },
            Duration::from_secs(5),
        ));
        let start = Instant::now();
        let mac_engine = engine_with(
            &tailnet,
            relooking,
            "dev-mac",
            SEED + 10 + i as u64,
            LinksConfig::default(),
        )
        .await;
        assert_eq!(
            finds.load(Ordering::Relaxed),
            2,
            "{fault:?}: found again within the one call"
        );
        let waited = start.elapsed();
        match fault {
            ApiFault::Slow => assert_eq!(waited, SLOW_API, "a stuck daemon costs its timeout once"),
            _ => assert_eq!(waited, Duration::ZERO),
        }
        let devices: Vec<String> = mac_engine
            .directory
            .refresh()
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.device_id)
            .collect();
        assert_eq!(devices, ["dev-laptop"]);
    }
}

/// The control plane calling a peer offline and online again, over and over, never takes
/// down a link that carries packets, nor drops the peer from discovery.
#[tokio::test(start_paused = true)]
async fn online_flaps_never_take_a_live_link_down() {
    const SEED: u64 = 37;
    let _seed = report_seed("online_flaps_never_take_a_live_link_down", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
    let mac_engine = engine(&tailnet, mac, "dev-mac", SEED).await;
    let laptop_engine = engine(&tailnet, laptop, "dev-laptop", SEED + 1).await;
    let watching = [mac_engine.watch(), laptop_engine.watch()];
    mac_engine.directory.refresh().await.unwrap();
    let link = mac_engine.links.ready("dev-laptop").await.unwrap();
    let id = link.id();

    for flap in 0..24 {
        tailnet.set_online(laptop, flap % 2 == 1);
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            mac_engine.links.state("dev-laptop"),
            LinkState::Live,
            "flap {flap}"
        );
    }
    let links = mac_engine.links.links();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].id, id, "the same link throughout");
    assert_eq!(laptop_engine.links.links().len(), 1);
    assert_eq!(
        found(&mac_engine),
        ["dev-laptop"],
        "still found on the hint alone"
    );
    assert!(
        links[0].path.is_some() && links[0].peer_path.is_some(),
        "each end reports its path"
    );
    for w in watching {
        w.abort();
    }
}

/// Two engines that dial each other at the same moment keep one link, the one the smaller
/// device-id dialled, at both ends; it stays the only one.
#[tokio::test(start_paused = true)]
async fn two_engines_dialling_each_other_keep_the_smaller_ids_link() {
    for seed in [41_u64, 42, 43] {
        let _seed = report_seed(
            "two_engines_dialling_each_other_keep_the_smaller_ids_link",
            seed,
        );
        let tailnet = SimTailnet::new(
            Faults::over(onicode_net_sim::Link {
                delay: Duration::from_millis(3),
                ..onicode_net_sim::Link::CLEAR
            }),
            seed,
        );
        let mac = tailnet.add(NodeSpec::owned("mac", USER));
        let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
        let a = engine(&tailnet, mac, "dev-a", seed).await;
        let b = engine(&tailnet, laptop, "dev-b", seed + 1).await;
        a.directory.refresh().await.unwrap();
        b.directory.refresh().await.unwrap();

        let (from_a, from_b) = tokio::join!(a.links.ready("dev-b"), b.links.ready("dev-a"));
        assert!(from_a.is_ok(), "seed {seed}: {from_a:?}");
        // The larger id's own dial may be the one closed; it has the kept link either way.
        let _ = from_b;
        // Both keep the link dev-a dialled: dialled at dev-a, accepted at dev-b.
        let kept = |dialed: bool| move |e: &Engine| matches!(e.links.links().as_slice(), [one] if one.dialed == dialed);
        assert!(
            until(&a, Duration::from_secs(10), kept(true)).await,
            "seed {seed}: dev-a keeps its own dial"
        );
        assert!(
            until(&b, Duration::from_secs(10), kept(false)).await,
            "seed {seed}: dev-b keeps dev-a's dial"
        );
        let (at_a, at_b) = (a.links.links(), b.links.links());
        let ids = (at_a[0].id, at_b[0].id);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(
            (a.links.links()[0].id, b.links.links()[0].id),
            ids,
            "seed {seed}: no flapping after"
        );
    }
}

/// A peer that sleeps past the silence limit loses its link; the end that dialled it dials
/// again once it wakes, within the first minute's backoff.
#[tokio::test(start_paused = true)]
async fn a_link_is_redialled_by_its_dialer_after_the_peer_sleeps() {
    const SEED: u64 = 38;
    let _seed = report_seed(
        "a_link_is_redialled_by_its_dialer_after_the_peer_sleeps",
        SEED,
    );
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
    let mac_engine = engine(&tailnet, mac, "dev-mac", SEED).await;
    let laptop_engine = engine(&tailnet, laptop, "dev-laptop", SEED + 1).await;
    let watching = [mac_engine.watch(), laptop_engine.watch()];
    mac_engine.directory.refresh().await.unwrap();
    let first = mac_engine.links.ready("dev-laptop").await.unwrap().id();

    tailnet.set_asleep(laptop, true);
    let dropped = |e: &Engine| e.links.state("dev-laptop") != LinkState::Live;
    assert!(
        until(&mac_engine, Duration::from_secs(40), dropped).await,
        "silence ends the link"
    );
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert!(matches!(
        mac_engine.links.state("dev-laptop"),
        LinkState::Backoff { .. } | LinkState::Dialing
    ));

    tailnet.set_asleep(laptop, false);
    let woke = Instant::now();
    let back = |e: &Engine| e.links.state("dev-laptop") == LinkState::Live;
    assert!(
        until(&mac_engine, Duration::from_secs(10), back).await,
        "redialled after the wake"
    );
    let bound =
        onicode_net::redial::MAX + onicode_net::hello::HANDSHAKE_TIMEOUT + Duration::from_secs(1);
    assert!(woke.elapsed() <= bound, "{:?}", woke.elapsed());
    assert_ne!(mac_engine.links.links()[0].id, first);
    for w in watching {
        w.abort();
    }
}

/// Answers `Who` with the caller it was given, and streams `Count`'s numbers.
struct Service;

#[async_trait]
impl RpcService for Service {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            "Who" => RpcReply::value(&onicode_rpc::caller().record()),
            "Count" => {
                let n = params.as_u64().unwrap_or(0);
                Ok(RpcReply::Stream(Box::pin(futures::stream::iter(
                    (0..n).map(serde_json::Value::from),
                ))))
            }
            other => Err(RpcError::UnknownMethod(other.to_owned())),
        }
    }
}

/// The engine's RPC runs over the link, a stream per call, as the calling peer; a stream's
/// items arrive over its own stream; what only the device's own window may call is refused.
#[tokio::test(start_paused = true)]
async fn rpc_runs_over_the_link_as_the_calling_peer() {
    const SEED: u64 = 39;
    let _seed = report_seed("rpc_runs_over_the_link_as_the_calling_peer", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let mac = tailnet.add(NodeSpec::owned("mac", USER));
    let laptop = tailnet.add(NodeSpec::owned("laptop", USER));
    let mac_engine = engine(&tailnet, mac, "dev-mac", SEED).await;
    let served = LinksConfig {
        service: Some(Arc::new(Service)),
        ..LinksConfig::default()
    };
    let _laptop_engine = engine_with(
        &tailnet,
        tailnet.local_api(laptop),
        "dev-laptop",
        SEED + 1,
        served,
    )
    .await;
    mac_engine.directory.refresh().await.unwrap();

    let who = mac_engine
        .links
        .call("dev-laptop", "Who", serde_json::Value::Null)
        .await
        .unwrap();
    assert_eq!(
        who,
        serde_json::json!({ "peer": true, "device": "dev-mac" })
    );
    let mut count = mac_engine
        .links
        .subscribe("dev-laptop", "Count", serde_json::json!(3))
        .await
        .unwrap();
    let mut items = Vec::new();
    while let Some(item) = count.recv().await {
        items.push(item);
    }
    assert_eq!(
        items,
        [
            serde_json::json!(0),
            serde_json::json!(1),
            serde_json::json!(2)
        ]
    );
    let refused = mac_engine
        .links
        .call(
            "dev-laptop",
            onicode_rpc::methods::ACCESS_TOKEN,
            serde_json::Value::Null,
        )
        .await;
    assert!(
        matches!(refused, Err(PeerError::Rpc(RpcError::Failed(_)))),
        "{refused:?}"
    );
    assert_eq!(
        mac_engine.links.links().len(),
        1,
        "every call on the one link"
    );
}
