//! L-05 explicit human queued inputs. Distinct from the internal `queued_prompt`
//! used by automation/handoff. State persists inside session state (serde default),
//! so no table or schema version change is required.
//!
//! Dispatch rules: one item is claimed only after the prior turn ends cleanly
//! (`end_turn`, no cancel/timeout/error/quota/storage failure). Anything else pauses
//! the queue for deliberate human resume. An item found `sending` after restart is
//! ambiguous: it is marked `error` and never replayed automatically.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_ITEMS: usize = 20;
pub const MAX_ITEM_BYTES: usize = 32768;
pub const MAX_TOTAL_BYTES: usize = 65536;
pub const MAX_REQUEST_ID: usize = 100;
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
    /// Client enqueue request ID; retried enqueues with the same payload are duplicates.
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
    /// Returns (item id, duplicate).
    pub fn enqueue(&mut self, request_id: &str, text: &str, now: u64) -> Res<(String, bool)> {
        validate(text)?;
        if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Missing or oversized request ID",
            ));
        }
        if let Some(existing) = self.items.iter().find(|i| i.request_id == request_id) {
            if existing.text == text || existing.revision > 0 {
                return Ok((existing.id.clone(), true));
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
        Ok((id, false))
    }
    /// Edits a pending (or errored, deliberately re-queued) item if `revision` matches.
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
    /// Deliberate human resume. Errored items stay errored until edited.
    pub fn resume(&mut self) {
        self.paused = None;
        for item in &mut self.items {
            if item.state == ItemState::Paused {
                item.state = ItemState::Queued;
            }
        }
    }
    /// Inbox-priority hook: explicit human input pending (including paused) outranks automatic wakes.
    pub fn has_pending(&self) -> bool {
        self.items.iter().any(|i| {
            matches!(
                i.state,
                ItemState::Queued | ItemState::Paused | ItemState::Sending
            )
        })
    }
    /// Records the finished turn's outcome for the item in flight.
    pub fn finish_turn(&mut self, outcome: &Outcome) {
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
        let mut ambiguous = false;
        for item in &mut self.items {
            if item.state == ItemState::Sending {
                item.state = ItemState::Error;
                item.note = Some("Service restarted during dispatch; delivery unknown. Edit to re-queue or remove.".into());
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

/// Runtime hook at turn end. Returns true if the next queued input was dispatched
/// (status stays running); false means the caller should set idle.
pub fn after_turn(s: &Session, mut outcome: Outcome) -> bool {
    let claimed = {
        let mut data = s.data.lock().unwrap();
        if outcome == Outcome::Success {
            if data.storage_error.is_some() {
                outcome = Outcome::StorageFailure;
            } else if data.continuation.limited || data.continuation.due_at.is_some() {
                outcome = Outcome::Quota;
            }
        }
        let had = !data.input_queue.items.is_empty();
        data.input_queue.finish_turn(&outcome);
        let claimed = if outcome == Outcome::Success && !data.lifecycle.suspended {
            data.input_queue.claim()
        } else {
            None
        };
        if let Some((_, id)) = &claimed {
            data.requests.insert(id.clone());
            data.status = "running".into();
            data.turn_active = true;
        }
        if !had && claimed.is_none() {
            return false;
        }
        claimed
    };
    publish(s);
    let Some((text, id)) = claimed else {
        return false;
    };
    s.emit(
        "user",
        json!({"text":text,"requestId":id,"initiatedBy":"queue"}),
    );
    s.emit("status", json!({"status":"running"}));
    if let Err((_, e)) = s.send_command(crate::runtime::Command::Prompt(text)) {
        let mut data = s.data.lock().unwrap();
        data.input_queue.finish_turn(&Outcome::Error);
        drop(data);
        s.emit("error", json!({"message":e}));
        publish(s);
        return false;
    }
    true
}

fn publish(s: &Session) {
    let queue = s.data.lock().unwrap().input_queue.clone();
    s.emit("input_queue", json!(queue));
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
    publish(s);
    let queue = s.data.lock().unwrap().input_queue.clone();
    Ok(Json(json!({"queue":queue,"result":extra})))
}

pub async fn get(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let queue = s.data.lock().unwrap().input_queue.clone();
    Ok(Json(json!({"queue":queue})))
}
pub async fn enqueue(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Enqueue>,
) -> ApiResult {
    let s = session(&app, &id)?;
    let (item, duplicate) = {
        let mut data = s.data.lock().unwrap();
        if data.storage_error.is_some() {
            return Err(fail(
                StatusCode::CONFLICT,
                "Session storage failed; queue is unavailable",
            ));
        }
        data.input_queue
            .enqueue(&req.request_id, &req.text, crate::timeouts::now_ms())?
    };
    reply(&s, json!({"id":item,"duplicate":duplicate}))
}
pub async fn edit(
    State(app): State<App>,
    Path((id, item)): Path<(String, String)>,
    Json(req): Json<Edit>,
) -> ApiResult {
    let s = session(&app, &id)?;
    s.data
        .lock()
        .unwrap()
        .input_queue
        .edit(&item, req.revision, &req.text)?;
    reply(&s, Value::Null)
}
pub async fn remove(State(app): State<App>, Path((id, item)): Path<(String, String)>) -> ApiResult {
    let s = session(&app, &id)?;
    s.data.lock().unwrap().input_queue.remove(&item)?;
    reply(&s, Value::Null)
}
/// Deliberate resume; if the session is idle, sends the first queued item now.
pub async fn resume(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let idle = {
        let mut data = s.data.lock().unwrap();
        data.input_queue.resume();
        data.status == "idle" && !data.turn_active
    };
    if idle {
        crate::service_restart::allow_start(&app)?;
        let dispatched = after_turn(&s, Outcome::Success);
        return reply(&s, json!({"dispatched":dispatched}));
    }
    reply(&s, json!({"dispatched":false}))
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
        assert_eq!(q.claim().unwrap().0, "three");
    }

    #[test]
    fn duplicate_request_and_changed_payload() {
        let mut q = q();
        let (a, dup) = q.enqueue("r1", "hello", 1).unwrap();
        assert!(!dup);
        assert_eq!(q.enqueue("r1", "hello", 2).unwrap(), (a.clone(), true));
        assert_eq!(
            q.enqueue("r1", "changed", 2).unwrap_err().0,
            StatusCode::CONFLICT
        );
        // After a deliberate edit, a retried original enqueue stays a duplicate, not a new prompt.
        q.edit(&a, 0, "edited").unwrap();
        assert_eq!(q.enqueue("r1", "hello", 3).unwrap(), (a, true));
        assert_eq!(q.items.len(), 1);
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
            assert!(q.paused.is_some(), "{outcome:?} pauses");
            assert!(q.claim().is_none(), "{outcome:?} blocks dispatch");
            assert_eq!(q.items[0].state, ItemState::Error);
            q.resume();
            assert_eq!(
                q.claim().unwrap().0,
                "second",
                "errored item is not replayed"
            );
            // Explicit edit re-queues the errored item under a new dispatch ID.
            q.finish_turn(&Outcome::Success);
            q.edit(&a, 0, "first again").unwrap();
            assert_eq!(q.claim().unwrap().1, "queue:q1:r1");
        }
    }

    #[test]
    fn restart_with_inflight_item_is_ambiguous_and_paused() {
        let mut q = q();
        q.enqueue("r1", "mutating", 1).unwrap();
        q.enqueue("r2", "next", 1).unwrap();
        q.claim().unwrap();
        let restored: Queue = serde_json::from_value(serde_json::to_value(&q).unwrap()).unwrap();
        let mut q = restored;
        q.recover();
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
    fn sent_history_is_pruned_and_pending_hook() {
        let mut q = q();
        assert!(!q.has_pending());
        for i in 0..8 {
            q.enqueue(&format!("r{i}"), "x", 1).unwrap();
        }
        assert!(q.has_pending());
        while q.claim().is_some() {
            q.finish_turn(&Outcome::Success);
        }
        assert_eq!(q.items.len(), KEEP_SENT);
        assert!(!q.has_pending());
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
