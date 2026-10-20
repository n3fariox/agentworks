//! Worker mode: `agentworks agent --stdio`. The Agentworks server starts this
//! over SSH and speaks WebSocket with it over ssh's stdin and stdout, so stdout
//! carries only the protocol and all diagnostics go to stderr. The worker keeps
//! no state of its own: when the link drops it exits, and the server starts a
//! fresh worker when it reconnects.
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::Message;

const USAGE: &str = "Usage: agentworks agent --stdio (started by the Agentworks server over SSH)";

pub async fn run(args: &[String]) -> Result<(), String> {
    if args != ["--stdio"] {
        return Err(USAGE.into());
    }
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("{}\n", crate::hosts::READY).as_bytes())
        .await
        .and(stdout.flush().await)
        .map_err(|e| format!("Could not write to stdout: {e}"))?;
    let stream = tokio::io::join(tokio::io::stdin(), stdout);
    let (mut ws, _) = tokio_tungstenite::client_async("ws://agentworks/worker", stream)
        .await
        .map_err(|e| format!("WebSocket handshake with the server failed: {e}"))?;
    let providers: Vec<&str> = crate::hosts::PROVIDERS
        .into_iter()
        .filter(|p| crate::installed(p))
        .collect();
    let hello = json!({
        "type": "hello",
        "protocol": crate::hosts::PROTOCOL,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "hostname": hostname().unwrap_or_default(),
        "providers": providers,
    });
    ws.send(Message::text(hello.to_string()))
        .await
        .map_err(|e| format!("Could not reach the server: {e}"))?;
    let reply = match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            serde_json::from_str::<Value>(&text).unwrap_or(Value::Null)
        }
        _ => return Err("The server did not answer the hello".into()),
    };
    match reply.get("type").and_then(Value::as_str) {
        Some("welcome") => {}
        Some("error") => {
            return Err(reply["message"]
                .as_str()
                .unwrap_or("Rejected by the server")
                .into());
        }
        _ => return Err("The server sent an unexpected reply".into()),
    }
    eprintln!("agentworks agent: connected ({})", providers.join(", "));

    loop {
        match ws.next().await {
            // The server closed the link, or ssh went away and stdin hit EOF.
            None | Some(Ok(Message::Close(_))) => {
                eprintln!("agentworks agent: the link closed");
                return Ok(());
            }
            Some(Err(e)) => return Err(format!("The link failed: {e}")),
            // Pings are answered by the WebSocket library while reading.
            Some(Ok(_)) => {}
        }
    }
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
