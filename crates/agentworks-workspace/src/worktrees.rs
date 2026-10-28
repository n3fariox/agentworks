//! Filesystem policy, Git worktree discovery, and preparation.
use crate::{Error, cleanup::Owned, fail};
use http::StatusCode;
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
fn bad(message: impl Into<String>) -> Error {
    fail(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> Error {
    fail(StatusCode::CONFLICT, message)
}

/// Filesystem policy shared by the server and the remote worker.
pub trait Context: Sync {
    fn git_settings(&self) -> crate::config::Settings {
        crate::config::Settings::default()
    }
    fn roots(&self) -> Vec<PathBuf>;
    fn checked(&self, path: &str) -> Result<PathBuf, Error>;
    /// Reserve a creation durably before any filesystem mutation.
    fn arm(&self, key: Option<&str>) -> Result<(), Error>;
    /// Remove host-specific secrets before reporting Git diagnostics.
    fn safe_error(&self, message: &str) -> String;
}
pub fn allowed(app: &(impl Context + ?Sized), path: &FsPath) -> Result<PathBuf, Error> {
    app.roots()
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
pub(crate) async fn git(
    app: &(impl Context + ?Sized),
    cwd: &FsPath,
    args: &[&str],
) -> Result<String, Error> {
    git_limit(app, cwd, args, 256 * 1024).await
}
pub(crate) async fn git_limit(
    app: &(impl Context + ?Sized),
    cwd: &FsPath,
    args: &[&str],
    limit: u64,
) -> Result<String, Error> {
    let root = allowed(app, cwd)?;
    let (ok, out, err, _) = tokio::time::timeout(
        Duration::from_secs(10),
        crate::git::git_bounded(cwd, &root, args, limit),
    )
    .await
    .map_err(|_| {
        conflict("Git operation timed out; inspect retained branch and files before retrying")
    })?
    .map_err(conflict)?;
    if !ok {
        return Err(bad(format!(
            "Git operation failed: {}",
            app.safe_error(&err).chars().take(2000).collect::<String>()
        )));
    }
    Ok(out)
}
pub(crate) struct Repository {
    pub top: PathBuf,
    /// Main checkout root; new worktrees live beside each other under it even
    /// when created from a linked worktree.
    pub main_top: PathBuf,
    pub common: PathBuf,
    pub linked: bool,
}
pub(crate) async fn repository(
    app: &(impl Context + ?Sized),
    cwd: &FsPath,
) -> Result<Repository, Error> {
    let top = git(app, cwd, &["rev-parse", "--show-toplevel"]).await?;
    let top = app.checked(&top)?;
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
    let main_top = common
        .file_name()
        .filter(|name| *name == ".git")
        .and(common.parent())
        .filter(|parent| allowed(app, parent).is_ok())
        .map_or_else(|| top.clone(), FsPath::to_path_buf);
    Ok(Repository {
        top,
        main_top,
        linked: dir != common,
        common,
    })
}

pub async fn list_in(
    app: &(impl Context + ?Sized),
    cwd: &FsPath,
) -> Result<serde_json::Value, Error> {
    let cache_root = repository(app, cwd).await?.common.join("agentworks-cow");
    let output = git(app, cwd, &["worktree", "list", "--porcelain", "-z"]).await?;
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
            if FsPath::new(path).starts_with(&cache_root) {
                continue;
            }
            // Missing/prunable paths cannot be safely canonicalized and are omitted.
            if let Ok(path) = app.checked(path) {
                worktrees.push(json!({"path":path,"branch":branch,"commit":commit,"isMain":index==0,"locked":locked,"prunable":prunable}));
            }
        }
    }
    Ok(json!({"worktrees":worktrees}))
}

// Resolve an absent destination through existing ancestors. Reject traversal and symlinks;
// never reuse even an empty existing destination. OS-user filesystem races remain possible.
pub(crate) fn destination(app: &(impl Context + ?Sized), path: &FsPath) -> Result<PathBuf, Error> {
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

pub async fn prepare_in(
    app: &(impl Context + ?Sized),
    workspace: Option<&Workspace>,
    policy: Policy,
    external: bool,
    source: PathBuf,
    creation_key: Option<&str>,
) -> Result<(PathBuf, Option<Owned>), Error> {
    // Histories are loaded in their actual supplied provider cwd, never in a new worktree.
    let mode = if external {
        Mode::Directory
    } else {
        workspace.map(|w| w.mode).unwrap_or(policy.default_mode)
    };
    if mode != Mode::NewWorktree
        && workspace.is_some_and(|w| w.branch.is_some() || w.base_ref.is_some())
    {
        return Err(bad("branch and baseRef only apply to newWorktree"));
    }
    if mode == Mode::Directory && workspace.is_some_and(|w| w.path.is_some()) {
        return Err(bad("path only applies to worktree modes"));
    }
    let mut owned = None;
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
                let selected = app.checked(path)?;
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
            let config = crate::config::parse(&crate::config::read(app, &repo.main_top)?)?;
            let settings = app.git_settings().resolve(&config);
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
                .unwrap_or_else(|| repo.main_top.join(".worktrees").join(&unique));
            let path = destination(app, &path)?;
            let parent = path
                .parent()
                .ok_or_else(|| bad("Invalid worktree destination"))?;
            app.arm(creation_key)?;
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
            app.checked(path_str)?;
            let population = populate(app, &repo, &path, &commit, &branch, true, &settings).await
                .map_err(|(code, message)| (code, format!("{message}. Destination {} and any branch/files are retained; no cleanup was attempted", path.display())))?;
            let cwd = app.checked(path_str)?;
            // Only checkouts created here are owned; selected/directory worktrees never are.
            owned = Some(Owned {
                path: cwd.clone(),
                branch,
                common_dir: repo.common.clone(),
                main_top: repo.main_top.clone(),
                base_commit: commit,
                removed: None,
                hooks: (settings.run_hooks
                    && (!config.hooks.enter.is_empty() || !config.hooks.exit.is_empty()))
                .then_some(crate::hooks::State {
                    definitions: config.hooks,
                    ..Default::default()
                }),
                population: Some(population),
                settings: Some(settings),
            });
            cwd
        }
    };
    if policy.require_worktree && !repository(app, &cwd).await?.linked {
        return Err(bad(
            "Project policy requires the final cwd to be a linked Git worktree",
        ));
    }
    // Includes directory mode and imports: all launches recheck effective roots at execution.
    let cwd = app.checked(
        cwd.to_str()
            .ok_or_else(|| bad("Working directory must be UTF-8"))?,
    )?;
    Ok((cwd, owned))
}

pub(crate) async fn populate(
    app: &(impl Context + ?Sized),
    repo: &Repository,
    path: &FsPath,
    commit: &str,
    branch: &str,
    new: bool,
    settings: &crate::config::Settings,
) -> Result<String, Error> {
    match crate::cow::populate(app, repo, path, commit, branch, new, settings).await? {
        None => Ok("cow".into()),
        Some(reason) => {
            let path = path
                .to_str()
                .ok_or_else(|| bad("Worktree path must be UTF-8"))?;
            let mut args = vec!["worktree", "add"];
            if new {
                args.extend(["--no-track", "-b", branch]);
            }
            args.extend(["--", path, if new { commit } else { branch }]);
            git(app, &repo.top, &args).await?;
            Ok(reason)
        }
    }
}

pub async fn configuration_in(
    app: &(impl Context + ?Sized),
    cwd: &FsPath,
    text: Option<&str>,
    fingerprint: Option<&str>,
) -> Result<serde_json::Value, Error> {
    let repo = repository(app, cwd).await?;
    if let Some(text) = text {
        crate::config::write(
            app,
            &repo.main_top,
            text,
            fingerprint.ok_or_else(|| bad("Supply the configuration fingerprint before saving"))?,
        )?;
    }
    let text = crate::config::read(app, &repo.main_top)?;
    let config = crate::config::parse(&text)?;
    Ok(
        json!({"repository":repo.main_top,"path":repo.main_top.join(crate::config::CONFIG_PATH),"fingerprint":crate::sha256::hex(text.as_bytes()),"text":text,"git":config,"effective":app.git_settings().resolve(&config)}),
    )
}
