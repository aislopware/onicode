//! The provider calls behind the workspace's background chores: a session's generated title, the
//! work item its first prompt names, and a new worktree's branch name. A seam, so the hub's tests
//! answer them without a provider, `gh` or the network.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use onicode_core::session_groups::WorkItem;
use onicode_core::session_title::{GeneratedTitle, TextModel, WorkItemHint};

pub trait Chores: Send + Sync + 'static {
    /// A title for the session whose first prompt is `message`.
    fn title(
        &self,
        model: TextModel,
        cwd: &Path,
        message: &str,
        cancel: Arc<AtomicBool>,
    ) -> Option<GeneratedTitle>;

    /// The issue or pull request `message` names, if any.
    fn work_item(&self, message: &str, cwd: &Path, hint: Option<&WorkItemHint>)
    -> Option<WorkItem>;

    /// A branch name for a checkout whose first prompt is `message`.
    fn branch(
        &self,
        model: TextModel,
        cwd: &Path,
        message: &str,
        cancel: Arc<AtomicBool>,
    ) -> Option<String>;
}

/// The real chores: the background model, and `gh` for a work item a hint names.
pub struct ProviderChores;

impl Chores for ProviderChores {
    fn title(
        &self,
        model: TextModel,
        cwd: &Path,
        message: &str,
        cancel: Arc<AtomicBool>,
    ) -> Option<GeneratedTitle> {
        onicode_core::session_title::generate_for(model, cwd, message, cancel)
    }

    fn work_item(
        &self,
        message: &str,
        cwd: &Path,
        hint: Option<&WorkItemHint>,
    ) -> Option<WorkItem> {
        onicode_core::session_groups::resolve_work_item(message, cwd, hint)
    }

    fn branch(
        &self,
        model: TextModel,
        cwd: &Path,
        message: &str,
        cancel: Arc<AtomicBool>,
    ) -> Option<String> {
        onicode_core::git::naming::generate(model, cwd, message, cancel)
    }
}
