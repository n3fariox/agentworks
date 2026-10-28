//! Reflink population from a clean, commit-keyed linked checkout.
//! Git owns registration/index/branches; only regular-file data is cloned.
use crate::{
    Error,
    config::{CopyMode, Settings},
    fail,
    worktrees::{self, Context, Repository},
};
use http::StatusCode;
use std::{
    fs,
    path::{Component, Path},
};

fn error(message: impl Into<String>) -> Error {
    fail(StatusCode::CONFLICT, message)
}
fn clone_file(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
        let source = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)?;
        let target = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        if unsafe {
            libc::ioctl(
                target.as_raw_fd(),
                0x40049409 as libc::c_ulong,
                source.as_raw_fd(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        target.set_permissions(source.metadata()?.permissions())?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, target);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Reflink backend requires Linux",
        ))
    }
}

fn checked_directory(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(error(
            "CoW cache must not traverse symlinks or non-directories",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| error("Could not create CoW cache directory"))
        }
        Err(_) => Err(error("CoW cache is unavailable")),
    }
}
fn unsupported(settings: &Settings, reason: &str) -> Result<Option<String>, Error> {
    if settings.copy == CopyMode::RequireCow {
        Err(error(format!("CoW required: {reason}")))
    } else {
        Ok(Some(format!("plain: {reason}")))
    }
}

/// Returns a plain-checkout reason before destination mutation, or None after CoW population.
pub async fn populate(
    app: &(impl Context + ?Sized),
    repo: &Repository,
    path: &Path,
    commit: &str,
    branch: &str,
    new: bool,
    settings: &Settings,
) -> Result<Option<String>, Error> {
    if settings.copy == CopyMode::Plain {
        return Ok(Some("plain".into()));
    }
    // Avoid checkout formats whose per-worktree metadata requires additional handling.
    for key in [
        "core.sparseCheckout",
        "core.splitIndex",
        "extensions.worktreeConfig",
    ] {
        let root = worktrees::allowed(app, &repo.top)?;
        let (_, out, _, _) =
            crate::git::git_bounded(&repo.top, &root, &["config", "--bool", "--get", key], 4096)
                .await
                .map_err(error)?;
        if out == "true" {
            return unsupported(
                settings,
                "repository uses sparse, split-index, or worktree-specific configuration",
            );
        }
    }
    let tree =
        worktrees::git_limit(app, &repo.top, &["ls-tree", "-r", commit], 16 * 1024 * 1024).await?;
    if tree.lines().any(|l| l.starts_with("160000 ")) {
        return unsupported(settings, "repository contains submodules");
    }
    let cache = repo.common.join("agentworks-cow");
    checked_directory(&cache)?;
    let lock_path = cache.join("lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options
        .open(lock_path)
        .map_err(|_| error("Could not open CoW cache lock"))?;
    lock.try_lock()
        .map_err(|_| error("CoW cache is busy; retry shortly"))?;
    // Probe the actual cache/destination pair; filesystem names alone are insufficient.
    let probe = cache.join(format!("probe-{}", uuid::Uuid::new_v4()));
    let clone = path
        .parent()
        .unwrap()
        .join(format!(".cow-probe-{}", uuid::Uuid::new_v4()));
    fs::write(&probe, b"agentworks reflink probe")
        .map_err(|_| error("Could not create CoW probe"))?;
    let result = clone_file(&probe, &clone);
    let _ = fs::remove_file(&probe);
    let _ = fs::remove_file(&clone);
    if let Err(e) = result {
        if matches!(e.raw_os_error(), Some(18 | 95 | 25 | 22))
            || e.kind() == std::io::ErrorKind::Unsupported
        {
            return unsupported(
                settings,
                "source and destination do not support reflinks together",
            );
        }
        return Err(error(
            "CoW probe failed; check permissions and available space",
        ));
    }
    let template = cache.join(commit);
    let template_text = template
        .to_str()
        .ok_or_else(|| error("CoW template path must be UTF-8"))?;
    if fs::symlink_metadata(&template).is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir()) {
        return Err(error("CoW template must be a directory without symlinks"));
    }
    if !template.exists() {
        worktrees::git(
            app,
            &repo.top,
            &["worktree", "add", "--detach", "--", template_text, commit],
        )
        .await?;
    }
    // A tampered/partial cache is never force-reset or reused.
    if worktrees::git(app, &template, &["rev-parse", "HEAD"]).await? != commit
        || !worktrees::git(
            app,
            &template,
            &["status", "--porcelain", "--untracked-files=all"],
        )
        .await?
        .is_empty()
    {
        return Err(error(
            "CoW template is not clean at the requested commit; inspect the repository's agentworks-cow cache",
        ));
    }
    let path_text = path
        .to_str()
        .ok_or_else(|| error("Worktree path must be UTF-8"))?;
    let mut args = vec!["worktree", "add", "--no-checkout"];
    if new {
        args.extend(["--no-track", "-b", branch]);
    }
    args.extend(["--", path_text, if new { commit } else { branch }]);
    worktrees::git(app, &repo.top, &args).await?;
    let files = worktrees::git_limit(app, &template, &["ls-files", "-z"], 16 * 1024 * 1024).await?;
    for name in files.split('\0').filter(|s| !s.is_empty()) {
        let relative = Path::new(name);
        if !relative
            .components()
            .all(|c| matches!(c, Component::Normal(v) if v != ".git"))
        {
            return Err(error("Invalid tracked path in CoW template"));
        }
        let source = template.join(relative);
        let target = path.join(relative);
        parents(path, relative)?;
        let meta =
            fs::symlink_metadata(&source).map_err(|_| error("CoW template file is unavailable"))?;
        if meta.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                fs::read_link(&source).map_err(|_| error("Could not read tracked symlink"))?,
                &target,
            )
            .map_err(|_| error("Could not copy tracked symlink"))?;
            #[cfg(not(unix))]
            return Err(error("Tracked symlinks require Unix CoW support"));
        } else if meta.is_file() {
            clone_file(&source, &target).map_err(|_| {
                error("Could not reflink tracked file; branch and partial checkout retained")
            })?;
        } else {
            return Err(error("Unsupported tracked file in CoW template"));
        }
        tokio::task::yield_now().await;
    }
    let template_git = worktrees::git(app, &template, &["rev-parse", "--absolute-git-dir"]).await?;
    let target_git = worktrees::git(app, path, &["rev-parse", "--absolute-git-dir"]).await?;
    fs::copy(
        Path::new(&template_git).join("index"),
        Path::new(&target_git).join("index"),
    )
    .map_err(|_| error("Could not initialize CoW worktree index"))?;
    worktrees::git(app, path, &["update-index", "--really-refresh"]).await?;
    worktrees::git(app, path, &["reset", "--hard", "--quiet", commit]).await?;
    for name in &settings.share {
        copy_cache(app, &repo.top, path, Path::new(name)).await?;
    }
    Ok(None)
}
fn parents(root: &Path, relative: &Path) -> Result<(), Error> {
    let mut at = root.to_path_buf();
    if let Some(parent) = relative.parent() {
        for part in parent.components() {
            at.push(part);
            checked_directory(&at)?;
        }
    }
    Ok(())
}
async fn copy_cache(
    app: &(impl Context + ?Sized),
    source: &Path,
    target: &Path,
    name: &Path,
) -> Result<(), Error> {
    if !crate::config::relative(name.to_str().unwrap_or_default()) {
        return Err(error("Invalid cache share directory"));
    }
    // Only ignored, untracked directories may be supplied as build caches.
    if !source.join(name).exists() {
        return Ok(());
    }
    if !worktrees::git(app, source, &["ls-files", "--", name.to_str().unwrap()])
        .await?
        .is_empty()
    {
        return Err(error("Shared build directory contains tracked files"));
    }
    worktrees::git(app, source, &["check-ignore", "--", name.to_str().unwrap()]).await?;
    if target.join(name).exists() {
        return Err(error(
            "Shared build directory collides with checkout contents",
        ));
    }
    let mut at = source.to_path_buf();
    for component in name.components() {
        at.push(component);
        let meta = fs::symlink_metadata(&at)
            .map_err(|_| error("Shared build directory is unavailable"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(error("Shared build directory must not traverse symlinks"));
        }
    }
    let mut pending = vec![name.to_path_buf()];
    while let Some(relative) = pending.pop() {
        let src = source.join(&relative);
        let dst = target.join(&relative);
        let meta = fs::symlink_metadata(&src)
            .map_err(|_| error("Shared build cache changed while copying"))?;
        parents(target, &relative)?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            checked_directory(&dst)?;
            for entry in
                fs::read_dir(&src).map_err(|_| error("Could not read shared build cache"))?
            {
                let entry = entry.map_err(|_| error("Could not read shared build cache entry"))?;
                pending.push(relative.join(entry.file_name()));
            }
        } else if meta.is_file() && !meta.file_type().is_symlink() {
            clone_file(&src, &dst)
                .map_err(|_| error("Could not reflink build cache; partial checkout retained"))?;
        } else if meta.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                fs::read_link(&src).map_err(|_| error("Could not read build cache symlink"))?,
                &dst,
            )
            .map_err(|_| error("Could not copy build cache symlink"))?;
        } else {
            return Err(error("Shared build cache contains a special file"));
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}
