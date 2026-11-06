//! Session docs and the registry between real engines (mock harness) on a simulated tailnet, in
//! virtual time: a doc syncs replica to replica by version vectors over `doc/{chatId}` streams,
//! the registry by per-origin version vectors over `registry` streams.

#[path = "support/tailnet.rs"]
mod tailnet;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_doc::{
    MessagePart, MessageRole, MessageStatus, RegistryRow, SessionCommandPayload, SessionDoc,
    SessionMessageEntry, Vv, encode_hlc,
};
use onicode_engine::peers::Peers;
use onicode_engine::{DEFAULT_ORG_ID, DEFAULT_USER_ID, EngineCore, EngineProfile, HarnessRegistry};
use onicode_harness::{Harness, HarnessError, RunControls};
use onicode_net::{Role, StreamHead, StreamKind};
use onicode_net_sim::NodeId;
use onicode_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode,
};
use onicode_sync::DocsStore;
use onicode_sync::registry::Frame;

const CHAT: &str = "chat-peer-sync";
const A: &str = "device-a";
const B: &str = "device-b";
const C: &str = "device-c";

/// Virtual time a convergence may take: past every redial backoff's ceiling.
const SETTLE: Duration = Duration::from_secs(900);

/// Answers each run with one line, and counts the runs.
struct CountingHarness(Arc<AtomicUsize>);

#[async_trait]
impl Harness for CountingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }

    fn display_name(&self) -> &str {
        "Counting"
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
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(futures::stream::iter([
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "counting-1".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: format!("hs-{n}"),
                assistant_message_id: format!("a-{n}"),
            }),
            Ok(AgentEvent::TextDelta {
                text: format!("reply {n}"),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(format!("hs-{n}")),
            }),
        ])
        .boxed())
    }
}

struct Engine {
    core: EngineCore,
    runs: Arc<AtomicUsize>,
    store: DocsStore,
}

/// An engine named `device` in `root/device`; `seed` writes snapshots into its store first.
fn engine(root: &std::path::Path, device: &str, seed: &[(&str, &[u8])]) -> Engine {
    let dir = root.join(device);
    std::fs::create_dir_all(&dir).expect("data dir");
    std::fs::write(dir.join("device-id"), device).expect("device id");
    let store = DocsStore::open(
        EngineProfile::development(&dir, DEFAULT_ORG_ID, DEFAULT_USER_ID).store_root(),
    )
    .expect("store opens");
    for (doc, bytes) in seed {
        store.save_snapshot(doc, bytes).expect("seed snapshot");
    }
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CountingHarness(runs.clone())));
    let core = EngineCore::assemble(&dir, Arc::new(registry), HarnessId::Mock, None)
        .expect("engine assembles");
    Engine { core, runs, store }
}

/// The engines, each on a machine of its own, linked while it lives.
struct Mesh {
    tailnet: tailnet::Tailnet,
    joined: Vec<(NodeId, Peers)>,
}

impl Mesh {
    async fn new(engines: &[&Engine], seed: u64) -> Self {
        let cores: Vec<_> = engines.iter().map(|engine| &engine.core).collect();
        let (tailnet, joined) = tailnet::mesh(&cores, seed).await;
        Self { tailnet, joined }
    }

    /// Takes the `i`th engine's machine off the network (`true`) or back on.
    fn asleep(&self, i: usize, asleep: bool) {
        self.tailnet.sim.set_asleep(self.joined[i].0, asleep);
    }
}

/// Polls `done` in virtual time until it holds.
async fn settle(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + SETTLE;
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never settled"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Lets the sessions run for `length` of virtual time.
async fn run_for(length: Duration) {
    tokio::time::sleep(length).await;
}

fn run(message_id: &str, prompt: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: Box::new(RunRequest {
            mcp: None,
            prompt: prompt.into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "/tmp".into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            access: Default::default(),
            plan: false,
            attachments: Vec::new(),
            worktree: None,
            resume: None,
            env: Default::default(),
        }),
        message_id: message_id.into(),
    }
}

fn entries(engine: &Engine) -> Vec<SessionMessageEntry> {
    engine
        .core
        .doc_host
        .open(CHAT)
        .expect("chat opens")
        .doc()
        .read_entries()
        .expect("entries read")
}

/// Completed replies in `engine`'s replica.
fn replies(engine: &Engine) -> usize {
    entries(engine)
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

fn holds_message(engine: &Engine, message_id: &str) -> bool {
    entries(engine).iter().any(|e| e.id == message_id)
}

/// Whether `engine`'s replica holds the queued run for `message_id` (executed or not).
fn holds_command(engine: &Engine, message_id: &str) -> bool {
    engine
        .core
        .doc_host
        .open(CHAT)
        .expect("chat opens")
        .doc()
        .read_commands()
        .expect("commands read")
        .iter()
        .any(|c| {
            matches!(&c.payload,
                SessionCommandPayload::Run { message_id: id, .. } if id == message_id)
        })
}

fn vv(engine: &Engine) -> loro::VersionVector {
    engine
        .core
        .doc_host
        .open(CHAT)
        .expect("chat opens")
        .doc()
        .doc()
        .oplog_vv()
}

fn unacked(engine: &Engine) -> u64 {
    engine.store.outbox_len(CHAT).expect("outbox reads")
}

fn knows_chat(engine: &Engine) -> bool {
    engine.core.workspace.chat(CHAT).ok().flatten().is_some()
}

/// Three engines, `A` hosting [`CHAT`], linked, with the chat's row everywhere.
async fn three(root: &std::path::Path, seed: u64) -> (Engine, Engine, Engine, Mesh) {
    let (a, b, c) = (
        engine(root, A, &[]),
        engine(root, B, &[]),
        engine(root, C, &[]),
    );
    let mesh = Mesh::new(&[&a, &b, &c], seed).await;
    a.core
        .workspace
        .create_chat(CHAT, None, Some(A), None, None)
        .expect("chat row");
    settle("the chat's row reaching every engine", || {
        knows_chat(&b) && knows_chat(&c)
    })
    .await;
    (a, b, c, mesh)
}

/// A viewer sends; the host executes it once; the transcript reaches every replica.
#[tokio::test(start_paused = true)]
async fn a_viewers_send_runs_once_on_the_host_and_reaches_every_replica() {
    let root = tempfile::tempdir().unwrap();
    let (a, b, c, _mesh) = three(root.path(), 81).await;
    let _watching = c.core.doc_host.open(CHAT).expect("C opens the chat");

    b.core
        .doc_host
        .queue_command(CHAT, run("m-1", "hello"))
        .expect("B queues");
    settle("the reply on every replica", || {
        [&a, &b, &c]
            .iter()
            .all(|e| replies(e) == 1 && holds_message(e, "m-1"))
    })
    .await;
    settle("B's send acked by the host", || unacked(&b) == 0).await;
    run_for(Duration::from_secs(60)).await;
    assert_eq!(a.runs.load(Ordering::SeqCst), 1, "the host ran it once");
    assert_eq!(b.runs.load(Ordering::SeqCst), 0, "a viewer never runs it");
    assert_eq!(c.runs.load(Ordering::SeqCst), 0, "a replica never runs it");
    assert!([&a, &b, &c].iter().all(|e| replies(e) == 1));
}

/// The host is away: the sender hands its write to a third engine, which holds it as a mailbox
/// and forwards it when the host is back. Only the host's ack retires the sender's outbox.
#[tokio::test(start_paused = true)]
async fn a_mailbox_carries_a_send_to_the_host_and_only_the_hosts_ack_retires_it() {
    let root = tempfile::tempdir().unwrap();
    let (a, b, c, mesh) = three(root.path(), 82).await;
    let _following = b.core.doc_host.open(CHAT).expect("B opens the chat");
    mesh.asleep(0, true);
    settle("A's links gone", || {
        [&b, &c]
            .iter()
            .all(|e| e.core.links().is_none_or(|links| links.live(A).is_none()))
    })
    .await;

    b.core
        .doc_host
        .queue_command(CHAT, run("m-1", "while the host sleeps"))
        .expect("B queues");
    settle("the send held by the mailbox", || holds_command(&c, "m-1")).await;
    assert!(!holds_message(&a, "m-1"), "the host is away");
    // The mailbox acked it; the host has not.
    run_for(Duration::from_secs(30)).await;
    assert!(
        unacked(&b) > 0,
        "a mailbox's ack must not retire the outbox"
    );

    // The sender goes away too: the send can only reach the host through the mailbox.
    mesh.asleep(1, true);
    mesh.asleep(0, false);
    settle("the host running the forwarded send", || {
        replies(&a) == 1 && replies(&c) == 1
    })
    .await;
    assert!(
        unacked(&b) > 0,
        "the sender has not heard the host's ack yet"
    );

    mesh.asleep(1, false);
    settle("the sender caught up and acked by the host", || {
        replies(&b) == 1 && unacked(&b) == 0
    })
    .await;
    run_for(Duration::from_secs(60)).await;
    assert_eq!(a.runs.load(Ordering::SeqCst), 1, "the host ran it once");
    assert_eq!(
        b.runs.load(Ordering::SeqCst) + c.runs.load(Ordering::SeqCst),
        0
    );
}

/// A partitioned replica that wrote meanwhile converges with the rest once healed.
#[tokio::test(start_paused = true)]
async fn partition_and_heal_converge_every_replicas_frontiers() {
    let root = tempfile::tempdir().unwrap();
    let (a, b, c, mesh) = three(root.path(), 83).await;
    let _b = b.core.doc_host.open(CHAT).expect("B opens the chat");
    let _c = c.core.doc_host.open(CHAT).expect("C opens the chat");

    mesh.asleep(2, true);
    b.core
        .doc_host
        .queue_command(CHAT, run("m-b", "from B"))
        .expect("B queues");
    c.core
        .doc_host
        .queue_command(CHAT, run("m-c", "from C, partitioned"))
        .expect("C queues");
    settle("B's send answered", || replies(&a) == 1 && replies(&b) == 1).await;
    assert!(!holds_command(&a, "m-c"), "the partition holds");

    mesh.asleep(2, false);
    settle("every replica holding the same ops", || {
        let (va, vb, vc) = (vv(&a), vv(&b), vv(&c));
        replies(&a) == 2 && va == vb && vb == vc
    })
    .await;
    let frontiers = |e: &Engine| {
        e.core
            .doc_host
            .open(CHAT)
            .expect("chat opens")
            .doc()
            .doc()
            .oplog_frontiers()
    };
    assert_eq!(frontiers(&a), frontiers(&b));
    assert_eq!(frontiers(&b), frontiers(&c));
    assert_eq!(a.runs.load(Ordering::SeqCst), 2, "each send ran once");
    settle("every outbox retired", || {
        unacked(&b) == 0 && unacked(&c) == 0
    })
    .await;
}

/// Registry rows converge on every engine, and a peer cannot write a field only a row's owner
/// may: its writes to the rest of the row still land.
#[tokio::test(start_paused = true)]
async fn registry_rows_converge_and_owner_only_fields_refuse_other_writers() {
    let root = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        engine(root.path(), A, &[]),
        engine(root.path(), B, &[]),
        engine(root.path(), C, &[]),
    );
    let mesh = Mesh::new(&[&a, &b, &c], 84).await;
    a.core
        .workspace
        .create_space("space-a", A, "/tmp/space-a", None, false)
        .expect("space row");
    b.core
        .workspace
        .create_chat(CHAT, None, Some(B), None, None)
        .expect("chat row");
    c.core.workspace.rename_device(A, "Studio").expect("rename");
    let same = |x: &Engine, y: &Engine| {
        let (x, y) = (&x.core.workspace, &y.core.workspace);
        x.read_chats().ok() == y.read_chats().ok()
            && x.read_spaces().ok() == y.read_spaces().ok()
            && x.read_devices().ok() == y.read_devices().ok()
    };
    settle("registry rows equal everywhere", || {
        knows_chat(&a)
            && knows_chat(&c)
            && c.core.workspace.space("space-a").ok().flatten().is_some()
            && a.core
                .workspace
                .read_devices()
                .is_ok_and(|d| d.iter().any(|d| d.id == A && d.name == "Studio"))
            && same(&a, &b)
            && same(&b, &c)
    })
    .await;
    assert!(
        [&a, &b, &c]
            .iter()
            .all(|e| e.core.workspace.registry_synced())
    );

    // A device on the tailnet writes B's chat row as itself: a title anyone may set, and a
    // preview only the chat's host may.
    let intruder = mesh
        .tailnet
        .join(mesh.tailnet.node(), "device-x", Role::Viewer, None, None)
        .await;
    let head = StreamHead::new(StreamKind::Registry, "");
    let (tx, _rx) = loop {
        match intruder.links.open(A, &head).await {
            Ok(stream) => break stream,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };
    let clock = encode_hlc(
        chrono::Utc::now().timestamp_millis() + 60_000,
        0,
        "device-x",
    );
    let forged = RegistryRow {
        kind: onicode_doc::KIND_CHATS.into(),
        id: CHAT.into(),
        vv: Vv::from([("device-x".to_owned(), 1)]),
        deleted: false,
        del_hlc: None,
        fields: [
            ("title".to_owned(), serde_json::json!("named by x")),
            ("lastMessagePreview".to_owned(), serde_json::json!("forged")),
        ]
        .into(),
        clocks: [
            ("title".to_owned(), clock.clone()),
            ("lastMessagePreview".to_owned(), clock),
        ]
        .into(),
    };
    tx.send_frame(&Frame::Hello { vv: Vv::new() }.encode().unwrap())
        .unwrap();
    tx.send_frame(
        &Frame::Rows {
            rows: vec![forged],
            vv: Some(Vv::from([("device-x".to_owned(), 1)])),
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    settle("the allowed write on every engine", || {
        [&a, &b, &c].iter().all(|e| {
            e.core
                .workspace
                .chat(CHAT)
                .ok()
                .flatten()
                .is_some_and(|chat| chat.title.as_deref() == Some("named by x"))
        })
    })
    .await;
    for e in [&a, &b, &c] {
        let chat = e.core.workspace.chat(CHAT).unwrap().unwrap();
        assert_ne!(
            chat.last_message_preview.as_deref(),
            Some("forged"),
            "{}: an owner-only field took a non-owner's write",
            e.core.device_id
        );
        assert_eq!(chat.device_id, B, "ownership never moves");
    }
}

/// A replica behind the host's shallow root cannot be sent the ops it lacks: it gets the
/// snapshot, adopts it, and holds the whole transcript.
#[tokio::test(start_paused = true)]
async fn a_straggler_behind_the_shallow_root_adopts_a_snapshot() {
    let message = |id: &str| SessionMessageEntry {
        id: id.into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: id.into(),
        }],
        created_at: 0,
        device_id: A.into(),
        status: None,
        continuation_of: None,
        duration_ms: None,
    };
    let source = SessionDoc::init(CHAT).unwrap();
    source.push_message(&message("old")).unwrap();
    let stale = source.export_snapshot().unwrap();
    source.push_message(&message("newer")).unwrap();
    source.push_message(&message("newest")).unwrap();
    let trimmed = source
        .doc()
        .export(loro::ExportMode::shallow_snapshot(
            &source.doc().oplog_frontiers(),
        ))
        .unwrap();

    let root = tempfile::tempdir().unwrap();
    let a = engine(root.path(), A, &[(CHAT, &trimmed)]);
    let b = engine(root.path(), B, &[(CHAT, &stale)]);
    let _mesh = Mesh::new(&[&a, &b], 85).await;
    assert!(
        a.core.doc_host.open(CHAT).unwrap().doc().doc().is_shallow(),
        "the host holds only the trimmed history"
    );
    a.core
        .workspace
        .create_chat(CHAT, None, Some(A), None, None)
        .expect("chat row");
    settle("the chat's row on the straggler", || knows_chat(&b)).await;

    b.core.doc_host.open(CHAT).expect("B opens the chat");
    settle("the straggler holding the whole transcript", || {
        holds_message(&b, "newest") && holds_message(&b, "old")
    })
    .await;
    let adopted = b.core.doc_host.open(CHAT).unwrap();
    assert!(adopted.doc().doc().is_shallow(), "it adopted the snapshot");
    assert_eq!(vv(&a), vv(&b));
}
