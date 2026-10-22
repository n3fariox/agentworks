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
