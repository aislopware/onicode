//! Which automation created a session.
use std::collections::HashMap;

use serde_json::Value;

use super::Store;

impl Store {
    pub fn set_automation_id(&self, id: i64, automation: &str) -> Result<(), String> {
        self.shared_patch(id, &[("automation_id", Value::String(automation.into()))])
    }

    pub fn automation_session_ids(&self) -> Result<HashMap<i64, String>, String> {
        let field = self.shared_field("automation_id", "NULL");
        let mut query = self
            .conn
            .prepare(&format!(
                "SELECT id,{field} FROM sessions WHERE {field} IS NOT NULL"
            ))
            .map_err(|error| error.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?;
        rows.map(|row| {
            let (id, automation) = row.map_err(|error| error.to_string())?;
            Ok((self.shared_register(&id)?, automation))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_metadata_uses_the_original_desktop_session_identity() {
        let store = Store::in_memory().unwrap();
        let session = store.create("codex", "/project").unwrap();
        store
            .set_automation_id(session.id, "automation-one")
            .unwrap();
        assert_eq!(
            store
                .automation_session_ids()
                .unwrap()
                .get(&session.id)
                .map(String::as_str),
            Some("automation-one")
        );
        let persisted = store.persisted_session_id(session.id).unwrap();
        let raw: String = store
            .conn
            .query_row(
                "SELECT automation_id FROM sessions WHERE id=?1",
                [persisted],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw, "automation-one");
    }
}
