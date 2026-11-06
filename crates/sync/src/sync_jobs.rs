//! Durable sync jobs: a per-doc marker of work owed, versioned so completing an old job never
//! erases one scheduled while the work ran. Discovery reads metadata only; no doc is opened until
//! the engine admits a bounded page.
use rusqlite::{OptionalExtension, params};

use crate::store::{DocsStore, StoreError, store_blocking};

/// The job kind marking a chat that holds ops its host has not acked: the local outbox of a
/// viewer, or what a mailbox engine took in for a host that was away.
pub const FORWARD: &str = "forward";

/// Schedules `kind` for `doc` inside an open transaction, superseding an older one.
pub(crate) fn schedule_in(
    tx: &rusqlite::Transaction<'_>,
    doc: &str,
    kind: &str,
) -> Result<(), StoreError> {
    tx.execute("UPDATE sync_job_clock SET value=value+1 WHERE id=1", [])?;
    tx.execute(
        "INSERT INTO chat_sync_jobs(doc_id,kind,version) SELECT ?1,?2,value FROM sync_job_clock WHERE id=1
         ON CONFLICT(doc_id,kind) DO UPDATE SET version=excluded.version",
        params![doc, kind],
    )?;
    Ok(())
}

impl DocsStore {
    /// Unacked outbox rows and scheduled jobs; counts only, payloads stay on disk.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn sync_work_counts(&self) -> Result<(u64, u64), StoreError> {
        store_blocking(|| {
            Ok(self.conn().query_row(
                "SELECT (SELECT count(*) FROM chat_outbox), (SELECT count(*) FROM chat_sync_jobs)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
    }

    /// Schedules `kind` for `doc`; a job already there takes the new version.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn schedule_sync_job(&self, doc: &str, kind: &str) -> Result<(), StoreError> {
        store_blocking(|| {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            schedule_in(&tx, doc, kind)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// The version of `doc`'s `kind` job, if one is scheduled.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn sync_job_version(&self, doc: &str, kind: &str) -> Result<Option<i64>, StoreError> {
        store_blocking(|| {
            Ok(self
                .conn()
                .query_row(
                    "SELECT version FROM chat_sync_jobs WHERE doc_id=?1 AND kind=?2",
                    params![doc, kind],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    /// Completes the job only if it is still at `version`.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn complete_sync_job(&self, doc: &str, kind: &str, version: i64) -> Result<(), StoreError> {
        store_blocking(|| {
            self.conn().execute(
                "DELETE FROM chat_sync_jobs WHERE doc_id=?1 AND kind=?2 AND version=?3",
                params![doc, kind, version],
            )?;
            Ok(())
        })
    }

    /// Up to `limit` (at most 64) docs with a `kind` job, after `after` in id order.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn pending_sync_jobs(
        &self,
        kind: &str,
        after: &str,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        store_blocking(|| {
            let conn = self.conn();
            let mut q = conn.prepare("SELECT doc_id FROM chat_sync_jobs WHERE kind=?1 AND doc_id>?2 ORDER BY doc_id LIMIT ?3")?;
            Ok(
                q.query_map(params![kind, after, page(limit)], |r| r.get(0))?
                    .collect::<Result<_, _>>()?,
            )
        })
    }

    /// Up to `limit` (at most 64) docs owing their host something, after `after` in id order:
    /// unacked outbox rows or a [`FORWARD`] job.
    ///
    /// # Errors
    ///
    /// SQLite's.
    pub fn pending_forwards(&self, after: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        store_blocking(|| {
            let conn = self.conn();
            let mut q = conn.prepare(
                "SELECT doc_id FROM (SELECT doc_id FROM chat_outbox
                  UNION SELECT doc_id FROM chat_sync_jobs WHERE kind=?1)
                 WHERE doc_id>?2 ORDER BY doc_id LIMIT ?3",
            )?;
            Ok(
                q.query_map(params![FORWARD, after, page(limit)], |r| r.get(0))?
                    .collect::<Result<_, _>>()?,
            )
        })
    }
}

fn page(limit: usize) -> i64 {
    i64::try_from(limit.min(64)).unwrap_or(64)
}

#[cfg(test)]
mod tests {
    use loro::{ID, VersionVector};

    use super::*;

    #[test]
    fn forwards_page_without_duplicates_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DocsStore::open(dir.path()).unwrap();
            let mut end = VersionVector::new();
            end.set_end(ID::new(1, 1));
            for n in 0..80 {
                let id = format!("chat-{n:03}");
                store.schedule_sync_job(&id, FORWARD).unwrap();
                store.enqueue_outbox(&id, b"bytes", &end).unwrap();
            }
            store.enqueue_outbox("chat-200", b"bytes", &end).unwrap();
        }
        let store = DocsStore::open(dir.path()).unwrap();
        let mut after = String::new();
        let mut seen = Vec::new();
        loop {
            let page = store.pending_forwards(&after, 100).unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 64);
            after = page.last().unwrap().clone();
            seen.extend(page);
        }
        assert_eq!(seen.len(), 81);
        store.delete_snapshot("chat-000").unwrap();
        assert_eq!(store.pending_forwards("", 1).unwrap(), ["chat-001"]);
    }

    #[test]
    fn completing_an_old_version_keeps_a_newer_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocsStore::open(dir.path()).unwrap();
        store.schedule_sync_job("chat", FORWARD).unwrap();
        let old = store.sync_job_version("chat", FORWARD).unwrap().unwrap();
        store.schedule_sync_job("chat", FORWARD).unwrap();
        store.complete_sync_job("chat", FORWARD, old).unwrap();
        let newer = store.sync_job_version("chat", FORWARD).unwrap().unwrap();
        assert!(newer > old);
        assert_eq!(store.pending_sync_jobs(FORWARD, "", 8).unwrap(), ["chat"]);
        store.complete_sync_job("chat", FORWARD, newer).unwrap();
        assert_eq!(store.sync_job_version("chat", FORWARD).unwrap(), None);
    }
}
