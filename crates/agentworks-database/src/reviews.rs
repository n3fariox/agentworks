//! Automation review occurrence and action ledgers. Scripts and dispatch stay outside.
use crate::{Result, Store, inbox::Transaction};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
pub struct Reviews<'a>(&'a Connection);
impl Transaction<'_> {
    pub fn reviews(&self) -> Reviews<'_> {
        Reviews(&self.0)
    }
}
impl Reviews<'_> {
    pub fn baseline(&self, id: &str, trigger: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.0.query_row("SELECT scope,snapshot FROM automation_trigger_baselines WHERE automation_id=?1 AND trigger_name=?2",params![id,trigger],|r|Ok((r.get(0)?,r.get(1)?))).optional()
    }
    pub fn append_event(
        &self,
        id: &str,
        automation: &str,
        revision: u64,
        payload: &str,
        at: u64,
    ) -> rusqlite::Result<usize> {
        self.0.execute("INSERT INTO automation_events(id,automation_id,revision,payload,created) VALUES (?1,?2,?3,?4,?5)",params![id,automation,revision as i64,payload,at as i64])
    }
    pub fn save_baseline(
        &self,
        id: &str,
        trigger: &str,
        scope: &str,
        snapshot: &str,
    ) -> rusqlite::Result<usize> {
        self.0.execute("INSERT INTO automation_trigger_baselines(automation_id,trigger_name,scope,snapshot) VALUES (?1,?2,?3,?4) ON CONFLICT(automation_id,trigger_name) DO UPDATE SET scope=excluded.scope,snapshot=excluded.snapshot",params![id,trigger,scope,snapshot])
    }
}
impl Store {
    pub fn record_review_poll(
        &self,
        id: &str,
        at: i64,
        error: Option<&str>,
    ) -> rusqlite::Result<usize> {
        self.connection.execute("INSERT INTO automation_github_polls(automation_id,checked_at,error) VALUES (?1,?2,?3) ON CONFLICT(automation_id) DO UPDATE SET checked_at=excluded.checked_at,error=excluded.error",params![id,at,error])
    }
    pub fn pending_review_events(&self, id: &str, revision: u64) -> Result<Vec<(String, Value)>> {
        self.connection.prepare("SELECT id,payload FROM automation_events WHERE automation_id=?1 AND revision=?2 AND status='pending' ORDER BY created,id LIMIT 100")?.query_map(params![id,revision as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?
            .map(|r|{let(id,payload)=r?;Ok((id,serde_json::from_str(&payload)?))}).collect()
    }
    pub fn finish_review_event(
        &mut self,
        id: &str,
        status: &str,
        detail: &str,
        target: Option<&str>,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute("UPDATE automation_events SET status=?2,detail=?3,target=COALESCE(?4,target) WHERE id=?1 AND status='pending'",params![id,status,detail,target])?;
        if status == "skipped" {
            tx.execute("UPDATE automation_event_actions SET status='skipped',detail=?2 WHERE event_id=?1 AND status='pending'",params![id,detail])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn prepare_review_actions(
        &mut self,
        id: &str,
        actions: &[(String, String)],
        decision: &str,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        for (key, payload) in actions {
            tx.execute("INSERT INTO automation_event_actions(event_id,action_key,payload) VALUES (?1,?2,?3)",params![id,key,payload])?;
        }
        tx.execute(
            "UPDATE automation_events SET action=?2 WHERE id=?1",
            params![id, decision],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn review_prepared(&self, id: &str) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT action IS NOT NULL FROM automation_events WHERE id=?1",
            [id],
            |r| r.get(0),
        )
    }
    pub fn review_actions<T: DeserializeOwned>(
        &self,
        id: &str,
    ) -> Result<Vec<(String, T, String, Option<String>)>> {
        self.connection.prepare("SELECT action_key,payload,status,target FROM automation_event_actions WHERE event_id=?1 ORDER BY rowid")?.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?)))?
            .map(|r|{let(k,p,s,t)=r?;Ok((k,serde_json::from_str(&p)?,s,t))}).collect()
    }
    pub fn complete_review_action(
        &self,
        id: &str,
        key: &str,
        target: &str,
    ) -> rusqlite::Result<usize> {
        self.connection.execute("UPDATE automation_event_actions SET status='completed',target=?3 WHERE event_id=?1 AND action_key=?2",params![id,key,target])
    }
    pub fn review_retry_detail(&self, id: &str, detail: &str) -> rusqlite::Result<usize> {
        self.connection.execute(
            "UPDATE automation_events SET detail=?2 WHERE id=?1",
            params![id, detail],
        )
    }
    pub fn review_poll_status(&self, id: &str) -> rusqlite::Result<Option<Value>> {
        self.connection.query_row("SELECT checked_at,error FROM automation_github_polls WHERE automation_id=?1",[id],|r|Ok(json!({"checkedAt":r.get::<_,i64>(0)?,"error":r.get::<_,Option<String>>(1)?}))).optional()
    }
}
