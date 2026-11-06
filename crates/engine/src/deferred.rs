//! An engine served before its profile is open. Identity (`EngineInfo`)
//! answers at once; every other call waits until the profile assembles.
//! Existing subscriptions attach to the assembled service without
//! reconnecting.
//!
//! Both owners of a desktop engine use it: the app's background engine
//! ([`crate::Engine::run_managed`]) and the app's in-process fallback.

use std::sync::Arc;

use async_trait::async_trait;
use onicode_proto::EngineInfo;
use onicode_rpc::{RpcError, RpcReply, RpcService, methods};
use tokio::sync::{Mutex, OnceCell, watch};
use tokio::task::JoinHandle;

use crate::{Engine, EngineConfig, EngineProfile, EngineRuntime, InstanceLock};

/// Where assembly stands.
#[derive(Clone, Debug)]
pub enum DeferredState {
    /// Assembling (or about to): data calls wait, briefly.
    Waiting,
    Ready,
    Failed(String),
}

/// Serves engine identity immediately, then holds data calls only until the
/// runtime assembles.
struct DeferredEngineRpc {
    engine_info: EngineInfo,
    state: watch::Receiver<DeferredState>,
    service: Arc<OnceCell<Arc<dyn RpcService>>>,
}

#[async_trait]
impl RpcService for DeferredEngineRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == methods::ENGINE_INFO {
            return RpcReply::value(&self.engine_info);
        }
        let mut state = self.state.clone();
        if method == methods::ENGINE_READY {
            return match wait(&mut state).await {
                Ok(()) => RpcReply::value(&serde_json::json!({ "ready": true })),
                Err(message) => Err(RpcError::Failed(message)),
            };
        }
        wait(&mut state).await.map_err(RpcError::Failed)?;
        let service = self
            .service
            .get()
            .ok_or_else(|| RpcError::Failed("engine became ready without an RPC service".into()))?;
        service.handle(method, params).await
    }
}

/// Wait for assembly's outcome.
pub async fn wait(state: &mut watch::Receiver<DeferredState>) -> Result<(), String> {
    loop {
        let current = { state.borrow().clone() };
        match current {
            DeferredState::Waiting => {}
            DeferredState::Ready => return Ok(()),
            DeferredState::Failed(message) => return Err(message),
        }
        state
            .changed()
            .await
            .map_err(|_| "engine assembly ended without a result".to_string())?;
    }
}

/// A desktop engine between "identity known" and "profile open".
pub struct DeferredEngine {
    /// The service to serve — over the in-memory transport, the IPC port, or
    /// both. Identity now; everything else once assembled.
    pub service: Arc<dyn RpcService>,
    pub engine_info: EngineInfo,
    /// The assembled runtime, once there is one.
    pub runtime: Arc<Mutex<Option<EngineRuntime>>>,
    state_tx: Option<watch::Sender<DeferredState>>,
    state: watch::Receiver<DeferredState>,
    assembled: Arc<OnceCell<Arc<dyn RpcService>>>,
    profile: Option<EngineProfile>,
}

impl DeferredEngine {
    /// Resolve identity and the profile's directories (no stores opened yet).
    pub async fn prepare(config: &EngineConfig) -> anyhow::Result<Self> {
        let profile = EngineProfile::local(&config.data_dir)?;
        let engine_info = Engine::engine_info(config)?;
        let (state_tx, state) = watch::channel(DeferredState::Waiting);
        let assembled = Arc::new(OnceCell::new());
        let service: Arc<dyn RpcService> = Arc::new(DeferredEngineRpc {
            engine_info: engine_info.clone(),
            state: state.clone(),
            service: assembled.clone(),
        });
        Ok(Self {
            service,
            engine_info,
            runtime: Arc::new(Mutex::new(None)),
            state_tx: Some(state_tx),
            state,
            assembled,
            profile: Some(profile),
        })
    }

    /// Watch assembly's outcome.
    pub fn state(&self) -> watch::Receiver<DeferredState> {
        self.state.clone()
    }

    /// Assemble in the background. `served_ipc_port` is the port this
    /// process actually serves (agents' injected MCP server dials it);
    /// `on_ready` runs once the runtime is open, before data calls are
    /// released. Call once.
    pub fn spawn_boot(
        &mut self,
        config: EngineConfig,
        lock: InstanceLock,
        served_ipc_port: Option<u16>,
        on_ready: impl FnOnce(&EngineRuntime) + Send + 'static,
    ) -> JoinHandle<()> {
        let state_tx = self.state_tx.take().expect("spawn_boot called twice");
        let profile = self.profile.take().expect("spawn_boot called twice");
        let runtime_slot = self.runtime.clone();
        let assembled = self.assembled.clone();
        // Agents' MCP servers get the scoped token, never the app's.
        let ipc_token_file = onicode_rpc::ipc_auth::agent_token_path(&config.data_dir);
        // The instance lock rides into the boot task and is consumed by
        // assembly: this process owns the data dir from the moment it
        // decided to host it.
        tokio::spawn(async move {
            match Engine::assemble_runtime_with_lock(&config, profile, lock).await {
                Ok(runtime) => {
                    let service: Arc<dyn RpcService> = runtime.core().rpc_service();
                    if let Some(port) = served_ipc_port {
                        runtime.core().sessions.set_ipc_port(port);
                        runtime.core().sessions.set_ipc_token_file(ipc_token_file);
                    }
                    on_ready(&runtime);
                    *runtime_slot.lock().await = Some(runtime);
                    if assembled.set(service).is_err() {
                        state_tx.send_replace(DeferredState::Failed(
                            "engine RPC service was assembled more than once".into(),
                        ));
                        return;
                    }
                    state_tx.send_replace(DeferredState::Ready);
                }
                Err(err) => {
                    tracing::error!(error = %err, "engine assembly failed");
                    state_tx.send_replace(DeferredState::Failed(format!("{err:#}")));
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn engine(state: watch::Receiver<DeferredState>) -> DeferredEngineRpc {
        DeferredEngineRpc {
            engine_info: EngineInfo {
                device_id: "device-1".into(),
                cursor_sdk_version: None,
                capabilities: Vec::new(),
                build: None,
            },
            state,
            service: Arc::new(OnceCell::new()),
        }
    }

    /// Identity answers at once; every other call waits until the profile
    /// assembles, and a failed assembly answers its reason.
    #[tokio::test]
    async fn calls_wait_while_the_profile_assembles() {
        let (state_tx, state) = watch::channel(DeferredState::Waiting);
        let rpc = engine(state);
        assert!(
            rpc.handle(methods::ENGINE_INFO, serde_json::json!({}))
                .await
                .is_ok()
        );
        for method in [
            methods::WATCH_CHATS,
            methods::LIST_HARNESSES,
            methods::MUTATE,
            methods::QUEUE_MESSAGE,
            methods::ENGINE_READY,
        ] {
            let waiting = tokio::time::timeout(
                Duration::from_millis(100),
                rpc.handle(method, serde_json::json!({})),
            )
            .await;
            assert!(waiting.is_err(), "{method} waits for the profile");
        }

        state_tx.send_replace(DeferredState::Failed("boom".into()));
        let failed = rpc
            .handle(methods::LIST_HARNESSES, serde_json::json!({}))
            .await;
        assert!(matches!(failed, Err(RpcError::Failed(m)) if m == "boom"));
    }
}
