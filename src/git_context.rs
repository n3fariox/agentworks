//! Read-only, bounded Git inspection. No session state or repository files are changed.
use crate::{ApiResult, App, effective_roots, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::{
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

const OUTPUT_LIMIT: u64 = 16 * 1024;
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn workspace(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let roots = effective_roots(&app);
    // Recheck current roots and symlinks without rejecting a removed saved directory.
    let cwd = tokio::fs::canonicalize(&s.cwd).await.ok();
    let path = cwd.as_deref().unwrap_or(&s.cwd);
    let root = roots
        .iter()
        .filter(|r| path.starts_with(r))
        .min_by_key(|r| r.components().count())
        .ok_or_else(|| {
            fail(
                StatusCode::FORBIDDEN,
                "Project must be inside an allowed root directory",
            )
        })?;
    let context = inspect(&s.cwd, root).await;
    Ok(Json(context))
}

fn unavailable(cwd: &FsPath, message: &str) -> Value {
    json!({"cwd":cwd,"status":"unavailable","repositoryRoot":null,"worktreeRoot":null,
        "gitDir":null,"gitCommonDir":null,"branch":null,"head":null,"commit":null,"linkedWorktree":null,"message":message})
}

async fn inspect(cwd: &FsPath, root: &FsPath) -> Value {
    let result = tokio::time::timeout(INSPECTION_TIMEOUT, inspect_inner(cwd, root)).await;
    match result {
        Ok(Ok(value)) => value,
        Ok(Err(message)) => unavailable(cwd, message),
        Err(_) => unavailable(cwd, "Git inspection timed out"),
    }
}

async fn inspect_inner(cwd: &FsPath, root: &FsPath) -> Result<Value, &'static str> {
    if !tokio::fs::metadata(cwd).await.is_ok_and(|m| m.is_dir()) {
        return Err("Working directory is missing or unavailable");
    }
    let top = git(cwd, root, &["rev-parse", "--show-toplevel"]).await?;
    if !top.0 {
        if top.2.contains("not a git repository") {
            let mut value = unavailable(cwd, "This directory is not in a Git worktree");
            value["status"] = json!("non_git");
            return Ok(value);
        }
        return Err("Git context is unavailable");
    }
    let worktree = PathBuf::from(&top.1);
    if !worktree.starts_with(root) {
        return Err("Git worktree is outside the allowed root directory");
    }
    let git_dir = required(cwd, root, &["rev-parse", "--absolute-git-dir"]).await?;
    let common = required(
        cwd,
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let branch = git(cwd, root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    if !branch.0 && branch.3 != Some(1) {
        return Err("Git HEAD is unavailable");
    }
    let commit = git(cwd, root, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    // An unborn branch is usable even before its first commit. A broken detached HEAD isn't.
    if !commit.0 {
        let head = git(cwd, root, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
        if !branch.0 || head.0 {
            return Err("Git commit is unavailable");
        }
    }
    let common_path = PathBuf::from(&common);
    let repository = if common_path.file_name().is_some_and(|n| n == ".git") {
        common_path.parent().unwrap_or(&common_path)
    } else {
        &common_path
    };
    Ok(
        json!({"cwd":cwd,"status":"git","repositoryRoot":repository,"worktreeRoot":worktree,
        "gitDir":git_dir,"gitCommonDir":common,"branch":if branch.0 {Some(branch.1)} else {None},
        "head":if branch.0 {"branch"} else {"detached"},
        "commit":if commit.0 {Some(commit.1)} else {None},"linkedWorktree":git_dir != common,
        "message":if commit.0 {None} else {Some("No commits yet")}}),
    )
}

async fn required(cwd: &FsPath, root: &FsPath, args: &[&str]) -> Result<String, &'static str> {
    let value = git(cwd, root, args).await?;
    if value.0 {
        Ok(value.1)
    } else {
        Err("Git context is unavailable")
    }
}

pub(crate) async fn git(
    cwd: &FsPath,
    root: &FsPath,
    args: &[&str],
) -> Result<(bool, String, String, Option<i32>), &'static str> {
    git_bounded(cwd, root, args, OUTPUT_LIMIT).await
}

pub(crate) async fn git_bounded(
    cwd: &FsPath,
    root: &FsPath,
    args: &[&str],
    output_limit: u64,
) -> Result<(bool, String, String, Option<i32>), &'static str> {
    let mut command = Command::new("git");
    // Remove all inherited repository/config/index/object overrides, including future GIT_* keys.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(cwd)
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "submodule.recurse=false",
        ])
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", root.parent().unwrap_or(root))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| "Git is not installed or could not start")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = async |stream: tokio::process::ChildStdout| {
        let mut bytes = Vec::new();
        stream
            .take(output_limit + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Git output could not be read")?;
        if bytes.len() as u64 > output_limit {
            return Err("Git output exceeded the inspection limit");
        }
        String::from_utf8(bytes)
            .map(|s| s.trim_end_matches(['\n', '\r']).to_owned())
            .map_err(|_| "Git output is not valid UTF-8")
    };
    let read_error = async {
        let mut bytes = Vec::new();
        stderr
            .take(output_limit + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Git output could not be read")?;
        if bytes.len() as u64 > output_limit {
            return Err("Git output exceeded the inspection limit");
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    let (out, err, status) = tokio::try_join!(read(stdout), read_error, async {
        child.wait().await.map_err(|_| "Git inspection failed")
    })?;
    Ok((status.success(), out, err, status.code()))
}
