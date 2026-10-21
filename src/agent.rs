//! Worker mode: `agentworks agent --stdio`. The Agentworks server starts this
//! over SSH and speaks WebSocket with it over ssh's stdin and stdout, so stdout
//! carries only the protocol and all diagnostics go to stderr. The worker keeps
//! no state of its own: when the link drops it stops every process it started
//! and exits, and the server starts a fresh worker when it reconnects.
//!
//! The worker starts agent adapters on request and relays their ACP lines.
//! Tools come from mise: a non-interactive ssh command does not activate it, so
//! each process gets `mise env` for its working directory.
use crate::hosts::{PROTOCOL, PROVIDERS, READY, ws_config};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::Message;

const USAGE: &str = "Usage: agentworks agent --stdio (started by the Agentworks server over SSH)";
const STDERR_LINES: usize = 8;

pub async fn run(args: &[String]) -> Result<(), String> {
    if args != ["--stdio"] {
        return Err(USAGE.into());
    }
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("{READY}\n").as_bytes())
        .await
        .and(stdout.flush().await)
        .map_err(|e| format!("Could not write to stdout: {e}"))?;
    let stream = tokio::io::join(tokio::io::stdin(), stdout);
    let (mut ws, _) = tokio_tungstenite::client_async_with_config(
        "ws://agentworks/worker",
        stream,
        Some(ws_config()),
    )
    .await
    .map_err(|e| format!("WebSocket handshake with the server failed: {e}"))?;
    let base = mise_env(&home()).await;
    let providers = available_providers(&base);
    let hello = json!({
        "type": "hello",
        "protocol": PROTOCOL,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "hostname": hostname().unwrap_or_default(),
        "providers": providers,
    });
    ws.send(Message::text(hello.to_string()))
        .await
        .map_err(|e| format!("Could not reach the server: {e}"))?;
    let reply = match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            serde_json::from_str::<Value>(&text).unwrap_or(Value::Null)
        }
        _ => return Err("The server did not answer the hello".into()),
    };
    match reply.get("type").and_then(Value::as_str) {
        Some("welcome") if reply["protocol"].as_u64() == Some(PROTOCOL) => {}
        Some("error") => {
            return Err(reply["message"]
                .as_str()
                .unwrap_or("Rejected by the server")
                .into());
        }
        _ => return Err("The server sent an unexpected reply".into()),
    }
    let _ownership = ownership_at(
        &home().join(".cache/agentworks/workers"),
        reply["hostId"].as_str().unwrap_or(""),
    )
    .await?;
    ws.send(Message::text(
        json!({"type":"ready","protocol":PROTOCOL}).to_string(),
    ))
    .await
    .map_err(|e| format!("Could not confirm host ownership: {e}"))?;
    eprintln!("agentworks agent: connected ({})", providers.join(", "));

    let (mut sink, mut frames) = ws.split();
    let (out, mut outgoing) = mpsc::channel::<Message>(256);
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });
    let mut processes: HashMap<u32, Process> = HashMap::new();
    let mut retiring: Vec<Process> = Vec::new();
    let mut reap = tokio::time::interval(Duration::from_secs(1));
    let mut last_heard = Instant::now();
    let ended = loop {
        let frame = tokio::select! {
            frame = frames.next() => frame,
            _ = reap.tick() => {
                if last_heard.elapsed() > Duration::from_secs(45) {
                    break Err("The server stopped responding; stopping this host's processes".into());
                }
                processes.retain(|_, process| !process.task.is_finished());
                retiring.retain(|process| !process.task.is_finished());
                continue;
            }
        };
        if frame.is_some() {
            last_heard = Instant::now();
        }
        match frame {
            // The server closed the link, or ssh went away and stdin hit EOF.
            None | Some(Ok(Message::Close(_))) => break Ok(()),
            Some(Err(e)) => break Err(format!("The link failed: {e}")),
            Some(Ok(Message::Binary(data))) if data.len() >= 4 => {
                let channel = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                if let Some(process) = processes.get(&channel) {
                    let _ = process.input.send(data[4..].to_vec());
                }
            }
            Some(Ok(Message::Text(text))) => {
                let Ok(request) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let channel = request["channel"].as_u64().unwrap_or(0) as u32;
                match request["type"].as_str() {
                    Some("spawn") => {
                        if let Some(mut previous) =
                            processes.insert(channel, start(request, channel, out.clone()))
                        {
                            previous.stop();
                            retiring.push(previous);
                        }
                    }
                    Some("kill") => {
                        if let Some(mut process) = processes.remove(&channel) {
                            process.stop();
                            retiring.push(process);
                        }
                    }
                    Some("checkDir") => {
                        let out = out.clone();
                        tokio::spawn(async move {
                            let mut reply = json!({"type":"reply","request":request["request"]});
                            match check_dir(request["path"].as_str().unwrap_or("")) {
                                Ok(path) => reply["path"] = json!(path),
                                Err(error) => reply["error"] = json!(error),
                            }
                            let _ = out.send(Message::text(reply.to_string())).await;
                        });
                    }
                    _ => {}
                }
            }
            // Pings are answered by the WebSocket library while reading.
            Some(Ok(_)) => {}
        }
    };
    // Stop the socket writer first: exit reports must not block teardown on
    // a full outgoing queue after the peer has gone away.
    writer.abort();
    // Signal every adapter before waiting, so teardown is bounded per link.
    // Killed/replaced channels still own cleanup tasks until their grace period
    // finishes. Keep the host lock through those tasks as well as active ones.
    for process in processes.values_mut().chain(retiring.iter_mut()) {
        process.stop();
    }
    for mut process in processes.into_values().chain(retiring) {
        let _ = (&mut process.task).await;
    }
    eprintln!("agentworks agent: the link closed");
    ended
}

/// Kernel ownership survives a half-open SSH connection and fences replacements.
/// Keep the file in place after unlocking: unlinking would allow two lock inodes.
/// No session state is stored here; the lock is held only by this worker's FD.
async fn ownership_at(dir: &Path, host_id: &str) -> Result<std::fs::File, String> {
    let id = uuid::Uuid::parse_str(host_id).map_err(|_| "Invalid host identity")?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Could not create worker lock directory: {e}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
    let file = options
        .open(dir.join(format!("{id}.lock")))
        .map_err(|e| format!("Could not open worker ownership lock: {e}"))?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock)
                if started.elapsed() < Duration::from_secs(50) =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => {
                return Err(format!(
                    "Could not acquire exclusive host ownership: {error}"
                ));
            }
        }
    }
}

/// The supervisor owns the child and its process group until it has stopped.
/// Dropping a channel asks that supervisor to stop; no stale PID is retained
/// in this map after the child exits.
struct Process {
    input: mpsc::UnboundedSender<Vec<u8>>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Process {
    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start(request: Value, channel: u32, out: mpsc::Sender<Message>) -> Process {
    let (input, lines) = mpsc::unbounded_channel();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        if let Err(message) = run_process(request, channel, out.clone(), lines, stopped).await {
            let exit = json!({"type":"exit","channel":channel,"message":message});
            let _ = out.send(Message::text(exit.to_string())).await;
        }
    });
    Process {
        input,
        stop: Some(stop),
        task,
    }
}

async fn run_process(
    request: Value,
    channel: u32,
    out: mpsc::Sender<Message>,
    mut lines: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stopped: oneshot::Receiver<()>,
) -> Result<(), String> {
    let provider = request["provider"].as_str().unwrap_or("");
    if !PROVIDERS.contains(&provider) {
        return Err("Unknown provider".into());
    }
    let cwd = PathBuf::from(request["cwd"].as_str().unwrap_or(""));
    if !cwd.is_dir() {
        return Err(format!(
            "{} is not a directory on this host.",
            cwd.display()
        ));
    }
    crate::runtime::prepare(provider).map_err(|e| format!("Could not prepare {provider}: {e}"))?;
    // Environment discovery must not block link pings, other channels or kill.
    let env = tokio::select! {
        env = mise_env(&cwd) => env,
        _ = &mut stopped => return Ok(()),
    };
    // Remote sessions always use the host's own sign-ins.
    let config = crate::runtime::agent_config(
        provider,
        false,
        request["hostProfile"].as_bool().unwrap_or(false),
    );
    let program = resolve(config.command(), &env).ok_or_else(|| {
        format!(
            "{} was not found on this host's PATH (including mise).",
            config.command().display()
        )
    })?;
    let mut child = Command::new(program)
        .args(config.arguments())
        .current_dir(&cwd)
        .envs(&env)
        .envs(config.environment())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start {provider}: {e}"))?;
    let group = child.id().ok_or("The process exited immediately")? as i32;
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("Could not attach to the process".into());
    };

    let input_task = tokio::spawn(async move {
        while let Some(mut line) = lines.recv().await {
            line.push(b'\n');
            if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let errors: Arc<Mutex<VecDeque<String>>> = Default::default();
    let collected = errors.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line: String = line.trim().chars().take(300).collect();
            if line.is_empty() {
                continue;
            }
            let mut tail = collected.lock().unwrap();
            if tail.len() == STDERR_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    });
    let stdout_out = out.clone();
    let mut stdout_task = tokio::spawn(async move {
        let out = stdout_out;
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                        line.pop();
                    }
                    let mut frame = Vec::with_capacity(line.len() + 4);
                    frame.extend_from_slice(&channel.to_be_bytes());
                    frame.extend_from_slice(&line);
                    if out.send(Message::binary(frame)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    let status = tokio::select! {
        status = child.wait() => status,
        _ = stopped => {
            // Do not reap the group leader before escalation: its PID must
            // remain reserved until we have sent the final group signal.
            unsafe { libc::killpg(group, libc::SIGTERM); }
            tokio::time::sleep(Duration::from_secs(2)).await;
            unsafe { libc::killpg(group, libc::SIGKILL); }
            child.wait().await
        }
    };
    input_task.abort();
    // Drain final ACP output before the exit frame, but never let an
    // inherited stdout descriptor keep this supervisor alive indefinitely.
    let _ = tokio::time::timeout(Duration::from_secs(1), &mut stdout_task).await;
    stdout_task.abort();
    stderr_task.abort();
    let tail: Vec<String> = errors.lock().unwrap().iter().cloned().collect();
    let mut message = match status {
        Ok(status) => format!("The agent exited ({status})."),
        Err(e) => format!("The agent failed: {e}."),
    };
    if !tail.is_empty() {
        message.push_str(&format!(" Last output: {}", tail.join(" / ")));
    }
    let exit = json!({"type":"exit","channel":channel,"message":message});
    let _ = out.send(Message::text(exit.to_string())).await;
    Ok(())
}

/// Which providers can actually start here, using the mise environment.
fn available_providers(env: &HashMap<String, String>) -> Vec<&'static str> {
    let node = resolve(Path::new("node"), env).is_some();
    PROVIDERS
        .into_iter()
        .filter(|provider| {
            let config = crate::runtime::agent_config(provider, false, false);
            match *provider {
                "opencode" => resolve(config.command(), env).is_some(),
                // The demo agent and the ACP adapters are node scripts.
                // The adapter itself, or the demo agent's script under node.
                _ => {
                    let script = config.arguments().first().map(Path::new);
                    node && (config.command().is_file() || script.is_some_and(Path::is_file))
                }
            }
        })
        .collect()
}

/// `mise env` for a directory, falling back to the global configuration when
/// the directory's own configuration cannot be used (for example, an
/// untrusted project config). Without mise, the inherited environment stands.
async fn mise_env(dir: &Path) -> HashMap<String, String> {
    let Some(mise) = find_mise() else {
        return HashMap::new();
    };
    for dir in [dir, &home()] {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            Command::new(&mise)
                .args(["env", "--json", "-C"])
                .arg(dir)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await;
        match output {
            Ok(Ok(output)) if output.status.success() => {
                if let Ok(map) = serde_json::from_slice::<HashMap<String, String>>(&output.stdout) {
                    return map;
                }
            }
            Ok(Ok(output)) => eprintln!(
                "agentworks agent: mise env in {} failed: {}",
                dir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            _ => eprintln!(
                "agentworks agent: mise env in {} did not finish",
                dir.display()
            ),
        }
    }
    HashMap::new()
}

fn find_mise() -> Option<PathBuf> {
    resolve(Path::new("mise"), &HashMap::new()).or_else(|| {
        let local = home().join(".local/bin/mise");
        local.is_file().then_some(local)
    })
}

/// Finds a bare program name on the PATH that the process will actually get.
fn resolve(program: &Path, env: &HashMap<String, String>) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return program.is_file().then(|| program.to_path_buf());
    }
    let path = env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn check_dir(path: &str) -> Result<String, String> {
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    };
    let canonical = std::fs::canonicalize(&expanded)
        .map_err(|_| format!("{path} does not exist on this host."))?;
    if !canonical.is_dir() {
        return Err(format!("{path} is not a directory on this host."));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn replacement_waits_for_ownership_and_other_hosts_remain_independent() {
        let dir =
            std::env::temp_dir().join(format!("agentworks-worker-lock-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let first = ownership_at(&dir, &id).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), ownership_at(&dir, &id))
                .await
                .is_err()
        );
        let other = ownership_at(&dir, &uuid::Uuid::new_v4().to_string())
            .await
            .unwrap();
        drop(first);
        let replacement = ownership_at(&dir, &id).await.unwrap();
        assert!(ownership_at(&dir, "../../outside").await.is_err());
        drop((other, replacement));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
