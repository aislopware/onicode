//! What the engine keeps beside a session (`docs/onicode/decisions/engine.md`, "Ids"): the
//! protocol id a session is known by, and the ledger of processed mutating calls, so a repeated
//! op is answered with its first receipt.
//!
//! The native schema keeps the id in `native_sessions.uid` (unique) and the ledger in `ops`. The
//! desktop schema is never altered, so its sessions' ids and the ledger live in the settings
//! sidecar (`session_uids` and `ops`, sidecar version 2); a `:memory:` store keeps them in memory.
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
        let Some(shared) = &self.shared else {
            let saved: Option<Option<String>> = sql(self
                .conn
                .query_row("SELECT uid FROM native_sessions WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .optional())?;
            match saved {
                None => return Err("Session no longer exists".into()),
                Some(Some(uid)) => return parse_uid(&uid),
                Some(None) => {}
            }
            // `uid IS NULL` keeps a racing writer's uid, which the read below then returns.
            sql(self.conn.execute(
                "UPDATE native_sessions SET uid=?2 WHERE id=?1 AND uid IS NULL",
                params![id, Uuid::now_v7().to_string()],
            ))?;
            let uid: String = sql(self.conn.query_row(
                "SELECT uid FROM native_sessions WHERE id=?1",
                [id],
                |row| row.get(0),
            ))?;
            return parse_uid(&uid);
        };
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
        let Some(shared) = &self.shared else {
            sql(self.conn.execute(
                "UPDATE native_sessions SET uid=?2 WHERE id=?1 AND uid IS NULL",
                params![id, uid.to_string()],
            ))?;
            return self.session_uid(id);
        };
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
        let Some(shared) = &self.shared else {
            return sql(self
                .conn
                .query_row(
                    "SELECT id FROM native_sessions WHERE uid=?1",
                    [uid],
                    |row| row.get(0),
                )
                .optional());
        };
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
        let conn = match &self.shared {
            None => &self.conn,
            Some(shared) => match &shared.extras_db {
                Some(conn) => conn,
                None => {
                    return Ok(shared
                        .ledger
                        .borrow()
                        .ops
                        .get(op)
                        .map(|(_, receipt, _)| receipt.clone()));
                }
            },
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
        let conn = match &self.shared {
            None => &self.conn,
            Some(shared) => match &shared.extras_db {
                Some(conn) => conn,
                None => {
                    shared
                        .ledger
                        .borrow_mut()
                        .ops
                        .entry(op.into())
                        .or_insert_with(|| (session, receipt.into(), at_ms));
                    return Ok(());
                }
            },
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
        let conn = match &self.shared {
            None => &self.conn,
            Some(shared) => match &shared.extras_db {
                Some(conn) => conn,
                None => {
                    let mut ledger = shared.ledger.borrow_mut();
                    let count = ledger.ops.len();
                    ledger.ops.retain(|_, (_, _, at)| *at >= before_ms);
                    return Ok(count - ledger.ops.len());
                }
            },
        };
        sql(conn.execute("DELETE FROM ops WHERE at<?1", [before]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stores(dir: &std::path::Path) -> Vec<Store> {
        vec![
            Store::open(std::path::Path::new(":memory:")).unwrap(),
            Store::open(&dir.join("native.db")).unwrap(),
            Store::open_monocode(std::path::Path::new(":memory:")).unwrap(),
            Store::open_monocode(&dir.join("monocode.db")).unwrap(),
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
        type Open = fn(&std::path::Path) -> Result<Store, String>;
        let opens: [Open; 2] = [Store::open, Store::open_monocode];
        for open in opens {
            let path = dir.path().join(format!("reopen-{}.db", Uuid::now_v7()));
            let store = open(&path).unwrap();
            let session = store.create("codex", "/project").unwrap();
            let uid = store.session_uid(session.id).unwrap();
            drop(store);
            let store = open(&path).unwrap();
            assert_eq!(store.session_uid(session.id).unwrap(), uid);
            assert_eq!(store.session_by_uid(uid).unwrap(), Some(session.id));
        }
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
