//! What a Settings page reads that no other group serves (`ReadWorktreeMarks`), read in the Git
//! group's foreground lane: a person waits on the page.
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use onicode_protocol::Call;
use onicode_protocol::settings_pages::{ReadWorktreeMarks, WorktreeMarks};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use crate::git::{CheckoutHub, Lane};
use crate::service::{Group, reply, routed};

/// Every method this service serves.
const METHODS: [&str; 1] = [ReadWorktreeMarks::NAME];

pub struct SettingsPagesService {
    git: Arc<CheckoutHub>,
}

impl SettingsPagesService {
    #[must_use]
    pub fn new(git: Arc<CheckoutHub>) -> Self {
        Self { git }
    }

    async fn worktree_marks(&self, body: ReadWorktreeMarks) -> Result<Vec<WorktreeMarks>, String> {
        let ReadWorktreeMarks { checkout, trees } = body;
        self.git
            .run(Lane::Foreground, move |_| {
                Ok(trees
                    .into_iter()
                    .map(|tree| {
                        let (locked, dirty, unpushed) =
                            onicode_core::worktree_management::status(Path::new(&checkout), &tree);
                        WorktreeMarks {
                            path: tree.path,
                            locked,
                            dirty,
                            unpushed,
                        }
                    })
                    .collect())
            })
            .await
    }
}

impl Group for SettingsPagesService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for SettingsPagesService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            ReadWorktreeMarks::NAME => {
                let (_, body) = routed(params)?;
                reply(self.worktree_marks(body).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
