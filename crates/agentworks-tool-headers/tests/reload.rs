//! Exercise the leaf crate through the same public API used by the interpreter.
use agentworks_starlark::{TOOL_HEADER_SCRIPT, engine::Engine};
use agentworks_tool_headers::{HeaderTracker, Headers, key};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;

#[test]
fn reload_publishes_changes_and_forget_releases_session_state() {
    let engine = Arc::new(Engine::new().unwrap());
    let script = engine
        .compile_registered("tool_header.star", TOOL_HEADER_SCRIPT)
        .unwrap();
    let headers = Headers::default();
    let mut tracker = HeaderTracker::new(engine, headers.clone());
    let (tx, mut rx) = broadcast::channel::<Value>(8);
    let call = json!({
        "toolCallId": "a", "title": "bash -lc 'cd /p && cargo test'",
        "rawInput": {"command": ["/bin/bash", "-lc", "cd /p && cargo test"]},
        "acpSessionId": "child", "parentSessionId": "root"
    });
    let tool_key = key(&call).unwrap();
    assert!(
        tracker
            .update("s", "codex", "/p", 7, &call, Some(&tx))
            .is_none()
    );

    tracker.reload(script.handlers.clone());
    let event = rx.try_recv().unwrap();
    assert_eq!(
        event,
        json!({"kind": "tool_header", "seq": 7, "data": {
            "toolCallId": "a", "header": "cargo test",
            "acpSessionId": "child", "parentSessionId": "root"
        }})
    );
    assert_eq!(headers.lock().unwrap()["s"][&tool_key].1, "cargo test");
    tracker.reload(script.handlers.clone());
    assert!(rx.try_recv().is_err(), "unchanged reload publishes nothing");

    tracker.reload(Vec::new());
    assert_eq!(rx.try_recv().unwrap()["data"]["header"], Value::Null);
    assert!(headers.lock().unwrap()["s"].is_empty());
    tracker.forget("s");
    assert!(!headers.lock().unwrap().contains_key("s"));
    tracker.reload(script.handlers.clone());
    assert!(
        rx.try_recv().is_err(),
        "forgotten tools are not re-evaluated"
    );
    assert_eq!(tx.receiver_count(), 1);
    drop(tx);
    assert!(
        matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)),
        "forget releases the cached broadcast sender"
    );
}
