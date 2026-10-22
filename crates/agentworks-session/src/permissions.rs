use serde::{Deserialize, Serialize};
/// Claude's normal Auto mode, never bypassPermissions/full access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeDefault {
    #[default]
    Auto,
    Default,
    AcceptEdits,
    Plan,
}
impl ClaudeDefault {
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
        }
    }
}

/// ACP mode IDs from the pinned Codex adapter (not raw sandbox policy names).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum CodexDefault {
    #[default]
    #[serde(rename = "agent-full-access")]
    FullAccess,
    #[serde(rename = "read-only")]
    ReadOnly,
    #[serde(rename = "workspace-write")]
    WorkspaceWrite,
    #[serde(rename = "agent")]
    Agent,
}
impl CodexDefault {
    pub fn id(self) -> &'static str {
        match self {
            Self::FullAccess => "agent-full-access",
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::Agent => "agent",
        }
    }
}
