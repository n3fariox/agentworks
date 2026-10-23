#[tokio::main]
async fn main() {
    let code = match agentworks_codex_acp::run().await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agentworks-codex-acp: {e}");
            1
        }
    };
    // Tokio's blocking stdin reader can outlive the protocol connection.
    std::process::exit(code);
}
