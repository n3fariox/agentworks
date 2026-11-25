//! Durable agent claims. Reports never change turn state or authorize work.
use crate::{ApiResult, App, Session, communication as mail, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    status: String,
    summary: String,
    branch: Option<String>,
    commit: Option<String>,
    #[serde(default)]
    checks: Vec<Check>,
    request_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Check {
    name: String,
    result: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Query {
    session_id: Option<String>,
}
fn error(_: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not save or read completion report",
    )
}
fn bounded(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max
}
impl Report {
    fn validate(&self) -> mail::Result<()> {
        if !matches!(self.status.as_str(), "completed" | "blocked")
            || !bounded(&self.summary, 4096)
            || !bounded(&self.request_id, 100)
            || self.branch.as_ref().is_some_and(|s| !bounded(s, 256))
            || self.commit.as_ref().is_some_and(|s| {
                !matches!(s.len(), 40 | 64) || !s.bytes().all(|b| b.is_ascii_hexdigit())
            })
            || self.checks.len() > 32
            || self
                .checks
                .iter()
                .any(|c| !bounded(&c.name, 256) || !bounded(&c.result, 2048))
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Invalid completion report fields or bounds",
            ));
        }
        // Keep the complete native bridge request below its 64 KiB frame limit.
        if serde_json::to_vec(self).map_err(error)?.len() > 24000 {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Completion report exceeds 24000 bytes",
            ));
        }
        Ok(())
    }
}
impl crate::store::Store {
    pub fn latest_completion(&self, id: &str, scope: Option<&str>) -> mail::Result<Value> {
        Ok(json!({"report":self.0.latest_completion(id,scope).map_err(error)?}))
    }
}
pub fn report(app: &App, s: &Session, req: Report) -> mail::Result<Value> {
    mail::require(app, s, "communicate")?;
    let req: Report = {
        let mut value = serde_json::to_value(&req).map_err(error)?;
        crate::secrets::scrub(&s.id, &mut value);
        serde_json::from_value(value).map_err(error)?
    };
    req.validate()?;
    let payload = serde_json::to_string(&req).map_err(error)?;
    let project = mail::project(s);
    let title = s.display_title();
    let mut store = app.store.lock().unwrap();
    use agentworks_database::mail::CompletionOutcome;
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let request = req.request_id.clone();
    let outcome = store
        .report_completion(
            &s.id,
            &title,
            project.as_deref(),
            &request,
            &payload,
            serde_json::to_value(req).map_err(error)?,
            created,
        )
        .map_err(error)?;
    let report = match outcome {
        CompletionOutcome::Saved(report) => report,
        CompletionOutcome::Duplicate(report) => {
            return Ok(json!({"report":report,"duplicate":true}));
        }
        CompletionOutcome::Conflict => {
            return Err(fail(
                StatusCode::CONFLICT,
                "Request ID already used for a different completion report",
            ));
        }
    };
    let id = report["id"].as_i64().unwrap();
    drop(store);
    {
        let mut data = s.data.lock().unwrap();
        data.continuation
            .stop("Explicit completion or blocked report");
        data.input_queue
            .block("Completion reported; inspect, then resume.");
    }
    s.emit(
        "completion_reported",
        json!({"reportId":id,"status":report["status"],"summary":report["summary"]}),
    );
    Ok(json!({"report":report,"duplicate":false}))
}
pub fn get(app: &App, a: &mail::Actor, req: Query) -> mail::Result<Value> {
    let grants = mail::actor_access(app, a)?;
    if !grants.communicate && !mail::coordinator(&grants) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Completion report access is not granted",
        ));
    }
    let id = mail::session_ref(req.session_id.as_deref().unwrap_or(&a.id));
    let scope = if id == a.id {
        None
    } else {
        let target = mail::peer_for(app, a, id)?;
        if !mail::coordinator(&grants) {
            mail::require(app, &target, "communicate")?;
        }
        // The orchestrator reads reports saved in the target's current project.
        mail::project(&target).or_else(|| a.project.clone())
    };
    app.store
        .lock()
        .unwrap()
        .latest_completion(id, scope.as_deref())
}
/// Human API follows the same session visibility as other human session endpoints.
pub async fn get_completion(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    session(&app, &id)?;
    Ok(Json(
        app.store.lock().unwrap().latest_completion(&id, None)?,
    ))
}
