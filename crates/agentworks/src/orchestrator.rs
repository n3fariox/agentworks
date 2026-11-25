//! The orchestrator slot: one ungrouped session, pinned above the session browser, that
//! routes and manages work across every project and never codes itself.
//!
//! Sessions never move in or out of the slot. The slot creates its own session in a
//! server-owned directory (`New orchestrator`), replaces it with a fork on another harness
//! (the old one is deleted), or is cleared (its session is deleted). The slotted session
//! cannot be archived, deleted, forked, moved or regrouped through the ordinary endpoints,
//! and the slot is locked while its turn runs.
use crate::{ApiResult, App, communication as mail, fail};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewOrchestrator {
    provider: String,
    model: Option<String>,
    model_provider: Option<String>,
    title: Option<String>,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkOrchestrator {
    provider: String,
    model: Option<String>,
    model_provider: Option<String>,
    title: Option<String>,
    boundary_seq: Option<u64>,
    context: Option<String>,
    summary: Option<String>,
    request_id: String,
}

fn current(app: &App) -> Option<Arc<crate::Session>> {
    let id = app.catalog.lock().unwrap().data.orchestrator.clone()?;
    app.sessions.lock().unwrap().get(&id).cloned()
}
fn directory() -> Result<String, (StatusCode, String)> {
    mail::orchestrator_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .ok_or_else(|| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "The orchestrator directory is unavailable",
            )
        })
}
/// The slot is locked while the orchestrator's turn runs.
fn between_turns(s: &crate::Session) -> Result<(), (StatusCode, String)> {
    let d = s.data.lock().unwrap();
    if d.turn_active || matches!(d.status.as_str(), "running" | "waiting" | "cancelling") {
        return Err(fail(
            StatusCode::CONFLICT,
            "The orchestrator is mid-turn; wait for it to finish, or stop it",
        ));
    }
    Ok(())
}
fn save_slot(app: &App, id: Option<String>) -> Result<(), (StatusCode, String)> {
    app.catalog
        .lock()
        .unwrap()
        .set_orchestrator(id)
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save the orchestrator slot",
            )
        })
}
fn role(provider: &str) -> Option<String> {
    crate::roles::supported(provider).then(|| "orchestrator".to_owned())
}
fn workspace() -> Option<crate::worktrees::Workspace> {
    serde_json::from_value(json!({"mode":"directory"})).ok()
}

pub async fn get(State(app): State<App>) -> ApiResult {
    let live = mail::orchestrator(&app);
    Ok(Json(
        json!({"sessionId":live.as_ref().map(|s| s.id.clone()),"session":live.map(|s| s.meta()),"directory":mail::orchestrator_dir()}),
    ))
}

/// Creates a fresh orchestrator session in the orchestrator directory and slots it.
pub async fn create(State(app): State<App>, Json(req): Json<NewOrchestrator>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    if let Some(s) = current(&app) {
        return Err(fail(
            StatusCode::CONFLICT,
            format!(
                "“{}” is already the orchestrator; fork or clear it first",
                s.display_title()
            ),
        ));
    }
    let provider = req.provider.trim().to_owned();
    if provider.is_empty() || provider == "auto" {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Choose a harness for the orchestrator",
        ));
    }
    let created = crate::create_session(
        State(app.clone()),
        Json(crate::Create {
            role: role(&provider),
            provider,
            cwd: directory()?,
            title: Some(req.title.unwrap_or_else(|| "Orchestrator".into())),
            model: req.model,
            model_provider: req.model_provider,
            project_id: None,
            host_id: Some("local".into()),
            workspace: workspace(),
            agent_access: Some(mail::Role::Coordinator.access()),
            request_id: Some(format!("orchestrator:{}", req.request_id)),
            orchestrator_slot: true,
            ..Default::default()
        }),
    )
    .await?
    .0;
    save_slot(&app, created["id"].as_str().map(str::to_owned))?;
    Ok(Json(created))
}

/// Forks the orchestrator to another harness. The fork takes the slot and the old
/// session is deleted.
pub async fn fork(State(app): State<App>, Json(req): Json<ForkOrchestrator>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let old = current(&app)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "There is no orchestrator to fork"))?;
    between_turns(&old)?;
    let provider = req.provider.trim().to_owned();
    if provider.is_empty() || provider == "auto" {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Choose a harness for the orchestrator",
        ));
    }
    let child = crate::fork_session_inner(
        &app,
        &old.id,
        crate::Fork {
            boundary_seq: req.boundary_seq,
            context: req.context,
            summary: req.summary,
            title: Some(req.title.unwrap_or_else(|| "Orchestrator".into())),
            provider: Some(provider),
            model: req.model,
            model_provider: Some(req.model_provider),
            project_id: None,
            host_id: Some("local".into()),
            cwd: Some(directory()?),
            workspace: workspace(),
            agent_access: Some(mail::Role::Coordinator.access()),
            request_id: Some(format!("orchestrator:{}", req.request_id)),
            resume_policy: None,
        },
        true,
    )
    .await?
    .0;
    save_slot(&app, child["id"].as_str().map(str::to_owned))?;
    // The replaced orchestrator is tossed, not kept as a regular session.
    if let Err((_, error)) = crate::delete_session_unlocked(&app, &old).await {
        tracing::warn!(session = %old.id, %error, "Could not delete the replaced orchestrator");
        return Ok(Json(
            json!({"session":child,"warning":format!("The fork is the orchestrator, but the old session could not be deleted: {error}")}),
        ));
    }
    Ok(Json(json!({ "session": child })))
}

/// Clears the slot and deletes its session.
pub async fn clear(State(app): State<App>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let Some(old) = current(&app) else {
        save_slot(&app, None)?;
        return Ok(Json(json!({"cleared":true})));
    };
    between_turns(&old)?;
    save_slot(&app, None)?;
    crate::delete_session_unlocked(&app, &old).await?;
    Ok(Json(json!({"cleared":true})))
}
