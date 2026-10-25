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
/// Human-selected defaults for newly created sessions. Service restart is never part of a role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Off,
    #[default]
    Participant,
    Coordinator,
}
impl Role {
    pub fn access(self) -> Access {
        Access {
            communicate: self != Self::Off,
            start_sessions: self == Self::Coordinator,
            prompt_sessions: self == Self::Coordinator,
            cancel_sessions: self == Self::Coordinator,
            run_automations: self == Self::Coordinator,
            restart_service: false,
            manage_review_queue: false,
            archive_sessions: self == Self::Coordinator,
        }
    }
}
// An omitted project field preserves its value; explicit null restores inheritance.
pub fn role_patch<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<Role>>, D::Error> {
    Option::<Role>::deserialize(deserializer).map(Some)
}
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
    #[serde(default)]
    pub restart_service: bool,
    #[serde(default)]
    pub manage_review_queue: bool,
    /// Explicit controller grant; never inherited from a session role.
    #[serde(default)]
    pub archive_sessions: bool,
}
impl Access {
    pub fn enabled(&self) -> bool {
        self.communicate
            || self.start_sessions
            || self.prompt_sessions
            || self.cancel_sessions
            || self.run_automations
            || self.restart_service
            || self.manage_review_queue
            || self.archive_sessions
    }
}
pub use agentworks_session::origins::ControlOrigin;
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
    #[serde(default, deserialize_with = "reply_ref")]
    pub reply_to: Option<i64>,
}
fn reply_ref<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<i64>, D::Error> {
    let v = Value::deserialize(d)?;
    if v.is_null() {
        return Ok(None);
    }
    message_ref(&v)
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("invalid replyTo"))
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
        let mut messages=q.query_map(params![id,after],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"uri":format!("{MESSAGE_URI}{}",r.get::<_,i64>(0)?),"sender":r.get::<_,String>(1)?,"senderUri":actor_uri(&r.get::<_,String>(1)?),"senderTitle":r.get::<_,String>(2)?,"recipient":r.get::<_,String>(3)?,"recipientUri":actor_uri(&r.get::<_,String>(3)?),"recipientTitle":r.get::<_,String>(4)?,"text":r.get::<_,String>(5)?,"replyTo":r.get::<_,Option<i64>>(6)?,"replyToUri":r.get::<_,Option<i64>>(6)?.map(|n|format!("{MESSAGE_URI}{n}")),"created":r.get::<_,i64>(7)?,"acknowledged":r.get::<_,bool>(8)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
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
    app.store
        .lock()
        .unwrap()
        .access(&s.id)
        .map(imply_coordinator_archive)
}
fn imply_coordinator_archive(mut access: Access) -> Access {
    // Coordinator grants already include same-project session management. Let
    // older saved coordinator grants use the same safe archive path too.
    if coordinator(&access) {
        access.archive_sessions = true;
    }
    access
}
pub fn project(s: &Session) -> Option<String> {
    s.data.lock().unwrap().project_id.clone()
}
/// Stable reference agents can copy verbatim into tool calls.
pub const SESSION_URI: &str = "agentworks://sessions/";
pub fn session_uri(id: &str) -> String {
    format!("{SESSION_URI}{id}")
}
/// Session URI, or controller URI for `controller:<id>` actors.
pub fn actor_uri(id: &str) -> String {
    match id.strip_prefix(crate::controllers::ACTOR_PREFIX) {
        Some(c) => format!("{}{c}", crate::controllers::URI),
        None => session_uri(id),
    }
}
/// Authenticated tool caller: a session's harness or a named external controller (C-01).
/// Controllers never borrow a session identity; their IDs carry the `controller:` prefix.
#[derive(Clone)]
pub struct Actor {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    pub session: Option<Arc<Session>>,
}
impl Actor {
    pub fn of(s: &Arc<Session>) -> Self {
        Self {
            id: s.id.clone(),
            title: s.display_title(),
            project: project(s),
            session: Some(s.clone()),
        }
    }
    pub fn controller(c: &crate::controllers::Controller) -> Self {
        Self {
            id: crate::controllers::actor_id(&c.id),
            title: c.name.clone(),
            project: Some(c.project_id.clone()),
            session: None,
        }
    }
}
/// Grants are reread on every call, so revocation applies to existing connections.
pub fn actor_access(app: &App, a: &Actor) -> Result<Access> {
    if a.session.is_some() {
        return app
            .store
            .lock()
            .unwrap()
            .access(&a.id)
            .map(imply_coordinator_archive);
    }
    let id =
        a.id.strip_prefix(crate::controllers::ACTOR_PREFIX)
            .unwrap_or_default();
    Ok(app
        .store
        .lock()
        .unwrap()
        .controller(id)?
        .map(|c| Access {
            restart_service: false,
            ..c.access
        })
        .unwrap_or_default())
}
fn permitted(a: &Access, capability: &str) -> bool {
    match capability {
        "communicate" => a.communicate,
        "start_sessions" => a.start_sessions,
        "prompt_sessions" => a.prompt_sessions,
        "cancel_sessions" => a.cancel_sessions,
        "run_automations" => a.run_automations,
        "restart_service" => a.restart_service,
        "manage_review_queue" => a.manage_review_queue,
        "archive_sessions" => a.archive_sessions,
        _ => false,
    }
}
fn not_granted(capability: &str) -> (StatusCode, String) {
    fail(
        StatusCode::FORBIDDEN,
        format!("Agentworks capability not granted: {capability}"),
    )
}
pub fn require_actor(app: &App, a: &Actor, capability: &str) -> Result<Access> {
    let access = actor_access(app, a)?;
    let allowed = if capability == "archive_sessions" && a.session.is_some() {
        coordinator(&access)
    } else {
        permitted(&access, capability)
    };
    if !allowed {
        return Err(not_granted(capability));
    }
    Ok(access)
}
/// Accepts a bare session ID or an `agentworks://sessions/<id>` URI.
pub fn session_ref(id: &str) -> &str {
    id.strip_prefix(SESSION_URI).unwrap_or(id)
}
pub const MESSAGE_URI: &str = "agentworks://messages/";
/// Accepts a message number or an `agentworks://messages/<n>` URI.
pub fn message_ref(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str()?.strip_prefix(MESSAGE_URI)?.parse().ok())
}
pub fn peer(app: &App, actor: &Session, id: &str) -> Result<Arc<Session>> {
    peer_of(app, &project(actor), id)
}
pub fn peer_of(app: &App, scope: &Option<String>, id: &str) -> Result<Arc<Session>> {
    let target = session(app, session_ref(id))?;
    if scope.is_none() || *scope != project(&target) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Target is outside this session's project",
        ));
    }
    Ok(target)
}
pub fn require(app: &App, s: &Session, capability: &str) -> Result<Access> {
    let a = access(app, s)?;
    if !permitted(&a, capability) {
        return Err(not_granted(capability));
    }
    Ok(a)
}
pub fn coordinator(a: &Access) -> bool {
    a.start_sessions || a.prompt_sessions || a.cancel_sessions || a.run_automations
}
pub fn peers(app: &App, s: &Arc<Session>) -> Result<Value> {
    peers_as(app, &Actor::of(s))
}
pub fn peers_as(app: &App, a: &Actor) -> Result<Value> {
    let grants = actor_access(app, a)?;
    if !grants.enabled() {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "Agentworks discovery is not granted",
        ));
    }
    let coordinator = coordinator(&grants) || (a.session.is_none() && grants.archive_sessions);
    let scope = a.project.clone();
    let candidates: Vec<_> = app.sessions.lock().unwrap().values().cloned().collect();
    let mut values = Vec::new();
    let mut controllers = Vec::new();
    if let Some(project) = scope.as_deref().filter(|_| grants.communicate) {
        // Minimal metadata so sessions can reply to messaging controllers.
        for c in app.store.lock().unwrap().live_controllers(project)? {
            let id = crate::controllers::actor_id(&c.id);
            if c.access.communicate && id != a.id {
                controllers
                    .push(json!({"id":id,"uri":actor_uri(&id),"title":c.name,"kind":"controller"}));
            }
        }
    }
    if scope.is_some() {
        for p in candidates {
            if p.id != a.id && project(&p) == scope && (coordinator || access(app, &p)?.communicate)
            {
                let d = p.data.lock().unwrap();
                values.push(json!({"id":p.id,"uri":session_uri(&p.id),"title":d.lifecycle.title.as_deref().unwrap_or(&p.title),"archived":d.lifecycle.archived,"provider":p.provider,"status":d.status,"hostId":p.host_id}));
            }
        }
    }
    Ok(
        json!({"self":a.id,"selfUri":actor_uri(&a.id),"projectId":scope,"sessions":values,"controllers":controllers}),
    )
}
pub fn send(app: &App, s: &Arc<Session>, req: SendMessage) -> Result<Value> {
    send_as(app, &Actor::of(s), req)
}
pub fn send_as(app: &App, s: &Actor, req: SendMessage) -> Result<Value> {
    require_actor(app, s, "communicate")?;
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
    // Recipient is a same-project session or a live messaging controller.
    let (target_id, target_title, target) =
        if let Some(id) = crate::controllers::reference(&req.recipient) {
            let c = app
                .store
                .lock()
                .unwrap()
                .controller(id)?
                .filter(|c| s.project.as_ref() == Some(&c.project_id))
                .ok_or_else(|| {
                    fail(
                        StatusCode::FORBIDDEN,
                        "Target is outside this session's project",
                    )
                })?;
            if !c.access.communicate {
                return Err(not_granted("communicate"));
            }
            (crate::controllers::actor_id(&c.id), c.name, None)
        } else {
            let target = peer_of(app, &s.project, &req.recipient)?;
            require(app, &target, "communicate")?;
            (target.id.clone(), target.display_title(), Some(target))
        };
    if target_id == s.id {
        return Err(fail(StatusCode::BAD_REQUEST, "Choose another session"));
    }
    let sender_title = s.title.clone();
    let mut store = app.store.lock().unwrap();
    let tx = store.connection.transaction().map_err(db_error)?;
    let previous:Option<(i64,String,String,Option<i64>)>=tx.query_row("SELECT id,recipient,text,reply_to FROM agent_messages WHERE sender=?1 AND request_id=?2",params![s.id,req.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    if let Some((id, recipient, text, reply)) = previous {
        if recipient != target_id || text != req.text || reply != req.reply_to {
            return Err(fail(
                StatusCode::CONFLICT,
                "Request ID already used for a different message",
            ));
        }
        return Ok(json!({"id":id,"uri":format!("{MESSAGE_URI}{id}"),"duplicate":true}));
    }
    if let Some(reply) = req.reply_to {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_messages WHERE id=?1 AND ((sender=?2 AND recipient=?3) OR (sender=?3 AND recipient=?2)))",params![reply,s.id,target_id],|r|r.get(0)).map_err(db_error)?;
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
            [&target_id],
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
    tx.execute("INSERT INTO agent_messages(sender,sender_title,recipient,recipient_title,text,reply_to,created,request_id) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![s.id,sender_title,target_id,target_title,req.text,req.reply_to,now(),req.request_id]).map_err(db_error)?;
    let id = tx.last_insert_rowid();
    tx.execute("INSERT INTO agent_audit(actor,action,target,outcome,created) VALUES (?1,'send_message',?2,'accepted',?3)",params![s.id,target_id,now()]).map_err(db_error)?;
    // Only session peers can feed opt-in wake delivery; controllers need an explicit prompt grant.
    if target.is_some() && s.session.is_some() {
        crate::inbox_delivery::enqueue(&tx, &target_id, id)?;
    }
    tx.commit().map_err(db_error)?;
    drop(store);
    for session in [s.session.as_ref(), target.as_ref()].into_iter().flatten() {
        session.emit("mailbox_updated", json!({"messageId":id}));
    }
    Ok(json!({"id":id,"uri":format!("{MESSAGE_URI}{id}"),"duplicate":false}))
}
pub fn acknowledge(app: &App, recipient: &str, id: i64) -> Result<Value> {
    let count = app
        .store
        .lock()
        .unwrap()
        .connection
        .execute(
            "UPDATE agent_messages SET acknowledged=1 WHERE id=?1 AND recipient=?2",
            params![id, recipient],
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
    let req = imply_coordinator_archive(req);
    if !req.communicate {
        crate::inbox_delivery::revoke_related(&app, &s, "Communication grant revoked")?;
    }
    app.store.lock().unwrap().save_access(&id, &req)?;
    s.emit("agent_access", serde_json::to_value(&req).unwrap());
    // Grants apply immediately. Refresh cached tool catalogs after the active turn ends.
    let idle = matches!(
        s.data.lock().unwrap().status.as_str(),
        "idle" | "suspended" | "error"
    );
    let attached = s.commands.lock().unwrap().is_some();
    if idle
        && crate::service_restart::allow_start(&app).is_ok()
        && !s.data.lock().unwrap().lifecycle.suspended
        && (attached || req.enabled())
    {
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
                        if crate::service_restart::allow_start(&app).is_err() {
                            continue;
                        }
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
    Ok(Json(acknowledge(&app, &s.id, message)?))
}
pub async fn audit_log(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    session(&app, &id)?;
    let store = app.store.lock().unwrap();
    // Controller names are joined in so attributed operations stay readable after revocation.
    let mut q=store.connection.prepare("SELECT a.actor,a.action,a.target,a.outcome,a.created,c.name FROM agent_audit a LEFT JOIN agent_controllers c ON a.actor='controller:'||c.id WHERE a.actor=?1 OR a.target=?1 ORDER BY a.id DESC LIMIT 100").map_err(db_error)?;
    let events=q.query_map([id],|r|Ok(json!({"actor":r.get::<_,String>(0)?,"action":r.get::<_,String>(1)?,"target":r.get::<_,String>(2)?,"outcome":r.get::<_,String>(3)?,"created":r.get::<_,i64>(4)?,"actorTitle":r.get::<_,Option<String>>(5)?}))).map_err(db_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?;
    Ok(Json(json!({"events":events})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_coordinator_grants_imply_archive_but_participants_do_not() {
        let legacy_coordinator = Access {
            communicate: true,
            start_sessions: true,
            prompt_sessions: true,
            cancel_sessions: true,
            run_automations: true,
            ..Default::default()
        };
        assert!(!legacy_coordinator.archive_sessions);
        assert!(imply_coordinator_archive(legacy_coordinator).archive_sessions);
        assert!(!imply_coordinator_archive(Role::Participant.access()).archive_sessions);
        assert!(Role::Coordinator.access().archive_sessions);
    }
}
