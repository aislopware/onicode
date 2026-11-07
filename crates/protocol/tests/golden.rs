//! The protocol's golden table: every method's name, kind and agent flag, and a hash of the serde
//! shape of every type it carries, recorded in `tests/golden/protocol.txt` with the `PROTOCOL` it
//! was recorded at. A change to any of them fails here until `PROTOCOL` is bumped and the table is
//! recorded again (`ONICODE_GOLDEN=record`); recording over a table of the same `PROTOCOL` is
//! refused.
//!
//! The shapes are traced by serde-reflection through the types' own Serialize and Deserialize
//! impls, so a renamed field, a new variant or a changed `#[serde]` attribute counts, in the
//! protocol's types and in the core types they carry alike. Each hash covers the type and every
//! named type it reaches.
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt::Write as _;
use std::path::PathBuf;

use onicode_protocol::agent::AccessMode;
use onicode_protocol::engine::{EngineInfo, Hello, StopEngine, UpdateEngine};
use onicode_protocol::files::{
    AdoptAttachments, Create, DeletePath, Destination, Entries, ListDir, ListDrives, Move,
    PathKind, Read, SaveAttachments, SaveNoteImages, SearchContent, SearchHomeFolders, SearchPaths,
    Stat, StopSearch, WatchDir, Write,
};
use onicode_protocol::git::{
    Branches, ChangeSet, Commit, CommitContext, CommitFiles, CreateBranch, CreatePr,
    CreateWorktree, Diff, Discard, FileDiff, HeadMessage, History, NewWorktree, PrContext,
    PrStatus, Pull, Push, ReadCheckpoint, ReadDiffStats, ReadSessionChanges, ReadStatus, Refs,
    RemoveWorktree, Restore, RestoreCheckpoint, Stage, StageTarget, Switch, Sync, Unstage,
    WatchGit, Worktrees,
};
use onicode_protocol::provider::Kind;
use onicode_protocol::search::Target;
use onicode_protocol::session::{
    Answer, Btw, Compact, Configure, Decide, EditLastTurn, Fork, KeepQuestionOpen, QueueChange,
    QueueEdit, ReadBtw, ReadSession, ReadTranscript, RecallLastTurn, ResetThread, Rewind, SaveBtw,
    SaveQueue, Send, SendNow, Steer, SteerNow, Stop, StopBtw, SwitchProvider, Verdict,
    WatchSession,
};
use onicode_protocol::terminal::{
    CloseTerminal, Input, ListTerminals, OpenTerminal, ReadForeground, ResizeTerminal,
    StopForeground, TerminalEvent, TerminalInput, WatchTerminal,
};
use onicode_protocol::transcript::Row;
use onicode_protocol::workspace::{
    Archive, ClaimInboxEvents, CreateNote, CreateSession, Delete, DeleteNote, FinishAutomationRun,
    GenerateTitle, ListNotes, MarkSeen, NameWorktree, NewSession, Pin, PrepareAutomationRun,
    PreviewSession, ReminderChange, Rename, SaveGroups, SaveOrchestration, SaveResponseNote,
    SearchSessions, SetReminder, SetWorkItem, UpsertNote, WatchWorkspace,
};
use onicode_protocol::{
    Call, Cursor, Item, METHODS, MessageId, Method, MethodKind, OpId, PROTOCOL, Rejection, Routed,
    SessionId, TerminalId, Watch, agents, engine_settings,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_reflection::{Format, FormatHolder, Registry, Samples, Tracer, TracerConfig};

/// One line of the table: its label and the formats it hashes, by role.
type Line = (String, Vec<(&'static str, Format)>);

/// Every type of the table traced into one registry.
struct Shapes {
    tracer: Tracer,
    samples: Samples,
    methods: Vec<Method>,
    lines: Vec<Line>,
}

impl Shapes {
    fn new() -> Self {
        // Serde's compact mode, the one serde-reflection replays samples in: a Uuid checks its
        // input, so the tracer hands each id type a real one.
        let mut tracer = Tracer::new(TracerConfig::default());
        let mut samples = Samples::new();
        tracer.trace_value(&mut samples, &SessionId::new()).unwrap();
        tracer.trace_value(&mut samples, &MessageId::new()).unwrap();
        tracer.trace_value(&mut samples, &OpId::new()).unwrap();
        tracer
            .trace_value(&mut samples, &TerminalId::new())
            .unwrap();
        // A row's echo id is a bare Uuid, so the row itself is the sample, every field set so none
        // is skipped.
        let row = Row {
            role: "user".into(),
            text: String::new(),
            first: true,
            sent_at: Some(1),
            id: Some(uuid::Uuid::now_v7()),
        };
        tracer.trace_value(&mut samples, &row).unwrap();
        Self {
            tracer,
            samples,
            methods: Vec::new(),
            lines: Vec::new(),
        }
    }

    fn trace<T: DeserializeOwned>(&mut self) -> Format {
        let (mut format, _) = self
            .tracer
            .trace_type::<T>(&self.samples)
            .unwrap_or_else(|error| panic!("tracing {}: {error}", std::any::type_name::<T>()));
        format.normalize().unwrap();
        format
    }

    fn call<C: Call>(&mut self) {
        let params = self.trace::<C::Params>();
        let reply = self.trace::<C::Reply>();
        let agent = if C::AGENT { " agent" } else { "" };
        self.methods.push(Method {
            name: C::NAME,
            kind: MethodKind::Call,
            agent: C::AGENT,
        });
        self.lines.push((
            format!("call {}{agent}", C::NAME),
            vec![("params", params), ("reply", reply)],
        ));
    }

    fn watch<W: Watch>(&mut self) {
        let params = self.trace::<W::Params>();
        let state = self.trace::<W::State>();
        let event = self.trace::<W::Event>();
        self.methods.push(Method {
            name: W::NAME,
            kind: MethodKind::Watch,
            agent: false,
        });
        self.lines.push((
            format!("watch {}", W::NAME),
            vec![("params", params), ("state", state), ("event", event)],
        ));
    }

    fn envelope<T: DeserializeOwned>(&mut self, name: &str) {
        let shape = self.trace::<T>();
        self.lines
            .push((format!("envelope {name}"), vec![("shape", shape)]));
    }

    /// An enum only ever reached inside another type: tracing it alone finds every variant.
    fn variants<T: DeserializeOwned>(&mut self) {
        self.trace::<T>();
    }

    fn table(self) -> (Vec<Method>, String) {
        let registry = self
            .tracer
            .registry()
            .unwrap_or_else(|error| panic!("{error}: trace the enums it names with `variants`"));
        let mut table = format!("protocol {PROTOCOL}\n");
        for (label, formats) in &self.lines {
            table.push_str(label);
            for (role, format) in formats {
                write!(table, " {role}={}", digest(format, &registry)).unwrap();
            }
            table.push('\n');
        }
        (self.methods, table)
    }
}

/// The FNV-1a hash of the format and of every container it reaches, as 16 hex digits.
fn digest(format: &Format, registry: &Registry) -> String {
    let format = serde_json::to_value(format).unwrap();
    let mut reached = BTreeMap::new();
    reach(&format, registry, &mut reached);
    let text = serde_json::to_string(&sorted(&serde_json::json!([format, reached]))).unwrap();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// `value` with every object's keys in order, whether or not serde_json keeps insertion order
/// (its `preserve_order` feature, which other crates of the workspace turn on).
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_unstable();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Collects the containers `value` names (`{"TYPENAME": name}`), and the ones they name.
fn reach(value: &Value, registry: &Registry, reached: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                if key == "TYPENAME"
                    && let Value::String(name) = inner
                {
                    if let Entry::Vacant(entry) = reached.entry(name.clone()) {
                        let container = serde_json::to_value(&registry[name]).unwrap();
                        entry.insert(container.clone());
                        reach(&container, registry, reached);
                    }
                } else {
                    reach(inner, registry, reached);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                reach(item, registry, reached);
            }
        }
        _ => {}
    }
}

fn shapes() -> (Vec<Method>, String) {
    let mut shapes = Shapes::new();
    // Engine.
    shapes.call::<Hello>();
    shapes.call::<EngineInfo>();
    shapes.call::<UpdateEngine>();
    shapes.call::<StopEngine>();
    // Session.
    shapes.watch::<WatchSession>();
    shapes.call::<Send>();
    shapes.call::<Steer>();
    shapes.call::<Stop>();
    shapes.call::<Decide>();
    shapes.call::<Answer>();
    shapes.call::<KeepQuestionOpen>();
    shapes.call::<QueueEdit>();
    shapes.call::<SendNow>();
    shapes.call::<SteerNow>();
    shapes.call::<Configure>();
    shapes.call::<EditLastTurn>();
    shapes.call::<Rewind>();
    shapes.call::<Fork>();
    shapes.call::<Btw>();
    shapes.call::<SwitchProvider>();
    shapes.call::<ResetThread>();
    shapes.call::<Compact>();
    shapes.call::<ReadTranscript>();
    shapes.call::<ReadSession>();
    shapes.call::<SaveQueue>();
    shapes.call::<RecallLastTurn>();
    shapes.call::<ReadBtw>();
    shapes.call::<SaveBtw>();
    shapes.call::<StopBtw>();
    // Workspace.
    shapes.watch::<WatchWorkspace>();
    shapes.call::<CreateSession>();
    shapes.call::<Rename>();
    shapes.call::<Archive>();
    shapes.call::<Pin>();
    shapes.call::<Delete>();
    shapes.call::<SetWorkItem>();
    shapes.call::<SetReminder>();
    shapes.call::<SaveGroups>();
    shapes.call::<SearchSessions>();
    shapes.call::<PreviewSession>();
    shapes.call::<MarkSeen>();
    shapes.call::<SaveOrchestration>();
    shapes.call::<GenerateTitle>();
    shapes.call::<NameWorktree>();
    shapes.call::<ListNotes>();
    shapes.call::<CreateNote>();
    shapes.call::<UpsertNote>();
    shapes.call::<DeleteNote>();
    shapes.call::<SaveResponseNote>();
    shapes.call::<PrepareAutomationRun>();
    shapes.call::<FinishAutomationRun>();
    shapes.call::<ClaimInboxEvents>();
    // Files.
    shapes.watch::<WatchDir>();
    shapes.call::<ListDir>();
    shapes.call::<Stat>();
    shapes.call::<Read>();
    shapes.call::<Write>();
    shapes.call::<Create>();
    shapes.call::<Move>();
    shapes.call::<DeletePath>();
    shapes.call::<SearchPaths>();
    shapes.call::<SearchContent>();
    shapes.call::<StopSearch>();
    shapes.call::<ListDrives>();
    shapes.call::<SearchHomeFolders>();
    shapes.call::<SaveAttachments>();
    shapes.call::<AdoptAttachments>();
    shapes.call::<SaveNoteImages>();
    // Git.
    shapes.watch::<WatchGit>();
    shapes.call::<ReadStatus>();
    shapes.call::<Diff>();
    shapes.call::<FileDiff>();
    shapes.call::<Branches>();
    shapes.call::<Refs>();
    shapes.call::<History>();
    shapes.call::<CommitFiles>();
    shapes.call::<ReadDiffStats>();
    shapes.call::<Switch>();
    shapes.call::<CreateBranch>();
    shapes.call::<Stage>();
    shapes.call::<Unstage>();
    shapes.call::<Discard>();
    shapes.call::<Commit>();
    shapes.call::<Push>();
    shapes.call::<Pull>();
    shapes.call::<Sync>();
    shapes.call::<HeadMessage>();
    shapes.call::<CommitContext>();
    shapes.call::<PrContext>();
    shapes.call::<PrStatus>();
    shapes.call::<CreatePr>();
    shapes.call::<Worktrees>();
    shapes.call::<CreateWorktree>();
    shapes.call::<RemoveWorktree>();
    shapes.call::<ReadCheckpoint>();
    shapes.call::<RestoreCheckpoint>();
    shapes.call::<ReadSessionChanges>();
    // Terminal.
    shapes.watch::<WatchTerminal>();
    shapes.call::<OpenTerminal>();
    shapes.call::<ListTerminals>();
    shapes.call::<ResizeTerminal>();
    shapes.call::<CloseTerminal>();
    shapes.call::<TerminalInput>();
    shapes.call::<ReadForeground>();
    shapes.call::<StopForeground>();
    // Agents.
    shapes.watch::<agents::WatchAccounts>();
    shapes.call::<agents::RefreshUsage>();
    shapes.call::<agents::RefreshSignIn>();
    shapes.call::<agents::ConsumeUsageReset>();
    shapes.call::<agents::Catalog>();
    shapes.call::<agents::Skills>();
    shapes.call::<agents::CreateSkill>();
    shapes.call::<agents::Commands>();
    shapes.call::<agents::AddAccount>();
    shapes.call::<agents::RenameAccount>();
    shapes.call::<agents::RemoveAccount>();
    shapes.call::<agents::SelectAccount>();
    shapes.call::<agents::Login>();
    shapes.call::<agents::CancelLogin>();
    shapes.call::<agents::Logout>();
    shapes.call::<agents::McpServers>();
    shapes.call::<agents::AddMcpServer>();
    shapes.call::<agents::RemoveMcpServer>();
    shapes.call::<agents::McpLogin>();
    shapes.call::<agents::McpHealth>();
    shapes.call::<agents::Harnesses>();
    shapes.call::<agents::InspectHarness>();
    shapes.call::<agents::HarnessUpdates>();
    shapes.call::<agents::RunHarnessUpdate>();
    shapes.call::<agents::Generate>();
    shapes.call::<agents::StopGenerate>();
    shapes.call::<agents::KeepIdleAgents>();
    shapes.call::<agents::TransferThread>();
    shapes.call::<agents::ForgetThread>();
    // Engine settings.
    shapes.watch::<engine_settings::WatchEngineSettings>();
    shapes.call::<engine_settings::SetEngineSetting>();
    shapes.call::<engine_settings::TakeViewerState>();
    // Work.
    shapes.call::<onicode_protocol::work::InboxList>();
    shapes.call::<onicode_protocol::work::ReadInboxDetails>();
    shapes.call::<onicode_protocol::work::ReadInboxThread>();
    shapes.call::<onicode_protocol::work::ReadInboxDiff>();
    shapes.call::<onicode_protocol::work::ReadInboxChecks>();
    shapes.call::<onicode_protocol::work::PostInboxComment>();
    shapes.call::<onicode_protocol::work::ConnectInbox>();
    shapes.call::<onicode_protocol::work::RunInboxAction>();
    shapes.call::<onicode_protocol::work::InboxMedia>();
    shapes.call::<onicode_protocol::work::InboxAvatar>();
    shapes.call::<onicode_protocol::work::ListAutomations>();
    shapes.call::<onicode_protocol::work::AutomationRuns>();
    shapes.call::<onicode_protocol::work::UpsertAutomation>();
    shapes.call::<onicode_protocol::work::SetAutomationEnabled>();
    shapes.call::<onicode_protocol::work::RunAutomationNow>();
    shapes.call::<onicode_protocol::work::DeleteAutomation>();
    shapes.call::<onicode_protocol::work::HasEventAutomations>();
    shapes.call::<onicode_protocol::work::ProjectGroups>();
    shapes.call::<onicode_protocol::work::LinkedActivity>();
    shapes.call::<onicode_protocol::work::AgentProcesses>();
    shapes.call::<onicode_protocol::work::StopAgentTool>();
    shapes.call::<onicode_protocol::work::ReadWorkItem>();
    shapes.call::<onicode_protocol::work::ReadWorkItemDetails>();
    // App access.
    shapes.call::<onicode_protocol::app_access::ReadAppReceipt>();
    shapes.call::<onicode_protocol::app_access::SaveAppReceipt>();
    shapes.call::<onicode_protocol::app_access::AppModels>();
    shapes.call::<onicode_protocol::app_access::StartAppSession>();
    shapes.call::<onicode_protocol::app_access::ReadAppConversation>();
    shapes.call::<onicode_protocol::app_access::AppNotes>();
    // Settings pages.
    shapes.call::<onicode_protocol::settings_pages::ReadWorktreeMarks>();
    // What every call and watch travels in.
    shapes.envelope::<Cursor>("Cursor");
    shapes.envelope::<Item<(), ()>>("Item");
    shapes.envelope::<Routed<()>>("Routed");
    shapes.variants::<Rejection>();
    shapes.variants::<Verdict>();
    shapes.variants::<QueueChange>();
    shapes.variants::<Kind>();
    shapes.variants::<AccessMode>();
    shapes.variants::<NewSession>();
    shapes.variants::<ReminderChange>();
    shapes.variants::<Target>();
    shapes.variants::<Entries>();
    shapes.variants::<PathKind>();
    shapes.variants::<Destination>();
    shapes.variants::<ChangeSet>();
    shapes.variants::<StageTarget>();
    shapes.variants::<NewWorktree>();
    shapes.variants::<Restore>();
    shapes.variants::<Input>();
    shapes.variants::<TerminalEvent>();
    shapes.variants::<agents::AccountsEvent>();
    shapes.variants::<agents::UsageRead>();
    shapes.variants::<agents::CatalogRead>();
    shapes.variants::<onicode_protocol::usage::Status>();
    shapes.variants::<engine_settings::EngineSetting>();
    shapes.variants::<engine_settings::EngineSettingsEvent>();
    shapes.table()
}

#[test]
fn every_method_is_in_the_golden_table() {
    let (methods, _) = shapes();
    assert_eq!(methods, METHODS);
    let mut names: Vec<_> = METHODS.iter().map(|method| method.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), METHODS.len(), "two methods share a name");
}

#[test]
fn the_table_matches_its_golden() {
    let (_, table) = shapes();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/protocol.txt");
    let recorded = std::fs::read_to_string(&path).ok();
    if recorded.as_deref() == Some(table.as_str()) {
        return;
    }
    let record = match std::env::var("ONICODE_GOLDEN").as_deref() {
        Ok("record") => true,
        Ok("check" | "") | Err(_) => false,
        Ok(other) => panic!("ONICODE_GOLDEN={other}: expected record or check"),
    };
    let recorded_protocol = recorded
        .as_deref()
        .and_then(|text| text.lines().next())
        .and_then(|line| line.strip_prefix("protocol "))
        .and_then(|number| number.parse::<u32>().ok());
    if record {
        assert_ne!(
            recorded_protocol,
            Some(PROTOCOL),
            "the table changed while PROTOCOL stayed {PROTOCOL}: bump it in \
             crates/protocol/src/lib.rs, then record again"
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &table).unwrap();
        return;
    }
    panic!(
        "the protocol's table changed.\n--- recorded\n{}--- now\n{table}Bump PROTOCOL in \
         crates/protocol/src/lib.rs, then run this test with ONICODE_GOLDEN=record",
        recorded.unwrap_or_default()
    );
}
