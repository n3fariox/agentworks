//! Values encrypted at rest. A random data key encrypts each value (AES-256-GCM, bound
//! to the row's ID and name). The key file never holds that key: it holds one or more
//! *slots*, each the same data key wrapped under a different unlock secret.
//!
//! - One password slot: `argon2id(password, salt)`. Without a password the password is
//!   empty and the key file opens on its own.
//! - Any number of passkey slots: HKDF-SHA256 of the 32-byte secret a passkey returns
//!   through the WebAuthn PRF extension for that slot's salt. Passkeys need a password
//!   as the fallback.
//!
//! Adding or removing a way to unlock re-wraps the same data key, so stored values are
//! never re-encrypted. Version 1 key files (a single password slot) are read as-is and
//! written as version 2 when they change.
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    hkdf,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Write, path::Path};
use zeroize::Zeroizing;

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const WRAP_AAD: &[u8] = b"agentworks-secrets-key:v1";
const FORMAT: u32 = 2;
/// WebAuthn PRF inputs and outputs are 32 bytes.
const PRF_LEN: usize = 32;
const MAX_PASSKEYS: usize = 16;
const MAX_LABEL_CHARS: usize = 60;

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

/// The data key wrapped under `argon2id(password, salt)`; `password` is false for the
/// empty password. Also the whole of a version 1 key file, plus its `version`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordSlot {
    kdf: String,
    params: KdfParams,
    salt: String,
    wrapped_key: String,
    password: bool,
}
/// The data key wrapped under a passkey's PRF secret for `prf_salt`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasskeySlot {
    id: String,
    label: String,
    /// base64url, as WebAuthn reports it.
    credential_id: String,
    /// The hostname the passkey was created for; browsers offer it only there.
    rp_id: String,
    /// base64url of the 32-byte PRF input.
    prf_salt: String,
    wrapped_key: String,
    created: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Slot {
    Password(PasswordSlot),
    Passkey(PasskeySlot),
}
#[derive(Clone, Serialize, Deserialize)]
struct KeyFile {
    version: u32,
    slots: Vec<Slot>,
}
impl KeyFile {
    fn password(&self) -> &PasswordSlot {
        self.slots
            .iter()
            .find_map(|slot| match slot {
                Slot::Password(slot) => Some(slot),
                Slot::Passkey(_) => None,
            })
            .expect("read_key_file checks for one password slot")
    }
    fn passkeys(&self) -> impl Iterator<Item = &PasskeySlot> {
        self.slots.iter().filter_map(|slot| match slot {
            Slot::Passkey(slot) => Some(slot),
            Slot::Password(_) => None,
        })
    }
    fn with_password(&self, password: PasswordSlot) -> Self {
        let mut slots = vec![Slot::Password(password)];
        slots.extend(self.passkeys().cloned().map(Slot::Passkey));
        Self {
            version: FORMAT,
            slots,
        }
    }
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
    /// The password or passkey secret does not unwrap the key file.
    Wrong,
    /// The new password or passkey is not acceptable.
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
/// A passkey that can unlock the key file, as the browser needs it. Nothing here is
/// secret: the salt is useless without the authenticator.
#[derive(Clone, Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Passkey {
    pub id: String,
    pub label: String,
    pub credential_id: String,
    pub rp_id: String,
    pub prf_salt: String,
    pub created: i64,
}
impl From<&PasskeySlot> for Passkey {
    fn from(slot: &PasskeySlot) -> Self {
        Self {
            id: slot.id.clone(),
            label: slot.label.clone(),
            credential_id: slot.credential_id.clone(),
            rp_id: slot.rp_id.clone(),
            prf_salt: slot.prf_salt.clone(),
            created: slot.created,
        }
    }
}
/// A passkey the browser just created, with the PRF secret it returned for `prf_salt`.
pub struct NewPasskey {
    pub label: String,
    pub credential_id: String,
    pub rp_id: String,
    pub prf_salt: String,
    pub prf: Zeroizing<Vec<u8>>,
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
/// The passkeys that can unlock the key file.
pub fn passkeys(path: &Path) -> Result<Vec<Passkey>, String> {
    Ok(read_key_file(path)?.passkeys().map(Passkey::from).collect())
}
/// Removes a passkey slot. Needs no key: it only takes a way to unlock away.
pub fn remove_passkey(path: &Path, id: &str) -> Result<bool, PasswordError> {
    let mut file = read_key_file(path).map_err(PasswordError::Storage)?;
    let before = file.slots.len();
    file.slots
        .retain(|slot| !matches!(slot, Slot::Passkey(p) if p.id == id));
    if file.slots.len() == before {
        return Ok(false);
    }
    file.version = FORMAT;
    save(path, &file)?;
    Ok(true)
}
impl Locked {
    pub fn unlock(&self, password: &str) -> Result<Vault, PasswordError> {
        unwrap(self.file.password(), password.as_bytes()).map(Vault::new)
    }
    /// Unlocks with the PRF secret the passkey slot `id` returned.
    pub fn unlock_passkey(&self, id: &str, prf: &[u8]) -> Result<Vault, PasswordError> {
        let slot = self
            .file
            .passkeys()
            .find(|slot| slot.id == id)
            .ok_or_else(|| PasswordError::Invalid("That passkey was removed".into()))?;
        unwrap_passkey(slot, prf).map(Vault::new)
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
    /// Reads the key file and checks that `current` opens its password slot (any value
    /// when there is no password) and that it holds this vault's key.
    fn verified(&self, path: &Path, current: &str) -> Result<KeyFile, PasswordError> {
        let file = read_key_file(path).map_err(PasswordError::Storage)?;
        let slot = file.password();
        let current = if slot.password { current } else { "" };
        if unwrap(slot, current.as_bytes())? != self.data_key {
            return Err(PasswordError::Storage(
                "the key file no longer holds this server's key; restart Agentworks".into(),
            ));
        }
        Ok(file)
    }
    /// Re-wraps this vault's data key under `new` (empty removes the password) and
    /// replaces the key file atomically, keeping its passkeys. `current` must unwrap the
    /// existing file when it has a password, and the file must hold this vault's key.
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
        let file = self.verified(path, current)?;
        if new.is_empty() && file.passkeys().next().is_some() {
            return Err(PasswordError::Invalid(
                "Remove the passkeys first; they need a password to fall back on".into(),
            ));
        }
        let kdf = if new.is_empty() {
            DEFAULT_KDF
        } else {
            password_kdf
        };
        let slot = wrap(&self.data_key, new.as_bytes(), kdf).map_err(PasswordError::Storage)?;
        save(path, &file.with_password(slot))
    }
    /// Adds a passkey slot. Needs the current password, which must exist.
    pub fn add_passkey(
        &self,
        path: &Path,
        current: &str,
        new: NewPasskey,
        now: i64,
    ) -> Result<Passkey, PasswordError> {
        let mut file = self.verified(path, current)?;
        if !file.password().password {
            return Err(PasswordError::Invalid(
                "Set a password first; passkeys need it to fall back on".into(),
            ));
        }
        let invalid = |message: &str| PasswordError::Invalid(message.into());
        let label = match new.label.trim() {
            "" => "Passkey",
            label => label,
        };
        if label.chars().count() > MAX_LABEL_CHARS {
            return Err(invalid("Passkey names are limited to 60 characters"));
        }
        if !URL_SAFE_NO_PAD
            .decode(&new.credential_id)
            .is_ok_and(|id| !id.is_empty() && id.len() <= 1024)
        {
            return Err(invalid("The passkey's credential ID is not valid"));
        }
        let rp_id = new.rp_id.to_ascii_lowercase();
        if rp_id.is_empty()
            || rp_id.len() > 253
            || !rp_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Err(invalid("The passkey's site is not a valid hostname"));
        }
        let salt = URL_SAFE_NO_PAD
            .decode(&new.prf_salt)
            .ok()
            .filter(|salt| salt.len() == PRF_LEN)
            .ok_or_else(|| invalid("The passkey's salt must be 32 bytes"))?;
        if new.prf.len() != PRF_LEN {
            return Err(invalid("This passkey did not return a 32-byte secret"));
        }
        if file
            .passkeys()
            .any(|slot| slot.credential_id == new.credential_id)
        {
            return Err(invalid("This passkey can already unlock secrets"));
        }
        if file.passkeys().count() >= MAX_PASSKEYS {
            return Err(invalid(
                "Remove a passkey before adding another (16 at most)",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let kek = passkey_kek(&new.prf, &salt, &id).map_err(PasswordError::Storage)?;
        let wrapped = aead_seal(&aead(&*kek), &passkey_info(&id), &self.data_key)
            .map_err(PasswordError::Storage)?;
        let slot = PasskeySlot {
            id,
            label: label.to_owned(),
            credential_id: new.credential_id,
            rp_id,
            prf_salt: new.prf_salt,
            wrapped_key: STANDARD.encode(wrapped),
            created: now,
        };
        let added = Passkey::from(&slot);
        file.slots.push(Slot::Passkey(slot));
        file.version = FORMAT;
        save(path, &file)?;
        Ok(added)
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

/// Binds a passkey slot's key and wrapping to that slot.
fn passkey_info(id: &str) -> Vec<u8> {
    format!("agentworks-secrets-passkey:v1\0{id}").into_bytes()
}
fn passkey_kek(prf: &[u8], salt: &[u8], id: &str) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    let info = passkey_info(id);
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    hkdf::Salt::new(hkdf::HKDF_SHA256, salt)
        .extract(prf)
        .expand(&[&info], hkdf::HKDF_SHA256)
        .and_then(|okm| okm.fill(&mut *out))
        .map_err(|_| "key derivation failed".to_owned())?;
    Ok(out)
}

/// Wraps `data_key` under a key derived from `password` with a fresh salt.
fn wrap(data_key: &[u8], password: &[u8], kdf: KdfParams) -> Result<PasswordSlot, String> {
    let salt = random::<SALT_LEN>()?;
    let kek = derive(password, &salt, kdf)?;
    Ok(PasswordSlot {
        kdf: "argon2id".into(),
        params: kdf,
        salt: STANDARD.encode(salt),
        wrapped_key: STANDARD.encode(aead_seal(&aead(&*kek), WRAP_AAD, data_key)?),
        password: !password.is_empty(),
    })
}

fn damaged() -> PasswordError {
    PasswordError::Storage("the key file is damaged".into())
}
fn unwrap(slot: &PasswordSlot, password: &[u8]) -> Result<Zeroizing<Vec<u8>>, PasswordError> {
    if slot.kdf != "argon2id" {
        return Err(PasswordError::Storage(
            "the key file uses an unsupported format".into(),
        ));
    }
    let salt = STANDARD.decode(&slot.salt).map_err(|_| damaged())?;
    let wrapped = STANDARD.decode(&slot.wrapped_key).map_err(|_| damaged())?;
    let kek = derive(password, &salt, slot.params).map_err(PasswordError::Storage)?;
    // A wrong password and a tampered file both fail the tag check; without a
    // password, failure can only mean damage.
    match aead_open(&aead(&*kek), WRAP_AAD, &wrapped) {
        Some(key) if key.len() == KEY_LEN => Ok(key),
        Some(_) => Err(damaged()),
        None if slot.password => Err(PasswordError::Wrong),
        None => Err(PasswordError::Storage(
            "the key file could not be unwrapped".into(),
        )),
    }
}
fn unwrap_passkey(slot: &PasskeySlot, prf: &[u8]) -> Result<Zeroizing<Vec<u8>>, PasswordError> {
    if prf.len() != PRF_LEN {
        return Err(PasswordError::Wrong);
    }
    let salt = URL_SAFE_NO_PAD
        .decode(&slot.prf_salt)
        .map_err(|_| damaged())?;
    let wrapped = STANDARD.decode(&slot.wrapped_key).map_err(|_| damaged())?;
    let kek = passkey_kek(prf, &salt, &slot.id).map_err(PasswordError::Storage)?;
    match aead_open(&aead(&*kek), &passkey_info(&slot.id), &wrapped) {
        Some(key) if key.len() == KEY_LEN => Ok(key),
        Some(_) => Err(damaged()),
        None => Err(PasswordError::Wrong),
    }
}

/// Reads either format. Version 1 is a single password slot with a `version` field.
fn read_key_file(path: &Path) -> Result<KeyFile, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let damaged = || format!("{} is damaged", path.display());
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| damaged())?;
    let file = match value["version"].as_u64() {
        Some(1) => KeyFile {
            version: 1,
            slots: vec![Slot::Password(
                serde_json::from_value(value).map_err(|_| damaged())?,
            )],
        },
        Some(2) => serde_json::from_value(value).map_err(|_| damaged())?,
        _ => return Err(format!("{} uses an unsupported format", path.display())),
    };
    let passwords = file
        .slots
        .iter()
        .filter(|slot| matches!(slot, Slot::Password(_)))
        .count();
    if passwords != 1 {
        return Err(damaged());
    }
    Ok(file)
}

fn open_with(path: &Path, kdf: KdfParams) -> Result<Opened, String> {
    match std::fs::metadata(path) {
        Ok(_) => {
            let file = read_key_file(path)?;
            if file.password().password {
                return Ok(Opened::Locked(Locked { file }));
            }
            let key = unwrap(file.password(), b"").map_err(|e| e.to_string())?;
            Ok(Opened::Unlocked(Box::new(Vault::new(key))))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = Zeroizing::new(random::<KEY_LEN>()?.to_vec());
            let file = KeyFile {
                version: FORMAT,
                slots: vec![Slot::Password(wrap(&key, b"", kdf)?)],
            };
            write_key_file(path, &file)
                .map_err(|e| format!("could not create {}: {e}", path.display()))?;
            Ok(Opened::Unlocked(Box::new(Vault::new(key))))
        }
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

fn save(path: &Path, file: &KeyFile) -> Result<(), PasswordError> {
    replace_key_file(path, file)
        .map_err(|e| PasswordError::Storage(format!("could not save {}: {e}", path.display())))
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
        assert_eq!(file["version"], 2);
        let slot = &file["slots"][0];
        assert_eq!(
            (
                slot["kind"].as_str(),
                slot["kdf"].as_str(),
                &slot["password"]
            ),
            (Some("password"), Some("argon2id"), &Value::Bool(false))
        );
        assert_eq!(file["slots"].as_array().unwrap().len(), 1);
        let wrapped = STANDARD
            .decode(slot["wrappedKey"].as_str().unwrap())
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

    fn new_passkey(credential: &[u8], salt: [u8; 32], prf: [u8; 32]) -> NewPasskey {
        NewPasskey {
            label: " Laptop Touch ID ".into(),
            credential_id: URL_SAFE_NO_PAD.encode(credential),
            rp_id: "LocalHost".into(),
            prf_salt: URL_SAFE_NO_PAD.encode(salt),
            prf: Zeroizing::new(prf.to_vec()),
        }
    }
    fn locked(path: &Path) -> Locked {
        match open_with(path, TEST_KDF).unwrap() {
            Opened::Locked(locked) => locked,
            Opened::Unlocked(_) => panic!("expected a locked key file"),
        }
    }

    #[test]
    fn a_version_1_key_file_opens_and_is_upgraded_when_it_changes() {
        let dir = temp();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k");
        let key = random::<KEY_LEN>().unwrap();
        let slot = wrap(&key, b"", TEST_KDF).unwrap();
        let mut legacy = serde_json::to_value(&slot).unwrap();
        legacy["version"] = 1.into();
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let vault = open_or_create(&path, TEST_KDF).unwrap();
        assert_eq!(&*vault.data_key, &key);
        assert!(passkeys(&path).unwrap().is_empty());
        vault
            .set_password_with(&path, "", "correct horse", TEST_KDF)
            .unwrap();
        let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file["version"], 2);
        assert_eq!(
            &*locked(&path).unlock("correct horse").unwrap().data_key,
            &key
        );
        let mut unknown = legacy.clone();
        unknown["version"] = 9.into();
        std::fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(
            open_with(&path, TEST_KDF)
                .err()
                .unwrap()
                .contains("unsupported format")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn passkeys_unlock_the_same_key_and_need_a_password_to_fall_back_on() {
        let dir = temp();
        let path = dir.join("k");
        let vault = open_or_create(&path, TEST_KDF).unwrap();
        let (salt, prf) = (random::<32>().unwrap(), random::<32>().unwrap());
        assert!(matches!(
            vault.add_passkey(&path, "", new_passkey(b"cred-1", salt, prf), 1),
            Err(PasswordError::Invalid(message)) if message.contains("Set a password first")
        ));
        vault
            .set_password_with(&path, "", "correct horse", TEST_KDF)
            .unwrap();
        assert_eq!(
            vault
                .add_passkey(&path, "wrong one", new_passkey(b"cred-1", salt, prf), 1)
                .unwrap_err(),
            PasswordError::Wrong
        );
        let mut short = new_passkey(b"cred-1", salt, prf);
        short.prf = Zeroizing::new(vec![1; 16]);
        assert!(matches!(
            vault.add_passkey(&path, "correct horse", short, 1),
            Err(PasswordError::Invalid(_))
        ));
        let added = vault
            .add_passkey(&path, "correct horse", new_passkey(b"cred-1", salt, prf), 7)
            .unwrap();
        assert_eq!(
            (added.label.as_str(), added.rp_id.as_str(), added.created),
            ("Laptop Touch ID", "localhost", 7)
        );
        assert!(matches!(
            vault.add_passkey(&path, "correct horse", new_passkey(b"cred-1", salt, prf), 8),
            Err(PasswordError::Invalid(message)) if message.contains("already")
        ));
        let (salt2, prf2) = (random::<32>().unwrap(), random::<32>().unwrap());
        let second = vault
            .add_passkey(
                &path,
                "correct horse",
                new_passkey(b"cred-2", salt2, prf2),
                9,
            )
            .unwrap();
        assert_eq!(passkeys(&path).unwrap(), [added.clone(), second.clone()]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains(&URL_SAFE_NO_PAD.encode(prf)),
            "secrets are never stored"
        );

        // Each passkey opens the same key; a wrong secret or another slot's does not.
        let file = locked(&path);
        assert_eq!(
            &*file.unlock_passkey(&added.id, &prf).unwrap().data_key,
            &*vault.data_key
        );
        assert_eq!(
            &*file.unlock_passkey(&second.id, &prf2).unwrap().data_key,
            &*vault.data_key
        );
        assert_eq!(
            file.unlock_passkey(&added.id, &prf2).unwrap_err(),
            PasswordError::Wrong
        );
        assert_eq!(
            file.unlock_passkey(&added.id, &[0; 5]).unwrap_err(),
            PasswordError::Wrong
        );
        assert!(matches!(
            file.unlock_passkey("missing", &prf),
            Err(PasswordError::Invalid(_))
        ));

        // Changing the password keeps the passkeys; removing it needs them gone first.
        vault
            .set_password_with(&path, "correct horse", "battery staple", TEST_KDF)
            .unwrap();
        assert!(locked(&path).unlock_passkey(&added.id, &prf).is_ok());
        assert!(matches!(
            vault.set_password_with(&path, "battery staple", "", TEST_KDF),
            Err(PasswordError::Invalid(message)) if message.contains("Remove the passkeys")
        ));
        assert!(remove_passkey(&path, &added.id).unwrap());
        assert!(!remove_passkey(&path, &added.id).unwrap());
        assert!(matches!(
            locked(&path).unlock_passkey(&added.id, &prf),
            Err(PasswordError::Invalid(_))
        ));
        assert!(remove_passkey(&path, &second.id).unwrap());
        vault
            .set_password_with(&path, "battery staple", "", TEST_KDF)
            .unwrap();
        assert!(open_or_create(&path, TEST_KDF).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
