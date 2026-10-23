//! Roles: named instructions appended to an agent's system prompt over ACP.
//!
//! A role is context only. It never grants Agentworks access; grants stay with
//! the human controls in Agents & inbox. Built-in roles ship with the binary and
//! are read-only; custom roles are stored in SQLite. A session keeps a snapshot
//! of the role text it was created with (`SessionData::role`), so editing or
//! deleting a role does not change running sessions.

use crate::{ApiResult, App, fail};
use agentworks_session::model::RoleSnapshot;
use agentworks_workspace::sha256;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub const SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS roles(id TEXT PRIMARY KEY,definition TEXT NOT NULL);";

const MAX_NAME: usize = 80;
const MAX_DESCRIPTION: usize = 200;
const MAX_INSTRUCTIONS: usize = 16384;

/// (id, name, description, instructions)
const BUILTIN: &[(&str, &str, &str, &str)] = &[
    (
        "coordinator",
        "Coordinator",
        "Plans work, delegates to other sessions and integrates results",
        "You are coordinating work across several agent sessions in this project.\n\
         - Break the goal into tasks that can proceed independently, and give each one a clear outcome and stopping point.\n\
         - Delegate only when it is faster than doing the work yourself. Tell each session what to report back.\n\
         - Track what is in flight. Check your inbox at natural checkpoints rather than waiting idly.\n\
         - Review delegated results before relying on them, and report the combined outcome to the human.\n\
         - The Agentworks tools you have reflect what the human granted. If a step needs access you lack, say so instead of working around it.",
    ),
    (
        "implementer",
        "Implementer",
        "Makes focused, tested changes",
        "You are implementing a change.\n\
         - Read the surrounding code first and match its conventions.\n\
         - Keep the change scoped to the task. Note unrelated problems instead of fixing them.\n\
         - Run the relevant tests and checks before reporting. Report failures with their output.\n\
         - Finish with a short summary of what changed, what was verified and anything left undone.",
    ),
    (
        "reviewer",
        "Reviewer",
        "Reviews changes for correctness without editing them",
        "You are reviewing changes, not making them.\n\
         - Do not edit files unless the human asks you to.\n\
         - Prioritise correctness: bugs, regressions, missed cases, unsafe behaviour. Style comes last.\n\
         - Verify each finding against the code before reporting it, and give the file, line and a concrete failure scenario.\n\
         - Say plainly when you found nothing significant.",
    ),
    (
        "researcher",
        "Researcher",
        "Investigates questions and reports findings",
        "You are investigating a question.\n\
         - Gather evidence from the code, documentation and tools before drawing conclusions.\n\
         - Do not change files unless the human asks you to.\n\
         - Separate what you verified from what you infer, and cite where each finding came from.\n\
         - End with a direct answer and a recommendation.",
    ),
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default)]
    pub builtin: bool,
    pub updated: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    name: String,
    #[serde(default)]
    description: String,
    instructions: String,
}

/// Harnesses known to accept an ACP `systemPrompt`. The capability advertised
/// at initialization is still checked before a session is opened.
pub fn supported(provider: &str) -> bool {
    matches!(provider, "claude" | "claude-rust" | "demo")
}

pub fn hash(instructions: &str) -> String {
    sha256::hex(&sha256::digest(instructions.as_bytes()))
}

fn builtins() -> impl Iterator<Item = Role> {
    BUILTIN
        .iter()
        .map(|(id, name, description, instructions)| Role {
            id: (*id).into(),
            name: (*name).into(),
            description: (*description).into(),
            instructions: (*instructions).into(),
            builtin: true,
            updated: 0,
        })
}

pub fn list(db: &Connection) -> rusqlite::Result<Vec<Role>> {
    let mut stmt = db.prepare("SELECT definition FROM roles")?;
    let mut custom = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|d| d.ok().and_then(|d| serde_json::from_str::<Role>(&d).ok()))
        .collect::<Vec<_>>();
    custom.sort_by_key(|r| r.name.to_lowercase());
    Ok(builtins().chain(custom).collect())
}

pub fn get(db: &Connection, id: &str) -> rusqlite::Result<Option<Role>> {
    if let Some(role) = builtins().find(|r| r.id == id) {
        return Ok(Some(role));
    }
    Ok(db
        .query_row("SELECT definition FROM roles WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .and_then(|d| serde_json::from_str(&d).ok()))
}

pub fn snapshot(role: &Role) -> RoleSnapshot {
    RoleSnapshot {
        id: role.id.clone(),
        name: role.name.clone(),
        hash: hash(&role.instructions),
        instructions: role.instructions.clone(),
    }
}

/// The role a new session is created with, checked against its harness.
pub fn resolve(
    db: &Connection,
    id: Option<&str>,
    provider: &str,
) -> Result<Option<RoleSnapshot>, (StatusCode, String)> {
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    if !supported(provider) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "This agent does not accept role instructions yet",
        ));
    }
    let role = get(db, id)
        .map_err(storage)?
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "Role not found"))?;
    Ok(Some(snapshot(&role)))
}

/// Small shared context; live permissions and coordination state belong to MCP.
pub fn shared_context(session_id: &str) -> String {
    format!(
        "# Agentworks\n\nYour session is agentworks://sessions/{session_id}. \
         Use the Agentworks MCP tools when available to discover peers, read your inbox at useful checkpoints, \
         send updates and report completion. Follow the MCP server instructions and tool descriptions; \
         available tools reflect your current access. Reuse the same requestId when retrying an identical operation."
    )
}

/// The text sent as the ACP system prompt for a session's role.
pub fn system_prompt(role: &RoleSnapshot) -> String {
    format!(
        "# Agentworks role: {}\n\nThe human assigned you this role for the session.\n\n{}",
        role.name, role.instructions
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn storage(_error: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not access role storage",
    )
}

fn validated(id: String, input: Input) -> Result<Role, (StatusCode, String)> {
    let name = input.name.trim().to_string();
    let description = input.description.trim().to_string();
    let instructions = input.instructions.trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a role name of 1 to 80 characters",
        ));
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use a description up to 200 characters",
        ));
    }
    if instructions.is_empty() || instructions.len() > MAX_INSTRUCTIONS {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Use role instructions up to 16 KB",
        ));
    }
    Ok(Role {
        id,
        name,
        description,
        instructions,
        builtin: false,
        updated: now(),
    })
}

fn save(db: &Connection, role: &Role) -> Result<(), (StatusCode, String)> {
    db.execute(
        "INSERT INTO roles(id,definition) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition",
        params![role.id, serde_json::to_string(role).map_err(storage)?],
    )
    .map_err(storage)?;
    Ok(())
}

fn read_only() -> (StatusCode, String) {
    fail(
        StatusCode::CONFLICT,
        "Built-in roles cannot be changed. Create a custom role from it instead.",
    )
}

pub async fn get_roles(State(app): State<App>) -> ApiResult {
    let store = app.store.lock().unwrap();
    Ok(Json(json!(list(&store.connection).map_err(storage)?)))
}

pub async fn create(State(app): State<App>, Json(input): Json<Input>) -> ApiResult {
    let role = validated(Uuid::new_v4().to_string(), input)?;
    save(&app.store.lock().unwrap().connection, &role)?;
    Ok(Json(json!(role)))
}

pub async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<Input>,
) -> ApiResult {
    let store = app.store.lock().unwrap();
    match get(&store.connection, &id).map_err(storage)? {
        None => Err(fail(StatusCode::NOT_FOUND, "Role not found")),
        Some(r) if r.builtin => Err(read_only()),
        Some(_) => {
            let role = validated(id, input)?;
            save(&store.connection, &role)?;
            Ok(Json(json!(role)))
        }
    }
}

pub async fn delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let store = app.store.lock().unwrap();
    if builtins().any(|r| r.id == id) {
        return Err(read_only());
    }
    let removed = store
        .connection
        .execute("DELETE FROM roles WHERE id=?1", [&id])
        .map_err(storage)?;
    if removed == 0 {
        return Err(fail(StatusCode::NOT_FOUND, "Role not found"));
    }
    Ok(Json(json!({"ok":true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db
    }
    fn input(name: &str, instructions: &str) -> Input {
        Input {
            name: name.into(),
            description: String::new(),
            instructions: instructions.into(),
        }
    }

    #[test]
    fn builtins_come_first_and_custom_roles_sort_by_name() {
        let db = db();
        for name in ["zeta", "Alpha"] {
            save(&db, &validated(name.into(), input(name, "Do it.")).unwrap()).unwrap();
        }
        let ids: Vec<_> = list(&db).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            [
                "coordinator",
                "implementer",
                "reviewer",
                "researcher",
                "Alpha",
                "zeta"
            ]
        );
    }

    #[test]
    fn resolve_snapshots_text_and_rejects_unknown_or_unsupported() {
        let db = db();
        assert_eq!(resolve(&db, None, "codex").unwrap(), None);
        assert_eq!(resolve(&db, Some(""), "codex").unwrap(), None);
        let role = resolve(&db, Some("reviewer"), "claude").unwrap().unwrap();
        assert_eq!(role.name, "Reviewer");
        assert_eq!(role.hash, hash(&role.instructions));
        assert_eq!(
            resolve(&db, Some("missing"), "claude").unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert!(
            resolve(&db, Some("reviewer"), "codex")
                .unwrap_err()
                .1
                .contains("does not accept")
        );
    }

    #[test]
    fn validation_trims_and_bounds_input() {
        let role = validated("x".into(), input("  Name  ", "  text  ")).unwrap();
        assert_eq!(
            (role.name.as_str(), role.instructions.as_str()),
            ("Name", "text")
        );
        assert!(validated("x".into(), input(" ", "text")).is_err());
        assert!(validated("x".into(), input("Name", " ")).is_err());
        assert!(validated("x".into(), input("Name", &"a".repeat(MAX_INSTRUCTIONS + 1))).is_err());
    }

    #[test]
    fn snapshot_is_independent_of_later_edits() {
        let db = db();
        save(
            &db,
            &validated("custom".into(), input("Custom", "First.")).unwrap(),
        )
        .unwrap();
        let before = resolve(&db, Some("custom"), "claude").unwrap().unwrap();
        save(
            &db,
            &validated("custom".into(), input("Custom", "Second.")).unwrap(),
        )
        .unwrap();
        assert_eq!(before.instructions, "First.");
        assert_eq!(
            resolve(&db, Some("custom"), "claude")
                .unwrap()
                .unwrap()
                .instructions,
            "Second."
        );
    }
}
