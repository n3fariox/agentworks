//! Snapshotted worktree lifecycle hooks, with bounded process-group execution.
use crate::{Error, cleanup::Owned, config::Hooks, fail, worktrees::Context};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub definitions: Hooks,
    pub enter: Status,
    pub exit: Status,
    pub last_message: Option<String>,
}

#[cfg(unix)]
struct Group(i32);
#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

async fn bounded(mut stream: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            return Ok(output);
        }
        if output.len() + n > 32 * 1024 {
            return Err(std::io::Error::other("Hook output limit exceeded"));
        }
        output.extend_from_slice(&buffer[..n]);
    }
}
pub async fn run(
    app: &(impl Context + ?Sized),
    owned: &Owned,
    event: &str,
    reason: &str,
) -> Result<String, Error> {
    crate::cleanup::verify(app, owned)
        .await
        .map_err(|e| fail(StatusCode::CONFLICT, e))?;
    let Some(state) = &owned.hooks else {
        return Ok(String::new());
    };
    state.definitions.validate()?;
    let hooks = match event {
        "enter" => &state.definitions.enter,
        "exit" => &state.definitions.exit,
        _ => return Err(fail(StatusCode::BAD_REQUEST, "Invalid hook event")),
    };
    let mut messages = Vec::new();
    for hook in hooks {
        let mut command = Command::new(&hook.command[0]);
        command
            .args(&hook.command[1..])
            .current_dir(&owned.path)
            .env("AGENTWORKS_WORKTREE", &owned.path)
            .env("AGENTWORKS_REPOSITORY", &owned.main_top)
            .env("AGENTWORKS_BRANCH", &owned.branch)
            .env("AGENTWORKS_HOOK_EVENT", event)
            .env("AGENTWORKS_HOOK_REASON", reason)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|_| {
            fail(
                StatusCode::CONFLICT,
                format!("Could not start hook {}", hook.name),
            )
        })?;
        #[cfg(unix)]
        let _group = Group(child.id().unwrap() as i32);
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(hook.timeout_seconds), async {
            tokio::try_join!(child.wait(), bounded(stdout), bounded(stderr))
        })
        .await;
        match result {
            Ok(Ok((status, out, err))) => {
                let message = app.safe_error(&format!(
                    "{}{}",
                    String::from_utf8_lossy(&out),
                    String::from_utf8_lossy(&err)
                ));
                if !status.success() {
                    return Err(fail(
                        StatusCode::CONFLICT,
                        format!(
                            "Hook {} failed ({status}): {}",
                            hook.name,
                            message.chars().take(4000).collect::<String>()
                        ),
                    ));
                }
                messages.push(format!(
                    "{}: {}",
                    hook.name,
                    message.chars().take(4000).collect::<String>()
                ));
            }
            Ok(Err(_)) => {
                return Err(fail(
                    StatusCode::CONFLICT,
                    format!(
                        "Hook {} exceeded its output limit or could not read output",
                        hook.name
                    ),
                ));
            }
            Err(_) => {
                return Err(fail(
                    StatusCode::CONFLICT,
                    format!(
                        "Hook {} timed out after {} seconds",
                        hook.name, hook.timeout_seconds
                    ),
                ));
            }
        }
    }
    Ok(messages.join("\n").chars().take(16000).collect())
}
