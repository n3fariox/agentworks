//! Human lifecycle controls. Metadata lives in the versioned JSON state so old databases
//! retain their original names and visibility without a destructive migration.
use crate::*;

#[derive(Default, Deserialize)]
pub struct Filter {
    // active (default), archived, or all. Archiving leaves harness execution unchanged; its terminal is stopped.
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Update {
    title: Option<String>,
    archived: Option<bool>,
    /// C-04b archive cleanup of an owned worktree; omitted means `auto`.
    worktree: Option<crate::worktree_cleanup::Cleanup>,
    /// Fingerprint from `GET /worktree` that the human confirmed for dirty removal.
    confirm_dirty: Option<String>,
}
impl Update {
    /// MCP archive exposes only the default safe cleanup policy.
    pub fn archive() -> Self {
        Self {
            title: None,
            archived: Some(true),
            worktree: None,
            confirm_dirty: None,
        }
    }
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
    if req.archived != Some(true) && (req.worktree.is_some() || req.confirm_dirty.is_some()) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "worktree and confirmDirty only apply when archiving",
        ));
    }
    let was_archived = s.data.lock().unwrap().lifecycle.archived;
    let mut cleanup = None;
    if req.archived == Some(true) && !was_archived {
        cleanup = Some(
            crate::worktree_cleanup::cleanup(
                &app,
                &s,
                req.worktree.unwrap_or_default(),
                req.confirm_dirty.as_deref(),
            )
            .await?,
        );
    }
    let mut restored = None;
    if req.archived == Some(false) && was_archived {
        // A failed or occupied restore leaves the session archived.
        restored = crate::worktree_cleanup::restore(&app, &s).await?;
    }
    let Json(mut meta) = apply(&s, title, req.archived).await?;
    if let Some(outcome) = cleanup {
        meta["worktreeCleanup"] = outcome;
    }
    if let Some(outcome) = restored {
        meta["worktreeRestore"] = outcome;
    }
    Ok(Json(meta))
}
/// Commits title/archive metadata; callers hold the run lock.
pub async fn apply(s: &Session, title: Option<String>, archived: Option<bool>) -> ApiResult {
    if archived == Some(true) {
        crate::inbox_delivery::cancel(s, "Recipient archived")?;
    }
    commit(s, title, archived)?;
    if archived == Some(true) {
        let _ = crate::terminal::archive(s).await?;
    }
    let meta = s.meta();
    s.emit("session_updated", meta.clone());
    Ok(Json(meta))
}
/// Commit before publishing: failed metadata changes are never acknowledged.
pub fn commit(
    s: &Session,
    title: Option<String>,
    archived: Option<bool>,
) -> Result<(), (StatusCode, String)> {
    let mut data = s.data.lock().unwrap();
    let mut next = data.clone();
    if let Some(title) = title {
        next.lifecycle.title = Some(title);
    }
    if let Some(archived) = archived {
        next.lifecycle.archived = archived;
    }
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
    Ok(())
}
pub async fn stop_worker(s: &Session) {
    let worker = s.worker.lock().unwrap().take();
    let commands = s.commands.lock().unwrap().take();
    if let Some(mut worker) = worker {
        let supports_close =
            s.data.lock().unwrap().agent_capabilities["sessionCapabilities"]["close"].is_object();
        if !worker.is_finished() && supports_close {
            s.clear_approvals();
            let (reply, result) = tokio::sync::oneshot::channel();
            let sent = commands.as_ref().is_some_and(|tx| {
                tx.try_send(crate::session_runtime::Command::Close(reply))
                    .is_ok()
            });
            let outcome = if sent {
                match tokio::time::timeout(std::time::Duration::from_secs(6), result).await {
                    Ok(Ok(Ok(()))) => "closed",
                    Ok(Ok(Err(_))) => "failed",
                    Ok(Err(_)) => "disconnected",
                    Err(_) => "timeout",
                }
            } else {
                "unavailable"
            };
            s.emit("provider_close", json!({"outcome":outcome}));
            if outcome == "closed"
                && tokio::time::timeout(std::time::Duration::from_secs(1), &mut worker)
                    .await
                    .is_ok()
            {
                return;
            }
        }
        worker.abort();
        // Join before acknowledging shutdown: the old runtime must never
        // publish a late ready/idle or rewrite provider history after delete.
        let _ = worker.await;
    }
}
pub async fn suspend_locked(s: &Session) -> ApiResult {
    let inbox_error = crate::inbox_delivery::cancel(s, "Explicit suspend").err();
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
    if let Some(error) = inbox_error {
        return Err(error);
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
