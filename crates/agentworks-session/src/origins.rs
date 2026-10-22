//! Persisted launch and submission identities, independent of live application objects.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationOrigin {
    pub automation_id: String,
    pub name: String,
    pub run_id: String,
    pub request_id: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub initiated_by: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ControlOrigin {
    pub actor: String,
    pub request: String,
    #[serde(default)]
    pub payload: Value,
    /// C-01 display name for external controllers, which are not sessions.
    #[serde(
        default,
        rename = "actorTitle",
        skip_serializing_if = "Option::is_none"
    )]
    pub actor_title: Option<String>,
}
/// Persisted ownership of one particular native inbox submission, not an authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxTurn {
    pub request_id: String,
    pub generation: u64,
    pub native: String,
}
