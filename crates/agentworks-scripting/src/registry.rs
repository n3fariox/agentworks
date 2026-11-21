//! Persistence and lifecycle of installed hooks, preserving shared script schemas.
use agentworks_starlark::engine::{HookInfo, content_hash};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// `script_order` holds human-set routing priorities. It is an additive table;
/// older binaries ignore it and retain filename ordering.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scripts(name TEXT PRIMARY KEY,capability TEXT NOT NULL,source TEXT NOT NULL,hash TEXT NOT NULL,enabled INTEGER NOT NULL,builtin INTEGER NOT NULL,updated INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS script_order(name TEXT PRIMARY KEY,priority INTEGER NOT NULL);";

/// Accepted routing priorities; scripts without one use 0.
pub const PRIORITY_RANGE: std::ops::RangeInclusive<i64> = -1000..=1000;

/// Shipped scripts. A built-in follows the shipped source until a user saves
/// their own version under the same name; it can be disabled but not deleted.
pub(crate) const BUILTIN: &[(&str, &str)] = &[
    (
        "bazel_progress.star",
        agentworks_starlark::BAZEL_PROGRESS_SCRIPT,
    ),
    (
        "inbox_message.star",
        agentworks_starlark::INBOX_MESSAGE_SCRIPT,
    ),
    ("tool_header.star", agentworks_starlark::TOOL_HEADER_SCRIPT),
];

#[derive(Serialize)]
pub struct Script {
    pub name: String,
    pub capability: String,
    /// Validated installation-time registrations, in declaration order.
    pub hooks: Vec<HookInfo>,
    pub source: String,
    pub hash: String,
    pub enabled: bool,
    /// Routing priority: higher runs first, then filename, then declaration order.
    pub priority: i64,
    /// Follows the shipped source (unmodified built-in).
    pub builtin: bool,
    /// The name of a shipped script, whether or not the user modified it.
    pub shipped: bool,
    pub updated: i64,
    /// Current compile/type error for an enabled script, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn valid_name(name: &str) -> bool {
    let stem = name.strip_suffix(".star").unwrap_or("");
    !stem.is_empty()
        && stem.len() <= 64
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Insert missing built-ins and move unmodified built-ins to the shipped source.
pub fn seed(db: &Connection) -> rusqlite::Result<()> {
    // Preserve saved edits, enable flags and ownership. Legacy assignments become
    // explicit declarations once; filename routing is no longer used.
    let legacy = {
        let mut query = db.prepare("SELECT name,capability,source FROM scripts WHERE capability IN ('parser','message_formatter')")?;
        query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let tx = db.unchecked_transaction()?;
    for (name, capability, mut source) in legacy {
        source.push_str(if capability == "parser" {
            "\nagent.register_progress(on_line, command_pattern = COMMAND_PATTERN, display_name = DISPLAY_NAME)\n"
        } else {
            "\nagent.register_formatter(format_message, message_type = \"inbox.peer\")\n"
        });
        tx.execute(
            "UPDATE scripts SET capability='hooks',source=?2,hash=?3 WHERE name=?1",
            params![name, source, content_hash(&source)],
        )?;
    }
    tx.commit()?;
    db.execute("DELETE FROM script_order WHERE name IN (SELECT name FROM scripts WHERE name='github_badges.star' AND builtin=1)",[])?;
    db.execute(
        "DELETE FROM scripts WHERE name='github_badges.star' AND builtin=1",
        [],
    )?;
    for (name, source) in BUILTIN {
        let hash = content_hash(source);
        let existing: Option<(bool, String)> = db
            .query_row(
                "SELECT builtin,hash FROM scripts WHERE name=?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match existing {
            None => {
                db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'hooks',?2,?3,1,1,?4)",
                    params![name, source, hash, now()])?;
            }
            Some((true, current)) if current != hash => {
                db.execute(
                    "UPDATE scripts SET source=?2,hash=?3,updated=?4 WHERE name=?1",
                    params![name, source, hash, now()],
                )?;
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// All scripts in routing order: priority descending, then filename by byte
/// value. Hooks within a script keep declaration order.
pub fn list(db: &Connection) -> rusqlite::Result<Vec<Script>> {
    let mut stmt = db.prepare(
        "SELECT s.name,s.capability,s.source,s.hash,s.enabled,s.builtin,s.updated,COALESCE(o.priority,0) AS priority
         FROM scripts s LEFT JOIN script_order o ON o.name=s.name WHERE s.capability<>'automation'
         ORDER BY priority DESC, s.name COLLATE BINARY",
    )?;
    stmt.query_map([], |r| {
        Ok(Script {
            name: r.get(0)?,
            capability: r.get(1)?,
            hooks: Vec::new(),
            source: r.get(2)?,
            hash: r.get(3)?,
            enabled: r.get(4)?,
            priority: r.get(7)?,
            builtin: r.get(5)?,
            shipped: false,
            updated: r.get(6)?,
            error: None,
        })
        .map(|mut s: Script| {
            s.shipped = BUILTIN.iter().any(|(name, _)| *name == s.name);
            s
        })
    })?
    .collect()
}

/// Save a validated user script. Saving over a built-in makes it user-owned.
pub fn save(db: &Connection, name: &str, source: &str) -> rusqlite::Result<()> {
    db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'hooks',?2,?3,1,0,?4)
        ON CONFLICT(name) DO UPDATE SET source=excluded.source,hash=excluded.hash,capability=excluded.capability,builtin=0,updated=excluded.updated",
        params![name, source, content_hash(source), now()])?;
    Ok(())
}

pub fn set_enabled(db: &Connection, name: &str, enabled: bool) -> rusqlite::Result<bool> {
    Ok(db.execute(
        "UPDATE scripts SET enabled=?2,updated=?3 WHERE name=?1",
        params![name, enabled, now()],
    )? == 1)
}

/// Set a script's routing priority. 0 removes the explicit entry.
pub fn set_priority(db: &Connection, name: &str, priority: i64) -> rusqlite::Result<bool> {
    let exists = db
        .query_row("SELECT 1 FROM scripts WHERE name=?1", [name], |_| Ok(()))
        .optional()?
        .is_some();
    if exists {
        if priority == 0 {
            db.execute("DELETE FROM script_order WHERE name=?1", [name])?;
        } else {
            db.execute(
                "INSERT INTO script_order(name,priority) VALUES (?1,?2) ON CONFLICT(name) DO UPDATE SET priority=excluded.priority",
                params![name, priority],
            )?;
        }
    }
    Ok(exists)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    Missing,
    /// An unmodified built-in: disable it instead.
    Builtin,
    Deleted,
    /// A modified built-in went back to the shipped source.
    Restored,
}

/// Delete a user script, or restore a modified built-in to its shipped source.
/// Restoring keeps the priority; deleting forgets it.
pub fn delete(db: &Connection, name: &str) -> rusqlite::Result<Removal> {
    let builtin: Option<bool> = db
        .query_row("SELECT builtin FROM scripts WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    let shipped = BUILTIN.iter().find(|(n, _)| *n == name);
    Ok(match (builtin, shipped) {
        (None, _) => Removal::Missing,
        (Some(true), _) => Removal::Builtin,
        (Some(false), Some((_, source))) => {
            db.execute(
                "UPDATE scripts SET source=?2,hash=?3,builtin=1,updated=?4 WHERE name=?1",
                params![name, source, content_hash(source), now()],
            )?;
            Removal::Restored
        }
        (Some(false), None) => {
            let tx = db.unchecked_transaction()?;
            tx.execute("DELETE FROM scripts WHERE name=?1", [name])?;
            tx.execute("DELETE FROM script_order WHERE name=?1", [name])?;
            tx.commit()?;
            Removal::Deleted
        }
    })
}
