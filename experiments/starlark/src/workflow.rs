//! Rust-owned durable workflow host for workflow scripts.
//!
//! A script only maps `(state, event)` to a `Decision`. This host persists the
//! decision, its new state and proposed actions in one SQLite transaction, then
//! dispatches pending actions with stable request IDs after re-checking
//! authorization. Restarting re-dispatches only actions that were never marked
//! dispatched; the dispatcher deduplicates by request ID, so a crash between
//! dispatch and marking cannot create a second session. Cancellation, retries
//! and waiting are Rust's: no Starlark stack lives across events.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::engine::{Compiled, Diagnostic, Engine, HOST_API_VERSION};

const MAX_ACTIONS_PER_DECISION: usize = 4;
const MAX_ACTIONS_PER_RUN: i64 = 16;
const MAX_EVENTS_PER_RUN: i64 = 256;
const MAX_PROMPT: usize = 4000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub kind: String,
    pub key: String,
    pub project: String,
    pub prompt: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub state: serde_json::Map<String, Json>,
    pub actions: Vec<Action>,
    pub outcome: Option<String>,
}

impl Decision {
    fn validate(&self) -> Result<(), String> {
        if self.actions.len() > MAX_ACTIONS_PER_DECISION {
            return Err(format!("more than {MAX_ACTIONS_PER_DECISION} actions in one decision"));
        }
        for a in &self.actions {
            if a.kind != "start_session" {
                return Err(format!("unknown action kind {:?}", a.kind));
            }
            let key_ok = !a.key.is_empty()
                && a.key.len() <= 64
                && a.key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !key_ok {
                return Err(format!("invalid action key {:?}", a.key));
            }
            if a.project.is_empty() || a.prompt.is_empty() || a.prompt.len() > MAX_PROMPT {
                return Err("action project/prompt empty or prompt too long".into());
            }
        }
        if self.outcome.as_ref().is_some_and(|o| o.is_empty() || o.len() > 64) {
            return Err("outcome must be 1..=64 bytes".into());
        }
        Ok(())
    }
}

/// Dispatch-time authorization. Production would call the existing
/// project/workspace/grant checks (`communication::require_actor`) here.
pub trait Authorizer {
    fn authorize(&self, workflow: &Workflow, action: &Action) -> Result<(), String>;
}

/// Performs an effect. Must be idempotent by `request_id` (as `agent_starts`
/// deduplicates `control_start` by actor and request ID).
pub trait Dispatcher {
    fn dispatch(&mut self, request_id: &str, action: &Action) -> Result<String, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub id: String,
    pub owner: String,
    pub script: String,
    pub script_hash: String,
    pub api_version: u32,
    pub status: String,
    pub outcome: Option<String>,
    pub state: Json,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Applied,
    Duplicate,
    Ignored(&'static str),
}

#[derive(Debug)]
pub enum WorkflowError {
    Db(String),
    Script(Diagnostic),
    VersionMismatch { pinned: String, offered: String },
    NotFound,
    Bounds(String),
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::Db(e) => write!(f, "database: {e}"),
            WorkflowError::Script(d) => write!(f, "script: {d}"),
            WorkflowError::VersionMismatch { pinned, offered } => {
                write!(
                    f,
                    "workflow is pinned to script {pinned}, not {offered}; upgrade explicitly"
                )
            }
            WorkflowError::NotFound => write!(f, "workflow not found"),
            WorkflowError::Bounds(e) => write!(f, "bounds: {e}"),
        }
    }
}

impl From<rusqlite::Error> for WorkflowError {
    fn from(e: rusqlite::Error) -> Self {
        WorkflowError::Db(e.to_string())
    }
}

type WfResult<T> = std::result::Result<T, WorkflowError>;

pub struct WorkflowHost {
    pub db: Connection,
    engine: Arc<Engine>,
    script: Arc<Compiled>,
}

pub fn request_id(workflow: &str, key: &str) -> String {
    format!("wf:{workflow}:{key}")
}

impl WorkflowHost {
    /// Open (or reopen after restart) the store with the currently installed script.
    pub fn open(path: &std::path::Path, engine: Arc<Engine>, script: Arc<Compiled>) -> WfResult<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS workflows(
                id TEXT PRIMARY KEY, owner TEXT NOT NULL, script TEXT NOT NULL, script_hash TEXT NOT NULL,
                api_version INTEGER NOT NULL, status TEXT NOT NULL, outcome TEXT, state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_events(
                workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, event_id TEXT NOT NULL, event TEXT NOT NULL,
                script_hash TEXT NOT NULL, disposition TEXT NOT NULL,
                PRIMARY KEY(workflow_id, event_id), UNIQUE(workflow_id, seq));
            CREATE TABLE IF NOT EXISTS workflow_actions(
                workflow_id TEXT NOT NULL, seq INTEGER NOT NULL, action_key TEXT NOT NULL, request_id TEXT NOT NULL UNIQUE,
                action TEXT NOT NULL, status TEXT NOT NULL, detail TEXT,
                PRIMARY KEY(workflow_id, action_key));",
        )?;
        Ok(WorkflowHost { db, engine, script })
    }

    pub fn get(&self, id: &str) -> WfResult<Workflow> {
        self.db
            .query_row(
                "SELECT id,owner,script,script_hash,api_version,status,outcome,state FROM workflows WHERE id=?1",
                [id],
                |r| {
                    Ok(Workflow {
                        id: r.get(0)?,
                        owner: r.get(1)?,
                        script: r.get(2)?,
                        script_hash: r.get(3)?,
                        api_version: r.get(4)?,
                        status: r.get(5)?,
                        outcome: r.get(6)?,
                        state: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(Json::Null),
                    })
                },
            )
            .optional()?
            .ok_or(WorkflowError::NotFound)
    }

    /// Create a run pinned to the installed script and deliver its start event.
    pub fn start(&mut self, id: &str, owner: &str, data: Json) -> WfResult<Delivery> {
        self.db.execute(
            "INSERT OR IGNORE INTO workflows(id,owner,script,script_hash,api_version,status,state) VALUES (?1,?2,?3,?4,?5,'running','{}')",
            params![id, owner, self.script.name, self.script.hash, HOST_API_VERSION],
        )?;
        self.deliver(id, json!({"id": "start", "kind": "start", "data": data}))
    }

    /// Explicit upgrade path: re-pin a running workflow to the installed script.
    /// The existing state is kept; the next event is evaluated by the new version.
    pub fn upgrade(&mut self, id: &str) -> WfResult<()> {
        let tx = self.db.transaction()?;
        let previous: String = tx
            .query_row("SELECT script_hash FROM workflows WHERE id=?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(WorkflowError::NotFound)?;
        tx.execute(
            "UPDATE workflows SET script_hash=?2 WHERE id=?1",
            params![id, self.script.hash],
        )?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM workflow_events WHERE workflow_id=?1",
            [id],
            |r| r.get(0),
        )?;
        let event =
            json!({"id": format!("upgrade:{seq}"), "kind": "host_upgrade", "from": previous, "to": self.script.hash});
        tx.execute(
            "INSERT INTO workflow_events(workflow_id,seq,event_id,event,script_hash,disposition) VALUES (?1,?2,?3,?4,?5,'host')",
            params![id, seq, event["id"].as_str(), event.to_string(), self.script.hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Report an effect's completion. The event ID is derived from the stable
    /// request ID, so duplicate completions are recognised.
    pub fn complete_action(&mut self, id: &str, key: &str, outcome: &str, data: Json) -> WfResult<Delivery> {
        let event_id = format!("{}:completed", request_id(id, key));
        self.db.execute(
            "UPDATE workflow_actions SET status='completed', detail=?3 WHERE workflow_id=?1 AND action_key=?2 AND status='dispatched'",
            params![id, key, outcome],
        )?;
        self.deliver(
            id,
            json!({"id": event_id, "kind": "action_completed", "action_key": key, "outcome": outcome, "data": data}),
        )
    }

    /// Human/host cancellation. Pending actions are cancelled, not dispatched later.
    pub fn cancel(&mut self, id: &str) -> WfResult<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE workflows SET status='cancelled', outcome='cancelled' WHERE id=?1 AND status='running'",
            [id],
        )?;
        tx.execute(
            "UPDATE workflow_actions SET status='cancelled' WHERE workflow_id=?1 AND status='pending'",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Evaluate one event and persist the decision atomically.
    pub fn deliver(&mut self, id: &str, event: Json) -> WfResult<Delivery> {
        let wf = self.get(id)?;
        let event_id = event["id"]
            .as_str()
            .ok_or_else(|| WorkflowError::Bounds("event without id".into()))?
            .to_owned();
        let seen: bool = self
            .db
            .query_row(
                "SELECT 1 FROM workflow_events WHERE workflow_id=?1 AND event_id=?2",
                params![id, event_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if seen {
            return Ok(Delivery::Duplicate);
        }
        let seq: i64 = self.db.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM workflow_events WHERE workflow_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if seq > MAX_EVENTS_PER_RUN {
            return Err(WorkflowError::Bounds(format!("more than {MAX_EVENTS_PER_RUN} events")));
        }
        if wf.status != "running" {
            self.record(id, seq, &event_id, &event, &wf.script_hash, "ignored_not_running")?;
            return Ok(Delivery::Ignored("workflow is not running"));
        }
        if wf.script_hash != self.script.hash {
            return Err(WorkflowError::VersionMismatch {
                pinned: wf.script_hash,
                offered: self.script.hash.clone(),
            });
        }
        let decision = match self.evaluate(&wf.state, &event) {
            Ok(d) => d,
            Err(d) => {
                // Attributable failure: record it, fail the run, propose nothing.
                let tx = self.db.transaction()?;
                tx.execute(
                    "INSERT INTO workflow_events(workflow_id,seq,event_id,event,script_hash,disposition) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id, seq, event_id, event.to_string(), wf.script_hash, format!("script_error: {d}")],
                )?;
                tx.execute(
                    "UPDATE workflows SET status='failed', outcome='script_error' WHERE id=?1",
                    [id],
                )?;
                tx.execute(
                    "UPDATE workflow_actions SET status='cancelled' WHERE workflow_id=?1 AND status='pending'",
                    [id],
                )?;
                tx.commit()?;
                return Err(WorkflowError::Script(d));
            }
        };
        let tx = self.db.transaction()?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workflow_actions WHERE workflow_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if existing + decision.actions.len() as i64 > MAX_ACTIONS_PER_RUN {
            return Err(WorkflowError::Bounds(format!(
                "more than {MAX_ACTIONS_PER_RUN} actions in one run"
            )));
        }
        tx.execute(
            "INSERT INTO workflow_events(workflow_id,seq,event_id,event,script_hash,disposition) VALUES (?1,?2,?3,?4,?5,'applied')",
            params![id, seq, event_id, event.to_string(), wf.script_hash],
        )?;
        for action in &decision.actions {
            // Same key twice is the same effect: never inserted twice.
            tx.execute(
                "INSERT OR IGNORE INTO workflow_actions(workflow_id,seq,action_key,request_id,action,status) VALUES (?1,?2,?3,?4,?5,'pending')",
                params![id, seq, action.key, request_id(id, &action.key), serde_json::to_string(action).unwrap()],
            )?;
        }
        let state = Json::Object(decision.state.clone()).to_string();
        match &decision.outcome {
            Some(outcome) => tx.execute(
                "UPDATE workflows SET state=?2, status='completed', outcome=?3 WHERE id=?1",
                params![id, state, outcome],
            )?,
            None => tx.execute("UPDATE workflows SET state=?2 WHERE id=?1", params![id, state])?,
        };
        tx.commit()?;
        Ok(Delivery::Applied)
    }

    fn record(&self, id: &str, seq: i64, event_id: &str, event: &Json, hash: &str, disposition: &str) -> WfResult<()> {
        self.db.execute(
            "INSERT INTO workflow_events(workflow_id,seq,event_id,event,script_hash,disposition) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, seq, event_id, event.to_string(), hash, disposition],
        )?;
        Ok(())
    }

    fn evaluate(&self, state: &Json, event: &Json) -> std::result::Result<Decision, Diagnostic> {
        let (value, _) = self.engine.call(&self.script, state, event)?;
        let decision: Decision = serde_json::from_value(value)
            .map_err(|e| Engine::invalid(&self.script, format!("result does not match host API v1: {e}")))?;
        decision.validate().map_err(|e| Engine::invalid(&self.script, e))?;
        Ok(decision)
    }

    /// Dispatch every pending action of running workflows, re-checking
    /// authorization now. Returns the request IDs dispatched by this call.
    pub fn dispatch_pending(
        &mut self,
        auth: &dyn Authorizer,
        dispatcher: &mut dyn Dispatcher,
    ) -> WfResult<Vec<String>> {
        let pending: Vec<(String, String, String, String)> = {
            let mut stmt = self.db.prepare(
                "SELECT a.workflow_id, a.action_key, a.request_id, a.action FROM workflow_actions a
                 JOIN workflows w ON w.id=a.workflow_id
                 WHERE a.status='pending' AND w.status='running' ORDER BY a.workflow_id, a.seq, a.action_key",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut sent = Vec::new();
        for (wf_id, key, request, action) in pending {
            let action: Action = serde_json::from_str(&action).map_err(|e| WorkflowError::Db(e.to_string()))?;
            let wf = self.get(&wf_id)?;
            if let Err(reason) = auth.authorize(&wf, &action) {
                self.db.execute(
                    "UPDATE workflow_actions SET status='denied', detail=?3 WHERE workflow_id=?1 AND action_key=?2",
                    params![wf_id, key, reason],
                )?;
                let event = json!({"id": format!("{request}:denied"), "kind": "action_denied", "action_key": key,
                    "data": {"reason": reason}});
                self.deliver(&wf_id, event)?;
                continue;
            }
            match dispatcher.dispatch(&request, &action) {
                Ok(session) => {
                    self.db.execute(
                        "UPDATE workflow_actions SET status='dispatched', detail=?3 WHERE workflow_id=?1 AND action_key=?2",
                        params![wf_id, key, session],
                    )?;
                    sent.push(request);
                }
                // Left pending: a later dispatch reuses the same request ID.
                Err(e) => return Err(WorkflowError::Db(format!("dispatch {request}: {e}"))),
            }
        }
        Ok(sent)
    }

    pub fn actions(&self, id: &str) -> WfResult<Vec<(String, String)>> {
        let mut stmt = self
            .db
            .prepare("SELECT action_key, status FROM workflow_actions WHERE workflow_id=?1 ORDER BY seq, action_key")?;
        let rows = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Deterministic replay: re-evaluate persisted applied events from empty
    /// state without dispatching anything. Returns the rebuilt state and the
    /// proposed action keys, for comparison with what was committed.
    pub fn replay(&self, id: &str) -> WfResult<(Json, Vec<String>, Option<String>)> {
        let events: Vec<String> = {
            let mut stmt = self.db.prepare(
                "SELECT event FROM workflow_events WHERE workflow_id=?1 AND disposition='applied' ORDER BY seq",
            )?;
            stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut state = json!({});
        let mut keys = Vec::new();
        let mut outcome = None;
        for e in events {
            let event: Json = serde_json::from_str(&e).map_err(|e| WorkflowError::Db(e.to_string()))?;
            let d = self.evaluate(&state, &event).map_err(WorkflowError::Script)?;
            keys.extend(d.actions.iter().map(|a| a.key.clone()));
            state = Json::Object(d.state);
            outcome = d.outcome.or(outcome);
        }
        Ok((state, keys, outcome))
    }
}
