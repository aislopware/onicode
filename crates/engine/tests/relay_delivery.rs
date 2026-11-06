//! Peer-relay delivery fallback (durable-by-design §Phase 3): engine A queues
//! a command for a chat hosted on engine B, A's chat2 rows can NEVER reach an
//! edge (none is configured — the 03:45 incident shape, rows dark while the
//! peer link lives), so after the rows grace A relay-forwards the entry over
//! the tailnet link. B claims the client-minted id in its processed
//! ledger before executing — so when the doc row later "arrives" (simulated
//! by writing the same entry into B's doc), the drain dedupes it to a no-op:
//! exactly-once across both roads.

#[path = "support/tailnet.rs"]
mod tailnet;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_doc::{MessageRole, MessageStatus, SessionCommandPayload};
use onicode_engine::{EngineCore, HarnessRegistry};
use onicode_harness::{Harness, HarnessError, RunControls};
use onicode_proto::{
    AgentEvent, Device, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};
use onicode_rpc::methods;

const CHAT: &str = "chat-relay-fallback";

struct InstantHarness;

#[async_trait]
impl Harness for InstantHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "Instant"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }

    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::iter([
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "instant-1".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-relay".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "relayed reply".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("hs-relay".into()),
            }),
        ])
        .boxed())
    }
}

fn registry() -> Arc<HarnessRegistry> {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(InstantHarness));
    Arc::new(registry)
}

fn assemble(dir: &std::path::Path, device_id: &str) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), device_id).expect("write device id");
    EngineCore::assemble(dir, registry(), HarnessId::Mock, None).expect("engine assembles")
}

fn complete_assistant_count(core: &EngineCore) -> usize {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_dark_command_delivers_over_the_peer_relay_exactly_once() {
    let dirs = tempfile::tempdir().expect("tempdir");

    // Engine B hosts the chat.
    let core_b = assemble(&dirs.path().join("b"), "device-b");

    // Engine A reaches B over the tailnet — and has NO edge, so its
    // chat2 rows can never flush (the rows-dark half of the incident shape).
    let core_a = assemble(&dirs.path().join("a"), "device-a");
    let _mesh = tailnet::mesh(&[&core_a, &core_b], 60).await;

    // A knows the chat is hosted on B (local registry writes).
    core_a.workspace.upsert_device_row(&Device {
        id: "device-b".into(),
        name: "b".into(),
        platform: "linux".into(),
        last_seen_at: Some(chrono::Utc::now()),
        created_at: None,
        version: Some(env!("CARGO_PKG_VERSION").into()),
        cursor_sdk_version: None,
        capabilities: onicode_proto::capabilities::current(),
    });
    let client_a = onicode_rpc::memory_client(core_a.rpc_service());
    client_a
        .call(
            methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": "device-b" }),
        )
        .await
        .expect("createChat on A");
    core_b
        .workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("pre-title on B (no auto-title harness run)");

    // The send: a durable local write on A. Rows go nowhere; the escort's
    // grace elapses; the entry crosses the peer link instead.
    let command = serde_json::to_value(SessionCommandPayload::Run {
        request: Box::new(RunRequest {
            mcp: None,
            prompt: "over the relay".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "~".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            access: Default::default(),
            plan: false,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            env: Default::default(),
        }),
        message_id: "msg-relay-1".into(),
    })
    .expect("command json");
    client_a
        .call(
            methods::QUEUE_COMMAND,
            serde_json::json!({ "chatId": CHAT, "command": command }),
        )
        .await
        .expect("queue on A");

    // B executes it — allow the 10s rows grace plus relay dial time.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if complete_assistant_count(&core_b) == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "relayed command never executed on B"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let entries = core_b
        .doc_host
        .open(CHAT)
        .expect("open on B")
        .doc()
        .read_entries()
        .expect("read entries");
    assert!(
        entries
            .iter()
            .any(|e| e.id == "msg-relay-1" && e.role == MessageRole::User),
        "B persisted the user message under the client-minted id"
    );

    // Exactly-once: the doc row "arrives" later over chat2 sync — simulate by
    // writing A's exact entry into B's doc, which kicks B's drain. The
    // processed ledger must dedupe it to a no-op.
    let entry = core_a
        .doc_host
        .open(CHAT)
        .expect("open on A")
        .doc()
        .read_commands()
        .expect("read A's commands")
        .into_iter()
        .next()
        .expect("A queued exactly one command");
    let handle_b = core_b.doc_host.open(CHAT).expect("open on B");
    handle_b
        .doc()
        .queue_command(&entry)
        .expect("simulate the synced doc row");
    // A second relay attempt (a retrying sender) must also dedupe.
    let dup = core_b
        .doc_host
        .ingest_relayed_command(CHAT, entry)
        .await
        .expect("duplicate relay accepted");
    assert_eq!(dup, "duplicate");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        complete_assistant_count(&core_b),
        1,
        "the doc row + a duplicate relay must not double-run the command"
    );

    core_a.shutdown().await;
    core_b.shutdown().await;
}
