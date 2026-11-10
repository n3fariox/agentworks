//! L-05 explicit human queued inputs. Distinct from the internal `queued_prompt`
//! used by automation/handoff. Queue items persist inside session state (serde
//! default); request-ID bindings live in the module-owned `input_queue_bindings`
//! table created at startup, so there is no schema version change.
//!
//! Every API mutation is saved before it is acknowledged and rolled back if the save
//! fails. The runtime only records a turn outcome; `scheduler` dispatches under the
//! shared communication -> run -> workspace locks through the same acceptance path as
//! a direct prompt, after drain, root, idle, suspend, storage, quota and pause gates.
//! Anything other than a clean `end_turn` pauses the queue for deliberate resume. An
//! item found `sending` after restart is ambiguous: it is marked `error` and never
//! replayed automatically.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Request-ID bindings keep the exact original payload and outlive display pruning,
/// removal, sending and restarts. When full, new enrollment is refused rather than
/// forgetting old IDs.
pub const MAX_BINDINGS: i64 = 1000;
pub const MAX_BINDING_BYTES: i64 = 4 * 1024 * 1024;

use agentworks_session::SessionData;
use agentworks_session::queue::{MAX_REQUEST_ID, validate};
pub use agentworks_session::queue::{Outcome, Queue};
type Res<T> = Result<T, (StatusCode, String)>;

fn db(error: impl std::fmt::Display) -> (StatusCode, String) {
    eprintln!(
        "Queue binding storage error: {}",
        agentworks_runtime::provider::safe_error(&error.to_string())
    );
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Queue storage is unavailable",
    )
}

/// Module-owned table, created at startup (no schema version change).
pub fn initialize(app: &App) -> Result<(), String> {
    app.store
        .lock()
        .unwrap()
        .connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS input_queue_bindings(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,request_id TEXT NOT NULL,payload TEXT NOT NULL,item_id TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(session_id,request_id));",
        )
        .map_err(|e| e.to_string())
}

pub enum Bound {
    New,
    Duplicate(String),
    Conflict,
}

/// Exact-payload check of a request ID against the durable bindings.
pub fn check_binding(s: &Session, request_id: &str, text: &str) -> Res<Bound> {
    let store = s.store.lock().unwrap();
    let row: Option<(String, String)> = store
        .connection
        .query_row(
            "SELECT payload,item_id FROM input_queue_bindings WHERE session_id=?1 AND request_id=?2",
            params![s.id, request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    Ok(match row {
        Some((payload, item)) if payload == text => Bound::Duplicate(item),
        Some(_) => Bound::Conflict,
        None => Bound::New,
    })
}

pub fn conflict() -> (StatusCode, String) {
    fail(
        StatusCode::CONFLICT,
        "Request ID was already used for a different message",
    )
}

/// Coordinator hook for inbox wake integration: true when explicit human input is
/// pending or unresolved.
#[allow(dead_code)]
pub fn has_pending(s: &Session) -> bool {
    s.data.lock().unwrap().input_queue.has_pending()
}

/// Applies `change` and persists it (plus an optional request binding for the newest
/// item) in one SQLite transaction; on failure the queue is restored and nothing is
/// acknowledged.
fn transact<T>(
    s: &Session,
    bind: Option<(&str, &str)>,
    change: impl FnOnce(&mut Queue) -> Res<T>,
) -> Res<T> {
    transact_session(s, bind, |data| change(&mut data.input_queue))
}

/// `transact` for changes that must see session status under the same lock.
fn transact_session<T>(
    s: &Session,
    bind: Option<(&str, &str)>,
    change: impl FnOnce(&mut SessionData) -> Res<T>,
) -> Res<T> {
    let result = {
        let mut data = s.data.lock().unwrap();
        if data.storage_error.is_some() {
            return Err(fail(
                StatusCode::CONFLICT,
                "Session storage failed; queue is unavailable",
            ));
        }
        let before = data.input_queue.clone();
        let value = change(&mut data)?;
        if data.input_queue != before {
            let item = data
                .input_queue
                .items
                .last()
                .map(|i| i.id.clone())
                .unwrap_or_default();
            let mut store = s.store.lock().unwrap();
            let saved = match store.connection.execute_batch("BEGIN IMMEDIATE") {
                Err(e) => Err(e.to_string()),
                Ok(()) => {
                    let inner = (|| -> Result<(), String> {
                        if let Some((request_id, text)) = bind {
                            store
                                .connection
                                .execute(
                                    "INSERT INTO input_queue_bindings(session_id,request_id,payload,item_id,created) VALUES (?1,?2,?3,?4,?5)",
                                    params![s.id, request_id, text, item, crate::timeouts::now_ms() as i64],
                                )
                                .map_err(|e| e.to_string())?;
                        }
                        store.save_state(&s.id, &data).map_err(|e| e.to_string())
                    })();
                    match inner {
                        Ok(()) => match store.connection.execute_batch("COMMIT") {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                let _ = store.connection.execute_batch("ROLLBACK");
                                Err(e.to_string())
                            }
                        },
                        Err(e) => {
                            let _ = store.connection.execute_batch("ROLLBACK");
                            Err(e)
                        }
                    }
                }
            };
            drop(store);
            if let Err(error) = saved {
                data.input_queue = before;
                eprintln!(
                    "Could not persist queue for {}: {}",
                    s.id,
                    agentworks_runtime::provider::safe_error(&error)
                );
                return Err(fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Queue change could not be saved; nothing was changed",
                ));
            }
        }
        value
    };
    broadcast(s);
    Ok(result)
}

/// Queue state is broadcast unsequenced so it does not consume transcript history.
pub(crate) fn broadcast(s: &Session) {
    let queue = s.data.lock().unwrap().input_queue.clone();
    let _ = s.tx.send(json!({"kind":"input_queue","data":queue}));
}

/// Runtime hook at turn end: records the outcome only (never dispatches).
pub fn record_outcome(s: &Session, mut outcome: Outcome) {
    {
        let mut data = s.data.lock().unwrap();
        if outcome == Outcome::Success {
            if data.storage_error.is_some() {
                outcome = Outcome::StorageFailure;
            } else if data.continuation.limited || data.continuation.due_at.is_some() {
                outcome = Outcome::Quota;
            }
        }
        let before = data.input_queue.clone();
        data.input_queue.finish_turn(&outcome);
        if data.input_queue == before {
            return;
        }
        if s.store.lock().unwrap().save_state(&s.id, &data).is_err() {
            // Cannot record durably: never dispatch on unsaved state.
            data.input_queue.block("Storage failure; queue paused.");
        }
    }
    broadcast(s);
}

/// Dispatches one queued item. Caller holds communication/run/workspace locks and
/// passed `allow_start`. Returns true when a turn was accepted.
async fn dispatch_next(app: &App, s: &Arc<Session>, deliberate: bool) -> bool {
    {
        let data = s.data.lock().unwrap();
        if !(deliberate || data.input_queue.ready) {
            return false;
        }
    }
    // Root policy is re-validated at dispatch, like other launches.
    if crate::hosts::validate_session(app, s).await.is_err() {
        let mut data = s.data.lock().unwrap();
        data.input_queue
            .block("Session host, harness or directory is unavailable; queue paused.");
        let _ = s.store.lock().unwrap().save_state(&s.id, &data);
        drop(data);
        broadcast(s);
        return false;
    }
    if s.commands.lock().unwrap().is_none() {
        // Asleep is not unavailable: wake it, and the item runs once it is idle.
        if crate::idle_sleep::asleep(s) {
            let _ = crate::idle_sleep::wake(app, s).await;
            return false;
        }
        let _ = transact(s, None, |q| {
            q.block("Native session is unavailable; inspect and resume the session first.");
            Ok(())
        });
        return false;
    }
    let (text, id) = {
        let mut data = s.data.lock().unwrap();
        let eligible = (deliberate || data.input_queue.ready)
            && data.status == "idle"
            && !data.turn_active
            && !data.lifecycle.suspended
            && !data.lifecycle.archived
            && data.provider_session_id.is_some()
            && data.storage_error.is_none()
            && !(data.continuation.limited || data.continuation.due_at.is_some());
        if !eligible {
            if data.input_queue.ready && data.status != "idle" {
                // Another turn started first; that turn's outcome decides readiness.
                data.input_queue.ready = false;
            }
            return false;
        }
        let before = data.input_queue.clone();
        data.input_queue.ready = false;
        let Some(claimed) = data.input_queue.claim() else {
            if data.input_queue != before {
                let _ = s.store.lock().unwrap().save_state(&s.id, &data);
            }
            return false;
        };
        // The sending claim must be durable before dispatch so a crash is detectable.
        if s.store.lock().unwrap().save_state(&s.id, &data).is_err() {
            data.input_queue = before;
            data.input_queue
                .block("Storage failure; queue paused before sending.");
            drop(data);
            broadcast(s);
            return false;
        }
        claimed
    };
    broadcast(s);
    dispatch_claimed(s, text, id)
}

fn dispatch_claimed(s: &Session, text: String, id: String) -> bool {
    match crate::accept_prompt(s, text, id, Some("queue".into()), None, Vec::new()) {
        Ok(_) => {
            broadcast(s);
            true
        }
        Err((_, message)) => {
            {
                let mut data = s.data.lock().unwrap();
                data.input_queue.finish_turn(&Outcome::Error);
                let _ = s.store.lock().unwrap().save_state(&s.id, &data);
            }
            s.emit("error", json!({ "message": message }));
            broadcast(s);
            false
        }
    }
}

/// Queue scheduler: same lock order and drain gate as the limit-resume scheduler;
/// never recurses through HTTP handlers.
pub async fn scheduler(app: App) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        interval.tick().await;
        let ready: Vec<Arc<Session>> = app
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.data.lock().unwrap().input_queue.ready)
            .cloned()
            .collect();
        if ready.is_empty() {
            continue;
        }
        let _communication = app.communication_lock.lock().await;
        let _gate = app.run_lock.lock().await;
        let _workspace = app.workspace_lock.lock().await;
        if crate::service_restart::allow_start(&app).is_err() {
            continue;
        }
        for s in &ready {
            dispatch_next(&app, s, false).await;
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Enqueue {
    text: String,
    request_id: String,
}
#[derive(Deserialize)]
pub struct Edit {
    text: String,
    revision: u32,
}

fn reply(s: &Session, extra: Value) -> ApiResult {
    let queue = s.data.lock().unwrap().input_queue.clone();
    Ok(Json(json!({"queue":queue,"result":extra})))
}

pub async fn get(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    reply(&s, Value::Null)
}
pub async fn enqueue(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Enqueue>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    enqueue_locked(&app, &id, &req.text, &req.request_id, false)
}

/// Called under run_lock. Automated inputs use the same durable bindings,
/// bounds and pause gates as queued human inputs. An idle session may consume
/// the new item without a separate click; existing pauses are never cleared.
pub(crate) fn enqueue_locked(
    app: &App,
    id: &str,
    text: &str,
    request_id: &str,
    automated: bool,
) -> ApiResult {
    let req = Enqueue {
        text: text.into(),
        request_id: request_id.into(),
    };
    let s = session(&app, &id)?;
    validate(&req.text)?;
    if req.request_id.is_empty() || req.request_id.len() > MAX_REQUEST_ID {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Missing or oversized request ID",
        ));
    }
    if s.data.lock().unwrap().requests.contains(&req.request_id) {
        // Already accepted as a direct prompt; never queue it again.
        return reply(&s, json!({"duplicate":true,"sentDirectly":true}));
    }
    match check_binding(&s, &req.request_id, &req.text)? {
        Bound::Duplicate(item) => return reply(&s, json!({"id":item,"duplicate":true})),
        Bound::Conflict => return Err(conflict()),
        Bound::New => {}
    }
    let (count, bytes): (i64, i64) = s
        .store
        .lock()
        .unwrap()
        .connection
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM input_queue_bindings WHERE session_id=?1",
            params![s.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db)?;
    if count >= MAX_BINDINGS || bytes + req.text.len() as i64 > MAX_BINDING_BYTES {
        return Err(fail(
            StatusCode::CONFLICT,
            "This session's queued-input history is full; send directly or continue in a new session",
        ));
    }
    let (item, _) = transact_session(&s, Some((&req.request_id, &req.text)), |d| {
        let now = crate::timeouts::now_ms();
        // Read with the enqueue so startup cannot finish in between and strand the item.
        if !automated && matches!(d.status.as_str(), "starting" | "resuming") {
            return d
                .input_queue
                .enqueue_during_startup(&req.request_id, &req.text, now);
        }
        let idle = d.status == "idle"
            && !d.turn_active
            && !d.lifecycle.suspended
            && !d.lifecycle.archived
            && d.queued_prompt.is_none();
        let q = &mut d.input_queue;
        let item = q.enqueue(&req.request_id, &req.text, now)?;
        if automated && idle && q.paused.is_none() {
            q.ready = true;
        }
        Ok(item)
    })?;
    reply(&s, json!({"id":item,"duplicate":false}))
}
pub async fn edit(
    State(app): State<App>,
    Path((id, item)): Path<(String, String)>,
    Json(req): Json<Edit>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    transact(&s, None, |q| q.edit(&item, req.revision, &req.text))?;
    reply(&s, Value::Null)
}
pub async fn remove(State(app): State<App>, Path((id, item)): Path<(String, String)>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    transact(&s, None, |q| q.remove(&item))?;
    reply(&s, Value::Null)
}
/// Deliberate resume. All admission checks finish before the persisted pause is
/// cleared; if idle, the first queued item is sent now.
pub async fn resume(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _communication = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    let s = session(&app, &id)?;
    crate::service_restart::allow_start(&app)?;
    crate::hosts::validate_session(&app, &s).await?;
    {
        let data = s.data.lock().unwrap();
        if data.lifecycle.suspended || data.lifecycle.archived || data.provider_session_id.is_none()
        {
            return Err(fail(
                StatusCode::CONFLICT,
                "Resume the session before resuming its queue",
            ));
        }
        if data.continuation.limited || data.continuation.due_at.is_some() {
            return Err(fail(
                StatusCode::CONFLICT,
                "Session is paused for a usage limit; the queue cannot bypass it",
            ));
        }
    }
    if s.commands.lock().unwrap().is_none() {
        return Err(fail(
            StatusCode::CONFLICT,
            "Native session is unavailable; resume the session first",
        ));
    }
    transact(&s, None, |q| {
        q.resume();
        Ok(())
    })?;
    let dispatched = dispatch_next(&app, &s, true).await;
    reply(&s, json!({ "dispatched": dispatched }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentworks_session::queue::ItemState;
    use std::sync::Mutex;

    struct Fixture {
        session: Session,
        receiver: tokio::sync::mpsc::Receiver<crate::session_runtime::Command>,
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("queue-unit-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let store = Arc::new(Mutex::new(
                crate::store::Store::open(&root.join("db.sqlite3")).unwrap(),
            ));
            let catalog = Arc::new(Mutex::new(
                crate::Catalog::open(root.join("catalog.json")).unwrap(),
            ));
            let data = serde_json::from_value(json!({"status":"idle","seq":0,"requests":[],"models":null,"available_commands":[],"turn_active":false,"provider_session_id":"native-queue"})).unwrap();
            let meta = crate::store::Metadata {
                id: "queue-test".into(),
                provider: "demo".into(),
                cwd: root.clone(),
                title: "Queue test".into(),
                initial_model: None,
                created: 1,
                host_id: "local".into(),
                location_id: "fixture".into(),
            };
            store
                .lock()
                .unwrap()
                .insert(&meta, &data, None, None)
                .unwrap();
            let (commands, receiver) = tokio::sync::mpsc::channel(1);
            let session = Session {
                id: meta.id,
                provider: meta.provider,
                cwd: root.clone(),
                title: meta.title,
                initial_model: None,
                created: 1,
                host_id: meta.host_id,
                location_id: meta.location_id,
                data: Mutex::new(data),
                tx: tokio::sync::broadcast::channel(128).0,
                commands: Mutex::new(Some(commands)),
                store,
                approvals: Default::default(),
                worker: Default::default(),
                mcp_endpoint: "127.0.0.1:4505".parse().unwrap(),
                mcp_token: Mutex::new("fixture-only".into()),
                mcp_refresh_pending: Default::default(),
                fresh_environment: Default::default(),
                notifications: crate::notifications::Hub::new(catalog),
                progress: crate::tool_progress::Hub::empty(),
                account_quota: Arc::default(),
            };
            Self {
                session,
                receiver,
                root,
            }
        }
        fn claim(&self) -> (String, String) {
            let mut data = self.session.data.lock().unwrap();
            data.input_queue
                .enqueue("first", "first queued input", 1)
                .unwrap();
            data.input_queue
                .enqueue("later", "later queued input", 2)
                .unwrap();
            let claimed = data.input_queue.claim().unwrap();
            self.session
                .store
                .lock()
                .unwrap()
                .save_state(&self.session.id, &data)
                .unwrap();
            claimed
        }
        fn saved_queue(&self) -> Queue {
            self.session
                .store
                .lock()
                .unwrap()
                .load()
                .unwrap()
                .remove(0)
                .1
                .input_queue
        }
        fn assert_inspectable(queue: &Queue) {
            assert_eq!(queue.items[0].state, ItemState::Error);
            assert_eq!(queue.items[1].state, ItemState::Paused);
            assert!(
                queue.items[0]
                    .note
                    .as_deref()
                    .unwrap()
                    .contains("unconfirmed")
            );
            assert!(queue.paused.is_some());
            assert!(!queue.ready);
            assert!(queue.has_pending());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn hold_for_startup(f: &Fixture) {
        let mut data = f.session.data.lock().unwrap();
        data.status = "starting".into();
        data.input_queue
            .enqueue_during_startup("held", "typed during startup", 1)
            .unwrap();
    }

    #[test]
    fn startup_success_releases_held_input() {
        let f = Fixture::new();
        hold_for_startup(&f);
        f.session.status("idle");
        assert!(f.saved_queue().ready);
    }

    #[test]
    fn startup_failure_pauses_held_input_durably() {
        let f = Fixture::new();
        hold_for_startup(&f);
        let mut events = f.session.tx.subscribe();
        f.session.status("error");
        let saved = f.saved_queue();
        assert!(!saved.ready);
        assert_eq!(saved.items[0].state, ItemState::Paused);
        assert!(saved.paused.unwrap().contains("did not start"));
        let mut broadcast = false;
        while let Ok(event) = events.try_recv() {
            broadcast |= event["kind"] == "input_queue" && event["data"]["paused"].is_string();
        }
        assert!(broadcast, "the composer must learn about the pause");
    }

    #[test]
    fn failed_command_handoff_never_marks_input_sent() {
        for closed in [false, true] {
            let mut f = Fixture::new();
            let (text, id) = f.claim();
            if closed {
                f.receiver.close();
            } else {
                f.session
                    .send_command(crate::session_runtime::Command::Prompt(
                        "occupy channel".into(),
                    ))
                    .unwrap();
            }
            let mut events = f.session.tx.subscribe();
            assert!(!dispatch_claimed(&f.session, text, id));
            Fixture::assert_inspectable(&f.session.data.lock().unwrap().input_queue);
            Fixture::assert_inspectable(&f.saved_queue());
            while let Ok(event) = events.try_recv() {
                if event["kind"] == "input_queue" {
                    assert!(
                        event["data"]["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|i| i["state"] != "sent")
                    );
                }
            }
            // A late successful outcome cannot turn the rejected dispatch into Sent
            // or release later inputs.
            record_outcome(&f.session, Outcome::Success);
            Fixture::assert_inspectable(&f.saved_queue());
            if !closed {
                f.receiver.try_recv().unwrap();
            }
            assert!(f.receiver.try_recv().is_err());
        }
    }

    #[test]
    fn successful_command_handoff_persists_sent_before_turn_outcome() {
        let mut f = Fixture::new();
        let (text, id) = f.claim();
        assert!(dispatch_claimed(&f.session, text, id));
        assert!(matches!(
            f.receiver.try_recv(),
            Ok(crate::session_runtime::Command::Prompt(_))
        ));
        let saved = f.saved_queue();
        assert_eq!(saved.items[0].state, ItemState::Sent);
        assert_eq!(saved.items[1].state, ItemState::Queued);
        assert!(!saved.ready);
        record_outcome(&f.session, Outcome::Error);
        let saved = f.saved_queue();
        assert_eq!(saved.items[0].state, ItemState::Sent);
        assert_eq!(saved.items[1].state, ItemState::Paused);
    }

    #[test]
    fn failed_handoff_persistence_keeps_delivery_uncertain() {
        let mut f = Fixture::new();
        let (text, id) = f.claim();
        f.session.store.lock().unwrap().connection.execute_batch(
            "CREATE TRIGGER reject_sent BEFORE UPDATE ON sessions WHEN NEW.state LIKE '%\"sent\"%' BEGIN SELECT RAISE(ABORT,'injected handoff persistence failure'); END;"
        ).unwrap();
        assert!(!dispatch_claimed(&f.session, text, id));
        // The command really was handed off, but no durable delivery claim was saved.
        assert!(matches!(
            f.receiver.try_recv(),
            Ok(crate::session_runtime::Command::Prompt(_))
        ));
        Fixture::assert_inspectable(&f.session.data.lock().unwrap().input_queue);
        Fixture::assert_inspectable(&f.saved_queue());
        record_outcome(&f.session, Outcome::Success);
        Fixture::assert_inspectable(&f.saved_queue());
    }
}
