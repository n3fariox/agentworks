//! The application observes hook execution through the public scripting API.
use agentworks_presentation::tool_call::key;
use agentworks_scripting::{Hub, SCHEMA, seed, set_enabled};
use agentworks_starlark::engine::Engine;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::sync::broadcast;

// A synchronous request on the same interpreter queue provides an ordering
// barrier without relying on sleeps or exposing the thread's internal state.
fn flush(hub: &Hub) {
    assert!(hub.format_inbox_in_session(None, &[]).is_empty());
}

#[test]
fn reload_publishes_changes_and_forget_releases_session_state() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(SCHEMA).unwrap();
    seed(&db).unwrap();
    set_enabled(&db, "tool_header.star", false).unwrap();
    let hub = Hub::new(Arc::new(Engine::new().unwrap()), Vec::new());
    hub.reload(&db).unwrap();
    let (tx, mut rx) = broadcast::channel::<Value>(8);
    let call = json!({
        "sessionUpdate": "tool_call", "toolCallId": "a",
        "title": "bash -lc 'cd /p && cargo test'",
        "rawInput": {"command": ["/bin/bash", "-lc", "cd /p && cargo test"]},
        "acpSessionId": "child", "parentSessionId": "root"
    });
    hub.update("s", ("codex", Path::new("/p")), &tx, 7, &call);
    flush(&hub);
    assert!(hub.headers("s").is_empty());
    assert!(rx.try_recv().is_err());

    set_enabled(&db, "tool_header.star", true).unwrap();
    hub.reload(&db).unwrap();
    flush(&hub);
    let event = rx.try_recv().unwrap();
    assert_eq!(
        event,
        json!({"kind": "tool_header", "seq": 7, "data": {
            "toolCallId": "a", "header": "cargo test",
            "acpSessionId": "child", "parentSessionId": "root"
        }})
    );
    assert_eq!(hub.headers("s")[&key(&call).unwrap()], "cargo test");
    hub.reload(&db).unwrap();
    flush(&hub);
    assert!(rx.try_recv().is_err(), "unchanged reload publishes nothing");

    set_enabled(&db, "tool_header.star", false).unwrap();
    hub.reload(&db).unwrap();
    assert_eq!(rx.blocking_recv().unwrap()["data"]["header"], Value::Null);
    assert!(hub.headers("s").is_empty());
    hub.forget("s");
    set_enabled(&db, "tool_header.star", true).unwrap();
    hub.reload(&db).unwrap();
    flush(&hub);
    assert!(hub.headers("s").is_empty());
    assert!(
        rx.try_recv().is_err(),
        "forgotten tools are not re-evaluated"
    );
    drop(tx);
    assert!(
        matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)),
        "forget releases the cached broadcast sender"
    );
}
