//! Name a new checkout without delaying or joining the user's provider turn.
use onicode_core::git::naming;
use onicode_protocol::workspace::NameWorktree;

use super::*;

impl Conversation {
    /// Asks the engine to rename a new checkout's temporary branch after the first prompt.
    pub(super) fn generate_worktree_branch(&mut self, message: &str, cx: &mut Context<Self>) {
        if message.trim().is_empty() || self.naming_worktree {
            return;
        }
        let Some(cwd) = self.settings.worktree_cwd.clone() else {
            return;
        };
        if naming::temporary_branch(Path::new(&cwd), self.summary.id).is_none() {
            return;
        }
        self.naming_worktree = true;
        let body = NameWorktree {
            session: self.summary.id,
            root: self.summary.cwd.clone(),
            cwd: cwd.clone(),
            message: message.to_owned(),
            model: self.background_text_model(cx),
        };
        let named =
            crate::engine_client::call::<NameWorktree>(&self.engine, Some(OpId::new()), body, cx);
        cx.spawn(async move |this, cx| {
            let named = named.await;
            let _ = this.update(cx, |this, cx| {
                this.naming_worktree = false;
                if this.execution_cwd() != cwd {
                    return;
                }
                if let Ok(named) = named
                    && let Some(branch) = named.branch
                {
                    this.branch = Some(branch);
                    this.refresh_git(cx);
                    cx.emit(SessionChanged);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
