use crate::{Result, Store};
use rusqlite::{OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
impl Store {
    /// Unreadable custom roles are omitted, as in the application before extraction.
    pub fn custom_roles<T: DeserializeOwned>(&self) -> rusqlite::Result<Vec<T>> {
        let mut q = self.connection.prepare("SELECT definition FROM roles")?;
        Ok(q.query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok().and_then(|s| serde_json::from_str(&s).ok()))
            .collect())
    }
    pub fn custom_role<T: DeserializeOwned>(&self, id: &str) -> rusqlite::Result<Option<T>> {
        Ok(self
            .connection
            .query_row("SELECT definition FROM roles WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|s| serde_json::from_str(&s).ok()))
    }
    pub fn save_role<T: Serialize>(&self, id: &str, definition: &T) -> Result<()> {
        self.connection.execute("INSERT INTO roles(id,definition) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition", params![id, serde_json::to_string(definition)?])?;
        Ok(())
    }
    pub fn delete_role(&self, id: &str) -> rusqlite::Result<usize> {
        self.connection
            .execute("DELETE FROM roles WHERE id=?1", [id])
    }
}
