//! Explicit per-session permission-mode selection through the harness config API.
use crate::{ApiResult, App, fail, session};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Claude's normal Auto mode, never bypassPermissions/full access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeDefault {
    #[default]
    Auto,
    Default,
    AcceptEdits,
    Plan,
}
impl ClaudeDefault {
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
        }
    }
}

pub fn initial_claude_default(
    provider: &str,
    external: bool,
    setting: Option<ClaudeDefault>,
) -> Option<ClaudeDefault> {
    (provider == "claude" && !external).then(|| setting.unwrap_or_default())
}

/// Runs only for a new native Claude session. Await adapter confirmation before
/// publishing readiness or allowing the queued initial prompt to start.
pub async fn apply_initial<F, Fut>(
    mut config: Value,
    mode: ClaudeDefault,
    select: F,
) -> Result<Value, agent_client_protocol::Error>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, agent_client_protocol::Error>>,
{
    let mode = mode.id();
    if !config["options"]
        .as_array()
        .is_some_and(|items| items.iter().any(|o| o["value"] == mode))
    {
        if config.is_null() {
            config = json!({});
        }
        config["defaultFallback"] = json!(format!(
            "Claude default '{mode}' is not advertised by this adapter. Using the adapter's current permission mode; choose an available mode in Session options."
        ));
        return Ok(config);
    }
    let selected = select(
        config["configId"].as_str().unwrap().to_owned(),
        mode.to_owned(),
    )
    .await?;
    let confirmed = parse(&selected)
        .filter(|value| value["currentValue"] == mode)
        .ok_or_else(|| {
            agent_client_protocol::Error::internal_error().data(
                "Harness did not confirm the Claude default permission mode; no prompt was sent",
            )
        })?;
    Ok(confirmed)
}

pub fn parse(config: &Value) -> Option<Value> {
    let option = config["configOptions"]
        .as_array()?
        .iter()
        .find(|o| o["category"] == "mode" || o["id"] == "mode")?;
    let id = option["id"].as_str()?;
    let current = option["currentValue"].as_str()?;
    let options: Vec<Value> = option["options"]
        .as_array()?
        .iter()
        .filter(|o| o["value"].is_string())
        .cloned()
        .collect();
    if !options.iter().any(|o| o["value"] == current) {
        return None;
    }
    Some(json!({"configId":id,"currentValue":current,"options":options}))
}

pub async fn get(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    let config = s.data.lock().unwrap().permission_modes.clone();
    Ok(Json(config))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    mode: String,
}

pub async fn set(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Choice>,
) -> ApiResult {
    let _run = app.run_lock.lock().await;
    crate::service_restart::allow_start(&app)?;
    let s = session(&app, &id)?;
    {
        let mut data = s.data.lock().unwrap();
        if data.status != "idle" {
            return Err(fail(
                StatusCode::CONFLICT,
                "Choose a permission mode while the session is idle",
            ));
        }
        if !data.permission_modes["options"]
            .as_array()
            .is_some_and(|options| options.iter().any(|o| o["value"] == req.mode))
        {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Choose one of this session's advertised permission modes",
            ));
        }
        data.status = "configuring".into();
    }
    s.emit("status", json!({"status":"configuring"}));
    if let Err(error) = s.send_command(crate::runtime::Command::PermissionMode(req.mode)) {
        s.status("error");
        return Err(error);
    }
    Ok(Json(json!({"accepted":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn advertised(current: &str) -> Value {
        json!({"configOptions":[{"id":"mode","category":"mode","currentValue":current,"options":[{"value":"default"},{"value":"auto"},{"value":"plan"}]}]})
    }

    #[test]
    fn defaults_are_only_captured_for_new_claude_sessions() {
        assert_eq!(
            initial_claude_default("claude", false, None),
            Some(ClaudeDefault::Auto)
        );
        assert_eq!(
            initial_claude_default("claude", false, Some(ClaudeDefault::Plan)),
            Some(ClaudeDefault::Plan)
        );
        for provider in ["codex", "opencode", "demo"] {
            assert_eq!(
                initial_claude_default(provider, false, Some(ClaudeDefault::Auto)),
                None
            );
        }
        assert_eq!(
            initial_claude_default("claude", true, Some(ClaudeDefault::Auto)),
            None
        );
        // Existing saved state without this field never acquires today's default.
        let state: crate::SessionData = serde_json::from_value(json!({"status":"suspended","seq":0,"requests":[],"models":null,"available_commands":[],"turn_active":false,"project_id":null,"provider_session_id":"native-saved"})).unwrap();
        assert!(state.initial_claude_permission_mode.is_none());
    }

    #[tokio::test]
    async fn new_claude_waits_for_auto_acceptance_before_initial_prompt() {
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let (accept_tx, accept_rx) = tokio::sync::oneshot::channel();
        let (prompt_tx, mut prompt_rx) = tokio::sync::oneshot::channel();
        let initialization = tokio::spawn(async move {
            let config = apply_initial(
                parse(&advertised("default")).unwrap(),
                ClaudeDefault::Auto,
                |id, mode| async move {
                    request_tx.send((id, mode)).unwrap();
                    Ok(accept_rx.await.unwrap())
                },
            )
            .await?;
            prompt_tx.send(config["currentValue"].clone()).unwrap();
            Ok::<_, agent_client_protocol::Error>(())
        });
        assert_eq!(request_rx.await.unwrap(), ("mode".into(), "auto".into()));
        assert!(matches!(
            prompt_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        accept_tx.send(advertised("auto")).unwrap();
        initialization.await.unwrap().unwrap();
        assert_eq!(prompt_rx.await.unwrap(), "auto");
    }

    #[tokio::test]
    async fn unsupported_default_keeps_adapter_mode_without_selection() {
        for config in [parse(&advertised("default")).unwrap(), Value::Null] {
            let result = apply_initial(config.clone(), ClaudeDefault::AcceptEdits, |_, _| async {
                panic!("unsupported mode must never be requested");
                #[allow(unreachable_code)]
                Ok(Value::Null)
            })
            .await
            .unwrap();
            assert_eq!(result["currentValue"], config["currentValue"]);
            assert!(
                result["defaultFallback"]
                    .as_str()
                    .unwrap()
                    .contains("not advertised")
            );
        }
    }

    #[tokio::test]
    async fn rejected_or_unconfirmed_mode_stops_initialization() {
        let config = parse(&advertised("default")).unwrap();
        assert!(
            apply_initial(config.clone(), ClaudeDefault::Auto, |_, _| async {
                Err(agent_client_protocol::Error::invalid_params())
            })
            .await
            .is_err()
        );
        for response in [advertised("default"), json!({})] {
            assert!(
                apply_initial(config.clone(), ClaudeDefault::Auto, |_, _| async {
                    Ok(response)
                })
                .await
                .is_err()
            );
        }
    }

    #[test]
    fn keeps_advertised_modes_and_actual_fallback() {
        let config = json!({"configOptions":[{"id":"model","category":"model","currentValue":"x","options":[{"value":"x"}]},{"id":"mode","category":"mode","currentValue":"acceptEdits","options":[{"value":"default"},{"value":"auto"},{"value":"acceptEdits"}]}]});
        let parsed = parse(&config).unwrap();
        assert_eq!(parsed["currentValue"], "acceptEdits");
        assert_eq!(parsed["options"].as_array().unwrap().len(), 3);
        assert!(parse(&json!({"configOptions":[{"id":"mode","currentValue":"invented","options":[{"value":"auto"}]}]})).is_none());
        assert!(parse(&json!({"configOptions":[]})).is_none());
    }
}
