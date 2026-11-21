//! C-01: named external controller identities for MCP clients outside Agentworks sessions.
//! Each controller is project-scoped, separately granted, revocable and never a session.
//! Tokens are shown once and stored only as SHA-256 digests.
use crate::{ApiResult, App, communication as mail, fail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::OnceLock};

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

pub use agentworks_database::controllers::Controller;
impl crate::store::Store {
    pub fn controller_by_token(&self, token: &str) -> mail::Result<Option<Controller>> {
        self.0
            .controller_by_digest(&crate::sha256::hex(token.as_bytes()), now())
            .map_err(db_error)
    }
    pub fn controller(&self, id: &str) -> mail::Result<Option<Controller>> {
        self.0.controller(id).map_err(db_error)
    }
    pub fn live_controllers(&self, project: &str) -> mail::Result<Vec<Controller>> {
        self.0.live_controllers(project).map_err(db_error)
    }
    fn controller_rows(&self, project: &str) -> mail::Result<Vec<Value>> {
        let mut rows = self.0.controller_rows(project).map_err(db_error)?;
        for row in &mut rows {
            let id = row["id"].as_str().unwrap().to_owned();
            row["actor"] = json!(actor_id(&id));
            row["uri"] = json!(format!("{URI}{id}"));
        }
        Ok(rows)
    }
    pub fn revoke_project_controllers(&self, project: &str) -> mail::Result<()> {
        self.0
            .revoke_project_controllers(project, now())
            .map_err(db_error)
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
        if !store
            .create_controller(
                &id,
                &name,
                &project,
                &grants,
                &crate::sha256::hex(token.as_bytes()),
                created,
                MAX_LIVE_PER_PROJECT,
            )
            .map_err(db_error)?
        {
            return Err(fail(
                StatusCode::CONFLICT,
                "Limit: 20 live controllers per project. Revoke unused controllers first.",
            ));
        }
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
    use agentworks_database::controllers::Revocation;
    match store.revoke_controller(&id, now()).map_err(db_error)? {
        Revocation::Missing => Err(fail(StatusCode::NOT_FOUND, "Controller not found")),
        Revocation::AlreadyRevoked => Ok(Json(json!({"revoked":true,"alreadyRevoked":true}))),
        Revocation::Revoked => Ok(Json(json!({"revoked":true,"alreadyRevoked":false}))),
    }
}

pub async fn audit(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let store = app.store.lock().unwrap();
    let actor = actor_id(&id);
    let known = store.controller_exists(&id).map_err(db_error)?;
    if !known {
        return Err(fail(StatusCode::NOT_FOUND, "Controller not found"));
    }
    let events = store.controller_audit(&actor).map_err(db_error)?;
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
