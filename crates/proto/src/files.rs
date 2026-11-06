//! Paths on any peer and transfers between them (`docs/onicode/decisions/one-machine.md`).
//!
//! The payloads of `ListPath`, `PathRoots`, `PathMutate`, `StartTransfer`, `ServeTransfer`,
//! `WatchTransfers`, `ControlTransfer` and `ResolveConflict`. Every request may also carry
//! `targetDeviceId`, which the router reads and these types ignore.

use serde::{Deserialize, Serialize};

/// `ListPath`: one directory on the serving engine. `~` expands to home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPathRequest {
    /// An absolute path, or one starting with `~`.
    pub path: String,
    /// List names starting with a dot too.
    #[serde(default)]
    pub show_hidden: bool,
}

/// What a listed entry is. A package is a directory Finder shows as one file (`.app`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link, not followed.
    Symlink,
    /// A bundle or package directory.
    Package,
}

/// One entry of a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathEntry {
    /// The file name.
    pub name: String,
    /// What it is.
    pub kind: PathKind,
    /// Bytes; 0 for a directory.
    pub size: u64,
    /// Last modification, epoch milliseconds.
    pub modified_ms: i64,
    /// Unix permission bits.
    pub mode: u32,
    /// A symlink's target as stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `ListPath`'s reply: directories first, then the rest, each by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathListing {
    /// The listed directory, absolute, `~` expanded.
    pub path: String,
    /// Its entries.
    pub entries: Vec<PathEntry>,
    /// Its parent; `None` at the root.
    #[serde(default)]
    pub parent: Option<String>,
}

/// A mounted volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathVolume {
    /// Its display name.
    pub name: String,
    /// Its mount point.
    pub path: String,
}

/// `PathRoots`' reply: where browsing starts on a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathRoots {
    /// The home directory.
    pub home: String,
    /// Mounted volumes.
    pub volumes: Vec<PathVolume>,
    /// Recent workspace and chat folders.
    pub recent: Vec<String>,
}

/// What `PathMutate` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathOp {
    /// Make the directory `path`.
    Mkdir,
    /// Rename `path` to `to`: a bare name stays in the same folder.
    Rename,
    /// Move `path` to the Trash.
    Trash,
}

/// `PathMutate`: one change to the serving engine's filesystem. Replies `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMutateRequest {
    /// The change.
    pub op: PathOp,
    /// The path it acts on.
    pub path: String,
    /// The new name or path for [`PathOp::Rename`].
    #[serde(default)]
    pub to: Option<String>,
}

/// Where a transfer reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferFrom {
    /// The source device (a `WatchDevices` id); the local engine's own id means here.
    pub device_id: String,
    /// Absolute paths, each copied with everything under it.
    pub paths: Vec<String>,
}

/// Where a transfer writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferTo {
    /// The destination device, which owns the job.
    pub device_id: String,
    /// The folder the items land in.
    pub dir: String,
}

/// What a transfer does when an item's name is taken at the destination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictPolicy {
    /// Stop and ask ([`TransferState::Conflict`]), as Finder does.
    #[default]
    Ask,
    /// Replace what is there.
    Replace,
    /// Keep both, naming the new one as Finder does (`name 2.ext`).
    KeepBoth,
    /// Leave what is there and skip the item.
    Skip,
}

/// `StartTransfer`. Any engine takes it and forwards it to the destination engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTransferRequest {
    /// The source.
    pub from: TransferFrom,
    /// The destination.
    pub to: TransferTo,
    /// What a taken name does.
    #[serde(default)]
    pub conflict: ConflictPolicy,
}

/// `StartTransfer`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTransferReply {
    /// The new job's id.
    pub transfer_id: String,
}

/// `ServeTransfer`, sent by the destination to the source before it pulls: the source serves
/// `xfer/<transferId>/...` streams for these paths only. Replies `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServeTransferRequest {
    /// The job's id.
    pub transfer_id: String,
    /// The absolute paths the source may read for it.
    pub paths: Vec<String>,
    /// Bytes between digests on a range (`onicode_transfer::BLOCK` when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
}

/// Where a job stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferState {
    /// Waiting to start.
    Queued,
    /// Moving bytes, or waiting for the source to come back.
    Running,
    /// Held by the owner.
    Paused,
    /// Waiting for [`ResolveConflictRequest`].
    Conflict,
    /// Every item is in place.
    Done,
    /// Stopped on an error it cannot get past.
    Failed,
    /// Stopped by the owner; nothing partial is left.
    Cancelled,
}

impl TransferState {
    /// Whether the job has ended for good.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// The name a job stopped on, and both sides of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferConflict {
    /// The destination path that is taken.
    pub path: String,
    /// What is there: bytes (0 for a folder).
    pub existing_size: u64,
    /// What is there: modified, epoch milliseconds.
    pub existing_modified_ms: i64,
    /// What would replace it: bytes, the whole tree for a folder.
    pub incoming_size: u64,
    /// What would replace it: modified, epoch milliseconds.
    pub incoming_modified_ms: i64,
}

/// One job as `WatchTransfers` reports it. The first item of the stream is a list of these,
/// every later item one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferUpdate {
    /// The job.
    pub transfer_id: String,
    /// Its source.
    pub from: TransferFrom,
    /// Its destination.
    pub to: TransferTo,
    /// Where it stands.
    pub state: TransferState,
    /// Files, folders and links in it.
    pub items: u32,
    /// Those in place.
    pub items_done: u32,
    /// File bytes in it.
    pub bytes: u64,
    /// Those written and checked.
    pub bytes_done: u64,
    /// The recent rate.
    pub bytes_per_sec: u64,
    /// The item last started, relative to the destination folder.
    #[serde(default)]
    pub current: Option<String>,
    /// What it waits on in [`TransferState::Conflict`].
    #[serde(default)]
    pub conflict: Option<TransferConflict>,
    /// Why it failed, or the last error it is retrying past.
    #[serde(default)]
    pub error: Option<String>,
}

/// What `ControlTransfer` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferAction {
    /// Hold it; nothing is lost.
    Pause,
    /// Go on from where it stopped.
    Resume,
    /// Stop and remove what is partial.
    Cancel,
}

/// `ControlTransfer`, on the destination engine. Replies `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTransferRequest {
    /// The job.
    pub transfer_id: String,
    /// What to do.
    pub action: TransferAction,
}

/// The answer to a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictChoice {
    /// Replace what is there.
    Replace,
    /// Keep both.
    KeepBoth,
    /// Skip the item.
    Skip,
}

impl From<ConflictChoice> for ConflictPolicy {
    fn from(choice: ConflictChoice) -> Self {
        match choice {
            ConflictChoice::Replace => Self::Replace,
            ConflictChoice::KeepBoth => Self::KeepBoth,
            ConflictChoice::Skip => Self::Skip,
        }
    }
}

/// `ResolveConflict`, on the destination engine. Replies `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveConflictRequest {
    /// The job.
    pub transfer_id: String,
    /// The answer.
    pub choice: ConflictChoice,
    /// Answer every later conflict of the job the same way.
    #[serde(default)]
    pub apply_to_all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_spells_its_fields_as_written() {
        let request: StartTransferRequest = serde_json::from_value(serde_json::json!({
            "from": { "deviceId": "a", "paths": ["/x"] },
            "to": { "deviceId": "b", "dir": "/y" },
            "conflict": "keepBoth",
            "targetDeviceId": "b",
        }))
        .unwrap();
        assert_eq!(request.conflict, ConflictPolicy::KeepBoth);
        let entry = serde_json::to_value(PathEntry {
            name: "A.app".into(),
            kind: PathKind::Package,
            size: 0,
            modified_ms: 1,
            mode: 0o755,
            target: None,
        })
        .unwrap();
        assert_eq!(entry["kind"], "package");
        assert_eq!(entry["modifiedMs"], 1);
        let resolve: ResolveConflictRequest = serde_json::from_value(serde_json::json!({
            "transferId": "t", "choice": "skip", "applyToAll": true
        }))
        .unwrap();
        assert!(resolve.apply_to_all);
        assert_eq!(
            serde_json::to_value(TransferState::Cancelled).unwrap(),
            "cancelled"
        );
    }
}
