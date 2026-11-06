//! DocHost — per-chat `SessionDoc` handles: snapshot persistence (debounced), replica sync with
//! the chat's host over the tailnet (`doc_server`), and the HOST-ONLY durable command executor.
//!
//! - the doc IS the outbox: commands and user entries commit locally, and each local write is also
//!   kept in the store's outbox until the chat's host acks a version holding it; the engine is
//!   fully functional with no peer reachable;
//! - on every doc change (local commit or remote import) the handle re-emits the joined transcript
//!   to watchers, drains pending commands, and schedules a snapshot save;
//! - command drain: evaluate via `evaluate_command` (with the DocsStore processed ledger), mark
//!   processed BEFORE execute, execute through the sessions engine, then write the outcome status
//!   back into the doc as the sole outcome writer.
//!
//! Chat ownership is gated on the registry (`chats[chat_id].deviceId`), with
//! claim-on-first-command for unknown chats. A chat another engine hosts is followed live while
//! it is open here, so a queued command reaches the host as soon as both are on the tailnet.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use base64::Engine as _;
use onicode_doc::{
    COMMAND_DEFAULT_TTL_MS, CommandBasedOn, CommandDisposition, DocError, EvaluationContext,
    MessagePart, MessageRole, MessageStatus, QueueDeliveryGate, QueuedMessage, SessionCommandEntry,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry, SubagentStatus,
    evaluate_command, join_continuation_entries,
};
use onicode_proto::{ConversationSourceContext, HarnessId, UserInputAnswer, UserInputQuestion};
use onicode_sync::DocsStore;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::edge::EdgeConfig;
use crate::http_error::describe_http_error;
use crate::project_actions::{
    ProjectActionSetupHandoff, ProjectActionsStore, launch_project_setup_action,
};
use crate::sessions::{SessionsEngine, SteerOutcome};
use crate::workspace_host::WorkspaceHost;
use crate::{EngineError, Terminals, new_id, now_ms};

/// How long a doc stays quiet before the eviction pass looks at the warm set again.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;

/// An edit client renews every 20s. Sixty seconds tolerates two missed
/// heartbeats without turning a vanished client into an invisible permanent
/// lock. Expiry fails closed into ReviewRequired rather than releasing.
pub const QUEUE_EDIT_LEASE_MS: i64 = 60_000;

/// Warm-doc LRU: how many unwatched, run-less docs stay fully open. Everything
/// beyond this (and beyond [`onicode_doc::DOC_LRU_BYTE_BUDGET`]) is evicted
/// oldest-access-first — reopening from the SQLite snapshot measured within
/// ~11ms of a warm doc, so the cap trades no perceptible open latency.
const WARM_DOC_CAP: usize = 12;

/// Resident-memory estimate per compressed snapshot byte. Loro snapshots are
/// columnar+compressed; the in-memory doc plus mirror runs well above the blob
/// size. A rough multiplier is enough here — the budget is a safety ceiling,
/// the count cap does the day-to-day work.
const RESIDENT_BYTES_PER_SNAPSHOT_BYTE: usize = 6;

/// Floor per open doc (room socket buffers, tasks) regardless of content size.
const DOC_RESIDENT_FLOOR_BYTES: usize = 512 * 1024;

/// A quiesce for a Cloud sleep the stop never followed lifts after this.
const QUIESCE_LEASE: std::time::Duration = std::time::Duration::from_secs(180);

/// Queued-attachment transfer pacing: chunk pushes are bounded per call (a
/// stalled-but-open relay link never fails on its own) and a timeout marks
/// the link suspect; attempts retry on this backoff, cut short by the online
/// bus / system wake.
const TRANSFER_CHUNK_B64: usize = 60_000;
const TRANSFER_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const TRANSFER_COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const TRANSFER_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(2);
const TRANSFER_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(30);
/// A command whose attachment bytes are still in transit waits at most this
/// long before the drain rejects it loudly (and the transfer task gives up on
/// the same clock) — a chat must never wedge behind bytes that aren't coming.
const ATTACHMENT_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const ATTACHMENT_WAIT_MAX_MS: i64 = ATTACHMENT_WAIT_MAX.as_millis() as i64;
/// Re-check cadence while a chat's queue is deferred on in-transit bytes
/// (the happy path is event-driven — UploadCommit kicks the drain — this
/// timer only covers the give-up transition and missed kicks).
const ATTACHMENT_WAIT_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

/// Transfer attempt outcome: transient failures retry (the link may heal),
/// permanent ones stop (the host actively refused, or the bytes are gone).
enum TransferError {
    Transient(String),
    Permanent(String),
}

#[derive(Debug, Clone)]
pub struct DocHostConfig {
    pub device_id: String,
    /// Harness for doc-command runs on chats without a workspace `config` row.
    pub default_harness: HarnessId,
    /// Where tool-output sidecar blobs go. `None` = no sidecars (local profiles, tests). Chats
    /// themselves never touch it: they sync between engines.
    pub edge: Option<EdgeConfig>,
}

struct DocHostInner {
    store: Arc<DocsStore>,
    config: DocHostConfig,
    /// Set-once (first wins), cleared by `shutdown_workers`: sessions and
    /// doc-host reference each other through Arcs, so a retired runtime's
    /// graph only drops once this back-edge is severed.
    sessions: Mutex<Option<SessionsEngine>>,
    /// Quiesced for a Cloud sleep ([`DocHost::quiesce_for_sleep`]): drains
    /// leave new work pending in the doc, for the machine once it wakes.
    quiesced: AtomicBool,
    /// Bumped per quiesce: only the newest one's lease lifts it.
    quiesce_generation: AtomicU64,
    /// Drains between their quiesce check and the end of their dispatch (a
    /// run they start is registered — and Working — by then).
    dispatching: Arc<AtomicUsize>,
    workspace: OnceLock<WorkspaceHost>,
    /// Worktree materialization for Run commands (see `set_repos`).
    repos: OnceLock<crate::repos::Repos>,
    project_action_runtime: OnceLock<(ProjectActionsStore, Terminals)>,
    /// Cancels every worker spawned through `spawn_worker` — the loops'
    /// own exit conditions (weak handle death, closed channels) don't cover
    /// runtime replacement, where Edge-capable tasks must stop doing
    /// network work even while something still pins the graph.
    shutdown: CancellationToken,
    /// Tracks every spawned worker so `shutdown_workers` can await them.
    tasks: TaskTracker,
    handles: Mutex<HashMap<String, Arc<ChatDocHandle>>>,
    document_loads: AtomicU64,
    focus_clock: AtomicU64,
    /// Serialize cold opens without blocking access to already-live handles.
    opening: Mutex<()>,
    /// Attachment-wait re-drain timers armed (one per chat): a command
    /// deferred on in-transit attachment bytes re-checks on a cadence, and
    /// each deferral must not stack another timer.
    drain_waiting: Mutex<HashSet<String>>,
    /// Uploads store (engine assembly) — resolves `pending://` attachment
    /// refs and jails transfer reads to the uploads dir.
    uploads: OnceLock<crate::uploads::Uploads>,
    /// Connectivity watch (`WatchConnectivity`): lazily-started monitor
    /// publishes the sync posture on change (see `watch_connectivity`).
    connectivity: OnceLock<watch::Sender<onicode_proto::Connectivity>>,
    connectivity_started: AtomicBool,
    /// In-flight queued-attachment transfers, published per landed chunk
    /// (see `watch_transfers`). Entries live exactly as long as bytes are
    /// moving: added when a file's push starts, removed on commit or failure
    /// (a retry re-adds), so consumers can render a real percent while the
    /// relay leg runs and fall back to indeterminate otherwise.
    transfers: watch::Sender<Vec<onicode_proto::TransferProgress>>,
    /// Command ids currently BETWEEN mark-processed and their resolution in a
    /// drain. Distinguishes "executing right now" from "consumed by the
    /// ledger but dead" (a crash between mark and resolve): the drain
    /// terminalizes the latter as Rejected instead of leaving a forever-
    /// Pending entry no retry could ever reach (2026-08-19 swallowed-send).
    executing: Mutex<HashSet<String>>,
    /// Links to the other engines, once the tailnet is up: chats follow their hosts over them,
    /// and queued attachment bytes travel to remote hosts.
    links: OnceLock<Arc<onicode_peer::PeerLinks>>,
    /// Shared client for sidecar blob PUT/GET (30s timeout, uploads.rs
    /// discipline — diff_sync's untimed client hung on dead links).
    http: reqwest::Client,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The queued-attachment transfers a command's `pending://` refs imply —
/// shared by the retry escort and the retry re-issue.
fn command_transfers(entry: &SessionCommandEntry) -> Vec<crate::uploads::AttachmentTransfer> {
    let refs: Vec<String> = match &entry.payload {
        SessionCommandPayload::Run { request, .. } => request
            .attachments
            .iter()
            .filter(|p| crate::uploads::is_pending_ref(p))
            .cloned()
            .collect(),
        SessionCommandPayload::Steer { prompt, .. } => crate::uploads::pending_refs_in(prompt),
        _ => Vec::new(),
    };
    refs.iter()
        .filter_map(|r| crate::uploads::parse_pending_ref(r))
        .map(
            |(upload_id, file_name)| crate::uploads::AttachmentTransfer {
                upload_id: upload_id.to_string(),
                file_name: file_name.to_string(),
            },
        )
        .collect()
}

/// Retires a transfer's progress entry on drop — the one exit point for
/// `push_attachments`' many returns (commit landed, chunk timeout, host
/// refusal), so no failure path can leave a phantom ring behind.
struct TransferProgressGuard<'a> {
    host: &'a DocHost,
    upload_id: &'a str,
}

impl Drop for TransferProgressGuard<'_> {
    fn drop(&mut self) {
        self.host.transfer_progress_clear(self.upload_id);
    }
}

#[derive(Clone)]
pub struct DocHost {
    inner: Arc<DocHostInner>,
}

/// A [`DocHost`] that does not keep it alive: what peer sessions and followers hold.
#[derive(Clone)]
pub(crate) struct WeakDocHost(Weak<DocHostInner>);

impl WeakDocHost {
    pub(crate) fn upgrade(&self) -> Option<DocHost> {
        self.0.upgrade().map(|inner| DocHost { inner })
    }
}

/// Who hosts a chat, as this engine's registry has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatHost {
    /// This engine (or no registry is wired: bare doc hosts host everything).
    Here,
    /// Another device.
    Peer(String),
    /// No row names a host yet.
    Unknown,
}

/// How a taken queue row reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueSend {
    /// Nothing is running: start a turn with it.
    NextTurn,
    /// Something is running and can take input mid-turn: steer it in.
    Steer,
    /// The user said now: stop what is running first.
    Interrupt,
}

const ATTACHMENT_ONLY_PROMPT: &str = "See the attached image(s).";
const ATTACHMENT_PROMPT_HEADER: &str = "Attached images (local files — open them to view):";

/// Queue rows keep the user's editable text separate from attachment paths.
/// Rebuild the transcript/harness transport only when the row is dispatched.
///
/// Older clients stored the already-expanded prompt in `text`; recognize the
/// exact trailer implied by `attachments` so those rows are not expanded a
/// second time after an upgrade.
fn queued_message_prompt(text: &str, attachments: &[String]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let refs = attachments
        .iter()
        .map(|path| format!("- {path}"))
        .collect::<Vec<_>>()
        .join("\n");
    let trailer = format!("\n\n{ATTACHMENT_PROMPT_HEADER}\n{refs}");
    let body = text.strip_suffix(&trailer).unwrap_or(text);
    let body = if body.trim().is_empty() {
        ATTACHMENT_ONLY_PROMPT
    } else {
        body
    };
    format!("{body}{trailer}")
}

fn queue_text_hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BeginQueueEditOutcome {
    Acquired {
        lease_id: String,
        text: String,
        attachments: Vec<String>,
        base_text_hash: String,
        expires_at_ms: i64,
    },
    Locked {
        owner_device_id: String,
        expires_at_ms: i64,
    },
    Missing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RenewQueueEditOutcome {
    Renewed { expires_at_ms: i64 },
    Lost,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishQueueEditAction {
    Commit,
    Cancel,
    Discard,
    ReleaseUnchanged,
}

/// What a finish applies to a leased queued message: the action and, for
/// `Commit`, the revised text checked against the hash it was edited from.
/// `attachments: None` keeps the item's attachments as they are.
#[derive(Debug, Clone, Copy)]
pub struct QueueEditFinish<'a> {
    pub action: FinishQueueEditAction,
    pub text: Option<&'a str>,
    pub expected_text_hash: Option<&'a str>,
    pub attachments: Option<&'a [String]>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FinishQueueEditOutcome {
    Committed,
    Cancelled,
    Discarded,
    Released,
    Conflict { current_text: String },
    Lost,
    Missing,
}

/// Content and its historical presentation cutoff travel atomically, even
/// when the watch coalesces several backfill and live commits.
#[derive(Clone, Default)]
pub struct TranscriptSnapshot {
    pub entries: Arc<Vec<SessionMessageEntry>>,
    pub replay_baseline: Arc<onicode_doc::TranscriptBaseline>,
}

/// One open chat doc: the `SessionDoc`, its change plumbing, and the room client.
pub struct ChatDocHandle {
    chat_id: String,
    device_id: String,
    doc: Arc<SessionDoc>,
    messages_tx: watch::Sender<TranscriptSnapshot>,
    /// Serialize historical imports with publication so an async doc-change
    /// task cannot publish recovered content before its presentation cutoff.
    transcript_import: Mutex<()>,
    transcript_history: Arc<Mutex<crate::transcript_history::TranscriptHistory>>,
    /// Pending-message queue watch (WatchQueue). Cheap to rebuild — a handful
    /// of short rows — so unlike the transcript mirror it publishes on every
    /// change without a dirty flag.
    queue_tx: watch::Sender<Vec<QueuedMessage>>,
    /// Serializes everything that TAKES from the queue. Both the doc-change
    /// task and the turn-end status watcher call `drain_queue`, and nothing
    /// keeps those two apart: without this they interleave across the
    /// `dispatch` await, each taking a different head and each sending, so a
    /// queue meant to release one message releases all of them.
    ///
    /// It also covers the gap a send-now's interrupt opens — between stopping
    /// the turn and starting its own the chat reads Idle, and an idle chat with
    /// a queue is exactly what the flush drains.
    drain_lock: tokio::sync::Mutex<()>,
    /// Serialize prompt commands while still allowing interrupt/input controls.
    command_drain_lock: tokio::sync::Mutex<()>,
    /// Queue rows held as explicit steers for a turn-boundary agent. They
    /// lead ordinary queued rows, in the order they were steered.
    steered_rows: Mutex<Vec<String>>,
    /// Sends held for a turn-boundary agent's turn end (rather than queued
    /// by the user), with the run configuration each was sent with: the row
    /// carries only its text and images.
    held_sends: Mutex<HashMap<String, onicode_proto::RunRequest>>,
    /// The latest Stop command that has acted. A prompt sent before it that
    /// only executes now (a loaded host, a slow agent) was cancelled by it:
    /// running it would start the stopped work again in a fresh runtime.
    stopped_by: Mutex<Option<String>>,
    /// An explicit user interrupt freezes automatic queue delivery. The next
    /// explicit prompt or queue send resumes it; incidental doc/status changes
    /// must not turn Cancel into "send the next row".
    queue_paused: AtomicBool,
    /// True when the doc changed while nobody watched: the mirror rebuild is
    /// deferred to the next `watch_messages` attach instead of paid per commit.
    mirror_dirty: AtomicBool,
    /// Epoch ms of the last open/watch touch — the LRU eviction key.
    last_access: AtomicI64,
    /// User navigation only. Opens, watches, writes and reconnects never bump it.
    last_focus: AtomicU64,
    /// Last known snapshot blob size — the eviction budget estimate's input.
    snapshot_bytes: AtomicUsize,
    writers: Arc<AtomicUsize>,
    pub(crate) persistence: Arc<crate::chat_persistence::ChatPersistence>,
    /// A local write could not reach the outbox: the doc stays in memory until a snapshot holding
    /// it lands (the persister clears this).
    publication_failed: Arc<AtomicBool>,
    /// Ticks on every change, local or imported: the change task and peer sessions watch it.
    changes: watch::Sender<u64>,
    /// A follower (`doc_server::follow`) runs for this chat.
    following: AtomicBool,
    /// A live session with the chat's host is up.
    host_live: AtomicBool,
    /// Cuts a follower's wait short (a retry, a link to the host coming up).
    kick: Arc<tokio::sync::Notify>,
    /// Local-update feed into the outbox (drop = unsubscribe).
    _outbox_sub: loro::Subscription,
    /// Doc subscription (drop = unsubscribe) — bumps the change watch on every commit.
    _sub: loro::Subscription,
}

/// One drain counted in [`DocHostInner::dispatching`] until dropped.
struct DispatchGuard(Arc<AtomicUsize>);
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct DocWriter {
    doc: Arc<SessionDoc>,
    writers: Arc<AtomicUsize>,
}
impl std::ops::Deref for DocWriter {
    type Target = SessionDoc;

    fn deref(&self) -> &Self::Target {
        &self.doc
    }
}
impl Drop for DocWriter {
    fn drop(&mut self) {
        self.writers.fetch_sub(1, Ordering::AcqRel);
    }
}
/// The provider the chat's own latest switch seam moved to: `None` when the
/// chat never switched, `Some(None)` for a seam naming no known harness.
/// Seams above a fork's seam were copied from the source chat and don't
/// count: the fork's own provider history starts there.
pub(crate) fn latest_switch(entries: &[SessionMessageEntry]) -> Option<Option<HarnessId>> {
    for entry in entries.iter().rev() {
        for part in &entry.parts {
            match part {
                MessagePart::Switch { to_harness, .. } => {
                    return Some(to_harness.as_deref().and_then(HarnessId::from_slug));
                }
                MessagePart::Fork { .. } => return None,
                _ => {}
            }
        }
    }
    None
}

impl ChatDocHandle {
    /// The turn ending now is followed at once by a message the user has
    /// already sent: one still on its way through the command log (a burst
    /// the agent outran), or one held for a turn-boundary agent's turn end.
    /// The work goes on, so the end is not a completion to notify.
    pub(crate) fn continues_with_sent_message(&self) -> bool {
        // Not yet in the transcript: delivery writes the user's message
        // before dispatching it, so the command that started the turn now
        // ending (still marked pending a moment longer) never counts.
        let in_flight = self.doc.read_commands().is_ok_and(|commands| {
            let pending: Vec<&str> = commands
                .iter()
                .filter(|c| c.status == SessionCommandStatus::Pending)
                .filter_map(|c| match &c.payload {
                    SessionCommandPayload::Run { message_id, .. } => Some(message_id.as_str()),
                    SessionCommandPayload::Steer { message_id, .. } => {
                        Some(message_id.as_deref().unwrap_or(c.id.as_str()))
                    }
                    _ => None,
                })
                .collect();
            !pending.is_empty()
                && self.doc.read_entries().is_ok_and(|entries| {
                    pending
                        .iter()
                        .any(|id| !entries.iter().any(|e| e.id == *id))
                })
        });
        if in_flight {
            return true;
        }
        if self.queue_paused.load(Ordering::Acquire) {
            return false;
        }
        let Ok(Some(head)) = self.doc.read_queue().map(|q| q.into_iter().next()) else {
            return false;
        };
        head.delivery_gate.is_none()
            && (lock(&self.held_sends).contains_key(&head.id)
                || lock(&self.steered_rows).contains(&head.id))
    }

    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    /// A receiver that ticks on every later change to the doc.
    pub(crate) fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub(crate) fn kick(&self) -> Arc<tokio::sync::Notify> {
        self.kick.clone()
    }

    pub(crate) fn set_following(&self, following: bool) {
        self.following.store(following, Ordering::Release);
    }

    pub(crate) fn set_host_live(&self, live: bool) {
        self.host_live.store(live, Ordering::Release);
    }

    /// Whether a live session with the chat's host is up (always false on the host itself).
    pub fn host_live(&self) -> bool {
        self.host_live.load(Ordering::Acquire)
    }

    pub(crate) fn commit_voice(
        &self,
        transcript: &onicode_proto::voice::VoiceTranscript,
    ) -> Result<Option<String>, EngineError> {
        let _owner = lock(&self.transcript_import);
        onicode_doc::voice::commit_voice_transcript(
            &self.doc,
            transcript,
            &self.device_id,
            chrono::Utc::now().timestamp_millis(),
        )
        .map_err(EngineError::from)
    }

    pub fn doc(&self) -> &SessionDoc {
        &self.doc
    }

    pub fn writer(&self) -> DocWriter {
        self.writers.fetch_add(1, Ordering::AcqRel);
        DocWriter {
            doc: self.doc.clone(),
            writers: self.writers.clone(),
        }
    }

    pub fn doc_arc(&self) -> Arc<SessionDoc> {
        self.doc.clone()
    }

    /// Joined transcript watch — re-sent on every doc change (WatchDocMessages).
    ///
    /// Attach-time refresh: the mirror is only maintained while watched, so a
    /// doc that changed unwatched materializes here, once, instead of on every
    /// commit it sat through in the background.
    pub fn watch_messages(&self) -> watch::Receiver<TranscriptSnapshot> {
        self.touch();
        // Subscribe BEFORE the dirty check: a commit racing this attach then
        // sees a live receiver and publishes, instead of re-marking dirty
        // after our refresh and leaving the new watcher a cleared mirror.
        let _import = lock(&self.transcript_import);
        let rx = {
            if self.messages_tx.receiver_count() == 0 {
                // A new viewing session must not inherit the former viewer's
                // live-part protection, even if no commit happened while away.
                *lock(&self.transcript_history) = Default::default();
            }
            self.messages_tx.subscribe()
        };
        if self.mirror_dirty.load(Ordering::Acquire) {
            self.publish_messages_locked();
        }
        rx
    }

    /// Queue watch — the composer's held messages, re-sent on every doc change.
    pub fn watch_queue(&self) -> watch::Receiver<Vec<QueuedMessage>> {
        self.touch();
        let rx = self.queue_tx.subscribe();
        self.publish_queue();
        rx
    }

    /// Where a newly steered row goes: after the rows already steered, ahead
    /// of every ordinary row. Records `id` as steered.
    fn steer_slot(&self, id: &str) -> Result<usize, DocError> {
        let mut steered = lock(&self.steered_rows);
        let queue = self.doc.read_queue()?;
        steered.retain(|row| queue.iter().any(|q| &q.id == row));
        let slot = queue
            .iter()
            .take_while(|row| steered.contains(&row.id))
            .count();
        steered.push(id.to_string());
        Ok(slot)
    }

    fn publish_queue(&self) {
        match self.doc.read_queue() {
            Ok(items) => {
                self.queue_tx.send_if_modified(|slot| {
                    if *slot == items {
                        false
                    } else {
                        *slot = items;
                        true
                    }
                });
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "queue read failed");
            }
        }
    }

    fn touch(&self) {
        self.last_access.store(now_ms(), Ordering::Relaxed);
    }

    /// Write a complete user message entry, idempotent by id (the client-minted message
    /// id — a re-executed command or optimistic echo never duplicates the entry).
    pub fn write_user_message(
        &self,
        message_id: &str,
        text: &str,
        created_at: i64,
    ) -> Result<(), DocError> {
        if self.doc.read_entries()?.iter().any(|e| e.id == message_id) {
            return Ok(());
        }
        self.doc.push_message(&SessionMessageEntry {
            id: message_id.to_string(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: text.to_string(),
            }],
            created_at,
            device_id: self.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            duration_ms: None,
        })
    }

    /// Record that the chat moved from provider `from` to provider `to`: a
    /// system entry carrying the [`MessagePart::Switch`] seam, plus the
    /// `providerSwitched` meta flag that makes the next fresh provider session
    /// owe the transcript. Idempotent against a retried dispatch: nothing is
    /// written while the chat's latest seam already points at `to`. Returns
    /// whether a seam was written.
    pub fn write_provider_switch(
        &self,
        from: (HarnessId, &str),
        to: (HarnessId, &str),
        created_at: i64,
    ) -> Result<bool, DocError> {
        let ((from_id, from), (to_id, to)) = (from, to);
        let entries = self.doc.read_entries()?;
        if entries.is_empty() {
            return Ok(false);
        }
        if latest_switch(&entries) == Some(Some(to_id)) {
            return Ok(false);
        }
        let id = format!("switch:{created_at}");
        // The send that switches wrote its message first: the seam goes
        // before it (and any other unanswered message), so it reads as the
        // new provider's question, not the old one's.
        let unanswered = entries
            .iter()
            .rev()
            .take_while(|entry| entry.role == MessageRole::User)
            .count();
        self.doc.insert_message_before_last(
            unanswered,
            &SessionMessageEntry {
                id: id.clone(),
                role: MessageRole::System,
                parts: vec![MessagePart::Switch {
                    id,
                    from: from.to_string(),
                    to: to.to_string(),
                    from_harness: Some(from_id.as_str().to_owned()),
                    to_harness: Some(to_id.as_str().to_owned()),
                }],
                created_at,
                device_id: self.device_id.clone(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
                duration_ms: None,
            },
        )?;
        self.doc.mark_provider_switched()?;
        Ok(true)
    }

    /// Recovery sweep: settle this device's running subagent chips (including
    /// chips in completed parent turns), then stamp abandoned `streaming`
    /// entries `aborted`, appending
    /// `note` as a visible error part so the transcript says WHY the turn
    /// ended (onicode folded "Run interrupted by backend restart" the same
    /// way). Returns the stamped entries' `(id, created_at)` — recovery uses
    /// them for the resume-freshness check.
    pub fn mark_abandoned_streams(&self, note: &str) -> Result<Vec<(String, i64)>, DocError> {
        let mut stamped = Vec::new();
        let mut chips_changed = false;
        for entry in self.doc.read_entries()? {
            if entry.role != MessageRole::Assistant || entry.device_id != self.device_id {
                continue;
            }
            for part in &entry.parts {
                if let MessagePart::Tool {
                    id,
                    subagent_status: Some(SubagentStatus::Running),
                    ..
                } = part
                {
                    chips_changed |=
                        self.doc
                            .update_subagent_chip(id, None, Some("failed"), None)?;
                }
            }
            if entry.status == Some(MessageStatus::Streaming)
                && self
                    .doc
                    .set_message_status(&entry.id, MessageStatus::Aborted)?
            {
                let part_id = format!("{}-recovery", entry.id);
                if let Err(err) = self.doc.append_error_part(&entry.id, &part_id, note) {
                    tracing::warn!(chat = %self.chat_id, error = %err, "recovery note append failed");
                }
                stamped.push((entry.id.clone(), entry.created_at));
            }
        }
        if chips_changed || !stamped.is_empty() {
            self.publish_messages();
        }
        Ok(stamped)
    }

    fn publish_messages(&self) {
        let _import = lock(&self.transcript_import);
        self.publish_messages_locked();
    }

    // Caller holds transcript_import, shared with attach and mirror clearing.
    fn publish_messages_locked(&self) {
        self.mirror_dirty.store(false, Ordering::Release);
        match self.doc.read_entries() {
            Ok(entries) => {
                let replay_baseline =
                    lock(&self.transcript_history).snapshot(self.doc.doc(), &entries);
                let joined = join_continuation_entries(entries);
                // send_replace: update the watch even with no subscribers yet, so a
                // late subscriber's first borrow sees the current transcript.
                self.messages_tx.send_replace(TranscriptSnapshot {
                    entries: Arc::new(joined),
                    replay_baseline: replay_baseline.clone(),
                });
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "transcript read failed");
            }
        }
    }

    pub(crate) fn import_transcript<T>(&self, import: impl FnOnce() -> T) -> T {
        let _guard = lock(&self.transcript_import);
        import()
    }

    /// Per-commit publish path: unwatched docs just mark the mirror dirty —
    /// rebuilding a full transcript nobody reads was a per-tick cost on every
    /// open doc (and kept a second transcript copy hot).
    fn publish_messages_if_watched(&self) {
        // Serialize the receiver check AND clear with attach. Otherwise an
        // unwatched worker can clear the mirror after a new watcher rebuilt it.
        let _import = lock(&self.transcript_import);
        if self.messages_tx.receiver_count() == 0 {
            self.mirror_dirty.store(true, Ordering::Release);
            // Shrink the stale mirror: watch_messages rebuilds on attach.
            self.messages_tx.send_replace(TranscriptSnapshot::default());
            *lock(&self.transcript_history) = Default::default();
        } else {
            self.publish_messages_locked();
        }
    }

    /// Rough resident cost for the LRU budget.
    fn resident_estimate(&self) -> usize {
        let bytes = self
            .snapshot_bytes
            .load(Ordering::Relaxed)
            .max(self.persistence.snapshot_bytes());
        (bytes * RESIDENT_BYTES_PER_SNAPSHOT_BYTE).max(DOC_RESIDENT_FLOOR_BYTES)
    }
}

impl DocHost {
    pub fn new(store: Arc<DocsStore>, config: DocHostConfig) -> Self {
        Self {
            inner: Arc::new(DocHostInner {
                store,
                config,
                sessions: Mutex::new(None),
                quiesced: AtomicBool::new(false),
                quiesce_generation: AtomicU64::new(0),
                dispatching: Arc::new(AtomicUsize::new(0)),
                workspace: OnceLock::new(),
                repos: OnceLock::new(),
                project_action_runtime: OnceLock::new(),
                shutdown: CancellationToken::new(),
                tasks: TaskTracker::new(),
                handles: Mutex::new(HashMap::new()),
                document_loads: AtomicU64::new(0),
                focus_clock: AtomicU64::new(0),
                opening: Mutex::new(()),
                drain_waiting: Mutex::new(HashSet::new()),
                uploads: OnceLock::new(),
                connectivity: OnceLock::new(),
                connectivity_started: AtomicBool::new(false),
                transfers: watch::channel(Vec::new()).0,
                executing: Mutex::new(HashSet::new()),
                links: OnceLock::new(),
                http: reqwest::Client::builder()
                    .pool_max_idle_per_host(2)
                    .pool_idle_timeout(std::time::Duration::from_secs(10))
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .read_timeout(std::time::Duration::from_secs(30))
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new()),
            }),
        }
    }

    /// A handle that does not keep this host alive.
    pub(crate) fn downgrade(&self) -> WeakDocHost {
        WeakDocHost(Arc::downgrade(&self.inner))
    }

    /// Serves the doc streams peers open to this engine (register on the stream router).
    pub fn doc_server(&self) -> Arc<dyn onicode_peer::StreamHandler> {
        Arc::new(crate::doc_server::DocServer(self.downgrade()))
    }

    pub(crate) fn store(&self) -> &Arc<DocsStore> {
        &self.inner.store
    }

    pub(crate) fn links(&self) -> Option<Arc<onicode_peer::PeerLinks>> {
        self.inner.links.get().cloned()
    }

    /// Every background task rides the tracker, raced against the shutdown
    /// token: the loops' own exits stay authoritative in normal operation;
    /// the token is the retirement override.
    pub(crate) fn spawn_worker(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let cancel = self.inner.shutdown.clone();
        self.inner.tasks.spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = fut => {}
            }
        });
    }

    /// `spawn_worker` for sites that pre-resolve a runtime handle (callers
    /// reachable from bare sync contexts, where `tasks.spawn` would panic).
    fn spawn_worker_on(
        &self,
        runtime: &tokio::runtime::Handle,
        fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let cancel = self.inner.shutdown.clone();
        self.inner.tasks.spawn_on(
            async move {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = fut => {}
                }
            },
            runtime,
        );
    }

    /// The sessions engine, once wired. `None` before assembly or after
    /// `shutdown_workers` — callers treat both as "executor unavailable".
    fn sessions(&self) -> Option<SessionsEngine> {
        lock(&self.inner.sessions).clone()
    }

    /// Entered by a drain that may start work; `None` while quiesced. Count
    /// first, check second (both SeqCst): a quiesce that set its flag then
    /// either sees this drain counted or this drain sees the flag.
    fn dispatch_guard(&self) -> Option<DispatchGuard> {
        self.inner.dispatching.fetch_add(1, Ordering::SeqCst);
        let guard = DispatchGuard(self.inner.dispatching.clone());
        (!self.inner.quiesced.load(Ordering::SeqCst)).then_some(guard)
    }

    /// Before a Cloud sleep stops this engine: start nothing the stop would
    /// cut off. Commands arriving from here on stay pending in their chat's
    /// room and run on the woken machine (its boot reconcile finds them).
    /// `Err(why)` when a turn is in flight or a command is already waiting:
    /// the sleep is declined and work resumes at once. A quiesce the stop
    /// never follows lifts itself after `QUIESCE_LEASE`.
    pub async fn quiesce_for_sleep(&self) -> Result<(), String> {
        let generation = self.inner.quiesce_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.quiesced.store(true, Ordering::SeqCst);
        // Drains already past their check finish dispatching first.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.inner.dispatching.load(Ordering::SeqCst) > 0 {
            if tokio::time::Instant::now() >= deadline {
                self.lift_quiesce();
                return Err("work is still being dispatched".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let running = self
            .sessions()
            .map_or(0, |sessions| sessions.active_count());
        let busy = if running > 0 {
            Some(format!("{running} chat(s) have a turn in flight"))
        } else if self.has_waiting_commands() {
            Some("a message is waiting to run".to_string())
        } else {
            None
        };
        if let Some(why) = busy {
            self.lift_quiesce();
            return Err(why);
        }
        let host = self.clone();
        self.spawn_worker(async move {
            tokio::time::sleep(QUIESCE_LEASE).await;
            if host.inner.quiesce_generation.load(Ordering::SeqCst) == generation {
                tracing::warn!(
                    "quiesced for a sleep that never stopped this engine; resuming work"
                );
                host.lift_quiesce();
            }
        });
        Ok(())
    }

    /// Start no new work until [`Self::lift_quiesce`]: a new version trying out
    /// an update, whose changes may yet be undone. Controls keep working, as
    /// they do in a quiesce, and what queues meanwhile runs when it's lifted.
    pub fn hold_work(&self) {
        self.inner.quiesced.store(true, Ordering::SeqCst);
    }

    /// End a quiesce and run everything that queued meanwhile.
    pub fn lift_quiesce(&self) {
        if !self.inner.quiesced.swap(false, Ordering::SeqCst) {
            return;
        }
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            let host = self.clone();
            self.spawn_worker(async move {
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
            });
        }
    }

    /// A hosted chat has a command no drain has executed yet.
    fn has_waiting_commands(&self) -> bool {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
        handles.iter().any(|handle| {
            self.is_host(&handle.chat_id)
                && handle.doc.read_commands().is_ok_and(|commands| {
                    commands
                        .iter()
                        .any(|c| c.status == SessionCommandStatus::Pending && !is_processed(&c.id))
                })
        })
    }

    /// Wire the sessions engine (engine assembly; see `SessionsEngine::set_doc_host`).
    pub fn set_sessions(&self, sessions: SessionsEngine) {
        let statuses = sessions.watch_sessions();
        {
            // First set wins (the OnceLock contract this slot replaced).
            let mut slot = lock(&self.inner.sessions);
            if slot.is_none() {
                *slot = Some(sessions);
            }
        }
        // Commands may already be pending in warm-opened docs.
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            let host = self.clone();
            self.spawn_worker(async move {
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
            });
        }
        self.spawn_queue_flush_watcher(statuses);
    }

    /// The turn-end hook for held messages. Doc changes drive `drain_queue` for
    /// everything else, but a turn ENDING is not a doc change the queue can
    /// see — so watch session status instead and re-drain every warm chat.
    /// `drain_queue` is a cheap no-op for empty queues and busy agents, which
    /// is why this can afford to be indiscriminate.
    fn spawn_queue_flush_watcher(
        &self,
        mut statuses: watch::Receiver<Vec<onicode_proto::Session>>,
    ) {
        let host = self.clone();
        self.spawn_worker(async move {
            while statuses.changed().await.is_ok() {
                let handles: Vec<_> = lock(&host.inner.handles).values().cloned().collect();
                for handle in handles {
                    host.drain_queue(&handle).await;
                }
            }
        });
    }

    /// Retire this host's workers (runtime replacement, e.g. sign-out): cancel
    /// and await every spawned task (peer sessions and followers among them),
    /// drop every open chat handle (ending the watcher streams), and sever the
    /// sessions back-edge so the replaced engine graph can actually drop. Idempotent.
    pub async fn shutdown_workers(&self) {
        self.inner.shutdown.cancel();
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
        // Snapshot open docs BEFORE releasing their handles: the handles map
        // holds the only strong doc refs, and an unflushed doc dies with it.
        let host = self.clone();
        if let Err(error) = tokio::task::spawn_blocking(move || host.flush_all()).await {
            tracing::error!(%error, "shutdown snapshot flush failed");
        }
        // Take the map under the lock, drop the handles outside it.
        let handles = std::mem::take(&mut *lock(&self.inner.handles));
        drop(handles);
        lock(&self.inner.sessions).take();
    }

    /// Freeze every open queue before settling live runs during shutdown.
    /// Interrupting a run publishes Idle, which normally wakes the turn-end
    /// queue drainer; without this barrier quitting the host could promote a
    /// queued row in the narrow window before workers are retired.
    pub fn pause_all_queues(&self) {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            handle.queue_paused.store(true, Ordering::Release);
        }
    }

    /// Test-only retirement sentinel: reports true once the doc-host graph
    /// has actually been freed.
    #[doc(hidden)]
    pub fn retirement_probe(&self) -> Box<dyn Fn() -> bool + Send + Sync> {
        let weak = Arc::downgrade(&self.inner);
        Box::new(move || weak.upgrade().is_none())
    }

    /// Wire the repos engine (engine assembly) — worktree materialization for
    /// Run commands carrying a [`onicode_proto::WorktreeSpec`].
    pub fn set_repos(&self, repos: crate::repos::Repos) {
        let _ = self.inner.repos.set(repos);
    }

    pub fn set_project_action_runtime(
        &self,
        project_actions: ProjectActionsStore,
        terminals: Terminals,
    ) {
        let _ = self
            .inner
            .project_action_runtime
            .set((project_actions, terminals));
    }

    /// Wire the uploads store (engine assembly) — `pending://` ref resolution
    /// and the transfer-read jail.
    pub fn set_uploads(&self, uploads: crate::uploads::Uploads) {
        let _ = self.inner.uploads.set(uploads);
    }

    /// Wire the links to the other engines (engine assembly, once the tailnet is up): open chats
    /// start following their hosts, chats owing a host forward whenever it is reachable, and
    /// queued attachment bytes travel to remote hosts.
    pub fn set_links(&self, links: Arc<onicode_peer::PeerLinks>) {
        if self.inner.links.set(links).is_err() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        for handle in &handles {
            self.spawn_follow(handle);
        }
        self.spawn_worker(crate::doc_server::forward(self.downgrade()));
    }

    /// Starts the chat's follower unless one runs (needs links and a runtime).
    fn spawn_follow(&self, handle: &Arc<ChatDocHandle>) {
        if self.inner.links.get().is_none() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if !handle.following.swap(true, Ordering::AcqRel) {
            self.spawn_worker(crate::doc_server::follow(
                self.downgrade(),
                Arc::downgrade(handle),
            ));
        }
    }

    /// Who hosts `chat_id`, as the registry has it.
    pub(crate) fn chat_host(&self, chat_id: &str) -> ChatHost {
        let Some(workspace) = self.workspace() else {
            return ChatHost::Here;
        };
        match workspace.chat(chat_id) {
            Ok(Some(chat)) if chat.device_id == self.inner.config.device_id => ChatHost::Here,
            Ok(Some(chat)) if !chat.device_id.is_empty() => ChatHost::Peer(chat.device_id),
            _ => ChatHost::Unknown,
        }
    }

    /// Whether this replica holds writes its (remote) host has not acked: an outbox row or a
    /// [`onicode_sync::FORWARD`] job.
    pub(crate) fn owes_host(&self, chat_id: &str) -> bool {
        let store = &self.inner.store;
        store.has_outbox(chat_id).unwrap_or(true)
            || !matches!(
                store.sync_job_version(chat_id, onicode_sync::FORWARD),
                Ok(None)
            )
    }

    /// A peer session imported ops. Ops that came from anyone but the host may be missing at the
    /// host, so a replica that does not host the chat owes them a forward.
    pub(crate) fn imported_from(
        &self,
        handle: &ChatDocHandle,
        peer: &str,
        status: &loro::ImportStatus,
    ) {
        let news = status.success.iter().any(|(_, &(start, end))| end > start);
        if !news {
            return;
        }
        // News from a mailbox, not the host: it owes the host until the host acks it.
        if let ChatHost::Peer(host) = self.chat_host(&handle.chat_id)
            && host != peer
            && let Err(error) = self
                .inner
                .store
                .schedule_sync_job(&handle.chat_id, onicode_sync::FORWARD)
        {
            tracing::warn!(chat = %handle.chat_id, %error, "forward job failed");
        }
    }

    /// `peer` acked holding `vv`. Only the host's ack retires the outbox and settles a forward:
    /// a mailbox holding our writes is not the host executing them.
    pub(crate) fn peer_acked(&self, handle: &ChatDocHandle, peer: &str, vv: &loro::VersionVector) {
        if self.chat_host(&handle.chat_id) != ChatHost::Peer(peer.to_owned()) {
            return;
        }
        let chat_id = &handle.chat_id;
        let store = &self.inner.store;
        // Read the job first: a forward owed after this point keeps a newer version.
        let job = store.sync_job_version(chat_id, onicode_sync::FORWARD);
        if let Err(error) = store.retire_outbox(chat_id, vv) {
            tracing::warn!(chat = %chat_id, %error, "outbox retire failed");
        }
        if let Ok(Some(version)) = job
            && vv.includes_vv(&handle.doc.doc().oplog_vv())
            && let Err(error) = store.complete_sync_job(chat_id, onicode_sync::FORWARD, version)
        {
            tracing::warn!(chat = %chat_id, %error, "forward job completion failed");
        }
    }

    /// Replaces the chat's doc with a peer's snapshot (this replica was behind the peer's shallow
    /// root). This replica's own writes carry over: its unacked outbox, then its unresolved
    /// commands. The handle is dropped; the next open reads the adopted doc.
    pub(crate) fn adopt_snapshot(
        &self,
        handle: &Arc<ChatDocHandle>,
        snapshot: &[u8],
    ) -> Result<(), String> {
        let chat_id = handle.chat_id.as_str();
        let store = &self.inner.store;
        let raw = loro::LoroDoc::from_snapshot(snapshot).map_err(|e| e.to_string())?;
        for row in store.outbox(chat_id).map_err(|e| e.to_string())? {
            // Ops resting on history the snapshot trimmed cannot apply; the commands below
            // still carry the sends among them.
            if let Err(error) = raw.import(&row.bytes) {
                tracing::debug!(chat = %chat_id, %error, "outbox row does not apply to the adopted doc");
            }
        }
        let adopted = SessionDoc::from_doc(raw);
        let present: HashSet<String> = adopted
            .read_commands()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|command| command.id)
            .collect();
        for command in handle.doc.read_commands().unwrap_or_default() {
            if command.status == SessionCommandStatus::Pending
                && command.issued_by == self.inner.config.device_id
                && !present.contains(&command.id)
            {
                adopted.queue_command(&command).map_err(|e| e.to_string())?;
            }
        }
        let bytes = adopted.export_snapshot().map_err(|e| e.to_string())?;
        handle.persistence.retire();
        store
            .adopt_snapshot(chat_id, &bytes)
            .map_err(|e| e.to_string())?;
        let mut handles = lock(&self.inner.handles);
        if handles
            .get(chat_id)
            .is_some_and(|open| Arc::ptr_eq(open, handle))
        {
            handles.remove(chat_id);
        }
        tracing::info!(chat = %chat_id, "adopted a peer's snapshot");
        Ok(())
    }

    /// Re-evaluate every open chat's commands and queue NOW. Called after an
    /// upload commit lands bytes on this device: a Run or queued row deferred
    /// on those bytes (`pending://` refs not yet on disk) becomes executable
    /// the moment its transfer completes — event-driven, not timer luck.
    pub fn kick_drains(&self) {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            let host = self.clone();
            self.spawn_worker(async move {
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
            });
        }
    }

    /// Wire the workspace host (engine assembly) — the source of chat-ownership rows.
    pub fn set_workspace(&self, workspace: WorkspaceHost) {
        let _ = self.inner.workspace.set(workspace);
    }

    /// The workspace host, once wired (tests may assemble a DocHost without one).
    pub fn workspace(&self) -> Option<&WorkspaceHost> {
        self.inner.workspace.get()
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    /// Open (or return) the chat's doc handle: load the local snapshot (or init fresh),
    /// start the change-driven task, and follow the chat's host when another engine hosts it.
    pub fn open(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        let handle = self.open_local(chat_id)?;
        self.spawn_follow(&handle);
        Ok(handle)
    }

    /// Explicit viewport navigation; automatic watch retries and MCP reads use
    /// `open` instead. The sequence also orders focuses in the same millisecond.
    pub fn focus_chat(&self, chat_id: &str) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        let focus = self.inner.focus_clock.fetch_add(1, Ordering::AcqRel) + 1;
        handle.last_focus.fetch_max(focus, Ordering::AcqRel);
        handle.kick.notify_one();
        Ok(())
    }

    /// Materialize one authoritative local document without touching the network. The outbox
    /// feed is installed before any writer can reach the doc.
    pub fn open_local(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        if let Some(handle) = lock(&self.inner.handles).get(chat_id) {
            handle.touch();
            return Ok(handle.clone());
        }
        let opening = lock(&self.inner.opening);
        if let Some(handle) = lock(&self.inner.handles).get(chat_id) {
            handle.touch();
            return Ok(handle.clone());
        }
        self.inner.document_loads.fetch_add(1, Ordering::Relaxed);
        let mut snapshot_len = 0usize;
        let raw = loro::LoroDoc::new();
        let doc = match self.inner.store.load_snapshot(chat_id)? {
            Some(bytes) => {
                snapshot_len = bytes.len();
                raw.import(&bytes)
                    .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                SessionDoc::from_doc(raw)
            }
            None => {
                // The base is saved before any write: the outbox holds updates past it, which
                // replay into nothing on a doc whose own init ops were lost in a crash.
                let doc = SessionDoc::init(chat_id)?;
                let base = doc
                    .doc()
                    .export(loro::ExportMode::Snapshot)
                    .map_err(|e| EngineError::Other(format!("snapshot export failed: {e}")))?;
                self.inner.store.save_snapshot(chat_id, &base)?;
                doc
            }
        };
        // Writes the snapshot debounce never saved (a crash) are still in the outbox. Imported
        // updates do not echo as local writes, so replaying them enqueues nothing.
        for row in self.inner.store.outbox(chat_id)? {
            doc.doc()
                .import(&row.bytes)
                .map_err(|e| EngineError::Other(format!("outbox replay failed: {e}")))?;
        }
        let doc = Arc::new(doc);
        let publication_failed = Arc::new(AtomicBool::new(false));
        let is_host: crate::chat_persistence::IsHost = {
            let host = self.downgrade();
            let chat = chat_id.to_string();
            Box::new(move || {
                host.upgrade()
                    .is_some_and(|host| host.chat_host(&chat) == ChatHost::Here)
            })
        };
        let persistence = crate::chat_persistence::ChatPersistence::new(
            &doc,
            self.inner.store.clone(),
            chat_id.to_string(),
            is_host,
            publication_failed.clone(),
        );
        let changed_persistence = persistence.clone();

        let (changes, changed_rx) = watch::channel(0u64);
        let changed_tx = changes.clone();
        let (messages_tx, _) = watch::channel(TranscriptSnapshot::default());
        let transcript_history = Arc::new(Mutex::new(
            crate::transcript_history::TranscriptHistory::default(),
        ));
        let history = transcript_history.clone();
        let watched = messages_tx.clone();
        let weak_doc = Arc::downgrade(&doc);
        let sub = doc.doc().subscribe_root(Arc::new(move |diff| {
            // This callback runs before the change worker can publish. The
            // import origin belongs to the event, so concurrent local commits
            // cannot inherit a remote replay's presentation classification.
            if watched.receiver_count() > 0 {
                if let Some(doc) = weak_doc.upgrade() {
                    lock(&history).observe(doc.doc(), &diff);
                }
            } else {
                *lock(&history) = Default::default();
            }
            changed_persistence.dirty(false);
            changed_tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        // Every local write goes to the outbox before anything else sees it: it survives a crash
        // before the next snapshot, and stays owed until the host acks a version holding it.
        let outbox_sub = {
            let store = self.inner.store.clone();
            let chat = chat_id.to_string();
            let failed = publication_failed.clone();
            doc.doc()
                .subscribe_local_update(Box::new(move |bytes: &Vec<u8>| {
                    // The blob names its own end, so the doc is not re-entered from its hook.
                    let enqueued = loro::LoroDoc::decode_import_blob_meta(bytes, false)
                        .map_err(|e| e.to_string())
                        .and_then(|meta| {
                            store
                                .enqueue_outbox(&chat, bytes, &meta.partial_end_vv)
                                .map_err(|e| e.to_string())
                        });
                    if let Err(error) = enqueued {
                        failed.store(true, Ordering::Release);
                        tracing::error!(%chat, %error, "outbox write failed; the doc stays in memory");
                    }
                    true
                }))
        };
        // The mirror starts dirty and empty; watch_messages materializes it
        // once on attach instead of maintaining an unwatched transcript.
        let initial_queue = doc.read_queue().unwrap_or_default();
        // A queue already present when a handle is materialized came from a
        // persisted snapshot (or a synced replica), not from a prompt the
        // user submitted to this live engine. Keep that recovered work frozen
        // until an explicit prompt / Send now / Steer action thaws it. Rows
        // appended after the handle exists retain the normal automatic drain.
        let recovered_queue_pending = !initial_queue.is_empty();
        let (queue_tx, _) = watch::channel(initial_queue);

        let handle = Arc::new(ChatDocHandle {
            chat_id: chat_id.to_string(),
            device_id: self.inner.config.device_id.clone(),
            doc: doc.clone(),
            messages_tx,
            transcript_import: Mutex::default(),
            transcript_history,
            queue_tx,
            drain_lock: tokio::sync::Mutex::new(()),
            command_drain_lock: tokio::sync::Mutex::new(()),
            steered_rows: Mutex::new(Vec::new()),
            held_sends: Mutex::new(HashMap::new()),
            stopped_by: Mutex::new(None),
            queue_paused: AtomicBool::new(recovered_queue_pending),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            last_focus: AtomicU64::new(0),
            snapshot_bytes: AtomicUsize::new(snapshot_len),
            writers: Arc::new(AtomicUsize::new(0)),
            persistence,
            publication_failed,
            changes,
            following: AtomicBool::new(false),
            host_live: AtomicBool::new(false),
            kick: Arc::new(tokio::sync::Notify::new()),
            _outbox_sub: outbox_sub,
            _sub: sub,
        });
        // Snapshot recovery may restore several independently edited rows.
        // Each row needs its own checked expiry wake; otherwise a non-head
        // edit could remain displayed as live indefinitely.
        self.arm_existing_queue_edit_expiries(&handle);
        // Publish only after the outbox feed is installed.
        lock(&self.inner.handles).insert(chat_id.to_string(), handle.clone());
        drop(opening);
        self.spawn_worker(chat_task(self.clone(), Arc::downgrade(&handle), changed_rx));
        self.evict_over_budget();
        Ok(handle)
    }

    /// LRU eviction: while the warm set exceeds [`WARM_DOC_CAP`] or the
    /// resident estimate exceeds `DOC_LRU_BYTE_BUDGET`, close the
    /// least-recently-touched unpinned docs. Pinned (never evicted):
    /// - watched docs (`messages_tx` has receivers — a UI transcript);
    /// - docs with a live writer (`Arc<SessionDoc>` held outside the handle — a run streaming into
    ///   it);
    /// - host-side docs with pending commands (the executor owes them work).
    ///
    /// Eviction flushes a final snapshot, so reopen loses nothing; missed
    /// remote updates re-arrive through the room join's VV backfill.
    fn evict_over_budget(&self) {
        // Do not let a cold reopen race the retiring handle's final flush.
        let _opening = lock(&self.inner.opening);
        let mut by_age: Vec<(i64, String)> = {
            let handles = lock(&self.inner.handles);
            handles
                .values()
                .map(|h| (h.last_access.load(Ordering::Relaxed), h.chat_id.clone()))
                .collect()
        };
        by_age.sort_unstable();
        for (_, chat_id) in by_age {
            let (count, estimate) = {
                let handles = lock(&self.inner.handles);
                (
                    handles.len(),
                    handles
                        .values()
                        .map(|h| h.resident_estimate())
                        .sum::<usize>(),
                )
            };
            if count <= WARM_DOC_CAP && estimate <= onicode_doc::DOC_LRU_BYTE_BUDGET {
                return;
            }
            let evicted = {
                let mut handles = lock(&self.inner.handles);
                match handles.get(&chat_id) {
                    Some(handle) if !self.pinned(handle) && Arc::strong_count(handle) == 1 => {
                        handles.remove(&chat_id)
                    }
                    _ => None,
                }
            };
            if let Some(handle) = evicted {
                // Final flush outside the map lock; ≤1s of changes could be
                // pending in the snapshot debounce.
                self.save_snapshot(&handle);
                tracing::debug!(chat = %handle.chat_id, "doc evicted (LRU)");
            }
        }
    }

    fn pinned(&self, handle: &Arc<ChatDocHandle>) -> bool {
        // The outbox outlives this handle. Only a failed disk write requires
        // retaining the in-memory copy until a snapshot lands.
        if handle.publication_failed.load(Ordering::Acquire) {
            return true;
        }
        // Delivering to the host right now: stay until the host has it all,
        // or the forward that opened this chat would stop half way.
        if handle.host_live() && self.owes_host(&handle.chat_id) {
            return true;
        }
        if handle.messages_tx.receiver_count() > 0 || handle.queue_tx.receiver_count() > 0 {
            return true;
        }
        // The handle itself holds one doc ref; more means a live writer.
        if Arc::strong_count(&handle.doc) > 1 {
            return true;
        }
        if self.is_host(&handle.chat_id) {
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            match handle.doc.read_commands() {
                Ok(commands) => commands
                    .iter()
                    .any(|c| c.status == SessionCommandStatus::Pending && !is_processed(&c.id)),
                // Unreadable ledger: keep the doc, never evict blind.
                Err(_) => true,
            }
        } else {
            false
        }
    }

    /// The in-flight queued-attachment transfer set: current entries first,
    /// then a fresh snapshot per landed chunk (see `push_attachments`).
    pub fn watch_transfers(&self) -> watch::Receiver<Vec<onicode_proto::TransferProgress>> {
        self.inner.transfers.subscribe()
    }

    /// Publish one transfer's progress (upserted by uploadId).
    fn transfer_progress_set(&self, upload_id: &str, file_name: &str, done: u64, total: u64) {
        self.inner.transfers.send_modify(|list| {
            match list.iter_mut().find(|t| t.upload_id == upload_id) {
                Some(t) => {
                    t.done = done;
                    t.total = total;
                }
                None => list.push(onicode_proto::TransferProgress {
                    upload_id: upload_id.to_string(),
                    file_name: file_name.to_string(),
                    done,
                    total,
                }),
            }
        });
    }

    /// Retire a transfer's progress entry (commit landed, or the attempt
    /// failed and the retry will re-publish).
    fn transfer_progress_clear(&self, upload_id: &str) {
        self.inner.transfers.send_modify(|list| {
            list.retain(|t| t.upload_id != upload_id);
        });
    }

    /// The connectivity stream: current posture first, then every change.
    /// Lazily starts a monitor — a 1s recompute over in-memory stats
    /// (atomics + small locks), published only when the value changes. The
    /// retry countdown renders client-side from `retry_at_ms`, so quiet
    /// periods emit nothing at all.
    pub fn watch_connectivity(&self) -> watch::Receiver<onicode_proto::Connectivity> {
        let tx = self
            .inner
            .connectivity
            .get_or_init(|| watch::channel(self.compute_connectivity()).0);
        let rx = tx.subscribe();
        if !self.inner.connectivity_started.swap(true, Ordering::SeqCst)
            && tokio::runtime::Handle::try_current().is_ok()
        {
            let host = self.clone();
            self.spawn_worker(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let next = host.compute_connectivity();
                    if let Some(tx) = host.inner.connectivity.get() {
                        tx.send_if_modified(|cur| {
                            if *cur == next {
                                false
                            } else {
                                *cur = next;
                                true
                            }
                        });
                    }
                }
            });
        }
        rx
    }

    /// One snapshot of the sync posture: `Disabled` (the default) on a local
    /// profile; else `Offline` with no network path, `Reconnecting` until the
    /// tailnet is up, `Connected` after — with each open chat's standing with
    /// its host.
    fn compute_connectivity(&self) -> onicode_proto::Connectivity {
        use onicode_proto::{ChatConnectivity, Connectivity, ConnectivityState};
        if self.workspace().is_none_or(WorkspaceHost::local_only) {
            return Connectivity::default();
        }
        let mut handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        handles.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        let chats = handles
            .iter()
            .map(|handle| {
                let here = self.chat_host(&handle.chat_id) == ChatHost::Here;
                let live = here || handle.host_live();
                ChatConnectivity {
                    chat_id: handle.chat_id.clone(),
                    sync_state: self.chat_sync_state_for(handle),
                    connected: live,
                    delivery_live: live,
                    pending_pushes: if here {
                        0
                    } else {
                        self.inner.store.outbox_len(&handle.chat_id).unwrap_or(0)
                    },
                }
            })
            .collect();
        let state = if onicode_sync::wake::path_is_offline() {
            ConnectivityState::Offline
        } else if self.inner.links.get().is_none() {
            ConnectivityState::Reconnecting
        } else {
            ConnectivityState::Connected
        };
        Connectivity {
            state,
            retry_at_ms: 0,
            last_failure: None,
            chats,
        }
    }

    pub fn chat_sync_state(&self, chat_id: &str) -> onicode_proto::ChatSyncState {
        let handle = lock(&self.inner.handles).get(chat_id).cloned();
        handle.map_or(onicode_proto::ChatSyncState::Local, |handle| {
            self.chat_sync_state_for(&handle)
        })
    }

    fn chat_sync_state_for(&self, handle: &ChatDocHandle) -> onicode_proto::ChatSyncState {
        use onicode_proto::ChatSyncState as S;
        if handle.publication_failed.load(Ordering::Acquire) {
            return S::StorageError;
        }
        match self.chat_host(&handle.chat_id) {
            // The host's doc is the chat's truth.
            ChatHost::Here => S::Synced,
            ChatHost::Unknown => S::Waiting,
            ChatHost::Peer(_) if !handle.host_live() => S::Offline,
            ChatHost::Peer(_) if self.inner.store.has_outbox(&handle.chat_id).unwrap_or(true) => {
                S::Connecting
            }
            ChatHost::Peer(_) => S::Synced,
        }
    }

    /// What keeps docs open and what sync still owes, for `SyncStatus`.
    pub fn sync_resources(&self) -> serde_json::Value {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        let count = |keep: &dyn Fn(&ChatDocHandle) -> bool| {
            handles.iter().filter(|handle| keep(handle)).count()
        };
        serde_json::json!({
            "openDocuments": handles.len(),
            "documentLoads": self.inner.document_loads.load(Ordering::Relaxed),
            "retainedBy": {
                "views": count(&|h| h.messages_tx.receiver_count() > 0 || h.queue_tx.receiver_count() > 0),
                "writers": count(&|h| h.writers.load(Ordering::Acquire) > 0),
                "storageFailures": count(&|h| h.publication_failed.load(Ordering::Acquire)),
            },
            "followingHosts": count(&|h| h.host_live()),
            "durable": self.inner.store.sync_work_counts().ok().map(|(writes, jobs)| {
                serde_json::json!({"unackedWrites": writes, "pendingJobs": jobs})
            }),
        })
    }

    /// Each open chat's standing with its host, for `SyncStatus` / `onicode sync`.
    pub fn sync_statuses(&self) -> Vec<serde_json::Value> {
        let mut handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        handles.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        handles
            .iter()
            .map(|handle| {
                let host = match self.chat_host(&handle.chat_id) {
                    ChatHost::Here => Some(self.inner.config.device_id.clone()),
                    ChatHost::Peer(device) => Some(device),
                    ChatHost::Unknown => None,
                };
                serde_json::json!({
                    "chatId": handle.chat_id,
                    "host": host,
                    "hostLive": handle.host_live(),
                    "state": self.chat_sync_state_for(handle),
                    "unackedWrites": self.inner.store.outbox_len(&handle.chat_id).unwrap_or(0),
                })
            })
            .collect()
    }

    /// Drop a chat's doc unconditionally and delete its local snapshot — the
    /// chat is gone (DeleteChat / DeleteSpace cascade). Watchers see the
    /// stream end; a racing writer keeps its orphaned doc until the run ends.
    pub fn purge_chat(&self, chat_id: &str) {
        let removed = lock(&self.inner.handles).remove(chat_id);
        drop(removed);
        if let Err(err) = self.inner.store.delete_snapshot(chat_id) {
            tracing::warn!(chat = %chat_id, error = %err, "snapshot delete failed");
        }
    }

    /// Composer path: append an immutable pending command entry (rule 1). Durable by
    /// construction — the change subscription kicks the drain, so a local host executes
    /// immediately and an offline doc simply holds the entry until it syncs.
    pub fn queue_command(
        &self,
        chat_id: &str,
        payload: SessionCommandPayload,
    ) -> Result<String, EngineError> {
        self.queue_command_with_transfers(chat_id, payload, Vec::new())
    }

    /// [`Self::queue_command`] plus queued-attachment transfers: the command's
    /// `pending://` refs name bytes already committed to THIS device's uploads
    /// dir; when another device hosts the chat, a background task pushes them
    /// over the peer link (retry until they land) while the command is
    /// already durably queued. The send is a local write — attachment bytes
    /// chase it, never gate it (2026-08-19 incident).
    pub fn queue_command_with_transfers(
        &self,
        chat_id: &str,
        payload: SessionCommandPayload,
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    ) -> Result<String, EngineError> {
        let handle = self.open(chat_id)?;
        let id = new_id();
        let now = now_ms();
        let based_on = handle.doc.read_entries()?.last().map(|m| CommandBasedOn {
            turn_id: Some(m.id.clone()),
            frontier: None,
        });
        let is_message = matches!(
            payload,
            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
        );
        let entry = SessionCommandEntry {
            id: id.clone(),
            payload,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now,
            based_on,
            expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
            status: SessionCommandStatus::Pending,
            resolution: None,
        };
        handle.doc.queue_command(&entry)?;
        // Sending a message revives an archived chat: the user is acting in it
        // again, so the LWW row flips back to active on every device. Best-
        // effort — the command itself is durable regardless.
        if is_message {
            self.unarchive_on_send(chat_id);
        }
        // The command reaches a remote host with the doc; its attachment
        // bytes travel beside it.
        self.spawn_attachment_delivery(chat_id, transfers);
        Ok(id)
    }

    /// A send revives an archived chat on every device (best-effort).
    fn unarchive_on_send(&self, chat_id: &str) {
        let Some(workspace) = self.workspace() else {
            return;
        };
        match workspace.chat(chat_id) {
            Ok(Some(chat)) if chat.archived => {
                if let Err(err) = workspace.set_chat_archived(chat_id, false) {
                    tracing::warn!(chat = %chat_id, error = %err, "unarchive on send failed");
                }
            }
            _ => {}
        }
    }

    /// Hold a message for later: append it to the doc's queue. Any device may
    /// write here (unlike `messages`), and the change subscription kicks
    /// [`Self::drain_queue`], so a queue that lands while the agent is already
    /// idle goes straight out instead of waiting for a turn that never comes.
    pub fn queue_message(
        &self,
        chat_id: &str,
        text: &str,
        attachments: Vec<String>,
    ) -> Result<String, EngineError> {
        self.queue_message_with_behavior(chat_id, text, attachments, false)
    }

    /// Append a message while preserving the submitter's active-turn policy
    /// on the synchronized row. This matters when another device hosts the
    /// chat: the host, not the submitting UI, decides when to drain it.
    pub fn queue_message_with_behavior(
        &self,
        chat_id: &str,
        text: &str,
        attachments: Vec<String>,
        hold_for_turn_end: bool,
    ) -> Result<String, EngineError> {
        let handle = self.open(chat_id)?;
        let id = new_id();
        handle.doc.push_queued(&QueuedMessage {
            id: id.clone(),
            text: text.to_string(),
            attachments,
            hold_for_turn_end,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now_ms(),
            edited_at: None,
            delivery_gate: None,
        })?;
        handle.publish_queue();
        // Same reasoning as a command: the user is acting in this chat again.
        self.unarchive_on_send(chat_id);
        Ok(id)
    }

    /// Retype a queued message. Empty text deletes the row — emptying the box
    /// is how you say "drop it". `false` when the row is already gone.
    pub fn update_queued_message(
        &self,
        chat_id: &str,
        id: &str,
        text: &str,
    ) -> Result<bool, EngineError> {
        let handle = self.open(chat_id)?;
        let changed = handle.doc.set_queued_text(id, text, now_ms())?;
        if changed {
            handle.publish_queue();
        }
        Ok(changed)
    }

    /// Reorder a queued message (drag, or the up/down buttons).
    pub fn move_queued_message(
        &self,
        chat_id: &str,
        id: &str,
        to_index: usize,
    ) -> Result<bool, EngineError> {
        let handle = self.open(chat_id)?;
        let changed = handle.doc.move_queued(id, to_index)?;
        if changed {
            handle.publish_queue();
        }
        Ok(changed)
    }

    /// Cancel one queued message at the chat host. Removal shares the same
    /// lock as automatic and explicit delivery, so the acknowledgement is the
    /// linearization point: `true` guarantees this host did not take the row.
    pub async fn remove_queued_message(
        &self,
        chat_id: &str,
        id: &str,
    ) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        let removed = handle.doc.remove_queued(id)?;
        if removed {
            handle.publish_queue();
        }
        Ok(removed)
    }

    /// Acquire the host-side right to edit one queued row. This operation and
    /// every queue take share `drain_lock`, making the ACK the linearization
    /// point: after Acquired the row cannot race into the agent.
    pub async fn begin_queued_message_edit(
        &self,
        chat_id: &str,
        id: &str,
        owner_device_id: &str,
        owner_instance_id: &str,
    ) -> Result<BeginQueueEditOutcome, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        let now = now_ms();
        let Some(item) = handle
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == id)
        else {
            return Ok(BeginQueueEditOutcome::Missing);
        };
        if let Some(QueueDeliveryGate::Editing {
            owner_device_id,
            expires_at_ms,
            ..
        }) = &item.delivery_gate
            && *expires_at_ms > now
        {
            return Ok(BeginQueueEditOutcome::Locked {
                owner_device_id: owner_device_id.clone(),
                expires_at_ms: *expires_at_ms,
            });
        }

        let lease_id = new_id();
        let expires_at_ms = now + QUEUE_EDIT_LEASE_MS;
        let gate = QueueDeliveryGate::Editing {
            lease_id: lease_id.clone(),
            owner_device_id: owner_device_id.to_string(),
            owner_instance_id: owner_instance_id.to_string(),
            acquired_at_ms: now,
            expires_at_ms,
            base_text_hash: queue_text_hash(&item.text),
        };
        let base_text_hash = queue_text_hash(&item.text);
        if !handle.doc.set_queued_delivery_gate(id, Some(&gate))? {
            return Ok(BeginQueueEditOutcome::Missing);
        }
        handle.publish_queue();
        // A crash immediately after the client sees Acquired must not reopen
        // the row as sendable from a pre-lease snapshot.
        self.save_snapshot(&handle);
        self.arm_queue_edit_expiry(&handle, id, &lease_id, expires_at_ms);
        Ok(BeginQueueEditOutcome::Acquired {
            lease_id,
            text: item.text,
            attachments: item.attachments,
            base_text_hash,
            expires_at_ms,
        })
    }

    /// Extend an edit lease. An already-expired generation is never revived;
    /// it remains blocked and will be surfaced as ReviewRequired.
    pub async fn renew_queued_message_edit(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
    ) -> Result<RenewQueueEditOutcome, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        let now = now_ms();
        let Some(item) = handle
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == id)
        else {
            return Ok(RenewQueueEditOutcome::Missing);
        };
        let QueueDeliveryGate::Editing {
            lease_id: current,
            owner_device_id,
            owner_instance_id,
            acquired_at_ms,
            expires_at_ms,
            base_text_hash,
        } = item
            .delivery_gate
            .unwrap_or(QueueDeliveryGate::ReviewRequired {
                previous_lease_id: String::new(),
                owner_device_id: String::new(),
                since_ms: now,
                base_text_hash: String::new(),
            })
        else {
            return Ok(RenewQueueEditOutcome::Lost);
        };
        if current != lease_id || expires_at_ms <= now {
            if current == lease_id && expires_at_ms <= now {
                let review = QueueDeliveryGate::ReviewRequired {
                    previous_lease_id: current,
                    owner_device_id,
                    since_ms: now,
                    base_text_hash,
                };
                let _ = handle.doc.set_queued_delivery_gate(id, Some(&review));
                handle.publish_queue();
                self.save_snapshot(&handle);
            }
            return Ok(RenewQueueEditOutcome::Lost);
        }
        let expires_at_ms = now + QUEUE_EDIT_LEASE_MS;
        let renewed = QueueDeliveryGate::Editing {
            lease_id: current,
            owner_device_id,
            owner_instance_id,
            acquired_at_ms,
            expires_at_ms,
            base_text_hash,
        };
        if !handle.doc.set_queued_delivery_gate(id, Some(&renewed))? {
            return Ok(RenewQueueEditOutcome::Missing);
        }
        handle.publish_queue();
        self.arm_queue_edit_expiry(&handle, id, lease_id, expires_at_ms);
        Ok(RenewQueueEditOutcome::Renewed { expires_at_ms })
    }

    /// Resolve an edit lease. A late finish may still resolve the matching
    /// ReviewRequired generation, but can never affect a newer lease.
    pub async fn finish_queued_message_edit(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        action: FinishQueueEditAction,
        text: Option<&str>,
        expected_text_hash: Option<&str>,
    ) -> Result<FinishQueueEditOutcome, EngineError> {
        self.finish_queued_message_edit_with_attachments(
            chat_id,
            id,
            lease_id,
            QueueEditFinish {
                action,
                text,
                expected_text_hash,
                attachments: None,
            },
        )
        .await
    }

    pub async fn finish_queued_message_edit_with_attachments(
        &self,
        chat_id: &str,
        id: &str,
        lease_id: &str,
        finish: QueueEditFinish<'_>,
    ) -> Result<FinishQueueEditOutcome, EngineError> {
        let QueueEditFinish {
            action,
            text,
            expected_text_hash,
            attachments,
        } = finish;
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        if action == FinishQueueEditAction::Commit
            && (text.is_none() || expected_text_hash.is_none())
        {
            return Err(EngineError::Other(
                "commit requires text and expectedTextHash".into(),
            ));
        }
        let handle = self.open(chat_id)?;
        let outcome = {
            let _drain = handle.drain_lock.lock().await;
            let Some(item) = handle
                .doc
                .read_queue()?
                .into_iter()
                .find(|item| item.id == id)
            else {
                return Ok(FinishQueueEditOutcome::Missing);
            };
            let (current_lease, base_text_hash) = match &item.delivery_gate {
                Some(QueueDeliveryGate::Editing {
                    lease_id,
                    base_text_hash,
                    ..
                }) => (lease_id, base_text_hash),
                Some(QueueDeliveryGate::ReviewRequired {
                    previous_lease_id,
                    base_text_hash,
                    ..
                }) => (previous_lease_id, base_text_hash),
                None => return Ok(FinishQueueEditOutcome::Lost),
            };
            if current_lease != lease_id {
                return Ok(FinishQueueEditOutcome::Lost);
            }
            if action == FinishQueueEditAction::Commit
                && (expected_text_hash != Some(base_text_hash.as_str())
                    || queue_text_hash(&item.text) != *base_text_hash)
            {
                return Ok(FinishQueueEditOutcome::Conflict {
                    current_text: item.text,
                });
            }

            let replacement = match action {
                FinishQueueEditAction::Commit => Some(text.unwrap_or_default()),
                FinishQueueEditAction::Cancel | FinishQueueEditAction::ReleaseUnchanged => None,
                FinishQueueEditAction::Discard => Some(""),
            };
            if !handle.doc.finish_queued_edit_with_attachments(
                id,
                replacement,
                if action == FinishQueueEditAction::Commit {
                    attachments
                } else {
                    None
                },
                now_ms(),
            )? {
                return Ok(FinishQueueEditOutcome::Missing);
            }
            handle.publish_queue();
            self.save_snapshot(&handle);
            match action {
                FinishQueueEditAction::Commit => FinishQueueEditOutcome::Committed,
                FinishQueueEditAction::Cancel => FinishQueueEditOutcome::Cancelled,
                FinishQueueEditAction::Discard => FinishQueueEditOutcome::Discarded,
                FinishQueueEditAction::ReleaseUnchanged => FinishQueueEditOutcome::Released,
            }
        };
        // Turn-end may already have happened while the edit was open.
        self.drain_queue(&handle).await;
        Ok(outcome)
    }

    fn arm_existing_queue_edit_expiries(&self, handle: &Arc<ChatDocHandle>) {
        let Ok(queue) = handle.doc.read_queue() else {
            return;
        };
        for item in queue {
            if let Some(QueueDeliveryGate::Editing {
                lease_id,
                expires_at_ms,
                ..
            }) = item.delivery_gate
            {
                self.arm_queue_edit_expiry(handle, &item.id, &lease_id, expires_at_ms);
            }
        }
    }

    fn arm_queue_edit_expiry(
        &self,
        handle: &Arc<ChatDocHandle>,
        id: &str,
        lease_id: &str,
        expires_at_ms: i64,
    ) {
        let delay_ms = expires_at_ms.saturating_sub(now_ms()).max(0) as u64;
        let host = self.clone();
        let handle = handle.clone();
        let id = id.to_string();
        let lease_id = lease_id.to_string();
        self.spawn_worker(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            host.expire_queued_message_edit(&handle, &id, &lease_id, expires_at_ms)
                .await;
        });
    }

    /// Expire exactly the lease generation that scheduled this wake. A stale
    /// timer from before a renewal observes a different deadline and no-ops;
    /// timers for other rows are completely independent.
    async fn expire_queued_message_edit(
        &self,
        handle: &Arc<ChatDocHandle>,
        id: &str,
        lease_id: &str,
        scheduled_expires_at_ms: i64,
    ) {
        let changed = {
            let _drain = handle.drain_lock.lock().await;
            let Ok(queue) = handle.doc.read_queue() else {
                return;
            };
            let Some(item) = queue.into_iter().find(|item| item.id == id) else {
                return;
            };
            let Some(QueueDeliveryGate::Editing {
                lease_id: current_lease_id,
                owner_device_id,
                expires_at_ms,
                base_text_hash,
                ..
            }) = item.delivery_gate
            else {
                return;
            };
            if current_lease_id != lease_id
                || expires_at_ms != scheduled_expires_at_ms
                || expires_at_ms > now_ms()
            {
                return;
            }
            let review = QueueDeliveryGate::ReviewRequired {
                previous_lease_id: current_lease_id,
                owner_device_id,
                since_ms: now_ms(),
                base_text_hash,
            };
            let Ok(changed) = handle.doc.set_queued_delivery_gate(id, Some(&review)) else {
                return;
            };
            if changed {
                handle.publish_queue();
                self.save_snapshot(handle);
            }
            changed
        };
        if changed {
            // If this was the head, the drain now observes ReviewRequired. If
            // it was not, publishing still updates every client's row state.
            self.drain_queue(handle).await;
        }
    }

    /// "Send this one now": take it out of the queue and put it in front of the
    /// agent, interrupting whatever is running. Deliberately blunt — it is the
    /// explicit override. The empty-composer Enter gesture reaches this path
    /// only when the selected provider cannot steer the row. `false` when
    /// another device already took it.
    pub async fn send_queued_now(&self, chat_id: &str, id: &str) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        // Sending one now sends ONE: the lock keeps the flush out of the idle
        // window the interrupt opens, and out of the take itself.
        let _drain = handle.drain_lock.lock().await;
        let Some(candidate) = handle
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == id)
        else {
            return Ok(false);
        };
        if candidate.delivery_gate.is_some() {
            return Err(EngineError::Other(
                "queued message is blocked for editing or review".into(),
            ));
        }
        let Some(item) = handle.doc.take_queued(id)? else {
            return Ok(false);
        };
        let was_paused = handle.queue_paused.swap(false, Ordering::AcqRel);
        handle.publish_queue();
        if let Err(err) = self
            .dispatch_queued(&handle, &item, QueueSend::Interrupt)
            .await
        {
            if was_paused {
                handle.queue_paused.store(true, Ordering::Release);
            }
            // Put it back rather than swallowing what the user typed — at the
            // head, because the user just said this one was the urgent one.
            let _ = handle.doc.insert_queued(0, &item);
            handle.publish_queue();
            return Err(err);
        }
        Ok(true)
    }

    /// Promote one held row without ever interrupting a turn. A live,
    /// steerable turn receives it as steering; if that turn has already ended,
    /// it starts normally as the next turn. Turn-boundary providers retain it
    /// in their mailbox until ready. Attachment-bearing rows stay untouched.
    pub async fn steer_queued_now(&self, chat_id: &str, id: &str) -> Result<bool, EngineError> {
        if !self.is_host(chat_id) {
            return Err(EngineError::Other(format!(
                "device {} does not host chat {chat_id}",
                self.inner.config.device_id
            )));
        }
        let handle = self.open(chat_id)?;
        let _drain = handle.drain_lock.lock().await;
        let Some(candidate) = handle
            .doc
            .read_queue()?
            .into_iter()
            .find(|item| item.id == id)
        else {
            return Ok(false);
        };
        if !candidate.attachments.is_empty() {
            return Err(EngineError::Other(
                "messages with attachments cannot be steered mid-turn".into(),
            ));
        }
        if candidate.delivery_gate.is_some() {
            return Err(EngineError::Other(
                "queued message is blocked for editing or review".into(),
            ));
        }
        if self
            .sessions()
            .is_some_and(|sessions| sessions.defers_to_turn_end(chat_id, None))
        {
            // Send next: lead the ordinary rows; the drain delivers it the
            // moment the current turn ends.
            let Some(item) = handle.doc.take_queued(id)? else {
                return Ok(false);
            };
            handle
                .doc
                .insert_queued(handle.steer_slot(&item.id)?, &item)?;
            handle.queue_paused.store(false, Ordering::Release);
            handle.publish_queue();
            return Ok(true);
        }
        let Some(item) = handle.doc.take_queued(id)? else {
            return Ok(false);
        };
        let was_paused = handle.queue_paused.swap(false, Ordering::AcqRel);
        handle.publish_queue();
        // Always attempt the non-interrupting path first. If no turn exists,
        // `dispatch_queued` falls through to NextTurn; if a new turn appeared
        // after our capability check, the prompt steers that turn instead.
        if let Err(err) = self.dispatch_queued(&handle, &item, QueueSend::Steer).await {
            if was_paused {
                handle.queue_paused.store(true, Ordering::Release);
            }
            let _ = handle.doc.insert_queued(0, &item);
            handle.publish_queue();
            return Err(err);
        }
        Ok(true)
    }

    /// Host-only: hand queued messages to the agent when there is somewhere to
    /// put them.
    ///
    /// - Idle: send the head as the next turn (and loop — the agent is free).
    /// - Turn in flight: hold. The turn-end watcher comes back for it.
    ///
    /// One at a time by design: each send changes the status this reads.
    pub async fn drain_queue(&self, handle: &Arc<ChatDocHandle>) {
        let Some(_dispatch) = self.dispatch_guard() else {
            return; // quiesced for a Cloud sleep; lifting it re-drains
        };
        let Some(sessions) = self.sessions() else {
            return; // executor not wired yet; the set_sessions kick re-drains
        };
        if !self.is_host(&handle.chat_id) {
            return;
        }
        // One drain at a time per chat. Waiters are cheap: whoever takes the
        // lock next re-reads the queue and the status, so a drain that became
        // unnecessary while it waited simply finds nothing to do.
        let _drain = handle.drain_lock.lock().await;
        if handle.queue_paused.load(Ordering::Acquire) {
            return;
        }
        loop {
            let Ok(Some(head)) = handle.doc.read_queue().map(|q| q.into_iter().next()) else {
                return;
            };
            match &head.delivery_gate {
                Some(QueueDeliveryGate::Editing {
                    lease_id,
                    owner_device_id,
                    expires_at_ms,
                    base_text_hash,
                    ..
                }) if *expires_at_ms <= now_ms() => {
                    let review = QueueDeliveryGate::ReviewRequired {
                        previous_lease_id: lease_id.clone(),
                        owner_device_id: owner_device_id.clone(),
                        since_ms: now_ms(),
                        base_text_hash: base_text_hash.clone(),
                    };
                    let _ = handle.doc.set_queued_delivery_gate(&head.id, Some(&review));
                    handle.publish_queue();
                    self.save_snapshot(handle);
                    return;
                }
                Some(QueueDeliveryGate::Editing {
                    lease_id,
                    expires_at_ms,
                    ..
                }) => {
                    self.arm_queue_edit_expiry(handle, &head.id, lease_id, *expires_at_ms);
                    return;
                }
                Some(QueueDeliveryGate::ReviewRequired { .. }) => return,
                None => {}
            }
            // In flight, not just Working: an agent parked on a question owns
            // the turn too, and the composer queues on the same reading. Taking
            // `AwaitingInput` for idle would send the follow-up as a fresh turn
            // and abandon the question.
            if sessions.turn_in_flight(&handle.chat_id) {
                return; // All queued messages wait, including rows from older clients.
            }
            // A row from another device names its images by `pending://` ref
            // while the bytes chase it over the peer link. Hold it (in order)
            // until they land — UploadCommit re-drains — rather than handing
            // the agent refs it cannot open.
            if !self.missing_row_attachments(&head).is_empty() {
                tracing::info!(chat = %handle.chat_id, row = %head.id,
                    "queued row held: attachment bytes in transit");
                self.arm_attachment_wait(handle);
                return;
            }
            let send = QueueSend::NextTurn;
            // Take it only once we know it is going out — a row that stays in
            // the queue on a failed send is recoverable; a vanished one is not.
            let Ok(Some(item)) = handle.doc.take_queued(&head.id) else {
                return;
            };
            // The taken row, not `head`, is what gets sent, and the row can
            // change between the two reads. Judge the row in hand: if its
            // bytes are still in transit it goes back to the front and waits
            // like any held row. Not a failed send — pausing here would strand
            // it once its bytes land, since only a user action thaws a pause.
            if !self.missing_row_attachments(&item).is_empty() {
                if let Err(err) = handle.doc.insert_queued(0, &item) {
                    tracing::warn!(chat = %handle.chat_id, row = %item.id, error = %err,
                        "queued row could not be put back");
                }
                handle.publish_queue();
                self.arm_attachment_wait(handle);
                return;
            }
            lock(&handle.steered_rows).retain(|row| row != &item.id);
            handle.publish_queue();
            if let Err(err) = self.dispatch_queued(handle, &item, send).await {
                tracing::warn!(chat = %handle.chat_id, error = %err, "queued send failed");
                handle.queue_paused.store(true, Ordering::Release);
                let _ = handle.doc.insert_queued(0, &item);
                handle.publish_queue();
                return;
            }
        }
    }

    /// A Stop acts on everything sent before it, in the order the user sent
    /// it. Controls bypass the prompt drain (so a stalled provider stays
    /// stoppable), which let a Stop execute before a send queued ahead of it
    /// had dispatched: with no turn yet it did nothing and the turn then ran
    /// in full; mid-turn, the earlier steer landed after the Stop and read as
    /// the user's next message, re-running in the fresh runtime. Wait for
    /// earlier prompts to dispatch (milliseconds) — bounded short while a
    /// turn is in flight, so a provider wedged on backpressure still stops.
    async fn await_earlier_prompts(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        stop_id: &str,
    ) {
        let started = tokio::time::Instant::now();
        loop {
            let bound = if sessions.turn_in_flight(&handle.chat_id) {
                std::time::Duration::from_secs(2)
            } else {
                std::time::Duration::from_secs(15)
            };
            if started.elapsed() >= bound {
                return;
            }
            let Ok(commands) = handle.doc.read_commands() else {
                return;
            };
            let Some(at) = commands.iter().position(|c| c.id == stop_id) else {
                return;
            };
            let earlier_prompt = commands[..at].iter().any(|c| {
                c.status == SessionCommandStatus::Pending
                    && matches!(
                        c.payload,
                        SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
                    )
            });
            if !earlier_prompt {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// A prompt queued after a control that is still executing (a Stop
    /// tearing the runtime down) waits for it: running ahead of it put the
    /// user's next message into the runtime the Stop was killing.
    async fn await_earlier_controls(&self, handle: &Arc<ChatDocHandle>, prompt_id: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            let Ok(commands) = handle.doc.read_commands() else {
                return;
            };
            let Some(at) = commands.iter().position(|c| c.id == prompt_id) else {
                return;
            };
            let earlier_control = {
                let executing = lock(&self.inner.executing);
                commands[..at].iter().any(|c| {
                    c.status == SessionCommandStatus::Pending
                        && !matches!(
                            c.payload,
                            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
                        )
                        && executing.contains(&c.id)
                })
            };
            if !earlier_control {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Stop the active turn without treating the resulting Idle transition as
    /// permission to release the next queued message. The same lock used by
    /// drains closes the race between clicking Cancel and the status watcher.
    /// A parked runtime still running background work is stopped too: the
    /// idle reaper spares it, so Stop is what ends that work.
    async fn interrupt_and_pause_queue(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
    ) -> Result<bool, EngineError> {
        let _drain = handle.drain_lock.lock().await;
        if !sessions.turn_in_flight(&handle.chat_id)
            && !sessions.holds_background_work(&handle.chat_id)
        {
            return Ok(false);
        }
        handle.queue_paused.store(true, Ordering::Release);
        self.cancel_held_sends(handle);
        match sessions.interrupt(&handle.chat_id).await {
            Ok(true) => Ok(true),
            Ok(false) => {
                handle.queue_paused.store(false, Ordering::Release);
                Ok(false)
            }
            Err(err) => {
                handle.queue_paused.store(false, Ordering::Release);
                Err(err)
            }
        }
    }

    /// A Stop cancels the messages sent during the turn it stops: the sends
    /// and steers held for that turn's end leave the queue (the user's own
    /// queued rows stay, paused) and land in the transcript unrun — never
    /// to start the stopped work again in the next runtime.
    fn cancel_held_sends(&self, handle: &Arc<ChatDocHandle>) {
        let held: Vec<String> = {
            let mut sends = lock(&handle.held_sends);
            let mut steered = lock(&handle.steered_rows);
            let ids = sends.keys().cloned().chain(steered.drain(..)).collect();
            sends.clear();
            ids
        };
        let mut changed = false;
        for id in held {
            let Ok(Some(row)) = handle.doc.take_queued(&id) else {
                continue;
            };
            changed = true;
            if let Err(err) = handle.write_user_message(&row.id, &row.text, row.issued_at) {
                tracing::warn!(chat = %handle.chat_id, error = %err, "cancelled held send write failed");
            }
        }
        if changed {
            handle.publish_queue();
        }
    }

    /// Send one taken queue row.
    async fn dispatch_queued(
        &self,
        handle: &Arc<ChatDocHandle>,
        item: &QueuedMessage,
        send: QueueSend,
    ) -> Result<(), EngineError> {
        let Some(sessions) = self.sessions() else {
            return Err(EngineError::Other("sessions engine not wired".into()));
        };
        let chat_id = &handle.chat_id;
        // Keep the queue row's identity when it becomes a real user message.
        // The submitting viewport learns this id from QueueMessage and can
        // therefore wait without disturbing the active turn's runway, then
        // anchor the prompt only once this exact row reaches the transcript.
        let message_id = item.id.clone();
        if !self.missing_row_attachments(item).is_empty() {
            return Err(EngineError::Other(
                "this message's images are still uploading".into(),
            ));
        }
        let mut attachments = item.attachments.clone();
        // A send held for the turn end goes out exactly as it was sent: its
        // text never carried the queue's image note.
        let held = lock(&handle.held_sends).remove(&item.id);
        let mut prompt = if held.is_some() {
            item.text.clone()
        } else {
            queued_message_prompt(&item.text, &attachments)
        };
        self.resolve_attachment_refs(&mut prompt, &mut attachments);
        if send == QueueSend::Steer {
            match sessions
                .steer(chat_id, &prompt, Some(message_id.clone()))
                .await?
            {
                SteerOutcome::Accepted => return Ok(()),
                SteerOutcome::DeferredByUpdate => {
                    return Err(EngineError::Other(
                        "agent update pending; the message remains queued".into(),
                    ));
                }
                // The run died under us between the status read and the send;
                // fall through and start a fresh turn with it.
                SteerOutcome::NotSteerable => {}
            }
        }
        // Same reading of "busy" as the drain: a turn parked on a question is
        // still a turn, and it has to be stopped before this one starts.
        // The message replaces the turn, not the agent: its background work
        // keeps running (only the user's Stop tears that down).
        if send == QueueSend::Interrupt && sessions.turn_in_flight(chat_id) {
            sessions.stop_turn(chat_id).await?;
        }
        let previous = sessions.last_request(chat_id);
        // ... and on its model, effort and options, not the chat's saved ones.
        let request = held.or_else(|| {
            self.request_from_chat_row(chat_id, &prompt)
                .map(|mut current| {
                    if let Some(previous) = &previous {
                        current.auto_approve = previous.auto_approve;
                        current.worktree = previous.worktree.clone();
                    }
                    current
                })
                .or(previous)
        });
        let Some(mut request) = request else {
            return Err(EngineError::Other(
                "no live run and no prior run config".into(),
            ));
        };
        request.prompt = prompt;
        request.resume = None; // dispatch re-derives the harness session
        request.attachments = attachments;
        let harness = self.harness_for_request(chat_id, &request);
        self.dispatch_with_source_context(&sessions, chat_id, harness, request, Some(message_id))
            .await?;
        Ok(())
    }

    /// The chat's host device when it is NOT this engine.
    fn remote_host_for(&self, chat_id: &str) -> Option<String> {
        match self.chat_host(chat_id) {
            ChatHost::Peer(device) => Some(device),
            ChatHost::Here | ChatHost::Unknown => None,
        }
    }

    /// Pushes a queued command's attachment bytes to the chat's remote host
    /// over the peer link, retrying until they land: the command must never
    /// run without its images. No-op without attachments or for chats this
    /// engine hosts.
    fn spawn_attachment_delivery(
        &self,
        chat_id: &str,
        transfers: Vec<crate::uploads::AttachmentTransfer>,
    ) {
        if transfers.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return; // bare sync callers (unit tests) skip rather than panic
        };
        let host = self.clone();
        let chat = chat_id.to_string();
        self.spawn_worker_on(&runtime, async move {
            if host.remote_host_for(&chat).is_some() {
                host.deliver_attachments(&chat, &transfers).await;
            }
        });
    }

    /// Push staged bytes until they land (event-driven backoff), `true` on
    /// success. Re-resolves the host each attempt (a claim can move the chat).
    async fn deliver_attachments(
        &self,
        chat: &str,
        transfers: &[crate::uploads::AttachmentTransfer],
    ) -> bool {
        let mut wake = onicode_sync::wake::subscribe();
        let mut online = onicode_sync::wake::subscribe_online();
        let mut backoff = TRANSFER_BACKOFF_BASE;
        let deadline = tokio::time::Instant::now() + ATTACHMENT_WAIT_MAX;
        loop {
            let Some(target) = self.remote_host_for(chat) else {
                return true; // became locally hosted: bytes already here
            };
            match self.push_attachments(&target, transfers).await {
                Ok(()) => {
                    // The host's UploadCommit kicks its own drains.
                    tracing::info!(chat = %chat, device = %target,
                        count = transfers.len(), "queued attachments delivered");
                    return true;
                }
                Err(TransferError::Permanent(err)) => {
                    tracing::warn!(chat = %chat, device = %target, error = %err,
                        "queued attachment transfer failed permanently");
                    return false;
                }
                Err(TransferError::Transient(err)) => {
                    tracing::warn!(chat = %chat, device = %target, error = %err,
                        backoff_ms = backoff.as_millis() as u64,
                        "queued attachment transfer retrying");
                }
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(chat = %chat, "queued attachment transfer gave up (wait cap)");
                return false;
            }
            while wake.try_recv().is_ok() {}
            while online.try_recv().is_ok() {}
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = wake.recv() => {}
                _ = online.recv() => {}
            }
            backoff = (backoff * 2).min(TRANSFER_BACKOFF_CAP);
        }
    }

    /// User-driven retry (the failed-send affordance): re-kick every
    /// delivery road for a chat whose queued sends haven't been adopted —
    /// the follower tries the host again at once, a local drain pass runs,
    /// and each still-pending command's attachment transfers are re-derived
    /// from its `pending://` refs (idempotent: re-pushing landed bytes
    /// re-commits the same file; the processed ledger keeps execution
    /// exactly-once).
    pub fn retry_delivery(&self, chat_id: &str) -> Result<(), EngineError> {
        let handle = self.open(chat_id)?;
        handle.kick.notify_one();
        let commands = handle.doc.read_commands()?;
        let pending: Vec<SessionCommandEntry> = commands
            .iter()
            .filter(|c| {
                c.status == SessionCommandStatus::Pending
                    && !self.inner.store.is_processed(&c.id).unwrap_or(false)
            })
            .cloned()
            .collect();
        for entry in pending {
            self.spawn_attachment_delivery(chat_id, command_transfers(&entry));
        }
        // Dead attempts: a Run/Steer whose user message never landed and
        // whose command can never execute again — Rejected (execute failed,
        // or the dead-command sweep terminalized it), or consumed by the
        // ledger with no outcome and not currently executing (crash between
        // mark and resolve). Exactly-once is per command ID, so a
        // user-driven retry mints a FRESH attempt: new id, same payload and
        // message id (the executor's user-entry pre-write dedupes by message
        // id). One re-issue per message — the LATEST attempt speaks for it.
        let messages = handle.doc.read_entries().unwrap_or_default();
        let message_landed = |mid: &str| messages.iter().any(|m| m.id == mid);
        let mut latest_dead: HashMap<String, &SessionCommandEntry> = HashMap::new();
        for c in &commands {
            let Some(mid) = (match &c.payload {
                SessionCommandPayload::Run { message_id, .. } => Some(message_id.as_str()),
                SessionCommandPayload::Steer { message_id, .. } => message_id.as_deref(),
                _ => None,
            }) else {
                continue;
            };
            if message_landed(mid) {
                continue;
            }
            let dead = match c.status {
                // Rejected: execute failed or the sweep terminalized a crash
                // window. Expired: the entry outlived its TTL undelivered —
                // an explicit user retry is exactly the consent to re-send.
                SessionCommandStatus::Rejected | SessionCommandStatus::Expired => true,
                SessionCommandStatus::Pending => {
                    self.inner.store.is_processed(&c.id).unwrap_or(false)
                        && !lock(&self.inner.executing).contains(&c.id)
                }
                _ => false,
            };
            if !dead {
                continue;
            }
            // A LIVE pending attempt for the same message (queued or being
            // escorted above) makes a re-issue a duplicate — skip.
            let live_attempt = commands.iter().any(|o| {
                o.id != c.id
                    && o.status == SessionCommandStatus::Pending
                    && !self.inner.store.is_processed(&o.id).unwrap_or(false)
                    && match (&o.payload, &c.payload) {
                        (
                            SessionCommandPayload::Run { message_id: a, .. },
                            SessionCommandPayload::Run { message_id: b, .. },
                        ) => a == b,
                        (
                            SessionCommandPayload::Steer { message_id: a, .. },
                            SessionCommandPayload::Steer { message_id: b, .. },
                        ) => a == b,
                        _ => false,
                    }
            });
            if live_attempt {
                continue;
            }
            match latest_dead.entry(mid.to_string()) {
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    if c.issued_at > slot.get().issued_at {
                        slot.insert(c);
                    }
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(c);
                }
            }
        }
        for old in latest_dead.values() {
            if old.status == SessionCommandStatus::Pending {
                // Terminalize the consumed-but-dead original so the doc tells
                // the truth and the next retry pass doesn't see it again.
                self.resolve_command(
                    &handle,
                    &old.id,
                    SessionCommandStatus::Rejected,
                    Some("interrupted before completion — superseded by retry"),
                );
            }
            let now = now_ms();
            let reissue = SessionCommandEntry {
                id: new_id(),
                payload: old.payload.clone(),
                issued_by: self.inner.config.device_id.clone(),
                issued_at: now,
                based_on: messages.last().map(|m| CommandBasedOn {
                    turn_id: Some(m.id.clone()),
                    frontier: None,
                }),
                expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
                status: SessionCommandStatus::Pending,
                resolution: None,
            };
            tracing::info!(chat = %chat_id, old = %old.id, new = %reissue.id,
                "retry re-issues a dead send attempt");
            handle.doc.queue_command(&reissue)?;
            self.spawn_attachment_delivery(chat_id, command_transfers(&reissue));
        }
        // Locally-hosted (or already-synced) commands: a drain pass is the
        // whole retry.
        if tokio::runtime::Handle::try_current().is_ok() {
            let host = self.clone();
            let handle = handle.clone();
            self.spawn_worker(async move { host.drain_commands(&handle).await });
        }
        Ok(())
    }

    /// One transfer attempt: chunked `UploadChunk` + `UploadCommit` straight
    /// over the peer link (same wire the UI's legacy path used, so old and
    /// new engines interoperate). Timeouts mark the link suspect —
    /// `invalidate` drops the cached socket so the retry dials fresh instead
    /// of feeding a zombie pipe forever (2026-08-19 incident).
    async fn push_attachments(
        &self,
        target: &str,
        transfers: &[crate::uploads::AttachmentTransfer],
    ) -> Result<(), TransferError> {
        use TransferError::{Permanent, Transient};
        let Some(links) = self.inner.links.get() else {
            return Err(Permanent("peer links not wired".into()));
        };
        let Some(uploads) = self.inner.uploads.get() else {
            return Err(Permanent("uploads not wired".into()));
        };
        let client = links
            .client(target)
            .await
            .map_err(|e| Transient(format!("peer link: {e}")))?;
        for transfer in transfers {
            // Bytes come from the uploads jail only — a transfer names an
            // upload identity, never an arbitrary path.
            let source = uploads.pending_target(&transfer.upload_id, &transfer.file_name);
            let bytes = tokio::fs::read(&source)
                .await
                .map_err(|e| Permanent(format!("staged attachment missing: {e}")))?;
            // Progress entry for the sender's thumbnail ring, updated per
            // landed chunk. The guard retires it on EVERY exit — commit,
            // timeout, refusal — so a dead attempt falls back to the
            // indeterminate spinner and the retry re-publishes from 0.
            let total = bytes.len() as u64;
            self.transfer_progress_set(&transfer.upload_id, &transfer.file_name, 0, total);
            let _progress = TransferProgressGuard {
                host: self,
                upload_id: &transfer.upload_id,
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let mut start = 0usize;
            let mut seq = 0u64;
            loop {
                let end = (start + TRANSFER_CHUNK_B64).min(b64.len());
                let params = serde_json::json!({
                    "uploadId": transfer.upload_id, "seq": seq, "data": &b64[start..end],
                });
                let call = client.call(onicode_rpc::methods::UPLOAD_CHUNK, params);
                match tokio::time::timeout(TRANSFER_CHUNK_TIMEOUT, call).await {
                    Err(_) => {
                        links.invalidate(target);
                        return Err(Transient("chunk push timed out; peer link suspect".into()));
                    }
                    Ok(Err(onicode_rpc::RpcError::Failed(err))) => {
                        return Err(Permanent(format!("host refused chunk: {err}")));
                    }
                    Ok(Err(err)) => {
                        links.invalidate(target);
                        return Err(Transient(format!("chunk push failed: {err}")));
                    }
                    Ok(Ok(_)) => {}
                }
                start = end;
                seq += 1;
                // b64 → raw: 4 chars carry 3 bytes; min-clamp absorbs the
                // final chunk's padding overshoot.
                let done = ((start as u64) * 3 / 4).min(total);
                self.transfer_progress_set(&transfer.upload_id, &transfer.file_name, done, total);
                if start >= b64.len() {
                    break;
                }
            }
            let params = serde_json::json!({
                "uploadId": transfer.upload_id, "fileName": transfer.file_name,
            });
            let call = client.call(onicode_rpc::methods::UPLOAD_COMMIT, params);
            match tokio::time::timeout(TRANSFER_COMMIT_TIMEOUT, call).await {
                Err(_) => {
                    links.invalidate(target);
                    return Err(Transient("commit timed out; peer link suspect".into()));
                }
                Ok(Err(onicode_rpc::RpcError::Failed(err))) => {
                    return Err(Permanent(format!("host refused commit: {err}")));
                }
                Ok(Err(err)) => {
                    links.invalidate(target);
                    return Err(Transient(format!("commit failed: {err}")));
                }
                Ok(Ok(_)) => {}
            }
        }
        Ok(())
    }

    /// Upload a tool result's full output/diff to the R2 sidecar
    /// (`PUT {edge}/blob/{chatId}/{partId}[.diff]`, docs/chat2-sync.md A2).
    /// Fire-and-forget: the doc already carries the summary, so a lost upload
    /// degrades to "full output unavailable" — it must never block or fail
    /// the run. Offline/edge-less engines skip silently.
    pub fn upload_tool_sidecar(&self, chat_id: &str, payload: onicode_doc::SidecarPayload) {
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return; // bare sync callers (unit tests) skip rather than panic
        };
        let http = self.inner.http.clone();
        let base = format!(
            "{}/blob/{}/{}",
            edge.url.trim_end_matches('/'),
            chat_id,
            encode_part_segment(&payload.part_id)
        );
        self.spawn_worker_on(&runtime, async move {
            let Ok(bearer) = edge.bearer().await else {
                return; // token unavailable; serve the local summary
            };
            let mut puts: Vec<(String, &'static str, Vec<u8>)> = Vec::new();
            if let Some(output) = &payload.output {
                puts.push((
                    base.clone(),
                    "text/plain; charset=utf-8",
                    output.clone().into_bytes(),
                ));
            }
            if let Some(diff) = &payload.diff
                && let Ok(json) = serde_json::to_vec(diff)
            {
                puts.push((format!("{base}.diff"), "application/json", json));
            }
            for (url, content_type, body) in puts {
                let sent = http
                    .put(&url)
                    .bearer_auth(&bearer)
                    .header("content-type", content_type)
                    .body(body)
                    .send()
                    .await;
                match sent {
                    Ok(res) if res.status().is_success() => {}
                    Ok(res) => tracing::warn!(url, status = res.status().as_u16(),
                        "tool sidecar upload rejected"),
                    Err(err) => {
                        let err = describe_http_error(err);
                        tracing::warn!(url, error = %err, "tool sidecar upload failed (best-effort)")
                    }
                }
            }
        });
    }

    /// Fetch a sidecar blob by its doc-resident ref (`{chatId}/{partId}` or
    /// `…​.diff`) — the UI's lazy "Show full output" path, served over RPC
    /// because the UI crate has no HTTP client or edge bearer.
    pub async fn fetch_tool_blob(&self, blob_ref: &str) -> Result<String, EngineError> {
        // Same shape `apply_sidecar_refs` writes; anything else is a forged ref.
        let valid = blob_ref.split_once('/').is_some_and(|(chat, part)| {
            !chat.is_empty()
                && chat.len() <= 128
                && chat
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !part.is_empty()
                && part.len() <= 200
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:#~-".contains(&b))
        });
        if !valid {
            return Err(EngineError::Other(format!("bad blob ref: {blob_ref}")));
        }
        let Some(edge) = self.inner.config.edge.clone() else {
            return Err(EngineError::Other("offline: no edge configured".into()));
        };
        let bearer = edge.bearer().await?;
        // `valid` above guarantees the split; re-split to encode the part
        // segment for transport (PART_RE allows `#`, which a raw URL would
        // truncate as a fragment — the 2026-08-10 silent-collision bug).
        let (chat, part) = blob_ref.split_once('/').expect("validated above");
        let url = format!(
            "{}/blob/{}/{}",
            edge.url.trim_end_matches('/'),
            chat,
            encode_part_segment(part)
        );
        let res = self
            .inner
            .http
            .get(&url)
            .bearer_auth(&bearer)
            .send()
            .await
            .map_err(|e| {
                EngineError::Other(format!("sidecar fetch failed: {}", describe_http_error(e)))
            })?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "sidecar fetch: HTTP {}",
                res.status().as_u16()
            )));
        }
        res.text().await.map_err(|e| {
            EngineError::Other(format!(
                "sidecar body read failed: {}",
                describe_http_error(e)
            ))
        })
    }

    /// §2.2 writer discipline: we host a chat iff its workspace row's `deviceId` is
    /// ours; a chat with no row is claimable (claim-on-first-command). Without a
    /// wired workspace host (bare-DocHost tests) every open chat is ours — M2's
    /// behavior, now the degenerate case.
    fn is_host(&self, chat_id: &str) -> bool {
        self.workspace().is_none_or(|ws| ws.is_host(chat_id))
    }

    /// Chat-config harness when the workspace row carries one, else the default.
    pub(crate) fn harness_for(&self, chat_id: &str) -> HarnessId {
        self.workspace()
            .and_then(|ws| ws.chat_config(chat_id))
            .map(|config| config.harness)
            .unwrap_or(self.inner.config.default_harness)
    }

    /// The harness a request dispatches on: the request's own pick when it
    /// carries one (rides the command plane, immune to registry-row races),
    /// else [`Self::harness_for`].
    pub(crate) fn harness_for_request(
        &self,
        chat_id: &str,
        request: &onicode_proto::RunRequest,
    ) -> HarnessId {
        request.harness.unwrap_or_else(|| self.harness_for(chat_id))
    }

    /// Drain pending commands (host-only): evaluate → mark processed BEFORE execute →
    /// execute → write the outcome as the sole outcome writer.
    pub async fn drain_commands(&self, handle: &Arc<ChatDocHandle>) {
        self.drain_command_kind(handle, false).await;
    }

    async fn drain_command_kind(&self, handle: &Arc<ChatDocHandle>, controls_only: bool) {
        // A quiesced engine still serves controls (interrupt, answers): only
        // new work waits for the machine to wake.
        let _dispatch = if controls_only {
            None
        } else {
            match self.dispatch_guard() {
                Some(guard) => Some(guard),
                None => return, // quiesced for a Cloud sleep; lifting it re-drains
            }
        };
        let Some(sessions) = self.sessions() else {
            return; // executor not wired yet (or retired); the set_sessions kick re-drains
        };
        if !self.is_host(&handle.chat_id) {
            return;
        }
        // Do not let another drain overtake a prompt waiting for mailbox
        // capacity (or preflight). Controls bypass this lock so a stalled
        // provider can still be interrupted or have its question answered.
        let mut prompt_guard = if controls_only {
            None
        } else {
            handle.command_drain_lock.try_lock().ok()
        };
        // Entries this pass decided to leave alone (processed dedupe hits).
        let mut skipped: HashSet<String> = HashSet::new();
        loop {
            let commands = match handle.doc.read_commands() {
                Ok(commands) => commands,
                Err(err) => {
                    tracing::warn!(chat = %handle.chat_id, error = %err, "command read failed");
                    return;
                }
            };
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            // Dead-command sweep: Pending in the doc, consumed by the ledger,
            // and NOT mid-execution in this process — the crash window
            // between mark-processed and the outcome write. Left alone it is
            // a send no drain or retry can ever reach ("Sending…" forever,
            // 2026-08-19); terminalize it so the truth lands in the doc and
            // a user retry can mint a fresh attempt. `commands` may predate a
            // racing drain that has since resolved the command and released
            // its claim (resolve comes first), so once no claim is seen the
            // status is read again rather than overwriting that outcome.
            let still_pending = |id: &str| {
                handle.doc.read_commands().is_ok_and(|now| {
                    now.iter()
                        .any(|c| c.id == id && c.status == SessionCommandStatus::Pending)
                })
            };
            for c in &commands {
                if c.status == SessionCommandStatus::Pending
                    && !skipped.contains(&c.id)
                    && is_processed(&c.id)
                    && !lock(&self.inner.executing).contains(&c.id)
                    && still_pending(&c.id)
                {
                    tracing::warn!(chat = %handle.chat_id, command = %c.id,
                        "command consumed but never resolved (crash mid-execute?); rejecting");
                    self.resolve_command(
                        handle,
                        &c.id,
                        SessionCommandStatus::Rejected,
                        Some("interrupted before completion — retry to send again"),
                    );
                    skipped.insert(c.id.clone());
                }
            }
            let Some(entry) = commands
                .iter()
                .find(|c| {
                    c.status == SessionCommandStatus::Pending
                        && !skipped.contains(&c.id)
                        && !is_processed(&c.id)
                        && (prompt_guard.is_some()
                            || !matches!(
                                c.payload,
                                SessionCommandPayload::Run { .. }
                                    | SessionCommandPayload::Steer { .. }
                            ))
                })
                .cloned()
            else {
                if prompt_guard.is_none() && !controls_only {
                    // Wait after handling controls, then re-read: simply
                    // returning here could miss a newly appended prompt.
                    prompt_guard = Some(handle.command_drain_lock.lock().await);
                    continue;
                }
                return;
            };
            let messages = handle.doc.read_entries().unwrap_or_default();
            let current_turn_id = messages.last().map(|m| m.id.clone());
            let turn_is_past = |turn_id: &str| messages.iter().any(|m| m.id == turn_id);
            let disposition = evaluate_command(
                &entry,
                &EvaluationContext {
                    is_processed: &is_processed,
                    now_ms: now_ms(),
                    entries: &commands,
                    current_turn_id: current_turn_id.as_deref(),
                    turn_is_past: &turn_is_past,
                },
            );
            // Queued-attachment gate (BEFORE the processed mark — a deferred
            // command must stay eligible): a Run/Steer naming `pending://`
            // refs whose bytes haven't landed on this device yet waits for
            // the transfer instead of running without its images. The wait is
            // bounded; past it the command fails loudly.
            if matches!(disposition, CommandDisposition::Execute) {
                let missing = self.missing_attachments(&entry);
                if !missing.is_empty() {
                    if now_ms().saturating_sub(entry.issued_at) < ATTACHMENT_WAIT_MAX_MS {
                        tracing::info!(chat = %handle.chat_id, command = %entry.id,
                            missing = missing.len(), "command deferred: attachment bytes in transit");
                        self.arm_attachment_wait(handle);
                        return; // preserve order; UploadCommit / the wait timer re-kick
                    }
                    if let Err(err) = self.inner.store.mark_processed(&entry.id) {
                        tracing::error!(chat = %handle.chat_id, error = %err,
                            "processed-ledger write failed; halting drain");
                        return;
                    }
                    tracing::warn!(chat = %handle.chat_id, command = %entry.id,
                        "command rejected: attachments never arrived");
                    self.resolve_command(
                        handle,
                        &entry.id,
                        SessionCommandStatus::Rejected,
                        Some("attachments never arrived"),
                    );
                    continue;
                }
            }
            // In-flight claim: guards the dead-command sweep (an id in
            // `executing` is alive, not crashed) and serializes racing
            // drains on the same entry.
            if !lock(&self.inner.executing).insert(entry.id.clone()) {
                skipped.insert(entry.id.clone());
                continue;
            }
            // Mark BEFORE executing: a crash mid-execution must never double-run a
            // command whose side effect may already have happened. The ledger
            // write is the claim: `false` means another path (a racing drain
            // of controls, which bypass the prompt lock, or a relayed copy)
            // claimed the id and finished since this pass read it as
            // unprocessed. Running it again would, say, interrupt the turn
            // that started after the first interrupt.
            match self.inner.store.mark_processed(&entry.id) {
                Ok(true) => {}
                Ok(false) => {
                    lock(&self.inner.executing).remove(&entry.id);
                    skipped.insert(entry.id.clone());
                    continue;
                }
                Err(err) => {
                    tracing::error!(chat = %handle.chat_id, error = %err, "processed-ledger write failed; halting drain");
                    lock(&self.inner.executing).remove(&entry.id);
                    return;
                }
            }
            match disposition {
                CommandDisposition::Skip => {
                    skipped.insert(entry.id.clone());
                }
                CommandDisposition::Expired => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Expired, None);
                }
                CommandDisposition::Superseded => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Superseded, None);
                }
                CommandDisposition::Execute => {
                    if matches!(
                        entry.payload,
                        SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
                    ) {
                        self.await_earlier_controls(handle, &entry.id).await;
                    }
                    let (status, resolution) = match self.execute(&sessions, handle, &entry).await {
                        Ok(outcome) => outcome,
                        Err(err) => (SessionCommandStatus::Rejected, Some(err.to_string())),
                    };
                    self.resolve_command(handle, &entry.id, status, resolution.as_deref());
                }
            }
            lock(&self.inner.executing).remove(&entry.id);
        }
    }

    /// The command's `pending://` attachment refs whose bytes are NOT on this
    /// device's disk yet. Empty when the command names none, when everything
    /// has landed, or when no uploads store is wired (tests) — absence of the
    /// subsystem must never wedge a queue.
    fn missing_attachments(&self, entry: &SessionCommandEntry) -> Vec<String> {
        let refs: Vec<String> = match &entry.payload {
            SessionCommandPayload::Run { request, .. } => request
                .attachments
                .iter()
                .filter(|p| crate::uploads::is_pending_ref(p))
                .cloned()
                .collect(),
            SessionCommandPayload::Steer { prompt, .. } => crate::uploads::pending_refs_in(prompt),
            _ => Vec::new(),
        };
        if refs.is_empty() {
            return refs;
        }
        let Some(uploads) = self.inner.uploads.get() else {
            return Vec::new();
        };
        refs.into_iter()
            .filter(|r| uploads.resolve_pending(r).is_none())
            .collect()
    }

    /// Arm (once per chat) the deferred-command re-check loop: while a
    /// pending unprocessed command still waits on attachment bytes, re-drain
    /// on a cadence so the bounded wait actually expires even if every
    /// event-driven kick was missed.
    fn arm_attachment_wait(&self, handle: &Arc<ChatDocHandle>) {
        let chat = handle.chat_id.clone();
        if !lock(&self.inner.drain_waiting).insert(chat.clone()) {
            return;
        }
        let weak = Arc::downgrade(handle);
        let host = self.clone();
        self.spawn_worker(async move {
            loop {
                tokio::time::sleep(ATTACHMENT_WAIT_RECHECK).await;
                let Some(handle) = weak.upgrade() else { break };
                if !host.awaiting_attachments(&handle) {
                    break;
                }
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
                let Some(handle) = weak.upgrade() else { break };
                if !host.awaiting_attachments(&handle) {
                    break;
                }
            }
            lock(&host.inner.drain_waiting).remove(&chat);
        });
    }

    /// True while some pending, unprocessed command still waits on bytes.
    fn awaiting_attachments(&self, handle: &Arc<ChatDocHandle>) -> bool {
        let commands = handle.doc.read_commands().unwrap_or_default();
        let command_waiting = commands.iter().any(|c| {
            c.status == SessionCommandStatus::Pending
                && !self.inner.store.is_processed(&c.id).unwrap_or(false)
                && !self.missing_attachments(c).is_empty()
        });
        command_waiting
            || handle
                .doc
                .read_queue()
                .ok()
                .and_then(|q| q.into_iter().next())
                .is_some_and(|head| !self.missing_row_attachments(&head).is_empty())
    }

    /// Rewrite a request's landed `pending://` refs to this device's absolute
    /// paths — in the attachments list AND the prompt text — so the harness
    /// (and the persisted user entry) see ordinary local files, exactly like
    /// the legacy pre-upload flow produced.
    fn resolve_request_attachments(&self, request: &mut onicode_proto::RunRequest) {
        self.resolve_attachment_refs(&mut request.prompt, &mut request.attachments);
    }

    /// Rewrite landed `pending://` refs in `attachments` (and wherever the
    /// prompt names them) to this device's absolute paths.
    fn resolve_attachment_refs(&self, prompt: &mut String, attachments: &mut [String]) {
        let Some(uploads) = self.inner.uploads.get() else {
            return;
        };
        for path in attachments.iter_mut() {
            if let Some(abs) = uploads.resolve_pending(path) {
                *prompt = prompt.replace(path.as_str(), &abs);
                *path = abs;
            }
        }
    }

    /// A queue row's `pending://` refs whose bytes are not on this device
    /// yet ([`Self::missing_attachments`] for rows). Covers refs in the
    /// row's text too: older clients inlined the attachment trailer.
    fn missing_row_attachments(&self, item: &QueuedMessage) -> Vec<String> {
        let Some(uploads) = self.inner.uploads.get() else {
            return Vec::new();
        };
        let mut refs: Vec<String> = item
            .attachments
            .iter()
            .filter(|p| crate::uploads::is_pending_ref(p))
            .cloned()
            .collect();
        for r in crate::uploads::pending_refs_in(&item.text) {
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
        refs.into_iter()
            .filter(|r| uploads.resolve_pending(r).is_none())
            .collect()
    }

    /// [`Self::resolve_request_attachments`] for a bare prompt (Steer).
    fn resolve_prompt_attachments(&self, prompt: &str) -> String {
        let Some(uploads) = self.inner.uploads.get() else {
            return prompt.to_string();
        };
        let mut out = prompt.to_string();
        for r in crate::uploads::pending_refs_in(prompt) {
            if let Some(abs) = uploads.resolve_pending(&r) {
                out = out.replace(&r, &abs);
            }
        }
        out
    }

    /// Host-only outcome write (ledger rule 2).
    fn resolve_command(
        &self,
        handle: &ChatDocHandle,
        command_id: &str,
        status: SessionCommandStatus,
        resolution: Option<&str>,
    ) {
        if let Err(err) = handle
            .doc
            .set_command_status(command_id, status, resolution)
        {
            tracing::warn!(
                chat = %handle.chat_id,
                command = %command_id,
                error = %err,
                "command outcome write failed"
            );
        }
    }

    /// `entry` is a prompt sent before the Stop that has already acted on
    /// this chat: the Stop cancelled it. Returns its message id.
    fn cancelled_by_stop(
        &self,
        handle: &Arc<ChatDocHandle>,
        entry: &SessionCommandEntry,
    ) -> Option<String> {
        let message_id = match &entry.payload {
            SessionCommandPayload::Run { message_id, .. } => message_id.clone(),
            SessionCommandPayload::Steer { message_id, .. } => {
                message_id.clone().unwrap_or_else(|| entry.id.clone())
            }
            _ => return None,
        };
        let stop = lock(&handle.stopped_by).clone()?;
        let commands = handle.doc.read_commands().ok()?;
        let at = commands.iter().position(|c| c.id == entry.id)?;
        let stop_at = commands.iter().position(|c| c.id == stop)?;
        (at < stop_at).then_some(message_id)
    }

    async fn execute(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        entry: &SessionCommandEntry,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        if let Some(message_id) = self.cancelled_by_stop(handle, entry) {
            // Still the user's message: it stays in the transcript, unrun.
            let prompt = match &entry.payload {
                SessionCommandPayload::Run { request, .. } => request.prompt.clone(),
                SessionCommandPayload::Steer { prompt, .. } => prompt.clone(),
                _ => String::new(),
            };
            if let Err(err) =
                handle.write_user_message(&message_id, &prompt, entry.issued_at.min(now_ms()))
            {
                tracing::warn!(chat = %chat_id, error = %err, "cancelled message write failed");
            }
            tracing::info!(chat = %chat_id, "prompt sent before a Stop that has acted: cancelled");
            return Ok((
                SessionCommandStatus::Applied,
                Some("cancelled by the Stop sent after it".into()),
            ));
        }
        match &entry.payload {
            SessionCommandPayload::Run {
                request,
                message_id,
            } => {
                let mut request = (**request).clone();
                // Queued-attachment refs (`pending://`) resolve to this
                // host's absolute paths before anything persists or
                // dispatches — the drain already gated on the bytes being
                // present, so every ref resolves here.
                self.resolve_request_attachments(&mut request);
                // Worktree directive (WorktreeSpec): materialize on THIS host at
                // drain time — the durable command plane replaces the sender's
                // old blocking CreateWorktree relay RPC, whose lost reply wedged
                // the composer on "Sending…" while the run proceeded anyway.
                // `take()` resolves the request before dispatch, so the journal
                // and steer→new-turn fallbacks reuse the created path instead of
                // minting another checkout.
                let worktree_spec = request.worktree.take();
                let fresh_worktree = match &worktree_spec {
                    Some(spec) => {
                        let (cwd, fresh) = self.materialize_worktree(chat_id, spec).await?;
                        request.cwd = cwd;
                        fresh
                    }
                    None => None,
                };
                // Claim-on-first-command: a run for a chat with no workspace row
                // creates the row under our device id (we are about to host it).
                if let Some(ws) = self.workspace() {
                    ws.claim_chat(chat_id, Some(&request.cwd))?;
                    // Older clients send `~` for a projectless run; a chat
                    // created with a scratch folder runs there instead.
                    if onicode_proto::projectless::is_home_cwd(&request.cwd)
                        && let Ok(Some(chat)) = ws.chat(chat_id)
                        && let Some(cwd) = chat.cwd.filter(|cwd| {
                            chat.space_id.is_none()
                                && onicode_proto::projectless::is_scratch_cwd(cwd)
                        })
                    {
                        request.cwd = cwd;
                    }
                    // A pre-existing row (the client's createChat raced ahead)
                    // still carries the repo folder — repoint it at the fresh
                    // worktree, and stamp the actual `onicode/<name>` branch so
                    // the footer and the title-rename flow see it.
                    if let Some(wt) = &fresh_worktree {
                        if let Err(err) = ws.set_chat_cwd(chat_id, &wt.path) {
                            tracing::warn!(chat = %chat_id, error = %err, "worktree cwd stamp failed");
                        }
                        if let Err(err) = ws.set_chat_branch(chat_id, &wt.branch) {
                            tracing::warn!(chat = %chat_id, error = %err, "worktree branch stamp failed");
                        }
                    }
                }
                if let Some(spec) = worktree_spec.as_ref()
                    && spec.space_id.is_some()
                {
                    self.complete_worktree_setup_handoff(
                        &entry.id,
                        chat_id,
                        spec,
                        fresh_worktree.as_ref(),
                    );
                }
                let harness = self.harness_for_request(chat_id, &request);
                // A row with no config renders no harness glyph (and every
                // later dispatch falls back to the engine default), so stamp
                // what this run actually executes with. Claimed rows and
                // catalog-not-loaded createChats both land here; the racing
                // real createChat carries the same picked values.
                if let Some(ws) = self.workspace()
                    && ws.chat_config(chat_id).is_none()
                {
                    let config = onicode_proto::ChatConfig {
                        harness,
                        model: request.model.clone(),
                        reasoning: request.reasoning,
                        model_options: request.model_options.clone(),
                        sandbox: request.sandbox,
                        access: request.access,
                        plan: request.plan,
                    };
                    if let Err(err) = ws.set_chat_config(chat_id, &config) {
                        tracing::warn!(chat = %chat_id, error = %err, "run-config backfill failed");
                    }
                }
                // A run on another provider than the live turn's (a stale
                // client, another device's switch) waits for the turn: the
                // provider never changes by killing a turn mid-flight.
                let other_provider = sessions
                    .live_harness(chat_id)
                    .is_some_and(|live| live != harness);
                if other_provider || sessions.defers_to_turn_end(chat_id, Some((harness, &request)))
                {
                    self.hold_until_turn_end(
                        handle,
                        message_id,
                        &request.prompt,
                        entry.issued_at,
                        false,
                        Some(&request),
                    )?;
                    return Ok((
                        SessionCommandStatus::Applied,
                        Some("held until the turn ends".into()),
                    ));
                }
                // Timestamp canonicalization: the user message lands in
                // history at the moment the user SENT it (the entry's
                // issued_at, clamped against clock skew) — not whenever this
                // host got around to draining a queued command. Idempotent by
                // id, so the dispatch path's own execution-time write dedupes
                // to a no-op. A provider switch's seam lands with it, above.
                sessions.note_provider_switch(chat_id, harness, handle);
                if let Err(err) = handle.write_user_message(
                    message_id,
                    &request.prompt,
                    entry.issued_at.min(now_ms()),
                ) {
                    tracing::warn!(chat = %chat_id, error = %err, "canonical user-message write failed");
                }
                self.dispatch_with_source_context(
                    sessions,
                    chat_id,
                    harness,
                    request,
                    Some(message_id.clone()),
                )
                .await?;
                // A fresh user-authored turn is the deliberate action that
                // thaws a queue frozen by Cancel. Clear only after dispatch
                // succeeds so a failed send cannot silently unfreeze it.
                handle.queue_paused.store(false, Ordering::Release);
                Ok((SessionCommandStatus::Applied, None))
            }
            SessionCommandPayload::Steer { prompt, message_id } => {
                self.deliver_prompt(
                    sessions,
                    handle,
                    prompt,
                    message_id.clone(),
                    entry.issued_at,
                )
                .await
            }
            SessionCommandPayload::Interrupt {} => {
                self.await_earlier_prompts(sessions, handle, &entry.id)
                    .await;
                *lock(&handle.stopped_by) = Some(entry.id.clone());
                // Sends before this Stop that are still on their way (a slow
                // dispatch the wait above gave up on) are cancelled with it.
                if let Ok(commands) = handle.doc.read_commands()
                    && let Some(at) = commands.iter().position(|c| c.id == entry.id)
                {
                    let earlier = commands[..at]
                        .iter()
                        .filter(|c| c.status == SessionCommandStatus::Pending)
                        .filter_map(|c| match &c.payload {
                            SessionCommandPayload::Run { message_id, .. } => {
                                Some(message_id.clone())
                            }
                            SessionCommandPayload::Steer { message_id, .. } => {
                                Some(message_id.clone().unwrap_or_else(|| c.id.clone()))
                            }
                            _ => None,
                        })
                        .collect();
                    sessions.cancel_sent_messages(&handle.chat_id, earlier);
                }
                self.interrupt_and_pause_queue(sessions, handle).await?;
                Ok((SessionCommandStatus::Applied, None))
            }
            SessionCommandPayload::RespondInput {
                request_id,
                answers,
            } => {
                if sessions.respond_input(chat_id, request_id, answers.clone())? {
                    return Ok((SessionCommandStatus::Applied, None));
                }
                // No live resolver. Only a request id the doc shows as an
                // OPEN question on a SETTLED entry gets the orphan fallback:
                // a mismatched or already-resolved id is a stale/buggy answer
                // and must still reject, and a still-streaming entry's
                // question belongs to the live run (a just-consumed resolver
                // racing a second answer must not spawn a duplicate turn).
                let questions = handle.doc.read_entries().ok().and_then(|entries| {
                    entries
                        .iter()
                        .rev()
                        .filter(|e| e.status != Some(MessageStatus::Streaming))
                        .find_map(|e| {
                            e.parts.iter().find_map(|p| match p {
                                MessagePart::Input {
                                    request_id: rid,
                                    questions,
                                    resolved: false,
                                    ..
                                } if rid == request_id => Some(questions.clone()),
                                _ => None,
                            })
                        })
                });
                let Some(questions) = questions else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no pending input request".into()),
                    ));
                };
                // Closed without an answer: there is nothing to deliver, and
                // a resumed turn would only say so. Record it and stop.
                if answers.is_empty() {
                    handle.doc.resolve_input(request_id, answers)?;
                    return Ok((
                        SessionCommandStatus::Applied,
                        Some("closed unanswered".into()),
                    ));
                }
                // The run died under the question (engine restart, crash).
                // The question is still open in the doc and the command is
                // durable, so honor it anyway — stamp the part resolved and
                // deliver the answers as the next (resumed) turn, the same
                // fallback a dead-run steer takes. The question UI stays up
                // until the user answers (user requirement); this is what
                // makes that answer still WORK.
                let request = sessions
                    .last_request(chat_id)
                    .or_else(|| self.request_from_chat_row(chat_id, ""));
                let Some(mut request) = request else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no pending input request and no prior run config".into()),
                    ));
                };
                request.prompt = respond_input_prompt(&questions, answers);
                request.resume = None; // dispatch re-derives the harness session
                request.attachments = Vec::new();
                if let Err(err) = handle.doc.resolve_input(request_id, answers) {
                    tracing::warn!(chat = %chat_id, request = %request_id, error = %err,
                        "orphaned input resolve failed");
                }
                let harness = self.harness_for_request(chat_id, &request);
                self.dispatch_with_source_context(sessions, chat_id, harness, request, None)
                    .await?;
                Ok((
                    SessionCommandStatus::Applied,
                    Some("answered as new turn".into()),
                ))
            }
        }
    }

    /// Park a prompt for a turn-boundary agent in the visible queue instead
    /// of its mailbox, keeping the message id so the transcript entry written
    /// at delivery is the same message. Steers lead ordinary rows.
    fn hold_until_turn_end(
        &self,
        handle: &Arc<ChatDocHandle>,
        message_id: &str,
        prompt: &str,
        issued_at: i64,
        steer: bool,
        request: Option<&onicode_proto::RunRequest>,
    ) -> Result<(), EngineError> {
        let item = QueuedMessage {
            id: message_id.to_string(),
            text: prompt.to_string(),
            attachments: request.map(|r| r.attachments.clone()).unwrap_or_default(),
            hold_for_turn_end: false,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: issued_at.min(now_ms()),
            edited_at: None,
            delivery_gate: None,
        };
        if handle.doc.read_queue()?.iter().any(|row| row.id == item.id) {
            return Ok(()); // a redelivered command: already held
        }
        if let Some(request) = request {
            lock(&handle.held_sends).insert(item.id.clone(), request.clone());
        }
        if steer {
            handle
                .doc
                .insert_queued(handle.steer_slot(&item.id)?, &item)?;
        } else {
            handle.doc.push_queued(&item)?;
        }
        // Sending is the deliberate action that thaws a queue frozen by Cancel.
        handle.queue_paused.store(false, Ordering::Release);
        handle.publish_queue();
        Ok(())
    }

    /// Put a typed prompt in front of a live agent: steer it in, or — with no
    /// live steerable run — deliver the durable command as the next turn.
    /// After an engine restart `last_request` is empty too, so rebuild the run
    /// config from the chat's workspace row (onicode derived dispatch config from
    /// the chat row the same way — sessions.ts:601-620); dispatch's engine-owned
    /// resume then reattaches the prior harness conversation.
    ///
    /// Turn-boundary drivers retain explicit steers until their next boundary.
    async fn deliver_prompt(
        &self,
        sessions: &SessionsEngine,
        handle: &Arc<ChatDocHandle>,
        prompt: &str,
        message_id: Option<String>,
        issued_at: i64,
    ) -> Result<(SessionCommandStatus, Option<String>), EngineError> {
        let chat_id = &handle.chat_id;
        // Explicit steering uses the run mailbox when the agent reads it
        // mid-turn. A turn-boundary agent would read it only after the turn,
        // so it waits in the queue, ahead of ordinary rows, and reaches the
        // transcript when it is actually delivered. Never interrupt to hurry
        // steering.
        let prompt = self.resolve_prompt_attachments(prompt);
        // A live turn without a mailbox can't take it either: the fresh
        // dispatch below would interrupt it.
        let unsteerable_turn =
            sessions.turn_in_flight(chat_id) && !sessions.live_run_steerable(chat_id);
        if !prompt.trim().is_empty()
            && (unsteerable_turn || sessions.defers_to_turn_end(chat_id, None))
        {
            let id = message_id.unwrap_or_else(new_id);
            self.hold_until_turn_end(handle, &id, &prompt, issued_at, true, None)?;
            return Ok((
                SessionCommandStatus::Applied,
                Some("held until the turn ends".into()),
            ));
        }
        // The transcript shows the send while mailbox backpressure holds it.
        // A pending agent update instead holds it in the queue below, where a
        // transcript copy would duplicate it until the update finishes.
        if !sessions.live_run_update_pending(chat_id)
            && let Some(message_id) = message_id.as_deref()
            && let Err(err) =
                handle.write_user_message(message_id, &prompt, issued_at.min(now_ms()))
        {
            tracing::warn!(chat = %chat_id, error = %err, "canonical user-message write failed");
        }
        match sessions
            .steer_at(chat_id, &prompt, message_id.clone(), issued_at)
            .await?
        {
            SteerOutcome::Accepted => {
                handle.queue_paused.store(false, Ordering::Release);
                Ok((SessionCommandStatus::Applied, None))
            }
            SteerOutcome::NotSteerable => {
                if let Some(message_id) = message_id.as_deref() {
                    handle.write_user_message(message_id, &prompt, issued_at.min(now_ms()))?;
                }
                let request = sessions
                    .last_request(chat_id)
                    .or_else(|| self.request_from_chat_row(chat_id, &prompt));
                let Some(mut request) = request else {
                    return Ok((
                        SessionCommandStatus::Rejected,
                        Some("no live run and no prior run config".into()),
                    ));
                };
                request.prompt = prompt;
                request.resume = None; // dispatch re-derives the harness session
                // A reused config must not re-inline the PREVIOUS turn's
                // images; this prompt's own refs (if any) ride its text.
                request.attachments = Vec::new();
                let harness = self.harness_for_request(chat_id, &request);
                self.dispatch_with_source_context(sessions, chat_id, harness, request, message_id)
                    .await?;
                handle.queue_paused.store(false, Ordering::Release);
                Ok((
                    SessionCommandStatus::Applied,
                    Some("queued as new turn".into()),
                ))
            }
            SteerOutcome::DeferredByUpdate => {
                let id = message_id.unwrap_or_else(new_id);
                self.hold_until_turn_end(handle, &id, &prompt, issued_at, true, None)?;
                // The completed turn's status publication normally re-drains
                // this queue. Also cover completion racing the enqueue itself.
                self.drain_queue(handle).await;
                Ok((
                    SessionCommandStatus::Applied,
                    Some("held until the agent update finishes".into()),
                ))
            }
        }
    }

    async fn capture_source_context(&self, cwd: &str) -> Option<ConversationSourceContext> {
        let repos = self.inner.repos.get()?;
        let path = Path::new(cwd);
        let identity = repos.checkout_identity(path).await.ok()?;
        let branch = repos.current_branch(path).await.ok()?;
        let head_sha = repos.head_sha(path).await.ok().flatten();
        Some(ConversationSourceContext {
            checkout_id: identity.id,
            repo_root: identity.root.to_string_lossy().into_owned(),
            cwd: cwd.to_string(),
            branch,
            head_sha,
            observed_at: chrono::Utc::now(),
        })
    }

    /// Every fresh harness dispatch crosses this boundary, including
    /// steer/input fallbacks and crash recovery. Capture immediately before
    /// dispatch so the conversation records the checkout the harness will
    /// actually observe, rather than whichever checkout state was current on
    /// an earlier turn.
    pub(crate) async fn dispatch_with_source_context(
        &self,
        sessions: &SessionsEngine,
        chat_id: &str,
        harness: HarnessId,
        request: onicode_proto::RunRequest,
        message_id: Option<String>,
    ) -> Result<String, EngineError> {
        if let Some(workspace) = self.workspace()
            && let Some(context) = self.capture_source_context(&request.cwd).await
            && let Err(err) = workspace.set_chat_source_context(chat_id, &context)
        {
            tracing::warn!(chat = %chat_id, error = %err, "conversation source stamp failed");
        }
        sessions
            .dispatch(chat_id, harness, request, message_id)
            .await
    }

    async fn materialize_worktree(
        &self,
        chat_id: &str,
        spec: &onicode_proto::WorktreeSpec,
    ) -> Result<(String, Option<onicode_proto::Worktree>), EngineError> {
        if let Some(ws) = self.workspace()
            && let Ok(Some(chat)) = ws.chat(chat_id)
            && let Some(cwd) = chat.cwd
            && crate::workspace_host::linked_worktree_root(std::path::Path::new(&cwd)).is_some_and(
                |root| same_file::is_same_file(&root, &spec.repo_path).unwrap_or(false),
            )
        {
            tracing::info!(chat = %chat_id, cwd = %cwd, "worktree spec: reusing the chat's existing worktree");
            return Ok((cwd, None));
        }
        let repos = self
            .inner
            .repos
            .get()
            .ok_or_else(|| EngineError::Other("repos engine not wired".into()))?;
        let worktree = repos
            .create_worktree(std::path::Path::new(&spec.repo_path), &spec.base)
            .await?;
        tracing::info!(
            chat = %chat_id,
            path = %worktree.path,
            branch = %worktree.branch,
            "worktree materialized for run"
        );
        Ok((worktree.path.clone(), Some(worktree)))
    }

    fn complete_worktree_setup_handoff(
        &self,
        command_id: &str,
        chat_id: &str,
        spec: &onicode_proto::WorktreeSpec,
        fresh_worktree: Option<&onicode_proto::Worktree>,
    ) {
        let Some((project_actions, terminals)) = self.inner.project_action_runtime.get() else {
            return;
        };
        let outcome = match (spec.space_id.as_deref(), fresh_worktree) {
            (Some(space_id), Some(worktree)) => self
                .resolve_and_launch_worktree_setup(
                    project_actions,
                    terminals,
                    space_id,
                    spec,
                    worktree,
                )
                .unwrap_or_else(|err| ProjectActionSetupHandoff {
                    setup_action: None,
                    setup_error: Some(err.to_string()),
                }),
            _ => ProjectActionSetupHandoff {
                setup_action: None,
                setup_error: None,
            },
        };
        if let Some(run) = &outcome.setup_action {
            let _ = terminals.associate_chat(&run.terminal.id, chat_id);
        }
        project_actions.complete_setup_handoff(command_id, chat_id, outcome);
    }

    fn resolve_and_launch_worktree_setup(
        &self,
        project_actions: &ProjectActionsStore,
        terminals: &Terminals,
        space_id: &str,
        spec: &onicode_proto::WorktreeSpec,
        worktree: &onicode_proto::Worktree,
    ) -> Result<ProjectActionSetupHandoff, EngineError> {
        let workspace = self
            .workspace()
            .ok_or_else(|| EngineError::Other("workspace host not wired".into()))?;
        let space = workspace
            .space(space_id)?
            .ok_or_else(|| EngineError::Other("Project not found".into()))?;
        if space.device_id != self.inner.config.device_id {
            return Err(EngineError::Other(
                "Project belongs to another device".into(),
            ));
        }
        let project_root = std::fs::canonicalize(&space.path)?;
        let requested_root = std::fs::canonicalize(&spec.repo_path)?;
        if project_root != requested_root {
            return Err(EngineError::Other(
                "Project path does not match worktree repository".into(),
            ));
        }
        // The store keys configuration by the original Space path, which may
        // be a symlink. Keep canonical paths for validation and execution only.
        let setup_action = project_actions
            .setup_action(space_id, std::path::Path::new(&space.path))?
            .map(|action| {
                launch_project_setup_action(
                    terminals,
                    &action,
                    &project_root,
                    std::path::Path::new(&worktree.path),
                    120,
                    32,
                )
            })
            .transpose()?;
        Ok(ProjectActionSetupHandoff {
            setup_action,
            setup_error: None,
        })
    }

    /// A steer-turned-run with no in-process `last_request` (engine restarted
    /// since the last turn): rebuild the run config from the chat's workspace
    /// row — cwd from the row, model/reasoning/options/sandbox from its config
    /// (composer defaults otherwise). `None` without a workspace host or row.
    // (Also the RespondInput dead-run fallback's config source.)
    pub(crate) fn request_from_chat_row(
        &self,
        chat_id: &str,
        prompt: &str,
    ) -> Option<onicode_proto::RunRequest> {
        let workspace = self.workspace()?;
        let chat = match workspace.chat(chat_id) {
            Ok(chat) => chat?,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "workspace chat read failed");
                return None;
            }
        };
        let config = chat.config;
        Some(onicode_proto::RunRequest {
            mcp: None,
            prompt: prompt.to_string(),
            harness: config.as_ref().map(|c| c.harness),
            model: config.as_ref().and_then(|c| c.model.clone()),
            reasoning: config.as_ref().and_then(|c| c.reasoning),
            model_options: config
                .as_ref()
                .map(|c| c.model_options.clone())
                .unwrap_or_default(),
            cwd: chat.cwd.unwrap_or_default(),
            sandbox: config
                .as_ref()
                .map(|c| c.sandbox)
                .unwrap_or(onicode_proto::SandboxLevel::WorkspaceWrite),
            auto_approve: false,
            access: config.as_ref().map(|c| c.access).unwrap_or_default(),
            plan: config.as_ref().is_some_and(|c| c.plan),
            attachments: Vec::new(),
            resume: None,
            worktree: None,
            env: Default::default(),
        })
    }

    fn save_snapshot(&self, handle: &ChatDocHandle) {
        handle.persistence.flush_sync();
    }

    /// A fork must be durable before publishing its discoverable registry row.
    pub(crate) fn persist_fork(&self, handle: &ChatDocHandle) -> Result<(), EngineError> {
        let bytes = handle.doc.export_snapshot()?;
        self.inner.store.save_snapshot(&handle.chat_id, &bytes)?;
        handle.snapshot_bytes.store(bytes.len(), Ordering::Relaxed);
        Ok(())
    }

    /// Persist every open doc now (shutdown path; bypasses the debounce).
    pub fn flush_all(&self) {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            self.save_snapshot(&handle);
        }
    }
}

/// The resumed-turn prompt for answers to a question whose run died: each
/// answer paired with its question text so the reattached conversation reads
/// naturally. Pure.
pub fn respond_input_prompt(
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> String {
    let mut lines = vec!["Answering your earlier question:".to_string()];
    for answer in answers {
        let picked = answer.labels.join(", ");
        let question = questions
            .iter()
            .find(|q| q.id == answer.question_id)
            .map(|q| q.question.trim())
            .filter(|q| !q.is_empty());
        match question {
            Some(question) => lines.push(format!("{question} — {picked}")),
            None => lines.push(picked),
        }
    }
    lines.join("\n")
}

/// Percent-encode one URL path segment of a sidecar part id. PART_RE's
/// alphabet includes `#` and `:` — legal in R2 keys and doc refs, but a raw
/// `#` in a URL is a fragment delimiter (the request would silently hit the
/// truncated key, colliding parts). The Worker decodes before validating.
fn encode_part_segment(part_id: &str) -> String {
    let mut out = String::with_capacity(part_id.len());
    for byte in part_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod transfer_progress_tests {
    use std::sync::Arc;

    use super::{DocHost, DocHostConfig, TransferProgressGuard};

    fn host() -> (tempfile::TempDir, DocHost) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(onicode_sync::DocsStore::open(dir.path()).expect("store opens"));
        let host = DocHost::new(
            store,
            DocHostConfig {
                device_id: "dev-test".into(),
                default_harness: onicode_proto::HarnessId::Mock,
                edge: None,
            },
        );
        (dir, host)
    }

    #[tokio::test]
    async fn whale_snapshot_opens_and_reopens_without_network() {
        let (_dir, host) = host();
        let source = onicode_doc::SessionDoc::init("persisted-whale").unwrap();
        for i in 0..2000 {
            source
                .push_message(&onicode_doc::SessionMessageEntry {
                    id: format!("row-{i}"),
                    role: onicode_doc::MessageRole::User,
                    parts: vec![onicode_doc::MessagePart::Text {
                        id: "text".into(),
                        text: "x".repeat(2048),
                    }],
                    created_at: i,
                    device_id: "remote".into(),
                    status: None,
                    continuation_of: None,
                    duration_ms: None,
                })
                .unwrap();
        }
        host.inner
            .store
            .save_snapshot("persisted-whale", &source.export_snapshot().unwrap())
            .unwrap();
        drop(source);
        let start = std::time::Instant::now();
        let handle = host.open("persisted-whale").unwrap();
        let rx = handle.watch_messages();
        assert_eq!(rx.borrow().entries.len(), 2000);
        eprintln!("offline whale cold open: {:?}", start.elapsed());
        drop(rx);
        // An unwatched commit clears the mirror; attach still serves local data.
        handle.publish_messages_if_watched();
        let start = std::time::Instant::now();
        assert_eq!(handle.watch_messages().borrow().entries.len(), 2000);
        eprintln!("offline whale rebuilt mirror: {:?}", start.elapsed());
    }

    #[tokio::test]
    async fn transcript_attach_and_unwatched_clear_share_a_critical_section() {
        let (_dir, host) = host();
        let handle = host.open("cached").unwrap();
        handle
            .write_user_message("row", "locally persisted transcript", 0)
            .unwrap();
        let rx = handle.watch_messages();
        assert_eq!(rx.borrow().entries.len(), 1);
        drop(rx);

        // Freeze attach's critical section. An unwatched publisher must not
        // pass its receiver check and clear the mirror while attach owns it.
        let guard = super::lock(&handle.transcript_import);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker_handle = handle.clone();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_handle.publish_messages_if_watched();
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        let result = done_rx.recv_timeout(std::time::Duration::from_millis(100));
        // Simulate the subscription attaching before the worker can inspect it.
        let rx = handle.messages_tx.subscribe();
        drop(guard);
        worker.join().unwrap();
        assert!(
            matches!(result, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "unwatched clear escaped attach's critical section"
        );
        assert_eq!(rx.borrow().entries.len(), 1, "no empty reset after attach");
        drop(rx);
        handle.publish_messages_if_watched();
        assert!(handle.messages_tx.borrow().entries.is_empty());
        assert_eq!(
            handle.watch_messages().borrow().entries.len(),
            1,
            "offline reopen rebuilds from local content"
        );
    }

    #[test]
    fn set_upserts_by_upload_id_and_clear_retires_only_its_entry() {
        let (_dir, host) = host();
        let rx = host.watch_transfers();
        assert!(rx.borrow().is_empty());

        host.transfer_progress_set("u1", "a.png", 0, 1_000);
        host.transfer_progress_set("u2", "b.png", 0, 400);
        host.transfer_progress_set("u1", "a.png", 300, 1_000);
        let snapshot = rx.borrow().clone();
        assert_eq!(snapshot.len(), 2, "upsert must not duplicate u1");
        let u1 = snapshot.iter().find(|t| t.upload_id == "u1").unwrap();
        assert_eq!((u1.done, u1.total), (300, 1_000));

        host.transfer_progress_clear("u1");
        let snapshot = rx.borrow().clone();
        assert_eq!(snapshot.len(), 1, "u2 must survive u1's retirement");
        assert_eq!(snapshot[0].upload_id, "u2");
    }

    #[test]
    fn guard_retires_the_entry_on_every_exit_path() {
        let (_dir, host) = host();
        let rx = host.watch_transfers();
        host.transfer_progress_set("u1", "a.png", 0, 9);
        {
            let _guard = TransferProgressGuard {
                host: &host,
                upload_id: "u1",
            };
            assert_eq!(rx.borrow().len(), 1);
            // An early return / error propagation drops the guard here.
        }
        assert!(
            rx.borrow().is_empty(),
            "a failed attempt must not leave a phantom ring behind"
        );
    }

    #[test]
    fn late_subscriber_sees_current_set_first() {
        let (_dir, host) = host();
        host.transfer_progress_set("u1", "a.png", 750, 1_000);
        // watch_stream's contract: current value first, then changes — a UI
        // attaching mid-transfer must render the ring immediately.
        let rx = host.watch_transfers();
        assert_eq!(rx.borrow().len(), 1);
        assert_eq!(rx.borrow()[0].done, 750);
    }
}

#[cfg(test)]
mod source_context_tests {
    use std::process::Command;
    use std::sync::Arc;

    use super::{DocHost, DocHostConfig};

    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn capture_source_context_reads_the_dispatch_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "feature/captured"]);
        git(&repo, &["config", "user.name", "Onicode Test"]);
        git(&repo, &["config", "user.email", "onicode@example.com"]);
        std::fs::write(repo.join("README.md"), "capture\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "capture"]);

        let store =
            Arc::new(onicode_sync::DocsStore::open(dir.path().join("docs")).expect("store opens"));
        let host = DocHost::new(
            store,
            DocHostConfig {
                device_id: "device-a".into(),
                default_harness: onicode_proto::HarnessId::Mock,
                edge: None,
            },
        );
        host.set_repos(crate::repos::Repos::new(
            &dir.path().join("data"),
            "device-a",
        ));

        let context = host
            .capture_source_context(repo.to_str().unwrap())
            .await
            .expect("source context");
        assert_eq!(context.branch, "feature/captured");
        assert_eq!(context.cwd, repo.to_string_lossy());
        assert_eq!(
            context.repo_root,
            repo.canonicalize().unwrap().to_string_lossy()
        );
        assert!(context.head_sha.is_some());
        assert!(!context.checkout_id.is_empty());
    }
}

#[cfg(test)]
mod part_segment_tests {
    use super::encode_part_segment;

    #[test]
    fn hash_and_colon_are_escaped_unreserved_pass_through() {
        assert_eq!(encode_part_segment("m1#c1"), "m1%23c1");
        assert_eq!(encode_part_segment("tool:call_9"), "tool%3Acall_9");
        assert_eq!(encode_part_segment("plain-id_0.diff~"), "plain-id_0.diff~");
    }
}

#[cfg(test)]
mod queued_message_prompt_tests {
    use super::{ATTACHMENT_ONLY_PROMPT, ATTACHMENT_PROMPT_HEADER, queued_message_prompt};

    #[test]
    fn dispatch_adds_the_attachment_transport_to_visible_queue_text() {
        let paths = vec!["/tmp/image.png".to_string()];
        assert_eq!(
            queued_message_prompt("inspect this", &paths),
            format!("inspect this\n\n{ATTACHMENT_PROMPT_HEADER}\n- /tmp/image.png")
        );
    }

    #[test]
    fn legacy_expanded_rows_are_not_expanded_twice() {
        let paths = vec!["/tmp/image.png".to_string()];
        let legacy = format!("inspect this\n\n{ATTACHMENT_PROMPT_HEADER}\n- /tmp/image.png");
        assert_eq!(queued_message_prompt(&legacy, &paths), legacy);
    }

    #[test]
    fn attachment_only_rows_get_a_non_empty_prompt_body() {
        let paths = vec!["/tmp/image.png".to_string()];
        assert_eq!(
            queued_message_prompt("", &paths),
            format!("{ATTACHMENT_ONLY_PROMPT}\n\n{ATTACHMENT_PROMPT_HEADER}\n- /tmp/image.png")
        );
    }
}

/// Per-chat background task: reacts to doc changes (local commits and remote imports)
/// by re-publishing the transcript watch, draining commands, and, once quiet, an eviction pass
/// (snapshots ride the chat's persister).
/// Holds only a weak handle so a dropped host tears the task down.
async fn chat_task(host: DocHost, weak: Weak<ChatDocHandle>, mut changed_rx: watch::Receiver<u64>) {
    // Prompt delivery may wait for mailbox capacity. Keep a separate watcher
    // for interrupt/question controls so that wait cannot block recovery.
    let control_host = host.clone();
    let control_weak = weak.clone();
    let mut control_changes = changed_rx.clone();
    host.spawn_worker(async move {
        loop {
            let Some(handle) = control_weak.upgrade() else {
                break;
            };
            control_host.drain_command_kind(&handle, true).await;
            drop(handle);
            if control_changes.changed().await.is_err() {
                break;
            }
        }
    });
    // Initial pass: the snapshot may already carry pending commands. The
    // mirror stays lazy — it materializes on the first watch attach.
    {
        let Some(handle) = weak.upgrade() else { return };
        host.drain_commands(&handle).await;
        host.drain_queue(&handle).await;
    }
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // doc handle (and its change sender) is gone
                }
                let Some(handle) = weak.upgrade() else { break };
                let publishing = handle.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    publishing.publish_messages_if_watched();
                    publishing.publish_queue();
                }).await;
                host.drain_commands(&handle).await;
                host.drain_queue(&handle).await;
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                if weak.strong_count() == 0 {
                    break;
                }
                // Post-quiesce eviction pass: sizes just refreshed.
                host.evict_over_budget();
            }
        }
    }
}

#[cfg(test)]
mod outbox_tests {
    use super::*;

    fn host(dir: &Path) -> DocHost {
        DocHost::new(
            Arc::new(DocsStore::open(dir).unwrap()),
            DocHostConfig {
                device_id: "writer".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        )
    }

    fn write(handle: &ChatDocHandle, text: &str) {
        let body = handle.doc.doc().get_text("body");
        body.insert(body.len_unicode(), text).unwrap();
        handle.doc.doc().commit();
    }

    #[tokio::test]
    async fn every_local_write_lands_in_the_outbox_and_replays_after_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let handle = host.open_local("chat").unwrap();
        write(&handle, "one ");
        write(&handle, "two");
        let store = host.store().clone();
        let rows = store.outbox("chat").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(handle.doc.doc().oplog_vv().includes_vv(&rows[1].end));
        let base = store
            .load_snapshot("chat")
            .unwrap()
            .expect("the base is saved on open");
        // A crash before the debounced snapshot: only the outbox holds the writes.
        drop(handle);
        host.shutdown_workers().await;
        store.save_snapshot("chat", &base).unwrap();
        for row in &rows {
            store.enqueue_outbox("chat", &row.bytes, &row.end).unwrap();
        }
        let reopened = DocHost::new(
            store,
            DocHostConfig {
                device_id: "writer".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let handle = reopened.open_local("chat").unwrap();
        assert_eq!(handle.doc.doc().get_text("body").to_string(), "one two");
        assert_eq!(
            reopened.store().outbox_len("chat").unwrap(),
            2,
            "replaying the outbox enqueues nothing new"
        );
        reopened.shutdown_workers().await;
    }

    #[tokio::test]
    async fn a_failed_outbox_write_pins_the_doc_until_a_snapshot_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let handle = host.open_local("failed").unwrap();
        let db = rusqlite::Connection::open(dir.path().join("docs.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER injected_disk_failure BEFORE INSERT ON chat_outbox \
             BEGIN SELECT RAISE(FAIL,'injected disk failure'); END;",
        )
        .unwrap();
        write(&handle, "must survive a failed write");
        assert_eq!(
            host.chat_sync_state("failed"),
            onicode_proto::ChatSyncState::StorageError
        );
        assert!(host.pinned(&handle));
        db.execute_batch("DROP TRIGGER injected_disk_failure;")
            .unwrap();
        host.save_snapshot(&handle);
        assert!(!host.pinned(&handle));
        assert_eq!(
            host.store().pending_forwards("", 8).unwrap(),
            ["failed"],
            "the write the outbox missed is still owed to the host"
        );
        drop(handle);
        host.shutdown_workers().await;
        let reopened = DocHost::new(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            DocHostConfig {
                device_id: "writer".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        assert_eq!(
            reopened
                .open_local("failed")
                .unwrap()
                .doc
                .doc()
                .get_text("body")
                .to_string(),
            "must survive a failed write"
        );
        reopened.shutdown_workers().await;
    }

    #[tokio::test]
    async fn caller_and_watch_protect_the_open_to_attach_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let held = host.open_local("held").unwrap();
        for i in 0..30 {
            host.open_local(&format!("other-{i}")).unwrap();
        }
        assert!(lock(&host.inner.handles).contains_key("held"));
        let watch = held.watch_messages();
        drop(held);
        for i in 30..60 {
            host.open_local(&format!("other-{i}")).unwrap();
        }
        assert!(lock(&host.inner.handles).contains_key("held"));
        drop(watch);
        host.open_local("overflow").unwrap();
        assert!(!lock(&host.inner.handles).contains_key("held"));
        host.shutdown_workers().await;
    }

    /// A doc host whose registry names another device as the host of `chats`.
    fn viewer(dir: &Path, chats: &[&str]) -> DocHost {
        let store = Arc::new(DocsStore::open(dir).unwrap());
        let host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: "viewer".into(),
                default_harness: HarnessId::Mock,
                edge: None,
            },
        );
        let workspace = WorkspaceHost::open(
            store,
            crate::workspace_host::WorkspaceHostConfig {
                device_id: "viewer".into(),
                device_name: "viewer".into(),
                platform: "macos".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
                local_only: false,
            },
        )
        .unwrap();
        for chat in chats {
            workspace
                .create_chat(chat, None, Some("host-device"), None, None)
                .unwrap();
        }
        host.set_workspace(workspace);
        host
    }

    #[tokio::test]
    async fn a_viewers_eviction_keeps_unacked_writes_owed_across_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let host = viewer(dir.path(), &["evicted"]);
        assert_eq!(
            host.chat_host("evicted"),
            ChatHost::Peer("host-device".into())
        );
        let handle = host.open_local("evicted").unwrap();
        write(&handle, "unacknowledged");
        drop(handle);
        for i in 0..=WARM_DOC_CAP {
            host.open_local(&format!("other-{i}")).unwrap();
        }
        host.evict_over_budget();
        assert!(!lock(&host.inner.handles).contains_key("evicted"));
        let before = host.store().outbox("evicted").unwrap();
        assert_eq!(before.len(), 1, "no host acked it, so it is still owed");
        let reopened = host.open_local("evicted").unwrap();
        assert_eq!(
            reopened.doc.doc().get_text("body").to_string(),
            "unacknowledged"
        );
        assert_eq!(host.store().outbox("evicted").unwrap(), before);
        drop(reopened);
        host.shutdown_workers().await;
    }
}
