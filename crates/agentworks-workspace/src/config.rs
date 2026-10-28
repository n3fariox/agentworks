//! One portable repository configuration file, with an extensible top-level namespace.
use crate::{Error, fail, worktrees::Context};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub const CONFIG_PATH: &str = ".agentworks/config.toml";
pub const CONFIG_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CopyMode {
    #[default]
    Plain,
    PreferCow,
    RequireCow,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Worktrees {
    pub copy: Option<CopyMode>,
    pub share: Option<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "hook_timeout")]
    pub timeout_seconds: u64,
}
fn hook_timeout() -> u64 {
    60
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hooks {
    pub enter: Vec<Hook>,
    pub exit: Vec<Hook>,
}
impl Hooks {
    pub fn validate(&self) -> Result<(), Error> {
        for hooks in [&self.enter, &self.exit] {
            if hooks.len() > 8
                || hooks
                    .iter()
                    .map(|h| h.timeout_seconds.min(301))
                    .sum::<u64>()
                    > 300
            {
                return Err(bad(
                    "Use at most eight hooks and 300 seconds combined timeout per event",
                ));
            }
            for h in hooks {
                if h.name.is_empty()
                    || h.name.len() > 100
                    || h.name.chars().any(char::is_control)
                    || h.command.is_empty()
                    || h.command.len() > 64
                    || h.command[0].is_empty()
                    || h.command.iter().any(|a| a.len() > 4096 || a.contains('\0'))
                    || !(1..=300).contains(&h.timeout_seconds)
                {
                    return Err(bad(
                        "Hooks need a name, command arguments, and a timeout from 1 to 300 seconds",
                    ));
                }
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Git {
    pub worktrees: Worktrees,
    pub hooks: Hooks,
}
#[derive(Deserialize)]
struct Document {
    version: Option<u32>,
    #[serde(default)]
    git: Git,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub copy: CopyMode,
    pub share: Vec<String>,
    pub run_hooks: bool,
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        validate_share(&self.share)
    }
    pub fn resolve(&self, git: &Git) -> Self {
        Self {
            copy: git.worktrees.copy.unwrap_or(self.copy),
            share: git
                .worktrees
                .share
                .clone()
                .unwrap_or_else(|| self.share.clone()),
            run_hooks: self.run_hooks,
        }
    }
}
pub fn relative(path: &str) -> bool {
    !path.is_empty() && Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
        && !Path::new(path).components().any(|c| matches!(c, Component::Normal(v) if v == ".git" || v == ".worktrees" || v == ".agentworks"))
}
fn validate_share(paths: &[String]) -> Result<(), String> {
    if paths.len() > 32 || paths.iter().any(|p| p.len() > 512 || !relative(p)) {
        return Err("Share at most 32 relative build directories, without traversal or Git/Agentworks state".into());
    }
    Ok(())
}
pub fn parse(text: &str) -> Result<Git, Error> {
    if text.len() > CONFIG_LIMIT {
        return Err(bad("Repository configuration exceeds 64 KiB"));
    }
    // Parser diagnostics can echo secrets from unrelated TOML sections.
    let doc: Document = toml::from_str(text).map_err(|_| {
        bad("Invalid .agentworks/config.toml; check TOML syntax and Git field names")
    })?;
    if doc.version.is_some_and(|v| v != 1) {
        return Err(bad("Unsupported repository configuration version"));
    }
    validate_share(doc.git.worktrees.share.as_deref().unwrap_or_default()).map_err(bad)?;
    doc.git.hooks.validate()?;
    Ok(doc.git)
}
fn bad(message: impl Into<String>) -> Error {
    fail(StatusCode::BAD_REQUEST, message)
}

pub fn config_path(app: &(impl Context + ?Sized), top: &Path) -> Result<PathBuf, Error> {
    let mut path = app.checked(
        top.to_str()
            .ok_or_else(|| bad("Repository path must be UTF-8"))?,
    )?;
    for part in [".agentworks", "config.toml"] {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(bad("Repository configuration must not traverse symlinks"));
            }
            Ok(m) if part == ".agentworks" && !m.is_dir() => {
                return Err(bad(".agentworks must be a directory"));
            }
            Ok(m) if part == "config.toml" && !m.is_file() => {
                return Err(bad("Repository configuration must be a regular file"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(bad("Repository configuration is unavailable")),
        }
    }
    Ok(path)
}
pub fn read(app: &(impl Context + ?Sized), top: &Path) -> Result<String, Error> {
    let path = config_path(app, top)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let f = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(_) => return Err(bad("Repository configuration is unavailable")),
    };
    let mut text = String::new();
    f.take((CONFIG_LIMIT + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|_| bad("Repository configuration must be UTF-8"))?;
    if text.len() > CONFIG_LIMIT {
        return Err(bad("Repository configuration exceeds 64 KiB"));
    }
    Ok(text)
}
pub fn write(
    app: &(impl Context + ?Sized),
    top: &Path,
    text: &str,
    expected: &str,
) -> Result<(), Error> {
    parse(text)?;
    if crate::sha256::hex(read(app, top)?.as_bytes()) != expected {
        return Err(fail(
            StatusCode::CONFLICT,
            "Repository configuration changed; reload before saving",
        ));
    }
    let path = config_path(app, top)?;
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent)
        .map_err(|_| bad("Could not create repository configuration directory"))?;
    config_path(app, top)?;
    let staging = parent.join(format!(".config-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&staging, path)
    })();
    let _ = std::fs::remove_file(staging);
    result.map_err(|_| bad("Could not save repository configuration"))
}
