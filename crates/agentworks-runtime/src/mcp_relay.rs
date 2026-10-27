//! Bounded bridge for the native MCP facade. Only messaging crosses a worker link.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::Message;

pub fn allowed(operation: &str) -> bool {
    matches!(
        operation,
        "tools/list" | "list_sessions" | "read_inbox" | "send_message" | "acknowledge_message"
    )
}
pub fn valid_call(call: &Value) -> bool {
    call["token"]
        .as_str()
        .is_some_and(|t| !t.is_empty() && t.len() <= 128)
        && call["operation"].as_str().is_some_and(allowed)
        && call["arguments"].is_object()
}
pub fn unavailable() -> Value {
    json!({"error":{"status":503,"message":"Agentworks relay unavailable; mutation outcome may be uncertain. Do not retry without the original requestId."}})
}
pub fn denied() -> Value {
    json!({"error":{"status":403,"message":"Operation or credential is not authorized on this remote channel"}})
}
pub async fn read(read: impl tokio::io::AsyncRead + Unpin, limit: u64) -> std::io::Result<Value> {
    let mut bytes = Vec::new();
    BufReader::new(read)
        .take(limit + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.len() as u64 > limit || bytes.last() != Some(&b'\n') {
        return Err(std::io::Error::other("Invalid bridge frame"));
    }
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}
/// Destination and credential are supplied by the server runtime, never by the worker.
pub async fn forward(
    endpoint: SocketAddr,
    token: &str,
    operation: &str,
    arguments: Value,
) -> Value {
    if !endpoint.ip().is_loopback() || !allowed(operation) {
        return denied();
    }
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let mut socket = TcpStream::connect(endpoint).await?;
        let mut bytes = serde_json::to_vec(
            &json!({"token":token,"operation":operation,"arguments":arguments}),
        )?;
        if bytes.len() > 65535 {
            return Err(std::io::Error::other("Request too large"));
        }
        bytes.push(b'\n');
        socket.write_all(&bytes).await?;
        read(socket, 2 * 1024 * 1024).await
    })
    .await;
    let mut value = match result {
        Ok(Ok(v)) if v.is_object() => v,
        _ => return unavailable(),
    };
    if operation == "tools/list" && value.get("error").is_none() && !value["tools"].is_array() {
        return unavailable();
    }
    if operation == "tools/list" {
        if let Some(tools) = value["tools"].as_array_mut() {
            tools.retain(|tool| tool["name"].as_str().is_some_and(allowed));
        }
    }
    value
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;
struct Request {
    pending: Pending,
    id: u64,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}
/// Owned by the worker link; dropping it closes sockets and cancels pending calls.
pub struct WorkerBridge {
    pub address: SocketAddr,
    pending: Pending,
    task: tokio::task::JoinHandle<()>,
}
impl WorkerBridge {
    pub async fn start(out: mpsc::Sender<Message>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let pending: Pending = Default::default();
        let calls = pending.clone();
        let task = tokio::spawn(async move {
            let sequence = Arc::new(AtomicU64::new(0));
            let slots = Arc::new(tokio::sync::Semaphore::new(16));
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                    socket = listener.accept() => {
                        let Ok((mut socket, _)) = socket else { break; };
                        let Ok(slot) = slots.clone().try_acquire_owned() else { continue; };
                        let pending = calls.clone(); let out = out.clone();
                        let id = sequence.fetch_add(1, Ordering::Relaxed) + 1;
                        tasks.spawn(async move {
                            let _slot = slot;
                            let _ = tokio::time::timeout(Duration::from_secs(18), async {
                                let value = read(&mut socket, 65536).await?;
                                let result = if !valid_call(&value) { denied() } else {
                                    let (tx, rx) = oneshot::channel();
                                    pending.lock().unwrap().insert(id, tx);
                                    let _request = Request { pending, id };
                                    let message = json!({"type":"mcpRequest","request":id,"call":value});
                                    if out.try_send(Message::text(message.to_string())).is_err() { unavailable() }
                                    else { rx.await.unwrap_or_else(|_| unavailable()) }
                                };
                                let mut bytes = serde_json::to_vec(&result)?; bytes.push(b'\n');
                                socket.write_all(&bytes).await
                            }).await;
                        });
                    }
                }
            }
        });
        Ok(Self {
            address,
            pending,
            task,
        })
    }
    pub fn reply(&self, value: &Value) {
        if let Some(id) = value["request"].as_u64() {
            if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                let result = value
                    .get("result")
                    .filter(|v| v.is_object())
                    .cloned()
                    .unwrap_or_else(unavailable);
                let _ = tx.send(result);
            }
        }
    }
}
impl Drop for WorkerBridge {
    fn drop(&mut self) {
        self.task.abort();
        self.pending.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bridge_frames_require_a_newline_valid_json_and_size_bound() {
        assert!(read(&b"{}"[..], 10).await.is_err());
        assert!(read(&b"garbage\n"[..], 10).await.is_err());
        assert!(read(&b"{}\n"[..], 2).await.is_err());
        assert_eq!(read(&b"{}\n"[..], 3).await.unwrap(), json!({}));
    }
    #[tokio::test]
    async fn forward_uses_bound_identity_and_filters_discovery() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let call = read(&mut socket, 65536).await.unwrap();
            assert_eq!(call["token"], "bound-session");
            assert_eq!(call["operation"], "tools/list");
            socket.write_all(b"{\"tools\":[{\"name\":\"send_message\"},{\"name\":\"restart_service\"},{\"name\":\"unknown\"}]}\n").await.unwrap();
        });
        let value = forward(address, "bound-session", "tools/list", json!({})).await;
        assert_eq!(value, json!({"tools":[{"name":"send_message"}]}));
        service.await.unwrap();
        assert_eq!(
            forward(address, "t", "read_inbox", json!({})).await["error"]["status"],
            503
        );
        assert_eq!(
            forward(address, "t", "restart_service", json!({})).await["error"]["status"],
            403
        );
        assert_eq!(
            forward("192.0.2.1:1".parse().unwrap(), "t", "read_inbox", json!({})).await["error"]["status"],
            403
        );
    }
    #[tokio::test]
    async fn worker_roundtrip_correlates_replies_and_fails_closed_on_teardown() {
        let (out, mut frames) = mpsc::channel(4);
        let bridge = WorkerBridge::start(out).await.unwrap();
        let mut socket = TcpStream::connect(bridge.address).await.unwrap();
        socket
            .write_all(b"{\"token\":\"t\",\"operation\":\"read_inbox\",\"arguments\":{}}\n")
            .await
            .unwrap();
        let frame = frames.recv().await.unwrap();
        let call: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        bridge.reply(&json!({"request":999,"result":{"wrong":true}}));
        assert_eq!(bridge.pending.lock().unwrap().len(), 1);
        bridge.reply(&json!({"request":call["request"],"result":{"messages":[]}}));
        assert_eq!(read(socket, 65536).await.unwrap(), json!({"messages":[]}));
        let mut socket = TcpStream::connect(bridge.address).await.unwrap();
        socket
            .write_all(b"{\"token\":\"t\",\"operation\":\"send_message\",\"arguments\":{}}\n")
            .await
            .unwrap();
        frames.recv().await.unwrap();
        assert_eq!(bridge.pending.lock().unwrap().len(), 1);
        let pending = bridge.pending.clone();
        drop(bridge);
        assert!(pending.lock().unwrap().is_empty());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), read(socket, 65536))
                .await
                .unwrap()
                .is_err()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn missing_worker_reply_expires_and_a_late_reply_cannot_complete_another_call() {
        let (out, mut frames) = mpsc::channel(4);
        let bridge = WorkerBridge::start(out).await.unwrap();
        let mut socket = TcpStream::connect(bridge.address).await.unwrap();
        socket
            .write_all(b"{\"token\":\"t\",\"operation\":\"send_message\",\"arguments\":{}}\n")
            .await
            .unwrap();
        let frame: Value =
            serde_json::from_str(frames.recv().await.unwrap().to_text().unwrap()).unwrap();
        tokio::time::advance(Duration::from_secs(19)).await;
        assert!(read(socket, 65536).await.is_err());
        assert!(bridge.pending.lock().unwrap().is_empty());
        bridge.reply(&json!({"request":frame["request"],"result":{"accepted":true}}));
        assert!(bridge.pending.lock().unwrap().is_empty());
        assert!(frames.try_recv().is_err()); // No automatic retry of the mutation.
    }
    #[tokio::test]
    async fn malformed_service_reply_is_unavailable() {
        for reply in [
            b"garbage\n".as_slice(),
            b"null\n",
            b"{}\n",
            b"{\"tools\":[]}",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let reply = reply.to_vec();
            let service = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                read(&mut socket, 65536).await.unwrap();
                socket.write_all(&reply).await.unwrap();
            });
            assert_eq!(
                forward(address, "t", "tools/list", json!({})).await["error"]["status"],
                503
            );
            service.await.unwrap();
        }
    }

    #[tokio::test]
    async fn worker_rejects_unknown_methods_without_forwarding() {
        let (out, mut frames) = mpsc::channel(4);
        let bridge = WorkerBridge::start(out).await.unwrap();
        let mut socket = TcpStream::connect(bridge.address).await.unwrap();
        socket
            .write_all(b"{\"operation\":\"start_session\"}\n")
            .await
            .unwrap();
        assert_eq!(read(socket, 65536).await.unwrap()["error"]["status"], 403);
        assert!(frames.try_recv().is_err());
    }
}
