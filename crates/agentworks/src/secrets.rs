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
    vault::Vault,
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
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const MAX_ASSIGNMENTS: usize = 64;

struct Storage {
    path: PathBuf,
    vault: Result<Vault, String>,
}
static STORAGE: OnceLock<Storage> = OnceLock::new();

/// Opens the key file, creating it on first use. Failure leaves secrets unavailable
/// rather than stopping the server; a damaged key file is never replaced.
pub fn init(path: PathBuf) {
    let vault = Vault::open_or_create(&path);
    if let Err(error) = &vault {
        eprintln!("agentworks: secrets unavailable: {error}");
    }
    let _ = STORAGE.set(Storage { path, vault });
}

fn loaded() -> Option<&'static Vault> {
    STORAGE.get().and_then(|s| s.vault.as_ref().ok())
}
fn vault() -> Result<&'static Vault, (StatusCode, String)> {
    match STORAGE.get().map(|s| &s.vault) {
        Some(Ok(vault)) => Ok(vault),
        Some(Err(error)) => Err(unavailable(error)),
        None => Err(unavailable("not initialized")),
    }
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

pub async fn list(State(app): State<App>) -> ApiResult {
    let storage = STORAGE.get();
    let vault = loaded();
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
    Ok(Json(json!({
        "available": vault.is_some(),
        "error": storage.and_then(|s| s.vault.as_ref().err()),
        "keyFile": storage.map(|s| s.path.display().to_string()),
        "secrets": secrets,
    })))
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
    Ok(Json(view(&saved, Some(vault))))
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
        let (vault, value) = (vault.unwrap(), input.value.as_ref().unwrap());
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
            vault.map(|_| &seal as &dyn Fn(&SealedSecret) -> Result<_, Failure>),
            now(),
        )
        .map_err(|failure| match failure {
            Failure::Database => db_error(""),
            Failure::Seal(error) => unavailable(&error),
        })?
        .ok_or_else(not_found)?;
    Ok(Json(view(&saved, loaded())))
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
}
pub fn environment(store: &agentworks_database::Store, session: &str) -> mail::Result<Environment> {
    let mut out = Environment::default();
    for a in assignments(store, session)? {
        if a.delivery != Delivery::Environment {
            continue;
        }
        let plain = match loaded() {
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
    answer: oneshot::Receiver<Option<Value>>,
}
static PENDING_USES: OnceLock<Mutex<HashMap<String, PendingUse>>> = OnceLock::new();
fn pending_uses() -> std::sync::MutexGuard<'static, HashMap<String, PendingUse>> {
    PENDING_USES.get_or_init(Default::default).lock().unwrap()
}

#[derive(Deserialize)]
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
    let assigned = checked(app, s, &names)?;
    let asked = {
        let allowed = allowed_uses();
        let allowed = allowed.get(&s.id);
        assigned.iter().any(|a| {
            a.approval == Approval::EachUse && !allowed.is_some_and(|set| set.contains(&a.name))
        })
    };
    if asked {
        return ask(s, names, &request);
    }
    release(app, s, &names)
}
/// Every name must be assigned to this session; unassigned and missing look the same.
fn checked(app: &App, s: &Session, names: &[String]) -> mail::Result<Vec<Assignment>> {
    let assigned = assignments(&app.store.lock().unwrap(), &s.id)?;
    names
        .iter()
        .map(|name| {
            assigned
                .iter()
                .find(|a| &a.name == name)
                .cloned()
                .ok_or_else(|| {
                    fail(
                        StatusCode::FORBIDDEN,
                        format!("{name} is not assigned to this session"),
                    )
                })
        })
        .collect()
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
                answer: receiver,
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
fn poll_use(app: &App, s: &Session, id: &str) -> mail::Result<Value> {
    let (answer, names) = {
        let mut pending = pending_uses();
        let Some(entry) = pending.get_mut(id).filter(|p| p.session == s.id) else {
            return Err(fail(
                StatusCode::NOT_FOUND,
                "This approval is no longer pending",
            ));
        };
        match entry.answer.try_recv() {
            Err(oneshot::error::TryRecvError::Empty) => return Ok(json!({"pending": id})),
            result => (result, pending.remove(id).unwrap().names),
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
            // Assignments may have changed while the human was deciding.
            checked(app, s, &names)?;
            if option == "allow-session" {
                allowed_uses()
                    .entry(s.id.clone())
                    .or_default()
                    .extend(names.iter().cloned());
            }
            let mut released = release(app, s, &names)?;
            released["approval"] = json!(if option == "allow-session" {
                "The user approved this use, and further uses of these secrets in this session."
            } else {
                "The user approved this use."
            });
            Ok(released)
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
fn release(app: &App, s: &Session, names: &[String]) -> mail::Result<Value> {
    let vault = vault()?;
    let mut values = Vec::new();
    {
        let store = app.store.lock().unwrap();
        for name in names {
            let value = store
                .secret_named(name)
                .map_err(db_error)?
                .and_then(|secret| value(&secret, vault))
                .ok_or_else(|| {
                    fail(
                        StatusCode::CONFLICT,
                        format!("{name} must be entered again in Settings → Secrets"),
                    )
                })?;
            values.push((name.clone(), value));
        }
    }
    register(&s.id, &values);
    app.store
        .lock()
        .unwrap()
        .audit(&s.id, "secret.use", &names.join(","), "released")?;
    let map: serde_json::Map<String, Value> = values
        .iter()
        .map(|(name, value)| (name.clone(), json!(value.as_str())))
        .collect();
    Ok(json!({"values":map,"cwd":s.cwd()}))
}

pub async fn get_assignments(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    crate::session(&app, &id)?;
    let vault = loaded();
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
    let (before, after) = app
        .store
        .lock()
        .unwrap()
        .replace_secret_assignments(&id, &grants, "human", now(), summary)
        .map_err(db_error)?
        .ok_or_else(|| {
            fail(
                StatusCode::NOT_FOUND,
                "A selected secret no longer exists. Reopen session settings",
            )
        })?;
    let (before, after): (Vec<Assignment>, Vec<Assignment>) = (
        before.into_iter().map(assignment).collect(),
        after.into_iter().map(assignment).collect(),
    );
    let names = |list: &[Assignment]| list.iter().map(Assignment::describe).collect::<Vec<_>>();
    if names(&before) != names(&after) {
        allowed_uses().remove(&id);
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
