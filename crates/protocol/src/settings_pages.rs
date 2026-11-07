//! What a Settings page reads that no other group serves: the marks the Worktrees page shows on
//! each linked worktree. The page's branches and worktrees come from the Git group's
//! [`Refs`](crate::git::Refs).
use serde::{Deserialize, Serialize};

use crate::Call;
use crate::git::Worktree;

/// One linked worktree's marks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeMarks {
    pub path: String,
    /// `git worktree lock` holds it.
    pub locked: bool,
    /// Whether it has uncommitted changes; `None` when Git could not say.
    pub dirty: Option<bool>,
    /// Commits its branch has that its upstream lacks.
    pub unpushed: usize,
}

/// The marks of `trees`, linked worktrees of `checkout`, in their order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadWorktreeMarks {
    pub checkout: String,
    pub trees: Vec<Worktree>,
}

impl Call for ReadWorktreeMarks {
    type Params = Self;
    type Reply = Vec<WorktreeMarks>;

    const NAME: &'static str = "ReadWorktreeMarks";
}
