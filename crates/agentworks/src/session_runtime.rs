use crate::{Approval, Session, use_api_key};
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, CreateElicitationRequest, CreateElicitationResponse,
    ElicitationAction, EmbeddedResource, EmbeddedResourceResource, ForkSessionRequest,
    ImageContent, LoadSessionRequest, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SessionConfigOptionValue, SetSessionConfigOptionRequest,
    TextContent, TextResourceContents,
};
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo, DynConnectTo};
#[cfg(test)]
use agent_client_protocol::{AcpAgentConfig, schema::v1::SessionNotification};
use agentworks_runtime::acp::initialize_on;
#[cfg(test)]
use agentworks_runtime::acp::initialize_request;
use agentworks_runtime::provider::parse_model_config;
use agentworks_runtime::provider::{agent_config, is_free_model, prepare, safe_error};
use agentworks_runtime::subagents::{Notification, Sessions};
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
    /// A human prompt with attachments, consumed once; never replayed by queue,
    /// continuation or restart paths, which only carry text.
    AttachedPrompt {
        text: String,
        attachments: Vec<crate::attachments::Payload>,
    },
    InboxPrompt {
        text: String,
        owner: crate::inbox_delivery::Turn,
    },
    Close(oneshot::Sender<Result<(), String>>),
    Cancel,
    CancelInbox(crate::inbox_delivery::Turn),
    Model(String),
    PermissionMode(String),
    Config {
        id: String,
        value: Value,
    },
}

impl Command {
    pub(crate) fn cancels(&self, active: Option<&crate::inbox_delivery::Turn>) -> bool {
        match self {
            Self::Cancel => true,
            Self::CancelInbox(owner) => active == Some(owner),
            _ => false,
        }
    }
}

fn agent(provider: &str) -> AcpAgent {
    agent_for(provider, false)
}
fn agent_for(provider: &str, host_profile: bool) -> AcpAgent {
    AcpAgent::new(agent_config(provider, use_api_key(), host_profile))
}

async fn open_session(
    connection: &ConnectionTo<Agent>,
    s: &Session,
) -> Result<Value, agent_client_protocol::Error> {
    if {
        let data = s.data.lock().unwrap();
        data.provider_history_deleted || data.provider_history_delete_pending
    } {
        return Err(agent_client_protocol::Error::invalid_params().data(
            "Provider-history deletion was requested; inspect or retry deletion before continuing",
        ));
    }
    let init = initialize_on(
        connection,
        &s.provider,
        s.host_id == "local" && use_api_key(),
    )
    .await?;
    s.data.lock().unwrap().agent_capabilities = init["agentCapabilities"].clone();
    s.emit(
        "provider_capabilities",
        init["agentCapabilities"]["sessionCapabilities"].clone(),
    );
    let caps = init["agentCapabilities"]["promptCapabilities"].clone();
    s.data.lock().unwrap().prompt_capabilities = if caps.is_object() { caps } else { json!({}) };
    s.emit("attachment_policy", crate::attachments::session_policy(s));
    let saved = s.data.lock().unwrap().provider_session_id.clone();
    let Some(id) = saved else {
        let lineage = s.data.lock().unwrap().lineage.clone();
        if lineage.native_attempted {
            return Err(agent_client_protocol::Error::internal_error().data("Native fork outcome is uncertain. Inspect provider history; automatic retry is disabled."));
        }
        if lineage.context == "auto"
            && lineage.parent_host_id.as_deref().unwrap_or("local") == s.host_id
            && lineage
                .parent_provider
                .as_deref()
                .is_none_or(|p| p == s.provider)
            && lineage.source_message_id.is_some()
            && lineage.source_session_id.is_some()
            && init["agentCapabilities"]["sessionCapabilities"]["fork"].is_object()
        {
            {
                let mut data = s.data.lock().unwrap();
                data.lineage.native_attempted = true;
                s.store
                    .lock()
                    .unwrap()
                    .save_state(&s.id, &data)
                    .map_err(|_| {
                        agent_client_protocol::Error::internal_error()
                            .data("Could not persist native fork intent")
                    })?;
            }
            let meta = json!({"jetbrains":{"air":{"fork":{"version":1,"messageId":lineage.source_message_id}}}});
            let response = connection
                .send_request(
                    ForkSessionRequest::new(lineage.source_session_id.unwrap(), s.cwd.clone())
                        .mcp_servers(crate::mcp::configuration(s))
                        .meta(meta.as_object().unwrap().clone()),
                )
                .block_task()
                .await?;
            if Some(response.session_id.to_string())
                == s.data.lock().unwrap().lineage.source_session_id
            {
                return Err(agent_client_protocol::Error::internal_error()
                    .data("Harness returned the parent session as its fork; refusing to use it"));
            }
            {
                let mut data = s.data.lock().unwrap();
                data.provider_session_id = Some(response.session_id.to_string());
                data.lineage.native_used = true;
                data.carried_context = Some(format!(
                    "Native fork at recorded parent boundary {}. Original transcript directory: {}. Current directory: {}. Relative references in inherited history mean the original directory; verify files before retargeting them.",
                    lineage.boundary_seq,
                    lineage.parent_cwd.unwrap_or_default(),
                    s.cwd.display()
                ));
                s.store
                    .lock()
                    .unwrap()
                    .save_state(&s.id, &data)
                    .map_err(|_| {
                        agent_client_protocol::Error::internal_error()
                            .data("Could not persist native fork result; inspect provider history")
                    })?;
            }
            s.emit(
                "fork_mode",
                json!({"mode":"native","boundarySeq":lineage.boundary_seq}),
            );
            return Ok(serde_json::to_value(response).unwrap_or(Value::Null));
        }
        if lineage.parent_id.is_some() {
            s.emit("fork_mode", json!({"mode":"fresh","context":lineage.context,"truncated":lineage.truncated,
                "detail":"Fresh provider session; recorded context is sent with the first prompt. Hidden provider state is not equivalent."}));
        }
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
pub async fn discover_on(
    provider: &str,
    cwd: PathBuf,
    host: Option<Arc<crate::hosts::Live>>,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let api_key = host.is_none() && use_api_key();
    let (transport, _guard): (DynConnectTo<Client>, _) = if let Some(host) = host {
        let (lines, guard) = host.spawn(provider, &cwd.to_string_lossy(), false)?;
        (DynConnectTo::new(lines), Some(guard))
    } else {
        prepare(provider)?;
        (DynConnectTo::new(agent(provider)), None)
    };
    agentworks_runtime::acp::discover(transport, provider, cwd, api_key).await
}

pub async fn list_sessions_on(
    provider: &str,
    cwd: Option<PathBuf>,
    cursor: Option<String>,
    host_profile: bool,
    remote: Option<(Arc<crate::hosts::Live>, PathBuf)>,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let api_key = remote.is_none() && use_api_key();
    let (transport, _guard): (DynConnectTo<Client>, _) = if let Some((host, spawn_cwd)) = remote {
        let (lines, guard) = host.spawn(provider, &spawn_cwd.to_string_lossy(), host_profile)?;
        (DynConnectTo::new(lines), Some(guard))
    } else {
        prepare(provider)?;
        (DynConnectTo::new(agent_for(provider, host_profile)), None)
    };
    agentworks_runtime::acp::list_sessions(transport, provider, cwd, cursor, api_key).await
}

pub async fn run(
    s: Arc<Session>,
    mut commands: mpsc::Receiver<Command>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if s.host_id == "local" {
        prepare(&s.provider)?;
    }
    let child_sessions = {
        let data = s.data.lock().unwrap();
        let mut sessions = Sessions::new(data.provider_session_id.clone());
        if data.provider_session_id.is_some() {
            for event in &data.events {
                if event["kind"] == "acp" {
                    sessions.restore(&event["data"]);
                }
            }
        }
        Arc::new(std::sync::Mutex::new(sessions))
    };
    let notify_sessions = child_sessions.clone();
    let permission_sessions = child_sessions.clone();
    let question_sessions = child_sessions.clone();
    let notify_s = s.clone();
    let permission_s = s.clone();
    let question_s = s.clone();
    let replaying = Arc::new(AtomicBool::new(true));
    let notify_replaying = replaying.clone();
    let initial_mode = {
        let data = s.data.lock().unwrap();
        if s.provider == "claude" && data.provider_session_id.is_none() {
            data.initial_claude_permission_mode.map(|mode| mode.id())
        } else if s.provider == "codex" && data.provider_session_id.is_none() {
            data.initial_codex_permission_mode.map(|mode| mode.id())
        } else {
            None
        }
    };
    let saved_permission_modes = s.data.lock().unwrap().permission_modes.clone();
    let saved_config = s.data.lock().unwrap().session_config.clone();
    let import_pending = s.data.lock().unwrap().import_pending;
    let host_profile = s.data.lock().unwrap().opencode_host;
    let (transport, _spawn_guard): (DynConnectTo<Client>, _) = if s.host_id == "local" {
        (
            DynConnectTo::new(agent_for(&s.provider, host_profile)),
            None,
        )
    } else {
        let live = crate::hosts::registry()
            .and_then(|hosts| hosts.live(&s.host_id))
            .ok_or("The remote host is not connected.")?;
        let (lines, guard) = live.spawn(&s.provider, &s.cwd.to_string_lossy(), host_profile)?;
        // Keep the worker process alive until this runtime returns or is cancelled.
        (DynConnectTo::new(lines), Some(guard))
    };
    let history = Arc::new(std::sync::Mutex::new((Vec::<Value>::new(), 0usize, false)));
    let notify_history = history.clone();
    Client.builder()
        .on_receive_notification(async move |notification:Notification,_cx| {
            let Some((update,is_child))=notify_sessions.lock().unwrap().route(notification) else {return Ok(());};
            if !is_child && update["sessionUpdate"]=="available_commands_update" {
                notify_s.set_commands(update["availableCommands"].clone());
            } else if !is_child && matches!(update["sessionUpdate"].as_str(), Some("config_option_update"|"current_mode_update")) {
                crate::session_config::publish(&notify_s, &update);
            }
            if import_pending && notify_replaying.load(Ordering::SeqCst) && (is_child || !matches!(update["sessionUpdate"].as_str(),Some("available_commands_update"|"config_option_update"|"current_mode_update"))) {
                let mut staged=notify_history.lock().unwrap();staged.1+=update.to_string().len();
                if staged.1>32*1024*1024 || staged.0.len()>=50000 {staged.2=true;}else{staged.0.push(update.clone());}
            }
            if !notify_replaying.load(Ordering::SeqCst) || (!is_child && matches!(update["sessionUpdate"].as_str(),Some("available_commands_update"|"config_option_update"|"current_mode_update"))) {
                if !is_child {crate::limit_resume::observe(&notify_s, &update);}
                crate::timeouts::observe(&notify_s.id,&update,crate::timeouts::now_ms());
                notify_s.emit("acp",update);
            }
            Ok(())
        },agent_client_protocol::on_receive_notification!())
        .on_receive_request(async move |request:RequestPermissionRequest,responder,_cx| {
            let id=Uuid::new_v4().to_string();
            let value=serde_json::to_value(&request).unwrap_or(Value::Null);
            if !permission_sessions.lock().unwrap().contains(&request.session_id.to_string()) {
                return responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
            }
            let (tx,rx)=oneshot::channel();
            {
                let mut approvals = permission_s.approvals.lock().unwrap();
                let mut data = permission_s.data.lock().unwrap();
                if data.lifecycle.suspended || !data.turn_active || data.status == "cancelling" {
                    return responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
                }
                let approval=Approval{request:value,kind:"permission",unsupported:None,answer:tx};
                let event=approval.event(&id);
                approvals.insert(id.clone(), approval);
                data.status = "waiting".into();
                drop(data);
                // Publish before cancellation can remove this approval. Otherwise a
                // late approval event could follow approval_resolved in the browser.
                permission_s.emit("approval",event);
                permission_s.emit("status",json!({"status":"waiting"}));
            }
            let outcome=match rx.await {
                Ok(Some(Value::String(option)))=>RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option)),
                _=>RequestPermissionOutcome::Cancelled,
            };
            responder.respond(RequestPermissionResponse::new(outcome))
        },agent_client_protocol::on_receive_request!())
        .on_receive_request(async move |request:CreateElicitationRequest,responder,cx| {
            // L-02: register and publish inside the dispatch loop (ordering with session
            // updates), then wait in a task so `$/cancel_request` can still be processed.
            let id=Uuid::new_v4().to_string();
            let value=serde_json::to_value(&request).unwrap_or(Value::Null);
            if value["sessionId"].as_str().is_some_and(|id| !question_sessions.lock().unwrap().contains(id)) {
                return responder.respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
            }
            let (tx,rx)=oneshot::channel();
            {
                let mut approvals = question_s.approvals.lock().unwrap();
                let mut data = question_s.data.lock().unwrap();
                if data.lifecycle.suspended || !data.turn_active || data.status == "cancelling" {
                    return responder.respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
                }
                let unsupported=crate::elicitation::unsupported(&value);
                let approval=Approval{request:value,kind:"question",unsupported,answer:tx};
                let event=approval.event(&id);
                approvals.insert(id.clone(), approval);
                data.status = "waiting".into();
                drop(data);
                question_s.emit("approval",event);
                question_s.emit("status",json!({"status":"waiting"}));
            }
            let s=question_s.clone();
            let cancellation=responder.cancellation();
            cx.spawn(async move {
                let answer=tokio::select! {
                    answer=rx=>answer.ok().flatten(),
                    _=cancellation.cancelled()=>{crate::elicitation::withdraw(&s,&id);None},
                };
                // Turn cancellation and closed sessions resolve to `cancel`, never a guess.
                let response=answer.and_then(|v|serde_json::from_value::<CreateElicitationResponse>(v).ok())
                    .unwrap_or_else(||CreateElicitationResponse::new(ElicitationAction::Cancel));
                responder.respond(response)
            })
        },agent_client_protocol::on_receive_request!())
        .connect_with(transport,move |connection:ConnectionTo<Agent>|async move {
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
            child_sessions.lock().unwrap().set_root(provider_id.clone());
            let desired_model={
                let mut data=s.data.lock().unwrap();
                let desired=if data.provider_session_id.is_some(){data.models["currentValue"].as_str().map(str::to_owned).or_else(||s.initial_model.clone())}else{s.initial_model.clone()};
                data.provider_session_id=Some(provider_id.clone());desired
            };
            s.emit("provider_session",json!({"id":provider_id}));
            crate::session_config::publish(&s, &created);
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
                    let selected=serde_json::to_value(selected).unwrap_or(Value::Null);
                    crate::session_config::publish(&s, &selected);
                    if let Some(config)=parse_model_config(&selected,s.provider=="opencode") {model_config=config;}
                }
                s.set_models(model_config.clone());
            }
            let mut mode_config=s.data.lock().unwrap().permission_modes.clone();
            if let Some(mode)=saved_permission_modes["currentValue"].as_str() {
                if mode_config["options"].as_array().is_some_and(|items|items.iter().any(|o|o["value"]==mode)) {
                    let config=s.data.lock().unwrap().session_config.clone();
                    let selected=crate::session_config::select(&connection,&provider_id,&config,mode_config["configId"].as_str().unwrap(),json!(mode),crate::session_config::legacy_mode(&config)).await?;
                    crate::session_config::publish(&s,&selected);
                    mode_config=s.data.lock().unwrap().permission_modes.clone();
                } else {
                    return Err(agent_client_protocol::Error::internal_error().data("Saved permission mode is no longer advertised; inspect session configuration"));
                }
            } else if let Some(mode)=initial_mode {
                mode_config=crate::permission_modes::apply_initial(mode_config,if s.provider=="codex" {"Codex"} else {"Claude"},mode,|config_id,mode| {
                    let connection=&connection;let provider_id=&provider_id;let s=&s;
                    async move {
                    let config=s.data.lock().unwrap().session_config.clone();
                    let selected=crate::session_config::select(&connection,&provider_id,&config,&config_id,json!(mode),crate::session_config::legacy_mode(&config)).await?;
                    crate::session_config::publish(&s,&selected);
                    Ok(s.data.lock().unwrap().session_config.clone())
                }}).await?;
            }
            s.set_permission_modes(mode_config.clone());
            // Restore additional advertised settings after model and permission selection,
            // as those selections can change the available settings and values.
            let config=s.data.lock().unwrap().session_config.clone();
            let restored=crate::session_config::restore(&connection,&provider_id,config.clone(),&saved_config).await?;
            if restored!=config {crate::session_config::publish(&s,&restored);}
            crate::limit_resume::ready(&s);
            s.emit("ready",json!({"providerSessionId":provider_id,"configuration":serde_json::to_value(&created).unwrap_or(Value::Null)}));
            if !s.start_queued_prompt().map_err(|e|agent_client_protocol::Error::internal_error().data(e))? {s.status("idle");}
            while let Some(command)=commands.recv().await {
                match command {
                    Command::Close(reply)=>{
                        let capabilities=s.data.lock().unwrap().agent_capabilities.clone();
                        let result=agentworks_runtime::acp::close_session(&connection,&provider_id,&capabilities).await.map_err(|e|safe_error(&e.to_string()));
                        let _=reply.send(result);return Ok(());
                    },
                    Command::Cancel | Command::CancelInbox(_)=>{},
                    command @ (Command::Model(_) | Command::PermissionMode(_) | Command::Config{..})=>{
                        // Read live configuration, including agent-initiated updates.
                        let config=s.data.lock().unwrap().session_config.clone();
                        let (id,value,legacy)=match command {
                            Command::Model(model)=>{
                                let model_config=s.data.lock().unwrap().models.clone();
                                (model_config["configId"].as_str().unwrap_or("model").to_owned(),json!(model),false)
                            },
                            Command::PermissionMode(mode)=>{
                                let mode_config=s.data.lock().unwrap().permission_modes.clone();
                                (mode_config["configId"].as_str().unwrap_or("mode").to_owned(),json!(mode),crate::session_config::legacy_mode(&config))
                            },
                            Command::Config{id,value}=>(id,value,false),
                            _=>unreachable!(),
                        };
                        match crate::session_config::select(&connection,&provider_id,&config,&id,value,legacy).await {
                            Ok(selected)=>crate::session_config::publish(&s,&selected),
                            Err(error)=>{
                                // Close on timeout: the provider may have applied the change.
                                if error.to_string().contains("configuration timed out") {return Err(error);}
                                s.emit("error",json!({"message":safe_error(&error.to_string())}));
                            }
                        }
                        s.status("idle");
                    }
                    command @ (Command::Prompt(_) | Command::AttachedPrompt{..} | Command::InboxPrompt{..})=>{
                        let (text,inbox_owner,attachments)=match command {Command::Prompt(text)=>(text,None,Vec::new()),Command::AttachedPrompt{text,attachments}=>(text,None,attachments),Command::InboxPrompt{text,owner}=>(text,Some(owner),Vec::new()),_=>unreachable!()};
                        if let Some(owner)=&inbox_owner {
                            if !crate::inbox_delivery::begin(&s,owner,&provider_id.to_string()) {continue;}
                        }else{s.data.lock().unwrap().inbox_turn=None;}

                        // L-08: a forked session hands its carried transcript to the
                        // harness with this first prompt, as a separate block so the
                        // user's own message stays exactly as they typed it.
                        let carried=s.data.lock().unwrap().carried_context.take();
                        let handoff=carried.is_some();
                        let mut blocks=Vec::with_capacity(2);
                        if let Some(carried)=carried {
                            blocks.push(ContentBlock::Text(TextContent::new(carried)));
                        }
                        blocks.push(ContentBlock::Text(TextContent::new(text)));
                        // Embedded content only: the harness never receives a host path.
                        blocks.extend(attachments.into_iter().map(attachment_block));
                        // Persist the claim before dispatch. Ambiguous delivery is never replayed.
                        if handoff {
                            s.emit("carried_context",json!({"phase":"claimed","detail":"Context dispatch claimed; interrupted or failed delivery requires inspection."}));
                            if s.data.lock().unwrap().storage_error.is_some() {
                                return Err(agent_client_protocol::Error::internal_error().data("Could not persist context dispatch"));
                            }
                        }
                        let request=connection.send_request(PromptRequest::new(provider_id.clone(),blocks)).block_task();
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
                                    let outcome=match (&result,timed_out) {
                                        (_,Some(_))=>crate::input_queue::Outcome::TimedOut,
                                        (Err(_),None)=>crate::input_queue::Outcome::Error,
                                        (Ok(_),None) if cancelled=>crate::input_queue::Outcome::Cancelled,
                                        (Ok(r),None) if serde_json::to_value(&r.stop_reason).ok()==Some(json!("end_turn"))=>crate::input_queue::Outcome::Success,
                                        (Ok(_),None)=>crate::input_queue::Outcome::Cancelled,
                                    };
                                    match (result,timed_out) {
                                        // A timed-out turn is a deliberate cancellation: never schedule quota continuation.
                                        (Ok(result),Some(kind))=>{crate::limit_resume::finish(&s,Some(&json!("cancelled")),None);s.emit("turn_end",json!({"stopReason":result.stop_reason,"timedOut":kind}));},
                                        (Err(error),Some(kind))=>{crate::limit_resume::finish(&s,Some(&json!("cancelled")),None);s.emit("error",json!({"message":safe_error(&error.to_string()),"timedOut":kind}));},
                                        (Ok(result),None)=>{crate::limit_resume::finish(&s, Some(&serde_json::to_value(&result.stop_reason).unwrap_or(Value::Null)), None);s.emit("turn_end",json!({"stopReason":result.stop_reason}));},
                                        (Err(error),None)=>{let raw=serde_json::to_value(&error).unwrap_or(Value::Null);crate::limit_resume::enrich_codex(&s,&raw).await;crate::limit_resume::finish(&s,None,Some(&raw));s.emit("error",json!({"message":safe_error(&error.to_string())}));},
                                    }
                                    crate::inbox_delivery::finished(&s,inbox_owner.as_ref());
                                    crate::input_queue::record_outcome(&s,outcome);
                                    s.status("idle");
                                    break;
                                }
                                command=commands.recv()=>match command {
                                    Some(Command::Close(reply))=>{
                                        s.clear_approvals();
                                        let capabilities=s.data.lock().unwrap().agent_capabilities.clone();
                                        let result=agentworks_runtime::acp::close_session(&connection,&provider_id,&capabilities).await.map_err(|e|safe_error(&e.to_string()));
                                        let _=reply.send(result);return Ok(());
                                    },
                                    Some(command @ (Command::Cancel | Command::CancelInbox(_)))=>{
                                        if command.cancels(inbox_owner.as_ref()) && !cancelled {
                                            s.clear_approvals();
                                            connection.send_notification(CancelNotification::new(provider_id.clone()))?;
                                            s.status("cancelling");
                                            deadline.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(grace));
                                            cancelled=true;
                                        }
                                    }
                                    Some(Command::Prompt(_) | Command::AttachedPrompt{..} | Command::InboxPrompt{..})=>{},
                                    Some(Command::Model(_)|Command::PermissionMode(_)|Command::Config{..})=>{},
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

/// Attachments travel as embedded ACP content: base64 images and text resources
/// with a synthetic URI. Adapters map these to their native image/context inputs.
fn attachment_block(attachment: crate::attachments::Payload) -> ContentBlock {
    match attachment {
        crate::attachments::Payload::Image { mime, base64 } => {
            ContentBlock::Image(ImageContent::new(base64, mime))
        }
        crate::attachments::Payload::Text { uri, mime, text } => ContentBlock::Resource(
            EmbeddedResource::new(EmbeddedResourceResource::TextResourceContents(
                TextResourceContents::new(text, uri).mime_type(mime),
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn attachment_blocks_use_acp_embedded_content() {
        use crate::attachments::Payload;
        let image = super::attachment_block(Payload::Image {
            mime: "image/png".into(),
            base64: "iVBO".into(),
        });
        assert_eq!(
            serde_json::to_value(image).unwrap(),
            serde_json::json!({"type":"image","data":"iVBO","mimeType":"image/png"})
        );
        let text = super::attachment_block(Payload::Text {
            uri: "agentworks://attachments/a/notes.md".into(),
            mime: "text/markdown".into(),
            text: "# hi".into(),
        });
        assert_eq!(
            serde_json::to_value(text).unwrap(),
            serde_json::json!({"type":"resource","resource":{"uri":"agentworks://attachments/a/notes.md","mimeType":"text/markdown","text":"# hi"}})
        );
    }
    #[test]
    fn terminal_output_capabilities_and_raw_metadata_survive_v1() {
        let request = serde_json::to_value(super::initialize_request()).unwrap();
        assert_eq!(request["clientCapabilities"]["terminal"], false);
        assert_eq!(
            request["clientCapabilities"]["_meta"]["terminal_output"],
            true
        );
        assert_eq!(
            request["clientCapabilities"]["_meta"]["terminal_output_delta"],
            true
        );
        let raw = serde_json::json!({
            "sessionId":"fixture-session",
            "update":{"sessionUpdate":"tool_call_update","toolCallId":"build",
                "_meta":{"terminal_output_delta":{"terminal_id":"build","data":"\u{1b}[32m[1 / 3]\r"},
                    "terminal_exit":{"terminal_id":"build","exit_code":1,"signal":null},
                    "unknown_fixture":{"keep":true}}}
        });
        let notification: super::SessionNotification = serde_json::from_value(raw.clone()).unwrap();
        let stored = serde_json::to_value(notification.update).unwrap();
        assert_eq!(stored["_meta"], raw["update"]["_meta"]);
    }

    use super::*;
    #[test]
    fn codex_subscription_metadata_survives_typed_acp_notification() {
        // Same typed receive/serialize boundary used by on_receive_notification.
        let update = json!({
            "sessionUpdate":"usage_update", "used":1234, "size":200000,
            "_meta":{"_codex/subscriptionLimits":{"version":1,"limits":[{
                "limitId":"codex", "primary":{
                    "usedPercent":{"value":15,"observedAtMs":1792376797970_u64},
                    "resetsAt":{"value":1792385436,"observedAtMs":1792376797970_u64}
                }
            }]}}
        });
        let notification: SessionNotification = serde_json::from_value(json!({
            "sessionId":"parent", "update":update
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(notification.update).unwrap(), update);
    }
}

#[cfg(test)]
mod fork_tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    // Real ACP wire fixture: no provider binary, credentials or model calls.
    #[tokio::test]
    async fn native_fork_is_anchored_and_uncertain_creation_is_never_repeated() {
        for (reject, host_id, source_host) in [
            (false, "local", "local"),
            (true, "local", "local"),
            (false, "remote", "remote"),
            (false, "remote", "elsewhere"),
        ] {
            let dir =
                std::env::temp_dir().join(format!("agentworks-native-fork-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Arc::new(Mutex::new(
                crate::store::Store::open(&dir.join("sessions.sqlite3")).unwrap(),
            ));
            let catalog = Arc::new(Mutex::new(
                crate::Catalog::open(dir.join("catalog.json")).unwrap(),
            ));
            let mut data: crate::SessionData = serde_json::from_value(json!({
                "status":"starting","seq":0,"requests":[],"models":null,"available_commands":[],"turn_active":false,
                "lineage":{"parentId":"parent","parentProvider":"codex","parentHostId":source_host,"parentCwd":"/old","context":"auto",
                    "sourceSessionId":"source-native","sourceMessageId":"answer-1","boundarySeq":42}
            })).unwrap();
            data.carried_context = Some("recorded transcript".into());
            let (tx, _) = tokio::sync::broadcast::channel(16);
            let session = Arc::new(Session {
                id: Uuid::new_v4().to_string(),
                provider: "codex".into(),
                cwd: dir.clone(),
                title: "Child".into(),
                initial_model: None,
                created: 1,
                host_id: host_id.into(),
                location_id: "local".into(),
                data: Mutex::new(data),
                tx,
                commands: Mutex::new(None),
                store: store.clone(),
                approvals: Mutex::new(HashMap::new()),
                worker: Mutex::new(None),
                mcp_endpoint: "127.0.0.1:1".parse().unwrap(),
                mcp_token: Mutex::new("child-token".into()),
                mcp_refresh_pending: AtomicBool::new(false),
                notifications: crate::notifications::Hub::new(catalog),
                progress: crate::tool_progress::Hub::empty(),
            });
            store
                .lock()
                .unwrap()
                .insert(
                    &session.metadata(),
                    &session.data.lock().unwrap(),
                    None,
                    None,
                )
                .unwrap();
            let script = r#"
import sys,json
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); p=r.get('params',{})
 if m=='initialize': out={'protocolVersion':1,'agentCapabilities':{'sessionCapabilities':{'fork':{},'resume':{}}},'authMethods':[]}
 elif m=='session/fork':
  assert p['sessionId']=='source-native'
  assert p['_meta']['jetbrains']['air']['fork']['messageId']=='answer-1'
  if sys.argv[2]=='local': assert p['mcpServers'][0]['env'][1]['value']=='child-token'
  else: assert p.get('mcpServers',[])==[]
  if sys.argv[1]=='reject':
   print(json.dumps({'jsonrpc':'2.0','id':r['id'],'error':{'code':-32603,'message':'ambiguous fork failure'}}),flush=True);continue
  out={'sessionId':'child-native'}
 elif m=='session/new':
  assert sys.argv[2]!=sys.argv[3];out={'sessionId':'fresh-native'}
 elif m=='session/resume':
  if sys.argv[2]!='local': assert p.get('mcpServers',[])==[]
  assert p['sessionId']=='child-native';out={}
 else: raise Exception('Unexpected method '+str(m))
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
"#;
            let agent = AcpAgent::new(
                AcpAgentConfig::new("python3")
                    .arg("-c")
                    .arg(script)
                    .arg(if reject { "reject" } else { "success" })
                    .arg(host_id)
                    .arg(source_host),
            );
            let s = session.clone();
            Client
                .builder()
                .connect_with(agent, move |connection: ConnectionTo<Agent>| async move {
                    let first = open_session(&connection, &s).await;
                    assert_eq!(first.is_err(), reject);
                    assert_eq!(
                        s.data.lock().unwrap().lineage.native_attempted,
                        host_id == source_host
                    );
                    if host_id != source_host {
                        assert_eq!(first.unwrap()["sessionId"], "fresh-native");
                        assert!(!s.data.lock().unwrap().lineage.native_used);
                    } else if reject {
                        let error = open_session(&connection, &s).await.unwrap_err();
                        assert!(error.to_string().contains("uncertain"));
                        assert_eq!(
                            s.data.lock().unwrap().carried_context.as_deref(),
                            Some("recorded transcript")
                        );
                    } else {
                        assert_eq!(first.unwrap()["sessionId"], "child-native");
                        assert!(s.data.lock().unwrap().lineage.native_used);
                        assert_eq!(
                            s.data.lock().unwrap().provider_session_id.as_deref(),
                            Some("child-native")
                        );
                        // A reconnect resumes the durable child, without creating another fork.
                        open_session(&connection, &s).await.unwrap();
                    }
                    Ok::<_, agent_client_protocol::Error>(())
                })
                .await
                .unwrap();
            let loaded = store.lock().unwrap().load().unwrap();
            assert_eq!(loaded[0].1.lineage.native_attempted, host_id == source_host);
            assert_eq!(
                loaded[0].1.lineage.native_used,
                !reject && host_id == source_host
            );
            drop(session);
            drop(store);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

/// Uses the same execution host and adapter profile as the original session.
/// No native resume/new/prompt occurs on this management connection.
pub async fn delete_provider_history(
    s: &Session,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if s.data.lock().unwrap().provider_history_deleted {
        return Ok(());
    }
    let (id, host_profile) = {
        let data = s.data.lock().unwrap();
        (
            data.provider_session_id.clone().ok_or(
                "No provider session ID was recorded; the Agentworks transcript was retained",
            )?,
            data.opencode_host,
        )
    };
    s.data.lock().unwrap().provider_history_delete_pending = true;
    s.emit(
        "provider_history_delete_requested",
        json!({"providerSessionId":id}),
    );
    if s.data.lock().unwrap().storage_error.is_some() {
        return Err(
            "Could not persist provider-history deletion intent; no deletion request was sent"
                .into(),
        );
    }
    let (transport, _guard): (DynConnectTo<Client>, _) = if s.host_id == "local" {
        prepare(&s.provider)?;
        (
            DynConnectTo::new(agent_for(&s.provider, host_profile)),
            None,
        )
    } else {
        let host = crate::hosts::registry()
            .and_then(|hosts| hosts.live(&s.host_id))
            .ok_or("The remote host is not connected; no history was deleted")?;
        let (lines, guard) = host.spawn(&s.provider, &s.cwd.to_string_lossy(), host_profile)?;
        (DynConnectTo::new(lines), Some(guard))
    };
    agentworks_runtime::acp::delete_session(
        transport,
        &s.provider,
        &id,
        s.host_id == "local" && use_api_key(),
    )
    .await?;
    {
        let mut data = s.data.lock().unwrap();
        data.provider_history_deleted = true;
        data.provider_history_delete_pending = false;
    }
    s.emit("provider_history_deleted", json!({"providerSessionId":id}));
    Ok(())
}
