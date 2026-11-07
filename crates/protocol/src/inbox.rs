//! The inbox's data: the GitHub issues and pull requests `gh` lists, how a viewer filters them
//! and what it has seen. Fetching them is the engine's.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::{Datelike, Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::session_groups::WorkItem;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxLabel")]
pub struct Label {
    pub name: String,
    pub color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxPerson")]
pub struct Person {
    pub login: String,
    pub avatar_url: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
// Named apart from the protocol's `Item` so its schema traces on its own; JSON never shows the
// name.
#[serde(default, rename = "InboxItem", rename_all = "camelCase")]
pub struct Item {
    pub kind: String,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub state_reason: String,
    pub created_at: String,
    pub updated_at: String,
    pub labels: Vec<Label>,
    pub assignees: Vec<Person>,
    pub draft: bool,
    pub repo: String,
    pub project_path: String,
}
impl Item {
    pub fn key(&self) -> String {
        if !self.repo.trim().is_empty() {
            format!(
                "{}:{}:{}",
                self.repo.trim().to_lowercase(),
                self.kind,
                self.number
            )
        } else if !self.url.trim().is_empty() {
            self.url.trim().to_lowercase()
        } else {
            format!("{}:{}", self.kind, self.number)
        }
    }

    pub fn reference(&self) -> String {
        format!("#{}", self.number)
    }

    pub fn updated_ms(&self) -> i64 {
        timestamp_ms(&self.updated_at)
    }

    pub fn status(&self) -> &'static str {
        if self.state.eq_ignore_ascii_case("merged") {
            "Merged"
        } else if self.state.eq_ignore_ascii_case("closed") {
            "Closed"
        } else if self.draft {
            "Draft"
        } else {
            "Open"
        }
    }

    pub fn work_item(&self) -> WorkItem {
        WorkItem {
            kind: self.kind.clone(),
            repo: self.repo.clone(),
            number: self.number.max(0) as u64,
            url: self.url.clone(),
        }
    }

    pub fn prompt(&self, body: &str, ask: bool) -> String {
        format!(
            "{}\n\nGitHub {}: {}\n{}\n\n{}",
            if ask {
                "Help me understand and discuss this work item."
            } else {
                "Work on this item. Read its description and discussion, inspect the project, and implement the required changes."
            },
            self.reference(),
            self.title,
            self.url,
            body
        )
    }
}
pub fn timestamp_ms(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|at| at.timestamp_millis())
        .unwrap_or(0)
}
pub fn relative_time(value: &str) -> String {
    let at = timestamp_ms(value);
    if at == 0 {
        return String::new();
    }
    let seconds = ((crate::time::now_ms() as i64 - at).max(0)) / 1000;
    match seconds {
        0..60 => "now".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        86400..604800 => format!("{}d", seconds / 86400),
        _ => chrono::DateTime::from_timestamp_millis(at)
            .map(|d| d.format("%b %-d").to_string())
            .unwrap_or_default(),
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxDetails")]
pub struct Details {
    pub body: String,
    pub author: String,
    pub author_avatar_url: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub review_decision: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxComment")]
pub struct Comment {
    pub id: String,
    pub kind: String,
    pub author: String,
    pub author_avatar_url: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
    pub state: String,
    pub path: String,
    pub line: Option<i64>,
    pub resolved: bool,
    pub thread_id: String,
    pub replies: Vec<Comment>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxCommit")]
pub struct Commit {
    pub oid: String,
    pub message_headline: String,
    pub author: String,
    pub committed_date: String,
    pub url: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxThread")]
pub struct Thread {
    pub comments: Vec<Comment>,
    pub commits: Vec<Commit>,
    pub truncated: bool,
    pub review_decision: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxDiffFile")]
pub struct DiffFile {
    pub path: String,
    pub additions: i64,
    pub deletions: i64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxDiff")]
pub struct Diff {
    pub additions: i64,
    pub deletions: i64,
    pub files: Vec<DiffFile>,
    pub patch: String,
    pub truncated: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxCheck")]
pub struct Check {
    pub name: String,
    pub workflow: String,
    pub state: String,
    pub url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxChecks")]
pub struct Checks {
    pub head_oid: String,
    pub checks: Vec<Check>,
}
/// GitHub's connection: whether `gh` is installed and signed in to github.com. Named
/// `InboxStatus` on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename = "InboxStatus", rename_all = "camelCase")]
pub struct Status {
    pub connected: bool,
    pub installed: bool,
    pub authenticated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", rename = "InboxStatusFilter")]
pub struct StatusFilter {
    pub open: bool,
    pub draft: bool,
    pub closed: bool,
    pub merged: bool,
}
/// Named `InboxFilters` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename = "InboxFilters", rename_all = "camelCase")]
pub struct Filters {
    pub assigned_to_me: bool,
    pub hidden_projects: Vec<String>,
    pub hidden_kinds: Vec<String>,
    pub time: String,
    pub status: StatusFilter,
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            assigned_to_me: false,
            hidden_projects: vec![],
            hidden_kinds: vec![],
            time: "all".into(),
            status: StatusFilter::default(),
        }
    }
}
impl Filters {
    pub fn fetch_state(&self) -> &str {
        if self.status.closed || self.status.merged || !(self.status.open || self.status.draft) {
            "all"
        } else {
            "open"
        }
    }

    pub fn matches(&self, item: &Item, search: &str) -> bool {
        if self.hidden_projects.contains(&item.project_path)
            || self.hidden_kinds.contains(&item.kind)
        {
            return false;
        }
        let s = &self.status;
        if (s.open || s.closed || s.draft || s.merged)
            && !match item.status() {
                "Open" => s.open,
                "Draft" => s.draft,
                "Closed" => s.closed,
                "Merged" => s.merged,
                _ => true,
            }
        {
            return false;
        }
        let now = Local::now();
        let start = match self.time.as_str() {
            "today" => Local
                .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
                .single()
                .map(|d| d.timestamp_millis())
                .unwrap_or(0),
            "7d" => now.timestamp_millis() - 7 * 86400000,
            "30d" => now.timestamp_millis() - 30 * 86400000,
            _ => 0,
        };
        if start > 0 && item.updated_ms() < start {
            return false;
        }
        let text = format!(
            "{} {} {} {}",
            item.title,
            item.reference(),
            item.repo,
            item.labels
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_lowercase();
        search
            .split_whitespace()
            .all(|word| text.contains(&word.to_lowercase()))
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename = "InboxSeen")]
pub struct Seen {
    pub seeded: bool,
    pub items: BTreeMap<String, i64>,
}
impl Seen {
    pub fn unseen(&self, item: &Item) -> bool {
        self.seeded
            && self
                .items
                .get(&item.key())
                .is_none_or(|at| item.updated_ms() > *at)
    }

    pub fn mark(&mut self, item: &Item) {
        let old = self.items.entry(item.key()).or_default();
        *old = (*old).max(item.updated_ms());
    }

    pub fn seed(&mut self, items: &[Item]) {
        if !self.seeded && !items.is_empty() {
            for item in items {
                self.mark(item);
            }
            self.seeded = true;
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename = "InboxViewState")]
pub struct ViewState {
    pub filters: Filters,
    pub seen: Seen,
    pub list_width: f32,
}
impl Default for ViewState {
    fn default() -> Self {
        Self {
            filters: Filters::default(),
            seen: Seen::default(),
            list_width: 280.,
        }
    }
}
impl ViewState {
    pub fn fetch_scope(&self, projects: &[PathBuf]) -> String {
        let mut paths = projects
            .iter()
            .map(|p| crate::files::path_key(&p.to_string_lossy()))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        format!(
            "{}:{}:{}",
            self.filters.assigned_to_me,
            self.filters.fetch_state(),
            paths.join("|")
        )
    }
}
/// Named `InboxSnapshot` on the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "InboxSnapshot")]
pub struct Snapshot {
    pub items: Vec<Item>,
    /// Why the fetch failed, if it did.
    pub error: Option<String>,
    pub connection: Status,
}

/// The Tauri InboxNotificationTracker: the baseline and revisions are separate from the user's
/// read state. Startup, failed fetches and query changes never trigger automations for an
/// existing backlog.
#[derive(Clone, Debug, Default)]
pub struct NotificationTracker {
    revisions: HashMap<String, i64>,
    primed: bool,
    scope: Option<String>,
}
impl NotificationTracker {
    pub fn observe(&mut self, snapshot: &Snapshot, scope: String) -> Vec<Item> {
        if self.scope.as_ref() != Some(&scope) {
            self.primed = false;
        }
        self.scope = Some(scope);
        if snapshot.error.is_some() {
            return vec![];
        }
        let mut appeared = vec![];
        for item in &snapshot.items {
            let at = item.updated_ms();
            if at == 0 {
                continue;
            }
            let project = if item.project_path.is_empty() {
                item.repo.to_lowercase()
            } else {
                crate::files::path_key(&item.project_path)
            };
            let key = format!("{project}:{}:{}", item.kind, item.number);
            let previous = self.revisions.get(&key).copied();
            if self.primed && previous.is_none() {
                appeared.push(item.clone());
            }
            self.revisions.insert(key, previous.unwrap_or(0).max(at));
        }
        self.primed = true;
        appeared
    }
}
