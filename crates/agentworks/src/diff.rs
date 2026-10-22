//! HTTP adapter for per-session comparisons.
use crate::{ApiResult, App, effective_roots, fail, git_context, session};
pub use agentworks_workspace::diff::Request;
use agentworks_workspace::diff::compare;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde_json::json;
use std::time::Duration;

pub async fn view(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(req): Query<Request>,
) -> ApiResult {
    match tokio::time::timeout(Duration::from_secs(10), view_inner(app, id, req)).await {
        Ok(result) => result,
        Err(_) => Err(fail(
            StatusCode::REQUEST_TIMEOUT,
            "Git diff inspection timed out",
        )),
    }
}

async fn view_inner(app: App, id: String, req: Request) -> ApiResult {
    let s = session(&app, &id)?;
    // Reuse the workspace endpoint's live root/symlink and worktree boundary checks.
    let Json(context) = git_context::workspace(State(app.clone()), Path(id)).await?;
    if context["status"] != "git" {
        return Ok(Json(
            json!({"context":context,"unavailable":"Diff requires an available Git worktree"}),
        ));
    }
    let cwd = tokio::fs::canonicalize(&s.cwd)
        .await
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "Session directory is unavailable"))?;
    let roots = effective_roots(&app);
    let root = roots
        .iter()
        .filter(|r| cwd.starts_with(r))
        .min_by_key(|r| r.components().count())
        .ok_or_else(|| {
            fail(
                StatusCode::FORBIDDEN,
                "Session directory is outside allowed roots",
            )
        })?;
    let worktree = tokio::fs::canonicalize(context["worktreeRoot"].as_str().unwrap())
        .await
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "Worktree is unavailable"))?;
    if !worktree.starts_with(root) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Git worktree is outside allowed roots",
        ));
    }
    // Run from the worktree root so nested sessions see the entire worktree.
    let mut value = compare(&worktree, root, &req)
        .await
        .map_err(|message| fail(StatusCode::BAD_REQUEST, message))?;
    value["context"] = context;
    Ok(Json(value))
}
