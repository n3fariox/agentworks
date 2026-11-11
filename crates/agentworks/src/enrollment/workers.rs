//! Finds the worker binary to install on a host of a given platform.
//!
//! A server bundle carries one worker per platform beside the server binary:
//!
//! ```text
//! agentworks                                   the server
//! workers/linux-x86_64/agentworks-worker
//! workers/linux-aarch64/agentworks-worker
//! workers/macos-aarch64/agentworks-worker
//! workers/windows-x86_64/agentworks-worker.exe
//! ```
//!
//! Service releases share one directory, so `service --install` gives each its
//! own copy named after the release (`<exe>-workers/`), which is checked first;
//! a newer release must not change an older one's workers. Platform names are
//! `<os>-<arch>` from Rust's `std::env::consts`, as workers report in `hello`.
//! Builds without a bundle fall back to the older siblings `<exe>-worker` (this
//! platform) and `<exe>-worker.exe` (Windows x86_64), and finally to the full
//! server binary, which serves every worker command, for this platform.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

/// The bundle directory beside the server.
pub const BUNDLE: &str = "workers";
/// A service release's own copy: `<release>-workers`.
pub const RELEASE_SUFFIX: &str = "-workers";

/// This server's own platform, e.g. `linux-x86_64`.
pub fn native() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The executable file name of a worker for `platform`.
pub fn file_name(platform: &str) -> &'static str {
    if platform.starts_with("windows-") {
        "agentworks-worker.exe"
    } else {
        "agentworks-worker"
    }
}

/// `uname -sm` output as a platform name. Unknown systems keep their own
/// lowercase name, so they find no worker rather than a wrong one.
pub fn from_uname(uname: &str) -> Option<String> {
    let mut parts = uname.split_whitespace();
    let os = match parts.next()? {
        "Linux" => "linux".to_owned(),
        "Darwin" => "macos".to_owned(),
        other => other.to_ascii_lowercase(),
    };
    Some(format!("{os}-{}", arch(parts.next()?)))
}

/// `%PROCESSOR_ARCHITECTURE%` as a platform name.
pub fn from_windows(arch_name: &str) -> Option<String> {
    let arch_name = arch_name.trim();
    (!arch_name.is_empty()).then(|| format!("windows-{}", arch(arch_name)))
}

fn arch(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "amd64" | "x86_64" | "x64" => "x86_64".into(),
        "arm64" | "aarch64" => "aarch64".into(),
        other => other.into(),
    }
}

/// Where a server at `exe` looks for its bundle, most specific first.
fn bundle_dirs(exe: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(name) = exe.file_name() {
        let mut name = name.to_os_string();
        name.push(RELEASE_SUFFIX);
        dirs.push(exe.with_file_name(name));
    }
    dirs.push(exe.with_file_name(BUNDLE));
    dirs
}

/// The bundle directory this server uses, if it has one.
pub fn bundle() -> Option<PathBuf> {
    bundle_dirs(&std::env::current_exe().ok()?)
        .into_iter()
        .find(|d| d.is_dir())
}

/// What will be installed on a host of `platform`.
pub enum Worker {
    Built(PathBuf),
    /// The full server binary; only possible for this server's own platform.
    Server(PathBuf),
}

/// The worker for `platform`, or an error naming the platforms this server has.
pub fn for_platform(platform: &str) -> Result<Worker, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(path) = bundle()
        .map(|d| d.join(platform).join(file_name(platform)))
        .filter(|p| p.is_file())
    {
        return Ok(Worker::Built(path));
    }
    if platform == native() {
        return Ok(match sibling_worker() {
            Some(path) => Worker::Built(path),
            None => Worker::Server(exe),
        });
    }
    if platform == "windows-x86_64"
        && let Some(path) = windows_sibling()
    {
        return Ok(Worker::Built(path));
    }
    Err(format!(
        "This server has no worker for {platform} hosts. It has: {}. Build one into {} (see scripts/build-bundle.sh).",
        available().join(", "),
        bundle()
            .unwrap_or_else(|| exe.with_file_name(BUNDLE))
            .join(platform)
            .display(),
    ))
}

/// Platforms this server can install, sorted.
pub fn available() -> Vec<String> {
    let mut platforms: Vec<String> = bundle()
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let platform = e.file_name().into_string().ok()?;
            e.path()
                .join(file_name(&platform))
                .is_file()
                .then_some(platform)
        })
        .collect();
    platforms.push(native());
    if windows_sibling().is_some() {
        platforms.push("windows-x86_64".into());
    }
    platforms.sort();
    platforms.dedup();
    platforms
}

/// The worker-only executable built or installed beside this one (`<exe>-worker`),
/// if it speaks this server's worker protocol. Cargo relinks only changed binaries,
/// so timestamps cannot tell a stale sibling from a current one.
pub fn sibling_worker() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut name = exe.file_name()?.to_os_string();
    name.push("-worker");
    let worker = exe.with_file_name(name);
    let probe = std::process::Command::new(&worker)
        .arg("--worker-protocol")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let reply: serde_json::Value = serde_json::from_slice(&probe.stdout).ok()?;
    (probe.status.success() && reply["workerProtocol"] == agentworks_runtime::remote::PROTOCOL)
        .then_some(worker)
}

/// The Windows worker built beside this server (`<exe>-worker.exe`), e.g. by
/// `npm run build:windows-worker`. It cannot be probed here; install checks it.
pub fn windows_sibling() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut name = exe.file_name()?.to_os_string();
    name.push("-worker.exe");
    Some(exe.with_file_name(name)).filter(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platforms_use_the_names_workers_report() {
        assert_eq!(
            from_uname("Linux x86_64\n").as_deref(),
            Some("linux-x86_64")
        );
        assert_eq!(
            from_uname("Linux aarch64").as_deref(),
            Some("linux-aarch64")
        );
        assert_eq!(from_uname("Darwin arm64").as_deref(), Some("macos-aarch64"));
        assert_eq!(
            from_uname("FreeBSD amd64").as_deref(),
            Some("freebsd-x86_64")
        );
        assert_eq!(from_uname(""), None);
        assert_eq!(from_windows("AMD64\r\n").as_deref(), Some("windows-x86_64"));
        assert_eq!(from_windows("ARM64").as_deref(), Some("windows-aarch64"));
        assert_eq!(from_windows(" "), None);
        assert_eq!(
            native(),
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        );
        assert_eq!(file_name("windows-x86_64"), "agentworks-worker.exe");
        assert_eq!(file_name("linux-aarch64"), "agentworks-worker");
        assert_eq!(
            bundle_dirs(Path::new("/r/agentworks-0123")),
            [
                Path::new("/r/agentworks-0123-workers"),
                Path::new("/r/workers")
            ]
        );
    }
}
