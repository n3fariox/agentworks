//! Read-only per-session comparisons. All user refs become verified commit IDs before diffing.
use crate::{ApiResult, App, effective_roots, fail, git_context, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path as FsPath, time::Duration};

const LIST_LIMIT: u64 = 128 * 1024;
const PATCH_LIMIT: u64 = 256 * 1024;

#[derive(Deserialize, Default)]
pub struct Request {
    #[serde(default)]
    mode: String,
    base: Option<String>,
    commit: Option<String>,
    file: Option<String>,
}

pub async fn view(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(req): Query<Request>,
) -> ApiResult {
    match tokio::time::timeout(Duration::from_secs(10), view_inner(app, id, req)).await {
        Ok(result) => result,
        Err(_) => Err(fail(
            StatusCode::REQUEST_TIMEOUT,
            "Git diff inspection timed out",
        )),
    }
}

async fn view_inner(app: App, id: String, req: Request) -> ApiResult {
    let s = session(&app, &id)?;
    // Reuse the workspace endpoint's live root/symlink and worktree boundary checks.
    let Json(context) = git_context::workspace(State(app.clone()), Path(id)).await?;
    if context["status"] != "git" {
        return Ok(Json(
            json!({"context":context,"unavailable":"Diff requires an available Git worktree"}),
        ));
    }
    let cwd = tokio::fs::canonicalize(&s.cwd)
        .await
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "Session directory is unavailable"))?;
    let roots = effective_roots(&app);
    let root = roots
        .iter()
        .filter(|r| cwd.starts_with(r))
        .min_by_key(|r| r.components().count())
        .ok_or_else(|| {
            fail(
                StatusCode::FORBIDDEN,
                "Session directory is outside allowed roots",
            )
        })?;
    let worktree = tokio::fs::canonicalize(context["worktreeRoot"].as_str().unwrap())
        .await
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "Worktree is unavailable"))?;
    if !worktree.starts_with(root) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Git worktree is outside allowed roots",
        ));
    }
    // Run from the worktree root so nested sessions see the entire worktree.
    let mut value = compare(&worktree, root, &req)
        .await
        .map_err(|message| fail(StatusCode::BAD_REQUEST, message))?;
    value["context"] = context;
    Ok(Json(value))
}

async fn run(cwd: &FsPath, root: &FsPath, args: &[&str], limit: u64) -> Result<String, String> {
    let result = git_context::git_bounded_raw(cwd, root, args, limit)
        .await
        .map_err(str::to_owned)?;
    if result.0 {
        Ok(result.1)
    } else {
        Err("Git comparison unavailable; check the revision and repository".into())
    }
}
fn valid_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('-')
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}
async fn resolve(cwd: &FsPath, root: &FsPath, value: &str) -> Result<String, String> {
    if !valid_ref(value) {
        return Err(
            "Revision must be 1–256 characters, without whitespace or a leading option".into(),
        );
    }
    run(
        cwd,
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{value}^{{commit}}"),
        ],
        1024,
    )
    .await
    .map(|value| value.trim_end().to_owned())
}
fn paths(value: &str) -> Vec<&str> {
    value.split('\0').filter(|p| !p.is_empty()).collect()
}
async fn auxiliary(cwd: &FsPath, root: &FsPath, args: &[&str]) -> Value {
    match run(cwd, root, args, LIST_LIMIT).await {
        Ok(value) => {
            let files = paths(&value);
            json!({"files":files.iter().take(2000).collect::<Vec<_>>(), "unavailable":if files.len()>2000 { Some("Only the first 2000 paths are shown") } else { None }})
        }
        Err(message) => json!({"files":[],"unavailable":message}),
    }
}
async fn compare(cwd: &FsPath, root: &FsPath, req: &Request) -> Result<Value, String> {
    let mode = if req.mode.is_empty() {
        "unstaged"
    } else {
        &req.mode
    };
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--ignore-submodules=all",
    ];
    let mut from = None;
    let mut to = None;
    let mut merge_base = None;
    let mut resolved_base = None;
    let mut root_commit = false;
    match mode {
        "unstaged" => {}
        "branch" => {
            let base = resolve(cwd, root, req.base.as_deref().unwrap_or("main")).await?;
            let head = resolve(cwd, root, "HEAD").await?;
            let common = run(cwd, root, &["merge-base", &base, &head], 1024)
                .await?
                .trim_end()
                .to_owned();
            resolved_base = Some(base);
            merge_base = Some(common.clone());
            from = Some(common);
            to = Some(head);
        }
        "commit" => {
            let commit = resolve(cwd, root, req.commit.as_deref().unwrap_or("HEAD")).await?;
            // A merge commit is compared to its first parent; an initial commit to the empty tree.
            let parents = run(
                cwd,
                root,
                &["rev-list", "--parents", "--max-count=1", &commit, "--"],
                LIST_LIMIT,
            )
            .await?;
            from = parents.split_whitespace().nth(1).map(str::to_owned);
            root_commit = from.is_none();
            to = Some(commit);
        }
        _ => return Err("Unknown diff mode".into()),
    }
    if root_commit {
        args = vec![
            "diff-tree",
            "--root",
            "--no-commit-id",
            "-r",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--ignore-submodules=all",
        ];
    }
    if let Some(value) = &from {
        args.push(value);
    }
    if let Some(value) = &to {
        args.push(value);
    }
    let mut list_args = args.clone();
    list_args.extend(["--name-only", "-z", "--"]);
    let list = run(cwd, root, &list_args, LIST_LIMIT).await?;
    let files = paths(&list);
    if files.len() > 2000 {
        return Err("Comparison exceeds the 2000-file inspection limit".into());
    }
    let mut patch = None;
    let mut patch_unavailable = None;
    if let Some(file) = &req.file {
        if !files.contains(&file.as_str()) {
            return Err("Selected file is not in this comparison; refresh the file list".into());
        }
        let mut patch_args = args.clone();
        patch_args.extend(["--patch", "--unified=3", "--", file]);
        // Literal pathspec prevents special file names from widening a selected file request.
        patch_args.insert(0, "--literal-pathspecs");
        match run(cwd, root, &patch_args, PATCH_LIMIT).await {
            Ok(value) => patch = Some(value),
            Err(message) => patch_unavailable = Some(message),
        }
    }
    let staged = auxiliary(
        cwd,
        root,
        &[
            "diff",
            "--cached",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-only",
            "-z",
            "--",
        ],
    )
    .await;
    let untracked = auxiliary(
        cwd,
        root,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    )
    .await;
    let refs = run(
        cwd,
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
        LIST_LIMIT,
    )
    .await;
    Ok(
        json!({"mode":mode,"base":req.base,"from":from,"to":to,"mergeBase":merge_base,"resolvedBase":resolved_base,
        "rootCommit":root_commit,"files":files,"file":req.file,"patch":patch,"patchUnavailable":patch_unavailable,
        "patchLimitBytes":PATCH_LIMIT,"staged":staged,"untracked":untracked,
        "bases":refs.unwrap_or_default().lines().take(500).collect::<Vec<_>>(),
        "handling":"Unstaged compares the index to the worktree. Staged and untracked files are listed separately and excluded. Branch and commit views compare committed trees. Renames appear as deletion/addition. Submodule contents are excluded."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refs_reject_options_and_controls() {
        for value in ["", "--output=/tmp/file", "main\nHEAD", "a b", "\0"] {
            assert!(!valid_ref(value));
        }
        for value in ["HEAD", "origin/main", "HEAD~2", "feature/diff"] {
            assert!(valid_ref(value));
        }
    }
}
