//! Explicit review ledger and durable manual integration reservation. Never runs Git.
use crate::{ApiResult, App, communication as mail, fail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS review_items(id INTEGER PRIMARY KEY AUTOINCREMENT,project TEXT NOT NULL,state TEXT NOT NULL,item TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS review_one_integration ON review_items(state) WHERE state='applying';
CREATE TABLE IF NOT EXISTS review_events(id INTEGER PRIMARY KEY AUTOINCREMENT,project TEXT NOT NULL,actor TEXT NOT NULL,request_id TEXT NOT NULL,payload TEXT NOT NULL,response TEXT NOT NULL,created INTEGER NOT NULL,UNIQUE(actor,request_id));
CREATE INDEX IF NOT EXISTS review_project_events ON review_events(project,id);
";

#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum Operation {
    Propose {
        title: String,
        branch: String,
        commits: Vec<String>,
        target: String,
        base: String,
    },
    Review {
        id: i64,
        revision: u64,
        result: ReviewResult,
        note: String,
    },
    Test {
        id: i64,
        revision: u64,
        result: TestResult,
        note: String,
    },
    Begin {
        id: i64,
        revision: u64,
        note: String,
    },
    Finish {
        id: i64,
        revision: u64,
        outcome: Outcome,
        commit: Option<String>,
        note: String,
    },
    Reject {
        id: i64,
        revision: u64,
        note: String,
    },
}
#[derive(Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ReviewResult {
    Approved,
    ChangesRequested,
}
#[derive(Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TestResult {
    Passed,
    Failed,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Integrated,
    Failed,
    Cancelled,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub request_id: String,
    #[serde(flatten)]
    pub operation: Operation,
}
// Serde's flatten and deny_unknown_fields cannot be combined. Remove only the
// envelope key, then strictly decode the action-specific fields.
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(d)?;
        let request_id = value
            .as_object_mut()
            .and_then(|v| v.remove("requestId"))
            .ok_or_else(|| serde::de::Error::custom("Missing requestId"))?;
        let request_id = serde_json::from_value(request_id).map_err(serde::de::Error::custom)?;
        let operation = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            request_id,
            operation,
        })
    }
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: i64,
    project: String,
    title: String,
    branch: String,
    commits: Vec<String>,
    target: String,
    base: String,
    revision: u64,
    state: String,
    review: Option<ReviewResult>,
    test: Option<TestResult>,
    integration_commit: Option<String>,
}
fn error(_: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not persist or read review queue",
    )
}
fn invalid(message: &str) -> (StatusCode, String) {
    fail(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: &str) -> (StatusCode, String) {
    fail(StatusCode::CONFLICT, message)
}
fn bounded(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Authorization precedes this call, including duplicate lookups. Each successful
/// operation, its immutable response and its audit row commit in one transaction.
pub fn update(
    db: &mut Connection,
    project: &str,
    actor: &str,
    req: Request,
) -> mail::Result<Value> {
    if !bounded(&req.request_id, 100) {
        return Err(invalid("Invalid requestId"));
    }
    let payload = json!({"project":project,"request":req}).to_string();
    if payload.len() > 24000 {
        return Err(invalid("Review request exceeds 24000 bytes"));
    }
    let tx = db.transaction().map_err(error)?;
    let prior: Option<(String, String)> = tx
        .query_row(
            "SELECT payload,response FROM review_events WHERE actor=?1 AND request_id=?2",
            params![actor, req.request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    if let Some((old, response)) = prior {
        if old != payload {
            return Err(conflict(
                "Request ID already used for different review operation",
            ));
        }
        let mut response: Value = serde_json::from_str(&response).map_err(error)?;
        response["duplicate"] = json!(true);
        return Ok(response);
    }
    let mut item = match &req.operation {
        Operation::Propose {
            title,
            branch,
            commits,
            target,
            base,
        } => {
            if !bounded(title, 256)
                || !bounded(branch, 256)
                || !bounded(target, 256)
                || !oid(base)
                || commits.is_empty()
                || commits.len() > 32
                || commits.iter().any(|c| !oid(c))
                || commits
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != commits.len()
            {
                return Err(invalid(
                    "Proposal requires title, source/target branches, full base and 1–32 distinct full commit IDs",
                ));
            }
            tx.execute(
                "INSERT INTO review_items(project,state,item) VALUES(?1,'queued','{}')",
                [project],
            )
            .map_err(error)?;
            Item {
                id: tx.last_insert_rowid(),
                project: project.into(),
                title: title.clone(),
                branch: branch.clone(),
                commits: commits.clone(),
                target: target.clone(),
                base: base.clone(),
                revision: 0,
                state: "queued".into(),
                review: None,
                test: None,
                integration_commit: None,
            }
        }
        op => {
            let (id, revision, note) = match op {
                Operation::Review {
                    id, revision, note, ..
                }
                | Operation::Test {
                    id, revision, note, ..
                }
                | Operation::Begin { id, revision, note }
                | Operation::Finish {
                    id, revision, note, ..
                }
                | Operation::Reject { id, revision, note } => (*id, *revision, note),
                _ => unreachable!(),
            };
            if note.trim().is_empty() || note.len() > 4096 {
                return Err(invalid(
                    "An evidence or decision note of 1–4096 bytes is required",
                ));
            }
            let raw: Option<String> = tx
                .query_row(
                    "SELECT item FROM review_items WHERE id=?1 AND project=?2",
                    params![id, project],
                    |r| r.get(0),
                )
                .optional()
                .map_err(error)?;
            let mut item: Item = serde_json::from_str(&raw.ok_or_else(|| {
                fail(
                    StatusCode::NOT_FOUND,
                    "Review item not found in this project",
                )
            })?)
            .map_err(error)?;
            if item.revision != revision {
                return Err(conflict("Review item changed; refresh before deciding"));
            }
            if matches!(item.state.as_str(), "integrated" | "rejected" | "cancelled") {
                return Err(conflict("Review item is terminal; propose a new item"));
            }
            if item.state == "applying" && !matches!(op, Operation::Finish { .. }) {
                return Err(conflict(
                    "Integration reserved; inspect and record its outcome first",
                ));
            }
            match op {
                Operation::Review { result, .. } => item.review = Some(result.clone()),
                Operation::Test { result, .. } => item.test = Some(result.clone()),
                Operation::Begin { .. } => {
                    if item.review != Some(ReviewResult::Approved)
                        || item.test != Some(TestResult::Passed)
                    {
                        return Err(conflict(
                            "Approved review and passed tests required before integration",
                        ));
                    }
                    let busy: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM review_items WHERE state='applying')",
                            [],
                            |r| r.get(0),
                        )
                        .map_err(error)?;
                    if busy {
                        return Err(conflict("Another integration holds the service-wide slot"));
                    }
                    item.state = "applying".into();
                }
                Operation::Finish {
                    outcome, commit, ..
                } => {
                    if item.state != "applying" {
                        return Err(conflict("Begin integration before recording its outcome"));
                    }
                    match outcome {
                        Outcome::Integrated => {
                            if !commit.as_deref().is_some_and(oid) {
                                return Err(invalid(
                                    "Integrated outcome requires full resulting commit ID",
                                ));
                            }
                            item.state = "integrated".into();
                            item.integration_commit = commit.clone();
                        }
                        Outcome::Failed => {
                            item.state = "failed".into();
                            item.review = None;
                            item.test = None;
                        }
                        Outcome::Cancelled => item.state = "cancelled".into(),
                    }
                    if !matches!(outcome, Outcome::Integrated) && commit.is_some() {
                        return Err(invalid("Only integrated outcomes take a commit"));
                    }
                }
                Operation::Reject { .. } => item.state = "rejected".into(),
                _ => unreachable!(),
            }
            item
        }
    };
    item.revision += 1;
    let raw = serde_json::to_string(&item).map_err(error)?;
    tx.execute(
        "UPDATE review_items SET state=?2,item=?3 WHERE id=?1",
        params![item.id, item.state, raw],
    )
    .map_err(error)?;
    let response = json!({"item":item,"duplicate":false});
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    tx.execute("INSERT INTO review_events(project,actor,request_id,payload,response,created) VALUES(?1,?2,?3,?4,?5,?6)",params![project,actor,req.request_id,payload,response.to_string(),created]).map_err(error)?;
    tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES(?1,'review_queue',?2,?3,?4)",params![actor,item.id.to_string(),payload,created]).map_err(error)?;
    tx.commit().map_err(error)?;
    Ok(response)
}

pub fn list(db: &Connection, project: &str, after: i64) -> mail::Result<Value> {
    if after < 0 {
        return Err(invalid("Invalid review cursor"));
    }
    let mut q=db.prepare("SELECT response,actor,request_id,created,id FROM review_events WHERE project=?1 AND id>?2 ORDER BY id LIMIT 101").map_err(error)?;
    let mut events=q.query_map(params![project,after],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?))).map_err(error)?.map(|row|{let (raw,actor,request,created,id)=row.map_err(error)?;Ok(json!({"id":id,"actor":actor,"requestId":request,"created":created,"response":serde_json::from_str::<Value>(&raw).map_err(error)?}))}).collect::<mail::Result<Vec<_>>>()?;
    let has_more = events.len() > 100;
    events.truncate(100);
    // Include the full operation/evidence in the history, not just the resulting state.
    for event in &mut events {
        let payload: String = db
            .query_row(
                "SELECT payload FROM review_events WHERE id=?1",
                [event["id"].as_i64()],
                |r| r.get(0),
            )
            .map_err(error)?;
        event["operation"] =
            serde_json::from_str::<Value>(&payload).map_err(error)?["request"].clone();
    }
    let mut q = db
        .prepare("SELECT item FROM review_items WHERE project=?1 ORDER BY id")
        .map_err(error)?;
    let items = q
        .query_map([project], |r| r.get::<_, String>(0))
        .map_err(error)?
        .map(|row| serde_json::from_str::<Value>(&row.map_err(error)?).map_err(error))
        .collect::<mail::Result<Vec<_>>>()?;
    Ok(
        json!({"items":items,"nextCursor":events.last().map(|e|e["id"].clone()).unwrap_or(json!(after)),"events":events,"hasMore":has_more}),
    )
}
/// Deletion must not orphan the durable global reservation. Caller holds the
/// communication lock, shared with all begins/finishes.
pub fn ensure_project_removable(db: &Connection, project: &str) -> mail::Result<()> {
    let active: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM review_items WHERE project=?1 AND state='applying')",
            [project],
            |r| r.get(0),
        )
        .map_err(error)?;
    if active {
        return Err(conflict(
            "Resolve this project's manual integration reservation before deleting it",
        ));
    }
    Ok(())
}
fn project_exists(app: &App, id: &str) -> mail::Result<()> {
    if !app.catalog.lock().unwrap().contains(id) {
        return Err(fail(StatusCode::NOT_FOUND, "Project not found"));
    }
    Ok(())
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    #[serde(default)]
    pub after: i64,
}
pub async fn get(
    State(app): State<App>,
    Path(project): Path<String>,
    axum::extract::Query(cursor): axum::extract::Query<Cursor>,
) -> ApiResult {
    project_exists(&app, &project)?;
    Ok(Json(list(
        &app.store.lock().unwrap().connection,
        &project,
        cursor.after,
    )?))
}
pub async fn post(
    State(app): State<App>,
    Path(project): Path<String>,
    Json(req): Json<Request>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    project_exists(&app, &project)?;
    Ok(Json(update(
        &mut app.store.lock().unwrap().connection,
        &project,
        "human",
        req,
    )?))
}
/// MCP scope comes only from authenticated session state; no caller-supplied actor/project.
pub fn agent(app: &App, s: &crate::Session, args: Value, write: bool) -> mail::Result<Value> {
    mail::require(app, s, "manage_review_queue")?;
    let project = mail::project(s).ok_or_else(|| {
        fail(
            StatusCode::FORBIDDEN,
            "Review queue requires project membership",
        )
    })?;
    project_exists(app, &project)?;
    if write {
        let req =
            serde_json::from_value(args).map_err(|_| invalid("Invalid review operation fields"))?;
        update(
            &mut app.store.lock().unwrap().connection,
            &project,
            &s.id,
            req,
        )
    } else {
        let cursor: Cursor =
            serde_json::from_value(args).map_err(|_| invalid("Invalid review query"))?;
        list(
            &app.store.lock().unwrap().connection,
            &project,
            cursor.after,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE agent_audit(id INTEGER PRIMARY KEY,actor TEXT,action TEXT,target TEXT,outcome TEXT,created INTEGER);").unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db
    }
    fn proposal(key: &str) -> Value {
        json!({"action":"propose","requestId":key,"title":"Feature","branch":"worker","commits":["a".repeat(40)],"target":"main","base":"b".repeat(40)})
    }
    fn run(db: &mut Connection, project: &str, v: Value) -> mail::Result<Value> {
        update(db, project, "human", serde_json::from_value(v).unwrap())
    }
    fn act(db: &mut Connection, item: &Value, action: &str, extra: Value) -> mail::Result<Value> {
        let mut v = json!({"action":action,"requestId":uuid::Uuid::new_v4().to_string(),"id":item["id"],"revision":item["revision"],"note":"Evidence and deliberate decision"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        run(db, item["project"].as_str().unwrap(), v)
    }
    fn ready(db: &mut Connection, key: &str) -> Value {
        let i = run(db, "p", proposal(key)).unwrap()["item"].clone();
        let i = act(db, &i, "review", json!({"result":"approved"})).unwrap()["item"].clone();
        act(db, &i, "test", json!({"result":"passed"})).unwrap()["item"].clone()
    }
    #[test]
    fn strict_contract_and_bounds() {
        let mut db = db();
        assert!(serde_json::from_value::<Request>(proposal("valid")).is_ok());
        for extra in [
            json!({"actor":"spoof"}),
            json!({"project":"other"}),
            json!({"result":"approved"}),
        ] {
            let mut v = proposal("bad");
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(serde_json::from_value::<Request>(v).is_err());
        }
        for patch in [
            json!({"commits":[]}),
            json!({"commits":["short"]}),
            json!({"base":"main"}),
            json!({"target":""}),
            json!({"requestId":"bad\nkey"}),
            json!({"commits":vec!["a".repeat(40);33]}),
        ] {
            let mut v = proposal("bounds");
            v.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert_eq!(run(&mut db, "p", v).unwrap_err().0, StatusCode::BAD_REQUEST);
        }
        assert!(list(&db, "p", -1).is_err());
    }
    #[test]
    fn independent_evidence_and_terminal_states() {
        let mut db = db();
        let i = run(&mut db, "p", proposal("one")).unwrap()["item"].clone();
        assert!(i["review"].is_null());
        assert!(i["test"].is_null());
        assert_eq!(
            act(&mut db, &i, "begin", json!({})).unwrap_err().0,
            StatusCode::CONFLICT
        );
        let i = act(&mut db, &i, "review", json!({"result":"approved"})).unwrap()["item"].clone();
        assert!(act(&mut db, &i, "begin", json!({})).is_err());
        let i = act(&mut db, &i, "test", json!({"result":"failed"})).unwrap()["item"].clone();
        assert!(act(&mut db, &i, "begin", json!({})).is_err());
        let i = act(&mut db, &i, "test", json!({"result":"passed"})).unwrap()["item"].clone();
        let i = act(&mut db, &i, "begin", json!({})).unwrap()["item"].clone();
        assert!(act(&mut db, &i, "review", json!({"result":"changesRequested"})).is_err());
        assert!(act(&mut db, &i, "finish", json!({"outcome":"integrated"})).is_err());
        let i = act(
            &mut db,
            &i,
            "finish",
            json!({"outcome":"integrated","commit":"c".repeat(40)}),
        )
        .unwrap()["item"]
            .clone();
        assert_eq!(i["state"], "integrated");
        assert_eq!(i["integrationCommit"], "c".repeat(40));
        assert!(ensure_project_removable(&db, "p").is_ok());
        assert!(act(&mut db, &i, "begin", json!({})).is_err());
        let ledger = list(&db, "p", 0).unwrap();
        assert_eq!(ledger["events"].as_array().unwrap().len(), 6);
        assert_eq!(ledger["events"][1]["operation"]["result"], "approved");
    }
    #[test]
    fn global_slot_failure_reapproval_and_retry_safety() {
        let mut db = db();
        let a = ready(&mut db, "one");
        let mut b = ready(&mut db, "two");
        let begin = json!({"action":"begin","id":a["id"],"revision":a["revision"],"note":"Start manual integration","requestId":"begin"});
        let first = run(&mut db, "p", begin.clone()).unwrap();
        assert_eq!(run(&mut db, "p", begin.clone()).unwrap()["duplicate"], true);
        assert!(act(&mut db, &b, "begin", json!({})).is_err());
        assert!(ensure_project_removable(&db, "p").is_err());
        assert!(ensure_project_removable(&db, "other").is_ok());
        assert!(
            db.execute(
                "UPDATE review_items SET state='applying' WHERE id=?1",
                [b["id"].as_i64()]
            )
            .is_err(),
            "SQL enforces global slot too"
        );
        let mut changed = begin.clone();
        changed["note"] = json!("Changed");
        assert_eq!(
            run(&mut db, "p", changed).unwrap_err().0,
            StatusCode::CONFLICT
        );
        assert!(run(&mut db, "other", begin).is_err());
        let failed = act(
            &mut db,
            &first["item"],
            "finish",
            json!({"outcome":"failed"}),
        )
        .unwrap()["item"]
            .clone();
        assert!(failed["review"].is_null());
        assert!(failed["test"].is_null());
        assert!(act(&mut db, &failed, "begin", json!({})).is_err());
        // Another project cannot alter a known item ID; the slot also covers it.
        let cross = json!({"action":"reject","id":b["id"],"revision":b["revision"],"note":"Wrong project","requestId":"cross"});
        assert_eq!(
            run(&mut db, "other", cross).unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        let stale = b.clone();
        b = act(&mut db, &b, "begin", json!({})).unwrap()["item"].clone();
        assert!(act(&mut db, &stale, "reject", json!({})).is_err());
        let done = act(&mut db, &b, "finish", json!({"outcome":"cancelled"})).unwrap();
        assert_eq!(done["item"]["state"], "cancelled");
        // Old retries are snapshots and never change the current state.
        let retry=run(&mut db,"p",json!({"action":"begin","id":a["id"],"revision":a["revision"],"note":"Start manual integration","requestId":"begin"})).unwrap();
        assert_eq!(retry["item"], first["item"]);
        assert_eq!(list(&db, "p", 0).unwrap()["items"][0]["state"], "failed");
    }
    #[test]
    fn transaction_failure_rolls_back_state_and_key() {
        let mut db = db();
        db.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON agent_audit BEGIN SELECT RAISE(FAIL,'disk failure'); END;").unwrap();
        assert!(run(&mut db, "p", proposal("atomic")).is_err());
        assert_eq!(list(&db, "p", 0).unwrap()["items"], json!([]));
        db.execute_batch("DROP TRIGGER fail_audit;").unwrap();
        assert_eq!(
            run(&mut db, "p", proposal("atomic")).unwrap()["duplicate"],
            false
        );
    }
    #[test]
    fn restart_retains_reservation_and_immutable_history() {
        let root = std::env::temp_dir().join(format!("review-{}", uuid::Uuid::new_v4()));
        let path = root.join("db.sqlite3");
        let (a, b);
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            a = ready(&mut store.connection, "one");
            b = ready(&mut store.connection, "two");
            act(&mut store.connection, &a, "begin", json!({})).unwrap();
            store
                .connection
                .execute_batch("PRAGMA user_version=7; DROP INDEX review_project_events;")
                .unwrap();
        }
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            let ledger = list(&store.connection, "p", 0).unwrap();
            assert_eq!(ledger["items"][0]["state"], "applying");
            assert!(act(&mut store.connection, &b, "begin", json!({})).is_err());
            assert_eq!(
                run(&mut store.connection, "p", proposal("one")).unwrap()["duplicate"],
                true
            );
            assert!(
                !crate::communication::Role::Coordinator
                    .access()
                    .manage_review_queue
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
