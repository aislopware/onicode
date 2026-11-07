//! Bounded hover-card metadata. Never read transcripts or discover agent state.
pub use onicode_protocol::sessions::SessionPreviewData;
use rusqlite::OptionalExtension;

use super::Store;

impl Store {
    pub fn session_preview(&self, id: i64) -> Result<SessionPreviewData, String> {
        // Old desktop schemas may not have worktree metadata. Read just
        // these two scalar columns, never blocks_json/model_settings.
        let cwd = self.shared_field("worktree_cwd", "NULL");
        let result = self
            .conn
            .query_row(
                &format!(
                    "SELECT substr(model,1,512),substr({cwd},1,4096) FROM sessions WHERE id=?1"
                ),
                [self.persisted_session_id(id)?],
                |row| {
                    Ok(SessionPreviewData {
                        model: row.get(0)?,
                        worktree_cwd: row.get(1)?,
                    })
                },
            )
            .optional();
        result
            .map(|value| value.unwrap_or_default())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_preview_never_decodes_blocks_or_model_controls() {
        let store = Store::in_memory().unwrap();
        let session = store.create("codex", "/project").unwrap();
        let original = store.persisted_session_id(session.id).unwrap();
        store.conn.execute("UPDATE sessions SET model='codex:gpt-6-sol',model_settings='broken JSON',blocks_json='broken JSON',worktree_cwd='/linked' WHERE id=?1", [original]).unwrap();
        let preview = store.session_preview(session.id).unwrap();
        assert_eq!(preview.model.as_deref(), Some("codex:gpt-6-sol"));
        assert_eq!(preview.worktree_cwd.as_deref(), Some("/linked"));
    }
}
