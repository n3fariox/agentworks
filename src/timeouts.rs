// L-07 execution timeout policy and a pure, deterministic turn clock.
// The runtime samples the clock; nothing here retries tools or turns.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

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
    // Preserves the previous fixed behavior: 15-minute wall-clock turn, 15 s grace.
    fn default() -> Self {
        Policy {
            total_seconds: Some(900),
            total_includes_human_wait: true,
            inactivity_warning_seconds: Some(300),
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

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
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

/// Meaningful output resets inactivity; transport/status heartbeats do not.
pub fn is_meaningful(update: &Value) -> bool {
    match update["sessionUpdate"].as_str() {
        Some("agent_message_chunk" | "agent_thought_chunk" | "tool_call" | "plan") => true,
        Some("tool_call_update") => {
            update.get("content").is_some_and(|c| !c.is_null())
                || update.get("rawOutput").is_some_and(|c| !c.is_null())
                || matches!(update["status"].as_str(), Some("completed" | "failed"))
        }
        _ => false,
    }
}

fn activity() -> &'static Mutex<HashMap<String, u64>> {
    static CELL: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    CELL.get_or_init(Default::default)
}
pub fn observe(session: &str, update: &Value, now_ms: u64) {
    if is_meaningful(update) {
        activity().lock().unwrap().insert(session.into(), now_ms);
    }
}
pub fn last_activity(session: &str) -> Option<u64> {
    activity().lock().unwrap().get(session).copied()
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
    fn heartbeat_is_not_meaningful_output() {
        assert!(is_meaningful(
            &json!({"sessionUpdate":"agent_message_chunk"})
        ));
        assert!(!is_meaningful(&json!({"sessionUpdate":"usage_update"})));
        assert!(!is_meaningful(
            &json!({"sessionUpdate":"tool_call_update","status":"in_progress"})
        ));
        assert!(is_meaningful(
            &json!({"sessionUpdate":"tool_call_update","content":[{}]})
        ));
        observe("t1", &json!({"sessionUpdate":"usage_update"}), 5);
        assert_eq!(last_activity("t1"), None);
        observe("t1", &json!({"sessionUpdate":"plan"}), 7);
        assert_eq!(last_activity("t1"), Some(7));
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
