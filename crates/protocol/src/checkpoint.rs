//! What a turn's checkpoint recorded: the files it changed and whether each can be undone.
use serde::{Deserialize, Serialize};

/// One file a turn changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFile {
    pub path: String,
    pub relative: String,
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
    /// True when an exact recorded before/after diff is available.
    pub exact: bool,
    /// False when restoring could overwrite a change made outside this session.
    pub undoable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointStatus {
    pub files: Vec<CheckpointFile>,
}
