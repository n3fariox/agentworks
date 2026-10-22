//! Provider launch configuration, executable discovery, and diagnostic sanitization.
use agent_client_protocol::AcpAgentConfig;
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn key_present(provider: &str) -> bool {
    let keys: &[&str] = match provider {
        "claude" => &["ANTHROPIC_API_KEY"],
        "codex" => &["CODEX_API_KEY", "OPENAI_API_KEY"],
        _ => return true,
    };
    keys.iter()
        .any(|k| std::env::var(k).is_ok_and(|s| !s.trim().is_empty()))
}
pub fn use_api_key() -> bool {
    std::env::var("AGENTWORKS_AUTH_MODE").is_ok_and(|mode| mode == "api_key")
}
pub fn runtime_root() -> PathBuf {
    std::env::var_os("AGENTWORKS_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")))
}
pub fn adapter_path(provider: &str) -> PathBuf {
    runtime_root()
        .join("node_modules/.bin")
        .join(if provider == "claude" {
            "claude-agent-acp"
        } else {
            "codex-acp"
        })
}
pub fn opencode_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AGENTWORKS_OPENCODE_PATH") {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join("opencode"))
            .find(|p| p.is_file())
    })
}
pub fn installed(provider: &str) -> bool {
    match provider {
        "demo" => true,
        "opencode" => opencode_path().is_some(),
        _ => adapter_path(provider).exists(),
    }
}

pub fn agent_config(provider: &str, api_key_mode: bool, host_profile: bool) -> AcpAgentConfig {
    let config = if provider == "demo" {
        AcpAgentConfig::new(std::env::var_os("AGENTWORKS_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                runtime_root()
                    .join("scripts/demo-agent.mjs")
                    .to_string_lossy(),
            )
    } else if provider == "opencode" {
        let home = runtime_root().join(".agentworks/opencode");
        let mut config =
            AcpAgentConfig::new(opencode_path().unwrap_or_else(|| "opencode".into())).arg("acp");
        if !host_profile {
            for (name, dir) in [
                ("XDG_DATA_HOME", "data"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_STATE_HOME", "state"),
                ("XDG_CONFIG_HOME", "config"),
            ] {
                config = config.env(name, home.join(dir).to_string_lossy());
            }
        }
        // Free-model testing is isolated from the user's paid provider accounts.
        config
            .env("OPENAI_API_KEY", "")
            .env("CODEX_API_KEY", "")
            .env("ANTHROPIC_API_KEY", "")
    } else {
        let mut config = if let Some(node) = std::env::var_os("AGENTWORKS_NODE") {
            AcpAgentConfig::new(node).arg(adapter_path(provider).to_string_lossy())
        } else {
            AcpAgentConfig::new(adapter_path(provider))
        }
        .env("NO_BROWSER", "1");
        if api_key_mode {
            let home = runtime_root().join(".agentworks").join(provider);
            if provider == "codex" {
                config = config
                    .env("CODEX_HOME", home.to_string_lossy())
                    .env("ANTHROPIC_API_KEY", "");
            } else {
                config = config
                    .env("CLAUDE_CONFIG_DIR", home.to_string_lossy())
                    .env("OPENAI_API_KEY", "")
                    .env("CODEX_API_KEY", "");
            }
        } else {
            // Inherit the host CLI configuration/credential store without copying tokens.
            // An incidental API-key env var must not switch subscription sessions to API billing.
            config = config
                .env("OPENAI_API_KEY", "")
                .env("CODEX_API_KEY", "")
                .env("ANTHROPIC_API_KEY", "")
                .env("DEFAULT_AUTH_REQUEST", "");
        }
        config
    };
    config
}

pub fn prepare(provider: &str) -> std::io::Result<()> {
    if provider == "opencode" {
        for dir in ["data", "cache", "state", "config"] {
            std::fs::create_dir_all(runtime_root().join(".agentworks/opencode").join(dir))?;
        }
    } else if provider != "demo" && use_api_key() {
        std::fs::create_dir_all(runtime_root().join(".agentworks").join(provider))?;
    }
    Ok(())
}

pub fn is_free_model(id: &str) -> bool {
    id.starts_with("opencode/") && (id.ends_with("-free") || id == "opencode/big-pickle")
}
pub fn parse_model_config(config: &Value, free_only: bool) -> Option<Value> {
    let model = config["configOptions"]
        .as_array()?
        .iter()
        .find(|c| c["category"] == "model" || c["id"] == "model")?;
    fn collect(items: &[Value], out: &mut Vec<Value>, free_only: bool) {
        for option in items {
            if let Some(group) = option["options"].as_array() {
                collect(group, out, free_only);
            } else if option["value"]
                .as_str()
                .is_some_and(|id| !free_only || is_free_model(id))
            {
                out.push(option.clone());
            }
        }
    }
    let mut options = vec![];
    collect(model["options"].as_array()?, &mut options, free_only);
    if options.is_empty() {
        return None;
    }
    let current = if options.iter().any(|o| o["value"] == model["currentValue"]) {
        model["currentValue"].clone()
    } else {
        options[0]["value"].clone()
    };
    Some(json!({"configId":model["id"],"currentValue":current,"options":options}))
}

// Never surface configured credentials through an adapter diagnostic.
pub fn safe_error(message: &str) -> String {
    let mut safe = message.to_string();
    for key in ["OPENAI_API_KEY", "CODEX_API_KEY", "ANTHROPIC_API_KEY"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                safe = safe.replace(&value, "[redacted]");
            }
        }
    }
    safe.chars().take(2500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn existing_signin_inherits_credential_locations_and_does_not_force_api_keys() {
        for provider in ["claude", "codex"] {
            let config = agent_config(provider, false, false);
            assert!(!config.environment().contains_key("CODEX_HOME"));
            assert!(!config.environment().contains_key("CLAUDE_CONFIG_DIR"));
            assert_eq!(
                config
                    .environment()
                    .get("OPENAI_API_KEY")
                    .map(String::as_str),
                Some("")
            );
            assert_eq!(
                config
                    .environment()
                    .get("ANTHROPIC_API_KEY")
                    .map(String::as_str),
                Some("")
            );
            assert_eq!(
                config
                    .environment()
                    .get("DEFAULT_AUTH_REQUEST")
                    .map(String::as_str),
                Some("")
            );
        }
    }
    #[test]
    fn opencode_host_import_uses_host_store_without_api_key_overrides() {
        let isolated = agent_config("opencode", false, false);
        let host = agent_config("opencode", false, true);
        assert!(isolated.environment().contains_key("XDG_DATA_HOME"));
        for key in [
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        ] {
            assert!(!host.environment().contains_key(key));
        }
        assert_eq!(
            host.environment().get("OPENAI_API_KEY").map(String::as_str),
            Some("")
        );
    }
    #[test]
    fn explicit_api_key_mode_keeps_its_own_store() {
        let codex = agent_config("codex", true, false);
        let claude = agent_config("claude", true, false);
        assert!(codex.environment()["CODEX_HOME"].ends_with(".agentworks/codex"));
        assert!(claude.environment()["CLAUDE_CONFIG_DIR"].ends_with(".agentworks/claude"));
    }
}
