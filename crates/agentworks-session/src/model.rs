//! Persisted session payload. Serde defaults and field names are kept compatible with existing databases.
use crate::{
    continuation,
    lifecycle::Metadata,
    lineage::Lineage,
    origins::{AutomationOrigin, ControlOrigin, InboxTurn},
    permissions::{ClaudeDefault, CodexDefault},
    queue, timeouts,
};
use agentworks_workspace::cleanup::Owned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub status: String,
    #[serde(default)]
    pub inbox_turn: Option<InboxTurn>,
    #[serde(default)]
    pub lifecycle: Metadata,
    pub seq: u64,
    #[serde(skip)]
    pub events: VecDeque<Value>,
    #[serde(skip)]
    pub bytes: usize,
    pub requests: HashSet<String>,
    /// Most recent submitted prompt survives the bounded in-memory event window.
    #[serde(default)]
    pub last_prompt: Option<Value>,
    pub models: Value,
    #[serde(default)]
    pub permission_modes: Value,
    #[serde(default)]
    pub session_config: Value,
    #[serde(default)]
    pub agent_capabilities: Value,
    #[serde(default)]
    pub provider_history_deleted: bool,
    #[serde(default)]
    pub provider_history_delete_pending: bool,
    // Captured at creation, never inferred for restored/imported sessions.
    // A native session ID prevents this default from overriding reconnect state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_claude_permission_mode: Option<ClaudeDefault>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_codex_permission_mode: Option<CodexDefault>,
    pub available_commands: Value,
    pub turn_active: bool,
    pub project_id: Option<String>,
    pub provider_session_id: Option<String>,
    #[serde(default)]
    pub external: bool,
    #[serde(default)]
    pub import_pending: bool,
    #[serde(default)]
    pub opencode_host: bool,
    #[serde(default)]
    pub automation: Option<AutomationOrigin>,
    #[serde(default)]
    pub queued_prompt: Option<String>,
    #[serde(default)]
    pub control_origin: Option<ControlOrigin>,
    #[serde(skip)]
    pub storage_error: Option<String>,
    #[serde(default)]
    pub continuation: continuation::State,
    /// L-07 per-session override; None uses the saved default at turn launch.
    #[serde(default)]
    pub timeouts: Option<timeouts::Policy>,
    /// L-05 explicit human queued inputs (distinct from `queued_prompt`).
    #[serde(default)]
    pub input_queue: queue::Queue,
    /// L-08 fork lineage; empty for a session that was not forked.
    #[serde(default)]
    pub lineage: Lineage,
    /// L-08 fork context held until the first prompt hands it to the harness.
    #[serde(default)]
    pub carried_context: Option<String>,
    /// ACP `promptCapabilities` advertised at the last initialization.
    #[serde(default)]
    pub prompt_capabilities: Value,
    /// C-04b; present only when Agentworks created this session's worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Owned>,
}
