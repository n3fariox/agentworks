//! HTTP and session coordination for workspace preparation.
use crate::{ApiResult, App, Create, checked_cwd, effective_roots, fail, worktree_cleanup::Owned};
pub(crate) use agentworks_workspace::worktrees::{Context, list_in, prepare_in};
pub use agentworks_workspace::worktrees::{Mode, Policy, Workspace};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
type Error = agentworks_workspace::Error;

fn bad(message: impl Into<String>) -> Error {
    fail(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> Error {
    fail(StatusCode::CONFLICT, message)
}

pub async fn get_policy(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let catalog = app.catalog.lock().unwrap();
    let project = catalog
        .data
        .projects
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Project not found"))?;
    Ok(Json(json!(project.workspace_policy)))
}
pub async fn put_policy(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(policy): Json<Policy>,
) -> ApiResult {
    if policy.require_worktree && policy.default_mode == Mode::Directory {
        return Err(bad(
            "A requiring project must default to newWorktree or existingWorktree",
        ));
    }
    let _workspace = app.workspace_lock.lock().await;
    app.catalog
        .lock()
        .unwrap()
        .workspace_policy(&id, policy.clone())
        .map_err(bad)?;
    Ok(Json(json!(policy)))
}

// Distinct namespaces prevent direct clients from colliding with MCP/automation requests.
pub fn creation_key(req: &Create) -> Result<Option<String>, Error> {
    if let Some(o) = &req.control_origin {
        return Ok(Some(format!("mcp:{}:{}", o.actor, o.request)));
    }
    if let Some(o) = &req.automation {
        return Ok(Some(format!(
            "automation:{}:{}",
            o.automation_id, o.request_id
        )));
    }
    req.request_id
        .as_ref()
        .map(|id| {
            if id.is_empty() || id.len() > 100 || id.chars().any(char::is_control) {
                return Err(bad(
                    "Supply a requestId between 1 and 100 bytes without control characters",
                ));
            }
            Ok(format!("api:{id}"))
        })
        .transpose()
}
pub fn creation_payload(req: &Create) -> String {
    if let Some(o) = &req.control_origin {
        return o.payload.to_string();
    }
    // Generated run IDs and timestamps are deliberately excluded from automation fingerprints.
    json!({"create":req,"automation":req.automation.as_ref().map(|a| (&a.automation_id,&a.request_id)),"prompt":req.queued_prompt}).to_string()
}

impl Context for App {
    fn git_settings(&self) -> agentworks_workspace::config::Settings {
        self.catalog
            .lock()
            .unwrap()
            .data
            .settings
            .git
            .clone()
            .unwrap_or_default()
    }
    fn safe_error(&self, message: &str) -> String {
        agentworks_runtime::provider::safe_error(message)
    }
    fn roots(&self) -> Vec<PathBuf> {
        effective_roots(self)
    }
    fn checked(&self, path: &str) -> Result<PathBuf, Error> {
        checked_cwd(self, path)
    }
    fn arm(&self, key: Option<&str>) -> Result<(), Error> {
        arm_creation(self, key)
    }
}

#[derive(Deserialize)]
pub struct List {
    cwd: String,
    #[serde(default, rename = "hostId")]
    host_id: Option<String>,
}
pub async fn list(State(app): State<App>, Json(req): Json<List>) -> ApiResult {
    if let Some(host) = req.host_id.as_deref().filter(|h| *h != "local") {
        return crate::hosts::workspace_list(&app, host, &req.cwd).await;
    }
    let cwd = app.checked(&req.cwd)?;
    Ok(Json(list_in(&app, &cwd).await?))
}
// A supplied cwd is not evidence of an external history's original location. Verify
// discovery metadata before loading it; never move a provider conversation to satisfy policy.
pub async fn verify_import(app: &App, req: &Create, source: &FsPath) -> Result<(), Error> {
    let Some(id) = req.external_id.as_deref() else {
        return Ok(());
    };
    let _permit = app
        .discovery
        .clone()
        .try_acquire_owned()
        .map_err(|_| conflict("Harness discovery is busy; retry import shortly"))?;
    let discover = async {
        let mut cursor = None;
        for _ in 0..100 {
            let page = crate::hosts::list_sessions(
                app,
                req.host_id.as_deref().unwrap_or("local"),
                &req.provider,
                Some(source.to_owned()),
                cursor.clone(),
                req.external_host,
            )
            .await
            .map_err(|e| {
                bad(format!(
                    "Cannot verify original import cwd: {}",
                    agentworks_runtime::provider::safe_error(&e.1)
                ))
            })?;
            for entry in page["sessions"].as_array().into_iter().flatten() {
                if entry["sessionId"].as_str() == Some(id) {
                    let actual = entry["cwd"]
                        .as_str()
                        .ok_or_else(|| bad("Provider did not report this history's cwd"))?;
                    if crate::hosts::directory_on(
                        app,
                        req.host_id.as_deref().unwrap_or("local"),
                        &req.provider,
                        actual,
                    )
                    .await?
                        != source
                    {
                        return Err(bad("Imported history must use its original provider cwd"));
                    }
                    return Ok(());
                }
            }
            let next = page["nextCursor"].as_str().map(str::to_owned);
            if next.is_none() || next == cursor {
                break;
            }
            cursor = next;
        }
        Err(bad(
            "Cannot verify original provider cwd: history was not found in bounded session discovery",
        ))
    };
    tokio::time::timeout(
        Duration::from_secs(if req.host_id.as_deref().is_some_and(|h| h != "local") {
            75
        } else {
            12
        }),
        discover,
    )
    .await
    .map_err(|_| conflict("Import cwd verification timed out"))?
}

pub fn arm_creation(app: &App, key: Option<&str>) -> Result<(), Error> {
    if let Some(key) = key {
        app.store.lock().unwrap().arm_creation(key).map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not durably reserve creation before mutation",
            )
        })?;
    }
    Ok(())
}

pub async fn prepare(
    app: &App,
    req: &Create,
    source: PathBuf,
    creation_key: Option<&str>,
) -> Result<(PathBuf, Option<Owned>), Error> {
    let policy = {
        let catalog = app.catalog.lock().unwrap();
        match req.project_id.as_deref() {
            Some(id) => catalog
                .data
                .projects
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| bad("Project not found"))?
                .workspace_policy
                .clone(),
            None => Policy::default(),
        }
    };
    prepare_in(
        app,
        req.workspace.as_ref(),
        policy,
        req.external_id.is_some(),
        source,
        creation_key,
    )
    .await
}
