//! Settings live in a sidecar beside the sessions database. Updating one key
//! must not clone, serialize and fsync every cached app result under the Store
//! mutex. SQLite commits only the changed key and retains durable receipts.
use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use super::sql;

type Snapshot = BTreeMap<String, String>;

/// The sidecar's newest `user_version`.
const LATEST: i64 = 3;

/// Brings a writable sidecar from version 0 (new) to [`LATEST`], one step per version, each in its
/// own writer transaction so a second window waiting on the lock sees the step already done.
fn migrate(conn: &Connection) -> Result<(), String> {
    loop {
        let tx = sql(Transaction::new_unchecked(
            conn,
            TransactionBehavior::Immediate,
        ))?;
        let version: i64 = sql(tx.query_row("PRAGMA user_version", [], |row| row.get(0)))?;
        match version {
            0 => sql(tx.execute_batch(
                "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 PRAGMA user_version=1;",
            ))?,
            // 2: the engine's protocol ids for the desktop schema's sessions, whose own ids are
            // never altered, and its ledger of processed mutating calls (engine.md "Ids").
            1 => sql(tx.execute_batch(
                "CREATE TABLE session_uids(session TEXT PRIMARY KEY, uid TEXT NOT NULL UNIQUE);
                 CREATE TABLE ops(op TEXT PRIMARY KEY, session TEXT NOT NULL,
                   receipt TEXT NOT NULL, at INTEGER NOT NULL);
                 CREATE INDEX ops_at ON ops(at);
                 PRAGMA user_version=2;",
            ))?,
            // 3: the forwarding engine's outbox of calls for an engine that was offline
            // (engine.md "Authority").
            2 => sql(tx.execute_batch(
                "CREATE TABLE outbox(n INTEGER PRIMARY KEY AUTOINCREMENT,
                   op TEXT NOT NULL UNIQUE, engine TEXT NOT NULL, method TEXT NOT NULL,
                   params TEXT NOT NULL, at INTEGER NOT NULL);
                 CREATE INDEX outbox_engine ON outbox(engine, n);
                 PRAGMA user_version=3;",
            ))?,
            _ => return Ok(()),
        }
        sql(tx.commit())?;
    }
}

pub(super) fn open(
    database: &Path,
    read_only: bool,
) -> Result<(Snapshot, Option<Connection>), String> {
    if database == Path::new(":memory:") {
        return Ok((BTreeMap::new(), None));
    }
    let name = database.file_name().unwrap_or_default().to_string_lossy();
    let path = database.with_file_name(format!("{name}.gpui.sqlite3"));
    if read_only && !path.exists() {
        return Ok((BTreeMap::new(), None));
    }
    if !read_only {
        // Private to the user before any value is stored.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.to_string()),
        }
    }
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = sql(Connection::open_with_flags(&path, flags))?;
    sql(conn.busy_timeout(std::time::Duration::from_secs(5)))?;
    let version: i64 = sql(conn.query_row("PRAGMA user_version", [], |row| row.get(0)))?;
    if version > LATEST {
        return Err(format!("Unsupported native settings version: {version}"));
    }
    if read_only && version == 0 {
        // A writer has not created the table yet: nothing is saved.
        return Ok((BTreeMap::new(), None));
    }
    if !read_only {
        sql(conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;"))?;
        migrate(&conn)?;
    }
    let values = {
        let mut query = sql(conn.prepare("SELECT key,value FROM settings"))?;
        let values = sql(query.query_map([], |row| Ok((row.get(0)?, row.get(1)?))))?;
        sql(values.collect())?
    };
    Ok((values, Some(conn)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn settings_survive_restart_in_a_private_sidecar_without_touching_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monocode.db");
        let receipt = "large result".repeat(100_000);
        let store = Store::open(&path).unwrap();
        store
            .save_setting("cached-result:1:request", &receipt)
            .unwrap();
        drop(store);
        let database = std::fs::read(&path).unwrap();
        let store = Store::open(&path).unwrap();
        store.save_setting("workspace", "new tabs").unwrap();
        drop(store);
        assert_eq!(std::fs::read(&path).unwrap(), database);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.setting("workspace").unwrap().as_deref(),
            Some("new tabs")
        );
        assert_eq!(
            reopened.setting("cached-result:1:request").unwrap(),
            Some(receipt)
        );
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("monocode.db.gpui.sqlite3"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn small_updates_do_not_rewrite_large_cached_results_and_unchanged_saves_do_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("monocode.db")).unwrap();
        store
            .save_setting("cached-result:1:large", &"x".repeat(10_000_000))
            .unwrap();
        let conn = store.shared.extras_db.as_ref().unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let wal = dir.path().join("monocode.db.gpui.sqlite3-wal");
        for i in 0..10 {
            store.save_setting("workspace", &i.to_string()).unwrap();
        }
        let bytes = std::fs::metadata(&wal).unwrap().len();
        assert!(bytes < 100_000, "ten tiny updates wrote {bytes} bytes");
        let changes = conn.total_changes();
        store.save_setting("workspace", "9").unwrap();
        assert_eq!(conn.total_changes(), changes);
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), bytes);
    }

    #[test]
    fn failed_save_preserves_the_committed_cache_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("monocode.db")).unwrap();
        store.save_setting("workspace", "old").unwrap();
        let conn = store.shared.extras_db.as_ref().unwrap();
        conn.execute_batch("PRAGMA query_only=ON").unwrap();
        assert!(store.save_setting("workspace", "new").is_err());
        assert_eq!(store.setting("workspace").unwrap().as_deref(), Some("old"));
        conn.execute_batch("PRAGMA query_only=OFF").unwrap();
        store.save_setting("workspace", "new").unwrap();
        assert_eq!(store.setting("workspace").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn read_only_access_without_a_sidecar_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monocode.db");
        let (values, conn) = open(&path, true).unwrap();
        assert!(values.is_empty());
        assert!(conn.is_none());
        assert!(!dir.path().join("monocode.db.gpui.sqlite3").exists());
    }

    #[test]
    fn read_only_reopen_sees_current_settings_and_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monocode.db");
        let store = Store::open(&path).unwrap();
        store.save_setting("workspace", "current").unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        assert_eq!(
            reader.setting("workspace").unwrap().as_deref(),
            Some("current")
        );
        assert!(reader.save_setting("workspace", "wrong").is_err());
    }
}
