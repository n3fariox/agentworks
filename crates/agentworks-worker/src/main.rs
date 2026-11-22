//! Worker-only executable for remote hosts: no dashboard, store or scripting.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match agentworks_worker::dispatch(&args).await {
        Some(result) => result,
        None => Err(
            "Usage: agentworks-worker agent|claude-acp|codex-acp|demo-acp|goose-acp|mcp|--probe|--worker-protocol"
                .into(),
        ),
    }
}
