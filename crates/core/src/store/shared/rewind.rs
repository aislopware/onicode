//! Rewinding a desktop-schema conversation to one of its turns: the source blocks before the
//! prompt that starts at a row index are kept, and the provider thread is let go.
use super::*;

impl Store {
    /// Keeps the source blocks before the prompt at row `start`; with `let_thread_go`, the
    /// provider thread goes too.
    pub(in crate::store) fn shared_cut(
        &self,
        id: i64,
        start: usize,
        let_thread_go: bool,
    ) -> Result<(), String> {
        self.shared_writable()?;
        let tx = sql(self.conn.unchecked_transaction())?;
        let blocks = self.shared_blocks(id)?;
        let mut offset = 0;
        let mut cut = None;
        for (index, block) in blocks.iter().enumerate() {
            if offset == start && block["role"] == "user" && block["internal"] != true {
                cut = Some(index);
                break;
            }
            offset += blocks::project(std::slice::from_ref(block))?.len();
        }
        let cut = cut.ok_or("No prompt starts this turn any more")?;
        let next = &blocks[..cut];
        self.shared_store_blocks(id, next)?;
        let mut fields =
            vec![
                ("updated_at", json!(crate::time::now_ms())),
                (
                    "has_user_message",
                    json!(next.iter().any(|b| b["role"] == "user"
                        && b["internal"] != true
                        && b["draft"] != true)),
                ),
            ];
        if let_thread_go {
            fields.push(("provider_session_id", Value::Null));
        }
        self.shared_patch(id, &fields)?;
        sql(tx.commit())?;
        if let Some(shared) = self.shared.as_ref() {
            shared
                .snapshots
                .borrow_mut()
                .insert(id, HistoryRevision::of(next));
        }
        Ok(())
    }
}
