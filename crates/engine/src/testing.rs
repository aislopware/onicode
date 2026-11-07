//! The seam a viewer's tests and debug fixtures write and read the engine's store through, as
//! typed calls: the store itself never leaves the engine. Nothing here runs in a release viewer
//! but the debug-only fixtures, which write the store with no engine running ([`Seed::offline`]).
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use onicode_core::accounts::Book;
use onicode_core::agent::Settings;
use onicode_core::follow_up::State;
use onicode_core::notes::Note;
use onicode_core::store::{Session, Store};
use onicode_core::transcript::Row;

use crate::accounts::OfflineProbe;
use crate::host::{InstanceLock, prepare_dir, stop_holder};
use crate::local::{DATABASE, Options};
use crate::turn::{Database, locked};
use crate::work::{OfflineGh, OfflineTrackers};

/// An engine's options for a test: no disk watched, and no provider CLI, tracker or `gh` asked.
#[must_use]
pub fn quiet() -> Options {
    Options {
        quiet: true,
        chores: None,
        probe: Some(Arc::new(OfflineProbe)),
        trackers: Some(Arc::new(OfflineTrackers)),
        gh: Some(Arc::new(OfflineGh)),
    }
}

/// The engine's store, written and read one typed call at a time.
#[derive(Clone)]
pub struct Seed {
    store: Database,
}

impl Seed {
    pub(crate) fn new(store: Database) -> Self {
        Self { store }
    }

    /// Hands `write` the store in `data_dir` with no engine running: the engine that runs is
    /// stopped (its agents with it), and the instance lock is held meanwhile so none starts; the
    /// next viewer to connect launches one that reads what `write` wrote. Debug fixtures only.
    pub fn offline<T>(data_dir: &Path, write: impl FnOnce(&Self) -> T) -> Result<T, String> {
        prepare_dir(data_dir)
            .map_err(|error| format!("Cannot prepare {}: {error}", data_dir.display()))?;
        if InstanceLock::held(data_dir) && !stop_holder(data_dir, Duration::from_secs(3)) {
            return Err("The running engine did not stop".into());
        }
        let _lock = InstanceLock::wait(data_dir, Duration::from_secs(3))
            .map_err(|error| format!("Cannot take the engine lock: {error}"))?
            .ok_or("Another engine started meanwhile")?;
        let path = data_dir.join(DATABASE);
        let store = Store::open_monocode(&path)
            .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
        Ok(write(&Self::new(Arc::new(Mutex::new(store)))))
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
