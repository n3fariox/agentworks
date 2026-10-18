// L-07 execution timeout policy and a pure, deterministic turn clock.
// The runtime samples the clock; nothing here retries tools or turns.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    /// None disables the total limit for deliberately long work.
    pub total_seconds: Option<u64>,
    /// Total limit counts wall time, including human approval waits.
    #[serde(default = "yes")]
    pub total_includes_human_wait: bool,
    pub inactivity_warning_seconds: Option<u64>,
    /// Optional; default warns only so quiet healthy tools are not stopped.
    pub inactivity_timeout_seconds: Option<u64>,
    pub cancel_grace_seconds: u64,
}
fn yes() -> bool {
    true
}
impl Default for Policy {
    // Preserves the previous fixed behavior: 15-minute turn, 15 s grace, no inactivity rules.
    fn default() -> Self {
        Policy {
            total_seconds: Some(900),
            total_includes_human_wait: true,
            inactivity_warning_seconds: None,
            inactivity_timeout_seconds: None,
            cancel_grace_seconds: 15,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        let within = |v: Option<u64>, lo, hi| v.is_none_or(|v| (lo..=hi).contains(&v));
        if !within(self.total_seconds, 60, 7 * 86400) {
            return Err("Total limit must be 1 minute to 7 days, or disabled".into());
        }
        if !within(self.inactivity_warning_seconds, 30, 86400)
            || !within(self.inactivity_timeout_seconds, 60, 86400)
        {
            return Err("Inactivity warning must be 30 s–24 h and timeout 1 min–24 h".into());
        }
        if let (Some(w), Some(t)) = (
            self.inactivity_warning_seconds,
            self.inactivity_timeout_seconds,
        ) {
            if w >= t {
                return Err("Inactivity warning must come before the inactivity timeout".into());
            }
        }
        if !(5..=300).contains(&self.cancel_grace_seconds) {
            return Err("Cancellation grace must be 5–300 seconds".into());
        }
        Ok(())
    }
}

/// Monotonic milliseconds since process start; immune to wall-clock jumps.
pub fn now_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64
}
fn default_cell() -> &'static Mutex<Policy> {
    static CELL: OnceLock<Mutex<Policy>> = OnceLock::new();
    CELL.get_or_init(Default::default)
}
pub fn set_default(policy: Policy) {
    *default_cell().lock().unwrap() = policy;
}
pub fn default_policy() -> Policy {
    default_cell().lock().unwrap().clone()
}
/// Effective policy is fixed when a turn launches; later edits affect later turns.
pub fn effective(session_override: Option<Policy>) -> Policy {
    session_override.unwrap_or_else(default_policy)
}

fn digest(value: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    value.to_string().hash(&mut h);
    h.finish()
}
fn empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

/// Per-turn activity. Meaningful output resets inactivity; heartbeats,
/// status-only updates and repeated identical snapshots do not.
#[derive(Default)]
pub struct Activity {
    pub last: Option<u64>,
    generation: u64,
    snapshots: HashMap<String, u64>,
}
impl Activity {
    fn changed(&mut self, key: &str, value: &Value) -> bool {
        let d = digest(value);
        self.snapshots.insert(key.to_owned(), d) != Some(d)
    }
    pub fn meaningful(&mut self, update: &Value) -> bool {
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk" | "agent_thought_chunk") => {
                let c = &update["content"];
                !(c["type"] == "text" && empty(c.get("text"))) && !empty(Some(c))
            }
            // Only a new call (or a changed one) counts; repeated announcements are heartbeats.
            Some("tool_call") => {
                let id = update["toolCallId"].as_str().unwrap_or_default();
                self.changed(&format!("{id}\0call"), update)
            }
            Some("plan") => self.changed("\0plan", &update["entries"]),
            Some("tool_call_update") => {
                let id = update["toolCallId"].as_str().unwrap_or_default();
                let terminal = matches!(update["status"].as_str(), Some("completed" | "failed"))
                    && self.changed(&format!("{id}\0status"), &update["status"]);
                let output = (!empty(update.get("content")) || !empty(update.get("rawOutput")))
                    && self.changed(id, &json!([update["content"], update["rawOutput"]]));
                terminal || output
            }
            _ => false,
        }
    }
}

fn turns() -> &'static Mutex<HashMap<String, Activity>> {
    static CELL: OnceLock<Mutex<HashMap<String, Activity>>> = OnceLock::new();
    CELL.get_or_init(Default::default)
}
/// Start a fresh activity record; stale values from earlier turns are discarded.
/// The guard removes it on every exit path (errors, `?`, task abort/Suspend).
pub fn begin(session: &str) -> TurnGuard {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    turns().lock().unwrap().insert(
        session.into(),
        Activity {
            generation,
            ..Default::default()
        },
    );
    TurnGuard {
        session: session.into(),
        generation,
    }
}
pub struct TurnGuard {
    session: String,
    generation: u64,
}
impl Drop for TurnGuard {
    fn drop(&mut self) {
        let mut turns = turns().lock().unwrap();
        // A newer turn's record is never removed by an older guard.
        if turns
            .get(&self.session)
            .is_some_and(|t| t.generation == self.generation)
        {
            turns.remove(&self.session);
        }
    }
}
/// Updates outside an active turn are ignored, so the map holds only running turns.
pub fn observe(session: &str, update: &Value, now_ms: u64) {
    if let Some(turn) = turns().lock().unwrap().get_mut(session) {
        if turn.meaningful(update) {
            turn.last = Some(now_ms);
        }
    }
}
pub fn last_activity(session: &str) -> Option<u64> {
    turns().lock().unwrap().get(session).and_then(|t| t.last)
}

#[derive(Debug, PartialEq)]
pub enum Event {
    InactivityWarning { quiet_seconds: u64 },
    TotalApproaching { remaining_seconds: u64 },
    Expired { reason: &'static str },
}

pub struct Clock {
    pub policy: Policy,
    start: u64,
    quiet_since: u64,
    wait_started: Option<u64>,
    waited_ms: u64,
    warned_quiet: bool,
    warned_total: bool,
}
impl Clock {
    pub fn new(policy: Policy, now: u64) -> Self {
        Clock {
            policy,
            start: now,
            quiet_since: now,
            wait_started: None,
            waited_ms: 0,
            warned_quiet: false,
            warned_total: false,
        }
    }
    /// Sample at `now`; `activity` is the last meaningful output time and
    /// `waiting` is true while a human approval/question is pending.
    pub fn step(&mut self, now: u64, activity: Option<u64>, waiting: bool) -> Vec<Event> {
        let mut out = Vec::new();
        match (waiting, self.wait_started) {
            (true, None) => self.wait_started = Some(now),
            (false, Some(w)) => {
                self.waited_ms += now.saturating_sub(w);
                self.wait_started = None;
                // A resolved human wait restarts the quiet window.
                self.quiet_since = now;
                self.warned_quiet = false;
            }
            _ => {}
        }
        if let Some(a) = activity.filter(|a| *a > self.quiet_since) {
            self.quiet_since = a;
            self.warned_quiet = false;
        }
        if let Some(total) = self.policy.total_seconds {
            let pending = self.wait_started.map_or(0, |w| now.saturating_sub(w));
            let mut elapsed = now.saturating_sub(self.start);
            if !self.policy.total_includes_human_wait {
                elapsed = elapsed.saturating_sub(self.waited_ms + pending);
            }
            let limit = total * 1000;
            if elapsed >= limit {
                out.push(Event::Expired { reason: "total" });
                return out;
            }
            let lead = (total / 10).clamp(5, 60) * 1000;
            if !self.warned_total && limit - elapsed <= lead {
                self.warned_total = true;
                out.push(Event::TotalApproaching {
                    remaining_seconds: (limit - elapsed).div_ceil(1000),
                });
            }
        }
        if !waiting {
            let quiet = now.saturating_sub(self.quiet_since);
            if let Some(t) = self.policy.inactivity_timeout_seconds {
                if quiet >= t * 1000 {
                    out.push(Event::Expired {
                        reason: "inactivity",
                    });
                    return out;
                }
            }
            if let Some(w) = self.policy.inactivity_warning_seconds {
                if !self.warned_quiet && quiet >= w * 1000 {
                    self.warned_quiet = true;
                    out.push(Event::InactivityWarning {
                        quiet_seconds: quiet / 1000,
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn p(total: Option<u64>, warn: Option<u64>, stop: Option<u64>) -> Policy {
        Policy {
            total_seconds: total,
            inactivity_warning_seconds: warn,
            inactivity_timeout_seconds: stop,
            ..Default::default()
        }
    }
    #[test]
    fn default_preserves_fixed_deadline_and_bounds_validate() {
        let d = Policy::default();
        assert_eq!((d.total_seconds, d.cancel_grace_seconds), (Some(900), 15));
        assert_eq!(
            (d.inactivity_warning_seconds, d.inactivity_timeout_seconds),
            (None, None)
        );
        assert!(d.total_includes_human_wait);
        assert!(d.validate().is_ok());
        assert!(p(None, None, None).validate().is_ok());
        assert!(p(Some(59), None, None).validate().is_err());
        assert!(p(Some(900), Some(120), Some(120)).validate().is_err());
        assert!(Policy {
            cancel_grace_seconds: 4,
            ..d.clone()
        }
        .validate()
        .is_err());
        let back: Policy = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }
    #[test]
    fn heartbeats_and_repeated_snapshots_are_not_meaningful() {
        let mut a = Activity::default();
        let text =
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}});
        assert!(a.meaningful(&text));
        assert!(!a.meaningful(
            &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""}})
        ));
        assert!(!a.meaningful(&json!({"sessionUpdate":"usage_update"})));
        let quiet =
            json!({"sessionUpdate":"tool_call_update","toolCallId":"x","status":"in_progress"});
        assert!(!a.meaningful(&quiet));
        assert!(!a.meaningful(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"x","content":[]})
        ));
        let snap =
            json!({"sessionUpdate":"tool_call_update","toolCallId":"x","content":[{"t":"1"}]});
        assert!(a.meaningful(&snap));
        assert!(
            !a.meaningful(&snap),
            "identical snapshot must not reset inactivity"
        );
        assert!(a.meaningful(
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"x","content":[{"t":"2"}]})
        ));
        let done =
            json!({"sessionUpdate":"tool_call_update","toolCallId":"x","status":"completed"});
        assert!(a.meaningful(&done) && !a.meaningful(&done));
    }
    #[test]
    fn activity_is_per_turn_and_released_by_guard() {
        observe("t1", &json!({"sessionUpdate":"tool_call","toolCallId":"a"}), 3);
        assert_eq!(last_activity("t1"), None, "ignored outside a turn");
        let first = begin("t1");
        observe("t1", &json!({"sessionUpdate":"usage_update"}), 5);
        assert_eq!(last_activity("t1"), None);
        let call = json!({"sessionUpdate":"tool_call","toolCallId":"a","title":"run"});
        observe("t1", &call, 7);
        observe("t1", &call, 9);
        assert_eq!(last_activity("t1"), Some(7), "repeated tool_call is a heartbeat");
        let second = begin("t1");
        assert_eq!(last_activity("t1"), None, "new turn discards stale activity");
        drop(first);
        assert!(turns().lock().unwrap().contains_key("t1"), "old guard keeps newer turn");
        // Simulated abort/early return: dropping the guard always releases the entry.
        let task = std::thread::spawn(move || {
            let _g = second;
            panic!("aborted turn");
        });
        assert!(task.join().is_err());
        assert!(!turns().lock().unwrap().contains_key("t1"));
    }
    #[test]
    fn monotonic_clock_advances() {
        let a = now_ms();
        assert!(now_ms() >= a);
    }
    #[test]
    fn warns_before_stopping_quiet_tools_and_activity_resets() {
        let mut c = Clock::new(p(None, Some(60), Some(120)), 0);
        assert!(c.step(59_000, None, false).is_empty());
        assert_eq!(
            c.step(60_000, None, false),
            vec![Event::InactivityWarning { quiet_seconds: 60 }]
        );
        assert!(c.step(61_000, None, false).is_empty());
        assert!(c.step(100_000, Some(100_000), false).is_empty());
        assert!(c.step(219_000, Some(100_000), false).len() == 1);
        assert_eq!(
            c.step(220_000, Some(100_000), false),
            vec![Event::Expired {
                reason: "inactivity"
            }]
        );
    }
    #[test]
    fn human_wait_pauses_inactivity_and_total_wait_semantics() {
        let mut c = Clock::new(p(Some(100), None, Some(60)), 0);
        assert!(c.step(10_000, None, true).is_empty());
        assert!(c.step(80_000, None, true).is_empty());
        assert!(c.step(81_000, None, false).is_empty());
        assert_eq!(
            c.step(100_000, None, false),
            vec![Event::Expired { reason: "total" }]
        );
        let mut c = Clock::new(
            Policy {
                total_includes_human_wait: false,
                ..p(Some(100), None, None)
            },
            0,
        );
        c.step(0, None, true);
        c.step(500_000, None, false);
        assert_eq!(
            c.step(590_000, None, false),
            vec![Event::TotalApproaching {
                remaining_seconds: 10
            }]
        );
        assert_eq!(
            c.step(600_000, None, false),
            vec![Event::Expired { reason: "total" }]
        );
    }
    #[test]
    fn disabled_total_supports_long_work() {
        let mut c = Clock::new(p(None, None, None), 0);
        assert!(c.step(30 * 86_400_000, None, false).is_empty());
    }
}
