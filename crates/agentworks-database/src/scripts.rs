//! Persistence and lifecycle of installed hooks, preserving shared script schemas.
use crate::content_hash;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// Accepted routing priorities; scripts without one use 0.
pub const PRIORITY_RANGE: std::ops::RangeInclusive<i64> = -1000..=1000;

#[derive(Serialize)]
pub struct ScriptRecord {
    pub name: String,
    pub capability: String,
    pub source: String,
    pub hash: String,
    pub enabled: bool,
    /// Routing priority: higher runs first, then filename, then declaration order.
    pub priority: i64,
    /// Follows the shipped source (unmodified built-in).
    pub builtin: bool,
    pub updated: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Insert missing built-ins and move unmodified built-ins to the shipped source.
pub(crate) fn seed(db: &Connection, builtin: &[(&str, &str)]) -> rusqlite::Result<()> {
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
    for (name, source) in builtin {
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
pub(crate) fn list(db: &Connection) -> rusqlite::Result<Vec<ScriptRecord>> {
    let mut stmt = db.prepare(
        "SELECT s.name,s.capability,s.source,s.hash,s.enabled,s.builtin,s.updated,COALESCE(o.priority,0) AS priority
         FROM scripts s LEFT JOIN script_order o ON o.name=s.name WHERE s.capability<>'automation'
         ORDER BY priority DESC, s.name COLLATE BINARY",
    )?;
    stmt.query_map([], |r| {
        Ok(ScriptRecord {
            name: r.get(0)?,
            capability: r.get(1)?,
            source: r.get(2)?,
            hash: r.get(3)?,
            enabled: r.get(4)?,
            priority: r.get(7)?,
            builtin: r.get(5)?,
            updated: r.get(6)?,
        })
    })?
    .collect()
}

/// Save a validated user script. Saving over a built-in makes it user-owned.
pub(crate) fn save(db: &Connection, name: &str, source: &str) -> rusqlite::Result<()> {
    db.execute("INSERT INTO scripts(name,capability,source,hash,enabled,builtin,updated) VALUES (?1,'hooks',?2,?3,1,0,?4)
        ON CONFLICT(name) DO UPDATE SET source=excluded.source,hash=excluded.hash,capability=excluded.capability,builtin=0,updated=excluded.updated",
        params![name, source, content_hash(source), now()])?;
    Ok(())
}

pub(crate) fn set_enabled(db: &Connection, name: &str, enabled: bool) -> rusqlite::Result<bool> {
    Ok(db.execute(
        "UPDATE scripts SET enabled=?2,updated=?3 WHERE name=?1",
        params![name, enabled, now()],
    )? == 1)
}

/// Set a script's routing priority. 0 removes the explicit entry.
pub(crate) fn set_priority(db: &Connection, name: &str, priority: i64) -> rusqlite::Result<bool> {
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
pub(crate) fn delete(
    db: &Connection,
    name: &str,
    shipped: Option<&str>,
) -> rusqlite::Result<Removal> {
    let builtin: Option<bool> = db
        .query_row("SELECT builtin FROM scripts WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match (builtin, shipped) {
        (None, _) => Removal::Missing,
        (Some(true), _) => Removal::Builtin,
        (Some(false), Some(source)) => {
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

impl crate::Store {
    pub fn seed_scripts(&self, builtin: &[(&str, &str)]) -> rusqlite::Result<()> {
        seed(&self.connection, builtin)
    }
    pub fn scripts(&self) -> rusqlite::Result<Vec<ScriptRecord>> {
        list(&self.connection)
    }
    pub fn save_script(&self, name: &str, source: &str) -> rusqlite::Result<()> {
        save(&self.connection, name, source)
    }
    pub fn enable_script(&self, name: &str, enabled: bool) -> rusqlite::Result<bool> {
        set_enabled(&self.connection, name, enabled)
    }
    pub fn prioritize_script(&self, name: &str, priority: i64) -> rusqlite::Result<bool> {
        set_priority(&self.connection, name, priority)
    }
    pub fn remove_script(&self, name: &str, shipped: Option<&str>) -> rusqlite::Result<Removal> {
        delete(&self.connection, name, shipped)
    }
}

impl crate::Store {
    pub fn script_is_automation(&self, name: &str) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM scripts WHERE name=?1 AND capability='automation')",
            [name],
            |r| r.get(0),
        )
    }
    pub fn script_source(&self, name: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row("SELECT source FROM scripts WHERE name=?1", [name], |r| {
                r.get(0)
            })
            .optional()
    }
    pub fn enabled_automation_source(&self, name: &str) -> rusqlite::Result<Option<String>> {
        self.connection.query_row("SELECT source FROM scripts WHERE name=?1 AND enabled=1 AND capability='automation'", [name], |r| r.get(0)).optional()
    }
}

impl crate::Store {
    /// Remove the obsolete shipped template and return user hook files eligible
    /// for interpreter-validated classification. No interpreter is loaded here.
    pub fn automation_migration_candidates(&self) -> rusqlite::Result<Vec<(String, String)>> {
        self.connection.execute("DELETE FROM scripts WHERE name='github_review.star' AND builtin=1 AND NOT EXISTS(SELECT 1 FROM automations WHERE json_extract(definition,'$.workflow.trigger.script')='github_review.star')", [])?;
        self.connection.prepare("SELECT name,source FROM scripts WHERE capability='hooks' AND instr(source,'register_github_trigger')>0")?
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?)))?.collect()
    }
    pub fn classify_automation_script(&self, name: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE scripts SET capability='automation',builtin=0 WHERE name=?1",
            [name],
        )?;
        Ok(())
    }
    pub fn automation_scripts(&self) -> rusqlite::Result<Vec<(String, String, String, bool, u64)>> {
        self.connection.prepare("SELECT name,source,hash,enabled,updated FROM scripts WHERE capability='automation' ORDER BY name")?
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get::<_,i64>(4)? as u64)))?.collect()
    }
    pub fn repair_script_hash(&self, name: &str, hash: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE scripts SET hash=?2 WHERE name=?1",
            params![name, hash],
        )?;
        Ok(())
    }
}

impl crate::Store {
    pub fn enabled_scripts(&self) -> rusqlite::Result<Vec<(String, String)>> {
        self.connection.prepare("SELECT s.name,s.hash FROM scripts s LEFT JOIN script_order o ON s.name=o.name WHERE s.enabled=1 ORDER BY COALESCE(o.priority,0) DESC,s.name COLLATE BINARY")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect()
    }
    pub fn enabled_script_hash(&self, name: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT hash FROM scripts WHERE name=?1 AND enabled=1",
                [name],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn script_exists(&self, name: &str) -> rusqlite::Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM scripts WHERE name=?1)",
            [name],
            |r| r.get(0),
        )
    }
    pub fn automation_source(&self, name: &str) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT source FROM scripts WHERE name=?1 AND capability='automation'",
                [name],
                |r| r.get(0),
            )
            .optional()
    }
}

impl crate::Store {
    /// Runtime reconciliation pauses a stored automation without changing its
    /// source-edit timestamp. Human toggles use `enable_script` instead.
    pub fn disable_automation_script(&self, name: &str) -> rusqlite::Result<()> {
        self.connection
            .execute("UPDATE scripts SET enabled=0 WHERE name=?1", [name])?;
        Ok(())
    }
}
