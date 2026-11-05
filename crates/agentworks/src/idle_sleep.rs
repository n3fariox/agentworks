//! Idle sleep: releases a harness that has sat idle between turns for the saved
//! number of minutes. Like a restart, it keeps the transcript, native context,
//! workspace and terminal. Opening the session, a prompt, queued input or peer
//! mail wakes it; an explicit suspend does not count as sleep.
use crate::{ApiResult, App, Session, fail};
use agentworks_session::{async_tasks, timeouts::now_ms};
use axum::http::StatusCode;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// How long a session may stay idle; None never sleeps.
fn limit_ms(app: &App) -> Option<u64> {
    let minutes = app
        .catalog
        .lock()
        .unwrap()
        .data
        .settings
        .idle_sleep_minutes
        .unwrap_or(crate::catalog::DEFAULT_IDLE_SLEEP_MINUTES);
    (minutes > 0).then(|| minutes as u64 * 60_000)
}

pub fn asleep(s: &Session) -> bool {
    let d = s.data.lock().unwrap();
    // The flag is set before stopping the worker. Treat that transition as
    // asleep too, so a prompt racing the sweep will wake after it gets run_lock.
    d.lifecycle.asleep && !d.lifecycle.suspended
}

/// Idle with nothing in flight that a harness restart would interrupt or lose.
pub(crate) fn sleepable(s: &Session) -> bool {
    // Approvals before session state, as in the other schedulers.
    if !s.approvals.lock().unwrap().is_empty() {
        return false;
    }
    let d = s.data.lock().unwrap();
    d.status == "idle"
        && !d.turn_active
        && !d.lifecycle.suspended
        && d.provider_session_id.is_some()
        && d.storage_error.is_none()
        && d.inbox_turn.is_none()
        && d.queued_prompt.is_none()
        && !d.input_queue.has_pending()
        && !d.continuation.limited
        && d.continuation.due_at.is_none()
        && d.async_tasks
            .tasks
            .iter()
            .all(|t| async_tasks::terminal(t["state"].as_str().unwrap_or("")))
}

/// Caller holds `run_lock`.
pub(crate) async fn sleep(s: &Session, idle_ms: u64) {
    // Set before the status event so it reports the session as asleep.
    s.data.lock().unwrap().lifecycle.asleep = true;
    crate::lifecycle::stop_worker(s).await;
    s.clear_approvals();
    s.status("suspended");
    tracing::info!(
        session = %s.id,
        idle_minutes = idle_ms / 60_000,
        "Idle session put to sleep"
    );
}

/// Starts waking a sleeping session without waiting for it. Takes `run_lock`.
pub async fn start_waking(app: &App, s: &Arc<Session>) -> Result<(), (StatusCode, String)> {
    let _run = app.run_lock.lock().await;
    crate::service_restart::allow_start(app)?;
    if asleep(s) {
        let _ = crate::resume_locked(app, s).await?;
    }
    Ok(())
}

/// Wakes a sleeping session for a prompt and waits until it can take a turn.
/// Never called with `communication_lock` held: the waking harness needs it
/// for its own MCP connection.
pub async fn wake_for_prompt(app: &App, s: &Arc<Session>) -> Result<(), (StatusCode, String)> {
    start_waking(app, s).await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        match s.data.lock().unwrap().status.as_str() {
            "resuming" | "starting" | "configuring" => {}
            "error" | "closed" => {
                return Err(fail(
                    StatusCode::CONFLICT,
                    "This session was asleep and could not wake; inspect the session error",
                ));
            }
            _ => return Ok(()),
        }
        if std::time::Instant::now() > deadline {
            return Err(fail(
                StatusCode::CONFLICT,
                "This session is still waking up; try again shortly",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wakes a sleeping session for queued work. Caller holds `run_lock`. A failed
/// wake (such as the active-harness limit) leaves it asleep for the next tick.
pub async fn wake(app: &App, s: &Arc<Session>) -> ApiResult {
    crate::service_restart::allow_start(app)?;
    crate::resume_locked(app, s).await
}

pub async fn scheduler(app: App) {
    // Session id -> (last prompt seq, when it was first seen idle with it).
    let mut idle: HashMap<String, (Option<u64>, u64)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        let limit = limit_ms(&app);
        let sessions: Vec<Arc<Session>> = app.sessions.lock().unwrap().values().cloned().collect();
        idle.retain(|id, _| sessions.iter().any(|s| &s.id == id));
        let now = now_ms();
        let mut due = Vec::new();
        for s in &sessions {
            if !sleepable(s) {
                idle.remove(&s.id);
                continue;
            }
            // A turn shorter than one tick still records a new prompt.
            let last = s
                .data
                .lock()
                .unwrap()
                .last_prompt
                .as_ref()
                .and_then(|e| e["seq"].as_u64());
            let since = idle.entry(s.id.clone()).or_insert((last, now));
            if since.0 != last {
                *since = (last, now);
            }
            let idle_ms = now.saturating_sub(since.1);
            if limit.is_some_and(|limit| idle_ms >= limit) {
                due.push((s.clone(), idle_ms));
            }
        }
        if due.is_empty() {
            continue;
        }
        // Match the bridge, inbox and queue lock order. This prevents an MCP
        // prompt from passing its asleep check while the sweep starts stopping
        // that session under the bridge's communication lock.
        let _communication = app.communication_lock.lock().await;
        let _run = app.run_lock.lock().await;
        if crate::service_restart::allow_start(&app).is_err() {
            continue;
        }
        // The setting can change while this sweep waits for either gate.
        let current_limit = limit_ms(&app);
        for (s, idle_ms) in due {
            // Re-checked under the lock: a prompt may have started meanwhile.
            if current_limit.is_some_and(|limit| idle_ms >= limit) && sleepable(&s) {
                sleep(&s, idle_ms).await;
                idle.remove(&s.id);
            }
        }
    }
}
