//! One service-owned shell per session or host. Ghostty alone interprets PTY output and
//! answers host queries; browsers receive presentation updates, never raw output.
use crate::{App, Session, fail};
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
use libghostty_vt::{
    Terminal, TerminalOptions,
    render::{CellIterator, RenderState, RowIterator},
    screen::{CellWide, TrackedGridRef},
    style::{Style, StyleColor, Underline},
    terminal::{Mode, Point, PointCoordinate, PointSpace},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

const SCROLLBACK_ROWS: usize = 10_000;
// Despite the C/Rust options docs saying "lines", Ghostty's pinned Screen.init
// passes this value to PageList as a byte budget. Allow enough native storage
// for wide terminals; HistoryState separately enforces the 10,000-row limit.
const GHOSTTY_SCROLLBACK_BYTES: usize = 64 * 1024 * 1024;

type Result<T> = std::result::Result<T, String>;
static STOPPING: AtomicBool = AtomicBool::new(false);
static TERMINALS: OnceLock<Mutex<HashMap<String, Arc<Handle>>>> = OnceLock::new();
fn registry() -> &'static Mutex<HashMap<String, Arc<Handle>>> {
    TERMINALS.get_or_init(Default::default)
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Frame {
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub history: History,
    pub alternate: bool,
    pub full: String,
    pub delta: String,
    pub exited: bool,
    pub code: Option<i32>,
    pub error: Option<String>,
}
// Snapshots share immutable history; wire updates contain only new rows. This
// also lets skipped watch frames and reconnects recover without losing output.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct History {
    pub epoch: u64,
    pub start: u64,
    pub end: u64,
    pub reset: bool,
    pub rows: Arc<Vec<Arc<String>>>,
}
impl History {
    pub fn since(&self, cursor: &mut Option<(u64, u64)>) -> Self {
        let offset = cursor.and_then(|(epoch, end)| {
            (epoch == self.epoch && end >= self.start && end <= self.end)
                .then(|| (end - self.start) as usize)
        });
        let update = Self {
            reset: offset.is_none(),
            rows: offset.map_or_else(
                || self.rows.clone(),
                |offset| Arc::new(self.rows[offset..].to_vec()),
            ),
            ..self.clone()
        };
        *cursor = Some((self.epoch, self.end));
        update
    }
    pub fn restore(&self, previous: &Self) -> Option<Self> {
        if self.end < self.start || self.end - self.start > SCROLLBACK_ROWS as u64 {
            return None;
        }
        let rows = if self.reset {
            self.rows.clone()
        } else {
            if self.epoch != previous.epoch
                || self.start < previous.start
                || self.end != previous.end + self.rows.len() as u64
            {
                return None;
            }
            let drop = (self.start - previous.start) as usize;
            if drop > previous.rows.len() {
                return None;
            }
            Arc::new(
                previous.rows[drop..]
                    .iter()
                    .chain(self.rows.iter())
                    .cloned()
                    .collect(),
            )
        };
        (rows.len() as u64 == self.end - self.start).then(|| Self {
            rows,
            reset: true,
            ..self.clone()
        })
    }
}
#[derive(Default)]
struct HistoryState {
    snapshot: History,
    last: Option<TrackedGridRef>,
    dimensions: (u16, u16),
}
impl HistoryState {
    fn update(&mut self, terminal: &Terminal<'_, '_>) -> Result<History> {
        // The alternate screen has no history. Keep the primary cache intact
        // while applications such as vim or less own the terminal.
        if terminal.active_screen().map_err(|e| e.to_string())?
            == libghostty_vt::screen::Screen::Alternate
        {
            return Ok(self.snapshot.clone());
        }
        let count = terminal.scrollback_rows().map_err(|e| e.to_string())?;
        let dimensions = (
            terminal.cols().map_err(|e| e.to_string())?,
            terminal.rows().map_err(|e| e.to_string())?,
        );
        let anchor = self
            .last
            .as_ref()
            .map(|last| last.point(PointSpace::History))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten();
        let base = count.saturating_sub(SCROLLBACK_ROWS);
        let keep = anchor.map(|point| (point.y as usize + 1).saturating_sub(base));
        let rebuild = self.dimensions != dimensions
            || (!self.snapshot.rows.is_empty() && keep.is_none())
            || keep.is_some_and(|keep| keep > self.snapshot.rows.len() || keep > count - base);
        let from = if rebuild {
            base
        } else {
            anchor.map_or(base, |point| (point.y as usize + 1).max(base))
        };
        if rebuild || from < count || keep.is_some_and(|keep| keep < self.snapshot.rows.len()) {
            let snapshot = &mut self.snapshot;
            if rebuild {
                snapshot.epoch += 1;
                snapshot.start = 0;
                snapshot.rows = Arc::default();
            }
            let rows = Arc::make_mut(&mut snapshot.rows);
            if !rebuild {
                let discard = rows.len().saturating_sub(keep.unwrap_or(0));
                rows.drain(..discard);
                snapshot.start += discard as u64;
            }
            let palette = terminal.color_palette().map_err(|e| e.to_string())?;
            for y in from..count {
                let mut text = String::from("\x1b[0m");
                let mut style = Style::default();
                let mut chars = vec!['\0'; 32];
                for x in 0..dimensions.0 {
                    let grid = terminal
                        .grid_ref(Point::History(PointCoordinate { x, y: y as u32 }))
                        .map_err(|e| e.to_string())?;
                    let width = grid
                        .cell()
                        .and_then(|cell| cell.wide())
                        .map_err(|e| e.to_string())?;
                    if matches!(width, CellWide::SpacerHead | CellWide::SpacerTail) {
                        continue;
                    }
                    let mut next = grid.style().map_err(|e| e.to_string())?;
                    for color in [
                        &mut next.fg_color,
                        &mut next.bg_color,
                        &mut next.underline_color,
                    ] {
                        if let StyleColor::Palette(index) = *color {
                            *color = StyleColor::Rgb(palette.get(index));
                        }
                    }
                    if next != style {
                        sgr(&mut text, next);
                        style = next;
                    }
                    let len = match grid.graphemes(&mut chars) {
                        Ok(len) => len,
                        Err(libghostty_vt::Error::OutOfSpace { required }) => {
                            chars.resize(required, '\0');
                            grid.graphemes(&mut chars).map_err(|e| e.to_string())?
                        }
                        Err(error) => return Err(error.to_string()),
                    };
                    if len == 0 {
                        text.push(' ');
                    } else {
                        text.extend(chars[..len].iter());
                    }
                }
                text.push_str("\x1b[0m");
                rows.push(Arc::new(text));
            }
            snapshot.end = snapshot.start + rows.len() as u64;
            snapshot.reset = true;
        }
        self.dimensions = dimensions;
        self.last = if count > 0 {
            Some(
                terminal
                    .track_grid_ref(Point::History(PointCoordinate {
                        x: 0,
                        y: count as u32 - 1,
                    }))
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        Ok(self.snapshot.clone())
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Binary { data: Vec<u8> },
}
pub struct Handle {
    pub input: mpsc::Sender<Input>,
    pub frames: watch::Receiver<Frame>,
    pub stopped: Arc<AtomicBool>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
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

pub fn spawn(cwd: PathBuf) -> Result<Handle> {
    let (input, incoming) = mpsc::channel(64);
    let (frames, receiver) = watch::channel(Frame {
        cols: 80,
        rows: 24,
        ..Default::default()
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    std::thread::Builder::new()
        .name("session-terminal".into())
        .spawn(move || {
            if let Err(error) = run(cwd, incoming, &frames, stop) {
                frames.send_modify(|frame| {
                    frame.seq += 1;
                    frame.exited = true;
                    frame.error = Some(error);
                    frame.delta.clear();
                });
            }
            // Stop acknowledgements are sent only after run's Shell has dropped
            // and its owned processes have been cleaned up and reaped.
            if !frames.borrow().exited {
                frames.send_modify(|frame| {
                    frame.seq += 1;
                    frame.exited = true;
                    frame.delta.clear();
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Handle {
        input,
        frames: receiver,
        stopped,
    })
}

struct Shell {
    child: std::process::Child,
    master: std::fs::File,
    reaped: bool,
}
impl Shell {
    // Observe exit without reaping: the shell PID/session ID stays reserved
    // until cleanup has finished. A numeric ID from a reaped child is unsafe
    // to use for finding or signaling terminal jobs.
    #[cfg(target_os = "linux")]
    fn exited(&self) -> io::Result<bool> {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    #[cfg(target_os = "linux")]
    fn jobs(&self) -> Vec<std::fs::File> {
        if self.reaped {
            return Vec::new();
        }
        let sid = self.child.id() as i32;
        // Fail closed for an unexpected session or a /proc mount from another
        // PID namespace. Never translate /proc IDs into process-group signals.
        let own = unsafe { libc::getpid() };
        let proc_pid = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|stat| stat.split_whitespace().next()?.parse::<i32>().ok());
        if sid <= 1 || sid == unsafe { libc::getsid(0) } || proc_pid != Some(own) {
            return Vec::new();
        }
        std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
                if pid <= 1 || pid == own || unsafe { libc::getsid(pid) } != sid {
                    return None;
                }
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
                if fd < 0 {
                    return None;
                }
                let handle = unsafe { std::fs::File::from_raw_fd(fd as i32) };
                // Recheck after opening: the handle pins the target identity, and
                // the unreaped session leader prevents session-ID reuse.
                (unsafe { libc::getsid(pid) } == sid).then_some(handle)
            })
            .collect()
    }
}
impl Drop for Shell {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        for job in self.jobs() {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    job.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        // Child owns an unreaped process. No negative PID or group kill is used.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
// A terminal is a new interactive context, even when its service was launched
// by an agent, CI runner, desktop terminal or service manager. Keep the user's
// credentials, paths, locale and desktop/SSH integration; remove only launcher
// controls and process-specific state. NO_BROWSER is intentionally preserved.
fn transient_shell_variable(key: &std::ffi::OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    if key.starts_with("AGENTWORKS_") {
        return true;
    }
    if ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"]
        .iter()
        .any(|prefix| {
            key.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit())
            })
        })
    {
        return true;
    }
    matches!(
        key,
        "NO_COLOR"
            | "FORCE_COLOR"
            | "CLICOLOR"
            | "CLICOLOR_FORCE"
            | "CI"
            | "CONTINUOUS_INTEGRATION"
            | "CODEX_CI"
            | "CODEX_SESSION_ID"
            | "CODEX_THREAD_ID"
            | "CODEX_VERSION"
            | "CODEX_MANAGED_BY_NPM"
            | "CODEX_MANAGED_PACKAGE_ROOT"
            | "CODEX_SANDBOX_NETWORK_DISABLED"
            | "CLAUDECODE"
            | "CLAUDE_CODE_ENTRYPOINT"
            | "CLAUDE_CODE_SESSION_ID"
            | "PAGER"
            | "GIT_PAGER"
            | "GH_PAGER"
            | "NOTIFY_SOCKET"
            | "LISTEN_PID"
            | "LISTEN_FDS"
            | "LISTEN_FDNAMES"
            | "WATCHDOG_PID"
            | "WATCHDOG_USEC"
            | "INVOCATION_ID"
            | "JOURNAL_STREAM"
            | "SYSTEMD_EXEC_PID"
            | "MANAGERPID"
            | "MANAGERPIDFDID"
            | "MEMORY_PRESSURE_WATCH"
            | "MEMORY_PRESSURE_WRITE"
            | "TERM_PROGRAM"
            | "TERM_PROGRAM_VERSION"
            | "TERM_SESSION_ID"
            | "TERMCAP"
            | "LINES"
            | "COLUMNS"
            | "COLORFGBG"
            | "TTY"
            | "SSH_TTY"
            | "TMUX"
            | "TMUX_PANE"
            | "STY"
            | "WINDOW"
            | "WINDOWID"
            | "WT_SESSION"
            | "WT_PROFILE_ID"
            | "VTE_VERSION"
            | "KONSOLE_VERSION"
            | "KONSOLE_DBUS_SESSION"
            | "KONSOLE_DBUS_SERVICE"
            | "KONSOLE_DBUS_WINDOW"
            | "ITERM_SESSION_ID"
            | "ITERM_PROFILE"
            | "ALACRITTY_SOCKET"
            | "ALACRITTY_WINDOW_ID"
            | "PWD"
            | "OLDPWD"
            | "SHLVL"
            | "_"
            | "BASHOPTS"
            | "SHELLOPTS"
            | "GIT_DIR"
            | "GIT_WORK_TREE"
            | "GIT_INDEX_FILE"
            | "GIT_COMMON_DIR"
            | "GIT_OBJECT_DIRECTORY"
            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
            | "GIT_NAMESPACE"
            | "GIT_PREFIX"
            | "GIT_CONFIG"
            | "GIT_CONFIG_COUNT"
            | "GIT_CONFIG_PARAMETERS"
    )
}
fn shell_environment(command: &mut Command, keys: impl IntoIterator<Item = std::ffi::OsString>) {
    for key in keys {
        if transient_shell_variable(&key) {
            command.env_remove(key);
        }
    }
    command
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "agentworks")
        .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}
fn shell(cwd: PathBuf) -> io::Result<Shell> {
    let (mut master, mut slave) = (-1, -1);
    let size = libc::winsize {
        ws_col: 80,
        ws_row: 24,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    for file in [&master, &slave] {
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let program = std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/') && std::path::Path::new(s).is_file())
        .unwrap_or_else(|| "/bin/sh".into());
    let mut command = Command::new(program);
    shell_environment(&mut command, std::env::vars_os().map(|(key, _)| key));
    command
        .arg("-i")
        .current_dir(cwd)
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    Ok(Shell {
        child,
        master,
        reaped: false,
    })
}
fn run(
    cwd: PathBuf,
    mut incoming: mpsc::Receiver<Input>,
    frames: &watch::Sender<Frame>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    // Ghostty is created, used and dropped on this thread: no unsafe Send wrapper.
    let mut terminal = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: GHOSTTY_SCROLLBACK_BYTES,
    })
    .map_err(|e| e.to_string())?;
    terminal
        .set_default_fg_color(Some(libghostty_vt::style::RgbColor {
            r: 0xe2,
            g: 0xe8,
            b: 0xf0,
        }))
        .map_err(|e| e.to_string())?;
    terminal
        .set_default_bg_color(Some(libghostty_vt::style::RgbColor {
            r: 0x10,
            g: 0x12,
            b: 0x18,
        }))
        .map_err(|e| e.to_string())?;
    let replies = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let reply_sink = replies.clone();
    terminal
        .on_pty_write(move |_, bytes| reply_sink.borrow_mut().extend_from_slice(bytes))
        .map_err(|e| e.to_string())?;
    let mut shell = shell(cwd).map_err(|e| e.to_string())?;
    let mut previous = Vec::new();
    let mut history_state = HistoryState::default();
    let mut pending = Vec::new();
    let mut offset = 0;
    let mut dirty = true;
    let mut last_frame = Instant::now() - Duration::from_secs(1);
    let mut exit = None;
    let mut exit_at = None;
    let mut eof = false;
    let mut buf = [0; 16384];
    loop {
        if stop.load(Ordering::Acquire) {
            for _ in 0..16 {
                match shell.master.read(&mut buf) {
                    Ok(n) if n > 0 => terminal.vt_write(&buf[..n]),
                    _ => break,
                }
            }
            publish(
                &terminal,
                &mut history_state,
                &mut previous,
                frames,
                false,
                None,
            )?;
            return Ok(());
        }
        // Bound processing per tick, preserving complete input chunks and resize order.
        for _ in 0..64 {
            if offset < pending.len() {
                break;
            }
            pending.clear();
            offset = 0;
            match incoming.try_recv() {
                Ok(Input::Input { data }) => pending.extend_from_slice(data.as_bytes()),
                Ok(Input::Binary { data }) => pending.extend(data),
                Ok(Input::Resize { cols, rows }) => {
                    let (cols, rows) = (cols.clamp(20, 300), rows.clamp(5, 120));
                    if terminal.cols().ok() != Some(cols) || terminal.rows().ok() != Some(rows) {
                        let size = libc::winsize {
                            ws_col: cols,
                            ws_row: rows,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        if unsafe {
                            libc::ioctl(shell.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size)
                        } < 0
                        {
                            return Err(io::Error::last_os_error().to_string());
                        }
                        terminal
                            .resize(cols, rows, 0, 0)
                            .map_err(|e| e.to_string())?;
                        previous.clear();
                        dirty = true;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if offset < pending.len() {
            match shell.master.write(&pending[offset..]) {
                Ok(n) => offset += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        if offset > 0 {
            pending.drain(..offset);
            offset = 0;
        }
        // Drain at most 256 KiB per tick so chatty programs cannot starve input.
        for _ in 0..16 {
            match shell.master.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    terminal.vt_write(&buf[..n]);
                    dirty = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    eof = true;
                    break;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        let reply = std::mem::take(&mut *replies.borrow_mut());
        // Responses join the pending writer, never competing with browser input.
        pending.extend(reply);
        if pending.len().saturating_sub(offset) > 1024 * 1024 {
            return Err("Terminal input queue exceeded its limit".into());
        }
        if exit_at.is_none() {
            #[cfg(target_os = "linux")]
            let exited = shell.exited().map_err(|e| e.to_string())?;
            #[cfg(not(target_os = "linux"))]
            let exited = {
                exit = shell.child.try_wait().map_err(|e| e.to_string())?;
                exit.is_some()
            };
            if exited {
                exit_at = Some(Instant::now());
            }
        }
        let done = exit_at.is_some()
            && (eof || exit_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)));
        if done {
            // Capture descendants while the leader is still unreaped. Signal
            // only those pinned identities, then collect the actual exit code.
            #[cfg(target_os = "linux")]
            for job in shell.jobs() {
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        job.as_raw_fd(),
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    );
                }
            }
            exit = Some(shell.child.wait().map_err(|e| e.to_string())?);
            shell.reaped = true;
        }
        if (dirty && last_frame.elapsed() >= Duration::from_millis(33)) || done {
            publish(
                &terminal,
                &mut history_state,
                &mut previous,
                frames,
                done,
                exit.and_then(|status| status.code()),
            )?;
            dirty = false;
            last_frame = Instant::now();
        }
        if done {
            return Ok(());
        }
        let mut poll = libc::pollfd {
            fd: shell.master.as_raw_fd(),
            events: libc::POLLIN
                | if offset < pending.len() {
                    libc::POLLOUT
                } else {
                    0
                },
            revents: 0,
        };
        unsafe {
            libc::poll(&mut poll, 1, 10);
        }
        if eof {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn publish(
    terminal: &Terminal<'_, '_>,
    history_state: &mut HistoryState,
    previous: &mut Vec<String>,
    frames: &watch::Sender<Frame>,
    exited: bool,
    code: Option<i32>,
) -> Result<()> {
    let history = history_state.update(terminal)?;
    let alternate = terminal
        .active_screen()
        .map_err(|error| error.to_string())?
        == libghostty_vt::screen::Screen::Alternate;
    let (full, delta, rows) = display(terminal, previous)?;
    *previous = rows;
    frames.send_modify(|frame| {
        frame.seq += 1;
        frame.cols = terminal.cols().unwrap_or(80);
        frame.rows = terminal.rows().unwrap_or(24);
        frame.history = history;
        frame.alternate = alternate;
        frame.full = full;
        frame.delta = delta;
        frame.exited = exited;
        frame.code = code;
    });
    Ok(())
}

fn sgr(out: &mut String, style: Style) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (style.bold, 1),
        (style.faint, 2),
        (style.italic, 3),
        (style.blink, 5),
        (style.inverse, 7),
        (style.invisible, 8),
        (style.strikethrough, 9),
        (style.overline, 53),
    ] {
        if flag {
            let _ = write!(out, ";{code}");
        }
    }
    if style.underline != Underline::None {
        let _ = write!(out, ";4:{}", style.underline as u32);
    }
    for (color, code) in [
        (style.fg_color, 38),
        (style.bg_color, 48),
        (style.underline_color, 58),
    ] {
        match color {
            StyleColor::None => {}
            StyleColor::Palette(index) => {
                let _ = write!(out, ";{code};5;{}", index.0);
            }
            StyleColor::Rgb(rgb) => {
                let _ = write!(out, ";{code};2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
        }
    }
    out.push('m');
}
fn display(
    terminal: &Terminal<'_, '_>,
    previous: &[String],
) -> Result<(String, String, Vec<String>)> {
    let mut state = RenderState::new().map_err(|e| e.to_string())?;
    let snapshot = state.update(terminal).map_err(|e| e.to_string())?;
    let mut row_iterator = RowIterator::new().map_err(|e| e.to_string())?;
    let mut cell_iterator = CellIterator::new().map_err(|e| e.to_string())?;
    let mut iterator = row_iterator.update(&snapshot).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    while let Some(row) = iterator.next() {
        let mut text = String::from("\x1b[0m");
        let mut style = Style::default();
        let mut cells = cell_iterator.update(row).map_err(|e| e.to_string())?;
        while let Some(cell) = cells.next() {
            let width = cell
                .raw_cell()
                .and_then(|c| c.wide())
                .map_err(|e| e.to_string())?;
            if matches!(width, CellWide::SpacerHead | CellWide::SpacerTail) {
                continue;
            }
            let mut next = cell.style().map_err(|e| e.to_string())?;
            // Resolve palette colors on the host; palette updates then redraw
            // affected rows without relying on the browser's initial palette.
            if let Some(color) = cell.fg_color().map_err(|e| e.to_string())? {
                next.fg_color = StyleColor::Rgb(color);
            }
            if let Some(color) = cell.bg_color().map_err(|e| e.to_string())? {
                next.bg_color = StyleColor::Rgb(color);
            }
            if next != style {
                sgr(&mut text, next);
                style = next;
            }
            let chars = cell.graphemes().map_err(|e| e.to_string())?;
            if chars.is_empty() {
                text.push(' ');
            } else {
                for c in chars {
                    text.push(c);
                }
            }
        }
        rows.push(text);
    }
    // xterm is a presentation surface: no origin mode, scrolling regions or host
    // queries reach it. Ghostty retains all emulator internals across attachments.
    let prefix = "\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[?6l\x1b[r";
    let mut full = format!("{prefix}\x1b[0m\x1b[2J");
    let mut delta = prefix.to_string();
    let colors = snapshot.colors().map_err(|e| e.to_string())?;
    let mut theme = String::new();
    for (number, color) in [(10, colors.foreground), (11, colors.background)] {
        let _ = write!(
            theme,
            "\x1b]{number};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
            color.r, color.g, color.b
        );
    }
    full.push_str(&theme);
    delta.push_str(&theme);
    for (index, row) in rows.iter().enumerate() {
        let line = format!("\x1b[{};1H\x1b[0m\x1b[2K{row}", index + 1);
        full.push_str(&line);
        if previous.get(index) != Some(row) {
            delta.push_str(&line);
        }
    }
    let mut suffix = format!(
        "\x1b[0m\x1b[{};{}H\x1b[?25{}",
        terminal.cursor_y().map_err(|e| e.to_string())? + 1,
        terminal.cursor_x().map_err(|e| e.to_string())? + 1,
        if terminal.is_cursor_visible().unwrap_or(true) {
            'h'
        } else {
            'l'
        }
    );
    for (mode, number) in [
        (Mode::BRACKETED_PASTE, 2004),
        (Mode::DECCKM, 1),
        (Mode::KEYPAD_KEYS, 66),
        (Mode::X10_MOUSE, 9),
        (Mode::NORMAL_MOUSE, 1000),
        (Mode::BUTTON_MOUSE, 1002),
        (Mode::ANY_MOUSE, 1003),
        (Mode::FOCUS_EVENT, 1004),
        (Mode::UTF8_MOUSE, 1005),
        (Mode::SGR_MOUSE, 1006),
        (Mode::URXVT_MOUSE, 1015),
    ] {
        let _ = write!(
            suffix,
            "\x1b[?{number}{}",
            if terminal.mode(mode).unwrap_or(false) {
                'h'
            } else {
                'l'
            }
        );
    }
    suffix.push_str("\x1b[?2026l");
    full.push_str(&suffix);
    delta.push_str(&suffix);
    Ok((full, delta, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terminal() -> Terminal<'static, 'static> {
        Terminal::new(TerminalOptions {
            cols: 40,
            rows: 8,
            max_scrollback: 100,
        })
        .unwrap()
    }
    #[test]
    fn presentation_round_trips_wide_combining_colors_and_scroll_regions() {
        let mut host = terminal();
        host.vt_write("hello 界 e\u{301}\r\n\x1b[38;2;100;50;200mcolored\x1b[0m\x1b[3;6r\x1b[6;1Hbottom\nscroll\x1b[?2004h\x1b[?1006h\x1b[?1000h".as_bytes());
        let (full, _, rows) = display(&host, &[]).unwrap();
        let mut browser = terminal();
        browser.vt_write(full.as_bytes());
        assert_eq!(display(&browser, &[]).unwrap().2, rows);
        assert_eq!(browser.cursor_x().unwrap(), host.cursor_x().unwrap());
        assert_eq!(browser.cursor_y().unwrap(), host.cursor_y().unwrap());
        assert!(browser.mode(Mode::BRACKETED_PASTE).unwrap());
        host.vt_write(b"\x1b[4;1Hnew\x1b[?25l");
        let (_, delta, changed) = display(&host, &rows).unwrap();
        browser.vt_write(delta.as_bytes());
        assert_eq!(display(&browser, &[]).unwrap().2, changed);
        let mut fresh = terminal();
        fresh.vt_write(display(&host, &[]).unwrap().0.as_bytes());
        assert_eq!(display(&fresh, &[]).unwrap().2, changed);
        assert!(!fresh.is_cursor_visible().unwrap());
    }
    #[test]
    fn ghostty_answers_queries_without_a_browser() {
        let mut host = terminal();
        let replies = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = replies.clone();
        host.on_pty_write(move |_, data| sink.borrow_mut().extend_from_slice(data))
            .unwrap();
        host.vt_write(b"\x1b[3;5H\x1b[6n");
        assert_eq!(&*replies.borrow(), b"\x1b[3;5R");
        assert!(!display(&host, &[]).unwrap().0.contains("\x1b[6n"));
    }
    #[test]
    fn history_tracks_append_eviction_resize_clear_and_alternate_screen() {
        let mut host = terminal();
        let mut history = HistoryState::default();
        let mut cursor = None;
        let mut replica = History::default();
        for batch in 0..12 {
            for row in 0..25 {
                host.vt_write(
                    format!("\x1b[31mline-{} 界 e\u{301}\x1b[0m\r\n", batch * 25 + row).as_bytes(),
                );
            }
            let snapshot = history.update(&host).unwrap();
            let fresh = HistoryState::default().update(&host).unwrap();
            assert_eq!(snapshot.rows, fresh.rows, "batch {batch}");
            assert!(snapshot.rows.len() <= SCROLLBACK_ROWS);
            if batch % 3 == 0 {
                let patch = snapshot.since(&mut cursor);
                replica = patch.restore(&replica).expect("valid history patch");
                assert_eq!(replica.rows, snapshot.rows);
            }
        }
        let before = history.update(&host).unwrap();
        host.vt_write(b"\x1b[?1049halternate-only\r\n");
        assert_eq!(history.update(&host).unwrap().rows, before.rows);
        host.vt_write(b"\x1b[?1049l");
        host.resize(25, 10, 0, 0).unwrap();
        let resized = history.update(&host).unwrap();
        assert_ne!(resized.epoch, before.epoch);
        assert_eq!(
            resized.rows,
            HistoryState::default().update(&host).unwrap().rows
        );
        host.vt_write(b"\x1b[3J");
        let cleared = history.update(&host).unwrap();
        assert!(cleared.rows.is_empty());
        assert!(cleared.since(&mut cursor).reset);
    }
    #[test]
    fn scrollback_snapshot_is_bounded_to_ten_thousand_rows() {
        let mut host = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 5,
            max_scrollback: GHOSTTY_SCROLLBACK_BYTES,
        })
        .unwrap();
        let output = (0..10_100)
            .map(|row| format!("bounded-{row:05}\r\n"))
            .collect::<String>();
        host.vt_write(output.as_bytes());
        let mut state = HistoryState::default();
        let snapshot = state.update(&host).unwrap();
        assert_eq!(snapshot.rows.len(), SCROLLBACK_ROWS);
        assert!(!snapshot.rows[0].contains("bounded-00000"));
        assert!(snapshot.rows.last().unwrap().contains("bounded-10095"));
        let mut cursor = None;
        snapshot.since(&mut cursor);
        host.vt_write(b"bounded-next\r\n");
        let next = state.update(&host).unwrap();
        assert_eq!(next.rows.len(), SCROLLBACK_ROWS);
        assert_eq!(next.start, snapshot.start + 1);
        let patch = next.since(&mut cursor);
        assert!(!patch.reset);
        assert_eq!(patch.rows.len(), 1);
        assert_eq!(patch.restore(&snapshot).unwrap().rows, next.rows);
    }
    #[test]
    fn history_wire_updates_are_incremental_and_recover_skipped_history() {
        let mut host = terminal();
        let mut state = HistoryState::default();
        let mut cursor = None;
        host.vt_write(b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\nseventh\r\neighth\r\nninth\r\n");
        let first = state.update(&host).unwrap();
        let patch = first.since(&mut cursor);
        assert!(patch.reset);
        assert!(!patch.rows.is_empty());
        let mut replica = patch.restore(&History::default()).unwrap();
        host.vt_write(b"tenth\r\n");
        let next = state.update(&host).unwrap();
        let patch = next.since(&mut cursor);
        assert!(!patch.reset);
        assert_eq!(patch.rows.len(), 1);
        replica = patch.restore(&replica).unwrap();
        assert_eq!(replica.rows, next.rows);
        assert!(next.since(&mut cursor).rows.is_empty());
        let mut invalid = patch.clone();
        invalid.end += 2;
        assert!(invalid.restore(&replica).is_none());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_pins_only_its_own_terminal_session_and_keeps_leader_unreaped() {
        let mut sentinel = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut shell = shell(std::env::temp_dir()).unwrap();
        let sid = shell.child.id() as i32;
        assert_ne!(sid, unsafe { libc::getsid(0) });
        assert_eq!(unsafe { libc::getsid(sid) }, sid);
        for job in shell.jobs() {
            let info =
                std::fs::read_to_string(format!("/proc/self/fdinfo/{}", job.as_raw_fd())).unwrap();
            let pid = info
                .lines()
                .find_map(|line| line.strip_prefix("Pid:")?.trim().parse::<i32>().ok())
                .unwrap();
            assert_ne!(pid, sentinel.id() as i32);
            assert_ne!(pid, unsafe { libc::getpid() });
            assert_eq!(unsafe { libc::getsid(pid) }, sid);
        }
        // Use the child-owned handle, then observe twice without releasing its PID.
        shell.child.kill().unwrap();
        for _ in 0..100 {
            if shell.exited().unwrap() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(shell.exited().unwrap());
        assert!(shell.exited().unwrap());
        drop(shell);
        assert!(sentinel.try_wait().unwrap().is_none());
        sentinel.kill().unwrap();
        sentinel.wait().unwrap();
    }
    #[test]
    fn shell_environment_removes_launcher_state_preserves_user_settings_and_no_browser() {
        // Inject settings into this child only; never mutate the test runner's
        // environment or rely on whichever variables the runner inherited.
        let dropped = [
            "NO_COLOR",
            "FORCE_COLOR",
            "CI",
            "CODEX_CI",
            "CODEX_THREAD_ID",
            "CLAUDECODE",
            "PAGER",
            "GIT_PAGER",
            "GH_PAGER",
            "AGENTWORKS_DB_PATH",
            "AGENTWORKS_SUPERVISOR_TOKEN",
            "NOTIFY_SOCKET",
            "LISTEN_FDS",
            "WATCHDOG_PID",
            "TMUX",
            "SSH_TTY",
            "COLUMNS",
            "PWD",
            "GIT_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
        ];
        let kept = [
            ("NO_BROWSER", "1"),
            ("HOME", "/user/home"),
            ("PATH", "/user/bin"),
            ("LANG", "C.UTF-8"),
            ("XDG_CONFIG_HOME", "/user/config"),
            ("XDG_RUNTIME_DIR", "/user/run"),
            ("SSH_AUTH_SOCK", "/user/agent.sock"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/user/bus"),
            ("DISPLAY", ":0"),
            ("CODEX_HOME", "/user/codex"),
            ("OPENAI_API_KEY", "fixture-key"),
            ("ANTHROPIC_API_KEY", "fixture-key"),
            ("GIT_SSH_COMMAND", "fixture-ssh"),
            ("TERMINFO_DIRS", "/user/terminfo"),
        ];
        let mut command = Command::new("/usr/bin/env");
        command.arg("-0");
        for key in dropped {
            command.env(key, "launcher-only");
        }
        for (key, value) in kept {
            command.env(key, value);
        }
        command
            .env("TERM", "dumb")
            .env("COLORTERM", "")
            .env("TERM_PROGRAM", "parent-terminal");
        let keys = dropped
            .into_iter()
            .chain(kept.into_iter().map(|(key, _)| key))
            .chain(["TERM", "COLORTERM", "TERM_PROGRAM"])
            .map(std::ffi::OsString::from);
        shell_environment(&mut command, keys);
        let output = command.output().unwrap();
        assert!(output.status.success());
        let values: HashMap<_, _> = std::str::from_utf8(&output.stdout)
            .unwrap()
            .split('\0')
            .filter_map(|entry| entry.split_once('='))
            .collect();
        for key in dropped {
            assert!(!values.contains_key(key), "{key} leaked into shell");
        }
        for (key, value) in kept {
            assert_eq!(values.get(key), Some(&value), "{key} changed");
        }
        assert_eq!(values.get("TERM"), Some(&"xterm-256color"));
        assert_eq!(values.get("COLORTERM"), Some(&"truecolor"));
        assert_eq!(values.get("TERM_PROGRAM"), Some(&"agentworks"));
    }
    #[tokio::test]
    async fn shell_keeps_state_drains_final_output_and_reports_real_exit() {
        let handle = spawn(std::env::temp_dir()).unwrap();
        handle.input.send(Input::Input {data:"export AW_TERMINAL_TEST=retained\nprintf 'color:%s:%s\\n' \"$TERM\" \"${NO_COLOR-unset}\"\nprintf 'marker-%s\\n' \"$AW_TERMINAL_TEST\"\nexit 7\n".into()}).await.unwrap();
        let mut frames = handle.frames.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if frames.borrow().exited {
                    break;
                }
                frames.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let frame = frames.borrow().clone();
        assert_eq!(frame.code, Some(7));
        assert!(
            frame.full.contains("color:xterm-256color:unset"),
            "{}",
            frame.full
        );
        assert!(frame.full.contains("marker-retained"), "{}", frame.full);
    }
}
