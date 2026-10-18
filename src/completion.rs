//! Durable agent claims. Reports never change turn state or authorize work.
use crate::{ApiResult, App, Session, communication as mail, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::{OptionalExtension, params};
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
        let raw: Option<String> = self.connection.query_row(
            "SELECT report FROM agent_completions WHERE author=?1 AND (?2 IS NULL OR project_id=?2) ORDER BY id DESC LIMIT 1",
            params![id, scope], |r| r.get(0)).optional().map_err(error)?;
        Ok(
            json!({"report": raw.map(|s| serde_json::from_str::<Value>(&s)).transpose().map_err(error)?}),
        )
    }
}
pub fn report(app: &App, s: &Session, req: Report) -> mail::Result<Value> {
    mail::require(app, s, "communicate")?;
    req.validate()?;
    let payload = serde_json::to_string(&req).map_err(error)?;
    let project = mail::project(s);
    let title = s.display_title();
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction().map_err(error)?;
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT payload,report FROM agent_completions WHERE author=?1 AND request_id=?2",
            params![s.id, req.request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some((old, report)) = previous {
        if old != payload {
            return Err(fail(
                StatusCode::CONFLICT,
                "Request ID already used for a different completion report",
            ));
        }
        return Ok(
            json!({"report":serde_json::from_str::<Value>(&report).map_err(error)?,"duplicate":true}),
        );
    }
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    tx.execute("INSERT INTO agent_completions(author,author_title,project_id,request_id,payload,report,created) VALUES (?1,?2,?3,?4,?5,'{}',?6)", params![s.id,title,project,req.request_id,payload,created]).map_err(error)?;
    let id = tx.last_insert_rowid();
    let mut report = serde_json::to_value(req).map_err(error)?;
    let obj = report.as_object_mut().unwrap();
    obj.insert("id".into(), json!(id));
    obj.insert("author".into(), json!(s.id));
    obj.insert("authorTitle".into(), json!(title));
    obj.insert("projectId".into(), json!(project));
    obj.insert("created".into(), json!(created));
    tx.execute(
        "UPDATE agent_completions SET report=?2 WHERE id=?1",
        params![id, report.to_string()],
    )
    .map_err(error)?;
    tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,'completion_reported',?1,?2,?3)",params![s.id,format!("report:{id}"),created]).map_err(error)?;
    tx.commit().map_err(error)?;
    drop(store);
    s.data
        .lock()
        .unwrap()
        .continuation
        .stop("Explicit completion or blocked report");
    s.emit("completion_reported", json!({"reportId":id}));
    Ok(json!({"report":report,"duplicate":false}))
}
pub fn get(app: &App, s: &Session, req: Query) -> mail::Result<Value> {
    let grants = mail::access(app, s)?;
    if !grants.communicate && !mail::coordinator(&grants) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Completion report access is not granted",
        ));
    }
    let id = req.session_id.as_deref().unwrap_or(&s.id);
    let scope = if id == s.id {
        None
    } else {
        let target = mail::peer(app, s, id)?;
        if !mail::coordinator(&grants) {
            mail::require(app, &target, "communicate")?;
        }
        mail::project(s)
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
