//! Rig device login is staged in memory, then committed by the current attempt.
use super::*;
use rig_core::{
    http_client::{
        self, Bytes, HttpClientExt, LazyBody, MultipartForm, Request, Response as HttpResponse,
        StreamingResponse,
    },
    providers::chatgpt::auth::{
        AuthSource, Authenticator, DeviceCodeHandler, MemoryTokenStore, TokenStore,
    },
};
use std::{sync::Arc, time::Duration};

// Rig fixes its issuer URLs. Keep our operator-controlled fixture issuer usable
// without altering production endpoints or introducing a second OAuth client.
#[derive(Clone)]
struct Transport(rig_reqwest::ReqwestClient);
fn issuer_url(url: &str) -> String {
    url.strip_prefix("https://auth.openai.com")
        .map(|path| format!("{}{path}", credentials::issuer()))
        .unwrap_or_else(|| url.to_owned())
}
fn rewrite<T>(mut request: Request<T>) -> http_client::Result<Request<T>> {
    *request.uri_mut() = issuer_url(&request.uri().to_string())
        .parse()
        .map_err(|error| http_client::Error::Instance(Box::new(error)))?;
    Ok(request)
}
impl HttpClientExt for Transport {
    fn send<T, U>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<HttpResponse<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let request = rewrite(request.map(Into::<Bytes>::into));
        let client = self.0.clone();
        async move { client.send(request?).await }
    }
    fn send_multipart<U>(
        &self,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<HttpResponse<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        let request = rewrite(request);
        let client = self.0.clone();
        async move { client.send_multipart(request?).await }
    }
    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let request = rewrite(request);
        let client = self.0.clone();
        async move { client.send_streaming(request?).await }
    }
}
fn auth_error(error: rig_core::providers::chatgpt::auth::AuthError) -> String {
    // Rig can include raw response bodies in errors. Expose only the status.
    match error {
        rig_core::providers::chatgpt::auth::AuthError::Http(error) => error.non_success_status()
            .map(device_error).unwrap_or_else(|| "Could not reach OpenAI device sign-in.".into()),
        _ => "Device sign-in failed. Try again, enable device-code login in ChatGPT, or use browser sign-in.".into(),
    }
}
pub async fn start(Path(id): Path<String>) -> ApiResult {
    profile(&id)?;
    let state = random();
    let expires = credentials::now() + 900;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| bad("Could not initialize OAuth client"))?;
    let http = Transport(client.into());
    let store = Arc::new(MemoryTokenStore::default());
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let auth = Authenticator::new(
        AuthSource::OAuth,
        store.clone(),
        DeviceCodeHandler::new(move |prompt| {
            let _ = prompt_tx.send(prompt);
        }),
        false,
    );
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
    let task_id = id.clone();
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(900), async {
            let context = auth
                .sign_in_with_device_code(&http)
                .await
                .map_err(auth_error)?;
            let record = store
                .load("chatgpt")
                .await
                .map_err(|_| "Could not read Rig credentials")?
                .ok_or("Rig did not return credentials")?;
            let mut token: credentials::Credentials =
                serde_json::from_str(&record).map_err(|_| "Rig returned incomplete credentials")?;
            if token.access_token.is_empty()
                || token.refresh_token.is_empty()
                || token.expires_at <= credentials::now()
            {
                return Err("Rig returned incomplete or expired credentials".into());
            }
            token.account_id = context.account_id;
            Ok(token)
        })
        .await
        .unwrap_or_else(|_| Err("Sign-in expired. Try again.".into()));
        let mut transactions = TRANSACTIONS.lock().await;
        if !active(&transactions, &task_id, &task_state) {
            return;
        }
        // Persistence stays under the attempt lock, so cancellation, deletion,
        // and another sign-in cannot race a late Rig result into the real store.
        let result = match result {
            Ok(token) => credentials::save(&task_id, token).await,
            Err(error) => Err(error),
        };
        let status = transactions.status.get_mut(&task_id).unwrap();
        match result {
            Ok(()) => status.phase = "connected",
            Err(error) => {
                status.phase = "error";
                status.error = Some(error);
            }
        }
        transactions.rig_tasks.remove(&task_id);
    });
    transactions
        .rig_tasks
        .insert(id.clone(), task.abort_handle());
    drop(transactions);
    let prompt = tokio::time::timeout(Duration::from_secs(20), prompt_rx.recv()).await;
    let mut transactions = TRANSACTIONS.lock().await;
    if !active(&transactions, &id, &state) {
        let message = transactions
            .status
            .get(&id)
            .filter(|s| s.state == state)
            .and_then(|s| s.error.clone())
            .unwrap_or_else(|| "Sign-in was cancelled or superseded".into());
        return Err(bad(message));
    }
    match prompt {
        Ok(Some(prompt)) => Ok(Json(
            json!({"status":"pending", "verificationUrl":issuer_url(&prompt.verification_uri), "userCode":prompt.user_code, "expiresAt":expires}),
        )),
        _ => {
            transactions.cancel(&id).await;
            Err(bad("Could not start Rig device sign-in. Try again."))
        }
    }
}
