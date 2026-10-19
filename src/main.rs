mod automations;
mod browser_origin;
mod catalog;
mod communication;
mod completion;
mod diff;
mod fork;
mod git_context;
mod inbox_delivery;
mod input_queue;
mod lifecycle;
mod limit_resume;
mod mcp;
mod notifications;
mod permission_modes;
mod runtime;
mod service_restart;
mod store;
mod timeouts;
mod workspace;
mod worktrees;
use catalog::{Catalog, Settings};

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

const MAX_EVENTS: usize = 3000;
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
type ApiResult = Result<Json<Value>, (StatusCode, String)>;

#[derive(Clone)]
struct App {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    roots: Arc<Vec<PathBuf>>,
    port: u16,
    bind: IpAddr,
    catalog: Arc<Mutex<Catalog>>,
    discovery: Arc<tokio::sync::Semaphore>,
    store: Arc<Mutex<store::Store>>,
    run_lock: Arc<tokio::sync::Mutex<()>>,
    // Last async lock: communication -> run -> workspace -> short std guards.
    workspace_lock: Arc<tokio::sync::Mutex<()>>,
    communication_lock: Arc<tokio::sync::Mutex<()>>,
    mcp_endpoint: std::net::SocketAddr,
    restart: Arc<service_restart::Manager>,
    /// U-02 cross-session stream of noteworthy events.
    notifications: notifications::Hub,
}

struct Session {
    id: String,
    provider: String,
    cwd: PathBuf,
    title: String,
    initial_model: Option<String>,
    created: u64,
    host_id: String,
    location_id: String,
    data: Mutex<SessionData>,
    tx: broadcast::Sender<Value>,
    commands: Mutex<Option<mpsc::Sender<runtime::Command>>>,
    store: Arc<Mutex<store::Store>>,
    approvals: Mutex<HashMap<String, Approval>>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    mcp_endpoint: std::net::SocketAddr,
    mcp_token: Mutex<String>,
    mcp_refresh_pending: std::sync::atomic::AtomicBool,
    /// U-02; cloned from the app hub so `emit` needs no global state.
    notifications: notifications::Hub,
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionData {
    status: String,
    #[serde(default)]
    inbox_turn: Option<inbox_delivery::Turn>,
    #[serde(default)]
    lifecycle: lifecycle::Metadata,
    seq: u64,
    #[serde(skip)]
    events: VecDeque<Value>,
    #[serde(skip)]
    bytes: usize,
    requests: HashSet<String>,
    models: Value,
    #[serde(default)]
    permission_modes: Value,
    // Captured at creation, never inferred for restored/imported sessions.
    // A native session ID prevents this default from overriding reconnect state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial_claude_permission_mode: Option<permission_modes::ClaudeDefault>,
    available_commands: Value,
    turn_active: bool,
    project_id: Option<String>,
    provider_session_id: Option<String>,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    import_pending: bool,
    #[serde(default)]
    opencode_host: bool,
    #[serde(default)]
    automation: Option<automations::Origin>,
    #[serde(default)]
    queued_prompt: Option<String>,
    #[serde(default)]
    control_origin: Option<communication::ControlOrigin>,
    #[serde(skip)]
    storage_error: Option<String>,
    #[serde(default)]
    continuation: limit_resume::State,
    /// L-07 per-session override; None uses the saved default at turn launch.
    #[serde(default)]
    timeouts: Option<timeouts::Policy>,
    /// L-05 explicit human queued inputs (distinct from `queued_prompt`).
    #[serde(default)]
    input_queue: input_queue::Queue,
    /// L-08 fork lineage; empty for a session that was not forked.
    #[serde(default)]
    lineage: fork::Lineage,
    /// L-08 fork context held until the first prompt hands it to the harness.
    #[serde(default)]
    carried_context: Option<String>,
}

struct Approval {
    request: Value,
    answer: oneshot::Sender<Option<String>>,
}

impl Session {
    fn emit(&self, kind: &str, mut data: Value) {
        fn redact(value: &mut Value, token: &str) {
            match value {
                Value::String(s) => *s = s.replace(token, "[redacted]"),
                Value::Array(a) => {
                    for v in a {
                        redact(v, token)
                    }
                }
                Value::Object(o) => {
                    for v in o.values_mut() {
                        redact(v, token)
                    }
                }
                _ => {}
            }
        }
        redact(&mut data, &self.mcp_token.lock().unwrap());
        let mut state = self.data.lock().unwrap();
        if kind == "status" {
            // A concurrent worker may have advanced state before this event acquired
            // the lock. Publish the actual state rather than an obsolete transition.
            data["status"] = json!(state.status);
            data["explicitlySuspended"] = json!(state.lifecycle.suspended);
        }
        if let Some(origin) = state.automation.as_mut().filter(|a| a.outcome.is_none()) {
            origin.outcome = match kind {
                "turn_end" => Some(
                    if data["timedOut"].is_string() {
                        "timed_out"
                    } else if data["stopReason"] == "cancelled" {
                        "cancelled"
                    } else {
                        "completed"
                    }
                    .into(),
                ),
                "error" => Some(
                    if data["timedOut"].is_string() {
                        "timed_out"
                    } else {
                        "failed"
                    }
                    .into(),
                ),
                "interrupted" => Some("interrupted".into()),
                _ => None,
            };
        }
        if matches!(
            kind,
            "continuation" | "user" | "status" | "completion_reported"
        ) {
            data["continuationState"] =
                serde_json::to_value(&state.continuation).unwrap_or(Value::Null);
        }
        state.seq += 1;
        let event = json!({"seq":state.seq,"kind":kind,"data":data});
        // U-02: fan out noteworthy events to connected browsers.
        if let Some(category) = notifications::classify(kind, &event["data"]) {
            let (title, project_id) = (
                state
                    .lifecycle
                    .title
                    .clone()
                    .unwrap_or_else(|| self.title.clone()),
                state.project_id.clone(),
            );
            self.notifications.publish(
                &notifications::Context {
                    session_id: &self.id,
                    title: &title,
                    provider: &self.provider,
                    project_id: project_id.as_deref(),
                },
                state.seq,
                category,
                &event["data"],
            );
        }
        state.bytes += event.to_string().len();
        state.events.push_back(event.clone());
        while state.events.len() > MAX_EVENTS || state.bytes > MAX_EVENT_BYTES {
            if let Some(old) = state.events.pop_front() {
                state.bytes -= old.to_string().len();
            }
        }
        if let Err(error) = self.store.lock().unwrap().event(&self.id, &state, &event) {
            if state.storage_error.is_none() {
                eprintln!(
                    "Could not persist session {}: {}",
                    self.id,
                    runtime::safe_error(&error.to_string())
                );
            }
            state.storage_error = Some(
                "Session could not be saved to disk. Check server storage before continuing."
                    .into(),
            );
            state.status = "error".into();
        }
        let _ = self.tx.send(event);
        if let Some(message) = &state.storage_error {
            let _ = self
                .tx
                .send(json!({"kind":"storage_error","seq":state.seq,"data":{"message":message}}));
        }
    }
    fn status(&self, status: &str) {
        {
            let mut data = self.data.lock().unwrap();
            data.status = if data.storage_error.is_some() {
                "error".into()
            } else {
                status.into()
            };
            if matches!(status, "idle" | "error" | "closed" | "suspended") {
                data.turn_active = false;
                data.inbox_turn = None;
            }
        }
        self.emit("status", json!({"status":status}));
    }
    fn meta(&self) -> Value {
        let data = self.data.lock().unwrap();
        // Display name of the current model for the session browser.
        let current = data.models["currentValue"]
            .as_str()
            .or(self.initial_model.as_deref());
        let model = current.map(|value| {
            data.models["options"]
                .as_array()
                .and_then(|o| o.iter().find(|o| o["value"] == value))
                .and_then(|o| o["name"].as_str())
                .unwrap_or(value)
                .to_owned()
        });
        json!({"id":self.id,"provider":self.provider,"cwd":self.cwd,"title":data.lifecycle.title.as_deref().unwrap_or(&self.title),"model":model,
            "archived":data.lifecycle.archived,"explicitlySuspended":data.lifecycle.suspended,"created":self.created,"status":data.status,"turnRunning":data.turn_active,
            "projectId":data.project_id,"hostId":self.host_id,"locationId":self.location_id,"automation":data.automation,"initiatedBy":data.control_origin,"continuation":data.continuation,"fork":data.lineage})
    }
    fn snapshot(&self) -> Value {
        let approvals = self.approvals.lock().unwrap();
        let data = self.data.lock().unwrap();
        json!({"kind":"snapshot","seq":data.seq,"session":{"id":self.id,"provider":self.provider,
            "cwd":self.cwd,"title":data.lifecycle.title.as_deref().unwrap_or(&self.title),"archived":data.lifecycle.archived,"explicitlySuspended":data.lifecycle.suspended,"status":data.status,"projectId":data.project_id,"hostId":self.host_id,"locationId":self.location_id,"automation":data.automation,"initiatedBy":data.control_origin,"continuation":data.continuation,"fork":data.lineage,"timeouts":data.timeouts,"effectiveTimeouts":timeouts::effective(data.timeouts.clone()),"inputQueue":data.input_queue},"events":data.events,
            "truncated":data.events.front().is_some_and(|e|e["seq"].as_u64().unwrap_or(0)>1),
            "permissionModes":data.permission_modes,"models":data.models,"commands":data.available_commands,"storageError":data.storage_error,
            "approvals":approvals.iter().map(|(id,a)|json!({"id":id,"request":a.request})).collect::<Vec<_>>()})
    }
    fn display_title(&self) -> String {
        self.data
            .lock()
            .unwrap()
            .lifecycle
            .title
            .clone()
            .unwrap_or_else(|| self.title.clone())
    }
    fn metadata(&self) -> store::Metadata {
        store::Metadata {
            id: self.id.clone(),
            provider: self.provider.clone(),
            cwd: self.cwd.clone(),
            title: self.title.clone(),
            initial_model: self.initial_model.clone(),
            created: self.created,
            host_id: self.host_id.clone(),
            location_id: self.location_id.clone(),
        }
    }
    fn send_command(&self, command: runtime::Command) -> Result<(), (StatusCode, String)> {
        if !matches!(
            command,
            runtime::Command::Cancel | runtime::Command::CancelInbox(_)
        ) && self.data.lock().unwrap().storage_error.is_some()
        {
            return Err(fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Session could not be saved to disk",
            ));
        }
        self.commands
            .lock()
            .unwrap()
            .as_ref()
            .ok_or_else(|| fail(StatusCode::GONE, "Agent process unavailable"))?
            .try_send(command)
            .map_err(|_| {
                fail(
                    StatusCode::CONFLICT,
                    "Agent is unavailable or a command is pending",
                )
            })
    }
    fn set_commands(&self, commands: Value) {
        self.data.lock().unwrap().available_commands = commands.clone();
        self.emit("commands", commands);
    }
    fn set_models(&self, models: Value) {
        self.data.lock().unwrap().models = models.clone();
        self.emit("models", models);
    }
    fn set_permission_modes(&self, modes: Value) {
        self.data.lock().unwrap().permission_modes = modes.clone();
        self.emit("permission_modes", modes);
    }
    fn start_queued_prompt(&self) -> Result<bool, String> {
        let text = {
            let mut data = self.data.lock().unwrap();
            let Some(text) = data.queued_prompt.take() else {
                return Ok(false);
            };
            let id = data
                .automation
                .as_ref()
                .map(|a| format!("automation:{}", a.run_id))
                .ok_or("Missing automation metadata")?;
            if let Some(origin) = data.automation.as_mut() {
                origin.outcome = None;
            }
            data.inbox_turn = None;
            data.input_queue.begin_turn(&text);
            data.requests.insert(id.clone());
            data.status = "running".into();
            data.turn_active = true;
            (text, id)
        };
        self.emit("user", json!({"text":text.0,"requestId":text.1}));
        self.emit("status", json!({"status":"running"}));
        self.send_command(runtime::Command::Prompt(text.0))
            .map_err(|(_, e)| e)?;
        Ok(true)
    }
    fn import_history(&self, updates: Vec<Value>) -> Result<(), String> {
        let mut data = self.data.lock().unwrap();
        if !data.import_pending {
            return Ok(());
        }
        let mut next = data.clone();
        let mut events = Vec::with_capacity(updates.len() + 1);
        for update in updates {
            next.seq += 1;
            events.push(json!({"seq":next.seq,"kind":"acp","data":update}));
        }
        next.seq += 1;
        next.import_pending = false;
        events.push(
            json!({"seq":next.seq,"kind":"history_imported","data":{"updates":events.len()}}),
        );
        for event in &events {
            next.bytes += event.to_string().len();
            next.events.push_back(event.clone());
            while next.events.len() > MAX_EVENTS || next.bytes > MAX_EVENT_BYTES {
                if let Some(old) = next.events.pop_front() {
                    next.bytes -= old.to_string().len();
                }
            }
        }
        self.store
            .lock()
            .unwrap()
            .batch(&self.id, &next, &events)
            .map_err(|_| "Could not save imported history to disk")?;
        *data = next;
        // Publish one snapshot so a large replay does not overflow browser event queues.
        drop(data);
        let _ = self.tx.send(self.snapshot());
        Ok(())
    }
    fn clear_approvals(&self) {
        let approvals = std::mem::take(&mut *self.approvals.lock().unwrap());
        for (id, approval) in approvals {
            let _ = approval.answer.send(None);
            self.emit("approval_resolved", json!({"id":id,"cancelled":true}));
        }
    }
}

fn fail(code: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (code, msg.into())
}
fn session(app: &App, id: &str) -> Result<Arc<Session>, (StatusCode, String)> {
    app.sessions
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Session not found"))
}
fn key_present(provider: &str) -> bool {
    let keys: &[&str] = match provider {
        "claude" => &["ANTHROPIC_API_KEY"],
        "codex" => &["CODEX_API_KEY", "OPENAI_API_KEY"],
        _ => return true,
    };
    keys.iter()
        .any(|k| std::env::var(k).is_ok_and(|s| !s.trim().is_empty()))
}
fn use_api_key() -> bool {
    std::env::var("AGENTWORKS_AUTH_MODE").is_ok_and(|mode| mode == "api_key")
}
fn adapter_path(provider: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("node_modules/.bin")
        .join(if provider == "claude" {
            "claude-agent-acp"
        } else {
            "codex-acp"
        })
}
fn opencode_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTWORKS_OPENCODE_PATH") {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join("opencode"))
            .find(|p| p.is_file())
    })
}
fn installed(provider: &str) -> bool {
    match provider {
        "demo" => true,
        "opencode" => opencode_path().is_some(),
        _ => adapter_path(provider).exists(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--service-protocol") {
        println!("{}", json!({"serviceProtocol":1}));
        return Ok(());
    }
    if args.get(1).is_some_and(|a| a == "mcp") {
        return mcp::stdio().await.map_err(|e| e.to_string().into());
    }
    if args.get(1).is_some_and(|a| a == "--probe") {
        let provider = args.get(2).map(String::as_str).unwrap_or("demo");
        if !["claude", "codex", "opencode", "demo"].contains(&provider) {
            return Err("Use --probe claude|codex|opencode|demo".into());
        }
        runtime::probe(provider).await?;
        return Ok(());
    }
    let auth_mode = std::env::var("AGENTWORKS_AUTH_MODE").unwrap_or_else(|_| "existing".into());
    if !["existing", "api_key"].contains(&auth_mode.as_str()) {
        return Err("AGENTWORKS_AUTH_MODE must be existing or api_key".into());
    }
    let port = std::env::var("AGENTWORKS_PORT")
        .unwrap_or("4317".into())
        .parse::<u16>()?;
    let bind = std::env::var("AGENTWORKS_BIND")
        .unwrap_or_else(|_| "127.0.0.1".into())
        .parse::<IpAddr>()?;
    let roots: Vec<PathBuf> = match std::env::var_os("AGENTWORKS_ROOTS") {
        Some(paths) => std::env::split_paths(&paths).collect(),
        None => vec![std::env::current_dir()?],
    }
    .into_iter()
    .map(std::fs::canonicalize)
    .collect::<Result<_, _>>()?;
    if roots.is_empty() || roots.iter().any(|p| !p.is_dir()) {
        return Err("AGENTWORKS_ROOTS must contain existing directories".into());
    }
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    let catalog_path = std::env::var_os("AGENTWORKS_CATALOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".agentworks/catalog.json")
        });
    let db_path = std::env::var_os("AGENTWORKS_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| catalog_path.with_extension("sessions.sqlite3"));
    let mut store = store::Store::open(&db_path).map_err(|e| e.to_string())?;
    store
        .import_legacy(&db_path.with_file_name("legacy-sessions.json"))
        .map_err(|e| e.to_string())?;
    inbox_delivery::reconcile(&mut store).map_err(|(_, e)| e)?;
    let restored = store.load().map_err(|e| e.to_string())?;
    let mcp_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let mcp_endpoint = mcp_listener.local_addr()?;
    let catalog = Arc::new(Mutex::new(Catalog::open(catalog_path)?));
    let app = App {
        sessions: Default::default(),
        discovery: Arc::new(tokio::sync::Semaphore::new(2)),
        roots: Arc::new(roots),
        port,
        bind,
        catalog: catalog.clone(),
        store: Arc::new(Mutex::new(store)),
        run_lock: Arc::new(tokio::sync::Mutex::new(())),
        workspace_lock: Arc::new(tokio::sync::Mutex::new(())),
        communication_lock: Arc::new(tokio::sync::Mutex::new(())),
        mcp_endpoint,
        restart: Arc::new(service_restart::Manager::new()),
        notifications: notifications::Hub::new(catalog),
    };
    if let Some(policy) = app
        .catalog
        .lock()
        .unwrap()
        .data
        .settings
        .timeouts
        .clone()
        .filter(|p| p.validate().is_ok())
    {
        timeouts::set_default(policy);
    }
    for (metadata, mut data) in restored {
        inbox_delivery::restore(&mut data);
        data.continuation.reconcile();
        data.input_queue.recover();
        let pending = data.queued_prompt.take().is_some();
        let interrupted = data.turn_active
            || pending
            || matches!(data.status.as_str(), "running" | "waiting" | "cancelling");
        if let Some(project) = data.project_id.as_deref() {
            app.catalog
                .lock()
                .unwrap()
                .attach_location(project, &metadata.location_id)?;
        }
        data.status = "suspended".into();
        data.turn_active = false;
        data.inbox_turn = None;
        let (tx, _) = broadcast::channel(128);
        let s = Arc::new(Session {
            id: metadata.id,
            provider: metadata.provider,
            cwd: metadata.cwd,
            title: metadata.title,
            initial_model: metadata.initial_model,
            created: metadata.created,
            host_id: metadata.host_id,
            location_id: metadata.location_id,
            data: Mutex::new(data),
            tx,
            commands: Mutex::new(None),
            store: app.store.clone(),
            approvals: Default::default(),
            worker: Default::default(),
            mcp_endpoint: app.mcp_endpoint,
            mcp_token: Mutex::new(Uuid::new_v4().to_string()),
            mcp_refresh_pending: std::sync::atomic::AtomicBool::new(false),
            notifications: app.notifications.clone(),
        });
        if interrupted {
            s.emit("interrupted",json!({"message":"The server stopped during this turn. It was not automatically retried. Check the last tool result before continuing."}));
        }
        s.status("suspended");
        app.sessions.lock().unwrap().insert(s.id.clone(), s);
    }
    service_restart::initialize(&app)?;
    input_queue::initialize(&app)?;
    tokio::spawn(service_restart::scheduler(app.clone()));
    let scheduler = tokio::spawn(limit_resume::scheduler(app.clone()));
    let inbox_scheduler = tokio::spawn(inbox_delivery::scheduler(app.clone()));
    let input_scheduler = tokio::spawn(input_queue::scheduler(app.clone()));
    tokio::spawn(mcp::serve_bridge(app.clone(), mcp_listener));
    let router = Router::new()
        .route(
            "/api/service/restart",
            get(service_restart::status).post(service_restart::post),
        )
        .route("/api/service/restart/cancel", post(service_restart::cancel))
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route(
            "/context-menu.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/context-menu.js"),
                )
            }),
        )
        .route(
            "/flyout.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/flyout.js"),
                )
            }),
        )
        .route(
            "/side-panel.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/side-panel.js"),
                )
            }),
        )
        .route(
            "/resize-handle.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/resize-handle.js"),
                )
            }),
        )
        .route(
            "/usage-status.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/usage-status.js"),
                )
            }),
        )
        .route(
            "/notifications.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/notifications.js"),
                )
            }),
        )
        .route(
            "/workspace.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/workspace.js"),
                )
            }),
        )
        .route(
            "/worktree-controls.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/worktree-controls.js"),
                )
            }),
        )
        .route(
            "/input-queue.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/input-queue.js"),
                )
            }),
        )
        .route(
            "/mentions.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/mentions.js"),
                )
            }),
        )
        .route(
            "/timeout-controls.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/timeout-controls.js"),
                )
            }),
        )
        .route(
            "/diff.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/diff.js"),
                )
            }),
        )
        .route(
            "/git-context.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/git-context.js"),
                )
            }),
        )
        .route(
            "/workspace.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/workspace.css"),
                )
            }),
        )
        .route(
            "/inbox-delivery.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/inbox-delivery.js"),
                )
            }),
        )
        .route(
            "/communication.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/communication.js"),
                )
            }),
        )
        .route(
            "/palettes.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/palettes.js"),
                )
            }),
        )
        .route(
            "/display-preferences.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/display-preferences.js"),
                )
            }),
        )
        .route(
            "/work-logs.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/work-logs.js"),
                )
            }),
        )
        .route("/api/locations", post(workspace::register_location))
        .route("/api/locations/{id}/files", get(workspace::list_files))
        .route("/api/locations/{id}/file", get(workspace::read_file))
        .route(
            "/api/automations",
            get(automations::list).post(automations::create),
        )
        .route(
            "/api/automations/{id}",
            axum::routing::put(automations::update).delete(automations::delete),
        )
        .route("/api/automations/{id}/run", post(automations::run))
        .route("/api/automations/{id}/runs", get(automations::runs))
        .route("/api/status", get(status))
        .route("/api/activity", get(activity))
        .route("/api/settings", get(get_settings).put(save_settings))
        .route("/api/timeouts", get(get_timeouts).put(save_timeouts))
        .route(
            "/api/notifications",
            get(get_notifications).put(save_notifications),
        )
        .route("/api/notifications/events", get(notification_events))
        .route(
            "/api/sessions/{id}/timeouts",
            axum::routing::put(save_session_timeouts),
        )
        .route("/api/models", post(discover_models))
        .route("/api/external-sessions", post(list_external_sessions))
        .route("/api/sessions/import", post(import_session))
        .route("/api/catalog", get(get_catalog))
        .route("/api/projects", post(create_project))
        .route(
            "/api/projects/{id}",
            axum::routing::patch(rename_project).delete(delete_project),
        )
        .route("/api/projects/{id}/archive", post(archive_project))
        .route(
            "/api/projects/{id}/workspace-policy",
            get(worktrees::get_policy).put(worktrees::put_policy),
        )
        .route("/api/worktrees/list", post(worktrees::list))
        .route("/api/sessions/{id}/project", post(assign_project))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session)
                .patch(lifecycle::update)
                .delete(delete_session),
        )
        .route(
            "/api/sessions/{id}/agent-access",
            get(communication::get_access).put(communication::set_access),
        )
        .route(
            "/api/sessions/{id}/completion",
            get(completion::get_completion),
        )
        .route(
            "/api/sessions/{id}/inbox-delivery",
            get(inbox_delivery::get).put(inbox_delivery::configure),
        )
        .route("/api/sessions/{id}/peers", get(communication::get_peers))
        .route(
            "/api/sessions/{id}/mailbox",
            get(communication::get_mailbox).post(communication::post_message),
        )
        .route(
            "/api/sessions/{id}/mailbox/{message}/ack",
            post(communication::ack_message),
        )
        .route(
            "/api/sessions/{id}/agent-audit",
            get(communication::audit_log),
        )
        .route(
            "/api/sessions/{id}/continuation",
            axum::routing::put(limit_resume::configure),
        )
        .route("/api/sessions/{id}/prompt", post(prompt))
        .route(
            "/api/sessions/{id}/queue",
            get(input_queue::get).post(input_queue::enqueue),
        )
        .route("/api/sessions/{id}/queue/resume", post(input_queue::resume))
        .route(
            "/api/sessions/{id}/queue/{item}",
            axum::routing::patch(input_queue::edit).delete(input_queue::remove),
        )
        .route("/api/sessions/{id}/cancel", post(cancel))
        .route("/api/sessions/{id}/stop", post(cancel))
        .route("/api/sessions/{id}/suspend", post(lifecycle::suspend))
        .route("/api/sessions/{id}/resume", post(resume))
        .route("/api/sessions/{id}/model", post(set_model))
        .route("/api/sessions/{id}/fork", post(fork_session))
        .route(
            "/api/sessions/{id}/permission-mode",
            get(permission_modes::get).post(permission_modes::set),
        )
        .route("/api/sessions/{id}/approvals/{approval}", post(approve))
        .route("/api/sessions/{id}/events", get(events))
        .route("/api/sessions/{id}/file", get(file))
        .route("/api/sessions/{id}/workspace", get(git_context::workspace))
        .route("/api/sessions/{id}/diff", get(diff::view))
        .route("/api/markdown", post(markdown))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app.clone());
    println!("Agentworks listening on {bind}:{port}");
    println!("Local URL: http://127.0.0.1:{port}");
    println!(
        "Persistent sessions: {}. Ctrl-C to stop.",
        db_path.display()
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            scheduler.abort();
            let _ = scheduler.await;
            input_scheduler.abort();
            let _ = input_scheduler.await;
            inbox_scheduler.abort();
            let _=inbox_scheduler.await;
            for s in app.sessions.lock().unwrap().values() {
                if let Some(worker) = s.worker.lock().unwrap().take() { worker.abort(); }
                *s.commands.lock().unwrap()=None;
                if s.data.lock().unwrap().turn_active {s.emit("interrupted",json!({"message":"Server stopped during a turn; it was not automatically retried."}));}
                s.clear_approvals();
                s.status("suspended");
            }
        })
        .await?;
    Ok(())
}

async fn guard(
    State(app): State<App>,
    headers: HeaderMap,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let direct_host = allowed_host(host, app.bind, app.port);
    let forwarded_origin = browser_origin::forwarded_origin(&headers);
    let ws = req.uri().path().ends_with("/events");
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ws {
        eprintln!(
            "WebSocket request path={:?} host={:?} origin={:?} x_forwarded_proto={:?} x_forwarded_host={:?} direct_host={} forwarded_origin={:?}",
            req.uri().path(),
            log_header(&headers, header::HOST),
            log_header(&headers, header::ORIGIN),
            log_header(&headers, "x-forwarded-proto"),
            log_header(&headers, "x-forwarded-host"),
            direct_host,
            forwarded_origin
        );
    }
    if !direct_host && forwarded_origin.is_none() {
        if ws {
            eprintln!("WebSocket rejected reason=unrecognized_host");
        }
        return (StatusCode::FORBIDDEN, "Unrecognized host").into_response();
    }
    let write = !matches!(req.method().as_str(), "GET" | "HEAD");
    if write || ws {
        if !(direct_host && origin == format!("http://{host}"))
            && !browser_origin::origin_matches_forwarded(&headers, origin)
        {
            if ws {
                eprintln!(
                    "WebSocket rejected reason=origin_mismatch path={:?}",
                    req.uri().path()
                );
            }
            return (StatusCode::FORBIDDEN, "Same-origin requests required").into_response();
        }
    }
    if ws {
        eprintln!("WebSocket guard passed path={:?}", req.uri().path());
    }
    let mut response = next.run(req).await;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    h.insert(header::CONTENT_SECURITY_POLICY,"default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; frame-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    response
}

fn log_header(headers: &HeaderMap, name: impl axum::http::header::AsHeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(256).collect())
        .unwrap_or_else(|| "<missing>".into())
}

fn allowed_host(host: &str, bind: IpAddr, port: u16) -> bool {
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    if authority.port_u16().unwrap_or(80) != port || authority.host().contains('@') {
        return false;
    }
    if !bind.is_loopback() {
        return true;
    }
    authority.host() == "localhost"
        || authority
            .host()
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip == bind)
}
async fn status(State(app): State<App>) -> Json<Value> {
    let defaults = app.catalog.lock().unwrap().data.settings.clone();
    Json(
        json!({"roots":effective_roots(&app),"defaultModels":defaults.default_models,"defaultCommunicationRole":defaults.default_communication_role.unwrap_or_default(),"providers":[
            {"id":"demo","name":"Demo agent","installed":true,"keyPresent":true},
            {"id":"claude","name":"Claude","installed":adapter_path("claude").exists(),"keyPresent":key_present("claude"),"authMode":if use_api_key(){"api_key"}else{"existing"}},
            {"id":"codex","name":"Codex","installed":adapter_path("codex").exists(),"keyPresent":key_present("codex"),"authMode":if use_api_key(){"api_key"}else{"existing"}},
            {"id":"opencode","name":"OpenCode · free models","installed":installed("opencode"),"keyPresent":true}
        ]}),
    )
}
fn effective_roots(app: &App) -> Vec<PathBuf> {
    app.catalog
        .lock()
        .unwrap()
        .data
        .settings
        .roots
        .clone()
        .unwrap_or_else(|| app.roots.as_ref().clone())
}
fn checked_cwd(app: &App, path: &str) -> Result<PathBuf, (StatusCode, String)> {
    let cwd = std::fs::canonicalize(path).map_err(|_| {
        fail(
            StatusCode::BAD_REQUEST,
            "Choose an existing project directory",
        )
    })?;
    if !cwd.is_dir() || !effective_roots(app).iter().any(|r| cwd.starts_with(r)) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Project must be inside an allowed root directory",
        ));
    }
    Ok(cwd)
}
async fn get_settings(State(app): State<App>) -> Json<Value> {
    let roots = effective_roots(&app);
    let settings = app.catalog.lock().unwrap().data.settings.clone();
    Json(
        json!({"roots":roots,"defaultModels":settings.default_models,"claudeDefaultPermissionMode":settings.claude_default_permission_mode.unwrap_or_default(),"defaultCommunicationRole":settings.default_communication_role.unwrap_or_default()}),
    )
}

/// U-02 saved policy; browser capability and permission stay in the browser.
async fn get_notifications(State(app): State<App>) -> Json<Value> {
    let catalog = app.catalog.lock().unwrap();
    Json(json!({
        "settings":catalog.data.settings.notifications,
        "categories":notifications::Event::ALL.iter().map(|e|json!({"id":e,"label":e.label()})).collect::<Vec<_>>(),
        "projects":catalog.data.projects.iter().map(|p|json!({"id":p.id,"name":p.name})).collect::<Vec<_>>(),
    }))
}
async fn save_notifications(
    State(app): State<App>,
    Json(settings): Json<notifications::Settings>,
) -> ApiResult {
    settings
        .validate()
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    app.catalog
        .lock()
        .unwrap()
        .notifications(settings.clone())
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not save notification settings",
            )
        })?;
    Ok(Json(json!({"settings":settings})))
}
async fn save_settings(State(app): State<App>, Json(mut settings): Json<Settings>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    let roots = settings
        .roots
        .as_ref()
        .filter(|r| !r.is_empty() && r.len() <= 50)
        .ok_or_else(|| {
            fail(
                StatusCode::BAD_REQUEST,
                "Provide between 1 and 50 allowed root directories",
            )
        })?;
    let mut normalized = Vec::new();
    for root in roots {
        let path = std::fs::canonicalize(root).map_err(|_| {
            fail(
                StatusCode::BAD_REQUEST,
                "Every root must be an existing directory",
            )
        })?;
        if !path.is_dir() {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Every root must be a directory",
            ));
        }
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    for (provider, model) in &settings.default_models {
        if !["demo", "claude", "codex", "opencode"].contains(&provider.as_str())
            || model.is_empty()
            || model.len() > 256
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Invalid provider or model ID",
            ));
        }
        if provider == "opencode" && !runtime::is_free_model(model) {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "OpenCode is restricted to advertised free models",
            ));
        }
    }
    settings.roots = Some(normalized);
    persist_settings(&app, |current| {
        // Older settings forms may omit these controls; keep their saved defaults.
        settings.claude_default_permission_mode = settings
            .claude_default_permission_mode
            .or(current.claude_default_permission_mode);
        settings.default_communication_role = settings
            .default_communication_role
            .or(current.default_communication_role);
        settings.timeouts = settings.timeouts.take().or(current.timeouts.clone());
        settings.notifications = current.notifications.clone();
        settings
    })?;
    Ok(get_settings(State(app.clone())).await)
}
async fn get_timeouts() -> Json<Value> {
    Json(json!({"default":timeouts::default_policy()}))
}
async fn save_timeouts(State(app): State<App>, Json(policy): Json<timeouts::Policy>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    persist_settings(&app, |current| catalog::Settings {
        timeouts: Some(policy),
        ..current.clone()
    })?;
    Ok(get_timeouts().await)
}
#[derive(Deserialize)]
struct SessionTimeouts {
    policy: Option<timeouts::Policy>,
}
/// Applies to the next turn launch; a running turn keeps its fixed policy.
async fn save_session_timeouts(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<SessionTimeouts>,
) -> ApiResult {
    if let Some(policy) = &req.policy {
        policy
            .validate()
            .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    }
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let effective = {
        let mut data = s.data.lock().unwrap();
        let mut next = data.clone();
        next.timeouts = req.policy;
        // Commit before publishing: a failed save is never acknowledged or applied.
        s.store
            .lock()
            .unwrap()
            .save_state(&s.id, &next)
            .map_err(|_| {
                fail(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "Could not save session timeouts",
                )
            })?;
        *data = next;
        timeouts::effective(data.timeouts.clone())
    };
    let result = json!({"timeouts":s.data.lock().unwrap().timeouts,"effective":effective,"appliesTo":"nextTurn"});
    // Emitting persists session state and shows the change in the transcript.
    s.emit("timeout_settings", result.clone());
    Ok(Json(result))
}
/// Single validated write path: merge, persist, then publish the effective
/// default under the catalog lock so saved and effective policy never diverge.
fn persist_settings(
    app: &App,
    build: impl FnOnce(&Settings) -> Settings,
) -> Result<(), (StatusCode, String)> {
    let mut catalog = app.catalog.lock().unwrap();
    let next = build(&catalog.data.settings);
    if let Some(policy) = &next.timeouts {
        policy
            .validate()
            .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    }
    let policy = next.timeouts.clone().unwrap_or_default();
    catalog
        .settings(next)
        .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "Could not save settings"))?;
    timeouts::set_default(policy);
    Ok(())
}
#[derive(Deserialize)]
struct ModelDiscovery {
    provider: String,
    cwd: String,
}
async fn discover_models(State(app): State<App>, Json(req): Json<ModelDiscovery>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    service_restart::allow_start(&app)?;
    if !["demo", "claude", "codex", "opencode"].contains(&req.provider.as_str())
        || !installed(&req.provider)
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Provider is unknown or not installed",
        ));
    }
    if use_api_key() && !key_present(&req.provider) {
        return Err(fail(StatusCode::CONFLICT, "Provider API key is missing"));
    }
    let cwd = checked_cwd(&app, &req.cwd)?;
    let _permit = app.discovery.clone().try_acquire_owned().map_err(|_| {
        fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Model discovery is busy; try again shortly",
        )
    })?;
    let models = runtime::discover(&req.provider, cwd)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, runtime::safe_error(&e.to_string())))?;
    Ok(Json(models))
}
#[derive(Deserialize)]
struct ExternalList {
    #[serde(default, rename = "hostProfile")]
    host_profile: bool,
    provider: String,
    cwd: Option<String>,
    cursor: Option<String>,
}
async fn list_external_sessions(
    State(app): State<App>,
    Json(req): Json<ExternalList>,
) -> ApiResult {
    if !["demo", "claude", "codex", "opencode"].contains(&req.provider.as_str())
        || !installed(&req.provider)
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Provider is unknown or not installed",
        ));
    }
    if use_api_key() && !key_present(&req.provider) {
        return Err(fail(StatusCode::CONFLICT, "Provider API key is missing"));
    }
    if req.cursor.as_ref().is_some_and(|c| c.len() > 4096) {
        return Err(fail(StatusCode::BAD_REQUEST, "Invalid page cursor"));
    }
    let cwd = req
        .cwd
        .filter(|s| !s.trim().is_empty())
        .map(|s| checked_cwd(&app, &s))
        .transpose()?;
    let _permit = app.discovery.clone().try_acquire_owned().map_err(|_| {
        fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Harness discovery is busy; try again shortly",
        )
    })?;
    let mut response = runtime::list_sessions(&req.provider, cwd, req.cursor, req.host_profile)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, runtime::safe_error(&e.to_string())))?;
    let roots = effective_roots(&app);
    let mut visible = Vec::new();
    let sessions = app.sessions.lock().unwrap();
    for item in response["sessions"].as_array().into_iter().flatten() {
        let Some(path) = item["cwd"]
            .as_str()
            .and_then(|p| std::fs::canonicalize(p).ok())
        else {
            continue;
        };
        if !path.is_dir() || !roots.iter().any(|r| path.starts_with(r)) {
            continue;
        }
        let mut item = item.clone();
        item["cwd"] = json!(path);
        if let Some(existing) = sessions.values().find(|s| {
            if s.provider != req.provider {
                return false;
            }
            let data = s.data.lock().unwrap();
            data.opencode_host == (req.provider == "opencode" && req.host_profile)
                && data.provider_session_id.as_deref() == item["sessionId"].as_str()
        }) {
            item["agentworksId"] = json!(existing.id);
            item["archived"] = existing.meta()["archived"].clone();
            item["title"] = json!(existing.display_title());
        }
        visible.push(item);
    }
    response["sessions"] = json!(visible);
    Ok(Json(response))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportSession {
    #[serde(default)]
    host_profile: bool,
    provider: String,
    provider_session_id: String,
    cwd: String,
    title: Option<String>,
    project_id: Option<String>,
}
async fn import_session(State(app): State<App>, Json(req): Json<ImportSession>) -> ApiResult {
    if req.provider_session_id.is_empty()
        || req.provider_session_id.len() > 256
        || req.provider_session_id.chars().any(char::is_control)
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Invalid harness session ID"));
    }
    create_session(
        State(app),
        Json(Create {
            resume_policy: None,
            inherited_resume: None,
            workspace: None,
            request_id: None,
            agent_access: None,
            provider: req.provider.clone(),
            cwd: req.cwd,
            title: req.title,
            model: Some(String::new()),
            project_id: req.project_id,
            host_id: Some("local".into()),
            external_id: Some(req.provider_session_id),
            external_host: req.provider == "opencode" && req.host_profile,
            automation: None,
            queued_prompt: None,
            control_origin: None,
            fork: None,
        }),
    )
    .await
}
async fn get_catalog(State(app): State<App>) -> Json<Value> {
    let catalog = app.catalog.lock().unwrap();
    Json(
        json!({"hosts":[{"id":"local","name":"Local","status":"connected"}],
        "projects":catalog.data.projects,"locations":catalog.data.locations}),
    )
}
#[derive(Deserialize)]
struct ProjectName {
    name: String,
    #[serde(
        default,
        rename = "defaultCommunicationRole",
        deserialize_with = "communication::role_patch"
    )]
    default_communication_role: Option<Option<communication::Role>>,
}
async fn create_project(State(app): State<App>, Json(req): Json<ProjectName>) -> ApiResult {
    let project = app
        .catalog
        .lock()
        .unwrap()
        .project(None, &req.name, req.default_communication_role)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!(project)))
}
async fn rename_project(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<ProjectName>,
) -> ApiResult {
    let project = app
        .catalog
        .lock()
        .unwrap()
        .project(Some(&id), &req.name, req.default_communication_role)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!(project)))
}
/// What happens to a project's sessions when the project is archived or deleted.
/// The default applies the same operation; `ungroup` moves them to Ungrouped instead.
#[derive(Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProjectSessions {
    #[default]
    Cascade,
    Ungroup,
}
#[derive(Default, Deserialize)]
struct ProjectRemoval {
    #[serde(default)]
    sessions: ProjectSessions,
}
fn project_sessions(app: &App, id: &str) -> Vec<Arc<Session>> {
    app.sessions
        .lock()
        .unwrap()
        .values()
        .filter(|s| s.data.lock().unwrap().project_id.as_deref() == Some(id))
        .cloned()
        .collect()
}
/// Callers hold the communication and run locks.
fn ungroup_sessions(
    app: &App,
    sessions: &[Arc<Session>],
    reason: &str,
) -> Result<(), (StatusCode, String)> {
    for s in sessions {
        s.data.lock().unwrap().project_id = None;
        inbox_delivery::revoke_related(app, s, reason)?;
        s.emit("session_updated", json!({"projectId":null}));
    }
    Ok(())
}
async fn delete_project(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(req): Query<ProjectRemoval>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    if !app.catalog.lock().unwrap().contains(&id) {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    app.store
        .lock()
        .unwrap()
        .detach_automation_project(&id)
        .map_err(|_| {
            fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not update automation projects",
            )
        })?;
    let affected = project_sessions(&app, &id);
    if req.sessions == ProjectSessions::Cascade {
        // Delete sessions before the project so a failure leaves a retryable project.
        for s in &affected {
            delete_session_locked(&app, s).await?;
        }
    }
    app.catalog
        .lock()
        .unwrap()
        .remove(&id)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    if req.sessions == ProjectSessions::Ungroup {
        ungroup_sessions(&app, &affected, "Project deleted")?;
    }
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct ProjectArchive {
    archived: bool,
    #[serde(default)]
    sessions: ProjectSessions,
}
/// Archiving cascades to sessions (or ungroups them); unarchiving restores the
/// project and the sessions still inside it. Archiving never stops work.
async fn archive_project(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<ProjectArchive>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    if !app.catalog.lock().unwrap().contains(&id) {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    let affected = project_sessions(&app, &id);
    if req.archived && req.sessions == ProjectSessions::Ungroup {
        ungroup_sessions(&app, &affected, "Project archived")?;
    } else {
        for s in &affected {
            let _ = lifecycle::apply(s, None, Some(req.archived))?;
        }
    }
    let project = app
        .catalog
        .lock()
        .unwrap()
        .set_archived(&id, req.archived)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!(project)))
}
#[derive(Deserialize)]
struct ProjectChoice {
    #[serde(rename = "projectId")]
    project_id: Option<String>,
}
async fn assign_project(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<ProjectChoice>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let mut catalog = app.catalog.lock().unwrap();
    if req
        .project_id
        .as_deref()
        .is_some_and(|id| !catalog.contains(id))
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    if let Some(project) = req.project_id.as_deref() {
        catalog
            .attach_location(project, &s.location_id)
            .map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not save project directory",
                )
            })?;
    }
    drop(catalog);
    inbox_delivery::revoke_related(&app, &s, "Project assignment changed")?;
    s.data.lock().unwrap().project_id = req.project_id.clone();
    s.emit("session_updated", json!({"projectId":req.project_id}));
    Ok(Json(s.meta()))
}
async fn list_sessions(
    State(app): State<App>,
    Query(query): Query<lifecycle::Filter>,
) -> Json<Value> {
    let mut list: Vec<_> = app
        .sessions
        .lock()
        .unwrap()
        .values()
        .map(|s| s.meta())
        .filter(|s| query.includes(s["archived"].as_bool().unwrap_or(false)))
        .collect();
    list.sort_by_key(|s| std::cmp::Reverse(s["created"].as_u64().unwrap_or(0)));
    Json(json!(list))
}
async fn activity(State(app): State<App>) -> Json<Value> {
    // The async preparation guard releases on success, failure and cancellation.
    // It also covers brief policy/root edits, conservatively reporting those as busy.
    let workspace_preparing = app.workspace_lock.try_lock().is_err();
    let sessions: Vec<_> = app
        .sessions
        .lock()
        .unwrap()
        .values()
        .map(|s| {
            // Match snapshot's lock order so approval and turn state are read together.
            let approvals = s.approvals.lock().unwrap();
            let data = s.data.lock().unwrap();
            let busy = data.turn_active
                || !approvals.is_empty()
                || matches!(
                    data.status.as_str(),
                    "starting" | "resuming" | "configuring" | "cancelling"
                );
            json!({"id":s.id,"title":data.lifecycle.title.as_deref().unwrap_or(&s.title),"provider":s.provider,"status":data.status,
            "turnRunning":data.turn_active,"pendingApprovals":approvals.len(),"canClose":!busy,"continuation":data.continuation})
        })
        .collect();
    let busy: Vec<_> = sessions
        .iter()
        .filter(|s| s["canClose"] == false)
        .cloned()
        .collect();
    let scheduled: Vec<_> = sessions
        .iter()
        .filter(|s| s["continuation"]["dueAt"].is_number())
        .cloned()
        .collect();
    let unfinished: Vec<_> = sessions
        .iter()
        .filter(|s| s["continuation"]["limited"] == true)
        .cloned()
        .collect();
    let active = sessions.iter().filter(|s| s["turnRunning"] == true).count();
    let approvals: u64 = sessions
        .iter()
        .filter_map(|s| s["pendingApprovals"].as_u64())
        .sum();
    Json(
        json!({"safeToClose":busy.is_empty() && !workspace_preparing,"workspacePreparing":workspace_preparing,"activeTurns":active,"pendingApprovals":approvals,
        "busySessions":busy,"scheduledContinuations":scheduled,"unfinishedLimitedSessions":unfinished,"sessions":sessions}),
    )
}
async fn get_session(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(session(&app, &id)?.snapshot()))
}

/// L-08: creation options default to the parent's; a stable requestId is required.
/// The endpoint can
/// also be used directly. The child is always a new session under normal project
/// policy; conversation forking and worktree creation stay separate choices.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fork {
    boundary_seq: Option<u64>,
    context: Option<String>,
    summary: Option<String>,
    title: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    #[serde(rename = "projectId")]
    project_id: Option<String>,
    #[serde(rename = "hostId")]
    host_id: Option<String>,
    cwd: Option<String>,
    workspace: Option<worktrees::Workspace>,
    #[serde(rename = "agentAccess")]
    agent_access: Option<communication::Access>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "resumePolicy")]
    resume_policy: Option<limit_resume::PolicyInput>,
}

async fn fork_session(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Fork>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    service_restart::allow_start(&app)?;
    limit_resume::new_policy(req.resume_policy.clone())?;
    // Read-only: the parent's transcript is snapshotted without sending anything, so
    // a running turn is never interrupted and pending work is never copied.
    let _workspace = app.workspace_lock.lock().await;
    let request_id = req
        .request_id
        .as_deref()
        .filter(|id| !id.is_empty() && id.len() <= 100 && !id.chars().any(char::is_control))
        .ok_or_else(|| {
            fail(
                StatusCode::BAD_REQUEST,
                "Fork creation requires a stable requestId",
            )
        })?;
    let key = format!("fork:{id}:{request_id}");
    let payload = serde_json::to_string(&req).unwrap();
    let prior = app
        .store
        .lock()
        .unwrap()
        .reserve_creation(&key, &payload)
        .map_err(|e| fail(StatusCode::CONFLICT, e.to_string()))?;
    if let Some(child) = prior {
        return session(&app, &child).map(|s| Json(s.meta()));
    }
    let parent = session(&app, &id)?;
    let seed = fork::seed(
        &parent,
        req.boundary_seq,
        req.context.as_deref().unwrap_or("auto"),
        req.summary.as_deref(),
    )?;
    let provider = req
        .provider
        .clone()
        .unwrap_or_else(|| parent.provider.clone());
    let (title, inherited_model, project_id) = {
        let data = parent.data.lock().unwrap();
        (
            data.lifecycle
                .title
                .clone()
                .unwrap_or_else(|| parent.title.clone()),
            // A model only carries over to the same harness; another agent advertises
            // its own models and would reject a foreign ID.
            (provider == parent.provider)
                .then(|| {
                    data.models["currentValue"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| parent.initial_model.clone())
                })
                .flatten(),
            req.project_id.clone().or_else(|| data.project_id.clone()),
        )
    };
    let result = create_session_inner(
        &app,
        Create {
            resume_policy: req.resume_policy,
            inherited_resume: None,
            workspace: req.workspace,
            request_id: req.request_id,
            agent_access: req.agent_access,
            provider,
            cwd: req
                .cwd
                .unwrap_or_else(|| parent.cwd.to_string_lossy().into_owned()),
            title: Some(
                req.title
                    .unwrap_or_else(|| format!("{title} (fork)"))
                    .chars()
                    .take(80)
                    .collect(),
            ),
            model: req.model.or(inherited_model),
            project_id,
            host_id: req.host_id,
            external_id: None,
            external_host: false,
            automation: None,
            queued_prompt: None,
            control_origin: None,
            fork: Some(seed),
        },
        Some(&key),
    )
    .await;
    if let Err((_, message)) = &result {
        let _ = app.store.lock().unwrap().creation_error(&key, message);
    }
    result
}

#[derive(Serialize, Deserialize)]
struct Create {
    #[serde(rename = "resumePolicy")]
    resume_policy: Option<limit_resume::PolicyInput>,
    #[serde(skip)]
    inherited_resume: Option<limit_resume::State>,
    workspace: Option<worktrees::Workspace>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "agentAccess")]
    agent_access: Option<communication::Access>,
    provider: String,
    cwd: String,
    title: Option<String>,
    model: Option<String>,
    #[serde(rename = "projectId")]
    project_id: Option<String>,
    #[serde(rename = "hostId")]
    host_id: Option<String>,
    #[serde(skip)]
    external_id: Option<String>,
    #[serde(skip)]
    external_host: bool,
    #[serde(skip)]
    automation: Option<automations::Origin>,
    #[serde(skip)]
    queued_prompt: Option<String>,
    #[serde(skip)]
    control_origin: Option<communication::ControlOrigin>,
    /// L-08 prepared carry-over; built by the fork endpoint, never by a client.
    #[serde(skip)]
    fork: Option<fork::Seed>,
}
async fn create_session(State(app): State<App>, Json(req): Json<Create>) -> ApiResult {
    limit_resume::new_policy(req.resume_policy.clone())?;
    let _workspace = app.workspace_lock.lock().await;
    service_restart::allow_start(&app)?;
    let key = worktrees::creation_key(&req)?;
    if let Some(key) = &key {
        let payload = worktrees::creation_payload(&req);
        let prior = app
            .store
            .lock()
            .unwrap()
            .reserve_creation(key, &payload)
            .map_err(|e| fail(StatusCode::CONFLICT, e.to_string()))?;
        if let Some(id) = prior {
            return session(&app, &id)
                .map(|s| Json(s.meta()))
                .map_err(|_| fail(StatusCode::GONE, "Previously created session was deleted"));
        }
    }
    let result = create_session_inner(&app, req, key.as_deref()).await;
    if let (Some(key), Err((_, message))) = (&key, &result) {
        let _ = app.store.lock().unwrap().creation_error(key, message);
    }
    result
}
async fn create_session_inner(app: &App, req: Create, creation_key: Option<&str>) -> ApiResult {
    if req.model.as_ref().is_some_and(|m| m.len() > 256) {
        return Err(fail(StatusCode::BAD_REQUEST, "Model ID is too long"));
    }
    if !["demo", "claude", "codex", "opencode"].contains(&req.provider.as_str()) {
        return Err(fail(StatusCode::BAD_REQUEST, "Unknown provider"));
    }
    if use_api_key() && !key_present(&req.provider) {
        return Err(fail(
            StatusCode::CONFLICT,
            "Set this provider's API key in the server environment, then restart Agentworks.",
        ));
    }
    if !installed(&req.provider) {
        return Err(fail(
            StatusCode::CONFLICT,
            if req.provider == "opencode" {
                "OpenCode is not installed on PATH. Set AGENTWORKS_OPENCODE_PATH if needed."
            } else {
                "ACP adapter missing. Run npm ci --ignore-scripts."
            },
        ));
    }
    let source = checked_cwd(app, &req.cwd)?;
    // Registered imports/resume retain their original context, even after policy changes.
    if let Some(external) = &req.external_id {
        let existing = app
            .sessions
            .lock()
            .unwrap()
            .values()
            .find(|s| {
                let d = s.data.lock().unwrap();
                s.provider == req.provider
                    && d.opencode_host == req.external_host
                    && d.provider_session_id.as_ref() == Some(external)
            })
            .cloned();
        if let Some(existing) = existing {
            if existing.cwd != source {
                return Err(fail(
                    StatusCode::CONFLICT,
                    "This harness session is already registered with a different directory",
                ));
            }
            return Ok(Json(existing.meta()));
        }
    }
    worktrees::verify_import(app, &req, &source).await?;
    if req.host_id.as_deref().is_some_and(|id| id != "local") {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Only the Local host is available in this prototype",
        ));
    }
    if app
        .sessions
        .lock()
        .unwrap()
        .values()
        .filter(|s| {
            !matches!(
                s.data.lock().unwrap().status.as_str(),
                "suspended" | "error" | "closed"
            )
        })
        .count()
        >= 12
    {
        return Err(fail(
            StatusCode::CONFLICT,
            "Limit: 12 active harnesses. Suspend idle sessions first.",
        ));
    }
    let cwd = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        worktrees::prepare(app, &req, source, creation_key),
    )
    .await
    .map_err(|_| {
        fail(
            StatusCode::CONFLICT,
            "Workspace preparation timed out; inspect retained branch and files before retrying",
        )
    })??;
    let mut catalog = app.catalog.lock().unwrap();
    if req
        .project_id
        .as_deref()
        .is_some_and(|id| !catalog.contains(id))
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    let location = catalog.location(&cwd).map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save project location",
        )
    })?;
    if let Some(project) = req.project_id.as_deref() {
        catalog
            .attach_location(project, &location.id)
            .map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not save project directory",
                )
            })?;
    }
    let initial_access = req.agent_access.clone().or_else(|| {
        // Agent-created sessions and automation runs retain their explicit unprivileged behavior.
        if req.control_origin.is_some() || req.automation.is_some() {
            None
        } else {
            Some(
                catalog
                    .communication_role(req.project_id.as_deref())
                    .access(),
            )
        }
    });
    let (tx, _) = broadcast::channel(128);
    let title = req
        .title
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{} session", req.provider))
        .chars()
        .take(80)
        .collect();
    let fork = req.fork;
    let s = Arc::new(Session {
        id: Uuid::new_v4().to_string(),
        provider: req.provider.clone(),
        host_id: location.host_id,
        location_id: location.id,
        cwd,
        title,
        initial_model: req
            .model
            .or_else(|| {
                catalog
                    .data
                    .settings
                    .default_models
                    .get(&req.provider)
                    .cloned()
            })
            .filter(|m| !m.is_empty()),
        created: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        data: Mutex::new(SessionData {
            lifecycle: Default::default(),
            inbox_turn: None,
            status: "starting".into(),
            seq: 0,
            events: VecDeque::new(),
            bytes: 0,
            requests: HashSet::new(),
            models: Value::Null,
            permission_modes: Value::Null,
            initial_claude_permission_mode: permission_modes::initial_claude_default(
                &req.provider,
                req.external_id.is_some(),
                catalog.data.settings.claude_default_permission_mode,
            ),
            available_commands: json!([]),
            turn_active: false,
            project_id: req.project_id,
            provider_session_id: req.external_id.clone(),
            external: req.external_id.is_some(),
            import_pending: req.external_id.is_some(),
            opencode_host: req.external_host,
            automation: req.automation,
            queued_prompt: req.queued_prompt,
            input_queue: Default::default(),
            control_origin: req.control_origin,
            storage_error: None,
            continuation: req
                .inherited_resume
                .unwrap_or(limit_resume::new_policy(req.resume_policy)?),
            timeouts: None,
            lineage: Default::default(),
            carried_context: None,
        }),
        tx,
        commands: Mutex::new(None),
        store: app.store.clone(),
        approvals: Default::default(),
        worker: Default::default(),
        mcp_endpoint: app.mcp_endpoint,
        mcp_token: Mutex::new(Uuid::new_v4().to_string()),
        mcp_refresh_pending: std::sync::atomic::AtomicBool::new(false),
        notifications: app.notifications.clone(),
    });
    {
        let mut sessions = app.sessions.lock().unwrap();
        if let Some(id) = &req.external_id {
            if let Some(existing) = sessions.values().find(|s| {
                if s.provider != req.provider {
                    return false;
                }
                let data = s.data.lock().unwrap();
                data.opencode_host == req.external_host
                    && data.provider_session_id.as_ref() == Some(id)
            }) {
                if existing.cwd != s.cwd {
                    return Err(fail(
                        StatusCode::CONFLICT,
                        "This harness session is already registered with a different directory",
                    ));
                }
                return Ok(Json(existing.meta()));
            }
        }
        if sessions
            .values()
            .filter(|s| {
                !matches!(
                    s.data.lock().unwrap().status.as_str(),
                    "suspended" | "error" | "closed"
                )
            })
            .count()
            >= 12
        {
            return Err(fail(
                StatusCode::CONFLICT,
                "Limit: 12 active harnesses. Restart the server to suspend idle sessions first.",
            ));
        }
        if let Some(seed) = fork {
            fork::apply(&s, seed, &s.cwd)?;
        }
        worktrees::arm_creation(app, creation_key)?;
        app.store
            .lock()
            .unwrap()
            .insert(
                &s.metadata(),
                &s.data.lock().unwrap(),
                initial_access.as_ref(),
                creation_key,
            )
            .map_err(|_| {
                fail(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "Could not save this session to disk",
                )
            })?;
        sessions.insert(s.id.clone(), s.clone());
    }
    drop(catalog);
    s.emit("status", json!({"status":"starting"}));
    start_worker(&s);
    Ok(Json(s.meta()))
}
fn start_worker(s: &Arc<Session>) {
    *s.mcp_token.lock().unwrap() = Uuid::new_v4().to_string();
    let mut handle = s.worker.lock().unwrap();
    if let Some(worker) = handle.take() {
        worker.abort();
    }
    let (commands, rx) = mpsc::channel(8);
    *s.commands.lock().unwrap() = Some(commands);
    let worker_s = s.clone();
    let worker = tokio::spawn(async move {
        if let Err(error) = runtime::run(worker_s.clone(), rx).await {
            worker_s.clear_approvals();
            {
                let mut data = worker_s.data.lock().unwrap();
                if data.continuation.phase.as_deref() == Some("opening") {
                    data.continuation.due_at = None;
                    data.continuation.phase = Some("paused".into());
                    data.continuation.reason=Some("Native resume failed; no fresh context substituted. Resume deliberately after inspection".into());
                }
            }
            worker_s.emit(
                "error",
                json!({"message":runtime::safe_error(&error.to_string())}),
            );
            worker_s.status("error");
        }
    });
    *handle = Some(worker);
}
async fn resume(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    service_restart::allow_start(&app)?;
    let s = session(&app, &id)?;
    if !installed(&s.provider) || !s.cwd.is_dir() {
        return Err(fail(
            StatusCode::CONFLICT,
            "Harness or working directory is unavailable on this host",
        ));
    }
    {
        let sessions = app.sessions.lock().unwrap();
        let mut data = s.data.lock().unwrap();
        if !matches!(data.status.as_str(), "suspended" | "error") {
            return Ok(Json(json!({"accepted":true})));
        }
        if data.storage_error.is_some() {
            return Err(fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Session storage is unavailable",
            ));
        }
        // Exclude this session to avoid taking its data lock twice.
        if sessions
            .values()
            .filter(|other| {
                other.id != s.id
                    && !matches!(
                        other.data.lock().unwrap().status.as_str(),
                        "suspended" | "error" | "closed"
                    )
            })
            .count()
            >= 12
        {
            return Err(fail(StatusCode::CONFLICT, "Limit: 12 active harnesses"));
        }
        data.lifecycle.suspended = false;
        data.status = "resuming".into();
    }
    lifecycle::stop_worker(&s).await;
    s.status("resuming");
    start_worker(&s);
    Ok(Json(json!({"accepted":true})))
}
#[derive(Deserialize)]
struct Prompt {
    #[serde(skip)]
    initiated_by: Option<String>,
    text: String,
    #[serde(rename = "requestId")]
    request_id: String,
}
async fn prompt(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Prompt>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    service_restart::allow_start(&app)?;
    let s = session(&app, &id)?;
    if req.text.trim().is_empty()
        || req.text.len() > 32768
        || req.request_id.len() > 100
        || req.request_id.is_empty()
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Enter a message of up to 32 KB and a request ID",
        ));
    }
    // L-05: a request ID bound into the explicit queue is never also sent directly.
    match input_queue::check_binding(&s, &req.request_id, &req.text)? {
        input_queue::Bound::Duplicate(_) => {
            return Ok(Json(
                json!({"accepted":true,"duplicate":true,"queued":true}),
            ));
        }
        input_queue::Bound::Conflict => return Err(input_queue::conflict()),
        input_queue::Bound::New => {}
    }
    accept_prompt(&s, req.text, req.request_id, req.initiated_by)
}
/// Shared turn acceptance for direct prompts and L-05 queued dispatch. Callers hold
/// `run_lock` and have passed `service_restart::allow_start`.
fn accept_prompt(
    s: &Session,
    text: String,
    request_id: String,
    initiated_by: Option<String>,
) -> ApiResult {
    {
        let mut data = s.data.lock().unwrap();
        if data.requests.contains(&request_id) {
            return Ok(Json(json!({"accepted":true,"duplicate":true})));
        }
        if data.status != "idle" {
            return Err(fail(
                StatusCode::CONFLICT,
                "Wait for this session to become idle",
            ));
        }
        data.inbox_turn = None;
        let commands = data.available_commands.clone();
        data.continuation.begin_turn(&s.provider, &commands, &text);
        data.input_queue.begin_turn(&text);
        data.status = "running".into();
        data.turn_active = true;
        data.requests.insert(request_id.clone());
    }
    s.emit(
        "user",
        json!({"text":text,"requestId":request_id,"initiatedBy":initiated_by}),
    );
    s.emit("status", json!({"status":"running"}));
    if let Err(error) = s.send_command(runtime::Command::Prompt(text)) {
        s.status("error");
        return Err(error);
    }
    Ok(Json(json!({"accepted":true})))
}
async fn cancel(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let restart_error = service_restart::stop_requester(&app, &id).err();
    let s = session(&app, &id)?;
    let inbox_error = inbox_delivery::cancel(&s, "Explicit Stop").err();
    let status = {
        let mut data = s.data.lock().unwrap();
        data.continuation.stop("Explicit Stop");
        data.input_queue
            .block("Explicit Stop; inspect, then resume.");
        let status = data.status.clone();
        if matches!(status.as_str(), "running" | "waiting") {
            data.status = "cancelling".into();
        }
        status
    };
    s.emit("continuation", json!({"decision":"Explicit Stop"}));
    if matches!(status.as_str(), "starting" | "resuming" | "configuring") {
        let _ = lifecycle::suspend_locked(&s).await?;
    } else if status == "running" || status == "waiting" {
        s.emit("status", json!({"status":"cancelling"}));
        if let Err(error) = s.send_command(runtime::Command::Cancel) {
            let _ = lifecycle::suspend_locked(&s).await?;
            return Err(error);
        }
        s.clear_approvals();
    }
    if s.data.lock().unwrap().storage_error.is_some() {
        return Err(fail(
            StatusCode::INSUFFICIENT_STORAGE,
            "Stop requested, but saving failed; inspect server storage",
        ));
    }
    if let Some(error) = inbox_error {
        return Err(error);
    }
    if let Some(error) = restart_error {
        return Err(error);
    }
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct ModelChoice {
    model: String,
}
async fn set_model(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<ModelChoice>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    service_restart::allow_start(&app)?;
    let s = session(&app, &id)?;
    {
        let mut data = s.data.lock().unwrap();
        if data.status != "idle" {
            return Err(fail(
                StatusCode::CONFLICT,
                "Choose a model while the session is idle",
            ));
        }
        if !data.models["options"]
            .as_array()
            .is_some_and(|options| options.iter().any(|o| o["value"] == req.model))
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Choose one of this session's advertised models",
            ));
        }
        data.status = "configuring".into();
    }
    s.emit("status", json!({"status":"configuring"}));
    if let Err(error) = s.send_command(runtime::Command::Model(req.model)) {
        s.status("error");
        return Err(error);
    }
    Ok(Json(json!({"accepted":true})))
}
async fn delete_session(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    delete_session_locked(&app, &s).await?;
    Ok(Json(json!({"ok":true})))
}
/// Callers hold the communication and run locks.
async fn delete_session_locked(app: &App, s: &Session) -> Result<(), (StatusCode, String)> {
    inbox_delivery::revoke_related(app, s, "Session deleted")?;
    let _ = lifecycle::suspend_locked(s).await?;
    app.store.lock().unwrap().delete(&s.id).map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not delete saved session",
        )
    })?;
    app.sessions.lock().unwrap().remove(&s.id);
    s.status("closed");
    Ok(())
}
#[derive(Deserialize)]
struct Decision {
    #[serde(rename = "optionId")]
    option_id: String,
}
async fn approve(
    State(app): State<App>,
    Path((id, approval_id)): Path<(String, String)>,
    Json(req): Json<Decision>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    let s = session(&app, &id)?;
    let approval = {
        let mut pending = s.approvals.lock().unwrap();
        let a = pending
            .get(&approval_id)
            .ok_or_else(|| fail(StatusCode::CONFLICT, "Approval is no longer pending"))?;
        if !a.request["options"]
            .as_array()
            .is_some_and(|opts| opts.iter().any(|o| o["optionId"] == req.option_id))
        {
            return Err(fail(StatusCode::BAD_REQUEST, "Invalid approval option"));
        }
        pending.remove(&approval_id).unwrap()
    };
    let approvals = s.approvals.lock().unwrap();
    let running = {
        let mut data = s.data.lock().unwrap();
        let running = approvals.is_empty() && data.turn_active && data.status == "waiting";
        if running {
            data.status = "running".into();
        }
        running
    };
    drop(approvals);
    approval
        .answer
        .send(Some(req.option_id.clone()))
        .map_err(|_| fail(StatusCode::CONFLICT, "Agent no longer awaits this approval"))?;
    s.emit(
        "approval_resolved",
        json!({"id":approval_id,"optionId":req.option_id}),
    );
    if running {
        // The prompt may have completed immediately after receiving the answer.
        s.emit("status", json!({"status":"running"}));
    }
    Ok(Json(json!({"ok":true})))
}
async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, (StatusCode, String)> {
    let s = match session(&app, &id) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("Session WebSocket route rejected reason=session_not_found");
            return Err(error);
        }
    };
    Ok(ws.on_upgrade(move |socket| {
        eprintln!("Session WebSocket upgraded");
        event_socket(socket, s)
    }))
}

/// U-02 cross-session stream. Carries only noteworthy, policy-allowed events and
/// deliberately no snapshot: a reconnect must not replay notices already seen.
async fn notification_events(
    State(app): State<App>,
    ws: WebSocketUpgrade,
) -> Result<Response, (StatusCode, String)> {
    let receiver = app.notifications.subscribe();
    Ok(ws.on_upgrade(move |socket| notification_socket(socket, receiver)))
}
async fn notification_socket(socket: WebSocket, mut rx: broadcast::Receiver<Value>) {
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            next=rx.recv()=>{
                // A lagging tab loses notices rather than blocking the session that
                // produced them; it re-reads the sidebar on its next poll.
                let Ok(event)=next else {break};
                if sink.send(Message::Text(event.to_string().into())).await.is_err(){break;}
            }
            incoming=stream.next()=>match incoming {
                Some(Ok(Message::Ping(data)))=>{if sink.send(Message::Pong(data)).await.is_err(){break;}}
                Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,
                _=>{}
            }
        }
    }
}
async fn event_socket(socket: WebSocket, s: Arc<Session>) {
    let mut rx = s.tx.subscribe();
    let (mut sink, mut stream) = socket.split();
    let snapshot = s.snapshot();
    let mut seq = snapshot["seq"].as_u64().unwrap_or(0);
    if sink
        .send(Message::Text(snapshot.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            next=rx.recv()=>{
                let event=match next {Ok(event)=>event,Err(broadcast::error::RecvError::Lagged(_))=>s.snapshot(),Err(_)=>break};
                let n=event["seq"].as_u64().unwrap_or(0);
                // L-05 hook: queue state is broadcast unsequenced (not stored in event history).
                if event["kind"]!="input_queue" {if n<=seq {continue;} seq=n;}
                let closed=event["kind"]=="status" && (event["data"]["status"]=="closed" || event["data"]["status"]=="suspended");
                if sink.send(Message::Text(event.to_string().into())).await.is_err(){break;}
                if closed {let _=sink.send(Message::Close(None)).await;break;}
            }
            incoming=stream.next()=>match incoming {
                Some(Ok(Message::Ping(data)))=>{if sink.send(Message::Pong(data)).await.is_err(){break;}}
                Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,
                _=>{}
            }
        }
    }
}
#[derive(Deserialize)]
struct FileQuery {
    path: String,
    download: Option<bool>,
}
async fn file(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(req): Query<FileQuery>,
) -> Result<Response, (StatusCode, String)> {
    let s = session(&app, &id)?;
    workspace::serve_file(&s.cwd, req).await
}

#[derive(Deserialize)]
struct Markdown {
    text: String,
}
async fn markdown(Json(req): Json<Markdown>) -> Json<Value> {
    Json(json!({"html":render_markdown(&req.text)}))
}
fn render_markdown(text: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(
        text,
        pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH,
    );
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    ammonia::Builder::default()
        .url_relative(ammonia::UrlRelative::PassThrough)
        // Agents link host files as file:// URLs; the browser turns local ones
        // into in-app previews instead of navigating.
        .add_url_schemes(&["file"])
        .add_tag_attributes("code", &["class"])
        .attribute_filter(|element, attribute, value| {
            if element != "code" || attribute != "class" {
                return Some(value.into());
            }
            // Keep only fenced-code language hints for display preferences.
            let languages = value
                .split_whitespace()
                .filter(|class| {
                    class.strip_prefix("language-").is_some_and(|language| {
                        (1..=32).contains(&language.len())
                            && language
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"+#._-".contains(&b))
                    })
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!languages.is_empty()).then(|| languages.into())
        })
        .clean(&out)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_checks_follow_bind_address() {
        let local = "127.0.0.1".parse().unwrap();
        let network = "0.0.0.0".parse().unwrap();
        assert!(allowed_host("127.0.0.1:4317", local, 4317));
        assert!(!allowed_host("agentbox:4317", local, 4317));
        assert!(allowed_host("agentbox:4317", network, 4317));
        assert!(allowed_host("100.64.1.2:4317", network, 4317));
        assert!(!allowed_host("agentbox:9999", network, 4317));
        assert!(!allowed_host("", network, 4317));
    }
    #[test]
    fn markdown_removes_active_content() {
        let html = render_markdown(
            "# Hello\n<script>alert(1)</script>\n[x](javascript:alert(1))\n<img src=x onerror=alert(1)>",
        );
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("onerror"));
    }
    #[test]
    fn markdown_keeps_file_links() {
        let html = render_markdown("[a](file:///home/x/a.png) [b](/home/x/b.png)");
        assert!(html.contains(r#"href="file:///home/x/a.png""#));
        assert!(html.contains(r#"href="/home/x/b.png""#));
    }
    #[test]
    fn markdown_keeps_only_code_language_classes() {
        let html = render_markdown(
            "```rust\nfn main() {}\n```\n<code class=\"toast language-x\">a</code><p class=\"language-js\">b</p>",
        );
        assert!(html.contains("<code class=\"language-rust\">"));
        assert!(html.contains("<code class=\"language-x\">"));
        assert!(!html.contains("toast"));
        assert!(!html.contains("<p class"));
    }
}
