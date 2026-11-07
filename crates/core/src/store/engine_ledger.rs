//! What the engine keeps beside a session (`docs/onicode/decisions/engine.md`, "Ids"): the
//! protocol id a session is known by, and the ledger of processed mutating calls, so a repeated
//! op is answered with its first receipt.
//!
//! The sessions schema is never altered, so the ids and the ledger live in the settings sidecar
//! (`session_uids` and `ops`, sidecar version 2); a `:memory:` store keeps them in memory.
use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::Store;

/// Session uids and processed ops of a store without a sidecar.
#[derive(Default)]
pub(super) struct MemoryLedger {
    /// Persisted session id to uid.
    uids: HashMap<String, Uuid>,
    /// Op to its session, receipt and when it was recorded (Unix ms).
    ops: HashMap<String, (Uuid, String, u64)>,
    /// The outbox, oldest first (`outbox.rs`).
    pub(super) outbox: Vec<super::outbox::OutboxEntry>,
}

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

fn parse_uid(text: &str) -> Result<Uuid, String> {
    Uuid::parse_str(text).map_err(|e| format!("Bad session uid {text}: {e}"))
}

impl Store {
    /// The session's protocol id (`SessionId`), minted (UUIDv7) the first time it is asked for
    /// and kept from then on.
    pub fn session_uid(&self, id: i64) -> Result<Uuid, String> {
        let shared = &self.shared;
        let session = self.persisted_session_id(id)?;
        let Some(conn) = &shared.extras_db else {
            let mut ledger = shared.ledger.borrow_mut();
            return Ok(*ledger.uids.entry(session).or_insert_with(Uuid::now_v7));
        };
        sql(conn.execute(
            "INSERT OR IGNORE INTO session_uids(session,uid) VALUES(?1,?2)",
            params![session, Uuid::now_v7().to_string()],
        ))?;
        let uid: String = sql(conn.query_row(
            "SELECT uid FROM session_uids WHERE session=?1",
            [session],
            |row| row.get(0),
        ))?;
        parse_uid(&uid)
    }

    /// Gives a session without a uid the one its creator minted (`CreateSession`), and returns
    /// the uid the session holds: an earlier uid stands.
    pub fn adopt_session_uid(&self, id: i64, uid: Uuid) -> Result<Uuid, String> {
        let shared = &self.shared;
        let session = self.persisted_session_id(id)?;
        match &shared.extras_db {
            Some(conn) => {
                sql(conn.execute(
                    "INSERT OR IGNORE INTO session_uids(session,uid) VALUES(?1,?2)",
                    params![session, uid.to_string()],
                ))?;
            }
            None => {
                shared
                    .ledger
                    .borrow_mut()
                    .uids
                    .entry(session)
                    .or_insert(uid);
            }
        }
        self.session_uid(id)
    }

    /// The session known by `uid`, if it still exists.
    pub fn session_by_uid(&self, uid: Uuid) -> Result<Option<i64>, String> {
        let uid = uid.to_string();
        let shared = &self.shared;
        let session = match &shared.extras_db {
            Some(conn) => sql(conn
                .query_row(
                    "SELECT session FROM session_uids WHERE uid=?1",
                    [uid],
                    |row| row.get::<_, String>(0),
                )
                .optional())?,
            None => shared
                .ledger
                .borrow()
                .uids
                .iter()
                .find(|(_, saved)| saved.to_string() == uid)
                .map(|(session, _)| session.clone()),
        };
        Ok(session.and_then(|session| self.resolve_existing_session_id(&session)))
    }

    /// The receipt first recorded for `op`, if it was processed.
    pub fn op_receipt(&self, op: &str) -> Result<Option<String>, String> {
        let Some(conn) = &self.shared.extras_db else {
            return Ok(self
                .shared
                .ledger
                .borrow()
                .ops
                .get(op)
                .map(|(_, receipt, _)| receipt.clone()));
        };
        sql(conn
            .query_row("SELECT receipt FROM ops WHERE op=?1", [op], |row| {
                row.get(0)
            })
            .optional())
    }

    /// Records `op`'s receipt, unless it already has one: the first receipt stands.
    pub fn record_op(
        &self,
        op: &str,
        session: Uuid,
        receipt: &str,
        at_ms: u64,
    ) -> Result<(), String> {
        let at = at_ms.min(i64::MAX as u64) as i64;
        let Some(conn) = &self.shared.extras_db else {
            self.shared
                .ledger
                .borrow_mut()
                .ops
                .entry(op.into())
                .or_insert_with(|| (session, receipt.into(), at_ms));
            return Ok(());
        };
        sql(conn.execute(
            "INSERT OR IGNORE INTO ops(op,session,receipt,at) VALUES(?1,?2,?3,?4)",
            params![op, session.to_string(), receipt, at],
        ))?;
        Ok(())
    }

    /// Forgets the ops recorded before `before_ms`; returns how many.
    pub fn prune_ops(&self, before_ms: u64) -> Result<usize, String> {
        let before = before_ms.min(i64::MAX as u64) as i64;
        let Some(conn) = &self.shared.extras_db else {
            let mut ledger = self.shared.ledger.borrow_mut();
            let count = ledger.ops.len();
            ledger.ops.retain(|_, (_, _, at)| *at >= before_ms);
            return Ok(count - ledger.ops.len());
        };
        sql(conn.execute("DELETE FROM ops WHERE at<?1", [before]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores(dir: &std::path::Path) -> Vec<Store> {
        vec![
            Store::in_memory().unwrap(),
            Store::open(&dir.join("monocode.db")).unwrap(),
        ]
    }

    #[test]
    fn a_session_keeps_one_unique_uid_that_finds_it_again() {
        let dir = tempfile::tempdir().unwrap();
        for store in stores(dir.path()) {
            let first = store.create("codex", "/project").unwrap();
            let second = store.create("codex", "/project").unwrap();
            let uid = store.session_uid(first.id).unwrap();
            assert_eq!(uid.get_version_num(), 7);
            assert_eq!(store.session_uid(first.id).unwrap(), uid);
            assert_ne!(store.session_uid(second.id).unwrap(), uid);
            assert_eq!(store.session_by_uid(uid).unwrap(), Some(first.id));
            assert_eq!(store.session_by_uid(Uuid::now_v7()).unwrap(), None);
        }
    }

    #[test]
    fn a_created_session_takes_its_creators_uid_once() {
        let dir = tempfile::tempdir().unwrap();
        for store in stores(dir.path()) {
            let session = store.create("codex", "/project").unwrap();
            let minted = Uuid::now_v7();
            assert_eq!(store.adopt_session_uid(session.id, minted).unwrap(), minted);
            assert_eq!(
                store.adopt_session_uid(session.id, Uuid::now_v7()).unwrap(),
                minted
            );
            assert_eq!(store.session_by_uid(minted).unwrap(), Some(session.id));
        }
    }

    #[test]
    fn uids_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monocode.db");
        let store = Store::open(&path).unwrap();
        let session = store.create("codex", "/project").unwrap();
        let uid = store.session_uid(session.id).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.session_uid(session.id).unwrap(), uid);
        assert_eq!(store.session_by_uid(uid).unwrap(), Some(session.id));
    }

    #[test]
    fn the_first_receipt_stands_until_pruned() {
        let dir = tempfile::tempdir().unwrap();
        for store in stores(dir.path()) {
            let session = Uuid::now_v7();
            assert_eq!(store.op_receipt("op").unwrap(), None);
            store
                .record_op("op", session, "\"Applied\"", 1_000)
                .unwrap();
            store.record_op("op", session, "\"Later\"", 2_000).unwrap();
            store
                .record_op("newer", session, "\"Applied\"", 9_000)
                .unwrap();
            assert_eq!(
                store.op_receipt("op").unwrap().as_deref(),
                Some("\"Applied\"")
            );
            assert_eq!(store.prune_ops(5_000).unwrap(), 1);
            assert_eq!(store.op_receipt("op").unwrap(), None);
            assert!(store.op_receipt("newer").unwrap().is_some());
        }
    }
}
