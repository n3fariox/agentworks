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
use std::{collections::BTreeSet, sync::Arc};
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
                in_use: false,
            },
        }
    };
    result.shared_with = shared_with;
    if result.blocker.is_none() && result.state == "present" {
        result.blocker = if busy {
            Some("Session is running; stop it before removing its worktree".into())
        } else if !result.shared_with.is_empty() {
            result.in_use = true;
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
            "overridable": i.state == "present" && i.in_use,
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

/// True when the only blocker is shared use and `unused` names exactly the sessions sharing it.
pub(crate) fn unused_confirmed(i: &Inspection, unused: &[String]) -> bool {
    let shared: BTreeSet<&str> = i
        .shared_with
        .iter()
        .filter_map(|o| o["id"].as_str())
        .collect();
    i.in_use && !unused.is_empty() && shared == unused.iter().map(String::as_str).collect()
}

const SHARING_CHANGED: &str =
    "Sessions using this worktree changed since you confirmed; review them and confirm again";

/// Callers hold the run lock. Retained outcomes are successful for `Auto`; `Remove` fails instead.
/// `unused` lists the sharing sessions the human confirmed no longer need the checkout.
pub async fn cleanup(
    app: &App,
    s: &Session,
    mode: Cleanup,
    confirm: Option<&str>,
    unused: &[String],
) -> Result<Value, Error> {
    let first = inspect(app, s).await;
    let unused = if mode == Cleanup::Remove { unused } else { &[] };
    let blocker = |i: &Inspection| i.blocker.clone().filter(|_| !unused_confirmed(i, unused));
    // Consent covers only the sessions shown; any change in that set needs a fresh review.
    if !unused.is_empty() && first.in_use && !unused_confirmed(&first, unused) {
        return Err(conflict(SHARING_CHANGED));
    }
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
    crate::git_settings::exit(app, s, "archive", unused).await?;
    if mode == Cleanup::Keep {
        return Ok(json!({"outcome":"retained","reason":"Kept by request","worktree":summary}));
    }
    if let Some(reason) = blocker(&first) {
        return retained(&reason);
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
    if !unused.is_empty() && second.in_use && !unused_confirmed(&second, unused) {
        return retained(SHARING_CHANGED);
    }
    if blocker(&second).is_some() || second.fingerprint != first.fingerprint {
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
    /// IDs of the sessions sharing the checkout that the human confirmed are unused.
    #[serde(default)]
    confirm_unused: Vec<String>,
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
    remove_locked(&app, &s, req.confirm_dirty.as_deref(), &req.confirm_unused).await
}
async fn remove_locked(
    app: &App,
    s: &Arc<Session>,
    confirm: Option<&str>,
    unused: &[String],
) -> ApiResult {
    let outcome = cleanup(app, s, Cleanup::Remove, confirm, unused).await?;
    persist(s)?;
    s.emit("session_updated", s.meta());
    Ok(Json(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command, sync::Mutex};

    struct Fixture {
        app: App,
        root: PathBuf,
        worktree: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("worktree-cleanup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let repo = root.join("repo");
            std::fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q", "-b", "main"]);
            git(
                &repo,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "base",
                ],
            );
            let worktree = root.join("wt");
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "owned",
                    worktree.to_str().unwrap(),
                ],
            );
            let store = Arc::new(Mutex::new(
                crate::store::Store::open(&root.join("db.sqlite3")).unwrap(),
            ));
            let catalog = Arc::new(Mutex::new(
                crate::Catalog::open(root.join("catalog.json")).unwrap(),
            ));
            let app = App {
                sessions: Default::default(),
                roots: Arc::new(vec![root.clone()]),
                port: 4506,
                bind: "127.0.0.1".parse().unwrap(),
                catalog: catalog.clone(),
                discovery: Arc::new(tokio::sync::Semaphore::new(1)),
                store,
                run_lock: Default::default(),
                workspace_lock: Default::default(),
                communication_lock: Default::default(),
                mcp_endpoint: "127.0.0.1:4506".parse().unwrap(),
                restart: Arc::new(crate::service_restart::Manager::new()),
                notifications: crate::notifications::Hub::new(catalog),
                hosts: crate::hosts::Hosts::new(),
                host_usage: Arc::new(crate::host_usage::Hub::default()),
                account_quota: Arc::default(),
                github: Arc::new(crate::github::Hub::default()),
                web_auth: Default::default(),
                progress: crate::tool_progress::Hub::empty(),
            };
            let f = Self {
                app,
                root,
                worktree,
            };
            let owned = Owned {
                path: f.worktree.clone(),
                branch: "owned".into(),
                common_dir: PathBuf::from(git(
                    &repo,
                    &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                )),
                main_top: repo.clone(),
                base_commit: git(&repo, &["rev-parse", "HEAD"]),
                removed: None,
                hooks: None,
                population: None,
                settings: None,
            };
            f.session("owner", Some(owned));
            f.session("early", None);
            f
        }
        /// Adds a suspended local session whose cwd is inside the worktree.
        fn session(&self, id: &str, owned: Option<Owned>) -> Arc<Session> {
            let mut d: crate::SessionData = serde_json::from_value(json!({"status":"suspended","seq":0,"requests":[],"models":null,"available_commands":[],"turn_active":false})).unwrap();
            d.worktree = owned;
            let meta = crate::store::Metadata {
                id: id.into(),
                provider: "demo".into(),
                cwd: self.worktree.clone(),
                title: id.into(),
                initial_model: None,
                created: now(),
                host_id: "local".into(),
                location_id: "fixture".into(),
            };
            self.app
                .store
                .lock()
                .unwrap()
                .insert(&meta, &d, None, None)
                .unwrap();
            let s = Arc::new(Session {
                id: id.into(),
                provider: "demo".into(),
                cwd: self.worktree.clone(),
                title: id.into(),
                initial_model: None,
                created: now(),
                host_id: "local".into(),
                location_id: "fixture".into(),
                data: Mutex::new(d),
                tx: tokio::sync::broadcast::channel(16).0,
                commands: Mutex::new(None),
                store: self.app.store.clone(),
                approvals: Default::default(),
                worker: Default::default(),
                mcp_endpoint: self.app.mcp_endpoint,
                mcp_token: Mutex::new("fixture-only".into()),
                mcp_refresh_pending: Default::default(),
                fresh_environment: Default::default(),
                notifications: self.app.notifications.clone(),
                progress: self.app.progress.clone(),
                account_quota: self.app.account_quota.clone(),
            });
            self.app
                .sessions
                .lock()
                .unwrap()
                .insert(id.into(), s.clone());
            s
        }
        fn owner(&self) -> Arc<Session> {
            self.app.sessions.lock().unwrap()["owner"].clone()
        }
    }

    #[tokio::test]
    async fn unused_confirmation_is_bound_to_the_sessions_shown() {
        let f = Fixture::new();
        let owner = f.owner();
        let shown = inspect(&f.app, &owner).await;
        assert!(shown.in_use);
        let confirmed: Vec<String> = shown
            .shared_with
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(confirmed, ["early"]);

        // A session starts in the checkout after the human confirmed but before removal.
        f.session("late", None);
        let err = cleanup(&f.app, &owner, Cleanup::Remove, None, &confirmed)
            .await
            .unwrap_err();
        assert_eq!(
            (err.0, err.1.as_str()),
            (StatusCode::CONFLICT, SHARING_CHANGED)
        );
        assert!(f.worktree.is_dir());
        assert!(
            owner
                .data
                .lock()
                .unwrap()
                .worktree
                .as_ref()
                .unwrap()
                .removed
                .is_none()
        );

        // Without consent, or under Auto, the shared checkout is still retained.
        let err = cleanup(&f.app, &owner, Cleanup::Remove, None, &[])
            .await
            .unwrap_err();
        assert!(err.1.contains("Another session uses this worktree"));
        let kept = cleanup(&f.app, &owner, Cleanup::Auto, None, &confirmed)
            .await
            .unwrap();
        assert_eq!(kept["outcome"], "retained");
        assert!(f.worktree.is_dir());

        // Confirming exactly the current set removes it; the branch is retained.
        f.app.sessions.lock().unwrap().remove("late");
        let removed = cleanup(&f.app, &owner, Cleanup::Remove, None, &confirmed)
            .await
            .unwrap();
        assert_eq!(removed["outcome"], "removed");
        assert!(!f.worktree.exists());
        git(
            &f.root.join("repo"),
            &["rev-parse", "--verify", "refs/heads/owned"],
        );
    }

    #[tokio::test]
    async fn unused_confirmation_never_overrides_integrity_blockers() {
        let f = Fixture::new();
        let owner = f.owner();
        git(&f.worktree, &["switch", "-q", "-c", "elsewhere"]);
        let shown = inspect(&f.app, &owner).await;
        assert!(
            !shown.in_use
                && shown
                    .blocker
                    .as_deref()
                    .unwrap()
                    .contains("differs from the recorded branch")
        );
        let err = cleanup(&f.app, &owner, Cleanup::Remove, None, &["early".into()])
            .await
            .unwrap_err();
        assert!(err.1.contains("differs from the recorded branch"));
        assert!(f.worktree.is_dir());
    }
}
