//! SSH supervisors, worker framing, and remote ACP/terminal transports.
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    process::Command,
};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

/// A remote execution host. The server reaches it over SSH and starts the
/// Agentworks worker there; all session state stays on this server. The
/// built-in `local` host is synthesized at read time and is never stored here.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub id: String,
    pub name: String,
    /// Anything `ssh` accepts as a destination: `user@host`, a config alias, or
    /// an `ssh://` URI. Keys, ports and jump hosts come from the SSH config.
    #[serde(default)]
    pub destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_hosts_file: Option<PathBuf>,
    /// Shell command on the remote host that runs the Agentworks binary.
    #[serde(default)]
    pub command: String,
    /// Working directories offered for sessions on this host. Paths are the
    /// host's own; `~/` refers to the remote user's home.
    #[serde(default)]
    pub roots: Vec<String>,
}
pub const DEFAULT_REMOTE_COMMAND: &str = "agentworks";
pub const PROTOCOL: u64 = 3;
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// The worker prints this line on stdout before the WebSocket starts. Anything
/// earlier, such as output from the remote shell's startup files, is skipped.
pub const READY: &str = "agentworks worker ready";
const MAX_PREAMBLE: usize = 64 * 1024;
pub const PROVIDERS: [&str; 7] = [
    "demo",
    "claude",
    "claude-rust",
    "codex",
    "codex-rust",
    "opencode",
    "goose",
];
const PING_EVERY: Duration = Duration::from_secs(15);
const SILENCE_LIMIT: Duration = Duration::from_secs(45);
/// Covers the SSH handshake plus starting the remote binary.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const HELLO_TIMEOUT: Duration = Duration::from_secs(50);
const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const LOG_LINES: usize = 6;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// `connecting`, `connected`, or `offline`.
    pub status: &'static str,
    /// Why the host is offline, including the tail of ssh's output.
    pub detail: String,
    pub since: u64,
    /// What the worker reported in its hello: version, platform, hostname, providers.
    pub worker: Option<Value>,
}

pub struct Hosts {
    links: Mutex<HashMap<String, Link>>,
    /// The generation guards against a just-stopped supervisor writing status
    /// over its replacement.
    tasks: Mutex<HashMap<String, (u64, tokio::task::AbortHandle)>>,
    generation: AtomicU64,
    live: Mutex<HashMap<String, Arc<Live>>>,
    ssh: String,
}

static REGISTRY: std::sync::OnceLock<Arc<Hosts>> = std::sync::OnceLock::new();
/// The server's hosts, for code that only has a session in hand.
pub fn registry() -> Option<&'static Arc<Hosts>> {
    REGISTRY.get()
}

impl Hosts {
    /// Makes these hosts reachable through `registry()`; called once at startup.
    pub fn install(self: &Arc<Self>) {
        let _ = REGISTRY.set(self.clone());
    }
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            links: Default::default(),
            tasks: Default::default(),
            generation: AtomicU64::new(0),
            live: Default::default(),
            ssh: std::env::var("AGENTWORKS_SSH").unwrap_or_else(|_| "ssh".into()),
        })
    }
    /// Starts (or restarts) the supervisor that keeps this host connected.
    pub fn start(self: &Arc<Self>, host: Host) {
        self.stop(&host.id);
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let mut tasks = self.tasks.lock().unwrap();
        self.write_link(&host.id, "connecting", String::new(), None);
        let task = tokio::spawn(self.clone().supervise(host.clone(), generation));
        tasks.insert(host.id, (generation, task.abort_handle()));
    }
    /// Aborting the supervisor drops its ssh child, which is killed on drop.
    pub fn stop(&self, id: &str) {
        if let Some((_, task)) = self.tasks.lock().unwrap().remove(id) {
            task.abort();
        }
        self.links.lock().unwrap().remove(id);
        if let Some(live) = self.live.lock().unwrap().remove(id) {
            live.close("The host was disconnected.");
        }
    }
    pub fn stop_all(&self) {
        for (_, live) in self.live.lock().unwrap().drain() {
            live.close("The server is stopping.");
        }
        for (_, (_, task)) in self.tasks.lock().unwrap().drain() {
            task.abort();
        }
    }
    pub fn link(&self, id: &str) -> Link {
        self.links.lock().unwrap().get(id).cloned().unwrap_or(Link {
            status: "offline",
            detail: "Not started.".into(),
            since: 0,
            worker: None,
        })
    }
    fn write_link(&self, id: &str, status: &'static str, detail: String, worker: Option<Value>) {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.links.lock().unwrap().insert(
            id.into(),
            Link {
                status,
                detail,
                since,
                worker,
            },
        );
    }
    /// The connected worker for a host, if its link is up.
    pub fn live(&self, id: &str) -> Option<Arc<Live>> {
        self.live.lock().unwrap().get(id).cloned()
    }
    fn set_live(&self, id: &str, generation: u64, live: Option<Arc<Live>>) {
        let tasks = self.tasks.lock().unwrap();
        if tasks.get(id).is_some_and(|(g, _)| *g == generation) {
            let mut map = self.live.lock().unwrap();
            match live {
                Some(live) => map.insert(id.into(), live),
                None => map.remove(id),
            };
        }
    }
    fn set(
        &self,
        id: &str,
        generation: u64,
        status: &'static str,
        detail: String,
        worker: Option<Value>,
    ) {
        let tasks = self.tasks.lock().unwrap();
        if tasks.get(id).is_some_and(|(g, _)| *g == generation) {
            self.write_link(id, status, detail, worker);
        }
    }

    async fn supervise(self: Arc<Self>, host: Host, generation: u64) {
        let mut backoff = MIN_BACKOFF;
        let mut last_failure = String::new();
        loop {
            // Keep the previous failure visible while retrying.
            self.set(
                &host.id,
                generation,
                "connecting",
                last_failure.clone(),
                None,
            );
            let started = Instant::now();
            let reason = self.connect_once(&host, generation).await;
            // A link that stayed up for a while was healthy; retry promptly.
            if started.elapsed() > MAX_BACKOFF {
                backoff = MIN_BACKOFF;
            }
            self.set(
                &host.id,
                generation,
                "offline",
                format!("{reason} Retrying in {}s.", backoff.as_secs()),
                None,
            );
            last_failure = reason;
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Runs one ssh session for its whole life and returns why it ended.
    async fn connect_once(&self, host: &Host, generation: u64) -> String {
        let mut child = match Command::new(&self.ssh)
            .args(ssh_args(host))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return format!("Could not run {}: {e}.", self.ssh),
        };
        // `Child::wait` closes stdin before waiting, and we poll it to notice
        // ssh exiting, so the link's ends must live outside the child handle.
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return "Could not attach to ssh's input and output.".into();
        };
        let log: Arc<Mutex<VecDeque<String>>> = Default::default();
        collect(child.stderr.take(), log.clone());
        let tail = |log: &Arc<Mutex<VecDeque<String>>>| {
            let lines = log.lock().unwrap();
            if lines.is_empty() {
                String::new()
            } else {
                format!(
                    " Last output: {}",
                    lines.iter().cloned().collect::<Vec<_>>().join(" / ")
                )
            }
        };
        let exited = |status: io::Result<std::process::ExitStatus>| async {
            // Give the output readers a moment to capture ssh's last words.
            tokio::time::sleep(Duration::from_millis(200)).await;
            match status {
                Ok(status) => {
                    let output = tail(&log);
                    format!(
                        "ssh exited ({status}).{output}{}",
                        if host.identity_file.is_some() {
                            ""
                        } else {
                            auth_hint(&output)
                        }
                    )
                }
                Err(e) => format!("ssh failed: {e}."),
            }
        };

        let mut stdout = BufReader::new(stdout);
        tokio::select! {
            found = tokio::time::timeout(CONNECT_TIMEOUT, skip_preamble(&mut stdout, &log)) => match found {
                Ok(Ok(())) => {}
                // Usually ssh is exiting; its status and stderr say why.
                Ok(Err(reason)) => return match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                    Ok(status) => exited(status).await,
                    Err(_) => format!("{reason}{}", tail(&log)),
                },
                Err(_) => return format!("The worker did not start within {}s.{}", CONNECT_TIMEOUT.as_secs(), tail(&log)),
            },
            status = child.wait() => return exited(status).await,
        }
        // Any bytes buffered past the ready line belong to the WebSocket.
        let stream = tokio::io::join(stdout, stdin);

        let mut ws = match tokio::time::timeout(
            HELLO_TIMEOUT,
            tokio_tungstenite::accept_async_with_config(stream, Some(ws_config())),
        )
        .await
        {
            Ok(Ok(ws)) => ws,
            Ok(Err(e)) => return format!("WebSocket handshake failed: {e}."),
            Err(_) => return "WebSocket handshake timed out.".into(),
        };
        let hello = match tokio::time::timeout(HELLO_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                serde_json::from_str::<Value>(&text).unwrap_or(Value::Null)
            }
            _ => return "The worker did not introduce itself.".into(),
        };
        let worker = match check_hello(&hello) {
            Ok(worker) => worker,
            Err(message) => {
                let _ = ws
                    .send(Message::text(
                        json!({"type":"error","message":message}).to_string(),
                    ))
                    .await;
                let _ = ws.close(None).await;
                return message;
            }
        };
        if ws
            .send(Message::text(
                json!({"type":"welcome","protocol":PROTOCOL,"hostId":host.id}).to_string(),
            ))
            .await
            .is_err()
        {
            return "Connection lost during the handshake.".into();
        }

        // A replacement must own the host lock before any sessions may use it.
        // The previous worker retains that lock throughout process cleanup.
        match tokio::time::timeout(HELLO_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(Message::Text(text))))
                if serde_json::from_str::<Value>(&text)
                    .is_ok_and(|v| v["type"] == "ready" && v["protocol"] == PROTOCOL) => {}
            _ => return "The worker did not confirm exclusive host ownership.".into(),
        }

        // One writer owns the socket; sessions and pings queue frames for it.
        let (mut sink, mut frames) = ws.split();
        let (out, mut outgoing) = mpsc::channel::<Message>(256);
        let writer = tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let live = Arc::new(Live::new(out.clone(), worker.clone()));
        let _connection = ConnectionGuard {
            live: live.clone(),
            writer,
        };
        self.set_live(&host.id, generation, Some(live.clone()));
        self.set(
            &host.id,
            generation,
            "connected",
            String::new(),
            Some(worker),
        );

        let mut last_heard = Instant::now();
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let ended: Result<String, io::Result<std::process::ExitStatus>> = loop {
            tokio::select! {
                frame = frames.next() => match frame {
                    None | Some(Ok(Message::Close(_))) => break Ok("The worker closed the connection.".into()),
                    Some(Err(e)) => break Ok(format!("Connection error: {e}.")),
                    Some(Ok(message)) => {
                        last_heard = Instant::now();
                        live.receive(message);
                    }
                },
                status = child.wait() => break Err(status),
                _ = ping.tick() => {
                    if last_heard.elapsed() > SILENCE_LIMIT {
                        break Ok("The worker stopped responding.".into());
                    }
                    if out.try_send(Message::Ping(Default::default())).is_err() {
                        break Ok("Connection lost.".into());
                    }
                }
            }
        };
        // Sessions on this host see their streams end, as after a server restart.
        self.set_live(&host.id, generation, None);
        live.close("The host disconnected.");
        match ended {
            Err(status) => exited(status).await,
            // If ssh is exiting too, its output usually says why.
            Ok(reason) => {
                match tokio::time::timeout(Duration::from_millis(500), child.wait()).await {
                    Ok(status) => format!("{reason} {}", exited(status).await),
                    Err(_) => reason,
                }
            }
        }
    }
}

/// Also runs when a reconnect or shutdown aborts the supervisor future.
struct ConnectionGuard {
    live: Arc<Live>,
    writer: tokio::task::JoinHandle<()>,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.live.close("The host disconnected.");
        self.writer.abort();
    }
}

/// Generous limits: one ACP line, such as a replayed transcript or an image,
/// travels as one frame.
pub fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
}

/// A connected worker, as seen by the rest of the server. Each remote process
/// gets a channel number; its ACP lines travel as binary frames prefixed with
/// that number, and control messages travel as JSON text frames.
pub struct Live {
    closed: AtomicBool,
    out: mpsc::Sender<Message>,
    channels: Mutex<HashMap<u32, UnboundedSender<io::Result<String>>>>,
    next_channel: AtomicU32,
    terminals: Mutex<HashMap<u32, tokio::sync::watch::Sender<crate::terminal::Frame>>>,
    requests: Mutex<HashMap<u64, oneshot::Sender<Result<String, String>>>>,
    next_request: AtomicU64,
    worker: Value,
}

impl Live {
    /// Capabilities and host metadata advertised by this worker at connection time.
    pub fn worker(&self) -> &Value {
        &self.worker
    }

    fn new(out: mpsc::Sender<Message>, worker: Value) -> Self {
        Self {
            closed: AtomicBool::new(false),
            out,
            channels: Default::default(),
            next_channel: AtomicU32::new(0),
            terminals: Default::default(),
            requests: Default::default(),
            next_request: AtomicU64::new(0),
            worker,
        }
    }
    pub fn has_provider(&self, provider: &str) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.worker["providers"]
                .as_array()
                .is_some_and(|p| p.iter().any(|p| p == provider))
    }
    fn receive(&self, message: Message) {
        match message {
            Message::Binary(data) if data.len() >= 4 => {
                let channel = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                let line = String::from_utf8_lossy(&data[4..]).into_owned();
                if let Some(tx) = self.channels.lock().unwrap().get(&channel) {
                    let _ = tx.unbounded_send(Ok(line));
                }
            }
            Message::Text(text) => {
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    return;
                };
                match value["type"].as_str() {
                    Some("terminalFrame") => {
                        let channel = value["channel"].as_u64().unwrap_or(0) as u32;
                        if let Ok(mut frame) =
                            serde_json::from_value::<crate::terminal::Frame>(value["frame"].clone())
                        {
                            let mut terminals = self.terminals.lock().unwrap();
                            if let Some(tx) = terminals.get(&channel) {
                                let Some(history) = frame.history.restore(&tx.borrow().history)
                                else {
                                    return;
                                };
                                frame.history = history;
                                tx.send_replace(frame.clone());
                            }
                            if frame.exited {
                                terminals.remove(&channel);
                            }
                        }
                    }
                    Some("exit") => {
                        let channel = value["channel"].as_u64().unwrap_or(0) as u32;
                        if let Some(tx) = self.channels.lock().unwrap().remove(&channel) {
                            let message = value["message"].as_str().unwrap_or("");
                            if !message.is_empty() {
                                let _ =
                                    tx.unbounded_send(Err(io::Error::other(message.to_string())));
                            }
                        }
                    }
                    Some("reply") => {
                        let id = value["request"].as_u64().unwrap_or(0);
                        if let Some(tx) = self.requests.lock().unwrap().remove(&id) {
                            let _ = tx.send(match value["error"].as_str() {
                                Some(error) => Err(error.to_string()),
                                None => Ok(if value.get("result").is_some() {
                                    value["result"].to_string()
                                } else {
                                    value["path"].as_str().unwrap_or("").to_string()
                                }),
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    fn close(&self, reason: &str) {
        self.closed.store(true, Ordering::Release);
        for (_, tx) in self.channels.lock().unwrap().drain() {
            let _ = tx.unbounded_send(Err(io::Error::other(reason.to_string())));
        }
        for (_, tx) in self.terminals.lock().unwrap().drain() {
            tx.send_modify(|frame| {
                frame.seq += 1;
                frame.exited = true;
                frame.error = Some(reason.to_string());
                frame.delta.clear();
            });
        }
        for (_, tx) in self.requests.lock().unwrap().drain() {
            let _ = tx.send(Err(reason.to_string()));
        }
    }
    pub fn home_terminal(self: &Arc<Self>) -> Result<crate::terminal::Handle, String> {
        let home = self.worker["home"]
            .as_str()
            .filter(|home| home.starts_with('/'))
            .ok_or_else(|| "Update the remote worker to enable host terminals.".to_string())?;
        self.terminal(home)
    }
    pub fn terminal(self: &Arc<Self>, cwd: &str) -> Result<crate::terminal::Handle, String> {
        if self.worker["terminal"] != true {
            return Err("Update the remote worker to enable terminals.".into());
        }
        let channel = self.next_channel.fetch_add(1, Ordering::Relaxed) + 1;
        let (frames, receiver) = tokio::sync::watch::channel(crate::terminal::Frame {
            cols: 80,
            rows: 24,
            ..Default::default()
        });
        {
            let mut terminals = self.terminals.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Err("The host disconnected.".into());
            }
            terminals.insert(channel, frames);
        }
        if self
            .out
            .try_send(Message::text(
                json!({"type":"terminalOpen","channel":channel,"cwd":cwd}).to_string(),
            ))
            .is_err()
        {
            self.terminals.lock().unwrap().remove(&channel);
            return Err("The host disconnected.".into());
        }
        let (input, mut incoming) = mpsc::channel::<crate::terminal::Input>(64);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let live = self.clone();
        let mut closed = receiver.clone();
        tokio::spawn(async move {
            let mut check = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    _ = check.tick() => {if stop.load(Ordering::Acquire) || live.closed.load(Ordering::Acquire) {break;}},
                    input = incoming.recv() => {
                        let Some(input) = input else {break;};
                        if live.out.try_send(Message::text(json!({"type":"terminalInput","channel":channel,"input":input}).to_string())).is_err() {break;}
                    }
                }
            }
            let _ = live.out.try_send(Message::text(
                json!({"type":"terminalClose","channel":channel}).to_string(),
            ));
            // Keep the link alive until the worker confirms PTY cleanup. The
            // timeout also covers a blocked worker before host teardown.
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                while closed.changed().await.is_ok() {}
            })
            .await;
            if let Some(tx) = live.terminals.lock().unwrap().remove(&channel) {
                tx.send_modify(|frame| {
                    frame.seq += 1;
                    frame.exited = true;
                    frame.error = Some("Terminal connection closed".into());
                    frame.delta.clear();
                });
            }
        });
        Ok(crate::terminal::Handle {
            input,
            frames: receiver,
            stopped,
        })
    }
    /// Asks the worker to resolve a directory on its machine, returning the
    /// canonical path.
    pub async fn check_dir(&self, path: &str) -> Result<String, String> {
        self.request(json!({"type":"checkDir","path":path})).await
    }
    pub async fn workspace(&self, mut request: Value) -> Result<Value, String> {
        if self.worker["workspace"] != true {
            return Err("Update the worker on this host to enable remote worktrees.".into());
        }
        request["type"] = json!("workspace");
        let result = self.request(request).await?;
        serde_json::from_str(&result).map_err(|_| "Invalid workspace reply".into())
    }
    async fn request(&self, mut request: Value) -> Result<String, String> {
        // Lifecycle operations perform several bounded Git checks plus adapter
        // shutdown. Directory validation retains its shorter deadline.
        let timeout = if request["type"] == "workspace" {
            Duration::from_secs(120)
        } else {
            Duration::from_secs(15)
        };
        let id = self.next_request.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        {
            let mut requests = self.requests.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Err("The host disconnected.".into());
            }
            requests.insert(id, tx);
        }
        let _pending = DirectoryRequest { live: self, id };
        request["request"] = json!(id);
        let request = request.to_string();
        if self.out.send(Message::text(request)).await.is_err() {
            self.requests.lock().unwrap().remove(&id);
            return Err("The host disconnected.".into());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            _ => {
                self.requests.lock().unwrap().remove(&id);
                Err("The host did not answer.".into())
            }
        }
    }
    /// Starts an agent adapter on the worker. The returned transport carries
    /// its ACP lines; dropping the guard stops the remote process.
    pub fn spawn(
        self: &Arc<Self>,
        provider: &str,
        cwd: &str,
        host_profile: bool,
    ) -> Result<(RemoteLines, Spawned), String> {
        let channel = self.next_channel.fetch_add(1, Ordering::Relaxed) + 1;
        let (incoming_tx, incoming) = unbounded();
        {
            let mut channels = self.channels.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Err("The host disconnected.".into());
            }
            channels.insert(channel, incoming_tx);
        }
        let spawn = json!({"type":"spawn","channel":channel,"provider":provider,"cwd":cwd,"hostProfile":host_profile});
        if self.out.try_send(Message::text(spawn.to_string())).is_err() {
            self.channels.lock().unwrap().remove(&channel);
            return Err("The host disconnected.".into());
        }
        let (outgoing, mut lines) = futures::channel::mpsc::channel::<String>(64);
        let out = self.out.clone();
        let forward = tokio::spawn(async move {
            while let Some(line) = lines.next().await {
                let mut frame = Vec::with_capacity(line.len() + 4);
                frame.extend_from_slice(&channel.to_be_bytes());
                frame.extend_from_slice(line.as_bytes());
                if out.send(Message::binary(frame)).await.is_err() {
                    break;
                }
            }
        });
        let outgoing: RemoteSink = Box::pin(outgoing.sink_map_err(io::Error::other));
        Ok((
            agent_client_protocol::Lines::new(outgoing, incoming),
            Spawned {
                live: self.clone(),
                channel,
                forward,
            },
        ))
    }
}

/// An API timeout/cancellation must not leave an unanswered request registered.
struct DirectoryRequest<'a> {
    live: &'a Live,
    id: u64,
}
impl Drop for DirectoryRequest<'_> {
    fn drop(&mut self) {
        self.live.requests.lock().unwrap().remove(&self.id);
    }
}

type RemoteSink = std::pin::Pin<Box<dyn futures::Sink<String, Error = io::Error> + Send>>;
pub type RemoteLines =
    agent_client_protocol::Lines<RemoteSink, UnboundedReceiver<io::Result<String>>>;

/// Keeps a remote process alive; dropping it asks the worker to stop it.
pub struct Spawned {
    live: Arc<Live>,
    channel: u32,
    forward: tokio::task::JoinHandle<()>,
}
impl Drop for Spawned {
    fn drop(&mut self) {
        self.forward.abort();
        if self
            .live
            .channels
            .lock()
            .unwrap()
            .remove(&self.channel)
            .is_some()
        {
            let kill = json!({"type":"kill","channel":self.channel}).to_string();
            if let Err(mpsc::error::TrySendError::Full(message)) =
                self.live.out.try_send(Message::text(kill))
            {
                let out = self.live.out.clone();
                tokio::spawn(async move {
                    let _ = out.send(message).await;
                });
            }
        }
    }
}

fn ssh_args(host: &Host) -> Vec<String> {
    let mut args: Vec<String> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ConnectTimeout=20",
    ]
    .map(String::from)
    .into();
    args.extend(managed_ssh_args(host));
    args.push("--".into());
    args.push(host.destination.clone());
    // ssh hands this line to the remote user's shell. The command is operator
    // configuration.
    args.push(format!("{} agent --stdio", host.command));
    args
}

pub fn known_hosts_option(path: &std::path::Path) -> String {
    // -o values are parsed as ssh_config syntax, even though no shell is used.
    let path = path.to_string_lossy().replace('%', "%%");
    format!(
        "UserKnownHostsFile={}",
        serde_json::to_string(&path).unwrap()
    )
}

pub fn managed_ssh_args(host: &Host) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(identity) = &host.identity_file {
        args.extend([
            "-i".into(),
            identity.to_string_lossy().into_owned(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "IdentityAgent=none".into(),
        ]);
    }
    if let Some(known) = &host.known_hosts_file {
        args.extend([
            "-o".into(),
            known_hosts_option(known),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
        ]);
    }
    args
}

/// A service often starts without the login session's SSH agent, which makes
/// passphrase-protected keys unusable and ssh's own message unhelpful.
fn auth_hint(output: &str) -> &'static str {
    let agent = std::env::var_os("SSH_AUTH_SOCK").is_some_and(|s| !s.is_empty());
    if output.contains("Permission denied") && !agent {
        " This server has no SSH agent (SSH_AUTH_SOCK is not set), so passphrase-protected keys cannot be used. See Remote hosts in the README."
    } else {
        ""
    }
}

/// Validates the worker's hello and keeps only the fields the UI shows.
fn check_hello(hello: &Value) -> Result<Value, String> {
    if hello.get("type").and_then(Value::as_str) != Some("hello") {
        return Err("The worker sent an unexpected first message.".into());
    }
    let protocol = hello.get("protocol").and_then(Value::as_u64).unwrap_or(0);
    if protocol != PROTOCOL {
        return Err(format!(
            "The worker speaks protocol {protocol}; this server needs {PROTOCOL}. Update the Agentworks binary on the host."
        ));
    }
    let text = |key: &str, max: usize| {
        hello
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(max)
            .collect::<String>()
    };
    let providers: Vec<&str> = hello
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|p| PROVIDERS.iter().copied().find(|known| *known == p))
        .collect();
    Ok(json!({
        "version": text("version", 40),
        "platform": text("platform", 40),
        "hostname": text("hostname", 80),
        "providers": providers,
        "tools": hello["tools"],
        "workspace": hello["workspace"] == true,
        "worktreeCleanup": hello["worktreeCleanup"] == true,
        "terminal": hello["terminal"].as_bool().unwrap_or(false),
        "home": text("home", 4096),
    }))
}

fn push_line(log: &Mutex<VecDeque<String>>, line: &str) {
    let line: String = line.trim().chars().take(300).collect();
    if line.is_empty() {
        return;
    }
    let mut log = log.lock().unwrap();
    if log.len() == LOG_LINES {
        log.pop_front();
    }
    log.push_back(line);
}

fn collect(
    output: Option<impl AsyncRead + Unpin + Send + 'static>,
    log: Arc<Mutex<VecDeque<String>>>,
) {
    let Some(output) = output else { return };
    tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            push_line(&log, &line);
        }
    });
}

/// Reads ssh's stdout up to the worker's ready line, keeping what came before
/// for diagnostics.
async fn skip_preamble(
    stdout: &mut (impl AsyncBufRead + Unpin),
    log: &Arc<Mutex<VecDeque<String>>>,
) -> Result<(), String> {
    let mut seen = 0;
    loop {
        let mut line = Vec::new();
        let read = (&mut *stdout)
            .take((MAX_PREAMBLE - seen + 1) as u64)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| format!("Reading from ssh failed: {e}."))?;
        if read == 0 {
            return Err("ssh closed the connection before the worker started.".into());
        }
        seen += read;
        if seen > MAX_PREAMBLE {
            return Err("Too much output before the worker started.".into());
        }
        let text = String::from_utf8_lossy(&line);
        if text.trim_end() == READY {
            return Ok(());
        }
        push_line(log, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn aborting_a_connection_closes_sessions_and_pending_directory_checks() {
        let (out, _rx) = mpsc::channel(8);
        let live = Arc::new(Live::new(out, json!({"providers":["demo"]})));
        let (channel_tx, mut channel_rx) = unbounded();
        live.channels.lock().unwrap().insert(1, channel_tx);
        let (request_tx, request_rx) = oneshot::channel();
        live.requests.lock().unwrap().insert(1, request_tx);
        let writer = tokio::spawn(std::future::pending::<()>());
        let aborted = writer.abort_handle();
        drop(ConnectionGuard {
            live: live.clone(),
            writer,
        });
        assert!(!live.has_provider("demo"));
        assert!(live.spawn("demo", "/remote", false).is_err());
        assert!(
            live.check_dir("/remote")
                .await
                .unwrap_err()
                .contains("disconnected")
        );
        assert!(
            channel_rx
                .next()
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("disconnected")
        );
        assert!(channel_rx.next().await.is_none());
        assert!(
            request_rx
                .await
                .unwrap()
                .unwrap_err()
                .contains("disconnected")
        );
        tokio::task::yield_now().await;
        assert!(aborted.is_finished());
    }

    #[tokio::test]
    async fn cancelling_directory_validation_removes_its_pending_request() {
        let (out, mut frames) = mpsc::channel(8);
        let live = Arc::new(Live::new(out, json!({"providers":["demo"]})));
        let checking = live.clone();
        let task = tokio::spawn(async move { checking.check_dir("/remote/path").await });
        frames.recv().await.unwrap();
        assert_eq!(live.requests.lock().unwrap().len(), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(live.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn older_workers_reject_worktrees_without_sending_an_unknown_request() {
        let (out, mut frames) = mpsc::channel(8);
        let live = Live::new(out, json!({"providers":["demo"]}));
        assert!(
            live.workspace(json!({"operation":"list"}))
                .await
                .unwrap_err()
                .contains("Update the worker")
        );
        assert!(frames.try_recv().is_err());
        assert!(live.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn hello_preserves_tool_paths_and_explicit_workspace_support() {
        let worker = check_hello(&json!({"type":"hello","protocol":PROTOCOL,"providers":["codex"],"tools":{"codex":"/remote/bin/codex"},"workspace":true,"worktreeCleanup":true})).unwrap();
        assert_eq!(worker["tools"]["codex"], "/remote/bin/codex");
        assert_eq!(worker["workspace"], true);
        assert_eq!(worker["worktreeCleanup"], true);
        let old =
            check_hello(&json!({"type":"hello","protocol":PROTOCOL,"providers":["demo"]})).unwrap();
        assert_eq!(old["workspace"], false);
        assert_eq!(old["worktreeCleanup"], false);
    }

    #[test]
    fn ssh_command_runs_the_worker_over_stdio_and_cannot_take_options_from_the_destination() {
        let host = Host {
            id: "h".into(),
            name: "Box".into(),
            destination: "dev@box".into(),
            identity_file: None,
            known_hosts_file: None,
            command: "~/bin/agentworks".into(),
            roots: Vec::new(),
        };
        let args = ssh_args(&host);
        let dash = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &args[dash + 1..],
            ["dev@box", "~/bin/agentworks agent --stdio"]
        );
        assert!(!args.contains(&"-R".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
    }

    #[tokio::test]
    async fn preamble_is_skipped_and_logged_and_bytes_after_ready_are_kept() {
        let log: Arc<Mutex<VecDeque<String>>> = Default::default();
        let mut input: &[u8] = b"Welcome to box\n\nagentworks worker ready\nGET /worker";
        skip_preamble(&mut input, &log).await.unwrap();
        assert_eq!(input, b"GET /worker");
        assert_eq!(
            log.lock().unwrap().iter().collect::<Vec<_>>(),
            ["Welcome to box"]
        );
        let mut closed: &[u8] = b"bash: agentworks: command not found\n";
        assert!(
            skip_preamble(&mut closed, &log)
                .await
                .unwrap_err()
                .contains("before the worker started")
        );
        let noise = vec![b'x'; MAX_PREAMBLE + 10];
        let mut flood: &[u8] = &[noise.as_slice(), b"\n"].concat();
        assert!(
            skip_preamble(&mut flood, &log)
                .await
                .unwrap_err()
                .contains("Too much output")
        );
        assert_eq!(
            flood.len(),
            10,
            "Preamble reads must stop at the byte limit even without a newline"
        );
    }

    #[test]
    fn hello_requires_matching_protocol_and_drops_unknown_providers() {
        assert!(check_hello(&json!({"type":"welcome"})).is_err());
        let old = check_hello(&json!({"type":"hello","protocol":0})).unwrap_err();
        assert!(old.contains("Update the Agentworks binary"), "{old}");
        let worker = check_hello(
            &json!({"type":"hello","protocol":PROTOCOL,"version":"0.1.0","platform":"linux-x86_64",
            "hostname":"box","providers":["demo","claude","rm -rf"]}),
        )
        .unwrap();
        assert_eq!(worker["providers"], json!(["demo", "claude"]));
        assert_eq!(worker["hostname"], "box");
    }
}
