//! One service-owned shell per session or host. Ghostty alone interprets PTY output and
//! answers host queries; browsers receive presentation updates, never raw output.
use crate::{App, Session, fail};
pub use agentworks_terminal::{Frame, Handle, Input, spawn};
use axum::{
    Json,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

static STOPPING: AtomicBool = AtomicBool::new(false);
static TERMINALS: OnceLock<Mutex<HashMap<String, Arc<Handle>>>> = OnceLock::new();
fn registry() -> &'static Mutex<HashMap<String, Arc<Handle>>> {
    TERMINALS.get_or_init(Default::default)
}

pub async fn shutdown() {
    STOPPING.store(true, Ordering::Release);
    let handles: Vec<_> = registry().lock().unwrap().values().cloned().collect();
    for handle in &handles {
        handle.stopped.store(true, Ordering::Release);
    }
    for handle in handles {
        let mut frames = handle.frames.clone();
        while frames.changed().await.is_ok() {}
    }
}

fn saved_handle(frame: Frame) -> Arc<Handle> {
    let (input, _incoming) = mpsc::channel(1);
    let (_sender, frames) = watch::channel(frame);
    Arc::new(Handle {
        input,
        frames,
        stopped: Arc::new(AtomicBool::new(true)),
    })
}
/// Callers hold the app run lock, preventing an open/delete/archive race.
pub async fn stop(id: &str, forget: bool) -> Option<Frame> {
    let handle = registry().lock().unwrap().get(id).cloned();
    let snapshot = if let Some(handle) = handle {
        handle.stopped.store(true, Ordering::Release);
        let mut frames = handle.frames.clone();
        while frames.changed().await.is_ok() {}
        let mut snapshot = frames.borrow().clone();
        snapshot.delta.clear();
        Some(snapshot)
    } else {
        None
    };
    if forget {
        registry().lock().unwrap().remove(id);
    }
    snapshot
}
pub async fn archive(session: &Session) -> crate::ApiResult {
    if let Some(frame) = stop(&session.id, false).await {
        session
            .store
            .lock()
            .unwrap()
            .save_terminal(&session.id, &frame)
            .map_err(|_| {
                fail(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "Terminal stopped, but saving its history failed",
                )
            })?;
        // Retain just the presentation snapshot; no PTY, emulator or worker.
        registry()
            .lock()
            .unwrap()
            .insert(session.id.clone(), saved_handle(frame));
    }
    Ok(Json(serde_json::json!({"ok":true})))
}
#[derive(Default, Deserialize)]
pub struct OpenRequest {
    #[serde(default)]
    restart: bool,
}
pub async fn open(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<OpenRequest>,
) -> crate::ApiResult {
    let _run = app.run_lock.lock().await;
    let session = app
        .sessions
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Session not found"))?;
    let archived = session.data.lock().unwrap().lifecycle.archived;
    let mut terminals = registry().lock().unwrap();
    if STOPPING.load(Ordering::Acquire) {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "The service is stopping",
        ));
    }
    if archived && request.restart {
        return Err(fail(
            StatusCode::CONFLICT,
            "Unarchive the session before starting a terminal",
        ));
    }
    if !request.restart || archived {
        if !terminals.contains_key(&id) {
            if let Some(frame) = session.store.lock().unwrap().terminal(&id).map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not load saved terminal history",
                )
            })? {
                terminals.insert(id.clone(), saved_handle(frame));
            }
        }
        if terminals.contains_key(&id) {
            return Ok(Json(
                serde_json::json!({"ok":true,"available":true,"archived":archived}),
            ));
        }
    }
    if archived {
        return Ok(Json(
            serde_json::json!({"ok":true,"available":false,"archived":true}),
        ));
    }
    if terminals
        .get(&id)
        .is_some_and(|handle| !handle.frames.borrow().exited)
    {
        return Err(fail(StatusCode::CONFLICT, "The terminal is still running"));
    }
    crate::service_restart::allow_start(&app)?;
    let handle = if session.host_id == "local" {
        spawn(session.cwd.clone())
    } else {
        app.hosts
            .live(&session.host_id)
            .ok_or_else(|| "The host is offline.".to_string())
            .and_then(|host| host.terminal(&session.cwd.to_string_lossy()))
    }
    .map_err(|error| fail(StatusCode::BAD_REQUEST, error))?;
    session
        .store
        .lock()
        .unwrap()
        .clear_terminal(&id)
        .map_err(|_| {
            fail(
                StatusCode::INSUFFICIENT_STORAGE,
                "Could not replace saved terminal history",
            )
        })?;
    terminals.insert(id, Arc::new(handle));
    Ok(Json(
        serde_json::json!({"ok":true,"available":true,"archived":false}),
    ))
}
fn host_key(id: &str) -> String {
    format!("host:{id}")
}
fn require_host(app: &App, id: &str) -> std::result::Result<(), (StatusCode, String)> {
    if id == "local"
        || app
            .catalog
            .lock()
            .unwrap()
            .data
            .hosts
            .iter()
            .any(|host| host.id == id)
    {
        Ok(())
    } else {
        Err(fail(StatusCode::NOT_FOUND, "Host not found"))
    }
}
pub async fn open_host(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<OpenRequest>,
) -> crate::ApiResult {
    let _run = app.run_lock.lock().await;
    require_host(&app, &id)?;
    if STOPPING.load(Ordering::Acquire) {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "The service is stopping",
        ));
    }
    let key = host_key(&id);
    let mut terminals = registry().lock().unwrap();
    if let Some(handle) = terminals.get(&key) {
        if !request.restart {
            return Ok(Json(serde_json::json!({"ok":true,"available":true})));
        }
        if !handle.frames.borrow().exited {
            return Err(fail(StatusCode::CONFLICT, "The terminal is still running"));
        }
    }
    crate::service_restart::allow_start(&app)?;
    let handle = if id == "local" {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "The host user has no home directory configured".to_string());
        home.and_then(spawn)
    } else {
        app.hosts
            .live(&id)
            .ok_or_else(|| "The host is offline.".to_string())
            .and_then(|host| host.home_terminal())
    }
    .map_err(|error| fail(StatusCode::BAD_REQUEST, error))?;
    terminals.insert(key, Arc::new(handle));
    Ok(Json(serde_json::json!({"ok":true,"available":true})))
}
pub async fn stop_host(State(app): State<App>, Path(id): Path<String>) -> crate::ApiResult {
    let _run = app.run_lock.lock().await;
    require_host(&app, &id)?;
    let key = host_key(&id);
    if let Some(frame) = stop(&key, false).await {
        registry().lock().unwrap().insert(key, saved_handle(frame));
    }
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn remove_host(id: &str) {
    stop(&host_key(id), true).await;
    stop_auth_matching(|key| key.starts_with(&format!("provider-auth:{id}:"))).await;
}
async fn stop_auth_matching(matches: impl Fn(&str) -> bool) {
    let keys: Vec<_> = registry()
        .lock()
        .unwrap()
        .keys()
        .filter(|key| matches(key))
        .cloned()
        .collect();
    for key in keys {
        stop(&key, true).await;
    }
}
pub async fn remove_provider_auth(id: &str) {
    stop_auth_matching(|key| key.starts_with("provider-auth:") && key.ends_with(&format!(":{id}")))
        .await;
}
pub async fn host_events(
    State(app): State<App>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(error) = require_host(&app, &id) {
        return error.into_response();
    }
    let handle = registry().lock().unwrap().get(&host_key(&id)).cloned();
    match handle {
        Some(handle) => ws
            .max_message_size(32 * 1024)
            .on_upgrade(move |ws| socket(ws, handle, None)),
        None => (StatusCode::NOT_FOUND, "Open the terminal first").into_response(),
    }
}
pub async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(session) = app.sessions.lock().unwrap().get(&id).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let handle = registry().lock().unwrap().get(&id).cloned();
    match handle {
        Some(handle) => ws
            .max_message_size(32 * 1024)
            .on_upgrade(move |ws| socket(ws, handle, Some(session))),
        None => (StatusCode::CONFLICT, "Open the terminal first").into_response(),
    }
}
async fn socket(ws: WebSocket, handle: Arc<Handle>, session: Option<Arc<Session>>) {
    let (mut sink, mut stream) = ws.split();
    let mut frames = handle.frames.clone();
    let mut seq = None;
    let mut history_cursor = None;
    let mut alternate = None;
    let mut flow_control = false;
    loop {
        let frame = frames.borrow_and_update().clone();
        // A slow client skips intermediate updates and receives a full redraw.
        let history = frame.history.since(&mut history_cursor);
        let data = if seq.is_some()
            && seq == frame.seq.checked_sub(1)
            && !history.reset
            && history.rows.is_empty()
            && alternate == Some(frame.alternate)
        {
            &frame.delta
        } else {
            &frame.full
        };
        alternate = Some(frame.alternate);
        let archived = session
            .as_ref()
            .is_some_and(|session| session.data.lock().unwrap().lifecycle.archived);
        let message = serde_json::json!({"archived":archived,"history":history,"alternate":frame.alternate,"type":"frame","seq":frame.seq,"cols":frame.cols,"rows":frame.rows,"data":data,"exited":frame.exited,"code":frame.code,"error":frame.error});
        if !matches!(
            tokio::time::timeout(
                Duration::from_secs(10),
                sink.send(Message::Text(message.to_string().into()))
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
        seq = Some(frame.seq);
        let mut awaiting_ack = flow_control.then_some(frame.seq);
        loop {
            tokio::select! {
                // A rendering browser acknowledges each frame. Watch then
                // coalesces output while it is busy, bounding queued redraws.
                changed = frames.changed(), if !flow_control || awaiting_ack.is_none() => { if changed.is_err() { return; } break; }
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {continue;};
                        match value["type"].as_str() {
                            Some("flowControl") => {flow_control=true;awaiting_ack=seq;},
                            Some("ack") if value["seq"].as_u64()==awaiting_ack => {awaiting_ack=None;},
                            // The final frame may arrive before the previous
                            // redraw is acknowledged. Always accept flow-control
                            // messages, but never forward input to an exited PTY.
                            _ => if !frames.borrow().exited && !handle.stopped.load(Ordering::Acquire) && !session.as_ref().is_some_and(|session| session.data.lock().unwrap().lifecycle.archived) {
                                if let Ok(input) = serde_json::from_value::<Input>(value) {
                                    if handle.input.try_send(input).is_err() {return;}
                                }
                            }
                        }
                    }
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    _ => {}
                }
            }
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthQuery {
    host_id: Option<String>,
}
fn auth_key(id: &str, host: &str) -> String {
    format!("provider-auth:{host}:{id}")
}
pub async fn open_provider_auth(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ProviderAuthQuery>,
    Json(request): Json<OpenRequest>,
) -> crate::ApiResult {
    let _run = app.run_lock.lock().await;
    let host = query.host_id.as_deref().unwrap_or("local");
    require_host(&app, host)?;
    let connection = agentworks_runtime::model_providers::connection(&id, host)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    let key = auth_key(&id, host);
    if STOPPING.load(Ordering::Acquire) {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "The service is stopping",
        ));
    }
    let mut terminals = registry().lock().unwrap();
    if let Some(handle) = terminals.get(&key) {
        if !request.restart {
            return Ok(Json(serde_json::json!({"available":true})));
        }
        if !handle.frames.borrow().exited {
            return Err(fail(
                StatusCode::CONFLICT,
                "Sign-in is still running. Stop it before restarting.",
            ));
        }
    }
    crate::service_restart::allow_start(&app)?;
    let kind = &connection.profile.kind;
    let handle = if host == "local" {
        agentworks_runtime::model_providers::login_terminal(kind, &id)
    } else {
        app.hosts
            .live(host)
            .ok_or_else(|| fail(StatusCode::CONFLICT, "The host is offline"))?
            .provider_auth_terminal(kind, &id)
    }
    .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    terminals.insert(key, Arc::new(handle));
    Ok(Json(serde_json::json!({"available":true})))
}
pub async fn stop_provider_auth(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ProviderAuthQuery>,
) -> crate::ApiResult {
    require_host(&app, query.host_id.as_deref().unwrap_or("local"))?;
    stop(
        &auth_key(&id, query.host_id.as_deref().unwrap_or("local")),
        true,
    )
    .await;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn provider_auth_events(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ProviderAuthQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let host = query.host_id.as_deref().unwrap_or("local");
    if let Err(e) = agentworks_runtime::model_providers::authorize(&id, host) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    if let Err(e) = require_host(&app, host) {
        return e.into_response();
    }
    let handle = registry()
        .lock()
        .unwrap()
        .get(&auth_key(&id, host))
        .cloned();
    match handle {
        Some(handle) => ws
            .max_message_size(32 * 1024)
            .on_upgrade(move |ws| socket(ws, handle, None)),
        None => (StatusCode::NOT_FOUND, "Start sign-in first").into_response(),
    }
}
