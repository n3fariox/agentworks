//! Ownership verification, dirty fingerprints, removal, and restoration.
use crate::{Error, fail, worktrees};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Recorded only when `newWorktree` preparation created the checkout for this session.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Owned {
    pub path: PathBuf,
    pub branch: String,
    pub common_dir: PathBuf,
    pub main_top: PathBuf,
    pub base_commit: String,
    /// Milliseconds since the epoch when archive cleanup removed the checkout.
    #[serde(default)]
    pub removed: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Cleanup {
    /// Remove a clean owned worktree; retain anything dirty or unsafe.
    #[default]
    Auto,
    Keep,
    /// Remove even with changes, but only the exact dirty state the human confirmed.
    Remove,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dirty {
    staged: usize,
    unstaged: usize,
    untracked: usize,
    paths: Vec<String>,
    truncated: bool,
}
impl Dirty {
    pub fn any(&self) -> bool {
        self.staged + self.unstaged + self.untracked > 0
    }
}

#[derive(Serialize, Deserialize)]
pub struct Inspection {
    pub owned: Option<Owned>,
    pub state: String,
    pub dirty: Option<Dirty>,
    pub fingerprint: Option<String>,
    pub locked: bool,
    pub shared_with: Vec<Value>,
    /// Why removal is unsafe regardless of the dirty state.
    pub blocker: Option<String>,
}

fn conflict(message: impl Into<String>) -> Error {
    fail(StatusCode::CONFLICT, message)
}
fn utf8(path: &std::path::Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| conflict("Worktree path must be UTF-8"))
}

/// Confirms the path is still the recorded linked checkout of the recorded repository.
async fn verify(app: &(impl worktrees::Context + ?Sized), o: &Owned) -> Result<(), String> {
    let path = utf8(&o.path).map_err(|e| e.1)?;
    if app.checked(path).map_err(|e| e.1)? != o.path {
        return Err("Worktree path no longer resolves to its recorded location".into());
    }
    let repo = worktrees::repository(app, &o.path).await.map_err(|e| e.1)?;
    if repo.top != o.path || !repo.linked || repo.common != o.common_dir {
        return Err("Path is no longer the recorded linked worktree of this repository".into());
    }
    let head = worktrees::git(app, &o.path, &["symbolic-ref", "-q", "HEAD"])
        .await
        .map_err(|_| "HEAD is detached; commits might exist only in this checkout".to_owned())?;
    if head != format!("refs/heads/{}", o.branch) {
        return Err(format!(
            "Checked-out {} differs from the recorded branch {}; commits might exist only in this checkout",
            head.strip_prefix("refs/heads/").unwrap_or(&head),
            o.branch
        ));
    }
    Ok(())
}

/// Parses `status --porcelain=v1 -z`; rename/copy records carry an extra origin path.
fn parse_status(raw: &str) -> (Dirty, Vec<&str>) {
    let mut dirty = Dirty::default();
    let mut all = Vec::new();
    let mut entries = raw.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let bytes = entry.as_bytes();
        let Some(path) = entry.get(3..).filter(|p| !p.is_empty() && bytes[2] == b' ') else {
            continue;
        };
        let (x, y) = (bytes[0], bytes[1]);
        if x == b'?' {
            dirty.untracked += 1;
        } else {
            dirty.staged += usize::from(x != b' ');
            dirty.unstaged += usize::from(y != b' ');
            if matches!(x, b'R' | b'C') {
                entries.next();
            }
        }
        all.push(path);
        if dirty.paths.len() < 20 {
            dirty.paths.push(path.to_owned());
        } else {
            dirty.truncated = true;
        }
    }
    (dirty, all)
}

pub async fn inspect_in(
    app: &(impl worktrees::Context + ?Sized),
    owned: Option<Owned>,
) -> Inspection {
    let mut result = Inspection {
        owned: owned.clone(),
        state: "unowned".into(),
        dirty: None,
        fingerprint: None,
        locked: false,
        shared_with: Vec::new(),
        blocker: None,
    };
    let Some(o) = owned else {
        return result;
    };
    if o.removed.is_some() {
        result.state = "removed".into();
        return result;
    }
    if std::fs::symlink_metadata(&o.path).is_err() {
        result.state = "missing".into();
        result.blocker = Some("Worktree path is missing".into());
        return result;
    }
    result.state = "present".into();
    let checked = async {
        verify(app, &o).await?;
        let list = worktrees::git(app, &o.path, &["worktree", "list", "--porcelain", "-z"])
            .await
            .map_err(|e| e.1)?;
        let record = format!("worktree {}", o.path.display());
        let locked = list.split("\0\0").any(|r| {
            let mut fields = r.split('\0');
            fields.next() == Some(record.as_str())
                && fields.any(|f| f == "locked" || f.starts_with("locked "))
        });
        let head = worktrees::git(app, &o.path, &["rev-parse", "HEAD"])
            .await
            .map_err(|e| e.1)?;
        let raw = worktrees::git(
            app,
            &o.path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )
        .await
        .map_err(|e| format!("Could not inspect changes: {}", e.1))?;
        // Size/mtime catch later edits to a path that was already dirty when confirmed.
        let (dirty, paths) = parse_status(&raw);
        let mut material = format!("{head}\0{raw}");
        for path in paths {
            if let Ok(meta) = std::fs::symlink_metadata(o.path.join(path)) {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                material.push_str(&format!("\0{}:{modified}", meta.len()));
            }
        }
        Ok::<_, String>((locked, dirty, crate::sha256::hex(material.as_bytes())))
    };
    match checked.await {
        Ok((locked, dirty, fingerprint)) => {
            result.locked = locked;
            result.dirty = Some(dirty);
            result.fingerprint = Some(fingerprint);
        }
        Err(reason) => result.blocker = Some(reason),
    }
    if result.blocker.is_none() && result.locked {
        result.blocker = Some("Worktree is locked".into());
    }
    result
}

/// Re-inspect on the filesystem owner immediately before mutation, including
/// the confirmation fingerprint. Never force a locked or replaced checkout.
pub async fn remove_in(
    app: &(impl worktrees::Context + ?Sized),
    o: &Owned,
    expected: Option<&str>,
    force: bool,
) -> Result<Value, Error> {
    let current = inspect_in(app, Some(o.clone())).await;
    if current.state != "present" || current.blocker.is_some() {
        return Err(conflict(
            current
                .blocker
                .unwrap_or_else(|| "Worktree is not present".into()),
        ));
    }
    if expected.is_none() || current.fingerprint.as_deref() != expected {
        return Err(conflict(
            "Worktree changed while preparing removal; inspect it and retry",
        ));
    }
    let dirty = current.dirty.as_ref().is_some_and(Dirty::any);
    if dirty && !force {
        return Err(conflict("Worktree has uncommitted changes"));
    }
    let main = worktrees::repository(app, &o.main_top).await?;
    if main.common != o.common_dir {
        return Err(conflict(
            "Repository no longer matches the recorded worktree",
        ));
    }
    let path = utf8(&o.path)?;
    let mut args = vec!["worktree", "remove"];
    if dirty {
        args.push("--force");
    }
    args.extend(["--", path]);
    worktrees::git(app, &o.main_top, &args).await?;
    Ok(json!({"outcome":"removed","forced":dirty,"path":o.path,"branch":o.branch}))
}

pub async fn restore_in(
    app: &(impl worktrees::Context + ?Sized),
    o: &Owned,
) -> Result<Option<Value>, Error> {
    if o.removed.is_none() && std::fs::symlink_metadata(&o.path).is_ok() {
        verify(app, o).await.map_err(conflict)?;
        return Ok(None);
    }
    match std::fs::symlink_metadata(&o.path) {
        Ok(_) => {
            // Adopt only the same repository's checkout of the recorded branch.
            if verify(app, &o).await.is_ok() {
                return Ok(Some(
                    json!({"outcome":"adopted","path":o.path,"branch":o.branch}),
                ));
            }
            return Err(conflict(format!(
                "Restore path {} is occupied by something else. Move it aside or inspect it, then unarchive again; the session stays archived and branch {} is retained",
                o.path.display(),
                o.branch
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(conflict("Restore path is unavailable")),
    }
    if !o.main_top.is_dir() {
        return Err(conflict(
            "Repository checkout for this worktree is unavailable",
        ));
    }
    let repo = worktrees::repository(app, &o.main_top).await?;
    if repo.common != o.common_dir {
        return Err(conflict(
            "Repository no longer matches the recorded worktree",
        ));
    }
    worktrees::git(
        app,
        &o.main_top,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", o.branch),
        ],
    )
    .await
    .map_err(|_| {
        conflict(format!(
            "Branch {} no longer exists; restore refused",
            o.branch
        ))
    })?;
    let path = worktrees::destination(app, &o.path)?;
    let parent = path
        .parent()
        .ok_or_else(|| conflict("Invalid worktree path"))?;
    std::fs::create_dir_all(parent)
        .map_err(|_| conflict("Could not create worktree parent directory"))?;
    worktrees::destination(app, &path)?;
    std::fs::create_dir(&path).map_err(|_| {
        conflict("Restore path appeared concurrently; inspect it, then unarchive again")
    })?;
    let path_str = utf8(&path)?;
    if let Err((_, message)) = worktrees::git(
        app,
        &o.main_top,
        &["worktree", "add", "--", path_str, &o.branch],
    )
    .await
    {
        // Only the empty directory reserved above is removed; remove_dir refuses content.
        let _ = std::fs::remove_dir(&path);
        return Err(conflict(format!("Could not recreate worktree: {message}")));
    }
    verify(app, &o).await.map_err(conflict)?;
    Ok(Some(
        json!({"outcome":"restored","path":o.path,"branch":o.branch}),
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_status;
    #[test]
    fn counts_staged_unstaged_untracked_and_renames() {
        let (d, all) = parse_status(
            "M  staged.txt\0 M edited.txt\0MM both.txt\0R  new.txt\0old.txt\0?? fresh.txt\0",
        );
        assert_eq!((d.staged, d.unstaged, d.untracked), (3, 2, 1));
        assert_eq!(
            d.paths,
            [
                "staged.txt",
                "edited.txt",
                "both.txt",
                "new.txt",
                "fresh.txt"
            ]
        );
        assert_eq!(all.len(), 5);
        assert!(!parse_status("").0.any());
    }
}
