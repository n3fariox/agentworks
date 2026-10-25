//! Exercise the bridge through the same typed ACP SDK the application uses.
use agent_client_protocol::schema::v1::{
    CancelNotification, CloseSessionRequest, ContentBlock, CreateElicitationRequest,
    CreateElicitationResponse, DeleteSessionRequest, ForkSessionRequest, InitializeRequest,
    LoadSessionRequest, NewSessionRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigOptionValue, SessionNotification, SetSessionConfigOptionRequest, StopReason,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test]
async fn native_bridge_works_with_typed_acp_client() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let updates = Arc::new(Mutex::new(Vec::new()));
    let received = updates.clone();
    let config = AcpAgentConfig::new(env!("CARGO_BIN_EXE_agentworks-codex-acp")).env(
        "CODEX_PATH",
        root.join("tests/codex-acp.test.py").to_string_lossy(),
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        Client
            .builder()
            .on_receive_notification(
                async move |p: SessionNotification, _cx| {
                    received.lock().unwrap().push(serde_json::to_value(p)?);
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
                        json!({"action":"accept","content":{"color":"Blue"}}),
                    )?)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(
                AcpAgent::new(config),
                move |cx: ConnectionTo<Agent>| async move {
                    let init: InitializeRequest=serde_json::from_value(json!({"protocolVersion":1,"clientCapabilities":{"subagents":{},"session":{"configOptions":{"boolean":{}}},"elicitation":{"form":{}}}}))?;
                    let initialized=cx.send_request(init).block_task().await?;
                    assert!(serde_json::to_value(initialized)?["agentCapabilities"]["sessionCapabilities"]["close"].is_object());
                    let session = cx
                        .send_request(NewSessionRequest::new(root.clone()))
                        .block_task()
                        .await?;
                    assert!(!session.config_options.as_ref().unwrap().is_empty());
                    let configured=cx.send_request(SetSessionConfigOptionRequest::new(session.session_id.clone(),"fast-mode",SessionConfigOptionValue::boolean(true))).block_task().await?;
                    assert!(serde_json::to_value(configured)?["configOptions"].as_array().unwrap().iter().any(|o|o["id"]=="fast-mode" && o["currentValue"]==true));
                    for text in ["early", "normal", "approval", "question", "subagents", "/status", "/skills", "/compact", "/review"] {
                        let response = cx
                            .send_request(PromptRequest::new(
                                session.session_id.clone(),
                                vec![ContentBlock::from(text)],
                            ))
                            .block_task()
                            .await?;
                        assert_eq!(response.stop_reason, StopReason::EndTurn);
                    }
                    // A cancellation while idle must not poison the next turn.
                    cx.send_notification(CancelNotification::new(session.session_id.clone()))?;
                    let response = cx
                        .send_request(PromptRequest::new(
                            session.session_id.clone(),
                            vec![ContentBlock::from("normal")],
                        ))
                        .block_task()
                        .await?;
                    assert_eq!(response.stop_reason, StopReason::EndTurn);
                    cx.send_request(CloseSessionRequest::new(session.session_id.clone())).block_task().await?;
                    cx.send_request(LoadSessionRequest::new(session.session_id.clone(),root.clone())).block_task().await?;
                    let fork=cx.send_request(ForkSessionRequest::new(session.session_id.clone(),root)).block_task().await?;
                    assert_ne!(fork.session_id,session.session_id);
                    cx.send_request(DeleteSessionRequest::new(fork.session_id)).block_task().await?;
                    cx.send_request(DeleteSessionRequest::new(session.session_id)).block_task().await?;
                    Ok(())
                },
            ),
    )
    .await
    .expect("bridge timed out")
    .unwrap();
    let updates = updates.lock().unwrap();
    assert!(updates.iter().any(|p| p["sessionId"] == "child-a"
        && p["update"]["sessionUpdate"] == "subagent_update"
        && p["update"]["sessionId"] == "child-b"));
    assert!(updates.iter().any(|p| p["sessionId"] == "child-a"
        && p["update"]["sessionUpdate"] == "subagent_update"
        && p["update"]["sessionId"] == "child-b"
        && p["update"]["state"]["stopReason"] == "end_turn"));
}
