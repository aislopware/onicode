//! One device identity, however many boots and viewports race for it.

use std::path::Path;
use std::sync::{Arc, Barrier};

use onicode_engine::{
    Engine, EngineConfig, EngineCore, EngineProfile, HarnessId, default_registry,
};

fn config(data_dir: &Path) -> EngineConfig {
    EngineConfig {
        data_dir: data_dir.to_path_buf(),
        ipc_port: 0,
        default_harness: HarnessId::Mock,
        update_feed: "http://127.0.0.1:1".into(),
        peers: false,
    }
}

fn assemble(profile: EngineProfile) -> EngineCore {
    EngineCore::assemble_with_profile(profile, Arc::new(default_registry()), HarnessId::Mock)
        .expect("assemble profile")
}

async fn shutdown(core: EngineCore) {
    core.shutdown().await;
    drop(core);
}

fn concurrent_engine_info(
    config: Arc<EngineConfig>,
    workers: usize,
) -> std::collections::HashSet<String> {
    let barrier = Arc::new(Barrier::new(workers));
    (0..workers)
        .map(|_| {
            let config = config.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Engine::engine_info(&config)
                    .expect("resolve concurrent engine info")
                    .device_id
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|call| call.join().expect("engine-info worker"))
        .collect()
}

#[tokio::test]
async fn concurrent_engine_info_and_runtime_share_one_device_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Arc::new(config(dir.path()));
    let announced = concurrent_engine_info(config, 32);

    assert_eq!(announced.len(), 1, "every viewport announces one identity");
    let announced = announced.into_iter().next().expect("announced id");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("device-id"))
            .expect("persisted device id")
            .trim(),
        announced
    );

    let core = assemble(EngineProfile::local(dir.path()).expect("local profile"));
    assert_eq!(
        core.device_id, announced,
        "the assembled runtime must use the identity already announced"
    );
    shutdown(core).await;
}

#[tokio::test]
async fn empty_legacy_device_identity_is_repaired_once_for_all_boots() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("device-id"), b"").expect("seed truncated identity");
    let config = Arc::new(config(dir.path()));

    let announced = concurrent_engine_info(config, 32);
    assert_eq!(
        announced.len(),
        1,
        "legacy repair must publish one identity"
    );
    let announced = announced.into_iter().next().expect("repaired id");
    assert!(!announced.trim().is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("device-id"))
            .expect("repaired device id")
            .trim(),
        announced
    );

    let core = assemble(EngineProfile::local(dir.path()).expect("local profile"));
    assert_eq!(core.device_id, announced);
    shutdown(core).await;
}
