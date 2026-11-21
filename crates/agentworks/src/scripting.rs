//! HTTP and application adapters for the scripting subsystem.
use agentworks_scripting::{Hub, valid_name};
use agentworks_scripting::{
    PRIORITY_RANGE, Removal, delete, save, seed, set_enabled, set_priority,
};
use agentworks_starlark::engine::Engine;
use rusqlite::Connection;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

// HTTP API. Scripts are trusted (single-user host); validation guards mistakes.

#[derive(serde::Deserialize)]
pub struct Source {
    source: String,
}

#[derive(serde::Deserialize)]
pub struct Enabled {
    enabled: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Priority {
    priority: i64,
}

fn ensure_hook(db: &Connection, name: &str) -> Result<(), (axum::http::StatusCode, String)> {
    let automation: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM scripts WHERE name=?1 AND capability='automation')",
            [name],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if automation {
        return Err(crate::fail(
            axum::http::StatusCode::CONFLICT,
            "Manage automation scripts from the Automations tab",
        ));
    }
    Ok(())
}

fn db_error(e: rusqlite::Error) -> (axum::http::StatusCode, String) {
    crate::fail(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Script storage failed: {e}"),
    )
}

/// Scripts with the current compile error of each enabled one.
fn annotated(app: &crate::App, db: &Connection) -> crate::ApiResult {
    let scripts = app.scripts.scripts(db).map_err(db_error)?;
    Ok(axum::Json(json!({"scripts": scripts})))
}

fn reloaded(app: &crate::App, db: &Connection) -> crate::ApiResult {
    app.scripts.reload(db).map_err(db_error)?;
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
    ensure_hook(&app.store.lock().unwrap().connection, &name)?;
    app.scripts
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
    ensure_hook(&store.connection, &name)?;
    if !set_enabled(&store.connection, &name, body.enabled).map_err(db_error)? {
        return Err(crate::fail(
            axum::http::StatusCode::NOT_FOUND,
            "Script not found",
        ));
    }
    reloaded(&app, &store.connection)
}

/// Human-only routing control; scripts cannot set their own precedence.
pub async fn put_priority(
    axum::extract::State(app): axum::extract::State<crate::App>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::Json(body): axum::Json<Priority>,
) -> crate::ApiResult {
    use axum::http::StatusCode;
    if !PRIORITY_RANGE.contains(&body.priority) {
        return Err(crate::fail(
            StatusCode::BAD_REQUEST,
            "Script priority must be an integer from -1000 to 1000",
        ));
    }
    let store = app.store.lock().unwrap();
    ensure_hook(&store.connection, &name)?;
    if !set_priority(&store.connection, &name, body.priority).map_err(db_error)? {
        return Err(crate::fail(StatusCode::NOT_FOUND, "Script not found"));
    }
    reloaded(&app, &store.connection)
}

pub async fn delete_script(
    axum::extract::State(app): axum::extract::State<crate::App>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> crate::ApiResult {
    use axum::http::StatusCode;
    let store = app.store.lock().unwrap();
    ensure_hook(&store.connection, &name)?;
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
    crate::automations::migrate_scripts(db, &engine)?;
    let hub = Hub::new(engine, Vec::new());
    for error in hub.reload(db).map_err(|e| e.to_string())? {
        eprintln!("Script disabled for this run: {error}");
    }
    Ok(hub)
}

pub fn bind_badge_sessions(hub: &Hub, sessions: &Arc<Mutex<HashMap<String, Arc<crate::Session>>>>) {
    let registry = Arc::downgrade(sessions);
    let failed = Mutex::new(HashSet::new());
    hub.engine()
        .set_badge_sink(Arc::new(move |id, source, hash, effects| {
            let Some(registry) = registry.upgrade() else {
                return;
            };
            let target = registry.lock().unwrap().get(id).cloned();
            if let Some(session) = target {
                if let Err(error) = crate::session_badges::apply(&session, source, hash, effects) {
                    if failed.lock().unwrap().insert(format!("{source}@{hash}")) {
                        eprintln!("Badge update from {source} failed: {error}");
                    }
                }
            }
        }));
}
