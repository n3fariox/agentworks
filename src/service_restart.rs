//! Durable restart decisions. Only the external supervisor can launch the successor.
use crate::*;
use rusqlite::{OptionalExtension, params};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Manager {
    pub instance: String,
    blocked: AtomicBool,
    fault: Mutex<Option<String>>,
    control: Option<PathBuf>,
    releases: Option<PathBuf>,
}
impl Manager {
    pub fn new() -> Self {
        Self {
            instance: Uuid::new_v4().to_string(),
            blocked: AtomicBool::new(false),
            fault: Mutex::new(None),
            control: std::env::var_os("AGENTWORKS_SUPERVISOR_DIR").map(PathBuf::from),
            releases: std::env::var_os("AGENTWORKS_RELEASE_DIR").map(PathBuf::from),
        }
    }
    #[cfg(test)]
    pub(crate) fn block_admission_for_test(&self) {
        self.blocked.store(true, Ordering::SeqCst);
    }
    fn supported(&self) -> bool {
        let Some(dir) = &self.control else {
            return false;
        };
        // A flag alone cannot turn an unsupervised invocation into a supported service.
        let Ok(raw) = std::fs::read_to_string(dir.join("owner.json")) else {
            return false;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return false;
        };
        let parent = std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find_map(|l| {
                l.strip_prefix("PPid:")
                    .and_then(|s| s.trim().parse::<u64>().ok())
            });
        v["heartbeat"]
            .as_u64()
            .is_some_and(|t| now().saturating_sub(t) < 2000)
            && parent == v["pid"].as_u64()
            && v["token"].as_str() == std::env::var("AGENTWORKS_SUPERVISOR_TOKEN").ok().as_deref()
    }
    fn release(&self, name: Option<&str>) -> Result<PathBuf, String> {
        let root = self
            .releases
            .as_ref()
            .ok_or("No trusted release directory configured")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let p = if let Some(name) = name {
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                || name == "."
                || name == ".."
            {
                return Err("Release must be a prepared executable basename".into());
            }
            root.join(name).canonicalize().map_err(|e| e.to_string())?
        } else {
            std::env::current_exe()
                .map_err(|e| e.to_string())?
                .canonicalize()
                .map_err(|e| e.to_string())?
        };
        if p.parent() != Some(root.as_path()) || !p.is_file() {
            return Err("Executable is outside the trusted release directory".into());
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        if mode & 0o111 == 0 || mode & 0o022 != 0 {
            return Err("Release must be executable and not group/world writable".into());
        }
        Ok(p)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    request_id: String,
    mode: String,
    expires_in_seconds: u64,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    handoff: bool,
    #[serde(default)]
    handoff_deadline: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    actor: String,
    request: Request,
    instance: String,
    expires_at: u64,
    phase: String,
    detail: String,
    executable: PathBuf,
    #[serde(default)]
    handoff_phase: String,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn db(e: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INSUFFICIENT_STORAGE,
        format!("Restart storage: {e}"),
    )
}
fn records(app: &App) -> Result<Vec<Record>, (StatusCode, String)> {
    let store = app.store.lock().unwrap();
    let mut q = store
        .connection
        .prepare("SELECT record FROM service_restarts ORDER BY rowid DESC")
        .map_err(db)?;
    q.query_map([], |r| r.get::<_, String>(0))
        .map_err(db)?
        .map(|r| serde_json::from_str(&r.map_err(db)?).map_err(db))
        .collect()
}
fn save(app: &App, r: &Record) -> Result<(), (StatusCode, String)> {
    app.store.lock().unwrap().connection.execute("INSERT INTO service_restarts(actor,request_id,payload,record) VALUES (?1,?2,?3,?4) ON CONFLICT(actor,request_id) DO UPDATE SET record=excluded.record", params![r.actor,r.request.request_id,serde_json::to_string(&r.request).map_err(db)?,serde_json::to_string(r).map_err(db)?]).map_err(db)?;
    Ok(())
}
fn pending(r: &Record) -> bool {
    matches!(r.phase.as_str(), "draining" | "dispatching")
}
pub fn allow_start(app: &App) -> Result<(), (StatusCode, String)> {
    if app.restart.blocked.load(Ordering::SeqCst) || app.restart.fault.lock().unwrap().is_some() {
        Err(fail(
            StatusCode::CONFLICT,
            "Service is draining for restart; new work is blocked",
        ))
    } else {
        Ok(())
    }
}
pub fn initialize(app: &App) -> Result<(), String> {
    app.store.lock().unwrap().connection.execute_batch("CREATE TABLE IF NOT EXISTS service_restarts(actor TEXT NOT NULL,request_id TEXT NOT NULL,payload TEXT NOT NULL,record TEXT NOT NULL,UNIQUE(actor,request_id));").map_err(|e|e.to_string())?;
    for mut r in records(app).map_err(|e| e.1)? {
        if r.instance != app.restart.instance && pending(&r) {
            if r.phase == "dispatching"
                && app.restart.supported()
                && std::env::var("AGENTWORKS_RESTART_REQUEST").ok().as_deref()
                    == Some(&format!("{}:{}", r.actor, r.request.request_id))
            {
                r.phase = "ready".into();
                r.detail = "Successor loaded saved state and is ready".into();
                if r.request.handoff {
                    r.handoff_phase = "pending".into();
                }
            } else {
                r.phase = "uncertain".into();
                r.detail =
                    "Process stopped before restart readiness; inspect before retrying".into();
            }
        }
        if r.handoff_phase == "claimed"
            || r.handoff_phase == "opening"
            || r.handoff_phase == "dispatched"
        {
            r.handoff_phase = "uncertain".into();
            r.detail = "Native handoff interrupted; manual inspection required".into();
        }
        save(app, &r).map_err(|e| e.1)?;
    }
    Ok(())
}
fn activity(app: &App) -> Value {
    let sessions: Vec<_> = app.sessions.lock().unwrap().values().cloned().collect();
    let mut turns = 0;
    let mut approvals = 0;
    let mut starting = 0;
    for s in sessions {
        let d = s.data.lock().unwrap();
        turns += usize::from(d.turn_active);
        starting += usize::from(matches!(
            d.status.as_str(),
            "starting" | "resuming" | "configuring"
        ));
        drop(d);
        approvals += s.approvals.lock().unwrap().len();
    }
    json!({"activeTurns":turns,"approvals":approvals,"startingHarnesses":starting,"workspacePreparations":usize::from(app.workspace_lock.try_lock().is_err())})
}
pub async fn status(State(app): State<App>) -> ApiResult {
    let rs = records(&app)?;
    let external = app
        .restart
        .control
        .as_ref()
        .and_then(|p| std::fs::read(p.join("outcome.json")).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    Ok(Json(
        json!({"instanceId":app.restart.instance,"pid":std::process::id(),"executable":std::env::current_exe().ok(),"restartSupported":app.restart.supported(),"draining":app.restart.blocked.load(Ordering::SeqCst),"activity":activity(&app),"requests":rs.into_iter().take(20).collect::<Vec<_>>(),"supervisorOutcome":external,"managerError":*app.restart.fault.lock().unwrap()}),
    ))
}
pub async fn post(State(app): State<App>, Json(req): Json<Request>) -> ApiResult {
    let _communication = app.communication_lock.lock().await;
    request(&app, "human", req).await.map(Json)
}
pub async fn request(app: &App, actor: &str, req: Request) -> Result<Value, (StatusCode, String)> {
    // Caller owns communication lock (HTTP wrapper or private MCP bridge).
    if app.restart.fault.lock().unwrap().is_some() {
        return Err(fail(
            StatusCode::CONFLICT,
            "Restart manager is paused; inspect storage and restart manually",
        ));
    }
    let _run = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    if actor != "human" {
        communication::require(app, session(app, actor)?.as_ref(), "restart_service")?;
    }
    let payload = serde_json::to_string(&req).map_err(db)?;
    let prior: Option<(String, String)> = app
        .store
        .lock()
        .unwrap()
        .connection
        .query_row(
            "SELECT payload,record FROM service_restarts WHERE actor=?1 AND request_id=?2",
            params![actor, req.request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    if let Some((old, record)) = prior {
        if old != payload {
            return Err(fail(
                StatusCode::CONFLICT,
                "Restart requestId is bound to another payload",
            ));
        }
        return Ok(serde_json::from_str(&record).map_err(db)?);
    }
    if req.request_id.is_empty()
        || req.request_id.len() > 100
        || req.request_id.chars().any(char::is_control)
        || !["wait", "interrupt"].contains(&req.mode.as_str())
        || !(5..=900).contains(&req.expires_in_seconds)
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use requestId, wait/interrupt mode and 5–900 second expiry",
        ));
    }
    if !app.restart.supported() {
        return Err(fail(
            StatusCode::CONFLICT,
            "Restart requires the external Agentworks supervisor",
        ));
    }
    let executable = app
        .restart
        .release(req.release.as_deref())
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    validate_protocol(&executable)
        .await
        .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    if records(app)?.iter().any(pending) {
        return Err(fail(
            StatusCode::CONFLICT,
            "Another restart is already pending",
        ));
    }
    if req.handoff {
        if actor == "human"
            || req.mode != "wait"
            || !req
                .handoff_deadline
                .is_some_and(|d| d > now() && d <= now() + 1_800_000)
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Native handoff requires requesting MCP session, wait mode and deadline within 30 minutes",
            ));
        }
        let s = session(app, actor)?;
        let d = s.data.lock().unwrap();
        if d.provider_session_id.is_none() || d.lifecycle.suspended {
            return Err(fail(
                StatusCode::CONFLICT,
                "Saved native context required for handoff",
            ));
        }
    }
    let r = Record {
        actor: actor.into(),
        instance: app.restart.instance.clone(),
        expires_at: now() + req.expires_in_seconds * 1000,
        request: req,
        phase: "draining".into(),
        detail: "Blocking new work; waiting for existing work".into(),
        executable,
        handoff_phase: String::new(),
    };
    save(app, &r)?;
    app.restart.blocked.store(true, Ordering::SeqCst);
    Ok(json!(r))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cancel {
    actor: String,
    request_id: String,
}
pub async fn cancel(State(app): State<App>, Json(req): Json<Cancel>) -> ApiResult {
    let _communication = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    for mut r in records(&app)? {
        if r.actor == req.actor
            && r.request.request_id == req.request_id
            && r.phase != "dispatching"
        {
            r.phase = "cancelled".into();
            r.handoff_phase = "cancelled".into();
            r.detail = "Human cancelled restart/handoff".into();
            save(&app, &r)?;
        }
    }
    app.restart
        .blocked
        .store(records(&app)?.iter().any(pending), Ordering::SeqCst);
    Ok(Json(json!({"ok":true})))
}
pub fn stop_requester(app: &App, id: &str) -> Result<(), (StatusCode, String)> {
    let result = (|| {
        for mut r in records(app)? {
            if r.actor == id
                && (pending(&r) || matches!(r.handoff_phase.as_str(), "pending" | "opening"))
            {
                r.phase = "cancelled".into();
                r.handoff_phase = "cancelled".into();
                r.detail = "Requesting session stopped".into();
                save(app, &r)?;
            }
        }
        app.restart
            .blocked
            .store(records(app)?.iter().any(pending), Ordering::SeqCst);
        Ok(())
    })();
    if let Err((_, ref error)) = result {
        *app.restart.fault.lock().unwrap() = Some(format!("Restart manager paused: {error}"));
        app.restart.blocked.store(true, Ordering::SeqCst);
    }
    result
}
fn backup(app: &App, r: &Record) -> Result<(), String> {
    let root = app
        .restart
        .control
        .as_ref()
        .ok_or("Missing supervisor")?
        .join("backups")
        .join(format!("{}-{}", now(), Uuid::new_v4()));
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    // Catalog + SQLite are captured under their guards after all producers have stopped.
    let catalog = app.catalog.lock().unwrap();
    let store = app.store.lock().unwrap();
    let destination = root.join("sessions.sqlite3");
    store
        .connection
        .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])
        .map_err(|e| e.to_string())?;
    let copy = rusqlite::Connection::open(&destination).map_err(|e| e.to_string())?;
    let integrity: String = copy
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if integrity != "ok" {
        return Err(integrity);
    }
    for table in [
        "sessions",
        "events",
        "agent_access",
        "agent_messages",
        "automations",
        "automation_runs",
        "agent_audit",
        "agent_starts",
        "agent_completions",
        "session_creations",
        "service_restarts",
    ] {
        let query = format!("SELECT count(*) FROM {table}");
        let original: i64 = store
            .connection
            .query_row(&query, [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let saved: i64 = copy
            .query_row(&query, [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if original != saved {
            return Err(format!("Backup preservation mismatch: {table}"));
        }
        let full = format!("SELECT * FROM {table} ORDER BY rowid");
        let rows = |c: &rusqlite::Connection| -> rusqlite::Result<Vec<Vec<String>>> {
            let mut q = c.prepare(&full)?;
            let n = q.column_count();
            q.query_map([], |r| {
                (0..n)
                    .map(|i| r.get_ref(i).map(|v| format!("{v:?}")))
                    .collect()
            })?
            .collect()
        };
        if rows(&store.connection).map_err(|e| e.to_string())?
            != rows(&copy).map_err(|e| e.to_string())?
        {
            return Err(format!("Backup content mismatch: {table}"));
        }
    }
    let bytes = serde_json::to_vec_pretty(&catalog.data).map_err(|e| e.to_string())?;
    let _: catalog::CatalogData = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    std::fs::write(root.join("catalog.json"), bytes).map_err(|e| e.to_string())?;
    std::fs::write(root.join("request.json"), serde_json::to_vec(r).unwrap())
        .map_err(|e| e.to_string())?;
    for name in ["sessions.sqlite3", "catalog.json", "request.json"] {
        std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        std::fs::File::open(root.join(name))
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub async fn scheduler(app: App) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if app.restart.fault.lock().unwrap().is_some() {
            continue;
        }
        if let Err(e) = tick(&app).await {
            app.restart.blocked.store(true, Ordering::SeqCst);
            *app.restart.fault.lock().unwrap() = Some(format!(
                "Restart manager paused after storage failure: {}. Inspect storage and restart manually.",
                e.1
            ));
            eprintln!("Restart manager paused: {}", e.1);
        }
    }
}
async fn tick(app: &App) -> Result<(), (StatusCode, String)> {
    let _communication = app.communication_lock.lock().await;
    let _run = app.run_lock.lock().await;
    let Ok(_workspace) = app.workspace_lock.try_lock() else {
        return Ok(());
    };
    for mut r in records(app)? {
        if r.phase == "draining" {
            let authorized = r.actor == "human"
                || session(app, &r.actor)
                    .ok()
                    .is_some_and(|s| communication::require(app, &s, "restart_service").is_ok());
            if !authorized || now() >= r.expires_at || !app.restart.supported() {
                r.phase = "cancelled".into();
                r.detail = "Expired, revoked or supervisor unavailable".into();
                save(app, &r)?;
                app.restart.blocked.store(false, Ordering::SeqCst);
                continue;
            }
            let sessions: Vec<_> = app.sessions.lock().unwrap().values().cloned().collect();
            let busy = sessions.iter().any(|s| {
                let d = s.data.lock().unwrap();
                let busy = d.turn_active
                    || matches!(d.status.as_str(), "starting" | "resuming" | "configuring");
                drop(d);
                busy || !s.approvals.lock().unwrap().is_empty()
            });
            if busy && r.request.mode == "wait" {
                continue;
            }
            if let Err(error) = validate_protocol(&r.executable).await {
                r.phase = "failed".into();
                r.detail = error;
                save(app, &r)?;
                app.restart.blocked.store(false, Ordering::SeqCst);
                continue;
            }
            if let Err(error) = backup(app, &r) {
                r.phase = "failed".into();
                r.detail = error;
                save(app, &r)?;
                app.restart.blocked.store(false, Ordering::SeqCst);
                continue;
            }
            for s in &sessions {
                // Preserve policies/timers and explicit-suspend metadata. No automatic replay.
                lifecycle::stop_worker(s).await;
                s.clear_approvals();
                if s.data.lock().unwrap().turn_active {
                    s.emit("interrupted",json!({"message":"Deliberate global service restart interrupted this turn; inspect results before continuing."}));
                }
                s.status("suspended");
                if s.data.lock().unwrap().storage_error.is_some() {
                    r.phase = "failed".into();
                    r.detail =
                        "Session state could not be saved; manual inspection required".into();
                    save(app, &r)?;
                    app.restart.blocked.store(false, Ordering::SeqCst);
                    return Ok(());
                }
            }
            let result = (|| -> Result<(), String> {
                // Revalidate the immutable release immediately before dispatch.
                if app.restart.release(r.request.release.as_deref())? != r.executable {
                    return Err("Release changed during drain".into());
                }
                backup(app, &r)?;
                r.phase = "dispatching".into();
                r.detail = "Backup verified; external supervisor owns restart".into();
                save(app, &r).map_err(|e| e.1)?;
                let ticket = json!({"identity":format!("{}:{}",r.actor,r.request.request_id),"executable":r.executable,"instance":r.instance});
                let dir = app.restart.control.as_ref().unwrap();
                let temp = dir.join("dispatch.tmp");
                std::fs::write(&temp, serde_json::to_vec(&ticket).unwrap())
                    .map_err(|e| e.to_string())?;
                std::fs::File::open(&temp)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())?;
                std::fs::rename(temp, dir.join("dispatch.json")).map_err(|e| e.to_string())?;
                std::fs::File::open(dir)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())?;
                Ok(())
            })();
            if let Err(e) = result {
                r.phase = "failed".into();
                r.detail = e;
                save(app, &r)?;
                app.restart.blocked.store(false, Ordering::SeqCst);
                continue;
            }
            std::process::exit(75);
        }
        if r.phase == "ready" && matches!(r.handoff_phase.as_str(), "pending" | "opening") {
            let s = session(app, &r.actor).ok();
            let valid = s.as_ref().is_some_and(|s| {
                communication::require(app, s, "restart_service").is_ok() && {
                    let d = s.data.lock().unwrap();
                    d.provider_session_id.is_some()
                        && !d.lifecycle.suspended
                        && !d.turn_active
                        && d.storage_error.is_none()
                }
            });
            if !valid || !r.request.handoff_deadline.is_some_and(|d| d > now()) {
                r.handoff_phase = "cancelled".into();
                r.detail = "Handoff expired, stopped, busy or revoked".into();
                save(app, &r)?;
                continue;
            }
            // A model continuation requires the external readiness acknowledgment, including
            // successful selector persistence. Backend boot alone does not prove supervision ready.
            let receipt = app
                .restart
                .control
                .as_ref()
                .and_then(|p| std::fs::read(p.join("outcome.json")).ok())
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let identity = format!("{}:{}", r.actor, r.request.request_id);
            let acknowledged = receipt.is_some_and(|v| {
                v["phase"] == "ready"
                    && v["identity"] == identity
                    && v["instanceId"] == app.restart.instance
                    && v["pid"] == std::process::id()
                    && v["executable"] == json!(r.executable)
            });
            if !acknowledged {
                continue;
            }
            let s = s.unwrap();
            let state = s.data.lock().unwrap().status.clone();
            if r.handoff_phase == "pending" && state == "suspended" {
                let capacity = app
                    .sessions
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|other| {
                        other.id != s.id
                            && !matches!(
                                other.data.lock().unwrap().status.as_str(),
                                "suspended" | "error" | "closed"
                            )
                    })
                    .count();
                if capacity >= 12
                    || crate::checked_cwd(app, &s.cwd.to_string_lossy()).is_err()
                    || !crate::installed(&s.provider)
                {
                    r.handoff_phase = "cancelled".into();
                    r.detail = "Native handoff capacity/workspace/harness unavailable".into();
                    save(app, &r)?;
                    continue;
                }
                r.handoff_phase = "opening".into();
                save(app, &r)?;
                s.status("resuming");
                if s.data.lock().unwrap().storage_error.is_some() {
                    return Err(db("Native handoff state could not be saved"));
                }
                crate::start_worker(&s);
                continue;
            }
            if state == "error" {
                r.handoff_phase = "uncertain".into();
                r.detail = "Native context could not open; manual inspection required".into();
                save(app, &r)?;
                continue;
            }
            if state != "idle" {
                continue;
            }
            let identity = format!("restart:{}:{}", r.actor, r.request.request_id);
            r.handoff_phase = "claimed".into();
            save(app, &r)?;
            let text = format!(
                "[Agentworks supervised restart continuation; request {identity}; actor {}] Inspect the current assignment, conversation and tool results after the service update. Continue only unfinished authorized work. Never replay an earlier tool or prompt. If finished or stopped, report and stop.",
                r.actor
            );
            {
                let mut d = s.data.lock().unwrap();
                if !d.requests.insert(identity.clone()) {
                    continue;
                }
                d.turn_active = true;
                d.status = "running".into();
            }
            s.emit(
                "user",
                json!({"text":text,"requestId":identity,"initiatedBy":"service-restart manager"}),
            );
            if s.data.lock().unwrap().storage_error.is_some() {
                continue;
            }
            r.handoff_phase = "dispatched".into();
            save(app, &r)?;
            if s.send_command(runtime::Command::Prompt(text)).is_err() {
                r.handoff_phase = "uncertain".into();
                r.detail = "Dispatch uncertain; inspect manually".into();
                save(app, &r)?;
                s.status("error");
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_server_refuses_supervision() {
        let m = Manager {
            instance: "fixture".into(),
            blocked: AtomicBool::new(false),
            fault: Mutex::new(None),
            control: None,
            releases: None,
        };
        assert!(!m.supported());
        assert!(m.release(Some("/bin/sh")).is_err());
    }
    #[test]
    fn old_grants_deny_restart() {
        let a: communication::Access =
            serde_json::from_str(r#"{"communicate":true,"startSessions":true}"#).unwrap();
        assert!(!a.restart_service);
        assert!(!communication::Access::default().restart_service);
    }
}

// Private MCP bridge already owns communication; only the actor's own request can be cancelled.
pub async fn cancel_own(app: &App, actor: &str, id: &str) -> Result<Value, (StatusCode, String)> {
    let _run = app.run_lock.lock().await;
    for mut r in records(app)? {
        if r.actor == actor && r.request.request_id == id && r.phase != "dispatching" {
            r.phase = "cancelled".into();
            r.handoff_phase = "cancelled".into();
            r.detail = "Requester cancelled".into();
            save(app, &r)?;
        }
    }
    app.restart
        .blocked
        .store(records(app)?.iter().any(pending), Ordering::SeqCst);
    Ok(json!({"ok":true}))
}

// Only trusted prepared executables are probed, never a shell command or arbitrary path.
async fn validate_protocol(path: &std::path::Path) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new(path)
        .arg("--service-protocol")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Release metadata unavailable: {e}"))?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or("No release metadata")?
            .take(4097)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Cannot read release metadata")?;
        if bytes.len() > 4096 {
            return Err("Release metadata exceeded limit");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "Release metadata process failed")?;
        let v: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Release does not support supervised restart protocol")?;
        if !status.success() || v["serviceProtocol"] != 1 {
            return Err("Incompatible supervised restart protocol");
        }
        Ok(())
    })
    .await;
    match result {
        Ok(r) => r.map_err(String::from),
        Err(_) => Err("Release metadata deadline exceeded".into()),
    }
}
