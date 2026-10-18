//! Workspace preparation is shared by every new-session entry point. No cleanup is implicit.
use crate::{ApiResult, App, Create, checked_cwd, effective_roots, fail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Component, Path as FsPath, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    Directory,
    NewWorktree,
    ExistingWorktree,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub default_mode: Mode,
    pub require_worktree: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Workspace {
    pub mode: Mode,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub path: Option<String>,
}
type Error = (StatusCode, String);
fn bad(message: impl Into<String>) -> Error {
    fail(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> Error {
    fail(StatusCode::CONFLICT, message)
}

pub async fn get_policy(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let catalog = app.catalog.lock().unwrap();
    let project = catalog
        .data
        .projects
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Project not found"))?;
    Ok(Json(json!(project.workspace_policy)))
}
pub async fn put_policy(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(policy): Json<Policy>,
) -> ApiResult {
    if policy.require_worktree && policy.default_mode == Mode::Directory {
        return Err(bad(
            "A requiring project must default to newWorktree or existingWorktree",
        ));
    }
    let _workspace = app.workspace_lock.lock().await;
    app.catalog
        .lock()
        .unwrap()
        .workspace_policy(&id, policy.clone())
        .map_err(bad)?;
    Ok(Json(json!(policy)))
}

// Distinct namespaces prevent direct clients from colliding with MCP/automation requests.
pub fn creation_key(req: &Create) -> Result<Option<String>, Error> {
    if let Some(o) = &req.control_origin {
        return Ok(Some(format!("mcp:{}:{}", o.actor, o.request)));
    }
    if let Some(o) = &req.automation {
        return Ok(Some(format!(
            "automation:{}:{}",
            o.automation_id, o.request_id
        )));
    }
    req.request_id
        .as_ref()
        .map(|id| {
            if id.is_empty() || id.len() > 100 || id.chars().any(char::is_control) {
                return Err(bad(
                    "Supply a requestId between 1 and 100 bytes without control characters",
                ));
            }
            Ok(format!("api:{id}"))
        })
        .transpose()
}
pub fn creation_payload(req: &Create) -> String {
    if let Some(o) = &req.control_origin {
        return o.payload.to_string();
    }
    // Generated run IDs and timestamps are deliberately excluded from automation fingerprints.
    json!({"create":req,"automation":req.automation.as_ref().map(|a| (&a.automation_id,&a.request_id)),"prompt":req.queued_prompt}).to_string()
}

fn allowed(app: &App, path: &FsPath) -> Result<PathBuf, Error> {
    effective_roots(app)
        .into_iter()
        .filter(|r| path.starts_with(r))
        .min_by_key(|r| r.components().count())
        .ok_or_else(|| {
            fail(
                StatusCode::FORBIDDEN,
                "Git workspace must be inside an allowed root directory",
            )
        })
}
async fn git(app: &App, cwd: &FsPath, args: &[&str]) -> Result<String, Error> {
    let root = allowed(app, cwd)?;
    let (ok, out, err, _) = tokio::time::timeout(
        Duration::from_secs(10),
        crate::git_context::git_bounded(cwd, &root, args, 256 * 1024),
    )
    .await
    .map_err(|_| {
        conflict("Git operation timed out; inspect retained branch and files before retrying")
    })?
    .map_err(conflict)?;
    if !ok {
        return Err(bad(format!(
            "Git operation failed: {}",
            crate::runtime::safe_error(&err)
                .chars()
                .take(2000)
                .collect::<String>()
        )));
    }
    Ok(out)
}
struct Repository {
    top: PathBuf,
    linked: bool,
}
async fn repository(app: &App, cwd: &FsPath) -> Result<Repository, Error> {
    let top = git(app, cwd, &["rev-parse", "--show-toplevel"]).await?;
    let top = checked_cwd(app, &top)?;
    let dir = git(app, cwd, &["rev-parse", "--absolute-git-dir"]).await?;
    let common = git(
        app,
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let dir = std::fs::canonicalize(dir).map_err(|_| bad("Git directory is unavailable"))?;
    let common =
        std::fs::canonicalize(common).map_err(|_| bad("Git common directory is unavailable"))?;
    allowed(app, &dir)?;
    allowed(app, &common)?;
    Ok(Repository {
        top,
        linked: dir != common,
    })
}

#[derive(Deserialize)]
pub struct List {
    cwd: String,
}
pub async fn list(State(app): State<App>, Json(req): Json<List>) -> ApiResult {
    let cwd = checked_cwd(&app, &req.cwd)?;
    repository(&app, &cwd).await?;
    let output = git(&app, &cwd, &["worktree", "list", "--porcelain", "-z"]).await?;
    let mut worktrees = Vec::new();
    for (index, record) in output.split("\0\0").filter(|r| !r.is_empty()).enumerate() {
        let mut path = None;
        let mut branch = None;
        let mut commit = None;
        let mut locked = false;
        let mut prunable = false;
        for field in record.split('\0') {
            if let Some(v) = field.strip_prefix("worktree ") {
                path = Some(v);
            }
            if let Some(v) = field.strip_prefix("branch ") {
                branch = Some(v.strip_prefix("refs/heads/").unwrap_or(v));
            }
            if let Some(v) = field.strip_prefix("HEAD ") {
                commit = Some(v);
            }
            locked |= field == "locked" || field.starts_with("locked ");
            prunable |= field == "prunable" || field.starts_with("prunable ");
        }
        if let Some(path) = path {
            // Missing/prunable paths cannot be safely canonicalized and are omitted.
            if let Ok(path) = checked_cwd(&app, path) {
                worktrees.push(json!({"path":path,"branch":branch,"commit":commit,"isMain":index==0,"locked":locked,"prunable":prunable}));
            }
        }
    }
    Ok(Json(json!({"worktrees":worktrees})))
}

// Resolve an absent destination through existing ancestors. Reject traversal and symlinks;
// never reuse even an empty existing destination. OS-user filesystem races remain possible.
fn destination(app: &App, path: &FsPath) -> Result<PathBuf, Error> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(bad(
            "Worktree destination must be an absolute path without traversal",
        ));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(fail(
                    StatusCode::FORBIDDEN,
                    "Worktree destination must not traverse symlinks",
                ));
            }
            Ok(_) if current == path => {
                return Err(conflict(
                    "Worktree destination already exists; choose a new path",
                ));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(bad("Worktree destination parent is not a directory"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(bad("Worktree destination is unavailable")),
        }
    }
    allowed(app, path)?;
    Ok(path.to_owned())
}

// A supplied cwd is not evidence of an external history's original location. Verify
// discovery metadata before loading it; never move a provider conversation to satisfy policy.
pub async fn verify_import(app: &App, req: &Create, source: &FsPath) -> Result<(), Error> {
    let Some(id) = req.external_id.as_deref() else {
        return Ok(());
    };
    let _permit = app
        .discovery
        .clone()
        .try_acquire_owned()
        .map_err(|_| conflict("Harness discovery is busy; retry import shortly"))?;
    let discover = async {
        let mut cursor = None;
        for _ in 0..100 {
            let page = crate::runtime::list_sessions(
                &req.provider,
                Some(source.to_owned()),
                cursor.clone(),
                req.external_host,
            )
            .await
            .map_err(|e| {
                bad(format!(
                    "Cannot verify original import cwd: {}",
                    crate::runtime::safe_error(&e.to_string())
                ))
            })?;
            for entry in page["sessions"].as_array().into_iter().flatten() {
                if entry["sessionId"].as_str() == Some(id) {
                    let actual = entry["cwd"]
                        .as_str()
                        .ok_or_else(|| bad("Provider did not report this history's cwd"))?;
                    if checked_cwd(app, actual)? != source {
                        return Err(bad("Imported history must use its original provider cwd"));
                    }
                    return Ok(());
                }
            }
            let next = page["nextCursor"].as_str().map(str::to_owned);
            if next.is_none() || next == cursor {
                break;
            }
            cursor = next;
        }
        Err(bad(
            "Cannot verify original provider cwd: history was not found in bounded session discovery",
        ))
    };
    tokio::time::timeout(Duration::from_secs(12), discover)
        .await
        .map_err(|_| conflict("Import cwd verification timed out"))?
}

pub fn arm_creation(app: &App, key: Option<&str>) -> Result<(), Error> {
    if let Some(key) = key {
        app.store.lock().unwrap().arm_creation(key).map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not durably reserve creation before mutation",
            )
        })?;
    }
    Ok(())
}

pub async fn prepare(
    app: &App,
    req: &Create,
    source: PathBuf,
    creation_key: Option<&str>,
) -> Result<PathBuf, Error> {
    let policy = {
        let catalog = app.catalog.lock().unwrap();
        match req.project_id.as_deref() {
            Some(id) => catalog
                .data
                .projects
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| bad("Project not found"))?
                .workspace_policy
                .clone(),
            None => Policy::default(),
        }
    };
    // Histories are loaded in their actual supplied provider cwd, never in a new worktree.
    let mode = if req.external_id.is_some() {
        Mode::Directory
    } else {
        req.workspace
            .as_ref()
            .map(|w| w.mode)
            .unwrap_or(policy.default_mode)
    };
    let workspace = req.workspace.as_ref();
    if mode != Mode::NewWorktree
        && workspace.is_some_and(|w| w.branch.is_some() || w.base_ref.is_some())
    {
        return Err(bad("branch and baseRef only apply to newWorktree"));
    }
    if mode == Mode::Directory && workspace.is_some_and(|w| w.path.is_some()) {
        return Err(bad("path only applies to worktree modes"));
    }
    let cwd = match mode {
        Mode::Directory => source,
        Mode::ExistingWorktree => {
            let repo = repository(app, &source).await?;
            if !repo.linked {
                return Err(bad(
                    "Choose an existing linked Git worktree; the main checkout is not isolated",
                ));
            }
            if let Some(path) = workspace.and_then(|w| w.path.as_deref()) {
                let selected = checked_cwd(app, path)?;
                if !source.starts_with(&selected)
                    || repository(app, &selected).await?.top != repo.top
                {
                    return Err(bad(
                        "Existing worktree path must contain cwd in the same worktree",
                    ));
                }
            }
            source
        }
        Mode::NewWorktree => {
            let repo = repository(app, &source).await?;
            let unique = format!("session-{}", uuid::Uuid::new_v4());
            let branch = workspace
                .and_then(|w| w.branch.clone())
                .unwrap_or_else(|| format!("agentworks/{unique}"));
            if branch.is_empty()
                || branch.len() > 256
                || branch.starts_with('-')
                || branch.chars().any(char::is_control)
            {
                return Err(bad("Invalid worktree branch name"));
            }
            // check-ref-format (without --branch) never expands @{-1} into an existing branch.
            git(
                app,
                &repo.top,
                &["check-ref-format", &format!("refs/heads/{branch}")],
            )
            .await?;
            let base = workspace
                .and_then(|w| w.base_ref.as_deref())
                .unwrap_or("HEAD");
            if base.is_empty()
                || base.len() > 256
                || base.starts_with('-')
                || base.chars().any(char::is_control)
            {
                return Err(bad("Invalid worktree base ref"));
            }
            let commit = git(
                app,
                &repo.top,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{base}^{{commit}}"),
                ],
            )
            .await?;
            let path = workspace
                .and_then(|w| w.path.as_ref())
                .map(PathBuf::from)
                .unwrap_or_else(|| repo.top.join(".worktrees").join(&unique));
            let path = destination(app, &path)?;
            let parent = path
                .parent()
                .ok_or_else(|| bad("Invalid worktree destination"))?;
            arm_creation(app, creation_key)?;
            std::fs::create_dir_all(parent)
                .map_err(|_| bad("Could not create worktree parent directory"))?;
            // Recheck ancestors immediately before Git mutation, and canonical roots after it.
            destination(app, &path)?;
            let path_str = path
                .to_str()
                .ok_or_else(|| bad("Worktree path must be UTF-8"))?;
            // Exclusively reserve an absent destination; Git may fill only this new empty
            // directory, never an empty directory created earlier by somebody else.
            std::fs::create_dir(&path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    conflict("Worktree destination already exists; choose a new path")
                } else {
                    bad("Could not reserve worktree destination directory")
                }
            })?;
            checked_cwd(app, path_str)?;
            git(app, &repo.top, &["worktree", "add", "--no-track", "-b", &branch, "--", path_str, &commit]).await
                .map_err(|(code, message)| (code, format!("{message}. Destination {} and any branch/files are retained; no cleanup was attempted", path.display())))?;
            checked_cwd(app, path_str)?
        }
    };
    if policy.require_worktree && !repository(app, &cwd).await?.linked {
        return Err(bad(
            "Project policy requires the final cwd to be a linked Git worktree",
        ));
    }
    // Includes directory mode and imports: all launches recheck effective roots at execution.
    checked_cwd(
        app,
        cwd.to_str()
            .ok_or_else(|| bad("Working directory must be UTF-8"))?,
    )
}
