//! Source control's data: a checkout's changes, history, pull request and review sides, as the
//! engine reads them and a viewer shows them.
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    pub files: i64,
    pub additions: i64,
    pub deletions: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    pub relative: String,
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffIndex {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub files: Vec<ChangedFile>,
    pub additions: i64,
    pub deletions: i64,
    pub remote: Option<String>,
    pub upstream: Option<String>,
    pub default_branch: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub ahead_of_default: i64,
    pub head_pushed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRef {
    pub name: String,
    /// `"local"`, `"remote"`, or `"tag"`.
    pub kind: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCommit {
    pub sha: String,
    pub short_sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub timestamp: i64,
    pub subject: String,
    pub refs: Vec<HistoryRef>,
    pub head: bool,
}

/// Named `GitHistory` on the wire, beside the protocol's `History` call.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename = "GitHistory", rename_all = "camelCase")]
pub struct History {
    pub head: Option<String>,
    pub commits: Vec<HistoryCommit>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StagedContext {
    pub branch: Option<String>,
    pub summary: String,
    pub patch: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RangeContext {
    pub base: String,
    pub head: String,
    pub commit_summary: String,
    pub diff_summary: String,
    pub diff_patch: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
}

/// fs.rs `MAX_TEXT_FILE_BYTES`: larger sides show as too large to diff.
pub const MAX_TEXT_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Named `DiffSides` on the wire, beside the protocol's `FileDiff` call.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename = "DiffSides", rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub relative: String,
    pub status: String,
    pub original: String,
    pub current: String,
    pub binary: bool,
    pub too_large: bool,
}

/// The inputs to one diff, without reading its contents. Index object IDs
/// invalidate only the affected paths; disk metadata also catches edits that
/// leave Git's status and line counts unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiffVersion {
    pub index: String,
    pub head: Option<String>,
    pub disk: Option<DiskVersion>,
}

/// A file's disk metadata, as a diff's version compares it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskVersion {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    #[cfg(unix)]
    pub identity: (u64, u64, i64, i64),
}
