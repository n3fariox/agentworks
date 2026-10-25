//! Human controls for adapter-owned background jobs. Never starts a harness or a turn.
use crate::{ApiResult, App, fail, session, session_runtime::Command};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::oneshot;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub async_task_id: String,
    pub acp_session_id: String,
}

pub async fn stop(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(target): Json<Target>,
) -> ApiResult {
    let s = session(&app, &id)?;
    if !s
        .data
        .lock()
        .unwrap()
        .async_tasks
        .can_stop(&target.async_task_id, &target.acp_session_id)
    {
        return Err(fail(
            StatusCode::CONFLICT,
            "This background job is no longer confirmed running or cannot be stopped",
        ));
    }
    let (reply, result) = oneshot::channel();
    s.send_command(Command::StopAsyncTask {
        id: target.async_task_id,
        owner: target.acp_session_id,
        reply,
    })?;
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(15), result)
        .await
        .map_err(|_| {
            fail(
                StatusCode::GATEWAY_TIMEOUT,
                "Job stop timed out; inspect its status before retrying",
            )
        })?
        .map_err(|_| fail(StatusCode::GONE, "Agent process unavailable"))?
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({"stopped":stopped})))
}

pub async fn dispatch(
    connection: &agent_client_protocol::ConnectionTo<agent_client_protocol::Agent>,
    s: &Arc<crate::Session>,
    provider_id: &str,
    id: String,
    owner: String,
    reply: oneshot::Sender<Result<bool, String>>,
) {
    // Recheck after queueing: completion/disconnect may have won the race.
    if !s.data.lock().unwrap().async_tasks.can_stop(&id, &owner) {
        let _ = reply.send(Ok(false));
        return;
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        connection
            .send_request(agentworks_runtime::async_tasks::StopRequest {
                session_id: provider_id.to_owned(),
                async_task_id: id,
            })
            .block_task(),
    )
    .await;
    let result = match result {
        Ok(Ok(response)) => Ok(response.stopped),
        Ok(Err(error)) => Err(agentworks_runtime::provider::safe_error(&error.to_string())),
        Err(_) => Err("Job stop timed out; its outcome is unknown".into()),
    };
    let _ = reply.send(result);
}

/// Also runs on cancellation of the runtime future and on initialization failure.
pub struct Connection(pub Arc<crate::Session>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0
            .emit("async_task_connection", json!({"supported":false}));
    }
}
