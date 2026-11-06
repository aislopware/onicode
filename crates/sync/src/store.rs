//! `DocsStore` — local SQLite persistence for doc snapshots, the processed-command ledger
//! (ARCHITECTURE §2 command plane: entries are marked processed BEFORE execution so a crash can
//! never double-execute a command), and the outbox of local sends the chat's host has not acked.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loro::VersionVector;
use rusqlite::{Connection, OptionalExtension, params};

/// Errors surfaced by [`DocsStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A stored value no longer parses.
    #[error("corrupt row: {0}")]
    Corrupt(String),
}

/// One local write the chat's host has not acked yet.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    /// Order of the write.
    pub ordinal: i64,
    /// The write's Loro update bytes, replayed into the doc on open in case the snapshot
    /// holding them was never saved.
    pub bytes: Vec<u8>,
    /// The version that holds the write; the row retires once the host's acked vv covers it.
    pub end: VersionVector,
}

/// Synchronous command/doc callbacks still need commit-before-send semantics.
/// Relinquish a multithread runtime's core BEFORE either SQLite or its mutex
/// can block; moving only the snapshot writer leaves these contenders fatal.
pub(crate) fn store_blocking<T>(f: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

/// Ordered, append-only migrations. Each entry runs once inside a transaction;
/// `schema_migrations` records what has been applied.
const MIGRATIONS: &[&str] = &[
    // v1 — snapshots + processed-command ledger
    "CREATE TABLE snapshots (
        doc_id   TEXT PRIMARY KEY,
        bytes    BLOB NOT NULL,
        saved_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE processed_commands (
        command_id   TEXT PRIMARY KEY,
        processed_at INTEGER NOT NULL
     ) STRICT;",
    // v2 — chat2 room cursor + doc epoch (docs/chat2-sync.md C2). The cursor
    // is persisted in the SAME transaction as the snapshot bytes, so content
    // and cursor cannot diverge (restored backups / copied devices simply
    // redownload from their honest cursor). `epoch` marks rebuild lineage
    // (M1/M3): 2 = thin chat2 rebuild; NULL/0 = pre-migration s2 doc.
    "ALTER TABLE snapshots ADD COLUMN cursor INTEGER;
     ALTER TABLE snapshots ADD COLUMN epoch INTEGER;",
    // v3 — publication survives actor eviction and process restart.
    "CREATE TABLE chat_outbox (
        ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
        doc_id TEXT NOT NULL,
        batch_id TEXT NOT NULL UNIQUE,
        bytes BLOB NOT NULL,
        needs_checkpoint INTEGER NOT NULL DEFAULT 0
     ) STRICT;
     CREATE INDEX chat_outbox_doc ON chat_outbox(doc_id,ordinal);
     CREATE TABLE chat_outbox_initialized (doc_id TEXT PRIMARY KEY) STRICT;",
    // Older cursors may have advanced over parked imports. Trust only writes
    // made by the causal-aware persister, not every legacy epoch-2 snapshot.
    "ALTER TABLE snapshots ADD COLUMN cursor_verified INTEGER NOT NULL DEFAULT 0;",
    // Durable wake/recovery receipts, independent of live document handles.
    "CREATE TABLE chat_sync_jobs (
        doc_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        version INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY(doc_id, kind)
    ) STRICT;
    CREATE TABLE sync_job_clock (id INTEGER PRIMARY KEY CHECK(id=1), value INTEGER NOT NULL) STRICT;
    INSERT INTO sync_job_clock VALUES (1,0);",
    "ALTER TABLE chat_sync_jobs ADD COLUMN cursor TEXT NOT NULL DEFAULT '';",
    // v7 — replicas sync by version vector over peer streams: no room cursor or epoch, and an
    // outbox row retires when the host's acked vv covers its end.
    "DROP TABLE chat_outbox;
     DROP TABLE chat_outbox_initialized;
     ALTER TABLE snapshots DROP COLUMN cursor;
     ALTER TABLE snapshots DROP COLUMN epoch;
     ALTER TABLE snapshots DROP COLUMN cursor_verified;
     DELETE FROM chat_sync_jobs;
     ALTER TABLE chat_sync_jobs DROP COLUMN cursor;
     CREATE TABLE chat_outbox (
        ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
        doc_id TEXT NOT NULL,
        end_vv BLOB NOT NULL,
        bytes BLOB NOT NULL
     ) STRICT;
     CREATE INDEX chat_outbox_doc ON chat_outbox(doc_id,ordinal);",
];

const STORE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// WAL size retained after a checkpoint resets the log. SQLite's default (-1)
/// keeps the file at its all-time high-water mark (observed: 400 MB). Steady
/// state never reaches this — auto-checkpoint folds the log every ~4 MB
/// (1000 pages) — so the cap only bites after a large burst (a multi-MB
/// snapshot, a VACUUM). 64 MiB bounds idle disk use while sparing ordinary
/// bursts a truncate/re-extend cycle on every write.
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;

/// VACUUM ([`DocsStore::reclaim_free_space`]) only when free pages are BOTH a large share of the
/// file and a meaningful absolute size: a healthy database never pays the rewrite.
const VACUUM_MIN_FREE_BYTES: i64 = 32 * 1024 * 1024;
/// `freelist_count / page_count` above which reclaiming is worth a rewrite.
const VACUUM_MIN_FREE_DENOMINATOR: i64 = 4;

/// Maintenance takes a lock only if it is free, and otherwise skips (and logs).
/// Zero, not a short wait: SQLite counts a busy timeout in planned sleeps, not
/// elapsed time, so on a loaded machine three 250 ms waits took over 3 s.
const MAINTENANCE_BUSY_TIMEOUT: Duration = Duration::ZERO;

/// SQLite-backed store under a data directory (`{data_dir}/docs.sqlite3`).
///
/// Holds warm-open doc snapshots (the DO room is authoritative; these make
/// cold starts instant and offline restarts possible) and the command ledger
/// that gives command execution mark-BEFORE-execute idempotence.
pub struct DocsStore {
    conn: Mutex<Connection>,
    failed_publications: Mutex<HashSet<String>>,
    /// Snapshot jobs wait here asynchronously before entering the blocking pool.
    pub snapshot_writer: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl DocsStore {
    /// Open (creating directory, database, and schema as needed).
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("docs.sqlite3");
        let mut conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
        conn.busy_timeout(STORE_BUSY_TIMEOUT)?;
        migrate(&mut conn)?;
        // Best-effort hygiene must never stall boot behind another connection.
        conn.busy_timeout(MAINTENANCE_BUSY_TIMEOUT)?;
        // Only the cheap WAL truncate here: open sits on boot paths (engine
        // assembly, the iOS client), and a VACUUM's cost is linear in the
        // live size. Callers run `reclaim_free_space` off those paths.
        checkpoint_truncate(&conn, &path, "open");
        conn.busy_timeout(STORE_BUSY_TIMEOUT)?;
        Ok(Self {
            conn: Mutex::new(conn),
            failed_publications: Mutex::new(HashSet::new()),
            snapshot_writer: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Records a local write before anything else sees it, so it survives a crash before the
    /// next snapshot and stays "sending" until the host acks `end`.
    ///
    /// # Errors
    ///
    /// SQLite's; the doc is then remembered as owing a forward, made durable by its next
    /// snapshot save.
    pub fn enqueue_outbox(
        &self,
        doc_id: &str,
        bytes: &[u8],
        end: &VersionVector,
    ) -> Result<(), StoreError> {
        store_blocking(|| {
            let result = self.conn().execute(
                "INSERT INTO chat_outbox(doc_id,end_vv,bytes) VALUES (?1,?2,?3)",
                params![doc_id, end.encode(), bytes],
            );
            if result.is_err() {
                self.failed_publications
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(doc_id.to_string());
            }
            result?;
            Ok(())
        })
    }

    /// The doc's unacked local writes, oldest first.
    ///
    /// # Errors
    ///
    /// SQLite's, or [`StoreError::Corrupt`] for a row whose vv does not parse.
    pub fn outbox(&self, doc_id: &str) -> Result<Vec<OutboxRow>, StoreError> {
        store_blocking(|| {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT ordinal,end_vv,bytes FROM chat_outbox WHERE doc_id=?1 ORDER BY ordinal",
            )?;
            let rows = stmt
                .query_map(params![doc_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?, r.get(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(ordinal, end, bytes)| {
                    Ok(OutboxRow {
                        ordinal,
                        bytes,
                        end: VersionVector::decode(&end)
                            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
                    })
                })
                .collect()
        })
    }

    /// Whether the doc has unacked local writes, without reading them.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn has_outbox(&self, doc_id: &str) -> Result<bool, StoreError> {
        store_blocking(|| {
            Ok(self.conn().query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_outbox WHERE doc_id=?1)",
                params![doc_id],
                |row| row.get(0),
            )?)
        })
    }

    /// How many unacked local writes the doc has, without reading them.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn outbox_len(&self, doc_id: &str) -> Result<u64, StoreError> {
        store_blocking(|| {
            Ok(self.conn().query_row(
                "SELECT count(*) FROM chat_outbox WHERE doc_id=?1",
                params![doc_id],
                |row| row.get(0),
            )?)
        })
    }

    /// Retires the doc's rows whose end `covered` includes; returns how many.
    ///
    /// # Errors
    ///
    /// SQLite's, or [`StoreError::Corrupt`].
    pub fn retire_outbox(
        &self,
        doc_id: &str,
        covered: &VersionVector,
    ) -> Result<usize, StoreError> {
        let done: Vec<i64> = self
            .outbox_ends(doc_id)?
            .into_iter()
            .filter(|(_, end)| covered.includes_vv(end))
            .map(|(ordinal, _)| ordinal)
            .collect();
        if done.is_empty() {
            return Ok(0);
        }
        store_blocking(|| {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            for ordinal in &done {
                tx.execute("DELETE FROM chat_outbox WHERE ordinal=?1", params![ordinal])?;
            }
            tx.commit()?;
            Ok(done.len())
        })
    }

    fn outbox_ends(&self, doc_id: &str) -> Result<Vec<(i64, VersionVector)>, StoreError> {
        store_blocking(|| {
            let conn = self.conn();
            let mut stmt =
                conn.prepare("SELECT ordinal,end_vv FROM chat_outbox WHERE doc_id=?1")?;
            let rows = stmt
                .query_map(params![doc_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(ordinal, end)| {
                    VersionVector::decode(&end)
                        .map(|end| (ordinal, end))
                        .map_err(|e| StoreError::Corrupt(e.to_string()))
                })
                .collect()
        })
    }

    /// Latest saved snapshot for `doc_id`, if any.
    pub fn load_snapshot(&self, doc_id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        store_blocking(|| self.load_snapshot_blocking(doc_id))
    }

    fn load_snapshot_blocking(&self, doc_id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let bytes = self
            .conn()
            .query_row(
                "SELECT bytes FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(bytes)
    }

    /// Save (upsert) the snapshot for `doc_id`.
    pub fn save_snapshot(&self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        store_blocking(|| self.save_snapshot_blocking(doc_id, bytes))
    }

    fn save_snapshot_blocking(&self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO snapshots (doc_id, bytes, saved_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id) DO UPDATE SET bytes = excluded.bytes, saved_at = excluded.saved_at",
            params![doc_id, bytes, now_ms()],
        )?;
        self.invalidate_failed_publication(&tx, doc_id)?;
        tx.commit()?;
        self.failed_publications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(doc_id);
        Ok(())
    }

    /// Replaces the doc with one adopted from a peer's snapshot, which already holds this
    /// replica's own unacked writes: the outbox empties, and a [`crate::FORWARD`] job keeps the
    /// doc owing its host until the host acks it.
    ///
    /// # Errors
    ///
    /// SQLite's; nothing changes then.
    pub fn adopt_snapshot(&self, doc_id: &str, bytes: &[u8]) -> Result<(), StoreError> {
        store_blocking(|| {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO snapshots (doc_id, bytes, saved_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(doc_id) DO UPDATE SET bytes = excluded.bytes, saved_at = excluded.saved_at",
                params![doc_id, bytes, now_ms()],
            )?;
            tx.execute("DELETE FROM chat_outbox WHERE doc_id = ?1", params![doc_id])?;
            crate::sync_jobs::schedule_in(&tx, doc_id, crate::FORWARD)?;
            tx.commit()?;
            self.failed_publications
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(doc_id);
            Ok(())
        })
    }

    /// Delete the snapshot row for `doc_id` (destructive schema breaks: the
    /// legacy `workspace` row is dropped on open). Missing rows are a no-op.
    pub fn delete_snapshot(&self, doc_id: &str) -> Result<(), StoreError> {
        store_blocking(|| self.delete_snapshot_blocking(doc_id))
    }

    fn delete_snapshot_blocking(&self, doc_id: &str) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM snapshots WHERE doc_id = ?1", params![doc_id])?;
        tx.execute("DELETE FROM chat_outbox WHERE doc_id = ?1", params![doc_id])?;
        tx.execute(
            "DELETE FROM chat_sync_jobs WHERE doc_id = ?1",
            params![doc_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether a snapshot row exists for `doc_id` — presence only, no blob read.
    pub fn has_snapshot(&self, doc_id: &str) -> Result<bool, StoreError> {
        store_blocking(|| self.has_snapshot_blocking(doc_id))
    }

    fn has_snapshot_blocking(&self, doc_id: &str) -> Result<bool, StoreError> {
        let hit = self
            .conn()
            .query_row(
                "SELECT 1 FROM snapshots WHERE doc_id = ?1",
                params![doc_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// The full command ledger — profile-import reads the source's claims so
    /// imported pending commands can never re-execute under the new profile.
    pub fn processed_commands(&self) -> Result<Vec<(String, i64)>, StoreError> {
        store_blocking(|| self.processed_commands_blocking())
    }

    fn processed_commands_blocking(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT command_id, processed_at FROM processed_commands")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Merge foreign ledger claims (profile import). Existing claims win;
    /// returns how many rows were newly inserted.
    pub fn import_processed_commands(&self, rows: &[(String, i64)]) -> Result<usize, StoreError> {
        store_blocking(|| self.import_processed_commands_blocking(rows))
    }

    fn import_processed_commands_blocking(
        &self,
        rows: &[(String, i64)],
    ) -> Result<usize, StoreError> {
        let mut inserted = 0;
        let conn = self.conn();
        for (command_id, processed_at) in rows {
            inserted += conn.execute(
                "INSERT OR IGNORE INTO processed_commands (command_id, processed_at) VALUES (?1, ?2)",
                params![command_id, processed_at],
            )?;
        }
        Ok(inserted)
    }

    /// Whether `command_id` has already been claimed for execution.
    pub fn is_processed(&self, command_id: &str) -> Result<bool, StoreError> {
        store_blocking(|| self.is_processed_blocking(command_id))
    }

    fn is_processed_blocking(&self, command_id: &str) -> Result<bool, StoreError> {
        let hit = self
            .conn()
            .query_row(
                "SELECT 1 FROM processed_commands WHERE command_id = ?1",
                params![command_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    /// Claim `command_id` for execution — call BEFORE executing (ledger rule:
    /// a crash mid-execution must never re-run the command). Returns `true`
    /// if this call claimed it, `false` if it was already processed.
    pub fn mark_processed(&self, command_id: &str) -> Result<bool, StoreError> {
        store_blocking(|| self.mark_processed_blocking(command_id))
    }

    fn mark_processed_blocking(&self, command_id: &str) -> Result<bool, StoreError> {
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO processed_commands (command_id, processed_at) VALUES (?1, ?2)",
            params![command_id, now_ms()],
        )?;
        Ok(changed > 0)
    }

    // Writes whose outbox insert failed become durable only in a snapshot; the snapshot that
    // holds them also records that the doc owes its host a forward, in the same transaction.
    fn invalidate_failed_publication(
        &self,
        tx: &rusqlite::Transaction<'_>,
        doc_id: &str,
    ) -> Result<(), StoreError> {
        if self
            .failed_publications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(doc_id)
        {
            crate::sync_jobs::schedule_in(tx, doc_id, crate::sync_jobs::FORWARD)?;
        }
        Ok(())
    }

    /// Reclaim free space: WAL truncate, then a VACUUM only past the
    /// free-page thresholds, never waiting on a lock. Cheap when there is
    /// nothing to reclaim. Blocking and holds the store for the VACUUM (it
    /// rewrites the live database) — run it off any latency-sensitive path;
    /// best-effort — problems are logged, never returned.
    pub fn reclaim_free_space(&self) {
        store_blocking(|| {
            let conn = self.conn();
            let Some(path) = conn.path().map(std::path::PathBuf::from) else {
                return;
            };
            if let Err(err) = conn.busy_timeout(MAINTENANCE_BUSY_TIMEOUT) {
                tracing::warn!(%err, "docs store: reclaim skipped");
                return;
            }
            maintain(&conn, &path);
            if let Err(err) = conn.busy_timeout(STORE_BUSY_TIMEOUT) {
                tracing::warn!(%err, "docs store: busy timeout not restored after reclaim");
            }
        });
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        // A poisoned lock only means another thread panicked mid-query; the
        // connection itself is still usable.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
         ) STRICT",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now_ms()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

/// Fold and truncate a bloated WAL, then reclaim free pages if they dominate
/// the file. Every step is best-effort — failures (including SQLITE_BUSY from
/// another connection) are logged. Steady-state WAL growth is handled by
/// SQLite's auto-checkpoint plus `journal_size_limit`, so there is no
/// periodic pass.
fn maintain(conn: &Connection, path: &Path) {
    checkpoint_truncate(conn, path, "open");
    match reclaim_free_pages(conn) {
        // VACUUM in WAL mode writes the whole live database into the WAL.
        Ok(true) => checkpoint_truncate(conn, path, "vacuum"),
        Ok(false) => {}
        Err(err) => tracing::warn!(%err, "docs store: VACUUM skipped"),
    }
}

/// `PRAGMA wal_checkpoint(TRUNCATE)`; logs instead of failing.
fn checkpoint_truncate(conn: &Connection, path: &Path, reason: &str) {
    let wal_path = path.with_extension("sqlite3-wal");
    let wal_bytes = || std::fs::metadata(&wal_path).map_or(0, |m| m.len());
    let before = wal_bytes();
    let result = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    });
    match result {
        Ok((0, frames)) => {
            if before > 0 {
                tracing::info!(
                    reason,
                    frames,
                    wal_bytes_before = before,
                    wal_bytes_after = wal_bytes(),
                    "docs store: WAL checkpointed and truncated"
                );
            }
        }
        Ok((_, frames)) => tracing::warn!(
            reason,
            frames,
            wal_bytes = before,
            "docs store: WAL checkpoint busy (another connection is active); left for later"
        ),
        Err(err) => tracing::warn!(reason, %err, "docs store: WAL checkpoint failed"),
    }
}

/// VACUUM when free pages are both a large fraction of the file and above an
/// absolute floor. Returns whether a VACUUM ran. Rewrite cost is linear in the
/// LIVE size (~0.6 s for 150 MB live / 70 MB free on an M-series SSD, warm
/// cache); healthy databases skip it entirely.
fn reclaim_free_pages(conn: &Connection) -> rusqlite::Result<bool> {
    let pragma = |name: &str| conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0));
    let page_size = pragma("page_size")?;
    let page_count = pragma("page_count")?;
    let freelist = pragma("freelist_count")?;
    if !worth_vacuuming(page_size, page_count, freelist) {
        return Ok(false);
    }
    let started = Instant::now();
    conn.execute_batch("VACUUM")?;
    let after = pragma("page_count")? * page_size;
    tracing::info!(
        bytes_before = page_count * page_size,
        bytes_after = after,
        free_bytes = freelist * page_size,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "docs store: reclaimed free pages with VACUUM"
    );
    Ok(true)
}

fn worth_vacuuming(page_size: i64, page_count: i64, freelist: i64) -> bool {
    freelist * page_size >= VACUUM_MIN_FREE_BYTES
        && freelist * VACUUM_MIN_FREE_DENOMINATOR >= page_count
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn contended_connection_does_not_starve_a_two_worker_runtime() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let holder = store.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holding = std::thread::spawn(move || {
            let _connection = holder.conn();
            locked_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(600));
        });
        locked_rx.recv().unwrap();
        let started = std::time::Instant::now();
        let write = store.clone();
        let a = tokio::spawn(async move {
            write.save_snapshot("whale", &[0; 1024]).unwrap();
        });
        let read = store.clone();
        let b = tokio::spawn(async move {
            read.load_snapshot("registry1").unwrap();
        });
        // Both contenders have a chance to enter the shared connection wait.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(300),
            "SQLite contenders monopolized both runtime workers"
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let _accepted = listener.accept().await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(300),
            "network progress waited for SQLite"
        );
        drop(socket);
        a.await.unwrap();
        b.await.unwrap();
        holding.join().unwrap();
    }

    #[test]
    fn snapshot_roundtrip_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        assert_eq!(store.load_snapshot("chat-1").unwrap(), None);
        store.save_snapshot("chat-1", b"v1").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v1"[..])
        );
        store.save_snapshot("chat-1", b"v2-longer-bytes").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2-longer-bytes"[..])
        );
        // Distinct docs do not collide.
        store.save_snapshot("chat-2", b"other").unwrap();
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"v2-longer-bytes"[..])
        );
    }

    #[test]
    fn processed_ledger_claims_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();

        assert!(!store.is_processed("cmd-1").unwrap());
        assert!(store.mark_processed("cmd-1").unwrap(), "first mark claims");
        assert!(store.is_processed("cmd-1").unwrap());
        assert!(
            !store.mark_processed("cmd-1").unwrap(),
            "second mark must not re-claim"
        );
    }

    #[test]
    fn reopen_preserves_data_and_migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DocsStore::open(dir.path()).unwrap();
            store.save_snapshot("chat-1", b"persisted").unwrap();
            store.mark_processed("cmd-1").unwrap();
        }
        let store = DocsStore::open(dir.path()).unwrap(); // re-runs migrate()
        assert_eq!(
            store.load_snapshot("chat-1").unwrap().as_deref(),
            Some(&b"persisted"[..])
        );
        assert!(store.is_processed("cmd-1").unwrap());
        assert!(!store.mark_processed("cmd-1").unwrap());
    }
}

#[cfg(test)]
mod outbox_tests {
    use loro::{ID, VersionVector};

    use super::*;

    fn vv(peer: u64, end: i32) -> VersionVector {
        let mut vv = VersionVector::new();
        vv.set_end(ID::new(peer, end));
        vv
    }

    #[test]
    fn outbox_survives_restart_and_retires_only_what_the_ack_covers() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DocsStore::open(dir.path()).unwrap();
            store.enqueue_outbox("chat", b"first", &vv(1, 3)).unwrap();
            store.enqueue_outbox("chat", b"second", &vv(1, 7)).unwrap();
            store
                .enqueue_outbox("other", b"elsewhere", &vv(1, 1))
                .unwrap();
        }
        let store = DocsStore::open(dir.path()).unwrap();
        let rows = store.outbox("chat").unwrap();
        assert_eq!(
            rows.iter().map(|r| r.bytes.as_slice()).collect::<Vec<_>>(),
            [&b"first"[..], &b"second"[..]]
        );
        assert_eq!(rows[1].end, vv(1, 7));

        // An ack from a replica that lacks the second write retires the first only.
        assert_eq!(store.retire_outbox("chat", &vv(1, 5)).unwrap(), 1);
        assert_eq!(store.outbox("chat").unwrap().len(), 1);
        // Another peer's ops do not cover ours.
        assert_eq!(store.retire_outbox("chat", &vv(2, 100)).unwrap(), 0);
        assert_eq!(store.retire_outbox("chat", &vv(1, 7)).unwrap(), 1);
        assert!(!store.has_outbox("chat").unwrap());
        assert!(store.has_outbox("other").unwrap(), "retiring is per doc");

        store.delete_snapshot("other").unwrap();
        assert!(!store.has_outbox("other").unwrap());
    }

    #[test]
    fn a_failed_outbox_write_leaves_a_forward_owed_by_the_next_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.save_snapshot("chat", b"before").unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TRIGGER fail_publication BEFORE INSERT ON chat_outbox \
                 BEGIN SELECT RAISE(FAIL, 'injected disk failure'); END;",
            )
            .unwrap();
        assert!(store.enqueue_outbox("chat", b"send", &vv(1, 1)).is_err());
        assert!(store.pending_forwards("", 8).unwrap().is_empty());
        store.save_snapshot("chat", b"after").unwrap();
        drop(store);
        let reopened = DocsStore::open(dir.path()).unwrap();
        assert_eq!(reopened.pending_forwards("", 8).unwrap(), ["chat"]);
    }

    #[test]
    fn an_adopted_snapshot_replaces_the_outbox_with_a_forward() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.save_snapshot("chat", b"behind").unwrap();
        store.enqueue_outbox("chat", b"mine", &vv(1, 1)).unwrap();
        assert_eq!(store.outbox_len("chat").unwrap(), 1);
        store.adopt_snapshot("chat", b"adopted").unwrap();
        assert_eq!(store.load_snapshot("chat").unwrap().unwrap(), b"adopted");
        assert_eq!(store.outbox_len("chat").unwrap(), 0);
        assert_eq!(store.pending_forwards("", 8).unwrap(), ["chat"]);
    }
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    fn pragma(conn: &Connection, name: &str) -> i64 {
        conn.pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    fn file_len(path: &Path) -> u64 {
        std::fs::metadata(path).map_or(0, |m| m.len())
    }

    /// 48 MiB of snapshots, 40 MiB of them deleted: well past both thresholds.
    fn fragmented_store(dir: &Path) {
        let store = DocsStore::open(dir).unwrap();
        for i in 0..48 {
            store
                .save_snapshot(&format!("d{i}"), &vec![7; MIB])
                .unwrap();
        }
        for i in 0..40 {
            store.delete_snapshot(&format!("d{i}")).unwrap();
        }
        let conn = store.conn();
        assert!(worth_vacuuming(
            pragma(&conn, "page_size"),
            pragma(&conn, "page_count"),
            pragma(&conn, "freelist_count")
        ));
    }

    #[test]
    fn open_folds_and_truncates_a_bloated_wal() {
        let dir = tempfile::tempdir().unwrap();
        drop(DocsStore::open(dir.path()).unwrap());
        let wal = dir.path().join("docs.sqlite3-wal");
        // A live connection keeps the WAL from being cleaned up on close.
        let other = Connection::open(dir.path().join("docs.sqlite3")).unwrap();
        other.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        for i in 0..8 {
            other
                .execute(
                    "INSERT INTO snapshots(doc_id,bytes,saved_at) VALUES (?1,zeroblob(?2),0)",
                    params![format!("d{i}"), MIB as i64],
                )
                .unwrap();
        }
        assert!(file_len(&wal) > 8 * MIB as u64);

        let store = DocsStore::open(dir.path()).unwrap();
        assert_eq!(file_len(&wal), 0, "boot checkpoint truncates the WAL");
        assert!(store.has_snapshot("d7").unwrap(), "frames folded, not lost");
        assert_eq!(
            pragma(&store.conn(), "journal_size_limit"),
            JOURNAL_SIZE_LIMIT_BYTES
        );
    }

    #[test]
    fn reclaim_vacuums_a_fragmented_database_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("docs.sqlite3");
        fragmented_store(dir.path());
        let before = file_len(&db);

        let store = DocsStore::open(dir.path()).unwrap();
        // Open never pays for the rewrite; the reclaim pass does.
        assert!(pragma(&store.conn(), "freelist_count") > 0);
        store.reclaim_free_space();
        assert_eq!(pragma(&store.conn(), "freelist_count"), 0);
        assert!(file_len(&db) < before / 2, "file shrank");
        assert_eq!(file_len(&dir.path().join("docs.sqlite3-wal")), 0);
        assert_eq!(store.load_snapshot("d47").unwrap().unwrap().len(), MIB);
        assert!(
            !reclaim_free_pages(&store.conn()).unwrap(),
            "a compacted database is not rewritten again"
        );
    }

    #[test]
    fn reclaim_after_a_post_open_shrink_vacuums_without_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("docs.sqlite3");
        // Open-time maintenance finds nothing to do; the shrink happens after.
        let store = DocsStore::open(dir.path()).unwrap();
        store
            .save_snapshot("registry1", &vec![7; 48 * MIB])
            .unwrap();
        store.save_snapshot("registry1", &vec![7; MIB]).unwrap();
        assert!(pragma(&store.conn(), "freelist_count") > 0);
        let before = file_len(&db);

        store.reclaim_free_space();
        assert_eq!(pragma(&store.conn(), "freelist_count"), 0);
        assert!(file_len(&db) < before / 2, "file shrank");
        assert_eq!(file_len(&dir.path().join("docs.sqlite3-wal")), 0);
        assert_eq!(
            store.load_snapshot("registry1").unwrap().unwrap().len(),
            MIB
        );
        // The busy timeout is back at the store's normal wait.
        let busy: i64 = pragma(&store.conn(), "busy_timeout");
        assert_eq!(busy, STORE_BUSY_TIMEOUT.as_millis() as i64);
        // Idempotent and cheap once compact.
        store.reclaim_free_space();
        assert_eq!(pragma(&store.conn(), "freelist_count"), 0);
    }

    #[test]
    fn small_or_proportionally_minor_freelists_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        for i in 0..4 {
            store
                .save_snapshot(&format!("d{i}"), &vec![7; MIB])
                .unwrap();
        }
        for i in 0..3 {
            store.delete_snapshot(&format!("d{i}")).unwrap();
        }
        let freelist = pragma(&store.conn(), "freelist_count");
        assert!(freelist > 0);
        assert!(!reclaim_free_pages(&store.conn()).unwrap(), "below 32 MiB");
        assert_eq!(pragma(&store.conn(), "freelist_count"), freelist);

        let page = 4096;
        let floor = VACUUM_MIN_FREE_BYTES / page;
        assert!(worth_vacuuming(page, floor * 4, floor));
        assert!(!worth_vacuuming(page, floor * 4 + 1, floor), "under 25%");
        assert!(!worth_vacuuming(page, floor, floor - 1), "under the floor");
    }

    #[test]
    fn maintenance_yields_to_a_connection_holding_the_database() {
        // WAL lets VACUUM proceed beside a reader; a held write lock defers it.
        for (lock, vacuumed) in [
            ("BEGIN; SELECT count(*) FROM snapshots;", true),
            ("BEGIN IMMEDIATE;", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fragmented_store(dir.path());
            let other = Connection::open(dir.path().join("docs.sqlite3")).unwrap();
            other.execute_batch(lock).unwrap();

            let started = Instant::now();
            let store = DocsStore::open(dir.path()).expect("maintenance never fails open");
            store.reclaim_free_space();
            // Maintenance never sleeps on a lock, so this measures CPU work
            // only; it fails only if a step waits on the 5 s store timeout.
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{lock}: maintenance waited on the store busy timeout"
            );
            assert_eq!(store.load_snapshot("d47").unwrap().unwrap().len(), MIB);
            assert_eq!(
                pragma(&store.conn(), "freelist_count") == 0,
                vacuumed,
                "{lock}"
            );

            other.execute_batch("COMMIT").unwrap();
            store.save_snapshot("after", b"ok").unwrap();
            assert!(store.has_snapshot("after").unwrap());
            drop(store);
            // A deferred VACUUM simply runs on the next uncontended pass.
            let store = DocsStore::open(dir.path()).unwrap();
            store.reclaim_free_space();
            assert_eq!(pragma(&store.conn(), "freelist_count"), 0, "{lock}");
        }
    }
}
