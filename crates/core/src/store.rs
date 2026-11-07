//! Session storage: one SQLite layout (monocode's sessions schema, `shared_schema.sql`) with a
//! settings sidecar beside it.
mod managed_sessions;
mod notes;
mod session_metadata;
pub use session_metadata::{MobileSessionMetadata, SessionSidebarMetadata};
mod session_preview;
pub use session_preview::SessionPreviewData;
mod edit;
mod engine_ledger;
mod outbox;
mod rewind;
mod shared;
mod side_threads;
mod thread_files;
use std::path::Path;

pub use edit::EditSnapshot;
pub use onicode_protocol::sessions::{Reminder, Session, native_model};
pub use outbox::OutboxEntry;
use rusqlite::Connection;
pub use side_threads::DesktopSideThread;

use crate::transcript::{Row, Transcript};

pub struct Store {
    conn: Connection,
    path: std::path::PathBuf,
    shared: shared::Shared,
}
impl Store {
    pub fn reset_provider_id_if(&self, id: i64, expected: Option<&str>) -> Result<(), String> {
        self.shared_reset_provider_id_if(id, expected)
    }

    pub fn provider_accounts(&self) -> Result<crate::accounts::Book, String> {
        self.shared_accounts()
    }

    pub fn save_provider_accounts(&self, book: &crate::accounts::Book) -> Result<(), String> {
        self.shared_save_accounts(book)
    }

    /// The selected desktop or isolated-development database.
    pub fn database_path(&self) -> &Path {
        &self.path
    }

    pub fn data_dir(&self) -> Option<&Path> {
        (self.path != Path::new(":memory:"))
            .then(|| self.path.parent())
            .flatten()
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>, String> {
        self.shared_setting(key)
    }

    pub fn save_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.shared_save_setting(key, value)
    }

    /// Forgets the setting under `key`; a key never set is fine.
    pub fn delete_setting(&self, key: &str) -> Result<(), String> {
        self.shared_delete_setting(key)
    }

    pub fn ui_state(&self) -> Result<Option<String>, String> {
        self.shared_setting("workspace")
    }

    pub fn agent_settings(&self, id: i64) -> Result<Option<crate::agent::Settings>, String> {
        self.shared_agent(id)
    }

    /// Read workspace membership without loading every session's full agent settings.
    pub fn session_worktrees(
        &self,
    ) -> Result<std::collections::HashMap<i64, Option<String>>, String> {
        self.shared_worktrees()
    }

    /// The local sessions whose usage limit is armed to resume at its reset, found by a query
    /// over the saved settings rather than by reading every session's.
    pub fn armed_usage_limits(&self) -> Result<Vec<i64>, String> {
        Ok(self.shared_armed_usage_limits())
    }

    /// The provider session `id` runs on, without reading the other sessions.
    pub fn session_provider(&self, id: i64) -> Result<Option<String>, String> {
        self.shared_session_provider(id)
    }

    pub fn context_usage(&self, id: i64) -> Result<Option<crate::context::Usage>, String> {
        self.shared_context(id)
    }

    pub fn save_context_usage(&self, id: i64, usage: &crate::context::Usage) -> Result<(), String> {
        self.shared_patch(
            id,
            &[
                ("context_used", serde_json::json!(usage.used)),
                ("context_window", serde_json::json!(usage.window)),
            ],
        )
    }

    pub fn save_note(&self, note: &crate::response::ResponseNote) -> Result<(), String> {
        self.save_response_note(note)
    }

    pub fn notes(&self) -> Result<Vec<crate::response::ResponseNote>, String> {
        Ok(self
            .list_notes()?
            .into_iter()
            .map(|note| {
                let source_session = self.note_source_session(&note).unwrap_or(0);
                crate::response::ResponseNote {
                    id: note
                        .id
                        .strip_prefix("gpui-note-")
                        .and_then(|id| id.parse().ok())
                        .unwrap_or_else(|| shared::handle(&note.id) as u64),
                    title: note.title,
                    body: note.body,
                    tags: note.tags,
                    source_session,
                    source_cwd: note.source_cwd.unwrap_or_default(),
                }
            })
            .collect())
    }

    pub fn save_agent_settings(
        &self,
        id: i64,
        settings: &crate::agent::Settings,
    ) -> Result<(), String> {
        self.shared_save_agent(id, settings)
    }

    pub fn configure_agent(
        &self,
        id: i64,
        provider: &str,
        settings: &crate::agent::Settings,
    ) -> Result<(), String> {
        if self
            .agent_settings(id)?
            .is_some_and(|saved| saved.revision > settings.revision)
        {
            return Ok(());
        }
        self.change_provider(id, provider)?;
        self.save_agent_settings(id, settings)
    }

    pub fn change_provider(&self, id: i64, provider: &str) -> Result<(), String> {
        self.shared_change_provider(id, provider)
    }

    pub fn save_ui_state(&self, state: &str) -> Result<(), String> {
        self.shared_save_setting("workspace", state)
    }

    pub fn sessions(&self) -> Result<Vec<Session>, String> {
        let mut sessions = self.shared_sessions()?;
        sessions.sort_by_key(|s| (std::cmp::Reverse(s.updated_at), std::cmp::Reverse(s.id)));
        Ok(sessions)
    }

    /// Sessions whose latest message is saved as a draft and not yet sent.
    pub fn draft_sessions(&self) -> Result<std::collections::HashSet<i64>, String> {
        self.shared_drafts()
    }

    pub fn create(&self, provider: &str, cwd: &str) -> Result<Session, String> {
        self.shared_create(provider, cwd)
    }

    /// A WAL reader for background search, independent of the writer's mutex.
    /// In-memory fixture stores must continue using their original connection.
    pub fn search_reader(&self) -> Result<Option<Self>, String> {
        if self.path == Path::new(":memory:") {
            return Ok(None);
        }
        self.shared_search_reader().map(Some)
    }

    /// Stream saved message bodies without loading conversations into the UI.
    /// Rejoin row fragments so phrase matches span storage boundaries.
    pub fn search_sessions(
        &mut self,
        query: &str,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(Vec<crate::search::Hit>, bool), String> {
        self.shared_search(query, cancel)
    }

    pub fn load(&self, session: i64) -> Result<Transcript, String> {
        self.shared_load(session)
    }

    pub fn write_rows(&mut self, session: i64, start: usize, rows: &[Row]) -> Result<(), String> {
        self.write_rows_with_message(session, start, rows, None)
    }

    pub fn write_rows_with_message(
        &mut self,
        session: i64,
        start: usize,
        rows: &[Row],
        message_id: Option<&str>,
    ) -> Result<(), String> {
        self.shared_write_rows(session, start, rows, message_id)
    }

    /// Archiving preserves the transcript, provider context and configuration.
    pub fn set_archived(&self, id: i64, archived: bool) -> Result<(), String> {
        self.shared_patch(id, &[("archived", serde_json::json!(archived))])
    }

    pub fn set_pinned(&self, ids: &[i64], pinned: bool) -> Result<(), String> {
        self.shared_pins(ids, pinned)
    }

    pub fn set_work_item(
        &self,
        id: i64,
        item: Option<&crate::session_groups::WorkItem>,
    ) -> Result<(), String> {
        self.shared_patch(
            id,
            &[(
                "linked_work_item_json",
                item.map(shared::encode)
                    .transpose()?
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            )],
        )
    }

    /// A project's folders and folded groups.
    pub fn groups(&self, cwd: &str) -> Result<crate::session_groups::Groups, String> {
        self.shared_groups(cwd)
    }

    pub fn save_groups(
        &self,
        cwd: &str,
        groups: &crate::session_groups::Groups,
    ) -> Result<(), String> {
        self.shared_save_setting(&format!("groups:{cwd}"), &shared::encode(groups)?)
    }

    pub fn reminders(&self) -> Result<Vec<Reminder>, String> {
        self.shared_reminders()
    }

    /// Schedules (or reschedules, re-arming) each session's reminder.
    pub fn set_reminders(&mut self, ids: &[i64], due_at: u64) -> Result<(), String> {
        self.shared_set_reminders(ids, due_at)
    }

    /// Re-arms only the reminder occurrence the user snoozed. A stale popup
    /// must not overwrite a newer schedule created in another window.
    pub fn snooze_reminder_if(
        &self,
        id: i64,
        expected_due: u64,
        new_due: u64,
    ) -> Result<bool, String> {
        self.shared_snooze_reminder_if(id, expected_due, new_due)
    }

    /// Clears the sessions' reminders; with `expected`, only that one, so a
    /// stale delivery never clears a newer reminder.
    pub fn clear_reminders(&self, ids: &[i64], expected: Option<u64>) -> Result<(), String> {
        self.shared_clear_reminders(ids, expected)
    }

    /// Marks due, undelivered reminders delivered and returns them, once.
    pub fn take_due_reminders(&mut self, now: u64) -> Result<Vec<Reminder>, String> {
        let due: Vec<_> = self
            .reminders()?
            .into_iter()
            .filter(|r| r.due_at <= now && r.fired_at.is_none())
            .collect();
        self.shared_fire_reminders(&due, now)
    }

    /// Deletes the conversation, its settings, reminder and folder for good.
    pub fn delete(&mut self, id: i64) -> Result<(), String> {
        // Resolve the folder first; the persisted id is gone after deletion.
        let dir = self.thread_dir(id).ok().flatten();
        self.shared_delete(id)?;
        thread_files::remove_dir(dir);
        Ok(())
    }

    pub fn provider_id(&self, id: i64, provider_id: &str) -> Result<(), String> {
        self.shared_patch(
            id,
            &[("provider_session_id", serde_json::json!(provider_id))],
        )
    }

    /// Keep conversation history while detaching a removed working copy.
    pub fn detach_worktree(
        &self,
        path: &str,
        main: &str,
    ) -> Result<Vec<(Session, crate::agent::Settings)>, String> {
        self.shared_detach_worktree(path, main)
    }

    pub fn reset_provider_id(&self, id: i64) -> Result<(), String> {
        self.shared_patch(id, &[("provider_session_id", serde_json::Value::Null)])
    }

    pub fn title(&self, id: i64, title: &str) -> Result<(), String> {
        self.shared_patch(id, &[("title", serde_json::json!(title))])
    }

    /// A late generated title cannot replace a manual rename or recreate a deleted session.
    pub fn replace_title(&self, id: i64, expected: &str, title: &str) -> Result<bool, String> {
        self.shared_replace_title(id, expected, title)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn removed_worktree_keeps_history_and_requires_reattachment() {
        let mut store = super::Store::in_memory().unwrap();
        let session = store.create("codex", "/project-worktrees/feature").unwrap();
        let settings = crate::agent::Settings {
            worktree_cwd: Some("/project-worktrees/feature".into()),
            ..Default::default()
        };
        store.save_agent_settings(session.id, &settings).unwrap();
        store
            .provider_id(session.id, "old-provider-thread")
            .unwrap();
        store
            .write_rows(
                session.id,
                0,
                &[crate::transcript::Row {
                    role: "user".into(),
                    text: "Keep my conversation".into(),
                    first: true,
                    sent_at: None,
                    id: None,
                }],
            )
            .unwrap();
        let detached = store
            .detach_worktree("/project-worktrees/feature", "/project")
            .unwrap();
        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].0.cwd, "/project");
        assert!(detached[0].0.provider_id.is_none());
        assert!(detached[0].1.worktree_removed);
        assert_eq!(
            store.load(session.id).unwrap().rows[0].text,
            "Keep my conversation"
        );
        assert!(
            store
                .agent_settings(session.id)
                .unwrap()
                .unwrap()
                .worktree_removed
        );
    }

    #[test]
    fn workspace_membership_reads_main_and_worktree_sessions_without_loading_history() {
        let store = super::Store::in_memory().unwrap();
        let main = store.create("codex", "/project").unwrap();
        let feature = store.create("claude", "/project").unwrap();
        store
            .save_agent_settings(
                feature.id,
                &crate::agent::Settings {
                    worktree_cwd: Some("/project-feature".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let scopes = store.session_worktrees().unwrap();
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[&main.id], None);
        assert_eq!(scopes[&feature.id].as_deref(), Some("/project-feature"));
    }

    use super::*;
    #[test]
    fn archive_and_activity_time_survive_restart_without_changing_history() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("native.db");
        let mut store = Store::open(&path).unwrap();
        let session = store.create("codex", "/project").unwrap();
        assert!(session.updated_at > 0);
        store
            .conn
            .execute(
                "UPDATE sessions SET updated_at=1 WHERE id=?1",
                [store.persisted_session_id(session.id).unwrap()],
            )
            .unwrap();
        let rows = vec![Row {
            role: "user".into(),
            text: "Keep this conversation".into(),
            first: true,
            sent_at: None,
            id: None,
        }];
        store.write_rows(session.id, 0, &rows).unwrap();
        store.provider_id(session.id, "resume-this-thread").unwrap();
        let updated_at = store.sessions().unwrap()[0].updated_at;
        assert!(updated_at > 1);
        store.set_archived(session.id, true).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let saved = &store.sessions().unwrap()[0];
        assert!(saved.archived);
        assert_eq!(
            saved.updated_at, updated_at,
            "archiving does not make an old session look new"
        );
        assert_eq!(saved.provider_id.as_deref(), Some("resume-this-thread"));
        assert_eq!(store.load(session.id).unwrap().rows, rows);
        store.set_archived(session.id, false).unwrap();
        assert!(!store.sessions().unwrap()[0].archived);
    }
    #[test]
    fn pins_links_groups_and_reminders_survive_restart_and_delete_clears_them() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("native.db");
        let mut store = Store::open(&path).unwrap();
        let keep = store.create("codex", "/project").unwrap();
        let gone = store.create("claude", "/project").unwrap();
        store.set_pinned(&[keep.id], true).unwrap();
        let item =
            crate::session_groups::parse_github_work_item("https://github.com/acme/app/pull/9")
                .unwrap();
        store.set_work_item(keep.id, Some(&item)).unwrap();
        let mut groups = crate::session_groups::Groups::default();
        groups.create_folder(&[keep.id, gone.id], "f".into());
        groups.pinned_collapsed = true;
        store.save_groups("/project", &groups).unwrap();
        store.set_reminders(&[keep.id, gone.id], 100).unwrap();
        store.set_reminders(&[keep.id], 200).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        let saved = store
            .sessions()
            .unwrap()
            .into_iter()
            .find(|s| s.id == keep.id)
            .unwrap();
        assert!(saved.pinned);
        assert_eq!(saved.work_item, Some(item));
        assert_eq!(store.groups("/project").unwrap(), groups);
        assert_eq!(store.groups("/other").unwrap(), Default::default());
        let due = store.take_due_reminders(150).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].session_id, gone.id);
        assert!(
            store.take_due_reminders(150).unwrap().is_empty(),
            "delivered once"
        );
        store.clear_reminders(&[keep.id], Some(100)).unwrap();
        assert_eq!(
            store.reminders().unwrap().len(),
            2,
            "a stale time clears nothing"
        );
        store.delete(gone.id).unwrap();
        assert_eq!(store.reminders().unwrap().len(), 1);
        assert!(store.sessions().unwrap().iter().all(|s| s.id != gone.id));
        assert_eq!(
            store.load(gone.id).unwrap_err(),
            "Session no longer exists",
            "its history went with it"
        );
        store.clear_reminders(&[keep.id], None).unwrap();
        assert!(store.reminders().unwrap().is_empty());
        store.set_work_item(keep.id, None).unwrap();
        store.set_pinned(&[keep.id], false).unwrap();
        let saved = &store.sessions().unwrap()[0];
        assert!(!saved.pinned && saved.work_item.is_none());
    }
    #[test]
    fn sessions_made_in_one_millisecond_list_newest_first() {
        let mut store = Store::in_memory().unwrap();
        let made: Vec<i64> = (0..50)
            .map(|_| store.create("codex", "/project").unwrap().id)
            .collect();
        let listed: Vec<i64> = store.sessions().unwrap().iter().map(|s| s.id).collect();
        assert_eq!(listed, made.iter().rev().copied().collect::<Vec<_>>());
        // Writing to the oldest moves it first, past every stamp before it.
        store
            .write_rows(
                made[0],
                0,
                &[crate::transcript::Row {
                    role: "user".into(),
                    text: "Again".into(),
                    first: true,
                    sent_at: None,
                    id: None,
                }],
            )
            .unwrap();
        assert_eq!(store.sessions().unwrap()[0].id, made[0]);
    }

    #[test]
    fn saved_manual_names_and_deleted_sessions_reject_late_titles() {
        let mut store = Store::in_memory().unwrap();
        let session = store.create("codex", "/tmp").unwrap();
        let gone = store.create("codex", "/tmp").unwrap();
        assert!(
            store
                .replace_title(session.id, &session.title, "Initial prompt")
                .unwrap()
        );
        store.title(session.id, "Manual name").unwrap();
        assert!(
            !store
                .replace_title(session.id, "Initial prompt", "Generated name")
                .unwrap()
        );
        store.delete(gone.id).unwrap();
        assert!(
            !store
                .replace_title(gone.id, &gone.title, "Generated name")
                .unwrap(),
            "a late title cannot recreate a deleted session"
        );
        let sessions = store.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "Manual name");
    }
    #[test]
    fn send_times_survive_streaming_and_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("native.db");
        let mut store = Store::open(&path).unwrap();
        let session = store.create("codex", "/project").unwrap();
        let sent_at = Some(1_700_000_000_000);
        let mut transcript = Transcript::default();
        transcript.begin("user");
        transcript.rows[0].sent_at = sent_at;
        transcript.append(&"A long queued prompt. ".repeat(300));
        assert!(transcript.rows.len() > 1);
        assert!(transcript.rows[1..].iter().all(|r| r.sent_at.is_none()));
        let answer = transcript.begin("assistant");
        transcript.append("Starting");
        store.write_rows(session.id, 0, &transcript.rows).unwrap();
        let answer_time = transcript.rows[answer].sent_at;
        transcript.append(" and finished");
        store
            .write_rows(session.id, answer, &transcript.rows[answer..])
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let saved = store.load(session.id).unwrap();
        assert_eq!(saved.rows, transcript.rows);
        assert_eq!(saved.rows[answer].sent_at, answer_time);
        let legacy: Row =
            serde_json::from_str(r#"{"role":"user","text":"Old","first":true}"#).unwrap();
        assert_eq!(legacy.sent_at, None);
    }
    #[test]
    fn delayed_configuration_cannot_restore_an_old_model_or_provider() {
        let store = Store::in_memory().unwrap();
        let session = store.create("codex", "/tmp").unwrap();
        let old = crate::agent::Settings {
            revision: 1,
            model: Some("first-model".into()),
            ..Default::default()
        };
        let current = crate::agent::Settings {
            revision: 2,
            model: Some("selected-model".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        store
            .configure_agent(session.id, "claude", &current)
            .unwrap();
        store.provider_id(session.id, "new-thread").unwrap();
        store.configure_agent(session.id, "codex", &old).unwrap();
        let saved = store.agent_settings(session.id).unwrap().unwrap();
        assert_eq!(saved.model, current.model);
        assert_eq!(saved.effort, current.effort);
        let session = store.sessions().unwrap().remove(0);
        assert_eq!(session.provider, "claude");
        assert_eq!(session.provider_id.as_deref(), Some("new-thread"));
    }
    #[test]
    fn wal_restart_preserves_partial_output_and_provider_context() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("native.db");
        let mut store = Store::open(&path).unwrap();
        let s = store.create("codex", "/tmp").unwrap();
        assert!(!s.has_messages);
        assert!(!store.sessions().unwrap()[0].has_messages);
        let mut t = Transcript::default();
        t.begin("user");
        t.append("hello");
        t.begin("assistant");
        t.append("partial 🙂");
        store.write_rows(s.id, 0, &t.rows).unwrap();
        store.provider_id(s.id, "thread-1").unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.sessions().unwrap()[0].has_messages);
        assert_eq!(store.load(s.id).unwrap().rows, t.rows);
        assert_eq!(
            store.sessions().unwrap()[0].provider_id.as_deref(),
            Some("thread-1")
        );
    }
}
