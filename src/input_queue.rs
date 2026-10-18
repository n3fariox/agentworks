//! L-05 explicit human queued inputs. Distinct from the internal `queued_prompt`
//! used by automation/handoff. State persists inside session state (serde default),
//! so no table or schema version change is required.
//!
//! Every API mutation is saved before it is acknowledged and rolled back if the save
//! fails. The runtime only records a turn outcome; `scheduler` dispatches under the
//! shared `run_lock` through the same acceptance path as a direct prompt, after drain,
//! idle, suspend, storage, quota and pause gates. Anything other than a clean `end_turn`
//! pauses the queue for deliberate resume. An item found `sending` after restart is
//! ambiguous: it is marked `error` and never replayed automatically.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const MAX_ITEMS: usize = 20;
pub const MAX_ITEM_BYTES: usize = 32768;
pub const MAX_TOTAL_BYTES: usize = 65536;
pub const MAX_REQUEST_ID: usize = 100;
/// Request-ID bindings outlive display pruning, removal, sending and restarts.
/// Bounded so storage cannot grow; only the oldest of this many IDs is forgotten.
pub const MAX_BINDINGS: usize = 512;
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
    /// Client enqueue request ID; retried enqueues with the original payload are duplicates.
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

/// Durable tombstone: request ID bound to the originally submitted payload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub request_id: String,
    pub digest: String,
    pub item: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub items: Vec<Item>,
    #[serde(default)]
    pub paused: Option<String>,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub bindings: Vec<Binding>,
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

/// Stable FNV-1a digest plus length; independent of Rust's hasher versions.
pub fn digest(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}:{}", text.len())
}

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
    pub fn bound(&self, request_id: &str) -> bool {
        self.bindings.iter().any(|b| b.request_id == request_id)
    }
    /// Returns (item id, duplicate).
    pub fn enqueue(&mut self, request_id: &str, text: &str, now: u64) -> Res<(String, bool)> {
        validate(text)?;
        if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Missing or oversized request ID",
            ));
        }
        if let Some(b) = self.bindings.iter().find(|b| b.request_id == request_id) {
            if b.digest == digest(text) {
                return Ok((b.item.clone(), true));
            }
            return Err(fail(
                StatusCode::CONFLICT,
                "Request ID was already used for a different message",
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
        self.bindings.push(Binding {
            request_id: request_id.into(),
            digest: digest(text),
            item: id.clone(),
        });
        if self.bindings.len() > MAX_BINDINGS {
            let excess = self.bindings.len() - MAX_BINDINGS;
            self.bindings.drain(..excess);
        }
        Ok((id, false))
    }
    /// Edits a pending (or errored, deliberately re-queued) item if `revision` matches.
    /// The request-ID binding keeps the original payload.
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
        let previous = if item.state == ItemState::Error {
            0
        } else {
            item.text.len()
        };
        if others - previous + text.len() > MAX_TOTAL_BYTES {
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
    /// Stop/Suspend/completion: no dispatch until a deliberate resume.
    pub fn block(&mut self, reason: &str) {
        self.ready = false;
        self.pause(reason);
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
    /// Explicit-input priority predicate: pending human input (including paused) outranks
    /// automatic inbox wakes.
    pub fn has_pending(&self) -> bool {
        self.items.iter().any(|i| {
            matches!(
                i.state,
                ItemState::Queued | ItemState::Paused | ItemState::Sending
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
        } else if !diagnostic {
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

/// Coordinator hook for inbox wake integration: true when explicit human input is pending.
#[allow(dead_code)]
pub fn has_pending(s: &Session) -> bool {
    s.data.lock().unwrap().input_queue.has_pending()
}

/// Applies `change` and persists it; on save failure the queue is restored and
/// the change is not acknowledged.
fn transact<T>(s: &Session, change: impl FnOnce(&mut Queue) -> Res<T>) -> Res<T> {
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
            if let Err(error) = s.store.lock().unwrap().save_state(&s.id, &data) {
                data.input_queue = before;
                eprintln!(
                    "Could not persist queue for {}: {}",
                    s.id,
                    crate::runtime::safe_error(&error.to_string())
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

/// Dispatches one queued item. Caller holds `run_lock` and passed `allow_start`.
/// Returns true when a turn was accepted.
fn dispatch_next(s: &Session, deliberate: bool) -> bool {
    let (text, id) = {
        let mut data = s.data.lock().unwrap();
        let eligible = (deliberate || data.input_queue.ready)
            && data.status == "idle"
            && !data.turn_active
            && !data.lifecycle.suspended
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
    match crate::accept_prompt(s, text, id, Some("queue".into())) {
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

/// Queue scheduler: same locks and drain gate as other dispatchers; never recurses
/// through HTTP handlers.
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
            dispatch_next(s, false);
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
    if s.data.lock().unwrap().requests.contains(&req.request_id) {
        // Already accepted as a direct prompt; never queue it again.
        return reply(&s, json!({"duplicate":true,"sentDirectly":true}));
    }
    let (item, duplicate) = transact(&s, |q| {
        q.enqueue(&req.request_id, &req.text, crate::timeouts::now_ms())
    })?;
    reply(&s, json!({"id":item,"duplicate":duplicate}))
}
pub async fn edit(
    State(app): State<App>,
    Path((id, item)): Path<(String, String)>,
    Json(req): Json<Edit>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    transact(&s, |q| q.edit(&item, req.revision, &req.text))?;
    reply(&s, Value::Null)
}
pub async fn remove(State(app): State<App>, Path((id, item)): Path<(String, String)>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    transact(&s, |q| q.remove(&item))?;
    reply(&s, Value::Null)
}
/// Deliberate resume; admission is checked first. If idle, sends the first queued item now.
pub async fn resume(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _communication = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    let s = session(&app, &id)?;
    crate::service_restart::allow_start(&app)?;
    {
        let data = s.data.lock().unwrap();
        if data.lifecycle.suspended {
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
    transact(&s, |q| {
        q.resume();
        Ok(())
    })?;
    let dispatched = dispatch_next(&s, true);
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
    fn request_binding_survives_edit_remove_send_prune_and_restart() {
        let mut q = q();
        let (a, dup) = q.enqueue("r1", "hello", 1).unwrap();
        assert!(!dup);
        assert_eq!(q.enqueue("r1", "hello", 2).unwrap(), (a.clone(), true));
        assert_eq!(
            q.enqueue("r1", "changed", 2).unwrap_err().0,
            StatusCode::CONFLICT
        );
        q.edit(&a, 0, "edited").unwrap();
        assert_eq!(q.enqueue("r1", "hello", 3).unwrap(), (a.clone(), true));
        assert_eq!(
            q.enqueue("r1", "anything else", 3).unwrap_err().0,
            StatusCode::CONFLICT,
            "edited items do not accept arbitrary payloads"
        );
        q.remove(&a).unwrap();
        assert!(
            q.enqueue("r1", "hello", 4).unwrap().1,
            "tombstone after removal"
        );
        assert!(q.items.is_empty());
        // Send and prune well past the display history.
        for i in 0..10 {
            q.enqueue(&format!("s{i}"), "x", 1).unwrap();
            q.claim().unwrap();
            q.finish_turn(&Outcome::Success);
        }
        assert_eq!(q.items.len(), KEEP_SENT);
        assert!(
            q.enqueue("s0", "x", 5).unwrap().1,
            "pruned item stays bound"
        );
        assert!(q.enqueue("s0", "y", 5).is_err());
        let mut restored: Queue =
            serde_json::from_value(serde_json::to_value(&q).unwrap()).unwrap();
        restored.recover();
        assert!(
            restored.enqueue("s1", "x", 6).unwrap().1,
            "binding survives restart"
        );
        assert!(restored.bound("r1"));
    }

    #[test]
    fn bindings_are_bounded() {
        let mut q = q();
        for i in 0..MAX_BINDINGS + 10 {
            q.enqueue(&format!("r{i}"), "x", 1).unwrap();
            q.remove(&format!("q{}", i + 1)).unwrap();
        }
        assert_eq!(q.bindings.len(), MAX_BINDINGS);
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
