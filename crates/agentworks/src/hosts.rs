//! HTTP and session coordination for remote execution hosts.
use crate::{ApiResult, App, catalog::Host, fail};
pub use agentworks_runtime::remote::{Hosts, Live, known_hosts_option, managed_ssh_args, registry};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
pub fn view(hosts: &Hosts, host: &Host) -> Value {
    let link = hosts.link(&host.id);
    json!({
        "id": host.id,
        "name": host.name,
        "kind": "remote",
        "destination": host.destination,
        "command": host.command,
        "roots": host.roots,
        "managed": host.identity_file.is_some(),
        "status": link.status,
        "detail": link.detail,
        "since": link.since,
        "worker": link.worker,
    })
}

#[derive(Deserialize)]
pub struct NewHost {
    name: String,
    destination: String,
    #[serde(default)]
    command: String,
}
pub async fn add(State(app): State<App>, Json(req): Json<NewHost>) -> ApiResult {
    let host = app
        .catalog
        .lock()
        .unwrap()
        .add_host(&req.name, &req.destination, &req.command)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    app.hosts.start(host.clone());
    Ok(Json(view(&app.hosts, &host)))
}
pub async fn remove(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    if app
        .sessions
        .lock()
        .unwrap()
        .values()
        .any(|s| s.host_id == id)
    {
        return Err(fail(
            StatusCode::CONFLICT,
            "Delete this host's sessions before removing it",
        ));
    }
    let (host, credentials_dir) = {
        let mut catalog = app.catalog.lock().unwrap();
        let host = catalog.data.hosts.iter().find(|h| h.id == id).cloned();
        catalog
            .remove_host(&id)
            .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
        (host, catalog.credentials_dir())
    };
    crate::terminal::remove_host(&id).await;
    app.hosts.stop(&id);
    if let Some(key) = host.and_then(|h| h.identity_file) {
        // Only erase a generated key directory owned by this catalog.
        if let (Some(dir), Ok(base)) =
            (key.parent(), tokio::fs::canonicalize(credentials_dir).await)
        {
            if dir.parent() == Some(base.as_path())
                && key.file_name().is_some_and(|name| name == "id_ed25519")
                && dir
                    .file_name()
                    .is_some_and(|name| uuid::Uuid::parse_str(&name.to_string_lossy()).is_ok())
            {
                if tokio::fs::remove_dir_all(dir).await.is_err() {
                    eprintln!("Could not remove a deleted host's managed SSH key directory");
                }
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}
/// Validates a new remote session's directory: the host is connected and has
/// the provider, and the directory exists there inside one of the host's
/// working directories. Returns the host's canonical path.
pub(crate) async fn session_directory(
    app: &App,
    host_id: &str,
    req: &crate::Create,
) -> Result<std::path::PathBuf, (StatusCode, String)> {
    let roots = host_roots(app, host_id)?;
    let live = app.hosts.live(host_id).ok_or_else(|| {
        fail(
            StatusCode::CONFLICT,
            "That host is not connected. Reconnect it from the Hosts list.",
        )
    })?;
    if !live.has_provider(&req.provider) {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("{} is not available on that host.", req.provider),
        ));
    }
    if roots.is_empty() {
        return Err(fail(
            StatusCode::CONFLICT,
            "Add working directories for this host before starting sessions there.",
        ));
    }
    let cwd = live
        .check_dir(&req.cwd)
        .await
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    let cwd = std::path::PathBuf::from(cwd);
    for root in &roots {
        if let Ok(root) = live.check_dir(root).await {
            if cwd.starts_with(&root) {
                return Ok(cwd);
            }
        }
    }
    Err(fail(
        StatusCode::FORBIDDEN,
        "That directory is outside this host's working directories.",
    ))
}

/// Validate files and provider availability on the machine owning the path.
pub(crate) async fn directory_on(
    app: &App,
    host: &str,
    provider: &str,
    cwd: &str,
) -> Result<std::path::PathBuf, (StatusCode, String)> {
    if host == "local" {
        return crate::checked_cwd(app, cwd);
    }
    let req = crate::Create {
        provider: provider.into(),
        cwd: cwd.into(),
        ..Default::default()
    };
    tokio::time::timeout(Duration::from_secs(12), session_directory(app, host, &req))
        .await
        .map_err(|_| {
            fail(
                StatusCode::REQUEST_TIMEOUT,
                "Remote directory validation timed out",
            )
        })?
}
pub(crate) async fn validate_session(
    app: &App,
    s: &crate::Session,
) -> Result<(), (StatusCode, String)> {
    if s.host_id == "local" && !crate::installed(&s.provider) {
        return Err(fail(StatusCode::CONFLICT, "Harness is unavailable"));
    }
    let cwd = directory_on(app, &s.host_id, &s.provider, &s.cwd.to_string_lossy()).await?;
    if cwd != s.cwd {
        return Err(fail(
            StatusCode::CONFLICT,
            "Session directory no longer resolves to its saved location",
        ));
    }
    Ok(())
}
pub(crate) async fn list_sessions(
    app: &App,
    host: &str,
    provider: &str,
    cwd: Option<std::path::PathBuf>,
    cursor: Option<String>,
    profile: bool,
) -> Result<Value, (StatusCode, String)> {
    let remote = if host == "local" {
        None
    } else {
        let path = match cwd.as_ref() {
            Some(path) => path.to_string_lossy().into_owned(),
            None => host_roots(app, host)?.first().cloned().ok_or_else(|| {
                fail(
                    StatusCode::CONFLICT,
                    "Add working directories to this host first",
                )
            })?,
        };
        let spawn_cwd = directory_on(app, host, provider, &path).await?;
        let live = app.hosts.live(host).ok_or_else(|| {
            fail(
                StatusCode::CONFLICT,
                "Host disconnected during session discovery",
            )
        })?;
        Some((live, spawn_cwd))
    };
    crate::session_runtime::list_sessions_on(provider, cwd, cursor, profile, remote)
        .await
        .map_err(|e| {
            fail(
                StatusCode::BAD_GATEWAY,
                diagnostic(app, host, provider, &e.to_string()),
            )
        })
}

/// Authentication lives with the SSH user, never on the server machine.
pub(crate) fn diagnostic(app: &App, host: &str, provider: &str, message: &str) -> String {
    let safe = agentworks_runtime::provider::safe_error(message);
    if host == "local" || !safe.to_lowercase().contains("auth") {
        return safe;
    }
    let destination = app
        .catalog
        .lock()
        .unwrap()
        .data
        .hosts
        .iter()
        .find(|h| h.id == host)
        .map(|h| h.destination.clone())
        .unwrap_or_else(|| host.into());
    let command = match provider {
        "claude" => "claude auth login",
        "codex" => "codex login",
        "opencode" => "opencode auth login",
        _ => return safe,
    };
    format!(
        "{safe} Sign in on {destination} as the SSH user ({command}), then retry. Host settings show the discovered CLI paths."
    )
}

fn host_roots(app: &App, host_id: &str) -> Result<Vec<String>, (StatusCode, String)> {
    app.catalog
        .lock()
        .unwrap()
        .data
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .map(|h| h.roots.clone())
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "Host not found"))
}
pub(crate) async fn workspace_list(app: &App, host: &str, cwd: &str) -> ApiResult {
    let roots = host_roots(app, host)?;
    let live = app
        .hosts
        .live(host)
        .ok_or_else(|| fail(StatusCode::CONFLICT, "Host is not connected"))?;
    let result = live
        .workspace(json!({"operation":"list","cwd":cwd,"roots":roots}))
        .await
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(result))
}
pub(crate) async fn worktree_operation(
    app: &App,
    host: &str,
    operation: &str,
    mut request: Value,
) -> Result<Value, (StatusCode, String)> {
    let roots = host_roots(app, host)?;
    let live = app.hosts.live(host).ok_or_else(|| {
        fail(
            StatusCode::CONFLICT,
            "Host is not connected; reconnect before managing its worktree",
        )
    })?;
    if live.worker()["worktreeCleanup"] != true {
        return Err(fail(
            StatusCode::CONFLICT,
            "Update the worker on this host to enable worktree cleanup and restore",
        ));
    }
    request["operation"] = json!(operation);
    request["roots"] = json!(roots);
    live.workspace(request)
        .await
        .map_err(|e| fail(StatusCode::CONFLICT, e))
}

pub(crate) async fn prepare_workspace(
    app: &App,
    host: &str,
    req: &crate::Create,
    source: std::path::PathBuf,
    key: Option<&str>,
) -> Result<(std::path::PathBuf, Option<crate::worktree_cleanup::Owned>), (StatusCode, String)> {
    let roots = host_roots(app, host)?;
    let policy = {
        let catalog = app.catalog.lock().unwrap();
        match req.project_id.as_deref() {
            Some(id) => catalog
                .data
                .projects
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "Project not found"))?
                .workspace_policy
                .clone(),
            None => crate::worktrees::Policy::default(),
        }
    };
    let live = app
        .hosts
        .live(host)
        .ok_or_else(|| fail(StatusCode::CONFLICT, "Host is not connected"))?;
    let mode = if req.external_id.is_some() {
        crate::worktrees::Mode::Directory
    } else {
        req.workspace
            .as_ref()
            .map(|w| w.mode)
            .unwrap_or(policy.default_mode)
    };
    if mode == crate::worktrees::Mode::Directory && !policy.require_worktree {
        if req
            .workspace
            .as_ref()
            .is_some_and(|w| w.path.is_some() || w.branch.is_some() || w.base_ref.is_some())
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Worktree options only apply to worktree modes",
            ));
        }
        return Ok((source, None));
    }
    if live.worker()["workspace"] != true {
        return Err(fail(
            StatusCode::CONFLICT,
            "Update the worker on this host to enable remote worktrees.",
        ));
    }
    // Reserve durably before the worker can mutate Git. Never replay a lost mutation.
    if mode == crate::worktrees::Mode::NewWorktree {
        crate::worktrees::arm_creation(app, key)?;
    }
    let result = live.workspace(json!({"operation":"prepare","cwd":source,"roots":roots,"policy":policy,"workspace":if req.external_id.is_some() { json!({"mode":"directory"}) } else { json!(req.workspace) }})).await
        .map_err(|e| fail(StatusCode::CONFLICT, format!("{e} Inspect retained remote branches/files before retrying.")))?;
    let path = result["cwd"]
        .as_str()
        .ok_or_else(|| fail(StatusCode::BAD_GATEWAY, "Missing remote workspace path"))?;
    let owned = serde_json::from_value(result["owned"].clone()).map_err(|_| {
        fail(
            StatusCode::BAD_GATEWAY,
            "Invalid remote workspace ownership",
        )
    })?;
    Ok((path.into(), owned))
}

#[derive(Deserialize)]
pub struct Roots {
    roots: Vec<String>,
}
pub async fn set_roots(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Roots>,
) -> ApiResult {
    // Serialize policy changes with session creation and scheduler admission.
    let _workspace = app.workspace_lock.lock().await;
    let host = app
        .catalog
        .lock()
        .unwrap()
        .set_host_roots(&id, req.roots)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(view(&app.hosts, &host)))
}
/// Drops the current link, if any, and connects again immediately.
pub async fn reconnect(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let host = app
        .catalog
        .lock()
        .unwrap()
        .data
        .hosts
        .iter()
        .find(|h| h.id == id)
        .cloned()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Host not found"))?;
    app.hosts.start(host.clone());
    Ok(Json(view(&app.hosts, &host)))
}
