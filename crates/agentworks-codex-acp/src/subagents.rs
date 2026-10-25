//! Codex owns child execution. ACP exposes their association and read-only traffic.
use super::*;
use std::collections::{HashSet, VecDeque};

impl Bridge {
    fn known_session(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(id)
            || self.children.lock().unwrap().contains_key(id)
    }

    fn announce_child(
        &self,
        cx: &ConnectionTo<Client>,
        parent: &str,
        id: &str,
        mut patch: Value,
    ) -> Result<()> {
        if !self.known_session(parent)
            || id == parent
            || id.is_empty()
            || self.sessions.lock().unwrap().contains_key(id)
        {
            return Ok(());
        }
        {
            let mut children = self.children.lock().unwrap();
            if children.get(id).is_some_and(|owner| owner != parent) {
                return Ok(()); // Messaging another agent does not transfer ownership.
            }
            children.insert(id.into(), parent.into());
        }
        patch["sessionUpdate"] = json!("subagent_update");
        patch["sessionId"] = json!(id);
        patch["capabilities"] = json!({});
        if self.subagents.load(Ordering::Relaxed) {
            update(cx, parent, patch)?;
        }
        Ok(())
    }

    fn child_state(&self, cx: &ConnectionTo<Client>, id: &str, state: Value) -> Result<()> {
        if !self.subagents.load(Ordering::Relaxed) {
            return Ok(());
        }
        let parent = self.children.lock().unwrap().get(id).cloned();
        if let Some(parent) = parent {
            update(
                cx,
                &parent,
                json!({"sessionUpdate":"subagent_update","sessionId":id,"state":state}),
            )?;
        }
        Ok(())
    }

    fn collab_item(
        &self,
        cx: &ConnectionTo<Client>,
        parent: &str,
        item: &Value,
        done: bool,
        replay: bool,
    ) -> Result<()> {
        if !self.known_session(parent) {
            return Ok(());
        }
        if item["type"] == "subAgentActivity" {
            if let Some(id) = item["agentThreadId"].as_str() {
                let mut patch = json!({});
                if let Some(path) = item["agentPath"].as_str() {
                    patch["title"] = json!(path);
                }
                if item["kind"] == "started" || self.children.lock().unwrap().contains_key(id) {
                    self.announce_child(cx, parent, id, patch)?;
                }
                // Parent activity markers don't carry the child's outcome. Their duplicate
                // item/completed must not overwrite a more precise child turn/completed.
                if !done || replay {
                    let state = match item["kind"].as_str() {
                        Some("completed") => Some(json!({"state":"idle"})),
                        Some("started") => collab_state("running"),
                        Some("interrupted") => collab_state("interrupted"),
                        _ => None,
                    };
                    if let Some(state) = state {
                        self.child_state(cx, id, state)?;
                    }
                }
            }
            return Ok(());
        }
        if item["type"] != "collabAgentToolCall" {
            return Ok(());
        }
        let spawn = item["tool"] == "spawnAgent";
        for id in item["receiverThreadIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if spawn {
                let mut patch = json!({});
                if let Some(prompt) = item["prompt"].as_str() {
                    patch["description"] = json!(prompt);
                }
                self.announce_child(cx, parent, id, patch)?;
            }
            if !self.children.lock().unwrap().contains_key(id) {
                continue;
            }
            if let Some(state) =
                collab_state(item["agentsStates"][id]["status"].as_str().unwrap_or(""))
            {
                self.child_state(cx, id, state)?;
            }
            if done && self.subagents.load(Ordering::Relaxed) {
                if let Some(prompt) = item["prompt"].as_str() {
                    update(
                        cx,
                        id,
                        json!({"sessionUpdate":"session_message","messageId":item["id"],"senderSessionId":parent,"recipientSessionId":id,"content":[{"type":"text","text":prompt}]}),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn observe_subagents(
        &self,
        cx: &ConnectionTo<Client>,
        method: &str,
        p: &Value,
    ) -> Result<()> {
        if method == "thread/started" {
            let thread = &p["thread"];
            let source = &thread["source"]["subAgent"]["thread_spawn"];
            if let (Some(parent), Some(id)) = (
                thread["parentThreadId"]
                    .as_str()
                    .or_else(|| source["parent_thread_id"].as_str()),
                thread["id"].as_str(),
            ) {
                let mut patch = json!({});
                if let Some(title) = thread["agentNickname"]
                    .as_str()
                    .or_else(|| thread["name"].as_str())
                    .or_else(|| source["agent_nickname"].as_str())
                {
                    patch["title"] = json!(title);
                }
                if let Some(role) = thread["agentRole"]
                    .as_str()
                    .or_else(|| source["agent_role"].as_str())
                {
                    patch["description"] = json!(role);
                }
                self.announce_child(cx, parent, id, patch)?;
            }
            return Ok(());
        }
        let id = p["threadId"].as_str().unwrap_or("");
        if matches!(method, "item/started" | "item/completed") {
            self.collab_item(cx, id, &p["item"], method == "item/completed", false)?;
        }
        let state = match method {
            "turn/started" => Some(json!({"state":"running"})),
            "turn/completed" => collab_state(match p["turn"]["status"].as_str() {
                Some("completed") => "completed",
                Some("interrupted") => "interrupted",
                _ => "errored",
            }),
            "thread/closed" => Some(json!({"state":"unknown"})),
            "thread/status/changed" => match p["status"]["type"].as_str() {
                Some("active") => Some(
                    json!({"state":if p["status"]["activeFlags"].as_array().is_some_and(|flags| flags.iter().any(|v|v=="waitingOnApproval" || v=="waitingOnUserInput")){"requires_action"}else{"running"}}),
                ),
                Some("idle") => Some(json!({"state":"idle"})),
                Some("systemError") => collab_state("errored"),
                _ => Some(json!({"state":"unknown"})),
            },
            _ => None,
        };
        if let Some(state) = state {
            self.child_state(cx, id, state)?;
        }
        Ok(())
    }

    pub(super) async fn replay_tree(&self, cx: &ConnectionTo<Client>, root: &str) -> Result<()> {
        let mut queue = VecDeque::from([root.to_owned()]);
        let mut visited = HashSet::new();
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let result = self
                .history(&id, |_, item| {
                    self.collab_item(cx, &id, item, true, true)?;
                    replay_item(cx, &id, item)
                })
                .await;
            // Deleted or ephemeral historical children must not prevent loading their parent.
            if id == root {
                result?;
            } else if result.is_err() {
                self.child_state(cx, &id, json!({"state":"unknown"}))?;
            }
            if !self.subagents.load(Ordering::Relaxed) {
                continue;
            }
            queue.extend(
                self.children
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, parent)| **parent == id)
                    .map(|(child, _)| child.clone()),
            );
        }
        Ok(())
    }

    pub(super) fn approval_session(&self, params: &Value) -> Value {
        let mut params = params.clone();
        if !self.subagents.load(Ordering::Relaxed) {
            // Older clients still need a way to approve native child requests.
            let children = self.children.lock().unwrap();
            let mut id = params["threadId"].as_str().unwrap_or("").to_owned();
            while let Some(parent) = children.get(&id) {
                id = parent.clone();
            }
            params["threadId"] = json!(id);
        }
        params
    }

    pub(super) fn forget_children(&self, root: &str) {
        let mut queue = vec![root.to_owned()];
        let mut children = self.children.lock().unwrap();
        while let Some(parent) = queue.pop() {
            children.retain(|child, owner| {
                if *owner == parent {
                    queue.push(child.clone());
                    false
                } else {
                    true
                }
            });
        }
    }
}

fn collab_state(status: &str) -> Option<Value> {
    Some(match status {
        "pendingInit" | "running" => json!({"state":"running"}),
        "completed" => json!({"state":"idle","stopReason":"end_turn"}),
        "interrupted" | "shutdown" => json!({"state":"idle","stopReason":"cancelled"}),
        "errored" => json!({"state":"idle","stopReason":"error"}),
        "notFound" => json!({"state":"unknown"}),
        _ => return None,
    })
}
