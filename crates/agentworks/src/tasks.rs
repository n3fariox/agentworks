//! Minimal project tickets. Catalog updates are clone/save/replace under its mutex.
use crate::{ApiResult, App, catalog, communication as mail, fail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, io};

const MAX_REQUESTS: usize = 256;
const MAX_TASK_ID: u64 = 9_007_199_254_740_991;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub revision: u64,
    pub created: u64,
    pub updated: u64,
    pub created_by: String,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    Done,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectTasks {
    pub next_id: u64,
    pub tasks: Vec<Task>,
    pub requests: Vec<Receipt>,
}
impl Default for ProjectTasks {
    fn default() -> Self {
        Self {
            next_id: 1,
            tasks: vec![],
            requests: vec![],
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    actor: String,
    request_id: String,
    fingerprint: Value,
    response: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    status: Status,
    request_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Update {
    task_id: Value,
    revision: u64,
    title: Option<String>,
    body: Option<String>,
    status: Option<Status>,
    request_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Delete {
    task_id: Value,
    revision: u64,
    request_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Get {
    task_id: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {}
fn bad(s: &str) -> (StatusCode, String) {
    fail(StatusCode::BAD_REQUEST, s)
}
fn decode<T: serde::de::DeserializeOwned>(v: Value) -> mail::Result<T> {
    serde_json::from_value(v).map_err(|_| bad("Invalid task fields"))
}
fn title_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.chars().count() <= 160
}
fn request_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 100
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn uri(project: &str, id: u64) -> String {
    format!("agentworks://projects/{project}/tasks/{id}")
}
/// Numbers or canonical same-project URIs only; keys are display references, not identity.
pub fn task_ref(project: &str, v: &Value) -> Option<u64> {
    let n = v.as_u64().or_else(|| {
        let prefix = format!("agentworks://projects/{project}/tasks/");
        let s = v.as_str()?.strip_prefix(&prefix)?;
        let n: u64 = s.parse().ok()?;
        (s == n.to_string()).then_some(n)
    })?;
    (n > 0 && n <= MAX_TASK_ID).then_some(n)
}
fn view(p: &catalog::Project, t: &Task) -> Value {
    let mut v = serde_json::to_value(t).expect("serializable task");
    v["key"] = json!(format!("{}-{}", p.slug, t.id));
    v["uri"] = json!(uri(&p.id, t.id));
    v
}
pub fn validate_catalog(data: &catalog::CatalogData) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid task catalog");
    for (id, state) in &data.tasks {
        if !data.projects.iter().any(|p| &p.id == id)
            || state.next_id == 0
            || state.next_id > MAX_TASK_ID + 1
            || state.requests.len() > MAX_REQUESTS
        {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        for t in &state.tasks {
            if t.id == 0
                || t.id >= state.next_id
                || !ids.insert(t.id)
                || t.revision == 0
                || !title_ok(&t.title)
                || t.body.len() > 16384
                || t.created_by.is_empty()
                || t.updated < t.created
            {
                return Err(invalid());
            }
        }
        let mut receipts = HashSet::new();
        for r in &state.requests {
            if r.actor.is_empty()
                || !request_ok(&r.request_id)
                || !receipts.insert((&r.actor, &r.request_id))
                || !r.fingerprint.is_object()
                || !r.response.is_object()
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn execute(
    c: &mut catalog::Catalog,
    project: &str,
    actor: &str,
    args: Value,
    operation: &str,
) -> mail::Result<Value> {
    let p = c
        .data
        .projects
        .iter()
        .find(|p| p.id == project)
        .cloned()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Project not found"))?;
    if operation == "list_tasks" {
        let _: List = decode(args)?;
        let tasks: Vec<_> = c
            .data
            .tasks
            .get(project)
            .into_iter()
            .flat_map(|s| &s.tasks)
            .map(|t| view(&p, t))
            .collect();
        return Ok(json!({"tasks":tasks,"project":p}));
    }
    if operation == "get_task" {
        let req: Get = decode(args)?;
        let id = task_ref(project, &req.task_id)
            .ok_or_else(|| bad("Invalid taskId or project scope"))?;
        let t = c
            .data
            .tasks
            .get(project)
            .and_then(|s| s.tasks.iter().find(|t| t.id == id))
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Task not found"))?;
        return Ok(json!({"task":view(&p,t)}));
    }
    if p.archived {
        return Err(fail(
            StatusCode::CONFLICT,
            "Archived projects are read-only",
        ));
    }
    let request = args
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|s| request_ok(s))
        .ok_or_else(|| bad("Use a requestId between 1 and 100 bytes"))?
        .to_string();
    let fingerprint = json!({"operation":operation,"args":args});
    if let Some(r) = c.data.tasks.get(project).and_then(|s| {
        s.requests
            .iter()
            .find(|r| r.actor == actor && r.request_id == request)
    }) {
        if r.fingerprint != fingerprint {
            return Err(fail(
                StatusCode::CONFLICT,
                "requestId already used for different task operation",
            ));
        }
        return Ok(r.response.clone());
    }
    let mut data = c.data.clone();
    let state = data.tasks.entry(project.into()).or_default();
    let response = match operation {
        "create_task" => {
            let req: Create = decode(args)?;
            let _ = req.request_id;
            if !title_ok(&req.title) || req.body.len() > 16384 {
                return Err(bad(
                    "Use a title of 1-160 characters and body of at most 16384 bytes",
                ));
            }
            let id = state.next_id;
            if id > MAX_TASK_ID {
                return Err(fail(StatusCode::CONFLICT, "Task numbers exhausted"));
            }
            state.next_id = id
                .checked_add(1)
                .ok_or_else(|| fail(StatusCode::CONFLICT, "Task numbers exhausted"))?;
            let t = Task {
                id,
                title: req.title.trim().into(),
                body: req.body,
                status: req.status,
                revision: 1,
                created: now(),
                updated: now(),
                created_by: actor.into(),
            };
            let result = json!({"task":view(&p,&t)});
            state.tasks.push(t);
            result
        }
        "update_task" => {
            let req: Update = decode(args)?;
            let _ = req.request_id;
            let id = task_ref(project, &req.task_id)
                .ok_or_else(|| bad("Invalid taskId or project scope"))?;
            let t = state
                .tasks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Task not found"))?;
            if req.revision != t.revision {
                return Err(fail(StatusCode::CONFLICT, "Task revision conflict"));
            }
            if let Some(title) = req.title {
                if !title_ok(&title) {
                    return Err(bad("Use a title of 1-160 characters"));
                }
                t.title = title.trim().into();
            }
            if let Some(body) = req.body {
                if body.len() > 16384 {
                    return Err(bad("Body exceeds 16384 bytes"));
                }
                t.body = body;
            }
            if let Some(status) = req.status {
                t.status = status;
            }
            t.revision = t
                .revision
                .checked_add(1)
                .ok_or_else(|| fail(StatusCode::CONFLICT, "Task revision exhausted"))?;
            t.updated = now().max(t.updated);
            json!({"task":view(&p,t)})
        }
        "delete_task" => {
            let req: Delete = decode(args)?;
            let _ = req.request_id;
            let id = task_ref(project, &req.task_id)
                .ok_or_else(|| bad("Invalid taskId or project scope"))?;
            let t = state
                .tasks
                .iter()
                .find(|t| t.id == id)
                .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Task not found"))?;
            if req.revision != t.revision {
                return Err(fail(StatusCode::CONFLICT, "Task revision conflict"));
            }
            state.tasks.retain(|t| t.id != id);
            json!({"ok":true})
        }
        _ => return Err(bad("Unknown task operation")),
    };
    state.requests.push(Receipt {
        actor: actor.into(),
        request_id: request,
        fingerprint,
        response: response.clone(),
    });
    if state.requests.len() > MAX_REQUESTS {
        state.requests.remove(0);
    }
    c.save(data)
        .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "Could not save tasks"))?;
    Ok(response)
}
/// MCP caller holds the communication gate; scope and grants are always reread.
pub fn agent(app: &App, a: &mail::Actor, args: Value, operation: &str) -> mail::Result<Value> {
    let access = mail::actor_access(app, a)?;
    let write = !matches!(operation, "list_tasks" | "get_task");
    if (write && (a.session.is_none() || !mail::coordinator(&access)))
        || (!write && !access.communicate && !mail::coordinator(&access))
    {
        return Err(fail(StatusCode::FORBIDDEN, "Task capability not granted"));
    }
    let project = a
        .project
        .as_deref()
        .ok_or_else(|| fail(StatusCode::FORBIDDEN, "Tasks require project membership"))?;
    execute(
        &mut app.catalog.lock().unwrap(),
        project,
        &a.id,
        args,
        operation,
    )
}
pub async fn list(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(execute(
        &mut app.catalog.lock().unwrap(),
        &id,
        "human",
        json!({}),
        "list_tasks",
    )?))
}
pub async fn get(State(app): State<App>, Path((id, task)): Path<(String, u64)>) -> ApiResult {
    Ok(Json(execute(
        &mut app.catalog.lock().unwrap(),
        &id,
        "human",
        json!({"taskId":task}),
        "get_task",
    )?))
}
pub async fn create(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(args): Json<Value>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    Ok(Json(execute(
        &mut app.catalog.lock().unwrap(),
        &id,
        "human",
        args,
        "create_task",
    )?))
}
fn with_id(mut args: Value, id: u64) -> mail::Result<Value> {
    let obj = args
        .as_object_mut()
        .ok_or_else(|| bad("Invalid task fields"))?;
    if obj.contains_key("taskId") {
        return Err(bad("Use taskId in URL only"));
    }
    obj.insert("taskId".into(), json!(id));
    Ok(args)
}
pub async fn update(
    State(app): State<App>,
    Path((id, task)): Path<(String, u64)>,
    Json(args): Json<Value>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    Ok(Json(execute(
        &mut app.catalog.lock().unwrap(),
        &id,
        "human",
        with_id(args, task)?,
        "update_task",
    )?))
}
pub async fn delete(
    State(app): State<App>,
    Path((id, task)): Path<(String, u64)>,
    Json(args): Json<Value>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    Ok(Json(execute(
        &mut app.catalog.lock().unwrap(),
        &id,
        "human",
        with_id(args, task)?,
        "delete_task",
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (catalog::Catalog, String, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("agentworks-tasks-{}.json", uuid::Uuid::new_v4()));
        let mut c = catalog::Catalog::open(path.clone()).unwrap();
        let id = c.project(None, "Test project", None).unwrap().id;
        (c, id, path)
    }
    #[test]
    fn durable_retries_revisions_and_no_reuse() {
        let (mut c, p, path) = setup();
        let req = json!({"title":"First","body":"# Markdown","requestId":"create"});
        let first = execute(&mut c, &p, "human", req.clone(), "create_task").unwrap();
        assert_eq!(
            first,
            execute(&mut c, &p, "human", req, "create_task").unwrap()
        );
        assert_eq!(
            execute(
                &mut c,
                &p,
                "human",
                json!({"taskId":1,"revision":2,"requestId":"bad"}),
                "delete_task"
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let del = json!({"taskId":1,"revision":1,"requestId":"delete"});
        execute(&mut c, &p, "human", del.clone(), "delete_task").unwrap();
        c = catalog::Catalog::open(path.clone()).unwrap();
        assert_eq!(
            execute(&mut c, &p, "human", del, "delete_task").unwrap(),
            json!({"ok":true})
        );
        let next = execute(
            &mut c,
            &p,
            "human",
            json!({"title":"Second","requestId":"next"}),
            "create_task",
        )
        .unwrap();
        assert_eq!(next["task"]["id"], 2);
        c.project(Some(&p), "New name", None).unwrap();
        assert_eq!(next["task"]["uri"], uri(&p, 2));
        c.set_archived(&p, true).unwrap();
        assert!(execute(&mut c, &p, "human", json!({}), "list_tasks").is_ok());
        assert_eq!(
            execute(
                &mut c,
                &p,
                "human",
                json!({"title":"No","requestId":"archived"}),
                "create_task"
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn updates_conflicts_and_idempotency_payloads() {
        let (mut c, p, path) = setup();
        execute(
            &mut c,
            &p,
            "human",
            json!({"title":"First","requestId":"c"}),
            "create_task",
        )
        .unwrap();
        assert_eq!(
            execute(
                &mut c,
                &p,
                "human",
                json!({"title":"Different","requestId":"c"}),
                "create_task"
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let req = json!({"taskId":uri(&p,1),"revision":1,"title":"Updated","body":"**md**","status":"done","requestId":"u"});
        let updated = execute(&mut c, &p, "human", req.clone(), "update_task").unwrap();
        assert_eq!(updated["task"]["revision"], 2);
        assert_eq!(updated["task"]["status"], "done");
        assert_eq!(
            updated,
            execute(&mut c, &p, "human", req, "update_task").unwrap()
        );
        assert_eq!(
            execute(
                &mut c,
                &p,
                "human",
                json!({"taskId":1,"revision":1,"title":"Stale","requestId":"stale"}),
                "update_task"
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let old_uri = updated["task"]["uri"].clone();
        c.project_with_slug(Some(&p), "Renamed", None, Some("NEW"))
            .unwrap();
        let got = execute(&mut c, &p, "human", json!({"taskId":old_uri}), "get_task").unwrap();
        assert_eq!(got["task"]["key"], "NEW-1");
        assert_eq!(got["task"]["uri"], old_uri);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn save_failure_does_not_publish_and_invalid_state_fails_closed() {
        let (mut c, p, path) = setup();
        let before = serde_json::to_value(&c.data).unwrap();
        // A directory at the destination makes rename fail, without relying on permissions.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let result = execute(
            &mut c,
            &p,
            "human",
            json!({"title":"Unsaved","requestId":"failed"}),
            "create_task",
        );
        assert_eq!(result.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(serde_json::to_value(&c.data).unwrap(), before);
        std::fs::remove_dir(&path).unwrap();
        let _ = std::fs::remove_file(path.with_extension("json.tmp"));
        let mut data = c.data.clone();
        data.tasks.insert(
            p.clone(),
            ProjectTasks {
                next_id: 0,
                ..Default::default()
            },
        );
        assert!(validate_catalog(&data).is_err());
        std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(catalog::Catalog::open(path.clone()).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn strict_fields_and_scoped_uris() {
        assert_eq!(task_ref("p", &json!(uri("p", 1))), Some(1));
        assert_eq!(task_ref("p", &json!(uri("other", 1))), None);
        assert_eq!(task_ref("p", &json!(0)), None);
        assert_eq!(task_ref("p", &json!(MAX_TASK_ID + 1)), None);
        assert!(
            decode::<Create>(json!({"title":"a","requestId":"x","projectId":"other"})).is_err()
        );
        let (mut c, p, path) = setup();
        assert!(
            execute(
                &mut c,
                &p,
                "human",
                json!({"title":"a","body":"x".repeat(16385),"requestId":"x"}),
                "create_task"
            )
            .is_err()
        );
        std::fs::write(&path, b"{broken").unwrap();
        assert!(catalog::Catalog::open(path.clone()).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
