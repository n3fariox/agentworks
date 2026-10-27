//! Server-managed browser and experimental device-code Codex sign-in.
use crate::{ApiResult, fail};
use agentworks_runtime::{codex_auth as credentials, model_providers};
use axum::{
    Json,
    extract::{Path, Query},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::LazyLock};
use tokio::sync::Mutex;

mod browser;
mod rig_device;
pub use rig_device::start as start_rig_device;

struct Attempt {
    profile: String,
    redirect: String,
    verifier: String,
    expires: u64,
}
struct Status {
    state: String,
    phase: &'static str,
    error: Option<String>,
    expires: u64,
}
#[derive(Default)]
struct Transactions {
    pending: HashMap<String, Attempt>,
    rig_tasks: HashMap<String, tokio::task::AbortHandle>,
    browser: Option<browser::Binding>,
    status: HashMap<String, Status>,
}
static TRANSACTIONS: LazyLock<Mutex<Transactions>> =
    LazyLock::new(|| Mutex::new(Transactions::default()));
fn bad(message: impl Into<String>) -> (StatusCode, String) {
    fail(StatusCode::BAD_REQUEST, message)
}
fn profile(id: &str) -> Result<(), (StatusCode, String)> {
    let all = model_providers::dispatch(&json!({"operation":"list"})).map_err(bad)?;
    if all["profiles"].as_array().is_some_and(|profiles| {
        profiles
            .iter()
            .any(|p| p["id"] == id && p["kind"] == "chatgpt_codex")
    }) {
        Ok(())
    } else {
        Err(bad("Select a ChatGPT subscription provider"))
    }
}
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
impl Transactions {
    async fn cancel(&mut self, id: &str) {
        self.close_browser(id).await;
        if let Some(task) = self.rig_tasks.remove(id) {
            task.abort();
        }
        self.pending.retain(|_, attempt| attempt.profile != id);
        self.status.remove(id);
    }
    async fn expire(&mut self) {
        let now = credentials::now();
        let expired: Vec<_> = self
            .status
            .iter()
            .filter(|(_, status)| status.expires <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.close_browser(&id).await;
        }
        self.pending.retain(|_, attempt| attempt.expires > now);
        for (id, status) in &mut self.status {
            if matches!(status.phase, "pending" | "exchanging") && status.expires <= now {
                if let Some(task) = self.rig_tasks.remove(id) {
                    task.abort();
                }
                status.phase = "error";
                status.error = Some("Sign-in expired. Try again.".into());
            }
        }
    }
}
pub async fn start(Path(id): Path<String>) -> ApiResult {
    profile(&id)?;
    let redirect = browser::REDIRECT.to_owned();
    let state = random();
    let verifier = random();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url: reqwest::Url = format!("{}/oauth/authorize", credentials::issuer())
        .parse()
        .map_err(|_| bad("Invalid OAuth issuer"))?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", credentials::CLIENT_ID),
        ("redirect_uri", &redirect),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "goose"),
    ]);
    let expires = credentials::now() + 600;
    let mut transactions = TRANSACTIONS.lock().await;
    transactions.expire().await;
    transactions.cancel(&id).await;
    if transactions.pending.len() >= 128 {
        return Err(bad("Too many pending sign-ins"));
    }
    transactions
        .bind_browser(&id, &state, std::time::Duration::from_secs(600))
        .await?;
    transactions.pending.insert(
        state.clone(),
        Attempt {
            profile: id.clone(),
            redirect,
            verifier,
            expires,
        },
    );
    transactions.status.insert(
        id,
        Status {
            state,
            phase: "pending",
            error: None,
            expires,
        },
    );
    Ok(Json(
        json!({"authorizationUrl":url.as_str(),"status":"pending"}),
    ))
}
// Mirrors Codex CLI's public implementation. These endpoints are not a stable
// third-party API; keep device login separate from the existing browser flow.
#[derive(Deserialize)]
struct DeviceCode {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default)]
    interval: Value,
}
#[derive(Deserialize)]
struct DeviceApproval {
    authorization_code: String,
    code_verifier: String,
}
async fn device_request(
    client: &reqwest::Client,
    path: &str,
    body: Value,
) -> Result<reqwest::Response, String> {
    client
        .post(format!(
            "{}/api/accounts/deviceauth/{path}",
            credentials::issuer()
        ))
        .json(&body)
        .send()
        .await
        .map_err(|_| "Could not reach OpenAI device sign-in".into())
}
async fn device_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, String> {
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Could not read device sign-in response")?;
    if bytes.len() > 256 * 1024 {
        return Err("Device sign-in response is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid device sign-in response".into())
}
fn device_error(status: StatusCode) -> String {
    format!(
        "OpenAI device sign-in returned HTTP {}. Device-code login may need enabling in ChatGPT security settings or by your workspace admin. You can also use browser sign-in.",
        status.as_u16()
    )
}
fn active(transactions: &Transactions, id: &str, state: &str) -> bool {
    transactions.status.get(id).is_some_and(|s| {
        s.state == state
            && s.expires > credentials::now()
            && matches!(s.phase, "pending" | "exchanging")
    })
}
pub async fn start_device(Path(id): Path<String>) -> ApiResult {
    profile(&id)?;
    let state = random();
    let expires = credentials::now() + 900;
    {
        let mut transactions = TRANSACTIONS.lock().await;
        transactions.expire().await;
        transactions.cancel(&id).await;
        if transactions
            .status
            .values()
            .filter(|s| matches!(s.phase, "pending" | "exchanging"))
            .count()
            >= 128
        {
            return Err(bad("Too many pending sign-ins"));
        }
        transactions.status.insert(
            id.clone(),
            Status {
                state: state.clone(),
                phase: "pending",
                error: None,
                expires,
            },
        );
    }
    let setup = async {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| "Could not initialize OAuth client".to_owned())?;
        let response = device_request(
            &client,
            "usercode",
            json!({"client_id": credentials::CLIENT_ID}),
        )
        .await?;
        if !response.status().is_success() {
            return Err(device_error(response.status()));
        }
        let code: DeviceCode = device_json(response).await?;
        if code.device_auth_id.is_empty() || code.user_code.is_empty() {
            return Err("Device sign-in response did not include a code".into());
        }
        Ok((client, code))
    }
    .await;
    let mut transactions = TRANSACTIONS.lock().await;
    if !active(&transactions, &id, &state) {
        return Err(bad("Sign-in was cancelled or superseded"));
    }
    let (client, code) = match setup {
        Ok(result) => result,
        Err(error) => {
            let status = transactions.status.get_mut(&id).unwrap();
            status.phase = "error";
            status.error = Some(error.clone());
            return Err(bad(error));
        }
    };
    let result = json!({"status":"pending", "verificationUrl":format!("{}/codex/device", credentials::issuer()), "userCode":code.user_code, "expiresAt":expires});
    drop(transactions);
    tokio::spawn(async move {
        let interval = code
            .interval
            .as_u64()
            .or_else(|| code.interval.as_str().and_then(|s| s.trim().parse().ok()))
            .unwrap_or(5)
            .max(1);
        loop {
            {
                let mut transactions = TRANSACTIONS.lock().await;
                transactions.expire().await;
                if !active(&transactions, &id, &state) {
                    return;
                }
            }
            let result = async {
                let response = device_request(
                    &client,
                    "token",
                    json!({"device_auth_id":code.device_auth_id, "user_code":code.user_code}),
                )
                .await?;
                if matches!(
                    response.status(),
                    StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
                ) {
                    return Ok(None);
                }
                if !response.status().is_success() {
                    return Err(device_error(response.status()));
                }
                let approval: DeviceApproval = device_json(response).await?;
                {
                    let mut transactions = TRANSACTIONS.lock().await;
                    if !active(&transactions, &id, &state) {
                        return Ok(None);
                    }
                    transactions.status.get_mut(&id).unwrap().phase = "exchanging";
                }
                credentials::exchange_code(
                    &approval.authorization_code,
                    &format!("{}/deviceauth/callback", credentials::issuer()),
                    &approval.code_verifier,
                )
                .await
                .map(Some)
            }
            .await;
            let mut transactions = TRANSACTIONS.lock().await;
            transactions.expire().await;
            if !active(&transactions, &id, &state) {
                return;
            }
            let result = match result {
                Ok(None) => {
                    drop(transactions);
                    let remaining = expires.saturating_sub(credentials::now());
                    tokio::time::sleep(std::time::Duration::from_secs(interval.min(remaining)))
                        .await;
                    continue;
                }
                Ok(Some(mut token)) => {
                    token.account_id = account_id(token.id_token.as_deref())
                        .or_else(|| account_id(Some(&token.access_token)));
                    credentials::save(&id, token).await
                }
                Err(error) => Err(error),
            };
            let status = transactions.status.get_mut(&id).unwrap();
            match result {
                Ok(()) => status.phase = "connected",
                Err(error) => {
                    status.phase = "error";
                    status.error = Some(error);
                }
            }
            return;
        }
    });
    Ok(Json(result))
}
pub async fn status(Path(id): Path<String>) -> ApiResult {
    profile(&id)?;
    let mut transactions = TRANSACTIONS.lock().await;
    transactions.expire().await;
    let status = transactions.status.get(&id);
    let signed_in = credentials::has_credentials(&id);
    Ok(Json(
        json!({"signedIn":signed_in,"status":status.map(|s| s.phase).unwrap_or(if signed_in {"connected"} else {"disconnected"}),"error":status.and_then(|s| s.error.as_deref())}),
    ))
}
pub async fn cancel(Path(id): Path<String>) -> ApiResult {
    profile(&id)?;
    TRANSACTIONS.lock().await.cancel(&id).await;
    Ok(Json(json!({"ok":true})))
}
pub async fn disconnect(Path(id): Path<String>) -> ApiResult {
    profile(&id)?;
    TRANSACTIONS.lock().await.cancel(&id).await;
    credentials::disconnect(&id).await.map_err(bad)?;
    Ok(Json(json!({"ok":true})))
}
pub async fn remove(id: &str) {
    TRANSACTIONS.lock().await.cancel(id).await;
}

#[derive(Deserialize)]
pub struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
pub async fn callback(Query(query): Query<Callback>) -> Response {
    let Some(state) = query.state else {
        return bad("Missing OAuth state").into_response();
    };
    let attempt = {
        let mut transactions = TRANSACTIONS.lock().await;
        transactions.expire().await;
        let Some(attempt) = transactions.pending.remove(&state) else {
            return bad("Sign-in expired, cancelled, or already completed").into_response();
        };
        transactions.close_browser(&attempt.profile).await;
        if let Some(status) = transactions.status.get_mut(&attempt.profile) {
            status.phase = "exchanging";
        }
        attempt
    };
    let result = if query.error.is_some() {
        Err("Sign-in was declined by the provider".to_owned())
    } else if let Some(code) = query
        .code
        .filter(|code| !code.is_empty() && code.len() <= 16384)
    {
        credentials::exchange_code(&code, &attempt.redirect, &attempt.verifier).await
    } else {
        Err("The provider did not return an authorization code".to_owned())
    };
    let mut transactions = TRANSACTIONS.lock().await;
    // Cancel/retry during the token exchange must never resurrect old credentials.
    if !transactions
        .status
        .get(&attempt.profile)
        .is_some_and(|s| s.state == state && s.expires > credentials::now())
    {
        return bad("Sign-in was cancelled or superseded").into_response();
    }
    let result = match result {
        Ok(mut token) => {
            if profile(&attempt.profile).is_err() {
                Err("Provider was removed during sign-in".into())
            } else {
                token.account_id = account_id(token.id_token.as_deref())
                    .or_else(|| account_id(Some(&token.access_token)));
                credentials::save(&attempt.profile, token).await
            }
        }
        Err(error) => Err(error),
    };
    let status = transactions.status.get_mut(&attempt.profile).unwrap();
    match result {
        Ok(()) => {
            status.phase = "connected";
            Html("<html><head><title>Sign-in complete</title></head><body><h1>Sign-in complete</h1><p>Return to Agentworks. You can close this tab.</p></body></html>").into_response()
        }
        Err(error) => {
            status.phase = "error";
            status.error = Some(error);
            (StatusCode::BAD_REQUEST, Html("<html><head><title>Sign-in failed</title></head><body><h1>Sign-in failed</h1><p>Return to Agentworks for details and try again.</p></body></html>")).into_response()
        }
    }
}
// Routing metadata from tokens received directly from the configured issuer.
// This is not used to authenticate a browser or assert a user's identity.
fn account_id(token: Option<&str>) -> Option<String> {
    let payload = token?.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .or_else(|| claims["chatgpt_account_id"].as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rig_tasks_stop_on_expiry_and_cancellation() {
        for expired in [false, true] {
            let task = tokio::spawn(std::future::pending::<()>());
            let mut transactions = Transactions::default();
            transactions
                .rig_tasks
                .insert("profile".into(), task.abort_handle());
            transactions.status.insert(
                "profile".into(),
                Status {
                    state: "state".into(),
                    phase: "pending",
                    error: None,
                    expires: if expired { 0 } else { credentials::now() + 900 },
                },
            );
            if expired {
                transactions.expire().await;
            } else {
                transactions.cancel("profile").await;
            }
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(transactions.rig_tasks.is_empty());
            assert!(!active(&transactions, "profile", "state"));
        }
    }
    #[tokio::test]
    async fn device_approval_requires_current_unexpired_attempt() {
        let mut transactions = Transactions::default();
        transactions.status.insert(
            "profile".into(),
            Status {
                state: "current".into(),
                phase: "pending",
                error: None,
                expires: credentials::now() + 900,
            },
        );
        assert!(active(&transactions, "profile", "current"));
        assert!(!active(&transactions, "profile", "superseded"));
        transactions.status.get_mut("profile").unwrap().expires = 0;
        assert!(!active(&transactions, "profile", "current"));
        transactions.expire().await;
        assert_eq!(transactions.status["profile"].phase, "error");
        assert!(!active(&transactions, "profile", "current"));
        transactions.cancel("profile").await;
        assert!(!active(&transactions, "profile", "current"));
    }
    #[tokio::test]
    async fn expired_and_cancelled_transactions_cannot_be_redeemed() {
        let mut transactions = Transactions::default();
        transactions.pending.insert(
            "expired".into(),
            Attempt {
                profile: "profile".into(),
                redirect: "http://localhost:5555/auth/callback".into(),
                verifier: random(),
                expires: 0,
            },
        );
        transactions.status.insert(
            "profile".into(),
            Status {
                state: "expired".into(),
                phase: "pending",
                error: None,
                expires: 0,
            },
        );
        transactions.expire().await;
        assert!(transactions.pending.is_empty());
        assert_eq!(transactions.status["profile"].phase, "error");
        transactions.cancel("profile").await;
        assert!(transactions.status.is_empty());
    }
}
