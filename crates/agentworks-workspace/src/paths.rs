//! Host paths as Agentworks stores and shows them: `/`-separated on every host.
//! Windows accepts `/` as well as `\`, so one spelling serves display, storage,
//! prefix comparisons and the paths the browser sends back.
use std::path::{Path, PathBuf};

/// Canonical path. On Windows, the `\\?\` verbatim prefix is dropped wherever
/// the plain form names the same file, so roots and requested paths compare
/// equal component by component whichever spelling they arrived in.
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// The stored and displayed form of a host-local path.
pub fn display(path: &Path) -> String {
    let path = path.to_string_lossy();
    if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.into_owned()
    }
}

/// Validates a configured host path, whatever the host OS, and returns its
/// stored form: `/` separators and no trailing separator except at a root.
/// Accepts `/…`, `~`, `~/…` and drive paths (`C:\work`, `c:/work` → `C:/work`).
/// Drive-relative (`C:work`) and plain relative paths are rejected.
pub fn normalize(raw: &str) -> Option<String> {
    let mut path = raw.trim().replace('\\', "/");
    let bytes = path.as_bytes();
    let root = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if path.len() == 2 {
            path.push('/');
        }
        if path.as_bytes()[2] != b'/' {
            return None;
        }
        path[..1].make_ascii_uppercase();
        3
    } else if path.starts_with('/') || path.starts_with("~/") || path == "~" {
        1
    } else {
        return None;
    };
    while path.len() > root && path.ends_with('/') {
        path.pop();
    }
    if path == "~/" {
        path.pop();
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_posix_home_and_drive_paths() {
        for (raw, stored) in [
            ("/", "/"),
            ("/srv/work/", "/srv/work"),
            (" ~/projects/ ", "~/projects"),
            ("~", "~"),
            ("~/", "~"),
            ("C:\\work\\", "C:/work"),
            ("c:/Users/me", "C:/Users/me"),
            ("C:", "C:/"),
            ("C:\\", "C:/"),
            ("\\\\server\\share", "//server/share"),
        ] {
            assert_eq!(normalize(raw).as_deref(), Some(stored), "{raw}");
        }
        for raw in ["", "work", "./work", "C:work", "~user", "1:/x"] {
            assert_eq!(normalize(raw), None, "{raw}");
        }
    }

    #[test]
    fn displays_with_forward_slashes() {
        let dir = std::env::temp_dir();
        let canonical = canonicalize(&dir).unwrap();
        let shown = display(&canonical);
        assert!(!shown.contains('\\') && !shown.starts_with("//?/"));
        assert_eq!(canonicalize(&shown).unwrap(), canonical);
    }
}
