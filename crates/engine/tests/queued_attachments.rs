//! Queued attachments (send-is-a-local-write): a Run command carrying
//! `pending://` refs defers — Pending, unprocessed — until the bytes land in
//! this device's uploads dir, then executes with the refs rewritten to
//! absolute paths (2026-08-19 incident: attachment staging used to be a
//! blocking pre-step in FRONT of QueueCommand, so a dead peer link killed the
//! send instead of queueing it).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_doc::{
    MessageRole, MessageStatus, QueuedMessage, SessionCommandPayload, SessionCommandStatus,
    SessionMessageEntry,
};
use onicode_engine::{EngineCore, HarnessRegistry};
use onicode_harness::{Harness, HarnessError, RunControls};
use onicode_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};

const CHAT: &str = "chat-queued-att";

/// Every run the harness was handed: `(prompt, attachments)`. Recorded before
/// the harness's own checks, so a test can assert on them even when a failed
/// check is swallowed inside the run task.
type Runs = Arc<Mutex<Vec<(String, Vec<String>)>>>;

#[derive(Default)]
struct AckHarness {
    runs: Runs,
}

#[async_trait]
impl Harness for AckHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "Ack"
    }

    fn supports_steering(&self) -> bool {
        false
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }

    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.runs
            .lock()
            .unwrap()
            .push((request.prompt.clone(), request.attachments.clone()));
        // The harness must see ordinary local files, never pending refs.
        assert!(
            !request.prompt.contains("pending://"),
            "prompt reached the harness with unresolved pending refs: {}",
            request.prompt
        );
        for path in &request.attachments {
            assert!(
                std::path::Path::new(path).is_file(),
                "attachment path not a real file: {path}"
            );
        }
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "sess-qa".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta { text: "ack".into() }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-qa".into()),
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

fn complete_assistant_count(core: &EngineCore) -> usize {
    entries(core)
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

/// An engine whose only harness is an [`AckHarness`], plus the runs it records.
fn engine(dir: &std::path::Path) -> (EngineCore, Runs) {
    let harness = AckHarness::default();
    let runs = harness.runs.clone();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness));
    let core = EngineCore::assemble(&dir.join("data"), Arc::new(registry), HarnessId::Mock)
        .expect("engine core assembles");
    (core, runs)
}

/// A pre-titled chat with a cwd, so a queued row has a run config to send with.
async fn create_chat(core: &EngineCore, client: &onicode_rpc::RpcClient) {
    client
        .call(
            onicode_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": core.device_id }),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
    core.workspace
        .set_chat_cwd(CHAT, "~")
        .expect("set chat cwd");
}

/// Land an upload's bytes on this engine: the two RPCs an engine-to-engine
/// transfer ends in. The commit kicks the drains.
async fn land_upload(client: &onicode_rpc::RpcClient, upload_id: &str, file_name: &str) {
    client
        .call(
            onicode_rpc::methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId": upload_id, "seq": 0, "data": BASE64.encode(b"png-bytes"),
            }),
        )
        .await
        .expect("upload chunk");
    client
        .call(
            onicode_rpc::methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": upload_id, "fileName": file_name }),
        )
        .await
        .expect("upload commit");
}

/// Wait for the one turn a queued row starts. Fails with the state that tells
/// the stuck stage apart: a row still queued means no drain took it (paused
/// queue, lost kick); an empty queue means it was dispatched and the turn
/// never completed.
async fn wait_for_queued_turn(core: &EngineCore) {
    let handle = core.doc_host.open(CHAT).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while complete_assistant_count(core) != 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for queued row to send once its bytes land: queue {:?}, \
             entries {:?}",
            handle.doc().read_queue(),
            entries(core)
                .iter()
                .map(|e| (e.role, e.status))
                .collect::<Vec<_>>(),
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

/// The harness ran exactly once, on the committed file named `committed`, and
/// never saw a `pending://` ref. An early send fails here whatever the timing:
/// it reaches the harness without the attachment.
fn assert_one_run_on(runs: &Runs, committed: &str) {
    let runs = runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 1, "exactly one run: {runs:?}");
    let (prompt, attachments) = &runs[0];
    assert!(
        !prompt.contains("pending://"),
        "prompt leaked a ref: {prompt}"
    );
    assert_eq!(
        attachments.len(),
        1,
        "the run carries its file: {attachments:?}"
    );
    assert!(
        attachments[0].ends_with(committed) && std::path::Path::new(&attachments[0]).is_file(),
        "the run names the committed file {committed}: {attachments:?}"
    );
}

fn run_payload(message_id: &str, pending_ref: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: Box::new(RunRequest {
            mcp: None,
            prompt: format!(
                "look at this\n\nAttached images (local files — open them to view):\n- {pending_ref}"
            ),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "~".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            access: Default::default(),
            plan: false,
            attachments: vec![pending_ref.to_string()],
            worktree: None,
            resume: None,
            env: Default::default(),
        }),
        message_id: message_id.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn run_defers_until_attachment_bytes_land_then_executes_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, _runs) = engine(tmp.path());

    let client = onicode_rpc::memory_client(core.rpc_service());
    client
        .call(
            onicode_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": core.device_id }),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");

    // Queue a Run whose attachment bytes have NOT landed yet: it must sit
    // Pending — deferred, unprocessed, no turn — instead of running without
    // its image (or worse, handing the harness a dangling ref).
    let pending_ref = "pending://att-1/photo one.png";
    core.doc_host
        .queue_command(CHAT, run_payload("msg-qa-1", pending_ref))
        .expect("queue run command");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        complete_assistant_count(&core),
        0,
        "run must defer while attachment bytes are in transit"
    );
    let commands = core
        .doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].status,
        SessionCommandStatus::Pending,
        "deferred command must stay Pending (not Rejected/Expired)"
    );

    // The bytes land (the engine-to-engine transfer ends in exactly these two
    // RPCs on the host): UploadChunk + UploadCommit under the SAME identity.
    // The commit handler kicks the drains — no timers involved.
    client
        .call(
            onicode_rpc::methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId": "att-1", "seq": 0, "data": BASE64.encode(b"png-bytes"),
            }),
        )
        .await
        .expect("upload chunk");
    client
        .call(
            onicode_rpc::methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": "att-1", "fileName": "photo one.png" }),
        )
        .await
        .expect("upload commit");

    wait_for(
        || complete_assistant_count(&core) == 1,
        "deferred run to execute",
    )
    .await;

    // The persisted user entry carries the rewritten ABSOLUTE path (legacy
    // transcript shape — old clients render it as always), not the ref.
    let user_text = entries(&core)
        .iter()
        .find(|e| e.role == MessageRole::User)
        .and_then(|e| {
            e.parts.iter().find_map(|p| match p {
                onicode_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .expect("user entry persisted");
    assert!(
        !user_text.contains("pending://"),
        "persisted text must not leak pending refs: {user_text}"
    );
    assert!(
        user_text.contains("att-1-photo_one.png"),
        "persisted text names the committed file: {user_text}"
    );

    core.shutdown().await;
}

/// A file that is not an image rides the same deferred-run transport: it waits
/// for its bytes, runs with the committed path, and reads back as opaque bytes
/// so a queued message can be restored for editing.
#[tokio::test(flavor = "multi_thread")]
async fn run_with_a_non_image_file_defers_executes_and_reads_back() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, _runs) = engine(tmp.path());
    let client = onicode_rpc::memory_client(core.rpc_service());
    client
        .call(
            onicode_rpc::methods::MUTATE,
            serde_json::json!({ "op": "createChat", "chatId": CHAT, "deviceId": core.device_id }),
        )
        .await
        .expect("createChat");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");

    core.doc_host
        .queue_command(CHAT, run_payload("msg-file-1", "pending://att-2/notes.md"))
        .expect("queue run command");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        complete_assistant_count(&core),
        0,
        "run must defer while the file is in transit"
    );

    client
        .call(
            onicode_rpc::methods::UPLOAD_CHUNK,
            serde_json::json!({ "uploadId": "att-2", "seq": 0, "data": BASE64.encode(b"# notes") }),
        )
        .await
        .expect("upload chunk");
    client
        .call(
            onicode_rpc::methods::UPLOAD_COMMIT,
            serde_json::json!({ "uploadId": "att-2", "fileName": "notes.md" }),
        )
        .await
        .expect("upload commit");
    wait_for(
        || complete_assistant_count(&core) == 1,
        "deferred run to execute",
    )
    .await;

    let user_text = entries(&core)
        .iter()
        .find(|e| e.role == MessageRole::User)
        .and_then(|e| {
            e.parts.iter().find_map(|p| match p {
                onicode_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .expect("user entry persisted");
    assert!(!user_text.contains("pending://"), "{user_text}");
    let committed = user_text
        .lines()
        .find(|line| line.ends_with("att-2-notes.md"))
        .and_then(|line| line.strip_prefix("- "))
        .unwrap_or_else(|| panic!("committed path missing: {user_text}"));

    // Restoring the queued message for editing reads the file back.
    let chunk = client
        .call(
            onicode_rpc::methods::READ_ATTACHMENT_CHUNK,
            serde_json::json!({ "path": committed, "offset": 0 }),
        )
        .await
        .expect("read back the uploaded file");
    assert_eq!(chunk["mimeType"], "application/octet-stream");
    assert_eq!(chunk["data"], BASE64.encode(b"# notes"));

    core.shutdown().await;
}

/// Mobile sends made while a turn runs park on the shared queue with
/// `pending://` refs (the bytes chase them over the peer link). The queue
/// drain must wait for those bytes and hand the harness the committed local
/// files, exactly like a Run command — never the raw refs.
#[tokio::test(flavor = "multi_thread")]
async fn queued_row_waits_for_attachment_bytes_then_sends_resolved_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, runs) = engine(tmp.path());
    let client = onicode_rpc::memory_client(core.rpc_service());
    create_chat(&core, &client).await;
    let handle = core.doc_host.open(CHAT).unwrap();

    // A clean-text row (attachments ride the row, not the text) whose bytes
    // have not landed yet: the idle chat must NOT send it.
    let pending_ref = "pending://att-q1/queued shot.png";
    core.doc_host
        .queue_message_with_behavior(CHAT, "what is this", vec![pending_ref.into()], false)
        .expect("queue row");
    // Drains on a chat take turns on its drain lock, so once this one returns
    // a drain has judged the row with its bytes absent — no sleep to outwait.
    core.doc_host.drain_queue(&handle).await;
    assert!(
        runs.lock().unwrap().is_empty(),
        "a queued row must wait while its attachment bytes are in transit"
    );
    let queue = handle.doc().read_queue().unwrap();
    assert_eq!(queue.len(), 1, "row stays queued: {queue:?}");
    assert_eq!(queue[0].attachments, vec![pending_ref.to_string()]);

    land_upload(&client, "att-q1", "queued shot.png").await;
    wait_for_queued_turn(&core).await;
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert_one_run_on(&runs, "att-q1-queued_shot.png");
    let user_text = entries(&core)
        .iter()
        .find(|e| e.role == MessageRole::User)
        .and_then(|e| {
            e.parts.iter().find_map(|p| match p {
                onicode_doc::MessagePart::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        })
        .expect("user entry persisted");
    assert!(
        !user_text.contains("pending://"),
        "persisted text must not leak pending refs: {user_text}"
    );
    assert!(
        user_text.contains("att-q1-queued_shot.png"),
        "persisted text names the committed file: {user_text}"
    );

    core.shutdown().await;
}

/// The race behind the queued-row flake, forced. A row is written into the
/// shared doc field by field, and a drain on another thread can run between
/// any two fields (Loro shows each insert to readers at once). Run a drain at
/// every point of the real write: none may take the row or send it — a row
/// read before its `attachments` landed looked ready and went out without its
/// image. Once whole, it waits for its bytes like any queued row.
#[tokio::test(flavor = "multi_thread")]
async fn a_drain_racing_a_queued_row_write_never_sends_it_early() {
    let tmp = tempfile::tempdir().unwrap();
    let (core, runs) = engine(tmp.path());
    let client = onicode_rpc::memory_client(core.rpc_service());
    create_chat(&core, &client).await;
    let handle = core.doc_host.open(CHAT).unwrap();

    let row = QueuedMessage {
        id: "row-torn".into(),
        text: "what is this".into(),
        attachments: vec!["pending://att-t1/torn shot.png".into()],
        hold_for_turn_end: false,
        issued_by: "phone".into(),
        issued_at: 1,
        edited_at: None,
        delivery_gate: None,
    };
    let fields = onicode_doc::queued_row_fields(&row).unwrap();
    let queue = handle.doc().doc().get_movable_list("queue");
    let map = queue.push_container(loro::LoroMap::new()).unwrap();
    for (written, (key, value)) in fields.iter().enumerate() {
        core.doc_host.drain_queue(&handle).await;
        assert!(
            runs.lock().unwrap().is_empty(),
            "a drain sent the row with {written} of {} fields written",
            fields.len()
        );
        assert_eq!(
            queue.len(),
            1,
            "a drain took the row with {written} of {} fields written",
            fields.len()
        );
        map.insert(key, value.clone()).unwrap();
    }
    handle.doc().doc().commit();

    // Whole now, and its bytes are still in transit: held, not sent.
    core.doc_host.drain_queue(&handle).await;
    assert!(
        runs.lock().unwrap().is_empty(),
        "sent before its bytes landed"
    );
    assert_eq!(handle.doc().read_queue().unwrap(), vec![row]);

    land_upload(&client, "att-t1", "torn shot.png").await;
    wait_for_queued_turn(&core).await;
    assert!(handle.doc().read_queue().unwrap().is_empty());
    assert_one_run_on(&runs, "att-t1-torn_shot.png");

    core.shutdown().await;
}
