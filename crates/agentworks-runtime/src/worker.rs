//! Worker mode: `agentworks agent --stdio`. The Agentworks server starts this
//! over SSH and speaks WebSocket with it over ssh's stdin and stdout, so stdout
//! carries only the protocol and all diagnostics go to stderr. The worker keeps
//! no state of its own: when the link drops it stops every process it started
//! and exits, and the server starts a fresh worker when it reconnects.
//!
//! The worker starts agent adapters on request and relays their ACP lines.
//! Tools come from mise: a non-interactive ssh command does not activate it, so
//! each process gets `mise env` for its working directory.
use crate::remote::{PROTOCOL, PROVIDERS, READY, ws_config};
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
    let (out, mut outgoing) = mpsc::channel::<Message>(256);
    let mcp = crate::mcp_relay::WorkerBridge::start(out.clone())
        .await
        .map_err(|e| e.to_string())?;
    let base = mise_env(&home()).await;
    let providers = available_providers(&base);
    let hello = json!({
        "type": "hello",
        "protocol": PROTOCOL,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "hostname": hostname().unwrap_or_default(),
        "providers": providers,
        "tools": discovered_tools(&base),
        "mcpRelay": {"endpoint":mcp.address,"command":std::env::current_exe().map_err(|e| e.to_string())?},
        "workspace": true,
        "modelProviders": true,
        "sharedModelProviders": true,
        "worktreeCleanup": true,
        "directories": true,
        "terminal": true,
        "hostMetrics": if crate::metrics::supported() { 1 } else { 0 },
        "providerAuthTerminal": true,
        "managedCodexAuth": true,
        "home": home(),
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
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });
    let mut metrics = crate::metrics::WorkerMetrics::default();
    let mut workspace_tasks = tokio::task::JoinSet::new();
    let workspace_lock = Arc::new(tokio::sync::Mutex::new(()));
    let mut retiring_terminals = Vec::<crate::terminal::Handle>::new();
    let mut terminals: HashMap<u32, crate::terminal::Handle> = HashMap::new();
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
                while workspace_tasks.try_join_next().is_some() {}
                processes.retain(|_, process| !process.task.is_finished());
                retiring.retain(|process| !process.task.is_finished());
                retiring_terminals.retain(|handle| handle.frames.has_changed().is_ok());
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
                    Some("mcpReply") => mcp.reply(&request),

                    Some("metricsStart") if crate::metrics::supported() => {
                        if let Some(id) = request["subscription"].as_u64() {
                            metrics.start(id, out.clone());
                        }
                    }
                    Some("metricsStop") => {
                        if let Some(id) = request["subscription"].as_u64() {
                            metrics.stop(id);
                        }
                    }
                    Some("terminalOpen") => {
                        let cwd = PathBuf::from(request["cwd"].as_str().unwrap_or(""));
                        if terminals.contains_key(&channel) {
                            continue;
                        }
                        let opened = if let Some(auth) =
                            request.get("providerAuth").filter(|a| a.is_object())
                        {
                            crate::model_providers::login_terminal(
                                auth["kind"].as_str().unwrap_or(""),
                                auth["id"].as_str().unwrap_or(""),
                            )
                        } else {
                            crate::terminal::spawn(cwd)
                        };
                        match opened {
                            Ok(handle) => {
                                let mut frames = handle.frames.clone();
                                let out = out.clone();
                                tokio::spawn(async move {
                                    let mut history_cursor = None;
                                    loop {
                                        let mut frame = frames.borrow_and_update().clone();
                                        frame.history = frame.history.since(&mut history_cursor);
                                        let exited = frame.exited;
                                        if out.send(Message::text(json!({"type":"terminalFrame","channel":channel,"frame":frame}).to_string())).await.is_err() || exited {break;}
                                        if frames.changed().await.is_err() {
                                            break;
                                        }
                                    }
                                });
                                terminals.insert(channel, handle);
                            }
                            Err(error) => {
                                let frame = crate::terminal::Frame {
                                    exited: true,
                                    error: Some(error),
                                    ..Default::default()
                                };
                                let _ = out.try_send(Message::text(
                                    json!({"type":"terminalFrame","channel":channel,"frame":frame})
                                        .to_string(),
                                ));
                            }
                        }
                    }
                    Some("terminalInput") => {
                        if let Some(handle) = terminals.get(&channel) {
                            if let Ok(input) = serde_json::from_value(request["input"].clone()) {
                                if handle.input.try_send(input).is_err() {
                                    handle
                                        .stopped
                                        .store(true, std::sync::atomic::Ordering::Release);
                                }
                            }
                        }
                    }
                    Some("terminalClose") => {
                        if let Some(handle) = terminals.remove(&channel) {
                            handle
                                .stopped
                                .store(true, std::sync::atomic::Ordering::Release);
                            retiring_terminals.push(handle);
                        }
                    }
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
                    Some("workspace") => {
                        let out = out.clone();
                        let lock = workspace_lock.clone();
                        let removing = request["operation"] == "removeWorktree";
                        let path = request["owned"]["path"].as_str().map(PathBuf::from);
                        let affected = |process: &&Process| {
                            path.as_ref().is_some_and(|p| process.cwd.starts_with(p))
                        };
                        let active = removing
                            && processes
                                .values()
                                .filter(affected)
                                .any(|p| !p.task.is_finished());
                        let retiring: Vec<_> = if removing {
                            retiring
                                .iter()
                                .filter(affected)
                                .map(|p| p.finished.clone())
                                .collect()
                        } else {
                            Vec::new()
                        };
                        workspace_tasks.spawn(async move {
                            let _guard = lock.lock().await;
                            let result = async {
                                if active { return Err("An agent is still running in this worktree; stop it and retry".into()); }
                                // A kill request only initiates process-group shutdown. Wait for
                                // that shutdown before inspecting or removing its directory.
                                for mut finished in retiring {
                                    finished.wait_for(|done| *done).await.map_err(|_| "Could not confirm agent shutdown")?;
                                }
                                workspace_request(&request).await
                            }.await;
                            let mut reply = json!({"type":"reply","request":request["request"]});
                            match result {
                                Ok(value) => reply["result"] = value,
                                Err(error) => reply["error"] = json!(error),
                            }
                            let _ = out.send(Message::text(reply.to_string())).await;
                        });
                    }
                    Some("modelProviders") => {
                        let out = out.clone();
                        workspace_tasks.spawn(async move {
                            let result = crate::model_providers::execute(&request).await;
                            let mut reply = json!({"type":"reply","request":request["request"]});
                            match result {
                                Ok(value) => reply["result"] = value,
                                Err(error) => reply["error"] = json!(error),
                            }
                            let _ = out.send(Message::text(reply.to_string())).await;
                        });
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
    drop(metrics);
    writer.abort();
    for handle in terminals.values().chain(retiring_terminals.iter()) {
        handle
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
    }
    for mut handle in terminals.into_values().chain(retiring_terminals) {
        while handle.frames.changed().await.is_ok() {}
    }
    // Signal every adapter before waiting, so teardown is bounded per link.
    // Killed/replaced channels still own cleanup tasks until their grace period
    // finishes. Keep the host lock through those tasks as well as active ones.
    for process in processes.values_mut().chain(retiring.iter_mut()) {
        process.stop();
    }
    for mut process in processes.into_values().chain(retiring) {
        let _ = (&mut process.task).await;
    }
    // Keep exclusive host ownership until any Git mutation finishes. A lost reply
    // retains its checkout; a replacement must not race the old worker.
    while workspace_tasks.join_next().await.is_some() {}
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
    cwd: PathBuf,
    finished: tokio::sync::watch::Receiver<bool>,
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
    let cwd = PathBuf::from(request["cwd"].as_str().unwrap_or(""));
    let (completed, finished) = tokio::sync::watch::channel(false);
    let (input, lines) = mpsc::unbounded_channel();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        if let Err(message) = run_process(request, channel, out.clone(), lines, stopped).await {
            let exit = json!({"type":"exit","channel":channel,"message":message});
            let _ = out.send(Message::text(exit.to_string())).await;
        }
        let _ = completed.send(true);
    });
    Process {
        cwd,
        finished,
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
    crate::provider::prepare(provider).map_err(|e| format!("Could not prepare {provider}: {e}"))?;
    // Environment discovery must not block link pings, other channels or kill.
    let env = tokio::select! {
        env = mise_env(&cwd) => env,
        _ = &mut stopped => return Ok(()),
    };
    // Remote sessions always use the host's own sign-ins.
    let config = crate::provider::agent_config(
        provider,
        false,
        request["hostProfile"].as_bool().unwrap_or(false),
    );
    let config = if request["modelConnection"].is_object() {
        if provider != "goose" {
            return Err("Model profiles require the Goose harness".into());
        }
        let connection = serde_json::from_value(request["modelConnection"].clone())
            .map_err(|_| "Invalid model provider connection")?;
        crate::model_providers::configure_connection(config, connection)?
    } else if request["modelProvider"].is_string() {
        return Err("Update the server to use shared model providers".into());
    } else {
        config
    };
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

fn discovered_tools(env: &HashMap<String, String>) -> Value {
    let mut tools = serde_json::Map::new();
    for (provider, override_var) in [
        ("codex", "CODEX_PATH"),
        ("claude", "CLAUDE_CODE_EXECUTABLE"),
        ("opencode", "AGENTWORKS_OPENCODE_PATH"),
    ] {
        let configured = env
            .get(override_var)
            .cloned()
            .or_else(|| std::env::var(override_var).ok());
        let path = resolve(Path::new(configured.as_deref().unwrap_or(provider)), env);
        tools.insert(provider.into(), json!(path));
    }
    Value::Object(tools)
}

struct WorkspaceContext {
    roots: Vec<PathBuf>,
}
impl crate::worktrees::Context for WorkspaceContext {
    fn safe_error(&self, message: &str) -> String {
        crate::provider::safe_error(message)
    }
    fn roots(&self) -> Vec<PathBuf> {
        self.roots.clone()
    }
    fn checked(&self, path: &str) -> Result<PathBuf, (http::StatusCode, String)> {
        let path = check_dir(path)
            .map(PathBuf::from)
            .map_err(|e| (http::StatusCode::BAD_REQUEST, e))?;
        crate::worktrees::allowed(self, &path)?;
        Ok(path)
    }
    fn arm(&self, _key: Option<&str>) -> Result<(), (http::StatusCode, String)> {
        Ok(())
    }
}
async fn workspace_request(request: &Value) -> Result<Value, String> {
    use crate::worktrees::Context;
    let roots = request["roots"]
        .as_array()
        .ok_or("Missing host roots")?
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|r| check_dir(r).ok())
        .map(PathBuf::from)
        .collect();
    let context = WorkspaceContext { roots };
    match request["operation"].as_str() {
        Some("inspectWorktree" | "removeWorktree" | "restoreWorktree") => {
            let owned: crate::worktree_cleanup::Owned =
                serde_json::from_value(request["owned"].clone())
                    .map_err(|_| "Invalid worktree ownership")?;
            match request["operation"].as_str() {
                Some("inspectWorktree") => serde_json::to_value(
                    crate::worktree_cleanup::inspect_in(&context, Some(owned)).await,
                )
                .map_err(|_| "Invalid inspection".into()),
                Some("removeWorktree") => crate::worktree_cleanup::remove_in(
                    &context,
                    &owned,
                    request["fingerprint"].as_str(),
                    request["force"].as_bool().unwrap_or(false),
                )
                .await
                .map_err(|e| e.1),
                _ => crate::worktree_cleanup::restore_in(&context, &owned)
                    .await
                    .map(|result| json!(result))
                    .map_err(|e| e.1),
            }
        }
        Some("list") => {
            let cwd = context
                .checked(request["cwd"].as_str().unwrap_or(""))
                .map_err(|e| e.1)?;
            crate::worktrees::list_in(&context, &cwd)
                .await
                .map_err(|e| e.1)
        }
        Some("prepare") => {
            let cwd = context
                .checked(request["cwd"].as_str().unwrap_or(""))
                .map_err(|e| e.1)?;
            let workspace: Option<crate::worktrees::Workspace> =
                serde_json::from_value(request["workspace"].clone())
                    .map_err(|_| "Invalid workspace")?;
            let policy = serde_json::from_value(request["policy"].clone())
                .map_err(|_| "Invalid workspace policy")?;
            let (cwd, owned) = crate::worktrees::prepare_in(
                &context,
                workspace.as_ref(),
                policy,
                false,
                cwd,
                None,
            )
            .await
            .map_err(|e| e.1)?;
            Ok(json!({"cwd":cwd,"owned":owned}))
        }
        Some("browseDirectories" | "createDirectory") => {
            // `~` is the worker's home; any other path is resolved and contained by
            // the shared directory policy, which does not reveal why a path failed.
            let path = |key: &str| {
                request[key]
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .map(|p| check_dir(p).unwrap_or_else(|_| p.into()))
            };
            let (parent, name) = (path("path"), request["name"].as_str().map(String::from));
            let hidden = request["hidden"].as_bool().unwrap_or(false);
            let create = request["operation"] == "createDirectory";
            tokio::task::spawn_blocking(move || {
                if create {
                    crate::directories::create(
                        &context.roots,
                        parent.as_deref().unwrap_or(""),
                        name.as_deref().unwrap_or(""),
                    )
                    .map(|path| json!({"path":path}))
                } else {
                    crate::directories::browse(&context.roots, parent.as_deref(), hidden)
                }
            })
            .await
            .map_err(|_| "Directory operation failed".to_string())?
            .map_err(|e| e.1)
        }
        _ => Err("Unsupported workspace operation".into()),
    }
}

/// Which providers can actually start here, using the mise environment.
fn available_providers(env: &HashMap<String, String>) -> Vec<&'static str> {
    let interpreter = std::env::var_os("AGENTWORKS_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| "node".into());
    let node = resolve(&interpreter, env).is_some();
    PROVIDERS
        .into_iter()
        .filter(|provider| match *provider {
            "goose" => true,
            "codex-rust" => {
                let executable = env
                    .get("CODEX_PATH")
                    .cloned()
                    .or_else(|| std::env::var("CODEX_PATH").ok())
                    .unwrap_or_else(|| "codex".into());
                resolve(Path::new(&executable), env).is_some()
            }
            "claude-rust" => {
                let executable = env
                    .get("CLAUDE_CODE_EXECUTABLE")
                    .cloned()
                    .or_else(|| std::env::var("CLAUDE_CODE_EXECUTABLE").ok())
                    .unwrap_or_else(|| "claude".into());
                resolve(Path::new(&executable), env).is_some()
            }
            "opencode" => {
                let config = crate::provider::agent_config(provider, false, false);
                resolve(config.command(), env).is_some()
            }
            "demo" => true,
            _ => node && crate::provider::adapter_path(provider).is_file(),
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
    #[test]
    fn native_codex_is_available_without_a_node_interpreter() {
        let env = HashMap::from([
            ("PATH".into(), "/nonexistent".into()),
            (
                "CODEX_PATH".into(),
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]);
        let providers = available_providers(&env);
        assert!(providers.contains(&"codex-rust"));
        assert!(providers.contains(&"goose"));
        assert!(!providers.contains(&"codex"));
        assert!(!providers.contains(&"claude"));
    }
    #[test]
    fn native_claude_is_available_without_a_node_interpreter() {
        let env = HashMap::from([
            ("PATH".into(), "/nonexistent".into()),
            (
                "CLAUDE_CODE_EXECUTABLE".into(),
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]);
        let providers = available_providers(&env);
        assert!(providers.contains(&"claude-rust"));
        assert!(!providers.contains(&"claude"));
    }
    #[tokio::test]
    async fn directory_operations_stay_inside_host_roots() {
        let dir =
            std::env::temp_dir().join(format!("agentworks-worker-dirs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let root = dir.join("root");
        let roots = json!([root]);
        let created = workspace_request(
            &json!({"operation":"createDirectory","roots":roots,"path":root,"name":"thing"}),
        )
        .await
        .unwrap();
        assert_eq!(created["path"], json!(root.join("thing")));
        let listed =
            workspace_request(&json!({"operation":"browseDirectories","roots":roots,"path":root}))
                .await
                .unwrap();
        assert_eq!(listed["entries"][0]["name"], "thing");
        let top = workspace_request(&json!({"operation":"browseDirectories","roots":roots}))
            .await
            .unwrap();
        assert_eq!(top["entries"][0]["path"], json!(root));
        let outside = dir.join("outside");
        assert!(
            workspace_request(
                &json!({"operation":"createDirectory","roots":roots,"path":outside,"name":"x"})
            )
            .await
            .is_err()
        );
        assert!(
            workspace_request(
                &json!({"operation":"browseDirectories","roots":roots,"path":outside})
            )
            .await
            .is_err()
        );
        assert!(!outside.join("x").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
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
