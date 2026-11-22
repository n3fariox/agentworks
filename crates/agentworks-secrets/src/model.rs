//! Secret names and values, and how a secret is granted to a session.
use serde::{Deserialize, Serialize};

pub const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTION: usize = 200;
/// Variables Agentworks or the harnesses set themselves; a secret must never replace them.
const RESERVED: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDECODE",
    "CLAUDE_CONFIG_DIR",
    "CODEX_API_KEY",
    "CODEX_HOME",
    "CODEX_PATH",
    "DEFAULT_AUTH_REQUEST",
    "DYLD_INSERT_LIBRARIES",
    "HOME",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "NO_BROWSER",
    "OPENAI_API_KEY",
    "OPENAI_BASE_PATH",
    "OPENAI_BASE_URL",
    "OPENAI_CUSTOM_HEADERS",
    "OPENAI_HOST",
    "OPENAI_ORGANIZATION",
    "OPENAI_PROJECT",
    "PATH",
    "PWD",
    "SHELL",
    "TERM",
    "USER",
];
const RESERVED_PREFIXES: &[&str] = &["AGENTWORKS_", "CLAUDE_CODE_", "GOOSE_", "XDG_"];

/// Secret names are environment variable names that Agentworks does not manage.
pub fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = name.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(
            "Use an environment variable name: A–Z, 0–9 and _, not starting with a digit".into(),
        );
    }
    if RESERVED.contains(&name) || RESERVED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(format!(
            "{name} is set by Agentworks or a harness and cannot be a secret"
        ));
    }
    Ok(())
}
pub fn validate_value(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("Enter a value".into());
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err("Values are limited to 64 KiB".into());
    }
    if value.contains('\0') {
        return Err("Values cannot contain NUL characters".into());
    }
    Ok(())
}
/// The trimmed description.
pub fn validate_description(description: &str) -> Result<String, String> {
    let description = description.trim();
    if description.chars().count() > MAX_DESCRIPTION {
        return Err("Descriptions are limited to 200 characters".into());
    }
    Ok(description.to_owned())
}

/// How an assigned secret reaches the session.
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Delivery {
    /// Harness environment variable, also usable by the tool.
    Environment,
    #[default]
    Tool,
}
/// Whether the session may pass the secret to sessions it creates.
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Delegation {
    #[default]
    Restricted,
    Passable,
}
/// Whether each tool use asks the human first.
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Approval {
    #[default]
    None,
    EachUse,
}

/// One secret granted to one session. `origin` is `human` or the delegating session's ID.
#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub secret_id: String,
    pub name: String,
    pub delivery: Delivery,
    pub delegation: Delegation,
    pub approval: Approval,
    pub origin: String,
}
impl Assignment {
    /// A one-line summary for the transcript and the audit log, e.g. `NAME (tool, passable)`.
    pub fn describe(&self) -> String {
        let mut parts = vec![if self.delivery == Delivery::Environment {
            "environment"
        } else {
            "tool"
        }];
        if self.delegation == Delegation::Passable {
            parts.push("passable");
        }
        if self.approval == Approval::EachUse {
            parts.push("ask each use");
        }
        format!("{} ({})", self.name, parts.join(", "))
    }
    /// How the agent uses it; `in_environment` is whether the running harness has it.
    pub fn usage(&self, in_environment: bool) -> String {
        let tool = format!(
            "Use run_with_secrets with {{{{secret:{}}}}} in argv, env, stdin or files.",
            self.name
        );
        let mut usage = match self.delivery {
            Delivery::Environment if in_environment => format!(
                "Available as ${} in commands you run. run_with_secrets also works.",
                self.name
            ),
            Delivery::Environment => format!(
                "Assigned as ${} but not in your environment until the harness restarts. {tool}",
                self.name
            ),
            Delivery::Tool => tool,
        };
        if self.approval == Approval::EachUse {
            usage.push_str(" The user is asked before each run_with_secrets call.");
        }
        usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_environment_rules_and_avoid_managed_variables() {
        for name in ["GITHUB_TOKEN", "_X", "NPM_TOKEN_2"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        for name in [
            "",
            "github_token",
            "2FA",
            "A-B",
            "PATH",
            "OPENAI_API_KEY",
            "GOOSE_MODEL",
            "AGENTWORKS_PORT",
            "CLAUDE_CODE_X",
            "XDG_CONFIG_HOME",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        assert!(validate_value("").is_err());
        assert!(validate_value("a\0b").is_err());
        assert!(validate_value(&"x".repeat(MAX_VALUE_BYTES + 1)).is_err());
        assert!(validate_value("ok").is_ok());
    }
}
