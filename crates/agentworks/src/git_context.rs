//! HTTP adapter for repository inspection.
use crate::{ApiResult, App, effective_roots, fail, session};
use agentworks_workspace::git::inspect;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

pub async fn workspace(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    crate::local_only(&s)?;
    let roots = effective_roots(&app);
    // Recheck current roots and symlinks without rejecting a removed saved directory.
    let cwd = tokio::fs::canonicalize(&s.cwd).await.ok();
    let path = cwd.as_deref().unwrap_or(&s.cwd);
    let root = roots
        .iter()
        .filter(|r| path.starts_with(r))
        .min_by_key(|r| r.components().count())
        .ok_or_else(|| {
            fail(
                StatusCode::FORBIDDEN,
                "Project must be inside an allowed root directory",
            )
        })?;
    let context = inspect(&s.cwd, root).await;
    Ok(Json(context))
}
