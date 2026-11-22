use crate::{Result, Store};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
/// A live (unrevoked) controller, resolved freshly for every bridge request.
#[derive(Clone)]
pub struct Controller {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub access: crate::Access,
}

impl Store {
    pub fn controller_by_digest(&self, digest: &str, now: i64) -> Result<Option<Controller>> {
        let row: Option<(String, String, String, String)> = self
            .connection
            .query_row(
                "SELECT id,name,project_id,grants FROM agent_controllers WHERE token_hash=?1 AND revoked IS NULL",
                [digest],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            ?;
        let Some((id, name, project_id, grants)) = row else {
            return Ok(None);
        };
        self.connection.execute(
            "UPDATE agent_controllers SET last_used=?2 WHERE id=?1",
            params![id, now],
        )?;
        Ok(Some(Controller {
            id,
            name,
            project_id,
            access: serde_json::from_str(&grants)?,
        }))
    }
    /// Live controller lookup by ID; revoked controllers are absent.
    pub fn controller(&self, id: &str) -> Result<Option<Controller>> {
        self.connection
            .query_row(
                "SELECT id,name,project_id,grants FROM agent_controllers WHERE id=?1 AND revoked IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)),
            )
            .optional()
            ?
            .map(|(id, name, project_id, grants)| {
                Ok(Controller {
                    id,
                    name,
                    project_id,
                    access: serde_json::from_str(&grants)?,
                })
            })
            .transpose()
    }
    pub fn live_controllers(&self, project: &str) -> Result<Vec<Controller>> {
        let mut q = self
            .connection
            .prepare("SELECT id FROM agent_controllers WHERE project_id=?1 AND revoked IS NULL ORDER BY created")
            ?;
        let ids = q
            .query_map([project], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .filter_map(|id| self.controller(id).transpose())
            .collect()
    }
    pub fn controller_rows(&self, project: &str) -> Result<Vec<Value>> {
        let mut q=self.connection.prepare("SELECT id,name,project_id,grants,created,revoked,last_used FROM agent_controllers WHERE project_id=?1 ORDER BY created DESC LIMIT 200")?;
        Ok(q.query_map([project], |r| {
            let id: String = r.get(0)?;
            Ok(json!({"id":id,"name":r.get::<_,String>(1)?,"projectId":r.get::<_,String>(2)?,
                "access":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null),"created":r.get::<_,i64>(4)?,"revoked":r.get::<_,Option<i64>>(5)?,"lastUsed":r.get::<_,Option<i64>>(6)?}))
        })
        ?
         .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn revoke_project_controllers(&self, project: &str, now: i64) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) SELECT 'human','controller_revoked','controller:'||id,'project deleted',?2 FROM agent_controllers WHERE project_id=?1 AND revoked IS NULL",params![project,now])?;
        tx.execute(
            "UPDATE agent_controllers SET revoked=?2 WHERE project_id=?1 AND revoked IS NULL",
            params![project, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Revocation {
    Missing,
    AlreadyRevoked,
    Revoked,
}
impl Store {
    pub fn create_controller(
        &mut self,
        id: &str,
        name: &str,
        project: &str,
        grants: &str,
        digest: &str,
        created: i64,
        limit: i64,
    ) -> Result<bool> {
        let tx = self.connection.transaction()?;
        let live: i64 = tx.query_row(
            "SELECT count(*) FROM agent_controllers WHERE project_id=?1 AND revoked IS NULL",
            [project],
            |r| r.get(0),
        )?;
        if live >= limit {
            return Ok(false);
        }
        tx.execute("INSERT INTO agent_controllers(id,name,project_id,grants,token_hash,created) VALUES (?1,?2,?3,?4,?5,?6)",params![id,name,project,grants,digest,created])?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','controller_created',?1,?2,?3)",params![format!("controller:{id}"),grants,created])?;
        tx.commit()?;
        Ok(true)
    }
    pub fn revoke_controller(&self, id: &str, now: i64) -> Result<Revocation> {
        let tx = self.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE agent_controllers SET revoked=?2 WHERE id=?1 AND revoked IS NULL",
            params![id, now],
        )?;
        if changed == 0 {
            let known: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_controllers WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            return Ok(if known {
                Revocation::AlreadyRevoked
            } else {
                Revocation::Missing
            });
        }
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','controller_revoked',?1,'accepted',?2)",params![format!("controller:{id}"),now])?;
        tx.commit()?;
        Ok(Revocation::Revoked)
    }
    pub fn controller_exists(&self, id: &str) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_controllers WHERE id=?1)",
            [id],
            |r| r.get(0),
        )
    }
    pub fn controller_audit(&self, actor: &str) -> rusqlite::Result<Vec<Value>> {
        self.connection.prepare("SELECT actor,action,target,outcome,created FROM agent_audit WHERE actor=?1 OR target=?1 ORDER BY id DESC LIMIT 100")?
            .query_map([actor],|r|Ok(json!({"actor":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"created":r.get::<_,i64>(4)?})))?.collect()
    }
}
