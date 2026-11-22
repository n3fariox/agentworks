//! Session secrets: named values encrypted at rest. A random data key encrypts each
//! value (AES-256-GCM, bound to the row's ID and name); the key file next to the
//! database holds that data key wrapped by `argon2id(password, salt)`, never the key
//! itself. The password is empty until password unlock exists, so a password can be
//! added later by re-wrapping the data key without touching stored values.
use crate::{ApiResult, App, Session, communication as mail, fail};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use base64::{
    Engine,
    engine::general_purpose::STANDARD,
};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS secrets(id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,description TEXT NOT NULL,nonce BLOB NOT NULL,value BLOB NOT NULL,length INTEGER NOT NULL,created INTEGER NOT NULL,updated INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS session_secrets(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,delivery TEXT NOT NULL,delegation TEXT NOT NULL,approval TEXT NOT NULL,origin TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(session_id,secret_id));
  CREATE INDEX IF NOT EXISTS session_secrets_by_secret ON session_secrets(secret_id);";

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTION: usize = 200;
const MAX_ASSIGNMENTS: usize = 64;
const WRAP_AAD: &[u8] = b"agentworks-secrets-key:v1";
/// Variables Agentworks or the harnesses set themselves; a secret must never replace them.
const RESERVED: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDECODE",
    "CLAUDE_CONFIG_DIR",
    "CODEX_API_KEY",
    "CODEX_HOME",
    "CODEX_PATH",
    "DEFAULT_AUTH_REQUEST",
    "DYLD_INSERT_LIBRARIES",
    "HOME",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "NO_BROWSER",
    "OPENAI_API_KEY",
    "OPENAI_BASE_PATH",
    "OPENAI_BASE_URL",
    "OPENAI_CUSTOM_HEADERS",
    "OPENAI_HOST",
    "OPENAI_ORGANIZATION",
    "OPENAI_PROJECT",
    "PATH",
    "PWD",
    "SHELL",
    "TERM",
    "USER",
];
const RESERVED_PREFIXES: &[&str] = &["AGENTWORKS_", "CLAUDE_CODE_", "GOOSE_", "XDG_"];

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Debug)]
struct KdfParams {
    m: u32,
    t: u32,
    p: u32,
}
const DEFAULT_KDF: KdfParams = KdfParams {
    m: Params::DEFAULT_M_COST,
    t: Params::DEFAULT_T_COST,
    p: Params::DEFAULT_P_COST,
};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyFile {
    version: u32,
    kdf: String,
    params: KdfParams,
    salt: String,
    wrapped_key: String,
    password: bool,
}

pub struct Vault {
    key: LessSafeKey,
}
impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault")
    }
}

struct Storage {
    path: PathBuf,
    vault: Result<Vault, String>,
}
static STORAGE: OnceLock<Storage> = OnceLock::new();

/// Opens the key file, creating it on first use. Failure leaves secrets unavailable
/// rather than stopping the server; a damaged key file is never replaced.
pub fn init(path: PathBuf) {
    let vault = open_or_create(&path, DEFAULT_KDF);
    if let Err(error) = &vault {
        eprintln!("agentworks: secrets unavailable: {error}");
    }
    let _ = STORAGE.set(Storage { path, vault });
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

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "the system random source failed".to_owned())?;
    Ok(bytes)
}

fn aead(key: &[u8]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("256-bit key"))
}

/// Seals `plain` as nonce‖ciphertext‖tag.
fn seal(key: &LessSafeKey, aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    let nonce = random::<NONCE_LEN>()?;
    let mut sealed = Zeroizing::new(plain.to_vec());
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad),
        &mut *sealed,
    )
    .map_err(|_| "encryption failed".to_owned())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn open(key: &LessSafeKey, aad: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let (nonce, body) = sealed.split_at_checked(NONCE_LEN)?;
    let mut body = Zeroizing::new(body.to_vec());
    let plain = key
        .open_in_place(
            Nonce::try_assume_unique_for_key(nonce).ok()?,
            Aad::from(aad),
            &mut body,
        )
        .ok()?
        .to_vec();
    Some(Zeroizing::new(plain))
}

fn derive(
    password: &[u8],
    salt: &[u8],
    kdf: KdfParams,
) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    let params = Params::new(kdf.m, kdf.t, kdf.p, Some(KEY_LEN))
        .map_err(|_| "the key file has invalid key-derivation parameters".to_owned())?;
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, &mut *out)
        .map_err(|_| "key derivation failed".to_owned())?;
    Ok(out)
}

/// Wraps `data_key` under a key derived from `password` with a fresh salt.
fn wrap(data_key: &[u8], password: &[u8], kdf: KdfParams) -> Result<KeyFile, String> {
    let salt = random::<SALT_LEN>()?;
    let kek = derive(password, &salt, kdf)?;
    Ok(KeyFile {
        version: 1,
        kdf: "argon2id".into(),
        params: kdf,
        salt: STANDARD.encode(salt),
        wrapped_key: STANDARD.encode(seal(&aead(&*kek), WRAP_AAD, data_key)?),
        password: !password.is_empty(),
    })
}

fn unwrap(file: &KeyFile, password: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    if file.version != 1 || file.kdf != "argon2id" {
        return Err("the key file uses an unsupported format".into());
    }
    let salt = STANDARD
        .decode(&file.salt)
        .map_err(|_| "the key file is damaged".to_owned())?;
    let wrapped = STANDARD
        .decode(&file.wrapped_key)
        .map_err(|_| "the key file is damaged".to_owned())?;
    let kek = derive(password, &salt, file.params)?;
    open(&aead(&*kek), WRAP_AAD, &wrapped)
        .filter(|key| key.len() == KEY_LEN)
        .ok_or_else(|| "the key file could not be unwrapped".to_owned())
}

fn open_or_create(path: &FsPath, kdf: KdfParams) -> Result<Vault, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let file: KeyFile = serde_json::from_slice(&bytes)
                .map_err(|_| format!("{} is damaged", path.display()))?;
            if file.password {
                return Err(
                    "the key file requires a password, which this version cannot unlock".into(),
                );
            }
            let key = unwrap(&file, b"")?;
            Ok(Vault { key: aead(&key) })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = Zeroizing::new(random::<KEY_LEN>()?);
            write_key_file(path, &wrap(&*key, b"", kdf)?)
                .map_err(|e| format!("could not create {}: {e}", path.display()))?;
            Ok(Vault { key: aead(&*key) })
        }
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// Creates the key file without replacing an existing one (mode 0600, synced).
fn write_key_file(path: &FsPath, file: &KeyFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(path)?;
    out.write_all(&serde_json::to_vec_pretty(file)?)?;
    out.sync_all()
}

/// Values are bound to their row, so ciphertexts cannot be moved between secrets.
fn value_aad(id: &str, name: &str) -> Vec<u8> {
    format!("agentworks-secret:v1\0{id}\0{name}").into_bytes()
}

fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = name.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(
            "Use an environment variable name: A–Z, 0–9 and _, not starting with a digit".into(),
        );
    }
    if RESERVED.contains(&name) || RESERVED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(format!(
            "{name} is set by Agentworks or a harness and cannot be a secret"
        ));
    }
    Ok(())
}
fn validate_value(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("Enter a value".into());
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err("Values are limited to 64 KiB".into());
    }
    if value.contains('\0') {
        return Err("Values cannot contain NUL characters".into());
    }
    Ok(())
}
fn validate_description(description: &str) -> Result<String, String> {
    let description = description.trim();
    if description.chars().count() > MAX_DESCRIPTION {
        return Err("Descriptions are limited to 200 characters".into());
    }
    Ok(description.to_owned())
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
fn audit(db: &rusqlite::Connection, action: &str, name: &str) -> rusqlite::Result<usize> {
    db.execute(
        "INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human',?1,?2,'ok',?3)",
        params![action, format!("secret:{name}"), now()],
    )
}

struct Row {
    id: String,
    name: String,
    description: String,
    sealed: Vec<u8>,
    length: i64,
    created: i64,
    updated: i64,
}
const COLUMNS: &str = "id,name,description,nonce,value,length,created,updated";
fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let mut sealed: Vec<u8> = r.get(3)?;
    sealed.extend_from_slice(&r.get::<_, Vec<u8>>(4)?);
    Ok(Row {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        sealed,
        length: r.get(5)?,
        created: r.get(6)?,
        updated: r.get(7)?,
    })
}
/// Public view of a secret: never the value. `readable` is false when the value was
/// stored under a different key file and must be entered again.
fn view(row: &Row, vault: Option<&Vault>) -> Value {
    let readable =
        vault.map(|v| open(&v.key, &value_aad(&row.id, &row.name), &row.sealed).is_some());
    json!({"id":row.id,"name":row.name,"description":row.description,"length":row.length,
        "created":row.created,"updated":row.updated,"readable":readable})
}
fn find(db: &rusqlite::Connection, id: &str) -> Result<Row, (StatusCode, String)> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM secrets WHERE id=?1"),
        [id],
        row,
    )
    .optional()
    .map_err(db_error)?
    .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Secret not found"))
}
/// Stores nonce and ciphertext‖tag in their own columns.
fn sealed_value(
    vault: &Vault,
    id: &str,
    name: &str,
    value: &str,
) -> Result<(Vec<u8>, Vec<u8>), (StatusCode, String)> {
    let mut sealed =
        seal(&vault.key, &value_aad(id, name), value.as_bytes()).map_err(|e| unavailable(&e))?;
    let body = sealed.split_off(NONCE_LEN);
    Ok((sealed, body))
}

pub async fn list(State(app): State<App>) -> ApiResult {
    let storage = STORAGE.get();
    let vault = storage.and_then(|s| s.vault.as_ref().ok());
    let store = app.store.lock().unwrap();
    let mut query = store
        .connection
        .prepare(&format!("SELECT {COLUMNS} FROM secrets ORDER BY name"))
        .map_err(db_error)?;
    let secrets: Vec<Value> = query
        .query_map([], row)
        .map_err(db_error)?
        .map(|r| r.map(|r| view(&r, vault)))
        .collect::<rusqlite::Result<_>>()
        .map_err(db_error)?;
    let mut counts = store
        .connection
        .prepare("SELECT secret_id,count(*) FROM session_secrets GROUP BY secret_id")
        .map_err(db_error)?;
    let counts: std::collections::HashMap<String, i64> = counts
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(db_error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(db_error)?;
    let secrets: Vec<Value> = secrets
        .into_iter()
        .map(|mut s| {
            s["sessions"] = json!(counts.get(s["id"].as_str().unwrap()).unwrap_or(&0));
            s
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
    let (nonce, body) = sealed_value(vault, &id, name, &input.value)?;
    let created = now();
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction().map_err(db_error)?;
    let exists: bool = tx
        .query_row("SELECT 1 FROM secrets WHERE name=?1", [name], |_| Ok(true))
        .optional()
        .map_err(db_error)?
        .is_some();
    if exists {
        return Err(fail(
            StatusCode::CONFLICT,
            format!("A secret named {name} already exists"),
        ));
    }
    tx.execute(
        "INSERT INTO secrets(id,name,description,nonce,value,length,created,updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
        params![id, name, description, nonce, body, input.value.len() as i64, created],
    )
    .map_err(db_error)?;
    audit(&tx, "secret.create", name).map_err(db_error)?;
    let saved = find(&tx, &id)?;
    tx.commit().map_err(db_error)?;
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
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction().map_err(db_error)?;
    let current = find(&tx, &id)?;
    let updated = now();
    if let Some(description) = description {
        tx.execute(
            "UPDATE secrets SET description=?2,updated=?3 WHERE id=?1",
            params![id, description, updated],
        )
        .map_err(db_error)?;
        audit(&tx, "secret.update", &current.name).map_err(db_error)?;
    }
    if let (Some(vault), Some(value)) = (vault, &input.value) {
        let (nonce, body) = sealed_value(vault, &id, &current.name, value)?;
        tx.execute(
            "UPDATE secrets SET nonce=?2,value=?3,length=?4,updated=?5 WHERE id=?1",
            params![id, nonce, body, value.len() as i64, updated],
        )
        .map_err(db_error)?;
        audit(&tx, "secret.value", &current.name).map_err(db_error)?;
    }
    let saved = find(&tx, &id)?;
    tx.commit().map_err(db_error)?;
    let vault = STORAGE.get().and_then(|s| s.vault.as_ref().ok());
    Ok(Json(view(&saved, vault)))
}

pub async fn delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction().map_err(db_error)?;
    let current = find(&tx, &id)?;
    tx.execute("DELETE FROM secrets WHERE id=?1", [&id])
        .map_err(db_error)?;
    audit(&tx, "secret.delete", &current.name).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(Json(json!({"deleted": id})))
}

/// How an assigned secret reaches the session.
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Delivery {
    /// Harness environment variable, also usable by the tool.
    Environment,
    #[default]
    Tool,
}
/// Whether the session may pass the secret to sessions it creates.
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Delegation {
    #[default]
    Restricted,
    Passable,
}
/// Whether each tool use asks the human first.
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Approval {
    #[default]
    None,
    EachUse,
}
fn text<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}
fn parse<T: serde::de::DeserializeOwned>(text: &str) -> rusqlite::Result<T> {
    serde_json::from_value(json!(text))
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

/// One secret granted to one session. `origin` is `human` or the delegating session's ID.
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub secret_id: String,
    pub name: String,
    pub delivery: Delivery,
    pub delegation: Delegation,
    pub approval: Approval,
    pub origin: String,
}
pub fn assignments(db: &rusqlite::Connection, session: &str) -> rusqlite::Result<Vec<Assignment>> {
    let mut query = db.prepare("SELECT a.secret_id,s.name,a.delivery,a.delegation,a.approval,a.origin FROM session_secrets a JOIN secrets s ON s.id=a.secret_id WHERE a.session_id=?1 ORDER BY s.name")?;
    query
        .query_map([session], |r| {
            Ok(Assignment {
                secret_id: r.get(0)?,
                name: r.get(1)?,
                delivery: parse(&r.get::<_, String>(2)?)?,
                delegation: parse(&r.get::<_, String>(3)?)?,
                approval: parse(&r.get::<_, String>(4)?)?,
                origin: r.get(5)?,
            })
        })?
        .collect()
}
/// A session's `environment` secrets, decrypted for one harness start.
#[derive(Default)]
pub struct Environment {
    pub vars: Vec<(String, Zeroizing<String>)>,
    /// Names that could not be decrypted (storage unavailable or value needs re-entry).
    pub unavailable: Vec<String>,
}
pub fn environment(db: &rusqlite::Connection, session: &str) -> rusqlite::Result<Environment> {
    let mut out = Environment::default();
    let vault = STORAGE.get().and_then(|s| s.vault.as_ref().ok());
    for a in assignments(db, session)? {
        if a.delivery != Delivery::Environment {
            continue;
        }
        let plain = match vault {
            Some(vault) => {
                let row = db.query_row(
                    &format!("SELECT {COLUMNS} FROM secrets WHERE id=?1"),
                    [&a.secret_id],
                    row,
                )?;
                open(&vault.key, &value_aad(&row.id, &row.name), &row.sealed)
                    .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
            }
            None => None,
        };
        match plain {
            Some(value) => out.vars.push((a.name, Zeroizing::new(value))),
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

fn usage(a: &Assignment, in_environment: bool) -> String {
    let tool = format!(
        "Use run_with_secrets with {{{{secret:{}}}}} in argv, env, stdin or files.",
        a.name
    );
    let mut usage = match a.delivery {
        Delivery::Environment if in_environment => format!(
            "Available as ${} in commands you run. run_with_secrets also works.",
            a.name
        ),
        Delivery::Environment => format!(
            "Assigned as ${} but not in your environment until the harness restarts. {tool}",
            a.name
        ),
        Delivery::Tool => tool,
    };
    if a.approval == Approval::EachUse {
        usage.push_str(" The user is asked before each run_with_secrets call.");
    }
    usage
}
/// `list_session_secrets`: assigned secrets with instructions. Never values.
pub fn agent_list(app: &App, session: &str) -> mail::Result<Value> {
    let store = app.store.lock().unwrap();
    let assigned = assignments(&store.connection, session).map_err(db_error)?;
    let environment = delivered().get(session).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for a in &assigned {
        let description: String = store
            .connection
            .query_row(
                "SELECT description FROM secrets WHERE id=?1",
                [&a.secret_id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        out.push(
            json!({"name":a.name,"description":description,"delivery":a.delivery,
            "approval":a.approval,"usage":usage(a, environment.contains(&a.name))}),
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
    let assigned = assignments(&app.store.lock().unwrap().connection, &s.id).map_err(db_error)?;
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
            let row = store
                .connection
                .query_row(
                    &format!("SELECT {COLUMNS} FROM secrets WHERE name=?1"),
                    [name],
                    row,
                )
                .map_err(db_error)?;
            let plain = open(&vault.key, &value_aad(&row.id, &row.name), &row.sealed)
                .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                .ok_or_else(|| {
                    fail(
                        StatusCode::CONFLICT,
                        format!("{name} must be entered again in Settings → Secrets"),
                    )
                })?;
            values.push((name.clone(), Zeroizing::new(plain)));
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

fn describe(a: &Assignment) -> String {
    let mut parts = vec![if a.delivery == Delivery::Environment {
        "environment"
    } else {
        "tool"
    }];
    if a.delegation == Delegation::Passable {
        parts.push("passable");
    }
    if a.approval == Approval::EachUse {
        parts.push("ask each use");
    }
    format!("{} ({})", a.name, parts.join(", "))
}

pub async fn get_assignments(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    crate::session(&app, &id)?;
    let vault = STORAGE.get().and_then(|s| s.vault.as_ref().ok());
    let store = app.store.lock().unwrap();
    let assigned = assignments(&store.connection, &id).map_err(db_error)?;
    let mut query = store
        .connection
        .prepare(&format!("SELECT {COLUMNS} FROM secrets ORDER BY name"))
        .map_err(db_error)?;
    let secrets: Vec<Value> = query
        .query_map([], row)
        .map_err(db_error)?
        .map(|r| {
            r.map(|r| {
                let v = view(&r, vault);
                json!({"id":v["id"],"name":v["name"],"description":v["description"],"readable":v["readable"]})
            })
        })
        .collect::<rusqlite::Result<_>>()
        .map_err(db_error)?;
    Ok(Json(json!({"assignments": assigned, "secrets": secrets})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentInput {
    secret_id: String,
    #[serde(default)]
    delivery: Delivery,
    #[serde(default)]
    delegation: Delegation,
    #[serde(default)]
    approval: Approval,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentsInput {
    assignments: Vec<AssignmentInput>,
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
    let (before, after) = {
        let mut store = app.store.lock().unwrap();
        let tx = store.connection.transaction().map_err(db_error)?;
        let before = assignments(&tx, &id).map_err(db_error)?;
        let created = now();
        tx.execute("DELETE FROM session_secrets WHERE session_id=?1", [&id])
            .map_err(db_error)?;
        for a in &input.assignments {
            find(&tx, &a.secret_id).map_err(|(code, _)| {
                fail(
                    code,
                    "A selected secret no longer exists. Reopen session settings",
                )
            })?;
            let origin = before
                .iter()
                .find(|b| b.secret_id == a.secret_id)
                .map_or("human", |b| b.origin.as_str());
            tx.execute(
                "INSERT INTO session_secrets(session_id,secret_id,delivery,delegation,approval,origin,created) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![id, a.secret_id, text(a.delivery), text(a.delegation), text(a.approval), origin, created],
            )
            .map_err(db_error)?;
        }
        let after = assignments(&tx, &id).map_err(db_error)?;
        let summary = |list: &[Assignment]| list.iter().map(describe).collect::<Vec<_>>();
        if summary(&before) != summary(&after) {
            tx.execute(
                "INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','secrets',?1,?2,?3)",
                params![id, summary(&after).join(", "), created],
            )
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        (before, after)
    };
    let names = |list: &[Assignment]| list.iter().map(describe).collect::<Vec<_>>();
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

#[cfg(test)]
mod tests {
    use super::*;

    // Minimum Argon2 cost keeps debug-build tests fast; production uses DEFAULT_KDF.
    const TEST_KDF: KdfParams = KdfParams { m: 8, t: 1, p: 1 };

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("agentworks-secrets-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn key_file_holds_only_a_wrapped_key_and_reopens_to_the_same_key() {
        let dir = temp();
        let path = dir.join("sessions.secrets.key");
        let first = open_or_create(&path, TEST_KDF).unwrap();
        let sealed = seal(&first.key, b"aad", b"value").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file["kdf"], "argon2id");
        assert_eq!(file["password"], false);
        let wrapped = STANDARD
            .decode(file["wrappedKey"].as_str().unwrap())
            .unwrap();
        assert_eq!(wrapped.len(), NONCE_LEN + KEY_LEN + AES_256_GCM.tag_len());
        let second = open_or_create(&path, TEST_KDF).unwrap();
        assert_eq!(&*open(&second.key, b"aad", &sealed).unwrap(), b"value");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn values_are_bound_to_their_row() {
        let dir = temp();
        let vault = open_or_create(&dir.join("k"), TEST_KDF).unwrap();
        let (nonce, body) = sealed_value(&vault, "id-1", "TOKEN_A", "secret").unwrap();
        let sealed = [nonce, body].concat();
        assert!(open(&vault.key, &value_aad("id-1", "TOKEN_A"), &sealed).is_some());
        assert!(open(&vault.key, &value_aad("id-2", "TOKEN_A"), &sealed).is_none());
        assert!(open(&vault.key, &value_aad("id-1", "TOKEN_B"), &sealed).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_replaced_key_file_cannot_read_older_values() {
        let dir = temp();
        let path = dir.join("k");
        let old = open_or_create(&path, TEST_KDF).unwrap();
        let sealed = seal(&old.key, b"aad", b"value").unwrap();
        std::fs::remove_file(&path).unwrap();
        let new = open_or_create(&path, TEST_KDF).unwrap();
        assert!(open(&new.key, b"aad", &sealed).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rewrapping_with_a_password_keeps_the_data_key() {
        let key = random::<KEY_LEN>().unwrap();
        let file = wrap(&key, b"", TEST_KDF).unwrap();
        let data_key = unwrap(&file, b"").unwrap();
        let protected = wrap(&data_key, b"correct horse", TEST_KDF).unwrap();
        assert!(protected.password);
        assert_ne!(protected.salt, file.salt);
        assert_eq!(&*unwrap(&protected, b"correct horse").unwrap(), &key);
        assert!(unwrap(&protected, b"wrong").is_err());
        assert!(unwrap(&protected, b"").is_err());
    }

    #[test]
    fn damaged_or_password_key_files_are_reported_and_never_replaced() {
        let dir = temp();
        std::fs::create_dir_all(&dir).unwrap();
        let damaged = dir.join("damaged");
        std::fs::write(&damaged, b"not json").unwrap();
        assert!(
            open_or_create(&damaged, TEST_KDF)
                .unwrap_err()
                .contains("damaged")
        );
        assert_eq!(std::fs::read(&damaged).unwrap(), b"not json");
        let locked = dir.join("locked");
        let file = wrap(&random::<KEY_LEN>().unwrap(), b"pw", TEST_KDF).unwrap();
        write_key_file(&locked, &file).unwrap();
        assert!(
            open_or_create(&locked, TEST_KDF)
                .unwrap_err()
                .contains("password")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_follow_environment_rules_and_avoid_managed_variables() {
        for name in ["GITHUB_TOKEN", "_X", "NPM_TOKEN_2"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        for name in [
            "",
            "github_token",
            "2FA",
            "A-B",
            "PATH",
            "OPENAI_API_KEY",
            "GOOSE_MODEL",
            "AGENTWORKS_PORT",
            "CLAUDE_CODE_X",
            "XDG_CONFIG_HOME",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        assert!(validate_value("").is_err());
        assert!(validate_value("a\0b").is_err());
        assert!(validate_value(&"x".repeat(MAX_VALUE_BYTES + 1)).is_err());
        assert!(validate_value("ok").is_ok());
    }
}
