//! `onicode status` and `onicode sync`: the engine on this device and its sync with its peers,
//! over its local IPC.

use std::path::Path;

use onicode_engine::{EngineConfig, InstanceLock};

/// `onicode status`: where the engine keeps its data, and whether it runs and listens.
pub async fn status_cli(config: &EngineConfig) -> anyhow::Result<()> {
    println!("Data dir: {}", config.data_dir.display());
    match InstanceLock::holder(&config.data_dir) {
        Some(pid) => println!("Engine:   running (pid {pid})"),
        None => println!("Engine:   not running"),
    }
    match engine_info(config.ipc_port, &config.data_dir).await {
        Some(info) => {
            println!("IPC:      listening on 127.0.0.1:{}", config.ipc_port);
            println!("Device:   {}", info.device_id);
        }
        None => println!("IPC:      not listening on 127.0.0.1:{}", config.ipc_port),
    }
    println!(
        "Updates:  {}",
        if config.update_feed.is_empty() {
            "off (no ONICODE_UPDATE_FEED)"
        } else {
            config.update_feed.as_str()
        }
    );
    Ok(())
}

async fn engine_info(ipc_port: u16, data_dir: &Path) -> Option<onicode_engine::EngineInfo> {
    let client = onicode_rpc::connect_ipc(ipc_port, Some(data_dir))
        .await
        .ok()?;
    let value = client
        .call(onicode_rpc::methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .ok()?;
    serde_json::from_value(value).ok()
}

/// `onicode sync`: dial the running engine's IPC and print how its registry and each open chat
/// stand with their peers.
pub async fn sync_cli(ipc_port: u16, data_dir: &Path) -> anyhow::Result<()> {
    let client = onicode_rpc::connect_ipc(ipc_port, Some(data_dir))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "no engine listening on 127.0.0.1:{ipc_port} ({e}) — is onicode running?"
            )
        })?;
    let status = client
        .call(onicode_rpc::methods::SYNC_STATUS, serde_json::json!({}))
        .await
        .map_err(|e| anyhow::anyhow!("SyncStatus failed: {e}"))?;
    let text = |value: &serde_json::Value, key: &str| -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_owned()
    };
    println!("Device:    {}", text(&status, "deviceId"));
    let workspace = status.get("workspace").cloned().unwrap_or_default();
    let peers: Vec<&str> = workspace
        .get("caughtUpWith")
        .and_then(|v| v.as_array())
        .map(|peers| peers.iter().filter_map(|p| p.as_str()).collect())
        .unwrap_or_default();
    println!(
        "Workspace: {}{}",
        if workspace.get("synced").and_then(|v| v.as_bool()) == Some(true) {
            "synced"
        } else {
            "NOT SYNCED"
        },
        if peers.is_empty() {
            String::new()
        } else {
            format!(" · caught up with {}", peers.join(", "))
        }
    );
    let chats = status
        .get("chats")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if chats.is_empty() {
        println!("Chats:     none open");
    }
    for chat in &chats {
        let id = text(chat, "chatId");
        let host = chat
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown host");
        let live = chat.get("hostLive").and_then(|v| v.as_bool()) == Some(true);
        let unacked = chat
            .get("unackedWrites")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        println!(
            "Chat {}: {} · host {host}{} · unacked {unacked}",
            id.get(..8).unwrap_or(&id),
            text(chat, "state"),
            if live { " (live)" } else { "" },
        );
    }
    Ok(())
}
