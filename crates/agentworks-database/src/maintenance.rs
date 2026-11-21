use crate::{Result, Store};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
fn check_db(db: &Connection) -> Result<()> {
    let mut q = db.prepare("PRAGMA integrity_check")?;
    let rows = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows != ["ok"] {
        return Err(format!("SQLite integrity check failed: {rows:?}").into());
    }
    if db.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err("SQLite foreign key check failed".into());
    }
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if !(0..=crate::SCHEMA_VERSION).contains(&version) {
        return Err("Unsupported session database schema version".into());
    }
    Ok(())
}
pub fn compare(source: &Connection, copy: &Connection) -> Result<()> {
    let schema = |db: &Connection| -> rusqlite::Result<Vec<(String, String)>> {
        db.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    };
    let tables = schema(source)?;
    if tables != schema(copy)? {
        return Err("Backup schema mismatch".into());
    }
    for (table, _) in tables {
        let sql = format!(
            "SELECT * FROM \"{}\" ORDER BY rowid",
            table.replace('"', "\"\"")
        );
        let rows = |db: &Connection| -> rusqlite::Result<Vec<Vec<String>>> {
            let mut q = db.prepare(&sql)?;
            let count = q.column_count();
            q.query_map([], |r| {
                (0..count)
                    .map(|i| r.get_ref(i).map(|v| format!("{v:?}")))
                    .collect()
            })?
            .collect()
        };
        if rows(source)? != rows(copy)? {
            return Err(format!("Backup content mismatch: {table}").into());
        }
    }
    Ok(())
}

/// A read-only immutable backup, with no connection escape hatch.
pub struct Snapshot(Connection);
impl Snapshot {
    pub fn open(path: &Path) -> Result<Self> {
        // Immutable URI prevents even read-only SQLite from creating WAL/SHM files.
        // Percent-encode raw path bytes so '?'/'#' and non-UTF8 names stay literal.
        use std::os::unix::ffi::OsStrExt;
        let encoded: String = std::fs::canonicalize(path)?
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| format!("%{byte:02X}"))
            .collect();
        let db = Connection::open_with_flags(
            format!("file:{encoded}?immutable=1"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        check_db(&db)?;
        Ok(Self(db))
    }
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.0.query_row("PRAGMA user_version", [], |r| r.get(0))
    }
    pub fn compare(&self, other: &Self) -> Result<()> {
        compare(&self.0, &other.0)
    }
}
impl Store {
    pub fn check_integrity(&self) -> Result<()> {
        check_db(&self.connection)
    }
    pub fn compare_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        compare(&snapshot.0, &self.connection)
    }
    pub fn finish_restore(&self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        Ok(())
    }
    /// Create a standalone snapshot and verify every table and SQLite sequence.
    pub fn snapshot_verified(&self, destination: &Path) -> Result<()> {
        self.connection
            .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])?;
        let copy = Connection::open(destination)?;
        compare(&self.connection, &copy)
    }
}
