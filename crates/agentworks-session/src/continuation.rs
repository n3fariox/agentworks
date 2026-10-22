//! Durable continuation policy and state transitions driven by structured quota evidence.
use crate::fail;
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub enabled: bool,
    pub generation: u64,
    #[serde(default)]
    pub authorization: u64,
    pub origin: Option<String>,
    pub parent: Option<String>,
    pub parent_generation: Option<u64>,
    pub deadline: Option<u64>,
    pub max_attempts: u32,
    pub attempts: u32,
    pub limited: bool,
    pub due_at: Option<u64>,
    pub observed_at: Option<u64>,
    #[serde(default)]
    pub limit_event: Option<String>,
    pub source: Option<String>,
    pub reason: Option<String>,
    pub request_id: Option<String>,
    pub phase: Option<String>,
    // Evidence collected during a turn is never executable until its response terminates.
    pending: Option<Evidence>,
    // Keep the assignment in place: never restore a snapshot over a later Stop/policy edit.
    #[serde(default)]
    status_probe: Option<StatusProbe>,
}
#[derive(Clone, Serialize, Deserialize)]
struct StatusProbe {
    generation: u64,
    authorization: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Evidence {
    reset: Option<u64>,
    source: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyInput {
    pub enabled: bool,
    pub deadline: Option<u64>,
    pub max_attempts: Option<u32>,
    pub manual_due_at: Option<u64>,
}
impl State {
    pub fn cancel(&mut self, reason: &str) {
        self.generation += 1;
        self.due_at = None;
        self.pending = None;
        self.limited = false;
        self.phase = None;
        self.reason = Some(reason.into());
    }
    pub fn stop(&mut self, reason: &str) {
        self.cancel(reason);
        self.enabled = false;
        self.authorization += 1;
    }
    pub fn reconcile(&mut self) {
        // Known local reads are safe to abandon; resume only the saved assignment.
        self.status_probe = None;
        // A durable claim might have reached the provider before the crash. Never replay it.
        if self.phase.as_deref() == Some("claimed") || self.phase.as_deref() == Some("dispatched") {
            self.due_at = None;
            self.phase = Some("uncertain".into());
            self.reason=Some("Server stopped during continuation dispatch; inspect results and resume deliberately".into());
        }
        if self.phase.as_deref() == Some("opening") {
            self.phase = Some("scheduled".into());
            self.due_at = Some(now());
        }
        self.pending = None;
    }
    /// Prompt-acceptance hook, called under the existing run/session serialization.
    /// This purpose is distinct from initial queued prompts and automatic continuation.
    pub fn begin_turn(&mut self, provider: &str, commands: &Value, text: &str) {
        self.status_probe = None;
        if is_status_probe(provider, commands, text) {
            self.pending = None;
            self.status_probe = Some(StatusProbe {
                generation: self.generation,
                authorization: self.authorization,
            });
        } else {
            self.cancel("New human or controller turn");
        }
    }
    pub fn eligible(&self, time: u64) -> bool {
        self.enabled
            && self.limited
            && self.attempts < self.max_attempts
            && self.deadline.is_some_and(|d| time < d)
    }
}
// Installed adapter semantics are required in addition to advertisement. A skill or
// arbitrary slash command named similarly is never enough. Arguments are excluded.
fn is_status_probe(provider: &str, commands: &Value, text: &str) -> bool {
    let command = match (provider, text.trim()) {
        ("codex", "/status") | ("demo", "/status") => "status",
        ("claude", "/usage") | ("demo", "/usage") => "usage",
        _ => return false,
    };
    commands.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["name"] == command && item["input"].is_null())
    })
}
pub fn new_policy(input: Option<PolicyInput>) -> Result<State, (StatusCode, String)> {
    let mut state = State::default();
    if let Some(input) = input {
        if input.manual_due_at.is_some() {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Manual scheduling is for an existing limited session",
            ));
        }
        if input.enabled {
            let deadline = input.deadline.unwrap_or(now() + 24 * 60 * 60 * 1000);
            let attempts = input.max_attempts.unwrap_or(3);
            if deadline <= now()
                || deadline > now() + 30 * 24 * 60 * 60 * 1000
                || !(1..=100).contains(&attempts)
            {
                return Err(fail(
                    StatusCode::BAD_REQUEST,
                    "Use 1–100 attempts and a deadline within 30 days",
                ));
            }
            state.enabled = true;
            state.origin = Some("human".into());
            state.deadline = Some(deadline);
            state.max_attempts = attempts;
        }
    }
    Ok(state)
}
pub fn timestamp(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|n| *n > 0 && *n < 100_000_000_000)
        .and_then(|n| n.checked_mul(1000))
}
fn claude_evidence(update: &Value) -> Option<Evidence> {
    if update["sessionUpdate"] != "usage_update" {
        return None;
    }
    let info = &update["_meta"]["_claude/rateLimit"];
    if info["status"] != "rejected" {
        return None;
    }
    let mut reset = timestamp(&info["resetsAt"]);
    if info["overageStatus"] == "rejected" {
        reset = reset
            .zip(timestamp(&info["overageResetsAt"]))
            .map(|(a, b)| a.max(b));
    }
    Some(Evidence {
        reset,
        source: format!(
            "Claude ACP _claude/rateLimit (rejected {})",
            match info["rateLimitType"].as_str() {
                Some("five_hour") => "five-hour",
                Some("seven_day") => "seven-day",
                Some("seven_day_opus") => "seven-day Opus",
                Some("seven_day_sonnet") => "seven-day Sonnet",
                Some("overage") => "overage",
                _ => "quota window",
            }
        ),
    })
}
pub fn error_evidence(provider: &str, error: &Value) -> Option<Evidence> {
    // No free-text interpretation: exact provider enum on the prompt RPC error only.
    if matches!(provider, "claude" | "demo") && error["data"]["errorKind"] == "rate_limit" {
        return Some(Evidence {
            reset: None,
            source: "Claude ACP prompt error errorKind=rate_limit; reset unavailable".into(),
        });
    }
    if matches!(provider, "codex" | "demo")
        && matches!(
            error["data"]["codexErrorInfo"].as_str(),
            Some("usageLimitExceeded" | "rateLimitExceeded")
        )
    {
        Some(Evidence {
            reset: None,
            source: "Codex ACP prompt error codexErrorInfo; reset unavailable".into(),
        })
    } else {
        None
    }
}
pub fn finish_turn(
    p: &mut State,
    provider: &str,
    stop: Option<&Value>,
    error: Option<&Value>,
    time: u64,
) {
    let evidence = p
        .pending
        .take()
        .or_else(|| error.and_then(|e| error_evidence(provider, e)));
    if let Some(probe) = p.status_probe.take() {
        if probe.generation != p.generation || probe.authorization != p.authorization {
            return;
        }
        if stop.is_some_and(|v| v == "cancelled") {
            p.cancel("Status check cancelled");
        } else {
            // A local check is not completion of the unfinished assignment. Only
            // fresh structured blocking evidence may extend its existing reset.
            if p.limited
                && let Some(evidence) = evidence
            {
                if let Some(reset) = evidence.reset.filter(|r| *r > time) {
                    p.observed_at = Some(time);
                    p.source = Some(evidence.source);
                    if p.eligible(time) {
                        let backoff = 1_000u64.saturating_mul(1u64 << p.attempts.min(8));
                        let due = reset.max(time + backoff).max(p.due_at.unwrap_or(0));
                        p.due_at = p.deadline.filter(|d| due < *d).map(|_| due);
                        p.phase = Some(
                            if p.due_at.is_some() {
                                "scheduled"
                            } else {
                                "paused"
                            }
                            .into(),
                        );
                    }
                }
            }
        }
        return;
    }
    if stop.is_some_and(|v| v == "cancelled") {
        p.cancel("Turn cancelled");
    } else if let Some(evidence) =
        evidence.filter(|_| error.is_some() || stop.is_some_and(|v| v == "refusal"))
    {
        p.limited = true;
        p.observed_at = Some(time);
        p.limit_event = Some(uuid::Uuid::new_v4().to_string());
        p.source = Some(evidence.source);
        p.phase = Some("paused".into());
        p.reason = Some("Paused by verified provider usage limit".into());
        if p.eligible(time) {
            let backoff = 1_000u64.saturating_mul(1u64 << p.attempts.min(8));
            // Expired timestamps at observation are stale; never fabricate a precise reset.
            p.due_at = evidence
                .reset
                .filter(|r| *r > time)
                .map(|r| r.max(time + backoff));
            if p.due_at
                .is_some_and(|due| !p.deadline.is_some_and(|d| due < d))
            {
                p.due_at = None;
                p.reason = Some("Provider reset is beyond the saved authorization deadline".into());
            }
            if p.due_at.is_some() {
                p.phase = Some("scheduled".into());
            }
        } else if p.enabled {
            p.reason = Some("Paused by usage limit; saved attempt/deadline bound reached".into());
        }
    } else {
        p.cancel(if error.is_some() {
            "Non-quota failure; automatic continuation cancelled"
        } else {
            "Turn ended normally; no continuation"
        });
    }
}
pub fn inherit(parent_id: &str, policy: &State) -> Result<State, (StatusCode, String)> {
    if !policy.enabled
        || policy.attempts >= policy.max_attempts
        || !policy.deadline.is_some_and(|d| d > now())
    {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Parent has no enabled, unexpired continuation authorization",
        ));
    }
    Ok(State {
        enabled: true,
        origin: Some("inherited".into()),
        parent: Some(parent_id.to_owned()),
        parent_generation: Some(policy.authorization),
        deadline: policy.deadline,
        max_attempts: policy.max_attempts - policy.attempts,
        ..Default::default()
    })
}
impl State {
    /// Keep an in-flight diagnostic marker when replacing human authorization.
    pub fn preserve_status_probe(&mut self, previous: &mut Self) {
        self.status_probe = previous.status_probe.take();
    }
    /// A new dispatch cannot inherit evidence from an interrupted local read.
    pub fn clear_dispatch_evidence(&mut self) {
        self.status_probe = None;
        self.pending = None;
    }
    /// Record structured usage evidence only while the caller owns an active turn.
    pub fn observe(&mut self, provider: &str, update: &Value) {
        if !matches!(provider, "claude" | "demo") {
            return;
        }
        if let Some(evidence) = claude_evidence(update) {
            let reset = if let Some(old) = &self.pending {
                old.reset.zip(evidence.reset).map(|(a, b)| a.max(b))
            } else {
                evidence.reset
            };
            self.pending = Some(Evidence { reset, ..evidence });
        }
    }
    /// Attach a read-only native quota observation to the currently owned turn.
    pub fn record_native_limit(&mut self, reset: Option<u64>, source: &str) {
        self.pending = Some(Evidence {
            reset,
            source: source.to_owned(),
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scheduled() -> State {
        State {
            enabled: true,
            generation: 7,
            authorization: 3,
            deadline: Some(100_000),
            max_attempts: 4,
            attempts: 2,
            limited: true,
            due_at: Some(5_000),
            observed_at: Some(900),
            source: Some("original verified reset".into()),
            phase: Some("scheduled".into()),
            ..Default::default()
        }
    }
    fn commands() -> Value {
        json!([{"name":"status"},{"name":"usage"}])
    }
    #[test]
    fn only_exact_advertised_known_local_commands() {
        assert!(is_status_probe("codex", &commands(), " /status\n"));
        assert!(is_status_probe("claude", &commands(), "/usage"));
        for (provider, text) in [
            ("codex", "/usage"),
            ("claude", "/status"),
            ("opencode", "/status"),
            ("codex", "/status help"),
            ("codex", "/statusx"),
            ("codex", "/STATUS"),
            ("codex", "/status\ncontinue"),
            ("codex", "status"),
        ] {
            assert!(!is_status_probe(provider, &commands(), text));
        }
        assert!(!is_status_probe("codex", &json!([]), "/status"));
        assert!(!is_status_probe(
            "codex",
            &json!([{"name":"status","input":{"hint":"task"}}]),
            "/status"
        ));
    }
    #[test]
    fn probe_keeps_assignment_bounds_and_overdue_timer_after_restart() {
        let mut p = scheduled();
        let before = serde_json::to_value(&p).unwrap();
        p.begin_turn("codex", &commands(), "/status");
        finish_turn(&mut p, "codex", Some(&json!("end_turn")), None, 6_000);
        assert_eq!(serde_json::to_value(&p).unwrap(), before);
        p.begin_turn("codex", &commands(), "/status");
        let mut restored: State =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        restored.reconcile();
        assert_eq!(serde_json::to_value(&restored).unwrap(), before);
        assert!(restored.eligible(6_000));
        assert_eq!(restored.due_at, Some(5_000));
    }
    #[test]
    fn probe_evidence_never_renews_authorization_or_invents_reset() {
        for reset in [None, Some(1), Some(20_000), Some(110_000)] {
            let mut p = scheduled();
            p.begin_turn("claude", &commands(), "/usage");
            p.pending = Some(Evidence {
                reset,
                source: "fresh verified reset".into(),
            });
            // Local status can report exhaustion and still return a successful RPC.
            finish_turn(&mut p, "claude", Some(&json!("end_turn")), None, 6_000);
            assert_eq!(
                (
                    p.generation,
                    p.authorization,
                    p.attempts,
                    p.max_attempts,
                    p.deadline
                ),
                (7, 3, 2, 4, Some(100_000))
            );
            assert_eq!(
                p.due_at,
                match reset {
                    Some(20_000) => Some(20_000),
                    Some(110_000) => None,
                    _ => Some(5_000),
                }
            );
        }
        let mut p = State::default();
        p.begin_turn("codex", &commands(), "/status");
        finish_turn(
            &mut p,
            "codex",
            None,
            Some(&json!({"data":{"codexErrorInfo":"usageLimitExceeded"}})),
            6_000,
        );
        assert!(!p.enabled);
        assert!(!p.limited);
        assert!(p.due_at.is_none());
    }
    #[test]
    fn terminal_changes_win_over_late_probe_results() {
        for reason in ["Explicit Stop", "Completion reported", "Revoked"] {
            let mut p = scheduled();
            p.begin_turn("codex", &commands(), "/status");
            p.stop(reason);
            p.pending = Some(Evidence {
                reset: Some(20_000),
                source: "late".into(),
            });
            finish_turn(
                &mut p,
                "codex",
                None,
                Some(&json!({"data":{"codexErrorInfo":"usageLimitExceeded"}})),
                6_000,
            );
            assert!(!p.enabled);
            assert!(p.due_at.is_none());
            assert_eq!(p.reason.as_deref(), Some(reason));
        }
        let mut p = scheduled();
        p.begin_turn("codex", &commands(), "/status");
        // Mirrors configure replacing the authorization, preserving only purpose.
        let marker = p.status_probe.take();
        p = State {
            authorization: 4,
            generation: 8,
            status_probe: marker,
            ..Default::default()
        };
        finish_turn(&mut p, "codex", Some(&json!("end_turn")), None, 6_000);
        assert!(p.due_at.is_none());
        assert!(!p.enabled);
        let mut p = scheduled();
        p.begin_turn("codex", &commands(), "replacement work");
        assert!(p.due_at.is_none());
        assert!(!p.limited);
    }
    #[test]
    fn exact_provider_evidence_only() {
        assert!(
            error_evidence(
                "codex",
                &json!({"data":{"codexErrorInfo":"usageLimitExceeded"}})
            )
            .is_some()
        );
        for e in [
            json!({"message":"usageLimitExceeded resets tomorrow"}),
            json!({"data":{"codexErrorInfo":"contextWindowExceeded"}}),
            json!({"data":{"codexErrorInfo":"httpConnectionFailed"}}),
        ] {
            assert!(error_evidence("codex", &e).is_none());
        }
        assert!(
            error_evidence(
                "opencode",
                &json!({"data":{"codexErrorInfo":"usageLimitExceeded"}})
            )
            .is_none()
        );
        assert!(claude_evidence(&json!({"sessionUpdate":"tool_call_update","_meta":{"_claude/rateLimit":{"status":"rejected","resetsAt":123}}})).is_none());
        assert!(claude_evidence(&json!({"sessionUpdate":"usage_update","_meta":{"_claude/rateLimit":{"status":"allowed_warning","resetsAt":123}}})).is_none());
    }
    #[test]
    fn multiwindow_and_unknown() {
        let e=claude_evidence(&json!({"sessionUpdate":"usage_update","_meta":{"_claude/rateLimit":{"status":"rejected","resetsAt":100,"overageStatus":"rejected","overageResetsAt":200}}})).unwrap();
        assert_eq!(e.reset, Some(200000));
        let e=claude_evidence(&json!({"sessionUpdate":"usage_update","_meta":{"_claude/rateLimit":{"status":"rejected","resetsAt":100,"overageStatus":"rejected"}}})).unwrap();
        assert_eq!(e.reset, None);
    }
    #[test]
    fn defaults_cancellation_and_crash() {
        assert!(!State::default().enabled);
        let mut p = new_policy(Some(PolicyInput {
            enabled: true,
            deadline: None,
            max_attempts: None,
            manual_due_at: None,
        }))
        .unwrap();
        assert_eq!(p.max_attempts, 3);
        p.limited = true;
        p.due_at = Some(now());
        assert!(p.eligible(now()));
        p.phase = Some("claimed".into());
        p.reconcile();
        assert!(p.due_at.is_none());
        assert_eq!(p.phase.as_deref(), Some("uncertain"));
        let mut opening = State {
            phase: Some("opening".into()),
            ..Default::default()
        };
        opening.reconcile();
        assert_eq!(opening.phase.as_deref(), Some("scheduled"));
        assert!(opening.due_at.is_some());
        p.stop("Stop");
        assert!(!p.enabled);
        assert!(!p.limited);
        assert_eq!(p.generation, 1);
    }
}
