//! Optional AIR asyncTasks extension shared by Claude and Codex adapters.
use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "sessionUpdate",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Update {
    AsyncTaskSpawned {
        async_task_id: String,
        name: String,
        #[serde(flatten)]
        fields: Map<String, Value>,
    },
    AsyncTaskProgress {
        async_task_id: String,
        #[serde(flatten)]
        fields: Map<String, Value>,
    },
    AsyncTaskStateUpdate {
        async_task_id: String,
        state: String,
        #[serde(flatten)]
        fields: Map<String, Value>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "_session/async_task/stop", response = StopResponse)]
#[serde(rename_all = "camelCase")]
pub struct StopRequest {
    pub session_id: String,
    pub async_task_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonRpcResponse)]
pub struct StopResponse {
    pub stopped: bool,
}

pub fn supported(initialize: &Value) -> bool {
    let air = &initialize["_meta"]["jetbrains"]["air"];
    air["version"].as_u64().is_some_and(|v| v >= 1)
        && air["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|v| v == "asyncTasks"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn negotiates_only_the_named_versioned_extension_and_routes_job_updates() {
        let request = serde_json::to_value(crate::acp::initialize_request()).unwrap();
        assert_eq!(
            request["clientCapabilities"]["_meta"]["jetbrains"]["air"],
            json!({"version":1,"capabilities":["asyncTasks"]})
        );
        assert!(supported(
            &json!({"_meta":request["clientCapabilities"]["_meta"]})
        ));
        assert!(!supported(
            &json!({"_meta":{"jetbrains":{"air":{"version":0,"capabilities":["asyncTasks"]}}}})
        ));
        assert!(!supported(&json!({})));
        let mut sessions = crate::subagents::Sessions::new(Some("root".into()));
        for kind in [
            "async_task_spawned",
            "async_task_progress",
            "async_task_state_update",
        ] {
            let update = json!({"sessionUpdate":kind,"asyncTaskId":"job","name":"Build","state":"completed","usage":{"totalTokens":5}});
            let routed = sessions
                .route(serde_json::from_value(json!({"sessionId":"root","update":update})).unwrap())
                .unwrap()
                .0;
            assert_eq!(routed["usage"], update["usage"]);
            assert!(
                sessions
                    .route(
                        serde_json::from_value(json!({"sessionId":"foreign","update":update}))
                            .unwrap()
                    )
                    .is_none()
            );
        }
    }
}
