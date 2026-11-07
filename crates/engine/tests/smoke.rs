//! The opt-in smoke (engine.md, "Tests"): the real `onicode-engine` binary, started detached as
//! the app starts it and reached on its socket: `Hello`, a session created, a prompt sent to a
//! scripted provider, the turn finished, the engine stopped. Run it with nextest's
//! `--run-ignored only`; it opens no window and asks for nothing.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use onicode_engine::host::socket_path;
use onicode_engine::link::{Door, Link, SocketDoor, Timing};
use onicode_protocol::engine::{EngineInfo, Hello, StopEngine};
use onicode_protocol::session::{self, SessionEvent, SessionState, WatchSession};
use onicode_protocol::workspace::{CreateSession, NewSession};
use onicode_protocol::{Item, MessageId, OpId, Outcome, PROTOCOL, Routed, SessionId, Watch};
use onicode_rpc::{DialError, RpcClient, RpcSubscription};

/// A Codex that answers one turn with "Hello".
const PROVIDER: &str = r#"#!/usr/bin/env python3
import json, sys
def send(value):
    print(json.dumps(value), flush=True)
for line in sys.stdin:
    value = json.loads(line)
    method = value.get('method')
    if method == 'initialize':
        send({'id': value['id'], 'result': {}})
    elif method == 'thread/start':
        send({'id': value['id'], 'result': {'thread': {'id': 'thread'}}})
    elif method == 'turn/start':
        send({'id': value['id'], 'result': {'turn': {'id': 'turn'}}})
        send({'method': 'item/agentMessage/delta', 'params': {'itemId': 'answer', 'delta': 'Hello'}})
        send({'method': 'turn/completed', 'params': {'turn': {'status': 'completed'}}})
        break
"#;

/// The real binary, started as the app starts it, with the scripted provider and a home of its
/// own.
struct Smoke {
    socket: SocketDoor,
    data: PathBuf,
    home: PathBuf,
    provider: PathBuf,
}

impl Door for Smoke {
    fn dial(&self) -> BoxFuture<'static, Result<RpcClient, DialError>> {
        self.socket.dial()
    }

    fn launch(&self) -> Result<(), String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_onicode-engine"));
        command
            .arg(onicode_launch::DATA_DIR_ARG)
            .arg(&self.data)
            .env("HOME", &self.home)
            // Off the tailnet: a test never joins the real one.
            .env(onicode_engine::peers::PEERS_ENV, "0")
            .env("ONICODE_CODEX_BINARY", &self.provider);
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

fn watch(session: SessionId) -> serde_json::Value {
    let body = WatchSession {
        session,
        since: None,
        window_turns: 1,
    };
    serde_json::to_value(Routed {
        engine: None,
        op: None,
        body,
    })
    .unwrap()
}

/// The watch's next item; `None` when it ended.
async fn next(items: &mut RpcSubscription) -> Option<Item<SessionState, SessionEvent>> {
    let value = tokio::time::timeout(Duration::from_secs(30), items.recv())
        .await
        .expect("the engine goes quiet")?;
    Some(serde_json::from_value(value).unwrap())
}

#[test]
#[ignore = "opt-in smoke: the real engine binary and a scripted provider"]
fn the_engine_binary_serves_a_turn() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let provider = dir.path().join("codex");
    std::fs::write(&provider, PROVIDER).unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
    let engine = Path::new(env!("CARGO_BIN_EXE_onicode-engine"));
    let door = Smoke {
        socket: SocketDoor::with_engine(&data, engine),
        data: data.clone(),
        home,
        provider,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let runtime = tokio::runtime::Handle::current();
        let link = Link::new(Arc::new(door), runtime, Timing::default());
        let launched = Instant::now();
        link.client().await.unwrap();
        eprintln!("launch to Hello: {:?} (target 150 ms)", launched.elapsed());
        link.ask::<EngineInfo>(None, EngineInfo {}).await.unwrap();

        let session = SessionId::new();
        let created = link
            .ask::<CreateSession>(
                Some(OpId::new()),
                CreateSession {
                    session,
                    provider: "codex".into(),
                    cwd: dir.path().display().to_string(),
                    title: None,
                    start: NewSession::Bare,
                },
            )
            .await
            .unwrap();
        assert_eq!(created.outcome, Outcome::Applied);
        let mut items = link
            .subscribe(WatchSession::NAME, &watch(session))
            .await
            .unwrap();
        let Some(Item::Snapshot { mut state, .. }) = next(&mut items).await else {
            panic!("a watch starts from a snapshot");
        };
        let sent = link
            .ask::<session::Send>(
                Some(OpId::new()),
                session::Send {
                    session,
                    message: MessageId::new(),
                    text: "Say hello".into(),
                    attachments: Vec::new(),
                    turn: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(sent, Outcome::Applied);
        let status = loop {
            let events = match next(&mut items).await {
                Some(Item::Events { events, .. }) => events,
                Some(item) => panic!("an unexpected item mid-turn: {item:?}"),
                None => panic!("the watch ended mid-turn"),
            };
            let mut finished = None;
            for event in &events {
                onicode_engine::session::apply(&mut state, event);
                if let SessionEvent::Status {
                    status,
                    turn: Some(turn),
                } = event
                    && turn.finished
                {
                    finished = Some(status.clone());
                }
            }
            if let Some(status) = finished {
                break status;
            }
        };
        assert!(
            state.rows.iter().any(|row| row.text.contains("Hello")),
            "the provider's answer is in the transcript; the turn ended with {status:?}"
        );
        link.ask::<StopEngine>(None, StopEngine {}).await.unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket_path(&data).exists() {
        assert!(
            Instant::now() < deadline,
            "the stopped engine removes its socket"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
