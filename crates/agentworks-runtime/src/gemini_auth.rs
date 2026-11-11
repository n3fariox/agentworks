//! Host-local Gemini sign-in children, with structured form messages over stdio.
use crate::model_providers::{Profile, auth_root};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, LazyLock},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc},
};

struct Attempt {
    state: Arc<Mutex<Value>>,
    input: mpsc::Sender<String>,
    task: tokio::task::JoinHandle<()>,
}
static ATTEMPTS: LazyLock<Mutex<HashMap<String, Attempt>>> = LazyLock::new(Default::default);

pub async fn cancel(id: &str) {
    if let Some(attempt) = ATTEMPTS.lock().await.remove(id) {
        attempt.task.abort();
        let _ = attempt.task.await;
    }
}

pub async fn execute(request: &Value) -> Result<Value, String> {
    let profile: Profile =
        serde_json::from_value(request["profile"].clone()).map_err(|_| "Invalid Gemini profile")?;
    if profile.kind != "gemini_oauth" {
        return Err("Select a Gemini subscription provider".into());
    }
    let root = auth_root(&profile.id)?;
    let mut attempts = ATTEMPTS.lock().await;
    match request["operation"].as_str().unwrap_or("") {
        "gemini-start" => {
            if let Some(previous) = attempts.remove(&profile.id) {
                previous.task.abort();
                let _ = previous.task.await;
            }
            let mut child = tokio::process::Command::new(
                std::env::current_exe().map_err(|_| "Could not locate Agentworks")?,
            )
            .args(["goose-login", "gemini_oauth"])
            .env("GOOSE_PATH_ROOT", root)
            .env("AGENTWORKS_GEMINI_AUTH_JSON", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Could not start Gemini sign-in")?;
            let mut stdin = child.stdin.take().ok_or("Missing sign-in input")?;
            let mut output =
                BufReader::new(child.stdout.take().ok_or("Missing sign-in output")?).lines();
            let state = Arc::new(Mutex::new(json!({"status":"starting"})));
            let current = state.clone();
            let (input, mut incoming) = mpsc::channel::<String>(1);
            let task = tokio::spawn(async move {
                let run = async {
                    loop {
                        tokio::select! {
                            line = output.next_line() => {
                                let Some(line) = line.map_err(|_| "Could not read Gemini sign-in")? else { break; };
                                if let Ok(event) = serde_json::from_str::<Value>(&line) {
                                    let mut state = current.lock().await;
                                    match event["event"].as_str() {
                                        Some("authorization") => {
                                            *state = json!({"status":"pending","authorizationUrl":event["url"]});
                                        }
                                        Some("invalid-callback") => state["error"] = json!("Invalid callback URL. Paste the complete localhost URL from this sign-in's browser tab."),
                                        Some("exchanging") => *state = json!({"status":"exchanging"}),
                                        Some("complete") => *state = json!({"status":"complete"}),
                                        Some("error") => *state = json!({"status":"error","error":event["error"]}),
                                        _ => {}
                                    }
                                }
                            }
                            Some(url) = incoming.recv() => {
                                stdin.write_all(url.as_bytes()).await.map_err(|_| "Could not submit callback")?;
                                stdin.write_all(b"\n").await.map_err(|_| "Could not submit callback")?;
                                stdin.flush().await.map_err(|_| "Could not submit callback")?;
                            }
                        }
                    }
                    child
                        .wait()
                        .await
                        .map_err(|_| "Could not finish Gemini sign-in")?;
                    Ok::<_, &str>(())
                };
                let result = tokio::time::timeout(std::time::Duration::from_secs(360), run).await;
                let mut state = current.lock().await;
                if !matches!(state["status"].as_str(), Some("complete" | "error")) {
                    let error = match result {
                        Err(_) => "Sign-in expired. Try again.",
                        Ok(Err(error)) => error,
                        Ok(Ok(())) => "Gemini sign-in ended before completing. Try again.",
                    };
                    *state = json!({"status":"error","error":error});
                }
            });
            attempts.insert(profile.id, Attempt { state, input, task });
            Ok(json!({"status":"starting"}))
        }
        "gemini-status" => match attempts.get(&profile.id) {
            Some(attempt) => Ok(attempt.state.lock().await.clone()),
            None => Ok(json!({"status":"idle"})),
        },
        "gemini-callback" => {
            let url = request["callbackUrl"]
                .as_str()
                .ok_or("Paste the localhost callback URL")?
                .trim();
            if url.is_empty() || url.len() > 8192 || url.chars().any(char::is_control) {
                return Err("Paste a complete localhost callback URL".into());
            }
            let attempt = attempts.get(&profile.id).ok_or("Start sign-in first")?;
            let mut state = attempt.state.lock().await;
            if state["status"] != "pending" {
                return Err("This sign-in is not waiting for a callback".into());
            }
            state.as_object_mut().unwrap().remove("error");
            attempt
                .input
                .try_send(url.to_owned())
                .map_err(|_| "Sign-in is busy or has ended")?;
            Ok(json!({"ok":true}))
        }
        "gemini-cancel" => {
            if let Some(attempt) = attempts.remove(&profile.id) {
                attempt.task.abort();
                let _ = attempt.task.await;
            }
            Ok(json!({"status":"idle"}))
        }
        _ => Err("Unknown Gemini sign-in operation".into()),
    }
}
