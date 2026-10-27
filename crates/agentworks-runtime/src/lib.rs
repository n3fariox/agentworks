//! Provider launch configuration, ACP discovery, and SSH worker execution.
pub mod acp;
pub mod mcp_relay;
pub mod provider;
pub mod remote;
pub mod worker;

use agentworks_terminal as terminal;
use agentworks_workspace::{cleanup as worktree_cleanup, worktrees};

pub mod async_tasks;
pub mod model_providers;
pub mod subagents;
