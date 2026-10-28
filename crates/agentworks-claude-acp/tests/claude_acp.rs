//! Exercise the bridge through the same typed ACP SDK the application uses.
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, ContentBlock, CreateElicitationRequest,
    CreateElicitationResponse, DeleteSessionRequest, ForkSessionRequest, InitializeRequest,
    ListSessionsRequest, LoadSessionRequest, NewSessionRequest, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigOptionValue, SessionNotification,
    SetSessionConfigOptionRequest, StopReason,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::{path::PathBuf, time::Duration};

#[tokio::test]
async fn native_bridge_works_with_typed_acp_client() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config_dir =
        std::env::temp_dir().join(format!("agentworks-claude-acp-{}", std::process::id()));
    let cwd = config_dir.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = AcpAgentConfig::new(env!("CARGO_BIN_EXE_agentworks-claude-acp"))
        .env(
            "CLAUDE_CODE_EXECUTABLE",
            root.join("tests/claude-acp.test.py").to_string_lossy(),
        )
        .env("CLAUDE_CONFIG_DIR", config_dir.to_string_lossy());
    let updates = Arc::new(Mutex::new(Vec::new()));
    let seen = updates.clone();
    tokio::time::timeout(
        Duration::from_secs(10),
        Client
            .builder()
            .on_receive_notification(
                async move |n: SessionNotification, _cx| {
                    seen.lock().unwrap().push(serde_json::to_value(n.update)?);
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async |p: RequestPermissionRequest, responder, _cx| {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            p.options[0].option_id.clone(),
                        )),
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async |_: CreateElicitationRequest, responder, _cx| {
                    responder.respond(serde_json::from_value::<CreateElicitationResponse>(
                        json!({"action":"accept","content":{"question_0":"Blue"}}),
                    )?)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(
                AcpAgent::new(config),
                move |cx: ConnectionTo<Agent>| async move {
                    let init: InitializeRequest=serde_json::from_value(json!({"protocolVersion":1,"clientCapabilities":{"subagents":{},"elicitation":{"form":{}},"_meta":{"terminal_output":true,"terminal_output_delta":true}}}))?;
                    cx.send_request(init).block_task().await?;
                    let session = cx.send_request(NewSessionRequest::new(cwd.clone())).block_task().await?;
                    let id = session.session_id.clone();
                    let configured = cx.send_request(SetSessionConfigOptionRequest::new(id.clone(), "mode", SessionConfigOptionValue::value_id("plan"))).block_task().await?;
                    assert!(serde_json::to_value(configured)?["configOptions"].as_array().unwrap().iter().any(|o| o["id"] == "mode" && o["currentValue"] == "plan"));
                    for text in ["hello", "bash", "question", "plan", "tasks", "unfinished", "agent", "/usage", "/rename Parser work"] {
                        let response = cx.send_request(PromptRequest::new(id.clone(), vec![ContentBlock::from(text)])).block_task().await?;
                        assert_eq!(response.stop_reason, StopReason::EndTurn, "{text}");
                    }
                    cx.send_notification(CancelNotification::new(id.clone()))?;
                    // The fake CLI persists prompts like Claude Code, so the session resumes.
                    cx.send_request(CloseSessionRequest::new(id.clone())).block_task().await?;
                    cx.send_request(ResumeSessionRequest::new(id.clone(), cwd.clone())).block_task().await?;
                    let listed = cx.send_request(ListSessionsRequest::new()).block_task().await?;
                    assert!(listed.sessions.iter().any(|s| s.session_id == id));
                    cx.send_request(CloseSessionRequest::new(id.clone())).block_task().await?;
                    cx.send_request(LoadSessionRequest::new(id.clone(), cwd.clone())).block_task().await?;
                    let fork = cx.send_request(ForkSessionRequest::new(id.clone(), cwd.clone())).block_task().await?;
                    assert_ne!(fork.session_id, id);
                    cx.send_request(DeleteSessionRequest::new(fork.session_id)).block_task().await?;
                    cx.send_request(DeleteSessionRequest::new(id)).block_task().await?;
                    Ok(())
                },
            ),
    )
    .await
    .expect("bridge timed out")
    .unwrap();
    let kinds: Vec<_> = updates
        .lock()
        .unwrap()
        .iter()
        .map(|u| u["sessionUpdate"].as_str().unwrap().to_owned())
        .collect();
    for kind in [
        "agent_message_chunk",
        "tool_call",
        "tool_call_update",
        "plan",
        "usage_update",
        "available_commands_update",
        "current_mode_update",
        "config_option_update",
        "user_message_chunk",
        "subagent_update",
        "session_info_update",
    ] {
        assert!(
            kinds.iter().any(|k| k == kind),
            "{kind} did not parse as a typed update"
        );
    }
    assert!(
        updates
            .lock()
            .unwrap()
            .iter()
            .any(|u| u["sessionUpdate"] == "session_info_update" && u["title"] == "Parser work")
    );
    let _ = std::fs::remove_dir_all(config_dir);
}
