use agentworks_session::{SessionData, queue::ItemState};
use serde_json::{Value, json};

fn legacy() -> Value {
    json!({
        "status":"suspended", "seq":7, "requests":["old-request"],
        "models":null, "available_commands":[], "turn_active":false,
        "project_id":null, "provider_session_id":"native-saved"
    })
}

#[test]
fn old_database_payloads_load_without_enabling_new_execution_policies() {
    let state: SessionData = serde_json::from_value(legacy()).unwrap();
    assert_eq!(state.provider_session_id.as_deref(), Some("native-saved"));
    assert!(state.requests.contains("old-request"));
    assert!(state.initial_claude_permission_mode.is_none());
    assert!(state.initial_codex_permission_mode.is_none());
    assert!(!state.continuation.enabled);
    assert!(state.input_queue.items.is_empty());
    assert!(state.inbox_turn.is_none());
    assert!(!state.lifecycle.archived);
    assert!(state.lineage.parent_id.is_none());
    assert!(state.timeouts.is_none());
    assert!(state.worktree.is_none());
}

#[test]
fn persisted_identities_and_uncertain_dispatch_survive_the_crate_boundary() {
    let mut saved = legacy();
    saved["inbox_turn"] = json!({"requestId":"inbox-r", "generation":3, "native":"native-saved"});
    saved["lifecycle"] = json!({"title":"Stored title", "archived":true, "suspended":true});
    saved["initial_codex_permission_mode"] = json!("workspace-write");
    saved["permission_modes"] = json!({"configId":"mode","currentValue":"read-only","options":[{"value":"read-only"},{"value":"agent-full-access"}]});
    saved["initial_claude_permission_mode"] = json!("acceptEdits");
    saved["automation"] = json!({"automationId":"a", "name":"Recipe", "runId":"run", "requestId":"automation-r", "outcome":"running", "initiatedBy":"human"});
    saved["control_origin"] = json!({"actor":"controller:c", "request":"control-r", "payload":{"exact":"binding"}, "actorTitle":"Controller"});
    saved["continuation"] = json!({"enabled":true, "generation":4, "authorization":2, "maxAttempts":3, "attempts":1, "limited":true, "phase":"claimed", "requestId":"limit-r", "dueAt":12345, "deadline":99999});
    saved["input_queue"] = json!({"items":[{"id":"q1", "requestId":"queued-r", "text":"inspect", "revision":2, "state":"sending", "created":123}], "nextId":1, "ready":true});
    saved["lineage"] = json!({"parentId":"parent", "parentHostId":"host-a", "context":"auto", "boundarySeq":42, "nativeAttempted":true, "nativeUsed":false});
    saved["worktree"] = json!({"path":"/repo/.worktrees/child", "branch":"child", "commonDir":"/repo/.git", "mainTop":"/repo", "baseCommit":"abc123", "removed":null});

    let mut state: SessionData = serde_json::from_value(saved.clone()).unwrap();
    state.events.push_back(json!({"transient":true}));
    state.bytes = 100;
    state.storage_error = Some("transient failure".into());
    let encoded = serde_json::to_value(&state).unwrap();
    for key in [
        "inbox_turn",
        "lifecycle",
        "initial_claude_permission_mode",
        "initial_codex_permission_mode",
        "permission_modes",
        "automation",
        "control_origin",
        "worktree",
    ] {
        assert_eq!(
            encoded[key], saved[key],
            "persisted field {key} changed shape"
        );
    }
    for key in ["events", "bytes", "storage_error"] {
        assert!(
            encoded.get(key).is_none(),
            "transient field {key} was persisted"
        );
    }
    let mut restored: SessionData = serde_json::from_value(encoded).unwrap();
    restored.continuation.reconcile();
    restored.input_queue.recover();
    assert_eq!(restored.continuation.phase.as_deref(), Some("uncertain"));
    assert_eq!(restored.continuation.attempts, 1);
    assert!(restored.continuation.due_at.is_none());
    assert_eq!(restored.input_queue.items[0].state, ItemState::Error);
    assert_eq!(restored.input_queue.items[0].dispatch_id(), "queue:q1:r2");
    assert!(restored.input_queue.claim().is_none());
    assert!(restored.events.is_empty());
    assert!(restored.storage_error.is_none());
}
