#[tokio::main]
async fn main() {
    let code = match agentworks_demo_acp::run().await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agentworks-demo-acp: {e}");
            1
        }
    };
    // Tokio's blocking stdin reader can outlive the protocol connection.
    std::process::exit(code);
}
