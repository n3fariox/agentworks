use serde::{Deserialize, Serialize};
/// Recorded on the child and kept in its session state, so lineage survives restarts.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Lineage {
    pub parent_id: Option<String>,
    pub parent_title: Option<String>,
    pub parent_provider: Option<String>,
    pub parent_host_id: Option<String>,
    /// Parent event sequence the fork was taken at.
    pub boundary_seq: u64,
    /// Parent turns that had finished at or before the boundary.
    pub boundary_turn: usize,
    /// Parent working directory when the fork was taken; the child may run elsewhere.
    pub parent_cwd: Option<String>,
    pub created: u64,
    pub context: String,
    pub source_session_id: Option<String>,
    pub source_message_id: Option<String>,
    pub native_attempted: bool,
    pub native_used: bool,
    pub truncated: bool,
}
