//! The engine process's start, socket policy and stop, over real sockets in temporary data dirs.
use std::os::unix::fs::PermissionsExt;
use std::sync::Barrier;

use onicode_protocol::engine::{ResolveSession, StopEngine};
use onicode_protocol::session::Stop;
use onicode_protocol::{Outcome, SessionId};
use onicode_rpc::ipc_auth::{agent_token_path, read_token, token_path};
use onicode_rpc::{RpcClient, connect_unix};
use serde::Serialize;

use super::*;
use crate::testing::quiet;

fn open(dir: &Path) -> Result<Local, String> {
    Local::open(dir, Arc::new(ThreadSpawn), quiet())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn routed(body: impl Serialize) -> serde_json::Value {
    serde_json::to_value(Routed {
        engine: None,
        op: None,
        body,
    })
    .unwrap()
}

async fn dial(dir: &Path, agent: bool) -> RpcClient {
    let file = if agent {
        agent_token_path(dir)
    } else {
        token_path(dir)
    };
    let token = read_token(&file).unwrap().unwrap();
    connect_unix(&socket_path(dir), &token, agent.then_some("chat-1"))
        .await
        .unwrap()
}

async fn hello(client: &RpcClient, protocol: u32) -> Result<serde_json::Value, RpcError> {
    let body = Hello {
        protocol,
        version: "test".into(),
        capabilities: Vec::new(),
    };
    client.call(Hello::NAME, routed(body)).await
}

/// Binds an engine in `dir` and serves it on this runtime until the test ends or it is stopped.
fn serve(dir: &Path) -> tokio::task::JoinHandle<()> {
    let host = Host::bind(dir, open)
        .unwrap()
        .expect("no engine holds the lock");
    tokio::spawn(host.serve(std::future::pending()))
}

/// Two engines started at once over one data dir: exactly one takes the lock and serves, in
/// every seeded interleaving of their starts.
#[test]
fn two_engines_race_for_one_lock() {
    for round in 0..8_u64 {
        let dir = tempfile::tempdir().unwrap();
        let start = Arc::new(Barrier::new(2));
        let settled = Arc::new(Barrier::new(2));
        let racers: Vec<_> = (0..2_u64)
            .map(|racer| {
                let dir = dir.path().to_path_buf();
                let (start, settled) = (start.clone(), settled.clone());
                std::thread::spawn(move || {
                    start.wait();
                    std::thread::sleep(Duration::from_micros((round * 7 + racer * 13) % 5 * 100));
                    runtime().block_on(async {
                        let host = Host::bind(&dir, open).unwrap();
                        let bound = host.is_some();
                        // Both have tried before either lets go.
                        settled.wait();
                        bound
                    })
                })
            })
            .collect();
        let bound: Vec<bool> = racers
            .into_iter()
            .map(|racer| racer.join().unwrap())
            .collect();
        assert_eq!(
            bound.iter().filter(|bound| **bound).count(),
            1,
            "round {round}: {bound:?}"
        );
    }
}

/// A crashed engine leaves its socket file and a lock file naming it; the next engine takes
/// both over, and the data dir is the owner's alone.
#[test]
fn a_crashed_engines_socket_and_lock_are_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    prepare_dir(dir.path()).unwrap();
    drop(std::os::unix::net::UnixListener::bind(socket_path(dir.path())).unwrap());
    std::fs::write(
        dir.path().join(LOCK),
        r#"{"pid":999999,"exe":"/gone","built":null}"#,
    )
    .unwrap();
    assert!(InstanceLock::holder(dir.path()).is_none(), "no live holder");
    runtime().block_on(async {
        let _serving = serve(dir.path());
        let client = dial(dir.path(), false).await;
        hello(&client, PROTOCOL).await.unwrap();
        let row = client
            .call(ResolveSession::NAME, routed(ResolveSession { row: 1 }))
            .await
            .unwrap();
        assert_eq!(row, serde_json::Value::Null);
        let holder = InstanceLock::holder(dir.path()).unwrap();
        assert_eq!(holder.pid, std::process::id());
    });
    let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

/// An agent's token reaches the calls marked as an agent's and nothing else; the full token
/// reaches everything.
#[test]
fn an_agent_token_reaches_only_agent_calls() {
    let dir = tempfile::tempdir().unwrap();
    runtime().block_on(async {
        let _serving = serve(dir.path());
        let agent = dial(dir.path(), true).await;
        hello(&agent, PROTOCOL).await.unwrap();
        agent
            .call(EngineInfo::NAME, routed(EngineInfo {}))
            .await
            .unwrap();
        let denied = [
            (ResolveSession::NAME, routed(ResolveSession { row: 1 })),
            (
                Stop::NAME,
                routed(Stop {
                    session: SessionId::new(),
                }),
            ),
            (StopEngine::NAME, routed(StopEngine {})),
        ];
        for (method, params) in denied {
            match agent.call(method, params).await {
                Err(RpcError::Failed(reason)) => {
                    assert!(
                        reason.contains("isn't open to agents"),
                        "{method}: {reason}"
                    );
                }
                other => panic!("{method} reached the engine on an agent's token: {other:?}"),
            }
        }
        let full = dial(dir.path(), false).await;
        hello(&full, PROTOCOL).await.unwrap();
        full.call(ResolveSession::NAME, routed(ResolveSession { row: 1 }))
            .await
            .unwrap();
    });
}

/// Until a connection greets the engine with its protocol, only the frozen calls are answered.
#[test]
fn calls_wait_for_a_matching_hello() {
    let dir = tempfile::tempdir().unwrap();
    runtime().block_on(async {
        let _serving = serve(dir.path());
        let client = dial(dir.path(), false).await;
        let resolve = || routed(ResolveSession { row: 1 });
        let refused = client.call(ResolveSession::NAME, resolve()).await;
        assert!(matches!(&refused, Err(RpcError::Failed(reason)) if reason.contains("greet")));
        client
            .call(EngineInfo::NAME, routed(EngineInfo {}))
            .await
            .unwrap();
        let answer = hello(&client, PROTOCOL + 1).await.unwrap();
        let engine: Hello = serde_json::from_value(answer).unwrap();
        assert_eq!(engine.protocol, PROTOCOL, "the engine answers with its own");
        assert!(client.call(ResolveSession::NAME, resolve()).await.is_err());
        hello(&client, PROTOCOL).await.unwrap();
        client.call(ResolveSession::NAME, resolve()).await.unwrap();
    });
}

/// `StopEngine` ends the engine: the socket file goes before the lock does.
#[test]
fn stop_engine_removes_the_socket_and_lets_the_lock_go() {
    let dir = tempfile::tempdir().unwrap();
    runtime().block_on(async {
        let serving = serve(dir.path());
        let client = dial(dir.path(), false).await;
        hello(&client, PROTOCOL).await.unwrap();
        let outcome = client
            .call(StopEngine::NAME, routed(StopEngine {}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_value::<Outcome>(outcome).unwrap(),
            Outcome::Applied
        );
        tokio::time::timeout(Duration::from_secs(5), serving)
            .await
            .unwrap()
            .unwrap();
        assert!(!socket_path(dir.path()).exists());
        assert!(InstanceLock::acquire(dir.path()).unwrap().is_some());
        client.closed().await;
    });
}
