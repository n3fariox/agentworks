use crate::{
    ApiResult, App, Create, SessionData, checked_cwd, create_session, fail, installed, key_present,
    runtime, use_api_key,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Origin {
    pub automation_id: String,
    pub name: String,
    pub run_id: String,
    pub request_id: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub initiated_by: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub model: Option<String>,
    pub cwd: String,
    pub location_id: String,
    pub host_id: String,
    pub project_id: Option<String>,
    pub prompt: String,
    pub enabled: bool,
    pub created: u64,
    pub updated: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    name: String,
    provider: String,
    model: Option<String>,
    cwd: String,
    project_id: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
    prompt: String,
}
fn enabled() -> bool {
    true
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn definition(
    app: &App,
    id: Option<&str>,
    input: Input,
) -> Result<Definition, (StatusCode, String)> {
    let name = input.name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || input.prompt.trim().is_empty()
        || input.prompt.len() > 32768
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a name up to 80 characters and instructions up to 32 KB",
        ));
    }
    if !["demo", "claude", "codex", "opencode"].contains(&input.provider.as_str()) {
        return Err(fail(StatusCode::BAD_REQUEST, "Unknown harness"));
    }
    let model = input
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    if model.as_ref().is_some_and(|m| {
        m.len() > 256 || (input.provider == "opencode" && !runtime::is_free_model(m))
    }) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Invalid model; OpenCode supports free models only",
        ));
    }
    let cwd = checked_cwd(app, &input.cwd)?;
    let mut catalog = app.catalog.lock().unwrap();
    if input
        .project_id
        .as_deref()
        .is_some_and(|p| !catalog.contains(p))
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    let location = catalog.location(&cwd).map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save directory",
        )
    })?;
    if let Some(project) = &input.project_id {
        catalog
            .attach_location(project, &location.id)
            .map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not save project directory",
                )
            })?;
    }
    let created = if let Some(id) = id {
        app.store
            .lock()
            .unwrap()
            .automation(id)
            .map_err(storage)?
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Automation not found"))?
            .created
    } else {
        now()
    };
    Ok(Definition {
        id: id
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: name.into(),
        provider: input.provider,
        model,
        cwd: cwd.to_string_lossy().into_owned(),
        location_id: location.id,
        host_id: "local".into(),
        project_id: input.project_id,
        prompt: input.prompt,
        enabled: input.enabled,
        created,
        updated: now(),
    })
}
fn storage(_error: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not access automation storage",
    )
}
pub async fn list(State(app): State<App>) -> ApiResult {
    Ok(Json(json!(
        app.store.lock().unwrap().automations().map_err(storage)?
    )))
}
pub async fn create(State(app): State<App>, Json(input): Json<Input>) -> ApiResult {
    let def = definition(&app, None, input)?;
    app.store
        .lock()
        .unwrap()
        .save_automation(&def)
        .map_err(storage)?;
    Ok(Json(json!(def)))
}
pub async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Input>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _guard = app.run_lock.lock().await;
    let def = definition(&app, Some(&id), input)?;
    app.store
        .lock()
        .unwrap()
        .save_automation(&def)
        .map_err(storage)?;
    Ok(Json(json!(def)))
}
pub async fn delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _guard = app.run_lock.lock().await;
    app.store
        .lock()
        .unwrap()
        .delete_automation(&id)
        .map_err(storage)?;
    Ok(Json(json!({"ok":true})))
}
pub async fn runs(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(json!(
        app.store
            .lock()
            .unwrap()
            .automation_runs(&id)
            .map_err(storage)?
    )))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(skip)]
    pub(crate) initiated_by: Option<String>,
    pub(crate) request_id: String,
}
pub async fn run(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Run>,
) -> ApiResult {
    if input.request_id.is_empty()
        || input.request_id.len() > 100
        || input.request_id.chars().any(char::is_control)
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Supply a valid run request ID",
        ));
    }
    let _guard = app.run_lock.lock().await;
    crate::service_restart::allow_start(&app)?;
    let previous = app
        .store
        .lock()
        .unwrap()
        .run_session(&id, &input.request_id)
        .map_err(storage)?;
    if let Some(session_id) = previous {
        let sessions = app.sessions.lock().unwrap();
        let session = sessions.get(&session_id).ok_or_else(|| {
            fail(
                StatusCode::GONE,
                "This run's session was deleted; use a new request ID for a new run",
            )
        })?;
        return Ok(Json(json!({"session":session.meta(),"duplicate":true})));
    }
    let def = app
        .store
        .lock()
        .unwrap()
        .automation(&id)
        .map_err(storage)?
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Automation not found"))?;
    if !def.enabled {
        return Err(fail(StatusCode::CONFLICT, "This automation is paused"));
    }
    if !installed(&def.provider) || (use_api_key() && !key_present(&def.provider)) {
        return Err(fail(
            StatusCode::CONFLICT,
            "Harness or its authentication is unavailable",
        ));
    }
    if app.sessions.lock().unwrap().values().any(|s| {
        let d = s.data.lock().unwrap();
        d.automation
            .as_ref()
            .is_some_and(|a| a.automation_id == id && a.outcome.is_none())
            && matches!(
                d.status.as_str(),
                "starting" | "running" | "waiting" | "resuming" | "configuring" | "cancelling"
            )
    }) {
        return Err(fail(
            StatusCode::CONFLICT,
            "This automation already has an active run",
        ));
    }
    let origin = Origin {
        automation_id: id,
        name: def.name.clone(),
        run_id: Uuid::new_v4().to_string(),
        request_id: input.request_id,
        initiated_by: input.initiated_by,
        outcome: None,
    };
    let Json(session) = create_session(
        State(app.clone()),
        Json(Create {
            resume_policy: None,
            inherited_resume: None,
            workspace: None,
            request_id: None,
            agent_access: None,
            provider: def.provider,
            cwd: def.cwd,
            title: Some(format!("{} · Manual run", def.name)),
            model: def.model,
            project_id: def.project_id,
            host_id: Some(def.host_id),
            external_id: None,
            external_host: false,
            automation: Some(origin),
            queued_prompt: Some(def.prompt),
            control_origin: None,
            fork: None,
        }),
    )
    .await?;
    Ok(Json(json!({"session":session,"duplicate":false})))
}
pub fn run_status(data: &SessionData) -> &str {
    data.automation
        .as_ref()
        .and_then(|a| a.outcome.as_deref())
        .unwrap_or(match data.status.as_str() {
            "idle" => "starting",
            "suspended" => "interrupted",
            "error" => "failed",
            other => other,
        })
}
