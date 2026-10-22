//! Bounded input queue and deterministic dispatch/recovery transitions.
use crate::fail;
use http::StatusCode;
use serde::{Deserialize, Serialize};
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

pub fn validate(text: &str) -> Res<()> {
    if text.trim().is_empty() || text.len() > MAX_ITEM_BYTES {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Enter a message of up to 32 KB",
        ));
    }
    Ok(())
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
