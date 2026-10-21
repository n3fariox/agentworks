//! U-05 tool progress from installed Starlark parser scripts (E-01).
//!
//! Scripts are stored in SQLite and compiled once; there is no reloading from
//! disk. The shipped Bazel parser is seeded as a built-in. Tool updates reach a
//! single dedicated parser thread from `Session::emit`, so evaluation never runs
//! on async request threads. Derived progress is temporary: it is published as
//! unsequenced `tool_progress` events, included in snapshots, never persisted,
//! and rebuilt from the retained event window after a restart. Raw events and
//! tool status remain authoritative; a parser failure only removes progress.

use agentworks_starlark::engine::{Capability, Diagnostic, Engine, Installation, content_hash};
use agentworks_starlark::progress::{StarlarkParser, ToolProgress};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use tokio::sync::broadcast;

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scripts(name TEXT PRIMARY KEY,capability TEXT NOT NULL,source TEXT NOT NULL,hash TEXT NOT NULL,enabled INTEGER NOT NULL,builtin INTEGER NOT NULL,updated INTEGER NOT NULL);";

/// Shipped scripts. A built-in follows the shipped source until a user saves
/// their own version under the same name; it can be disabled but not deleted.
const BUILTIN: &[(&str, &str)] = &[(
    "bazel_progress.star",
    include_str!("../crates/starlark/scripts/bazel_progress.star"),
)];
/// Derived views retained per session; the oldest are dropped first.
const MAX_VIEWS_PER_SESSION: usize = 1000;

#[derive(Serialize)]
pub struct Script {
    pub name: String,
    pub capability: String,
    pub source: String,
    pub hash: String,
    pub enabled: bool,
    pub builtin: bool,
    pub updated: i64,
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
                db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'parser',?2,?3,1,1,?4)",
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
            source: r.get(2)?,
            hash: r.get(3)?,
            enabled: r.get(4)?,
            builtin: r.get(5)?,
            updated: r.get(6)?,
        })
    })?
    .collect()
}

/// Save a validated user script. Saving over a built-in makes it user-owned.
pub fn save(db: &Connection, name: &str, source: &str) -> rusqlite::Result<()> {
    db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'parser',?2,?3,1,0,?4)
        ON CONFLICT(name) DO UPDATE SET source=excluded.source,hash=excluded.hash,builtin=0,updated=excluded.updated",
        params![name, source, content_hash(source), now()])?;
    Ok(())
}

pub fn set_enabled(db: &Connection, name: &str, enabled: bool) -> rusqlite::Result<bool> {
    Ok(db.execute(
        "UPDATE scripts SET enabled=?2,updated=?3 WHERE name=?1",
        params![name, enabled, now()],
    )? == 1)
}

/// Returns `None` when absent and `Some(false)` for a built-in, which cannot be deleted.
pub fn delete(db: &Connection, name: &str) -> rusqlite::Result<Option<bool>> {
    let builtin: Option<bool> = db
        .query_row("SELECT builtin FROM scripts WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    match builtin {
        Some(false) => {
            db.execute("DELETE FROM scripts WHERE name=?1", [name])?;
            Ok(Some(true))
        }
        other => Ok(other.map(|_| false)),
    }
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
        seq: u64,
        data: Value,
        tx: broadcast::Sender<Value>,
    },
    Reload(Vec<StarlarkParser>),
    Forget(String),
}

/// Handle to the parser thread; cheap to clone.
#[derive(Clone)]
pub struct Hub {
    engine: Arc<Engine>,
    tx: mpsc::Sender<Message>,
    views: Views,
}

impl Hub {
    pub fn new(engine: Arc<Engine>, parsers: Vec<StarlarkParser>) -> Self {
        let views = Views::default();
        let (tx, rx) = mpsc::channel::<Message>();
        let mut tracker = Tracker::new(parsers, views.clone());
        std::thread::Builder::new()
            .name("tool-progress".into())
            .spawn(move || {
                for message in rx {
                    match message {
                        Message::Update {
                            session,
                            seq,
                            data,
                            tx,
                        } => {
                            if let Some(event) = tracker.update(&session, seq, &data) {
                                let _ = tx.send(event);
                            }
                        }
                        Message::Reload(parsers) => tracker.parsers = parsers,
                        Message::Forget(session) => tracker.forget(&session),
                    }
                }
            })
            .expect("spawn tool-progress thread");
        Hub { engine, tx, views }
    }

    /// A hub with no parsers, for tests and fixtures.
    #[cfg(test)]
    pub fn empty() -> Self {
        Hub::new(Arc::new(Engine::new().expect("host preludes")), Vec::new())
    }

    pub fn update(&self, session: &str, tx: &broadcast::Sender<Value>, seq: u64, data: &Value) {
        if matches!(
            data["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) {
            let _ = self.tx.send(Message::Update {
                session: session.to_owned(),
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

    /// Validate a parser script before it is saved.
    pub fn check(&self, name: &str, source: &str) -> Result<(), Diagnostic> {
        self.engine
            .compile(&Installation::of(name, Capability::Parser, source), source)
            .map(|_| ())
    }

    /// Recompile enabled scripts from the store and swap them in.
    pub fn reload(&self, db: &Connection) -> rusqlite::Result<Vec<Diagnostic>> {
        let (parsers, errors) = compile_enabled(&self.engine, db)?;
        let _ = self.tx.send(Message::Reload(parsers));
        Ok(errors)
    }
}

/// Compile enabled parser scripts in name order. Scripts that no longer compile
/// are skipped with a diagnostic rather than blocking startup.
pub fn compile_enabled(
    engine: &Arc<Engine>,
    db: &Connection,
) -> rusqlite::Result<(Vec<StarlarkParser>, Vec<Diagnostic>)> {
    let (mut parsers, mut errors) = (Vec::new(), Vec::new());
    for script in list(db)?
        .into_iter()
        .filter(|s| s.enabled && s.capability == "parser")
    {
        let install = Installation::of(&script.name, Capability::Parser, &script.source);
        match engine.compile(&install, &script.source) {
            Ok(compiled) => parsers.push(StarlarkParser {
                engine: engine.clone(),
                script: compiled,
            }),
            Err(d) => errors.push(d),
        }
    }
    Ok((parsers, errors))
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

fn reloaded(app: &crate::App, db: &Connection) -> crate::ApiResult {
    let errors = app.progress.reload(db).map_err(db_error)?;
    let scripts = list(db).map_err(db_error)?;
    Ok(axum::Json(
        json!({"scripts": scripts, "errors": errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()}),
    ))
}

pub async fn get_scripts(
    axum::extract::State(app): axum::extract::State<crate::App>,
) -> crate::ApiResult {
    let store = app.store.lock().unwrap();
    let scripts = list(&store.connection).map_err(db_error)?;
    Ok(axum::Json(json!({"scripts": scripts})))
}

/// Install or replace a parser script after compiling and type-checking it.
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
        None => Err(crate::fail(StatusCode::NOT_FOUND, "Script not found")),
        Some(false) => Err(crate::fail(
            StatusCode::CONFLICT,
            "Built-in scripts can be disabled, not deleted",
        )),
        Some(true) => reloaded(&app, &store.connection),
    }
}

/// Seed built-ins and start the parser thread with the enabled scripts.
pub fn start(db: &Connection) -> Result<Hub, String> {
    seed(db).map_err(|e| e.to_string())?;
    let engine = Arc::new(Engine::new()?);
    let (parsers, errors) = compile_enabled(&engine, db).map_err(|e| e.to_string())?;
    for error in errors {
        eprintln!("Parser script disabled for this run: {error}");
    }
    Ok(Hub::new(engine, parsers))
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
        serde_json::from_str(include_str!("../tests/fixtures/bazel-events.json")).unwrap()
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
            "/tests/fixtures/bazel-progress-events.json"
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
    fn builtins_are_seeded_updated_disabled_and_user_owned_when_saved() {
        let db = db();
        let rows = list(&db).unwrap();
        assert_eq!(rows.len(), 1);
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
        assert_eq!(delete(&db, "bazel_progress.star").unwrap(), Some(false));
        let edited = BUILTIN[0].1.replace("\"Bazel\"", "\"Blaze\"");
        save(&db, "bazel_progress.star", &edited).unwrap();
        seed(&db).unwrap();
        let row = list(&db).unwrap().remove(0);
        assert!(
            !row.builtin && row.source == edited,
            "a saved built-in is no longer overwritten"
        );
        assert_eq!(delete(&db, "missing.star").unwrap(), None);
        assert!(
            valid_name("make_progress.star") && !valid_name("../x.star") && !valid_name("x.py")
        );
    }
}
