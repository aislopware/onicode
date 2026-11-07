//! The app's own state, kept beside the engine's and never in it: the saved window, layout and
//! preferences (`workspace`), and the markers the app keeps for itself (what the Inbox, the update
//! report and linked work items last showed). One `ui.sqlite3` in the app's data directory,
//! read whole at launch and written on its own thread, so neither touches the main thread's frame.
#![expect(
    clippy::disallowed_types,
    reason = "`ui.sqlite3` is the app's own file, never the engine's store"
)]
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use gpui::Global;
use rusqlite::{Connection, params};

/// The file, in the app's data directory.
pub(crate) const FILE: &str = "ui.sqlite3";
/// The saved window, layout and preferences, as JSON.
pub(crate) const WORKSPACE: &str = "workspace";

enum Write {
    Set(String, String),
    /// Answers once every write before it is on disk.
    Flush(mpsc::Sender<()>),
}

struct Inner {
    values: Mutex<HashMap<String, String>>,
    /// The writer thread's queue; `None` for state in memory.
    writer: Option<mpsc::Sender<Write>>,
}

/// The app's own key/value state. Reads come from memory; each write is queued to the writer
/// thread, in order.
#[derive(Clone)]
pub(crate) struct UiState(Arc<Inner>);

/// The app's state, for any view to read and write.
pub(crate) struct Shared(pub(crate) UiState);

impl Global for Shared {}

impl UiState {
    /// The state in `dir`'s `ui.sqlite3`, created if missing.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        let path = dir.join(FILE);
        let failed =
            |error: &dyn std::fmt::Display| format!("Cannot open {}: {error}", path.display());
        std::fs::create_dir_all(dir).map_err(|error| failed(&error))?;
        let connection = Connection::open(&path).map_err(|error| failed(&error))?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS ui_state (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            )
            .map_err(|error| failed(&error))?;
        let values = read(&connection).map_err(|error| failed(&error))?;
        let (writer, writes) = mpsc::channel();
        std::thread::Builder::new()
            .name("onicode-ui-state".into())
            .spawn(move || write(&connection, &writes))
            .map_err(|error| failed(&error))?;
        Ok(Self(Arc::new(Inner {
            values: Mutex::new(values),
            writer: Some(writer),
        })))
    }

    /// State that lives in memory only: a test's, or an app whose file cannot open.
    pub(crate) fn memory() -> Self {
        Self(Arc::new(Inner {
            values: Mutex::default(),
            writer: None,
        }))
    }

    fn values(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.0.values.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The value saved under `key`.
    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.values().get(key).cloned()
    }

    /// Saves `value` under `key`: read back at once, written off the main thread.
    pub(crate) fn set(&self, key: &str, value: String) {
        {
            let mut values = self.values();
            if values.get(key) == Some(&value) {
                return;
            }
            values.insert(key.to_owned(), value.clone());
        }
        if let Some(writer) = &self.0.writer {
            let _ = writer.send(Write::Set(key.to_owned(), value));
        }
    }

    /// Waits until every write so far is on disk: before the app quits.
    pub(crate) fn flush(&self) {
        let Some(writer) = &self.0.writer else {
            return;
        };
        let (done, flushed) = mpsc::channel();
        if writer.send(Write::Flush(done)).is_ok() {
            let _ = flushed.recv();
        }
    }
}

fn read(connection: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    let mut statement = connection.prepare("SELECT key, value FROM ui_state")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Writes what arrives, the last value of each key in a burst only (a resize saves the
/// workspace every frame), and answers each flush once what came before it is on disk.
fn write(connection: &Connection, writes: &mpsc::Receiver<Write>) {
    while let Ok(first) = writes.recv() {
        let mut pending: HashMap<String, String> = HashMap::new();
        let mut flushed = Vec::new();
        for write in std::iter::once(first).chain(writes.try_iter()) {
            match write {
                Write::Set(key, value) => {
                    pending.insert(key, value);
                }
                Write::Flush(done) => flushed.push(done),
            }
        }
        for (key, value) in pending {
            if let Err(error) = connection.execute(
                "INSERT INTO ui_state (key, value) VALUES (?1, ?2) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            ) {
                eprintln!("Cannot save {key}: {error}");
            }
        }
        for done in flushed {
            let _ = done.send(());
        }
    }
}

/// The app's state, installing one in memory where none was opened (a test's).
pub(crate) fn shared(cx: &mut gpui::App) -> UiState {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let state = UiState::memory();
    cx.set_global(Shared(state.clone()));
    state
}

#[cfg(test)]
mod tests {
    use onicode_protocol::OpId;

    use super::*;

    /// A directory of its own under the system's temporary one, removed when dropped.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let name = format!("onicode-ui-state-{name}-{}", OpId::new());
            Self(std::env::temp_dir().join(name))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_write_reads_back_at_once_and_survives_a_reopen() {
        let scratch = Scratch::new("reopen");
        let state = UiState::open(&scratch.0).unwrap();
        state.set("onicode.installedUpdate", "1.2.0".to_owned());
        assert_eq!(
            state.get("onicode.installedUpdate").as_deref(),
            Some("1.2.0")
        );
        state.flush();
        let reopened = UiState::open(&scratch.0).unwrap();
        assert_eq!(
            reopened.get("onicode.installedUpdate").as_deref(),
            Some("1.2.0")
        );
    }
}
