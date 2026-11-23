//! Values encrypted at rest. A random data key encrypts each value (AES-256-GCM, bound
//! to the row's ID and name); the key file holds that data key wrapped by
//! `argon2id(password, salt)`, never the key itself. Without a password the password is
//! empty and the key file opens on its own. Setting, changing or removing a password
//! re-wraps the same data key, so stored values are never re-encrypted.
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};
use zeroize::Zeroizing;

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const WRAP_AAD: &[u8] = b"agentworks-secrets-key:v1";

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
/// A human-chosen password gets a costlier derivation (64 MiB, 3 passes) than the empty
/// one; it runs once per unlock.
const PASSWORD_KDF: KdfParams = KdfParams {
    m: 64 * 1024,
    t: 3,
    p: 1,
};
const MIN_PASSWORD_CHARS: usize = 8;
const MAX_PASSWORD_BYTES: usize = 1024;

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

/// The unwrapped data key. Dropping it zeroizes the key bytes this crate holds.
pub struct Vault {
    key: LessSafeKey,
    data_key: Zeroizing<Vec<u8>>,
}
/// A password-protected key file that has not been unlocked.
pub struct Locked {
    file: KeyFile,
}
pub enum Opened {
    Unlocked(Box<Vault>),
    Locked(Locked),
}
#[derive(Debug, PartialEq)]
pub enum PasswordError {
    /// The password does not unwrap the key file.
    Wrong,
    /// The new password is not acceptable.
    Invalid(String),
    /// The key file could not be read, written or understood.
    Storage(String),
}
impl std::fmt::Display for PasswordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wrong => f.write_str("The password is not correct"),
            Self::Invalid(message) | Self::Storage(message) => f.write_str(message),
        }
    }
}
/// A new password: at least 8 characters; empty means no password.
pub fn validate_password(password: &str) -> Result<(), String> {
    if !password.is_empty() && password.chars().count() < MIN_PASSWORD_CHARS {
        return Err("Use at least 8 characters".into());
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err("Passwords are limited to 1024 bytes".into());
    }
    Ok(())
}
/// Opens the key file, creating one without a password on first use. A damaged key
/// file is reported, never replaced.
pub fn open(path: &Path) -> Result<Opened, String> {
    open_with(path, DEFAULT_KDF)
}
impl Locked {
    pub fn unlock(&self, password: &str) -> Result<Vault, PasswordError> {
        unwrap(&self.file, password.as_bytes()).map(Vault::new)
    }
}
impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault")
    }
}
impl Vault {
    fn new(data_key: Zeroizing<Vec<u8>>) -> Self {
        Self {
            key: aead(&data_key),
            data_key,
        }
    }
    /// Re-wraps this vault's data key under `new` (empty removes the password) and
    /// replaces the key file atomically. `current` must unwrap the existing file when it
    /// has a password, and the file must hold this vault's key.
    pub fn set_password(&self, path: &Path, current: &str, new: &str) -> Result<(), PasswordError> {
        self.set_password_with(path, current, new, PASSWORD_KDF)
    }
    fn set_password_with(
        &self,
        path: &Path,
        current: &str,
        new: &str,
        password_kdf: KdfParams,
    ) -> Result<(), PasswordError> {
        validate_password(new).map_err(PasswordError::Invalid)?;
        let file = read_key_file(path).map_err(PasswordError::Storage)?;
        let current = if file.password { current } else { "" };
        if unwrap(&file, current.as_bytes())? != self.data_key {
            return Err(PasswordError::Storage(
                "the key file no longer holds this server's key; restart Agentworks".into(),
            ));
        }
        let kdf = if new.is_empty() {
            DEFAULT_KDF
        } else {
            password_kdf
        };
        let wrapped = wrap(&self.data_key, new.as_bytes(), kdf).map_err(PasswordError::Storage)?;
        replace_key_file(path, &wrapped)
            .map_err(|e| PasswordError::Storage(format!("could not save {}: {e}", path.display())))
    }
    /// Encrypts a value for the secret row `id`/`name`, as (nonce, ciphertext‖tag).
    pub fn seal_value(
        &self,
        id: &str,
        name: &str,
        value: &str,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut sealed = aead_seal(&self.key, &value_aad(id, name), value.as_bytes())?;
        let body = sealed.split_off(NONCE_LEN);
        Ok((sealed, body))
    }
    /// Decrypts a value sealed for the same row; `None` when it was sealed under another
    /// key file or for another row.
    pub fn open_value(&self, id: &str, name: &str, sealed: &[u8]) -> Option<Zeroizing<String>> {
        let bytes = aead_open(&self.key, &value_aad(id, name), sealed)?;
        String::from_utf8(bytes.to_vec()).ok().map(Zeroizing::new)
    }
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
fn aead_seal(key: &LessSafeKey, aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
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

fn aead_open(key: &LessSafeKey, aad: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
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
        wrapped_key: STANDARD.encode(aead_seal(&aead(&*kek), WRAP_AAD, data_key)?),
        password: !password.is_empty(),
    })
}

fn unwrap(file: &KeyFile, password: &[u8]) -> Result<Zeroizing<Vec<u8>>, PasswordError> {
    let damaged = || PasswordError::Storage("the key file is damaged".into());
    if file.version != 1 || file.kdf != "argon2id" {
        return Err(PasswordError::Storage(
            "the key file uses an unsupported format".into(),
        ));
    }
    let salt = STANDARD.decode(&file.salt).map_err(|_| damaged())?;
    let wrapped = STANDARD.decode(&file.wrapped_key).map_err(|_| damaged())?;
    let kek = derive(password, &salt, file.params).map_err(PasswordError::Storage)?;
    // A wrong password and a tampered file both fail the tag check; without a
    // password, failure can only mean damage.
    match aead_open(&aead(&*kek), WRAP_AAD, &wrapped) {
        Some(key) if key.len() == KEY_LEN => Ok(key),
        Some(_) => Err(damaged()),
        None if file.password => Err(PasswordError::Wrong),
        None => Err(PasswordError::Storage(
            "the key file could not be unwrapped".into(),
        )),
    }
}

fn read_key_file(path: &Path) -> Result<KeyFile, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|_| format!("{} is damaged", path.display()))
}

fn open_with(path: &Path, kdf: KdfParams) -> Result<Opened, String> {
    match std::fs::metadata(path) {
        Ok(_) => {
            let file = read_key_file(path)?;
            if file.password {
                return Ok(Opened::Locked(Locked { file }));
            }
            let key = unwrap(&file, b"").map_err(|e| e.to_string())?;
            Ok(Opened::Unlocked(Box::new(Vault::new(key))))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = Zeroizing::new(random::<KEY_LEN>()?.to_vec());
            write_key_file(path, &wrap(&key, b"", kdf)?)
                .map_err(|e| format!("could not create {}: {e}", path.display()))?;
            Ok(Opened::Unlocked(Box::new(Vault::new(key))))
        }
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// Replaces the key file: a private temporary file beside it is written, synced and
/// renamed over it, so a crash leaves either the old file or the new one.
fn replace_key_file(path: &Path, file: &KeyFile) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = write_key_file(&temporary, file).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    #[cfg(unix)]
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Creates the key file without replacing an existing one (mode 0600, synced).
fn write_key_file(path: &Path, file: &KeyFile) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::PathBuf;

    // Minimum Argon2 cost keeps debug-build tests fast; production uses DEFAULT_KDF.
    const TEST_KDF: KdfParams = KdfParams { m: 8, t: 1, p: 1 };

    fn open_or_create(path: &Path, kdf: KdfParams) -> Result<Vault, String> {
        match open_with(path, kdf)? {
            Opened::Unlocked(vault) => Ok(*vault),
            Opened::Locked(_) => Err("locked".into()),
        }
    }

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("agentworks-secrets-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn key_file_holds_only_a_wrapped_key_and_reopens_to_the_same_key() {
        let dir = temp();
        let path = dir.join("sessions.secrets.key");
        let first = open_or_create(&path, TEST_KDF).unwrap();
        let sealed = aead_seal(&first.key, b"aad", b"value").unwrap();
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
        assert_eq!(&*aead_open(&second.key, b"aad", &sealed).unwrap(), b"value");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn values_are_bound_to_their_row() {
        let dir = temp();
        let vault = open_or_create(&dir.join("k"), TEST_KDF).unwrap();
        let (nonce, body) = vault.seal_value("id-1", "TOKEN_A", "secret").unwrap();
        let sealed = [nonce, body].concat();
        assert_eq!(
            vault
                .open_value("id-1", "TOKEN_A", &sealed)
                .unwrap()
                .as_str(),
            "secret"
        );
        assert!(vault.open_value("id-2", "TOKEN_A", &sealed).is_none());
        assert!(vault.open_value("id-1", "TOKEN_B", &sealed).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_replaced_key_file_cannot_read_older_values() {
        let dir = temp();
        let path = dir.join("k");
        let old = open_or_create(&path, TEST_KDF).unwrap();
        let sealed = aead_seal(&old.key, b"aad", b"value").unwrap();
        std::fs::remove_file(&path).unwrap();
        let new = open_or_create(&path, TEST_KDF).unwrap();
        assert!(aead_open(&new.key, b"aad", &sealed).is_none());
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
        assert_eq!(
            unwrap(&protected, b"wrong").unwrap_err(),
            PasswordError::Wrong
        );
        assert_eq!(unwrap(&protected, b"").unwrap_err(), PasswordError::Wrong);
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
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_password_locks_the_key_file_and_unlocks_to_the_same_key() {
        let dir = temp();
        let path = dir.join("k");
        let vault = open_or_create(&path, TEST_KDF).unwrap();
        let (nonce, body) = vault.seal_value("id", "TOKEN", "value").unwrap();
        let sealed = [nonce, body].concat();
        assert_eq!(
            vault.set_password_with(&path, "", "short", TEST_KDF),
            Err(PasswordError::Invalid("Use at least 8 characters".into()))
        );
        vault
            .set_password_with(&path, "", "correct horse", TEST_KDF)
            .unwrap();
        let Opened::Locked(locked) = open_with(&path, TEST_KDF).unwrap() else {
            panic!("a password-protected key file opens locked");
        };
        assert_eq!(
            locked.unlock("wrong one").unwrap_err(),
            PasswordError::Wrong
        );
        let unlocked = locked.unlock("correct horse").unwrap();
        assert_eq!(
            unlocked
                .open_value("id", "TOKEN", &sealed)
                .unwrap()
                .as_str(),
            "value"
        );
        // Changing needs the current password; removing it opens without one.
        assert_eq!(
            unlocked.set_password_with(&path, "wrong one", "battery staple", TEST_KDF),
            Err(PasswordError::Wrong)
        );
        unlocked
            .set_password_with(&path, "correct horse", "battery staple", TEST_KDF)
            .unwrap();
        let Opened::Locked(locked) = open_with(&path, TEST_KDF).unwrap() else {
            panic!("still locked");
        };
        assert!(locked.unlock("correct horse").is_err());
        let unlocked = locked.unlock("battery staple").unwrap();
        unlocked
            .set_password_with(&path, "battery staple", "", TEST_KDF)
            .unwrap();
        let reopened = open_or_create(&path, TEST_KDF).unwrap();
        assert!(reopened.open_value("id", "TOKEN", &sealed).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the replaced file stays private");
        }
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["k"], "no temporary files remain");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_vault_never_overwrites_a_key_file_for_another_key() {
        let dir = temp();
        let (first, second) = (dir.join("a"), dir.join("b"));
        let vault = open_or_create(&first, TEST_KDF).unwrap();
        open_or_create(&second, TEST_KDF).unwrap();
        let before = std::fs::read(&second).unwrap();
        assert!(matches!(
            vault.set_password_with(&second, "", "correct horse", TEST_KDF),
            Err(PasswordError::Storage(_))
        ));
        assert_eq!(std::fs::read(&second).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
