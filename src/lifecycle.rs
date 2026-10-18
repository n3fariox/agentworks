//! Human lifecycle controls. Metadata lives in the versioned JSON state so old databases
//! retain their original names and visibility without a destructive migration.
use crate::*;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    pub title: Option<String>,
    pub archived: bool,
    pub suspended: bool,
}
#[derive(Default, Deserialize)]
pub struct Filter {
    // active (default), archived, or all. Archiving never changes execution.
    archived: Option<String>,
}
impl Filter {
    pub fn includes(&self, archived: bool) -> bool {
        match self.archived.as_deref() {
            Some("all") => true,
            Some("true") => archived,
            _ => !archived,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    title: Option<String>,
    archived: Option<bool>,
}
pub async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Update>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let title = req.title.map(|s| s.trim().to_owned());
    if title
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.chars().count() > 80 || s.chars().any(char::is_control))
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a title of 1–80 characters without control characters",
        ));
    }
    {
        let mut data = s.data.lock().unwrap();
        let mut next = data.clone();
        if let Some(title) = title {
            next.lifecycle.title = Some(title);
        }
        if let Some(archived) = req.archived {
            next.lifecycle.archived = archived;
        }
        // Commit before publishing: failed metadata changes are never acknowledged.
        s.store
            .lock()
            .unwrap()
            .save_state(&s.id, &next)
            .map_err(|_| {
                fail(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "Could not save lifecycle metadata",
                )
            })?;
        *data = next;
    }
    let meta = s.meta();
    s.emit("session_updated", meta.clone());
    Ok(Json(meta))
}
pub async fn stop_worker(s: &Session) {
    let worker = s.worker.lock().unwrap().take();
    *s.commands.lock().unwrap() = None;
    if let Some(worker) = worker {
        worker.abort();
        // Acknowledge shutdown only after the old worker cannot publish ready/idle.
        let _ = worker.await;
    }
}
pub async fn suspend_locked(s: &Session) -> ApiResult {
    {
        let mut data = s.data.lock().unwrap();
        data.lifecycle.suspended = true;
        data.continuation.stop("Explicit suspend");
        data.queued_prompt = None;
        data.input_queue
            .block("Explicit suspend; inspect, then resume.");
    }
    s.emit("continuation", json!({"decision":"Explicit suspend"}));
    stop_worker(s).await;
    s.clear_approvals();
    if s.data.lock().unwrap().turn_active {
        s.emit("interrupted", json!({"message":"Harness suspended explicitly during this turn. Inspect the last tool result before resuming; no input was replayed."}));
    }
    s.status("suspended");
    if s.data.lock().unwrap().storage_error.is_some() {
        return Err(fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Harness suspended, but saving failed; inspect server storage",
        ));
    }
    Ok(Json(s.meta()))
}
pub async fn suspend(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let restart_error = crate::service_restart::stop_requester(&app, &id).err();
    let result = suspend_locked(&s).await;
    if let Some(error) = restart_error {
        return Err(error);
    }
    result
}
