use crate::{MAX_EVENT_BYTES, MAX_EVENTS, SessionData};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Serialize, Deserialize)]
pub struct Metadata {
    pub id: String,
    pub provider: String,
    pub cwd: PathBuf,
    pub title: String,
    pub initial_model: Option<String>,
    pub created: u64,
    pub host_id: String,
    pub location_id: String,
}
pub struct Store {
    pub(crate) connection: Connection,
    _lock: std::fs::File,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = path.with_extension("sqlite3.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(lock_path)?;
        lock.try_lock()
            .map_err(|_| "Another Agentworks process is already using this session database")?;
        let connection = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 6 {
            return Err("Session database was created by a newer Agentworks version".into());
        }
        connection.execute_batch("BEGIN; CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, metadata TEXT NOT NULL, state TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS events(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, seq INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(session_id,seq));
          CREATE TABLE IF NOT EXISTS automations(id TEXT PRIMARY KEY, definition TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS automation_runs(id TEXT PRIMARY KEY,automation_id TEXT NOT NULL,request_id TEXT NOT NULL,session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,name TEXT NOT NULL,provider TEXT NOT NULL,created INTEGER NOT NULL,status TEXT NOT NULL,UNIQUE(automation_id,request_id));
          CREATE INDEX IF NOT EXISTS automation_runs_by_definition ON automation_runs(automation_id,created);
          CREATE TABLE IF NOT EXISTS agent_access(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,grants TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS agent_messages(id INTEGER PRIMARY KEY AUTOINCREMENT,sender TEXT NOT NULL,sender_title TEXT NOT NULL,recipient TEXT NOT NULL,recipient_title TEXT NOT NULL,text TEXT NOT NULL,reply_to INTEGER,created INTEGER NOT NULL,request_id TEXT NOT NULL,acknowledged INTEGER NOT NULL DEFAULT 0,UNIQUE(sender,request_id));
          CREATE INDEX IF NOT EXISTS agent_messages_recipient ON agent_messages(recipient,id);
          CREATE INDEX IF NOT EXISTS agent_messages_sender ON agent_messages(sender,id);
          CREATE TABLE IF NOT EXISTS agent_audit(id INTEGER PRIMARY KEY AUTOINCREMENT,actor TEXT NOT NULL,action TEXT NOT NULL,target TEXT NOT NULL,outcome TEXT NOT NULL,created INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS agent_starts(actor TEXT NOT NULL,request_id TEXT NOT NULL,session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,PRIMARY KEY(actor,request_id));
          CREATE TABLE IF NOT EXISTS agent_completions(id INTEGER PRIMARY KEY AUTOINCREMENT,author TEXT NOT NULL,author_title TEXT NOT NULL,project_id TEXT,request_id TEXT NOT NULL,payload TEXT NOT NULL,report TEXT NOT NULL,created INTEGER NOT NULL,UNIQUE(author,request_id));
          CREATE INDEX IF NOT EXISTS agent_completions_latest ON agent_completions(author,id DESC);
          CREATE TABLE IF NOT EXISTS session_creations(request_key TEXT PRIMARY KEY,payload TEXT NOT NULL,session_id TEXT,error TEXT,mutation_started INTEGER NOT NULL DEFAULT 0);
          PRAGMA user_version=6; COMMIT;")?;
        Ok(Self {
            connection,
            _lock: lock,
        })
    }
    pub fn insert(
        &mut self,
        metadata: &Metadata,
        state: &SessionData,
        access: Option<&crate::communication::Access>,
        creation_key: Option<&str>,
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO sessions(id,metadata,state) VALUES (?1,?2,?3)",
            params![
                metadata.id,
                serde_json::to_string(metadata)?,
                serde_json::to_string(state)?
            ],
        )?;
        if let Some(access) = access {
            tx.execute(
                "INSERT INTO agent_access(session_id,grants) VALUES (?1,?2)",
                params![metadata.id, serde_json::to_string(access)?],
            )?;
            tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','access',?1,?2,?3)",params![metadata.id,serde_json::to_string(access)?,metadata.created as i64])?;
        }
        if let Some(origin) = &state.control_origin {
            tx.execute(
                "INSERT INTO agent_starts(actor,request_id,session_id) VALUES (?1,?2,?3)",
                params![origin.actor, origin.request, metadata.id],
            )?;
        }
        if let Some(origin) = &state.automation {
            tx.execute("INSERT INTO automation_runs(id,automation_id,request_id,session_id,name,provider,created,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![origin.run_id,origin.automation_id,origin.request_id,metadata.id,origin.name,metadata.provider,i64::try_from(metadata.created)?,crate::automations::run_status(state)])?;
        }
        if let Some(key) = creation_key {
            tx.execute(
                "UPDATE session_creations SET session_id=?2,error=NULL WHERE request_key=?1",
                params![key, metadata.id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn reserve_creation(&mut self, key: &str, payload: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let prior: Option<(String, Option<String>, Option<String>, bool)> = self
            .connection
            .query_row(
                "SELECT payload,session_id,error,mutation_started FROM session_creations WHERE request_key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((saved, session, error, mutation_started)) = prior {
            if saved != payload {
                return Err("Request ID already used for a different session payload".into());
            }
            if let Some(id) = session {
                return Ok(Some(id));
            }
            if !mutation_started {
                // Payload binding persists, but no Git mutation/session insertion was armed.
                self.connection.execute(
                    "UPDATE session_creations SET error=NULL WHERE request_key=?1",
                    [key],
                )?;
                return Ok(None);
            }
            return Err(format!("Previous creation did not complete: {}. Inspect retained files before using a new request ID", error.as_deref().unwrap_or("interrupted preparation")).into());
        }
        self.connection.execute(
            "INSERT INTO session_creations(request_key,payload) VALUES (?1,?2)",
            params![key, payload],
        )?;
        Ok(None)
    }
    pub fn arm_creation(&mut self, key: &str) -> Result<()> {
        if self.connection.execute("UPDATE session_creations SET mutation_started=1 WHERE request_key=?1 AND session_id IS NULL", [key])? != 1 {
            return Err("Creation request could not be durably armed".into());
        }
        Ok(())
    }
    pub fn creation_error(&mut self, key: &str, message: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE session_creations SET error=?2 WHERE request_key=?1 AND session_id IS NULL",
            params![key, message],
        )?;
        Ok(())
    }
    pub fn save_state(&mut self, id: &str, state: &SessionData) -> Result<()> {
        if self.connection.execute(
            "UPDATE sessions SET state=?2 WHERE id=?1",
            params![id, serde_json::to_string(state)?],
        )? != 1
        {
            return Err("Session no longer exists".into());
        }
        Ok(())
    }
    pub fn event(&mut self, id: &str, state: &SessionData, event: &Value) -> Result<()> {
        let transaction = self.connection.transaction()?;
        // A late callback after deletion must never recreate a session.
        if transaction.execute(
            "UPDATE sessions SET state=?2 WHERE id=?1",
            params![id, serde_json::to_string(state)?],
        )? != 0
        {
            transaction.execute(
                "INSERT INTO events(session_id,seq,payload) VALUES (?1,?2,?3)",
                params![id, i64::try_from(state.seq)?, serde_json::to_string(event)?],
            )?;
        }
        if state.automation.is_some() {
            transaction.execute(
                "UPDATE automation_runs SET status=?2 WHERE session_id=?1 AND status<>?2",
                params![id, crate::automations::run_status(state)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn batch(&mut self, id: &str, state: &SessionData, events: &[Value]) -> Result<()> {
        let tx = self.connection.transaction()?;
        if tx.execute(
            "UPDATE sessions SET state=?2 WHERE id=?1",
            params![id, serde_json::to_string(state)?],
        )? == 0
        {
            return Err("Session was deleted while importing".into());
        }
        for event in events {
            tx.execute(
                "INSERT INTO events(session_id,seq,payload) VALUES (?1,?2,?3)",
                params![
                    id,
                    event["seq"].as_i64().ok_or("Invalid sequence")?,
                    serde_json::to_string(event)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let tx = self.connection.transaction()?;
        tx.execute(
            "UPDATE automation_runs SET status='deleted' WHERE session_id=?1",
            [id],
        )?;
        tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn automations(&self) -> Result<Vec<crate::automations::Definition>> {
        let mut stmt = self
            .connection
            .prepare("SELECT definition FROM automations ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|s| serde_json::from_str(&s))
            .collect::<serde_json::Result<Vec<_>>>()?)
    }
    pub fn automation(&self, id: &str) -> Result<Option<crate::automations::Definition>> {
        use rusqlite::OptionalExtension;
        let value = self
            .connection
            .query_row(
                "SELECT definition FROM automations WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.map(|s| serde_json::from_str(&s)).transpose()?)
    }
    pub fn save_automation(&mut self, definition: &crate::automations::Definition) -> Result<()> {
        self.connection.execute("INSERT INTO automations(id,definition) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition",params![definition.id,serde_json::to_string(definition)?])?;
        Ok(())
    }
    pub fn detach_automation_project(&mut self, id: &str) -> Result<()> {
        self.connection.execute("UPDATE automations SET definition=json_set(definition,'$.projectId',NULL) WHERE json_extract(definition,'$.projectId')=?1",[id])?;
        Ok(())
    }
    pub fn delete_automation(&mut self, id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM automations WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn automation_runs(&self, id: &str) -> Result<Vec<Value>> {
        let mut stmt=self.connection.prepare("SELECT id,session_id,name,provider,created,status FROM automation_runs WHERE automation_id=?1 ORDER BY created DESC LIMIT 100")?;
        Ok(stmt.query_map([id],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"sessionId":r.get::<_,Option<String>>(1)?,"name":r.get::<_,String>(2)?,"provider":r.get::<_,String>(3)?,"created":r.get::<_,i64>(4)?,"status":r.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn run_session(&self, id: &str, request_id: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let row = self
            .connection
            .query_row(
                "SELECT session_id FROM automation_runs WHERE automation_id=?1 AND request_id=?2",
                params![id, request_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(row.map(|session| session.unwrap_or_default()))
    }
    pub fn load(&self) -> Result<Vec<(Metadata, SessionData)>> {
        let mut query = self
            .connection
            .prepare("SELECT id,metadata,state FROM sessions ORDER BY rowid")?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            let (id, metadata, state) = row?;
            let metadata: Metadata = serde_json::from_str(&metadata)?;
            let mut state: SessionData = serde_json::from_str(&state)?;
            let mut events = self.connection.prepare(
                "SELECT payload FROM events WHERE session_id=?1 ORDER BY seq DESC LIMIT ?2",
            )?;
            let recent = events
                .query_map(params![id, MAX_EVENTS as i64], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            state.events = recent
                .into_iter()
                .rev()
                .map(|s| serde_json::from_str(&s))
                .collect::<serde_json::Result<VecDeque<_>>>()?;
            state.bytes = state.events.iter().map(|e| e.to_string().len()).sum();
            while state.bytes > MAX_EVENT_BYTES && state.events.len() > 1 {
                state.bytes -= state.events.pop_front().unwrap().to_string().len();
            }
            sessions.push((metadata, state));
        }
        Ok(sessions)
    }
    /// One-time import from the pre-persistence HTTP snapshot. Never overwrite an existing row.
    pub fn import_legacy(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let snapshots: Vec<Value> = serde_json::from_slice(&std::fs::read(path)?)?;
        let tx = self.connection.transaction()?;
        for snapshot in snapshots {
            let s = &snapshot["session"];
            let id = s["id"].as_str().ok_or("Legacy snapshot has no ID")?;
            let metadata = Metadata {
                id: id.into(),
                provider: s["provider"].as_str().ok_or("Missing provider")?.into(),
                cwd: PathBuf::from(s["cwd"].as_str().ok_or("Missing directory")?),
                title: s["title"].as_str().unwrap_or("Imported session").into(),
                initial_model: snapshot["models"]["currentValue"]
                    .as_str()
                    .map(str::to_owned),
                created: s["created"].as_u64().unwrap_or(0),
                host_id: s["hostId"].as_str().unwrap_or("local").into(),
                location_id: s["locationId"].as_str().ok_or("Missing location")?.into(),
            };
            let events = snapshot["events"].as_array().ok_or("Missing events")?;
            let provider_session_id = events
                .iter()
                .rev()
                .find(|e| e["kind"] == "ready")
                .and_then(|e| e["data"]["providerSessionId"].as_str())
                .map(str::to_owned);
            let state = SessionData {
                lifecycle: Default::default(),
                status: "suspended".into(),
                seq: snapshot["seq"].as_u64().unwrap_or(0),
                events: VecDeque::new(),
                bytes: 0,
                requests: events
                    .iter()
                    .filter(|e| e["kind"] == "user")
                    .filter_map(|e| e["data"]["requestId"].as_str().map(str::to_owned))
                    .collect(),
                models: snapshot["models"].clone(),
                available_commands: snapshot["commands"].clone(),
                turn_active: false,
                project_id: s["projectId"].as_str().map(str::to_owned),
                provider_session_id,
                external: false,
                import_pending: false,
                opencode_host: false,
                automation: None,
                queued_prompt: None,
                control_origin: None,
                storage_error: None,
                continuation: Default::default(),
                timeouts: None,
            };
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO sessions(id,metadata,state) VALUES (?1,?2,?3)",
                params![
                    id,
                    serde_json::to_string(&metadata)?,
                    serde_json::to_string(&state)?
                ],
            )?;
            if inserted != 0 {
                for event in events {
                    tx.execute(
                        "INSERT INTO events(session_id,seq,payload) VALUES (?1,?2,?3)",
                        params![
                            id,
                            event["seq"].as_i64().ok_or("Missing sequence")?,
                            serde_json::to_string(event)?
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        std::fs::rename(path, path.with_extension("imported.json"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn v5_migration_preserves_every_existing_table_and_defaults_policy_off() {
        let path =
            std::env::temp_dir().join(format!("agentworks-v5-{}.sqlite3", uuid::Uuid::new_v4()));
        let snapshot = |db: &Connection| -> Vec<(String, Vec<String>)> {
            let mut stmt=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
            let names: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            names
                .into_iter()
                .map(|name| {
                    let mut q = db
                        .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
                        .unwrap();
                    let columns = q.column_count();
                    let rows = q
                        .query_map([], |r| {
                            Ok((0..columns)
                                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                                .collect::<Vec<_>>()
                                .join("|"))
                        })
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap();
                    (name, rows)
                })
                .collect()
        };
        let before;
        {
            let store = Store::open(&path).unwrap();
            store.connection.execute_batch(r#"
              INSERT INTO sessions VALUES ('old','{}','{}');
              INSERT INTO events VALUES ('old',1,'{"kind":"user","seq":1}');
              INSERT INTO agent_access VALUES ('old','{"startSessions":true}');
              INSERT INTO agent_starts VALUES ('old','start','old');
              INSERT INTO agent_completions(author,author_title,request_id,payload,report,created) VALUES ('old','Old','done','{}','{"status":"completed"}',1);
              INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('old','access','old','accepted',1);
              INSERT INTO agent_messages(sender,sender_title,recipient,recipient_title,text,created,request_id) VALUES ('old','Old','old','Old','history',1,'mail');
              INSERT INTO automations VALUES ('auto','{}');
              INSERT INTO automation_runs VALUES ('run','auto','req','old','Test','demo',1,'completed');
              INSERT INTO session_creations VALUES ('create','{}','old',NULL,1);
              PRAGMA user_version=5;
            "#).unwrap();
            before = snapshot(&store.connection);
        }
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(before, snapshot(&store.connection));
            assert_eq!(
                store
                    .connection
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6
            );
        }
        let state:SessionData=serde_json::from_value(json!({"status":"idle","seq":0,"requests":[],"models":null,"available_commands":[],"turn_active":false,"project_id":null,"provider_session_id":null})).unwrap();
        assert!(!state.continuation.enabled);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("sqlite3.lock")).unwrap();
    }
    #[test]
    fn migrates_v3_without_changing_existing_data() {
        let path = std::env::temp_dir().join(format!(
            "agentworks-migration-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        {
            let store = Store::open(&path).unwrap();
            store.connection.execute_batch("DROP TABLE agent_completions;
                INSERT INTO sessions VALUES ('old','{\"title\":\"Original\"}','{\"status\":\"suspended\"}');
                INSERT INTO events VALUES ('old',7,'{\"seq\":7}');
                INSERT INTO agent_access VALUES ('old','{\"communicate\":true}');
                INSERT INTO agent_messages(sender,sender_title,recipient,recipient_title,text,created,request_id) VALUES ('old','Original','peer','Peer','Retain me',1,'request');
                INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('old','send_message','peer','accepted',1);
                PRAGMA user_version=3;").unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            let db = &store.connection;
            assert_eq!(
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6
            );
            for (sql, expected) in [
                (
                    "SELECT metadata FROM sessions WHERE id='old'",
                    "{\"title\":\"Original\"}",
                ),
                (
                    "SELECT state FROM sessions WHERE id='old'",
                    "{\"status\":\"suspended\"}",
                ),
                (
                    "SELECT payload FROM events WHERE session_id='old'",
                    "{\"seq\":7}",
                ),
                (
                    "SELECT grants FROM agent_access WHERE session_id='old'",
                    "{\"communicate\":true}",
                ),
                (
                    "SELECT text FROM agent_messages WHERE sender='old'",
                    "Retain me",
                ),
                (
                    "SELECT outcome FROM agent_audit WHERE actor='old'",
                    "accepted",
                ),
            ] {
                assert_eq!(
                    db.query_row(sql, [], |r| r.get::<_, String>(0)).unwrap(),
                    expected
                );
            }
            assert_eq!(
                store.latest_completion("old", None).unwrap()["report"],
                serde_json::Value::Null
            );
            // Historical identity intentionally survives deletion, as with mailbox senders.
            db.execute("INSERT INTO agent_completions(author,author_title,project_id,request_id,payload,report,created) VALUES ('old','Original','project','done','{}','{\"author\":\"old\",\"authorTitle\":\"Original\"}',1)", []).unwrap();
            db.execute("DELETE FROM sessions WHERE id='old'", [])
                .unwrap();
            assert_eq!(
                store.latest_completion("old", None).unwrap()["report"]["authorTitle"],
                "Original"
            );
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("sqlite3.lock")).unwrap();
    }

    #[test]
    fn v4_migration_and_interrupted_creation_reservations_survive_restart() {
        let path = std::env::temp_dir().join(format!(
            "agentworks-creation-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        {
            let store = Store::open(&path).unwrap();
            store
                .connection
                .execute_batch("DROP TABLE session_creations; PRAGMA user_version=4;")
                .unwrap();
        }
        {
            let mut store = Store::open(&path).unwrap();
            assert_eq!(store.reserve_creation("key", "payload").unwrap(), None);
            store
                .creation_error("key", "mutation-free validation failure")
                .unwrap();
            assert_eq!(store.reserve_creation("key", "payload").unwrap(), None);
            store.arm_creation("key").unwrap();
            assert_eq!(
                store.reserve_creation("preflight", "payload").unwrap(),
                None
            );
            assert!(
                store
                    .reserve_creation("key", "changed")
                    .unwrap_err()
                    .to_string()
                    .contains("different")
            );
        }
        {
            let mut store = Store::open(&path).unwrap();
            assert_eq!(
                store.reserve_creation("preflight", "payload").unwrap(),
                None
            );
            assert!(
                store
                    .reserve_creation("key", "payload")
                    .unwrap_err()
                    .to_string()
                    .contains("interrupted")
            );
            store.creation_error("key", "retained destination").unwrap();
            assert!(
                store
                    .reserve_creation("key", "payload")
                    .unwrap_err()
                    .to_string()
                    .contains("retained destination")
            );
            store
                .connection
                .execute(
                    "UPDATE session_creations SET session_id='old' WHERE request_key='key'",
                    [],
                )
                .unwrap();
            assert_eq!(
                store.reserve_creation("key", "payload").unwrap(),
                Some("old".into())
            );
        }
        std::fs::remove_file(&path).unwrap();
    }
    #[test]
    fn legacy_snapshot_import_preserves_identity_and_transcript() {
        let dir = std::env::temp_dir().join(format!("agentworks-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("legacy-sessions.json");
        std::fs::write(&legacy,serde_json::to_vec(&json!([{
            "seq":2,"session":{"id":"old-id","provider":"opencode","cwd":"/projects/app","title":"Keep this","created":123,"hostId":"local","locationId":"location","projectId":"project"},
            "models":{"currentValue":"opencode/big-pickle"},"commands":[],"events":[
                {"seq":1,"kind":"ready","data":{"providerSessionId":"native-id"}},
                {"seq":2,"kind":"user","data":{"text":"My conversation","requestId":"request"}}
            ]
        }])).unwrap()).unwrap();
        let mut store = Store::open(&dir.join("sessions.sqlite3")).unwrap();
        store.import_legacy(&legacy).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0.id, "old-id");
        assert_eq!(loaded[0].0.created, 123);
        assert_eq!(
            loaded[0].1.provider_session_id.as_deref(),
            Some("native-id")
        );
        assert_eq!(loaded[0].1.events.len(), 2);
        assert!(loaded[0].1.requests.contains("request"));
        assert_eq!(loaded[0].1.project_id.as_deref(), Some("project"));
        assert!(!legacy.exists());
        assert!(legacy.with_extension("imported.json").exists());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn events_and_state_commit_together_and_deleted_sessions_stay_deleted() {
        let path =
            std::env::temp_dir().join(format!("agentworks-store-{}.sqlite3", uuid::Uuid::new_v4()));
        let metadata = Metadata {
            id: "test".into(),
            provider: "demo".into(),
            cwd: "/projects/app".into(),
            title: "Test".into(),
            initial_model: None,
            created: 1,
            host_id: "local".into(),
            location_id: "location".into(),
        };
        let mut state = SessionData {
            lifecycle: Default::default(),
            status: "starting".into(),
            seq: 0,
            events: VecDeque::new(),
            bytes: 0,
            requests: Default::default(),
            models: Value::Null,
            available_commands: json!([]),
            turn_active: false,
            project_id: None,
            provider_session_id: Some("provider-session".into()),
            external: false,
            import_pending: false,
            opencode_host: false,
            automation: None,
            queued_prompt: None,
            control_origin: None,
            storage_error: None,
            continuation: Default::default(),
                timeouts: None,
        };
        {
            let mut store = Store::open(&path).unwrap();
            store.insert(&metadata, &state, None, None).unwrap();
            assert!(
                Store::open(&path).is_err(),
                "a second server must not open the same database"
            );
            state.seq = 1;
            state.status = "idle".into();
            let event = json!({"seq":1,"kind":"status","data":{"status":"idle"}});
            store.event("test", &state, &event).unwrap();
            // Duplicate sequence must roll back the accompanying state change.
            state.status = "running".into();
            assert!(store.event("test", &state, &event).is_err());
        }
        {
            let mut store = Store::open(&path).unwrap();
            let loaded = store.load().unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].1.status, "idle");
            assert_eq!(loaded[0].1.events.len(), 1);
            assert_eq!(
                loaded[0].1.provider_session_id.as_deref(),
                Some("provider-session")
            );
            store.delete("test").unwrap();
            state.seq = 2;
            store.event("test", &state, &json!({"seq":2})).unwrap();
            assert!(store.load().unwrap().is_empty());
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("sqlite3.lock")).unwrap();
    }
}
