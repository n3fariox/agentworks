//! Browsing and creating project directories beneath allowed roots.
//!
//! Shared by the server (local host) and the remote worker. Callers pass the
//! configured roots; every path is canonicalized here and must stay inside a
//! canonical root, so browser-supplied strings are never trusted.
use crate::{Error, fail};
use http::StatusCode;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Entries scanned per listing; larger directories are reported as truncated.
const SCAN_LIMIT: usize = 5000;

/// Existing roots, canonicalized and deduplicated, in configured order.
pub fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        if let Ok(root) = std::fs::canonicalize(root)
            && root.is_dir()
            && !out.contains(&root)
        {
            out.push(root);
        }
    }
    out
}

/// The outermost root containing `path`, which must already be canonical.
fn containing(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|r| path.starts_with(r))
        .min_by_key(|r| r.components().count())
        .cloned()
}

/// Canonical directory inside a root, with that root.
pub fn resolve(roots: &[PathBuf], path: &str) -> Result<(PathBuf, PathBuf), Error> {
    let requested = Path::new(path);
    if path.is_empty() || !requested.is_absolute() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use an absolute directory path",
        ));
    }
    let roots = canonical_roots(roots);
    // One answer for missing, outside and non-directory paths, so the API
    // cannot probe what exists beyond the roots.
    let hidden = || {
        fail(
            StatusCode::NOT_FOUND,
            "No such directory inside the allowed roots",
        )
    };
    let path = std::fs::canonicalize(requested).map_err(|_| hidden())?;
    let root = containing(&roots, &path).ok_or_else(hidden)?;
    if !path.is_dir() {
        return Err(hidden());
    }
    Ok((path, root))
}

/// Lists the roots (no `path`) or the subdirectories of one directory.
/// Symlinks that resolve outside the containing root are omitted.
pub fn browse(roots: &[PathBuf], path: Option<&str>, hidden: bool) -> Result<Value, Error> {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        let roots = canonical_roots(roots);
        let entries: Vec<Value> = roots
            .iter()
            .map(|r| json!({"name":r.to_string_lossy(),"path":r}))
            .collect();
        return Ok(
            json!({"path":null,"root":null,"parent":null,"entries":entries,"truncated":false}),
        );
    };
    let (path, root) = resolve(roots, path)?;
    let reader = std::fs::read_dir(&path)
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "Directory cannot be read"))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for (scanned, entry) in reader.enumerate() {
        if scanned >= SCAN_LIMIT {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let Ok(target) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if !target.starts_with(&root) || !target.is_dir() {
            continue;
        }
        entries.push(json!({"name":name,"path":target}));
    }
    entries.sort_by_key(|e| e["name"].as_str().unwrap_or_default().to_lowercase());
    let parent = path
        .parent()
        .filter(|p| path != root && p.starts_with(&root))
        .map(Path::to_path_buf);
    Ok(json!({"path":path,"root":root,"parent":parent,"entries":entries,"truncated":truncated}))
}

/// A single new path component.
pub fn check_name(name: &str) -> Result<(), Error> {
    let bad = |m: &str| Err(fail(StatusCode::BAD_REQUEST, m));
    if name.is_empty() || name.trim() != name {
        return bad("Use a folder name without leading or trailing spaces");
    }
    if name.len() > 255 {
        return bad("Use a folder name under 256 bytes");
    }
    if name.starts_with('.') {
        return bad("Folder names cannot start with a dot");
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return bad("Folder names cannot contain slashes or control characters");
    }
    Ok(())
}

/// Creates `name` directly inside `parent`, which must be inside a root.
/// Never follows a symlink at the new name and never reuses an existing entry.
pub fn create(roots: &[PathBuf], parent: &str, name: &str) -> Result<PathBuf, Error> {
    check_name(name)?;
    let (parent, root) = resolve(roots, parent)?;
    let created = make(&parent, &root, name)?;
    // Defense in depth: the result must still resolve to parent/name inside the root.
    match std::fs::canonicalize(&created) {
        Ok(path) if path == created && path.starts_with(&root) && path.is_dir() => Ok(path),
        _ => Err(fail(
            StatusCode::CONFLICT,
            "The new folder did not resolve inside the allowed root",
        )),
    }
}

fn exists() -> Error {
    fail(
        StatusCode::CONFLICT,
        "A file or folder with that name already exists",
    )
}

#[cfg(target_os = "linux")]
fn make(parent: &Path, root: &Path, name: &str) -> Result<PathBuf, Error> {
    use std::{ffi::CString, os::fd::AsRawFd, os::unix::fs::OpenOptionsExt};
    // Open the checked parent and verify the opened directory itself, so a path
    // component swapped for a symlink after canonicalization cannot redirect us.
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|_| fail(StatusCode::CONFLICT, "Parent directory changed; retry"))?;
    let opened =
        std::fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd())).map_err(|_| {
            fail(
                StatusCode::CONFLICT,
                "Could not verify the parent directory",
            )
        })?;
    if opened != parent || !opened.starts_with(root) {
        return Err(fail(
            StatusCode::CONFLICT,
            "Parent directory changed; retry",
        ));
    }
    let c_name =
        CString::new(name).map_err(|_| fail(StatusCode::BAD_REQUEST, "Invalid folder name"))?;
    // mkdirat never follows an existing entry (including a symlink) at `name`.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), c_name.as_ptr(), 0o777) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(match error.kind() {
            std::io::ErrorKind::AlreadyExists => exists(),
            std::io::ErrorKind::PermissionDenied => fail(
                StatusCode::FORBIDDEN,
                "No permission to create a folder here",
            ),
            _ => fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not create the folder",
            ),
        });
    }
    Ok(parent.join(name))
}

#[cfg(not(target_os = "linux"))]
fn make(parent: &Path, _root: &Path, name: &str) -> Result<PathBuf, Error> {
    // Best effort off Linux: create_dir does not follow a final symlink and
    // `create` re-checks containment of the result.
    let path = parent.join(name);
    std::fs::create_dir(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => exists(),
        std::io::ErrorKind::PermissionDenied => fail(
            StatusCode::FORBIDDEN,
            "No permission to create a folder here",
        ),
        _ => fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not create the folder",
        ),
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp() -> Temp {
        let dir = std::env::temp_dir().join(format!("agentworks-dirs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Temp(std::fs::canonicalize(dir).unwrap())
    }
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn lists_roots_then_only_contained_directories() {
        let t = temp();
        let root = t.0.join("root");
        for d in ["root/b", "root/A", "root/.hidden", "outside/secret"] {
            std::fs::create_dir_all(t.0.join(d)).unwrap();
        }
        std::fs::write(root.join("file.txt"), "x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(t.0.join("outside"), root.join("escape")).unwrap();
            std::os::unix::fs::symlink(root.join("b"), root.join("inner")).unwrap();
        }
        let roots = vec![root.clone(), t.0.join("missing")];
        let top = browse(&roots, None, false).unwrap();
        assert_eq!(top["entries"].as_array().unwrap().len(), 1);
        assert_eq!(top["entries"][0]["path"], json!(root));
        let listing = browse(&roots, Some(&s(&root)), false).unwrap();
        let names: Vec<&str> = listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        #[cfg(unix)]
        assert_eq!(names, ["A", "b", "inner"]);
        assert!(!names.contains(&"escape") && !names.contains(&"file.txt"));
        assert_eq!(listing["parent"], Value::Null);
        let child = browse(&roots, Some(&s(&root.join("b"))), true).unwrap();
        assert_eq!(child["parent"], json!(root));
        assert_eq!(child["root"], json!(root));
    }

    #[test]
    fn refuses_paths_outside_roots() {
        let t = temp();
        let root = t.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(t.0.join("outside")).unwrap();
        let roots = vec![root.clone()];
        let traversal = format!("{}/../outside", s(&root));
        assert_eq!(
            browse(&roots, Some(&traversal), false).unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            browse(&roots, Some("relative"), false).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            create(&roots, &traversal, "new").unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        assert!(!t.0.join("outside/new").exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(t.0.join("outside"), root.join("escape")).unwrap();
            let link = s(&root.join("escape"));
            assert_eq!(
                browse(&roots, Some(&link), false).unwrap_err().0,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                create(&roots, &link, "new").unwrap_err().0,
                StatusCode::NOT_FOUND
            );
            assert!(!t.0.join("outside/new").exists());
        }
    }

    #[test]
    fn creates_one_new_folder_and_never_reuses_entries() {
        let t = temp();
        let root = t.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let roots = vec![root.clone()];
        let created = create(&roots, &s(&root), "My project").unwrap();
        assert_eq!(created, root.join("My project"));
        assert!(created.is_dir());
        assert_eq!(
            create(&roots, &s(&root), "My project").unwrap_err().0,
            StatusCode::CONFLICT
        );
        std::fs::write(root.join("file"), "x").unwrap();
        assert_eq!(
            create(&roots, &s(&root), "file").unwrap_err().0,
            StatusCode::CONFLICT
        );
        #[cfg(unix)]
        {
            std::fs::create_dir_all(t.0.join("outside")).unwrap();
            std::os::unix::fs::symlink(t.0.join("outside/target"), root.join("dangling")).unwrap();
            assert_eq!(
                create(&roots, &s(&root), "dangling").unwrap_err().0,
                StatusCode::CONFLICT
            );
            assert!(!t.0.join("outside/target").exists());
        }
    }

    #[test]
    fn rejects_names_that_are_not_one_plain_component() {
        for name in [
            "", " a", "a ", ".", "..", ".git", "a/b", "a\\b", "a\0b", "a\nb",
        ] {
            assert_eq!(
                check_name(name).unwrap_err().0,
                StatusCode::BAD_REQUEST,
                "{name:?}"
            );
        }
        assert!(check_name(&"x".repeat(256)).is_err());
        for name in ["project", "My project", "v1.2", "naïve"] {
            assert!(check_name(name).is_ok(), "{name:?}");
        }
    }
}
