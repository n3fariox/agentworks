//! Secret rows and session assignments in the service database. Callers pass their own
//! connection or transaction, so they keep their transaction and locking boundaries.
use crate::{
    model::{Approval, Assignment, Delegation, Delivery},
    vault::Vault,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use zeroize::Zeroizing;

/// `session_secrets` references the service's `sessions` table.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS secrets(id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,description TEXT NOT NULL,nonce BLOB NOT NULL,value BLOB NOT NULL,length INTEGER NOT NULL,created INTEGER NOT NULL,updated INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS session_secrets(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,secret_id TEXT NOT NULL REFERENCES secrets(id) ON DELETE CASCADE,delivery TEXT NOT NULL,delegation TEXT NOT NULL,approval TEXT NOT NULL,origin TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(session_id,secret_id));
  CREATE INDEX IF NOT EXISTS session_secrets_by_secret ON session_secrets(secret_id);";

/// A stored secret. Its value stays sealed until opened with the vault.
pub struct Secret {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Value length in bytes.
    pub length: i64,
    pub created: i64,
    pub updated: i64,
    sealed: Vec<u8>,
}
impl Secret {
    pub fn value(&self, vault: &Vault) -> Option<Zeroizing<String>> {
        vault.open_value(&self.id, &self.name, &self.sealed)
    }
    /// Whether the value opens with `vault`; false when it was stored under a different
    /// key file and must be entered again. `None` without a vault.
    pub fn readable(&self, vault: Option<&Vault>) -> Option<bool> {
        vault.map(|vault| self.value(vault).is_some())
    }
}
const COLUMNS: &str = "id,name,description,nonce,value,length,created,updated";
fn row(r: &rusqlite::Row) -> rusqlite::Result<Secret> {
    let mut sealed: Vec<u8> = r.get(3)?;
    sealed.extend_from_slice(&r.get::<_, Vec<u8>>(4)?);
    Ok(Secret {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        sealed,
        length: r.get(5)?,
        created: r.get(6)?,
        updated: r.get(7)?,
    })
}

/// Every secret, by name.
pub fn all(db: &Connection) -> rusqlite::Result<Vec<Secret>> {
    db.prepare(&format!("SELECT {COLUMNS} FROM secrets ORDER BY name"))?
        .query_map([], row)?
        .collect()
}
pub fn find(db: &Connection, id: &str) -> rusqlite::Result<Option<Secret>> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM secrets WHERE id=?1"),
        [id],
        row,
    )
    .optional()
}
pub fn find_by_name(db: &Connection, name: &str) -> rusqlite::Result<Option<Secret>> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM secrets WHERE name=?1"),
        [name],
        row,
    )
    .optional()
}
/// Seals `value` for a new row. Fails on a duplicate name.
pub fn insert(
    db: &Connection,
    vault: &Vault,
    id: &str,
    name: &str,
    description: &str,
    value: &str,
    now: i64,
) -> Result<(), String> {
    let (nonce, body) = vault.seal_value(id, name, value)?;
    db.execute(
        "INSERT INTO secrets(id,name,description,nonce,value,length,created,updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
        params![id, name, description, nonce, body, value.len() as i64, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn set_description(
    db: &Connection,
    id: &str,
    description: &str,
    now: i64,
) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE secrets SET description=?2,updated=?3 WHERE id=?1",
        params![id, description, now],
    )?;
    Ok(())
}
/// Replaces the value of `secret`, sealed for its row.
pub fn set_value(
    db: &Connection,
    vault: &Vault,
    secret: &Secret,
    value: &str,
    now: i64,
) -> Result<(), String> {
    let (nonce, body) = vault.seal_value(&secret.id, &secret.name, value)?;
    db.execute(
        "UPDATE secrets SET nonce=?2,value=?3,length=?4,updated=?5 WHERE id=?1",
        params![secret.id, nonce, body, value.len() as i64, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
/// Deletes a secret and, by cascade, its assignments.
pub fn delete(db: &Connection, id: &str) -> rusqlite::Result<()> {
    db.execute("DELETE FROM secrets WHERE id=?1", [id])?;
    Ok(())
}
/// The number of sessions each secret is assigned to, by secret ID.
pub fn session_counts(db: &Connection) -> rusqlite::Result<HashMap<String, i64>> {
    db.prepare("SELECT secret_id,count(*) FROM session_secrets GROUP BY secret_id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

fn text<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}
fn parse<T: serde::de::DeserializeOwned>(text: &str) -> rusqlite::Result<T> {
    serde_json::from_value(json!(text))
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
}

/// A session's assignments, by secret name.
pub fn assignments(db: &Connection, session: &str) -> rusqlite::Result<Vec<Assignment>> {
    let mut query = db.prepare("SELECT a.secret_id,s.name,a.delivery,a.delegation,a.approval,a.origin FROM session_secrets a JOIN secrets s ON s.id=a.secret_id WHERE a.session_id=?1 ORDER BY s.name")?;
    query
        .query_map([session], |r| {
            Ok(Assignment {
                secret_id: r.get(0)?,
                name: r.get(1)?,
                delivery: parse(&r.get::<_, String>(2)?)?,
                delegation: parse(&r.get::<_, String>(3)?)?,
                approval: parse(&r.get::<_, String>(4)?)?,
                origin: r.get(5)?,
            })
        })?
        .collect()
}
/// What a human or a delegating session grants; unset fields take the safest default.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub secret_id: String,
    #[serde(default)]
    pub delivery: Delivery,
    #[serde(default)]
    pub delegation: Delegation,
    #[serde(default)]
    pub approval: Approval,
}
pub fn clear_assignments(db: &Connection, session: &str) -> rusqlite::Result<()> {
    db.execute("DELETE FROM session_secrets WHERE session_id=?1", [session])?;
    Ok(())
}
pub fn assign(
    db: &Connection,
    session: &str,
    grant: &Grant,
    origin: &str,
    now: i64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO session_secrets(session_id,secret_id,delivery,delegation,approval,origin,created) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![session, grant.secret_id, text(grant.delivery), text(grant.delegation), text(grant.approval), origin, now],
    )?;
    Ok(())
}

/// A session's `environment` secrets, decrypted for one harness start.
#[derive(Default)]
pub struct Environment {
    pub vars: Vec<(String, Zeroizing<String>)>,
    /// Names that could not be decrypted (storage unavailable or value needs re-entry).
    pub unavailable: Vec<String>,
}
pub fn environment(
    db: &Connection,
    session: &str,
    vault: Option<&Vault>,
) -> rusqlite::Result<Environment> {
    let mut out = Environment::default();
    for a in assignments(db, session)? {
        if a.delivery != Delivery::Environment {
            continue;
        }
        let value = match vault {
            Some(vault) => find(db, &a.secret_id)?.and_then(|secret| secret.value(vault)),
            None => None,
        };
        match value {
            Some(value) => out.vars.push((a.name, value)),
            None => out.unavailable.push(a.name),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{
        open_or_create,
        tests::{TEST_KDF, temp},
    };

    #[test]
    fn assignments_join_names_and_environment_decrypts_only_environment_delivery() {
        let dir = temp();
        let vault = open_or_create(&dir.join("k"), TEST_KDF).unwrap();
        let other = open_or_create(&dir.join("other"), TEST_KDF).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE sessions(id TEXT PRIMARY KEY); INSERT INTO sessions VALUES ('s');")
            .unwrap();
        db.execute_batch(SCHEMA).unwrap();
        insert(&db, &vault, "1", "B_TOKEN", "", "bravo-value", 1).unwrap();
        insert(&db, &vault, "2", "A_TOKEN", "", "alpha-value", 1).unwrap();
        insert(&db, &other, "3", "C_TOKEN", "", "charlie-value", 1).unwrap();
        assert!(insert(&db, &vault, "4", "A_TOKEN", "", "duplicate", 1).is_err());
        let grant = |id: &str, delivery| Grant {
            secret_id: id.into(),
            delivery,
            delegation: Delegation::Restricted,
            approval: Approval::None,
        };
        assign(&db, "s", &grant("1", Delivery::Environment), "human", 2).unwrap();
        assign(&db, "s", &grant("2", Delivery::Tool), "human", 2).unwrap();
        assign(&db, "s", &grant("3", Delivery::Environment), "human", 2).unwrap();
        let names: Vec<_> = assignments(&db, "s")
            .unwrap()
            .into_iter()
            .map(|a| a.describe())
            .collect();
        assert_eq!(
            names,
            [
                "A_TOKEN (tool)",
                "B_TOKEN (environment)",
                "C_TOKEN (environment)"
            ]
        );
        let env = environment(&db, "s", Some(&vault)).unwrap();
        assert_eq!(env.vars.len(), 1);
        assert_eq!(
            (env.vars[0].0.as_str(), env.vars[0].1.as_str()),
            ("B_TOKEN", "bravo-value")
        );
        assert_eq!(
            env.unavailable,
            ["C_TOKEN"],
            "sealed under another key file"
        );
        assert_eq!(
            environment(&db, "s", None).unwrap().unavailable,
            ["B_TOKEN", "C_TOKEN"]
        );
        assert_eq!(session_counts(&db).unwrap()["1"], 1);
        delete(&db, "1").unwrap();
        assert_eq!(
            assignments(&db, "s").unwrap().len(),
            2,
            "assignments cascade"
        );
        clear_assignments(&db, "s").unwrap();
        assert!(assignments(&db, "s").unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
