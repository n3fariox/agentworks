//! Resolve a script-owned subscription and validate its proposed effects.
use super::*;
use agentworks_starlark::engine::{Compiled, Engine};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptTrigger {
    pub script: String,
    #[serde(default = "default_handler")]
    pub handler: String,
    #[serde(default)]
    pub parameters: serde_json::Map<String, Value>,
}
fn default_handler() -> String {
    "default".into()
}
impl ScriptTrigger {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::tool_progress::valid_name(&self.script)
            || self.handler.is_empty()
            || self.handler.len() > 32
            || self.parameters.len() > 32
            || json!(self.parameters).to_string().len() > 8192
        {
            return Err("Choose a script trigger and parameters up to 8 KB");
        }
        Ok(())
    }
}
pub(super) struct Resolved {
    pub scope: GithubReview,
    pub script: Arc<Compiled>,
    pub source: String,
}
pub(super) fn binding(def: &Definition) -> Option<&ScriptTrigger> {
    match &def.workflow.as_ref()?.trigger {
        Trigger::Script(t) => Some(t),
        _ => None,
    }
}
pub(super) fn resolve(
    db: &Connection,
    engine: &Engine,
    binding: &ScriptTrigger,
) -> Result<Resolved, String> {
    let source: Option<String> = db
        .query_row(
            "SELECT source FROM scripts WHERE name=?1 AND enabled=1 AND capability='automation'",
            [&binding.script],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let source = source.ok_or("The automation script is missing or disabled")?;
    resolve_source(engine, binding, &source)
}
pub(super) fn resolve_source(
    engine: &Engine,
    binding: &ScriptTrigger,
    source: &str,
) -> Result<Resolved, String> {
    let module = engine
        .compile_automation(&binding.script, source)
        .map_err(|e| e.to_string())?;
    let matches: Vec<_> = module
        .handlers
        .iter()
        .filter(|h| {
            h.automation_trigger
                .as_ref()
                .is_some_and(|t| t["name"] == binding.handler)
        })
        .collect();
    if matches.len() != 1 {
        return Err("Choose exactly one named GitHub trigger registered by this script".into());
    }
    let script = matches[0].clone();
    let descriptor = script.automation_trigger.as_ref().unwrap();
    let field = |name: &str| -> Result<String, String> {
        let value = descriptor[name].as_str().ok_or("Invalid trigger field")?;
        if let Some(key) = value.strip_prefix('$') {
            binding
                .parameters
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("Supply the script parameter {key:?} as a string"))
        } else {
            Ok(value.into())
        }
    };
    let repo = field("repo")?;
    let (owner, name) = repo.split_once('/').ok_or("Use repository as owner/repo")?;
    let events = descriptor["events"]
        .as_array()
        .ok_or("Missing trigger events")?;
    let mut scope = GithubReview {
        host: field("host")?,
        owner: owner.into(),
        repo: name.into(),
        login: field("account")?,
        assigned: events.contains(&json!("pr.assigned_to_me")),
        review_requested: events.contains(&json!("pr.review_requested")),
        include_existing: descriptor["includeExisting"] == true,
    };
    scope.validate().map_err(str::to_owned)?;
    Ok(Resolved {
        scope,
        script,
        source: source.into(),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Effect {
    pub kind: String,
    pub key: String,
    pub project: Option<String>,
    pub prompt: Option<String>,
    pub outcome: Option<String>,
    pub target: Option<String>,
    pub settings: Option<Value>,
    pub workspace: Option<Value>,
    pub detail: Option<String>,
}
pub(super) fn effects(value: &Value) -> Result<Vec<Effect>, String> {
    if value.get("badgeEffects").is_some() {
        return Err("Workflow effects must be returned as actions".into());
    }
    let effects: Vec<Effect> =
        serde_json::from_value(value["result"].clone()).map_err(|e| e.to_string())?;
    if effects.len() > 4 {
        return Err("At most four actions per event".into());
    }
    let mut keys = std::collections::HashSet::new();
    for e in &effects {
        if e.key.is_empty()
            || e.key.len() > 32
            || !e
                .key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || !keys.insert(&e.key)
        {
            return Err("Action keys must be unique, 1–32 letters, digits, _ or -".into());
        }
        if e.project.is_some() || e.outcome.is_some() {
            return Err(
                "Use settings for session configuration and skip for a skipped event".into(),
            );
        }
        let valid = match e.kind.as_str() {
            "skip" => {
                effects.len() == 1
                    && e.detail
                        .as_ref()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 1024)
                    && e.target.is_none()
                    && e.prompt.is_none()
                    && e.settings.is_none()
                    && e.workspace.is_none()
            }
            "queue_input" => {
                e.target.as_ref().is_some_and(|s| !s.is_empty())
                    && e.prompt
                        .as_ref()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 32768)
                    && e.settings.is_none()
                    && e.workspace.is_none()
                    && e.detail.is_none()
            }
            "start_session" => {
                e.settings.as_ref().is_some_and(Value::is_object)
                    && e.prompt
                        .as_ref()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 32768)
                    && e.target.is_none()
                    && e.detail.is_none()
            }
            _ => false,
        };
        if !valid {
            return Err(format!("Invalid {} action", e.kind));
        }
    }
    Ok(effects)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Settings {
    pub provider: String,
    pub cwd: String,
    pub project_id: String,
    pub host_id: String,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub title: Option<String>,
    pub role: Option<String>,
}

/// Every trigger in a file belongs to one automation. This is also the import
/// boundary: source supplies its configuration; no saved form is consulted.
pub(super) fn resolve_all_source(
    engine: &Engine,
    binding: &ScriptTrigger,
    source: &str,
) -> Result<Vec<Resolved>, String> {
    let module = engine
        .compile_automation(&binding.script, source)
        .map_err(|e| e.to_string())?;
    module
        .handlers
        .iter()
        .map(|handler| {
            let mut binding = binding.clone();
            binding.handler = handler.automation_trigger.as_ref().unwrap()["name"]
                .as_str()
                .unwrap()
                .into();
            resolve_source(engine, &binding, source)
        })
        .collect()
}
pub(super) fn resolve_all(
    db: &Connection,
    engine: &Engine,
    binding: &ScriptTrigger,
) -> Result<Vec<Resolved>, String> {
    let source: String = db
        .query_row(
            "SELECT source FROM scripts WHERE name=?1 AND capability='automation' AND enabled=1",
            [&binding.script],
            |r| r.get(0),
        )
        .map_err(|_| "The automation script is missing or disabled")?;
    resolve_all_source(engine, binding, &source)
}
pub(super) fn manifest(
    engine: &Engine,
    name: &str,
    source: &str,
) -> Result<(Value, Vec<Resolved>), String> {
    let recipes = resolve_all_source(
        engine,
        &ScriptTrigger {
            script: name.into(),
            handler: "default".into(),
            parameters: Default::default(),
        },
        source,
    )?;
    let primary = recipes
        .iter()
        .find(|r| r.script.automation_trigger.as_ref().unwrap()["name"] == "default")
        .unwrap_or(&recipes[0]);
    let descriptor = primary.script.automation_trigger.clone().unwrap();
    let settings: Settings = serde_json::from_value(descriptor["settings"].clone())
        .map_err(|e| format!("Register session settings in the automation file: {e}"))?;
    for recipe in &recipes {
        let d = recipe.script.automation_trigger.as_ref().unwrap();
        let other: Settings = serde_json::from_value(d["settings"].clone())
            .map_err(|e| format!("Invalid trigger settings: {e}"))?;
        if other.project_id != settings.project_id || other.host_id != settings.host_id {
            return Err(
                "All triggers in one automation file must use the same project and execution host"
                    .into(),
            );
        }
        if !d["instructions"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 32768)
        {
            return Err("Each automation trigger must register its instructions".into());
        }
    }
    Ok((descriptor, recipes))
}

/// Migrate the earlier mixed script list without dropping user edits. The
/// shipped template is now code for the generator, not a global hook script.
pub(crate) fn migrate(db: &Connection, engine: &Engine) -> Result<(), String> {
    let has_definitions: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='automations')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if has_definitions {
        db.execute("DELETE FROM scripts WHERE name='github_review.star' AND builtin=1 AND NOT EXISTS(SELECT 1 FROM automations WHERE json_extract(definition,'$.workflow.trigger.script')='github_review.star')",[]).map_err(|e|e.to_string())?;
    } else {
        db.execute(
            "DELETE FROM scripts WHERE name='github_review.star' AND builtin=1",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    let mut q=db.prepare("SELECT name,source FROM scripts WHERE capability='hooks' AND instr(source,'register_github_trigger')>0").map_err(|e|e.to_string())?;
    let candidates = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    for (name, source) in candidates {
        if engine.compile_automation(&name, &source).is_ok() {
            db.execute(
                "UPDATE scripts SET capability='automation',builtin=0 WHERE name=?1",
                [name],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Rebuild only the operational index. Valid source supplies every display
/// field; missing files remove entries and cancel pending work, keeping history.
pub(super) fn reconcile(app: &App) -> Result<(), (StatusCode, String)> {
    let engine = app.progress.engine();
    let files: Vec<(String, String, String, bool, u64)> = {
        let store = app.store.lock().unwrap();
        let mut q=store.connection.prepare("SELECT name,source,hash,enabled,updated FROM scripts WHERE capability='automation' ORDER BY name").map_err(storage)?;
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, i64>(4)? as u64,
            ))
        })
        .map_err(storage)?
        .collect::<rusqlite::Result<_>>()
        .map_err(storage)?
    };
    let definitions = app
        .store
        .lock()
        .unwrap()
        .usable_automations()
        .map_err(storage)?;
    let known: std::collections::HashSet<_> = files.iter().map(|(n, ..)| n.as_str()).collect();
    for def in &definitions {
        if let Some(binding) = binding(def)
            && !known.contains(binding.script.as_str())
        {
            app.store
                .lock()
                .unwrap()
                .delete_automation(&def.id, None)
                .map_err(storage)?;
        }
    }
    for (name, source, hash, enabled, updated) in files {
        let mut previous: Vec<_> = definitions
            .iter()
            .filter(|d| binding(d).is_some_and(|b| b.script == name))
            .collect();
        previous.sort_by_key(|d| (d.created, d.id.clone()));
        let old = previous.first().copied();
        // One file owns exactly one entry, including after an older duplicate binding.
        for duplicate in previous.iter().skip(1) {
            // Unlink before removing the stale index so its shared file survives.
            let mut d = (*duplicate).clone();
            d.workflow = None;
            let mut store = app.store.lock().unwrap();
            store.save_automation(&d, None, None).map_err(storage)?;
            store.delete_automation(&d.id, None).map_err(storage)?;
        }
        let actual_hash = agentworks_starlark::engine::content_hash(&source);
        if hash != actual_hash {
            app.store
                .lock()
                .unwrap()
                .connection
                .execute(
                    "UPDATE scripts SET hash=?2 WHERE name=?1",
                    rusqlite::params![name, actual_hash],
                )
                .map_err(storage)?;
        }
        let hash = actual_hash;
        let (descriptor, _) = match manifest(&engine, &name, &source) {
            Ok(m) => m,
            Err(error) => {
                // An old or externally added invalid file still needs an entry
                // so it can be repaired/exported/deleted from Automations.
                app.store
                    .lock()
                    .unwrap()
                    .connection
                    .execute("UPDATE scripts SET enabled=0 WHERE name=?1", [&name])
                    .map_err(storage)?;
                if let Some(old) = old {
                    if old.script_error.as_deref() == Some(&error)
                        && old.script_hash.as_deref() == Some(&hash)
                        && !old.enabled
                    {
                        continue;
                    }
                    let mut def = old.clone();
                    def.script_error = Some(error);
                    def.script_hash = Some(hash.clone());
                    def.enabled = false;
                    def.revision += 1;
                    app.store
                        .lock()
                        .unwrap()
                        .save_automation(&def, None, None)
                        .map_err(storage)?;
                } else {
                    let def = Definition {
                        id: Uuid::new_v4().to_string(),
                        name: name.trim_end_matches(".star").into(),
                        provider: String::new(),
                        model: None,
                        model_provider: None,
                        cwd: String::new(),
                        location_id: String::new(),
                        host_id: "local".into(),
                        project_id: None,
                        prompt: String::new(),
                        workflow: Some(Workflow {
                            version: 1,
                            trigger: Trigger::Script(ScriptTrigger {
                                script: name.clone(),
                                handler: "default".into(),
                                parameters: Default::default(),
                            }),
                            steps: Vec::new(),
                        }),
                        enabled: false,
                        created: updated,
                        updated,
                        created_by: None,
                        revision: 1,
                        completed: None,
                        script_hash: Some(hash.clone()),
                        script_error: Some(error),
                    };
                    app.store
                        .lock()
                        .unwrap()
                        .save_automation(&def, None, None)
                        .map_err(storage)?;
                }
                continue;
            }
        };
        let settings: Settings =
            serde_json::from_value(descriptor["settings"].clone()).map_err(storage)?;
        let valid_project = app.catalog.lock().unwrap().contains(&settings.project_id);
        if !valid_project && enabled {
            app.store
                .lock()
                .unwrap()
                .connection
                .execute("UPDATE scripts SET enabled=0 WHERE name=?1", [&name])
                .map_err(storage)?;
        }
        let prompt = descriptor["instructions"].as_str().unwrap().to_owned();
        let steps = super::generator::steps(&descriptor);
        let location_id = app
            .catalog
            .lock()
            .unwrap()
            .data
            .locations
            .iter()
            .find(|l| l.host_id == settings.host_id && l.path.to_string_lossy() == settings.cwd)
            .map(|l| l.id.clone())
            .unwrap_or_default();
        let def = Definition {
            id: old.map_or_else(|| Uuid::new_v4().to_string(), |d| d.id.clone()),
            name: settings
                .title
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| name.trim_end_matches(".star").into()),
            provider: settings.provider,
            model: settings.model,
            model_provider: settings.model_provider,
            cwd: settings.cwd,
            host_id: settings.host_id,
            project_id: valid_project.then_some(settings.project_id),
            location_id,
            prompt,
            workflow: Some(Workflow {
                version: 1,
                trigger: Trigger::Script(ScriptTrigger {
                    script: name,
                    handler: descriptor["name"].as_str().unwrap().into(),
                    parameters: Default::default(),
                }),
                steps,
            }),
            enabled: enabled && valid_project,
            created: old.map_or(updated, |d| d.created),
            updated,
            created_by: old.and_then(|d| d.created_by.clone()),
            revision: old.map_or(1, |d| d.revision + 1),
            completed: None,
            script_hash: Some(hash),
            script_error: (!valid_project)
                .then(|| "The script's project is missing; update its configuration".into()),
        };
        if let Some(old) = old {
            let mut same = def.clone();
            same.revision = old.revision;
            same.updated = old.updated;
            if json!(old) == json!(same) {
                continue;
            }
        }
        app.store
            .lock()
            .unwrap()
            .save_automation(&def, None, None)
            .map_err(storage)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;
    fn source(
        f: &crate::github_tracking::tests::Fixture,
        title: &str,
        instructions: &str,
    ) -> String {
        let form=serde_json::from_value(json!({"host":"github.com","repo":"team/repo","account":"fixture","settings":{"provider":"demo","cwd":f.s.cwd,"projectId":"p","hostId":"local","title":title},"steps":[{"kind":"agent","text":instructions}]})).unwrap();
        super::super::generator::generate(&form).unwrap()
    }
    async fn fixture() -> crate::github_tracking::tests::Fixture {
        let f = crate::github_tracking::tests::Fixture::new().await;
        f.app
            .catalog
            .lock()
            .unwrap()
            .data
            .projects
            .push(serde_json::from_value(json!({"id":"p","name":"Reviews"})).unwrap());
        f
    }
    fn add(app: &App, name: &str, source: &str) {
        app.store.lock().unwrap().connection.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'automation',?2,?3,0,0,1)",rusqlite::params![name,source,agentworks_starlark::engine::content_hash(source)]).unwrap();
    }
    #[tokio::test]
    async fn files_populate_entries_and_source_edits_refresh_the_display_without_a_form() {
        let f = fixture().await;
        let first = source(&f, "Handwritten review", "First instructions");
        add(&f.app, "handwritten.star", &first);
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        let entries = page.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry["name"], "Handwritten review");
        assert_eq!(entry["workflow"]["steps"][0]["text"], "First instructions");
        assert_eq!(entry["registrations"].as_array().unwrap().len(), 1);
        let id = entry["id"].as_str().unwrap().to_owned();
        let second = source(&f, "Changed in source", "Edited instructions");
        // Even a direct source edit with no form update (or hash update) is detected.
        f.app
            .store
            .lock()
            .unwrap()
            .connection
            .execute(
                "UPDATE scripts SET source=?1 WHERE name='handwritten.star'",
                [&second],
            )
            .unwrap();
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert_eq!(page[0]["id"], id);
        assert_eq!(page[0]["name"], "Changed in source");
        assert_eq!(page[0]["prompt"], "Edited instructions");
        let hooks = crate::tool_progress::list(&f.app.store.lock().unwrap().connection).unwrap();
        assert!(!hooks.iter().any(|s| s.name == "handwritten.star"));
        f.app.store.lock().unwrap().connection.execute("UPDATE automations SET definition=json_set(definition,'$.name','cached form memory','$.prompt','stale form instructions') WHERE id=?1",[&id]).unwrap();
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert_eq!(page[0]["name"], "Changed in source");
        assert_eq!(page[0]["prompt"], "Edited instructions");
        let rev = page[0]["revision"].clone();
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert_eq!(page[0]["revision"], rev);
    }
    #[tokio::test]
    async fn deleting_a_file_removes_its_entry_cancels_pending_actions_and_keeps_history() {
        let f = fixture().await;
        add(&f.app, "owned.star", &source(&f, "Owned", "Review"));
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        let id = page[0]["id"].as_str().unwrap();
        {
            let store = f.app.store.lock().unwrap();
            store.connection.execute("INSERT INTO automation_events(id,automation_id,revision,payload,created,status) VALUES ('old',?1,1,'{\"number\":17}',1,'completed'),('pending',?1,1,'{\"number\":18}',2,'pending')",[id]).unwrap();
            store.connection.execute("INSERT INTO automation_event_actions(event_id,action_key,payload) VALUES ('pending','start','{}')",[]).unwrap();
            store
                .connection
                .execute("DELETE FROM scripts WHERE name='owned.star'", [])
                .unwrap();
        }
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert!(page.as_array().unwrap().is_empty());
        let runs = f.app.store.lock().unwrap().automation_runs(id).unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().any(|r| r["status"] == "completed"));
        assert!(runs.iter().any(|r| r["status"] == "skipped"));
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row(
                    "SELECT status FROM automation_event_actions WHERE event_id='pending'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "skipped"
        );
        add(&f.app, "owned.star", &source(&f, "Owned again", "Review"));
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        let next = page[0]["id"].as_str().unwrap();
        let _ = crate::automations::delete(State(f.app.clone()), Path(next.into()))
            .await
            .unwrap();
        assert!(
            !f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM scripts WHERE name='owned.star')",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
        );
    }
    #[tokio::test]
    async fn hook_settings_cannot_mutate_automation_files_and_hook_code_cannot_be_imported() {
        let f = fixture().await;
        let source = source(&f, "Owned", "Review");
        add(&f.app, "owned.star", &source);
        for result in [
            crate::tool_progress::put_script(
                State(f.app.clone()),
                Path("owned.star".into()),
                Json(serde_json::from_value(json!({"source":"invalid"})).unwrap()),
            )
            .await,
            crate::tool_progress::put_enabled(
                State(f.app.clone()),
                Path("owned.star".into()),
                Json(serde_json::from_value(json!({"enabled":true})).unwrap()),
            )
            .await,
            crate::tool_progress::put_priority(
                State(f.app.clone()),
                Path("owned.star".into()),
                Json(serde_json::from_value(json!({"priority":1})).unwrap()),
            )
            .await,
            crate::tool_progress::delete_script(State(f.app.clone()), Path("owned.star".into()))
                .await,
        ] {
            assert_eq!(result.err().unwrap().0, StatusCode::CONFLICT);
        }
        let hook = agentworks_starlark::INBOX_MESSAGE_SCRIPT;
        let input = serde_json::from_value(
            json!({"source":hook,"scriptName":"wrong_kind.star","enabled":false}),
        )
        .unwrap();
        assert_eq!(
            crate::automations::create(State(f.app.clone()), Json(input))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row(
                    "SELECT source FROM scripts WHERE name='owned.star'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            source
        );
        let result = crate::tool_progress::put_script(
            State(f.app.clone()),
            Path("sneaky.star".into()),
            Json(serde_json::from_value(json!({"source":source})).unwrap()),
        )
        .await;
        assert_eq!(result.err().unwrap().0, StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn migration_classifies_older_automation_sources_without_losing_user_edits() {
        let f = fixture().await;
        let source = source(&f, "Migrated", "Preserve my instructions");
        f.app.store.lock().unwrap().connection.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES ('older.star','hooks',?1,?2,0,0,1)",rusqlite::params![source,agentworks_starlark::engine::content_hash(&source)]).unwrap();
        migrate(
            &f.app.store.lock().unwrap().connection,
            &f.app.progress.engine(),
        )
        .unwrap();
        assert_eq!(
            f.app
                .store
                .lock()
                .unwrap()
                .connection
                .query_row(
                    "SELECT capability,source,enabled FROM scripts WHERE name='older.star'",
                    [],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, bool>(2)?
                    ))
                )
                .unwrap(),
            ("automation".into(), source, false)
        );
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert_eq!(page[0]["name"], "Migrated");
        assert_eq!(page[0]["enabled"], false);
    }
    #[test]
    fn version_14_default_poll_baselines_migrate_once_without_replaying_old_snapshots() {
        let path = std::env::temp_dir().join(format!("automation-v14-{}.sqlite3", Uuid::new_v4()));
        {
            let store = crate::store::Store::open(&path).unwrap();
            store.connection.execute_batch("DROP TABLE automation_trigger_baselines; INSERT INTO automation_github_baselines VALUES ('old','scope','{\"17\":{\"assigned\":true}}'); PRAGMA user_version=14;").unwrap();
        }
        {
            let store = crate::store::Store::open(&path).unwrap();
            assert_eq!(store.connection.query_row("SELECT trigger_name,snapshot FROM automation_trigger_baselines WHERE automation_id='old'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap(),("default".into(),"{\"17\":{\"assigned\":true}}".into()));
            assert_eq!(
                store
                    .connection
                    .query_row(
                        "SELECT count(*) FROM automation_github_baselines",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            store.connection.execute("UPDATE automation_trigger_baselines SET snapshot='new snapshot' WHERE automation_id='old'",[]).unwrap();
        }
        {
            let store = crate::store::Store::open(&path).unwrap();
            assert_eq!(store.connection.query_row("SELECT snapshot FROM automation_trigger_baselines WHERE automation_id='old'",[],|r|r.get::<_,String>(0)).unwrap(),"new snapshot");
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let _ = std::fs::remove_file(path.with_extension("sqlite3.lock"));
    }
    #[tokio::test]
    async fn invalid_legacy_orphan_files_remain_visible_and_repairable_in_automations() {
        let f = fixture().await;
        let source = format!(
            "{}\n# User's older reusable template\n",
            agentworks_starlark::GITHUB_REVIEW_SCRIPT
        );
        add(&f.app, "unfinished.star", &source);
        let Json(page) = crate::automations::list(State(f.app.clone()))
            .await
            .unwrap();
        assert_eq!(page.as_array().unwrap().len(), 1);
        assert_eq!(page[0]["enabled"], false);
        assert!(page[0]["scriptError"].as_str().is_some());
        let Json(file) = crate::automations::get_script(
            State(f.app.clone()),
            Path(page[0]["id"].as_str().unwrap().into()),
        )
        .await
        .unwrap();
        assert_eq!(file["source"], source);
        assert_eq!(file["form"], Value::Null);
        assert!(file["error"].as_str().is_some());
    }
}
