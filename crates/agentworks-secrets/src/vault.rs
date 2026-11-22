//! Values encrypted at rest. A random data key encrypts each value (AES-256-GCM, bound
//! to the row's ID and name); the key file holds that data key wrapped by
//! `argon2id(password, salt)`, never the key itself. The password is empty until
//! password unlock exists, so a password can be added later by re-wrapping the data
//! key without touching stored values.
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
pub(crate) struct KdfParams {
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

/// The unwrapped data key.
pub struct Vault {
    key: LessSafeKey,
}
impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault")
    }
}
impl Vault {
    /// Opens the key file, creating it on first use. A damaged key file is reported,
    /// never replaced.
    pub fn open_or_create(path: &Path) -> Result<Self, String> {
        open_or_create(path, DEFAULT_KDF)
    }
    /// Encrypts a value for the secret row `id`/`name`, as (nonce, ciphertext‖tag).
    pub fn seal_value(
        &self,
        id: &str,
        name: &str,
        value: &str,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut sealed = seal(&self.key, &value_aad(id, name), value.as_bytes())?;
        let body = sealed.split_off(NONCE_LEN);
        Ok((sealed, body))
    }
    /// Decrypts a value sealed for the same row; `None` when it was sealed under another
    /// key file or for another row.
    pub fn open_value(&self, id: &str, name: &str, sealed: &[u8]) -> Option<Zeroizing<String>> {
        let bytes = open(&self.key, &value_aad(id, name), sealed)?;
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

pub(crate) fn open_or_create(path: &Path, kdf: KdfParams) -> Result<Vault, String> {
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
pub(crate) mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::PathBuf;

    // Minimum Argon2 cost keeps debug-build tests fast; production uses DEFAULT_KDF.
    pub(crate) const TEST_KDF: KdfParams = KdfParams { m: 8, t: 1, p: 1 };

    pub(crate) fn temp() -> PathBuf {
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
}
