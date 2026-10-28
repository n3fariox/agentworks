//! Session archive and restore coordination for owned worktrees.
use crate::{ApiResult, App, Session, fail, session};
pub use agentworks_workspace::cleanup::{Cleanup, Owned};
use agentworks_workspace::cleanup::{Dirty, Inspection};
pub(crate) use agentworks_workspace::cleanup::{inspect_in, remove_in, restore_in};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
type Error = agentworks_workspace::Error;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn conflict(message: impl Into<String>) -> Error {
    fail(StatusCode::CONFLICT, message)
}
pub(crate) async fn inspect(app: &App, s: &Session) -> Inspection {
    let (owned, busy) = {
        let d = s.data.lock().unwrap();
        (
            d.worktree.clone(),
            d.turn_active
                || matches!(
                    d.status.as_str(),
                    "running" | "waiting" | "cancelling" | "starting" | "resuming" | "configuring"
                ),
        )
    };
    let shared_with = owned
        .as_ref()
        .map(|o| {
            app.sessions
                .lock()
                .unwrap()
                .values()
                .filter(|other| {
                    other.id != s.id && other.host_id == s.host_id && other.cwd.starts_with(&o.path)
                })
                .map(|other| json!({"id":other.id,"title":other.display_title()}))
                .collect()
        })
        .unwrap_or_default();
    let mut result = if s.host_id == "local" || owned.is_none() {
        inspect_in(app, owned).await
    } else {
        match remote(app, s, "inspectWorktree", json!({}))
            .await
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|_| conflict("Invalid remote worktree inspection"))
            }) {
            Ok(result) => result,
            Err((_, reason)) => Inspection {
                owned,
                state: "unavailable".into(),
                dirty: None,
                fingerprint: None,
                locked: false,
                shared_with: Vec::new(),
                blocker: Some(reason),
            },
        }
    };
    result.shared_with = shared_with;
    if result.blocker.is_none() && result.state == "present" {
        result.blocker = if busy {
            Some("Session is running; stop it before removing its worktree".into())
        } else if !result.shared_with.is_empty() {
            Some("Another session uses this worktree".into())
        } else {
            None
        };
    }
    result
}

fn view(s: &Session, i: &Inspection) -> Value {
    let dirty = i.dirty.as_ref().is_some_and(Dirty::any);
    json!({
        "owned": i.owned.is_some(),
        "state": i.state,
        "path": i.owned.as_ref().map_or(&s.cwd, |o| &o.path),
        "branch": i.owned.as_ref().map(|o| &o.branch),
        "baseCommit": i.owned.as_ref().map(|o| &o.base_commit),
        "removedAt": i.owned.as_ref().and_then(|o| o.removed),
        "population": i.owned.as_ref().and_then(|o| o.population.as_ref()),
        "hooks": i.owned.as_ref().and_then(|o| o.hooks.as_ref()),
        "dirty": i.dirty,
        "fingerprint": i.fingerprint,
        "locked": i.locked,
        "sharedWith": i.shared_with,
        "cleanup": {
            "allowed": i.state == "present" && i.blocker.is_none(),
            "requiresConfirmation": dirty,
            "reason": match i.state.as_str() {
                "unowned" => Some("This session did not create its worktree; Agentworks never removes it".into()),
                "removed" => Some("Removed on archive; unarchive to recreate it from the retained branch".into()),
                _ => i.blocker.clone(),
            },
        },
        "integration": {
            "merge": false,
            "note": "Agentworks never merges, resets or deletes this branch. Integration remains a separate explicit human or coordinator action.",
        },
    })
}

fn persist(s: &Session) -> Result<(), Error> {
    let data = s.data.lock().unwrap();
    s.store
        .lock()
        .unwrap()
        .save_state(&s.id, &data)
        .map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Worktree state changed, but saving session metadata failed; inspect server storage",
            )
        })
}

/// Callers hold the run lock. Retained outcomes are successful for `Auto`; `Remove` fails instead.
pub async fn cleanup(
    app: &App,
    s: &Session,
    mode: Cleanup,
    confirm: Option<&str>,
) -> Result<Value, Error> {
    let first = inspect(app, s).await;
    let summary = view(s, &first);
    let retained = |reason: &str| -> Result<Value, Error> {
        if mode == Cleanup::Remove {
            return Err(conflict(format!("Worktree not removed: {reason}")));
        }
        Ok(json!({"outcome":"retained","reason":reason,"worktree":summary}))
    };
    match first.state.as_str() {
        "unowned" => return Ok(json!({"outcome":"none","worktree":summary})),
        "removed" => return Ok(json!({"outcome":"removed","worktree":summary})),
        _ => {}
    }
    // Validate dirty-removal consent before an exit hook can change files.
    if mode == Cleanup::Remove
        && first.dirty.as_ref().is_some_and(Dirty::any)
        && confirm != first.fingerprint.as_deref()
    {
        return Err(conflict(
            "Worktree changes differ from the confirmed state; review them and confirm again",
        ));
    }
    crate::git_settings::exit(app, s, "archive").await?;
    if mode == Cleanup::Keep {
        return Ok(json!({"outcome":"retained","reason":"Kept by request","worktree":summary}));
    }
    if let Some(reason) = &first.blocker {
        return retained(reason);
    }
    let o = first.owned.clone().expect("owned worktree");
    let dirty = first.dirty.as_ref().is_some_and(Dirty::any);
    if dirty {
        if mode == Cleanup::Auto {
            return retained(
                "Worktree has uncommitted or untracked changes; confirm removal explicitly",
            );
        }
        if confirm != first.fingerprint.as_deref() {
            return Err(conflict(
                "Worktree changes differ from the confirmed state; review them and confirm again",
            ));
        }
    }
    // A harness cannot keep running inside a removed directory.
    if s.commands.lock().unwrap().is_some() || s.data.lock().unwrap().status != "suspended" {
        let restart = crate::service_restart::stop_requester(app, &s.id).err();
        let _ = crate::lifecycle::suspend_locked(s).await?;
        if let Some(error) = restart {
            return Err(error);
        }
    }
    let _ = crate::terminal::archive(s).await?;
    let _workspace = app.workspace_lock.lock().await;
    // The harness may have written files before it stopped.
    let second = inspect(app, s).await;
    if second.blocker.is_some() || second.fingerprint != first.fingerprint {
        return retained("Worktree changed while preparing removal; inspect it and retry");
    }
    let removal = if s.host_id == "local" {
        remove_in(app, &o, first.fingerprint.as_deref(), dirty).await
    } else {
        remote(
            app,
            s,
            "removeWorktree",
            json!({"fingerprint":first.fingerprint,"force":dirty}),
        )
        .await
    };
    if let Err((_, message)) = removal {
        return retained(&message);
    }
    if let Some(w) = s.data.lock().unwrap().worktree.as_mut() {
        w.removed = Some(now());
    }
    Ok(json!({"outcome":"removed","forced":dirty,"path":o.path,"branch":o.branch}))
}

async fn remote(
    app: &App,
    s: &Session,
    operation: &str,
    mut request: Value,
) -> Result<Value, Error> {
    request["owned"] = json!(s.data.lock().unwrap().worktree);
    crate::hosts::worktree_operation(app, &s.host_id, operation, request).await
}

/// Callers hold the run lock. Recreates a removed owned checkout from its retained branch.
pub async fn restore(app: &App, s: &Session) -> Result<Option<Value>, Error> {
    let Some(o) = s.data.lock().unwrap().worktree.clone() else {
        return Ok(None);
    };
    let _workspace = app.workspace_lock.lock().await;
    let result = if s.host_id == "local" {
        restore_in(app, &o).await?
    } else {
        serde_json::from_value(remote(app, s, "restoreWorktree", json!({})).await?)
            .map_err(|_| conflict("Invalid remote restore reply"))?
    };
    if let Some(w) = s.data.lock().unwrap().worktree.as_mut() {
        w.removed = None;
        if let Some(population) = result.as_ref().and_then(|v| v["population"].as_str()) {
            w.population = Some(population.to_owned());
        }
        if let Some(hooks) = &mut w.hooks {
            if hooks.exit == agentworks_workspace::hooks::Status::Succeeded {
                hooks.enter = agentworks_workspace::hooks::Status::Pending;
                hooks.exit = agentworks_workspace::hooks::Status::Pending;
            }
        }
    }
    crate::git_settings::run(app, s, "enter", "unarchive").await?;
    Ok(result)
}

pub async fn status(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let inspection = inspect(&app, &s).await;
    Ok(Json(view(&s, &inspection)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Removal {
    confirm_dirty: Option<String>,
}
/// Explicit later removal for an archived session whose worktree was retained.
pub async fn remove(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Removal>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    if !s.data.lock().unwrap().lifecycle.archived {
        return Err(conflict("Archive the session before removing its worktree"));
    }
    remove_locked(&app, &s, req.confirm_dirty.as_deref()).await
}
async fn remove_locked(app: &App, s: &Arc<Session>, confirm: Option<&str>) -> ApiResult {
    let outcome = cleanup(app, s, Cleanup::Remove, confirm).await?;
    persist(s)?;
    s.emit("session_updated", s.meta());
    Ok(Json(outcome))
}
