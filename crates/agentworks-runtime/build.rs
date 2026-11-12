//! Bakes the build version into `AGENTWORKS_BUILD`: the crate's major and
//! minor version with the commit count as the last digit, e.g. `0.1.376` for
//! the 376th commit. Server and worker both link this crate, so a host whose
//! worker reports a different version than the server is out of date.
//!
//! Packagers without a git checkout (or with a shallow one, whose count is
//! wrong) set `AGENTWORKS_BUILD_NUMBER`. Otherwise the version ends in `-dev`,
//! e.g. `0.1.0-dev`, which never counts as out of date.
use std::{env, path::PathBuf, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(env::var_os("CARGO_MANIFEST_DIR")?)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Commits reachable from HEAD; refused for shallow clones, which undercount.
fn commit_count() -> Option<u64> {
    if git(&["rev-parse", "--is-shallow-repository"]).as_deref() != Some("false") {
        return None;
    }
    git(&["rev-list", "--count", "HEAD"])?.parse().ok()
}

fn main() {
    println!("cargo:rerun-if-env-changed=AGENTWORKS_BUILD_NUMBER");
    let number = env::var("AGENTWORKS_BUILD_NUMBER")
        .ok()
        .and_then(|n| n.trim().parse::<u64>().ok())
        .or_else(commit_count);
    // Rebuild when HEAD moves: its file, the branch it names, and packed refs.
    for path in ["HEAD", "packed-refs"]
        .into_iter()
        .map(String::from)
        .chain(git(&["symbolic-ref", "-q", "HEAD"]))
    {
        if let Some(file) = git(&["rev-parse", "--git-path", &path]) {
            let file = PathBuf::from(file);
            let file = if file.is_absolute() {
                file
            } else {
                PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join(file)
            };
            // A missing file would rerun this script on every build.
            if file.exists() {
                println!("cargo:rerun-if-changed={}", file.display());
            }
        }
    }
    let (major, minor) = (
        env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
        env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
    );
    let version = match number {
        Some(n) => format!("{major}.{minor}.{n}"),
        None => format!("{major}.{minor}.0-dev"),
    };
    println!("cargo:rustc-env=AGENTWORKS_BUILD={version}");
}
