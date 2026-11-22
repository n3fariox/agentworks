use crate::{Result, Store};
use agentworks_session::SessionData;
use rusqlite::{OptionalExtension, params};
impl Store {
    pub fn initialize_input_queue(&self) -> rusqlite::Result<()> {
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS input_queue_bindings(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,request_id TEXT NOT NULL,payload TEXT NOT NULL,item_id TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(session_id,request_id));")
    }
    pub fn input_binding(
        &self,
        id: &str,
        request: &str,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.connection.query_row("SELECT payload,item_id FROM input_queue_bindings WHERE session_id=?1 AND request_id=?2",params![id,request],|r|Ok((r.get(0)?,r.get(1)?))).optional()
    }
    /// Bind a request and its resulting session queue atomically. Failed writes
    /// roll back both; the caller restores its in-memory state on error.
    pub fn save_input_queue(
        &mut self,
        id: &str,
        state: &SessionData,
        binding: Option<(&str, &str, &str)>,
        created: i64,
    ) -> Result<()> {
        self.written.remove(id);
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((request, text, item)) = binding {
            tx.execute("INSERT INTO input_queue_bindings(session_id,request_id,payload,item_id,created) VALUES (?1,?2,?3,?4,?5)",params![id,request,text,item,created])?;
        }
        if tx.execute(
            "UPDATE sessions SET state=?2 WHERE id=?1",
            params![id, serde_json::to_string(state)?],
        )? != 1
        {
            return Err("Session no longer exists".into());
        }
        tx.commit()?;
        Ok(())
    }
}

impl Store {
    pub fn input_binding_usage(&self, id: &str) -> rusqlite::Result<(i64, i64)> {
        self.connection.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM input_queue_bindings WHERE session_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))
    }
}
