//! Opt-in durable quota continuation. Only runtime transport callbacks supply evidence.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, State as AppState},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

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
struct Evidence {
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
    fn eligible(&self, time: u64) -> bool {
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
pub fn inherit(parent: &Session) -> Result<State, (StatusCode, String)> {
    let data = parent.data.lock().unwrap();
    let policy = &data.continuation;
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
        parent: Some(parent.id.clone()),
        parent_generation: Some(policy.authorization),
        deadline: policy.deadline,
        max_attempts: policy.max_attempts - policy.attempts,
        ..Default::default()
    })
}
pub async fn configure(
    AppState(app): AppState<App>,
    Path(id): Path<String>,
    Json(input): Json<PolicyInput>,
) -> ApiResult {
    let _gate = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let manual = input.manual_due_at;
    if let Some(due) = manual {
        if !input.enabled {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Manual scheduling requires enabled=true",
            ));
        }
        let mut data = s.data.lock().unwrap();
        let p = &mut data.continuation;
        if !p.eligible(now()) || due < now() || !p.deadline.is_some_and(|d| due < d) {
            return Err(fail(
                StatusCode::CONFLICT,
                "Enable policy on a limited session and choose a time before its deadline",
            ));
        }
        p.generation += 1;
        p.due_at = Some(due);
        p.source = Some("human-selected time; provider reset unknown".into());
        p.phase = Some("scheduled".into());
    } else {
        let mut policy = new_policy(Some(input))?;
        let mut data = s.data.lock().unwrap();
        policy.generation = data.continuation.generation + 1;
        policy.authorization = data.continuation.authorization + 1;
        // Enabling does not revive a cancelled timer. The human may schedule a known limited pause.
        policy.limited = data.continuation.limited;
        policy.observed_at = data.continuation.observed_at;
        policy.source = data.continuation.source.clone();
        policy.reason = Some(
            if policy.enabled {
                "Human enabled continuation"
            } else {
                "Human disabled continuation"
            }
            .into(),
        );
        policy.status_probe = data.continuation.status_probe.take();
        data.continuation = policy;
    }
    s.emit(
        "continuation",
        json!({"decision":"human policy/schedule change"}),
    );
    Ok(Json(s.meta()))
}
fn timestamp(value: &Value) -> Option<u64> {
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
fn error_evidence(provider: &str, error: &Value) -> Option<Evidence> {
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
pub fn observe(s: &Session, update: &Value) {
    if !matches!(s.provider.as_str(), "claude" | "demo") {
        return;
    }
    if let Some(evidence) = claude_evidence(update) {
        let mut data = s.data.lock().unwrap();
        if !data.turn_active {
            return;
        }
        let p = &mut data.continuation;
        let reset = if let Some(old) = &p.pending {
            old.reset.zip(evidence.reset).map(|(a, b)| a.max(b))
        } else {
            evidence.reset
        };
        p.pending = Some(Evidence { reset, ..evidence });
    }
}
pub fn finish(s: &Session, stop: Option<&Value>, error: Option<&Value>) {
    let mut data = s.data.lock().unwrap();
    let p = &mut data.continuation;
    finish_turn(p, &s.provider, stop, error, now());
    drop(data);
    s.emit("continuation", json!({"decision":"turn terminated"}));
}
fn finish_turn(
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
fn lineage(app: &App, s: &Session) -> bool {
    let mut policy = s.data.lock().unwrap().continuation.clone();
    let mut child = s.id.clone();
    for _ in 0..32 {
        let Some(parent_id) = policy.parent else {
            return true;
        };
        let Ok(parent) = session(app, &parent_id) else {
            return false;
        };
        if crate::communication::require(app, &parent, "start_sessions").is_err() {
            return false;
        }
        let Ok(child_session) = session(app, &child) else {
            return false;
        };
        if crate::communication::project(&parent).is_none()
            || crate::communication::project(&parent)
                != crate::communication::project(&child_session)
        {
            return false;
        }
        let next = parent.data.lock().unwrap().continuation.clone();
        if !next.enabled
            || Some(next.authorization) != policy.parent_generation
            || !next.deadline.is_some_and(|d| d > now())
        {
            return false;
        }
        child = parent_id;
        policy = next;
    }
    false
}
pub async fn scheduler(app: App) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut next_provider = std::collections::HashMap::<String, u64>::new();
    loop {
        interval.tick().await;
        // Same lock as human policy changes. No recursion through resume/prompt handlers.
        let _communication = app.communication_lock.lock().await;
        let _gate = app.run_lock.lock().await;
        let _workspace = app.workspace_lock.lock().await;
        if crate::service_restart::allow_start(&app).is_err() {
            continue;
        }
        let sessions: Vec<Arc<Session>> = app.sessions.lock().unwrap().values().cloned().collect();
        for s in &sessions {
            let p = s.data.lock().unwrap().continuation.clone();
            if !p.limited || p.due_at.is_none() {
                continue;
            }
            if !p.eligible(now()) || !lineage(&app, s) {
                {
                    let mut data = s.data.lock().unwrap();
                    data.continuation
                        .stop("Authorization expired, exhausted, stopped or revoked");
                    data.continuation.limited = true;
                    data.continuation.phase = Some("paused".into());
                }
                s.emit(
                    "continuation",
                    json!({"decision":"authorization no longer valid"}),
                );
                continue;
            }
            if p.due_at.unwrap() > now()
                || next_provider
                    .get(&format!("{}:{}", s.host_id, s.provider))
                    .is_some_and(|t| *t > now())
            {
                continue;
            }
            if crate::hosts::validate_session(&app, s).await.is_err() {
                // A disconnected host is transient. Keep the authorized timer pending;
                // reconnect alone never replays an already claimed request.
                if s.host_id != "local" && app.hosts.live(&s.host_id).is_none() {
                    continue;
                }
                s.data.lock().unwrap().continuation.due_at = None;
                s.emit("continuation",json!({"decision":"workspace or harness unavailable; manual intervention required"}));
                continue;
            }
            let active_capacity = sessions
                .iter()
                .filter(|other| {
                    other.id != s.id
                        && !matches!(
                            other.data.lock().unwrap().status.as_str(),
                            "suspended" | "error" | "closed"
                        )
                })
                .count();
            let approvals = s.approvals.lock().unwrap();
            let mut data = s.data.lock().unwrap();
            if data.turn_active || !approvals.is_empty() || data.storage_error.is_some() {
                continue;
            }
            if data.provider_session_id.is_none() {
                data.continuation.due_at = None;
                data.continuation.reason =
                    Some("Native context unavailable; manual intervention required".into());
                drop(data);
                drop(approvals);
                s.emit("continuation", json!({"decision":"missing native context"}));
                continue;
            }
            if matches!(data.status.as_str(), "suspended" | "error") {
                if active_capacity >= 12 {
                    continue;
                }
                // Clear deadline before opening: failed native resume must not loop or create context.
                data.continuation.due_at = None;
                data.continuation.phase = Some("opening".into());
                data.status = "resuming".into();
                drop(data);
                drop(approvals);
                s.emit(
                    "continuation",
                    json!({"decision":"opening saved native context"}),
                );
                crate::start_worker(&app, s);
                continue;
            }
            if data.status != "idle" {
                continue;
            }
            let generation = data.continuation.generation;
            let attempt = data.continuation.attempts + 1;
            let request = format!("limit:{}:{}:{}", s.id, generation, attempt);
            // A previous local read may have lost its transport without a turn response.
            // This claim is purposefully the original assignment, never that abandoned read.
            data.continuation.status_probe = None;
            data.continuation.pending = None;
            data.continuation.attempts = attempt;
            data.continuation.request_id = Some(request.clone());
            data.continuation.phase = Some("claimed".into());
            data.continuation.due_at = None;
            data.requests.insert(request.clone());
            data.turn_active = true;
            data.status = "running".into();
            drop(data);
            drop(approvals);
            // FULL synchronous SQLite event commits state + claim + request together before send.
            s.emit(
                "continuation",
                json!({"decision":"claimed","requestId":request}),
            );
            if s.data.lock().unwrap().storage_error.is_some() {
                continue;
            }
            let text = format!(
                "[Agentworks usage-limit continuation; request {request}] Inspect the current conversation, task and tool results, then continue only the unfinished assigned task within its saved scope and stop instructions. Do not replay a prior command or turn. If finished or instructed to stop, stop and report completion."
            );
            s.emit(
                "user",
                json!({"text":text,"requestId":request,"initiatedBy":"usage-limit scheduler"}),
            );
            s.data.lock().unwrap().continuation.phase = Some("dispatched".into());
            s.emit(
                "continuation",
                json!({"decision":"dispatch recorded","requestId":request}),
            );
            if s.data.lock().unwrap().storage_error.is_some() {
                continue;
            }
            if s.send_command(crate::runtime::Command::Prompt(text))
                .is_err()
            {
                s.status("error");
                {
                    let mut data = s.data.lock().unwrap();
                    data.continuation.phase = Some("uncertain".into());
                    data.continuation.reason =
                        Some("Dispatch uncertain; inspect results before manual resume".into());
                }
                s.emit("continuation", json!({"decision":"dispatch unavailable"}));
            }
            next_provider.insert(format!("{}:{}", s.host_id, s.provider), now() + 30_000);
            break;
        }
    }
}
// Called only after successful opening of the original native context, before the next scheduler tick.
pub fn ready(s: &Session) {
    let mut data = s.data.lock().unwrap();
    if data.continuation.phase.as_deref() == Some("opening") {
        data.continuation.due_at = Some(now());
        data.continuation.phase = Some("scheduled".into());
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

/// Read subscription quota only after a trusted prompt error. No thread or turn RPC is sent.
async fn read_codex_quota() -> Option<u64> {
    if crate::use_api_key() {
        return None;
    }
    read_codex_quota_at(
        std::env::var_os("AGENTWORKS_CODEX_QUOTA_PATH").unwrap_or_else(|| "codex".into()),
    )
    .await
}
async fn read_codex_quota_at(executable: std::ffi::OsString) -> Option<u64> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let operation = async {
        let mut child = tokio::process::Command::new(executable)
            .arg("app-server")
            .env("OPENAI_API_KEY", "")
            .env("CODEX_API_KEY", "")
            .env("ANTHROPIC_API_KEY", "")
            .env("NO_BROWSER", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut input = child.stdin.take()?;
        let mut output = BufReader::new(child.stdout.take()?.take(128 * 1024)).lines();
        input.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"agentworks_quota\",\"title\":\"Agentworks quota\",\"version\":\"0.1.0\"},\"capabilities\":{\"experimentalApi\":true}}}\n").await.ok()?;
        loop {
            let line = output.next_line().await.ok()??;
            let v: Value = serde_json::from_str(&line).ok()?;
            if v["id"] == 1 {
                if v.get("error").is_some() {
                    return None;
                }
                break;
            }
        }
        input.write_all(b"{\"method\":\"initialized\",\"params\":{}}\n{\"id\":2,\"method\":\"account/rateLimits/read\",\"params\":{}}\n").await.ok()?;
        let result = loop {
            let line = output.next_line().await.ok()??;
            let v: Value = serde_json::from_str(&line).ok()?;
            if v["id"] == 2 {
                break v.get("result")?.clone();
            }
        };
        let reset = codex_reset(&result);
        let _ = child.kill().await;
        let _ = child.wait().await;
        reset
    };
    tokio::time::timeout(std::time::Duration::from_secs(8), operation)
        .await
        .ok()
        .flatten()
}
fn codex_reset(result: &Value) -> Option<u64> {
    let mut latest = None;
    let mut unknown = false;
    let snapshots: Vec<&Value> = result["rateLimitsByLimitId"]
        .as_object()
        .filter(|o| !o.is_empty())
        .map(|o| o.values().collect())
        .unwrap_or_else(|| vec![&result["rateLimits"]]);
    for snapshot in snapshots {
        if snapshot["spendControlReached"] == true {
            unknown = true;
        }
        for key in ["primary", "secondary", "individualLimit"] {
            let window = &snapshot[key];
            if window["usedPercent"].as_f64().is_some_and(|v| v >= 100.0)
                || (key == "individualLimit"
                    && window["remainingPercent"]
                        .as_f64()
                        .is_some_and(|v| v <= 0.0))
            {
                if let Some(reset) = timestamp(&window["resetsAt"]).filter(|t| *t > now()) {
                    latest = Some(latest.unwrap_or(0).max(reset));
                } else {
                    unknown = true;
                }
            }
        }
    }
    if unknown { None } else { latest }
}
pub async fn enrich_codex(s: &Session, error: &Value) {
    if s.host_id != "local" || s.provider != "codex" || error_evidence("codex", error).is_none() {
        return;
    }
    let generation = s.data.lock().unwrap().continuation.generation;
    let reset = read_codex_quota().await;
    let mut data = s.data.lock().unwrap();
    if data.continuation.generation == generation && data.turn_active {
        data.continuation.pending = Some(Evidence {
            reset,
            source: if reset.is_some() {
                "Codex account/rateLimits/read; exhausted windows (read-only)"
            } else {
                "Codex native quota error; reset unavailable or stale"
            }
            .into(),
        });
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    #[test]
    fn claude_first_rejection_and_codex_individual_window() {
        assert!(error_evidence("claude", &json!({"data":{"errorKind":"rate_limit"}})).is_some());
        assert!(
            error_evidence(
                "claude",
                &json!({"data":{"errorKind":"authentication_failed"}})
            )
            .is_none()
        );
        assert_eq!(
            codex_reset(
                &json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":9999999900u64},"individualLimit":{"remainingPercent":0,"resetsAt":9999999999u64}}})
            ),
            Some(9999999999000)
        );
        assert_eq!(
            codex_reset(
                &json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":9999999900u64},"individualLimit":{"remainingPercent":0}}})
            ),
            None
        );
    }
    #[tokio::test]
    async fn codex_read_only_protocol_uses_latest_exhausted_window() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-quota.mjs");
        let before = now();
        let reset = read_codex_quota_at(path.into_os_string()).await.unwrap();
        assert!(reset > before + 490_000 && reset < now() + 510_000);
    }
    #[test]
    fn codex_unknown_stale_and_nonblocking_windows() {
        assert_eq!(
            codex_reset(
                &json!({"rateLimits":{"primary":{"usedPercent":50,"resetsAt":9999999999u64}}})
            ),
            None
        );
        assert_eq!(
            codex_reset(&json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":1}}})),
            None
        );
        assert_eq!(
            codex_reset(&json!({"rateLimits":{"primary":{"usedPercent":100}}})),
            None
        );
        assert_eq!(
            codex_reset(
                &json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":9999999999u64},"secondary":{"usedPercent":100}}})
            ),
            None
        );
    }
}
