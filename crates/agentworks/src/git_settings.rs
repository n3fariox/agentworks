//! Git settings, repository editor, and durable owned-worktree hook coordination.
use crate::*;
use agentworks_workspace::hooks::{self, Status};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryRequest {
    cwd: String,
    host_id: Option<String>,
    text: Option<String>,
    fingerprint: Option<String>,
}
pub async fn repository(State(app): State<App>, Json(req): Json<RepositoryRequest>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    let defaults = app
        .catalog
        .lock()
        .unwrap()
        .data
        .settings
        .git
        .clone()
        .unwrap_or_default();
    let host = req.host_id.as_deref().unwrap_or("local");
    let value = if host != "local" {
        crate::hosts::worktree_operation(&app, host, "gitConfig", json!({"cwd":req.cwd,"text":req.text,"fingerprint":req.fingerprint,"gitSettings":defaults})).await?
    } else {
        let cwd = checked_cwd(&app, &req.cwd)?;
        agentworks_workspace::worktrees::configuration_in(
            &app,
            &cwd,
            req.text.as_deref(),
            req.fingerprint.as_deref(),
        )
        .await?
    };
    Ok(Json(value))
}

fn persist(s: &Session) -> Result<(), (StatusCode, String)> {
    let data = s.data.lock().unwrap();
    s.store
        .lock()
        .unwrap()
        .save_state(&s.id, &data)
        .map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not save worktree hook state",
            )
        })
}

/// The snapshot is saved as Running before spawning any command. Unknown outcomes are never replayed automatically.
pub async fn run(
    app: &App,
    s: &Session,
    event: &str,
    reason: &str,
) -> Result<(), (StatusCode, String)> {
    let owned = {
        let mut data = s.data.lock().unwrap();
        let Some(owned) = &mut data.worktree else {
            return Ok(());
        };
        let Some(state) = &mut owned.hooks else {
            return Ok(());
        };
        let phase = if event == "enter" {
            &mut state.enter
        } else {
            &mut state.exit
        };
        if *phase == Status::Succeeded {
            return Ok(());
        }
        if *phase == Status::Running {
            return Err(fail(
                StatusCode::CONFLICT,
                "Previous hook outcome is unknown. Inspect the workspace, then use Retry hooks.",
            ));
        }
        *phase = Status::Running;
        state.last_message = Some(format!("Running {event} hooks ({reason})"));
        owned.clone()
    };
    persist(s)?;
    s.emit(
        "worktree_hooks",
        json!({"event":event,"status":"running","reason":reason}),
    );
    let mut unknown = false;
    let result = if s.host_id == "local" {
        hooks::run(app, &owned, event, reason).await
    } else {
        match crate::hosts::worktree_operation(
            app,
            &s.host_id,
            "runWorktreeHooks",
            json!({"owned":owned,"event":event,"reason":reason}),
        )
        .await
        {
            Ok(v) if v["ok"] == true => Ok(v["message"].as_str().unwrap_or_default().to_owned()),
            Ok(v) if v["ok"] == false => Err(fail(
                StatusCode::CONFLICT,
                v["message"].as_str().unwrap_or("Remote hook failed"),
            )),
            result => {
                unknown = true;
                Err(result.err().unwrap_or_else(|| {
                    fail(StatusCode::BAD_GATEWAY, "Invalid remote hook completion")
                }))
            }
        }
    };
    let message = match &result {
        Ok(message) => message.clone(),
        Err((_, message)) => message.clone(),
    };
    let status = if unknown {
        Status::Running
    } else if result.is_ok() {
        Status::Succeeded
    } else {
        Status::Failed
    };
    {
        let mut data = s.data.lock().unwrap();
        if let Some(state) = data.worktree.as_mut().and_then(|o| o.hooks.as_mut()) {
            if event == "enter" {
                state.enter = status;
            } else {
                state.exit = status;
            }
            state.last_message = Some(message.clone());
        }
    }
    persist(s)?;
    s.emit(
        "worktree_hooks",
        json!({"event":event,"status":status,"message":message}),
    );
    result.map(|_| ())
}

/// Called under the run lock for archive/delete, even when archive keeps the checkout.
pub async fn exit(app: &App, s: &Session, reason: &str) -> Result<(), (StatusCode, String)> {
    let needed = s
        .data
        .lock()
        .unwrap()
        .worktree
        .as_ref()
        .and_then(|o| o.hooks.as_ref())
        .is_some_and(|h| h.exit != Status::Succeeded);
    if !needed {
        return Ok(());
    }
    let _workspace = app.workspace_lock.lock().await;
    exit_locked(app, s, reason).await
}

/// Caller already holds the workspace lock (project cascade deletion).
pub async fn exit_locked(app: &App, s: &Session, reason: &str) -> Result<(), (StatusCode, String)> {
    let needed = s
        .data
        .lock()
        .unwrap()
        .worktree
        .as_ref()
        .and_then(|o| o.hooks.as_ref())
        .is_some_and(|h| h.exit != Status::Succeeded);
    if !needed {
        return Ok(());
    }
    if s.data.lock().unwrap().turn_active {
        return Err(fail(
            StatusCode::CONFLICT,
            "Stop the active turn before running exit hooks",
        ));
    }
    let inspection = crate::worktree_cleanup::inspect(app, s).await;
    if let Some(reason) = inspection.blocker {
        return Err(fail(StatusCode::CONFLICT, reason));
    }
    crate::service_restart::stop_requester(app, &s.id)?;
    let _ = crate::lifecycle::suspend_locked(s).await?;
    let _ = crate::terminal::archive(s).await?;
    run(app, s, "exit", reason).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    event: String,
}
pub async fn retry(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Retry>,
) -> ApiResult {
    if !["enter", "exit"].contains(&req.event.as_str()) {
        return Err(fail(StatusCode::BAD_REQUEST, "Choose enter or exit"));
    }
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let inspection = crate::worktree_cleanup::inspect(&app, &s).await;
    if let Some(reason) = inspection.blocker {
        return Err(fail(StatusCode::CONFLICT, reason));
    }
    {
        let mut data = s.data.lock().unwrap();
        let state = data
            .worktree
            .as_mut()
            .and_then(|o| o.hooks.as_mut())
            .ok_or_else(|| fail(StatusCode::CONFLICT, "No hooks recorded for this session"))?;
        let phase = if req.event == "enter" {
            &mut state.enter
        } else {
            &mut state.exit
        };
        if *phase == Status::Succeeded {
            return Err(fail(StatusCode::CONFLICT, "Hooks already completed"));
        }
        *phase = Status::Pending;
    }
    persist(&s)?;
    let _ = lifecycle::suspend_locked(&s).await?;
    let _ = terminal::archive(&s).await?;
    let _workspace = app.workspace_lock.lock().await;
    run(&app, &s, &req.event, "retry").await?;
    Ok(Json(json!({"ok":true})))
}
