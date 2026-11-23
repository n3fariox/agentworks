//! Service side of session secrets: Settings → Secrets and Session settings routes,
//! harness delivery, the bridge half of `run_with_secrets` with its approvals, and
//! per-session scrubbing. Storage, sealing and assignment rules are in
//! `agentworks_secrets`.
use crate::{ApiResult, App, Session, communication as mail, fail};
use agentworks_database::secrets::{SealedSecret, SealedValue, SecretAssignment, SecretGrant};
use agentworks_secrets::{
    model::{
        Approval, Assignment, Delegation, Delivery, validate_description, validate_name,
        validate_value,
    },
    vault::{self, Opened, PasswordError, Vault},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const MAX_ASSIGNMENTS: usize = 64;

enum VaultState {
    /// The key file is missing, damaged or unreadable.
    Unavailable(String),
    /// Password-protected and not unlocked. Unlocking reads the key file again.
    Locked,
    Unlocked {
        vault: Arc<Vault>,
        /// Whether the key file has a password, so the vault can be locked again.
        password: bool,
    },
}
struct Storage {
    path: PathBuf,
    state: Mutex<VaultState>,
    /// When a value was last sealed or opened; the idle lock counts from here.
    used: Mutex<Instant>,
    /// Consecutive failed unlocks and when the last one happened.
    failures: Mutex<(u32, Instant)>,
}
static STORAGE: OnceLock<Storage> = OnceLock::new();
/// Minutes without use before a password-protected vault locks itself; 0 never.
static LOCK_MINUTES: AtomicU32 = AtomicU32::new(crate::catalog::DEFAULT_SECRETS_LOCK_MINUTES);

/// Opens the key file, creating it on first use. Failure leaves secrets unavailable
/// rather than stopping the server; a damaged key file is never replaced. A
/// password-protected key file starts locked.
pub fn init(path: PathBuf) {
    let state = match vault::open(&path) {
        Ok(Opened::Unlocked(vault)) => VaultState::Unlocked {
            vault: Arc::from(vault),
            password: false,
        },
        Ok(Opened::Locked(_)) => VaultState::Locked,
        Err(error) => {
            eprintln!("agentworks: secrets unavailable: {error}");
            VaultState::Unavailable(error)
        }
    };
    let _ = STORAGE.set(Storage {
        path,
        state: Mutex::new(state),
        used: Mutex::new(Instant::now()),
        failures: Mutex::new((0, Instant::now())),
    });
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            lock_if_idle();
        }
    });
}
pub fn set_lock_minutes(minutes: u32) {
    LOCK_MINUTES.store(minutes, Ordering::Relaxed);
}
fn lock_if_idle() {
    let Some(storage) = STORAGE.get() else { return };
    if idle_expired(
        LOCK_MINUTES.load(Ordering::Relaxed),
        storage.used.lock().unwrap().elapsed(),
    ) {
        lock(storage);
    }
}
fn idle_expired(minutes: u32, idle: Duration) -> bool {
    minutes != 0 && idle >= Duration::from_secs(60 * u64::from(minutes))
}
/// Drops the data key of a password-protected vault. Values being used right now finish.
fn lock(storage: &Storage) -> bool {
    let mut state = storage.state.lock().unwrap();
    if !matches!(&*state, VaultState::Unlocked { password: true, .. }) {
        return false;
    }
    match vault::open(&storage.path) {
        Ok(Opened::Locked(_)) => *state = VaultState::Locked,
        // The file changed under us; never keep a key it does not match.
        Ok(Opened::Unlocked(_)) | Err(_) => {
            *state = VaultState::Unavailable("the key file changed; restart Agentworks".into())
        }
    }
    true
}
fn touch(storage: &Storage) {
    *storage.used.lock().unwrap() = Instant::now();
}

/// The unlocked vault, counting as a use for the idle lock.
fn loaded() -> Option<Arc<Vault>> {
    let storage = STORAGE.get()?;
    let vault = match &*storage.state.lock().unwrap() {
        VaultState::Unlocked { vault, .. } => vault.clone(),
        _ => return None,
    };
    touch(storage);
    Some(vault)
}
/// The unlocked vault without counting a use, for listings.
fn peek() -> Option<Arc<Vault>> {
    STORAGE.get().and_then(|s| match &*s.state.lock().unwrap() {
        VaultState::Unlocked { vault, .. } => Some(vault.clone()),
        _ => None,
    })
}
fn is_locked() -> bool {
    STORAGE
        .get()
        .is_some_and(|s| matches!(&*s.state.lock().unwrap(), VaultState::Locked))
}
fn vault() -> Result<Arc<Vault>, (StatusCode, String)> {
    if let Some(vault) = loaded() {
        return Ok(vault);
    }
    match STORAGE.get().map(|s| s.state.lock().unwrap()) {
        Some(state) => match &*state {
            VaultState::Locked => Err(locked()),
            VaultState::Unavailable(error) => Err(unavailable(error)),
            VaultState::Unlocked { .. } => unreachable!("loaded() returns an unlocked vault"),
        },
        None => Err(unavailable("not initialized")),
    }
}
fn locked() -> (StatusCode, String) {
    fail(
        StatusCode::LOCKED,
        "Secrets are locked. Unlock them in Settings → Secrets",
    )
}
fn unavailable(reason: &str) -> (StatusCode, String) {
    fail(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("Secret storage is unavailable: {reason}"),
    )
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn bad(message: impl Into<String>) -> (StatusCode, String) {
    fail(StatusCode::BAD_REQUEST, message)
}
fn db_error(_: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not save or read secrets",
    )
}
/// Public view of a secret: never the value. `readable` is false when the value was
/// stored under a different key file and must be entered again.
fn view(secret: &SealedSecret, vault: Option<&Vault>) -> Value {
    json!({"id":secret.id,"name":secret.name,"description":secret.description,"length":secret.length,
        "created":secret.created,"updated":secret.updated,"readable":readable(secret, vault)})
}
fn value(secret: &SealedSecret, vault: &Vault) -> Option<Zeroizing<String>> {
    vault.open_value(&secret.id, &secret.name, &secret.sealed)
}
/// `None` without a vault.
fn readable(secret: &SealedSecret, vault: Option<&Vault>) -> Option<bool> {
    vault.map(|vault| value(secret, vault).is_some())
}
fn not_found() -> (StatusCode, String) {
    fail(StatusCode::NOT_FOUND, "Secret not found")
}
/// Stored policy names that no longer parse fall back to the most restrictive policy.
fn assignment(row: SecretAssignment) -> Assignment {
    Assignment {
        delivery: Delivery::parse(&row.delivery).unwrap_or(Delivery::Tool),
        delegation: Delegation::parse(&row.delegation).unwrap_or(Delegation::Restricted),
        approval: Approval::parse(&row.approval).unwrap_or(Approval::EachUse),
        secret_id: row.secret_id,
        name: row.name,
        origin: row.origin,
    }
}
fn assignments(store: &agentworks_database::Store, session: &str) -> mail::Result<Vec<Assignment>> {
    Ok(store
        .secret_assignments(session)
        .map_err(db_error)?
        .into_iter()
        .map(assignment)
        .collect())
}

/// Lock state for Settings → Secrets and the unlock banner.
fn status() -> Value {
    let storage = STORAGE.get();
    let state = storage.map(|s| s.state.lock().unwrap());
    let (available, locked, password, error) = match state.as_deref() {
        Some(VaultState::Unlocked { password, .. }) => (true, false, *password, None),
        Some(VaultState::Locked) => (true, true, true, None),
        Some(VaultState::Unavailable(error)) => (false, false, false, Some(error.clone())),
        None => (false, false, false, Some("not initialized".to_owned())),
    };
    json!({"available": available, "locked": locked, "password": password, "error": error,
        "lockMinutes": LOCK_MINUTES.load(Ordering::Relaxed),
        "keyFile": storage.map(|s| s.path.display().to_string())})
}
pub async fn get_status() -> Json<Value> {
    Json(status())
}

/// Failed unlocks wait 1, 2, 4 … up to 60 seconds before the next attempt.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.saturating_sub(1).min(6)).min(Duration::from_secs(60))
}
fn audit_human(app: &App, action: &str, outcome: &str) -> mail::Result<()> {
    app.store
        .lock()
        .unwrap()
        .audit("human", action, "secrets", outcome)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unlock {
    password: Zeroizing<String>,
}
pub async fn unlock(State(app): State<App>, Json(input): Json<Unlock>) -> ApiResult {
    let storage = STORAGE
        .get()
        .ok_or_else(|| unavailable("not initialized"))?;
    {
        let (failures, last) = *storage.failures.lock().unwrap();
        let wait = backoff(failures).saturating_sub(last.elapsed());
        if failures > 0 && !wait.is_zero() {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                format!("Try again in {} seconds", wait.as_secs().max(1)),
            ));
        }
    }
    let file = match &*storage.state.lock().unwrap() {
        VaultState::Locked => vault::open(&storage.path).map_err(|e| unavailable(&e))?,
        VaultState::Unlocked { .. } => return Ok(Json(status())),
        VaultState::Unavailable(error) => return Err(unavailable(error)),
    };
    let Opened::Locked(locked) = file else {
        return Err(unavailable("the key file changed; restart Agentworks"));
    };
    // Key derivation is deliberately slow; keep it off the async workers.
    let password = input.password;
    let result = tokio::task::spawn_blocking(move || locked.unlock(&password))
        .await
        .map_err(|_| unavailable("unlock failed"))?;
    match result {
        Ok(vault) => {
            {
                let mut state = storage.state.lock().unwrap();
                if matches!(&*state, VaultState::Locked) {
                    *state = VaultState::Unlocked {
                        vault: Arc::new(vault),
                        password: true,
                    };
                }
            }
            *storage.failures.lock().unwrap() = (0, Instant::now());
            touch(storage);
            audit_human(&app, "secrets.unlock", "ok")?;
            restart_started_locked(&app);
            Ok(Json(status()))
        }
        Err(PasswordError::Wrong) => {
            let mut failures = storage.failures.lock().unwrap();
            *failures = (failures.0 + 1, Instant::now());
            drop(failures);
            audit_human(&app, "secrets.unlock", "failed")?;
            Err(fail(StatusCode::FORBIDDEN, "The password is not correct"))
        }
        Err(error) => Err(unavailable(&error.to_string())),
    }
}
/// Sessions that started while locked restart now, or after their turn, with their
/// environment secrets.
fn restart_started_locked(app: &App) {
    let ids: Vec<String> = started_locked().drain().collect();
    if ids.is_empty() {
        return;
    }
    let app = app.clone();
    tokio::spawn(async move {
        for id in ids {
            let Some(s) = app.sessions.lock().unwrap().get(&id).cloned() else {
                continue;
            };
            let _gate = app.communication_lock.lock().await;
            if let Err((_, error)) = crate::communication::refresh_harness(&app, &s, false).await {
                tracing::warn!(session = %id, "Could not restart with unlocked secrets: {error}");
            }
        }
    });
}
pub async fn lock_now(State(app): State<App>) -> ApiResult {
    let storage = STORAGE
        .get()
        .ok_or_else(|| unavailable("not initialized"))?;
    if lock(storage) {
        audit_human(&app, "secrets.lock", "ok")?;
    }
    Ok(Json(status()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Password {
    #[serde(default)]
    current: Zeroizing<String>,
    /// Empty removes the password.
    password: Zeroizing<String>,
}
/// Sets, changes or removes the password by re-wrapping the data key. Needs the vault
/// unlocked, and the current password when there is one.
pub async fn set_password(State(app): State<App>, Json(input): Json<Password>) -> ApiResult {
    let storage = STORAGE
        .get()
        .ok_or_else(|| unavailable("not initialized"))?;
    vault::validate_password(&input.password).map_err(bad)?;
    let vault = vault()?;
    let path = storage.path.clone();
    let (current, new) = (input.current, input.password);
    let removing = new.is_empty();
    let result = tokio::task::spawn_blocking(move || {
        vault.set_password(&path, &current, &new).map(|()| vault)
    })
    .await
    .map_err(|_| unavailable("the password could not be saved"))?;
    match result {
        Ok(vault) => {
            *storage.state.lock().unwrap() = VaultState::Unlocked {
                vault,
                password: !removing,
            };
            touch(storage);
            audit_human(
                &app,
                "secrets.password",
                if removing { "removed" } else { "set" },
            )?;
            Ok(Json(status()))
        }
        Err(PasswordError::Wrong) => {
            audit_human(&app, "secrets.password", "failed")?;
            Err(fail(
                StatusCode::FORBIDDEN,
                "The current password is not correct",
            ))
        }
        Err(PasswordError::Invalid(message)) => Err(bad(message)),
        Err(PasswordError::Storage(message)) => Err(unavailable(&message)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockTimeout {
    minutes: u32,
}
pub async fn set_lock_timeout(State(app): State<App>, Json(input): Json<LockTimeout>) -> ApiResult {
    if input.minutes != 0
        && !(5..=crate::catalog::MAX_SECRETS_LOCK_MINUTES).contains(&input.minutes)
    {
        return Err(bad("Choose 5 minutes to 24 hours, or never"));
    }
    let _workspace = app.workspace_lock.lock().await;
    crate::persist_settings(&app, |current| crate::catalog::Settings {
        secrets_lock_minutes: Some(input.minutes),
        ..current.clone()
    })?;
    set_lock_minutes(input.minutes);
    Ok(Json(status()))
}

pub async fn list(State(app): State<App>) -> ApiResult {
    // Listing is not a use, so it does not hold off the idle lock.
    let vault = peek();
    let vault = vault.as_deref();
    let store = app.store.lock().unwrap();
    let counts = store.secret_session_counts().map_err(db_error)?;
    let secrets: Vec<Value> = store
        .secrets()
        .map_err(db_error)?
        .iter()
        .map(|secret| {
            let mut v = view(secret, vault);
            v["sessions"] = json!(counts.get(&secret.id).unwrap_or(&0));
            v
        })
        .collect();
    let mut out = status();
    out["secrets"] = json!(secrets);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct Create {
    name: String,
    #[serde(default)]
    description: String,
    value: Zeroizing<String>,
}

pub async fn create(State(app): State<App>, Json(input): Json<Create>) -> ApiResult {
    let vault = vault()?;
    let name = input.name.trim();
    validate_name(name).map_err(bad)?;
    validate_value(&input.value).map_err(bad)?;
    let description = validate_description(&input.description).map_err(bad)?;
    let id = uuid::Uuid::new_v4().to_string();
    let (nonce, body) = vault
        .seal_value(&id, name, &input.value)
        .map_err(|e| unavailable(&e))?;
    let sealed = SealedValue {
        nonce: &nonce,
        body: &body,
        length: input.value.len(),
    };
    let saved = app
        .store
        .lock()
        .unwrap()
        .create_secret(&id, name, &description, &sealed, now())
        .map_err(db_error)?
        .ok_or_else(|| {
            fail(
                StatusCode::CONFLICT,
                format!("A secret named {name} already exists"),
            )
        })?;
    Ok(Json(view(&saved, Some(&vault))))
}

#[derive(Deserialize)]
pub struct Update {
    description: Option<String>,
    value: Option<Zeroizing<String>>,
}

pub async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Update>,
) -> ApiResult {
    let description = input
        .description
        .as_deref()
        .map(validate_description)
        .transpose()
        .map_err(bad)?;
    let vault = match &input.value {
        Some(value) => {
            validate_value(value).map_err(bad)?;
            Some(vault()?)
        }
        None => None,
    };
    /// Sealing failures are storage-unavailable errors, not database errors.
    enum Failure {
        Database,
        Seal(String),
    }
    impl From<agentworks_database::StorageError> for Failure {
        fn from(_: agentworks_database::StorageError) -> Self {
            Self::Database
        }
    }
    let seal = |current: &SealedSecret| -> Result<(Vec<u8>, Vec<u8>, usize), Failure> {
        let (vault, value) = (vault.as_ref().unwrap(), input.value.as_ref().unwrap());
        let (nonce, body) = vault
            .seal_value(&current.id, &current.name, value)
            .map_err(Failure::Seal)?;
        Ok((nonce, body, value.len()))
    };
    let saved = app
        .store
        .lock()
        .unwrap()
        .update_secret(
            &id,
            description.as_deref(),
            vault
                .as_ref()
                .map(|_| &seal as &dyn Fn(&SealedSecret) -> Result<_, Failure>),
            now(),
        )
        .map_err(|failure| match failure {
            Failure::Database => db_error(""),
            Failure::Seal(error) => unavailable(&error),
        })?
        .ok_or_else(not_found)?;
    Ok(Json(view(&saved, peek().as_deref())))
}

pub async fn delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    if !app
        .store
        .lock()
        .unwrap()
        .delete_secret(&id, now())
        .map_err(db_error)?
    {
        return Err(not_found());
    }
    Ok(Json(json!({"deleted": id})))
}

/// A session's `environment` secrets, decrypted for one harness start.
#[derive(Default)]
pub struct Environment {
    pub vars: Vec<(String, Zeroizing<String>)>,
    /// Names that could not be decrypted (storage unavailable or value needs re-entry).
    pub unavailable: Vec<String>,
    /// The vault was locked; the session restarts with them once it is unlocked.
    pub locked: bool,
}
/// Sessions whose harness started without environment secrets because the vault was
/// locked.
static STARTED_LOCKED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
fn started_locked() -> std::sync::MutexGuard<'static, HashSet<String>> {
    STARTED_LOCKED.get_or_init(Default::default).lock().unwrap()
}
pub fn environment(store: &agentworks_database::Store, session: &str) -> mail::Result<Environment> {
    let mut out = Environment::default();
    let vault = loaded();
    for a in assignments(store, session)? {
        if a.delivery != Delivery::Environment {
            continue;
        }
        let plain = match &vault {
            Some(vault) => store
                .secret(&a.secret_id)
                .map_err(db_error)?
                .and_then(|secret| value(&secret, vault)),
            None => None,
        };
        match plain {
            Some(plain) => out.vars.push((a.name, plain)),
            None => out.unavailable.push(a.name),
        }
    }
    out.locked = !out.unavailable.is_empty() && is_locked();
    if out.locked {
        started_locked().insert(session.to_owned());
    } else {
        started_locked().remove(session);
    }
    Ok(out)
}
/// Forms of each value delivered to a session, replaced in everything the session
/// stores or publishes.
static SCRUBBERS: std::sync::LazyLock<agentworks_secrets::redact::Scrubbers> =
    std::sync::LazyLock::new(Default::default);
pub fn register(session: &str, vars: &[(String, Zeroizing<String>)]) {
    SCRUBBERS.register(session, vars);
}
pub fn forget(session: &str) {
    SCRUBBERS.forget(session);
    delivered().remove(session);
    allowed_uses().remove(session);
    started_locked().remove(session);
}
pub fn scrub_text(session: &str, text: &str) -> String {
    SCRUBBERS.text(session, text)
}
pub fn scrub(session: &str, value: &mut Value) {
    SCRUBBERS.json(session, value);
}

static DELIVERED: OnceLock<Mutex<HashMap<String, HashSet<String>>>> = OnceLock::new();
fn delivered() -> std::sync::MutexGuard<'static, HashMap<String, HashSet<String>>> {
    DELIVERED.get_or_init(Default::default).lock().unwrap()
}
/// Records which names the running harness has in its environment.
pub fn mark_delivered(session: &str, vars: &[(String, Zeroizing<String>)]) {
    delivered().insert(
        session.to_owned(),
        vars.iter().map(|(name, _)| name.clone()).collect(),
    );
}
/// Per-session "allow for this session" answers, by secret name.
static ALLOWED_USES: OnceLock<Mutex<HashMap<String, HashSet<String>>>> = OnceLock::new();
fn allowed_uses() -> std::sync::MutexGuard<'static, HashMap<String, HashSet<String>>> {
    ALLOWED_USES.get_or_init(Default::default).lock().unwrap()
}

/// `list_session_secrets`: assigned secrets with instructions. Never values.
pub fn agent_list(app: &App, session: &str) -> mail::Result<Value> {
    let store = app.store.lock().unwrap();
    let assigned = assignments(&store, session)?;
    let environment = delivered().get(session).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for a in &assigned {
        let description = store
            .secret(&a.secret_id)
            .map_err(db_error)?
            .ok_or_else(not_found)?
            .description;
        out.push(
            json!({"name":a.name,"description":description,"delivery":a.delivery,
            "approval":a.approval,"usage":a.usage(environment.contains(&a.name))}),
        );
    }
    Ok(json!({"secrets":out,
        "rules":"Use a secret only for the task it was assigned for. Do not print, log, commit or send a value anywhere it is not needed."}))
}

/// How long a use waits for the human before the approval is withdrawn.
const APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(600);
const MAX_NAMES_PER_USE: usize = 16;
struct PendingUse {
    session: String,
    names: Vec<String>,
    waiting: Waiting,
}
enum Waiting {
    /// An approval card; the human's answer arrives here.
    Human(oneshot::Receiver<Option<Value>>),
    /// The vault is locked. `approved` carries an approval given before it locked.
    Unlock {
        approved: Approved,
        request: UseRequest,
        deadline: Instant,
    },
}
static PENDING_USES: OnceLock<Mutex<HashMap<String, PendingUse>>> = OnceLock::new();
fn pending_uses() -> std::sync::MutexGuard<'static, HashMap<String, PendingUse>> {
    PENDING_USES.get_or_init(Default::default).lock().unwrap()
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UseRequest {
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    env_keys: Vec<String>,
    #[serde(default)]
    file_keys: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    /// Polls an earlier request that is waiting for the human.
    #[serde(default)]
    approval: Option<String>,
}

/// Bridge side of `run_with_secrets`: authorizes one call and returns its values to the
/// facade, which runs the command on the session's host. Each request is short because
/// the bridge holds the communication lock; waiting for a human is done by polling.
pub fn resolve_use(app: &App, s: &Arc<Session>, arguments: Value) -> mail::Result<Value> {
    let request: UseRequest =
        serde_json::from_value(arguments).map_err(|_| bad("Invalid run_with_secrets request"))?;
    if let Some(id) = &request.approval {
        return poll_use(app, s, id);
    }
    let mut names = request.names.clone();
    names.sort();
    names.dedup();
    if names.is_empty() || names.len() > MAX_NAMES_PER_USE {
        return Err(bad("Use between 1 and 16 secrets in one call"));
    }
    proceed(app, s, names, request, Approved::No)
}
/// Releases the values, or starts the wait the use needs: an unlock, then an approval.
fn proceed(
    app: &App,
    s: &Arc<Session>,
    names: Vec<String>,
    request: UseRequest,
    approved: Approved,
) -> mail::Result<Value> {
    match authorize(app, s, &names, approved)? {
        Outcome::Released(mut released) => {
            if approved != Approved::No {
                released["approval"] = json!(if approved == Approved::ForSession {
                    "The user approved this use, and further uses of these secrets in this session."
                } else {
                    "The user approved this use."
                });
            }
            Ok(released)
        }
        Outcome::Ask => ask(s, names, &request),
        Outcome::Locked => Ok(wait_for_unlock(s, names, request, approved)),
    }
}
/// Holds a use until the human unlocks secrets; the facade polls like an approval.
fn wait_for_unlock(
    s: &Arc<Session>,
    names: Vec<String>,
    request: UseRequest,
    approved: Approved,
) -> Value {
    let id = uuid::Uuid::new_v4().to_string();
    s.emit("secrets_locked", json!({"secrets": names}));
    pending_uses().insert(
        id.clone(),
        PendingUse {
            session: s.id.clone(),
            names,
            waiting: Waiting::Unlock {
                approved,
                request,
                deadline: Instant::now() + APPROVAL_WAIT,
            },
        },
    );
    let expiring = id.clone();
    tokio::spawn(async move {
        // Leave time for a last poll to see the outcome.
        tokio::time::sleep(APPROVAL_WAIT + Duration::from_secs(30)).await;
        pending_uses().remove(&expiring);
    });
    json!({"pending": id})
}
/// Whether the human approved this call, and for how long.
#[derive(Clone, Copy, PartialEq)]
enum Approved {
    No,
    Once,
    ForSession,
}
/// The assignment rows for `names`; every name must be assigned to this session, and
/// unassigned and missing look the same.
fn pick<'a, T>(
    rows: &'a [(Assignment, T)],
    names: &[String],
) -> mail::Result<Vec<&'a (Assignment, T)>> {
    names
        .iter()
        .map(|name| {
            rows.iter().find(|(a, _)| &a.name == name).ok_or_else(|| {
                fail(
                    StatusCode::FORBIDDEN,
                    format!("{name} is not assigned to this session"),
                )
            })
        })
        .collect()
}
/// Whether an unapproved use must ask the human first.
fn needs_approval<T>(picked: &[&(Assignment, T)], allowed: Option<&HashSet<String>>) -> bool {
    picked.iter().any(|(a, _)| {
        a.approval == Approval::EachUse && !allowed.is_some_and(|set| set.contains(&a.name))
    })
}
enum Outcome {
    Released(Value),
    /// The human must approve first; nothing was released.
    Ask,
    /// The vault is locked; nothing was released or recorded.
    Locked,
}
/// Checks the current assignments and releases their values under one store lock.
/// Assignment changes take the same lock and clear session-wide approvals before
/// releasing it, so a use is always judged and served by one state. `None` means the
/// human must be asked first; nothing was released.
fn authorize(
    app: &App,
    s: &Session,
    names: &[String],
    approved: Approved,
) -> mail::Result<Outcome> {
    let store = app.store.lock().unwrap();
    let rows: Vec<(Assignment, SealedSecret)> = store
        .assigned_secrets(&s.id)
        .map_err(db_error)?
        .into_iter()
        .map(|(row, secret)| (assignment(row), secret))
        .collect();
    let picked = pick(&rows, names)?;
    // Unlock first, then approve: the card is shown only once values can be released.
    let vault = match vault() {
        Ok(vault) => vault,
        Err(_) if is_locked() => return Ok(Outcome::Locked),
        Err(error) => return Err(error),
    };
    {
        let mut allowed = allowed_uses();
        if approved == Approved::No && needs_approval(&picked, allowed.get(&s.id)) {
            return Ok(Outcome::Ask);
        }
        if approved == Approved::ForSession {
            allowed
                .entry(s.id.clone())
                .or_default()
                .extend(names.iter().cloned());
        }
    }
    let mut values = Vec::new();
    for (a, secret) in picked {
        let plain = value(secret, &vault).ok_or_else(|| {
            fail(
                StatusCode::CONFLICT,
                format!("{} must be entered again in Settings → Secrets", a.name),
            )
        })?;
        values.push((a.name.clone(), plain));
    }
    store.audit(&s.id, "secret.use", &names.join(","), "released")?;
    drop(store);
    register(&s.id, &values);
    let map: serde_json::Map<String, Value> = values
        .iter()
        .map(|(name, value)| (name.clone(), json!(value.as_str())))
        .collect();
    Ok(Outcome::Released(json!({"values":map,"cwd":s.cwd()})))
}
fn ask(s: &Arc<Session>, names: Vec<String>, request: &UseRequest) -> mail::Result<Value> {
    let id = uuid::Uuid::new_v4().to_string();
    let (answer, receiver) = oneshot::channel();
    let approval = crate::Approval {
        request: json!({
            "toolCall":{"toolCallId":id,"title":format!("Run a command with {}", names.join(", ")),
                "rawInput":{"argv":request.argv,"envKeys":request.env_keys,"fileKeys":request.file_keys,"cwd":request.cwd}},
            "options":[
                {"optionId":"allow-once","name":"Allow once","kind":"allow_once"},
                {"optionId":"allow-session","name":"Allow for this session","kind":"allow_always"},
                {"optionId":"deny","name":"Deny","kind":"reject_once"}]}),
        kind: "permission",
        unsupported: None,
        answer,
    };
    {
        let mut approvals = s.approvals.lock().unwrap();
        let mut data = s.data.lock().unwrap();
        if data.lifecycle.suspended || !data.turn_active || data.status == "cancelling" {
            return Err(fail(
                StatusCode::CONFLICT,
                "Secrets that need approval can only be used during a turn",
            ));
        }
        let event = approval.event(&id);
        approvals.insert(id.clone(), approval);
        data.status = "waiting".into();
        drop(data);
        // Register before publishing: the human can answer as soon as the card appears.
        pending_uses().insert(
            id.clone(),
            PendingUse {
                session: s.id.clone(),
                names,
                waiting: Waiting::Human(receiver),
            },
        );
        s.emit("approval", event);
        s.emit("status", json!({"status":"waiting"}));
    }
    let (session, expiring) = (s.clone(), id.clone());
    tokio::spawn(async move {
        tokio::time::sleep(APPROVAL_WAIT).await;
        crate::elicitation::withdraw(&session, &expiring);
        // Leave time for a last poll to see the outcome.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        pending_uses().remove(&expiring);
    });
    Ok(json!({"pending": id}))
}
fn poll_use(app: &App, s: &Arc<Session>, id: &str) -> mail::Result<Value> {
    let (answer, names) = {
        let mut pending = pending_uses();
        let Some(entry) = pending.get_mut(id).filter(|p| p.session == s.id) else {
            return Err(fail(
                StatusCode::NOT_FOUND,
                "This approval is no longer pending",
            ));
        };
        match &mut entry.waiting {
            Waiting::Human(answer) => match answer.try_recv() {
                Err(oneshot::error::TryRecvError::Empty) => return Ok(json!({"pending": id})),
                result => (result, pending.remove(id).unwrap().names),
            },
            Waiting::Unlock { deadline, .. } => {
                let expired = Instant::now() >= *deadline;
                if is_locked() && !expired {
                    return Ok(json!({"pending": id}));
                }
                let entry = pending.remove(id).unwrap();
                drop(pending);
                let Waiting::Unlock {
                    approved, request, ..
                } = entry.waiting
                else {
                    unreachable!()
                };
                if is_locked() {
                    app.store.lock().unwrap().audit(
                        &s.id,
                        "secret.use",
                        &entry.names.join(","),
                        "locked",
                    )?;
                    return Err(fail(
                        StatusCode::LOCKED,
                        "Secrets stayed locked for 10 minutes. Ask the user to unlock them in Settings → Secrets, then try again",
                    ));
                }
                return proceed(app, s, entry.names, request, approved);
            }
        }
    };
    let record = |outcome: &str| {
        app.store
            .lock()
            .unwrap()
            .audit(&s.id, "secret.use", &names.join(","), outcome)
    };
    match answer {
        Ok(Some(Value::String(option))) if option == "allow-once" || option == "allow-session" => {
            // Assignments may have changed while the human was deciding; they are
            // checked again with the release.
            let approved = if option == "allow-session" {
                Approved::ForSession
            } else {
                Approved::Once
            };
            let request = UseRequest {
                names: names.clone(),
                argv: Vec::new(),
                env_keys: Vec::new(),
                file_keys: Vec::new(),
                cwd: None,
                approval: None,
            };
            proceed(app, s, names, request, approved)
        }
        Ok(Some(_)) => {
            record("denied")?;
            Err(fail(StatusCode::FORBIDDEN, "The user denied this use"))
        }
        _ => {
            record("withdrawn")?;
            Err(fail(
                StatusCode::CONFLICT,
                "The approval was withdrawn before the user answered",
            ))
        }
    }
}
pub async fn get_assignments(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    crate::session(&app, &id)?;
    let vault = peek();
    let vault = vault.as_deref();
    let store = app.store.lock().unwrap();
    let assigned = assignments(&store, &id)?;
    let secrets: Vec<Value> = store
        .secrets()
        .map_err(db_error)?
        .iter()
        .map(|secret| {
            json!({"id":secret.id,"name":secret.name,"description":secret.description,
                "readable":readable(secret, vault)})
        })
        .collect();
    Ok(Json(json!({"assignments": assigned, "secrets": secrets})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentsInput {
    assignments: Vec<Grant>,
}
/// What a human grants; unset fields take the safest default.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    secret_id: String,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default)]
    delegation: Delegation,
    #[serde(default)]
    approval: Approval,
}

/// Replaces a session's assignments. Only humans reach this route; retained secrets
/// keep their origin, new ones are recorded as granted by a human.
pub async fn set_assignments(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<AssignmentsInput>,
) -> ApiResult {
    let s = crate::session(&app, &id)?;
    if input.assignments.len() > MAX_ASSIGNMENTS {
        return Err(bad("A session can have at most 64 secrets"));
    }
    let mut seen = std::collections::HashSet::new();
    if !input
        .assignments
        .iter()
        .all(|a| seen.insert(a.secret_id.as_str()))
    {
        return Err(bad("Each secret can be assigned once"));
    }
    let grants: Vec<SecretGrant> = input
        .assignments
        .iter()
        .map(|a| SecretGrant {
            secret_id: &a.secret_id,
            delivery: a.delivery.as_str(),
            delegation: a.delegation.as_str(),
            approval: a.approval.as_str(),
        })
        .collect();
    let summary = |list: &[SecretAssignment]| {
        list.iter()
            .cloned()
            .map(|row| assignment(row).describe())
            .collect::<Vec<_>>()
    };
    let (before, after) = {
        let mut store = app.store.lock().unwrap();
        let (before, after) = store
            .replace_secret_assignments(&id, &grants, "human", now(), summary)
            .map_err(db_error)?
            .ok_or_else(|| {
                fail(
                    StatusCode::NOT_FOUND,
                    "A selected secret no longer exists. Reopen session settings",
                )
            })?;
        // Before the store lock is released, so no use can pair the new assignments
        // with an earlier "allow for this session".
        if summary(&before) != summary(&after) {
            allowed_uses().remove(&id);
        }
        (before, after)
    };
    let (before, after): (Vec<Assignment>, Vec<Assignment>) = (
        before.into_iter().map(assignment).collect(),
        after.into_iter().map(assignment).collect(),
    );
    let names = |list: &[Assignment]| list.iter().map(Assignment::describe).collect::<Vec<_>>();
    if names(&before) != names(&after) {
        s.emit("secret_assignments", json!({"secrets": names(&after)}));
    }
    // The process environment is fixed at start: restart an idle harness now, a busy one
    // after its turn. A harness that is not running picks the change up when it starts.
    let environment = |list: &[Assignment]| {
        list.iter()
            .filter(|a| a.delivery == Delivery::Environment)
            .map(|a| a.secret_id.clone())
            .collect::<Vec<_>>()
    };
    let restart = environment(&before) != environment(&after);
    if restart {
        let _gate = app.communication_lock.lock().await;
        crate::communication::refresh_harness(&app, &s, false)
            .await
            .map_err(|(code, error)| {
                fail(
                    code,
                    format!("Secrets were saved, but the harness could not restart: {error}"),
                )
            })?;
    }
    Ok(Json(
        json!({"assignments": after, "environmentChanged": restart}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, approval: Approval) -> (Assignment, ()) {
        (
            Assignment {
                secret_id: name.to_lowercase(),
                name: name.into(),
                delivery: Delivery::Tool,
                delegation: Delegation::Restricted,
                approval,
                origin: "human".into(),
            },
            (),
        )
    }
    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn failed_unlocks_back_off_and_idle_locks_respect_never() {
        let waits: Vec<u64> = (1..=9).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert!(
            !idle_expired(0, Duration::from_secs(u64::MAX / 2)),
            "0 never locks"
        );
        assert!(!idle_expired(15, Duration::from_secs(15 * 60 - 1)));
        assert!(idle_expired(15, Duration::from_secs(15 * 60)));
    }

    #[test]
    fn uses_are_judged_against_the_rows_they_release() {
        let rows = [row("A", Approval::None), row("B", Approval::EachUse)];
        let picked = pick(&rows, &names(&["A"])).unwrap();
        assert!(!needs_approval(&picked, None));
        // A revoked assignment is refused even after the human approved it.
        assert_eq!(
            pick(&rows[..1], &names(&["A", "B"])).unwrap_err().0,
            StatusCode::FORBIDDEN
        );
        // A policy changed to eachUse asks, unless allowed for this session.
        let picked = pick(&rows, &names(&["A", "B"])).unwrap();
        assert!(needs_approval(&picked, None));
        assert!(needs_approval(&picked, Some(&HashSet::from(["A".into()]))));
        assert!(!needs_approval(&picked, Some(&HashSet::from(["B".into()]))));
    }
}
