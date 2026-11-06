//! Workspace registry — the row table behind the sidebar, replicated engine to engine and to thin
//! clients by per-origin version vectors (docs/registry-sync.md).
//!
//! Every replica is an origin. A local write applies at once, bumps the replica's `oseq` and
//! stamps the touched row's `vv[origin]`. A peer that holds `vv` is sent
//! [`RegistryDoc::rows_since`]`(vv)`; the receiver merges each row ([`merge_row`]) and raises its
//! vv to the sender's ([`RegistryDoc::merge_remote`]). The merge is a join: per-field
//! last-writer-wins by HLC, a deletion clock that drops every field written at or before it, row
//! vvs by pointwise max, and a row's `deviceId` kept from its first write. Replicas therefore
//! converge whatever the order or path rows take.
//!
//! Owner-only fields ([`owner_only`]: a device's own row, a space's git stamps, a chat's
//! host-written fields, session status) are accepted only from the row's owner — the device in
//! the field's clock must be the row's `deviceId` (a device row's own id) — both when written
//! locally and at merge, so a non-owner can never change them anywhere (I5). A chat's placement
//! ([`placement`]: where it runs) is accepted from the owner and from the row's creator, the
//! device that wrote its surviving `deviceId`: a viewer creates chats for a host, and nobody
//! else may move one. Deleting a row is not a field write; any device may.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use onicode_proto::{
    Chat, ChatConfig, Device, MAX_SIDEBAR_PINS, Session, SidebarPreferences, Space,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::schema::DocError;
use crate::workspace::{DeletedSpace, WorkspaceState};

/// Row kinds — synced sidebar entities plus user preferences.
pub const KIND_DEVICES: &str = "devices";
pub const KIND_SPACES: &str = "spaces";
pub const KIND_CHATS: &str = "chats";
pub const KIND_SESSIONS: &str = "sessions";
pub const KIND_PREFERENCES: &str = "preferences";

/// Readiness only; membership and order live on individual pins.
pub const SIDEBAR_PINS_STATE_ID: &str = "sidebarPins";
pub const KIND_SIDEBAR_PINS: &str = "sidebarPins";
mod sidebar_pins;
mod sidebar_sections;

/// Snapshot row id in the local `DocsStore` for the persisted registry state.
pub const REGISTRY_DOC_ID: &str = "registry1";

/// The field naming a row's owner.
const DEVICE_ID: &str = "deviceId";

/// A version vector: per origin, the highest `oseq` held with everything before it.
pub type Vv = BTreeMap<String, u64>;

// ── HLC ─────────────────────────────────────────────────────────────────────

/// Encode an HLC string: `{ms:013}-{counter:06}-{device}`. Fixed-width zero
/// padding makes lexicographic order = (ms, counter, device) order, and the
/// device suffix makes the order total (two writers can never tie).
pub fn encode_hlc(ms: i64, counter: u32, device: &str) -> String {
    format!("{ms:013}-{counter:06}-{device}")
}

/// The device that wrote `clock`.
#[must_use]
pub fn clock_writer(clock: &str) -> &str {
    clock.splitn(3, '-').nth(2).unwrap_or_default()
}

/// `a` strictly newer than `b` (`None` = never written, loses to any).
fn hlc_newer(a: &str, b: Option<&str>) -> bool {
    match b {
        None => true,
        Some(b) => a > b,
    }
}

/// Monotonic HLC source: never emits the same or an earlier clock twice, even
/// across a wall-clock regression or restart (state persists with the doc).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HlcClock {
    last_ms: i64,
    counter: u32,
}

impl HlcClock {
    /// Moves past `clock` (a peer's write), so the next local write outranks every write this
    /// replica has seen, even one it stamped itself before a restore.
    fn observe(&mut self, clock: &str) {
        let mut parts = clock.splitn(3, '-');
        let (Some(Ok(ms)), Some(Ok(counter))) = (
            parts.next().map(str::parse::<i64>),
            parts.next().map(str::parse::<u32>),
        ) else {
            return;
        };
        if (ms, counter) > (self.last_ms, self.counter) {
            self.last_ms = ms;
            self.counter = counter;
        }
    }

    fn next(&mut self, now_ms: i64, device: &str) -> String {
        if now_ms > self.last_ms {
            self.last_ms = now_ms;
            self.counter = 0;
        } else {
            self.counter += 1;
            if self.counter > 999_999 {
                self.last_ms += 1;
                self.counter = 0;
            }
        }
        encode_hlc(self.last_ms, self.counter, device)
    }
}

// ── rows ────────────────────────────────────────────────────────────────────

/// One row: its fields with their last-write clocks, a deletion clock, and which origins' writes
/// it holds. This is also the wire shape of the `registry` stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRow {
    pub kind: String,
    pub id: String,
    /// Per origin, the highest `oseq` whose write touched this row.
    #[serde(default)]
    pub vv: Vv,
    /// No field outlives the deletion clock.
    #[serde(default)]
    pub deleted: bool,
    /// Deletion clock: every field written at or before it is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub del_hlc: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    /// Per-field last-write clocks; a clock without a field is a cleared field.
    #[serde(default)]
    pub clocks: BTreeMap<String, String>,
}

impl RegistryRow {
    fn empty(kind: &str, id: &str) -> Self {
        Self {
            kind: kind.to_string(),
            id: id.to_string(),
            vv: Vv::new(),
            deleted: false,
            del_hlc: None,
            fields: BTreeMap::new(),
            clocks: BTreeMap::new(),
        }
    }

    /// The newest clock anywhere on the row.
    fn max_clock(&self) -> Option<&str> {
        let mut max = self.del_hlc.as_deref();
        for clock in self.clocks.values() {
            if max.is_none_or(|m| clock.as_str() > m) {
                max = Some(clock);
            }
        }
        max
    }

    /// The device whose writes the row's owner-only fields must come from; `None` for kinds
    /// every device writes, or while the row has no owner yet.
    #[must_use]
    pub fn owner(&self) -> Option<&str> {
        match self.kind.as_str() {
            KIND_DEVICES => Some(&self.id),
            KIND_SPACES | KIND_CHATS | KIND_SESSIONS => {
                self.fields.get(DEVICE_ID).and_then(Value::as_str)
            }
            _ => None,
        }
    }

    /// The device that wrote the row's surviving `deviceId`: who created it.
    #[must_use]
    pub fn creator(&self) -> Option<&str> {
        if !owned_by_device_id(&self.kind) {
            return None;
        }
        self.clocks
            .get(DEVICE_ID)
            .map(String::as_str)
            .map(clock_writer)
    }

    /// Whether the write of `key` at `clock` stands (see [`admits`]).
    fn accepts(&self, key: &str, clock: &str) -> bool {
        admits(
            &self.kind,
            key,
            clock_writer(clock),
            self.owner(),
            self.creator(),
        )
    }

    /// Drops the fields the deletion clock covers.
    fn drop_deleted(&mut self) -> bool {
        let Some(del) = self.del_hlc.clone() else {
            return false;
        };
        let gone: Vec<String> = self
            .clocks
            .iter()
            .filter(|(_, clock)| clock.as_str() <= del.as_str())
            .map(|(key, _)| key.clone())
            .collect();
        self.remove_fields(gone)
    }

    fn remove_fields(&mut self, keys: Vec<String>) -> bool {
        let changed = !keys.is_empty();
        for key in keys {
            self.clocks.remove(&key);
            self.fields.remove(&key);
        }
        changed
    }

    /// Drops what the deletion clock covers, then the writes the owner and creator left
    /// standing do not admit, and settles `deleted`; whether anything changed.
    ///
    /// In that order: the deletion may take the owner with it, and what the owner admitted
    /// must go with it.
    fn settle(&mut self) -> bool {
        let mut changed = self.drop_deleted();
        let refused: Vec<String> = self
            .clocks
            .iter()
            .filter(|(key, clock)| !self.accepts(key, clock))
            .map(|(key, _)| key.clone())
            .collect();
        changed |= self.remove_fields(refused);
        let deleted = self.del_hlc.is_some() && self.clocks.is_empty();
        if deleted != self.deleted {
            self.deleted = deleted;
            changed = true;
        }
        changed
    }
}

/// Whether `key` on a row of `kind` may be written only by the row's owner.
#[must_use]
pub fn owner_only(kind: &str, key: &str) -> bool {
    match kind {
        KIND_DEVICES => key != "name",
        KIND_SPACES => matches!(
            key,
            "gitDetected" | "gitCheckedAt" | "checkoutId" | "repositoryId" | "githubRepo"
        ),
        KIND_CHATS => matches!(
            key,
            "harnessSessionId"
                | "harnessSessionCwd"
                | "harnessSessionHarness"
                | "lastMessagePreview"
                | "lastMessageAt"
                | "checkoutId"
                | "sourceContext"
                | "roomGen"
        ),
        KIND_SESSIONS => key != DEVICE_ID,
        _ => false,
    }
}

/// Whether `key` on a row of `kind` says where it runs: written by its owner or its creator,
/// nobody else.
#[must_use]
pub fn placement(kind: &str, key: &str) -> bool {
    kind == KIND_CHATS && matches!(key, "cwd" | "branch" | "spaceId")
}

/// Whether a write of `key` by `writer` stands on a row with this owner and creator: owner-only
/// fields only from the owner, placement from the owner or the creator, and neither while the
/// row has no owner.
fn admits(kind: &str, key: &str, writer: &str, owner: Option<&str>, creator: Option<&str>) -> bool {
    if owner_only(kind, key) {
        owner == Some(writer)
    } else if placement(kind, key) {
        owner.is_some() && (owner == Some(writer) || creator == Some(writer))
    } else {
        true
    }
}

/// Kinds whose `deviceId` names the owner: the oldest write of it after the deletion clock
/// stands.
fn owned_by_device_id(kind: &str) -> bool {
    matches!(kind, KIND_SPACES | KIND_CHATS | KIND_SESSIONS)
}

/// Merges `incoming` into `local` (`None` = not held yet); the new row when anything changed.
///
/// A join, so the result is the same whatever order rows arrive in: the newer deletion clock,
/// per field the newer write (an owner-only field only from the owner), the oldest `deviceId`,
/// and per origin the larger oseq. Two devices writing different `deviceId`s for one row at
/// once can still disagree on owner-only fields; ownership never moves, so only a creation race
/// could do that.
#[must_use]
pub fn merge_row(local: Option<&RegistryRow>, incoming: &RegistryRow) -> Option<RegistryRow> {
    let kind = incoming.kind.as_str();
    let mut row = local
        .cloned()
        .unwrap_or_else(|| RegistryRow::empty(kind, &incoming.id));
    let mut changed = local.is_none();
    if let Some(del) = incoming.del_hlc.as_deref()
        && hlc_newer(del, row.del_hlc.as_deref())
    {
        row.del_hlc = Some(del.to_string());
        changed = true;
        // The owner the deletion covers must not outrank a later one below.
        changed |= row.settle();
    }
    // The owner first: it decides which of the other writes count. The oldest `deviceId`
    // written after the deletion clock, whatever order the writes arrive in.
    if owned_by_device_id(kind)
        && let Some(clock) = incoming.clocks.get(DEVICE_ID)
        && row
            .del_hlc
            .as_deref()
            .is_none_or(|del| clock.as_str() > del)
        && row.clocks.get(DEVICE_ID).is_none_or(|held| clock < held)
    {
        put_field(&mut row, incoming, DEVICE_ID, clock);
        changed = true;
    }
    for (key, clock) in &incoming.clocks {
        if owned_by_device_id(kind) && key == DEVICE_ID {
            continue;
        }
        // A refused write must not shadow the owner's older one.
        if !row.accepts(key, clock) {
            continue;
        }
        // Per field, the newest write the merged row's owner and creator admit. A held write
        // this merge's owner or creator no longer admits (the creator turned out to be an
        // older writer) loses to any admitted one, however old.
        let held = row.clocks.get(key).map(String::as_str);
        if held.is_none_or(|held| !row.accepts(key, held)) || hlc_newer(clock, held) {
            put_field(&mut row, incoming, key, clock);
            changed = true;
        }
    }
    for (origin, &oseq) in &incoming.vv {
        let held = row.vv.entry(origin.clone()).or_default();
        if oseq > *held {
            *held = oseq;
            changed = true;
        }
    }
    changed |= row.settle();
    changed.then_some(row)
}

fn put_field(row: &mut RegistryRow, incoming: &RegistryRow, key: &str, clock: &str) {
    match incoming.fields.get(key) {
        Some(value) => row.fields.insert(key.to_string(), value.clone()),
        None => row.fields.remove(key),
    };
    row.clocks.insert(key.to_string(), clock.to_string());
}

/// What a local write does to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    /// Creates, and revives a deleted row.
    Upsert,
    /// Writes only a live row ("never invent rows").
    Update,
    /// Deletes every field written so far.
    Delete,
}

/// One local write: the vocabulary the typed API writes in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowOp {
    pub kind: String,
    pub id: String,
    pub op: OpKind,
    /// Field writes; `Value::Null` clears the field (still a clocked write).
    /// Absent for deletes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<BTreeMap<String, Value>>,
    /// Clock for every write in `set` without an entry in `clocks`.
    pub hlc: String,
    /// Per-field clock overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clocks: Option<BTreeMap<String, String>>,
}

impl RowOp {
    fn clock_for<'a>(&'a self, field: &str) -> &'a str {
        self.clocks
            .as_ref()
            .and_then(|c| c.get(field))
            .map_or(self.hlc.as_str(), String::as_str)
    }
}

// ── the doc ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    v: u32,
    clock: HlcClock,
    oseq: u64,
    vv: Vv,
    rows: Vec<RegistryRow>,
}

/// The edge-room snapshot this replaces: its rows, plus writes the room never acked.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EdgeState {
    clock: HlcClock,
    rows: Vec<RegistryRow>,
    #[serde(default)]
    pending: Vec<EdgeBatch>,
}

#[derive(Deserialize)]
struct EdgeBatch {
    ops: Vec<RowOp>,
}

/// The local registry replica. Pure data — no I/O, no async; the `registry` stream sessions
/// (`onicode_sync::registry`) and the hosts drive it under a lock.
pub struct RegistryDoc {
    device_id: String,
    /// kind → id → row, tombstones included.
    rows: HashMap<String, HashMap<String, RegistryRow>>,
    clock: HlcClock,
    /// This replica's write counter as an origin.
    oseq: u64,
    vv: Vv,
    /// Bumped on every change (local or merged) — the hosts turn it into watch-channel
    /// publishes and snapshot debounces.
    generation: u64,
}

impl RegistryDoc {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            rows: HashMap::new(),
            clock: HlcClock::default(),
            oseq: 0,
            vv: Vv::new(),
            generation: 0,
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Monotonic local change counter (any change bumps it).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Everything this replica holds, per origin.
    pub fn vv(&self) -> &Vv {
        &self.vv
    }

    /// Runs `f` writing as `device`: its clocks, its origin and its ownership. For the demo, which
    /// plays every device of a made-up workspace in one local replica.
    pub fn write_as<R>(&mut self, device: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        let oseq = self.vv.get(device).copied().unwrap_or_default();
        let own_oseq = std::mem::replace(&mut self.oseq, oseq);
        let own = std::mem::replace(&mut self.device_id, device.to_owned());
        let result = f(self);
        self.device_id = own;
        self.oseq = own_oseq;
        result
    }

    // ── persistence ─────────────────────────────────────────────────────────

    pub fn to_bytes(&self) -> Result<Vec<u8>, DocError> {
        let state = PersistedState {
            v: 2,
            clock: self.clock.clone(),
            oseq: self.oseq,
            vv: self.vv.clone(),
            rows: self
                .rows
                .values()
                .flat_map(|m| m.values().cloned())
                .collect(),
        };
        Ok(serde_json::to_vec(&state)?)
    }

    /// Loads a snapshot. The edge-room snapshot (`v: 1`) loads once: its rows and unacked
    /// writes become this replica's own, stamped so they reach every peer.
    pub fn from_bytes(bytes: &[u8], device_id: &str) -> Result<Self, DocError> {
        let version = serde_json::from_slice::<Value>(bytes)?
            .get("v")
            .and_then(Value::as_u64);
        let mut doc = Self::new(device_id);
        match version {
            Some(2) => {
                let state: PersistedState = serde_json::from_slice(bytes)?;
                doc.clock = state.clock;
                doc.oseq = state.oseq;
                doc.vv = state.vv;
                for row in state.rows {
                    doc.put(row);
                }
            }
            Some(1) => {
                let state: EdgeState = serde_json::from_slice(bytes)?;
                doc.clock = state.clock;
                doc.oseq = 1;
                for mut row in state.rows {
                    row.vv = Vv::from([(doc.device_id.clone(), 1)]);
                    row.settle();
                    doc.put(row);
                }
                doc.vv.insert(doc.device_id.clone(), 1);
                let ops = state.pending.into_iter().flat_map(|b| b.ops).collect();
                doc.apply_local(ops);
            }
            other => {
                return Err(DocError::Schema(format!(
                    "unknown registry snapshot version {other:?}"
                )));
            }
        }
        Ok(doc)
    }

    // ── replication ─────────────────────────────────────────────────────────

    /// Every row holding a write `vv` lacks: what a peer at `vv` needs.
    #[must_use]
    pub fn rows_since(&self, vv: &Vv) -> Vec<RegistryRow> {
        self.rows
            .values()
            .flat_map(HashMap::values)
            .filter(|row| {
                row.vv
                    .iter()
                    .any(|(origin, &oseq)| vv.get(origin).is_none_or(|&held| oseq > held))
            })
            .cloned()
            .collect()
    }

    /// Merges rows from a peer, then raises this replica's vv to `vv` (sent with the last frame
    /// of a catch-up, and with every live frame); whether anything changed.
    pub fn merge_remote(&mut self, rows: &[RegistryRow], vv: Option<&Vv>) -> bool {
        let mut changed = false;
        for incoming in rows {
            let local = self
                .rows
                .get(&incoming.kind)
                .and_then(|m| m.get(&incoming.id));
            if let Some(row) = merge_row(local, incoming) {
                // Only what was admitted moves the clock: a refused forgery must not.
                for clock in row.clocks.values().chain(row.del_hlc.as_ref()) {
                    self.clock.observe(clock);
                }
                if let Some(&own) = row.vv.get(&self.device_id) {
                    self.oseq = self.oseq.max(own);
                }
                self.put(row);
                changed = true;
            }
        }
        for (origin, &oseq) in vv.into_iter().flatten() {
            let held = self.vv.entry(origin.clone()).or_default();
            if oseq > *held {
                *held = oseq;
            }
        }
        // Peers may hold more of our own writes than this replica remembers (a restored
        // snapshot): never reuse an oseq they have seen.
        if let Some(&own) = self.vv.get(&self.device_id) {
            self.oseq = self.oseq.max(own);
        }
        if changed {
            self.generation += 1;
        }
        changed
    }

    fn put(&mut self, row: RegistryRow) {
        self.rows
            .entry(row.kind.clone())
            .or_default()
            .insert(row.id.clone(), row);
    }

    // ── local writes ────────────────────────────────────────────────────────

    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    fn next_hlc(&mut self) -> String {
        let device = self.device_id.clone();
        self.clock.next(Self::now_ms(), &device)
    }

    /// Applies local writes, each stamped with the next `oseq`.
    fn apply_local(&mut self, ops: Vec<RowOp>) {
        let mut changed = false;
        for op in ops {
            changed |= self.apply_local_op(&op);
        }
        if changed {
            self.generation += 1;
        }
    }

    fn apply_local_op(&mut self, op: &RowOp) -> bool {
        let local = self.rows.get(&op.kind).and_then(|m| m.get(&op.id));
        let mut delta = RegistryRow::empty(&op.kind, &op.id);
        match op.op {
            OpKind::Delete => delta.del_hlc = Some(op.hlc.clone()),
            OpKind::Update if local.is_none_or(|r| r.deleted) => return false,
            OpKind::Update | OpKind::Upsert => {
                for (key, value) in op.set.iter().flatten() {
                    if !value.is_null() {
                        delta.fields.insert(key.clone(), value.clone());
                    }
                    delta
                        .clocks
                        .insert(key.clone(), op.clock_for(key).to_string());
                }
            }
        }
        // Owner-only fields and placement of a row this device may not write are dropped here,
        // so a refused write spends no oseq.
        let live = local.filter(|r| !r.deleted);
        let owner = live
            .and_then(RegistryRow::owner)
            .or_else(|| delta.owner())
            .map(str::to_owned);
        let creator = live
            .and_then(RegistryRow::creator)
            .or_else(|| delta.creator())
            .map(str::to_owned);
        if owner.as_deref().is_some_and(|o| o != self.device_id) {
            let kind = op.kind.as_str();
            let refused: Vec<String> = delta
                .clocks
                .keys()
                .filter(|key| {
                    !admits(
                        kind,
                        key,
                        &self.device_id,
                        owner.as_deref(),
                        creator.as_deref(),
                    )
                })
                .cloned()
                .collect();
            delta.remove_fields(refused);
            if delta.clocks.is_empty() && delta.del_hlc.is_none() {
                return false;
            }
        }
        let oseq = self.oseq + 1;
        delta.vv.insert(self.device_id.clone(), oseq);
        let Some(row) = merge_row(local, &delta) else {
            return false;
        };
        self.oseq = oseq;
        self.vv.insert(self.device_id.clone(), oseq);
        self.put(row);
        true
    }

    fn write(&mut self, kind: &str, id: &str, op: OpKind, set: BTreeMap<String, Value>) {
        let hlc = self.next_hlc();
        self.apply_local(vec![RowOp {
            kind: kind.to_string(),
            id: id.to_string(),
            op,
            set: Some(set),
            hlc,
            clocks: None,
        }]);
    }

    fn delete_row_ops(&mut self, keys: &[(&str, &str)]) {
        let hlc = self.next_hlc();
        let ops = keys
            .iter()
            .map(|(kind, id)| RowOp {
                kind: (*kind).to_string(),
                id: (*id).to_string(),
                op: OpKind::Delete,
                set: None,
                hlc: hlc.clone(),
                clocks: None,
            })
            .collect();
        self.apply_local(ops);
    }

    // ── reads ───────────────────────────────────────────────────────────────

    /// The row, tombstones included.
    fn row_or_tombstone(&self, kind: &str, id: &str) -> Option<RegistryRow> {
        self.rows.get(kind).and_then(|m| m.get(id)).cloned()
    }

    /// The row unless it is missing or deleted.
    fn live_row(&self, kind: &str, id: &str) -> Option<RegistryRow> {
        self.row_or_tombstone(kind, id).filter(|r| !r.deleted)
    }

    /// All live rows of `kind`.
    fn live_rows(&self, kind: &str) -> Vec<RegistryRow> {
        self.rows
            .get(kind)
            .into_iter()
            .flat_map(HashMap::values)
            .filter(|r| !r.deleted)
            .cloned()
            .collect()
    }

    fn read_kind<T: serde::de::DeserializeOwned>(&self, kind: &str) -> Vec<T> {
        let mut out = Vec::new();
        for row in self.live_rows(kind) {
            let value = Value::Object(row.fields.clone().into_iter().collect());
            match serde_json::from_value::<T>(value) {
                Ok(parsed) => out.push(parsed),
                Err(err) => {
                    tracing::warn!(kind, row = %row.id, error = %err, "skipping malformed registry row");
                }
            }
        }
        out
    }

    fn row_exists(&self, kind: &str, id: &str) -> bool {
        self.live_row(kind, id).is_some()
    }

    // ── typed API (the WorkspaceDoc surface) ────────────────────────────────

    /// Upsert a full device row (writer discipline: callers pass their OWN device).
    pub fn upsert_device(&mut self, device: &Device) -> Result<(), DocError> {
        let set = fields([
            ("id", json!(device.id)),
            ("name", json!(device.name)),
            ("platform", json!(device.platform)),
            ("lastSeenAt", opt_ms(device.last_seen_at)),
            ("createdAt", opt_ms(device.created_at)),
            ("version", opt_str(device.version.as_deref())),
            (
                "cursorSdkVersion",
                opt_str(device.cursor_sdk_version.as_deref()),
            ),
            ("cursorSdkEngineVersion", opt_str(device.version.as_deref())),
            ("capabilities", json!(device.capabilities)),
        ]);
        self.write(KIND_DEVICES, &device.id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    /// LWW rename (settings UI; any device may write). `false` when no such row.
    pub fn rename_device(&mut self, device_id: &str, name: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_DEVICES, device_id) {
            return Ok(false);
        }
        self.write(
            KIND_DEVICES,
            device_id,
            OpKind::Update,
            fields([("name", json!(name))]),
        );
        Ok(true)
    }

    /// Stamp `lastSeenAt` on an existing device row (boot/shutdown only —
    /// periodic liveness rides presence frames, never rows).
    pub fn set_device_last_seen(
        &mut self,
        device_id: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_DEVICES, device_id) {
            return Ok(false);
        }
        self.write(
            KIND_DEVICES,
            device_id,
            OpKind::Update,
            fields([("lastSeenAt", json!(at.timestamp_millis()))]),
        );
        Ok(true)
    }

    /// Whether `id`'s device row was deleted — a tombstone, not merely an
    /// unknown id (a Cloud session's machine leaves with its sandbox). A
    /// device that comes back revives its row with a newer write.
    pub fn device_deleted(&self, id: &str) -> bool {
        self.row_or_tombstone(KIND_DEVICES, id)
            .is_some_and(|r| r.deleted)
    }

    pub fn read_devices(&self) -> Result<Vec<Device>, DocError> {
        let mut devices: Vec<Device> = self
            .read_kind::<crate::workspace::RawDevice>(KIND_DEVICES)
            .into_iter()
            .map(Device::from)
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(devices)
    }

    // ── spaces ──────────────────────────────────────────────────────────────

    pub fn upsert_space(&mut self, space: &Space) -> Result<(), DocError> {
        let set = fields([
            ("id", json!(space.id)),
            ("deviceId", json!(space.device_id)),
            ("path", json!(space.path)),
            ("name", opt_str(space.name.as_deref())),
            ("gitDetected", json!(space.git_detected)),
            ("gitCheckedAt", opt_ms(space.git_checked_at)),
            ("checkoutId", opt_str(space.checkout_id.as_deref())),
            ("repositoryId", opt_str(space.repository_id.as_deref())),
            ("githubRepo", opt_str(space.github_repo.as_deref())),
            ("createdAt", json!(space.created_at.timestamp_millis())),
        ]);
        // (Its icon only when it has one: an upsert never clears a picture
        // set meanwhile from another device.)
        let mut set = set;
        if let Some(icon) = &space.icon {
            set.insert("icon".into(), json!(icon));
        }
        self.write(KIND_SPACES, &space.id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    pub fn space(&self, space_id: &str) -> Result<Option<Space>, DocError> {
        Ok(self
            .live_row(KIND_SPACES, space_id)
            .and_then(|row| row_to::<crate::workspace::RawSpace>(&row))
            .map(Space::from))
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, DocError> {
        let mut spaces: Vec<Space> = self
            .read_kind::<crate::workspace::RawSpace>(KIND_SPACES)
            .into_iter()
            .map(Space::from)
            .collect();
        spaces.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(spaces)
    }

    /// LWW display-name set from any device; `None` clears back to the derived
    /// name (basename of path). `false` when no such row.
    pub fn rename_space(&mut self, space_id: &str, name: Option<&str>) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([("name", opt_str(name))]),
        );
        Ok(true)
    }

    /// LWW picture set from any device, whole (emoji, image and hue in one
    /// field); `None` clears back to the default tile. `false` when no row.
    pub fn set_space_icon(
        &mut self,
        space_id: &str,
        icon: Option<&onicode_proto::SpaceIcon>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        let value = icon.map_or(Value::Null, |icon| json!(icon));
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([("icon", value)]),
        );
        Ok(true)
    }

    /// Owner-stamped git presence for the space folder (ownership is asserted
    /// by the engine layer, this is mechanism only).
    pub fn set_space_git(
        &mut self,
        space_id: &str,
        detected: bool,
        checkout_id: Option<&str>,
        repository_id: Option<&str>,
        github_repo: Option<&str>,
        checked_at: DateTime<Utc>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_SPACES, space_id) {
            return Ok(false);
        }
        self.write(
            KIND_SPACES,
            space_id,
            OpKind::Update,
            fields([
                ("gitDetected", json!(detected)),
                ("checkoutId", opt_str(checkout_id)),
                ("repositoryId", opt_str(repository_id)),
                ("githubRepo", opt_str(github_repo)),
                ("gitCheckedAt", json!(checked_at.timestamp_millis())),
            ]),
        );
        Ok(true)
    }

    /// Hard-delete a space and cascade to its chats: one batch tombstones the
    /// space row and every chat/session row whose `spaceId` matches — the
    /// server applies the batch atomically. Returns the removed chat ids so
    /// the engine can drop local state.
    pub fn delete_space(&mut self, space_id: &str) -> Result<DeletedSpace, DocError> {
        let existed = self.row_exists(KIND_SPACES, space_id);
        let chat_ids: Vec<String> = self
            .read_chats()?
            .into_iter()
            .filter(|c| c.space_id.as_deref() == Some(space_id))
            .map(|c| c.id)
            .collect();
        let mut keys: Vec<(&str, &str)> = Vec::with_capacity(chat_ids.len() * 2 + 1);
        for chat_id in &chat_ids {
            keys.push((KIND_CHATS, chat_id));
            keys.push((KIND_SESSIONS, chat_id));
        }
        keys.push((KIND_SPACES, space_id));
        self.delete_row_ops(&keys);
        Ok(DeletedSpace { existed, chat_ids })
    }

    // ── chats ───────────────────────────────────────────────────────────────

    pub fn upsert_chat(&mut self, chat: &Chat) -> Result<(), DocError> {
        let config = match &chat.config {
            Some(config) => serde_json::to_value(config)?,
            None => Value::Null,
        };
        let set = fields([
            ("id", json!(chat.id)),
            ("deviceId", json!(chat.device_id)),
            ("title", opt_str(chat.title.as_deref())),
            ("archived", json!(chat.archived)),
            ("cwd", opt_str(chat.cwd.as_deref())),
            ("branch", opt_str(chat.branch.as_deref())),
            ("checkoutId", opt_str(chat.checkout_id.as_deref())),
            (
                "sourceContext",
                chat.source_context
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            ("config", config),
            (
                "lastMessagePreview",
                opt_str(chat.last_message_preview.as_deref()),
            ),
            ("lastMessageAt", opt_ms(chat.last_message_at)),
            ("createdAt", json!(chat.created_at.timestamp_millis())),
            (
                "harnessSessionId",
                opt_str(chat.harness_session_id.as_deref()),
            ),
            (
                "harnessSessionCwd",
                opt_str(chat.harness_session_cwd.as_deref()),
            ),
            (
                "harnessSessionHarness",
                opt_str(chat.harness_session_harness.as_deref()),
            ),
            ("spaceId", opt_str(chat.space_id.as_deref())),
            ("parentChatId", opt_str(chat.parent_chat_id.as_deref())),
            ("lastSeenAt", opt_ms(chat.last_seen_at)),
            (
                "roomGen",
                chat.room_gen.map(|g| json!(g)).unwrap_or(Value::Null),
            ),
            ("parentChatId", opt_str(chat.parent_chat_id.as_deref())),
            (
                "childKind",
                opt_str(chat.child_kind.map(onicode_proto::ChildKind::as_str)),
            ),
        ]);
        self.write(KIND_CHATS, &chat.id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    /// Claim-on-first-command row: ONLY the fields the claim actually knows
    /// (identity, cwd, space). Absent fields carry no clocked write, so the
    /// real `createChat` upsert — racing behind on the registry channel with
    /// older clocks — still lands its `config`/`title` under per-field LWW
    /// instead of losing them to `Null` deletes.
    pub fn claim_chat(
        &mut self,
        chat_id: &str,
        cwd: Option<&str>,
        space_id: Option<&str>,
        created_at: DateTime<Utc>,
    ) {
        let mut set = fields([
            ("id", json!(chat_id)),
            ("deviceId", json!(self.device_id())),
            ("createdAt", json!(created_at.timestamp_millis())),
        ]);
        if let Some(cwd) = cwd {
            set.insert("cwd".into(), json!(cwd));
        }
        if let Some(space_id) = space_id {
            set.insert("spaceId".into(), json!(space_id));
        }
        self.write(KIND_CHATS, chat_id, OpKind::Upsert, set);
    }

    /// Synced seen marker (LWW) with a monotonic guard: no write when the
    /// stored stamp is already >= `at`.
    pub fn set_chat_seen(&mut self, chat_id: &str, at: DateTime<Utc>) -> Result<bool, DocError> {
        let Some(row) = self.live_row(KIND_CHATS, chat_id) else {
            return Ok(false);
        };
        let current = row.fields.get("lastSeenAt").and_then(Value::as_i64);
        if current.is_some_and(|ms| ms >= at.timestamp_millis()) {
            return Ok(true);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("lastSeenAt", json!(at.timestamp_millis()))]),
        );
        Ok(true)
    }

    pub fn chat(&self, chat_id: &str) -> Result<Option<Chat>, DocError> {
        Ok(self
            .live_row(KIND_CHATS, chat_id)
            .and_then(|row| row_to::<crate::workspace::RawChat>(&row))
            .map(Chat::from))
    }

    /// Whether `chat_id`'s row was deleted — a tombstone, not merely a chat
    /// whose row hasn't arrived yet.
    pub fn chat_deleted(&self, chat_id: &str) -> bool {
        self.row_or_tombstone(KIND_CHATS, chat_id)
            .is_some_and(|r| r.deleted)
    }

    /// Whether `chat_id`'s row has recorded its parent: `createChat` writes
    /// `parentChatId` either way (a null for none), a claimed row
    /// ([`Self::claim_chat`]) doesn't.
    pub fn chat_parent_recorded(&self, chat_id: &str) -> bool {
        self.live_row(KIND_CHATS, chat_id)
            .is_some_and(|row| row.clocks.contains_key("parentChatId"))
    }

    pub fn read_chats(&self) -> Result<Vec<Chat>, DocError> {
        let mut chats: Vec<Chat> = self
            .read_kind::<crate::workspace::RawChat>(KIND_CHATS)
            .into_iter()
            .map(Chat::from)
            .collect();
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(chats)
    }

    pub fn rename_chat(&mut self, chat_id: &str, title: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("title", json!(title))]),
        );
        Ok(true)
    }

    pub fn set_chat_archived(&mut self, chat_id: &str, archived: bool) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("archived", json!(archived))]),
        );
        Ok(true)
    }

    /// Flip the chat's sync room generation (docs/chat2-sync.md M2). The
    /// host calls this in the same breath as seeding the chat2 checkpoint;
    /// LWW per-field like every registry write, so the flip is per-chat and
    /// instantly revertible by writing 1 back.
    pub fn set_chat_room_gen(&mut self, chat_id: &str, room_gen: u32) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("roomGen", json!(room_gen))]),
        );
        Ok(true)
    }

    pub fn set_chat_branch(&mut self, chat_id: &str, branch: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("branch", json!(branch))]),
        );
        Ok(true)
    }

    pub fn set_chat_source_context(
        &mut self,
        chat_id: &str,
        context: &onicode_proto::ConversationSourceContext,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([
                ("sourceContext", serde_json::to_value(context)?),
                ("branch", json!(context.branch)),
                ("checkoutId", json!(context.checkout_id)),
            ]),
        );
        Ok(true)
    }

    /// Retarget the chat onto another folder — the mid-session "switch to an
    /// existing worktree" move. Harness resume is cwd-scoped, so the next run
    /// in the new folder starts a fresh harness conversation by design.
    pub fn set_chat_cwd(&mut self, chat_id: &str, cwd: &str) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("cwd", json!(cwd))]),
        );
        Ok(true)
    }

    pub fn set_chat_checkout(
        &mut self,
        chat_id: &str,
        checkout_id: &str,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("checkoutId", json!(checkout_id))]),
        );
        Ok(true)
    }

    pub fn set_chat_config(
        &mut self,
        chat_id: &str,
        config: &ChatConfig,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        let value = serde_json::to_value(config)?;
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([("config", value)]),
        );
        Ok(true)
    }

    /// Host-side resume continuity. An empty `session_id` is the explicit
    /// "do not resume" tombstone written after a harness rejects a resume.
    pub fn set_chat_harness_session(
        &mut self,
        chat_id: &str,
        session_id: &str,
        cwd: &str,
        harness: Option<&str>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([
                ("harnessSessionId", json!(session_id)),
                ("harnessSessionCwd", json!(cwd)),
                ("harnessSessionHarness", opt_str(harness)),
            ]),
        );
        Ok(true)
    }

    /// Host-side sidebar freshness: preview + timestamp of the latest message.
    pub fn set_chat_last_message(
        &mut self,
        chat_id: &str,
        preview: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, DocError> {
        if !self.row_exists(KIND_CHATS, chat_id) {
            return Ok(false);
        }
        self.write(
            KIND_CHATS,
            chat_id,
            OpKind::Update,
            fields([
                ("lastMessagePreview", json!(preview)),
                ("lastMessageAt", json!(at.timestamp_millis())),
            ]),
        );
        Ok(true)
    }

    /// Tombstone: delete the chat row (and its session-status row). The
    /// per-chat session doc remains — this removes the index entry only.
    /// Moves a chat to `device_id`. A row's owner never changes, so this deletes the chat and
    /// session rows and writes the chat again under the new owner, in one step no reader sees
    /// between. The old host's owner-only fields go with them; the new host writes its own.
    pub fn rehome_chat(&mut self, chat_id: &str, device_id: &str) -> Result<bool, DocError> {
        let Some(mut chat) = self.chat(chat_id)? else {
            return Ok(false);
        };
        if chat.device_id == device_id {
            return Ok(true);
        }
        chat.device_id = device_id.to_owned();
        self.delete_row_ops(&[(KIND_CHATS, chat_id), (KIND_SESSIONS, chat_id)]);
        self.upsert_chat(&chat)?;
        Ok(true)
    }

    /// Deletes a chat's session-status row, so whichever device writes it next owns it.
    pub fn delete_session(&mut self, chat_id: &str) {
        self.delete_row_ops(&[(KIND_SESSIONS, chat_id)]);
    }

    pub fn delete_chat(&mut self, chat_id: &str) -> Result<bool, DocError> {
        let existed = self.row_exists(KIND_CHATS, chat_id);
        self.delete_row_ops(&[(KIND_CHATS, chat_id), (KIND_SESSIONS, chat_id)]);
        Ok(existed)
    }

    // ── sessions ────────────────────────────────────────────────────────────

    /// Upsert a session-status row (writer discipline: each device writes only
    /// its own runs' rows). Staleness is checked client-side via `updatedAt`.
    pub fn upsert_session(&mut self, session: &Session) -> Result<(), DocError> {
        let set = fields([
            ("chatId", json!(session.chat_id)),
            ("deviceId", json!(session.device_id)),
            ("status", serde_json::to_value(session.status)?),
            ("lastCompletedTurn", json!(session.last_completed_turn)),
            ("lastTurnOutcome", json!(session.last_turn_outcome)),
            (
                "terminalActivity",
                serde_json::to_value(&session.terminal_activity)?,
            ),
            ("startedAt", opt_ms(session.started_at)),
            ("updatedAt", json!(session.updated_at.timestamp_millis())),
            ("runningSubagents", json!(session.running_subagents)),
            (
                "backgroundTasks",
                serde_json::to_value(&session.background_tasks)?,
            ),
        ]);
        self.write(KIND_SESSIONS, &session.chat_id.clone(), OpKind::Upsert, set);
        Ok(())
    }

    pub fn read_sessions(&self) -> Result<Vec<Session>, DocError> {
        let mut sessions: Vec<Session> = self
            .read_kind::<crate::workspace::RawSession>(KIND_SESSIONS)
            .into_iter()
            .map(Session::from)
            .collect();
        sessions.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        Ok(sessions)
    }

    // ── whole-doc read ──────────────────────────────────────────────────────

    pub fn sidebar_preferences(&self) -> Option<SidebarPreferences> {
        self.sidebar_pins_initialized().then(|| SidebarPreferences {
            sections: self.sidebar_sections(),
            pinned_session_ids: self
                .ordered_sidebar_pins()
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
        })
    }

    /// Initialize readiness and clean deleted pins from one authoritative snapshot.
    pub fn reconcile_sidebar_pins(&mut self, authoritative: bool) -> Result<bool, DocError> {
        if !authoritative {
            return Ok(false);
        }
        let initialized = self.sidebar_pins_initialized();
        self.initialize_sidebar_pins();
        let known: std::collections::HashSet<_> =
            self.read_chats()?.into_iter().map(|c| c.id).collect();
        let removed: Vec<_> = self
            .ordered_sidebar_pins()
            .into_iter()
            .filter(|(id, _)| !known.contains(id))
            .collect();
        for (id, _) in &removed {
            self.change_sidebar_pin(&onicode_proto::SidebarPinChange::Unpin {
                session_id: id.clone(),
            })?;
        }
        Ok(!initialized || !removed.is_empty())
    }

    pub fn read_all(&self) -> Result<WorkspaceState, DocError> {
        Ok(WorkspaceState {
            devices: self.read_devices()?,
            spaces: self.read_spaces()?,
            chats: self.read_chats()?,
            sessions: self.read_sessions()?,
        })
    }
}

// ── field helpers ───────────────────────────────────────────────────────────

fn fields<const N: usize>(entries: [(&str, Value); N]) -> BTreeMap<String, Value> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

fn opt_str(value: Option<&str>) -> Value {
    match value {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

fn opt_ms(value: Option<DateTime<Utc>>) -> Value {
    match value {
        Some(at) => json!(at.timestamp_millis()),
        None => Value::Null,
    }
}

fn row_to<T: serde::de::DeserializeOwned>(row: &RegistryRow) -> Option<T> {
    let value = Value::Object(row.fields.clone().into_iter().collect());
    match serde_json::from_value::<T>(value) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(kind = %row.kind, row = %row.id, error = %err, "skipping malformed registry row");
            None
        }
    }
}

// SessionStatus needs to serialize to the same strings the loro doc used
// ("idle"/"working"/…) — onicode_proto's serde derives already use camelCase;
// the compile-time check lives in the tests below.

#[cfg(test)]
mod tests;
