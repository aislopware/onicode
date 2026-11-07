//! The seam a viewer's tests and debug fixtures write and read the engine's store through, as
//! typed calls: the store itself never leaves the engine. Nothing here runs in a release viewer
//! but the debug-only fixtures.
use std::collections::HashSet;

use onicode_core::accounts::Book;
use onicode_core::agent::Settings;
use onicode_core::follow_up::State;
use onicode_core::notes::Note;
use onicode_core::store::Session;
use onicode_core::transcript::Row;

use crate::turn::{Database, locked};

/// The engine's store, written and read one typed call at a time.
#[derive(Clone)]
pub struct Seed {
    store: Database,
}

impl Seed {
    pub(crate) fn new(store: Database) -> Self {
        Self { store }
    }

    pub fn sessions(&self) -> Result<Vec<Session>, String> {
        locked(&self.store)?.sessions()
    }

    pub fn create(&self, provider: &str, cwd: &str) -> Result<Session, String> {
        locked(&self.store)?.create(provider, cwd)
    }

    pub fn title(&self, row: i64, title: &str) -> Result<(), String> {
        locked(&self.store)?.title(row, title)
    }

    pub fn write_rows(&self, row: i64, start: usize, rows: &[Row]) -> Result<(), String> {
        locked(&self.store)?.write_rows(row, start, rows)
    }

    /// The session's rows as stored.
    pub fn rows(&self, row: i64) -> Result<Vec<Row>, String> {
        Ok(locked(&self.store)?.load(row)?.rows)
    }

    pub fn provider_id(&self, row: i64, provider_id: &str) -> Result<(), String> {
        locked(&self.store)?.provider_id(row, provider_id)
    }

    /// The session's stored provider thread, if any.
    pub fn stored_provider_id(&self, row: i64) -> Result<Option<String>, String> {
        Ok(locked(&self.store)?
            .sessions()?
            .into_iter()
            .find(|session| session.id == row)
            .and_then(|session| session.provider_id))
    }

    pub fn agent_settings(&self, row: i64) -> Result<Option<Settings>, String> {
        locked(&self.store)?.agent_settings(row)
    }

    pub fn save_agent_settings(&self, row: i64, settings: &Settings) -> Result<(), String> {
        locked(&self.store)?.save_agent_settings(row, settings)
    }

    pub fn follow_ups(&self, row: i64) -> Result<State, String> {
        locked(&self.store)?.follow_ups(row)
    }

    pub fn save_follow_ups(&self, row: i64, state: &State) -> Result<(), String> {
        locked(&self.store)?.save_follow_ups(row, state)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>, String> {
        locked(&self.store)?.setting(key)
    }

    pub fn save_setting(&self, key: &str, value: &str) -> Result<(), String> {
        locked(&self.store)?.save_setting(key, value)
    }

    pub fn provider_accounts(&self) -> Result<Book, String> {
        locked(&self.store)?.provider_accounts()
    }

    pub fn save_provider_accounts(&self, book: &Book) -> Result<(), String> {
        locked(&self.store)?.save_provider_accounts(book)
    }

    pub fn list_notes(&self) -> Result<Vec<Note>, String> {
        locked(&self.store)?.list_notes()
    }

    pub fn get_note(&self, id: &str) -> Result<Option<Note>, String> {
        locked(&self.store)?.get_note(id)
    }

    pub fn create_note(&self, id: &str, cwd: Option<String>) -> Result<Note, String> {
        locked(&self.store)?.create_note(id, cwd)
    }

    pub fn draft_sessions(&self) -> Result<HashSet<i64>, String> {
        locked(&self.store)?.draft_sessions()
    }

    pub fn persisted_session_id(&self, row: i64) -> Result<String, String> {
        locked(&self.store)?.persisted_session_id(row)
    }

    pub fn delete(&self, row: i64) -> Result<(), String> {
        locked(&self.store)?.delete(row)
    }
}
