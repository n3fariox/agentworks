//! Session secrets: named values encrypted at rest. A random data key encrypts each
//! value (AES-256-GCM, bound to the row's ID and name); the key file next to the
//! database holds that data key wrapped by `argon2id(password, salt)`, never the key
//! itself. The password is empty until password unlock exists, so a password can be
//! added later by re-wrapping the data key without touching stored values.
use crate::{ApiResult, App, fail};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path as FsPath, PathBuf},
    sync::OnceLock,
};
use zeroize::Zeroizing;

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS secrets(id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,description TEXT NOT NULL,nonce BLOB NOT NULL,value BLOB NOT NULL,length INTEGER NOT NULL,created INTEGER NOT NULL,updated INTEGER NOT NULL);";

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTION: usize = 200;
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
