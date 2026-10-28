//! ACP negotiation and read-only discovery on local or remote transports.
use crate::provider::parse_model_config;
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AuthenticateRequest, BooleanConfigOptionCapabilities, ClientCapabilities,
        ClientSessionCapabilities, ElicitationCapabilities, ElicitationFormCapabilities,
        InitializeRequest, ListSessionsRequest, NewSessionRequest, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse, SessionConfigOptionsCapabilities,
        SubagentCapabilities,
    },
};
use agent_client_protocol::{Agent, Client, ConnectionTo, DynConnectTo};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
pub fn initialize_request() -> InitializeRequest {
    // Receive adapter-owned output only. This does not advertise terminal/* process hosting.
    // Support ACP forms, but not URL elicitations that could send users to arbitrary sites.
    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
        ClientCapabilities::new()
            .session(
                ClientSessionCapabilities::new().config_options(
                    SessionConfigOptionsCapabilities::new()
                        .boolean(BooleanConfigOptionCapabilities::new()),
                ),
            )
            .subagents(SubagentCapabilities::new())
            .elicitation(ElicitationCapabilities::new().form(ElicitationFormCapabilities::new()))
            .meta(
                json!({"terminal_output": true, "terminal_output_delta": true,
                    "jetbrains":{"air":{"version":1,"capabilities":["asyncTasks"]}}})
                .as_object()
                .unwrap()
                .clone(),
            ),
    )
}

pub async fn initialize_on(
    connection: &ConnectionTo<Agent>,
    provider: &str,
    api_key: bool,
) -> Result<Value, agent_client_protocol::Error> {
    let init = connection
        .send_request(initialize_request())
        .block_task()
        .await?;
    if init.protocol_version != ProtocolVersion::V1 {
        return Err(
            agent_client_protocol::Error::internal_error().data("Adapter did not negotiate ACP v1")
        );
    }
    if crate::provider::cli_provider(provider) == "codex"
        && api_key
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
async fn discovery_config(
    connection: &ConnectionTo<Agent>,
    provider: &str,
    cwd: PathBuf,
    api_key: bool,
) -> Result<(Value, Value), agent_client_protocol::Error> {
    let init = initialize_on(connection, provider, api_key).await?;
    // The Rust Codex bridge can discover models without creating a native thread.
    // Only use the extension when advertised, including on remote workers.
    if init["_meta"]["agentworks"]["modelDiscovery"] == true {
        let config = connection
            .send_request(agent_client_protocol::UntypedMessage {
                method: "_agentworks/models/list".into(),
                params: json!({}),
            })
            .block_task()
            .await?;
        return Ok((config, Value::Null));
    }
    let created = connection
        .send_request(NewSessionRequest::new(cwd))
        .block_task()
        .await?;
    Ok((
        serde_json::to_value(created).unwrap_or(Value::Null),
        init["agentCapabilities"].clone(),
    ))
}
/// Ask a selected ACP transport for its model list without sending a prompt.
pub async fn discover(
    transport: DynConnectTo<Client>,
    provider: &str,
    cwd: PathBuf,
    api_key: bool,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
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
                transport,
                move |connection: ConnectionTo<Agent>| async move {
                    let (created, capabilities) =
                        discovery_config(&connection, &provider, cwd, api_key).await?;
                    let config = parse_model_config(&created, free_only);
                    if capabilities["sessionCapabilities"]["close"].is_object() {
                        if let Some(id) = created["sessionId"].as_str() {
                            let _ = close_session(&connection, id, &capabilities).await;
                        }
                    }
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

/// List provider history through a selected ACP transport.
pub async fn list_sessions(
    transport: DynConnectTo<Client>,
    provider: &str,
    cwd: Option<PathBuf>,
    cursor: Option<String>,
    api_key: bool,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let provider = provider.to_owned();
    let result = tokio::time::timeout(
        Duration::from_secs(45),
        Client.builder().connect_with(
            transport,
            move |connection: ConnectionTo<Agent>| async move {
                let init = initialize_on(&connection, &provider, api_key).await?;
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

pub async fn probe(provider: &str) -> Result<(), Box<dyn std::error::Error>> {
    crate::provider::prepare(provider)?;
    let name = provider.to_string();
    let result=tokio::time::timeout(Duration::from_secs(30),Client.builder()
        .connect_with(agent_client_protocol::AcpAgent::new(crate::provider::agent_config(provider, crate::provider::use_api_key(), false)), move |connection:ConnectionTo<Agent>|async move {
            let init=connection.send_request(initialize_request()).block_task().await?;
            println!("{}",json!({"provider":name,"protocolVersion":init.protocol_version,"agentInfo":init.agent_info,
                "capabilities":init.agent_capabilities,"authMethods":init.auth_methods}));
            Ok(())
        })).await;
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(crate::provider::safe_error(&e.to_string()).into()),
        Err(_) => Err("ACP initialization timed out after 30 seconds".into()),
    }
}

/// Gracefully release a live session when the agent advertises the operation.
/// Closing never deletes the provider's durable conversation.
pub async fn close_session(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    capabilities: &Value,
) -> Result<(), agent_client_protocol::Error> {
    if !capabilities["sessionCapabilities"]["close"].is_object() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("This harness does not advertise graceful session close"));
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        connection
            .send_request(agent_client_protocol::schema::v1::CloseSessionRequest::new(
                session_id.to_owned(),
            ))
            .block_task(),
    )
    .await
    .map_err(|_| {
        agent_client_protocol::Error::internal_error().data("Graceful session close timed out")
    })??;
    Ok(())
}

/// Delete a specific durable provider session without loading or prompting it.
/// Callers must stop any adapter that may still write to this session first.
pub async fn delete_session(
    transport: DynConnectTo<Client>,
    provider: &str,
    session_id: &str,
    api_key: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let provider = provider.to_owned();
    let session_id = session_id.to_owned();
    tokio::time::timeout(Duration::from_secs(45), Client.builder().connect_with(
        transport, move |connection: ConnectionTo<Agent>| async move {
            let init = initialize_on(&connection, &provider, api_key).await?;
            if !init["agentCapabilities"]["sessionCapabilities"]["delete"].is_object() {
                return Err(agent_client_protocol::Error::invalid_params().data("This harness does not advertise provider-history deletion; the Agentworks transcript was retained"));
            }
            match connection.send_request(agent_client_protocol::schema::v1::DeleteSessionRequest::new(session_id)).block_task().await {
                Ok(_) => Ok(()),
                // An explicit retry after an uncertain response can find that
                // the first request already removed the requested history.
                Err(error) if error.code == agent_client_protocol::schema::v1::ErrorCode::ResourceNotFound => Ok(()),
                Err(error) => Err(error),
            }
        }
    )).await.map_err(|_| "Provider-history deletion timed out; its outcome is uncertain. The Agentworks transcript was retained; inspect provider history before retrying")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{AcpAgent, AcpAgentConfig};

    #[tokio::test]
    async fn discovery_negotiates_threadless_listing_and_keeps_legacy_fallback() {
        for mode in ["extension", "legacy", "error"] {
            let script = r#"
import json,sys
mode=sys.argv[1]
for line in sys.stdin:
 r=json.loads(line); m=r['method']
 if m=='initialize':
  out={'protocolVersion':1,'agentCapabilities':{'sessionCapabilities':{'close':{}}},'authMethods':[]}
  if mode!='legacy':out['_meta']={'agentworks':{'modelDiscovery':True}}
 elif m=='session/close':
  assert mode=='legacy';out={}
 elif m in ('session/new','_agentworks/models/list'):
  assert m==('session/new' if mode=='legacy' else '_agentworks/models/list'), 'Discovery tried to create a thread'
  if mode=='error':
   print(json.dumps({'jsonrpc':'2.0','id':r['id'],'error':{'code':-32603,'message':'Model listing refused'}}),flush=True);continue
  out={'configOptions':[{'id':'model','name':'Model','category':'model','type':'select','currentValue':'test-model','options':[{'value':'test-model','name':'Test'}]}]}
  if mode=='legacy':out['sessionId']='temporary'
 else:raise Exception('Unexpected method '+m)
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':out}),flush=True)
"#;
            let transport = DynConnectTo::new(AcpAgent::new(
                AcpAgentConfig::new("python3")
                    .arg("-c")
                    .arg(script)
                    .arg(mode),
            ));
            let result = discover(transport, "codex-rust", std::env::temp_dir(), false).await;
            if mode == "error" {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Model listing refused")
                );
            } else {
                assert_eq!(result.unwrap()["options"][0]["value"], "test-model");
            }
        }
    }
}
