//! Rewinding a conversation to any of its turns (the protocol's `Rewind`): the rows before the
//! turn's prompt stay, the provider thread goes, and the next prompt starts a new thread that
//! carries the kept turns as a handoff brief.
use rusqlite::params;

use super::Store;
use crate::agent::Handoff;

impl Store {
    /// Drops session `id`'s rows from `start` (a row where a user prompt starts) on, lets its
    /// provider thread go, keeps the turn receipts before `start`, and hands the next prompt the
    /// kept rows as a brief. `Err` when no prompt starts at `start`.
    pub fn rewind_to(&self, id: i64, start: usize) -> Result<(), String> {
        let rows = self.load(id)?.rows;
        if rows.get(start).is_none_or(|row| row.role != "user") {
            return Err("No prompt starts this turn".into());
        }
        let provider = self
            .sessions()?
            .into_iter()
            .find(|session| session.id == id)
            .map(|session| session.provider)
            .ok_or("Session no longer exists")?;
        if self.shared.is_some() {
            self.shared_rewind(id, start)?;
        } else {
            let at = i64::try_from(start)
                .map_err(|_| "Conversation row index exceeds SQLite's integer range".to_owned())?;
            let updated_at = crate::time::now_ms().min(i64::MAX as u64) as i64;
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|error| error.to_string())?;
            self.conn
                .execute(
                    "DELETE FROM native_rows WHERE session_id=?1 AND row_ix>=?2",
                    params![id, at],
                )
                .map_err(|error| error.to_string())?;
            self.conn
                .execute(
                    "UPDATE native_sessions SET provider_id=NULL,updated_at=?2 WHERE id=?1",
                    params![id, updated_at],
                )
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())?;
        }
        let mut settings = self.agent_settings(id)?.unwrap_or_default();
        settings.turns.retain(|&row, _| row < start);
        settings.handoff = (start > 0).then(|| Handoff {
            from: provider,
            brief: crate::response::handoff(&rows[..start]),
        });
        self.save_agent_settings(id, &settings)?;
        self.save_context_usage(id, &Default::default())
    }
}
