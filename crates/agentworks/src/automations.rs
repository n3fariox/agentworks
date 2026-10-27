use crate::{
    ApiResult, App, Create, SessionData, communication as mail, create_session, fail, installed,
    key_present, use_api_key,
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
    /// The agent that created the definition through MCP. Absent when a human
    /// created it. Kept across edits; unattended runs need its live grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Author>,
    /// Incremented on every save; agents pass it back to update. Legacy
    /// definitions start at 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revision: u64,
    /// When a one-time trigger was handled and the definition was paused
    /// under [`AfterRun::Complete`]. Cleared by saving or resuming it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Author {
    pub id: String,
    pub title: String,
}
/// "When [trigger], do [steps]". Only what the runtime can perform is
/// representable: a manual, weekly or one-time trigger, and agent steps that
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
    Once(Once),
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
/// One local date and time in an IANA time zone. The definition is completed
/// or deleted once that occurrence is handled, whatever its outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Once {
    pub time_zone: String,
    /// Local "YYYY-MM-DDTHH:MM".
    pub at: String,
    #[serde(default)]
    pub missed: Missed,
    #[serde(default)]
    pub after_run: AfterRun,
}
/// What happens to a one-time definition after its occurrence is handled.
/// Run and occurrence history is kept either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AfterRun {
    /// Pause it and record `completed`; it can be rescheduled.
    #[default]
    Complete,
    Delete,
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
        match &mut self.trigger {
            Trigger::Manual => {}
            Trigger::Schedule(schedule) => {
                schedule.time_zone = schedule.time_zone.trim().to_string();
                schedule.days.sort_unstable();
                schedule.days.dedup();
                schedule.plan()?;
            }
            Trigger::Once(once) => {
                once.time_zone = once.time_zone.trim().to_string();
                once.instant()?;
            }
        }
        Ok((self, prompt))
    }
}
fn zone(name: &str) -> Result<jiff::tz::TimeZone, &'static str> {
    jiff::tz::TimeZone::get(name).map_err(|_| "Choose a known IANA time zone, e.g. Europe/Berlin")
}
impl Once {
    /// The occurrence as Unix milliseconds. A local time skipped by a DST
    /// transition runs after the gap; a repeated one at its first instance.
    pub fn instant(&self) -> Result<i64, &'static str> {
        let zone = zone(&self.time_zone)?;
        let local: jiff::civil::DateTime = Some(&self.at)
            .filter(|at| {
                at.len() == 16
                    && at.bytes().enumerate().all(|(i, b)| match i {
                        4 | 7 => b == b'-',
                        10 => b == b'T',
                        13 => b == b':',
                        _ => b.is_ascii_digit(),
                    })
            })
            .and_then(|at| at.parse().ok())
            .ok_or("Use a date and time as YYYY-MM-DDTHH:MM")?;
        Ok(zone
            .to_zoned(local)
            .map_err(|_| "Use a date and time as YYYY-MM-DDTHH:MM")?
            .timestamp()
            .as_millisecond())
    }
}
impl Trigger {
    /// Occurrence plan and missed-run policy of a timed trigger; `None` for
    /// a manual one.
    pub fn timing(&self) -> Option<Result<(Plan, Missed), &'static str>> {
        match self {
            Trigger::Manual => None,
            Trigger::Schedule(schedule) => Some(schedule.plan().map(|p| (p, schedule.missed))),
            Trigger::Once(once) => Some(once.instant().map(|at| (Plan::Once(at), once.missed))),
        }
    }
}
/// Resolved occurrences of a timed trigger. Instants are Unix milliseconds.
pub enum Plan {
    Weekly(Weekly),
    Once(i64),
}
impl Plan {
    /// The latest occurrence in `(after, until]`.
    pub fn latest(&self, after: i64, until: i64) -> Option<i64> {
        match self {
            Plan::Weekly(weekly) => weekly.latest(after, until),
            Plan::Once(at) => (after < *at && *at <= until).then_some(*at),
        }
    }
    /// The first occurrence strictly after `after`.
    pub fn next(&self, after: i64) -> Option<i64> {
        match self {
            Plan::Weekly(weekly) => weekly.next(after),
            Plan::Once(at) => (*at > after).then_some(*at),
        }
    }
}
/// Resolved form of a [`Schedule`].
pub struct Weekly {
    zone: jiff::tz::TimeZone,
    hour: i8,
    minute: i8,
    days: [bool; 8],
}
impl Schedule {
    pub fn plan(&self) -> Result<Plan, &'static str> {
        let zone = zone(&self.time_zone)?;
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
        Ok(Plan::Weekly(Weekly {
            zone,
            hour,
            minute,
            days,
        }))
    }
}
impl Weekly {
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
/// Bounds for scheduling or resuming a one-time trigger, from now.
const ONCE_LEAD_MS: i64 = 60_000;
const ONCE_HORIZON_MS: i64 = 366 * 86_400_000;
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
/// the same occurrence twice. `automation_agent_requests` makes agent
/// mutations idempotent per `(actor, requestId)` (schema v12).
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS automation_schedules(automation_id TEXT PRIMARY KEY,cursor INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS automation_slots(automation_id TEXT NOT NULL,slot INTEGER NOT NULL,request_id TEXT NOT NULL,outcome TEXT NOT NULL,detail TEXT,created INTEGER NOT NULL,PRIMARY KEY(automation_id,slot));
CREATE TABLE IF NOT EXISTS automation_agent_requests(actor TEXT NOT NULL,request_id TEXT NOT NULL,operation TEXT NOT NULL,payload TEXT NOT NULL,result TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(actor,request_id));
";
impl Definition {
    /// The trigger when it is timed (weekly or one-time), which gives the
    /// definition a scheduler cursor.
    pub fn timed(&self) -> Option<&Trigger> {
        self.workflow
            .as_ref()
            .map(|w| &w.trigger)
            .filter(|t| !matches!(t, Trigger::Manual))
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
/// Validates `input` into a definition. Edits keep `previous`'s identity,
/// creation time and author; `author` only applies to new definitions.
async fn definition(
    app: &App,
    previous: Option<&Definition>,
    input: Input,
    author: Option<Author>,
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
    Ok(Definition {
        id: previous.map_or_else(|| Uuid::new_v4().to_string(), |p| p.id.clone()),
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
        created: previous.map_or_else(now, |p| p.created),
        updated: now(),
        created_by: previous.map_or(author, |p| p.created_by.clone()),
        revision: previous.map_or(1, |p| p.revision + 1),
        completed: None,
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
        .timed()
        .filter(|_| def.enabled)
        .and_then(|t| t.timing()?.ok())
        .and_then(|(plan, _)| plan.next(now));
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
    let defs = app
        .store
        .lock()
        .unwrap()
        .usable_automations()
        .map_err(storage)?;
    let now = now() as i64;
    let profiles = profiles_for(&defs.iter().collect::<Vec<_>>());
    Ok(Json(json!(
        defs.iter()
            .map(|d| view(d, now, &profiles))
            .collect::<Vec<_>>()
    )))
}
/// An agent's mutation request, recorded in the same transaction as its
/// effect so a retry with the same request ID replays `result` instead of
/// acting twice.
pub struct Ledger<'a> {
    pub actor: &'a str,
    pub request_id: &'a str,
    pub operation: &'a str,
    pub payload: &'a str,
    pub result: &'a str,
}
/// Saves a definition and (re)arms its schedule at the current instant when
/// it is new, changed or resumed, so occurrences before saving or while paused
/// never start. Unchanged enabled schedules keep their cursor, which is what
/// makes downtime visible as missed occurrences.
fn save(
    app: &App,
    previous: Option<&Definition>,
    def: &Definition,
    request: Option<(&str, &str, &str, &str)>,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let rearm = def.enabled
        && def.timed().is_some()
        && previous.is_none_or(|p| !p.enabled || p.timed() != def.timed());
    let now = now() as i64;
    // Arming at `now` would silently skip a one-time occurrence in the past.
    if rearm && let Some(Trigger::Once(once)) = def.timed() {
        let at = once
            .instant()
            .map_err(|m| fail(StatusCode::BAD_REQUEST, m))?;
        if at < now + ONCE_LEAD_MS || at > now + ONCE_HORIZON_MS {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "Choose a one-time run at least a minute from now and within a year",
            ));
        }
    }
    let value = view(def, now, &profiles_for(&[def]));
    let result = value.to_string();
    let ledger = request.map(|(actor, request_id, operation, payload)| Ledger {
        actor,
        request_id,
        operation,
        payload,
        result: &result,
    });
    app.store
        .lock()
        .unwrap()
        .save_automation(def, rearm.then_some(now), ledger.as_ref())
        .map_err(storage)?;
    Ok(value)
}
pub async fn create(State(app): State<App>, Json(input): Json<Input>) -> ApiResult {
    let _workspace = app.workspace_lock.lock().await;
    let def = definition(&app, None, input, None).await?;
    save(&app, None, &def, None).map(Json)
}
pub async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Input>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _guard = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    let previous = existing(&app, &id)?;
    let def = definition(&app, Some(&previous), input, None).await?;
    save(&app, Some(&previous), &def, None).map(Json)
}
fn existing(app: &App, id: &str) -> Result<Definition, (StatusCode, String)> {
    app.store
        .lock()
        .unwrap()
        .automation(id)
        .map_err(storage)?
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Automation not found"))
}
pub async fn delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let _guard = app.run_lock.lock().await;
    app.store
        .lock()
        .unwrap()
        .delete_automation(&id, None)
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
    let Ok(defs) = app.store.lock().unwrap().usable_automations() else {
        return;
    };
    for id in defs
        .into_iter()
        .filter(|d| d.enabled && d.timed().is_some())
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
    let Some(trigger) = def.timed() else {
        return Ok(());
    };
    let (plan, missed) = trigger.timing().ok_or("Untimed trigger")??;
    let Some(due) = due(&plan, missed, cursor, now) else {
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
        Due::Run { .. } if let Err(reason) = author_permits(app, &def) => ("skipped", Some(reason)),
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
    // A one-time definition is finished whatever the outcome; it is never
    // retried. This commits with the occurrence record.
    let finish = match trigger {
        Trigger::Once(Once {
            after_run: AfterRun::Complete,
            ..
        }) => Some(Finish::Complete(Box::new(Definition {
            enabled: false,
            completed: Some(now as u64),
            updated: now as u64,
            revision: def.revision + 1,
            ..def.clone()
        }))),
        Trigger::Once(Once {
            after_run: AfterRun::Delete,
            ..
        }) => Some(Finish::Delete),
        _ => None,
    };
    app.store.lock().unwrap().claim_slot(
        id,
        slot,
        &request,
        outcome,
        detail.as_deref(),
        now,
        finish.as_ref(),
    )?;
    Ok(())
}
/// What happens to a one-time definition when its occurrence is claimed.
pub enum Finish {
    Complete(Box<Definition>),
    Delete,
}
/// Unattended runs of an agent-created definition act on that agent's
/// authority, so it is checked again at every occurrence: the author must
/// still exist in the definition's project and hold manageAutomations and
/// startSessions. Human-created definitions run as before.
fn author_permits(app: &App, def: &Definition) -> Result<(), String> {
    let Some(author) = &def.created_by else {
        return Ok(());
    };
    let actor = match author.id.strip_prefix(crate::controllers::ACTOR_PREFIX) {
        Some(id) => app
            .store
            .lock()
            .unwrap()
            .controller(id)
            .map_err(|(_, message)| message)?
            .map(|c| mail::Actor::controller(&c)),
        None => app
            .sessions
            .lock()
            .unwrap()
            .get(&author.id)
            .map(mail::Actor::of),
    }
    .ok_or("The agent that created this automation no longer exists or was revoked")?;
    if actor.project.is_none() || actor.project != def.project_id {
        return Err("The agent that created this automation is no longer in its project".into());
    }
    for capability in ["manage_automations", "start_sessions"] {
        mail::require_actor(app, &actor, capability).map_err(|_| {
            format!("The agent that created this automation no longer holds {capability}")
        })?;
    }
    Ok(())
}
/// Agent-created definitions allowed per project, enabled or not.
pub const MAX_AGENT_DEFINITIONS: usize = 20;
/// Fields an agent supplies to create or replace a definition. Project and
/// host come from the caller's credential, never from input.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentSave {
    /// Read from the raw arguments for the request ledger.
    #[serde(rename = "requestId")]
    _request_id: serde::de::IgnoredAny,
    #[serde(default)]
    automation_id: Option<String>,
    #[serde(default)]
    revision: Option<u64>,
    name: String,
    provider: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_provider: Option<String>,
    cwd: String,
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    workflow: Option<Workflow>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentTarget {
    #[serde(default, rename = "requestId")]
    _request_id: Option<serde::de::IgnoredAny>,
    automation_id: String,
    #[serde(default)]
    enabled: Option<bool>,
}
fn invalid(message: &str) -> (StatusCode, String) {
    fail(StatusCode::BAD_REQUEST, message)
}
/// The caller's project, provided it may read automations there.
fn agent_scope(app: &App, a: &mail::Actor) -> mail::Result<(mail::Access, String)> {
    let access = mail::actor_access(app, a)?;
    if !access.run_automations && !access.manage_automations {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Agentworks capability not granted: run_automations or manage_automations",
        ));
    }
    let project = a
        .project
        .clone()
        .ok_or_else(|| fail(StatusCode::FORBIDDEN, "Assign a project first"))?;
    Ok((access, project))
}
fn in_project(app: &App, id: &str, project: &str) -> mail::Result<Definition> {
    existing(app, id)?
        .filter_project(project)
        .ok_or_else(|| fail(StatusCode::FORBIDDEN, "Automation is outside this project"))
}
impl Definition {
    fn filter_project(self, project: &str) -> Option<Self> {
        (self.project_id.as_deref() == Some(project)).then_some(self)
    }
    fn agent_created(&self) -> bool {
        self.created_by.is_some()
    }
}
fn agent_request_id(id: &str) -> mail::Result<()> {
    if id.is_empty() || id.len() > 100 || id.chars().any(char::is_control) {
        return Err(invalid("Supply a requestId of 1–100 characters"));
    }
    Ok(())
}
/// MCP automation tools. Reads need runAutomations or manageAutomations;
/// writes need manageAutomations, and creating or replacing a definition
/// also needs startSessions because its runs start sessions. Agents may pause
/// any definition in their project, but resume, replace or delete only
/// agent-created ones. Mutations replay their first result on retry.
pub async fn agent(
    app: &App,
    a: &mail::Actor,
    operation: &str,
    args: serde_json::Value,
) -> mail::Result<serde_json::Value> {
    let (access, project) = agent_scope(app, a)?;
    match operation {
        "list_automations" => {
            let defs = app
                .store
                .lock()
                .unwrap()
                .usable_automations()
                .map_err(storage)?;
            let now = now() as i64;
            let defs = defs
                .into_iter()
                .filter_map(|d| d.filter_project(&project))
                .map(|d| {
                    if access.manage_automations {
                        let mut summary = json!({"id":d.id,"name":d.name,"enabled":d.enabled,"revision":d.revision,"nextRun":view(&d, now, &[])["nextRun"],"trigger":d.workflow.as_ref().map_or(json!({"kind":"manual"}),|w|json!(w.trigger))});
                        if let Some(author) = &d.created_by {
                            summary["createdBy"] = json!(author);
                        }
                        summary
                    } else {
                        json!({"id":d.id,"name":d.name,"enabled":d.enabled})
                    }
                })
                .collect::<Vec<_>>();
            return Ok(json!({ "automations": defs }));
        }
        "get_automation" | "list_automation_runs" => {
            let target: AgentTarget =
                serde_json::from_value(args).map_err(|_| invalid("Invalid automation query"))?;
            let def = in_project(app, &target.automation_id, &project)?;
            return Ok(if operation == "get_automation" {
                view(&def, now() as i64, &profiles_for(&[&def]))
            } else {
                json!({"runs": app.store.lock().unwrap().automation_runs(&def.id).map_err(storage)?})
            });
        }
        _ => {}
    }
    mail::require_actor(app, a, "manage_automations")?;
    let request = args["requestId"]
        .as_str()
        .ok_or_else(|| invalid("Missing requestId"))?
        .to_owned();
    agent_request_id(&request)?;
    let _guard = app.run_lock.lock().await;
    let _workspace = app.workspace_lock.lock().await;
    let payload = args.to_string();
    let previous = app
        .store
        .lock()
        .unwrap()
        .automation_request(&a.id, &request)
        .map_err(storage)?;
    if let Some((op, saved, result)) = previous {
        let same = op == operation
            && serde_json::from_str::<serde_json::Value>(&saved).is_ok_and(|v| v == args);
        if !same {
            return Err(fail(
                StatusCode::CONFLICT,
                "requestId already used for a different automation request",
            ));
        }
        let mut result: serde_json::Value = serde_json::from_str(&result).map_err(storage)?;
        result["duplicate"] = json!(true);
        return Ok(result);
    }
    let ledger = (a.id.as_str(), request.as_str(), operation, payload.as_str());
    match operation {
        "create_automation" | "update_automation" => {
            mail::require_actor(app, a, "start_sessions")?;
            let req: AgentSave = serde_json::from_value(args)
                .map_err(|e| invalid(&format!("Invalid automation fields: {e}")))?;
            let previous = if operation == "update_automation" {
                let id = req
                    .automation_id
                    .as_deref()
                    .ok_or_else(|| invalid("Missing automationId"))?;
                let def = in_project(app, id, &project)?;
                if !def.agent_created() {
                    return Err(fail(
                        StatusCode::FORBIDDEN,
                        "Only automations created by agents can be changed by agents",
                    ));
                }
                if req.revision != Some(def.revision) {
                    return Err(fail(
                        StatusCode::CONFLICT,
                        format!(
                            "Automation changed; current revision is {}. Read it again before updating",
                            def.revision
                        ),
                    ));
                }
                Some(def)
            } else {
                if req.automation_id.is_some() || req.revision.is_some() {
                    return Err(invalid(
                        "Use update_automation to change an existing automation",
                    ));
                }
                let count = app
                    .store
                    .lock()
                    .unwrap()
                    .usable_automations()
                    .map_err(storage)?
                    .iter()
                    .filter(|d| d.agent_created() && d.project_id.as_deref() == Some(&project))
                    .count();
                if count >= MAX_AGENT_DEFINITIONS {
                    return Err(fail(
                        StatusCode::CONFLICT,
                        format!(
                            "Limit: {MAX_AGENT_DEFINITIONS} agent-created automations per project. Delete unused ones first."
                        ),
                    ));
                }
                None
            };
            let input = Input {
                name: req.name,
                provider: req.provider,
                model: req.model,
                model_provider: req.model_provider,
                cwd: req.cwd,
                host_id: Some(
                    a.session
                        .as_ref()
                        .map_or_else(|| "local".into(), |s| s.host_id.clone()),
                ),
                project_id: Some(project),
                enabled: req.enabled,
                prompt: req.prompt,
                workflow: req.workflow,
            };
            let author = Author {
                id: a.id.clone(),
                title: a.title.clone(),
            };
            let def = definition(app, previous.as_ref(), input, Some(author)).await?;
            save(app, previous.as_ref(), &def, Some(ledger))
        }
        "set_automation_enabled" => {
            let target: AgentTarget =
                serde_json::from_value(args).map_err(|_| invalid("Invalid automation fields"))?;
            let enabled = target.enabled.ok_or_else(|| invalid("Missing enabled"))?;
            let previous = in_project(app, &target.automation_id, &project)?;
            if enabled && !previous.agent_created() {
                return Err(fail(
                    StatusCode::FORBIDDEN,
                    "Agents can pause any project automation but resume only those created by agents",
                ));
            }
            let def = Definition {
                enabled,
                updated: now(),
                revision: previous.revision + 1,
                completed: previous.completed.filter(|_| !enabled),
                ..previous.clone()
            };
            save(app, Some(&previous), &def, Some(ledger))
        }
        "delete_automation" => {
            let target: AgentTarget =
                serde_json::from_value(args).map_err(|_| invalid("Invalid automation fields"))?;
            let def = in_project(app, &target.automation_id, &project)?;
            if !def.agent_created() {
                return Err(fail(
                    StatusCode::FORBIDDEN,
                    "Only automations created by agents can be deleted by agents",
                ));
            }
            let result = json!({"ok":true,"id":def.id});
            let result_text = result.to_string();
            app.store
                .lock()
                .unwrap()
                .delete_automation(
                    &def.id,
                    Some(&Ledger {
                        actor: &a.id,
                        request_id: &request,
                        operation,
                        payload: &payload,
                        result: &result_text,
                    }),
                )
                .map_err(storage)?;
            Ok(result)
        }
        _ => Err(fail(
            StatusCode::FORBIDDEN,
            "Agentworks operation is not exposed to agents",
        )),
    }
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
    fn once(time_zone: &str, at: &str) -> Once {
        Once {
            time_zone: time_zone.into(),
            at: at.into(),
            missed: Missed::Skip,
            after_run: AfterRun::Complete,
        }
    }
    #[test]
    fn one_time_triggers_validate_and_resolve_local_time() {
        let parsed: Workflow = serde_json::from_str(r#"{"version":1,"trigger":{"kind":"once","timeZone":" Europe/Berlin ","at":"2026-11-03T09:30","afterRun":"delete"},"steps":[{"kind":"agent","text":"x"}]}"#).unwrap();
        let (workflow, _) = parsed.compile().unwrap();
        let Trigger::Once(trigger) = &workflow.trigger else {
            panic!("once trigger expected");
        };
        assert_eq!(trigger.time_zone, "Europe/Berlin");
        assert_eq!(trigger.after_run, AfterRun::Delete);
        assert_eq!(trigger.instant(), Ok(ms("2026-11-03T08:30:00Z")));
        // DST: a skipped local time runs after the gap; a repeated one at its first instance.
        assert_eq!(
            once("America/New_York", "2026-03-08T02:30").instant(),
            Ok(ms("2026-03-08T07:30:00Z"))
        );
        assert_eq!(
            once("America/New_York", "2026-11-01T01:30").instant(),
            Ok(ms("2026-11-01T05:30:00Z"))
        );
        for bad in [
            once("Mars/Olympus", "2026-11-03T09:30"),
            once("UTC", "2026-11-03 09:30"),
            once("UTC", "2026-11-03T9:30"),
            once("UTC", "2026-11-03T09:30:00"),
            once("UTC", "2026-13-01T09:30"),
            once("UTC", "2026-02-30T09:30"),
            once("UTC", "+026-11-03T09:30"),
        ] {
            let workflow = Workflow {
                trigger: Trigger::Once(bad.clone()),
                ..recipe(vec![agent("x")])
            };
            assert!(workflow.compile().is_err(), "{bad:?}");
        }
        for invalid in [
            r#"{"kind":"once","timeZone":"UTC","at":"2026-11-03T09:30","afterRun":"keep"}"#,
            r#"{"kind":"once","timeZone":"UTC","at":"2026-11-03T09:30","days":[1]}"#,
        ] {
            assert!(
                serde_json::from_str::<Trigger>(invalid).is_err(),
                "{invalid}"
            );
        }
    }
    #[test]
    fn one_time_occurrence_is_due_once_under_the_missed_policy() {
        let at = ms("2026-11-03T09:30:00Z");
        let plan = Plan::Once(at);
        assert_eq!(plan.next(at - 1), Some(at));
        assert_eq!(plan.next(at), None);
        assert_eq!(due(&plan, Missed::Skip, at - 60_000, at - 1), None);
        assert_eq!(
            due(&plan, Missed::Skip, at - 60_000, at + 1_000),
            Some(Due::Run {
                slot: at,
                late: false,
                earlier: false
            })
        );
        // Handled: the cursor has passed it, so it is never due again.
        assert_eq!(due(&plan, Missed::RunOnce, at, at + 86_400_000), None);
        assert_eq!(
            due(&plan, Missed::Skip, at - 60_000, at + GRACE_MS + 1),
            Some(Due::Missed {
                slot: at,
                earlier: false
            })
        );
        assert_eq!(
            due(&plan, Missed::RunOnce, at - 60_000, at + GRACE_MS + 1),
            Some(Due::Run {
                slot: at,
                late: true,
                earlier: false
            })
        );
    }
    #[test]
    fn claiming_a_one_time_occurrence_completes_or_deletes_atomically() {
        let path =
            std::env::temp_dir().join(format!("agentworks-once-{}.sqlite3", uuid::Uuid::new_v4()));
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            let mut def: Definition = serde_json::from_str(r#"{"id":"a","name":"n","provider":"demo","model":null,"cwd":"/","locationId":"l","hostId":"local","projectId":null,"prompt":"p","enabled":true,"created":1,"updated":1,"revision":1}"#).unwrap();
            def.workflow = Some(Workflow {
                trigger: Trigger::Once(once("UTC", "2026-11-03T09:30")),
                ..recipe(vec![agent("p")])
            });
            let mut gone = def.clone();
            gone.id = "b".into();
            store.save_automation(&def, Some(100), None).unwrap();
            store.save_automation(&gone, Some(100), None).unwrap();
            let done = Definition {
                enabled: false,
                completed: Some(900),
                revision: 2,
                ..def.clone()
            };
            store
                .claim_slot(
                    "a",
                    500,
                    "schedule:500",
                    "missed",
                    Some("late"),
                    900,
                    Some(&Finish::Complete(Box::new(done))),
                )
                .unwrap();
            store
                .claim_slot(
                    "b",
                    500,
                    "schedule:500",
                    "skipped",
                    Some("busy"),
                    900,
                    Some(&Finish::Delete),
                )
                .unwrap();
            let saved = store.automation("a").unwrap().unwrap();
            assert!(!saved.enabled);
            assert_eq!(saved.completed, Some(900));
            assert_eq!(saved.revision, 2);
            assert_eq!(store.schedule_cursor("a").unwrap(), None);
            assert!(store.automation("b").unwrap().is_none());
            assert_eq!(store.schedule_cursor("b").unwrap(), None);
            // History survives both.
            assert_eq!(store.automation_runs("a").unwrap()[0]["status"], "missed");
            assert_eq!(store.automation_runs("b").unwrap()[0]["detail"], "busy");
            assert_eq!(view(&saved, 0, &[])["nextRun"], json!(null));
        }
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
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
            store.save_automation(&def, Some(100), None).unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), Some(100));
            // Saving without re-arming keeps the cursor.
            store.save_automation(&def, None, None).unwrap();
            assert_eq!(store.schedule_cursor("a").unwrap(), Some(100));
            store
                .claim_slot("a", 500, "schedule:500", "missed", Some("late"), 900, None)
                .unwrap();
            store
                .claim_slot("a", 500, "schedule:500", "started", None, 950, None)
                .unwrap();
            store
                .claim_slot("a", 400, "schedule:400", "skipped", None, 960, None)
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
            store.save_automation(&def, Some(1), None).unwrap();
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
        let json = serde_json::to_string(&def).unwrap();
        assert!(!json.contains("createdBy") && !json.contains("revision"));
    }
    #[test]
    fn agent_requests_commit_with_their_effect_and_bad_rows_are_skipped() {
        let path = std::env::temp_dir().join(format!(
            "agentworks-agent-automation-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            let mut def: Definition = serde_json::from_str(r#"{"id":"a","name":"first","provider":"demo","model":null,"cwd":"/","locationId":"l","hostId":"local","projectId":"p","prompt":"p","enabled":true,"created":1,"updated":1,"createdBy":{"id":"controller:c","title":"Bot"},"revision":1}"#).unwrap();
            let ledger = |result| Ledger {
                actor: "controller:c",
                request_id: "r1",
                operation: "create_automation",
                payload: "{}",
                result,
            };
            store
                .save_automation(&def, None, Some(&ledger("one")))
                .unwrap();
            assert_eq!(
                store.automation_request("controller:c", "r1").unwrap(),
                Some(("create_automation".into(), "{}".into(), "one".into()))
            );
            assert_eq!(
                store.automation_request("controller:other", "r1").unwrap(),
                None
            );
            // A reused request ID rolls back the whole save.
            def.name = "second".into();
            assert!(
                store
                    .save_automation(&def, None, Some(&ledger("two")))
                    .is_err()
            );
            assert!(
                store
                    .delete_automation("a", Some(&ledger("three")))
                    .is_err()
            );
            let saved = store.automation("a").unwrap().unwrap();
            assert_eq!(saved.name, "first");
            assert_eq!(saved.created_by.unwrap().title, "Bot");
            store
                .connection
                .execute("INSERT INTO automations VALUES ('b','{}')", [])
                .unwrap();
            // Restore validation stays strict; the runtime skips the bad row.
            assert!(store.automations().is_err());
            assert_eq!(
                store
                    .usable_automations()
                    .unwrap()
                    .iter()
                    .map(|d| d.id.as_str())
                    .collect::<Vec<_>>(),
                ["a"]
            );
        }
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
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
