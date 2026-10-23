use crate::{
    ApiResult, App, Create, SessionData, create_session, fail, installed, key_present, use_api_key,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub use agentworks_session::origins::AutomationOrigin as Origin;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub model: Option<String>,
    pub cwd: String,
    pub location_id: String,
    pub host_id: String,
    pub project_id: Option<String>,
    pub prompt: String,
    /// Recipe form of the definition. `prompt` is derived from it, so runs and
    /// older clients keep using the single prompt. Absent on legacy definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<Workflow>,
    pub enabled: bool,
    pub created: u64,
    pub updated: u64,
}
/// "When [trigger], do [steps]". Only what the runtime can perform is
/// representable: a manual trigger, and agent steps that are carried out in
/// order within one agent turn of the run's session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Workflow {
    pub version: u32,
    pub trigger: Trigger,
    pub steps: Vec<Step>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Trigger {
    Manual,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Step {
    Agent { text: String },
}
pub const MAX_STEPS: usize = 20;
const MAX_PROMPT: usize = 32768;
impl Workflow {
    /// Trims steps, rejects empty or oversized recipes and returns the
    /// normalised workflow with the prompt a run will send.
    pub fn compile(mut self) -> Result<(Workflow, String), &'static str> {
        if self.version != 1 {
            return Err("Unsupported workflow version");
        }
        for Step::Agent { text } in &mut self.steps {
            *text = text.trim().to_string();
        }
        if self.steps.is_empty()
            || self.steps.len() > MAX_STEPS
            || self
                .steps
                .iter()
                .any(|Step::Agent { text }| text.is_empty())
        {
            return Err("Add 1 to 20 steps, each with instructions");
        }
        let prompt = match self.steps.as_slice() {
            [Step::Agent { text }] => text.clone(),
            steps => {
                let mut prompt = String::from(
                    "Complete these steps in order. Finish each step before starting the next, and stop and report if a step cannot be completed.\n",
                );
                for (index, Step::Agent { text }) in steps.iter().enumerate() {
                    prompt.push_str(&format!("\nStep {}:\n{}\n", index + 1, text));
                }
                prompt
            }
        };
        if prompt.len() > MAX_PROMPT {
            return Err("Use instructions up to 32 KB in total");
        }
        Ok((self, prompt))
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    name: String,
    provider: String,
    model: Option<String>,
    cwd: String,
    host_id: Option<String>,
    project_id: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    workflow: Option<Workflow>,
}
fn enabled() -> bool {
    true
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
async fn definition(
    app: &App,
    id: Option<&str>,
    input: Input,
) -> Result<Definition, (StatusCode, String)> {
    let (workflow, prompt) = match input.workflow {
        Some(workflow) => {
            let (workflow, prompt) = workflow
                .compile()
                .map_err(|message| fail(StatusCode::BAD_REQUEST, message))?;
            (Some(workflow), prompt)
        }
        None => (None, input.prompt),
    };
    let name = input.name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || prompt.trim().is_empty()
        || prompt.len() > MAX_PROMPT
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a name up to 80 characters and instructions up to 32 KB",
        ));
    }
    if !agentworks_runtime::remote::PROVIDERS.contains(&input.provider.as_str()) {
        return Err(fail(StatusCode::BAD_REQUEST, "Unknown harness"));
    }
    let model = input
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    if model.as_ref().is_some_and(|m| {
        m.len() > 256
            || (input.provider == "opencode" && !agentworks_runtime::provider::is_free_model(m))
    }) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Invalid model; OpenCode supports free models only",
        ));
    }
    let host = input.host_id.as_deref().unwrap_or("local");
    let cwd = crate::hosts::directory_on(app, host, &input.provider, &input.cwd).await?;
    let mut catalog = app.catalog.lock().unwrap();
    if input
        .project_id
        .as_deref()
        .is_some_and(|p| !catalog.contains(p))
    {
        return Err(fail(StatusCode::BAD_REQUEST, "Project not found"));
    }
    let location = catalog.location_on(host, &cwd).map_err(|_| {
        fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save directory",
        )
    })?;
    if let Some(project) = &input.project_id {
        catalog
            .attach_location(project, &location.id)
            .map_err(|_| {
                fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not save project directory",
                )
            })?;
    }
    let created = if let Some(id) = id {
        app.store
            .lock()
            .unwrap()
            .automation(id)
            .map_err(storage)?
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Automation not found"))?
            .created
    } else {
        now()
    };
    Ok(Definition {
        id: id
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: name.into(),
        provider: input.provider,
        model,
        cwd: cwd.to_string_lossy().into_owned(),
        location_id: location.id,
        host_id: host.into(),
        project_id: input.project_id,
        prompt,
        workflow,
        enabled: input.enabled,
        created,
        updated: now(),
    })
}
fn storage(_error: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not access automation storage",
    )
}
pub async fn list(State(app): State<App>) -> ApiResult {
    Ok(Json(json!(
        app.store.lock().unwrap().automations().map_err(storage)?
    )))
}
pub async fn create(State(app): State<App>, Json(input): Json<Input>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    let def = definition(&app, None, input).await?;
    app.store
        .lock()
        .unwrap()
        .save_automation(&def)
        .map_err(storage)?;
    Ok(Json(json!(def)))
}
pub async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Input>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _guard = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    let def = definition(&app, Some(&id), input).await?;
    app.store
        .lock()
        .unwrap()
        .save_automation(&def)
        .map_err(storage)?;
    Ok(Json(json!(def)))
}
pub async fn delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _guard = app.run_lock.lock().await;
    app.store
        .lock()
        .unwrap()
        .delete_automation(&id)
        .map_err(storage)?;
    Ok(Json(json!({"ok":true})))
}
pub async fn runs(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(json!(
        app.store
            .lock()
            .unwrap()
            .automation_runs(&id)
            .map_err(storage)?
    )))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(skip)]
    pub(crate) initiated_by: Option<String>,
    pub(crate) request_id: String,
}
pub async fn run(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Run>,
) -> ApiResult {
    if input.request_id.is_empty()
        || input.request_id.len() > 100
        || input.request_id.chars().any(char::is_control)
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Supply a valid run request ID",
        ));
    }
    let _guard = app.run_lock.lock().await;
    crate::service_restart::allow_start(&app)?;
    let previous = app
        .store
        .lock()
        .unwrap()
        .run_session(&id, &input.request_id)
        .map_err(storage)?;
    if let Some(session_id) = previous {
        let sessions = app.sessions.lock().unwrap();
        let session = sessions.get(&session_id).ok_or_else(|| {
            fail(
                StatusCode::GONE,
                "This run's session was deleted; use a new request ID for a new run",
            )
        })?;
        return Ok(Json(json!({"session":session.meta(),"duplicate":true})));
    }
    let def = app
        .store
        .lock()
        .unwrap()
        .automation(&id)
        .map_err(storage)?
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Automation not found"))?;
    if !def.enabled {
        return Err(fail(StatusCode::CONFLICT, "This automation is paused"));
    }
    if def.host_id == "local"
        && (!installed(&def.provider) || (use_api_key() && !key_present(&def.provider)))
    {
        return Err(fail(
            StatusCode::CONFLICT,
            "Harness or its authentication is unavailable",
        ));
    }
    if app.sessions.lock().unwrap().values().any(|s| {
        let d = s.data.lock().unwrap();
        d.automation
            .as_ref()
            .is_some_and(|a| a.automation_id == id && a.outcome.is_none())
            && matches!(
                d.status.as_str(),
                "starting" | "running" | "waiting" | "resuming" | "configuring" | "cancelling"
            )
    }) {
        return Err(fail(
            StatusCode::CONFLICT,
            "This automation already has an active run",
        ));
    }
    let origin = Origin {
        automation_id: id,
        name: def.name.clone(),
        run_id: Uuid::new_v4().to_string(),
        request_id: input.request_id,
        initiated_by: input.initiated_by,
        outcome: None,
    };
    let Json(session) = create_session(
        State(app.clone()),
        Json(Create {
            resume_policy: None,
            inherited_resume: None,
            workspace: None,
            request_id: None,
            agent_access: None,
            role: None,
            inherited_role: None,
            provider: def.provider,
            cwd: def.cwd,
            title: Some(format!("{} · Manual run", def.name)),
            model: def.model,
            project_id: def.project_id,
            host_id: Some(def.host_id),
            external_id: None,
            external_host: false,
            automation: Some(origin),
            queued_prompt: Some(def.prompt),
            control_origin: None,
            fork: None,
        }),
    )
    .await?;
    Ok(Json(json!({"session":session,"duplicate":false})))
}
pub fn run_status(data: &SessionData) -> &str {
    data.automation
        .as_ref()
        .and_then(|a| a.outcome.as_deref())
        .unwrap_or(match data.status.as_str() {
            "idle" => "starting",
            "suspended" => "interrupted",
            "error" => "failed",
            other => other,
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn agent(text: &str) -> Step {
        Step::Agent { text: text.into() }
    }
    fn recipe(steps: Vec<Step>) -> Workflow {
        Workflow {
            version: 1,
            trigger: Trigger::Manual,
            steps,
        }
    }
    #[test]
    fn single_step_prompt_is_verbatim() {
        let (workflow, prompt) = recipe(vec![agent("  Review changes \n")])
            .compile()
            .unwrap();
        assert_eq!(prompt, "Review changes");
        assert_eq!(workflow.steps, vec![agent("Review changes")]);
    }
    #[test]
    fn steps_compile_in_order() {
        let (_, prompt) = recipe(vec![agent("Run tests"), agent("Summarise failures")])
            .compile()
            .unwrap();
        let first = prompt.find("Step 1:\nRun tests").unwrap();
        let second = prompt.find("Step 2:\nSummarise failures").unwrap();
        assert!(prompt.starts_with("Complete these steps in order."));
        assert!(first < second);
    }
    #[test]
    fn rejects_empty_oversized_and_unknown() {
        assert!(recipe(vec![]).compile().is_err());
        assert!(recipe(vec![agent("ok"), agent("   ")]).compile().is_err());
        assert!(
            recipe((0..=MAX_STEPS).map(|_| agent("x")).collect())
                .compile()
                .is_err()
        );
        assert!(
            recipe(vec![agent(&"x".repeat(MAX_PROMPT + 1))])
                .compile()
                .is_err()
        );
        assert!(
            Workflow {
                version: 2,
                ..recipe(vec![agent("x")])
            }
            .compile()
            .is_err()
        );
        for invalid in [
            r#"{"version":1,"trigger":{"kind":"schedule","cron":"* * * * *"},"steps":[{"kind":"agent","text":"x"}]}"#,
            r#"{"version":1,"trigger":{"kind":"manual"},"steps":[{"kind":"shell","text":"rm -rf /"}]}"#,
            r#"{"version":1,"trigger":{"kind":"manual"},"steps":[{"kind":"agent","text":"x","approval":true}]}"#,
        ] {
            assert!(
                serde_json::from_str::<Workflow>(invalid).is_err(),
                "{invalid}"
            );
        }
    }
    #[test]
    fn legacy_definitions_round_trip_without_workflow() {
        let legacy = r#"{"id":"a","name":"n","provider":"demo","model":null,"cwd":"/","locationId":"l","hostId":"local","projectId":null,"prompt":"p","enabled":true,"created":1,"updated":1}"#;
        let def: Definition = serde_json::from_str(legacy).unwrap();
        assert!(def.workflow.is_none());
        assert!(!serde_json::to_string(&def).unwrap().contains("workflow"));
    }
}
