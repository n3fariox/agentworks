//! The user's current login environment for a local harness. The service's own
//! environment is fixed when it starts, so edits to shell startup files (Unix)
//! or to user and system variables (Windows) would otherwise reach a harness
//! only after a service restart. Each local harness start reads the current
//! environment instead; restarting the harness is enough to pick up a change.
//!
//! On Unix this is an interactive login shell, reused for a minute per
//! directory so a burst of starts runs it once. Restart harness always reads
//! afresh. On Windows it is the registry environment a new logon would get.
//!
//! Only variables that differ from the service's are applied, and
//! harness-specific settings still win. A variable removed from the startup
//! files stays inherited from the service. Any failure falls back silently to
//! the service environment. Set `AGENTWORKS_LOGIN_ENV=0` to disable.
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

/// Shell bookkeeping that describes the probe, not the user's settings.
const PROCESS_STATE: [&str; 5] = ["PWD", "OLDPWD", "SHLVL", "_", OUTPUT];
const OUTPUT: &str = "AGENTWORKS_LOGIN_ENV_OUT";

/// `fresh` bypasses the Unix cache, for an explicit harness restart.
pub async fn changes(cwd: &Path, fresh: bool) -> BTreeMap<String, String> {
    if std::env::var("AGENTWORKS_LOGIN_ENV").as_deref() == Ok("0") {
        return BTreeMap::new();
    }
    match read(cwd, fresh).await {
        Ok(login) => differing(login, std::env::vars()),
        Err(error) => {
            eprintln!(
                "agentworks: login environment unavailable; using the service environment: {error}"
            );
            BTreeMap::new()
        }
    }
}

#[cfg(unix)]
mod shell {
    use super::{OUTPUT, is_path, pair};
    use std::{
        collections::BTreeMap,
        ffi::OsStr,
        os::unix::fs::OpenOptionsExt,
        path::{Path, PathBuf},
        process::Stdio,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    const TIMEOUT: Duration = Duration::from_secs(5);
    const REUSE_FOR: Duration = Duration::from_secs(60);
    type Vars = Arc<Vec<(String, String)>>;
    static CACHE: Mutex<BTreeMap<PathBuf, (Instant, Vars)>> = Mutex::new(BTreeMap::new());

    pub async fn read(cwd: &Path, fresh: bool) -> Result<Vec<(String, String)>, String> {
        let shell = std::env::var_os("SHELL")
            .filter(|s| !s.is_empty())
            .ok_or("SHELL is not set")?;
        cached(&shell, cwd, fresh).await
    }

    pub(super) async fn cached(
        shell: &OsStr,
        cwd: &Path,
        fresh: bool,
    ) -> Result<Vec<(String, String)>, String> {
        if !fresh
            && let Some((at, vars)) = CACHE.lock().unwrap().get(cwd)
            && at.elapsed() < REUSE_FOR
        {
            return Ok(vars.to_vec());
        }
        let vars = Arc::new(probe(shell, cwd).await?);
        let mut cache = CACHE.lock().unwrap();
        cache.retain(|_, (at, _)| at.elapsed() < REUSE_FOR);
        cache.insert(cwd.to_owned(), (Instant::now(), vars.clone()));
        Ok(vars.to_vec())
    }

    /// `env -0` from `$SHELL -l -i`. Output goes to a private file rather than a
    /// pipe: startup files may print, and a daemon they start may hold stdout open.
    async fn probe(shell: &OsStr, cwd: &Path) -> Result<Vec<(String, String)>, String> {
        let path = std::env::temp_dir().join(format!("agentworks-env-{}", uuid::Uuid::new_v4()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let result = async {
            let mut command = tokio::process::Command::new(shell);
            for name in agentworks_runtime::provider::SERVICE_ENVIRONMENT {
                command.env_remove(name);
            }
            let mut child = command
                .args(["-l", "-i", "-c", &format!("env -0 > \"${OUTPUT}\"")])
                .env(OUTPUT, &path)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| e.to_string())?;
            match tokio::time::timeout(TIMEOUT, child.wait()).await {
                Ok(Ok(status)) if status.success() => {}
                Ok(Ok(status)) => return Err(format!("login shell exited with {status}")),
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => return Err(format!("login shell did not finish in {TIMEOUT:?}")),
            }
            let raw = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
            let vars: Vec<_> = raw
                .split(|&b| b == 0)
                .filter_map(|entry| std::str::from_utf8(entry).ok())
                .filter_map(pair)
                .collect();
            // A startup file that execs another shell never runs the command.
            if !vars.iter().any(|(name, _)| is_path(name)) {
                return Err("login shell reported no PATH".into());
            }
            Ok(vars)
        }
        .await;
        let _ = std::fs::remove_file(&path);
        result
    }
}
#[cfg(unix)]
use shell::read;

#[cfg(windows)]
async fn read(_cwd: &Path, _fresh: bool) -> Result<Vec<(String, String)>, String> {
    tokio::task::spawn_blocking(registry::read)
        .await
        .map_err(|e| e.to_string())?
}

/// The environment a new logon would get: system then user variables from the
/// registry, expanded, without this process's own. Cheap enough not to cache.
#[cfg(windows)]
mod registry {
    use super::pair;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY},
        System::{
            Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    pub fn read() -> Result<Vec<(String, String)>, String> {
        let error = || std::io::Error::last_os_error().to_string();
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the current-process pseudo-handle is always valid; on success
        // `token` is an owned handle, closed below.
        let access = TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE;
        if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) } == 0 {
            return Err(error());
        }
        let mut block = std::ptr::null_mut();
        // SAFETY: `token` is open; `block` receives an allocation freed below.
        let created = unsafe { CreateEnvironmentBlock(&mut block, token, 0) } != 0;
        let failure = (!created).then(error);
        // SAFETY: `token` is owned and not used again.
        unsafe { CloseHandle(token) };
        if let Some(failure) = failure {
            return Err(failure);
        }
        let mut vars = Vec::new();
        let mut entry = block as *const u16;
        // SAFETY: the block is NUL-terminated UTF-16 strings ending with an empty
        // one, valid until DestroyEnvironmentBlock.
        unsafe {
            loop {
                let mut len = 0;
                while *entry.add(len) != 0 {
                    len += 1;
                }
                if len == 0 {
                    break;
                }
                if let Ok(text) = String::from_utf16(std::slice::from_raw_parts(entry, len)) {
                    vars.extend(pair(&text));
                }
                entry = entry.add(len + 1);
            }
            DestroyEnvironmentBlock(block);
        }
        Ok(vars)
    }
}

#[cfg(not(any(unix, windows)))]
async fn read(_cwd: &Path, _fresh: bool) -> Result<Vec<(String, String)>, String> {
    Err("not supported on this platform".into())
}

/// `NAME=value`. Windows per-drive entries such as `=C:=C:\` have no name.
fn pair(entry: &str) -> Option<(String, String)> {
    let (name, value) = entry.split_once('=')?;
    (!name.is_empty()).then(|| (name.to_owned(), value.to_owned()))
}

/// Windows variable names are case-insensitive: `Path` is `PATH`.
fn name_key(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_owned()
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn is_path(name: &str) -> bool {
    name_key(name) == "PATH"
}

fn differing(
    login: Vec<(String, String)>,
    service: impl IntoIterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    let service: HashMap<String, String> = service
        .into_iter()
        .map(|(name, value)| (name_key(&name), value))
        .collect();
    login
        .into_iter()
        .filter(|(name, value)| {
            !PROCESS_STATE.contains(&name.as_str()) && service.get(&name_key(name)) != Some(value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_skip_malformed_and_nameless_entries_and_keep_equals_in_values() {
        let vars: Vec<_> = ["PATH=/a:/b", "BROKEN", "=C:=C:\\", "OPTS=a=b"]
            .into_iter()
            .filter_map(pair)
            .collect();
        assert_eq!(
            vars,
            vec![
                ("PATH".into(), "/a:/b".into()),
                ("OPTS".into(), "a=b".into())
            ]
        );
    }

    #[test]
    fn only_changed_user_settings_are_applied() {
        let login = vec![
            ("PATH".into(), "/new:/usr/bin".into()),
            ("EDITOR".into(), "vim".into()),
            ("ADDED".into(), "1".into()),
            ("PWD".into(), "/tmp".into()),
            ("SHLVL".into(), "2".into()),
        ];
        let service = [
            ("PATH".into(), "/usr/bin".into()),
            ("EDITOR".into(), "vim".into()),
            ("PWD".into(), "/srv".into()),
        ];
        assert_eq!(
            differing(login, service),
            BTreeMap::from([
                ("ADDED".into(), "1".into()),
                ("PATH".into(), "/new:/usr/bin".into()),
            ])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repeated_starts_reuse_one_read_until_a_restart_asks_for_a_fresh_one() {
        // A directory of its own keeps this independent of other tests' entries.
        let dir = std::env::temp_dir().join(format!("agentworks-login-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let count = dir.join("count");
        let shell = dir.join("shell");
        // Counts its runs, then reports a PATH and the run number.
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\necho x >> '{}'\nexport RUN=$(wc -l < '{}')\nshift 3\nexec /bin/sh -c \"$1\"\n",
                count.display(),
                count.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run = |vars: Vec<(String, String)>| {
            vars.into_iter()
                .find(|(n, _)| n == "RUN")
                .unwrap()
                .1
                .trim()
                .to_owned()
        };
        assert_eq!(
            run(shell::cached(shell.as_os_str(), &dir, false).await.unwrap()),
            "1"
        );
        assert_eq!(
            run(shell::cached(shell.as_os_str(), &dir, false).await.unwrap()),
            "1"
        );
        assert_eq!(
            run(shell::cached(shell.as_os_str(), &dir, true).await.unwrap()),
            "2"
        );
        assert_eq!(
            run(shell::cached(shell.as_os_str(), &dir, false).await.unwrap()),
            "2"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
