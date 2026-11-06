//! Claude CLI stream-json wire frames (stdout JSONL + stdin lines).
//!
//! Tolerant by construction: every field defaults, unknown frame types map to
//! [`Frame::Other`], so a newer CLI never breaks parsing — we only read the
//! fields the normalizer needs (spec: docs/research/harness.md).

use serde::Deserialize;
use serde_json::{Value, json};

/// One parsed stdout line.
#[derive(Debug)]
pub(crate) enum Frame {
    System(Box<SystemFrame>),
    StreamEvent(StreamEventFrame),
    Assistant(MessageFrame),
    User(MessageFrame),
    RateLimit(RateLimitFrame),
    Result(ResultFrame),
    ControlRequest(ControlRequestFrame),
    /// The CLI's answer to one of our control requests.
    ControlResponse(ControlResponseFrame),
    /// Where the CLI has taken one of our stdin user messages.
    CommandLifecycle(CommandLifecycleFrame),
    /// `control_cancel_request`: the CLI no longer needs the answer to one
    /// of its own requests (the turn was interrupted, a steer cut the tool
    /// call off, another client answered it). No reply is written.
    ControlCancel(ControlCancelFrame),
    /// `tombstone`: a message already streamed is superseded or removed.
    Tombstone(TombstoneFrame),
    /// `tool_progress`: a long tool call is still running.
    ToolProgress(ToolProgressFrame),
    /// Anything unknown.
    Other,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlCancelFrame {
    #[serde(default)]
    pub request_id: String,
}

/// `{"type":"tombstone","message":{…}}`: a message already sent is
/// withdrawn (a stalled stream retried without streaming leaves its partial
/// message orphaned). The message is read loosely: whose feed it was on,
/// and any tool calls it opened.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct TombstoneFrame {
    #[serde(default)]
    pub message: Value,
}

impl TombstoneFrame {
    /// The `tool_use` ids the superseded message opened.
    pub fn tool_use_ids(&self) -> Vec<String> {
        let content = self
            .message
            .get("message")
            .and_then(|m| m.get("content"))
            .or_else(|| self.message.get("content"));
        content
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|b| b.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }

    /// Whether the superseded message belongs to a subagent's feed.
    pub fn parent_tool_use_id(&self) -> Option<&str> {
        self.message
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
    }
}

/// `{"type":"tool_progress","tool_use_id":…,"elapsed_time_seconds":…}`:
/// only whose call it is matters (a subagent's has no live consumer).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ToolProgressFrame {
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlResponseFrame {
    #[serde(default)]
    pub response: ControlResponseBody,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlResponseBody {
    #[serde(default)]
    pub request_id: String,
    /// `success` or `error`.
    #[serde(default)]
    pub subtype: String,
    /// Why an `error` response failed (a string on every CLI seen so far).
    #[serde(default)]
    pub error: Option<Value>,
    /// A success's payload: the initialize response, an interrupt receipt,
    /// a `rewind_files` or `get_context_usage` answer.
    #[serde(default)]
    pub response: Option<Value>,
}

impl ControlResponseBody {
    /// `initialize`'s `claude_code_version`. Nothing else of that response
    /// (its account block above all) is read.
    pub fn claude_code_version(&self) -> Option<&str> {
        self.response
            .as_ref()?
            .get("claude_code_version")?
            .as_str()
            .filter(|v| !v.trim().is_empty())
    }

    /// An `interrupt` receipt: the uuids of ours it cancelled; empty from a
    /// CLI that predates receipts.
    pub fn interrupt_receipt(&self) -> InterruptReceipt {
        let cancelled = self
            .response
            .as_ref()
            .and_then(|r| r.get("cancelled"))
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        InterruptReceipt { cancelled }
    }
}

/// What an `interrupt` control request did to the messages we wrote (its
/// `still_queued` list is not needed: whatever was not cancelled runs).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InterruptReceipt {
    /// Never to run; each also gets its terminal `cancelled` lifecycle.
    /// Empty from a CLI without receipts.
    pub cancelled: Vec<String>,
}

impl ControlResponseBody {
    /// The failure text of an `error` response; `None` for a success.
    pub fn error_message(&self) -> Option<String> {
        if self.subtype != "error" {
            return None;
        }
        Some(match &self.error {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
            Some(other @ Value::Object(_)) => other
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| other.to_string()),
            _ => "the request failed".to_owned(),
        })
    }
}

/// `{"type":"command_lifecycle","command_uuid":…,"state":…}` — emitted for
/// every stdin user message carrying a `uuid` (verified live against CLI
/// 2.1.286): `queued` when read, `started` when a turn takes it up (a `next`
/// steer starts mid-turn, at the step boundary it folds into), then
/// `completed` or `cancelled` (its turn was aborted by a later `now` steer
/// or an interrupt). Background-task wake turns carry no command at all.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CommandLifecycleFrame {
    #[serde(default)]
    pub command_uuid: String,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SystemFrame {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub session_id: String,
    /// `task_notification`: the spawning Agent tool's id, or the latest
    /// SendMessage id after a resume. Stop notifications may omit it.
    #[serde(default, alias = "toolUseId")]
    pub tool_use_id: Option<String>,
    /// `task_notification` terminal status (`completed`/`failed`/`killed`…).
    #[serde(default)]
    pub status: Option<String>,
    /// `task_started` / `task_notification`: stable agent/task id
    /// (`SendMessage`'s `to:` address), used to retain spawn identity.
    #[serde(default, alias = "taskId")]
    pub task_id: Option<String>,
    /// `task_started`: present only for AGENT tasks (a subagent spawning),
    /// absent on subagent-owned background shell tasks.
    #[serde(default)]
    pub subagent_type: Option<String>,
    /// `task_started`: the task's one-line name, `/<skill>` for a forked
    /// skill (live-verified 2.1.310).
    #[serde(default)]
    pub description: Option<String>,
    /// `task_started`: the subagent's opening prompt, a forked skill's body.
    /// A `Value`: a shape we don't expect must not cost the frame.
    #[serde(default)]
    pub prompt: Option<Value>,
    /// `background_tasks_changed`: every background task the session holds
    /// now (agents and shells) — the authoritative set, not a delta.
    #[serde(default)]
    pub tasks: Option<Vec<Value>>,
    /// `api_retry`: which retry this is, of how many, and the wait before it.
    #[serde(default)]
    pub attempt: Option<u64>,
    #[serde(default)]
    pub max_retries: Option<u64>,
    #[serde(default)]
    pub retry_delay_ms: Option<f64>,
    /// `api_retry`: the HTTP status and a terse error kind (`rate_limit`, …).
    #[serde(default)]
    pub error_status: Option<u64>,
    #[serde(default)]
    pub error: Option<Value>,
    /// `compact_boundary`: `{trigger, pre_tokens, post_tokens}`.
    #[serde(default)]
    pub compact_metadata: Option<Value>,
    /// `init` and `status`: the permission mode the CLI is in now. A `status`
    /// follows every change, ours or its own (leaving plan mode).
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: Option<String>,
    /// `session_state_changed`: `idle`, `running` or `requires_action`.
    /// `idle` comes once the turn's result is out and nothing the CLI holds
    /// back is left to run: the turn is over. Sent only to a host that sets
    /// `CLAUDE_CODE_SDK_READS_SESSION_STATE`.
    #[serde(default)]
    pub state: Option<String>,
    /// `turn_preempted`: the follow-up that superseded the turn.
    #[serde(default)]
    pub preempted_by_uuid: Option<String>,
    /// `hook_started` / `hook_response`: which hook, for which event. The
    /// hook's own output is never read.
    #[serde(default)]
    pub hook_id: Option<String>,
    #[serde(default)]
    pub hook_name: Option<String>,
    #[serde(default)]
    pub hook_event: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamEventFrame {
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub event: StreamEventBody,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamEventBody {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub delta: Delta,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
}

/// An `assistant` or `user` frame (an Anthropic API message envelope).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessageFrame {
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub message: MessageBody,
    /// Terse assistant-level error code (`rate_limit`, `billing_error`, …).
    #[serde(default)]
    pub error: Option<String>,
    /// A `user` frame's structured tool output (an Edit's or Write's
    /// `structuredPatch`, `originalFile`, `type`). A `Value`: its shape
    /// differs per tool.
    #[serde(default)]
    pub tool_use_result: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessageBody {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    /// Either a plain string or an array of content blocks.
    #[serde(default)]
    pub content: Value,
}

impl MessageBody {
    pub fn blocks(&self) -> impl Iterator<Item = ContentBlock> + '_ {
        self.content
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ContentBlock {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    /// A `thinking` block's (summarized) text.
    #[serde(default)]
    pub thinking: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub is_error: Option<bool>,
    /// A `tool_result`'s content: a string, or an array of content blocks.
    #[serde(default)]
    pub content: Value,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitFrame {
    #[serde(default)]
    pub rate_limit_info: RateLimitInfo,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitInfo {
    #[serde(default)]
    pub status: String,
    #[serde(rename = "rateLimitType", default)]
    pub rate_limit_type: Option<String>,
    /// `allowed`/`allowed_warning` when the account may spend provisioned
    /// overage past a rejected window.
    #[serde(rename = "overageStatus", default)]
    pub overage_status: Option<String>,
    #[serde(rename = "isUsingOverage", default)]
    pub is_using_overage: bool,
    #[serde(rename = "overageInUse", default)]
    pub overage_in_use: bool,
}

impl RateLimitInfo {
    /// A rejected window that actually blocks the turn: an account spending
    /// provisioned overage keeps running despite the reject.
    pub fn blocks(&self) -> bool {
        self.status == "rejected"
            && !matches!(
                self.overage_status.as_deref(),
                Some("allowed" | "allowed_warning")
            )
            && !self.is_using_overage
            && !self.overage_in_use
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ResultFrame {
    #[serde(default, rename = "modelUsage")]
    pub model_usage: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub errors: Vec<Value>,
    #[serde(default)]
    pub usage: UsageBody,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Set on API failures the CLI still files under `subtype: "success"`
    /// (an invalid key, an overloaded or rate-limited API): the turn did no
    /// real work.
    #[serde(default)]
    pub is_error: bool,
    /// Why the turn ended (`completed`, `aborted_streaming`, `api_error`,
    /// `prompt_too_long`, …).
    #[serde(default)]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub api_error_status: Option<u64>,
    /// Every user message (by the uuid we wrote it with) whose prompt this
    /// turn consumed, in order. Absent from older CLIs, which may still send
    /// the last one alone as `user_message_uuid`.
    #[serde(default)]
    pub user_message_uuids: Option<Vec<String>>,
    #[serde(default)]
    pub user_message_uuid: Option<String>,
    /// User sends still waiting in the CLI's queue: above 0, another turn
    /// (and result) follows without further input.
    #[serde(default)]
    pub queued_turn_count: Option<u64>,
    /// Set on the zeroed result a run writes before exiting on a known
    /// startup failure (`CLAUDE_CODE_STARTUP_FAILURE_RESULTS`).
    #[serde(default)]
    pub startup_failure_reason: Option<String>,
    /// Not a CLI field: the line said it was a result but could not be read
    /// (see [`parse_frame`]).
    #[serde(skip)]
    pub unreadable: bool,
}

impl ResultFrame {
    /// The uuids of ours this result's turn consumed: the list, else the
    /// lone `user_message_uuid`; `None` when the CLI reports neither.
    pub fn consumed(&self) -> Option<Vec<String>> {
        match (&self.user_message_uuids, &self.user_message_uuid) {
            (Some(list), _) => Some(list.clone()),
            (None, Some(one)) => Some(vec![one.clone()]),
            (None, None) => None,
        }
    }

    /// Stands in for a `result` line that would not parse. Dropping it would
    /// leave the turn open forever: the CLI's result is the only turn end.
    fn unreadable(line: &str) -> Self {
        Self {
            is_error: line.contains("\"is_error\":true"),
            unreadable: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct UsageBody {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// A CLI→client control request (`can_use_tool` is the one we act on).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlRequestFrame {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub request: ControlRequestBody,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlRequestBody {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub tool_name: String,
    /// `can_use_tool`: the tool's name as shown to people (an MCP tool's
    /// own), and what the model says the call is for.
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input: Value,
    /// `can_use_tool`: ready-made `PermissionUpdate`s ("always allow"
    /// rules) the host may echo back as `updatedPermissions`.
    #[serde(default)]
    pub permission_suggestions: Vec<Value>,
    /// `can_use_tool`: why the ask escalated (may carry ANSI escapes), and
    /// the path that made it ask.
    #[serde(default)]
    pub decision_reason: Option<String>,
    #[serde(default)]
    pub blocked_path: Option<String>,
    /// `can_use_tool`: accepting must not write a persistent rule.
    #[serde(default)]
    pub suppress_always_allow_rule: bool,
    /// `can_use_tool`: approve must not be pre-selected or one keystroke away.
    #[serde(default)]
    pub default_to_no: bool,
    /// `can_use_tool`: one-tap approve/deny must not be offered.
    #[serde(default)]
    pub requires_user_interaction: bool,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// `elicitation`: the MCP server asking, what it says, and how it wants
    /// the answer (`form` with a JSON schema, or `url` to open).
    #[serde(default)]
    pub mcp_server_name: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub requested_schema: Option<Value>,
    #[serde(default)]
    pub property_order: Vec<String>,
}

/// Parse one stdout JSONL line. `Err` = not JSON; unknown types = `Other`.
///
/// Two repairs before giving up on a line. Node's `JSON.stringify` writes a
/// string cut through a surrogate pair as a lone `\uD83D` escape, which
/// serde_json rejects: those become U+FFFD. An explicit `null` where a field
/// is normally present fails its typed parse: nulls read as absent. A
/// `result` line that still won't parse becomes an unreadable result rather
/// than an error, so the turn it ends still ends.
pub(crate) fn parse_frame(line: &str) -> Result<Frame, serde_json::Error> {
    let error = match parse_value(line).and_then(typed_frame) {
        Ok(frame) => return Ok(frame),
        Err(error) => error,
    };
    if let Ok(mut value) = parse_value(line) {
        strip_nulls(&mut value);
        if let Ok(frame) = typed_frame(value) {
            return Ok(frame);
        }
    }
    if top_level_type(line) == Some("result") {
        return Ok(Frame::Result(ResultFrame::unreadable(line)));
    }
    Err(error)
}

/// The `type` of a JSON object line, read at its top level alone however
/// torn the rest is: a `"type":"result"` nested in some other frame (a
/// tool's output, a message's content) must not end the turn.
fn top_level_type(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.checked_sub(1)?,
            b'"' => {
                let start = i + 1;
                let end = string_end(bytes, start)?;
                i = end + 1;
                if depth == 1
                    && &line[start..end] == "type"
                    && let Some(value) = line[i..].trim_start().strip_prefix(':')
                {
                    let value = value.trim_start().strip_prefix('"')?;
                    return Some(&value[..string_end(value.as_bytes(), 0)?]);
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index of the quote closing a JSON string whose contents start at `start`.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn parse_value(line: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(line).or_else(|error| match repair_lone_surrogates(line) {
        Some(repaired) => serde_json::from_str(&repaired),
        None => Err(error),
    })
}

fn typed_frame(value: Value) -> Result<Frame, serde_json::Error> {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let frame = match kind {
        "system" => Frame::System(serde_json::from_value(value)?),
        "stream_event" => Frame::StreamEvent(serde_json::from_value(value)?),
        "assistant" => Frame::Assistant(serde_json::from_value(value)?),
        "user" => Frame::User(serde_json::from_value(value)?),
        "rate_limit_event" => Frame::RateLimit(serde_json::from_value(value)?),
        "result" => Frame::Result(serde_json::from_value(value)?),
        "control_request" => Frame::ControlRequest(serde_json::from_value(value)?),
        "control_response" => Frame::ControlResponse(serde_json::from_value(value)?),
        "command_lifecycle" => Frame::CommandLifecycle(serde_json::from_value(value)?),
        "control_cancel_request" => Frame::ControlCancel(serde_json::from_value(value)?),
        "tombstone" => Frame::Tombstone(serde_json::from_value(value)?),
        "tool_progress" => Frame::ToolProgress(serde_json::from_value(value)?),
        _ => Frame::Other,
    };
    Ok(frame)
}

/// Drop every `null` object entry, so it reads as the absent field it means.
fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// `line` with every unpaired `\uD800`–`\uDFFF` escape replaced by
/// `\uFFFD`; `None` when there are none. Backslashes only occur inside JSON
/// strings, so escapes can be walked without tracking string boundaries.
fn repair_lone_surrogates(line: &str) -> Option<String> {
    fn unit(bytes: &[u8], at: usize) -> Option<u16> {
        if bytes.get(at..at + 2)? != b"\\u" {
            return None;
        }
        let hex = std::str::from_utf8(bytes.get(at + 2..at + 6)?).ok()?;
        u16::from_str_radix(hex, 16).ok()
    }
    let high = |u: u16| (0xd800..0xdc00).contains(&u);
    let low = |u: u16| (0xdc00..0xe000).contains(&u);
    let bytes = line.as_bytes();
    let mut out = String::new();
    let (mut at, mut copied) = (0, 0);
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            at += 1;
            continue;
        }
        let Some(u) = unit(bytes, at) else {
            // Any other escape: skip the escaped character with it.
            at += 2;
            continue;
        };
        if high(u) && unit(bytes, at + 6).is_some_and(low) {
            at += 12;
            continue;
        }
        if high(u) || low(u) {
            out.push_str(&line[copied..at]);
            out.push_str("\\uFFFD");
            copied = at + 6;
        }
        at += 6;
    }
    (copied > 0).then(|| {
        out.push_str(&line[copied..]);
        out
    })
}

/// A stdin user turn: `{"type":"user","message":{...},"parent_tool_use_id":null}`.
/// Steering = another such line mid-run (consumed at a step boundary).
#[cfg(test)]
pub(crate) fn user_message_line(text: &str) -> String {
    json!({
        "type": "user",
        "message": { "role": "user", "content": text },
        "parent_tool_use_id": null,
    })
    .to_string()
}

/// Fold this input into the next model step without aborting tools or tasks.
/// A steer line. `immediate` → `priority: "now"`: streaming text/thinking
/// stops at once and the steer is answered next (verified against CLI
/// 2.1.280). But `now` also aborts an in-flight MCP tool call ("The tool call
/// was interrupted before a result was received"), so while any tool is open
/// the steer goes as `next`: the tool finishes and the steer lands right after
/// its result, in the same turn. An interrupted turn still emits a `result`.
#[cfg(test)]
pub(crate) fn steer_message_line(text: &str, id: &str, immediate: bool) -> String {
    steer_message_line_with_images(text, id, immediate, Origin::Human, &[])
}

/// Who wrote a stdin user line. The CLI treats a line without an origin as
/// unattributed, and features that need a person behind the prompt (plugin
/// local slash commands among them) refuse it: a line the person typed says
/// so; one onicode composed itself (a revival's continuation) carries none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Human,
    Onicode,
}

impl Origin {
    fn stamp(self, line: &mut Value) {
        if self == Origin::Human {
            line["origin"] = json!({ "kind": "human" });
        }
    }
}

/// [`steer_message_line`] carrying inline images ahead of the text, in the
/// same block shape as [`user_message_line_with_images`].
pub(crate) fn steer_message_line_with_images(
    text: &str,
    id: &str,
    immediate: bool,
    origin: Origin,
    images: &[ImageBlock],
) -> String {
    let content = if images.is_empty() {
        Value::String(text.to_owned())
    } else {
        Value::Array(content_blocks(text, images))
    };
    let mut line = json!({"type":"user", "uuid":id, "priority": if immediate { "now" } else { "next" },
        "message":{"role":"user","content":content}, "parent_tool_use_id":null});
    origin.stamp(&mut line);
    line.to_string()
}

/// One inline image for a stdin user turn (Anthropic base64 image source).
pub(crate) struct ImageBlock {
    /// One of the API-supported media types (png/jpeg/gif/webp).
    pub media_type: String,
    /// Raw base64 (no data-URL prefix).
    pub data: String,
}

/// A stdin user turn whose content is an array of blocks: the attached images
/// first, then the text — the standard Anthropic image+text message shape
/// (verified against the real CLI: `--input-format stream-json` accepts image
/// content blocks in user frames). Empty `images` degrades to the plain line.
/// The run's opening prompt, tagged with `id` so its `command_lifecycle`
/// frames can be told apart from a steer's. Images precede the text, as in
/// [`user_message_line_with_images`].
pub(crate) fn prompt_line(text: &str, id: &str, origin: Origin, images: &[ImageBlock]) -> String {
    let content = if images.is_empty() {
        Value::String(text.to_owned())
    } else {
        Value::Array(content_blocks(text, images))
    };
    let mut line = json!({
        "type": "user",
        "uuid": id,
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null,
    });
    origin.stamp(&mut line);
    line.to_string()
}

#[cfg(test)]
pub(crate) fn user_message_line_with_images(text: &str, images: &[ImageBlock]) -> String {
    if images.is_empty() {
        return user_message_line(text);
    }
    json!({
        "type": "user",
        "message": { "role": "user", "content": content_blocks(text, images) },
        "parent_tool_use_id": null,
    })
    .to_string()
}

fn content_blocks(text: &str, images: &[ImageBlock]) -> Vec<Value> {
    let mut blocks: Vec<Value> = images
        .iter()
        .map(|img| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.media_type,
                    "data": img.data,
                },
            })
        })
        .collect();
    blocks.push(json!({ "type": "text", "text": text }));
    blocks
}

/// The run's opening control request. `perTaskStopAffordance` tells the CLI
/// this host stops tasks individually, so an interrupt ends the turn and
/// spares background agents — without it the CLI stops every background
/// agent on interrupt (verified live against 2.1.285; background shells
/// survive either way).
///
/// `supportedDialogKinds` is declared empty: onicode renders no
/// `request_user_dialog` kind, and the CLI fails such a flow closed (the
/// request is never sent). `rapidFollowupPreempt` lets the CLI stop a turn
/// that has shown nothing yet when a follow-up arrives, folding both into
/// one turn; it reports that as `turn_preempted` before the stopped turn's
/// result, which [`crate::claude`] treats as a boundary, not the turn's end.
pub(crate) fn initialize_request_line(request_id: &str, rapid_followup_preempt: bool) -> String {
    let mut request = json!({
        "subtype": "initialize",
        "perTaskStopAffordance": true,
        "supportedDialogKinds": [],
    });
    if rapid_followup_preempt {
        request["rapidFollowupPreempt"] = Value::Bool(true);
    }
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
    .to_string()
}

/// Restore the files the session changed since `user_message_id` (the uuid
/// a prompt was written with), or with `dry_run` only say what would change.
/// Needs a process launched with file checkpointing on.
pub(crate) fn rewind_files_request_line(
    request_id: &str,
    user_message_id: &str,
    dry_run: bool,
) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {
            "subtype": "rewind_files",
            "user_message_id": user_message_id,
            "dry_run": dry_run,
        },
    })
    .to_string()
}

/// Ask where the context window goes, by category. `summary` answers from
/// the last response's usage without per-category token-count calls.
pub(crate) fn context_usage_request_line(request_id: &str, full: bool) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {
            "subtype": "get_context_usage",
            "detail": if full { "full" } else { "summary" },
        },
    })
    .to_string()
}

/// Switch the live process's model (`None` = the user's default model).
pub(crate) fn set_model_request_line(request_id: &str, model: Option<&str>) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "set_model", "model": model },
    })
    .to_string()
}

/// Replace the live process's flag-layer settings (effort, fast mode, …).
pub(crate) fn apply_flag_settings_line(request_id: &str, settings: Value) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "apply_flag_settings", "settings": settings },
    })
    .to_string()
}

/// Switch the live process's permission mode. `bypassPermissions` only
/// works on a process launched with one of the skip-permissions flags
/// (verified live against 2.1.311).
pub(crate) fn set_permission_mode_line(request_id: &str, mode: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "set_permission_mode", "mode": mode },
    })
    .to_string()
}

/// Stop the in-flight turn only. `cancel_queued` drops steers the CLI has
/// queued but not yet consumed: a stopped turn never continues into them.
pub(crate) fn stop_turn_request_line(request_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt", "cancel_queued": true },
    })
    .to_string()
}

/// Stop one background task (`stop_task`): the CLI kills it, then reports
/// it `stopped` and the roster without it.
pub(crate) fn stop_task_request_line(request_id: &str, task_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "stop_task", "task_id": task_id },
    })
    .to_string()
}

/// Success reply to a CLI control request (`can_use_tool` allow/deny payloads).
pub(crate) fn control_response_line(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
    .to_string()
}

/// Error reply to a CLI control request we cannot serve. The CLI waits for
/// an answer to every request it sends; silence would stall it.
pub(crate) fn control_error_line(request_id: &str, error: &str) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error,
        },
    })
    .to_string()
}

/// `can_use_tool` allow payload with the (possibly updated) tool input.
pub(crate) fn allow_response(updated_input: Value) -> Value {
    json!({ "behavior": "allow", "updatedInput": updated_input })
}

/// `can_use_tool` allow that also adds `rules` (the CLI's own
/// `permission_suggestions`, as given or re-targeted) to its permissions,
/// so the same ask is not repeated. `permanent` classifies the decision as
/// a lasting one.
pub(crate) fn allow_with_rules_response(
    updated_input: Value,
    rules: Vec<Value>,
    tool_use_id: Option<&str>,
    permanent: bool,
) -> Value {
    let mut response = json!({
        "behavior": "allow",
        "updatedInput": updated_input,
        "decisionClassification": if permanent { "user_permanent" } else { "user_temporary" },
    });
    if !rules.is_empty() {
        response["updatedPermissions"] = Value::Array(rules);
    }
    if let Some(id) = tool_use_id {
        response["toolUseID"] = Value::String(id.to_owned());
    }
    response
}

/// `can_use_tool` deny with what the person said, and whether the turn
/// stops with it (`interrupt`).
pub(crate) fn deny_with_response(
    message: &str,
    interrupt: bool,
    tool_use_id: Option<&str>,
) -> Value {
    let mut response = json!({
        "behavior": "deny",
        "message": message,
        "decisionClassification": "user_reject",
    });
    if interrupt {
        response["interrupt"] = Value::Bool(true);
    }
    if let Some(id) = tool_use_id {
        response["toolUseID"] = Value::String(id.to_owned());
    }
    response
}

/// `can_use_tool` allow for `ExitPlanMode` that leaves plan mode for `mode`.
/// Without it the CLI drops to `default` (verified live against 2.1.311).
pub(crate) fn exit_plan_response(updated_input: Value, mode: &str) -> Value {
    json!({
        "behavior": "allow",
        "updatedInput": updated_input,
        "updatedPermissions": [{ "type": "setMode", "mode": mode, "destination": "session" }],
    })
}

/// `can_use_tool` deny payload: the model reads `message` as the result.
pub(crate) fn deny_response(message: &str) -> Value {
    json!({ "behavior": "deny", "message": message })
}

/// Client→CLI interrupt control request.
pub(crate) fn interrupt_request_line(request_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt" },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_priority_follows_tool_state() {
        let now: serde_json::Value =
            serde_json::from_str(&steer_message_line("hi", "u1", true)).unwrap();
        let next: serde_json::Value =
            serde_json::from_str(&steer_message_line("hi", "u2", false)).unwrap();
        assert_eq!(now["priority"], "now");
        assert_eq!(next["priority"], "next");
        assert_eq!(next["uuid"], "u2");
    }

    #[test]
    fn parses_known_and_unknown_frames() {
        let init = r#"{"type":"system","subtype":"init","model":"m","tools":["Bash"],"cwd":"/x","session_id":"s1"}"#;
        match parse_frame(init).expect("parses") {
            Frame::System(f) => {
                assert_eq!(f.subtype, "init");
                assert_eq!(f.session_id, "s1");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        assert!(matches!(
            parse_frame(r#"{"type":"mystery_frame"}"#).expect("parses"),
            Frame::Other
        ));
        assert!(parse_frame("not json").is_err());
    }

    #[test]
    fn lone_surrogate_escapes_are_repaired() {
        // A text delta cut through an emoji's surrogate pair.
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"a\ud83d b \ude00 \\ud83d 😀"}}}"#;
        assert!(serde_json::from_str::<Value>(line).is_err());
        match parse_frame(line).expect("repaired") {
            Frame::StreamEvent(f) => {
                // Lone halves become U+FFFD; an escaped backslash and a real
                // pair are left alone.
                assert_eq!(f.event.delta.text, "a\u{FFFD} b \u{FFFD} \\ud83d 😀");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        assert_eq!(repair_lone_surrogates(r#"{"a":"😀"}"#), None);
    }

    #[test]
    fn explicit_nulls_read_as_absent_fields() {
        let result = r#"{"type":"result","subtype":"success","usage":null,"modelUsage":null,"errors":null,"result":"ok","session_id":"s"}"#;
        match parse_frame(result).expect("parses") {
            Frame::Result(f) => {
                assert_eq!(f.result.as_deref(), Some("ok"));
                assert!(!f.unreadable);
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        let init = r#"{"type":"system","subtype":"init","model":null,"cwd":null,"tools":null,"session_id":"s1"}"#;
        match parse_frame(init).expect("parses") {
            Frame::System(f) => assert_eq!(f.session_id, "s1"),
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_result_still_ends_the_turn() {
        // A wrong-typed field fails the typed parse; a torn line fails JSON.
        for line in [
            r#"{"type":"result","subtype":"success","usage":{"input_tokens":"many"}}"#,
            r#"{"type":"result","subtype":"success","is_error":true,"result":"tor"#,
        ] {
            match parse_frame(line).expect("result frame") {
                Frame::Result(f) => {
                    assert!(f.unreadable);
                    assert_eq!(f.is_error, line.contains("is_error"));
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }
        // Anything else torn stays an error.
        assert!(parse_frame(r#"{"type":"assistant","message":"#).is_err());
        // Only the line's own type counts: a result nested in another frame
        // is no result, while a result whose type comes later still is.
        for line in [
            r#"{"type":"user","message":{"content":[{"type":"result","text":"tor"#,
            r#"{"type":"system","subtype":"init","session_id":5,"x":{"type":"result"}}"#,
            r#"{"type":"assistant","message":"say \"type\":\"result\" {","#,
        ] {
            assert!(parse_frame(line).is_err(), "{line}");
        }
        assert!(matches!(
            parse_frame(r#"{"subtype":"success","note":"{\"type\":1}","type":"result","res"#),
            Ok(Frame::Result(f)) if f.unreadable
        ));
    }

    #[test]
    fn control_response_errors_are_read() {
        let line = r#"{"type":"control_response","response":{"subtype":"error","request_id":"onicode_model_1","error":"Unknown model"}}"#;
        match parse_frame(line).expect("parses") {
            Frame::ControlResponse(f) => {
                assert_eq!(f.response.request_id, "onicode_model_1");
                assert_eq!(f.response.error_message().as_deref(), Some("Unknown model"));
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        let ok = r#"{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{}}}"#;
        match parse_frame(ok).expect("parses") {
            Frame::ControlResponse(f) => assert_eq!(f.response.error_message(), None),
            other => panic!("unexpected frame: {other:?}"),
        }
        let reply: Value = serde_json::from_str(&control_error_line("r1", "nope")).unwrap();
        assert_eq!(reply["response"]["subtype"], "error");
        assert_eq!(reply["response"]["request_id"], "r1");
    }

    #[test]
    fn user_line_shape_matches_protocol() {
        let line = user_message_line("hi");
        let v: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"], "hi");
        assert!(v["parent_tool_use_id"].is_null());
    }

    #[test]
    fn user_line_with_images_is_blocks_then_text() {
        let line = user_message_line_with_images(
            "what is this?",
            &[ImageBlock {
                media_type: "image/png".into(),
                data: "QUJD".into(),
            }],
        );
        let v: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["type"], "user");
        let content = v["message"]["content"].as_array().expect("array content");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "QUJD");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "what is this?");
        // No images ⇒ identical to the plain string line.
        assert_eq!(
            user_message_line_with_images("hi", &[]),
            user_message_line("hi")
        );
    }

    #[test]
    fn cancels_tombstones_and_tool_progress_parse() {
        match parse_frame(r#"{"type":"control_cancel_request","request_id":"cr-7"}"#).unwrap() {
            Frame::ControlCancel(f) => assert_eq!(f.request_id, "cr-7"),
            other => panic!("unexpected frame: {other:?}"),
        }
        let tomb = r#"{"type":"tombstone","message":{"type":"assistant","uuid":"m1","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"hal"},{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}}"#;
        match parse_frame(tomb).unwrap() {
            Frame::Tombstone(f) => {
                assert_eq!(f.tool_use_ids(), vec!["t1"]);
                assert_eq!(f.parent_tool_use_id(), None);
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        let sub = r#"{"type":"tombstone","message":{"parent_tool_use_id":"agent-1","content":[]}}"#;
        match parse_frame(sub).unwrap() {
            Frame::Tombstone(f) => assert_eq!(f.parent_tool_use_id(), Some("agent-1")),
            other => panic!("unexpected frame: {other:?}"),
        }
        let progress = r#"{"type":"tool_progress","tool_use_id":"t1","parent_tool_use_id":null,"elapsed_time_seconds":12.5}"#;
        match parse_frame(progress).unwrap() {
            Frame::ToolProgress(f) => assert_eq!(f.parent_tool_use_id, None),
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    fn response_body(raw: &str) -> ControlResponseBody {
        match parse_frame(raw).unwrap() {
            Frame::ControlResponse(f) => f.response,
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[test]
    fn receipts_and_versions_are_read_from_responses() {
        let receipt = response_body(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"s1","response":{"still_queued":["b"],"cancelled":["a","c"]}}}"#,
        );
        assert_eq!(receipt.interrupt_receipt().cancelled, vec!["a", "c"]);
        // A CLI without receipts cancels nothing we can name.
        let bare = response_body(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"s1"}}"#,
        );
        assert!(bare.interrupt_receipt().cancelled.is_empty());
        assert_eq!(bare.claude_code_version(), None);
        let init = response_body(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"onicode_initialize","response":{"claude_code_version":"2.1.320","account":{"email":"x@y"}}}}"#,
        );
        assert_eq!(init.claude_code_version(), Some("2.1.320"));
    }

    #[test]
    fn results_name_the_messages_they_consumed() {
        let result = |extra: &str| match parse_frame(&format!(
            r#"{{"type":"result","subtype":"success","result":"ok"{extra}}}"#
        ))
        .unwrap()
        {
            Frame::Result(f) => f,
            other => panic!("unexpected frame: {other:?}"),
        };
        let listed = result(r#","user_message_uuids":["u1","u2"],"queued_turn_count":1"#);
        assert_eq!(listed.consumed(), Some(vec!["u1".into(), "u2".into()]));
        assert_eq!(listed.queued_turn_count, Some(1));
        let one = result(r#","user_message_uuid":"u3""#);
        assert_eq!(one.consumed(), Some(vec!["u3".into()]));
        assert_eq!(result("").consumed(), None);
    }

    #[test]
    fn typed_lines_say_a_person_wrote_them() {
        let prompt: Value =
            serde_json::from_str(&prompt_line("hi", "u1", Origin::Human, &[])).unwrap();
        assert_eq!(prompt["origin"]["kind"], "human");
        assert_eq!(prompt["uuid"], "u1");
        let composed: Value =
            serde_json::from_str(&prompt_line("go on", "u2", Origin::Onicode, &[])).unwrap();
        assert!(composed.get("origin").is_none());
        let steer: Value = serde_json::from_str(&steer_message_line_with_images(
            "also",
            "u3",
            false,
            Origin::Human,
            &[],
        ))
        .unwrap();
        assert_eq!(steer["origin"]["kind"], "human");
        assert_eq!(steer["priority"], "next");
    }

    #[test]
    fn initialize_declares_what_this_host_handles() {
        let plain: Value =
            serde_json::from_str(&initialize_request_line("onicode_initialize", false)).unwrap();
        let request = &plain["request"];
        assert_eq!(request["subtype"], "initialize");
        assert_eq!(request["perTaskStopAffordance"], true);
        assert_eq!(request["supportedDialogKinds"], serde_json::json!([]));
        assert!(request.get("rapidFollowupPreempt").is_none());
        let rapid: Value =
            serde_json::from_str(&initialize_request_line("onicode_initialize", true)).unwrap();
        assert_eq!(rapid["request"]["rapidFollowupPreempt"], true);
    }

    #[test]
    fn queries_and_approval_replies_take_the_wire_shape() {
        let rewind: Value =
            serde_json::from_str(&rewind_files_request_line("q1", "u1", true)).unwrap();
        assert_eq!(rewind["request"]["subtype"], "rewind_files");
        assert_eq!(rewind["request"]["user_message_id"], "u1");
        assert_eq!(rewind["request"]["dry_run"], true);
        let usage: Value = serde_json::from_str(&context_usage_request_line("q2", false)).unwrap();
        assert_eq!(usage["request"]["subtype"], "get_context_usage");
        assert_eq!(usage["request"]["detail"], "summary");
        let rule = serde_json::json!({"type": "addRules", "destination": "session"});
        let allow =
            allow_with_rules_response(Value::Null, vec![rule.clone()], Some("toolu_1"), false);
        assert_eq!(allow["behavior"], "allow");
        assert_eq!(allow["updatedPermissions"], serde_json::json!([rule]));
        assert_eq!(allow["toolUseID"], "toolu_1");
        assert_eq!(allow["decisionClassification"], "user_temporary");
        let once = allow_with_rules_response(Value::Null, Vec::new(), None, false);
        assert!(once.get("updatedPermissions").is_none());
        assert!(once.get("toolUseID").is_none());
        let stop = deny_with_response("no", true, None);
        assert_eq!(stop["behavior"], "deny");
        assert_eq!(stop["interrupt"], true);
        assert_eq!(stop["decisionClassification"], "user_reject");
        assert!(
            deny_with_response("no", false, None)
                .get("interrupt")
                .is_none()
        );
    }

    #[test]
    fn approval_and_elicitation_requests_read_their_fields() {
        let raw = r#"{"type":"control_request","request_id":"cr-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1","permission_suggestions":[{"type":"addRules"}],"decision_reason":"why","blocked_path":"/x","suppress_always_allow_rule":true,"default_to_no":true,"requires_user_interaction":true}}"#;
        let Frame::ControlRequest(req) = parse_frame(raw).unwrap() else {
            panic!("not a control request");
        };
        let body = req.request;
        assert_eq!(body.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(body.permission_suggestions.len(), 1);
        assert_eq!(body.decision_reason.as_deref(), Some("why"));
        assert_eq!(body.blocked_path.as_deref(), Some("/x"));
        assert!(body.suppress_always_allow_rule && body.default_to_no);
        assert!(body.requires_user_interaction);
        let raw = r#"{"type":"control_request","request_id":"cr-2","request":{"subtype":"elicitation","mcp_server_name":"gh","message":"Sign in","mode":"url","url":"https://x.test","property_order":["a"]}}"#;
        let Frame::ControlRequest(req) = parse_frame(raw).unwrap() else {
            panic!("not a control request");
        };
        assert_eq!(req.request.mcp_server_name.as_deref(), Some("gh"));
        assert_eq!(req.request.mode.as_deref(), Some("url"));
        assert_eq!(req.request.url.as_deref(), Some("https://x.test"));
        assert_eq!(req.request.property_order, vec!["a"]);
    }
}
