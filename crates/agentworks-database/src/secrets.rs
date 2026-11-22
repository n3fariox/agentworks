//! Session secrets: sealed values and their assignments to sessions. Values arrive and
//! leave sealed; encryption, validation and assignment rules are in agentworks-secrets.
use crate::Store;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

/// A stored secret. `sealed` is nonce‖ciphertext‖tag.
pub struct SealedSecret {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Value length in bytes.
    pub length: i64,
    pub created: i64,
    pub updated: i64,
    pub sealed: Vec<u8>,
}
/// A sealed value as (nonce, ciphertext‖tag) and its plaintext length.
pub struct SealedValue<'a> {
    pub nonce: &'a [u8],
    pub body: &'a [u8],
    pub length: usize,
}
/// One secret granted to one session. Policies are stored as their names.
#[derive(Clone, PartialEq, Debug)]
pub struct SecretAssignment {
    pub secret_id: String,
    pub name: String,
    pub delivery: String,
    pub delegation: String,
    pub approval: String,
    /// `human` or the delegating session's ID.
    pub origin: String,
}
/// What to assign; the origin is kept for a secret that was already assigned.
pub struct SecretGrant<'a> {
    pub secret_id: &'a str,
    pub delivery: &'a str,
    pub delegation: &'a str,
    pub approval: &'a str,
}

/// Seals a new value for the current row: (nonce, ciphertext‖tag, plaintext length).
pub type Seal<'a, E> = &'a dyn Fn(&SealedSecret) -> Result<(Vec<u8>, Vec<u8>, usize), E>;
/// A session's assignments before and after a change.
pub type Replaced = (Vec<SecretAssignment>, Vec<SecretAssignment>);

const COLUMNS: &str = "id,name,description,nonce,value,length,created,updated";
fn row(r: &rusqlite::Row) -> rusqlite::Result<SealedSecret> {
    let mut sealed: Vec<u8> = r.get(3)?;
    sealed.extend_from_slice(&r.get::<_, Vec<u8>>(4)?);
    Ok(SealedSecret {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        sealed,
        length: r.get(5)?,
        created: r.get(6)?,
        updated: r.get(7)?,
    })
}
fn find(db: &Connection, column: &str, key: &str) -> rusqlite::Result<Option<SealedSecret>> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM secrets WHERE {column}=?1"),
        [key],
        row,
    )
    .optional()
}
fn audit(db: &Connection, action: &str, name: &str, now: i64) -> rusqlite::Result<usize> {
    db.execute(
        "INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human',?1,?2,'ok',?3)",
        params![action, format!("secret:{name}"), now],
    )
}
fn assignments(db: &Connection, session: &str) -> rusqlite::Result<Vec<SecretAssignment>> {
    db.prepare("SELECT a.secret_id,s.name,a.delivery,a.delegation,a.approval,a.origin FROM session_secrets a JOIN secrets s ON s.id=a.secret_id WHERE a.session_id=?1 ORDER BY s.name")?
        .query_map([session], |r| {
            Ok(SecretAssignment {
                secret_id: r.get(0)?,
                name: r.get(1)?,
                delivery: r.get(2)?,
                delegation: r.get(3)?,
                approval: r.get(4)?,
                origin: r.get(5)?,
            })
        })?
        .collect()
}

impl Store {
    /// Every secret, by name.
    pub fn secrets(&self) -> rusqlite::Result<Vec<SealedSecret>> {
        self.connection
            .prepare(&format!("SELECT {COLUMNS} FROM secrets ORDER BY name"))?
            .query_map([], row)?
            .collect()
    }
    pub fn secret(&self, id: &str) -> rusqlite::Result<Option<SealedSecret>> {
        find(&self.connection, "id", id)
    }
    pub fn secret_named(&self, name: &str) -> rusqlite::Result<Option<SealedSecret>> {
        find(&self.connection, "name", name)
    }
    /// The number of sessions each secret is assigned to, by secret ID.
    pub fn secret_session_counts(&self) -> rusqlite::Result<HashMap<String, i64>> {
        self.connection
            .prepare("SELECT secret_id,count(*) FROM session_secrets GROUP BY secret_id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }
    /// Adds a secret and its audit record. `None` when the name is taken.
    pub fn create_secret(
        &mut self,
        id: &str,
        name: &str,
        description: &str,
        value: &SealedValue<'_>,
        now: i64,
    ) -> rusqlite::Result<Option<SealedSecret>> {
        let tx = self.connection.transaction()?;
        if find(&tx, "name", name)?.is_some() {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO secrets(id,name,description,nonce,value,length,created,updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
            params![id, name, description, value.nonce, value.body, value.length as i64, now],
        )?;
        audit(&tx, "secret.create", name, now)?;
        let saved = find(&tx, "id", id)?;
        tx.commit()?;
        Ok(saved)
    }
    /// Changes a secret's description and/or value, auditing each. `value` seals the
    /// new value for the current row, which it receives. `None` when the secret is gone.
    pub fn update_secret<E: From<rusqlite::Error>>(
        &mut self,
        id: &str,
        description: Option<&str>,
        value: Option<Seal<'_, E>>,
        now: i64,
    ) -> Result<Option<SealedSecret>, E> {
        let tx = self.connection.transaction()?;
        let Some(current) = find(&tx, "id", id)? else {
            return Ok(None);
        };
        if let Some(description) = description {
            tx.execute(
                "UPDATE secrets SET description=?2,updated=?3 WHERE id=?1",
                params![id, description, now],
            )?;
            audit(&tx, "secret.update", &current.name, now)?;
        }
        if let Some(seal) = value {
            let (nonce, body, length) = seal(&current)?;
            tx.execute(
                "UPDATE secrets SET nonce=?2,value=?3,length=?4,updated=?5 WHERE id=?1",
                params![id, nonce, body, length as i64, now],
            )?;
            audit(&tx, "secret.value", &current.name, now)?;
        }
        let saved = find(&tx, "id", id)?;
        tx.commit()?;
        Ok(saved)
    }
    /// Deletes a secret, its assignments (by cascade) and records the audit. False when
    /// it did not exist.
    pub fn delete_secret(&mut self, id: &str, now: i64) -> rusqlite::Result<bool> {
        let tx = self.connection.transaction()?;
        let Some(current) = find(&tx, "id", id)? else {
            return Ok(false);
        };
        tx.execute("DELETE FROM secrets WHERE id=?1", [id])?;
        audit(&tx, "secret.delete", &current.name, now)?;
        tx.commit()?;
        Ok(true)
    }
    /// A session's assignments, by secret name.
    pub fn secret_assignments(&self, session: &str) -> rusqlite::Result<Vec<SecretAssignment>> {
        assignments(&self.connection, session)
    }
    /// Replaces a session's assignments and returns them (before, after). Retained
    /// secrets keep their origin; new ones get `origin`. When `summary` of the
    /// assignments changes, `summary` of the result is audited as `secrets`. `None`, with
    /// nothing changed, when a granted secret no longer exists.
    pub fn replace_secret_assignments(
        &mut self,
        session: &str,
        grants: &[SecretGrant<'_>],
        origin: &str,
        now: i64,
        summary: impl Fn(&[SecretAssignment]) -> Vec<String>,
    ) -> rusqlite::Result<Option<Replaced>> {
        let tx = self.connection.transaction()?;
        let before = assignments(&tx, session)?;
        tx.execute("DELETE FROM session_secrets WHERE session_id=?1", [session])?;
        for grant in grants {
            if find(&tx, "id", grant.secret_id)?.is_none() {
                return Ok(None);
            }
            let origin = before
                .iter()
                .find(|b| b.secret_id == grant.secret_id)
                .map_or(origin, |b| b.origin.as_str());
            tx.execute(
                "INSERT INTO session_secrets(session_id,secret_id,delivery,delegation,approval,origin,created) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![session, grant.secret_id, grant.delivery, grant.delegation, grant.approval, origin, now],
            )?;
        }
        let after = assignments(&tx, session)?;
        if summary(&before) != summary(&after) {
            tx.execute(
                "INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','secrets',?1,?2,?3)",
                params![session, summary(&after).join(", "), now],
            )?;
        }
        tx.commit()?;
        Ok(Some((before, after)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(bytes: &'static [u8]) -> SealedValue<'static> {
        SealedValue {
            nonce: &bytes[..2],
            body: &bytes[2..],
            length: bytes.len(),
        }
    }
    fn grant<'a>(id: &'a str, delivery: &'a str) -> SecretGrant<'a> {
        SecretGrant {
            secret_id: id,
            delivery,
            delegation: "restricted",
            approval: "none",
        }
    }
    fn audits(db: &Store) -> Vec<(String, String)> {
        db.connection
            .prepare("SELECT action,target FROM agent_audit ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn secrets_and_assignments_are_saved_audited_and_cascade() {
        let mut db = Store::memory().unwrap();
        db.connection
            .execute_batch("INSERT INTO sessions VALUES ('s','{}','{}');")
            .unwrap();
        let a = db
            .create_secret("1", "B_TOKEN", "b", &value(b"nnbravo"), 1)
            .unwrap()
            .unwrap();
        assert_eq!((a.sealed.as_slice(), a.length), (&b"nnbravo"[..], 7));
        db.create_secret("2", "A_TOKEN", "", &value(b"nnalpha"), 1)
            .unwrap()
            .unwrap();
        assert!(
            db.create_secret("3", "A_TOKEN", "", &value(b"nnx"), 1)
                .unwrap()
                .is_none(),
            "names are unique"
        );
        assert_eq!(
            db.secrets()
                .unwrap()
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["A_TOKEN", "B_TOKEN"]
        );
        let updated = db
            .update_secret::<rusqlite::Error>(
                "1",
                Some("new"),
                Some(&|current: &SealedSecret| {
                    assert_eq!(current.name, "B_TOKEN");
                    Ok((b"mm".to_vec(), b"changed".to_vec(), 7))
                }),
                2,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (updated.description.as_str(), updated.sealed.as_slice()),
            ("new", &b"mmchanged"[..])
        );

        let summary = |list: &[SecretAssignment]| {
            list.iter()
                .map(|a| format!("{} {}", a.name, a.delivery))
                .collect::<Vec<_>>()
        };
        let (before, after) = db
            .replace_secret_assignments(
                "s",
                &[grant("1", "environment"), grant("2", "tool")],
                "human",
                3,
                summary,
            )
            .unwrap()
            .unwrap();
        assert!(before.is_empty());
        assert_eq!(summary(&after), ["A_TOKEN tool", "B_TOKEN environment"]);
        assert!(
            db.replace_secret_assignments("s", &[grant("missing", "tool")], "human", 4, summary)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.secret_assignments("s").unwrap(),
            after,
            "a missing secret changes nothing"
        );
        // Retained secrets keep their origin.
        let (_, kept) = db
            .replace_secret_assignments("s", &[grant("1", "tool")], "other-session", 5, summary)
            .unwrap()
            .unwrap();
        assert_eq!(kept[0].origin, "human");
        assert_eq!(db.secret_session_counts().unwrap()["1"], 1);
        assert!(db.delete_secret("1", 6).unwrap());
        assert!(!db.delete_secret("1", 6).unwrap());
        assert!(
            db.secret_assignments("s").unwrap().is_empty(),
            "assignments cascade"
        );
        assert_eq!(
            audits(&db),
            [
                ("secret.create", "secret:B_TOKEN"),
                ("secret.create", "secret:A_TOKEN"),
                ("secret.update", "secret:B_TOKEN"),
                ("secret.value", "secret:B_TOKEN"),
                ("secrets", "s"),
                ("secrets", "s"),
                ("secret.delete", "secret:B_TOKEN"),
            ]
            .map(|(a, t)| (a.to_owned(), t.to_owned()))
        );
    }
}
