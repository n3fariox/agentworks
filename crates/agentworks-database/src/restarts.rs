//! Durable restart requests; process supervision and filesystem release selection are server concerns.
use crate::{Result, Store};
use rusqlite::{OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
impl Store {
    pub fn initialize_restarts(&self) -> rusqlite::Result<()> {
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS service_restarts(actor TEXT NOT NULL,request_id TEXT NOT NULL,payload TEXT NOT NULL,record TEXT NOT NULL,UNIQUE(actor,request_id));")
    }
    pub fn restart_records<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.connection
            .prepare("SELECT record FROM service_restarts ORDER BY rowid DESC")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn save_restart<T: Serialize>(
        &self,
        actor: &str,
        request: &str,
        payload: &str,
        record: &T,
    ) -> Result<()> {
        self.connection.execute("INSERT INTO service_restarts(actor,request_id,payload,record) VALUES (?1,?2,?3,?4) ON CONFLICT(actor,request_id) DO UPDATE SET record=excluded.record",params![actor,request,payload,serde_json::to_string(record)?])?;
        Ok(())
    }
    pub fn restart_request(
        &self,
        actor: &str,
        request: &str,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.connection
            .query_row(
                "SELECT payload,record FROM service_restarts WHERE actor=?1 AND request_id=?2",
                params![actor, request],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }
}
