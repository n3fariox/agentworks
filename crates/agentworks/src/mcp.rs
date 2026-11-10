//! Agentworks MCP bridge: authorization and durable state behind the stdio relay in
//! `agentworks_worker::mcp_proxy`.
use crate::{App, Session, communication as mail, communication::Actor};
use agent_client_protocol::schema::v1::{EnvVariable, McpServer, McpServerStdio};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

pub fn configuration(s: &Session) -> Vec<McpServer> {
    let (command, endpoint) = if s.host_id == "local" {
        (
            std::env::current_exe().expect("executable path"),
            s.mcp_endpoint.to_string(),
        )
    } else {
        let Some(live) = crate::hosts::registry().and_then(|hosts| hosts.live(&s.host_id)) else {
            return Vec::new();
        };
        let relay = &live.worker()["mcpRelay"];
        let (Some(command), Some(endpoint)) =
            (relay["command"].as_str(), relay["endpoint"].as_str())
        else {
            return Vec::new();
        };
        let Ok(address) = endpoint.parse::<SocketAddr>() else {
            return Vec::new();
        };
        if !address.ip().is_loopback() || !std::path::Path::new(command).is_absolute() {
            return Vec::new();
        }
        (command.into(), endpoint.to_owned())
    };
    let server = McpServerStdio::new("agentworks", command)
        .args(vec!["mcp".into()])
        .env(vec![
            EnvVariable::new("AGENTWORKS_MCP_ENDPOINT", endpoint),
            EnvVariable::new("AGENTWORKS_MCP_TOKEN", s.mcp_token.lock().unwrap().clone()),
        ]);
    vec![McpServer::Stdio(server)]
}
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}
/// Session-only tools (self-authored completion reports, service restart) are never listed
/// for external controllers, regardless of stored grants.
fn tools(a: &mail::Access, session: bool) -> Value {
    let mut tools = Vec::new();
    if a.enabled() {
        tools.push(tool("list_sessions","List participating peers and their status in this project. Each has an id and an agentworks://sessions/<id> uri; tools accept either. Coordinators and archive controllers also see project sessions without messaging access.",json!({}),&[]));
    }
    if a.communicate {
        tools.push(tool("read_inbox","Read received and sent messages after a cursor. Messages and sessions carry agentworks:// uris that tools accept verbatim. Listing does not acknowledge or run a turn; use read_message to read one in full.",json!({"after":{"type":"integer","minimum":0}}),&[]));
        tools.push(tool("read_message","Read one message in full by number or agentworks://messages/<n> URI. Reading a message you received acknowledges it.",json!({"messageId":{"type":"string","description":"Message number or agentworks://messages/<n> URI, passed exactly as given in read_inbox or a prompt."}}),&["messageId"]));
        tools.push(tool("send_message","Persist a message to a participating project peer. Does not wake or interrupt it. Set expectReply to be woken by its reply. Reuse requestId only for the same message.",json!({"recipient":{"type":"string","description":"Session ID or agentworks://sessions/<id> URI, passed exactly as given in list_sessions or a prompt."},"text":{"type":"string","maxLength":16384},"requestId":{"type":"string","maxLength":100},"replyTo":{"type":"string","description":"Message number or agentworks://messages/<n> URI, passed exactly as given in read_inbox or a prompt."},"expectReply":{"type":"boolean","description":"Wake you with the recipient's first reply (a message with replyTo set to this one), even if you are idle. Use for requests you are waiting on, such as reviews."}}),&["recipient","text","requestId"]));
        tools.push(tool("acknowledge_message","Acknowledge receipt of a message in your own inbox; does not indicate task completion.",json!({"messageId":{"type":"string","description":"Message number or agentworks://messages/<n> URI, passed exactly as given in read_inbox or a prompt."}}),&["messageId"]));
    }
    if a.communicate && session {
        tools.push(tool("report_completion", "Persist your own completion claim. Does not review, merge, test, or change turn status. Reuse requestId only for identical payloads.", json!({"status":{"type":"string","enum":["completed","blocked"]},"summary":{"type":"string","maxLength":4096},"branch":{"type":"string","maxLength":256},"commit":{"type":"string","pattern":"^([0-9a-fA-F]{40}|[0-9a-fA-F]{64})$"},"checks":{"type":"array","maxItems":32,"items":{"type":"object","properties":{"name":{"type":"string","maxLength":256},"result":{"type":"string","maxLength":2048}},"required":["name","result"],"additionalProperties":false}},"requestId":{"type":"string","maxLength":100}}), &["status","summary","requestId"]));
    }
    if a.communicate || mail::coordinator(a) {
        tools.push(tool("get_completion", "Read the latest agent claim for yourself or a permitted same-project session. Claims are not proof of review or success.", json!({"sessionId":{"type":"string","description":"Session ID or agentworks://sessions/<id> URI, passed exactly as given in list_sessions or a prompt."}}), &[]));
    }
    let task_id = json!({"oneOf":[{"type":"integer","minimum":1},{"type":"string","pattern":"^agentworks://projects/[^/]+/tasks/[1-9][0-9]*$"}]});
    if a.communicate || mail::coordinator(a) {
        tools.push(tool(
            "list_tasks",
            "List open tickets in your current project, with display keys and stable URIs. Set includeDone to also read completed tickets.",
            json!({"includeDone":{"type":"boolean","default":false}}),
            &[],
        ));
        tools.push(tool(
            "get_task",
            "Read a project ticket. Copy its numeric id or canonical URI, not its display key.",
            json!({"taskId":task_id}),
            &["taskId"],
        ));
    }
    if session && mail::coordinator(a) {
        tools.push(tool("create_task", "Create a project ticket with a Markdown body. Coordinator communication access required. Reuse requestId only for identical input.", json!({"title":{"type":"string","maxLength":160},"body":{"type":"string","maxLength":16384},"status":{"type":"string","enum":["open","done"]},"requestId":{"type":"string","maxLength":100}}), &["title","requestId"]));
        tools.push(tool("update_task", "Edit a ticket using its current revision. Markdown body. Refresh after a revision conflict; reuse requestId only for identical input.", json!({"taskId":task_id,"revision":{"type":"integer","minimum":1},"title":{"type":"string","maxLength":160},"body":{"type":"string","maxLength":16384},"status":{"type":"string","enum":["open","done"]},"requestId":{"type":"string","maxLength":100}}), &["taskId","revision","requestId"]));
        tools.push(tool("delete_task", "Permanently delete a project ticket using its current revision. Reuse requestId only for identical input.", json!({"taskId":task_id,"revision":{"type":"integer","minimum":1},"requestId":{"type":"string","maxLength":100}}), &["taskId","revision","requestId"]));
    }
    if a.restart_service && session {
        tools.push(tool("get_service_status", "Read global service activity and durable restart/handoff outcomes. Requires live restartService grant.",json!({}), &[]));
        tools.push(tool(
            "cancel_service_restart",
            "Cancel your own pending global restart or native handoff.",
            json!({"requestId":{"type":"string"}}),
            &["requestId"],
        ));
        tools.push(tool("restart_service", "GLOBAL service restart. Explicit human restartService grant required. Returns durable identity immediately. wait drains all sessions; interrupt stops all harnesses. Optional handoff resumes only your saved native context once, after finishing this turn.", json!({"requestId":{"type":"string"},"mode":{"type":"string","enum":["wait","interrupt"]},"expiresInSeconds":{"type":"integer","minimum":5,"maximum":900},"release":{"type":"string"},"handoff":{"type":"boolean"},"handoffDeadline":{"type":"integer"}}), &["requestId","mode","expiresInSeconds"]));
    }
    if a.start_sessions {
        tools.push(tool("start_session","Start a session using the project's configured communication role; no grants are copied from the caller. Does not submit a prompt. Reuse requestId on retry.",json!({"provider":{"type":"string","enum":agentworks_runtime::remote::PROVIDERS},"cwd":{"type":"string"},"inheritResumePolicy":{"type":"boolean","default":false},"title":{"type":"string"},"model":{"type":"string"},"modelProvider":{"type":"string"},"requestId":{"type":"string","maxLength":50},"workspace":{"type":"object","properties":{"mode":{"type":"string","enum":["directory","newWorktree","existingWorktree"]},"branch":{"type":"string"},"baseRef":{"type":"string"},"path":{"type":"string"}},"required":["mode"],"additionalProperties":false}}),&["provider","cwd","requestId"]));
    }
    if a.prompt_sessions {
        tools.push(tool("prompt_session","Submit a new turn to an idle session in this project; starts model work. A sleeping session is woken and asks you to retry shortly. Reuse requestId on retry.",json!({"sessionId":{"type":"string","description":"Session ID or agentworks://sessions/<id> URI, passed exactly as given in list_sessions or a prompt."},"text":{"type":"string"},"requestId":{"type":"string","maxLength":50}}),&["sessionId","text","requestId"]));
    }
    if (session && mail::coordinator(a)) || (!session && a.archive_sessions) {
        tools.push(tool("archive_session", "Archive a session in this controller’s project. Stops its terminal. Default worktree cleanup removes only safe, clean owned worktrees and retains dirty, busy or unsafe worktrees. Does not merge or delete branches. No unarchive or cleanup override is available.", json!({"sessionId":{"type":"string","minLength":1,"description":"Session ID or agentworks://sessions/<id> URI."}}), &["sessionId"]));
    }
    if a.cancel_sessions {
        tools.push(tool(
            "cancel_session",
            "Request cancellation of a running turn in this project.",
            json!({"sessionId":{"type":"string","description":"Session ID or agentworks://sessions/<id> URI, passed exactly as given in list_sessions or a prompt."}}),
            &["sessionId"],
        ));
    }
    if a.run_automations || a.manage_automations {
        tools.push(tool(
            "list_automations",
            "List existing automation names and IDs in this project. With manageAutomations, also triggers, next runs, revisions and agent authors.",
            json!({}),
            &[],
        ));
        let target = json!({"automationId":{"type":"string"}});
        tools.push(tool(
            "get_automation",
            "Read one project automation's full definition, revision and next run.",
            target.clone(),
            &["automationId"],
        ));
        tools.push(tool("list_automation_runs", "Read up to 100 recent runs and scheduled occurrences (started, missed or skipped, with reasons) of a project automation.", target, &["automationId"]));
    }
    if a.run_automations {
        tools.push(tool("run_automation","Run an existing automation in this project. Reuse requestId on retry. No grant is inherited by the run.",json!({"automationId":{"type":"string"},"requestId":{"type":"string","maxLength":50}}),&["automationId","requestId"]));
    }
    if a.manage_automations {
        let workflow = json!({"type":"object","description":"Recipe: {version:1, trigger:{kind:'manual'} or {kind:'schedule',timeZone:'Europe/Berlin',at:'09:30',days:[1..7 ISO weekdays],missed:'skip'|'runOnce'} or {kind:'once',timeZone,at:'YYYY-MM-DDTHH:MM' local, 1 minute to 1 year ahead,missed,afterRun:'complete' (pause, keep) |'delete'} or {kind:'script',script:'github_review.star',handler:'default',parameters:{host:'github.com',repo:'owner/repo',account:'login'}} (requires an enabled main-host account; unattended dispatch also requires promptSessions; ambiguous session matches are skipped), steps:[{kind:'agent',text}] (1-20, run in order in one turn)}. See docs/SCHEDULED-AUTOMATIONS.md."});
        let fields = |update: bool| {
            let mut p = json!({"requestId":{"type":"string","maxLength":100},"name":{"type":"string","maxLength":80},"provider":{"type":"string","enum":agentworks_runtime::remote::PROVIDERS},"model":{"type":"string"},"modelProvider":{"type":"string"},"cwd":{"type":"string","description":"Run directory on your host; must be inside its allowed roots."},"enabled":{"type":"boolean","default":true},"prompt":{"type":"string","description":"Single instruction; use workflow for steps or a schedule."},"workflow":workflow.clone()});
            if update {
                p["automationId"] = json!({"type":"string"});
                p["revision"] = json!({"type":"integer","description":"Revision from get_automation; a stale revision is rejected."});
            }
            p
        };
        tools.push(tool("create_automation", "Create an automation in this project on your host. Requires manageAutomations and startSessions. Scheduled runs act on your authority and are skipped if you lose those grants or leave the project. Runs inherit no grants. Reuse requestId only for an identical retry.", fields(false), &["requestId","name","provider","cwd"]));
        tools.push(tool("update_automation", "Replace an agent-created automation's definition. Pass the current revision. Reuse requestId only for an identical retry.", fields(true), &["requestId","automationId","revision","name","provider","cwd"]));
        tools.push(tool("set_automation_enabled", "Pause (enabled=false) any project automation, or resume an agent-created one. Resuming never runs occurrences missed while paused.", json!({"requestId":{"type":"string","maxLength":100},"automationId":{"type":"string"},"enabled":{"type":"boolean"}}), &["requestId","automationId","enabled"]));
        tools.push(tool(
            "delete_automation",
            "Delete an agent-created automation. Run history is kept.",
            json!({"requestId":{"type":"string","maxLength":100},"automationId":{"type":"string"}}),
            &["requestId", "automationId"],
        ));
    }
    json!({"tools":tools})
}
fn field(v: &Value, name: &str) -> mail::Result<String> {
    v[name]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| crate::fail(StatusCode::BAD_REQUEST, format!("Missing {name}")))
}
fn request_id(v: &Value) -> mail::Result<String> {
    let id = field(v, "requestId")?;
    if id.len() > 50 {
        return Err(crate::fail(
            StatusCode::BAD_REQUEST,
            "requestId must be at most 50 bytes",
        ));
    }
    Ok(id)
}
fn session_only(a: &Actor) -> mail::Result<&std::sync::Arc<Session>> {
    a.session.as_ref().ok_or_else(|| {
        crate::fail(
            StatusCode::FORBIDDEN,
            "Only Agentworks sessions can use this operation; controllers are not sessions",
        )
    })
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveSession {
    session_id: String,
}

async fn dispatch(app: &App, a: &Actor, operation: &str, args: Value) -> mail::Result<Value> {
    let grants = mail::actor_access(app, a)?;
    match operation {
        "tools/list" => Ok(tools(&grants, a.session.is_some())),
        "list_tasks" | "get_task" | "create_task" | "update_task" | "delete_task" => {
            crate::tasks::agent(app, a, args, operation)
        }
        "get_service_status" => {
            mail::require(app, session_only(a)?, "restart_service")?;
            crate::service_restart::status(State(app.clone()))
                .await
                .map(|j| j.0)
        }
        "cancel_service_restart" => {
            let s = session_only(a)?;
            mail::require(app, s, "restart_service")?;
            crate::service_restart::cancel_own(app, &s.id, &field(&args, "requestId")?).await
        }
        "restart_service" => {
            let s = session_only(a)?;
            mail::require(app, s, "restart_service")?;
            let req = serde_json::from_value(args)
                .map_err(|_| crate::fail(StatusCode::BAD_REQUEST, "Invalid restart request"))?;
            crate::service_restart::request(app, &s.id, req).await
        }
        "list_sessions" => mail::peers_as(app, a),
        "read_inbox" => {
            mail::require_actor(app, a, "communicate")?;
            let after = args
                .get("after")
                .map(|v| {
                    v.as_i64()
                        .ok_or_else(|| crate::fail(StatusCode::BAD_REQUEST, "Invalid cursor"))
                })
                .transpose()?
                .unwrap_or(0);
            app.store.lock().unwrap().mailbox(&a.id, after)
        }
        "report_completion" => {
            let req = serde_json::from_value(args).map_err(|_| {
                crate::fail(StatusCode::BAD_REQUEST, "Invalid completion report fields")
            })?;
            crate::completion::report(app, session_only(a)?, req)
        }
        "get_completion" => {
            let req = serde_json::from_value(args).map_err(|_| {
                crate::fail(StatusCode::BAD_REQUEST, "Invalid completion query fields")
            })?;
            crate::completion::get(app, a, req)
        }
        "send_message" => {
            let req = serde_json::from_value(args)
                .map_err(|_| crate::fail(StatusCode::BAD_REQUEST, "Invalid message fields"))?;
            mail::send_as(app, a, req)
        }
        "acknowledge_message" => {
            mail::require_actor(app, a, "communicate")?;
            let id = mail::message_ref(&args["messageId"])
                .ok_or_else(|| crate::fail(StatusCode::BAD_REQUEST, "Missing messageId"))?;
            let result = mail::acknowledge(app, &a.id, id)?;
            if let Some(s) = &a.session {
                mail::mailbox_updated(app, s, id);
            }
            Ok(result)
        }
        "read_message" => {
            mail::require_actor(app, a, "communicate")?;
            let id = mail::message_ref(&args["messageId"])
                .ok_or_else(|| crate::fail(StatusCode::BAD_REQUEST, "Missing messageId"))?;
            let message = app.store.lock().unwrap().read_message(&a.id, id)?;
            if let Some(s) = &a.session {
                mail::mailbox_updated(app, s, id);
            }
            Ok(message)
        }
        "start_session" => {
            mail::require_actor(app, a, "start_sessions")?;
            let scope = a.project.clone().ok_or_else(|| {
                crate::fail(
                    StatusCode::FORBIDDEN,
                    "Assign a project before starting agents",
                )
            })?;
            let request = request_id(&args)?;
            let previous = app.store.lock().unwrap().control_start(&a.id, &request)?;
            if let Some(id) = previous {
                if id.is_empty() {
                    return Err(crate::fail(
                        StatusCode::GONE,
                        "Previously started session was deleted",
                    ));
                }
                let prior = mail::peer_of(app, &a.project, &id)?;
                if prior
                    .data
                    .lock()
                    .unwrap()
                    .control_origin
                    .as_ref()
                    .is_some_and(|o| !o.payload.is_null() && o.payload != args)
                {
                    return Err(crate::fail(
                        StatusCode::CONFLICT,
                        "Request ID already used for a different session",
                    ));
                }
                return Ok(prior.meta());
            }
            if args
                .get("inheritResumePolicy")
                .is_some_and(|v| !v.is_boolean())
            {
                return Err(crate::fail(
                    StatusCode::BAD_REQUEST,
                    "inheritResumePolicy must be a boolean",
                ));
            }
            let inherited_resume = if args["inheritResumePolicy"].as_bool().unwrap_or(false) {
                Some(crate::limit_resume::inherit(session_only(a)?)?)
            } else {
                None
            };
            let req = crate::Create {
                resume_policy: None,
                inherited_resume,
                workspace: args
                    .get("workspace")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|_| crate::fail(StatusCode::BAD_REQUEST, "Invalid workspace"))?,
                request_id: None,
                agent_access: None,
                role: None,
                inherited_role: None,
                provider: field(&args, "provider")?,
                cwd: field(&args, "cwd")?,
                title: args["title"].as_str().map(str::to_owned),
                model: args["model"].as_str().map(str::to_owned),
                model_provider: args["modelProvider"].as_str().map(str::to_owned),
                project_id: Some(scope),
                host_id: Some(
                    a.session
                        .as_ref()
                        .map_or_else(|| "local".into(), |s| s.host_id.clone()),
                ),
                external_id: None,
                external_host: false,
                automation: None,
                queued_prompt: None,
                control_origin: Some(mail::ControlOrigin {
                    actor: a.id.clone(),
                    request,
                    payload: args.clone(),
                    actor_title: a.session.is_none().then(|| a.title.clone()),
                }),
                fork: None,
            };
            crate::create_session(State(app.clone()), Json(req))
                .await
                .map(|j| j.0)
        }
        "prompt_session" => {
            mail::require_actor(app, a, "prompt_sessions")?;
            let target = mail::peer_of(app, &a.project, &field(&args, "sessionId")?)?;
            if target.id == a.id {
                return Err(crate::fail(
                    StatusCode::BAD_REQUEST,
                    "Cannot prompt your own active session",
                ));
            }
            // The bridge holds communication_lock, which a waking harness needs for
            // its own MCP connection, so start the wake here and let the caller retry.
            if let Ok(s) = crate::session(app, &target.id)
                && crate::idle_sleep::asleep(&s)
            {
                crate::idle_sleep::start_waking(app, &s).await?;
                return Err(crate::fail(
                    StatusCode::CONFLICT,
                    "That session was asleep and is waking up; call prompt_session again in a few seconds",
                ));
            }
            let request = request_id(&args)?;
            crate::prompt(
                State(app.clone()),
                Path(target.id.clone()),
                Json(crate::Prompt {
                    initiated_by: Some(a.id.clone()),
                    initiator_title: a.session.is_none().then(|| a.title.clone()),
                    text: field(&args, "text")?,
                    request_id: format!("mcp:{}:{}", a.id, request),
                    attachments: Vec::new(),
                }),
            )
            .await
            .map(|j| j.0)
        }
        "archive_session" => {
            mail::require_actor(app, a, "archive_sessions")?;
            let req: ArchiveSession = serde_json::from_value(args).map_err(|_| {
                crate::fail(StatusCode::BAD_REQUEST, "Invalid archive request fields")
            })?;
            if req.session_id.is_empty() {
                return Err(crate::fail(StatusCode::BAD_REQUEST, "Missing sessionId"));
            }
            // The bridge holds communication_lock through this call, serializing
            // project reassignment/deletion and controller revocation.
            let target = mail::peer_of(app, &a.project, &req.session_id)?;
            crate::lifecycle::update(
                State(app.clone()),
                Path(target.id.clone()),
                Json(crate::lifecycle::Update::archive()),
            )
            .await
            .map(|j| j.0)
        }
        "cancel_session" => {
            mail::require_actor(app, a, "cancel_sessions")?;
            let target = mail::peer_of(app, &a.project, &field(&args, "sessionId")?)?;
            if target.id == a.id {
                return Err(crate::fail(
                    StatusCode::BAD_REQUEST,
                    "Use your harness cancellation instead",
                ));
            }
            crate::cancel(
                State(app.clone()),
                Path(target.id.clone()),
                axum::extract::Query(crate::CancelTarget::default()),
            )
            .await
            .map(|j| j.0)
        }
        "list_automations"
        | "get_automation"
        | "list_automation_runs"
        | "create_automation"
        | "update_automation"
        | "set_automation_enabled"
        | "delete_automation" => crate::automations::agent(app, a, operation, args).await,
        "run_automation" => {
            mail::require_actor(app, a, "run_automations")?;
            let scope = a
                .project
                .clone()
                .ok_or_else(|| crate::fail(StatusCode::FORBIDDEN, "Assign a project first"))?;
            let defs = app
                .store
                .lock()
                .unwrap()
                .usable_automations()
                .map_err(|_| {
                    crate::fail(StatusCode::INTERNAL_SERVER_ERROR, "Cannot read automations")
                })?;
            let id = field(&args, "automationId")?;
            if !defs
                .iter()
                .any(|d| d.id == id && d.project_id.as_deref() == Some(&scope))
            {
                return Err(crate::fail(
                    StatusCode::FORBIDDEN,
                    "Automation is outside this project",
                ));
            }
            let request = request_id(&args)?;
            crate::automations::run(
                State(app.clone()),
                Path(id),
                Json(crate::automations::Run {
                    initiated_by: Some(a.id.clone()),
                    request_id: format!("mcp:{}:{}", a.id, request),
                }),
            )
            .await
            .map(|j| j.0)
        }
        _ => Err(crate::fail(
            StatusCode::FORBIDDEN,
            "Agentworks operation is not exposed to agents",
        )),
    }
}
pub async fn serve_bridge(app: App, listener: TcpListener) {
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        let Ok((socket, _)) = listener.accept().await else {
            break;
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            drop(socket);
            continue;
        };
        let app = app.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _=tokio::time::timeout(Duration::from_secs(15),async {
                let (read,mut write)=socket.into_split();let mut bytes=Vec::new();
                BufReader::new(read).take(65537).read_until(b'\n',&mut bytes).await?;
                if bytes.len()>65536 || bytes.last()!=Some(&b'\n'){return Ok::<_,std::io::Error>(());}
                let Ok(v)=serde_json::from_slice::<Value>(&bytes) else {return Ok(())};
                let token=v["token"].as_str().unwrap_or("");
                let _gate=app.communication_lock.lock().await;
                // Controller tokens resolve only to live controllers, never to a session identity.
                let controller=crate::controllers::is_token(token);
                let actor=if controller {
                    app.store.lock().unwrap().controller_by_token(token).ok().flatten().map(|c|Actor::controller(&c))
                } else {
                    app.sessions.lock().unwrap().values().find(|s|s.mcp_token.lock().unwrap().as_str()==token).map(Actor::of)
                };
                let response=if let Some(s)=actor {
                    let operation=v["operation"].as_str().unwrap_or("");
                    let mutation=!matches!(operation,"tools/list"|"read_inbox"|"list_sessions"|"list_automations"|"get_automation"|"list_automation_runs"|"get_completion"|"get_service_status"|"list_tasks"|"get_task");
                    let action=operation.chars().take(80).collect::<String>();
                    let argument_target=v["arguments"]["sessionId"].as_str().or(v["arguments"]["recipient"].as_str()).or(v["arguments"]["automationId"].as_str()).unwrap_or("").chars().take(128).collect::<String>();
                    let request=v["arguments"]["requestId"].as_str().unwrap_or("").chars().take(100).collect::<String>();
                    if mutation {app.store.lock().unwrap().audit(&s.id,&action,&argument_target,&format!("requested:{request}")).map_err(|_|std::io::Error::other("Audit persistence failed"))?;}
                    let result=dispatch(&app,&s,operation,v["arguments"].clone()).await;
                    if mutation || result.is_err() {
                        let target=if operation=="start_session" {result.as_ref().ok().and_then(|v|v["id"].as_str()).unwrap_or(&argument_target)}else{&argument_target};
                        let outcome=if result.is_ok(){"accepted"}else{"rejected"};
                        app.store.lock().unwrap().audit(&s.id,&action,target,outcome).map_err(|_|std::io::Error::other("Audit persistence failed"))?;
                    }
                    match result {Ok(v)=>v,Err((code,message))=>json!({"error":{"status":code.as_u16(),"message":message}})}
                } else if controller {json!({"error":{"status":401,"message":"Agentworks controller credential is invalid or revoked"}})
                } else {json!({"error":{"status":401,"message":"Agentworks session credential expired"}})};
                let mut output=serde_json::to_vec(&response)?;output.push(b'\n');write.write_all(&output).await?;Ok(())
            }).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinators_and_explicit_controllers_can_archive_sessions() {
        for role in [mail::Role::Off, mail::Role::Participant] {
            let access = role.access();
            assert!(!access.archive_sessions);
            for session in [true, false] {
                assert!(
                    !tools(&access, session)["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|t| t["name"] == "archive_session")
                );
            }
        }
        let coordinator = mail::Role::Coordinator.access();
        assert!(coordinator.archive_sessions);
        for session in [true, false] {
            assert!(
                tools(&coordinator, session)["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["name"] == "archive_session")
            );
        }
        let old: mail::Access = serde_json::from_value(json!({"communicate":true})).unwrap();
        assert!(!old.archive_sessions);
        let access = mail::Access {
            archive_sessions: true,
            ..Default::default()
        };
        assert!(access.enabled());
        assert!(!mail::coordinator(&access)); // No completion/review authority implied.
        let names = |session| {
            tools(&access, session)["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(false), vec!["list_sessions", "archive_session"]);
        assert_eq!(names(true), vec!["list_sessions"]);
    }
    #[test]
    fn automation_management_is_an_explicit_grant() {
        for role in [
            mail::Role::Off,
            mail::Role::Participant,
            mail::Role::Coordinator,
        ] {
            assert!(!role.access().manage_automations);
        }
        let old: mail::Access = serde_json::from_value(json!({"runAutomations":true})).unwrap();
        assert!(!old.manage_automations);
        let access = mail::Access {
            manage_automations: true,
            ..Default::default()
        };
        assert!(access.enabled());
        assert!(!mail::coordinator(&access)); // No session-management authority implied.
        let names = tools(&access, true)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "list_sessions",
                "list_automations",
                "get_automation",
                "list_automation_runs",
                "create_automation",
                "update_automation",
                "set_automation_enabled",
                "delete_automation"
            ]
        );
    }
}
