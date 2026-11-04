//! Subcommands a remote host runs: the worker loop and the processes it starts by
//! re-invoking its own executable (ACP bridges, Goose, the MCP relay). The full
//! service dispatches these too, so either binary can serve as a worker.
pub mod mcp_proxy;

use serde_json::json;
use std::error::Error;

/// Runs a worker subcommand. Returns `None` when `args` names none.
pub async fn dispatch(args: &[String]) -> Option<Result<(), Box<dyn Error>>> {
    let command = args.get(1).map(String::as_str)?;
    Some(match command {
        #[cfg(feature = "goose")]
        "goose-browser-url" => goose_browser_url(args.get(2)),
        #[cfg(feature = "goose")]
        "goose-login" => {
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_target(false)
                .init();
            let result =
                agentworks_goose_acp::login(args.get(2).map(String::as_str).unwrap_or("")).await;
            match result {
                Ok(()) => println!(
                    "Sign-in completed. You can close this terminal and start a Goose session."
                ),
                Err(error) => {
                    eprintln!("Sign-in failed: {error}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        #[cfg(feature = "goose")]
        "goose-acp" => {
            let result = agentworks_goose_acp::run().await;
            if let Err(error) = &result {
                eprintln!("agentworks goose-acp: {error}");
            }
            std::process::exit(i32::from(result.is_err()));
        }
        bridge @ ("codex-acp" | "claude-acp") => {
            let result = if bridge == "codex-acp" {
                agentworks_codex_acp::run().await
            } else {
                agentworks_claude_acp::run().await
            };
            if let Err(error) = &result {
                eprintln!("agentworks {bridge}: {error}");
            }
            std::process::exit(i32::from(result.is_err()));
        }
        "demo-acp" => {
            let result = agentworks_demo_acp::run().await;
            if let Err(error) = &result {
                eprintln!("agentworks demo-acp: {error}");
            }
            std::process::exit(i32::from(result.is_err()));
        }
        // Not `--service-protocol`: a worker must never pass as a service restart target.
        "--worker-protocol" => {
            println!(
                "{}",
                json!({"workerProtocol":agentworks_runtime::remote::PROTOCOL})
            );
            Ok(())
        }
        "mcp" => mcp_proxy::stdio().await.map_err(|e| e.to_string().into()),
        "agent" => {
            // Worker cleanup completes inside run(). Tokio stdin uses an uncancellable
            // blocking reader, so runtime shutdown can hang while a dead peer still
            // holds stdin open. Only this stateless CLI mode exits explicitly.
            agentworks_runtime::log::init();
            let code = match agentworks_runtime::worker::run(&args[2..]).await {
                Ok(()) => 0,
                Err(error) => {
                    tracing::error!("Worker stopped: {error}");
                    1
                }
            };
            std::process::exit(code);
        }
        "--probe" => probe(args.get(2).map(String::as_str).unwrap_or("demo")).await,
        _ => return None,
    })
}

#[cfg(feature = "goose")]
fn goose_browser_url(url: Option<&String>) -> Result<(), Box<dyn Error>> {
    // Browser launchers discard stdout; the sign-in PTY is our UI.
    use std::io::Write;
    let url = url.ok_or("Missing authorization URL")?;
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    writeln!(tty, "Open this URL in your browser:\n{url}")?;
    Ok(())
}

async fn probe(provider: &str) -> Result<(), Box<dyn Error>> {
    if !agentworks_runtime::remote::PROVIDERS.contains(&provider) {
        return Err("Use --probe claude|claude-rust|codex|codex-rust|goose|opencode|demo".into());
    }
    agentworks_runtime::acp::probe(provider).await?;
    Ok(())
}
