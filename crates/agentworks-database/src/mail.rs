use crate::{Access, Result, Store};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct Message {
    pub id: i64,
    pub sender: String,
    pub sender_title: String,
    pub recipient: String,
    pub recipient_title: String,
    pub text: String,
    pub reply_to: Option<i64>,
    pub created: i64,
    pub acknowledged: bool,
    pub expect_reply: bool,
    pub reply_id: Option<i64>,
}
const MESSAGE_COLUMNS: &str = "SELECT m.id,m.sender,m.sender_title,m.recipient,m.recipient_title,m.text,m.reply_to,m.created,m.acknowledged,r.message_id IS NOT NULL,r.reply_id FROM agent_messages m LEFT JOIN reply_requests r ON r.message_id=m.id";
fn message_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        sender: r.get(1)?,
        sender_title: r.get(2)?,
        recipient: r.get(3)?,
        recipient_title: r.get(4)?,
        text: r.get(5)?,
        reply_to: r.get(6)?,
        created: r.get(7)?,
        acknowledged: r.get(8)?,
        expect_reply: r.get(9)?,
        reply_id: r.get(10)?,
    })
}
pub struct NewMessage<'a> {
    pub sender: &'a str,
    pub sender_title: &'a str,
    pub recipient: &'a str,
    pub recipient_title: &'a str,
    pub text: &'a str,
    pub reply_to: Option<i64>,
    pub request_id: &'a str,
    pub expect_reply: bool,
    pub enqueue: bool,
    pub created: i64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Inserted(i64),
    Duplicate(i64),
    Conflict,
    InvalidReply,
    InboxFull,
    RateLimited,
}
/// Policy is chosen by the caller while the message transaction is open.
pub enum DeliveryDecision {
    Ignore,
    Queue { generation: u64 },
    Cancel { generation: u64, reason: String },
}
impl Store {
    pub fn access(&self, id: &str) -> Result<Access> {
        let raw: Option<String> = self
            .connection
            .query_row(
                "SELECT grants FROM agent_access WHERE session_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|s| serde_json::from_str(&s))
            .transpose()
            .map(|a| a.unwrap_or_default())
            .map_err(Into::into)
    }
    pub fn save_access(&mut self, id: &str, access: &Access, now: i64) -> Result<()> {
        let tx = self.connection.transaction()?;
        let grants = serde_json::to_string(access)?;
        tx.execute("INSERT INTO agent_access(session_id,grants) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET grants=excluded.grants", params![id, grants])?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','access',?1,?2,?3)",params![id,grants,now])?;
        tx.commit()?;
        Ok(())
    }
    pub fn mailbox_messages(&self, id: &str, after: i64) -> rusqlite::Result<Vec<Message>> {
        self.connection.prepare(&format!("{MESSAGE_COLUMNS} WHERE (m.sender=?1 OR m.recipient=?1) AND m.id>?2 ORDER BY m.id LIMIT 21"))?
            .query_map(params![id,after],message_row)?.collect()
    }
    pub fn read_message(&self, actor: &str, message: i64) -> Result<Option<Message>> {
        let mut found = self
            .connection
            .query_row(
                &format!("{MESSAGE_COLUMNS} WHERE m.id=?1 AND (m.sender=?2 OR m.recipient=?2)"),
                params![message, actor],
                message_row,
            )
            .optional()?;
        if let Some(m) = &mut found
            && m.recipient == actor
            && !m.acknowledged
        {
            self.acknowledge(actor, Some(message))?;
            m.acknowledged = true;
        }
        Ok(found)
    }
    pub fn acknowledge(&self, recipient: &str, message: Option<i64>) -> rusqlite::Result<usize> {
        let tx = self.connection.unchecked_transaction()?;
        let count=tx.execute("UPDATE agent_messages SET acknowledged=1 WHERE recipient=?1 AND acknowledged=0 AND (?2 IS NULL OR id=?2)",params![recipient,message])?;
        tx.execute("UPDATE inbox_deliveries SET state='cancelled',reason='Acknowledged before delivery' WHERE recipient=?1 AND state='queued' AND message_id IN (SELECT id FROM agent_messages WHERE recipient=?1 AND acknowledged=1 AND (?2 IS NULL OR id=?2))",params![recipient,message])?;
        tx.commit()?;
        Ok(count)
    }
    pub fn unacknowledged(&self, id: &str) -> rusqlite::Result<i64> {
        self.connection.query_row(
            "SELECT count(*) FROM agent_messages WHERE recipient=?1 AND acknowledged=0",
            [id],
            |r| r.get(0),
        )
    }
    pub fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        outcome: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,?2,?3,?4,?5)",
            params![actor, action, target, outcome, now],
        )?;
        Ok(())
    }
    pub fn control_start(&self, actor: &str, request: &str) -> rusqlite::Result<Option<String>> {
        let row = self
            .connection
            .query_row(
                "SELECT session_id FROM agent_starts WHERE actor=?1 AND request_id=?2",
                params![actor, request],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(row.map(Option::unwrap_or_default))
    }
    /// Deduplication, limits, audit, reply binding and optional wake enrollment
    /// commit together. The callback evaluates application delivery policy;
    /// it cannot access SQL or partially commit this operation.
    pub fn send_message(
        &mut self,
        m: &NewMessage<'_>,
        decide: impl FnOnce(Option<&str>, bool, i64) -> Result<DeliveryDecision>,
    ) -> Result<SendOutcome> {
        let tx = self.connection.transaction()?;
        let previous:Option<(i64,String,String,Option<i64>,bool)>=tx.query_row("SELECT id,recipient,text,reply_to,EXISTS(SELECT 1 FROM reply_requests WHERE message_id=agent_messages.id) FROM agent_messages WHERE sender=?1 AND request_id=?2",params![m.sender,m.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        if let Some((id, recipient, text, reply, expect)) = previous {
            return Ok(
                if recipient == m.recipient
                    && text == m.text
                    && reply == m.reply_to
                    && expect == m.expect_reply
                {
                    SendOutcome::Duplicate(id)
                } else {
                    SendOutcome::Conflict
                },
            );
        }
        if let Some(reply) = m.reply_to {
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_messages WHERE id=?1 AND ((sender=?2 AND recipient=?3) OR (sender=?3 AND recipient=?2)))",params![reply,m.sender,m.recipient],|r|r.get(0))?;
            if !valid {
                return Ok(SendOutcome::InvalidReply);
            }
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM agent_messages WHERE recipient=?1 AND acknowledged=0",
            [m.recipient],
            |r| r.get(0),
        )?;
        if count >= 1000 {
            return Ok(SendOutcome::InboxFull);
        }
        let recent: i64 = tx.query_row(
            "SELECT count(*) FROM agent_messages WHERE sender=?1 AND created>?2",
            params![m.sender, m.created.saturating_sub(60000)],
            |r| r.get(0),
        )?;
        if recent >= 60 {
            return Ok(SendOutcome::RateLimited);
        }
        tx.execute("INSERT INTO agent_messages(sender,sender_title,recipient,recipient_title,text,reply_to,created,request_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![m.sender,m.sender_title,m.recipient,m.recipient_title,m.text,m.reply_to,m.created,m.request_id])?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,'send_message',?2,'accepted',?3)",params![m.sender,m.recipient,m.created])?;
        if m.expect_reply {
            tx.execute(
                "INSERT INTO reply_requests(message_id,requester) VALUES(?1,?2)",
                params![id, m.sender],
            )?;
        }
        if m.enqueue {
            let reply = if let Some(request) = m.reply_to {
                tx.execute("UPDATE reply_requests SET reply_id=?3 WHERE message_id=?1 AND requester=?2 AND reply_id IS NULL",params![request,m.recipient,id])?>0
            } else {
                false
            };
            let policy: Option<String> = tx
                .query_row(
                    "SELECT policy FROM inbox_policies WHERE session_id=?1",
                    [m.recipient],
                    |r| r.get(0),
                )
                .optional()?;
            let pending: i64 = tx.query_row(
                "SELECT count(*) FROM inbox_deliveries WHERE recipient=?1 AND state='queued'",
                [m.recipient],
                |r| r.get(0),
            )?;
            let decision = decide(policy.as_deref(), reply, pending)?;
            let row = match decision {
                DeliveryDecision::Ignore => None,
                DeliveryDecision::Queue { generation } => Some((generation, "queued", None)),
                DeliveryDecision::Cancel { generation, reason } => {
                    Some((generation, "cancelled", Some(reason)))
                }
            };
            if let Some((generation, state, reason)) = row {
                tx.execute("INSERT INTO inbox_deliveries(message_id,recipient,generation,state,reason) VALUES(?1,?2,?3,?4,?5)",params![id,m.recipient,generation as i64,state,reason])?;
            }
        }
        tx.commit()?;
        Ok(SendOutcome::Inserted(id))
    }
    pub fn latest_completion(&self, id: &str, scope: Option<&str>) -> Result<Option<Value>> {
        let raw:Option<String>=self.connection.query_row("SELECT report FROM agent_completions WHERE author=?1 AND (?2 IS NULL OR project_id=?2) ORDER BY id DESC LIMIT 1",params![id,scope],|r|r.get(0)).optional()?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }
}

pub enum CompletionOutcome {
    Saved(Value),
    Duplicate(Value),
    Conflict,
}
impl Store {
    pub fn report_completion(
        &mut self,
        author: &str,
        title: &str,
        project: Option<&str>,
        request: &str,
        payload: &str,
        mut report: Value,
        created: i64,
    ) -> Result<CompletionOutcome> {
        let tx = self.connection.transaction()?;
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT payload,report FROM agent_completions WHERE author=?1 AND request_id=?2",
                params![author, request],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old, saved)) = prior {
            return Ok(if old == payload {
                CompletionOutcome::Duplicate(serde_json::from_str(&saved)?)
            } else {
                CompletionOutcome::Conflict
            });
        }
        tx.execute("INSERT INTO agent_completions(author,author_title,project_id,request_id,payload,report,created) VALUES (?1,?2,?3,?4,?5,'{}',?6)",params![author,title,project,request,payload,created])?;
        let id = tx.last_insert_rowid();
        let obj = report
            .as_object_mut()
            .ok_or("Completion report must be an object")?;
        obj.insert("id".into(), json!(id));
        obj.insert("author".into(), json!(author));
        obj.insert("authorTitle".into(), json!(title));
        obj.insert("projectId".into(), json!(project));
        obj.insert("created".into(), json!(created));
        tx.execute(
            "UPDATE agent_completions SET report=?2 WHERE id=?1",
            params![id, report.to_string()],
        )?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,'completion_reported',?1,?2,?3)",params![author,format!("report:{id}"),created])?;
        tx.commit()?;
        Ok(CompletionOutcome::Saved(report))
    }
    pub fn message_recipient(&self, id: i64) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT recipient FROM agent_messages WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn agent_audit(&self, id: &str) -> rusqlite::Result<Vec<Value>> {
        self.connection.prepare("SELECT a.actor,a.action,a.target,a.outcome,a.created,c.name FROM agent_audit a LEFT JOIN agent_controllers c ON a.actor='controller:'||c.id WHERE a.actor=?1 OR a.target=?1 ORDER BY a.id DESC LIMIT 100")?
            .query_map([id],|r|Ok(json!({"actor":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"created":r.get::<_,i64>(4)?,"actorTitle":r.get::<_,Option<String>>(5)?})))?.collect()
    }
}
