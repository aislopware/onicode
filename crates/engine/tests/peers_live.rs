//! The opt-in two-Mac smoke (engine.md, "Tests"; transport.md: "a real tailnet run is only a smoke
//! check"): this Mac's real `onicode-engine` on the tailnet, and another Mac's engine found through
//! it. It measures what a person feels from a remote machine: a call forwarded one hop and back,
//! and a keystroke typed into a shell on the other Mac until its echo comes back.
//!
//! `scripts/dev/two-macs.sh HOST` starts the other Mac's engine over SSH and runs this with
//! `ONICODE_PEERS_LIVE=HOST` (its tailnet name, or a prefix of it) and nextest's
//! `--run-ignored only`. Without it the test says why it skipped. It opens no window, takes no
//! focus and asks for nothing; both engines use data and home folders of their own.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use onicode_engine::link::{Door, Link, SocketDoor, Timing};
use onicode_protocol::engine::{
    EngineDetails, EngineInfo, Hello, Machine, MachineKind, MachineLink, Machines, StopEngine,
    WatchMachines,
};
use onicode_protocol::terminal::{
    CloseTerminal, Input, OpenTerminal, TerminalEvent, TerminalInput, TerminalSize, ViewerId,
    WatchTerminal,
};
use onicode_protocol::{Call, EngineId, Item, OpId, PROTOCOL, Routed, TerminalId, Watch};
use onicode_rpc::{DialError, RpcClient, RpcSubscription};

/// The other Mac's tailnet name, or a prefix of it.
const LIVE_ENV: &str = "ONICODE_PEERS_LIVE";

/// Forwarded calls timed, after a few to warm the link.
const CALLS: usize = 200;

/// Keystrokes timed from send to echo.
const KEYS: usize = 60;

/// This Mac's engine, on the tailnet, with a home of its own.
struct Here {
    socket: SocketDoor,
    data: PathBuf,
    home: PathBuf,
}

impl Door for Here {
    fn dial(&self) -> BoxFuture<'static, Result<RpcClient, DialError>> {
        self.socket.dial()
    }

    fn launch(&self) -> Result<(), String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_onicode-engine"));
        command
            .arg(onicode_launch::DATA_DIR_ARG)
            .arg(&self.data)
            .env("HOME", &self.home)
            .env(onicode_engine::peers::PEERS_ENV, "1");
        let child = onicode_launch::spawn_detached(&mut command, None)
            .map_err(|error| error.to_string())?;
        onicode_launch::reap(child);
        Ok(())
    }

    fn current(&self, hello: &Hello) -> bool {
        hello.protocol == PROTOCOL
    }

    fn retire(&self) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }
}

fn routed<T: serde::Serialize>(
    engine: Option<&EngineId>,
    op: Option<OpId>,
    body: T,
) -> serde_json::Value {
    serde_json::to_value(Routed {
        engine: engine.cloned(),
        op,
        body,
    })
    .unwrap()
}

/// `body` on the engine `to`, one hop away.
async fn ask<C: Call>(
    link: &Arc<Link>,
    to: &EngineId,
    op: Option<OpId>,
    body: C::Params,
) -> Result<C::Reply, String> {
    let reply = link.call(C::NAME, &routed(Some(to), op, body)).await?;
    serde_json::from_value(reply).map_err(|error| error.to_string())
}

async fn next<W: Watch>(items: &mut RpcSubscription, within: Duration) -> Item<W::State, W::Event>
where
    W::State: serde::de::DeserializeOwned,
    W::Event: serde::de::DeserializeOwned,
{
    let value = tokio::time::timeout(within, items.recv())
        .await
        .expect("the watch goes quiet")
        .expect("the watch ends");
    serde_json::from_value(value).unwrap()
}

/// The other Mac's engine, once this engine links to it.
async fn find(link: &Arc<Link>, name: &str) -> Machine {
    let mut items = link
        .subscribe(
            WatchMachines::NAME,
            &routed(None, None, WatchMachines { since: None }),
        )
        .await
        .unwrap();
    let mut machines = Machines::default();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match next::<WatchMachines>(&mut items, Duration::from_secs(60)).await {
            Item::Snapshot { state, .. } => machines = state,
            Item::Events { events, .. } => {
                for event in &events {
                    machines.apply(event);
                }
            }
            other => panic!("an unexpected machines item: {other:?}"),
        }
        let found = machines
            .machines
            .iter()
            .find(|machine| machine.kind == MachineKind::Engine && machine.name.starts_with(name));
        if let Some(machine) = found {
            // A found engine is dialled when something is sent to it.
            if matches!(machine.link, MachineLink::Live | MachineLink::Found) {
                return machine.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "no engine named {name}… on the tailnet: {:#?}",
            machines.machines
        );
    }
}

/// The 50th, 90th and 99th percentiles and the worst of `samples`, in milliseconds.
fn spread(samples: &mut [Duration]) -> String {
    samples.sort_unstable();
    let at = |q: f64| {
        let index = ((samples.len() as f64 - 1.) * q).round() as usize;
        samples[index].as_secs_f64() * 1e3
    };
    format!(
        "p50 {:.2} ms, p90 {:.2} ms, p99 {:.2} ms, worst {:.2} ms",
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.)
    )
}

/// Whether `bytes` holds the echo of `mark`.
fn echoes(bytes: &[u8], mark: u8) -> bool {
    bytes.contains(&mark)
}

#[test]
#[ignore = "opt-in two-Mac smoke: a real engine on another Mac of the tailnet"]
fn another_macs_engine_answers_and_echoes_like_this_one() {
    let Some(name) = std::env::var(LIVE_ENV).ok().filter(|name| !name.is_empty()) else {
        eprintln!(
            "skipped: set {LIVE_ENV} to the other Mac's tailnet name (scripts/dev/two-macs.sh)"
        );
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let engine = Path::new(env!("CARGO_BIN_EXE_onicode-engine"));
    let door = Here {
        socket: SocketDoor::with_engine(&data, engine),
        data,
        home,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let handle = tokio::runtime::Handle::current();
        let link = Link::new(Arc::new(door), handle, Timing::default());
        link.client().await.unwrap();

        let machine = find(&link, &name).await;
        let there = machine.engine.clone();
        let details: EngineDetails = ask::<EngineInfo>(&link, &there, None, EngineInfo {})
            .await
            .expect("the other Mac's engine answers a forwarded call");
        assert_eq!(
            details.protocol, PROTOCOL,
            "both Macs run this build's protocol"
        );
        eprintln!(
            "peer {} ({}), path {:?}, tailnet rtt {:?} µs",
            machine.name, details.version, machine.path, machine.rtt_us
        );

        // A call forwarded one hop and back, as every remote action is.
        for _ in 0..10 {
            ask::<EngineInfo>(&link, &there, None, EngineInfo {})
                .await
                .unwrap();
        }
        let mut calls = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let sent = Instant::now();
            ask::<EngineInfo>(&link, &there, None, EngineInfo {})
                .await
                .unwrap();
            calls.push(sent.elapsed());
        }
        let mut local = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let sent = Instant::now();
            link.ask::<EngineInfo>(None, EngineInfo {}).await.unwrap();
            local.push(sent.elapsed());
        }
        eprintln!("forwarded call: {}", spread(&mut calls));
        eprintln!("local call:     {}", spread(&mut local));

        // A keystroke into a shell on the other Mac until its echo comes back.
        let terminal = TerminalId::new();
        let viewer = ViewerId::new();
        ask::<OpenTerminal>(
            &link,
            &there,
            Some(OpId::new()),
            OpenTerminal {
                terminal,
                viewer,
                cwd: "/tmp".into(),
                size: TerminalSize {
                    cols: 100,
                    rows: 30,
                    cell_width: 8,
                    cell_height: 16,
                },
                colors: Default::default(),
            },
        )
        .await
        .expect("a shell opens on the other Mac");
        let mut output = link
            .subscribe(
                WatchTerminal::NAME,
                &routed(
                    Some(&there),
                    None,
                    WatchTerminal {
                        terminal,
                        viewer,
                        since: None,
                    },
                ),
            )
            .await
            .unwrap();
        let Item::Snapshot { .. } =
            next::<WatchTerminal>(&mut output, Duration::from_secs(10)).await
        else {
            panic!("a terminal watch starts from a snapshot");
        };
        // Let the shell print its prompt before timing.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        while tokio::time::timeout(Duration::from_millis(100), output.recv())
            .await
            .is_ok()
        {}
        let mut echo = Vec::with_capacity(KEYS);
        for index in 0..KEYS {
            // Letters cycle so each echo is told from the one before.
            let mark = b'a' + u8::try_from(index % 26).unwrap();
            let sent = Instant::now();
            ask::<TerminalInput>(
                &link,
                &there,
                Some(OpId::new()),
                TerminalInput {
                    terminal,
                    viewer,
                    input: Input::Text(char::from(mark).to_string()),
                },
            )
            .await
            .unwrap();
            loop {
                match next::<WatchTerminal>(&mut output, Duration::from_secs(5)).await {
                    Item::Events { events, .. }
                        if events.iter().any(|event| {
                            matches!(event, TerminalEvent::Output(bytes) if echoes(&bytes.0, mark))
                        }) =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
            echo.push(sent.elapsed());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        eprintln!("keystroke echo: {}", spread(&mut echo));
        let _ = ask::<CloseTerminal>(&link, &there, Some(OpId::new()), CloseTerminal { terminal })
            .await;
        let _ = link.ask::<StopEngine>(None, StopEngine {}).await;

        let worst_typical = echo[echo.len() / 2];
        assert!(
            worst_typical < Duration::from_millis(100),
            "a remote keystroke echoes in under 100 ms at the median"
        );
    });
}
