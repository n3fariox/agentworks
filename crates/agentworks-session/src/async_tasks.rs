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
/// The record of an `async_task_finished` event: the job as last reported, so the
/// transcript can show its outcome even when its launch is outside the retained window.
fn result(task: &Value, reason: Option<&str>) -> Value {
    let mut result = task.clone();
    if let Some(object) = result.as_object_mut() {
        object.remove("resultSeq");
        object.remove("canStop");
        if let Some(reason) = reason {
            object.insert("reason".into(), json!(reason));
        }
    }
    result
}
impl State {
    /// A connection change ends control of every job the old process owned. Jobs not
    /// already resolved get a result whose state is honestly unknown.
    pub fn connection(&mut self, supported: bool) -> Vec<Value> {
        self.supported = supported;
        let mut results = vec![];
        for task in &mut self.tasks {
            let state = task["state"].as_str().unwrap_or("");
            if !terminal(state) && state != "unknown" {
                task["state"] = json!("unknown");
                results.push(result(task, Some("connection_ended")));
            }
        }
        results
    }
    /// Marks a job's outcome as shown in the transcript at `seq`; a relaunch clears it.
    pub fn record(&mut self, finished: &Value, seq: u64) {
        if let Some(task) = self.tasks.iter_mut().find(|t| {
            t["asyncTaskId"] == finished["asyncTaskId"]
                && t["acpSessionId"] == finished["acpSessionId"]
                && t["state"] == finished["state"]
        }) {
            task["resultSeq"] = json!(seq);
        }
    }
    /// Returns the job's result when this update ended it.
    pub fn observe(&mut self, update: &Value) -> Option<Value> {
        let kind = update["sessionUpdate"].as_str()?;
        if !matches!(
            kind,
            "async_task_spawned" | "async_task_progress" | "async_task_state_update"
        ) {
            return None;
        }
        let (Some(id), Some(owner)) = (
            update["asyncTaskId"].as_str(),
            update["acpSessionId"].as_str(),
        ) else {
            return None;
        };
        if id.is_empty() || owner.is_empty() {
            return None;
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
            _ => return None,
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
        let mut ended = None;
        if !finished {
            if kind == "async_task_spawned" {
                task["state"] = json!("running");
                if let Some(object) = task.as_object_mut() {
                    object.remove("resultSeq");
                }
            }
            if kind == "async_task_state_update"
                && let Some(state) = update["state"].as_str().filter(|s| {
                    terminal(s) || matches!(*s, "running" | "pending" | "paused" | "stopping")
                })
            {
                task["state"] = json!(state);
                if terminal(state) {
                    ended = Some(result(task, None));
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
        ended
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
    fn update(owner: &str, state: &str) -> Value {
        json!({"sessionUpdate":"async_task_state_update","acpSessionId":owner,"asyncTaskId":"same","state":state})
    }
    #[test]
    fn each_terminal_transition_yields_one_result_with_the_reported_details() {
        for end in ["completed", "failed", "stopped", "cancelled"] {
            let mut state = State::default();
            state.connection(true);
            assert!(state.observe(&spawn("root")).is_none());
            state.observe(&json!({"sessionUpdate":"async_task_progress","acpSessionId":"root","asyncTaskId":"same","usage":{"totalTokens":5}}));
            assert!(state.observe(&update("root", "paused")).is_none());
            assert_eq!(state.tasks[0]["state"], "paused");
            let mut ended = update("root", end);
            ended["summary"] = json!("Done");
            let result = state.observe(&ended).unwrap();
            assert_eq!(result["state"], end);
            assert_eq!(result["name"], "Build");
            assert_eq!(result["toolCallId"], "launch");
            assert_eq!(result["summary"], "Done");
            assert_eq!(result["usage"]["totalTokens"], 5);
            assert!(result.get("canStop").is_none() && result.get("reason").is_none());
            // Duplicate terminal reports and relaunches of a finished job stay silent.
            assert!(state.observe(&update("root", "failed")).is_none());
            assert!(state.observe(&spawn("root")).is_none());
            state.record(&result, 9);
            assert_eq!(state.tasks[0]["resultSeq"], 9);
            assert!(result.get("resultSeq").is_none());
        }
    }
    #[test]
    fn connection_loss_resolves_live_jobs_once_and_a_relaunch_reactivates_them() {
        let mut state = State::default();
        state.connection(true);
        state.observe(&spawn("root"));
        state.observe(&spawn("child"));
        state.observe(&update("child", "completed"));
        let lost = state.connection(false);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0]["acpSessionId"], "root");
        assert_eq!(lost[0]["state"], "unknown");
        assert_eq!(lost[0]["reason"], "connection_ended");
        state.record(&lost[0], 4);
        // The runtime announces the loss twice (start and drop); the second is silent.
        assert!(state.connection(false).is_empty());
        assert!(state.connection(true).is_empty());
        // Persistence keeps the resolution, so a reload does not re-list the job as active.
        let mut restored: State =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(restored.snapshot()["tasks"][0]["resultSeq"], 4);
        restored.connection(true);
        assert!(restored.observe(&spawn("root")).is_none());
        assert_eq!(restored.tasks[0]["state"], "running");
        assert!(restored.tasks[0].get("resultSeq").is_none());
        let result = restored.observe(&update("root", "completed")).unwrap();
        assert_eq!(result["state"], "completed");
        // A record for an outcome that no longer matches the job's state is ignored.
        restored.record(&lost[0], 12);
        assert!(restored.tasks[0].get("resultSeq").is_none());
    }
    #[test]
    fn a_job_interrupted_by_a_service_restart_is_resolved_when_its_session_reconnects() {
        let mut state = State::default();
        state.connection(true);
        state.observe(&spawn("root"));
        let mut restored: State =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        // Before a runtime starts, the snapshot shows the job unresolved, not finished.
        let snapshot = restored.snapshot();
        assert_eq!(snapshot["tasks"][0]["state"], "unknown");
        assert!(snapshot["tasks"][0].get("resultSeq").is_none());
        assert_eq!(restored.connection(false).len(), 1);
    }
}
