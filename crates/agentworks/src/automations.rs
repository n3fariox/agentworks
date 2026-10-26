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
    /// Goose model provider profile the run session is bound to. Absent means
    /// the harness uses the host's own environment configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
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
/// representable: a manual or weekly scheduled trigger, and agent steps that
/// are carried out in order within one agent turn of the run's session.
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
    Schedule(Schedule),
}
/// A-01 weekly schedule: one local wall-clock time on selected weekdays in an
/// IANA time zone. Run now remains available alongside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Schedule {
    pub time_zone: String,
    /// Local 24-hour "HH:MM".
    pub at: String,
    /// ISO weekdays, 1 = Monday … 7 = Sunday.
    pub days: Vec<u8>,
    #[serde(default)]
    pub missed: Missed,
}
/// What happens to an occurrence the server could not dispatch on time
/// (stopped, restarting or busy for longer than `GRACE_MS`). Only the latest
/// such occurrence is considered; earlier ones are never replayed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Missed {
    #[default]
    Skip,
    RunOnce,
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
        if let Trigger::Schedule(schedule) = &mut self.trigger {
            schedule.time_zone = schedule.time_zone.trim().to_string();
            schedule.days.sort_unstable();
            schedule.days.dedup();
            schedule.plan()?;
        }
        Ok((self, prompt))
    }
}
/// Resolved form of a [`Schedule`]. Instants are Unix milliseconds.
pub struct Plan {
    zone: jiff::tz::TimeZone,
    hour: i8,
    minute: i8,
    days: [bool; 8],
}
impl Schedule {
    pub fn plan(&self) -> Result<Plan, &'static str> {
        let zone = jiff::tz::TimeZone::get(&self.time_zone)
            .map_err(|_| "Choose a known IANA time zone, e.g. Europe/Berlin")?;
        let (hour, minute) = self
            .at
            .split_once(':')
            .filter(|(h, m)| {
                h.len() == 2
                    && m.len() == 2
                    && h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit())
            })
            .and_then(|(h, m)| Some((h.parse::<i8>().ok()?, m.parse::<i8>().ok()?)))
            .filter(|(h, m)| *h < 24 && *m < 60)
            .ok_or("Use a time of day as HH:MM")?;
        let mut days = [false; 8];
        if self.days.is_empty() || self.days.iter().any(|d| !(1..=7).contains(d)) {
            return Err("Choose at least one weekday");
        }
        for day in &self.days {
            days[usize::from(*day)] = true;
        }
        Ok(Plan {
            zone,
            hour,
            minute,
            days,
        })
    }
}
impl Plan {
    fn date(&self, at: i64) -> Option<jiff::civil::Date> {
        Some(
            jiff::Timestamp::from_millisecond(at)
                .ok()?
                .to_zoned(self.zone.clone())
                .date(),
        )
    }
    /// The occurrence on `date`, if it is a selected weekday. A time skipped by
    /// a DST transition runs after the gap; a repeated time runs once, at its
    /// first instance.
    fn slot(&self, date: jiff::civil::Date) -> Option<i64> {
        let weekday = date.weekday().to_monday_one_offset() as usize;
        self.days[weekday].then_some(())?;
        let zoned = self
            .zone
            .to_zoned(date.at(self.hour, self.minute, 0, 0))
            .ok()?;
        Some(zoned.timestamp().as_millisecond())
    }
    /// The latest occurrence in `(after, until]`.
    pub fn latest(&self, after: i64, until: i64) -> Option<i64> {
        let mut date = self.date(until)?.tomorrow().ok()?;
        for _ in 0..10 {
            if let Some(slot) = self.slot(date) {
                if slot <= after {
                    return None;
                }
                if slot <= until {
                    return Some(slot);
                }
            }
            date = date.yesterday().ok()?;
        }
        None
    }
    /// The first occurrence strictly after `after`.
    pub fn next(&self, after: i64) -> Option<i64> {
        let mut date = self.date(after)?.yesterday().ok()?;
        for _ in 0..10 {
            if let Some(slot) = self.slot(date).filter(|slot| *slot > after) {
                return Some(slot);
            }
            date = date.tomorrow().ok()?;
        }
        None
    }
}
/// How late an occurrence may start and still count as on time.
pub const GRACE_MS: i64 = 5 * 60_000;
#[derive(Debug, PartialEq)]
pub enum Due {
    /// Start a run for `slot`. `late` is a catch-up under [`Missed::RunOnce`].
    Run {
        slot: i64,
        late: bool,
        earlier: bool,
    },
    /// Record `slot` as missed without starting anything.
    Missed { slot: i64, earlier: bool },
}
/// Decides the single occurrence to handle after `cursor`, the last handled
/// (or arming) instant. `earlier` reports that older occurrences were also
/// passed over; they are collapsed into this one and never run.
pub fn due(plan: &Plan, missed: Missed, cursor: i64, now: i64) -> Option<Due> {
    let slot = plan.latest(cursor, now)?;
    let earlier = plan.latest(cursor, slot - 1).is_some();
    let late = now - slot > GRACE_MS;
    Some(match (late, missed) {
        (false, _) | (true, Missed::RunOnce) => Due::Run {
            slot,
            late,
            earlier,
        },
        (true, Missed::Skip) => Due::Missed { slot, earlier },
    })
}
/// Durable scheduler state. `automation_schedules.cursor` is the instant up to
/// which occurrences are handled; `automation_slots` records each handled
/// occurrence once. Started runs are keyed `schedule:<slot>` in
/// `automation_runs`, so a crash between starting and recording cannot start
/// the same occurrence twice.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS automation_schedules(automation_id TEXT PRIMARY KEY,cursor INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS automation_slots(automation_id TEXT NOT NULL,slot INTEGER NOT NULL,request_id TEXT NOT NULL,outcome TEXT NOT NULL,detail TEXT,created INTEGER NOT NULL,PRIMARY KEY(automation_id,slot));
";
impl Definition {
    pub fn schedule(&self) -> Option<&Schedule> {
        match &self.workflow.as_ref()?.trigger {
            Trigger::Schedule(schedule) => Some(schedule),
            Trigger::Manual => None,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    name: String,
    provider: String,
    model: Option<String>,
    #[serde(default)]
    model_provider: Option<String>,
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
    let model_provider = input
        .model_provider
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    crate::model_providers::selected(app, host, &input.provider, model_provider.as_deref()).await?;
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
        model_provider,
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
/// Definition plus `nextRun`, the next scheduled instant while enabled, and
/// `modelProviderStatus` for a bound profile. The status reports whether the
/// profile is usable on the definition's host and whether a key is stored; it
/// never claims the provider will accept that key.
fn view(def: &Definition, now: i64, profiles: &[serde_json::Value]) -> serde_json::Value {
    let next = def
        .schedule()
        .filter(|_| def.enabled)
        .and_then(|s| s.plan().ok())
        .and_then(|plan| plan.next(now));
    let mut value = json!(def);
    value["nextRun"] = json!(next);
    if let Some(id) = &def.model_provider {
        value["modelProviderStatus"] = match profiles.iter().find(|p| p["id"] == id.as_str()) {
            None => json!({"available":false,"detail":"Model provider profile no longer exists"}),
            Some(p)
                if !p["hostIds"]
                    .as_array()
                    .is_some_and(|h| h.iter().any(|h| h == def.host_id.as_str())) =>
            {
                json!({"available":false,"name":p["name"],"detail":"Model provider profile is not available on this host"})
            }
            Some(p) => {
                json!({"available":true,"name":p["name"],"keyConfigured":p["keyConfigured"]})
            }
        };
    }
    value
}
/// Public profile views, read only when some definition is bound to one.
fn profiles_for(defs: &[&Definition]) -> Vec<serde_json::Value> {
    if defs.iter().all(|d| d.model_provider.is_none()) {
        return Vec::new();
    }
    agentworks_runtime::model_providers::dispatch(&json!({"operation":"list"}))
        .ok()
        .and_then(|mut r| r["profiles"].as_array_mut().map(std::mem::take))
        .unwrap_or_default()
}
pub async fn list(State(app): State<App>) -> ApiResult {
    let defs = app.store.lock().unwrap().automations().map_err(storage)?;
    let now = now() as i64;
    let profiles = profiles_for(&defs.iter().collect::<Vec<_>>());
    Ok(Json(json!(
        defs.iter()
            .map(|d| view(d, now, &profiles))
            .collect::<Vec<_>>()
    )))
}
/// Saves a definition and (re)arms its schedule at the current instant when
/// it is new, changed or resumed, so occurrences before saving or while paused
/// never start. Unchanged enabled schedules keep their cursor, which is what
/// makes downtime visible as missed occurrences.
fn save(app: &App, previous: Option<&Definition>, def: &Definition) -> ApiResult {
    let rearm = def.enabled
        && def.schedule().is_some()
        && previous.is_none_or(|p| !p.enabled || p.schedule() != def.schedule());
    let now = now() as i64;
    app.store
        .lock()
        .unwrap()
        .save_automation(def, rearm.then_some(now))
        .map_err(storage)?;
    Ok(Json(view(def, now, &profiles_for(&[def]))))
}
pub async fn create(State(app): State<App>, Json(input): Json<Input>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    let def = definition(&app, None, input).await?;
    save(&app, None, &def)
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
    let previous = app.store.lock().unwrap().automation(&id).map_err(storage)?;
    save(&app, previous.as_ref(), &def)
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
    start(&app, id, input.request_id, input.initiated_by, "Manual run").await
}
/// Starts one run under the caller's `run_lock`. Request IDs deduplicate.
async fn start(
    app: &App,
    id: String,
    request_id: String,
    initiated_by: Option<String>,
    label: &str,
) -> ApiResult {
    crate::service_restart::allow_start(app)?;
    let previous = app
        .store
        .lock()
        .unwrap()
        .run_session(&id, &request_id)
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
    // A bound profile replaces the host environment as the credential source,
    // so it must still exist and be enabled on this host. Whether its key is
    // accepted is only known once the provider is contacted.
    if let Some(profile) = def.model_provider.as_deref() {
        crate::model_providers::selected(app, &def.host_id, &def.provider, Some(profile))
            .await
            .map_err(|(_, message)| {
                fail(
                    StatusCode::CONFLICT,
                    format!("Model provider profile is unavailable: {message}"),
                )
            })?;
    }
    if def.host_id == "local"
        && (!installed(&def.provider)
            || (def.model_provider.is_none() && use_api_key() && !key_present(&def.provider)))
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
        request_id,
        initiated_by,
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
            title: Some(format!("{} · {label}", def.name)),
            model: def.model,
            model_provider: def.model_provider,
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
/// Dispatches due scheduled occurrences through the same path as Run now.
pub async fn scheduler(app: App) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        tick(&app).await;
    }
}
async fn tick(app: &App) {
    let Ok(defs) = app.store.lock().unwrap().automations() else {
        return;
    };
    for id in defs
        .into_iter()
        .filter(|d| d.enabled && d.schedule().is_some())
        .map(|d| d.id)
    {
        // Edits, pauses and deletion hold this lock, so the definition and
        // cursor read below are current.
        let _guard = app.run_lock.lock().await;
        // While draining for restart nothing is claimed; the occurrence is
        // handled after startup under its missed-run policy.
        if crate::service_restart::allow_start(app).is_err() {
            return;
        }
        let _ = dispatch(app, &id, now() as i64).await;
    }
}
async fn dispatch(
    app: &App,
    id: &str,
    now: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (def, cursor) = {
        let mut store = app.store.lock().unwrap();
        let Some(def) = store.automation(id)?.filter(|d| d.enabled) else {
            return Ok(());
        };
        let Some(cursor) = store.schedule_cursor(id)? else {
            // Armed by an older path or a lost write: start counting from now.
            store.arm_schedule(id, now)?;
            return Ok(());
        };
        (def, cursor)
    };
    let Some(schedule) = def.schedule() else {
        return Ok(());
    };
    let Some(due) = due(&schedule.plan()?, schedule.missed, cursor, now) else {
        return Ok(());
    };
    let (slot, earlier) = match due {
        Due::Run { slot, earlier, .. } | Due::Missed { slot, earlier } => (slot, earlier),
    };
    let request = format!("schedule:{slot}");
    let collapsed = if earlier {
        " Earlier missed occurrences were not run."
    } else {
        ""
    };
    let started = app
        .store
        .lock()
        .unwrap()
        .run_session(id, &request)?
        .is_some();
    let (outcome, detail) = match due {
        // Started before a crash or lost write; record it without restarting.
        _ if started => ("started", None),
        Due::Missed { .. } => (
            "missed",
            Some(format!(
                "Not started on time; the missed-run policy skips it.{collapsed}"
            )),
        ),
        Due::Run { late, .. } => {
            match start(app, id.into(), request.clone(), None, "Scheduled run").await {
                Ok(_) => (
                    "started",
                    late.then(|| format!("Started late under the run-once policy.{collapsed}")),
                ),
                Err((_, message)) => ("skipped", Some(message)),
            }
        }
    };
    app.store
        .lock()
        .unwrap()
        .claim_slot(id, slot, &request, outcome, detail.as_deref(), now)?;
    Ok(())
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
    fn weekly(time_zone: &str, at: &str, days: &[u8]) -> Schedule {
        Schedule {
            time_zone: time_zone.into(),
            at: at.into(),
            days: days.into(),
            missed: Missed::Skip,
        }
    }
    fn ms(text: &str) -> i64 {
        text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }
    #[test]
    fn schedule_triggers_validate_and_normalise() {
        let parsed: Workflow = serde_json::from_str(r#"{"version":1,"trigger":{"kind":"schedule","timeZone":" Europe/Berlin ","at":"09:30","days":[5,1,1]},"steps":[{"kind":"agent","text":"x"}]}"#).unwrap();
        let (workflow, _) = parsed.compile().unwrap();
        assert_eq!(
            workflow.trigger,
            Trigger::Schedule(weekly("Europe/Berlin", "09:30", &[1, 5]))
        );
        for bad in [
            weekly("Mars/Olympus", "09:00", &[1]),
            weekly("", "09:00", &[1]),
            weekly("UTC", "9:00", &[1]),
            weekly("UTC", "24:00", &[1]),
            weekly("UTC", "12:60", &[1]),
            weekly("UTC", "+1:00", &[1]),
            weekly("UTC", "09:00", &[]),
            weekly("UTC", "09:00", &[0]),
            weekly("UTC", "09:00", &[8]),
        ] {
            let workflow = Workflow {
                trigger: Trigger::Schedule(bad.clone()),
                ..recipe(vec![agent("x")])
            };
            assert!(workflow.compile().is_err(), "{bad:?}");
        }
        for invalid in [
            r#"{"kind":"schedule","timeZone":"UTC","at":"09:00","days":[1],"cron":"* * * * *"}"#,
            r#"{"kind":"schedule","timeZone":"UTC","at":"09:00","days":[1],"missed":"all"}"#,
        ] {
            assert!(
                serde_json::from_str::<Trigger>(invalid).is_err(),
                "{invalid}"
            );
        }
        let run_once: Trigger = serde_json::from_str(
            r#"{"kind":"schedule","timeZone":"UTC","at":"09:00","days":[1],"missed":"runOnce"}"#,
        )
        .unwrap();
        assert!(matches!(
            run_once,
            Trigger::Schedule(Schedule {
                missed: Missed::RunOnce,
                ..
            })
        ));
    }
    #[test]
    fn occurrences_follow_local_weekdays_and_time_zone() {
        // 2026-10-26 is a Monday; Berlin is UTC+1 after the 2026-10-25 change.
        let plan = weekly("Europe/Berlin", "09:30", &[1, 3]).plan().unwrap();
        let monday = ms("2026-10-26T08:30:00Z");
        let wednesday = ms("2026-10-28T08:30:00Z");
        assert_eq!(plan.next(ms("2026-10-25T12:00:00Z")), Some(monday));
        assert_eq!(plan.next(monday), Some(wednesday));
        assert_eq!(plan.next(monday - 1), Some(monday));
        assert_eq!(plan.latest(monday - 1, wednesday + 1), Some(wednesday));
        assert_eq!(plan.latest(monday, wednesday - 1), None);
        assert_eq!(plan.latest(0, monday), Some(monday));
        // Sunday 23:30 in Auckland is Sunday morning UTC.
        let plan = weekly("Pacific/Auckland", "23:30", &[7]).plan().unwrap();
        assert_eq!(
            plan.next(ms("2026-10-24T00:00:00Z")),
            Some(ms("2026-10-25T10:30:00Z"))
        );
    }
    #[test]
    fn daylight_saving_gaps_run_after_and_repeats_run_once() {
        let plan = weekly("America/New_York", "02:30", &[1, 2, 3, 4, 5, 6, 7])
            .plan()
            .unwrap();
        // 2026-03-08 02:30 does not exist; it runs at 03:30 EDT.
        assert_eq!(
            plan.next(ms("2026-03-08T05:00:00Z")),
            Some(ms("2026-03-08T07:30:00Z"))
        );
        let plan = weekly("America/New_York", "01:30", &[1, 2, 3, 4, 5, 6, 7])
            .plan()
            .unwrap();
        // 2026-11-01 01:30 happens twice; only the first (EDT) instance runs.
        let first = ms("2026-11-01T05:30:00Z");
        assert_eq!(plan.next(ms("2026-11-01T04:00:00Z")), Some(first));
        assert_eq!(plan.next(first), Some(ms("2026-11-02T06:30:00Z")));
    }
    #[test]
    fn due_applies_grace_and_missed_run_policy() {
        let plan = weekly("UTC", "09:00", &[1, 2, 3, 4, 5, 6, 7])
            .plan()
            .unwrap();
        let slot = ms("2026-10-26T09:00:00Z");
        let armed = slot - 3_600_000;
        assert_eq!(due(&plan, Missed::Skip, armed, slot - 1), None);
        assert_eq!(due(&plan, Missed::Skip, slot, slot + 60_000), None);
        assert_eq!(
            due(&plan, Missed::Skip, armed, slot + GRACE_MS),
            Some(Due::Run {
                slot,
                late: false,
                earlier: false
            })
        );
        assert_eq!(
            due(&plan, Missed::Skip, armed, slot + GRACE_MS + 1),
            Some(Due::Missed {
                slot,
                earlier: false
            })
        );
        assert_eq!(
            due(&plan, Missed::RunOnce, armed, slot + GRACE_MS + 1),
            Some(Due::Run {
                slot,
                late: true,
                earlier: false
            })
        );
        // Three days of downtime collapse into the latest occurrence only.
        let latest = slot + 2 * 86_400_000;
        assert_eq!(
            due(&plan, Missed::RunOnce, armed, latest + 3_600_000),
            Some(Due::Run {
                slot: latest,
                late: true,
                earlier: true
            })
        );
        // Coming back on time for today's occurrence runs it normally.
        assert_eq!(
            due(&plan, Missed::Skip, armed, latest + 1_000),
            Some(Due::Run {
                slot: latest,
                late: false,
                earlier: true
            })
        );
    }
    #[test]
    fn slot_ledger_keeps_first_outcome_and_advances_cursor() {
        let path = std::env::temp_dir().join(format!(
            "agentworks-schedule-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            let mut def: Definition = serde_json::from_str(r#"{"id":"a","name":"n","provider":"demo","model":null,"cwd":"/","locationId":"l","hostId":"local","projectId":null,"prompt":"p","enabled":true,"created":1,"updated":1}"#).unwrap();
            def.workflow = Some(Workflow {
                trigger: Trigger::Schedule(weekly("UTC", "09:00", &[1])),
                ..recipe(vec![agent("p")])
            });
            store.save_automation(&def, Some(100)).unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), Some(100));
            // Saving without re-arming keeps the cursor.
            store.save_automation(&def, None).unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), Some(100));
            store
                .claim_slot("a", 500, "schedule:500", "missed", Some("late"), 900)
                .unwrap();
            store
                .claim_slot("a", 500, "schedule:500", "started", None, 950)
                .unwrap();
            store
                .claim_slot("a", 400, "schedule:400", "skipped", None, 960)
                .unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), Some(500));
            let runs = store.automation_runs("a").unwrap();
            assert_eq!(runs.len(), 2);
            assert_eq!(runs[1]["status"], "missed");
            assert_eq!(runs[1]["scheduledFor"], 500);
            assert_eq!(runs[1]["detail"], "late");
            assert!(runs[1]["sessionId"].is_null());
        }
        {
            // Durable across reopen; turning the trigger manual drops the cursor.
            let mut store = crate::store::Store::open(&path).unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), Some(500));
            let mut def = store.automation("a").unwrap().unwrap();
            def.workflow = Some(recipe(vec![agent("p")]));
            store.save_automation(&def, Some(1)).unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), None);
            assert_eq!(store.automation_runs("a").unwrap().len(), 2);
        }
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
    #[test]
    fn legacy_definitions_round_trip_without_workflow() {
        let legacy = r#"{"id":"a","name":"n","provider":"demo","model":null,"cwd":"/","locationId":"l","hostId":"local","projectId":null,"prompt":"p","enabled":true,"created":1,"updated":1}"#;
        let def: Definition = serde_json::from_str(legacy).unwrap();
        assert!(def.workflow.is_none());
        assert!(!serde_json::to_string(&def).unwrap().contains("workflow"));
        assert!(def.model_provider.is_none());
        assert!(
            !serde_json::to_string(&def)
                .unwrap()
                .contains("modelProvider")
        );
        assert!(view(&def, 0, &[]).get("modelProviderStatus").is_none());
    }
    #[test]
    fn bound_profiles_persist_and_report_availability_without_claiming_auth() {
        let mut def: Definition = serde_json::from_str(r#"{"id":"a","name":"n","provider":"goose","model":null,"modelProvider":"p1","cwd":"/","locationId":"l","hostId":"local","projectId":null,"prompt":"p","enabled":true,"created":1,"updated":1}"#).unwrap();
        assert_eq!(def.model_provider.as_deref(), Some("p1"));
        let again: Definition =
            serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert_eq!(again.model_provider.as_deref(), Some("p1"));
        let profiles =
            [json!({"id":"p1","name":"Gateway","hostIds":["local"],"keyConfigured":false})];
        let status = &view(&def, 0, &profiles)["modelProviderStatus"];
        assert_eq!(status["available"], true);
        assert_eq!(status["keyConfigured"], false);
        def.host_id = "remote".into();
        let status = &view(&def, 0, &profiles)["modelProviderStatus"];
        assert_eq!(status["available"], false);
        assert!(status.get("keyConfigured").is_none());
        let status = &view(&def, 0, &[])["modelProviderStatus"];
        assert_eq!(status["available"], false);
        assert!(
            status["detail"]
                .as_str()
                .unwrap()
                .contains("no longer exists")
        );
    }
}
