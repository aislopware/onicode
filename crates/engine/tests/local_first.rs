//! An engine boots on its own data alone: no account, no service, no tailnet.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use onicode_engine::{Engine, EngineConfig, EngineInfo, EngineProfile, HarnessId};
use onicode_rpc::{connect_ipc, connect_ws, memory_client, methods};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(data_dir: &std::path::Path, update_feed: String) -> EngineConfig {
    EngineConfig {
        data_dir: data_dir.to_path_buf(),
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        update_feed,
        peers: false,
    }
}

async fn rejecting_feed() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).await;
                seen.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"error":"revoked"}"#;
                let response = format!(
                    "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), requests, task)
}

#[tokio::test]
async fn a_boot_serves_its_own_data_without_any_account() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), "http://127.0.0.1:1".into());
    let profile = EngineProfile::local(dir.path()).unwrap();
    let runtime = Engine::assemble_runtime(&config, profile).await.unwrap();
    let client = memory_client(runtime.core().rpc_service());
    let info: EngineInfo = client
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(info.device_id, runtime.core().device_id);
    assert!(
        client
            .call(methods::LIST_HARNESSES, serde_json::json!({}))
            .await
            .unwrap()
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(dir.path().join("profiles/local").is_dir());
    // `peers: false` keeps it off the tailnet.
    assert!(runtime.core().links().is_none());
    runtime.shutdown().await;
}

#[tokio::test]
async fn the_runtime_checks_public_releases_and_stops_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let (feed, requests, feed_task) = rejecting_feed().await;
    let config = config(dir.path(), feed);
    let profile = EngineProfile::local(dir.path()).unwrap();

    let runtime = Engine::assemble_runtime(&config, profile).await.unwrap();

    assert!(runtime.core().links().is_none());
    let updater = runtime
        .core()
        .updater()
        .expect("the runtime starts the public release checker");
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    updater.check_now();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while requests.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("release checker requested manifest.json and latest.txt");

    runtime.shutdown().await;
    let stopped_at = requests.load(Ordering::SeqCst);
    updater.check_now();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(requests.load(Ordering::SeqCst), stopped_at);
    feed_task.abort();
}

#[tokio::test]
async fn headless_stop_rpc_drains_the_daemon_and_releases_ipc() {
    let dir = tempfile::tempdir().unwrap();
    let port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut engine_config = config(dir.path(), "http://127.0.0.1:1".into());
    engine_config.ipc_port = port;
    let daemon = tokio::spawn(Engine::new(engine_config).run());

    let client = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(client) = connect_ipc(port, Some(dir.path())).await {
                break client;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("headless IPC did not start");

    // The daemon published an owner-only bearer and refuses dials without it.
    let token_file = onicode_rpc::ipc_auth::token_path(dir.path());
    assert!(
        token_file.is_file(),
        "headless start must publish its IPC token"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "IPC token must be owner-only");
    }
    let url = format!("ws://127.0.0.1:{port}");
    assert!(
        connect_ws(&url, None).await.is_err(),
        "a dial without the token must be refused"
    );
    assert!(
        connect_ws(&url, Some("0000")).await.is_err(),
        "a dial with a wrong token must be refused"
    );

    assert_eq!(
        client
            .call(methods::STOP_ENGINE, serde_json::json!({}))
            .await
            .unwrap(),
        serde_json::json!({ "ok": true })
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), daemon)
        .await
        .expect("headless engine did not stop")
        .expect("headless task panicked")
        .expect("headless shutdown failed");

    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("headless IPC port remained occupied after shutdown");
}
