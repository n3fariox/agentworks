//! Subscription quota belongs to an account, not a session. Every session on the
//! same host and provider family shares one sign-in, so the latest report from any
//! of them is kept here and pushed to all browsers at once.
//!
//! Goose reaches models through provider profiles, so its family is the profile;
//! it may mirror another harness's account, but it is never merged with one.
use crate::{ApiResult, App, Session, fail, session};
use axum::{Json, extract::Path, http::StatusCode};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Mutex};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;

type QuotaCache = Option<(Instant, Result<Value, String>)>;

const CLAUDE_WINDOWS: [&str; 2] = ["five_hour", "seven_day"];
const CODEX_FIELDS: [&str; 3] = ["usedPercent", "windowDurationMins", "resetsAt"];

pub fn family(provider: &str, model_provider: Option<&str>) -> String {
    match provider {
        "claude" | "claude-rust" => "claude".into(),
        "codex" | "codex-rust" => "codex".into(),
        "goose" => format!("goose/{}", model_provider.unwrap_or("default")),
        other => other.into(),
    }
}

pub struct Hub {
    latest: Mutex<HashMap<(String, String), Value>>,
    tx: broadcast::Sender<Value>,
    antigravity_refresh: Mutex<HashMap<String, Arc<tokio::sync::Mutex<QuotaCache>>>>,
}

impl Default for Hub {
    fn default() -> Self {
        Hub {
            latest: Default::default(),
            tx: broadcast::channel(64).0,
            antigravity_refresh: Default::default(),
        }
    }
}

impl Hub {
    /// Records quota carried by an ACP `usage_update`. `time` is when Agentworks
    /// received it; Claude windows carry no source time of their own.
    pub fn observe(&self, host_id: &str, family: &str, session_id: &str, time: u64, data: &Value) {
        if data["sessionUpdate"] != "usage_update" {
            return;
        }
        let meta = &data["_meta"];
        let rate_limit = &meta["_claude/rateLimit"];
        let codex = &meta["_codex/subscriptionLimits"];
        let has_claude = CLAUDE_WINDOWS
            .iter()
            .any(|w| claude_window(rate_limit, w).is_some());
        let has_codex = codex["version"] == 1 && codex["limits"].is_array();
        if !has_claude && !has_codex {
            return;
        }
        let entry = {
            let mut latest = self.latest.lock().unwrap();
            let entry = latest
                .entry((host_id.to_owned(), family.to_owned()))
                .or_insert_with(|| json!({"hostId":host_id,"family":family}));
            let mut changed = false;
            if has_claude {
                changed |= merge_claude(entry, rate_limit, session_id, time);
            }
            if has_codex {
                changed |= merge_codex(entry, &codex["limits"], session_id, time);
            }
            if !changed {
                return;
            }
            entry.clone()
        };
        let _ = self.tx.send(json!({"kind":"quota","entry":entry}));
    }

    pub fn observe_antigravity(&self, host: &str, session: &str, report: &Value) {
        let Some(at) = report["observedAtMs"].as_u64() else {
            return;
        };
        if !report["windows"].is_array() {
            return;
        }
        let entry = {
            let mut latest = self.latest.lock().unwrap();
            let entry = latest
                .entry((host.into(), "antigravity".into()))
                .or_insert_with(|| json!({"hostId":host,"family":"antigravity"}));
            if entry["antigravity"]["observedAtMs"].as_u64() > Some(at)
                || (entry["antigravity"]["observedAtMs"].as_u64() == Some(at)
                    && entry["antigravity"]["sessionId"] == session)
            {
                return;
            }
            entry["antigravity"] = report.clone();
            entry["antigravity"]["sessionId"] = json!(session);
            entry.clone()
        };
        let _ = self.tx.send(json!({"kind":"quota","entry":entry}));
    }

    async fn refresh_antigravity(&self, host: &str) -> Result<Value, String> {
        // Coalesce browser/turn requests and avoid repeated backend requests.
        let gate = self
            .antigravity_refresh
            .lock()
            .unwrap()
            .entry(host.to_owned())
            .or_default()
            .clone();
        let mut cache = gate.lock().await;
        if let Some((at, result)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(30) {
                return result.clone();
            }
        }
        let result = if host == "local" {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            agentworks_runtime::antigravity::quota(crate::login_env::changes(&home, false).await)
                .await
        } else {
            match crate::hosts::registry().and_then(|hosts| hosts.live(host)) {
                Some(live) => live.antigravity_quota().await,
                None => Err("That host is not connected".into()),
            }
        }
        .map(|mut report| {
            report["observedAtMs"] = json!(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            );
            report
        });
        *cache = Some((Instant::now(), result.clone()));
        result
    }

    fn snapshot(&self) -> Value {
        let latest = self.latest.lock().unwrap();
        json!({"kind":"snapshot","entries":latest.values().collect::<Vec<_>>()})
    }
}

/// Only `unifiedWindows` fractions in 0..1 are a used share (see `web/usage-status.js`).
fn claude_window<'a>(rate_limit: &'a Value, key: &str) -> Option<&'a Value> {
    let window = &rate_limit["unifiedWindows"][key];
    window["utilization"]
        .as_f64()
        .is_some_and(|u| (0.0..=1.0).contains(&u))
        .then_some(window)
}

/// Each window keeps its own receipt time, so a report that omits a window
/// does not make the retained one look fresher than it is.
fn merge_claude(entry: &mut Value, rate_limit: &Value, session_id: &str, time: u64) -> bool {
    if !entry["claude"].is_object() {
        entry["claude"] = json!({"windows":{}});
    }
    let claude = &mut entry["claude"];
    let mut changed = false;
    for key in CLAUDE_WINDOWS {
        let Some(window) = claude_window(rate_limit, key) else {
            continue;
        };
        if claude["windows"][key]["at"].as_u64() > Some(time) {
            continue;
        }
        let status = if rate_limit["rateLimitType"] == key {
            rate_limit["status"].clone()
        } else {
            Value::Null
        };
        claude["windows"][key] = json!({"utilization":window["utilization"],"resetsAt":window["resetsAt"],
            "status":status,"at":time,"sessionId":session_id});
        changed = true;
    }
    changed
}

/// Codex cells carry adapter observation times. A session repeating an older
/// cached cell never replaces a newer one reported elsewhere.
fn merge_codex(entry: &mut Value, limits: &Value, session_id: &str, time: u64) -> bool {
    if !entry["codex"].is_object() {
        entry["codex"] = json!({"limits":{}});
    }
    let stored = entry["codex"]["limits"].as_object_mut().unwrap();
    let mut changed = false;
    for limit in limits.as_array().into_iter().flatten() {
        let Some(id) = limit["limitId"].as_str() else {
            continue;
        };
        let target = stored.entry(id).or_insert_with(|| json!({"limitId":id}));
        if let Some(name) = limit.get("limitName") {
            target["limitName"] = name.clone();
        }
        for role in ["primary", "secondary"] {
            let Some(raw) = limit.get(role) else { continue };
            if raw.is_null() {
                changed |= target.as_object_mut().unwrap().remove(role).is_some();
                continue;
            }
            if !raw.is_object() {
                continue;
            }
            if !target[role].is_object() {
                target[role] = Value::Object(Map::new());
            }
            for field in CODEX_FIELDS {
                let Some(cell) = raw.get(field) else { continue };
                let incoming = cell["observedAtMs"].as_u64().unwrap_or(time);
                if target[role][field]["observedAtMs"].as_u64() >= Some(incoming) {
                    continue;
                }
                target[role][field] = cell.clone();
                changed = true;
            }
        }
        target["sessionId"] = json!(session_id);
    }
    entry["codex"]["at"] = json!(time);
    changed
}

pub fn refresh_session(s: &Arc<Session>) {
    if s.provider != "antigravity" {
        return;
    }
    let s = s.clone();
    tokio::spawn(async move {
        match s.account_quota.refresh_antigravity(&s.host_id).await {
            Ok(report) => s.emit("antigravity_quota", report),
            Err(error) => tracing::debug!(%error, "Antigravity quota refresh failed"),
        }
    });
}

pub async fn refresh(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    if s.provider != "antigravity" {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Quota refresh is available for Antigravity sessions",
        ));
    }
    let report = app
        .account_quota
        .refresh_antigravity(&s.host_id)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    s.emit("antigravity_quota", report.clone());
    Ok(Json(report))
}

pub async fn events(State(app): State<App>, ws: WebSocketUpgrade) -> Response {
    let hub = app.account_quota.clone();
    ws.on_upgrade(move |socket| quota_socket(socket, hub))
}

async fn quota_socket(socket: WebSocket, hub: std::sync::Arc<Hub>) {
    // Subscribe before the snapshot so no report falls between them.
    let mut rx = hub.tx.subscribe();
    let (mut sink, mut stream) = socket.split();
    if sink
        .send(Message::Text(hub.snapshot().to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            next=rx.recv()=>{
                let message=match next {
                    Ok(event)=>event,
                    // A lagging tab catches up from the full current state.
                    Err(broadcast::error::RecvError::Lagged(_))=>hub.snapshot(),
                    Err(_)=>break,
                };
                if sink.send(Message::Text(message.to_string().into())).await.is_err(){break;}
            }
            incoming=stream.next()=>match incoming {
                Some(Ok(Message::Ping(data)))=>{if sink.send(Message::Pong(data)).await.is_err(){break;}}
                Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,
                _=>{}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(utilization: f64) -> Value {
        json!({"sessionUpdate":"usage_update","_meta":{"_claude/rateLimit":{"status":"allowed","rateLimitType":"five_hour",
            "unifiedWindows":{"five_hour":{"utilization":utilization,"resetsAt":100}}}}})
    }
    fn codex(used: f64, at: u64) -> Value {
        json!({"sessionUpdate":"usage_update","_meta":{"_codex/subscriptionLimits":{"version":1,"limits":[
            {"limitId":"codex","primary":{"usedPercent":{"value":used,"observedAtMs":at}}}]}}})
    }
    fn entry(hub: &Hub, host: &str, family: &str) -> Value {
        hub.latest.lock().unwrap()[&(host.to_owned(), family.to_owned())].clone()
    }

    #[test]
    fn antigravity_snapshot_retains_source_time_and_host_boundaries() {
        let hub = Hub::default();
        let report = |at, value| json!({"observedAtMs":at,"windows":[{"id":"5h","remainingFraction":value}]});
        hub.observe_antigravity("local", "a", &report(200, 0.8));
        hub.observe_antigravity("local", "b", &report(100, 0.9));
        hub.observe_antigravity("remote", "c", &report(300, 0.3));
        assert_eq!(
            entry(&hub, "local", "antigravity")["antigravity"]["observedAtMs"],
            200
        );
        assert_eq!(
            entry(&hub, "local", "antigravity")["antigravity"]["windows"][0]["remainingFraction"],
            0.8
        );
        assert_eq!(
            entry(&hub, "remote", "antigravity")["antigravity"]["windows"][0]["remainingFraction"],
            0.3
        );
    }

    #[test]
    fn families_share_harnesses_but_not_goose_profiles() {
        assert_eq!(family("claude-rust", None), family("claude", None));
        assert_eq!(family("codex-rust", Some("x")), "codex");
        assert_eq!(family("goose", Some("work")), "goose/work");
        assert_ne!(family("goose", Some("work")), family("goose", None));
    }

    #[test]
    fn latest_report_from_any_session_wins_per_host() {
        let hub = Hub::default();
        let mut rx = hub.tx.subscribe();
        hub.observe("local", "claude", "a", 10, &claude(0.2));
        hub.observe("local", "claude", "b", 20, &claude(0.5));
        hub.observe("rock", "claude", "c", 30, &claude(0.9));
        let local = entry(&hub, "local", "claude");
        assert_eq!(local["claude"]["windows"]["five_hour"]["utilization"], 0.5);
        assert_eq!(local["claude"]["windows"]["five_hour"]["sessionId"], "b");
        assert_eq!(local["claude"]["windows"]["five_hour"]["status"], "allowed");
        assert_eq!(
            entry(&hub, "rock", "claude")["claude"]["windows"]["five_hour"]["utilization"],
            0.9
        );
        assert_eq!(rx.try_recv().unwrap()["kind"], "quota");
        // An out-of-order older report does not replace a newer one.
        hub.observe("local", "claude", "a", 15, &claude(0.1));
        assert_eq!(
            entry(&hub, "local", "claude")["claude"]["windows"]["five_hour"]["utilization"],
            0.5
        );
    }

    #[test]
    fn codex_cells_keep_the_newest_adapter_observation() {
        let hub = Hub::default();
        hub.observe("local", "codex", "a", 1, &codex(40.0, 2000));
        hub.observe("local", "codex", "b", 2, &codex(10.0, 1000));
        let cell =
            &entry(&hub, "local", "codex")["codex"]["limits"]["codex"]["primary"]["usedPercent"];
        assert_eq!(cell["value"], 40.0);
    }

    #[test]
    fn ignores_updates_without_quota() {
        let hub = Hub::default();
        hub.observe(
            "local",
            "claude",
            "a",
            1,
            &json!({"sessionUpdate":"usage_update","used":1,"size":2}),
        );
        hub.observe(
            "local",
            "claude",
            "a",
            1,
            &json!({"sessionUpdate":"agent_message_chunk"}),
        );
        assert!(hub.latest.lock().unwrap().is_empty());
    }
}
