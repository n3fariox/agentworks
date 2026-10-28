//! Offline recovery tools. No command opens the configured live store or starts a harness.
use crate::{catalog::CatalogData, store::Store};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const FILES: [&str; 3] = ["catalog.json", "sessions.sqlite3", "request.json"];
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    id: String,
    hashes: [String; 3],
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(format!("Expected regular file: {}", path.display()).into());
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(format!("Expected directory without symlink: {}", path.display()).into());
    }
    Ok(())
}
fn hash(path: &Path) -> Result<String> {
    regular(path)?;
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
/// Shared with restart backup creation; fail rather than race pruning/publication.
pub fn lock(root: &Path) -> Result<fs::File> {
    directory(root)?;
    let path = root.join(".maintenance.lock");
    if exists(&path)? {
        regular(&path)?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.try_lock()
        .map_err(|_| "Backup maintenance is already running")?;
    Ok(file)
}
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
    if !(0..=crate::store::SCHEMA_VERSION).contains(&version) {
        return Err("Unsupported session database schema version".into());
    }
    Ok(())
}
fn source_db(root: &Path) -> Result<Connection> {
    let path = root.join("sessions.sqlite3");
    regular(&path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        if exists(&root.join(format!("sessions.sqlite3{suffix}")))? {
            return Err("Backup has SQLite sidecars; use a completed standalone snapshot".into());
        }
    }
    // Immutable URI prevents even read-only SQLite from creating WAL/SHM files.
    // Percent-encode raw path bytes so '?'/'#' and non-UTF8 names stay literal.
    use std::os::unix::ffi::OsStrExt;
    let encoded: String = fs::canonicalize(path)?
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
    Ok(db)
}
fn validate(root: &Path, require_manifest: bool) -> Result<()> {
    directory(root)?;
    for name in FILES {
        regular(&root.join(name))?;
    }
    let _: CatalogData = serde_json::from_slice(&fs::read(root.join("catalog.json"))?)?;
    let _: serde_json::Value = serde_json::from_slice(&fs::read(root.join("request.json"))?)?;
    let manifest_path = root.join("manifest.json");
    if require_manifest || exists(&manifest_path)? {
        regular(&manifest_path)?;
        let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
        if manifest.format != 1 || root.file_name().and_then(|s| s.to_str()) != Some(&manifest.id) {
            return Err("Invalid backup manifest identity/format".into());
        }
        for (name, expected) in FILES.iter().zip(&manifest.hashes) {
            if hash(&root.join(name))? != *expected {
                return Err(format!("Backup checksum mismatch: {name}").into());
            }
        }
    }
    source_db(root)?;
    Ok(())
}
/// Publish only after the restart snapshot has passed its preservation checks.
pub fn publish(root: &Path) -> Result<()> {
    validate(root, false)?;
    let hashes = FILES
        .map(|name| hash(&root.join(name)))
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .try_into()
        .unwrap();
    let manifest = Manifest {
        format: 1,
        id: root
            .file_name()
            .unwrap()
            .to_str()
            .ok_or("Non-UTF8 backup name")?
            .into(),
        hashes,
    };
    private_write(
        &root.join("manifest.json.tmp"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::rename(root.join("manifest.json.tmp"), root.join("manifest.json"))?;
    fs::File::open(root)?.sync_all()?;
    fs::File::open(root.parent().ok_or("Missing backup parent")?)?.sync_all()?;
    Ok(())
}
/// Compare all tables, including future additive tables and SQLite sequence state.
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

/// Retain newest N *completed* snapshots. Unknown/incomplete entries are protected.
fn retain(root: &Path, keep: usize, apply: bool) -> Result<serde_json::Value> {
    if keep == 0 {
        return Err("Retention must keep at least one completed backup".into());
    }
    let _lock = lock(root)?;
    let mut backups = Vec::new();
    let mut protected = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name() == ".maintenance.lock" {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only the timestamp-UUID namespace owned by supervised restart is eligible.
        let owned = name.split_once('-').is_some_and(|(time, id)| {
            time.parse::<u64>().is_ok() && uuid::Uuid::parse_str(id).is_ok()
        });
        if !owned || !entry.file_type()?.is_dir() || !exists(&path.join("manifest.json"))? {
            protected.push(name);
            continue;
        }
        // Validate every eligible snapshot before deleting any; corruption fails closed.
        validate(&path, true)?;
        let timestamp = name.split_once('-').unwrap().0.parse::<u64>()?;
        backups.push((timestamp, name));
    }
    backups.sort();
    let count = backups.len().saturating_sub(keep);
    let remove: Vec<_> = backups
        .iter()
        .take(count)
        .map(|(_, name)| name.clone())
        .collect();
    for name in &remove {
        let path = root.join(name);
        // Never recursively delete unexpected contents, even inside a valid backup.
        let expected = [
            "catalog.json",
            "sessions.sqlite3",
            "request.json",
            "manifest.json",
        ];
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            if !expected.iter().any(|name| entry.file_name() == *name)
                || !entry.file_type()?.is_file()
            {
                return Err(
                    format!("Unexpected backup contents: {}", entry.path().display()).into(),
                );
            }
        }
    }
    if apply {
        for name in &remove {
            let path = root.join(name);
            // Remove the completion marker first: interruption leaves protected remnants.
            fs::remove_file(path.join("manifest.json"))?;
            fs::File::open(&path)?.sync_all()?;
            for file in FILES {
                fs::remove_file(path.join(file))?;
            }
            fs::remove_dir(path)?;
        }
        fs::File::open(root)?.sync_all()?;
    }
    protected.sort();
    Ok(serde_json::json!({"applied":apply,"keep":keep,"remove":remove,"protected":protected}))
}

fn restore(source: &Path, destination: &Path) -> Result<serde_json::Value> {
    let parent = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let _lock = lock(parent)?;
    validate(source, false)?;
    let destination_parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if fs::canonicalize(destination_parent)?.starts_with(fs::canonicalize(source)?) {
        return Err("Restore destination cannot be inside its source backup".into());
    }
    // Exclusive creation refuses existing live directories, files, and symlinks.
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let result = (|| -> Result<_> {
        for file in FILES {
            let mut input = fs::File::open(source.join(file))?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(destination.join(file))?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
        let db_path = destination.join("sessions.sqlite3");
        let store = Store::open(&db_path)?;
        check_db(&store.connection)?;
        // The same deserializers as startup validate saved sessions and definitions.
        let sessions = store.load()?.len();
        store.automations()?;
        store
            .connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        drop(store);
        fs::File::open(&db_path)?.sync_all()?;
        let report = serde_json::json!({"format":1,"sessions":sessions,"schemaVersion":crate::store::SCHEMA_VERSION,"source":source,"catalog":destination.join("catalog.json"),"database":db_path});
        private_write(
            &destination.join("restore-ready.json.tmp"),
            &serde_json::to_vec_pretty(&report)?,
        )?;
        fs::rename(
            destination.join("restore-ready.json.tmp"),
            destination.join("restore-ready.json"),
        )?;
        fs::File::open(destination)?.sync_all()?;
        fs::File::open(
            destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

pub fn run(args: &[String]) -> Result<()> {
    let usage = "Usage: agentworks recovery retain BACKUPS_DIR --keep N [--apply] | restore BACKUP_DIR NEW_DIRECTORY";
    let report = match args.first().map(String::as_str) {
        Some("retain")
            if (args.len() == 4 || args.len() == 5)
                && args[2] == "--keep"
                && (args.len() == 4 || args[4] == "--apply") =>
        {
            retain(Path::new(&args[1]), args[3].parse()?, args.len() == 5)?
        }
        Some("restore") if args.len() == 3 => restore(Path::new(&args[1]), Path::new(&args[2]))?,
        _ => return Err(usage.into()),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("agentworks-recovery-{}", uuid::Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        fn backup(&self, time: u64, completed: bool) -> std::path::PathBuf {
            let path = self.0.join(format!("{time}-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            let store = Store::open(&path.join("sessions.sqlite3")).unwrap();
            let metadata = json!({"id":"session","provider":"demo","cwd":"/tmp","title":"Retain me","initial_model":null,"created":1,"host_id":"local","location_id":"local"});
            let state = json!({"status":"idle","seq":1,"requests":[],"models":null,"available_commands":[],"turn_active":false,"project_id":null,"provider_session_id":"native-context"});
            store
                .connection
                .execute(
                    "INSERT INTO sessions VALUES ('session',?1,?2)",
                    [metadata.to_string(), state.to_string()],
                )
                .unwrap();
            store.connection.execute_batch(r#"
                INSERT INTO events VALUES ('session',1,'{"seq":1,"kind":"user","text":"history"}');
                INSERT INTO agent_access VALUES ('session','{"communicate":true}');
                INSERT INTO agent_completions(author,author_title,request_id,payload,report,created) VALUES ('session','Test','done','{}','{"status":"completed"}',1);
            "#).unwrap();
            drop(store);
            fs::remove_file(path.join("sessions.sqlite3.lock")).unwrap();
            private_write(
                &path.join("catalog.json"),
                &serde_json::to_vec(&CatalogData::default()).unwrap(),
            )
            .unwrap();
            private_write(&path.join("request.json"), b"{}").unwrap();
            if completed {
                publish(&path).unwrap();
            }
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn retention_preview_apply_and_protected_entries() {
        let f = Fixture::new();
        let old = f.backup(9, true);
        let newest = f.backup(100, true);
        let legacy = f.backup(1, false);
        let incomplete = f.0.join("incomplete");
        fs::create_dir(&incomplete).unwrap();
        assert!(retain(&f.0, 0, true).is_err());
        let plan = retain(&f.0, 1, false).unwrap();
        assert_eq!(
            plan["remove"],
            json!([old.file_name().unwrap().to_str().unwrap()])
        );
        assert!(old.exists());
        retain(&f.0, 1, true).unwrap();
        assert!(!old.exists());
        assert!(newest.exists() && legacy.exists() && incomplete.exists());
        assert_eq!(retain(&f.0, 1, true).unwrap()["remove"], json!([]));
    }

    #[test]
    fn corruption_unexpected_contents_and_concurrent_maintenance_fail_closed() {
        let f = Fixture::new();
        let old = f.backup(1, true);
        let new = f.backup(2, true);
        let guard = lock(&f.0).unwrap();
        assert!(retain(&f.0, 1, true).is_err());
        assert!(restore(&old, &f.0.join("blocked")).is_err());
        drop(guard);
        fs::write(old.join("operator-notes"), b"keep").unwrap();
        assert!(retain(&f.0, 1, true).is_err());
        assert!(old.join("manifest.json").exists());
        fs::remove_file(old.join("operator-notes")).unwrap();
        fs::write(new.join("request.json"), b"{\"tampered\":true}").unwrap();
        assert!(retain(&f.0, 1, true).is_err());
        assert!(old.exists());
        assert!(restore(&new, &f.0.join("bad")).is_err());
        assert!(!f.0.join("bad").exists());
    }

    #[test]
    fn restore_roundtrip_preserves_source_and_refuses_occupied_destination() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let source = f.backup(1, true);
        let before = hash(&source.join("sessions.sqlite3")).unwrap();
        let dest = f.0.join("restored");
        let report = restore(&source, &dest).unwrap();
        assert_eq!(report["sessions"], 1);
        let db = source_db(&dest).unwrap();
        compare(&source_db(&source).unwrap(), &db).unwrap();
        assert_eq!(before, hash(&source.join("sessions.sqlite3")).unwrap());
        assert_eq!(
            fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dest.join("sessions.sqlite3"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(dest.join("restore-ready.json").exists());
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!source.join(format!("sessions.sqlite3{suffix}")).exists());
            assert!(!dest.join(format!("sessions.sqlite3{suffix}")).exists());
        }
        assert!(restore(&source, &dest).is_err());
        assert!(restore(&source, &source.join("nested")).is_err());
        assert_eq!(
            report,
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(dest.join("restore-ready.json")).unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn v10_restore_preserves_scheduled_automation_ledgers_and_upgrades_only_copy() {
        let f = Fixture::new();
        let source = f.backup(10, false);
        let definition = json!({
            "id":"weekly", "name":"Weekly", "provider":"demo", "model":null,
            "cwd":"/tmp", "locationId":"local", "hostId":"local", "projectId":null,
            "prompt":"Keep ledger", "enabled":true, "created":1, "updated":1,
            "workflow":{"version":1,"trigger":{"kind":"schedule","timeZone":"America/Chicago",
                "at":"09:30","days":[1,3,5],"missed":"runOnce"},
                "steps":[{"kind":"agent","text":"Keep ledger"}]}
        });
        {
            let db = Connection::open(source.join("sessions.sqlite3")).unwrap();
            db.execute(
                "INSERT INTO automations VALUES ('weekly',?1)",
                [definition.to_string()],
            )
            .unwrap();
            db.execute_batch("INSERT INTO automation_schedules VALUES ('weekly',3000);
                INSERT INTO automation_slots VALUES ('weekly',1000,'schedule:1000','started',NULL,1000);
                INSERT INTO automation_slots VALUES ('weekly',2000,'schedule:2000','missed','Server stopped',2000);
                INSERT INTO automation_slots VALUES ('weekly',3000,'schedule:3000','skipped','Busy',3000);
                INSERT INTO automation_runs VALUES ('run','weekly','schedule:1000','session','Weekly','demo',1000,'completed');
                PRAGMA user_version=10;").unwrap();
        }
        // Complete v10 snapshots must be usable for retention as well as restore.
        publish(&source).unwrap();
        assert_eq!(retain(&f.0, 1, false).unwrap()["remove"], json!([]));
        let before = hash(&source.join("sessions.sqlite3")).unwrap();
        let dest = f.0.join("current");
        assert_eq!(
            restore(&source, &dest).unwrap()["schemaVersion"],
            crate::store::SCHEMA_VERSION
        );
        let source_conn = source_db(&source).unwrap();
        let store = Store::open(&dest.join("sessions.sqlite3")).unwrap();
        assert_eq!(
            source_conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            crate::store::SCHEMA_VERSION
        );
        compare(&source_conn, &store.connection).unwrap();
        assert_eq!(store.schedule_cursor("weekly").unwrap(), Some(3000));
        assert_eq!(
            store
                .run_session("weekly", "schedule:1000")
                .unwrap()
                .as_deref(),
            Some("session")
        );
        assert_eq!(store.automation_runs("weekly").unwrap().len(), 3);
        // v12's agent request ledger is added empty.
        assert_eq!(
            store.automation_request("controller:any", "any").unwrap(),
            None
        );
        let saved = store.automations().unwrap().pop().unwrap();
        assert_eq!(serde_json::to_value(saved).unwrap(), definition);
        assert_eq!(store.load().unwrap().len(), 1); // Restore never dispatches the enabled runOnce policy.
        drop(store);
        let reopened = Store::open(&dest.join("sessions.sqlite3")).unwrap();
        compare(&source_conn, &reopened.connection).unwrap();
        assert_eq!(before, hash(&source.join("sessions.sqlite3")).unwrap());
    }

    #[test]
    fn v10_invalid_scheduled_definition_is_rejected_without_mutating_source() {
        let f = Fixture::new();
        let source = f.backup(10, false);
        {
            let db = Connection::open(source.join("sessions.sqlite3")).unwrap();
            db.execute(
                "INSERT INTO automations VALUES ('invalid',?1)",
                [json!({
                    "id":"invalid","name":"Invalid","provider":"demo","model":null,
                    "cwd":"/tmp","locationId":"local","hostId":"local","projectId":null,
                    "prompt":"Keep","enabled":true,"created":1,"updated":1,
                    "workflow":{"version":1,"trigger":{"kind":"schedule","timeZone":"UTC",
                        "at":"09:30","days":"invalid"},"steps":[{"kind":"agent","text":"Keep"}]}
                })
                .to_string()],
            )
            .unwrap();
            db.execute_batch("PRAGMA user_version=10;").unwrap();
        }
        publish(&source).unwrap();
        let before = hash(&source.join("sessions.sqlite3")).unwrap();
        let dest = f.0.join("invalid-v10");
        assert!(restore(&source, &dest).is_err());
        assert!(!dest.exists());
        assert_eq!(before, hash(&source.join("sessions.sqlite3")).unwrap());
    }

    #[test]
    fn v9_restore_adds_empty_schedule_tables_without_changing_saved_sessions() {
        let f = Fixture::new();
        let source = f.backup(9, false);
        {
            let db = Connection::open(source.join("sessions.sqlite3")).unwrap();
            db.execute_batch("DROP TABLE automation_schedules; DROP TABLE automation_slots; PRAGMA user_version=9;").unwrap();
        }
        let before = hash(&source.join("sessions.sqlite3")).unwrap();
        let dest = f.0.join("from-v9");
        assert_eq!(
            restore(&source, &dest).unwrap()["schemaVersion"],
            crate::store::SCHEMA_VERSION
        );
        let store = Store::open(&dest.join("sessions.sqlite3")).unwrap();
        assert_eq!(store.load().unwrap()[0].0.title, "Retain me");
        assert_eq!(store.load().unwrap()[0].1.events.len(), 1);
        assert_eq!(store.schedule_cursor("missing").unwrap(), None);
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM automation_slots", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(before, hash(&source.join("sessions.sqlite3")).unwrap());
    }

    #[test]
    fn legacy_v3_restore_upgrades_only_staged_copy_and_reopens() {
        let f = Fixture::new();
        let source = f.backup(1, false);
        {
            let db = Connection::open(source.join("sessions.sqlite3")).unwrap();
            db.execute_batch("DROP TABLE agent_completions; DROP TABLE session_creations; PRAGMA user_version=3;").unwrap();
        }
        let before = hash(&source.join("sessions.sqlite3")).unwrap();
        let dest = f.0.join("upgrade");
        restore(&source, &dest).unwrap();
        let store = Store::open(&dest.join("sessions.sqlite3")).unwrap();
        assert_eq!(store.load().unwrap()[0].0.title, "Retain me");
        assert_eq!(
            store.load().unwrap()[0].1.provider_session_id.as_deref(),
            Some("native-context")
        );
        assert_eq!(store.load().unwrap()[0].1.events.len(), 1);
        assert_eq!(
            store
                .connection
                .query_row("SELECT grants FROM agent_access", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "{\"communicate\":true}"
        );
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            crate::store::SCHEMA_VERSION
        );
        assert_eq!(before, hash(&source.join("sessions.sqlite3")).unwrap());
    }

    #[test]
    fn corrupt_sqlite_foreign_keys_and_incomplete_manifests_fail_safely() {
        let f = Fixture::new();
        let broken = f.backup(1, false);
        fs::write(broken.join("sessions.sqlite3"), b"not sqlite").unwrap();
        assert!(restore(&broken, &f.0.join("bad-db")).is_err());
        let orphan = f.backup(2, false);
        {
            let db = Connection::open(orphan.join("sessions.sqlite3")).unwrap();
            db.execute_batch(
                "PRAGMA foreign_keys=OFF; INSERT INTO events VALUES ('missing',1,'{}');",
            )
            .unwrap();
        }
        assert!(restore(&orphan, &f.0.join("bad-fk")).is_err());
        let partial = f.backup(3, false);
        fs::write(partial.join("manifest.json.tmp"), b"partial manifest").unwrap();
        assert_eq!(retain(&f.0, 1, true).unwrap()["remove"], json!([]));
        assert!(partial.exists());
        let dangling = f.backup(4, false);
        std::os::unix::fs::symlink("missing", dangling.join("manifest.json")).unwrap();
        assert!(restore(&dangling, &f.0.join("bad-manifest")).is_err());
        assert!(retain(&f.0, 1, true).is_err());
    }

    #[test]
    fn invalid_catalog_schema_state_sidecars_and_symlinks_are_rejected() {
        let f = Fixture::new();
        let source = f.backup(1, false);
        let dest = f.0.join("rejected");
        fs::write(source.join("sessions.sqlite3-wal"), []).unwrap();
        assert!(restore(&source, &dest).is_err());
        fs::remove_file(source.join("sessions.sqlite3-wal")).unwrap();
        let db = Connection::open(source.join("sessions.sqlite3")).unwrap();
        db.execute_batch("PRAGMA user_version=999;").unwrap();
        assert!(restore(&source, &dest).is_err());
        db.execute_batch("PRAGMA user_version=9; UPDATE sessions SET state='{}';")
            .unwrap();
        drop(db);
        assert!(restore(&source, &dest).is_err());
        assert!(!dest.exists()); // Failed upgrade/deserialization removes only our new directory.
        fs::write(source.join("catalog.json"), b"invalid").unwrap();
        assert!(restore(&source, &dest).is_err());
        fs::remove_file(source.join("catalog.json")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", source.join("catalog.json")).unwrap();
        assert!(restore(&source, &dest).is_err());
        std::os::unix::fs::symlink(&source, f.0.join("linked")).unwrap();
        assert!(restore(&f.0.join("linked"), &dest).is_err());
    }
}
