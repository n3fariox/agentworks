//! Durable restart decisions. Only systemd launches the successor: the manager repoints
//! the release directory's `current` link, writes a dispatch ticket and exits 75.
use crate::*;
use rusqlite::{OptionalExtension, params};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Manager {
    pub instance: String,
    blocked: AtomicBool,
    ready: AtomicBool,
    fault: Mutex<Option<String>>,
    control: Option<PathBuf>,
    releases: Option<PathBuf>,
    supervised: bool,
}
impl Manager {
    pub fn new() -> Self {
        Self {
            instance: Uuid::new_v4().to_string(),
            blocked: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            fault: Mutex::new(None),
            control: std::env::var_os("AGENTWORKS_SERVICE_DIR").map(PathBuf::from),
            releases: std::env::var_os("AGENTWORKS_RELEASE_DIR").map(PathBuf::from),
            supervised: crate::service_unit::under_systemd(),
        }
    }
    #[cfg(test)]
    pub(crate) fn block_admission_for_test(&self) {
        self.blocked.store(true, Ordering::SeqCst);
    }
    fn supported(&self) -> bool {
        // Environment alone cannot turn a direct invocation into a supported service.
        self.supervised && self.control.is_some() && self.releases.is_some()
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
    /// The successor instance that loaded this request's dispatch ticket.
    #[serde(default)]
    ready_instance: String,
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
/// Fence admission before the process begins graceful adapter shutdown.
pub fn begin_shutdown(app: &App) {
    let _ = crate::service_unit::notify("STOPPING=1");
    app.host_usage.shutdown();
    app.restart.blocked.store(true, Ordering::SeqCst);
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
    // Every start consumes the ticket, so a later cold start never reuses it.
    let ticket = app.restart.control.as_ref().and_then(|dir| {
        let path = dir.join("dispatch.json");
        let ticket = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let _ = std::fs::remove_file(path);
        ticket
    });
    let executable = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
    for mut r in records(app).map_err(|e| e.1)? {
        if r.instance != app.restart.instance && pending(&r) {
            let identity = format!("{}:{}", r.actor, r.request.request_id);
            let ticketed = ticket.as_ref().is_some_and(|t| {
                t["identity"] == identity
                    && t["instance"] == r.instance
                    && t["at"]
                        .as_u64()
                        .is_some_and(|at| now().saturating_sub(at) < 600_000)
            });
            if r.phase == "dispatching"
                && ticketed
                && app.restart.supported()
                && executable.as_ref() == Some(&r.executable)
            {
                r.phase = "ready".into();
                r.detail = "Successor loaded saved state and is ready".into();
                r.ready_instance = app.restart.instance.clone();
                if r.request.handoff {
                    r.handoff_phase = "pending".into();
                }
            } else if r.phase == "dispatching" && ticketed {
                r.phase = "uncertain".into();
                r.detail = format!(
                    "Release {} did not become ready; systemd started {} instead. Inspect journalctl --user -u agentworks before retrying",
                    r.executable.display(),
                    executable
                        .as_ref()
                        .map_or("an unknown executable".into(), |p| p.display().to_string())
                );
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
/// Called once the listener serves requests. Records this release as the rollback
/// target, then tells systemd the start succeeded.
pub fn ready(app: &App) {
    if !app.restart.supported() {
        return;
    }
    let stable = app.restart.release(None).and_then(|exe| {
        crate::service_unit::point(
            app.restart.releases.as_ref().unwrap(),
            crate::service_unit::STABLE,
            &exe,
        )
        .map_err(|e| e.to_string())
    });
    match crate::service_unit::notify("READY=1") {
        Ok(true) if stable.is_ok() => app.restart.ready.store(true, Ordering::SeqCst),
        Ok(_) => eprintln!(
            "Agentworks: could not record the stable release: {}",
            stable.err().unwrap_or_default()
        ),
        Err(e) => eprintln!("Agentworks: systemd readiness notification failed: {e}"),
    }
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
    Ok(Json(
        json!({"instanceId":app.restart.instance,"pid":std::process::id(),"executable":std::env::current_exe().ok(),"restartSupported":app.restart.supported(),"ready":app.restart.ready.load(Ordering::SeqCst),"draining":app.restart.blocked.load(Ordering::SeqCst),"activity":activity(&app),"requests":rs.into_iter().take(20).collect::<Vec<_>>(),"managerError":*app.restart.fault.lock().unwrap()}),
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
            "Restart requires the Agentworks systemd user service (agentworks service --install)",
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
        ready_instance: String::new(),
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
    let backups = app
        .restart
        .control
        .as_ref()
        .ok_or("Missing service directory")?
        .join("backups");
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&backups)
        .map_err(|e| e.to_string())?;
    let _maintenance = crate::recovery::lock(&backups).map_err(|e| e.to_string())?;
    let root = backups.join(format!("{}-{}", now(), Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
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
    crate::recovery::compare(&store.connection, &copy).map_err(|e| e.to_string())?;
    drop(copy);
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
    crate::recovery::publish(&root).map_err(|e| e.to_string())?;
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
                r.detail = "Expired, revoked or systemd service unavailable".into();
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
            // Close adapters concurrently so one bounded grace period covers
            // the whole service, rather than multiplying it by session count.
            futures::future::join_all(sessions.iter().map(|s| lifecycle::stop_worker(s))).await;
            for s in &sessions {
                // Preserve policies/timers and explicit-suspend metadata. No automatic replay.
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
                r.detail = "Backup verified; systemd owns restart".into();
                save(app, &r).map_err(|e| e.1)?;
                let ticket = json!({"identity":format!("{}:{}",r.actor,r.request.request_id),"executable":r.executable,"instance":r.instance,"at":now()});
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
                // systemd launches whatever `current` names once this process exits 75.
                crate::service_unit::point(
                    r.executable.parent().unwrap(),
                    crate::service_unit::CURRENT,
                    &r.executable,
                )
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
            crate::terminal::shutdown().await;
            app.hosts.stop_all();
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
            // A model continuation waits until systemd has the readiness notification and
            // this release is recorded as stable. Backend boot alone does not prove that.
            if !app.restart.ready.load(Ordering::SeqCst) || r.ready_instance != app.restart.instance
            {
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
                crate::start_worker(app, &s);
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
            if s.send_command(session_runtime::Command::Prompt(text))
                .is_err()
            {
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
            ready: AtomicBool::new(false),
            fault: Mutex::new(None),
            control: None,
            releases: None,
            supervised: false,
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
        if !status.success() || v["serviceProtocol"] != 2 {
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
