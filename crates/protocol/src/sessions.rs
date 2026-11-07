//! What the sidebar shows of a session: its stored summary, folders, reminders, linked work
//! item and orchestration, and the model background chores such as titles run on.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::agent::AccessMode;
pub use crate::orchestration::{Summary as Orchestration, SummaryTask};
pub use crate::session_groups::{Folder, Groups, WorkItem};
pub use crate::session_title::TextModel;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub title: String,
    pub provider: String,
    pub cwd: String,
    pub provider_id: Option<String>,
    pub has_messages: bool,
    /// Unix milliseconds; zero means an older session has no recorded time.
    pub updated_at: u64,
    pub archived: bool,
    pub pinned: bool,
    pub work_item: Option<WorkItem>,
}

/// A session's pending or delivered reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub session_id: i64,
    /// Unix milliseconds.
    pub due_at: u64,
    pub fired_at: Option<u64>,
}

/// What the sidebar shows beside each session, gathered in one read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSidebarMetadata {
    pub worktrees: HashMap<i64, Option<String>>,
    pub details: HashMap<i64, MobileSessionMetadata>,
    pub drafts: HashSet<i64>,
    pub sidebar_hidden: HashSet<i64>,
    pub started_by: HashMap<i64, i64>,
    pub orchestrations: HashMap<i64, Orchestration>,
    pub groups: HashMap<String, Groups>,
    pub reminders: Vec<Reminder>,
}

/// Scalar fields sent to paired phones, including sessions with no live view.
/// Model IDs use the native representation, as agent_settings does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobileSessionMetadata {
    pub model: Option<String>,
    pub branch: Option<String>,
    pub worktree_cwd: Option<String>,
    pub worktree_removed: bool,
    pub access: AccessMode,
}

/// A session's hover card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPreviewData {
    pub model: Option<String>,
    pub worktree_cwd: Option<String>,
}

/// A stored model id as its provider's CLI names it; `None` for the provider's default.
pub fn native_model(model: &str, provider: &str) -> Option<String> {
    let model = model.trim();
    let model = model.strip_prefix(&format!("{provider}:")).unwrap_or(model);
    let model = model.split('[').next().unwrap_or(model);
    if model.is_empty() || model == "default" {
        return None;
    }
    let model = if provider == "claude"
        && !model.starts_with("claude-")
        && model.chars().any(|c| c.is_ascii_digit())
    {
        format!("claude-{}", model.replace('.', "-"))
    } else {
        model.into()
    };
    Some(model)
}
