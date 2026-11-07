//! The forwarding engine's outbox (`docs/onicode/decisions/engine.md`, "Authority"): mutating
//! calls for an engine that was offline when they were made, kept until that engine has answered
//! them, so each is delivered once it returns, in the order they were made, across restarts of
//! either engine. The host's ledger answers a call delivered twice (a reply lost with its link)
//! with its first receipt, so a delivery that cannot tell whether it landed simply goes again.
//!
//! The settings sidecar keeps the entries in its `outbox` (sidecar version 3); a `:memory:` store
//! keeps them in memory.
use rusqlite::params;

use super::Store;

/// One call waiting for its engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEntry {
    /// The call's `OpId`, as the viewer minted it: one entry per op.
    pub op: String,
    /// The engine it is for.
    pub engine: String,
    /// The envelope's method.
    pub method: String,
    /// The routed params, as JSON, as the host is to receive them.
    pub params: String,
    /// When it was put in (Unix ms).
    pub at: u64,
}

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, String> {
    result.map_err(|e| e.to_string())
}

/// The connection that holds the outbox table, or `None` for a store in memory.
fn table(store: &Store) -> Option<&rusqlite::Connection> {
    store.shared.extras_db.as_ref()
}

impl Store {
    /// Puts `entry` in the outbox unless its op is there already; `true` when it was added. The
    /// first entry of an op stands, as the host's first receipt does.
    pub fn outbox_put(&self, entry: &OutboxEntry) -> Result<bool, String> {
        let Some(conn) = table(self) else {
            let mut ledger = self.shared.ledger.borrow_mut();
            if ledger.outbox.iter().any(|held| held.op == entry.op) {
                return Ok(false);
            }
            ledger.outbox.push(entry.clone());
            return Ok(true);
        };
        let at = entry.at.min(i64::MAX as u64) as i64;
        let added = sql(conn.execute(
            "INSERT OR IGNORE INTO outbox(op,engine,method,params,at) VALUES(?1,?2,?3,?4,?5)",
            params![entry.op, entry.engine, entry.method, entry.params, at],
        ))?;
        Ok(added > 0)
    }

    /// The entries waiting for `engine`, oldest first.
    pub fn outbox_for(&self, engine: &str) -> Result<Vec<OutboxEntry>, String> {
        let Some(conn) = table(self) else {
            return Ok(self
                .shared
                .ledger
                .borrow()
                .outbox
                .iter()
                .filter(|entry| entry.engine == engine)
                .cloned()
                .collect());
        };
        let mut query = sql(conn
            .prepare("SELECT op,engine,method,params,at FROM outbox WHERE engine=?1 ORDER BY n"))?;
        let rows = sql(query.query_map([engine], |row| {
            Ok(OutboxEntry {
                op: row.get(0)?,
                engine: row.get(1)?,
                method: row.get(2)?,
                params: row.get(3)?,
                at: u64::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
            })
        }))?;
        sql(rows.collect())
    }

    /// Every engine some entry waits for.
    pub fn outbox_engines(&self) -> Result<Vec<String>, String> {
        let Some(conn) = table(self) else {
            let mut engines: Vec<String> = self
                .shared
                .ledger
                .borrow()
                .outbox
                .iter()
                .map(|entry| entry.engine.clone())
                .collect();
            engines.sort();
            engines.dedup();
            return Ok(engines);
        };
        let mut query = sql(conn.prepare("SELECT DISTINCT engine FROM outbox ORDER BY engine"))?;
        let rows = sql(query.query_map([], |row| row.get(0)))?;
        sql(rows.collect())
    }

    /// Whether `op` waits in the outbox.
    pub fn outbox_holds(&self, op: &str) -> Result<bool, String> {
        let Some(conn) = table(self) else {
            return Ok(self
                .shared
                .ledger
                .borrow()
                .outbox
                .iter()
                .any(|entry| entry.op == op));
        };
        let count: i64 = sql(conn.query_row(
            "SELECT COUNT(*) FROM outbox WHERE op=?1",
            [op],
            |row| row.get(0),
        ))?;
        Ok(count > 0)
    }

    /// Takes `op` out of the outbox: its engine answered it.
    pub fn outbox_remove(&self, op: &str) -> Result<(), String> {
        let Some(conn) = table(self) else {
            self.shared
                .ledger
                .borrow_mut()
                .outbox
                .retain(|entry| entry.op != op);
            return Ok(());
        };
        sql(conn.execute("DELETE FROM outbox WHERE op=?1", [op]))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(op: &str, engine: &str, at: u64) -> OutboxEntry {
        OutboxEntry {
            op: op.into(),
            engine: engine.into(),
            method: "Rename".into(),
            params: format!("{{\"op\":\"{op}\"}}"),
            at,
        }
    }

    /// A store keeps one entry per op, in the order they were put in, per engine, and
    /// forgets one once removed; a file store keeps them across a reopen.
    #[test]
    fn the_outbox_keeps_one_entry_per_op_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let stores = [
            Store::in_memory().unwrap(),
            Store::open(&dir.path().join("monocode.db")).unwrap(),
        ];
        for store in &stores {
            assert!(store.outbox_put(&entry("b", "mac", 2)).unwrap());
            assert!(store.outbox_put(&entry("a", "mac", 1)).unwrap());
            assert!(store.outbox_put(&entry("c", "vps", 3)).unwrap());
            assert!(
                !store.outbox_put(&entry("a", "mac", 9)).unwrap(),
                "first entry stands"
            );
            let ops: Vec<_> = store
                .outbox_for("mac")
                .unwrap()
                .into_iter()
                .map(|entry| entry.op)
                .collect();
            assert_eq!(ops, ["b", "a"], "in the order put in");
            assert_eq!(store.outbox_engines().unwrap(), ["mac", "vps"]);
            assert!(store.outbox_holds("a").unwrap());
            store.outbox_remove("b").unwrap();
            assert!(!store.outbox_holds("b").unwrap());
            assert_eq!(store.outbox_for("mac").unwrap(), [entry("a", "mac", 1)]);
        }
        drop(stores);
        let reopened = Store::open(&dir.path().join("monocode.db")).unwrap();
        assert_eq!(reopened.outbox_for("mac").unwrap(), [entry("a", "mac", 1)]);
        assert_eq!(reopened.outbox_for("vps").unwrap(), [entry("c", "vps", 3)]);
    }
}
