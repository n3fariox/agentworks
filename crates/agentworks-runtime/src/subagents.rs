//! Keep ACP session identity intact, including the older draft used by codex-acp.
use agent_client_protocol::{JsonRpcNotification, schema::v1::SessionUpdate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, JsonRpcNotification)]
#[notification(method = "session/update")]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub session_id: String,
    pub update: Update,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Update {
    Current(SessionUpdate),
    Legacy(LegacyUpdate),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "sessionUpdate",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LegacyUpdate {
    SubagentSpawned {
        subagent_session_id: String,
        name: String,
        task: String,
        #[serde(default)]
        capabilities: Value,
    },
    SubagentStateUpdate {
        subagent_session_id: String,
        state: String,
    },
}

/// Connection-local ownership. Children are read-only in Agentworks; prompts and
/// cancellation always target the root. Never infer ownership from tool prose.
#[derive(Default)]
pub struct Sessions {
    root: Option<String>,
    parents: HashMap<String, String>,
}

impl Sessions {
    pub fn new(root: Option<String>) -> Self {
        Self {
            root,
            parents: HashMap::new(),
        }
    }

    /// Reconstruct announced ownership when resuming without provider replay.
    pub fn restore(&mut self, update: &Value) {
        let Some(id) = update["acpSessionId"].as_str() else {
            return;
        };
        if let Ok(notification) = serde_json::from_value(json!({"sessionId":id,"update":update})) {
            let _ = self.route(notification);
        }
    }

    pub fn set_root(&mut self, root: String) {
        self.root = Some(root);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.root.as_deref() == Some(id) || self.parents.contains_key(id)
    }

    /// Returns the persisted, flattened update and whether it belongs to a child.
    pub fn route(&mut self, notification: Notification) -> Option<(Value, bool)> {
        let source = notification.session_id;
        if source.is_empty() {
            return None;
        }
        // Fresh sessions may publish commands before session/new returns. The
        // first traffic is necessarily on the root (children must be announced).
        if self.root.is_none() {
            self.root = Some(source.clone());
        }
        if !self.contains(&source) {
            return None;
        }
        let mut update = serde_json::to_value(notification.update).ok()?;
        let child = match update["sessionUpdate"].as_str()? {
            "subagent_update" => update["sessionId"].as_str(),
            "subagent_spawned" => update["subagentSessionId"].as_str(),
            "subagent_state_update" => {
                let id = update["subagentSessionId"].as_str()?;
                if self.parents.get(id) != Some(&source) {
                    return None;
                }
                None
            }
            _ => None,
        };
        if let Some(child) = child {
            if child.is_empty() || self.root.as_deref() == Some(child) {
                return None;
            }
            if let Some(parent) = self.parents.get(child) {
                if parent != &source {
                    return None;
                }
            } else {
                self.parents.insert(child.to_owned(), source.clone());
            }
        }
        let is_child = self.parents.contains_key(&source);
        update["acpSessionId"] = json!(source);
        if let Some(parent) = self.parents.get(&source) {
            update["parentSessionId"] = json!(parent);
        }
        Some((update, is_child))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: &str, update: Value) -> Notification {
        serde_json::from_value(json!({"sessionId":id,"update":update})).unwrap()
    }
    #[test]
    fn routes_both_drafts_nested_children_and_rejects_foreign_traffic() {
        let mut sessions = Sessions::new(Some("root".into()));
        assert!(sessions.route(event("foreign",json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"no"}}))).is_none());
        let spawn = json!({"sessionUpdate":"subagent_spawned","subagentSessionId":"child","name":"Research","task":"Inspect","capabilities":{}});
        assert!(!sessions.route(event("root", spawn.clone())).unwrap().1);
        let nested = json!({"sessionUpdate":"subagent_update","sessionId":"nested","title":"Review","state":{"state":"running"}});
        let (routed, child) = sessions.route(event("child", nested.clone())).unwrap();
        assert!(child);
        assert_eq!(routed["parentSessionId"], "root");
        let (routed,_) = sessions.route(event("nested",json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}))).unwrap();
        assert_eq!(routed["acpSessionId"], "nested");
        assert_eq!(routed["parentSessionId"], "child");
        assert!(sessions.route(event("root", nested)).is_none()); // no reparenting
        assert!(
            sessions
                .route(event(
                    "child",
                    json!({"sessionUpdate":"subagent_update","sessionId":"root"})
                ))
                .is_none()
        );
        let terminal = json!({"sessionUpdate":"subagent_state_update","subagentSessionId":"child","state":"completed"});
        assert!(sessions.route(event("child", terminal.clone())).is_none());
        assert!(sessions.route(event("root", terminal)).is_some());
        let persisted = sessions.route(event("root", spawn)).unwrap().0;
        let mut resumed = Sessions::new(Some("root".into()));
        resumed.restore(&persisted);
        assert!(resumed.contains("child"));
        assert!(!resumed.contains("foreign"));
    }
}
