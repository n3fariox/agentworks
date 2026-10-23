//! Exercise the bridge through the same typed ACP SDK the application uses.
use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, CreateElicitationRequest, CreateElicitationResponse,
    NewSessionRequest, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, StopReason,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[tokio::test]
async fn native_bridge_works_with_typed_acp_client() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = AcpAgentConfig::new(env!("CARGO_BIN_EXE_agentworks-codex-acp")).env(
        "CODEX_PATH",
        root.join("tests/codex-acp.test.py").to_string_lossy(),
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        Client
            .builder()
            .on_receive_notification(
                async |_: SessionNotification, _cx| Ok(()),
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
                    agentworks_runtime::acp::initialize_on(&cx, "codex", false).await?;
                    let session = cx
                        .send_request(NewSessionRequest::new(root))
                        .block_task()
                        .await?;
                    assert!(!session.config_options.as_ref().unwrap().is_empty());
                    for text in ["early", "normal", "approval", "question"] {
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
                            session.session_id,
                            vec![ContentBlock::from("normal")],
                        ))
                        .block_task()
                        .await?;
                    assert_eq!(response.stop_reason, StopReason::EndTurn);
                    Ok(())
                },
            ),
    )
    .await
    .expect("bridge timed out")
    .unwrap();
}
