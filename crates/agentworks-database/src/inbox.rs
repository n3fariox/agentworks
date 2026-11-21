//! Inbox ledger operations. The server supplies authorization and dispatch decisions.
use crate::{Result, Store};
use agentworks_session::origins::InboxTurn;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const REPLY_WAKE: &str =
    "message_id IN (SELECT reply_id FROM reply_requests WHERE reply_id IS NOT NULL)";
const ELIGIBLE: &str = "d.recipient=?1 AND d.state='queued' AND ((?2 AND d.generation=?3) OR d.message_id IN (SELECT reply_id FROM reply_requests WHERE reply_id IS NOT NULL))";
/// A repository view, usable inside a transaction without exposing SQLite.
pub struct Inbox<'a>(pub(crate) &'a Connection);
pub struct Transaction<'a>(pub(crate) rusqlite::Transaction<'a>);
impl Store {
    pub fn inbox(&self) -> Inbox<'_> {
        Inbox(&self.connection)
    }
    pub fn transaction(&mut self) -> rusqlite::Result<Transaction<'_>> {
        Ok(Transaction(self.connection.transaction()?))
    }
}
impl Transaction<'_> {
    pub fn inbox(&self) -> Inbox<'_> {
        Inbox(&self.0)
    }
    pub fn commit(self) -> rusqlite::Result<()> {
        self.0.commit()
    }
}
impl Inbox<'_> {
    pub fn policy<T: DeserializeOwned>(&self, id: &str) -> Result<Option<T>> {
        let raw: Option<String> = self
            .0
            .query_row(
                "SELECT policy FROM inbox_policies WHERE session_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }
    pub fn save_policy<T: Serialize>(&self, id: &str, policy: &T) -> Result<()> {
        self.0.execute("INSERT INTO inbox_policies(session_id,policy) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET policy=excluded.policy",params![id,serde_json::to_string(policy)?])?;
        Ok(())
    }
    pub fn cancel_non_reply(&self, id: &str, reason: &str) -> rusqlite::Result<usize> {
        self.0.execute(&format!("UPDATE inbox_deliveries SET state='cancelled',reason=?2 WHERE recipient=?1 AND (state='claimed' OR (state='queued' AND NOT {REPLY_WAKE}))"),params![id,reason])
    }
    pub fn set_turn_reason(
        &self,
        id: &str,
        owner: &InboxTurn,
        reason: &str,
    ) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET reason=?4 WHERE recipient=?1 AND request_id=?2 AND generation=?3",params![id,owner.request_id,owner.generation as i64,reason])
    }
    pub fn set_turn_state(
        &self,
        id: &str,
        owner: &InboxTurn,
        state: &str,
        reason: &str,
    ) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state=?4,reason=?5 WHERE recipient=?1 AND request_id=?2 AND generation=?3",params![id,owner.request_id,owner.generation as i64,state,reason])
    }
    pub fn stop(&self, id: &str, reason: &str) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state='cancelled',reason=?2 WHERE recipient=?1 AND state IN ('queued','claimed')",params![id,reason])
    }
    pub fn drop_sender(&self, id: &str, reason: &str) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state='cancelled',reason=?2 WHERE state='queued' AND message_id IN (SELECT id FROM agent_messages WHERE sender=?1)",params![id,reason])
    }
    pub fn acknowledge_delivered(&self, id: &str, owner: &InboxTurn) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE agent_messages SET acknowledged=1 WHERE recipient=?1 AND acknowledged=0 AND length(text)<=256 AND id IN (SELECT message_id FROM inbox_deliveries WHERE recipient=?1 AND request_id=?2 AND generation=?3)",params![id,owner.request_id,owner.generation as i64])
    }
    pub fn submission(&self, id: &str, owner: &InboxTurn, sent: bool) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state=?4,reason=?5 WHERE recipient=?1 AND request_id=?2 AND generation=?3 AND state='claimed' AND reason IS NOT 'Native turn terminated'",params![id,owner.request_id,owner.generation as i64,if sent{"sent"}else{"paused"},if sent{None}else{Some("Native submission uncertain; inspect before reauthorization")}])
    }
    pub fn reply_waiting(&self, id: &str) -> rusqlite::Result<bool> {
        self.0.query_row(&format!("SELECT EXISTS(SELECT 1 FROM inbox_deliveries WHERE recipient=?1 AND state='queued' AND {REPLY_WAKE})"),[id],|r|r.get(0))
    }
    pub fn mail_waiting(&self, id: &str, live: bool, generation: u64) -> rusqlite::Result<bool> {
        self.0.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM inbox_deliveries d WHERE {ELIGIBLE})"),
            params![id, live, generation as i64],
            |r| r.get(0),
        )
    }
    pub fn pending(&self, id: &str) -> rusqlite::Result<i64> {
        self.0.query_row(
            "SELECT count(*) FROM inbox_deliveries WHERE recipient=?1 AND state='queued'",
            [id],
            |r| r.get(0),
        )
    }
    pub fn deliveries(&self, id: &str) -> rusqlite::Result<Vec<Value>> {
        self.0.prepare("SELECT message_id,state,request_id,reason FROM inbox_deliveries WHERE recipient=?1 ORDER BY message_id DESC LIMIT 40")?
            .query_map([id],|r|Ok(json!({"messageId":r.get::<_,i64>(0)?,"state":r.get::<_,String>(1)?,"requestId":r.get::<_,Option<String>>(2)?,"reason":r.get::<_,Option<String>>(3)?})))?.collect()
    }
    pub fn policy_request(&self, id: &str, request: &str) -> rusqlite::Result<Option<String>> {
        self.0
            .query_row(
                "SELECT payload FROM inbox_policy_requests WHERE session_id=?1 AND request_id=?2",
                params![id, request],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn record_policy_request(
        &self,
        id: &str,
        request: &str,
        payload: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.0.execute(
            "INSERT INTO inbox_policy_requests(session_id,request_id,payload) VALUES(?1,?2,?3)",
            params![id, request, payload],
        )?;
        self.0.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES('human','inbox_policy',?1,?2,?3)",params![id,payload,now])?;
        Ok(())
    }
    pub fn pause_interrupted(&self) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state='paused',reason='Restart during claim; inspect native results; this batch will not be replayed' WHERE state='claimed' OR (state='sent' AND reason IS NOT 'Native turn terminated')",[])
    }
    pub fn policies(&self) -> rusqlite::Result<Vec<(String, String)>> {
        self.0
            .prepare("SELECT session_id,policy FROM inbox_policies")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }
    pub fn oldest(
        &self,
        id: &str,
        enabled: bool,
        generation: u64,
    ) -> rusqlite::Result<Option<i64>> {
        self.0.query_row(&format!("SELECT min(m.created) FROM inbox_deliveries d JOIN agent_messages m ON m.id=d.message_id WHERE {ELIGIBLE}"),params![id,enabled,generation as i64],|r|r.get(0))
    }
    pub fn eligible_messages(
        &self,
        id: &str,
        enabled: bool,
        generation: u64,
        batch: u32,
    ) -> rusqlite::Result<Vec<Value>> {
        self.0.prepare(&format!("SELECT m.id,m.sender,m.sender_title,m.text,m.reply_to FROM inbox_deliveries d JOIN agent_messages m ON m.id=d.message_id WHERE {ELIGIBLE} ORDER BY m.id LIMIT ?4"))?.query_map(params![id,enabled,generation as i64,batch],|r|{
            let body=r.get::<_,String>(3)?;
            Ok(json!({"id":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"senderTitle":r.get::<_,String>(2)?.chars().take(80).collect::<String>(),"text":body.chars().take(256).collect::<String>(),"truncated":body.chars().count()>256,"replyTo":r.get::<_,Option<i64>>(4)?}))
        })?.collect()
    }
    pub fn drop_stale(&self, message: Option<i64>) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state='cancelled',reason='Sender can no longer message this session' WHERE message_id=?1 AND state='queued'",[message])
    }
    pub fn claim(
        &self,
        message: Option<i64>,
        request: &str,
        generation: u64,
    ) -> rusqlite::Result<usize> {
        self.0.execute("UPDATE inbox_deliveries SET state='claimed',request_id=?2,generation=?3 WHERE message_id=?1 AND state='queued'",params![message,request,generation as i64])
    }
}
