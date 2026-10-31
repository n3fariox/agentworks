//! Browser-independent PR tracking and a durable observation journal. Events
//! describe differences between snapshots, not exact GitHub delivery/actor facts.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS github_pr_tracking(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,scope TEXT,snapshot TEXT NOT NULL DEFAULT '{}',status TEXT NOT NULL DEFAULT 'unknown',observed_at INTEGER,next_poll INTEGER NOT NULL DEFAULT 0,hooks TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS github_pr_events(id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,payload TEXT NOT NULL,created INTEGER NOT NULL,delivered INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS github_pr_events_by_session ON github_pr_events(session_id,id);
";
const POLL_MS: i64 = crate::github::DEFAULT_POLL_MS as i64;
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn scope(context: &Value) -> Option<String> {
    let repo = &context["repository"];
    Some(
        json!([
            repo["host"].as_str()?.to_ascii_lowercase(),
            repo["owner"].as_str()?.to_ascii_lowercase(),
            repo["name"].as_str()?.to_ascii_lowercase(),
            context["connection"]["login"]
                .as_str()?
                .to_ascii_lowercase(),
            context["branch"]
        ])
        .to_string(),
    )
}
fn material(pr: &Value) -> Value {
    json!({"number":pr["number"],"url":pr["url"],"state":pr["state"],"draft":pr["draft"],"headSha":pr["headSha"],"headMatches":pr["headMatches"],"review":pr["reviewSummary"]["state"].as_str().or(pr["reviewDecision"].as_str()).unwrap_or("UNKNOWN"),"reviewComplete":pr["reviewSummary"]["complete"],"ci":pr["checks"]["state"],"checksComplete":pr["checks"]["complete"]})
}
fn hook_fingerprint(db: &Connection) -> rusqlite::Result<String> {
    use sha2::{Digest, Sha256};
    let mut rows = db.prepare("SELECT name,hash FROM scripts WHERE enabled=1 ORDER BY name")?;
    let values = rows
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(format!("{:x}", Sha256::digest(json!(values).to_string())))
}
/// Store the baseline and its new events in one transaction. A failed read is
/// availability information, never an empty PR list or a false review/check result.
fn observe(
    db: &mut Connection,
    id: &str,
    context: &Value,
    at: i64,
) -> Result<Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
    let hooks = hook_fingerprint(db)?;
    let tx = db.transaction()?;
    let previous: Option<(Option<String>, String, String, String)> = tx
        .query_row(
            "SELECT scope,snapshot,status,hooks FROM github_pr_tracking WHERE session_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let status = context["status"].as_str().unwrap_or("unavailable");
    let prior_status = previous.as_ref().map(|(_, _, s, _)| s.as_str());
    let mut baseline: Map<String, Value> = previous
        .as_ref()
        .map(|(_, s, _, _)| serde_json::from_str(s))
        .transpose()?
        .unwrap_or_default();
    let current_scope = scope(context);
    let old_scope = previous.as_ref().and_then(|(s, _, _, _)| s.clone());
    let mut events = Vec::new();
    if prior_status != Some(status) {
        events.push(json!({"kind":"github.availability_changed","initial":previous.is_none(),"before":prior_status,"after":status,"observedAt":at}));
    }
    if prior_status != Some(status) && status != "ready" {
        for old in baseline.values() {
            events.push(json!({"kind":"pr.unavailable","initial":false,"before":old,"after":null,"reason":status,"observedAt":at}));
        }
    }
    let mut saved_scope = old_scope.clone();
    if status == "ready" {
        let new_scope = current_scope.ok_or("Missing GitHub identity")?;
        let changed_scope = old_scope.as_ref() != Some(&new_scope);
        if changed_scope {
            for old in baseline.values() {
                events.push(json!({"kind":"pr.untracked","initial":false,"before":old,"after":null,"reason":"Repository, account or branch changed","observedAt":at}));
            }
            baseline.clear();
        }
        let new_hooks = previous.as_ref().is_none_or(|(_, _, _, h)| h != &hooks);
        let mut present = std::collections::HashSet::new();
        for pr in context["snapshot"]["resources"]
            .as_array()
            .ok_or("Invalid PR snapshot")?
            .iter()
            .filter(|p| p["kind"] == "pullRequest")
        {
            let key = pr["number"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("Invalid PR number")?
                .to_string();
            let after = material(pr);
            present.insert(key.clone());
            let before = baseline.get(&key);
            let fields: Vec<_> = after
                .as_object()
                .unwrap()
                .iter()
                .filter(|(k, v)| before.is_some_and(|b| b[*k] != **v))
                .map(|(k, _)| k.clone())
                .collect();
            if before != Some(&after) || new_hooks || prior_status != Some("ready") {
                let kind = if before.is_none() {
                    "pr.tracked"
                } else if before != Some(&after) {
                    "pr.status_changed"
                } else {
                    "pr.snapshot"
                };
                events.push(json!({"kind":kind,"initial":before.is_none()||kind=="pr.snapshot","before":before,"after":after,"changed":fields,"observedAt":at,"fetchedAt":context["snapshot"]["fetchedAt"]}));
            }
            baseline.insert(key, after);
        }
        // An incomplete candidate list cannot prove that an old PR disappeared.
        if context["snapshot"]["complete"] == true {
            let gone: Vec<_> = baseline
                .keys()
                .filter(|k| !present.contains(*k))
                .cloned()
                .collect();
            for key in gone {
                events.push(json!({"kind":"pr.untracked","initial":false,"before":baseline.remove(&key),"after":null,"reason":"PR no longer associated with this checkout","observedAt":at}));
            }
        }
        saved_scope = Some(new_scope);
    }
    for event in &mut events {
        let prior_scope = old_scope
            .as_ref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let use_prior = matches!(
            event["kind"].as_str(),
            Some("pr.untracked" | "pr.unavailable")
        );
        if let Some(prior) = prior_scope.filter(|_| use_prior || context["repository"].is_null()) {
            event["repository"] = json!({"host":prior[0],"owner":prior[1],"name":prior[2]});
            event["connection"] = json!({"host":prior[0],"login":prior[3]});
        } else {
            event["repository"] = context["repository"].clone();
            event["connection"] = context["connection"].clone();
        }
        tx.execute(
            "INSERT INTO github_pr_events(session_id,payload,created) VALUES (?1,?2,?3)",
            params![id, event.to_string(), at],
        )?;
        event["id"] = json!(tx.last_insert_rowid());
    }
    let retry = context["retryAt"]
        .as_i64()
        .unwrap_or_default()
        .max(
            context["nextPollAt"]
                .as_i64()
                .unwrap_or(at + context["pollIntervalMs"].as_i64().unwrap_or(POLL_MS)),
        )
        .max(at + 1000);
    // Preserve the previous hook fingerprint through an outage so newly enabled
    // handlers receive an initial snapshot when the source is readable again.
    let saved_hooks = if status == "ready" {
        hooks
    } else {
        previous.map(|(_, _, _, h)| h).unwrap_or_default()
    };
    tx.execute("INSERT INTO github_pr_tracking(session_id,scope,snapshot,status,observed_at,next_poll,hooks) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(session_id) DO UPDATE SET scope=excluded.scope,snapshot=excluded.snapshot,status=excluded.status,observed_at=excluded.observed_at,next_poll=excluded.next_poll,hooks=excluded.hooks",params![id,saved_scope,json!(baseline).to_string(),status,at,retry,saved_hooks])?;
    tx.commit()?;
    Ok(events)
}
async fn deliver(s: &Arc<Session>) -> Result<(), String> {
    let pending = {
        let store = s.store.lock().unwrap();
        let mut query = store.connection.prepare("SELECT id,payload FROM github_pr_events WHERE session_id=?1 AND delivered=0 ORDER BY id LIMIT 100").map_err(|_|"Could not read PR events")?;
        query
            .query_map([&s.id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|_| "Could not read PR events")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| "Could not read PR events")?
    };
    for (id, payload) in pending {
        let mut event: Value =
            serde_json::from_str(&payload).map_err(|_| "Invalid stored PR event")?;
        event["id"] = json!(id);
        let (hub, session, provider, cwd) = (
            s.progress.clone(),
            s.id.clone(),
            s.provider.clone(),
            s.cwd.to_string_lossy().into_owned(),
        );
        let accepted =
            tokio::task::spawn_blocking(move || hub.github_event(&session, &provider, &cwd, event))
                .await
                .unwrap_or(false);
        if !accepted {
            break;
        }
        let store = s.store.lock().unwrap();
        store
            .connection
            .execute("UPDATE github_pr_events SET delivered=1 WHERE id=?1", [id])
            .map_err(|_| "Could not acknowledge PR event")?;
        // Keep pending work and the latest 100 acknowledged observations per session.
        store.connection.execute("DELETE FROM github_pr_events WHERE session_id=?1 AND delivered=1 AND id < (SELECT COALESCE(min(id),0) FROM (SELECT id FROM github_pr_events WHERE session_id=?1 ORDER BY id DESC LIMIT 100))",[&s.id]).map_err(|_|"Could not prune PR events")?;
    }
    Ok(())
}
pub async fn refresh(app: &App, id: &str, query: crate::github::Refresh) -> ApiResult {
    let s = session(app, id)?;
    let gate = app.github.tracking_lock(id).await;
    let _guard = gate.lock().await;
    let Json(mut context) =
        crate::github::context(State(app.clone()), Path(id.into()), Query(query)).await?;
    if context.get("pollIntervalMs").is_none() {
        let old_scope: Option<String> = s
            .store
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT scope FROM github_pr_tracking WHERE session_id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        let old_host = old_scope
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|s| s[0].as_str().map(str::to_owned));
        context["pollIntervalMs"] =
            json!(old_host.map_or(crate::github::DEFAULT_POLL_MS, |host| {
                crate::github::poll_interval(app, &host)
            }));
    }
    let interval = context["pollIntervalMs"]
        .as_u64()
        .unwrap_or(crate::github::DEFAULT_POLL_MS);
    context["nextPollAt"] = json!(if context["status"] == "ready" {
        crate::github::snapshot_deadline(&context["snapshot"], interval)
    } else {
        context["retryAt"]
            .as_u64()
            .unwrap_or_default()
            .max(now() as u64 + interval)
    });
    crate::session_badges::sync_github(&s, &context)
        .map_err(|m| fail(StatusCode::INSUFFICIENT_STORAGE, m))?;
    let events =
        observe(&mut s.store.lock().unwrap().connection, id, &context, now()).map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not persist PR observations",
            )
        })?;
    for event in events {
        let _ = s.tx.send(json!({"kind":"github_event","data":event}));
    }
    deliver(&s)
        .await
        .map_err(|m| fail(StatusCode::INSUFFICIENT_STORAGE, m))?;
    // Hook setters may have contributed more badges since the native producer ran.
    let badges = crate::session_badges::stored_payload(&s);
    context
        .as_object_mut()
        .unwrap()
        .extend(badges.as_object().unwrap().clone());
    context["tracking"] = json!({"transport":"graphql_polling","intervalMs":context["pollIntervalMs"].as_i64().unwrap_or(POLL_MS),"browserIndependent":true,"observedAt":now()});
    let _ = s.tx.send(json!({"kind":"github_context","data":context}));
    Ok(Json(context))
}
#[derive(Default, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    after: i64,
}
pub async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult {
    let s = session(&app, &id)?;
    let store = s.store.lock().unwrap();
    let mut stmt = store.connection.prepare("SELECT id,payload,created FROM github_pr_events WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT 100").map_err(|_|fail(StatusCode::INTERNAL_SERVER_ERROR,"Could not read PR events"))?;
    let rows = stmt
        .query_map(params![id, query.after.max(0)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not read PR events",
            )
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not read PR events",
            )
        })?;
    let events: Vec<_> = rows
        .into_iter()
        .filter_map(|(id, payload, _)| {
            serde_json::from_str::<Value>(&payload)
                .ok()
                .map(|mut event| {
                    event["id"] = json!(id);
                    event
                })
        })
        .collect();
    Ok(Json(
        json!({"events":events,"limit":100,"retention":{"acknowledged":100,"pending":"untilDelivered"}}),
    ))
}
pub async fn scheduler(app: App) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        if let Err(error) = tick(&app).await {
            eprintln!("PR tracking failed: {error}");
        }
    }
}
async fn tick(app: &App) -> Result<(), String> {
    // No implicit monitoring before the user configures GitHub connections.
    if app
        .catalog
        .lock()
        .unwrap()
        .data
        .github
        .connections
        .is_empty()
    {
        return Ok(());
    }
    let sessions: Vec<_> = app
        .sessions
        .lock()
        .unwrap()
        .values()
        .filter(|s| !s.data.lock().unwrap().lifecycle.archived)
        .cloned()
        .collect();
    for s in sessions {
        let due = s
            .store
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT next_poll<=?2 FROM github_pr_tracking WHERE session_id=?1",
                params![s.id, now()],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .map_err(|_| "Could not read PR tracking schedule")?
            .unwrap_or(true);
        if due {
            if refresh(app, &s.id, crate::github::Refresh::default())
                .await
                .is_err()
            {
                // Root/remote-host failures are not an empty GitHub snapshot.
                s.store.lock().unwrap().connection.execute("INSERT INTO github_pr_tracking(session_id,next_poll) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET next_poll=excluded.next_poll",params![s.id,now()+POLL_MS]).map_err(|_|"Could not delay PR refresh")?;
            }
        } else {
            deliver(&s).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn pr() -> Value {
        json!({"kind":"pullRequest","number":17,"url":"https://github.com/team/repo/pull/17","state":"OPEN","draft":false,"headSha":"abc","headMatches":true,"reviewSummary":{"state":"REVIEW_REQUIRED","complete":true},"checks":{"state":"PENDING","complete":true,"items":[]}})
    }
    fn context(resource: Value) -> Value {
        json!({"status":"ready","repository":{"host":"github.com","owner":"team","name":"repo"},"connection":{"host":"github.com","login":"fixture"},"branch":"main","commit":"abc","snapshot":{"fetchedAt":now(),"complete":true,"resources":[resource]}})
    }
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE sessions(id TEXT PRIMARY KEY); INSERT INTO sessions VALUES ('s'); CREATE TABLE scripts(name TEXT,hash TEXT,enabled INTEGER);").unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db
    }
    #[test]
    fn status_changes_have_stable_ids_and_initial_snapshots_do_not_repeat() {
        let mut db = db();
        let mut value = context(pr());
        let first = observe(&mut db, "s", &value, 1).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1]["kind"], "pr.tracked");
        assert_eq!(first[1]["initial"], true);
        value["snapshot"]["fetchedAt"] = json!(20);
        assert!(observe(&mut db, "s", &value, 20).unwrap().is_empty());
        for (path, next, field) in [
            ("review", json!("APPROVED"), "review"),
            ("ci", json!("FAILURE"), "ci"),
            ("headSha", json!("def"), "headSha"),
            ("state", json!("MERGED"), "state"),
            ("draft", json!(true), "draft"),
        ] {
            match path {
                "review" => value["snapshot"]["resources"][0]["reviewSummary"]["state"] = next,
                "ci" => value["snapshot"]["resources"][0]["checks"]["state"] = next,
                _ => value["snapshot"]["resources"][0][path] = next,
            }
            let events = observe(&mut db, "s", &value, 30).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["kind"], "pr.status_changed");
            assert_eq!(events[0]["initial"], false);
            assert!(
                events[0]["changed"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(field))
            );
            assert!(observe(&mut db, "s", &value, 31).unwrap().is_empty());
        }
        let count: i64 = db
            .query_row("SELECT count(DISTINCT id) FROM github_pr_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 7);
    }
    #[test]
    fn outages_partial_results_scope_changes_and_new_hooks_are_explicit() {
        let mut db = db();
        let value = context(pr());
        observe(&mut db, "s", &value, 1).unwrap();
        let unavailable = json!({"status":"unavailable","retryAt":90000});
        let events = observe(&mut db, "s", &unavailable, 2).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["kind"], "github.availability_changed");
        assert_eq!(events[1]["kind"], "pr.unavailable");
        assert!(observe(&mut db, "s", &unavailable, 3).unwrap().is_empty());
        let events = observe(&mut db, "s", &value, 4).unwrap();
        assert_eq!(
            events.len(),
            2,
            "recovery sends availability and a fresh snapshot"
        );
        assert_eq!(events[1]["kind"], "pr.snapshot");
        let mut partial = value.clone();
        partial["snapshot"]["complete"] = json!(false);
        partial["snapshot"]["resources"] = json!([]);
        assert!(observe(&mut db, "s", &partial, 5).unwrap().is_empty());
        db.execute("INSERT INTO scripts VALUES ('new.star','version',1)", [])
            .unwrap();
        let events = observe(&mut db, "s", &value, 6).unwrap();
        assert_eq!(events[0]["kind"], "pr.snapshot");
        assert_eq!(events[0]["initial"], true);
        let mut moved = value.clone();
        moved["branch"] = json!("other");
        moved["repository"]["owner"] = json!("another");
        let events = observe(&mut db, "s", &moved, 7).unwrap();
        assert_eq!(events[0]["kind"], "pr.untracked");
        assert_eq!(events[1]["kind"], "pr.tracked");
        assert_eq!(events[0]["repository"]["owner"], "team");
        assert_eq!(events[1]["repository"]["owner"], "another");
        moved["snapshot"]["resources"] = json!([]);
        let events = observe(&mut db, "s", &moved, 8).unwrap();
        assert_eq!(events[0]["kind"], "pr.untracked");
    }
    pub(crate) struct Fixture {
        pub(crate) app: App,
        pub(crate) s: Arc<Session>,
        root: std::path::PathBuf,
    }
    impl Fixture {
        pub(crate) async fn new() -> Self {
            use std::sync::Mutex;
            let root = std::env::temp_dir().join(format!("pr-tracking-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            for args in [
                vec!["init", "-b", "main"],
                vec![
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@localhost",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ],
                vec![
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/team/repo.git",
                ],
            ] {
                assert!(
                    std::process::Command::new("git")
                        .args(args)
                        .current_dir(&root)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let store = Arc::new(Mutex::new(
                crate::store::Store::open(&root.join("db.sqlite3")).unwrap(),
            ));
            let catalog = Arc::new(Mutex::new(
                crate::Catalog::open(root.join("catalog.json")).unwrap(),
            ));
            let progress = crate::tool_progress::start(&store.lock().unwrap().connection).unwrap();
            let app = App {
                sessions: Default::default(),
                roots: Arc::new(vec![root.clone()]),
                port: 4505,
                bind: "127.0.0.1".parse().unwrap(),
                catalog: catalog.clone(),
                discovery: Arc::new(tokio::sync::Semaphore::new(1)),
                store: store.clone(),
                run_lock: Default::default(),
                workspace_lock: Default::default(),
                communication_lock: Default::default(),
                mcp_endpoint: "127.0.0.1:4505".parse().unwrap(),
                restart: Arc::new(crate::service_restart::Manager::new()),
                notifications: crate::notifications::Hub::new(catalog),
                hosts: crate::hosts::Hosts::new(),
                host_usage: Arc::new(crate::host_usage::Hub::default()),
                github: Arc::new(crate::github::Hub::default()),
                progress,
            };
            app.progress.bind_badge_sessions(&app.sessions);
            let config = crate::github::Config {
                connections: vec![crate::github::Connection {
                    host: "github.com".into(),
                    login: "fixture".into(),
                    enabled: true,
                }],
                ..Default::default()
            };
            app.catalog.lock().unwrap().github(config).unwrap();
            let req: crate::Create =
                serde_json::from_value(json!({"provider":"demo","cwd":root,"title":"PR tracking"}))
                    .unwrap();
            let Json(created) = crate::create_session(State(app.clone()), Json(req))
                .await
                .unwrap();
            let s = app
                .sessions
                .lock()
                .unwrap()
                .get(created["id"].as_str().unwrap())
                .unwrap()
                .clone();
            Self { app, s, root }
        }
        async fn seed(&self, pr: Value) {
            self.app
                .github
                .seed_pr_snapshot("main", context(pr)["snapshot"].clone())
                .await;
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(worker) = self.s.worker.lock().unwrap().take() {
                worker.abort();
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[tokio::test]
    async fn background_poll_updates_badges_and_hook_setters_without_a_browser_and_replays_durably()
    {
        let mut f = Fixture::new().await;
        let source = r#"load("@agentworks//github.star", "GithubContext")
def changed(ctx: GithubContext, event: dict[str, typing.Any]) -> None:
    if event["kind"] == "pr.status_changed" and event["after"]["review"] == "APPROVED":
        agent.set_badge("noticed", "Approved hook", tone = "success")
    return None
agent.register_github(changed)
"#;
        let _ = crate::tool_progress::put_script(
            State(f.app.clone()),
            Path("pr.star".into()),
            Json(serde_json::from_value(json!({"source":source})).unwrap()),
        )
        .await
        .unwrap();
        f.seed(pr()).await;
        tick(&f.app).await.unwrap();
        let first = crate::session_badges::stored_payload(&f.s);
        assert_eq!(first["badges"][0]["label"], "PR#17");
        let mut approved = pr();
        approved["reviewSummary"]["state"] = json!("APPROVED");
        approved["checks"]["state"] = json!("SUCCESS");
        f.seed(approved.clone()).await;
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute("UPDATE github_pr_tracking SET next_poll=0", [])
            .unwrap();
        tick(&f.app).await.unwrap();
        let badges = crate::session_badges::stored_payload(&f.s);
        assert!(
            badges["badges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["label"] == "Approved hook")
        );
        assert!(
            badges["badges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["label"] == "PR#17" && b["tone"] == "success")
        );
        // Lost acknowledgement after hook execution: replay retains event IDs and setters are idempotent.
        f.app.store.lock().unwrap().connection.execute("UPDATE github_pr_events SET delivered=0 WHERE json_extract(payload,'$.kind')='pr.status_changed'",[]).unwrap();
        let before = badges["badgeRevision"].clone();
        deliver(&f.s).await.unwrap();
        assert_eq!(
            crate::session_badges::stored_payload(&f.s)["badgeRevision"],
            before
        );
        let n: i64 = f
            .app
            .store
            .lock()
            .unwrap()
            .connection
            .query_row("SELECT count(*) FROM github_pr_events", [], |r| r.get(0))
            .unwrap();
        // A fresh in-memory hub observes the persisted baseline after restart.
        f.app.github = Arc::new(crate::github::Hub::default());
        f.seed(approved).await;
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute("UPDATE github_pr_tracking SET next_poll=0", [])
            .unwrap();
        tick(&f.app).await.unwrap();
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row("SELECT count(*) FROM github_pr_events", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            n
        );
        f.s.data.lock().unwrap().lifecycle.archived = true;
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute("UPDATE github_pr_tracking SET next_poll=0", [])
            .unwrap();
        tick(&f.app).await.unwrap();
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row("SELECT count(*) FROM github_pr_events", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            n
        );
    }
    #[tokio::test]
    async fn filters_skip_callback_evaluation_and_badges_are_bound_to_the_delivered_session() {
        let f = Fixture::new().await;
        let req: crate::Create = serde_json::from_value(
            json!({"provider":"demo","cwd":f.root,"title":"Second PR session"}),
        )
        .unwrap();
        let Json(created) = crate::create_session(State(f.app.clone()), Json(req))
            .await
            .unwrap();
        let other = f
            .app
            .sessions
            .lock()
            .unwrap()
            .get(created["id"].as_str().unwrap())
            .unwrap()
            .clone();
        let source = r#"load("@agentworks//github.star", "GithubContext")
def changed(ctx: GithubContext, event: dict[str, typing.Any]) -> None:
    # No inner event guard: an incorrectly routed initial/availability event
    # would fail the test by adding this badge.
    agent.set_badge("filtered", "Routed to session", tone = "info")
    return None
agent.register_github(changed, events = ["pr.status_changed"])
"#;
        let _ = crate::tool_progress::put_script(
            State(f.app.clone()),
            Path("filtered.star".into()),
            Json(serde_json::from_value(json!({"source":source})).unwrap()),
        )
        .await
        .unwrap();
        f.seed(pr()).await;
        tick(&f.app).await.unwrap();
        for s in [&f.s, &other] {
            assert!(
                !crate::session_badges::stored_payload(s)["badges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["id"] == "filtered")
            );
        }
        let mut approved = pr();
        approved["reviewSummary"]["state"] = json!("APPROVED");
        f.seed(approved).await;
        let _ = refresh(&f.app, &f.s.id, crate::github::Refresh::default())
            .await
            .unwrap();
        assert!(
            crate::session_badges::stored_payload(&f.s)["badges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["id"] == "filtered")
        );
        assert!(
            !crate::session_badges::stored_payload(&other)["badges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["id"] == "filtered")
        );
        // Event contents cannot redirect a setter to a different target session.
        let (hub, id, provider, cwd, forged) = (
            f.app.progress.clone(),
            f.s.id.clone(),
            f.s.provider.clone(),
            f.s.cwd.to_string_lossy().into_owned(),
            other.id.clone(),
        );
        assert!(
            tokio::task::spawn_blocking(move || hub.github_event(
                &id,
                &provider,
                &cwd,
                json!({"id":99999,"kind":"pr.status_changed","sessionId":forged})
            ))
            .await
            .unwrap()
        );
        assert!(
            !crate::session_badges::stored_payload(&other)["badges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["id"] == "filtered")
        );
        let _ = refresh(&f.app, &other.id, crate::github::Refresh::default())
            .await
            .unwrap();
        assert!(
            crate::session_badges::stored_payload(&other)["badges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["id"] == "filtered")
        );
        if let Some(worker) = other.worker.lock().unwrap().take() {
            worker.abort();
        }
    }
}
