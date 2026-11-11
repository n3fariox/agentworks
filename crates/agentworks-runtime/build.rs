//! Bakes the build identity into `AGENTWORKS_BUILD`: the crate version and the
//! git commit it was built from, e.g. `0.1.0+3f1c2d4e5a6b`. Server and worker
//! both link this crate, so a host whose worker reports a different build than
//! the server is out of date. Packagers without a git checkout can set
//! `AGENTWORKS_BUILD_COMMIT`; otherwise the commit is `unknown`, which never
//! counts as out of date.
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

fn main() {
    println!("cargo:rerun-if-env-changed=AGENTWORKS_BUILD_COMMIT");
    let commit = env::var("AGENTWORKS_BUILD_COMMIT")
        .ok()
        .filter(|c| !c.trim().is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
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
    println!(
        "cargo:rustc-env=AGENTWORKS_BUILD={}+{}",
        env::var("CARGO_PKG_VERSION").unwrap(),
        commit.trim()
    );
}
