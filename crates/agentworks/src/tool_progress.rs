//! U-05 tool progress and inbox message formatting from Starlark scripts (E-01).
//!
//! Scripts are stored in SQLite and compiled once; there is no reloading from
//! disk. The shipped Bazel parser is seeded as a built-in. Tool updates reach a
//! single dedicated interpreter thread from `Session::emit`, so evaluation never runs
//! on async request threads. Derived progress is temporary: it is published as
//! unsequenced `tool_progress` events, included in snapshots, never persisted,
//! and rebuilt from the retained event window after a restart. Inbox formatting
//! uses the same thread and script installation lifecycle, as do tool card
//! headers (`tool_headers`). Raw events and tool status remain authoritative; a
//! parser failure only removes progress.

use agentworks_starlark::engine::{
    Capability, Compiled, Diagnostic, Engine, HookInfo, content_hash,
};
use agentworks_starlark::progress::{StarlarkParser, ToolProgress};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use tokio::sync::broadcast;

use crate::tool_headers::{HeaderTracker, Headers};

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scripts(name TEXT PRIMARY KEY,capability TEXT NOT NULL,source TEXT NOT NULL,hash TEXT NOT NULL,enabled INTEGER NOT NULL,builtin INTEGER NOT NULL,updated INTEGER NOT NULL);";

/// Shipped scripts. A built-in follows the shipped source until a user saves
/// their own version under the same name; it can be disabled but not deleted.
const BUILTIN: &[(&str, &str)] = &[
    (
        "bazel_progress.star",
        agentworks_starlark::BAZEL_PROGRESS_SCRIPT,
    ),
    (
        "inbox_message.star",
        agentworks_starlark::INBOX_MESSAGE_SCRIPT,
    ),
    ("tool_header.star", agentworks_starlark::TOOL_HEADER_SCRIPT),
];

/// Derived views retained per session; the oldest are dropped first.
const MAX_VIEWS_PER_SESSION: usize = 1000;

#[derive(Serialize)]
pub struct Script {
    pub name: String,
    pub capability: String,
    /// Validated installation-time registrations, in declaration order.
    pub hooks: Vec<HookInfo>,
    pub source: String,
    pub hash: String,
    pub enabled: bool,
    /// Follows the shipped source (unmodified built-in).
    pub builtin: bool,
    /// The name of a shipped script, whether or not the user modified it.
    pub shipped: bool,
    pub updated: i64,
    /// Current compile/type error for an enabled script, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn valid_name(name: &str) -> bool {
    let stem = name.strip_suffix(".star").unwrap_or("");
    !stem.is_empty()
        && stem.len() <= 64
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Insert missing built-ins and move unmodified built-ins to the shipped source.
pub fn seed(db: &Connection) -> rusqlite::Result<()> {
    // Preserve saved edits, enable flags and ownership. Legacy assignments become
    // explicit declarations once; filename routing is no longer used.
    let legacy = {
        let mut query = db.prepare("SELECT name,capability,source FROM scripts WHERE capability IN ('parser','message_formatter')")?;
        query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let tx = db.unchecked_transaction()?;
    for (name, capability, mut source) in legacy {
        source.push_str(if capability == "parser" {
            "\nagent.register_progress(on_line, command_pattern = COMMAND_PATTERN, display_name = DISPLAY_NAME)\n"
        } else {
            "\nagent.register_formatter(format_message, message_type = \"inbox.peer\")\n"
        });
        tx.execute(
            "UPDATE scripts SET capability='hooks',source=?2,hash=?3 WHERE name=?1",
            params![name, source, content_hash(&source)],
        )?;
    }
    tx.commit()?;
    for (name, source) in BUILTIN {
        let hash = content_hash(source);
        let existing: Option<(bool, String)> = db
            .query_row(
                "SELECT builtin,hash FROM scripts WHERE name=?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'hooks',?2,?3,1,1,?4)",
                    params![name, source, hash, now()])?;
            }
            Some((true, current)) if current != hash => {
                db.execute(
                    "UPDATE scripts SET source=?2,hash=?3,updated=?4 WHERE name=?1",
                    params![name, source, hash, now()],
                )?;
            }
            Some(_) => {}
        }
    }
    Ok(())
}

pub fn list(db: &Connection) -> rusqlite::Result<Vec<Script>> {
    let mut stmt = db.prepare(
        "SELECT name,capability,source,hash,enabled,builtin,updated FROM scripts ORDER BY name",
    )?;
    stmt.query_map([], |r| {
        Ok(Script {
            name: r.get(0)?,
            capability: r.get(1)?,
            hooks: Vec::new(),
            source: r.get(2)?,
            hash: r.get(3)?,
            enabled: r.get(4)?,
            builtin: r.get(5)?,
            shipped: false,
            updated: r.get(6)?,
            error: None,
        })
        .map(|mut s: Script| {
            s.shipped = BUILTIN.iter().any(|(name, _)| *name == s.name);
            s
        })
    })?
    .collect()
}

/// Save a validated user script. Saving over a built-in makes it user-owned.
pub fn save(db: &Connection, name: &str, source: &str) -> rusqlite::Result<()> {
    db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'hooks',?2,?3,1,0,?4)
        ON CONFLICT(name) DO UPDATE SET source=excluded.source,hash=excluded.hash,capability=excluded.capability,builtin=0,updated=excluded.updated",
        params![name, source, content_hash(source), now()])?;
    Ok(())
}

pub fn set_enabled(db: &Connection, name: &str, enabled: bool) -> rusqlite::Result<bool> {
    Ok(db.execute(
        "UPDATE scripts SET enabled=?2,updated=?3 WHERE name=?1",
        params![name, enabled, now()],
    )? == 1)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    Missing,
    /// An unmodified built-in: disable it instead.
    Builtin,
    Deleted,
    /// A modified built-in went back to the shipped source.
    Restored,
}

/// Delete a user script, or restore a modified built-in to its shipped source.
pub fn delete(db: &Connection, name: &str) -> rusqlite::Result<Removal> {
    let builtin: Option<bool> = db
        .query_row("SELECT builtin FROM scripts WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    let shipped = BUILTIN.iter().find(|(n, _)| *n == name);
    Ok(match (builtin, shipped) {
        (None, _) => Removal::Missing,
        (Some(true), _) => Removal::Builtin,
        (Some(false), Some((_, source))) => {
            db.execute(
                "UPDATE scripts SET source=?2,hash=?3,builtin=1,updated=?4 WHERE name=?1",
                params![name, source, content_hash(source), now()],
            )?;
            Removal::Restored
        }
        (Some(false), None) => {
            db.execute("DELETE FROM scripts WHERE name=?1", [name])?;
            Removal::Deleted
        }
    })
}

type Views = Arc<Mutex<HashMap<String, HashMap<String, (u64, Value)>>>>;

/// Synchronous core of the parser thread; also used directly by tests.
pub struct Tracker {
    parsers: Vec<StarlarkParser>,
    live: HashMap<(String, String), ToolProgress>,
    views: Views,
}

impl Tracker {
    fn new(parsers: Vec<StarlarkParser>, views: Views) -> Self {
        Tracker {
            parsers,
            live: HashMap::new(),
            views,
        }
    }

    /// Apply one persisted ACP update; returns a `tool_progress` event when the
    /// derived view changed.
    pub fn update(&mut self, session: &str, seq: u64, data: &Value) -> Option<Value> {
        if !matches!(
            data["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) {
            return None;
        }
        // Child tool IDs are scoped to their ACP session, not the root parser.
        if data["parentSessionId"].is_string() {
            return None;
        }
        let tool = data["toolCallId"].as_str()?.to_owned();
        let key = (session.to_owned(), tool.clone());
        let progress = self
            .live
            .entry(key.clone())
            .or_insert_with(ToolProgress::without_raw);
        if !progress.apply(self.parsers.as_slice(), data, i64::try_from(seq).ok()) {
            return None;
        }
        let view = progress.derived();
        // Finished tools keep only their final view; their stream state is freed.
        let finished = progress.exit.is_some()
            || !matches!(
                progress.status.as_deref().unwrap_or("pending"),
                "pending" | "in_progress"
            );
        let untracked = progress.command.is_some() && progress.parser.is_none();
        if finished || untracked {
            self.live.remove(&key);
        }
        if view["parser"].is_null() {
            return None;
        }
        let mut views = self.views.lock().unwrap();
        let session_views = views.entry(session.to_owned()).or_default();
        if session_views.get(&tool).is_some_and(|(_, v)| *v == view) {
            return None;
        }
        session_views.insert(tool.clone(), (seq, view.clone()));
        if session_views.len() > MAX_VIEWS_PER_SESSION
            && let Some(oldest) = session_views
                .iter()
                .min_by_key(|(_, (s, _))| *s)
                .map(|(k, _)| k.clone())
        {
            session_views.remove(&oldest);
        }
        let mut data = view;
        data["toolCallId"] = json!(tool);
        Some(json!({"kind": "tool_progress", "seq": seq, "data": data}))
    }

    fn forget(&mut self, session: &str) {
        self.live.retain(|(s, _), _| s != session);
        self.views.lock().unwrap().remove(session);
    }
}

enum Message {
    Update {
        session: String,
        provider: String,
        cwd: String,
        seq: u64,
        data: Value,
        tx: broadcast::Sender<Value>,
    },
    Reload(Hooks),
    FormatInbox {
        messages: Vec<Value>,
        reply: mpsc::SyncSender<Vec<Option<String>>>,
    },
    Forget(String),
}

/// Handle to the parser thread; cheap to clone.
#[derive(Clone)]
pub struct Hub {
    engine: Arc<Engine>,
    tx: mpsc::Sender<Message>,
    views: Views,
    headers: Headers,
}

impl Hub {
    pub fn new(engine: Arc<Engine>, parsers: Vec<StarlarkParser>) -> Self {
        let (views, headers) = (Views::default(), Headers::default());
        let (tx, rx) = mpsc::channel::<Message>();
        let mut formatters: Vec<Arc<Compiled>> = Vec::new();
        let mut tracker = Tracker::new(parsers, views.clone());
        let mut header_tracker = HeaderTracker::new(engine.clone(), headers.clone());
        let interpreter = engine.clone();
        std::thread::Builder::new()
            .name("tool-progress".into())
            .spawn(move || {
                for message in rx {
                    match message {
                        Message::Update {
                            session,
                            provider,
                            cwd,
                            seq,
                            data,
                            tx,
                        } => {
                            if let Some(event) = tracker.update(&session, seq, &data) {
                                let _ = tx.send(event);
                            }
                            if let Some(event) = header_tracker.update(
                                &session,
                                &provider,
                                &cwd,
                                seq,
                                &data,
                                Some(&tx),
                            ) {
                                let _ = tx.send(event);
                            }
                        }
                        Message::Reload(hooks) => {
                            tracker.parsers = hooks.parsers;
                            formatters = hooks.formatters;
                            header_tracker.reload(hooks.headers);
                        }
                        Message::FormatInbox { messages, reply } => {
                            let formatted = messages
                                .iter()
                                .map(|message| {
                                    let script = formatters.iter().find(|script| {
                                        script.message_type.as_deref() == Some("inbox.peer")
                                            && script.text_pattern.as_ref().is_some_and(|pattern| {
                                                pattern.is_match(
                                                    message["text"].as_str().unwrap_or_default(),
                                                )
                                            })
                                    })?;
                                    match interpreter.call(
                                        script,
                                        &json!({"channel":"inbox", "state":{}}),
                                        message,
                                    ) {
                                        Ok((value, _)) => value["result"]
                                            .as_str()
                                            .filter(|s| !s.trim().is_empty())
                                            .map(str::to_owned),
                                        Err(error) => {
                                            eprintln!(
                                                "Inbox formatter failed; using quoted JSON: {error}"
                                            );
                                            None
                                        }
                                    }
                                })
                                .collect();
                            let _ = reply.send(formatted);
                        }
                        Message::Forget(session) => {
                            tracker.forget(&session);
                            header_tracker.forget(&session);
                        }
                    }
                }
            })
            .expect("spawn tool-progress thread");
        Hub {
            engine,
            tx,
            views,
            headers,
        }
    }

    /// A hub with no parsers, for tests and fixtures.
    #[cfg(test)]
    pub fn empty() -> Self {
        Hub::new(Arc::new(Engine::new().expect("host preludes")), Vec::new())
    }

    /// `provider` and `cwd` describe the session for tool header hooks.
    pub fn update(
        &self,
        session: &str,
        (provider, cwd): (&str, &std::path::Path),
        tx: &broadcast::Sender<Value>,
        seq: u64,
        data: &Value,
    ) {
        if matches!(
            data["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) {
            let _ = self.tx.send(Message::Update {
                session: session.to_owned(),
                provider: provider.to_owned(),
                cwd: cwd.to_string_lossy().into_owned(),
                seq,
                data: data.clone(),
                tx: tx.clone(),
            });
        }
    }

    pub fn forget(&self, session: &str) {
        let _ = self.tx.send(Message::Forget(session.to_owned()));
    }

    /// Current views for a snapshot: `{toolCallId: {seq, parser, progress, parserFailure}}`.
    pub fn views(&self, session: &str) -> Value {
        let views = self.views.lock().unwrap();
        let map = views
            .get(session)
            .map(|tools| {
                tools
                    .iter()
                    .map(|(id, (seq, view))| {
                        let mut v = view.clone();
                        v["seq"] = json!(seq);
                        (id.clone(), v)
                    })
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        Value::Object(map)
    }

    /// Current tool card headers for a snapshot, keyed by `tool_headers::key`.
    pub fn headers(&self, session: &str) -> HashMap<String, String> {
        self.headers
            .lock()
            .unwrap()
            .get(session)
            .map(|tools| {
                tools
                    .iter()
                    .map(|(k, (_, h))| (k.clone(), h.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Run message formatting on the interpreter thread, away from request threads.
    /// Timeout, disabled script or evaluation failure falls back to host-quoted JSON.
    pub fn format_inbox(&self, messages: &[Value]) -> Vec<Option<String>> {
        let (reply, rx) = mpsc::sync_channel(1);
        if self
            .tx
            .send(Message::FormatInbox {
                messages: messages.to_vec(),
                reply,
            })
            .is_err()
        {
            return vec![None; messages.len()];
        }
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_else(|_| vec![None; messages.len()])
    }

    /// Validate a script before it is saved.
    pub fn check(&self, name: &str, source: &str) -> Result<(), Diagnostic> {
        self.engine.compile_registered(name, source).map(|_| ())
    }

    /// Recompile enabled scripts from the store and swap them in.
    pub fn reload(&self, db: &Connection) -> rusqlite::Result<Vec<Diagnostic>> {
        let (hooks, errors) = compile_hooks(&self.engine, db)?;
        let _ = self.tx.send(Message::Reload(hooks));
        Ok(errors)
    }
}

/// Enabled handlers by hook, in routing order.
#[derive(Default)]
struct Hooks {
    parsers: Vec<StarlarkParser>,
    formatters: Vec<Arc<Compiled>>,
    headers: Vec<Arc<Compiled>>,
}

/// Compile enabled modules in filename order, then route their declarations in
/// source order. One broken module contributes no handlers to any hook.
fn compile_hooks(
    engine: &Arc<Engine>,
    db: &Connection,
) -> rusqlite::Result<(Hooks, Vec<Diagnostic>)> {
    let (mut hooks, mut errors) = (Hooks::default(), Vec::new());
    for script in list(db)?.into_iter().filter(|s| s.enabled) {
        match engine.compile_registered(&script.name, &script.source) {
            Ok(compiled) => {
                for handler in &compiled.handlers {
                    match handler.capability {
                        Capability::Parser => hooks.parsers.push(StarlarkParser {
                            engine: engine.clone(),
                            script: handler.clone(),
                        }),
                        Capability::MessageFormatter => hooks.formatters.push(handler.clone()),
                        Capability::ToolHeader => hooks.headers.push(handler.clone()),
                        Capability::Workflow => {
                            unreachable!("workflow registrations are not exposed")
                        }
                    }
                }
            }
            Err(error) => errors.push(error),
        }
    }
    Ok((hooks, errors))
}

#[cfg(test)]
fn compile_enabled(
    engine: &Arc<Engine>,
    db: &Connection,
) -> rusqlite::Result<(Vec<StarlarkParser>, Vec<Diagnostic>)> {
    let (hooks, errors) = compile_hooks(engine, db)?;
    Ok((hooks.parsers, errors))
}

// HTTP API. Scripts are trusted (single-user host); validation guards mistakes.

#[derive(serde::Deserialize)]
pub struct Source {
    source: String,
}

#[derive(serde::Deserialize)]
pub struct Enabled {
    enabled: bool,
}

fn db_error(e: rusqlite::Error) -> (axum::http::StatusCode, String) {
    crate::fail(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Script storage failed: {e}"),
    )
}

/// Scripts with the current compile error of each enabled one.
fn annotated(app: &crate::App, db: &Connection) -> crate::ApiResult {
    let mut scripts = list(db).map_err(db_error)?;
    for script in &mut scripts {
        let compiled = if script.enabled {
            Some(
                app.progress
                    .engine
                    .compile_registered(&script.name, &script.source),
            )
        } else {
            app.progress
                .engine
                .cached_registered(&script.name, &content_hash(&script.source))
                .map(Ok)
        };
        match compiled {
            Some(Ok(compiled)) => script.hooks = compiled.hooks.clone(),
            Some(Err(error)) => script.error = Some(error.to_string()),
            None => {}
        }
    }
    Ok(axum::Json(json!({"scripts": scripts})))
}

fn reloaded(app: &crate::App, db: &Connection) -> crate::ApiResult {
    app.progress.reload(db).map_err(db_error)?;
    annotated(app, db)
}

pub async fn get_scripts(
    axum::extract::State(app): axum::extract::State<crate::App>,
) -> crate::ApiResult {
    let store = app.store.lock().unwrap();
    annotated(&app, &store.connection)
}

/// Install or replace a script after validating all of its hook registrations.
pub async fn put_script(
    axum::extract::State(app): axum::extract::State<crate::App>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::Json(body): axum::Json<Source>,
) -> crate::ApiResult {
    use axum::http::StatusCode;
    if !valid_name(&name) {
        return Err(crate::fail(
            StatusCode::BAD_REQUEST,
            "Script names are lowercase letters, digits, - or _ ending in .star",
        ));
    }
    app.progress
        .check(&name, &body.source)
        .map_err(|d| crate::fail(StatusCode::BAD_REQUEST, d.to_string()))?;
    let store = app.store.lock().unwrap();
    save(&store.connection, &name, &body.source).map_err(db_error)?;
    reloaded(&app, &store.connection)
}

pub async fn put_enabled(
    axum::extract::State(app): axum::extract::State<crate::App>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::Json(body): axum::Json<Enabled>,
) -> crate::ApiResult {
    let store = app.store.lock().unwrap();
    if !set_enabled(&store.connection, &name, body.enabled).map_err(db_error)? {
        return Err(crate::fail(
            axum::http::StatusCode::NOT_FOUND,
            "Script not found",
        ));
    }
    reloaded(&app, &store.connection)
}

pub async fn delete_script(
    axum::extract::State(app): axum::extract::State<crate::App>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> crate::ApiResult {
    use axum::http::StatusCode;
    let store = app.store.lock().unwrap();
    match delete(&store.connection, &name).map_err(db_error)? {
        Removal::Missing => Err(crate::fail(StatusCode::NOT_FOUND, "Script not found")),
        Removal::Builtin => Err(crate::fail(
            StatusCode::CONFLICT,
            "Built-in scripts can be disabled, not deleted",
        )),
        Removal::Deleted | Removal::Restored => reloaded(&app, &store.connection),
    }
}

/// Seed built-ins and start the interpreter thread with the enabled scripts.
pub fn start(db: &Connection) -> Result<Hub, String> {
    seed(db).map_err(|e| e.to_string())?;
    let engine = Arc::new(Engine::new()?);
    let hub = Hub::new(engine, Vec::new());
    for error in hub.reload(db).map_err(|e| e.to_string())? {
        eprintln!("Script disabled for this run: {error}");
    }
    Ok(hub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        seed(&db).unwrap();
        db
    }

    fn tracker(db: &Connection) -> Tracker {
        let engine = Arc::new(Engine::new().unwrap());
        let (parsers, errors) = compile_enabled(&engine, db).unwrap();
        assert!(errors.is_empty());
        Tracker::new(parsers, Views::default())
    }

    fn fixture() -> Vec<Value> {
        serde_json::from_str(include_str!("../../../tests/fixtures/bazel-events.json")).unwrap()
    }

    /// Server output for the U-05 fixture. The browser tests replay this golden
    /// file, so they exercise the same events the service publishes.
    #[test]
    fn u05_fixture_matches_golden_progress_events() {
        let mut t = tracker(&db());
        let mut published = Vec::new();
        for event in fixture().iter().filter(|e| e["kind"] == "acp") {
            let seq = event["seq"].as_u64().unwrap();
            if let Some(p) = t.update("s", seq, &event["data"]) {
                published.push(p);
            }
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/bazel-progress-events.json"
        );
        let actual = serde_json::to_string_pretty(&published).unwrap() + "\n";
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(path, &actual).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            actual,
            "regenerate with UPDATE_GOLDEN=1 cargo test u05_fixture"
        );
        let last = &published.last().unwrap()["data"];
        assert_eq!(last["parser"], "Bazel");
        assert_eq!(last["progress"]["phase"], "Testing");
        assert!(t.live.is_empty(), "finished tool state is freed");
        assert_eq!(t.views.lock().unwrap()["s"].len(), 1);
    }

    #[test]
    fn duplicates_unrelated_tools_and_unchanged_views_publish_nothing() {
        let mut t = tracker(&db());
        let call = json!({"sessionUpdate":"tool_call","toolCallId":"b","rawInput":{"command":"bazel build //..."},"status":"in_progress"});
        let delta = |d: &str| json!({"sessionUpdate":"tool_call_update","toolCallId":"b","_meta":{"terminal_output_delta":{"terminal_id":"t","data":d}}});
        let first = t.update("s", 1, &call).expect("parser selected");
        assert_eq!(
            (
                first["data"]["parser"].as_str(),
                first["data"]["progress"].is_null()
            ),
            (Some("Bazel"), true)
        );
        assert!(t.update("s", 2, &delta("[1 / 3] a\r")).is_some());
        assert!(
            t.update("s", 2, &delta("[1 / 3] a\r")).is_none(),
            "duplicate seq"
        );
        assert!(
            t.update("s", 3, &delta("unrelated\r")).is_none(),
            "view unchanged"
        );
        let other = json!({"sessionUpdate":"tool_call","toolCallId":"m","rawInput":{"command":"make"},"status":"in_progress"});
        assert!(t.update("s", 4, &other).is_none());
        assert!(
            !t.live.contains_key(&("s".into(), "m".into())),
            "untracked tools hold no state"
        );
        assert!(
            t.update("s", 5, &json!({"sessionUpdate":"agent_message_chunk"}))
                .is_none()
        );
        t.forget("s");
        assert!(t.views.lock().unwrap().is_empty() && t.live.is_empty());
    }

    #[test]
    fn legacy_scripts_gain_registrations_once_without_losing_edits_or_enable_flags() {
        let db = db();
        let source = agentworks_starlark::BAZEL_PROGRESS_SCRIPT
            .split("\nagent.register_progress")
            .next()
            .unwrap()
            .replace("DISPLAY_NAME = \"Bazel\"", "DISPLAY_NAME = \"Custom\"");
        db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES ('legacy.star','parser',?1,?2,0,0,123)", params![source, content_hash(&source)]).unwrap();
        let formatter_source = agentworks_starlark::INBOX_MESSAGE_SCRIPT
            .split("\nagent.register_formatter")
            .next()
            .unwrap();
        db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES ('old_formatter.star','message_formatter',?1,?2,1,0,456)", params![formatter_source, content_hash(formatter_source)]).unwrap();
        seed(&db).unwrap();
        seed(&db).unwrap();
        let scripts = list(&db).unwrap();
        let legacy = scripts.iter().find(|s| s.name == "legacy.star").unwrap();
        assert_eq!(legacy.capability, "hooks");
        assert!(!legacy.enabled && !legacy.builtin);
        assert_eq!(legacy.updated, 123);
        assert!(legacy.source.starts_with(&source));
        assert_eq!(legacy.source.matches("agent.register_progress").count(), 1);
        assert_eq!(legacy.hash, content_hash(&legacy.source));
        let engine = Engine::new().unwrap();
        let compiled = engine
            .compile_registered(&legacy.name, &legacy.source)
            .unwrap();
        assert_eq!(compiled.handlers[0].display_name, "Custom");
        let old_formatter = scripts
            .iter()
            .find(|s| s.name == "old_formatter.star")
            .unwrap();
        assert_eq!(
            engine
                .compile_registered(&old_formatter.name, &old_formatter.source)
                .unwrap()
                .hooks[0]
                .message_type
                .as_deref(),
            Some("inbox.peer")
        );
    }

    #[test]
    fn modules_route_registered_hooks_by_filter_and_filename_not_reserved_names() {
        let db = db();
        let custom_formatter = agentworks_starlark::INBOX_MESSAGE_SCRIPT
            .replace("text = message.text", "text = \"Custom: \" + message.text")
            .replace(
                "message_type = \"inbox.peer\"",
                "message_type = \"inbox.peer\", text_pattern = \"^Special\"",
            );
        let mixed = format!(
            "{}\n{}",
            agentworks_starlark::BAZEL_PROGRESS_SCRIPT,
            custom_formatter
        );
        save(&db, "a_handlers.star", &mixed).unwrap();
        let hub = start(&db).unwrap();
        let message = |text| json!({"id":1,"sender":"peer","senderTitle":"Peer","text":text});
        assert_eq!(
            hub.format_inbox(&[message("Special review"), message("Other review")]),
            vec![
                Some("Custom: Special review".into()),
                Some("Other review".into())
            ]
        );
        let (hooks, errors) = compile_hooks(&hub.engine, &db).unwrap();
        assert!(errors.is_empty());
        assert_eq!(hooks.parsers.len(), 2);
        assert_eq!(hooks.formatters.len(), 2);
        assert_eq!(hooks.headers.len(), 1, "the built-in tool header script");
        assert_eq!(hooks.parsers[0].script.name, "a_handlers.star");
        set_enabled(&db, "a_handlers.star", false).unwrap();
        hub.reload(&db).unwrap();
        assert_eq!(
            hub.format_inbox(&[message("Special review")]),
            vec![Some("Special review".into())]
        );
        // First match owns the event even on failure; errors use the safe host fallback.
        let broken = mixed.replace("text = \"Custom: \" + message.text", "text = str(1 // 0)");
        save(&db, "a_handlers.star", &broken).unwrap();
        set_enabled(&db, "a_handlers.star", true).unwrap();
        hub.reload(&db).unwrap();
        assert_eq!(hub.format_inbox(&[message("Special review")]), vec![None]);
    }

    #[test]
    fn inbox_formatter_edits_disable_restore_and_failures() {
        let db = db();
        let hub = start(&db).unwrap();
        let messages = vec![
            json!({"id":187,"sender":"peer","senderTitle":"UI Hover","text":"Please review.\n\n- Branch: main", "truncated":true}),
        ];
        let formatted = hub.format_inbox(&messages).remove(0).unwrap();
        assert!(formatted.starts_with("Please review.\n\n- Branch: main"));
        assert!(formatted.contains("Message truncated"));
        let source = agentworks_starlark::INBOX_MESSAGE_SCRIPT.replace(
            "text = message.text",
            "text = \"**Review request**\\n\\n\" + message.text",
        );
        hub.check("inbox_message.star", &source).unwrap();
        save(&db, "inbox_message.star", &source).unwrap();
        hub.reload(&db).unwrap();
        assert!(
            hub.format_inbox(&messages)[0]
                .as_ref()
                .unwrap()
                .starts_with("**Review request**")
        );
        seed(&db).unwrap();
        assert!(!list(&db).unwrap()[1].builtin);
        set_enabled(&db, "inbox_message.star", false).unwrap();
        hub.reload(&db).unwrap();
        assert_eq!(hub.format_inbox(&messages), vec![None]);
        set_enabled(&db, "inbox_message.star", true).unwrap();
        let broken = agentworks_starlark::INBOX_MESSAGE_SCRIPT
            .replace("text = message.text", "text = str(1 // 0)");
        hub.check("inbox_message.star", &broken).unwrap();
        save(&db, "inbox_message.star", &broken).unwrap();
        hub.reload(&db).unwrap();
        assert_eq!(hub.format_inbox(&messages), vec![None]);
        assert_eq!(
            delete(&db, "inbox_message.star").unwrap(),
            Removal::Restored
        );
        hub.reload(&db).unwrap();
        assert_eq!(hub.format_inbox(&messages), vec![Some(formatted)]);
        assert!(
            hub.check("anything.star", agentworks_starlark::BAZEL_PROGRESS_SCRIPT)
                .is_ok()
        );
    }

    #[test]
    fn builtins_are_seeded_updated_disabled_and_user_owned_when_saved() {
        let db = db();
        let rows = list(&db).unwrap();
        assert_eq!(rows.len(), BUILTIN.len());
        assert!(rows[0].builtin && rows[0].enabled);
        db.execute("UPDATE scripts SET source='old',hash='old'", [])
            .unwrap();
        seed(&db).unwrap();
        assert_eq!(
            list(&db).unwrap()[0].source,
            BUILTIN[0].1,
            "unmodified built-ins follow the shipped source"
        );
        assert!(set_enabled(&db, "bazel_progress.star", false).unwrap());
        let engine = Arc::new(Engine::new().unwrap());
        assert!(compile_enabled(&engine, &db).unwrap().0.is_empty());
        assert_eq!(
            delete(&db, "bazel_progress.star").unwrap(),
            Removal::Builtin
        );
        let edited = BUILTIN[0].1.replace("\"Bazel\"", "\"Blaze\"");
        save(&db, "bazel_progress.star", &edited).unwrap();
        seed(&db).unwrap();
        let row = list(&db).unwrap().remove(0);
        assert!(
            !row.builtin && row.shipped && row.source == edited,
            "a saved built-in is no longer overwritten"
        );
        assert_eq!(
            delete(&db, "bazel_progress.star").unwrap(),
            Removal::Restored
        );
        let row = list(&db).unwrap().remove(0);
        assert!(
            row.builtin && row.source == BUILTIN[0].1,
            "restoring returns to the shipped source"
        );
        save(&db, "make.star", "x").unwrap();
        assert!(!list(&db).unwrap()[2].shipped);
        assert_eq!(delete(&db, "make.star").unwrap(), Removal::Deleted);
        assert_eq!(delete(&db, "missing.star").unwrap(), Removal::Missing);
        assert!(
            valid_name("make_progress.star") && !valid_name("../x.star") && !valid_name("x.py")
        );
    }
}
