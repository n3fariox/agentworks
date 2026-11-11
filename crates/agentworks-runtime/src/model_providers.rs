//! Shared provider definitions and credentials. Public views never contain keys.
use agent_client_protocol::AcpAgentConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "compatible_kind")]
    pub kind: String,
    pub default_model: String,
    #[serde(default = "local_hosts")]
    pub host_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "compatible_kind")]
    pub kind: String,
    pub default_model: String,
    #[serde(default = "local_hosts")]
    pub host_ids: Vec<String>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub remove_key: bool,
}

fn compatible_kind() -> String {
    "openai-compatible".into()
}

fn local_hosts() -> Vec<String> {
    vec!["local".into()]
}

// This payload travels only from the server to an authorized worker over SSH.
// It is never persisted by the worker or included in API responses.
#[derive(Serialize, Deserialize)]
pub struct Connection {
    pub profile: Profile,
    pub api_key: Option<String>,
}

impl Connection {
    pub fn test_request(&self, operation: &str) -> Value {
        let mut p = json!(&self.profile);
        p["apiKey"] = json!(&self.api_key);
        p["removeKey"] = json!(self.api_key.is_none());
        json!({"operation":operation,"profile":p})
    }
}

pub fn authorize(id: &str, host: &str) -> Result<()> {
    let _lock = storage_lock()?;
    if profile(id)?.host_ids.iter().any(|h| h == host) {
        Ok(())
    } else {
        Err("This provider is not available on this host".into())
    }
}

pub fn connection(id: &str, host: &str) -> Result<Connection> {
    let _lock = storage_lock()?;
    let p = profile(id)?;
    if !p.host_ids.iter().any(|h| h == host) {
        return Err("This provider is not available on this host".into());
    }
    Ok(Connection {
        profile: p,
        api_key: secrets()?.remove(id),
    })
}

pub fn draft_connection(request: &Value, host: &str) -> Result<Connection> {
    let _lock = storage_lock()?;
    let mut draft = request["profile"].clone();
    if draft["defaultModel"]
        .as_str()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        draft["defaultModel"] = json!("discovery");
    }
    if draft["name"].as_str().unwrap_or("").trim().is_empty() {
        draft["name"] = json!("Connection test");
    }
    let input: Input = serde_json::from_value(draft).map_err(|_| "Invalid provider connection")?;
    let (p, supplied, remove) = validate(input)?;
    if !p.host_ids.iter().any(|h| h == host) {
        return Err("Select this host in Available on hosts before testing".into());
    }
    let key = if remove {
        None
    } else {
        supplied.or(secrets()?.remove(&p.id))
    };
    Ok(Connection {
        profile: p,
        api_key: key,
    })
}

pub fn directory() -> PathBuf {
    let path = std::env::var_os("AGENTWORKS_MODEL_PROVIDERS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::provider::runtime_root().join(".agentworks/model-providers"));
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .expect("working directory")
            .join(path)
    }
}

pub(crate) fn storage_lock() -> Result<fs::File> {
    let dir = directory();
    fs::create_dir_all(&dir).map_err(|_| "Could not create model provider store")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect model provider store")?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(dir.join(".store.lock"))
        .map_err(|_| "Could not open model provider store lock")?;
    file.lock()
        .map_err(|_| "Could not lock model provider store")?;
    Ok(file)
}

pub(crate) fn read<T: serde::de::DeserializeOwned + Default>(name: &str) -> Result<T> {
    match fs::read(directory().join(name)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "Invalid model provider store".into())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(_) => Err("Could not read model provider store".into()),
    }
}

pub(crate) fn write(name: &str, value: &impl Serialize) -> Result<()> {
    let dir = directory();
    fs::create_dir_all(&dir).map_err(|_| "Could not create model provider store")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect model provider store")?;
    }
    let temp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .map_err(|_| "Could not save model provider store")?;
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|_| "Could not encode model provider store")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Could not save model provider store")?;
        fs::rename(&temp, dir.join(name)).map_err(|_| "Could not replace model provider store")
    })();
    let _ = fs::remove_file(temp);
    result.map_err(str::to_owned)
}

fn profiles() -> Result<Vec<Profile>> {
    read("profiles.json")
}
fn secrets() -> Result<BTreeMap<String, String>> {
    read("credentials.json")
}
fn profile(id: &str) -> Result<Profile> {
    profiles()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "Model provider profile no longer exists on this host".into())
}
fn view(profile: &Profile, keys: &BTreeMap<String, String>) -> Value {
    let mut value = json!(profile);
    value["keyConfigured"] = json!(keys.contains_key(&profile.id));
    value["type"] = json!(profile.kind);
    if profile.kind == "chatgpt_codex" {
        value["signedIn"] = json!(crate::codex_auth::has_credentials(&profile.id));
    }
    value
}

fn validate(input: Input) -> Result<(Profile, Option<String>, bool)> {
    let id = input.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err("Invalid model provider ID".into());
    }
    let name = input.name.trim().to_owned();
    let default_model = input.default_model.trim().to_owned();
    let base_url = if input.kind == "openai-compatible" {
        input.base_url.trim().trim_end_matches('/').to_owned()
    } else {
        String::new()
    };
    if !matches!(
        input.kind.as_str(),
        "openai-compatible" | "chatgpt_codex" | "claude-code"
    ) {
        return Err("Unknown model provider type".into());
    }
    if input.kind == "openai-compatible" {
        let url: http::Uri = base_url
            .parse()
            .map_err(|_| "Use an absolute HTTP or HTTPS base URL")?;
        if !matches!(url.scheme_str(), Some("http" | "https"))
            || url.host().is_none()
            || url.authority().is_some_and(|a| a.as_str().contains('@'))
            || url.query().is_some()
            || base_url.contains('#')
        {
            return Err(
                "Use an HTTP or HTTPS base URL without credentials, query, or fragment".into(),
            );
        }
    }
    if input.kind != "openai-compatible" && input.api_key.as_ref().is_some_and(|k| !k.is_empty()) {
        return Err("Subscription providers use host sign-in, not an API key".into());
    }
    if name.is_empty()
        || name.chars().count() > 80
        || name.chars().any(char::is_control)
        || default_model.is_empty()
        || default_model.len() > 256
        || default_model.chars().any(char::is_control)
        || base_url.len() > 2048
    {
        return Err("Provide a name, base URL, and default model (at most 256 characters)".into());
    }
    let mut host_ids = input.host_ids;
    host_ids.sort();
    host_ids.dedup();
    if host_ids.len() > 64
        || host_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 100 || id.chars().any(char::is_control))
    {
        return Err("Invalid available hosts".into());
    }
    let key = input.api_key.filter(|k| !k.is_empty());
    if key
        .as_ref()
        .is_some_and(|k| k.len() > 8192 || k.chars().any(char::is_control))
    {
        return Err("Invalid API key".into());
    }
    if input.remove_key && key.is_some() {
        return Err("Choose either replace or remove API key".into());
    }
    Ok((
        Profile {
            id,
            name,
            base_url,
            kind: input.kind,
            default_model,
            host_ids,
        },
        key,
        input.remove_key,
    ))
}

pub fn dispatch(request: &Value) -> Result<Value> {
    let _lock = storage_lock()?;
    match request["operation"].as_str().unwrap_or("") {
        "list" => {
            let keys = secrets()?;
            Ok(json!({"profiles":profiles()?.iter().map(|p| view(p, &keys)).collect::<Vec<_>>()}))
        }
        "save" => {
            let input: Input = serde_json::from_value(request["profile"].clone())
                .map_err(|_| "Invalid model provider profile")?;
            let (p, key, remove) = validate(input)?;
            let mut profiles = profiles()?;
            let mut keys = secrets()?;
            if remove || p.kind != "openai-compatible" {
                keys.remove(&p.id);
            }
            if let Some(key) = key {
                keys.insert(p.id.clone(), key);
            }
            if let Some(old) = profiles.iter_mut().find(|old| old.id == p.id) {
                *old = p.clone();
            } else {
                profiles.push(p.clone());
            }
            write("credentials.json", &keys)?;
            write("profiles.json", &profiles)?;
            Ok(view(&p, &keys))
        }
        "delete" => {
            let id = request["id"].as_str().ok_or("Missing profile ID")?;
            let mut profiles = profiles()?;
            profiles.retain(|p| p.id != id);
            crate::codex_auth::remove_unlocked(id)?;
            write("profiles.json", &profiles)?;
            let mut keys = secrets()?;
            keys.remove(id);
            write("credentials.json", &keys)?;
            Ok(json!({"ok":true}))
        }
        _ => Err("Unknown model provider operation".into()),
    }
}

pub async fn execute(request: &Value) -> Result<Value> {
    if request["operation"] == "models" || request["operation"] == "test" {
        let (p, key) = {
            let _lock = storage_lock()?;
            if request["profile"].is_object() {
                let input: Input = serde_json::from_value(request["profile"].clone())
                    .map_err(|_| "Invalid provider connection")?;
                let (p, supplied, remove) = validate(input)?;
                let key = if remove {
                    None
                } else {
                    supplied.or(secrets()?.remove(&p.id))
                };
                (p, key)
            } else {
                let id = request["id"].as_str().ok_or("Select a model provider")?;
                (profile(id)?, secrets()?.remove(id))
            }
        };
        models(p, key).await
    } else {
        dispatch(request)
    }
}

async fn models(p: Profile, key: Option<String>) -> Result<Value> {
    if p.kind != "openai-compatible" {
        return Err("Choose a model ID and sign in on the execution host; subscription model discovery is not available here".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not create provider connection")?;
    let mut request = client.get(format!("{}/models", p.base_url));
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Could not connect to the provider (check its URL and TLS configuration)")?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err("Provider rejected authentication".into());
    }
    if !response.status().is_success() {
        return Err(format!(
            "Provider reached, but model discovery returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "Provider reached, but returned an invalid model list")?;
    let data = body["data"]
        .as_array()
        .ok_or("Provider reached, but did not return an OpenAI model list")?;
    let options: Vec<_> = data
        .iter()
        .filter_map(|m| m["id"].as_str())
        .take(2000)
        .map(|id| json!({"value":id,"name":id}))
        .collect();
    Ok(
        json!({"connected":true,"authentication":"accepted by model listing endpoint","options":options,"currentValue":p.default_model}),
    )
}

pub fn configure(config: AcpAgentConfig, id: &str) -> Result<AcpAgentConfig> {
    configure_connection(config, connection(id, "local")?)
}

pub fn configure_connection(
    config: AcpAgentConfig,
    connection: Connection,
) -> Result<AcpAgentConfig> {
    let p = connection.profile;
    if p.kind != "openai-compatible" {
        let mut config = config
            .env("GOOSE_PROVIDER", &p.kind)
            .env("GOOSE_MODEL", &p.default_model)
            .env("GOOSE_PATH_ROOT", auth_root(&p.id)?.to_string_lossy());
        if p.kind == "chatgpt_codex" {
            config = config.env("AGENTWORKS_CODEX_MANAGED", "1");
        }
        if p.kind == "claude-code" {
            for key in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ] {
                config = config.env(key, "");
            }
            if let Some(command) = std::env::var_os("CLAUDE_CODE_EXECUTABLE") {
                config = config.env("CLAUDE_CODE_COMMAND", command.to_string_lossy());
            }
        }
        return Ok(config);
    }
    let key = connection.api_key.unwrap_or_default();
    let id = p.id.clone();
    let url: http::Uri = p
        .base_url
        .parse()
        .map_err(|_| "Invalid saved provider URL")?;
    let origin = format!(
        "{}://{}",
        url.scheme_str().ok_or("Invalid saved provider URL")?,
        url.authority().ok_or("Invalid saved provider URL")?
    );
    let prefix = url.path().trim_matches('/');
    // An explicit absolute path makes Goose honor Chat Completions even for
    // model names it would otherwise route to the Responses API.
    let path = if prefix.is_empty() {
        "/chat/completions".to_owned()
    } else {
        format!("/{prefix}/chat/completions")
    };
    Ok(config
        .env("GOOSE_PROVIDER", "openai")
        .env("GOOSE_MODEL", p.default_model)
        .env("OPENAI_BASE_URL", p.base_url)
        .env("OPENAI_HOST", origin)
        .env("OPENAI_BASE_PATH", path)
        .env("OPENAI_API_KEY", key)
        .env("OPENAI_CUSTOM_HEADERS", "")
        .env("OPENAI_ORGANIZATION", "")
        .env("OPENAI_PROJECT", "")
        .env(
            "GOOSE_PATH_ROOT",
            directory().join("goose").join(&id).to_string_lossy(),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_validation_and_public_view_exclude_keys() {
        let input = json!({"name":"Local", "baseUrl":"http://localhost:8080/v1/", "defaultModel":"model", "apiKey":"private-fixture"});
        let (p, key, _) = validate(serde_json::from_value(input.clone()).unwrap()).unwrap();
        assert_eq!(p.base_url, "http://localhost:8080/v1");
        let keys = BTreeMap::from([(p.id.clone(), key.unwrap())]);
        assert_eq!(view(&p, &keys)["keyConfigured"], true);
        assert!(!view(&p, &keys).to_string().contains("private-fixture"));
        for url in [
            "file:///etc/passwd",
            "https://user:secret@host/v1",
            "https://host/v1?key=secret",
            "/v1",
        ] {
            let mut bad = input.clone();
            bad["baseUrl"] = json!(url);
            assert!(validate(serde_json::from_value(bad).unwrap()).is_err());
        }
    }
    #[test]
    fn subscription_profiles_use_host_auth_without_api_credentials() {
        for kind in ["chatgpt_codex", "claude-code"] {
            let input = json!({"kind":kind,"name":"Subscription","defaultModel":"account-model"});
            let (p, key, _) = validate(serde_json::from_value(input.clone()).unwrap()).unwrap();
            assert!(key.is_none());
            assert!(p.base_url.is_empty());
            let expected_root = auth_root(&p.id).unwrap();
            let config = configure_connection(
                AcpAgentConfig::new("fixture"),
                Connection {
                    profile: p,
                    api_key: None,
                },
            )
            .unwrap();
            assert_eq!(config.environment()["GOOSE_PROVIDER"], kind);
            assert_eq!(config.environment()["GOOSE_MODEL"], "account-model");
            assert_eq!(
                config.environment()["GOOSE_PATH_ROOT"],
                expected_root.to_string_lossy()
            );
            assert!(!config.environment().contains_key("OPENAI_BASE_PATH"));
            if kind == "claude-code" {
                assert_eq!(config.environment()["ANTHROPIC_API_KEY"], "");
            }
            let mut invalid = input;
            invalid["apiKey"] = json!("must-not-be-used");
            assert!(validate(serde_json::from_value(invalid).unwrap()).is_err());
        }
        assert!(auth_root("../outside-store").is_err());
    }

    #[test]
    fn legacy_profiles_keep_the_compatible_provider() {
        let profile: Profile = serde_json::from_value(json!({"id":uuid::Uuid::new_v4().to_string(),"name":"Legacy","baseUrl":"https://example.com/v1","defaultModel":"fixture"})).unwrap();
        assert_eq!(profile.kind, "openai-compatible");
        let config = configure_connection(
            AcpAgentConfig::new("fixture"),
            Connection {
                profile,
                api_key: Some("fixture-key".into()),
            },
        )
        .unwrap();
        assert_eq!(
            config.environment()["OPENAI_BASE_PATH"],
            "/v1/chat/completions"
        );
        assert_eq!(config.environment()["OPENAI_API_KEY"], "fixture-key");
    }
}

/// Auth tokens belong to the execution host, never the shared credential payload.
pub fn auth_root(id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(id).map_err(|_| "Invalid model provider ID")?;
    Ok(directory().join("goose").join(id))
}

pub fn login_terminal(kind: &str, id: &str) -> Result<crate::terminal::Handle> {
    if kind != "claude-code" {
        return Err("This provider does not use subscription sign-in".into());
    }
    let root = auth_root(id)?;
    let executable = std::env::current_exe().map_err(|_| "Could not locate Agentworks")?;
    // webbrowser splits BROWSER on whitespace. procfs keeps Linux executable
    // paths usable even when the installation directory contains spaces.
    #[cfg(target_os = "linux")]
    let browser_executable = format!("/proc/{}/exe", std::process::id());
    #[cfg(not(target_os = "linux"))]
    let browser_executable = executable.to_string_lossy().into_owned();
    let mut command = std::process::Command::new(&executable);
    command
        .args(["goose-login", kind])
        .env("GOOSE_PATH_ROOT", root)
        .env(
            "BROWSER",
            format!("{browser_executable} goose-browser-url %s"),
        );
    crate::terminal::spawn_command(std::env::temp_dir(), command)
}
