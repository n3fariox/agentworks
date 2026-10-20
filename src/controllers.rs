//! C-01: named external controller identities for MCP clients outside Agentworks sessions.
//! Each controller is project-scoped, separately granted, revocable and never a session.
//! Tokens are shown once and stored only as SHA-256 digests.
use crate::{ApiResult, App, communication as mail, fail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::OnceLock};

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS agent_controllers(id TEXT PRIMARY KEY,name TEXT NOT NULL,project_id TEXT NOT NULL,grants TEXT NOT NULL,token_hash TEXT NOT NULL UNIQUE,created INTEGER NOT NULL,revoked INTEGER,last_used INTEGER);
  CREATE INDEX IF NOT EXISTS agent_controllers_project ON agent_controllers(project_id,created);";
/// Actor IDs carry this prefix so they can never collide with a session UUID.
pub const ACTOR_PREFIX: &str = "controller:";
pub const URI: &str = "agentworks://controllers/";
const TOKEN_PREFIX: &str = "awc_";
const MAX_LIVE_PER_PROJECT: i64 = 20;

static ENDPOINT_FILE: OnceLock<PathBuf> = OnceLock::new();

/// Publishes the per-process loopback bridge address for external stdio bridges (0600).
pub fn publish_endpoint(path: PathBuf, endpoint: std::net::SocketAddr) -> std::io::Result<()> {
    let temporary = path.with_extension("mcp-endpoint.tmp");
    let _ = std::fs::remove_file(&temporary);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&temporary)?;
    writeln!(file, "{endpoint}")?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    let _ = ENDPOINT_FILE.set(path);
    Ok(())
}

pub fn actor_id(id: &str) -> String {
    format!("{ACTOR_PREFIX}{id}")
}
/// Accepts `controller:<id>` or `agentworks://controllers/<id>`.
pub fn reference(value: &str) -> Option<&str> {
    value
        .strip_prefix(URI)
        .or_else(|| value.strip_prefix(ACTOR_PREFIX))
}
pub fn is_token(token: &str) -> bool {
    token.starts_with(TOKEN_PREFIX)
}

fn db_error(_: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not save or read controller data",
    )
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A live (unrevoked) controller, resolved freshly for every bridge request.
#[derive(Clone)]
pub struct Controller {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub access: mail::Access,
}

impl crate::store::Store {
    pub fn controller_by_token(&self, token: &str) -> mail::Result<Option<Controller>> {
        let digest = crate::sha256::hex(token.as_bytes());
        let row: Option<(String, String, String, String)> = self
            .connection
            .query_row(
                "SELECT id,name,project_id,grants FROM agent_controllers WHERE token_hash=?1 AND revoked IS NULL",
                [digest],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((id, name, project_id, grants)) = row else {
            return Ok(None);
        };
        self.connection
            .execute(
                "UPDATE agent_controllers SET last_used=?2 WHERE id=?1",
                params![id, now()],
            )
            .map_err(db_error)?;
        Ok(Some(Controller {
            id,
            name,
            project_id,
            access: serde_json::from_str(&grants).map_err(db_error)?,
        }))
    }
    /// Live controller lookup by ID; revoked controllers are absent.
    pub fn controller(&self, id: &str) -> mail::Result<Option<Controller>> {
        self.connection
            .query_row(
                "SELECT id,name,project_id,grants FROM agent_controllers WHERE id=?1 AND revoked IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)),
            )
            .optional()
            .map_err(db_error)?
            .map(|(id, name, project_id, grants)| {
                Ok(Controller {
                    id,
                    name,
                    project_id,
                    access: serde_json::from_str(&grants).map_err(db_error)?,
                })
            })
            .transpose()
    }
    pub fn live_controllers(&self, project: &str) -> mail::Result<Vec<Controller>> {
        let mut q = self
            .connection
            .prepare("SELECT id FROM agent_controllers WHERE project_id=?1 AND revoked IS NULL ORDER BY created")
            .map_err(db_error)?;
        let ids = q
            .query_map([project], |r| r.get::<_, String>(0))
            .map_err(db_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?;
        ids.iter()
            .filter_map(|id| self.controller(id).transpose())
            .collect()
    }
    fn controller_rows(&self, project: &str) -> mail::Result<Vec<Value>> {
        let mut q=self.connection.prepare("SELECT id,name,project_id,grants,created,revoked,last_used FROM agent_controllers WHERE project_id=?1 ORDER BY created DESC LIMIT 200").map_err(db_error)?;
        q.query_map([project], |r| {
            let id: String = r.get(0)?;
            Ok(json!({"id":id,"actor":actor_id(&id),"uri":format!("{URI}{id}"),"name":r.get::<_,String>(1)?,"projectId":r.get::<_,String>(2)?,
                "access":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null),"created":r.get::<_,i64>(4)?,"revoked":r.get::<_,Option<i64>>(5)?,"lastUsed":r.get::<_,Option<i64>>(6)?}))
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)
    }
    pub fn revoke_project_controllers(&self, project: &str) -> mail::Result<()> {
        let tx = self.connection.unchecked_transaction().map_err(db_error)?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) SELECT 'human','controller_revoked','controller:'||id,'project deleted',?2 FROM agent_controllers WHERE project_id=?1 AND revoked IS NULL",params![project,now()]).map_err(db_error)?;
        tx.execute(
            "UPDATE agent_controllers SET revoked=?2 WHERE project_id=?1 AND revoked IS NULL",
            params![project, now()],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewController {
    name: String,
    /// Least privilege: omitted grants mean project messaging only.
    access: Option<mail::Access>,
}

fn project_exists(app: &App, id: &str) -> mail::Result<()> {
    if !app.catalog.lock().unwrap().contains(id) {
        return Err(fail(StatusCode::NOT_FOUND, "Project not found"));
    }
    Ok(())
}

pub async fn list(State(app): State<App>, Path(project): Path<String>) -> ApiResult {
    project_exists(&app, &project)?;
    let rows = app.store.lock().unwrap().controller_rows(&project)?;
    Ok(Json(json!({"controllers":rows})))
}

pub async fn create(
    State(app): State<App>,
    Path(project): Path<String>,
    Json(req): Json<NewController>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    project_exists(&app, &project)?;
    let name = req.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a controller name of 1–80 characters without control characters",
        ));
    }
    let access = req.access.unwrap_or(mail::Access {
        communicate: true,
        ..Default::default()
    });
    if access.restart_service {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "External controllers cannot be granted service restart",
        ));
    }
    if !access.enabled() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Grant at least one capability",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let token = format!(
        "{TOKEN_PREFIX}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let grants = serde_json::to_string(&access).map_err(db_error)?;
    let created = now();
    {
        let mut store = app.store.lock().unwrap();
        let tx = store.connection.transaction().map_err(db_error)?;
        let live: i64 = tx
            .query_row(
                "SELECT count(*) FROM agent_controllers WHERE project_id=?1 AND revoked IS NULL",
                [&project],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if live >= MAX_LIVE_PER_PROJECT {
            return Err(fail(
                StatusCode::CONFLICT,
                "Limit: 20 live controllers per project. Revoke unused controllers first.",
            ));
        }
        tx.execute("INSERT INTO agent_controllers(id,name,project_id,grants,token_hash,created) VALUES (?1,?2,?3,?4,?5,?6)",params![id,name,project,grants,crate::sha256::hex(token.as_bytes()),created]).map_err(db_error)?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','controller_created',?1,?2,?3)",params![actor_id(&id),grants,created]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
    }
    let endpoint_file = ENDPOINT_FILE.get().map(|p| p.display().to_string());
    Ok(Json(json!({
        "controller": {"id":id,"actor":actor_id(&id),"uri":format!("{URI}{id}"),"name":name,"projectId":project,"access":access,"created":created,"revoked":null,"lastUsed":null},
        "token": token,
        "mcpServer": {
            "command": std::env::current_exe().ok(),
            "args": ["mcp"],
            "env": {"AGENTWORKS_MCP_ENDPOINT_FILE": endpoint_file, "AGENTWORKS_MCP_TOKEN": token},
        },
        "note": "This token is shown once. Store it privately; revoke the controller if it is exposed.",
    })))
}

pub async fn revoke(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    // Serializes with bridge dispatch: in-flight operations finish, later ones are refused.
    let _gate = app.communication_lock.lock().await;
    let store = app.store.lock().unwrap();
    let tx = store.connection.unchecked_transaction().map_err(db_error)?;
    let changed = tx
        .execute(
            "UPDATE agent_controllers SET revoked=?2 WHERE id=?1 AND revoked IS NULL",
            params![id, now()],
        )
        .map_err(db_error)?;
    if changed == 0 {
        let known: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_controllers WHERE id=?1)",
                [&id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !known {
            return Err(fail(StatusCode::NOT_FOUND, "Controller not found"));
        }
        return Ok(Json(json!({"revoked":true,"alreadyRevoked":true})));
    }
    tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','controller_revoked',?1,'accepted',?2)",params![actor_id(&id),now()]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(Json(json!({"revoked":true,"alreadyRevoked":false})))
}

pub async fn audit(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let store = app.store.lock().unwrap();
    let actor = actor_id(&id);
    let known: bool = store
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_controllers WHERE id=?1)",
            [&id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !known {
        return Err(fail(StatusCode::NOT_FOUND, "Controller not found"));
    }
    let mut q=store.connection.prepare("SELECT actor,action,target,outcome,created FROM agent_audit WHERE actor=?1 OR target=?1 ORDER BY id DESC LIMIT 100").map_err(db_error)?;
    let events=q.query_map([actor],|r|Ok(json!({"actor":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"created":r.get::<_,i64>(4)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    Ok(Json(json!({"events":events})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_never_parse_session_ids() {
        assert_eq!(reference("controller:abc"), Some("abc"));
        assert_eq!(reference("agentworks://controllers/abc"), Some("abc"));
        assert_eq!(reference("6f1c0b9e-0000-4000-8000-000000000000"), None);
        assert!(is_token("awc_x") && !is_token("6f1c0b9e-0000"));
    }
}
