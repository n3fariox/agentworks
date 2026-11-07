//! Event runs finish at durable dispatch, independently of the review session.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GithubReview {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub login: String,
    #[serde(default = "enabled")]
    pub assigned: bool,
    #[serde(default = "enabled")]
    pub review_requested: bool,
    #[serde(default)]
    pub include_existing: bool,
}
impl GithubReview {
    pub fn validate(&mut self) -> std::result::Result<(), &'static str> {
        self.host = self.host.trim().to_ascii_lowercase();
        self.owner = self.owner.trim().to_ascii_lowercase();
        self.repo = self.repo.trim().to_ascii_lowercase();
        self.login = self.login.trim().to_ascii_lowercase();
        if crate::github::parse_origin(&format!(
            "https://{}/{}/{}",
            self.host, self.owner, self.repo
        ))
        .is_none()
            || self.login.is_empty()
            || self.login.len() > 100
            || !self
                .login
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            || !(self.assigned || self.review_requested)
        {
            return Err(
                "Choose a GitHub host, owner, repository, account and at least one review trigger",
            );
        }
        Ok(())
    }
}
use super::scripts;
fn trigger(def: &Definition) -> Option<&scripts::ScriptTrigger> {
    scripts::binding(def)
}

/// Persist transport facts and the exact script version that will handle them.
fn observe(
    db: &mut Connection,
    def: &Definition,
    recipe: &scripts::Resolved,
    snapshot: &Value,
    at: u64,
) -> Result<()> {
    let scope = serde_json::to_string(&recipe.scope)?;
    let trigger_name = recipe.script.automation_trigger.as_ref().unwrap()["name"]
        .as_str()
        .unwrap();
    let tx = db.transaction()?;
    let old: Option<(String, String)> = tx
        .query_row(
            "SELECT scope,snapshot FROM automation_trigger_baselines WHERE automation_id=?1 AND trigger_name=?2",
            params![def.id,trigger_name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let previous: Option<Value> = old
        .filter(|(s, _)| *s == scope)
        .map(|(_, s)| serde_json::from_str(&s))
        .transpose()?;
    if previous.as_ref() == Some(snapshot) {
        return Ok(());
    }
    for (number, row) in snapshot.as_object().ok_or("Invalid review snapshot")? {
        let old = previous.as_ref().map(|v| &v[number]);
        let initial = previous.is_none();
        let assigned = recipe.scope.assigned
            && row["assigned"] == true
            && old.is_none_or(|r| r["assigned"] != true);
        let requested = recipe.scope.review_requested
            && row["requested"] == true
            && old.is_none_or(|r| r["requested"] != true);
        if (assigned || requested) && (!initial || recipe.scope.include_existing) {
            let mut pr = row["pr"].clone();
            pr["host"] = json!(recipe.scope.host);
            pr["owner"] = json!(recipe.scope.owner);
            pr["repo"] = json!(recipe.scope.repo);
            pr["url"] = json!(format!(
                "https://{}/{}/{}/pull/{number}",
                recipe.scope.host, recipe.scope.owner, recipe.scope.repo
            ));
            let payload = json!({"number":number.parse::<u64>()?,"pr":pr,"assigned":assigned,"reviewRequested":requested,"initial":initial,
                "subscription":recipe.scope,"recipe":{"script":recipe.script.name,"hash":recipe.script.hash,"source":recipe.source,"handler":trigger_name}});
            tx.execute("INSERT INTO automation_events(id,automation_id,revision,payload,created) VALUES (?1,?2,?3,?4,?5)",params![Uuid::new_v4().to_string(),def.id,def.revision as i64,payload.to_string(),at as i64])?;
        }
    }
    tx.execute("INSERT INTO automation_trigger_baselines(automation_id,trigger_name,scope,snapshot) VALUES (?1,?2,?3,?4) ON CONFLICT(automation_id,trigger_name) DO UPDATE SET scope=excluded.scope,snapshot=excluded.snapshot",params![def.id,trigger_name,scope,snapshot.to_string()])?;
    tx.commit()?;
    Ok(())
}
pub async fn scheduler(app: App) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        if let Err(e) = tick(&app).await {
            eprintln!(
                "Script automation: {}",
                agentworks_runtime::provider::safe_error(&e.to_string())
            );
        }
    }
}
async fn tick(app: &App) -> Result<()> {
    {
        let _communication = app.communication_lock.lock().await;
        let _run = app.run_lock.lock().await;
        scripts::reconcile(app).map_err(|(_, e)| e)?;
    }
    let defs = app.store.lock().unwrap().usable_automations()?;
    'definitions: for def in defs
        .into_iter()
        .filter(|d| d.enabled && d.script_error.is_none() && trigger(d).is_some())
    {
        if crate::service_restart::allow_start(app).is_err() {
            return Ok(());
        }
        let recipes = scripts::resolve_all(
            &app.store.lock().unwrap().connection,
            &app.progress.engine(),
            trigger(&def).unwrap(),
        );
        let mut errors = Vec::new();
        match recipes {
            Err(e) => errors.push(e),
            Ok(recipes) => {
                for recipe in recipes {
                    let fetched = crate::github::review_requests(app, &recipe.scope).await;
                    let _communication = app.communication_lock.lock().await;
                    let _run = app.run_lock.lock().await;
                    if crate::service_restart::allow_start(app).is_err() {
                        return Ok(());
                    }
                    if !app
                        .store
                        .lock()
                        .unwrap()
                        .automation(&def.id)?
                        .is_some_and(|d| d.enabled && d.revision == def.revision)
                    {
                        continue 'definitions;
                    }
                    match fetched {
                        Ok(snapshot) => observe(
                            &mut app.store.lock().unwrap().connection,
                            &def,
                            &recipe,
                            &snapshot,
                            now(),
                        )?,
                        Err(e) => errors.push(format!(
                            "{}: {e}",
                            recipe.script.automation_trigger.as_ref().unwrap()["name"]
                                .as_str()
                                .unwrap()
                        )),
                    }
                }
            }
        }
        let _communication = app.communication_lock.lock().await;
        let _run = app.run_lock.lock().await;
        if crate::service_restart::allow_start(app).is_err() {
            return Ok(());
        }
        if !app
            .store
            .lock()
            .unwrap()
            .automation(&def.id)?
            .is_some_and(|d| d.enabled && d.revision == def.revision)
        {
            continue;
        }
        app.store.lock().unwrap().connection.execute("INSERT INTO automation_github_polls(automation_id,checked_at,error) VALUES (?1,?2,?3) ON CONFLICT(automation_id) DO UPDATE SET checked_at=excluded.checked_at,error=excluded.error",params![def.id,now() as i64,(!errors.is_empty()).then(||errors.join("; "))])?;
        let pending: Vec<(String, Value)> = {
            let store = app.store.lock().unwrap();
            let mut q=store.connection.prepare("SELECT id,payload FROM automation_events WHERE automation_id=?1 AND revision=?2 AND status='pending' ORDER BY created,id LIMIT 100")?;
            q.query_map(params![def.id, def.revision as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .map(|r| {
                let (id, p) = r?;
                Ok((id, serde_json::from_str(&p)?))
            })
            .collect::<Result<_>>()?
        };
        for (id, payload) in pending {
            dispatch(app, &def, &id, &payload).await?;
        }
    }
    Ok(())
}
fn finish(app: &App, id: &str, status: &str, detail: &str, target: Option<&str>) -> Result<()> {
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction()?;
    tx.execute("UPDATE automation_events SET status=?2,detail=?3,target=COALESCE(?4,target) WHERE id=?1 AND status='pending'",params![id,status,detail,target])?;
    if status == "skipped" {
        tx.execute("UPDATE automation_event_actions SET status='skipped',detail=?2 WHERE event_id=?1 AND status='pending'",params![id,detail])?;
    }
    tx.commit()?;
    Ok(())
}
async fn decide(app: &App, def: &Definition, id: &str, payload: &Value) -> Result<()> {
    let source = payload["recipe"]["source"]
        .as_str()
        .ok_or("Missing pinned script source")?;
    let mut binding = trigger(def).ok_or("Missing script binding")?.clone();
    if let Some(handler) = payload["recipe"]["handler"].as_str() {
        binding.handler = handler.into();
    }
    let recipe = scripts::resolve_source(&app.progress.engine(), &binding, source)?;
    if payload["recipe"]["hash"] != recipe.script.hash {
        return Err("Pinned script hash does not match source".into());
    }
    let pr = &payload["pr"];
    // Read-only snapshot. Effects are authorized against live sessions later.
    let rows = crate::github::matching_sessions(
        app,
        &crate::github::SessionQuery {
            host: pr["host"].as_str().ok_or("Missing PR host")?.into(),
            owner: pr["owner"].as_str().ok_or("Missing PR owner")?.into(),
            repo: pr["repo"].as_str().ok_or("Missing PR repository")?.into(),
            number: pr["number"].as_u64().ok_or("Missing PR number")?,
        },
    )
    .map_err(|(_, e)| e)?;
    let sessions:Vec<_>=rows.into_iter().filter(|r|r["session"]["projectId"].as_str()==def.project_id.as_deref()).map(|r|{
        let s=&r["session"];
        json!({"id":s["id"],"title":s["title"],"archived":s["archived"],"status":s["status"],"provider":s["provider"],"cwd":s["cwd"],"hostId":s["hostId"],"projectId":s["projectId"],"pullRequests":[{"host":pr["host"],"owner":pr["owner"],"repo":pr["repo"],"number":pr["number"]}]})
    }).collect();
    let ctx = json!({"workflow":id,"owner":def.created_by.as_ref().map(|a|a.id.as_str()).unwrap_or("human"),"state":{},"parameters":trigger(def).unwrap().parameters,"instructions":def.prompt,"settings":{"provider":def.provider,"cwd":def.cwd,"projectId":def.project_id,"hostId":def.host_id,"model":def.model,"modelProvider":def.model_provider,"title":def.name},"sessions":sessions});
    let mut ctx = ctx;
    let descriptor = recipe.script.automation_trigger.as_ref().unwrap();
    if descriptor["settings"].is_object() {
        ctx["settings"] = descriptor["settings"].clone();
    }
    if let Some(instructions) = descriptor["instructions"].as_str() {
        ctx["instructions"] = json!(instructions);
    }
    let event = json!({"id":id,"kind":if payload["reviewRequested"]==true {"pr.review_requested"}else{"pr.assigned_to_me"},"data":{"pr":payload["pr"],"assigned":payload["assigned"],"reviewRequested":payload["reviewRequested"],"initial":payload["initial"],"subscription":payload["subscription"]}});
    let engine = app.progress.engine();
    let script = recipe.script.clone();
    let (value, _) = tokio::task::spawn_blocking(move || engine.call(&script, &ctx, &event))
        .await?
        .map_err(|e| e.to_string())?;
    let effects = scripts::effects(&value)?;
    // Validate every proposed effect before accepting any of them.
    for effect in &effects {
        validate_effect(app, def, effect)?;
    }
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction()?;
    for effect in &effects {
        tx.execute(
            "INSERT INTO automation_event_actions(event_id,action_key,payload) VALUES (?1,?2,?3)",
            params![id, effect.key, serde_json::to_string(effect)?],
        )?;
    }
    let decision = json!({"script":recipe.script.name,"hash":recipe.script.hash,"state":value["state"],"actions":effects});
    tx.execute(
        "UPDATE automation_events SET action=?2 WHERE id=?1",
        params![id, decision.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}
fn validate_effect(
    app: &App,
    def: &Definition,
    effect: &scripts::Effect,
) -> std::result::Result<(), String> {
    if effect.kind == "queue_input" {
        let target = crate::session(app, effect.target.as_deref().ok_or("Missing target")?)
            .map_err(|(_, e)| e)?;
        let data = target.data.lock().unwrap();
        if data.lifecycle.archived || data.project_id != def.project_id {
            return Err("The selected session was archived or moved to another project".into());
        }
    }
    if effect.kind == "start_session" {
        let settings: scripts::Settings =
            serde_json::from_value(effect.settings.clone().ok_or("Missing settings")?)
                .map_err(|e| e.to_string())?;
        if Some(settings.project_id.as_str()) != def.project_id.as_deref()
            || settings.host_id != def.host_id
        {
            return Err("Actions must use the automation's project and execution host".into());
        }
        if let Some(workspace) = &effect.workspace {
            serde_json::from_value::<crate::worktrees::Workspace>(workspace.clone())
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
async fn dispatch(app: &App, def: &Definition, id: &str, payload: &Value) -> Result<()> {
    if def.project_id.is_none() {
        return finish(
            app,
            id,
            "skipped",
            "The review project no longer exists",
            None,
        );
    }
    if let Some(error) = &def.script_error {
        return finish(app, id, "skipped", error, None);
    }
    if let Err(reason) = author_permits(app, def) {
        return finish(app, id, "skipped", &reason, None);
    }
    let scope: GithubReview = serde_json::from_value(payload["subscription"].clone())?;
    if !app
        .catalog
        .lock()
        .unwrap()
        .data
        .github
        .connections
        .iter()
        .any(|c| {
            c.enabled
                && c.host.eq_ignore_ascii_case(&scope.host)
                && c.login.eq_ignore_ascii_case(&scope.login)
        })
    {
        return finish(
            app,
            id,
            "skipped",
            "The selected GitHub account is disabled",
            None,
        );
    }
    let enabled: bool = app.store.lock().unwrap().connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM scripts WHERE name=?1 AND enabled=1 AND capability='automation')",
        [payload["recipe"]["script"]
            .as_str()
            .ok_or("Missing script name")?],
        |r| r.get(0),
    )?;
    if !enabled {
        return finish(
            app,
            id,
            "skipped",
            "The automation script is missing or disabled",
            None,
        );
    }
    let prepared: bool = app.store.lock().unwrap().connection.query_row(
        "SELECT action IS NOT NULL FROM automation_events WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if !prepared && let Err(e) = decide(app, def, id, payload).await {
        return finish(
            app,
            id,
            "skipped",
            &format!("Script decision failed: {e}"),
            None,
        );
    }
    let actions: Vec<(String, scripts::Effect, String, Option<String>)> = {
        let store = app.store.lock().unwrap();
        let mut q=store.connection.prepare("SELECT action_key,payload,status,target FROM automation_event_actions WHERE event_id=?1 ORDER BY rowid")?;
        q.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .map(|r| {
            let (k, p, s, t) = r?;
            Ok((k, serde_json::from_str(&p)?, s, t))
        })
        .collect::<Result<_>>()?
    };
    let mut target = None;
    let mut count = 0;
    for (key, effect, status, previous_target) in actions {
        if status == "completed" {
            target = previous_target.or(target);
            count += 1;
            continue;
        }
        if effect.kind == "skip" {
            return finish(
                app,
                id,
                "skipped",
                effect.detail.as_deref().unwrap_or("Skipped by script"),
                None,
            );
        }
        match perform(app, def, id, &key, &effect).await {
            Ok(session) => {
                app.store.lock().unwrap().connection.execute("UPDATE automation_event_actions SET status='completed',target=?3 WHERE event_id=?1 AND action_key=?2",params![id,key,session])?;
                target = Some(session);
                count += 1;
            }
            Err(error) => {
                if error.retry {
                    app.store.lock().unwrap().connection.execute(
                        "UPDATE automation_events SET detail=?2 WHERE id=?1",
                        params![id, format!("Dispatch will retry: {}", error.reason)],
                    )?;
                    return Ok(());
                }
                return finish(app, id, "skipped", &error.reason, target.as_deref());
            }
        }
    }
    finish(
        app,
        id,
        "completed",
        &format!("Script dispatched {count} action(s)"),
        target.as_deref(),
    )
}
struct EffectFailure {
    reason: String,
    retry: bool,
}
impl From<String> for EffectFailure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            retry: false,
        }
    }
}
impl From<&str> for EffectFailure {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}
async fn perform(
    app: &App,
    def: &Definition,
    id: &str,
    key: &str,
    effect: &scripts::Effect,
) -> std::result::Result<String, EffectFailure> {
    let request = format!("github:{id}:{key}");
    if effect.kind == "queue_input" {
        let target = effect.target.as_deref().ok_or("Missing target")?;
        let s = crate::session(app, target).map_err(|(status, reason)| EffectFailure {
            reason,
            retry: status.is_server_error(),
        })?;
        {
            let d = s.data.lock().unwrap();
            if d.lifecycle.archived || d.project_id != def.project_id {
                return Err("The selected session was archived or moved to another project".into());
            }
        }
        let _ = crate::input_queue::enqueue_locked(
            app,
            target,
            effect.prompt.as_deref().unwrap(),
            &request,
            true,
        )
        .map_err(|(status, reason)| EffectFailure {
            reason,
            retry: status.is_server_error(),
        })?;
        return Ok(target.into());
    }
    let settings: scripts::Settings =
        serde_json::from_value(effect.settings.clone().unwrap()).map_err(|e| e.to_string())?;
    let mut workspace = effect
        .workspace
        .clone()
        .map(serde_json::from_value::<crate::worktrees::Workspace>)
        .transpose()
        .map_err(|e| e.to_string())?;
    if let Some(w) = &mut workspace
        && w.mode == crate::worktrees::Mode::NewWorktree
        && w.branch.is_none()
    {
        w.branch = Some(format!("automation-{id}-{key}"));
    }
    let link = workspace
        .as_ref()
        .and_then(|w| w.pull_request.as_ref())
        .map(|pr| crate::github::Link {
            host: pr.host.clone(),
            owner: pr.owner.clone(),
            repo: pr.repo.clone(),
            kind: "pullRequest".into(),
            number: pr.number,
        });
    let Json(session) = create_session(
        State(app.clone()),
        Json(Create {
            workspace,
            provider: settings.provider,
            cwd: settings.cwd,
            title: settings.title,
            model: settings.model,
            model_provider: settings.model_provider,
            project_id: Some(settings.project_id),
            host_id: Some(settings.host_id),
            role: settings.role,
            automation: Some(Origin {
                automation_id: def.id.clone(),
                name: def.name.clone(),
                run_id: Uuid::new_v4().to_string(),
                request_id: request,
                initiated_by: None,
                outcome: Some("dispatched".into()),
            }),
            queued_prompt: effect.prompt.clone(),
            ..Default::default()
        }),
    )
    .await
    .map_err(|(status, reason)| EffectFailure {
        reason,
        retry: status.is_server_error(),
    })?;
    let target = session["id"].as_str().ok_or("Missing session ID")?;
    if let Some(link) = link {
        let mut catalog = app.catalog.lock().unwrap();
        let mut config = catalog.data.github.clone();
        let links = config.links.entry(target.into()).or_default();
        if !links.contains(&link) {
            links.push(link);
            catalog.github(config).map_err(|e| EffectFailure {
                reason: e.to_string(),
                retry: true,
            })?;
        }
    }
    Ok(target.into())
}
pub(super) fn poll_status(app: &App, id: &str) -> Value {
    app.store
        .lock()
        .unwrap()
        .connection
        .query_row(
            "SELECT checked_at,error FROM automation_github_polls WHERE automation_id=?1",
            [id],
            |r| Ok(json!({"checkedAt":r.get::<_,i64>(0)?,"error":r.get::<_,Option<String>>(1)?})),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(Value::Null)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn definition() -> Definition {
        serde_json::from_value(json!({"id":"review","name":"Review queue","provider":"demo","model":null,"cwd":"/","locationId":"l","hostId":"local","projectId":"p","prompt":"Review correctness and tests.","enabled":true,"created":1,"updated":1,"revision":1,
            "workflow":{"version":1,"trigger":{"kind":"script","script":"github_review.star","parameters":{}},"steps":[{"kind":"agent","text":"Review correctness and tests."}]}})).unwrap()
    }
    fn snapshot(number: u64, assigned: bool, requested: bool, sha: &str) -> Value {
        json!({number.to_string():{"assigned":assigned,"requested":requested,"pr":{"number":number,"headSha":sha}}})
    }
    fn events(db: &Connection) -> Vec<(String, Value)> {
        let mut q = db
            .prepare("SELECT id,payload FROM automation_events ORDER BY created,id")
            .unwrap();
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|r| {
                let (id, payload) = r.unwrap();
                (id, serde_json::from_str(&payload).unwrap())
            })
            .collect()
    }
    fn attach(app: &App, id: &str, number: u64) {
        app.catalog.lock().unwrap().data.github.links.insert(
            id.into(),
            vec![crate::github::Link {
                host: "github.com".into(),
                owner: "team".into(),
                repo: "repo".into(),
                kind: "pullRequest".into(),
                number,
            }],
        );
    }
    fn configure(f: &crate::github_tracking::tests::Fixture) -> Definition {
        let mut def = definition();
        def.cwd = f.s.cwd.to_string_lossy().into_owned();
        f.app
            .catalog
            .lock()
            .unwrap()
            .data
            .projects
            .push(serde_json::from_value(json!({"id":"p","name":"Reviews"})).unwrap());
        f.s.data.lock().unwrap().project_id = Some("p".into());
        crate::input_queue::initialize(&f.app).unwrap();
        let recipe = recipe(&def);
        def.script_hash = Some(recipe.script.hash.clone());
        f.app
            .store
            .lock()
            .unwrap()
            .save_automation_source(&def, None, None, Some(&recipe.source))
            .unwrap();
        def
    }
    fn observe_app(app: &App, def: &Definition, value: &Value, at: u64) {
        let mut store = app.store.lock().unwrap();
        let recipe = scripts::resolve(
            &store.connection,
            &app.progress.engine(),
            trigger(def).unwrap(),
        )
        .unwrap();
        super::observe(&mut store.connection, def, &recipe, value, at).unwrap();
    }
    fn recipe(def: &Definition) -> scripts::Resolved {
        let form=serde_json::from_value(json!({"host":"github.com","repo":"team/repo","account":"fixture","settings":{"provider":def.provider,"cwd":def.cwd,"projectId":"p","hostId":"local","title":def.name},"steps":[{"kind":"agent","text":def.prompt}]})).unwrap();
        let source = super::super::generator::generate(&form).unwrap();
        scripts::resolve_source(
            &agentworks_starlark::engine::Engine::new().unwrap(),
            trigger(def).unwrap(),
            &source,
        )
        .unwrap()
    }
    fn observe(db: &mut Connection, def: &Definition, value: &Value, at: u64) -> Result<()> {
        super::observe(db, def, &recipe(def), value, at)
    }
    #[test]
    fn script_bindings_are_untimed_and_validated() {
        let mut def = definition();
        assert!(def.workflow.clone().unwrap().compile().is_ok());
        assert!(def.timed().is_none());
        assert!(def.workflow.as_ref().unwrap().trigger.timing().is_none());
        if let Trigger::Script(t) = &mut def.workflow.as_mut().unwrap().trigger {
            t.script = "../unsafe.star".into();
        }
        assert!(def.workflow.unwrap().compile().is_err());
    }
    #[test]
    fn membership_edges_baseline_and_reopen_are_durable() {
        let path = std::env::temp_dir().join(format!("review-events-{}.sqlite3", Uuid::new_v4()));
        let mut def = definition();
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, true, "a"),
                1,
            )
            .unwrap();
            assert!(events(&store.connection).is_empty()); // baseline does not sweep backlog
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, true, "b"),
                2,
            )
            .unwrap();
            assert!(events(&store.connection).is_empty()); // pushes are not new requests
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, false, "b"),
                3,
            )
            .unwrap();
        }
        {
            let mut store = crate::store::Store::open(&path).unwrap();
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, true, "b"),
                4,
            )
            .unwrap();
            let rows = events(&store.connection);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].1["reviewRequested"], true);
            assert_eq!(rows[0].1["assigned"], false);
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, true, "b"),
                5,
            )
            .unwrap();
            assert_eq!(events(&store.connection).len(), 1);
            // An invalid/failed observation cannot erase membership.
            assert!(observe(&mut store.connection, &def, &Value::Null, 6).is_err());
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, true, "b"),
                7,
            )
            .unwrap();
            assert_eq!(events(&store.connection).len(), 1);
            observe(&mut store.connection, &def, &json!({}), 8).unwrap();
            observe(
                &mut store.connection,
                &def,
                &snapshot(17, true, true, "c"),
                9,
            )
            .unwrap();
            assert_eq!(events(&store.connection).len(), 2); // removed then re-added
            def.id = "include-existing".into();
            let mut backlog = recipe(&def);
            backlog.scope.include_existing = true;
            super::observe(
                &mut store.connection,
                &def,
                &backlog,
                &snapshot(18, true, true, "d"),
                10,
            )
            .unwrap();
            assert_eq!(events(&store.connection).len(), 3);
        }
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let _ = std::fs::remove_file(path.with_extension("sqlite3.lock"));
    }
    #[tokio::test]
    async fn existing_session_queue_replay_and_busy_reviews_do_not_hold_event_runs_open() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        attach(&f.app, &f.s.id, 17);
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"a".repeat(40)), 2);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        f.s.data.lock().unwrap().status = "idle".into();
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
        let runs = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap();
        assert_eq!(runs[0]["status"], "completed");
        assert_eq!(runs[0]["sessionId"], f.s.id);
        assert!(
            f.s.data.lock().unwrap().input_queue.items[0]
                .text
                .contains(&"a".repeat(40))
        );
        // Lost acknowledgement after queue acceptance; durable bindings dedup.
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute(
                "UPDATE automation_events SET status='pending' WHERE id=?1",
                [&id],
            )
            .unwrap();
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
        observe_app(&f.app, &def, &json!({}), 3);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"b".repeat(40)), 4);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(1);
        {
            let mut d = f.s.data.lock().unwrap();
            d.status = "running".into();
            d.input_queue.ready = false;
        }
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        let d = f.s.data.lock().unwrap();
        assert_eq!(d.input_queue.items.len(), 2);
        drop(d);
        assert!(
            f.app
                .store
                .lock()
                .unwrap()
                .automation_runs(&def.id)
                .unwrap()
                .iter()
                .all(|r| r["status"] == "completed")
        );
    }
    #[tokio::test]
    async fn ambiguous_matches_skip_and_archived_sessions_do_not_receive_inputs() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        attach(&f.app, &f.s.id, 17);
        let Json(other) = create_session(
            State(f.app.clone()),
            Json(Create {
                provider: "demo".into(),
                cwd: def.cwd.clone(),
                project_id: Some("p".into()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        let other_id = other["id"].as_str().unwrap();
        attach(&f.app, other_id, 17);
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"a".repeat(40)), 2);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        let runs = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap();
        assert_eq!(runs[0]["status"], "skipped");
        assert!(
            runs[0]["detail"]
                .as_str()
                .unwrap()
                .contains("Multiple matching")
        );
        assert!(f.s.data.lock().unwrap().input_queue.items.is_empty());
        f.app
            .sessions
            .lock()
            .unwrap()
            .get(other_id)
            .unwrap()
            .data
            .lock()
            .unwrap()
            .lifecycle
            .archived = true;
        observe_app(&f.app, &def, &json!({}), 3);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"a".repeat(40)), 4);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(1);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
    }
    #[tokio::test]
    async fn new_reviews_get_pinned_worktrees_and_creation_replay_is_idempotent() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        let sha = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&def.cwd)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, false, true, &sha), 2);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        let runs = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["status"], "completed");
        let target = runs[0]["sessionId"].as_str().unwrap();
        assert_ne!(target, f.s.id);
        let s = crate::session(&f.app, target).unwrap();
        assert_ne!(s.cwd, f.s.cwd);
        let actual = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&s.cwd)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(actual.stdout).unwrap().trim(), sha);
        assert_eq!(
            s.data
                .lock()
                .unwrap()
                .automation
                .as_ref()
                .unwrap()
                .outcome
                .as_deref(),
            Some("dispatched")
        );
        let n = f.app.sessions.lock().unwrap().len();
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute(
                "UPDATE automation_events SET status='pending' WHERE id=?1",
                [&id],
            )
            .unwrap();
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.app.sessions.lock().unwrap().len(), n);
        // Another PR starts while this review is still alive.
        observe_app(&f.app, &def, &json!({}), 3);
        observe_app(&f.app, &def, &snapshot(18, true, false, &sha), 4);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(1);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.app.sessions.lock().unwrap().len(), n + 1);
        assert!(
            f.app
                .store
                .lock()
                .unwrap()
                .automation_runs(&def.id)
                .unwrap()
                .iter()
                .all(|r| r["status"] == "completed")
        );
    }
    #[tokio::test]
    async fn edits_cancel_pending_events_and_pause_rearms_the_baseline() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let mut def = configure(&f);
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, "a"), 2);
        def.revision += 1;
        f.app
            .store
            .lock()
            .unwrap()
            .save_automation(&def, None, None)
            .unwrap();
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .automation_runs(&def.id)
                .unwrap()[0]["status"],
            "skipped"
        );
        def.enabled = false;
        f.app
            .store
            .lock()
            .unwrap()
            .save_automation(&def, None, None)
            .unwrap();
        def.enabled = true;
        f.app
            .store
            .lock()
            .unwrap()
            .save_automation(&def, None, None)
            .unwrap();
        observe_app(&f.app, &def, &snapshot(17, true, true, "a"), 3);
        assert_eq!(events(&f.app.store.lock().unwrap().connection).len(), 1);
    }
    #[tokio::test]
    async fn background_ticks_preserve_baseline_on_failure_and_share_cached_membership() {
        let mut f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        attach(&f.app, &f.s.id, 17);
        let scope = &recipe(&def).scope;
        f.app
            .github
            .seed_review_requests(scope, snapshot(17, true, true, &"a".repeat(40)))
            .await;
        tick(&f.app).await.unwrap();
        assert!(events(&f.app.store.lock().unwrap().connection).is_empty());
        // Disabled account is an unavailable poll, never an empty membership.
        f.app.catalog.lock().unwrap().data.github.connections[0].enabled = false;
        tick(&f.app).await.unwrap();
        assert!(poll_status(&f.app, &def.id)["error"].as_str().is_some());
        f.app.catalog.lock().unwrap().data.github.connections[0].enabled = true;
        // Simulate losing the in-memory hub on restart. The stored baseline
        // must still suppress a duplicate initial request.
        f.app.github = std::sync::Arc::new(crate::github::Hub::default());
        f.app
            .github
            .seed_review_requests(scope, snapshot(17, true, true, &"a".repeat(40)))
            .await;
        tick(&f.app).await.unwrap();
        assert!(events(&f.app.store.lock().unwrap().connection).is_empty());
        f.app
            .github
            .seed_review_requests(scope, snapshot(17, true, false, &"a".repeat(40)))
            .await;
        tick(&f.app).await.unwrap();
        f.app
            .github
            .seed_review_requests(scope, snapshot(17, true, true, &"b".repeat(40)))
            .await;
        tick(&f.app).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .automation_runs(&def.id)
                .unwrap()[0]["status"],
            "completed"
        );
        tick(&f.app).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
    }
    #[tokio::test]
    async fn agent_authors_need_live_prompt_authority_before_dispatch() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let mut def = configure(&f);
        def.created_by = Some(Author {
            id: f.s.id.clone(),
            title: "Review coordinator".into(),
        });
        attach(&f.app, &f.s.id, 17);
        let mut access = mail::Access {
            manage_automations: true,
            start_sessions: true,
            ..Default::default()
        };
        f.app
            .store
            .lock()
            .unwrap()
            .save_access(&f.s.id, &access)
            .unwrap();
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"a".repeat(40)), 2);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        let runs = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap();
        assert_eq!(runs[0]["status"], "skipped");
        assert!(
            runs[0]["detail"]
                .as_str()
                .unwrap()
                .contains("prompt_sessions")
        );
        assert!(f.s.data.lock().unwrap().input_queue.items.is_empty());
        access.prompt_sessions = true;
        f.app
            .store
            .lock()
            .unwrap()
            .save_access(&f.s.id, &access)
            .unwrap();
        observe_app(&f.app, &def, &json!({}), 3);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"b".repeat(40)), 4);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(1);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
    }
    #[tokio::test]
    async fn deleting_the_project_pauses_review_subscriptions_and_cancels_pending_dispatch() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, "a"), 2);
        f.app
            .store
            .lock()
            .unwrap()
            .detach_automation_project("p")
            .unwrap();
        let saved = f
            .app
            .store
            .lock()
            .unwrap()
            .automation(&def.id)
            .unwrap()
            .unwrap();
        assert!(!saved.enabled);
        assert!(saved.project_id.is_none());
        assert_eq!(saved.revision, def.revision + 1);
        let runs = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap();
        assert_eq!(runs[0]["status"], "skipped");
        assert!(
            runs[0]["detail"]
                .as_str()
                .unwrap()
                .contains("project deleted")
        );
    }
    fn custom_source(body: &str, events: &str) -> String {
        format!(
            r#"load("@agentworks//workflow.star", "WorkflowContext", "Event", "Action", "parameter", "sessions_for_pr", "queue_input", "start_session", "skip")
SETTINGS = None
def custom(ctx: WorkflowContext, event: Event) -> list[Action]:
{body}
agent.register_github_trigger(custom, host="github.com", repo="team/repo", account="fixture", events={events}, settings=SETTINGS, instructions="Review correctness and tests.")
"#
        )
    }
    fn save_recipe(app: &App, source: &str) {
        let def = app
            .store
            .lock()
            .unwrap()
            .automation("review")
            .unwrap()
            .unwrap();
        let settings = json!({"provider":def.provider,"cwd":def.cwd,"projectId":def.project_id,"hostId":def.host_id,"title":def.name});
        let source = source.replace(
            "SETTINGS = None",
            &format!("SETTINGS = {}", super::super::generator::literal(&settings)),
        );
        app.progress
            .engine()
            .compile_automation("github_review.star", &source)
            .unwrap();
        app.store
            .lock()
            .unwrap()
            .save_automation_source(&def, None, None, Some(&source))
            .unwrap();
    }
    #[tokio::test]
    async fn script_owns_registration_routing_composition_and_pinned_replay() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        attach(&f.app, &f.s.id, 17);
        let source = custom_source(
            "    matches = sessions_for_pr(ctx.sessions, event.data[\"pr\"])\n    return [queue_input(matches[0], \"Custom first\", key=\"one\"), queue_input(matches[0], \"Custom second\", key=\"two\")]",
            "[\"pr.review_requested\"]",
        );
        save_recipe(&f.app, &source);
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, false, &"a".repeat(40)), 2);
        assert!(events(&f.app.store.lock().unwrap().connection).is_empty()); // subscription is script-owned
        observe_app(&f.app, &def, &snapshot(17, true, true, &"a".repeat(40)), 3);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        // Editing the recipe affects future events, not an already observed one.
        save_recipe(
            &f.app,
            &custom_source(
                "    return [skip(\"Custom policy: leave this PR alone\")]",
                "[\"pr.review_requested\"]",
            ),
        );
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(
            f.s.data
                .lock()
                .unwrap()
                .input_queue
                .items
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>(),
            ["Custom first", "Custom second"]
        );
        assert_eq!(f.app.sessions.lock().unwrap().len(), 1);
        // Lose both action acknowledgements: the queue bindings still dedup.
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute(
                "UPDATE automation_event_actions SET status='pending' WHERE event_id=?1",
                [&id],
            )
            .unwrap();
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute(
                "UPDATE automation_events SET status='pending' WHERE id=?1",
                [&id],
            )
            .unwrap();
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 2);
        observe_app(&f.app, &def, &json!({}), 4);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"b".repeat(40)), 5);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(1);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 2);
        assert!(
            f.app
                .store
                .lock()
                .unwrap()
                .automation_runs(&def.id)
                .unwrap()
                .iter()
                .any(|r| r["status"] == "skipped"
                    && r["detail"].as_str().unwrap().contains("Custom policy"))
        );
    }
    #[tokio::test]
    async fn invalid_or_cross_project_effects_are_rejected_before_any_effect() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        attach(&f.app, &f.s.id, 17);
        let source = custom_source(
            "    settings = dict(ctx.settings)\n    settings[\"projectId\"] = \"another-project\"\n    return [queue_input(ctx.sessions[0], \"must not dispatch\"), start_session(settings=settings, prompt=\"forbidden\")]",
            "[\"pr.assigned_to_me\"]",
        );
        save_recipe(&f.app, &source);
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, &"a".repeat(40)), 2);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        assert!(f.s.data.lock().unwrap().input_queue.items.is_empty());
        assert_eq!(f.app.sessions.lock().unwrap().len(), 1);
        let runs = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap();
        assert_eq!(runs[0]["status"], "skipped");
        assert!(
            runs[0]["detail"]
                .as_str()
                .unwrap()
                .contains("project and execution host")
        );
    }
    #[tokio::test]
    async fn generated_file_settings_and_instructions_drive_dispatch() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        configure(&f);
        let form=serde_json::from_value(json!({"host":"github.com","repo":"team/repo","account":"fixture","settings":{"provider":"demo","cwd":f.s.cwd,"projectId":"p","hostId":"local","title":"From the file"},"steps":[{"kind":"agent","text":"The file owns this prompt."}]})).unwrap();
        let source = super::super::generator::generate(&form).unwrap();
        let Json(created) = crate::automations::create(
            State(f.app.clone()),
            Json(
                serde_json::from_value(json!({"name":"Source authority","source":source})).unwrap(),
            ),
        )
        .await
        .unwrap();
        let mut def = f
            .app
            .store
            .lock()
            .unwrap()
            .automation(created["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        // The catalog fields are only an index/fallback for older bindings.
        def.provider = "not-a-harness".into();
        def.prompt = "stale indexed instructions".into();
        let sha = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&f.s.cwd)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        observe_app(&f.app, &def, &json!({}), 1);
        observe_app(&f.app, &def, &snapshot(17, true, true, &sha), 2);
        let (id, payload) = events(&f.app.store.lock().unwrap().connection).remove(0);
        dispatch(&f.app, &def, &id, &payload).await.unwrap();
        let run = f
            .app
            .store
            .lock()
            .unwrap()
            .automation_runs(&def.id)
            .unwrap()
            .remove(0);
        assert_eq!(run["status"], "completed");
        let session = crate::session(&f.app, run["sessionId"].as_str().unwrap()).unwrap();
        assert_eq!(session.provider, "demo");
        let effect: String = f
            .app
            .store
            .lock()
            .unwrap()
            .connection
            .query_row(
                "SELECT payload FROM automation_event_actions WHERE event_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(effect.contains("The file owns this prompt."));
        assert!(!effect.contains("stale indexed instructions"));
    }
    #[tokio::test]
    async fn multiple_named_triggers_share_one_automation_and_poll_cache_without_sharing_cursors() {
        let f = crate::github_tracking::tests::Fixture::new().await;
        let def = configure(&f);
        attach(&f.app, &f.s.id, 17);
        let source = recipe(&def).source.replace(
            "events = [\"pr.assigned_to_me\", \"pr.review_requested\"]",
            "events = [\"pr.assigned_to_me\"]",
        );
        let source = format!(
            "{source}\nagent.register_github_trigger(review_requested,name=\"review-again\",host=\"github.com\",repo=\"team/repo\",account=\"fixture\",events=[\"pr.review_requested\"],settings=SETTINGS,instructions=instruction_steps(STEPS),steps=STEPS)\n"
        );
        let Json(saved) = crate::automations::update(
            State(f.app.clone()),
            axum::extract::Path(def.id.clone()),
            Json(serde_json::from_value(json!({"source":source})).unwrap()),
        )
        .await
        .unwrap();
        let def = f
            .app
            .store
            .lock()
            .unwrap()
            .automation(saved["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let scope = scripts::resolve_all(
            &f.app.store.lock().unwrap().connection,
            &f.app.progress.engine(),
            trigger(&def).unwrap(),
        )
        .unwrap()
        .remove(0)
        .scope;
        f.app.github.seed_review_requests(&scope, json!({})).await;
        tick(&f.app).await.unwrap();
        assert!(events(&f.app.store.lock().unwrap().connection).is_empty());
        f.app
            .github
            .seed_review_requests(&scope, snapshot(17, false, true, &"a".repeat(40)))
            .await;
        tick(&f.app).await.unwrap();
        let rows = events(&f.app.store.lock().unwrap().connection);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1["recipe"]["handler"], "review-again");
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 1);
        f.app
            .github
            .seed_review_requests(&scope, snapshot(17, true, true, &"a".repeat(40)))
            .await;
        tick(&f.app).await.unwrap();
        assert_eq!(events(&f.app.store.lock().unwrap().connection).len(), 2);
        assert_eq!(f.s.data.lock().unwrap().input_queue.items.len(), 2);
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row(
                    "SELECT count(*) FROM automation_trigger_baselines WHERE automation_id=?1",
                    [&def.id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert_eq!(page.as_array().unwrap().len(), 1);
        assert_eq!(page[0]["registrations"].as_array().unwrap().len(), 2);
    }
}
