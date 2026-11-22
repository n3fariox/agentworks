//! Durable agent mailboxes and explicit, human-managed application capabilities.
use crate::{ApiResult, App, Session, fail, session};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};

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
            archive_sessions: self == Self::Coordinator,
            manage_automations: false,
            ..Default::default()
        }
    }
}
// An omitted project field preserves its value; explicit null restores inheritance.
pub fn role_patch<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<Role>>, D::Error> {
    Option::<Role>::deserialize(deserializer).map(Some)
}
pub use agentworks_database::Access;
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
    /// Wake the sending session once when the recipient first replies to this message.
    #[serde(default)]
    pub expect_reply: bool,
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

fn message_value(m: agentworks_database::mail::Message) -> Value {
    json!({"id":m.id,"uri":format!("{MESSAGE_URI}{}",m.id),"sender":m.sender,"senderUri":actor_uri(&m.sender),"senderTitle":m.sender_title,"recipient":m.recipient,"recipientUri":actor_uri(&m.recipient),"recipientTitle":m.recipient_title,"text":m.text,"replyTo":m.reply_to,"replyToUri":m.reply_to.map(|n|format!("{MESSAGE_URI}{n}")),"created":m.created,"acknowledged":m.acknowledged,"expectReply":m.expect_reply,"replyId":m.reply_id})
}
impl crate::store::Store {
    pub fn access(&self, id: &str) -> Result<Access> {
        self.0.access(id).map_err(db_error)
    }
    pub fn save_access(&mut self, id: &str, access: &Access) -> Result<()> {
        self.0.save_access(id, access, now()).map_err(db_error)
    }
    pub fn mailbox(&self, id: &str, after: i64) -> Result<Value> {
        if after < 0 {
            return Err(fail(StatusCode::BAD_REQUEST, "Invalid message cursor"));
        }
        let mut messages: Vec<Value> = self
            .0
            .mailbox_messages(id, after)
            .map_err(db_error)?
            .into_iter()
            .map(message_value)
            .collect();
        let has_more = messages.len() > 20;
        messages.truncate(20);
        Ok(
            json!({"nextCursor":messages.last().map(|v|v["id"].clone()).unwrap_or(json!(after)),"messages":messages,"hasMore":has_more}),
        )
    }
    pub fn read_message(&self, id: &str, message: i64) -> Result<Value> {
        self.0
            .read_message(id, message)
            .map_err(db_error)?
            .map(message_value)
            .ok_or_else(|| fail(StatusCode::NOT_FOUND, "Message not found in this mailbox"))
    }
    pub fn acknowledge(&self, recipient: &str, message: Option<i64>) -> Result<usize> {
        self.0.acknowledge(recipient, message).map_err(db_error)
    }
    pub fn unacknowledged(&self, id: &str) -> Result<i64> {
        self.0.unacknowledged(id).map_err(db_error)
    }
    pub fn audit(&self, actor: &str, action: &str, target: &str, outcome: &str) -> Result<()> {
        self.0
            .audit(actor, action, target, outcome, now())
            .map_err(db_error)
    }
    pub fn control_start(&self, actor: &str, request: &str) -> Result<Option<String>> {
        self.0.control_start(actor, request).map_err(db_error)
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
        "archive_sessions" => a.archive_sessions,
        "manage_automations" => a.manage_automations,
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
/// Accepts a message number, as a JSON number or a string, or an `agentworks://messages/<n>`
/// URI. Prompts name messages by number while the tool schema types the field as a string,
/// so `"42"` is the common form agents send.
pub fn message_ref(v: &Value) -> Option<i64> {
    let n = match v.as_str() {
        Some(s) => {
            let s = s.trim();
            s.strip_prefix(MESSAGE_URI).unwrap_or(s).parse().ok()?
        }
        None => v.as_i64()?,
    };
    (n > 0).then_some(n)
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
pub fn send(app: &App, s: &Arc<Session>, mut req: SendMessage) -> Result<Value> {
    req.text = crate::secrets::scrub_text(&s.id, &req.text);
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
            // Archived sessions keep their old mail but are not active recipients.
            if target.data.lock().unwrap().lifecycle.archived {
                return Err(fail(StatusCode::CONFLICT, "Recipient session is archived"));
            }
            (target.id.clone(), target.display_title(), Some(target))
        };
    if target_id == s.id {
        return Err(fail(StatusCode::BAD_REQUEST, "Choose another session"));
    }
    let sender_title = s.title.clone();
    let mut store = app.store.lock().unwrap();
    use agentworks_database::mail::{NewMessage, SendOutcome};
    let result = store
        .send_message(
            &NewMessage {
                sender: &s.id,
                sender_title: &sender_title,
                recipient: &target_id,
                recipient_title: &target_title,
                text: &req.text,
                reply_to: req.reply_to,
                request_id: &req.request_id,
                expect_reply: req.expect_reply && s.session.is_some(),
                enqueue: target.is_some() && s.session.is_some(),
                created: now(),
            },
            crate::inbox_delivery::enrollment,
        )
        .map_err(db_error)?;
    let id = match result {
        SendOutcome::Inserted(id) => id,
        SendOutcome::Duplicate(id) => {
            return Ok(json!({"id":id,"uri":format!("{MESSAGE_URI}{id}"),"duplicate":true}));
        }
        SendOutcome::Conflict => {
            return Err(fail(
                StatusCode::CONFLICT,
                "Request ID already used for a different message",
            ));
        }
        SendOutcome::InvalidReply => {
            return Err(fail(
                StatusCode::FORBIDDEN,
                "Reply target is not in this conversation",
            ));
        }
        SendOutcome::InboxFull => {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                "Recipient inbox is full",
            ));
        }
        SendOutcome::RateLimited => {
            return Err(fail(
                StatusCode::TOO_MANY_REQUESTS,
                "Message rate exceeded; retry later",
            ));
        }
    };
    drop(store);
    for session in [s.session.as_ref(), target.as_ref()].into_iter().flatten() {
        mailbox_updated(app, session, id);
    }
    Ok(json!({"id":id,"uri":format!("{MESSAGE_URI}{id}"),"duplicate":false}))
}
/// Carries the session's live unacknowledged count for the browser's inbox indicator.
pub fn mailbox_updated(app: &App, session: &Session, message: i64) {
    let unacknowledged = app
        .store
        .lock()
        .unwrap()
        .unacknowledged(&session.id)
        .unwrap_or(0);
    session.emit(
        "mailbox_updated",
        json!({"messageId":message,"unacknowledged":unacknowledged}),
    );
}
pub fn acknowledge(app: &App, recipient: &str, id: i64) -> Result<Value> {
    let store = app.store.lock().unwrap();
    let known = store.message_recipient(id).map_err(db_error)?.as_deref() == Some(recipient);
    if !known {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "Message not found in this inbox",
        ));
    }
    store.acknowledge(recipient, Some(id))?;
    Ok(json!({"acknowledged":true}))
}
pub async fn get_access(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let s = session(&app, &id)?;
    Ok(Json(serde_json::to_value(access(&app, &s)?).unwrap()))
}
/// Restarts a session's harness so it picks up changed tools, grants or environment:
/// now when idle, otherwise once the current turn ends. A harness that is not running
/// is started only when `start_detached` is set. Callers hold the communication lock.
pub(crate) async fn refresh_harness(
    app: &App,
    s: &std::sync::Arc<crate::Session>,
    start_detached: bool,
) -> Result<()> {
    let idle = matches!(
        s.data.lock().unwrap().status.as_str(),
        "idle" | "suspended" | "error"
    );
    let attached = s.commands.lock().unwrap().is_some();
    if idle
        && crate::service_restart::allow_start(app).is_ok()
        && !{
            let data = s.data.lock().unwrap();
            data.lifecycle.suspended || data.lifecycle.archived
        }
        && (attached || start_detached)
    {
        s.status("suspended");
        let _ = crate::resume(State(app.clone()), Path(s.id.clone())).await?;
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
    Ok(())
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
        crate::inbox_delivery::drop_from_sender(&app, &s, "Sender communication grant revoked")?;
    }
    app.store.lock().unwrap().save_access(&id, &req)?;
    s.emit("agent_access", serde_json::to_value(&req).unwrap());
    // Grants apply immediately. Refresh cached tool catalogs after the active turn ends.
    if let Err((code, error)) = refresh_harness(&app, &s, req.enabled()).await {
        return Err(fail(
            code,
            format!("Access was saved, but tools could not reconnect: {error}"),
        ));
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
    let result = acknowledge(&app, &s.id, message)?;
    mailbox_updated(&app, &s, message);
    Ok(Json(result))
}
/// Human-only: clear a backlog of old messages in one step. Agents acknowledge individually.
pub async fn ack_all(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let _gate = app.communication_lock.lock().await;
    let s = session(&app, &id)?;
    let count = {
        let store = app.store.lock().unwrap();
        let count = store.acknowledge(&s.id, None)?;
        store.audit(
            "human",
            "acknowledge_all",
            &s.id,
            &format!("acknowledged:{count}"),
        )?;
        count
    };
    mailbox_updated(&app, &s, 0);
    Ok(Json(json!({"acknowledged":count})))
}
pub async fn audit_log(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    session(&app, &id)?;
    let store = app.store.lock().unwrap();
    // Controller names are joined in so attributed operations stay readable after revocation.
    let events = store.agent_audit(&id).map_err(db_error)?;
    Ok(Json(json!({"events":events})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_refs_accept_numbers_numeric_strings_and_uris() {
        for v in [
            json!(42),
            json!("42"),
            json!(" 42 "),
            json!("agentworks://messages/42"),
        ] {
            assert_eq!(message_ref(&v), Some(42), "{v}");
        }
        for v in [
            json!(null),
            json!(0),
            json!(-1),
            json!("0"),
            json!(""),
            json!("#42"),
            json!("agentworks://messages/"),
            json!("agentworks://sessions/42"),
            json!(4.2),
        ] {
            assert_eq!(message_ref(&v), None, "{v}");
        }
    }

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
