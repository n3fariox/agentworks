//! Folder browsing and creation beneath allowed roots, for picking a project
//! or session directory. Local requests use this server's allowed roots;
//! remote requests run on the host's worker against that host's directories.
//! Both enforce containment with `agentworks_workspace::directories`.
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
}
pub async fn browse(State(app): State<App>, Query(req): Query<Browse>) -> ApiResult {
    let host = req.host_id.as_deref().unwrap_or("local");
    if host != "local" {
        return hosts::directories(
            &app,
            host,
            json!({"operation":"browseDirectories","path":req.path,"hidden":req.hidden}),
        )
        .await
        .map(Json);
    }
    let roots = effective_roots(&app);
    let path = (!req.path.is_empty()).then_some(req.path);
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
        )
        .await
        .map(Json);
    }
    let roots = effective_roots(&app);
    let path =
        tokio::task::spawn_blocking(move || directories::create(&roots, &req.parent, &req.name))
            .await
            .map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not create the folder",
                )
            })??;
    Ok(Json(json!({ "path": path })))
}
