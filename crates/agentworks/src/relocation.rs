//! Change where a session works: its working directory and its ACP `additionalDirectories`.
//! ACP has no in-place change; every open request (new, load, resume, fork) carries both, so
//! the session is reopened with the new values. The conversation is kept, and the agent is
//! told with its next prompt, because paths earlier in the transcript may mean the old place.
use crate::*;

/// Additional directories one session may hold.
const MAX_ADDITIONAL: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    cwd: String,
}

pub async fn change(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Change>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let running = reopenable(&s, "Change the directory")?;
    if running {
        // A running harness must not start a turn between this check and the restart.
        service_restart::allow_start(&app)?;
    }
    let project_id = s.data.lock().unwrap().project_id.clone();
    if project_id.as_deref().is_some_and(|project| {
        app.catalog
            .lock()
            .unwrap()
            .data
            .projects
            .iter()
            .any(|p| p.id == project && p.workspace_policy.require_worktree)
    }) {
        return Err(fail(
            StatusCode::CONFLICT,
            "This project requires an isolated worktree for each session; fork into a new worktree instead",
        ));
    }
    let cwd = resolve(&app, &s, &req.cwd).await?;
    let from = s.cwd();
    if cwd == from {
        return Ok(Json(json!({"accepted":true,"unchanged":true,"cwd":cwd})));
    }
    let location = {
        let mut catalog = app.catalog.lock().unwrap();
        let location = catalog.location_on(&s.host_id, &cwd).map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save the session's directory",
            )
        })?;
        if let Some(project) = project_id.as_deref() {
            catalog
                .attach_location(project, &location.id)
                .map_err(|_| {
                    fail(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Could not add the directory to this session's project",
                    )
                })?;
        }
        location
    };
    stop(&s, running).await;
    let place = Place {
        cwd: cwd.clone(),
        location_id: location.id,
    };
    // The new working directory is no longer also an additional one.
    commit(&s, moved_note(&from, &cwd), Some(place), |data| {
        data.additional_directories.retain(|d| d != &cwd)
    })?;
    s.emit(
        "directory_changed",
        json!({"from":from,"to":cwd,"restarted":running}),
    );
    s.emit("session_updated", s.meta());
    restart(&app, &s, running).await?;
    Ok(Json(json!({"accepted":true,"cwd":cwd,"restarted":running})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Additional {
    directories: Vec<String>,
}

/// Replaces the session's additional directories with `directories`, in order.
pub async fn set_additional(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Additional>,
) -> ApiResult {
    if req.directories.len() > MAX_ADDITIONAL {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            format!("A session can have at most {MAX_ADDITIONAL} additional directories"),
        ));
    }
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let running = reopenable(&s, "Change additional directories")?;
    if running {
        service_restart::allow_start(&app)?;
    }
    // Capabilities are known once the harness has initialized; before that, the list is
    // saved and checked when it starts.
    let unsupported = {
        let data = s.data.lock().unwrap();
        let capabilities = &data.agent_capabilities["sessionCapabilities"];
        capabilities.is_object() && !capabilities["additionalDirectories"].is_object()
    };
    if unsupported && !req.directories.is_empty() {
        return Err(fail(
            StatusCode::CONFLICT,
            "This harness does not support additional directories",
        ));
    }
    let cwd = s.cwd();
    let mut directories = Vec::new();
    for path in &req.directories {
        let path = resolve(&app, &s, path).await?;
        if path != cwd && !directories.contains(&path) {
            directories.push(path);
        }
    }
    let before = s.data.lock().unwrap().additional_directories.clone();
    if directories == before {
        return Ok(Json(
            json!({"accepted":true,"unchanged":true,"directories":directories}),
        ));
    }
    stop(&s, running).await;
    let next = directories.clone();
    commit(&s, additional_note(&directories), None, |data| {
        data.additional_directories = next
    })?;
    s.emit(
        "additional_directories_changed",
        json!({"directories":directories,"previous":before,"restarted":running}),
    );
    s.emit("session_updated", s.meta());
    restart(&app, &s, running).await?;
    Ok(Json(
        json!({"accepted":true,"directories":directories,"restarted":running}),
    ))
}

/// The additional directories to send when opening the provider session. A harness that
/// does not advertise the capability gets none, and the transcript says so.
pub fn additional(s: &Session, init: &Value) -> Vec<PathBuf> {
    let directories = s.data.lock().unwrap().additional_directories.clone();
    if directories.is_empty()
        || init["agentCapabilities"]["sessionCapabilities"]["additionalDirectories"].is_object()
    {
        return directories;
    }
    s.emit(
        "additional_directories_ignored",
        json!({"directories":directories}),
    );
    Vec::new()
}

/// Whether the harness is running; refuses a session that cannot be reopened now.
fn reopenable(s: &Session, action: &str) -> Result<bool, (StatusCode, String)> {
    let data = s.data.lock().unwrap();
    if data.lifecycle.archived {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("This session is archived; unarchive it first. {action} is not available."),
        ));
    }
    if data.provider_history_deleted || data.provider_history_delete_pending {
        return Err(fail(
            StatusCode::CONFLICT,
            "Provider-history deletion was requested; this session cannot be reopened",
        ));
    }
    match data.status.as_str() {
        "idle" if !data.turn_active => Ok(true),
        "suspended" | "error" => Ok(false),
        _ => Err(fail(
            StatusCode::CONFLICT,
            format!("{action} between turns: wait for this turn to finish, or stop it"),
        )),
    }
}

/// An existing directory inside the allowed roots of the session's host, canonical.
async fn resolve(app: &App, s: &Session, path: &str) -> Result<PathBuf, (StatusCode, String)> {
    if s.host_id == "local" {
        return checked_cwd(app, path);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(12),
        hosts::remote_directory(app, &s.host_id, path),
    )
    .await
    .map_err(|_| {
        fail(
            StatusCode::REQUEST_TIMEOUT,
            "Remote directory validation timed out",
        )
    })?
}

async fn stop(s: &Session, running: bool) {
    if running {
        lifecycle::stop_worker(s).await;
        s.clear_approvals();
        s.status("suspended");
    }
}

/// A harness that was running reopens with the saved place; a stopped one uses it on resume.
async fn restart(app: &App, s: &Arc<Session>, running: bool) -> Result<(), (StatusCode, String)> {
    if running {
        let _ = resume_locked(app, s).await?;
    }
    Ok(())
}

/// The note the agent receives with its next prompt, after any pending fork handoff.
fn moved_note(from: &FsPath, to: &FsPath) -> String {
    format!(
        "Agentworks moved this session's working directory from {} to {}. Relative paths earlier in this conversation refer to {}; absolute paths keep their original meaning. Verify files in the new directory before editing them.",
        from.display(),
        to.display(),
        from.display()
    )
}

fn additional_note(directories: &[PathBuf]) -> String {
    if directories.is_empty() {
        return "Agentworks removed this session's additional directories. Work only in the working directory.".into();
    }
    let list = directories
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Agentworks changed this session's additional directories. Besides the working directory, you can now also work in: {list}."
    )
}

/// Saves the change, its note and any new place together; nothing changes if the write fails.
fn commit(
    s: &Session,
    note: String,
    place: Option<Place>,
    update: impl FnOnce(&mut SessionData),
) -> Result<(), (StatusCode, String)> {
    let mut data = s.data.lock().unwrap();
    let mut next = data.clone();
    update(&mut next);
    next.carried_context = Some(match next.carried_context.take() {
        Some(pending) => format!("{pending}\n\n{note}"),
        None => note,
    });
    let mut metadata = s.metadata();
    if let Some(place) = &place {
        metadata.cwd = place.cwd.clone();
        metadata.location_id = place.location_id.clone();
    }
    s.store
        .lock()
        .unwrap()
        .save_session(&metadata, &next)
        .map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not save the session's directories",
            )
        })?;
    if let Some(place) = place {
        *s.place.write().unwrap() = place;
    }
    *data = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_name_the_directories_and_how_to_read_old_paths() {
        let text = moved_note(FsPath::new("/repo/old"), FsPath::new("/repo/new"));
        assert!(text.contains("from /repo/old to /repo/new"));
        assert!(text.contains("Relative paths earlier in this conversation refer to /repo/old"));
        let text = additional_note(&["/repo/lib".into(), "/repo/docs".into()]);
        assert!(text.ends_with("also work in: /repo/lib, /repo/docs."));
        assert!(additional_note(&[]).contains("removed"));
    }
}
