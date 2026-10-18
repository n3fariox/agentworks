//! Durable agent mailboxes and explicit, human-managed application capabilities.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub type Result<T> = std::result::Result<T, (StatusCode, String)>;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Access {
    #[serde(default)]
    pub communicate: bool,
    #[serde(default)]
    pub start_sessions: bool,
    #[serde(default)]
    pub prompt_sessions: bool,
    #[serde(default)]
    pub cancel_sessions: bool,
    #[serde(default)]
    pub run_automations: bool,
}
impl Access {
    pub fn enabled(&self) -> bool {
        self.communicate
            || self.start_sessions
            || self.prompt_sessions
            || self.cancel_sessions
            || self.run_automations
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ControlOrigin {
    pub actor: String,
    pub request: String,
    #[serde(default)]
    pub payload: Value,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cursor {
    #[serde(default)]
    pub after: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendMessage {
    pub recipient: String,
    pub text: String,
    pub request_id: String,
    pub reply_to: Option<i64>,
}
fn db_error(_: impl std::fmt::Display) -> (StatusCode, String) {
    fail(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Could not save or read agent communication data",
    )
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

impl crate::store::Store {
    pub fn access(&self, id: &str) -> Result<Access> {
        let raw: Option<String> = self
            .connection
            .query_row(
                "SELECT grants FROM agent_access WHERE session_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        raw.map(|v| serde_json::from_str(&v).map_err(db_error))
            .unwrap_or(Ok(Access::default()))
    }
    pub fn save_access(&mut self, id: &str, access: &Access) -> Result<()> {
        let tx = self.connection.transaction().map_err(db_error)?;
        tx.execute("INSERT INTO agent_access(session_id,grants) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET grants=excluded.grants",params![id,serde_json::to_string(access).map_err(db_error)?]).map_err(db_error)?;
        tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES ('human','access',?1,?2,?3)",params![id,serde_json::to_string(access).map_err(db_error)?,now()]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    pub fn mailbox(&self, id: &str, after: i64) -> Result<Value> {
        if after < 0 {
            return Err(fail(StatusCode::BAD_REQUEST, "Invalid message cursor"));
        }
        let mut q=self.connection.prepare("SELECT id,sender,sender_title,recipient,recipient_title,text,reply_to,created,acknowledged FROM agent_messages WHERE (sender=?1 OR recipient=?1) AND id>?2 ORDER BY id LIMIT 21").map_err(db_error)?;
        let mut messages=q.query_map(params![id,after],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"sender":r.get::<_,String>(1)?,"senderTitle":r.get::<_,String>(2)?,"recipient":r.get::<_,String>(3)?,"recipientTitle":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"replyTo":r.get::<_,Option<i64>>(6)?,"created":r.get::<_,i64>(7)?,"acknowledged":r.get::<_,bool>(8)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
        let has_more = messages.len() > 20;
        messages.truncate(20);
        Ok(
            json!({"nextCursor":messages.last().map(|v|v["id"].clone()).unwrap_or(json!(after)),"messages":messages,"hasMore":has_more}),
        )
    }
    pub fn audit(&self, actor: &str, action: &str, target: &str, outcome: &str) -> Result<()> {
        self.connection.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,?2,?3,?4,?5)",params![actor,action,target,outcome,now()]).map_err(db_error)?;
        Ok(())
    }
    pub fn control_start(&self, actor: &str, request: &str) -> Result<Option<String>> {
        let value = self
            .connection
            .query_row(
                "SELECT session_id FROM agent_starts WHERE actor=?1 AND request_id=?2",
                params![actor, request],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(db_error)?;
        Ok(value.map(Option::unwrap_or_default))
    }
}

pub fn access(app: &App, s: &Session) -> Result<Access> {
    app.store.lock().unwrap().access(&s.id)
}
pub fn project(s: &Session) -> Option<String> {
    s.data.lock().unwrap().project_id.clone()
}
pub fn peer(app: &App, actor: &Session, id: &str) -> Result<Arc<Session>> {
    let target = session(app, id)?;
    let scope = project(actor);
    if scope.is_none() || scope != project(&target) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Target is outside this session's project",
        ));
    }
    Ok(target)
}
pub fn require(app: &App, s: &Session, capability: &str) -> Result<Access> {
    let a = access(app, s)?;
    let permitted = match capability {
        "communicate" => a.communicate,
        "start_sessions" => a.start_sessions,
        "prompt_sessions" => a.prompt_sessions,
        "cancel_sessions" => a.cancel_sessions,
        "run_automations" => a.run_automations,
        _ => false,
    };
    if !permitted {
        return Err(fail(
            StatusCode::FORBIDDEN,
            format!("Agentworks capability not granted: {capability}"),
        ));
    }
    Ok(a)
}
pub fn coordinator(a: &Access) -> bool {
    a.start_sessions || a.prompt_sessions || a.cancel_sessions || a.run_automations
}
pub fn peers(app: &App, s: &Session) -> Result<Value> {
    let grants = access(app, s)?;
    if !grants.enabled() {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Agentworks discovery is not granted",
        ));
    }
    let coordinator = coordinator(&grants);
    let scope = project(s);
    let candidates: Vec<_> = app.sessions.lock().unwrap().values().cloned().collect();
    let mut values = Vec::new();
    if scope.is_some() {
        for p in candidates {
            if p.id != s.id && project(&p) == scope && (coordinator || access(app, &p)?.communicate)
            {
                let d = p.data.lock().unwrap();
                values.push(json!({"id":p.id,"title":d.lifecycle.title.as_deref().unwrap_or(&p.title),"archived":d.lifecycle.archived,"provider":p.provider,"status":d.status,"hostId":p.host_id}));
            }
        }
    }
    Ok(json!({"self":s.id,"projectId":scope,"sessions":values}))
}
pub fn send(app: &App, s: &Session, req: SendMessage) -> Result<Value> {
    require(app, s, "communicate")?;
    if req.text.trim().is_empty()
        || req.text.len() > 16384
        || req.request_id.is_empty()
        || req.request_id.len() > 100
    {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "Message must be 1–16384 bytes with a request ID up to 100 bytes",
        ));
    }
    let target = peer(app, s, &req.recipient)?;
    require(app, &target, "communicate")?;
    if target.id == s.id {
        return Err(fail(StatusCode::BAD_REQUEST, "Choose another session"));
    }
    let sender_title = s.display_title();
    let target_title = target.display_title();
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction().map_err(db_error)?;
    let previous:Option<(i64,String,String,Option<i64>)>=tx.query_row("SELECT id,recipient,text,reply_to FROM agent_messages WHERE sender=?1 AND request_id=?2",params![s.id,req.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    if let Some((id, recipient, text, reply)) = previous {
        if recipient != req.recipient || text != req.text || reply != req.reply_to {
            return Err(fail(
                StatusCode::CONFLICT,
                "Request ID already used for a different message",
            ));
        }
        return Ok(json!({"id":id,"duplicate":true}));
    }
    if let Some(reply) = req.reply_to {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_messages WHERE id=?1 AND ((sender=?2 AND recipient=?3) OR (sender=?3 AND recipient=?2)))",params![reply,s.id,target.id],|r|r.get(0)).map_err(db_error)?;
        if !valid {
            return Err(fail(
                StatusCode::FORBIDDEN,
                "Reply target is not in this conversation",
            ));
        }
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM agent_messages WHERE recipient=?1 AND acknowledged=0",
            [&target.id],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if count >= 1000 {
        return Err(fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Recipient inbox is full",
        ));
    }
    let recent: i64 = tx
        .query_row(
            "SELECT count(*) FROM agent_messages WHERE sender=?1 AND created>?2",
            params![s.id, now().saturating_sub(60000)],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if recent >= 60 {
        return Err(fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Message rate exceeded; retry later",
        ));
    }
    tx.execute("INSERT INTO agent_messages(sender,sender_title,recipient,recipient_title,text,reply_to,created,request_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![s.id,sender_title,target.id,target_title,req.text,req.reply_to,now(),req.request_id]).map_err(db_error)?;
    let id = tx.last_insert_rowid();
    tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,'send_message',?2,'accepted',?3)",params![s.id,target.id,now()]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    drop(store);
    s.emit("mailbox_updated", json!({"messageId":id}));
    target.emit("mailbox_updated", json!({"messageId":id}));
    Ok(json!({"id":id,"duplicate":false}))
}
pub fn acknowledge(app: &App, s: &Session, id: i64) -> Result<Value> {
    let count = app
        .store
        .lock()
        .unwrap()
        .connection
        .execute(
            "UPDATE agent_messages SET acknowledged=1 WHERE id=?1 AND recipient=?2",
            params![id, s.id],
        )
        .map_err(db_error)?;
    if count == 0 {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "Message not found in this inbox",
        ));
    }
    Ok(json!({"acknowledged":true}))
}
pub async fn get_access(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    Ok(Json(serde_json::to_value(access(&app, &s)?).unwrap()))
}
pub async fn set_access(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<Access>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let s = session(&app, &id)?;
    app.store.lock().unwrap().save_access(&id, &req)?;
    s.emit("agent_access", serde_json::to_value(&req).unwrap());
    // Grants apply immediately. Refresh cached tool catalogs after the active turn ends.
    let idle = matches!(
        s.data.lock().unwrap().status.as_str(),
        "idle" | "suspended" | "error"
    );
    let attached = s.commands.lock().unwrap().is_some();
    if idle && !s.data.lock().unwrap().lifecycle.suspended && (attached || req.enabled()) {
        s.status("suspended");
        if let Err((code, error)) = crate::resume(State(app.clone()), Path(id.clone())).await {
            return Err(fail(
                code,
                format!("Access was saved, but tools could not reconnect: {error}"),
            ));
        }
    } else if !idle
        && !s
            .mcp_refresh_pending
            .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        let app = app.clone();
        let s = s.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _gate = app.communication_lock.lock().await;
                if !app.sessions.lock().unwrap().contains_key(&s.id) {
                    break;
                }
                let status = s.data.lock().unwrap().status.clone();
                if matches!(status.as_str(), "idle" | "suspended" | "error" | "closed") {
                    if status == "idle" {
                        s.status("suspended");
                        let _ = crate::resume(State(app.clone()), Path(s.id.clone())).await;
                    }
                    break;
                }
            }
            s.mcp_refresh_pending
                .store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    Ok(Json(serde_json::to_value(req).unwrap()))
}
pub async fn get_peers(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    Ok(Json(peers(&app, &s)?))
}
pub async fn get_mailbox(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<Cursor>,
) -> ApiResult {
    session(&app, &id)?;
    Ok(Json(app.store.lock().unwrap().mailbox(&id, q.after)?))
}
pub async fn post_message(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(req): Json<SendMessage>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let s = session(&app, &id)?;
    Ok(Json(send(&app, &s, req)?))
}
pub async fn ack_message(
    State(app): State<App>,
    Path((id, message)): Path<(String, i64)>,
) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let s = session(&app, &id)?;
    Ok(Json(acknowledge(&app, &s, message)?))
}
pub async fn audit_log(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    session(&app, &id)?;
    let store = app.store.lock().unwrap();
    let mut q=store.connection.prepare("SELECT actor,action,target,outcome,created FROM agent_audit WHERE actor=?1 OR target=?1 ORDER BY id DESC LIMIT 100").map_err(db_error)?;
    let events=q.query_map([id],|r|Ok(json!({"actor":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"created":r.get::<_,i64>(4)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    Ok(Json(json!({"events":events})))
}
