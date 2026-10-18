use crate::{Approval, Session, adapter_path, opencode_path, use_api_key};
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AuthenticateRequest, CancelNotification, ContentBlock, InitializeRequest,
        ListSessionsRequest, LoadSessionRequest, NewSessionRequest, PromptRequest,
        RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
        ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigOptionValue,
        SessionNotification, SetSessionConfigOptionRequest, TextContent,
    },
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub enum Command {
    Prompt(String),
    Cancel,
    Model(String),
}

fn agent_config(provider: &str, api_key_mode: bool, host_profile: bool) -> AcpAgentConfig {
    let config = if provider == "demo" {
        AcpAgentConfig::new("node").arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/demo-agent.mjs")
                .to_string_lossy(),
        )
    } else if provider == "opencode" {
        let home = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".agentworks/opencode");
        let mut config =
            AcpAgentConfig::new(opencode_path().unwrap_or_else(|| "opencode".into())).arg("acp");
        if !host_profile {
            for (name, dir) in [
                ("XDG_DATA_HOME", "data"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_CONFIG_HOME", "config"),
            ] {
                config = config.env(name, home.join(dir).to_string_lossy());
            }
        }
        // Free-model testing is isolated from the user's paid provider accounts.
        config
            .env("OPENAI_API_KEY", "")
            .env("CODEX_API_KEY", "")
            .env("ANTHROPIC_API_KEY", "")
    } else {
        let mut config = AcpAgentConfig::new(adapter_path(provider)).env("NO_BROWSER", "1");
        if api_key_mode {
            let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".agentworks")
                .join(provider);
            if provider == "codex" {
                config = config
                    .env("CODEX_HOME", home.to_string_lossy())
                    .env("ANTHROPIC_API_KEY", "");
            } else {
                config = config
                    .env("CLAUDE_CONFIG_DIR", home.to_string_lossy())
                    .env("OPENAI_API_KEY", "")
                    .env("CODEX_API_KEY", "");
            }
        } else {
            // Inherit the host CLI configuration/credential store without copying tokens.
            // An incidental API-key env var must not switch subscription sessions to API billing.
            config = config
                .env("OPENAI_API_KEY", "")
                .env("CODEX_API_KEY", "")
                .env("ANTHROPIC_API_KEY", "")
                .env("DEFAULT_AUTH_REQUEST", "");
        }
        config
    };
    config
}

fn agent(provider: &str) -> AcpAgent {
    agent_for(provider, false)
}
fn agent_for(provider: &str, host_profile: bool) -> AcpAgent {
    AcpAgent::new(agent_config(provider, use_api_key(), host_profile))
}

fn prepare(provider: &str) -> std::io::Result<()> {
    if provider == "opencode" {
        for dir in ["data", "cache", "state", "config"] {
            std::fs::create_dir_all(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join(".agentworks/opencode")
                    .join(dir),
            )?;
        }
    } else if provider != "demo" && use_api_key() {
        std::fs::create_dir_all(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".agentworks")
                .join(provider),
        )?;
    }
    Ok(())
}

pub fn is_free_model(id: &str) -> bool {
    id.starts_with("opencode/") && (id.ends_with("-free") || id == "opencode/big-pickle")
}
fn parse_model_config(config: &Value, free_only: bool) -> Option<Value> {
    let model = config["configOptions"]
        .as_array()?
        .iter()
        .find(|c| c["category"] == "model" || c["id"] == "model")?;
    fn collect(items: &[Value], out: &mut Vec<Value>, free_only: bool) {
        for option in items {
            if let Some(group) = option["options"].as_array() {
                collect(group, out, free_only);
            } else if option["value"]
                .as_str()
                .is_some_and(|id| !free_only || is_free_model(id))
            {
                out.push(option.clone());
            }
        }
    }
    let mut options = vec![];
    collect(model["options"].as_array()?, &mut options, free_only);
    if options.is_empty() {
        return None;
    }
    let current = if options.iter().any(|o| o["value"] == model["currentValue"]) {
        model["currentValue"].clone()
    } else {
        options[0]["value"].clone()
    };
    Some(json!({"configId":model["id"],"currentValue":current,"options":options}))
}

// Never surface configured credentials through an adapter diagnostic.
pub fn safe_error(message: &str) -> String {
    let mut safe = message.to_string();
    for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "ANTHROPIC_API_KEY"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                safe = safe.replace(&value, "[redacted]");
            }
        }
    }
    safe.chars().take(2500).collect()
}

pub async fn probe(provider: &str) -> Result<(), Box<dyn std::error::Error>> {
    prepare(provider)?;
    let name = provider.to_string();
    let result=tokio::time::timeout(Duration::from_secs(30),Client.builder()
        .connect_with(agent(provider), move |connection:ConnectionTo<Agent>|async move {
            let init=connection.send_request(InitializeRequest::new(ProtocolVersion::V1)).block_task().await?;
            println!("{}",json!({"provider":name,"protocolVersion":init.protocol_version,"agentInfo":init.agent_info,
                "capabilities":init.agent_capabilities,"authMethods":init.auth_methods}));
            Ok(())
        })).await;
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(safe_error(&e.to_string()).into()),
        Err(_) => Err("ACP initialization timed out after 30 seconds".into()),
    }
}

async fn initialize(
    connection: &ConnectionTo<Agent>,
    provider: &str,
) -> Result<Value, agent_client_protocol::Error> {
    let init = connection
        .send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    if init.protocol_version != ProtocolVersion::V1 {
        return Err(
            agent_client_protocol::Error::internal_error().data("Adapter did not negotiate ACP v1")
        );
    }
    if provider == "codex"
        && use_api_key()
        && init
            .auth_methods
            .iter()
            .any(|m| m.id().0.as_ref() == "api-key")
    {
        connection
            .send_request(AuthenticateRequest::new("api-key".to_string()))
            .block_task()
            .await?;
    }
    Ok(serde_json::to_value(init).unwrap_or(Value::Null))
}
async fn new_session(
    connection: &ConnectionTo<Agent>,
    provider: &str,
    cwd: PathBuf,
) -> Result<Value, agent_client_protocol::Error> {
    initialize(connection, provider).await?;
    Ok(serde_json::to_value(
        connection
            .send_request(NewSessionRequest::new(cwd))
            .block_task()
            .await?,
    )
    .unwrap_or(Value::Null))
}
async fn open_session(
    connection: &ConnectionTo<Agent>,
    s: &Session,
) -> Result<Value, agent_client_protocol::Error> {
    let init = initialize(connection, &s.provider).await?;
    let saved = s.data.lock().unwrap().provider_session_id.clone();
    let Some(id) = saved else {
        if !s.data.lock().unwrap().requests.is_empty() {
            return Err(agent_client_protocol::Error::internal_error().data("Saved conversation has no harness session ID; its history is available but it cannot be resumed safely."));
        }
        return Ok(serde_json::to_value(
            connection
                .send_request(
                    NewSessionRequest::new(s.cwd.clone()).mcp_servers(crate::mcp::configuration(s)),
                )
                .block_task()
                .await?,
        )
        .unwrap_or(Value::Null));
    };
    let import_pending = s.data.lock().unwrap().import_pending;
    let result = if import_pending {
        if init["agentCapabilities"]["loadSession"] != true {
            return Err(agent_client_protocol::Error::internal_error()
                .data("This harness cannot load existing conversation history"));
        }
        connection
            .send_request(
                LoadSessionRequest::new(id.clone(), s.cwd.clone())
                    .mcp_servers(crate::mcp::configuration(s)),
            )
            .block_task()
            .await
            .map(|response| serde_json::to_value(response).unwrap_or(Value::Null))
    } else if init["agentCapabilities"]["sessionCapabilities"]["resume"].is_object() {
        connection
            .send_request(
                ResumeSessionRequest::new(id.clone(), s.cwd.clone())
                    .mcp_servers(crate::mcp::configuration(s)),
            )
            .block_task()
            .await
            .map(|response| serde_json::to_value(response).unwrap_or(Value::Null))
    } else if init["agentCapabilities"]["loadSession"] == true {
        connection
            .send_request(
                LoadSessionRequest::new(id.clone(), s.cwd.clone())
                    .mcp_servers(crate::mcp::configuration(s)),
            )
            .block_task()
            .await
            .map(|response| serde_json::to_value(response).unwrap_or(Value::Null))
    } else {
        return Err(agent_client_protocol::Error::internal_error().data("This harness cannot resume saved sessions. The transcript is preserved; no new context was substituted."));
    };
    let mut config = match result {
        Ok(config) => config,
        Err(error)
            if error.code == agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound
                && {
                    let data = s.data.lock().unwrap();
                    !data.external && data.requests.is_empty()
                } =>
        {
            // Claude does not write a provider transcript until its first prompt. Nothing to lose here.
            return Ok(serde_json::to_value(
                connection
                    .send_request(
                        NewSessionRequest::new(s.cwd.clone())
                            .mcp_servers(crate::mcp::configuration(s)),
                    )
                    .block_task()
                    .await?,
            )
            .unwrap_or(Value::Null));
        }
        Err(error) => return Err(error),
    };
    config["sessionId"] = json!(id);
    Ok(config)
}
/// Starts a temporary ACP session to ask the harness for its actual model list. No prompt/inference.
pub async fn discover(
    provider: &str,
    cwd: PathBuf,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    prepare(provider)?;
    let provider = provider.to_owned();
    let free_only = provider == "opencode";
    let result = tokio::time::timeout(
        Duration::from_secs(75),
        Client
            .builder()
            .on_receive_request(
                async move |_request: RequestPermissionRequest, responder, _cx| {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(
                agent(&provider),
                move |connection: ConnectionTo<Agent>| async move {
                    let created = new_session(&connection, &provider, cwd).await?;
                    let config = parse_model_config(
                        &serde_json::to_value(created).unwrap_or(Value::Null),
                        free_only,
                    );
                    Ok(config.unwrap_or(json!({"options":[]})))
                },
            ),
    )
    .await;
    match result {
        Ok(result) => Ok(result?),
        Err(_) => Err("Model discovery timed out; try again".into()),
    }
}

pub async fn list_sessions(
    provider: &str,
    cwd: Option<PathBuf>,
    cursor: Option<String>,
    host_profile: bool,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    prepare(provider)?;
    let provider = provider.to_owned();
    let result = tokio::time::timeout(
        Duration::from_secs(45),
        Client.builder().connect_with(
            agent_for(&provider, host_profile),
            move |connection: ConnectionTo<Agent>| async move {
                let init = initialize(&connection, &provider).await?;
                if !init["agentCapabilities"]["sessionCapabilities"]["list"].is_object() {
                    return Err(agent_client_protocol::Error::internal_error()
                        .data("This harness does not advertise session listing"));
                }
                let response = connection
                    .send_request(ListSessionsRequest::new().cwd(cwd).cursor(cursor))
                    .block_task()
                    .await?;
                Ok(serde_json::to_value(response).unwrap_or(Value::Null))
            },
        ),
    )
    .await;
    match result {
        Ok(result) => Ok(result?),
        Err(_) => Err("Session discovery timed out; try again".into()),
    }
}

pub async fn run(
    s: Arc<Session>,
    mut commands: mpsc::Receiver<Command>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    prepare(&s.provider)?;
    let notify_s = s.clone();
    let permission_s = s.clone();
    let replaying = Arc::new(AtomicBool::new(true));
    let notify_replaying = replaying.clone();
    let import_pending = s.data.lock().unwrap().import_pending;
    let host_profile = s.data.lock().unwrap().opencode_host;
    let history = Arc::new(std::sync::Mutex::new((Vec::<Value>::new(), 0usize, false)));
    let notify_history = history.clone();
    Client.builder()
        .on_receive_notification(async move |notification:SessionNotification,_cx| {
            let update=serde_json::to_value(&notification.update).unwrap_or(Value::Null);
            if update["sessionUpdate"]=="available_commands_update" {
                notify_s.set_commands(update["availableCommands"].clone());
            } else if update["sessionUpdate"]=="config_option_update" {
                if let Some(config)=parse_model_config(&update,notify_s.provider=="opencode") {notify_s.set_models(config);}
            }
            if import_pending && notify_replaying.load(Ordering::SeqCst) && !matches!(update["sessionUpdate"].as_str(),Some("available_commands_update"|"config_option_update")) {
                let mut staged=notify_history.lock().unwrap();staged.1+=update.to_string().len();
                if staged.1>32*1024*1024 || staged.0.len()>=50000 {staged.2=true;}else{staged.0.push(update.clone());}
            }
            if !notify_replaying.load(Ordering::SeqCst) || matches!(update["sessionUpdate"].as_str(),Some("available_commands_update"|"config_option_update")) {
                crate::limit_resume::observe(&notify_s, &update);
                crate::timeouts::observe(&notify_s.id,&update,crate::timeouts::now_ms());
                notify_s.emit("acp",update);
            }
            Ok(())
        },agent_client_protocol::on_receive_notification!())
        .on_receive_request(async move |request:RequestPermissionRequest,responder,_cx| {
            let id=Uuid::new_v4().to_string();
            let value=serde_json::to_value(&request).unwrap_or(Value::Null);
            let (tx,rx)=oneshot::channel();
            {
                let mut approvals = permission_s.approvals.lock().unwrap();
                let mut data = permission_s.data.lock().unwrap();
                if data.lifecycle.suspended || !data.turn_active || data.status == "cancelling" {
                    return responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
                }
                approvals.insert(id.clone(), Approval{request:value.clone(),answer:tx});
                data.status = "waiting".into();
                drop(data);
                // Publish before cancellation can remove this approval. Otherwise a
                // late approval event could follow approval_resolved in the browser.
                permission_s.emit("approval",json!({"id":id,"request":value}));
                permission_s.emit("status",json!({"status":"waiting"}));
            }
            let outcome=match rx.await {
                Ok(Some(option))=>RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option)),
                _=>RequestPermissionOutcome::Cancelled,
            };
            responder.respond(RequestPermissionResponse::new(outcome))
        },agent_client_protocol::on_receive_request!())
        .connect_with(agent_for(&s.provider,host_profile),move |connection:ConnectionTo<Agent>|async move {
            let created=tokio::time::timeout(Duration::from_secs(75),open_session(&connection,&s))
                .await.map_err(|_|agent_client_protocol::Error::internal_error().data("Session initialization timed out"))??;
            if import_pending && s.provider=="opencode" {
                if let Some(model)=created["configOptions"].as_array().and_then(|options|options.iter().find(|o|o["category"]=="model" || o["id"]=="model")).and_then(|o|o["currentValue"].as_str()) {
                    if !is_free_model(model) {return Err(agent_client_protocol::Error::internal_error().data("This OpenCode session uses a paid model. Agentworks currently supports free OpenCode models only; no model was changed."));}
                }
            }
            if import_pending {
                let (updates,_,overflow)=std::mem::take(&mut *history.lock().unwrap());
                if overflow {return Err(agent_client_protocol::Error::internal_error().data("Conversation history exceeds the 32 MB import limit; no partial history was imported"));}
                s.import_history(updates).map_err(|e|agent_client_protocol::Error::internal_error().data(e))?;
            }
            replaying.store(false,Ordering::SeqCst);
            let provider_id=created["sessionId"].as_str().ok_or_else(||agent_client_protocol::Error::internal_error().data("Harness returned no session ID"))?.to_owned();
            let desired_model={
                let mut data=s.data.lock().unwrap();
                let desired=if data.provider_session_id.is_some(){data.models["currentValue"].as_str().map(str::to_owned).or_else(||s.initial_model.clone())}else{s.initial_model.clone()};
                data.provider_session_id=Some(provider_id.clone());desired
            };
            s.emit("provider_session",json!({"id":provider_id}));
            let mut model_config=parse_model_config(&serde_json::to_value(&created).unwrap_or(Value::Null),s.provider=="opencode").unwrap_or(Value::Null);
            if model_config.is_null() {model_config=s.data.lock().unwrap().models.clone();}
            if s.provider=="opencode" && model_config.is_null() {
                return Err(agent_client_protocol::Error::internal_error().data("OpenCode did not advertise any free models; no paid model was selected"));
            }
            if let Some(model)=&desired_model {
                if !model_config["options"].as_array().is_some_and(|items|items.iter().any(|o|o["value"]==*model)) {
                    return Err(agent_client_protocol::Error::invalid_params().data("Selected/default model is no longer advertised. Load models and choose another, or clear the default in Settings."));
                }
                model_config["currentValue"]=json!(model);
            }
            if !model_config.is_null() {
                if desired_model.is_some() || s.provider=="opencode" {
                    let request=SetSessionConfigOptionRequest::new(provider_id.clone(),model_config["configId"].as_str().unwrap().to_owned(),SessionConfigOptionValue::value_id(model_config["currentValue"].as_str().unwrap().to_owned()));
                    let selected=tokio::time::timeout(Duration::from_secs(30),connection.send_request(request).block_task())
                        .await.map_err(|_|agent_client_protocol::Error::internal_error().data("Model selection timed out"))??;
                    if let Some(config)=parse_model_config(&serde_json::to_value(selected).unwrap_or(Value::Null),s.provider=="opencode") {model_config=config;}
                }
                s.set_models(model_config.clone());
            }
            crate::limit_resume::ready(&s);
            s.emit("ready",json!({"providerSessionId":provider_id,"configuration":serde_json::to_value(&created).unwrap_or(Value::Null)}));
            if !s.start_queued_prompt().map_err(|e|agent_client_protocol::Error::internal_error().data(e))? {s.status("idle");}
            while let Some(command)=commands.recv().await {
                match command {
                    Command::Cancel=>{},
                    Command::Model(model)=>{
                        let request=SetSessionConfigOptionRequest::new(provider_id.clone(),model_config["configId"].as_str().unwrap_or("model").to_owned(),SessionConfigOptionValue::value_id(model.clone()));
                        match tokio::time::timeout(Duration::from_secs(30),connection.send_request(request).block_task()).await {
                            Ok(Ok(selected))=>{
                                model_config["currentValue"]=json!(model);
                                if let Some(config)=parse_model_config(&serde_json::to_value(selected).unwrap_or(Value::Null),s.provider=="opencode") {model_config=config;}
                                s.set_models(model_config.clone());
                            }
                            Ok(Err(error))=>s.emit("error",json!({"message":safe_error(&error.to_string())})),
                            Err(_)=>return Err(agent_client_protocol::Error::internal_error().data("Model selection timed out; session closed")),
                        }
                        s.status("idle");
                    }
                    Command::Prompt(text)=>{
                        let request=connection.send_request(PromptRequest::new(provider_id.clone(),vec![ContentBlock::Text(TextContent::new(text))])).block_task();
                        tokio::pin!(request);
                        // Policy is fixed at launch; later edits apply to later turns.
                        let mut clock=crate::timeouts::Clock::new(crate::timeouts::effective(s.data.lock().unwrap().timeouts.clone()),crate::timeouts::now_ms());
                        let _activity=crate::timeouts::begin(&s.id);
                        s.emit("timeout_policy",json!({"policy":clock.policy}));
                        let grace=clock.policy.cancel_grace_seconds;
                        // Far-future sleep until a cancel/timeout arms the grace deadline.
                        let deadline=tokio::time::sleep(Duration::from_secs(400*86400));
                        tokio::pin!(deadline);
                        let mut tick=tokio::time::interval(Duration::from_secs(1));
                        let mut cancelled=false;
                        let mut timed_out:Option<&str>=None;
                        loop {
                            tokio::select! {
                                result=&mut request=>{
                                    s.clear_approvals();
                                    match (result,timed_out) {
                                        // A timed-out turn is a deliberate cancellation: never schedule quota continuation.
                                        (Ok(result),Some(kind))=>{crate::limit_resume::finish(&s,Some(&json!("cancelled")),None);s.emit("turn_end",json!({"stopReason":result.stop_reason,"timedOut":kind}));},
                                        (Err(error),Some(kind))=>{crate::limit_resume::finish(&s,Some(&json!("cancelled")),None);s.emit("error",json!({"message":safe_error(&error.to_string()),"timedOut":kind}));},
                                        (Ok(result),None)=>{crate::limit_resume::finish(&s, Some(&serde_json::to_value(&result.stop_reason).unwrap_or(Value::Null)), None);s.emit("turn_end",json!({"stopReason":result.stop_reason}));},
                                        (Err(error),None)=>{let raw=serde_json::to_value(&error).unwrap_or(Value::Null);crate::limit_resume::enrich_codex(&s,&raw).await;crate::limit_resume::finish(&s,None,Some(&raw));s.emit("error",json!({"message":safe_error(&error.to_string())}));},
                                    }
                                    s.status("idle");
                                    break;
                                }
                                command=commands.recv()=>match command {
                                    Some(Command::Cancel)=>{
                                        if !cancelled {
                                            s.clear_approvals();
                                            connection.send_notification(CancelNotification::new(provider_id.clone()))?;
                                            s.status("cancelling");
                                            deadline.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(grace));
                                            cancelled=true;
                                        }
                                    }
                                    Some(Command::Prompt(_))=>{},
                                    Some(Command::Model(_))=>{},
                                    None=>return Ok(()),
                                },
                                _=tick.tick(),if !cancelled=>{
                                    // Lock order: approvals guard released before any emit/status.
                                    let waiting=!s.approvals.lock().unwrap().is_empty();
                                    for event in clock.step(crate::timeouts::now_ms(),crate::timeouts::last_activity(&s.id),waiting) {
                                        match event {
                                            crate::timeouts::Event::InactivityWarning{quiet_seconds}=>s.emit("timeout_warning",json!({"kind":"inactivity","quietSeconds":quiet_seconds})),
                                            crate::timeouts::Event::TotalApproaching{remaining_seconds}=>s.emit("timeout_warning",json!({"kind":"total","remainingSeconds":remaining_seconds})),
                                            crate::timeouts::Event::Expired{reason}=>{
                                                // Cancel once and wait for the agent; never retry the tool or turn.
                                                timed_out=Some(reason);cancelled=true;
                                                s.clear_approvals();
                                                connection.send_notification(CancelNotification::new(provider_id.clone()))?;
                                                s.emit("timeout",json!({"kind":reason,"graceSeconds":grace}));
                                                s.status("cancelling");
                                                deadline.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(grace));
                                            }
                                        }
                                    }
                                }
                                _=&mut deadline=>{if timed_out.is_some(){crate::limit_resume::finish(&s,Some(&json!("cancelled")),None);}return Err(agent_client_protocol::Error::internal_error().data(format!("Agent did not stop within {grace} seconds after {}; session closed",match timed_out {Some("total")=>"the total turn limit",Some(_)=>"the inactivity timeout",None=>"cancellation"})))},
                            }
                        }
                    }
                }
            }
            Ok(())
        }).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_signin_inherits_credential_locations_and_does_not_force_api_keys() {
        for provider in ["claude", "codex"] {
            let config = agent_config(provider, false, false);
            assert!(!config.environment().contains_key("CODEX_HOME"));
            assert!(!config.environment().contains_key("CLAUDE_CONFIG_DIR"));
            assert_eq!(
                config
                    .environment()
                    .get("OPENAI_API_KEY")
                    .map(String::as_str),
                Some("")
            );
            assert_eq!(
                config
                    .environment()
                    .get("ANTHROPIC_API_KEY")
                    .map(String::as_str),
                Some("")
            );
            assert_eq!(
                config
                    .environment()
                    .get("DEFAULT_AUTH_REQUEST")
                    .map(String::as_str),
                Some("")
            );
        }
    }
    #[test]
    fn opencode_host_import_uses_host_store_without_api_key_overrides() {
        let isolated = agent_config("opencode", false, false);
        let host = agent_config("opencode", false, true);
        assert!(isolated.environment().contains_key("XDG_DATA_HOME"));
        for key in [
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ] {
            assert!(!host.environment().contains_key(key));
        }
        assert_eq!(
            host.environment().get("OPENAI_API_KEY").map(String::as_str),
            Some("")
        );
    }
    #[test]
    fn explicit_api_key_mode_keeps_its_own_store() {
        let codex = agent_config("codex", true, false);
        let claude = agent_config("claude", true, false);
        assert!(codex.environment()["CODEX_HOME"].ends_with(".agentworks/codex"));
        assert!(claude.environment()["CLAUDE_CONFIG_DIR"].ends_with(".agentworks/claude"));
    }
}
