//! Persisted job observations, independent of tool-call and turn completion.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub tasks: Vec<Value>,
    // A saved capability never authorizes control of a replacement process.
    #[serde(skip)]
    pub supported: bool,
}
pub fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "stopped" | "cancelled")
}
impl State {
    pub fn connection(&mut self, supported: bool) {
        self.supported = supported;
        for task in &mut self.tasks {
            if !terminal(task["state"].as_str().unwrap_or("")) {
                task["state"] = json!("unknown");
            }
        }
    }
    pub fn observe(&mut self, update: &Value) {
        let Some(kind) = update["sessionUpdate"].as_str() else {
            return;
        };
        if !matches!(
            kind,
            "async_task_spawned" | "async_task_progress" | "async_task_state_update"
        ) {
            return;
        }
        let (Some(id), Some(owner)) = (
            update["asyncTaskId"].as_str(),
            update["acpSessionId"].as_str(),
        ) else {
            return;
        };
        if id.is_empty() || owner.is_empty() {
            return;
        }
        let index = self
            .tasks
            .iter()
            .position(|t| t["asyncTaskId"] == id && t["acpSessionId"] == owner);
        let index = match index {
            Some(i) => i,
            None if kind == "async_task_spawned" => {
                self.tasks
                    .push(json!({"asyncTaskId":id,"acpSessionId":owner,"state":"running"}));
                self.tasks.len() - 1
            }
            _ => return,
        };
        let task = &mut self.tasks[index];
        let finished = terminal(task["state"].as_str().unwrap_or(""));
        for key in [
            "name",
            "taskType",
            "description",
            "showInTranscript",
            "canStop",
            "outputFilePath",
            "toolCallId",
            "summary",
            "progress",
            "usage",
            "lastToolName",
            "parentSessionId",
        ] {
            if let Some(value) = update.get(key) {
                task[key] = value.clone();
            }
        }
        if !finished {
            if kind == "async_task_spawned" {
                task["state"] = json!("running");
            }
            if kind == "async_task_state_update" {
                if let Some(state) = update["state"]
                    .as_str()
                    .filter(|s| terminal(s) || matches!(*s, "running" | "pending" | "stopping"))
                {
                    task["state"] = json!(state);
                }
            }
        }
        // Keep all live jobs, but bound completed history separately from the transcript.
        while self
            .tasks
            .iter()
            .filter(|t| terminal(t["state"].as_str().unwrap_or("")))
            .count()
            > 100
        {
            if let Some(i) = self
                .tasks
                .iter()
                .position(|t| terminal(t["state"].as_str().unwrap_or("")))
            {
                self.tasks.remove(i);
            }
        }
    }
    pub fn can_stop(&self, id: &str, owner: &str) -> bool {
        self.supported
            && self.tasks.iter().any(|t| {
                t["asyncTaskId"] == id
                    && t["acpSessionId"] == owner
                    && t["canStop"] == true
                    && t["state"] == "running"
            })
    }
    pub fn snapshot(&self) -> Value {
        let mut tasks = self.tasks.clone();
        if !self.supported {
            for task in &mut tasks {
                if !terminal(task["state"].as_str().unwrap_or("")) {
                    task["state"] = json!("unknown");
                }
            }
        }
        json!({"supported":self.supported,"tasks":tasks})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spawn(owner: &str) -> Value {
        json!({"sessionUpdate":"async_task_spawned","acpSessionId":owner,"asyncTaskId":"same","name":"Build","toolCallId":"launch","canStop":true})
    }
    #[test]
    fn jobs_are_scoped_and_terminal_results_survive_duplicate_launches() {
        let mut state = State::default();
        state.connection(true);
        state.observe(&spawn("root"));
        state.observe(&spawn("child"));
        assert_eq!(state.tasks.len(), 2);
        assert!(state.can_stop("same", "child"));
        state.observe(&json!({"sessionUpdate":"async_task_state_update","acpSessionId":"root","asyncTaskId":"same","state":"completed","summary":"Passed"}));
        state.observe(&spawn("root"));
        assert_eq!(state.tasks[0]["state"], "completed");
        assert!(!state.can_stop("same", "root"));
        assert!(state.can_stop("same", "child"));
        state.connection(false);
        assert_eq!(state.tasks[1]["state"], "unknown");
        assert_eq!(state.tasks[0]["state"], "completed");
        assert!(!state.can_stop("same", "child"));
        state.connection(true);
        assert!(!state.can_stop("same", "child"));
        state.observe(&spawn("child"));
        assert!(state.can_stop("same", "child"));
    }
    #[test]
    fn persistence_never_restores_live_control_and_unknown_updates_do_not_create_jobs() {
        let mut state = State::default();
        state.connection(true);
        state.observe(&spawn("root"));
        let restored: State =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert!(!restored.can_stop("same", "root"));
        assert_eq!(restored.snapshot()["tasks"][0]["state"], "unknown");
        state.observe(&json!({"sessionUpdate":"async_task_state_update","acpSessionId":"foreign","asyncTaskId":"same","state":"completed"}));
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.tasks[0]["state"], "running");
    }
}
