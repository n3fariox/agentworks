//! Provider launch configuration, executable discovery, and diagnostic sanitization.
use agent_client_protocol::AcpAgentConfig;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Adapter identity stays distinct in sessions; each Rust adapter shares its CLI/account.
pub fn cli_provider(provider: &str) -> &str {
    match provider {
        "codex-rust" => "codex",
        "claude-rust" => "claude",
        _ => provider,
    }
}
/// `claude` and `codex` once named the npm adapters. Sessions saved with them keep
/// their ID but now run the Rust bridge, which shares the same native history.
pub fn is_legacy(provider: &str) -> bool {
    matches!(provider, "claude" | "codex")
}

pub fn key_present(provider: &str) -> bool {
    let keys: &[&str] = match cli_provider(provider) {
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
/// Test hook: an ACP agent executable that stands in for the Claude/Codex bridges.
fn acp_override() -> Option<PathBuf> {
    std::env::var_os("AGENTWORKS_ACP_COMMAND").map(PathBuf::from)
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
    if matches!(cli_provider(provider), "codex" | "claude") {
        if acp_override().is_some() {
            return true;
        }
        let (var, name) = if cli_provider(provider) == "claude" {
            ("CLAUDE_CODE_EXECUTABLE", "claude")
        } else {
            ("CODEX_PATH", "codex")
        };
        let executable = std::env::var_os(var).unwrap_or_else(|| name.into());
        return std::path::Path::new(&executable).is_file()
            || std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|p| p.join(&executable).is_file())
            });
    }
    match provider {
        "demo" | "goose" => true,
        "opencode" => opencode_path().is_some(),
        _ => false,
    }
}

/// The service's own configuration and process-manager state. A harness never
/// needs these, and inheriting them would let a server started by an agent open
/// the live store, or let a child impersonate the service to systemd.
pub const SERVICE_ENVIRONMENT: &[&str] = &[
    "AGENTWORKS_BIND",
    "AGENTWORKS_PORT",
    "AGENTWORKS_ROOTS",
    "AGENTWORKS_CATALOG_PATH",
    "AGENTWORKS_DB_PATH",
    "AGENTWORKS_AUTH_MODE",
    "AGENTWORKS_PWA",
    "AGENTWORKS_MODEL_PROVIDERS_DIR",
    "AGENTWORKS_SUPERVISOR_DIR",
    "AGENTWORKS_SUPERVISOR_TOKEN",
    "AGENTWORKS_RELEASE_DIR",
    "AGENTWORKS_RESTART_REQUEST",
    "AGENTWORKS_OPENCODE_PATH",
    "AGENTWORKS_MCP_ENDPOINT",
    "AGENTWORKS_MCP_ENDPOINT_FILE",
    "AGENTWORKS_MCP_TOKEN",
    "AGENTWORKS_MCP_TOKEN_FILE",
    "AGENTWORKS_LOGIN_ENV",
    "NOTIFY_SOCKET",
    "LISTEN_PID",
    "LISTEN_FDS",
    "LISTEN_FDNAMES",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "SYSTEMD_EXEC_PID",
    "MANAGERPID",
    "MANAGERPIDFDID",
    "MEMORY_PRESSURE_WATCH",
    "MEMORY_PRESSURE_WRITE",
];

/// Tells a harness, and everything it runs, which Agentworks session it serves.
pub fn session_environment(id: &str) -> [(&'static str, String); 2] {
    [
        ("AGENTWORKS_SESSION_ID", id.to_owned()),
        (
            "AGENTWORKS_SESSION_URI",
            format!("agentworks://sessions/{id}"),
        ),
    ]
}

pub fn agent_config(provider: &str, api_key_mode: bool, host_profile: bool) -> AcpAgentConfig {
    let config = if provider == "demo" {
        AcpAgentConfig::new(std::env::current_exe().expect("executable path")).arg("demo-acp")
    } else if provider == "goose" {
        // Goose owns provider authentication; retain its keys and endpoint settings
        // independently of the Claude/Codex subscription-versus-key switch.
        let mut config =
            AcpAgentConfig::new(std::env::current_exe().expect("executable path")).arg("goose-acp");
        if !host_profile && std::env::var_os("GOOSE_PATH_ROOT").is_none() {
            let root = runtime_root().join(".agentworks/goose");
            let root = if root.is_absolute() {
                root
            } else {
                std::env::current_dir()
                    .expect("working directory")
                    .join(root)
            };
            config = config.env("GOOSE_PATH_ROOT", root.to_string_lossy());
        }
        config
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
        let mut config = if let Some(command) = acp_override() {
            AcpAgentConfig::new(command)
        } else {
            let bridge = if cli_provider(provider) == "codex" {
                "codex-acp"
            } else {
                "claude-acp"
            };
            AcpAgentConfig::new(std::env::current_exe().expect("executable path")).arg(bridge)
        }
        .env("NO_BROWSER", "1");
        if api_key_mode {
            let home = runtime_root()
                .join(".agentworks")
                .join(cli_provider(provider));
            if cli_provider(provider) == "codex" {
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
    SERVICE_ENVIRONMENT
        .iter()
        .fold(config, |config, name| config.env_remove(*name))
}

pub fn prepare(provider: &str) -> std::io::Result<()> {
    if provider == "opencode" {
        for dir in ["data", "cache", "state", "config"] {
            std::fs::create_dir_all(runtime_root().join(".agentworks/opencode").join(dir))?;
        }
    } else if !matches!(provider, "demo" | "goose") && use_api_key() {
        std::fs::create_dir_all(
            runtime_root()
                .join(".agentworks")
                .join(cli_provider(provider)),
        )?;
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
    fn every_harness_withholds_service_configuration() {
        for provider in ["demo", "goose", "opencode", "claude", "codex-rust"] {
            let config = agent_config(provider, false, false);
            for name in [
                "AGENTWORKS_DB_PATH",
                "AGENTWORKS_SUPERVISOR_TOKEN",
                "AGENTWORKS_MCP_TOKEN",
                "AGENTWORKS_MCP_TOKEN_FILE",
                "AGENTWORKS_MCP_ENDPOINT",
                "AGENTWORKS_MCP_ENDPOINT_FILE",
                "NOTIFY_SOCKET",
            ] {
                assert!(
                    config.removed_environment().contains(name),
                    "{provider} {name}"
                );
                assert!(
                    !config.environment().contains_key(name),
                    "{provider} {name}"
                );
            }
        }
    }
    #[test]
    fn existing_signin_inherits_credential_locations_and_does_not_force_api_keys() {
        for provider in ["claude", "claude-rust", "codex", "codex-rust"] {
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

    #[test]
    fn embedded_goose_retains_provider_credentials_in_both_auth_modes() {
        for api_key_mode in [false, true] {
            let config = agent_config("goose", api_key_mode, false);
            assert_eq!(config.command(), std::env::current_exe().unwrap());
            assert_eq!(config.arguments(), &["goose-acp"]);
            for key in [
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GOOSE_PROVIDER",
                "OPENAI_BASE_URL",
            ] {
                assert!(!config.environment().contains_key(key));
            }
            if std::env::var_os("GOOSE_PATH_ROOT").is_none() {
                assert!(
                    std::path::Path::new(&config.environment()["GOOSE_PATH_ROOT"]).is_absolute()
                );
            }
        }
        assert!(installed("goose"));
        assert!(
            !agent_config("goose", false, true)
                .environment()
                .contains_key("GOOSE_PATH_ROOT")
        );
    }

    #[test]
    fn native_codex_is_a_distinct_adapter_with_shared_codex_authentication() {
        let native = agent_config("codex-rust", true, false);
        assert_eq!(native.command(), std::env::current_exe().unwrap());
        assert_eq!(native.arguments(), &["codex-acp"]);
        assert_eq!(
            native.environment().get("CODEX_HOME"),
            agent_config("codex", true, false)
                .environment()
                .get("CODEX_HOME")
        );
        assert!(!native.environment().contains_key("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn legacy_ids_run_the_rust_bridges() {
        assert!(is_legacy("claude") && is_legacy("codex") && !is_legacy("claude-rust"));
        for (legacy, native) in [("claude", "claude-rust"), ("codex", "codex-rust")] {
            let (old, new) = (
                agent_config(legacy, false, false),
                agent_config(native, false, false),
            );
            assert_eq!(old.command(), new.command());
            assert_eq!(old.arguments(), new.arguments());
            assert_eq!(installed(legacy), installed(native));
        }
    }

    #[test]
    fn native_claude_is_a_distinct_adapter_with_shared_claude_authentication() {
        let native = agent_config("claude-rust", true, false);
        assert_eq!(native.command(), std::env::current_exe().unwrap());
        assert_eq!(native.arguments(), &["claude-acp"]);
        assert_eq!(
            native.environment().get("CLAUDE_CONFIG_DIR"),
            agent_config("claude", true, false)
                .environment()
                .get("CLAUDE_CONFIG_DIR")
        );
        assert!(!native.environment().contains_key("CODEX_HOME"));
        assert_eq!(key_present("claude-rust"), key_present("claude"));
    }
}
