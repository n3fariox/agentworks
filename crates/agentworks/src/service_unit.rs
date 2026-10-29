//! systemd user service: installation, readiness notification and release rollback.
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const UNIT: &str = "agentworks.service";
const TEMPLATE: &str = include_str!("../../../scripts/service/agentworks.service.in");
/// The release systemd launches. A restart request repoints it before exiting 75.
pub const CURRENT: &str = "current";
/// The last release that reached READY=1; `--rollback` restores `current` from it.
pub const STABLE: &str = "stable";

/// Atomically repoints `dir/link` at a release in the same directory.
pub fn point(dir: &Path, link: &str, release: &Path) -> std::io::Result<()> {
    let name = release
        .file_name()
        .ok_or_else(|| std::io::Error::other("Release has no file name"))?;
    let temp = dir.join(format!(".{link}.{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(name, &temp)?;
    if let Err(e) = fs::rename(&temp, dir.join(link)) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    fs::File::open(dir)?.sync_all()
}

/// Sends a state string to the service manager. Returns false outside systemd.
pub fn notify(state: &str) -> std::io::Result<bool> {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(false);
    };
    let socket = std::os::unix::net::UnixDatagram::unbound()?;
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "linux")]
    if let Some(name) = path.as_bytes().strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        let address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
        socket.send_to_addr(state.as_bytes(), &address)?;
        return Ok(true);
    }
    socket.send_to(state.as_bytes(), &path)?;
    Ok(true)
}

/// True when this process was started by systemd as a service's process, not merely
/// a descendant that inherited its environment and cgroup.
pub fn under_systemd() -> bool {
    if std::env::var_os("NOTIFY_SOCKET").is_none() || std::env::var_os("INVOCATION_ID").is_none() {
        return false;
    }
    let service = fs::read_to_string("/proc/self/cgroup")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("0::").map(|p| p.ends_with(".service")))
        .unwrap_or(false);
    let parent = fs::read_to_string(format!(
        "/proc/{}/comm",
        std::os::unix::process::parent_id()
    ))
    .is_ok_and(|c| c.trim() == "systemd");
    service && parent
}

/// ExecStopPost: a release that failed before readiness is replaced by the last ready one.
/// A clean stop or a deliberate restart (exit 75) leaves `current` as it is.
fn rollback() -> Result<()> {
    let result = std::env::var("SERVICE_RESULT").unwrap_or_default();
    let status = std::env::var("EXIT_STATUS").unwrap_or_default();
    if result.is_empty() || result == "success" || (result == "exit-code" && status == "75") {
        return Ok(());
    }
    let dir = PathBuf::from(
        std::env::var_os("AGENTWORKS_RELEASE_DIR").ok_or("AGENTWORKS_RELEASE_DIR is not set")?,
    );
    let stable = fs::read_link(dir.join(STABLE))?;
    let current = fs::read_link(dir.join(CURRENT)).ok();
    if current.as_deref() == Some(stable.as_path()) {
        return Ok(());
    }
    point(&dir, CURRENT, &stable)?;
    eprintln!(
        "Agentworks: {} stopped with {result} {status} before it was ready; rolled back to {}",
        current.map_or("unknown release".into(), |p| p.display().to_string()),
        stable.display()
    );
    Ok(())
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").ok_or("HOME is not set")?,
    ))
}
fn xdg(var: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => Ok(p),
        _ => Ok(home()?.join(fallback)),
    }
}

/// Reads paths from an installed unit so reinstalling keeps the existing layout,
/// including units written for the former Python supervisor.
fn previous(unit: &str) -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let mut releases = None;
    let mut service = None;
    let mut working = None;
    for line in unit.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("WorkingDirectory=") {
            working = Some(PathBuf::from(v));
        } else if let Some(v) = line.strip_prefix("Environment=AGENTWORKS_RELEASE_DIR=") {
            releases = Some(PathBuf::from(v));
        } else if let Some(v) = line.strip_prefix("Environment=AGENTWORKS_SERVICE_DIR=") {
            service = Some(PathBuf::from(v));
        } else if line.starts_with("ExecStart=") {
            let words: Vec<_> = line.split_whitespace().collect();
            for pair in words.windows(2) {
                match pair[0] {
                    "--release-dir" => releases = Some(PathBuf::from(pair[1])),
                    "--control-dir" => service = Some(PathBuf::from(pair[1])),
                    _ => {}
                }
            }
        }
    }
    (releases, service, working)
}

fn unit_path(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or("Path is not UTF-8")?;
    // Unit files expand %, $ and quotes; keep paths literal.
    if !path.is_absolute()
        || text
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "%$\"'\\;".contains(c))
    {
        return Err(format!("Unsupported path for a unit file: {text}").into());
    }
    Ok(text.into())
}

/// The built-in store location belongs to development runs, so the service must name its own.
fn explicit_store(env: &str) -> Result<()> {
    for key in ["AGENTWORKS_CATALOG_PATH", "AGENTWORKS_DB_PATH"] {
        let value = env
            .lines()
            .filter_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
            .last()
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\''));
        if !value.is_some_and(|v| Path::new(v).is_absolute()) {
            return Err(format!(
                "service.env must set {key} to an absolute path; the built-in default is for development runs only"
            )
            .into());
        }
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(path.canonicalize()?)
}

fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Copies this executable into the release directory as an immutable release.
fn install_release(releases: &Path, name: Option<&str>) -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe.parent() == Some(releases) && name.is_none_or(|n| exe.file_name() == Some(n.as_ref())) {
        return Ok(exe);
    }
    let hash = digest(&exe)?;
    let name = name.map_or_else(|| format!("agentworks-{}", &hash[..12]), String::from);
    if name.is_empty()
        || name.len() > 128
        || [CURRENT, STABLE, ".", ".."].contains(&name.as_str())
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err("Release must be an executable basename".into());
    }
    let target = releases.join(&name);
    if target.exists() {
        if digest(&target)? != hash {
            return Err(format!(
                "{} already exists with different contents",
                target.display()
            )
            .into());
        }
        return Ok(target);
    }
    let temp = releases.join(format!(".install-{}", uuid::Uuid::new_v4()));
    let copied = (|| -> Result<()> {
        fs::copy(&exe, &temp)?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o500))?;
        fs::File::open(&temp)?.sync_all()?;
        fs::rename(&temp, &target)?;
        Ok(())
    })();
    if copied.is_err() {
        let _ = fs::remove_file(&temp);
    }
    copied?;
    fs::File::open(releases)?.sync_all()?;
    Ok(target)
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()?;
    if !status.success() {
        return Err(format!("systemctl --user {} failed: {status}", args.join(" ")).into());
    }
    Ok(())
}

struct Install {
    releases: Option<PathBuf>,
    service: Option<PathBuf>,
    working: Option<PathBuf>,
    release: Option<String>,
    restart: bool,
}

fn install(options: Install) -> Result<()> {
    let units = xdg("XDG_CONFIG_HOME", ".config")?.join("systemd/user");
    let unit_file = units.join(UNIT);
    let old = fs::read_to_string(&unit_file).ok();
    let (old_releases, old_service, old_working) = old.as_deref().map(previous).unwrap_or_default();
    let releases = private_dir(&match options.releases.or(old_releases) {
        Some(p) => p,
        None => xdg("XDG_DATA_HOME", ".local/share")?.join("agentworks/releases"),
    })?;
    let service = private_dir(&match options.service.or(old_service) {
        Some(p) => p,
        None => xdg("XDG_STATE_HOME", ".local/state")?.join("agentworks/service"),
    })?;
    let working = match options.working.or(old_working) {
        Some(p) => p,
        None => std::env::current_dir()?,
    }
    .canonicalize()?;
    let unit = TEMPLATE
        .replace("@WORKING_DIRECTORY@", &unit_path(&working)?)
        .replace("@RELEASE_DIRECTORY@", &unit_path(&releases)?)
        .replace("@SERVICE_DIRECTORY@", &unit_path(&service)?);

    let env = home()?.join(".config/agentworks/service.env");
    let env =
        fs::read_to_string(&env).map_err(|e| format!("Cannot read {}: {e}", env.display()))?;
    explicit_store(&env)?;

    let release = install_release(&releases, options.release.as_deref())?;
    point(&releases, CURRENT, &release)?;
    point(&releases, STABLE, &release)?;
    println!("Release: {}", release.display());

    fs::DirBuilder::new().recursive(true).create(&units)?;
    if let Some(old) = old.filter(|o| *o != unit) {
        let backup = units.join(format!("{UNIT}.bak"));
        fs::write(&backup, old)?;
        println!("Previous unit saved to {}", backup.display());
    }
    let temp = units.join(format!(".{UNIT}.{}", uuid::Uuid::new_v4()));
    fs::write(&temp, &unit)?;
    fs::rename(&temp, &unit_file)?;
    println!("Unit: {}", unit_file.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    if options.restart {
        // --no-block: when run from inside the service, this process is about to be stopped.
        systemctl(&["restart", "--no-block", UNIT])?;
        println!("Restart queued. Follow it with: journalctl --user -fu {UNIT}");
    } else {
        println!("Enabled. Start or switch to it with: systemctl --user restart {UNIT}");
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let usage = "Usage: agentworks service --install [--release-dir DIR] [--service-dir DIR] [--working-directory DIR] [--release NAME] [--restart]";
    match args.first().map(String::as_str) {
        Some("--rollback") if args.len() == 1 => rollback(),
        Some("--install") => {
            let mut options = Install {
                releases: None,
                service: None,
                working: None,
                release: None,
                restart: false,
            };
            let mut rest = args[1..].iter();
            while let Some(flag) = rest.next() {
                let mut value = || rest.next().cloned().ok_or(usage);
                match flag.as_str() {
                    "--release-dir" => options.releases = Some(value()?.into()),
                    "--service-dir" => options.service = Some(value()?.into()),
                    "--working-directory" => options.working = Some(value()?.into()),
                    "--release" => options.release = Some(value()?),
                    "--restart" => options.restart = true,
                    _ => return Err(usage.into()),
                }
            }
            install(options)
        }
        _ => Err(usage.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_previous_layouts() {
        let legacy = "[Service]\nWorkingDirectory=/w\nExecStart=/usr/bin/python3 /s.py --release-dir /r --release a --control-dir /c\n";
        assert_eq!(
            previous(legacy),
            (Some("/r".into()), Some("/c".into()), Some("/w".into()))
        );
        let rendered = TEMPLATE
            .replace("@WORKING_DIRECTORY@", "/w")
            .replace("@RELEASE_DIRECTORY@", "/r")
            .replace("@SERVICE_DIRECTORY@", "/c");
        assert_eq!(
            previous(&rendered),
            (Some("/r".into()), Some("/c".into()), Some("/w".into()))
        );
        assert!(!rendered.contains('@'));
    }
    #[test]
    fn requires_explicit_store_paths() {
        assert!(explicit_store("AGENTWORKS_PORT=4318\n").is_err());
        assert!(explicit_store("AGENTWORKS_CATALOG_PATH=/c.json\n").is_err());
        assert!(
            explicit_store(
                "AGENTWORKS_CATALOG_PATH=/c.json\nAGENTWORKS_DB_PATH=relative.sqlite3\n"
            )
            .is_err()
        );
        assert!(
            explicit_store("AGENTWORKS_CATALOG_PATH=\"/c.json\"\nAGENTWORKS_DB_PATH=/d.sqlite3\n")
                .is_ok()
        );
    }
    #[test]
    fn rejects_paths_unit_files_would_expand() {
        assert!(unit_path(Path::new("/home/a b")).is_err());
        assert!(unit_path(Path::new("/home/%h")).is_err());
        assert!(unit_path(Path::new("relative")).is_err());
        assert!(unit_path(Path::new("/home/agentworks/releases")).is_ok());
    }
    #[test]
    fn point_replaces_link_atomically() {
        let dir = std::env::temp_dir().join(format!("agentworks-unit-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        point(&dir, CURRENT, &dir.join("a")).unwrap();
        point(&dir, CURRENT, &dir.join("b")).unwrap();
        assert_eq!(fs::read_link(dir.join(CURRENT)).unwrap(), Path::new("b"));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
