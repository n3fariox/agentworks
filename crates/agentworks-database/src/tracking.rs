//! Durable PR baselines and observation journal; snapshot interpretation is caller policy.
use crate::{Store, inbox::Transaction};
use rusqlite::{Connection, OptionalExtension, params};

pub struct Tracking<'a>(&'a Connection);
pub type Baseline = (Option<String>, String, String, String);
pub struct BaselineWrite<'a> {
    pub session: &'a str,
    pub scope: Option<&'a str>,
    pub snapshot: &'a str,
    pub status: &'a str,
    pub observed_at: i64,
    pub next_poll: i64,
    pub hooks: &'a str,
}
impl Transaction<'_> {
    pub fn tracking(&self) -> Tracking<'_> {
        Tracking(&self.0)
    }
}
impl Tracking<'_> {
    pub fn baseline(&self, id: &str) -> rusqlite::Result<Option<Baseline>> {
        self.0
            .query_row(
                "SELECT scope,snapshot,status,hooks FROM github_pr_tracking WHERE session_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
    }
    pub fn append_event(&self, id: &str, payload: &str, at: i64) -> rusqlite::Result<i64> {
        self.0.execute(
            "INSERT INTO github_pr_events(session_id,payload,created) VALUES (?1,?2,?3)",
            params![id, payload, at],
        )?;
        Ok(self.0.last_insert_rowid())
    }
    pub fn save_baseline(&self, b: &BaselineWrite<'_>) -> rusqlite::Result<()> {
        self.0.execute("INSERT INTO github_pr_tracking(session_id,scope,snapshot,status,observed_at,next_poll,hooks) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(session_id) DO UPDATE SET scope=excluded.scope,snapshot=excluded.snapshot,status=excluded.status,observed_at=excluded.observed_at,next_poll=excluded.next_poll,hooks=excluded.hooks",params![b.session,b.scope,b.snapshot,b.status,b.observed_at,b.next_poll,b.hooks])?;
        Ok(())
    }
}
impl Store {
    pub fn script_versions_by_name(&self) -> rusqlite::Result<Vec<(String, String)>> {
        self.connection
            .prepare("SELECT name,hash FROM scripts WHERE enabled=1 ORDER BY name")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }
    pub fn pending_pr_events(&self, id: &str) -> rusqlite::Result<Vec<(i64, String)>> {
        self.connection.prepare("SELECT id,payload FROM github_pr_events WHERE session_id=?1 AND delivered=0 ORDER BY id LIMIT 100")?.query_map([id],|r|Ok((r.get(0)?,r.get(1)?)))?.collect()
    }
    pub fn acknowledge_pr_event(&self, id: i64) -> rusqlite::Result<usize> {
        self.connection
            .execute("UPDATE github_pr_events SET delivered=1 WHERE id=?1", [id])
    }
    pub fn prune_pr_events(&self, id: &str) -> rusqlite::Result<usize> {
        self.connection.execute("DELETE FROM github_pr_events WHERE session_id=?1 AND delivered=1 AND id < (SELECT COALESCE(min(id),0) FROM (SELECT id FROM github_pr_events WHERE session_id=?1 ORDER BY id DESC LIMIT 100))",[id])
    }
    pub fn pr_scope(&self, id: &str) -> rusqlite::Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT scope FROM github_pr_tracking WHERE session_id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn pr_events(&self, id: &str, after: i64) -> rusqlite::Result<Vec<(i64, String, i64)>> {
        self.connection.prepare("SELECT id,payload,created FROM github_pr_events WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT 100")?.query_map(params![id,after],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect()
    }
    pub fn pr_due(&self, id: &str, now: i64) -> rusqlite::Result<bool> {
        Ok(self
            .connection
            .query_row(
                "SELECT next_poll<=?2 FROM github_pr_tracking WHERE session_id=?1",
                params![id, now],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(true))
    }
    pub fn delay_pr(&self, id: &str, next: i64) -> rusqlite::Result<usize> {
        self.connection.execute("INSERT INTO github_pr_tracking(session_id,next_poll) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET next_poll=excluded.next_poll",params![id,next])
    }
    pub fn invalidate_host_tracking(&self, host: &str) -> rusqlite::Result<usize> {
        self.connection.execute("UPDATE github_pr_tracking SET next_poll=0 WHERE json_extract(scope,'$[0]')=?1 OR scope IS NULL",[host])
    }
    pub fn tracked_sessions_for_pr(
        &self,
        host: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
        self.connection
            .prepare(
                "SELECT session_id,observed_at FROM github_pr_tracking
                 WHERE lower(json_extract(scope,'$[0]'))=lower(?1)
                   AND lower(json_extract(scope,'$[1]'))=lower(?2)
                   AND lower(json_extract(scope,'$[2]'))=lower(?3)
                   AND json_extract(snapshot,?4) IS NOT NULL",
            )?
            .query_map(params![host, owner, repo, format!("$.\"{number}\"")], |r| {
                Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or_default()))
            })?
            .collect()
    }
}
