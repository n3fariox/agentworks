//! Folder browsing and creation beneath allowed roots, for picking a project
//! or session directory. Local requests use this server's allowed roots;
//! remote requests run on the host's worker against that host's directories.
//! Both enforce containment with `agentworks_workspace::directories`.
//!
//! `scope=home` browses beneath the host's home directory instead, for choosing
//! the allowed roots themselves; it starts in the home directory.
use crate::{ApiResult, App, effective_roots, fail, hosts};
use agentworks_workspace::directories;
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Browse {
    host_id: Option<String>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    scope: Scope,
}
#[derive(Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Roots,
    Home,
}
/// Remote workers expand `~` to their own home.
const REMOTE_HOME: &str = "~";
fn local_home() -> Result<std::path::PathBuf, (StatusCode, String)> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(Into::into)
        .ok_or_else(|| {
            fail(
                StatusCode::CONFLICT,
                "This server has no home directory to browse",
            )
        })
}
fn local_roots(app: &App, scope: Scope) -> Result<Vec<std::path::PathBuf>, (StatusCode, String)> {
    match scope {
        Scope::Roots => Ok(effective_roots(app)),
        Scope::Home => Ok(vec![local_home()?]),
    }
}
fn remote_roots(scope: Scope) -> Option<Vec<String>> {
    (scope == Scope::Home).then(|| vec![REMOTE_HOME.to_owned()])
}
pub async fn browse(State(app): State<App>, Query(req): Query<Browse>) -> ApiResult {
    let host = req.host_id.as_deref().unwrap_or("local");
    // Home browsing opens in the home directory rather than a list of roots.
    let start = |home: String| match (req.scope, req.path.is_empty()) {
        (Scope::Home, true) => home,
        _ => req.path.clone(),
    };
    if host != "local" {
        return hosts::directories(
            &app,
            host,
            json!({"operation":"browseDirectories","path":start(REMOTE_HOME.into()),"hidden":req.hidden}),
            remote_roots(req.scope),
        )
        .await
        .map(Json);
    }
    let roots = local_roots(&app, req.scope)?;
    let path = start(roots[0].to_string_lossy().into_owned());
    let path = (!path.is_empty()).then_some(path);
    tokio::task::spawn_blocking(move || directories::browse(&roots, path.as_deref(), req.hidden))
        .await
        .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "Directory read failed"))?
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    host_id: Option<String>,
    parent: String,
    name: String,
    #[serde(default)]
    scope: Scope,
}
pub async fn create(State(app): State<App>, Json(req): Json<Create>) -> ApiResult {
    // Serialized with session creation, which may read the same roots.
    let _workspace = app.workspace_lock.lock().await;
    let host = req.host_id.as_deref().unwrap_or("local");
    if host != "local" {
        return hosts::directories(
            &app,
            host,
            json!({"operation":"createDirectory","path":req.parent,"name":req.name}),
            remote_roots(req.scope),
        )
        .await
        .map(Json);
    }
    let roots = local_roots(&app, req.scope)?;
    let path =
        tokio::task::spawn_blocking(move || directories::create(&roots, &req.parent, &req.name))
            .await
            .map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not create the folder",
                )
            })??;
    Ok(Json(
        json!({ "path": agentworks_workspace::paths::display(&path) }),
    ))
}
