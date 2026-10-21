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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const MAX_ITEMS: usize = 20;
pub const MAX_ITEM_BYTES: usize = 32768;
pub const MAX_TOTAL_BYTES: usize = 65536;
pub const MAX_REQUEST_ID: usize = 100;
/// Request-ID bindings keep the exact original payload and outlive display pruning,
/// removal, sending and restarts. When full, new enrollment is refused rather than
/// forgetting old IDs.
pub const MAX_BINDINGS: i64 = 1000;
pub const MAX_BINDING_BYTES: i64 = 4 * 1024 * 1024;
const KEEP_SENT: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemState {
    #[default]
    Queued,
    Sending,
    Sent,
    Paused,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    /// Client enqueue request ID (bound durably to the original payload).
    pub request_id: String,
    pub text: String,
    pub revision: u32,
    pub state: ItemState,
    pub created: u64,
    #[serde(default)]
    pub note: Option<String>,
}

impl Item {
    /// Dispatch identity bound to payload revision; edits before claim change it.
    pub fn dispatch_id(&self) -> String {
        format!("queue:{}:r{}", self.id, self.revision)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub items: Vec<Item>,
    #[serde(default)]
    pub paused: Option<String>,
    #[serde(default)]
    pub next_id: u64,
    /// Set only when the last turn ended cleanly; the scheduler may then dispatch.
    #[serde(default)]
    pub ready: bool,
    /// The current turn is a status-only diagnostic probe; it never triggers dispatch.
    #[serde(default)]
    pub diagnostic: bool,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Success,
    Cancelled,
    TimedOut,
    Error,
    Quota,
    StorageFailure,
}

type Res<T> = Result<T, (StatusCode, String)>;

/// Status-only probes the UI sends (`/usage`, `/status`); they must not start queued work.
pub fn is_diagnostic(text: &str) -> bool {
    matches!(text.trim(), "/usage" | "/status")
}

impl Queue {
    fn pending_bytes(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.state != ItemState::Sent)
            .map(|i| i.text.len())
            .sum()
    }
    fn live(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.state != ItemState::Sent)
            .count()
    }
    fn find(&mut self, id: &str) -> Res<&mut Item> {
        self.items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Queued input not found"))
    }
    /// Appends an item. Request-ID deduplication is done against the durable bindings
    /// before this is called. Returns (item id, duplicate=false).
    pub fn enqueue(&mut self, request_id: &str, text: &str, now: u64) -> Res<(String, bool)> {
        validate(text)?;
        if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Missing or oversized request ID",
            ));
        }
        if self.live() >= MAX_ITEMS || self.pending_bytes() + text.len() > MAX_TOTAL_BYTES {
            return Err(fail(
                StatusCode::CONFLICT,
                "Queue is full; send or remove pending inputs",
            ));
        }
        self.next_id += 1;
        let id = format!("q{}", self.next_id);
        self.items.push(Item {
            id: id.clone(),
            request_id: request_id.into(),
            text: text.into(),
            revision: 0,
            state: if self.paused.is_some() {
                ItemState::Paused
            } else {
                ItemState::Queued
            },
            created: now,
            note: None,
        });
        Ok((id, false))
    }
    /// Edits a pending (or errored, deliberately re-queued) item if `revision` matches.
    /// The durable request-ID binding keeps the original payload.
    pub fn edit(&mut self, id: &str, revision: u32, text: &str) -> Res<()> {
        validate(text)?;
        let others = self.pending_bytes();
        let paused = self.paused.is_some();
        let item = self.find(id)?;
        if matches!(item.state, ItemState::Sending | ItemState::Sent) {
            return Err(fail(
                StatusCode::CONFLICT,
                "Input was already claimed for sending",
            ));
        }
        if item.revision != revision {
            return Err(fail(
                StatusCode::CONFLICT,
                "Input changed elsewhere; reload the queue",
            ));
        }
        if others - item.text.len() + text.len() > MAX_TOTAL_BYTES {
            return Err(fail(StatusCode::CONFLICT, "Queue is full"));
        }
        item.text = text.into();
        item.revision += 1;
        item.note = None;
        item.state = if paused {
            ItemState::Paused
        } else {
            ItemState::Queued
        };
        Ok(())
    }
    pub fn remove(&mut self, id: &str) -> Res<()> {
        let item = self.find(id)?;
        if item.state == ItemState::Sending {
            return Err(fail(
                StatusCode::CONFLICT,
                "Input is being sent; stop the turn first",
            ));
        }
        self.items.retain(|i| i.id != id);
        Ok(())
    }
    pub fn pause(&mut self, reason: &str) {
        if !self
            .items
            .iter()
            .any(|i| matches!(i.state, ItemState::Queued | ItemState::Paused))
        {
            return;
        }
        self.paused = Some(reason.into());
        for item in &mut self.items {
            if item.state == ItemState::Queued {
                item.state = ItemState::Paused;
            }
        }
    }
    /// Stop/Suspend/completion: no dispatch until a deliberate resume. The pause is kept
    /// even if the queue is empty or the turn later ends with `end_turn`.
    pub fn block(&mut self, reason: &str) {
        self.ready = false;
        self.pause(reason);
        self.paused = Some(reason.into());
    }
    /// Deliberate human resume. Errored items stay errored until edited.
    pub fn resume(&mut self) {
        self.paused = None;
        for item in &mut self.items {
            if item.state == ItemState::Paused {
                item.state = ItemState::Queued;
            }
        }
    }
    /// Explicit-input priority predicate: pending or unresolved human input (including
    /// paused and errored items awaiting inspection) outranks automatic inbox wakes.
    pub fn has_pending(&self) -> bool {
        self.items.iter().any(|i| {
            matches!(
                i.state,
                ItemState::Queued | ItemState::Paused | ItemState::Sending | ItemState::Error
            )
        })
    }
    /// Called at every turn acceptance (direct, automation or queued).
    pub fn begin_turn(&mut self, text: &str) {
        self.ready = false;
        self.diagnostic = is_diagnostic(text);
    }
    /// Records the finished turn's outcome for the item in flight.
    pub fn finish_turn(&mut self, outcome: &Outcome) {
        let diagnostic = std::mem::take(&mut self.diagnostic);
        self.ready = false;
        for item in &mut self.items {
            if item.state == ItemState::Sending {
                if *outcome == Outcome::Success {
                    item.state = ItemState::Sent;
                } else {
                    item.state = ItemState::Error;
                    item.note = Some(format!("Turn ended: {outcome:?}; not resent"));
                }
            }
        }
        if *outcome != Outcome::Success {
            self.pause(&format!(
                "Previous turn ended: {outcome:?}. Inspect, then resume."
            ));
        } else if !diagnostic && self.paused.is_none() {
            self.ready = true;
        }
        let sent: Vec<usize> = (0..self.items.len())
            .filter(|&i| self.items[i].state == ItemState::Sent)
            .collect();
        if sent.len() > KEEP_SENT {
            let drop: Vec<String> = sent[..sent.len() - KEEP_SENT]
                .iter()
                .map(|&i| self.items[i].id.clone())
                .collect();
            self.items.retain(|i| !drop.contains(&i.id));
        }
    }
    /// Claims the next queued item (marks it sending) if not paused and nothing in flight.
    pub fn claim(&mut self) -> Option<(String, String)> {
        if self.paused.is_some() || self.items.iter().any(|i| i.state == ItemState::Sending) {
            return None;
        }
        let item = self
            .items
            .iter_mut()
            .find(|i| i.state == ItemState::Queued)?;
        item.state = ItemState::Sending;
        Some((item.text.clone(), item.dispatch_id()))
    }
    /// After restart: an in-flight item is ambiguous and must be inspected; never replayed.
    pub fn recover(&mut self) {
        self.ready = false;
        self.diagnostic = false;
        let mut ambiguous = false;
        for item in &mut self.items {
            if item.state == ItemState::Sending {
                item.state = ItemState::Error;
                item.note = Some(
                    "Service restarted during dispatch; delivery unknown. Edit to re-queue or remove."
                        .into(),
                );
                ambiguous = true;
            }
        }
        if ambiguous || self.items.iter().any(|i| i.state == ItemState::Queued) {
            self.pause("Service restarted; inspect queued inputs, then resume.");
        }
    }
}

fn validate(text: &str) -> Res<()> {
    if text.trim().is_empty() || text.len() > MAX_ITEM_BYTES {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Enter a message of up to 32 KB",
        ));
    }
    Ok(())
}

fn db(error: impl std::fmt::Display) -> (StatusCode, String) {
    eprintln!(
        "Queue binding storage error: {}",
        crate::runtime::safe_error(&error.to_string())
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
    let result = {
        let mut data = s.data.lock().unwrap();
        if data.storage_error.is_some() {
            return Err(fail(
                StatusCode::CONFLICT,
                "Session storage failed; queue is unavailable",
            ));
        }
        let before = data.input_queue.clone();
        let value = change(&mut data.input_queue)?;
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
                    crate::runtime::safe_error(&error)
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
fn broadcast(s: &Session) {
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
fn dispatch_next(app: &App, s: &Session, deliberate: bool) -> bool {
    if s.host_id != "local" {
        return false;
    }
    {
        let data = s.data.lock().unwrap();
        if !(deliberate || data.input_queue.ready) {
            return false;
        }
    }
    // Root policy is re-validated at dispatch, like other launches.
    if crate::checked_cwd(app, &s.cwd.to_string_lossy()).is_err() {
        let mut data = s.data.lock().unwrap();
        data.input_queue
            .block("Session directory is outside allowed roots; queue paused.");
        let _ = s.store.lock().unwrap().save_state(&s.id, &data);
        drop(data);
        broadcast(s);
        return false;
    }
    if s.commands.lock().unwrap().is_none() {
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
    match crate::accept_prompt(s, text, id, Some("queue".into()), None) {
        Ok(_) => true,
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
            dispatch_next(&app, s, false);
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
    let s = session(&app, &id)?;
    crate::local_only(&s)?;
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
    let (item, _) = transact(&s, Some((&req.request_id, &req.text)), |q| {
        q.enqueue(&req.request_id, &req.text, crate::timeouts::now_ms())
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
    crate::local_only(&s)?;
    crate::service_restart::allow_start(&app)?;
    crate::checked_cwd(&app, &s.cwd.to_string_lossy())?;
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
    let dispatched = dispatch_next(&app, &s, true);
    reply(&s, json!({ "dispatched": dispatched }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Queue {
        Queue::default()
    }

    #[test]
    fn fifo_order_edit_and_remove() {
        let mut q = q();
        let (a, _) = q.enqueue("r1", "one", 1).unwrap();
        let (b, _) = q.enqueue("r2", "two", 2).unwrap();
        q.enqueue("r3", "three", 3).unwrap();
        q.remove(&b).unwrap();
        q.edit(&a, 0, "one!").unwrap();
        assert!(q.edit(&a, 0, "stale").is_err(), "stale revision rejected");
        let (text, id) = q.claim().unwrap();
        assert_eq!((text.as_str(), id.as_str()), ("one!", "queue:q1:r1"));
        assert!(q.claim().is_none(), "only one in flight");
        q.finish_turn(&Outcome::Success);
        assert!(q.ready);
        assert_eq!(q.claim().unwrap().0, "three");
    }

    #[test]
    fn claimed_item_cannot_be_edited_or_removed() {
        let mut q = q();
        let (a, _) = q.enqueue("r1", "x", 1).unwrap();
        q.claim().unwrap();
        assert!(q.edit(&a, 0, "y").is_err());
        assert!(q.remove(&a).is_err());
    }

    #[test]
    fn failures_pause_and_never_resend() {
        for outcome in [
            Outcome::Cancelled,
            Outcome::TimedOut,
            Outcome::Error,
            Outcome::Quota,
            Outcome::StorageFailure,
        ] {
            let mut q = q();
            let (a, _) = q.enqueue("r1", "first", 1).unwrap();
            q.enqueue("r2", "second", 1).unwrap();
            q.claim().unwrap();
            q.finish_turn(&outcome);
            assert!(!q.ready, "{outcome:?} is not ready");
            assert!(q.paused.is_some(), "{outcome:?} pauses");
            assert!(q.claim().is_none(), "{outcome:?} blocks dispatch");
            assert_eq!(q.items[0].state, ItemState::Error);
            q.resume();
            assert_eq!(
                q.claim().unwrap().0,
                "second",
                "errored item is not replayed"
            );
            q.finish_turn(&Outcome::Success);
            q.edit(&a, 0, "first again").unwrap();
            assert_eq!(q.claim().unwrap().1, "queue:q1:r1");
        }
    }

    #[test]
    fn diagnostic_probe_turn_never_makes_queue_ready() {
        let mut q = q();
        q.enqueue("r1", "substantive", 1).unwrap();
        q.begin_turn("/usage");
        q.finish_turn(&Outcome::Success);
        assert!(!q.ready, "status probe must not trigger queued work");
        q.begin_turn("real work");
        q.finish_turn(&Outcome::Success);
        assert!(q.ready);
    }

    #[test]
    fn block_clears_readiness_at_turn_end_race() {
        let mut q = q();
        q.enqueue("r1", "a", 1).unwrap();
        q.finish_turn(&Outcome::Success);
        assert!(q.ready);
        q.block("Explicit Stop");
        assert!(!q.ready);
        assert!(q.claim().is_none());
    }

    #[test]
    fn stop_pause_survives_successful_end_and_errors_count_as_pending() {
        let mut q = q();
        q.enqueue("r1", "a", 1).unwrap();
        q.claim().unwrap();
        q.block("Explicit Stop");
        q.finish_turn(&Outcome::Success);
        assert!(!q.ready, "Stop wins even if the turn reports end_turn");
        assert!(q.paused.is_some());
        q.enqueue("r2", "b", 1).unwrap();
        assert_eq!(q.items[1].state, ItemState::Paused);
        let mut e = Queue::default();
        e.enqueue("r", "x", 1).unwrap();
        e.claim().unwrap();
        e.finish_turn(&Outcome::Error);
        e.resume();
        assert!(
            e.has_pending(),
            "unresolved error items block optional inbox wakes"
        );
    }

    #[test]
    fn restart_with_inflight_item_is_ambiguous_and_paused() {
        let mut q = q();
        q.enqueue("r1", "mutating", 1).unwrap();
        q.enqueue("r2", "next", 1).unwrap();
        q.claim().unwrap();
        q.ready = true;
        let mut q: Queue = serde_json::from_value(serde_json::to_value(&q).unwrap()).unwrap();
        q.recover();
        assert!(!q.ready);
        assert_eq!(q.items[0].state, ItemState::Error);
        assert!(q.paused.is_some());
        assert!(q.claim().is_none());
        q.resume();
        assert_eq!(q.claim().unwrap().0, "next");
    }

    #[test]
    fn bounds_on_count_bytes_and_request_id() {
        let mut q = q();
        assert!(q.enqueue("r", "  ", 1).is_err());
        assert!(q.enqueue("r", &"x".repeat(MAX_ITEM_BYTES + 1), 1).is_err());
        assert!(q.enqueue(&"r".repeat(101), "x", 1).is_err());
        q.enqueue("a", &"x".repeat(MAX_ITEM_BYTES), 1).unwrap();
        q.enqueue("b", &"x".repeat(MAX_ITEM_BYTES), 1).unwrap();
        assert!(q.enqueue("c", "x", 1).is_err(), "total bytes bounded");
        let mut q = Queue::default();
        for i in 0..MAX_ITEMS {
            q.enqueue(&format!("r{i}"), "x", 1).unwrap();
        }
        assert!(q.enqueue("over", "x", 1).is_err(), "count bounded");
    }

    #[test]
    fn enqueue_while_paused_stays_paused() {
        let mut q = q();
        q.enqueue("r1", "a", 1).unwrap();
        q.pause("stop");
        q.enqueue("r2", "b", 1).unwrap();
        assert_eq!(q.items[1].state, ItemState::Paused);
        assert!(q.claim().is_none());
    }
}
