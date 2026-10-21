//! Deterministic conditional two-step workflow with fake effects (no inference).
mod common;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agentworks_starlark::engine::{Capability, Compiled, DiagnosticKind, Engine};
use agentworks_starlark::workflow::{
    Action, Authorizer, Delivery, Dispatcher, Workflow, WorkflowError, WorkflowHost, request_id,
};
use common::*;
use rusqlite::{Connection, params};
use serde_json::json;

/// Durable fake session launcher, deduplicating by request ID like `agent_starts`.
struct FakeSessions(Connection);
impl FakeSessions {
    fn open(dir: &Path) -> Self {
        let db = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS sessions(request_id TEXT PRIMARY KEY, project TEXT, prompt TEXT)")
            .unwrap();
        FakeSessions(db)
    }
    fn count(&self) -> i64 {
        self.0
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap()
    }
}
impl Dispatcher for FakeSessions {
    fn dispatch(&mut self, request_id: &str, action: &Action) -> Result<String, String> {
        self.0
            .execute(
                "INSERT OR IGNORE INTO sessions VALUES (?1,?2,?3)",
                params![request_id, action.project, action.prompt],
            )
            .map_err(|e| e.to_string())?;
        Ok(format!("session-for-{request_id}"))
    }
}

/// Grants evaluated at dispatch time; revocation takes effect immediately.
struct Grants(RefCell<HashSet<(String, String)>>);
impl Grants {
    fn allow(owner: &str, project: &str) -> Self {
        Grants(RefCell::new(HashSet::from([(owner.to_owned(), project.to_owned())])))
    }
    fn revoke_all(&self) {
        self.0.borrow_mut().clear();
    }
}
impl Authorizer for Grants {
    fn authorize(&self, wf: &Workflow, action: &Action) -> Result<(), String> {
        if self.0.borrow().contains(&(wf.owner.clone(), action.project.clone())) {
            Ok(())
        } else {
            Err(format!("{} may not start sessions in {}", wf.owner, action.project))
        }
    }
}

struct Fixture {
    dir: PathBuf,
    engine: Arc<Engine>,
    script: Arc<Compiled>,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("agentworks-e01-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = engine();
        let script = install(&engine, "check_then_fix.star", Capability::Workflow, WORKFLOW).unwrap();
        Fixture { dir, engine, script }
    }
    /// Opening again simulates a service restart: only SQLite survives.
    fn host(&self) -> WorkflowHost {
        WorkflowHost::open(
            &self.dir.join("workflows.sqlite3"),
            Arc::clone(&self.engine),
            Arc::clone(&self.script),
        )
        .unwrap()
    }
    fn host_with(&self, script: Arc<Compiled>) -> WorkflowHost {
        WorkflowHost::open(&self.dir.join("workflows.sqlite3"), Arc::clone(&self.engine), script).unwrap()
    }
    fn sessions(&self) -> FakeSessions {
        FakeSessions::open(&self.dir)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn keys(host: &WorkflowHost, id: &str) -> Vec<(String, String)> {
    host.actions(id).unwrap()
}
fn pair(k: &str, s: &str) -> (String, String) {
    (k.into(), s.into())
}

#[test]
fn success_runs_only_the_first_step() {
    let f = Fixture::new("success");
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host();
    host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
    assert_eq!(
        host.dispatch_pending(&grants, &mut sessions).unwrap(),
        vec![request_id("wf1", "check")]
    );
    assert_eq!(
        host.complete_action("wf1", "check", "succeeded", json!({})).unwrap(),
        Delivery::Applied
    );
    let wf = host.get("wf1").unwrap();
    assert_eq!(
        (wf.status.as_str(), wf.outcome.as_deref()),
        ("completed", Some("passed"))
    );
    assert_eq!(keys(&host, "wf1"), vec![pair("check", "completed")]);
    assert_eq!(sessions.count(), 1);
}

#[test]
fn failure_runs_the_second_step_across_a_restart() {
    let f = Fixture::new("restart");
    let grants = Grants::allow("coordinator", "demo");
    {
        let mut host = f.host();
        host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
        host.dispatch_pending(&grants, &mut f.sessions()).unwrap();
        host.complete_action("wf1", "check", "failed", json!({"summary": "2 tests failed"}))
            .unwrap();
        // Crash after the decision is persisted but before dispatching "fix".
    }
    let mut host = f.host();
    assert_eq!(host.get("wf1").unwrap().state["step"], "fix");
    let mut sessions = f.sessions();
    assert_eq!(
        host.dispatch_pending(&grants, &mut sessions).unwrap(),
        vec![request_id("wf1", "fix")]
    );
    assert!(
        host.dispatch_pending(&grants, &mut sessions).unwrap().is_empty(),
        "nothing re-dispatched"
    );
    let prompt: String = sessions
        .0
        .query_row(
            "SELECT prompt FROM sessions WHERE request_id=?1",
            [request_id("wf1", "fix")],
            |r| r.get(0),
        )
        .unwrap();
    assert!(prompt.ends_with("2 tests failed"));
    host.complete_action("wf1", "fix", "succeeded", json!({})).unwrap();
    assert_eq!(host.get("wf1").unwrap().outcome.as_deref(), Some("fixed"));
    assert_eq!(sessions.count(), 2);
}

#[test]
fn crash_between_dispatch_and_marking_does_not_duplicate_the_session() {
    let f = Fixture::new("crash");
    let grants = Grants::allow("coordinator", "demo");
    {
        let mut host = f.host();
        host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
        // The effect commits, then the service dies before marking it dispatched.
        let action = Action {
            kind: "start_session".into(),
            key: "check".into(),
            project: "demo".into(),
            prompt: "p".into(),
        };
        f.sessions().dispatch(&request_id("wf1", "check"), &action).unwrap();
        assert_eq!(keys(&host, "wf1"), vec![pair("check", "pending")]);
    }
    let mut host = f.host();
    let mut sessions = f.sessions();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    assert_eq!(keys(&host, "wf1"), vec![pair("check", "dispatched")]);
    assert_eq!(sessions.count(), 1, "request ID deduplicated the retried dispatch");
}

#[test]
fn duplicate_completion_events_are_idempotent() {
    let f = Fixture::new("duplicates");
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host();
    host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
    assert_eq!(
        host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap(),
        Delivery::Duplicate
    );
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    host.complete_action("wf1", "check", "failed", json!({"summary": "x"}))
        .unwrap();
    assert_eq!(
        host.complete_action("wf1", "check", "failed", json!({"summary": "x"}))
            .unwrap(),
        Delivery::Duplicate
    );
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    assert_eq!(
        host.complete_action("wf1", "check", "succeeded", json!({})).unwrap(),
        Delivery::Duplicate
    );
    assert_eq!(
        keys(&host, "wf1"),
        vec![pair("check", "completed"), pair("fix", "dispatched")]
    );
    assert_eq!(sessions.count(), 2);
}

#[test]
fn cancellation_and_interruption_are_not_retried() {
    let f = Fixture::new("cancel");
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host();
    // Human cancels before dispatch: the pending action never runs, even after restart.
    host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
    host.cancel("wf1").unwrap();
    drop(host);
    let mut host = f.host();
    assert!(host.dispatch_pending(&grants, &mut sessions).unwrap().is_empty());
    assert_eq!(keys(&host, "wf1"), vec![pair("check", "cancelled")]);
    assert_eq!(
        host.complete_action("wf1", "check", "failed", json!({})).unwrap(),
        Delivery::Ignored("workflow is not running")
    );
    // The check session's turn is interrupted: the script stops instead of retrying.
    host.start("wf2", "coordinator", json!({"project": "demo"})).unwrap();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    host.complete_action("wf2", "check", "interrupted", json!({})).unwrap();
    assert_eq!(host.get("wf2").unwrap().outcome.as_deref(), Some("stopped"));
    assert!(host.dispatch_pending(&grants, &mut sessions).unwrap().is_empty());
    assert_eq!(sessions.count(), 1);
}

#[test]
fn authorization_is_checked_at_dispatch_and_revocation_wins() {
    let f = Fixture::new("auth");
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host();
    host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
    grants.revoke_all();
    assert!(host.dispatch_pending(&grants, &mut sessions).unwrap().is_empty());
    assert_eq!(keys(&host, "wf1"), vec![pair("check", "denied")]);
    assert_eq!(host.get("wf1").unwrap().outcome.as_deref(), Some("denied"));
    // A script cannot name a project its owner was never granted.
    host.start("wf2", "coordinator", json!({"project": "elsewhere"}))
        .unwrap();
    host.dispatch_pending(&Grants::allow("coordinator", "demo"), &mut sessions)
        .unwrap();
    assert_eq!(host.get("wf2").unwrap().outcome.as_deref(), Some("denied"));
    assert_eq!(sessions.count(), 0);
}

#[test]
fn running_workflows_are_pinned_to_their_script_version() {
    let f = Fixture::new("pinning");
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host();
    host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    drop(host);
    let edited = WORKFLOW.replace("\"passed\"", "\"passed-v2\"");
    let v2 = install(&f.engine, "check_then_fix.star", Capability::Workflow, &edited).unwrap();
    let mut host = f.host_with(Arc::clone(&v2));
    match host.complete_action("wf1", "check", "succeeded", json!({})) {
        Err(WorkflowError::VersionMismatch { pinned, offered }) => {
            assert_eq!(pinned, f.script.hash);
            assert_eq!(offered, v2.hash);
        }
        other => panic!("expected version mismatch, got {other:?}"),
    }
    host.upgrade("wf1").unwrap();
    host.complete_action("wf1", "check", "succeeded", json!({})).unwrap();
    assert_eq!(host.get("wf1").unwrap().outcome.as_deref(), Some("passed-v2"));
}

#[test]
fn replay_is_deterministic_and_does_not_dispatch() {
    let f = Fixture::new("replay");
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host();
    host.start("wf1", "coordinator", json!({"project": "demo"})).unwrap();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    host.complete_action("wf1", "check", "failed", json!({"summary": "s"}))
        .unwrap();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    host.complete_action("wf1", "fix", "failed", json!({})).unwrap();
    let stored = host.get("wf1").unwrap();
    let before = sessions.count();
    let (state, proposed, outcome) = host.replay("wf1").unwrap();
    assert_eq!(state, stored.state);
    assert_eq!(proposed, vec!["check", "fix"]);
    assert_eq!(outcome.as_deref(), Some("fix_failed"));
    assert_eq!(host.replay("wf1").unwrap().0, state);
    assert_eq!(sessions.count(), before, "replay proposes, never dispatches");
}

#[test]
fn script_errors_and_invalid_decisions_fail_the_run_attributably() {
    let f = Fixture::new("errors");
    let cases = [
        ("    fail(\"bad decision\")\n", DiagnosticKind::Runtime),
        (
            "    return decide(state, actions = [start_session(\"bad key!\", \"demo\", \"p\")])\n",
            DiagnosticKind::InvalidResult,
        ),
        (
            "    return decide(state, actions = [start_session(\"k\" + str(i), \"demo\", \"p\") for i in range(5)])\n",
            DiagnosticKind::InvalidResult,
        ),
        ("    return decide({\"blob\": \"x\" * 100000})\n", DiagnosticKind::Limit),
    ];
    for (i, (body, kind)) in cases.into_iter().enumerate() {
        let source =
            format!("{WORKFLOW_HEAD}def on_event(state: dict[str, typing.Any], event: Event) -> Decision:\n{body}");
        let script = install(&f.engine, &format!("bad{i}.star"), Capability::Workflow, &source).unwrap();
        let mut host = f.host_with(script);
        let id = format!("bad{i}");
        match host.start(&id, "coordinator", json!({"project": "demo"})) {
            Err(WorkflowError::Script(d)) => {
                assert_eq!(d.kind, kind, "{d}");
                assert_eq!(d.script, format!("bad{i}.star"));
            }
            other => panic!("case {i}: {other:?}"),
        }
        let wf = host.get(&id).unwrap();
        assert_eq!(
            (wf.status.as_str(), wf.outcome.as_deref()),
            ("failed", Some("script_error"))
        );
        assert!(host.actions(&id).unwrap().is_empty());
    }
}

#[test]
fn proposing_an_existing_action_key_again_is_one_effect() {
    let f = Fixture::new("same-key");
    let source = format!(
        "{WORKFLOW_HEAD}def on_event(state: dict[str, typing.Any], event: Event) -> Decision:\n    return decide(state, actions = [start_session(\"only\", \"demo\", \"p\")])\n"
    );
    let script = install(&f.engine, "same_key.star", Capability::Workflow, &source).unwrap();
    let (grants, mut sessions) = (Grants::allow("coordinator", "demo"), f.sessions());
    let mut host = f.host_with(script);
    host.start("wf1", "coordinator", json!({})).unwrap();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    host.complete_action("wf1", "only", "succeeded", json!({})).unwrap();
    host.dispatch_pending(&grants, &mut sessions).unwrap();
    assert_eq!(keys(&host, "wf1"), vec![pair("only", "completed")]);
    assert_eq!(sessions.count(), 1);
}
