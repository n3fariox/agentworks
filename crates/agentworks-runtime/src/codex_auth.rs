//! Server-owned Codex credentials. ACP workers receive access tokens only.
use crate::model_providers as store;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::LazyLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const TOKEN_METHOD: &str = "_agentworks/codex/token";
static REFRESH: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
type Result<T> = std::result::Result<T, String>;

// Deliberately no Debug: these values must not enter diagnostic/event output.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub expires_at: u64,
    pub account_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct AccessToken {
    pub access_token: String,
    pub expires_at: u64,
    pub account_id: Option<String>,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn issuer() -> String {
    std::env::var("AGENTWORKS_CODEX_OAUTH_ISSUER")
        .unwrap_or_else(|_| "https://auth.openai.com".into())
        .trim_end_matches('/')
        .to_owned()
}
fn records() -> Result<BTreeMap<String, Credentials>> {
    store::read("codex-oauth.json")
}
pub fn has_credentials(id: &str) -> bool {
    records().is_ok_and(|all| all.contains_key(id))
}
pub fn remove_unlocked(id: &str) -> Result<()> {
    let mut all = records()?;
    if all.remove(id).is_some() {
        store::write("codex-oauth.json", &all)?;
    }
    Ok(())
}
pub async fn save(id: &str, credentials: Credentials) -> Result<()> {
    let _refresh = REFRESH.lock().await;
    persist(id, credentials)
}
fn persist(id: &str, credentials: Credentials) -> Result<()> {
    let _lock = store::storage_lock()?;
    let profiles: Vec<store::Profile> = store::read("profiles.json")?;
    if !profiles
        .iter()
        .any(|p| p.id == id && p.kind == "chatgpt_codex")
    {
        return Err("Codex provider no longer exists".into());
    }
    let mut all = records()?;
    all.insert(id.to_owned(), credentials);
    store::write("codex-oauth.json", &all)
}
pub async fn disconnect(id: &str) -> Result<()> {
    let _refresh = REFRESH.lock().await;
    let _lock = store::storage_lock()?;
    remove_unlocked(id)
}

async fn exchange(params: &[(&str, &str)]) -> Result<TokenResponse> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Could not initialize OAuth client")?;
    let response = client
        .post(format!("{}/oauth/token", issuer()))
        .form(params)
        .send()
        .await
        .map_err(|_| "Could not reach OpenAI authentication")?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenAI authentication returned HTTP {}. Try signing in again.",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Could not read OAuth response")?;
    if bytes.len() > 256 * 1024 {
        return Err("OAuth response is too large".into());
    }
    let response: TokenResponse =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid OAuth response")?;
    if response.access_token.is_empty() {
        return Err("OAuth response did not include an access token".into());
    }
    Ok(response)
}
pub async fn exchange_code(code: &str, redirect: &str, verifier: &str) -> Result<Credentials> {
    let token = exchange(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect),
        ("code_verifier", verifier),
    ])
    .await?;
    let refresh_token = token
        .refresh_token
        .filter(|s| !s.is_empty())
        .ok_or("OAuth response did not include a refresh token")?;
    Ok(Credentials {
        access_token: token.access_token,
        refresh_token,
        id_token: token.id_token,
        expires_at: now().saturating_add(token.expires_in.unwrap_or(3600)),
        account_id: None,
    })
}
pub async fn access(id: &str) -> Result<AccessToken> {
    let _refresh = REFRESH.lock().await;
    let mut credentials = {
        let _lock = store::storage_lock()?;
        records()?
            .remove(id)
            .ok_or("Sign in with ChatGPT in Settings → Model providers first")?
    };
    if credentials.expires_at <= now().saturating_add(60) {
        let token = exchange(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", &credentials.refresh_token),
        ])
        .await?;
        credentials.access_token = token.access_token;
        if let Some(refresh) = token.refresh_token.filter(|s| !s.is_empty()) {
            credentials.refresh_token = refresh;
        }
        if token.id_token.is_some() {
            credentials.id_token = token.id_token;
        }
        credentials.expires_at = now().saturating_add(token.expires_in.unwrap_or(3600));
        persist(id, credentials.clone())?;
    }
    Ok(AccessToken {
        access_token: credentials.access_token,
        expires_at: credentials.expires_at,
        account_id: credentials.account_id,
    })
}
