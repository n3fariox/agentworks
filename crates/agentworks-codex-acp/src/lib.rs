//! Small ACP v1 bridge to the native Codex app-server. Codex owns execution and history.
use agent_client_protocol::{Agent, Client, ConnectionTo, Error, Stdio, UntypedMessage};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    process::Stdio as ProcessStdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
    sync::{Mutex as AsyncMutex, oneshot},
};

mod commands;

type Result<T> = std::result::Result<T, Error>;
type Reply = oneshot::Sender<Result<Value>>;

fn failure(message: impl ToString) -> Error {
    Error::internal_error().data(message.to_string())
}
fn invalid(message: &str) -> Error {
    Error::invalid_params().data(message.to_owned())
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| invalid(&format!("Missing {key}")))
}
fn message(method: &str, params: Value) -> UntypedMessage {
    UntypedMessage {
        method: method.into(),
        params,
    }
}
fn update(cx: &ConnectionTo<Client>, id: &str, update: Value) -> Result<()> {
    cx.send_notification(message(
        "session/update",
        json!({"sessionId":id,"update":update}),
    ))
}

#[derive(Default)]
struct Session {
    settings: Value,
    host_policy: Value,
    roots: Vec<String>,
    configuring: bool,
    usage: Value,
    last_completed: Option<String>,
    active: Option<ActiveTurn>,
}
struct ActiveTurn {
    id: Option<String>,
    cancelled: bool,
    compaction: bool,
    done: Reply,
}
struct Bridge {
    input: AsyncMutex<ChildStdin>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    sessions: Mutex<HashMap<String, Session>>,
    // Dropping an SDK request sends $/cancel_request, withdrawing the client's UI.
    questions: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    boolean_config: AtomicBool,
    limits: Mutex<Map<String, Value>>,
}

impl Bridge {
    async fn write(&self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input
            .lock()
            .await
            .write_all(&bytes)
            .await
            .map_err(failure)
    }
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let result = async {
            self.write(json!({"id":id,"method":method,"params":params}))
                .await?;
            tokio::time::timeout(Duration::from_secs(60), rx)
                .await
                .map_err(|_| failure(format!("Codex {method} timed out")))?
                .map_err(|_| failure("Codex app-server disconnected"))?
        }
        .await;
        self.pending.lock().unwrap().remove(&id);
        result
    }

    async fn request(
        self: &Arc<Self>,
        method: &str,
        p: Value,
        cx: &ConnectionTo<Client>,
    ) -> Result<Value> {
        match method {
            "initialize" => {
                if p["protocolVersion"] != 1 {
                    return Err(invalid("Only ACP v1 is supported"));
                }
                self.boolean_config.store(
                    p["clientCapabilities"]["session"]["configOptions"]["boolean"].is_object(),
                    Ordering::Relaxed,
                );
                self.rpc("initialize", json!({"clientInfo":{"name":"agentworks","title":"Agentworks","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
                self.write(json!({"method":"initialized","params":{}}))
                    .await?;
                Ok(
                    json!({"protocolVersion":1,"agentInfo":{"name":"agentworks-codex-acp","version":env!("CARGO_PKG_VERSION")},
                    "agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"embeddedContext":true},"mcpCapabilities":{"http":true},"sessionCapabilities":{"list":{},"close":{},"delete":{},"fork":{},"additionalDirectories":{}}},
                    "authMethods":[{"id":"api-key","name":"API key"}]}),
                )
            }
            "authenticate" => {
                if p["methodId"] != "api-key" {
                    return Err(invalid(
                        "Use the host Codex sign-in or api-key authentication",
                    ));
                }
                let key = std::env::var("CODEX_API_KEY")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        std::env::var("OPENAI_API_KEY")
                            .ok()
                            .filter(|s| !s.is_empty())
                    })
                    .ok_or_else(|| invalid("No Codex API key configured"))?;
                self.rpc("account/login/start", json!({"type":"apiKey","apiKey":key}))
                    .await?;
                Ok(json!({}))
            }
            "session/new" | "session/load" | "session/fork" => {
                let roots = workspace_roots(&p)?;
                if method != "session/new"
                    && self
                        .sessions
                        .lock()
                        .unwrap()
                        .get(field(&p, "sessionId")?)
                        .is_some_and(|s| s.active.is_some() || s.configuring)
                {
                    return Err(invalid("Cannot load or fork an active session"));
                }
                let mut params = json!({"cwd":field(&p,"cwd")?,"config":session_config(&p)?,"runtimeWorkspaceRoots":roots});
                let native = if method != "session/new" {
                    params["threadId"] = json!(field(&p, "sessionId")?);
                    params["excludeTurns"] = json!(true);
                    if method == "session/fork" {
                        let fork = &p["_meta"]["jetbrains"]["air"]["fork"];
                        if !fork.is_null() {
                            if fork["version"] != 1 {
                                return Err(invalid("Unsupported fork boundary version"));
                            }
                            let boundary = field(fork, "messageId")?;
                            let protocol_id = boundary
                                .rsplit_once(":segment:")
                                .filter(|(_, suffix)| {
                                    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                                })
                                .map_or(boundary, |(id, _)| id);
                            let mut turn = None;
                            self.history(field(&p, "sessionId")?, |turn_id, item| {
                                if item["id"] == boundary || item["id"] == protocol_id {
                                    turn = Some(turn_id.clone());
                                }
                                Ok(())
                            })
                            .await?;
                            params["lastTurnId"] = turn.filter(|id|id.is_string()).ok_or_else(||invalid("Fork boundary was not found; refusing to fork newer history"))?;
                        }
                        "thread/fork"
                    } else {
                        "thread/resume"
                    }
                } else {
                    "thread/start"
                };
                let response = self.rpc(native, params).await?;
                let id = field(&response["thread"], "id")?.to_owned();
                if method == "session/fork" && p["sessionId"] == id {
                    return Err(failure("Codex returned the parent thread as its fork"));
                }
                let settings = session_settings(&response);
                self.sessions.lock().unwrap().insert(
                    id.clone(),
                    Session {
                        host_policy: policy(&settings),
                        settings,
                        roots,
                        configuring: false,
                        usage: Value::Null,
                        last_completed: None,
                        active: None,
                    },
                );
                if method == "session/load" {
                    // History is replayed as ACP notifications; the app decides whether to display it.
                    self.history(&id, |_, item| replay_item(cx, &id, item))
                        .await?;
                }
                let options = self.config_options(&id).await?;
                update(
                    cx,
                    &id,
                    json!({"sessionUpdate":"available_commands_update","availableCommands":commands::available()}),
                )?;
                Ok(json!({"sessionId":id,"configOptions":options}))
            }
            "session/close" | "session/delete" => {
                let id = field(&p, "sessionId")?;
                if self
                    .sessions
                    .lock()
                    .unwrap()
                    .get(id)
                    .is_some_and(|s| s.active.is_some() || s.configuring)
                {
                    return Err(invalid(
                        "Cancel the active turn before closing or deleting its session",
                    ));
                }
                self.rpc(
                    if method == "session/close" {
                        "thread/unsubscribe"
                    } else {
                        "thread/delete"
                    },
                    json!({"threadId":id}),
                )
                .await?;
                self.sessions.lock().unwrap().remove(id);
                Ok(json!({}))
            }
            "session/list" => {
                let response = self
                    .rpc("thread/list", json!({"cwd":p["cwd"],"cursor":p["cursor"]}))
                    .await?;
                let sessions: Vec<_> = response["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| {
                        // ACP accepts RFC3339, whereas Codex uses Unix seconds.
                        let mut item = json!({"sessionId":t["id"],"cwd":t["cwd"]});
                        if let Some(title) = t["name"].as_str().or_else(|| t["preview"].as_str()) {
                            item["title"] = json!(title);
                        }
                        item
                    })
                    .collect();
                Ok(json!({"sessions":sessions,"nextCursor":response["nextCursor"]}))
            }
            "session/set_config_option" | "session/set_mode" => {
                let id = field(&p, "sessionId")?;
                let key = if method == "session/set_mode" {
                    "mode"
                } else {
                    field(&p, "configId")?
                };
                let value = if method == "session/set_mode" {
                    &p["modeId"]
                } else {
                    &p["value"]
                };
                let options = self.config_options(id).await?;
                if !options.as_array().unwrap().iter().any(|o| {
                    o["id"] == key
                        && ((o["type"] == "boolean" && value.is_boolean())
                            || o["options"]
                                .as_array()
                                .is_some_and(|opts| opts.iter().any(|o| o["value"] == *value)))
                }) {
                    return Err(invalid("Unknown config value"));
                }
                let mut settings = {
                    let mut sessions = self.sessions.lock().unwrap();
                    let s = sessions
                        .get_mut(id)
                        .ok_or_else(|| invalid("Unknown session"))?;
                    if s.active.is_some() || s.configuring {
                        return Err(invalid("Cannot configure an active turn"));
                    }
                    let mut patch = match key {
                        "model" => json!({"model":value}),
                        "reasoning_effort" => json!({"effort":value}),
                        "fast-mode" => {
                            json!({"serviceTier":if value==true || value=="on" {json!("fast")}else{Value::Null}})
                        }
                        "collaboration_mode" => {
                            json!({"collaborationMode":{"mode":value,"settings":{"model":s.settings["model"],"reasoning_effort":s.settings["effort"],"developer_instructions":null}}})
                        }
                        "mode" => {
                            if value == "host" {
                                s.host_policy.clone()
                            } else {
                                mode_policy(value.as_str().unwrap(), &s.roots)?
                            }
                        }
                        _ => return Err(invalid("Unsupported config option")),
                    };
                    if matches!(key, "model" | "reasoning_effort")
                        && s.settings["collaborationMode"].is_object()
                    {
                        let mut collaboration = s.settings["collaborationMode"].clone();
                        collaboration["settings"][if key == "model" {
                            "model"
                        } else {
                            "reasoning_effort"
                        }] = value.clone();
                        patch["collaborationMode"] = collaboration;
                    }
                    s.configuring = true;
                    patch
                };
                settings["threadId"] = json!(id);
                let result = self.rpc("thread/settings/update", settings.clone()).await;
                {
                    let mut sessions = self.sessions.lock().unwrap();
                    let s = sessions.get_mut(id).unwrap();
                    s.configuring = false;
                    if result.is_ok() {
                        settings.as_object_mut().unwrap().remove("threadId");
                        s.settings
                            .as_object_mut()
                            .unwrap()
                            .extend(settings.as_object().unwrap().clone());
                    }
                }
                result?;
                let options = self.config_options(id).await?;
                update(
                    cx,
                    id,
                    json!({"sessionUpdate":"config_option_update","configOptions":options}),
                )?;
                if method == "session/set_mode" {
                    update(
                        cx,
                        id,
                        json!({"sessionUpdate":"current_mode_update","currentModeId":value}),
                    )?;
                    Ok(json!({}))
                } else {
                    Ok(json!({"configOptions":options}))
                }
            }
            "session/prompt" => {
                if let Some((name, args)) = commands::parse(&p["prompt"]) {
                    if p["prompt"].as_array().is_some_and(|v| v.len() != 1) {
                        return Err(invalid(
                            "Send slash commands without attachments or extra context",
                        ));
                    }
                    self.command(field(&p, "sessionId")?, name, args, cx).await
                } else {
                    self.prompt(p).await
                }
            }
            _ => Err(Error::method_not_found()),
        }
    }

    async fn config_options(&self, id: &str) -> Result<Value> {
        let response = self.rpc("model/list", json!({})).await?;
        let sessions = self.sessions.lock().unwrap();
        let s = sessions.get(id).ok_or_else(|| invalid("Unknown session"))?;
        let models = response["data"]
            .as_array()
            .ok_or_else(|| failure("Codex returned no model list"))?;
        let options: Vec<_> = models
            .iter()
            .filter(|m| m["hidden"] != true)
            .map(|m| json!({"value":m["model"],"name":m["displayName"]}))
            .collect();
        let current = models.iter().find(|m| m["model"] == s.settings["model"]);
        let efforts: Vec<_> = current
            .into_iter()
            .flat_map(|m| {
                m["supportedReasoningEfforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .map(|e| json!({"value":e["reasoningEffort"],"name":e["reasoningEffort"]}))
            .collect();
        let mut result = vec![
            json!({"id":"model","name":"Model","category":"model","type":"select","currentValue":s.settings["model"],"options":options}),
        ];
        if !efforts.is_empty() {
            result.push(json!({"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":s.settings["effort"].as_str().or_else(||current.and_then(|m|m["defaultReasoningEffort"].as_str())),"options":efforts}));
        }
        let modes: Vec<_> = ["host","read-only","workspace-write","agent","agent-full-access"].iter().map(|id|json!({"value":id,"name":match *id {"host"=>"Host configuration","read-only"=>"Read-only","workspace-write"=>"Workspace access","agent"=>"Auto review",_=>"Full access"}})).collect();
        result.push(json!({"id":"mode","name":"Permissions","category":"mode","type":"select","currentValue":policy_mode(&s.settings),"options":modes}));
        result.push(json!({"id":"collaboration_mode","name":"Collaboration mode","category":"collaboration_mode","type":"select","currentValue":s.settings["collaborationMode"]["mode"].as_str().unwrap_or("default"),"options":[{"value":"default","name":"Default"},{"value":"plan","name":"Plan"}]}));
        if current.is_some_and(|m| {
            m["additionalSpeedTiers"]
                .as_array()
                .is_some_and(|v| v.iter().any(|v| v == "fast"))
        }) {
            let enabled = s.settings["serviceTier"] == "fast";
            result.push(if self.boolean_config.load(Ordering::Relaxed) {
                json!({"id":"fast-mode","name":"Fast mode","description":"Faster responses, increased usage","category":"model_config","type":"boolean","currentValue":enabled})
            }else{json!({"id":"fast-mode","name":"Fast mode","description":"Faster responses, increased usage","category":"model_config","type":"select","currentValue":if enabled{"on"}else{"off"},"options":[{"value":"off","name":"Off"},{"value":"on","name":"On"}]})});
        }
        Ok(json!(result))
    }

    async fn history(
        &self,
        id: &str,
        mut visit: impl FnMut(&Value, &Value) -> Result<()>,
    ) -> Result<()> {
        let metadata = self.rpc("thread/read", json!({"threadId":id})).await?;
        if metadata["thread"]["historyMode"] == "paginated" {
            let mut cursor = Value::Null;
            let mut seen = std::collections::HashSet::new();
            loop {
                let page = self
                    .rpc(
                        "thread/items/list",
                        json!({"threadId":id,"sortDirection":"asc","limit":100,"cursor":cursor}),
                    )
                    .await?;
                for entry in page["data"]
                    .as_array()
                    .ok_or_else(|| failure("Malformed Codex history page"))?
                {
                    visit(&entry["turnId"], &entry["item"])?;
                }
                cursor = page["nextCursor"].clone();
                if cursor.is_null() {
                    break;
                }
                if !seen.insert(cursor.to_string()) {
                    return Err(failure("Codex repeated a history cursor"));
                }
            }
        } else {
            let history = self
                .rpc("thread/read", json!({"threadId":id,"includeTurns":true}))
                .await?;
            for turn in history["thread"]["turns"]
                .as_array()
                .ok_or_else(|| failure("Malformed Codex history"))?
            {
                for item in turn["items"].as_array().into_iter().flatten() {
                    visit(&turn["id"], item)?;
                }
            }
        }
        Ok(())
    }

    async fn prompt(&self, p: Value) -> Result<Value> {
        let id = field(&p, "sessionId")?;
        let input = prompt_input(&p["prompt"])?;
        self.run_turn(id, "turn/start", json!({"threadId":id,"input":input}))
            .await
    }

    async fn run_turn(&self, id: &str, method: &str, params: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        {
            let mut sessions = self.sessions.lock().unwrap();
            let s = sessions
                .get_mut(id)
                .ok_or_else(|| invalid("Unknown session"))?;
            if s.active.is_some() || s.configuring {
                return Err(invalid("A turn is already running"));
            }
            s.active = Some(ActiveTurn {
                id: None,
                cancelled: false,
                compaction: method == "thread/compact/start",
                done: tx,
            });
        }
        let started = self.rpc(method, params).await;
        let response = match started {
            Ok(v) => v,
            Err(e) => {
                self.sessions.lock().unwrap().get_mut(id).unwrap().active = None;
                return Err(e);
            }
        };
        // Compaction acknowledges with {}; its turn ID arrives through turn/started.
        let turn_id = response["turn"]["id"].as_str();
        let cancelled = {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(active) = sessions.get_mut(id).and_then(|s| s.active.as_mut()) {
                if let Some(turn_id) = turn_id {
                    active.id = Some(turn_id.into());
                }
                if active.id.is_some() && std::mem::take(&mut active.cancelled) {
                    active.id.clone()
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(cancelled) = cancelled {
            self.rpc("turn/interrupt", json!({"threadId":id,"turnId":cancelled}))
                .await?;
        }
        let completed = rx.await.map_err(|_| failure("Codex turn disconnected"))??;
        if turn_id.is_some_and(|id| completed["id"] != id) {
            return Err(failure("Codex completed an unexpected turn"));
        }
        match completed["status"].as_str() {
            Some("completed") => Ok(json!({"stopReason":"end_turn"})),
            Some("interrupted") => Ok(json!({"stopReason":"cancelled"})),
            _ => Err(Error::internal_error().data(json!({"message":completed["error"]["message"],"codexErrorInfo":completed["error"]["codexErrorInfo"]}))),
        }
    }

    fn cancel(&self, p: Value) -> Result<Option<Value>> {
        let id = field(&p, "sessionId")?;
        let turn = {
            let mut sessions = self.sessions.lock().unwrap();
            sessions
                .get_mut(id)
                .and_then(|s| s.active.as_mut())
                .and_then(|a| {
                    a.cancelled = a.id.is_none();
                    a.id.clone()
                })
        };
        Ok(turn.map(|turn| json!({"threadId":id,"turnId":turn})))
    }

    async fn read(self: &Arc<Self>, output: ChildStdout, cx: ConnectionTo<Client>) -> Result<()> {
        let mut lines = BufReader::new(output).lines();
        while let Some(line) = lines.next_line().await.map_err(failure)? {
            let v: Value = serde_json::from_str(&line)?;
            if let Some(method) = v["method"].as_str() {
                if !v["id"].is_null() {
                    let bridge = self.clone();
                    let client = cx.clone();
                    let request_id = v["id"].to_string();
                    let method = method.to_owned();
                    // Remove completed entries; serverRequest/resolved withdraws unanswered ones.
                    self.questions
                        .lock()
                        .unwrap()
                        .retain(|_, task| !task.is_finished());
                    let task = tokio::spawn(async move {
                        let result = client_request(&client, &method, &v["params"]).await;
                        let reply = match result {
                            Ok(result) => json!({"id":v["id"],"result":result}),
                            Err(e) => {
                                json!({"id":v["id"],"error":{"code":-32603,"message":e.to_string()}})
                            }
                        };
                        let _ = bridge.write(reply).await;
                    });
                    self.questions.lock().unwrap().insert(request_id, task);
                } else if method == "serverRequest/resolved" {
                    if let Some(task) = self
                        .questions
                        .lock()
                        .unwrap()
                        .remove(&v["params"]["requestId"].to_string())
                    {
                        task.abort();
                    }
                } else if method == "account/rateLimits/updated" {
                    observe_limits(&mut self.limits.lock().unwrap(), &v["params"]["rateLimits"]);
                } else {
                    let p = &v["params"];
                    let id = p["threadId"].as_str().unwrap_or("");
                    if method == "turn/started"
                        || (method == "item/started"
                            && matches!(
                                p["item"]["type"].as_str(),
                                Some("contextCompaction" | "enteredReviewMode")
                            ))
                    {
                        let cancel = {
                            let mut sessions = self.sessions.lock().unwrap();
                            sessions
                                .get_mut(id)
                                .and_then(|s| s.active.as_mut())
                                .and_then(|active| {
                                    if let Some(turn) =
                                        p["turn"]["id"].as_str().or_else(|| p["turnId"].as_str())
                                    {
                                        // Review also emits an internal turn/started ID. Its API
                                        // response and item events retain the owning review ID.
                                        active.id.get_or_insert_with(|| turn.to_owned());
                                    }
                                    if active.id.is_some() && std::mem::take(&mut active.cancelled)
                                    {
                                        active.id.clone()
                                    } else {
                                        None
                                    }
                                })
                        };
                        if let Some(turn) = cancel {
                            let bridge = self.clone();
                            let params = json!({"threadId":id,"turnId":turn});
                            cx.spawn(async move {
                                bridge.rpc("turn/interrupt", params).await.map(|_| ())
                            })?;
                        }
                    }
                    if method == "turn/completed" {
                        let active = self.sessions.lock().unwrap().get_mut(id).and_then(|s| {
                            let completed = p["turn"]["id"].as_str()?;
                            if s.last_completed.as_deref() == Some(completed)
                                || s.active
                                    .as_ref()
                                    .and_then(|a| a.id.as_deref())
                                    .is_some_and(|active| active != completed)
                            {
                                return None;
                            }
                            s.last_completed = Some(completed.into());
                            s.active.take()
                        });
                        if let Some(active) = active {
                            let _ = active.done.send(Ok(p["turn"].clone()));
                        }
                    } else if self.sessions.lock().unwrap().contains_key(id) {
                        if method == "thread/settings/updated" {
                            if let Some(s) = self.sessions.lock().unwrap().get_mut(id) {
                                s.settings.as_object_mut().unwrap().extend(
                                    session_settings(&p["threadSettings"])
                                        .as_object()
                                        .unwrap()
                                        .clone(),
                                );
                            }
                        }
                        if let Some(mut event) = event_update(method, p) {
                            if event["sessionUpdate"] == "usage_update" {
                                if let Some(s) = self.sessions.lock().unwrap().get_mut(id) {
                                    s.usage = event.clone();
                                }
                                let limits = self.limits.lock().unwrap();
                                if !limits.is_empty() {
                                    event["_meta"] = json!({"_codex/subscriptionLimits":{"version":1,"limits":limits.values().collect::<Vec<_>>()}});
                                }
                            }
                            update(&cx, id, event)?;
                        }
                        // Manual compaction can finish at an item boundary without turn/completed.
                        // Automatic compaction inside a normal prompt must leave it running.
                        let completed_item = if method == "thread/compacted" {
                            Some("contextCompaction")
                        } else if method == "item/completed" {
                            p["item"]["type"].as_str()
                        } else {
                            None
                        };
                        if completed_item.is_some() {
                            let mut sessions = self.sessions.lock().unwrap();
                            let session = sessions.get_mut(id).unwrap();
                            if session.active.as_ref().is_some_and(|a| a.compaction)
                                && completed_item == Some("contextCompaction")
                            {
                                let active = session.active.take().unwrap();
                                session.last_completed = active
                                    .id
                                    .clone()
                                    .or_else(|| p["turnId"].as_str().map(str::to_owned));
                                let _ = active
                                    .done
                                    .send(Ok(json!({"id":active.id,"status":"completed"})));
                            }
                        }
                    }
                }
            } else if let Some(id) = v["id"].as_u64() {
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    let result = if let Some(e) = v.get("error") {
                        Err(Error::internal_error().data(e.clone()))
                    } else {
                        Ok(v["result"].clone())
                    };
                    let _ = tx.send(result);
                }
            }
        }
        Err(failure("Codex app-server exited"))
    }
}

fn workspace_roots(p: &Value) -> Result<Vec<String>> {
    let mut roots = vec![field(p, "cwd")?.to_owned()];
    let extra = p
        .get("additionalDirectories")
        .or_else(|| p["_meta"].get("additionalRoots"));
    if let Some(extra) = extra.filter(|v| !v.is_null()) {
        for root in extra
            .as_array()
            .ok_or_else(|| invalid("additionalDirectories must be an array"))?
        {
            let root = root
                .as_str()
                .ok_or_else(|| invalid("Workspace roots must be paths"))?;
            if !roots.iter().any(|r| r == root) {
                roots.push(root.into());
            }
        }
    }
    if roots.iter().any(|r| !std::path::Path::new(r).is_absolute()) {
        return Err(invalid("Workspace roots must be absolute"));
    }
    Ok(roots)
}
fn session_settings(response: &Value) -> Value {
    let mut settings = Map::new();
    for (key, native) in [
        ("model", "model"),
        ("effort", "reasoningEffort"),
        ("sandboxPolicy", "sandbox"),
        ("approvalPolicy", "approvalPolicy"),
        ("approvalsReviewer", "approvalsReviewer"),
        ("serviceTier", "serviceTier"),
        ("collaborationMode", "collaborationMode"),
    ] {
        if let Some(v) = response.get(key).or_else(|| response.get(native)) {
            settings.insert(key.into(), v.clone());
        }
    }
    json!(settings)
}
fn policy(settings: &Value) -> Value {
    json!(
        settings
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| matches!(
                k.as_str(),
                "sandboxPolicy" | "approvalPolicy" | "approvalsReviewer"
            ))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Map<_, _>>()
    )
}
fn mode_policy(mode: &str, roots: &[String]) -> Result<Value> {
    let sandbox = match mode {
        "read-only" => json!({"type":"readOnly","networkAccess":false}),
        "workspace-write" | "agent" => {
            json!({"type":"workspaceWrite","writableRoots":roots,"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
        }
        "agent-full-access" => json!({"type":"dangerFullAccess"}),
        _ => return Err(invalid("Unknown permission mode")),
    };
    Ok(
        json!({"sandboxPolicy":sandbox,"approvalPolicy":if mode=="agent-full-access"{"never"}else{"on-request"},"approvalsReviewer":if mode=="agent"{"auto_review"}else{"user"}}),
    )
}
fn policy_mode(settings: &Value) -> &str {
    match (
        settings["sandboxPolicy"]["type"].as_str(),
        settings["approvalPolicy"].as_str(),
        settings["approvalsReviewer"].as_str(),
    ) {
        (Some("dangerFullAccess"), Some("never"), Some("user")) => "agent-full-access",
        (Some("readOnly"), Some("on-request"), Some("user")) => "read-only",
        (Some("workspaceWrite"), Some("on-request"), Some("auto_review")) => "agent",
        (Some("workspaceWrite"), Some("on-request"), Some("user")) => "workspace-write",
        _ => "host",
    }
}
fn observe_limits(cache: &mut Map<String, Value>, snapshot: &Value) {
    if !snapshot.is_object() {
        return;
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let id = snapshot["limitId"].as_str().unwrap_or("codex");
    let entry = cache.entry(id).or_insert_with(|| json!({"limitId":id}));
    if let Some(name) = snapshot.get("limitName") {
        entry["limitName"] = name.clone();
    }
    for key in ["primary", "secondary"] {
        let Some(window) = snapshot.get(key) else {
            continue;
        };
        if window.is_null() {
            entry[key] = Value::Null;
            continue;
        }
        if !window.is_object() {
            continue;
        }
        if !entry[key].is_object() {
            entry[key] = json!({});
        }
        for name in ["usedPercent", "windowDurationMins", "resetsAt"] {
            if let Some(value) = window.get(name) {
                entry[key][name] = json!({"value":value,"observedAtMs":timestamp});
            }
        }
    }
}

fn session_config(p: &Value) -> Result<Value> {
    let mut config = match std::env::var("CODEX_CONFIG") {
        Ok(s) if !s.is_empty() => {
            serde_json::from_str::<Map<String, Value>>(&s).map_err(failure)?
        }
        _ => Map::new(),
    };
    let mut servers = Map::new();
    for server in p["mcpServers"].as_array().into_iter().flatten() {
        let value = match server["type"].as_str() {
            None | Some("stdio") => {
                json!({"command":field(server,"command")?,"args":server["args"].as_array().cloned().unwrap_or_default(),"env":name_values(&server["env"])})
            }
            Some("http") => {
                json!({"url":field(server,"url")?,"http_headers":name_values(&server["headers"])})
            }
            _ => return Err(invalid("Unsupported MCP transport")),
        };
        servers.insert(field(server, "name")?.into(), value);
    }
    if !servers.is_empty() {
        config.insert("mcp_servers".into(), json!(servers));
    }
    Ok(json!(config))
}
fn name_values(values: &Value) -> Map<String, Value> {
    values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v["name"].as_str()?.into(), v["value"].clone())))
        .collect()
}
fn prompt_input(blocks: &Value) -> Result<Vec<Value>> {
    blocks.as_array().ok_or_else(||invalid("prompt must be an array"))?.iter().map(|b| {
        let text = match b["type"].as_str() {
            Some("text") => field(b,"text")?.to_owned(),
            Some("image") => return Ok(json!({"type":"image","url":format!("data:{};base64,{}",field(b,"mimeType")?,field(b,"data")?)})),
            Some("resource_link") => field(b,"uri")?.to_owned(),
            Some("resource") => format!("<context ref=\"{}\">\n{}\n</context>",field(&b["resource"],"uri")?,field(&b["resource"],"text")?),
            _ => return Err(invalid("Unsupported prompt content")),
        };
        Ok(json!({"type":"text","text":text,"text_elements":[]}))
    }).collect()
}
fn event_update(method: &str, p: &Value) -> Option<Value> {
    match method {
        "item/completed" if p["item"]["type"] == "exitedReviewMode" => Some(
            json!({"sessionUpdate":"agent_message_chunk","messageId":p["item"]["id"],"content":{"type":"text","text":p["item"]["review"]}}),
        ),
        "item/started" if p["item"]["type"] == "enteredReviewMode" => Some(
            json!({"sessionUpdate":"agent_thought_chunk","messageId":p["item"]["id"],"content":{"type":"text","text":format!("Reviewing {}",p["item"]["review"].as_str().unwrap_or("changes"))}}),
        ),
        "item/agentMessage/delta"
        | "item/reasoning/textDelta"
        | "item/reasoning/summaryTextDelta" => Some(
            json!({"sessionUpdate":if method=="item/agentMessage/delta" {"agent_message_chunk"}else{"agent_thought_chunk"},"messageId":p["itemId"],"content":{"type":"text","text":p["delta"]}}),
        ),
        "item/started" | "item/completed" => tool_update(&p["item"], method == "item/completed"),
        "item/commandExecution/outputDelta" => Some(
            json!({"sessionUpdate":"tool_call_update","toolCallId":p["itemId"],"content":[{"type":"content","content":{"type":"text","text":p["delta"]}}]}),
        ),
        "turn/plan/updated" => Some(
            json!({"sessionUpdate":"plan","entries":p["plan"].as_array()?.iter().map(|s|json!({"content":s["step"],"priority":"medium","status":match s["status"].as_str(){Some("completed")=>"completed",Some("inProgress")=>"in_progress",_=>"pending"}})).collect::<Vec<_>>()}),
        ),
        "thread/tokenUsage/updated"
            if p["tokenUsage"]["modelContextWindow"]
                .as_u64()
                .is_some_and(|n| n > 0) =>
        {
            Some(
                json!({"sessionUpdate":"usage_update","used":p["tokenUsage"]["last"]["totalTokens"],"size":p["tokenUsage"]["modelContextWindow"]}),
            )
        }
        _ => None,
    }
}
fn tool_update(item: &Value, done: bool) -> Option<Value> {
    let kind = match item["type"].as_str()? {
        "commandExecution" => "execute",
        "fileChange" => "edit",
        "mcpToolCall" | "dynamicToolCall" | "contextCompaction" => "other",
        "webSearch" => "search",
        "imageView" => "read",
        _ => return None,
    };
    let title = item["command"]
        .as_str()
        .or_else(|| item["tool"].as_str())
        .unwrap_or(kind);
    let mut result = json!({"sessionUpdate":if done{"tool_call_update"}else{"tool_call"},"toolCallId":item["id"],"title":title,"kind":kind,"status":if !done{"in_progress"}else if matches!(item["status"].as_str(),Some("failed"|"declined")){"failed"}else{"completed"}});
    result[if done { "rawOutput" } else { "rawInput" }] = item.clone();
    let output = item["aggregatedOutput"].as_str().filter(|s| !s.is_empty());
    if let Some(output) = output {
        result["content"] = json!([{"type":"content","content":{"type":"text","text":output}}]);
    }
    Some(result)
}
fn replay_item(cx: &ConnectionTo<Client>, id: &str, item: &Value) -> Result<()> {
    match item["type"].as_str() {
        Some("agentMessage" | "exitedReviewMode") => update(
            cx,
            id,
            json!({"sessionUpdate":"agent_message_chunk","messageId":item["id"],"content":{"type":"text","text":item.get("text").unwrap_or(&item["review"])}}),
        ),
        Some("userMessage") => {
            for block in item["content"].as_array().into_iter().flatten() {
                if block["type"] == "text" {
                    update(
                        cx,
                        id,
                        json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":block["text"]}}),
                    )?;
                }
            }
            Ok(())
        }
        _ => {
            if let Some(event) = tool_update(item, true) {
                update(cx, id, event)?;
            }
            Ok(())
        }
    }
}

async fn client_request(cx: &ConnectionTo<Client>, method: &str, p: &Value) -> Result<Value> {
    let session = field(p, "threadId")?;
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            let available = p["availableDecisions"].as_array().cloned().unwrap_or_else(||vec![json!("accept"),json!("acceptForSession"),json!("decline"),json!("cancel")]);
            let options: Vec<_> = available.iter().filter_map(|d| {
                let (kind,name)=match d.as_str()? {"accept"=>("allow_once","Allow"),"acceptForSession"=>("allow_always","Allow for session"),"decline"=>("reject_once","Decline"),"cancel"=>("reject_once","Cancel"),_=>return None};
                Some(json!({"optionId":d,"name":name,"kind":kind}))
            }).collect();
            let response = cx.send_request(message("session/request_permission",json!({"sessionId":session,"toolCall":{"toolCallId":p["itemId"],"title":p["command"].as_str().or_else(||p["reason"].as_str()).unwrap_or("Approve file changes"),"status":"pending","rawInput":p},"options":options}))).block_task().await?;
            let chosen = &response["outcome"]["optionId"];
            let offered = chosen.as_str().is_some_and(|id| matches!(id,"accept"|"acceptForSession"|"decline"|"cancel")) && available.contains(chosen);
            Ok(json!({"decision":if response["outcome"]["outcome"]=="selected" && offered {chosen.clone()}else{json!("cancel")}}))
        }
        "item/permissions/requestApproval" => {
            let response = cx.send_request(message("session/request_permission",json!({"sessionId":session,"toolCall":{"toolCallId":p["itemId"],"title":"Additional permissions","status":"pending","rawInput":p},"options":[{"optionId":"allow","name":"Allow for this turn","kind":"allow_once"},{"optionId":"deny","name":"Deny","kind":"reject_once"}]}))).block_task().await?;
            Ok(json!({"permissions":if response["outcome"]["outcome"]=="selected" && response["outcome"]["optionId"]=="allow" {p["permissions"].clone()}else{json!({})},"scope":"turn"}))
        }
        "mcpServer/elicitation/request" if matches!(p["mode"].as_str(),Some("form"|"openai/form")) => {
            cx.send_request(message("elicitation/create",json!({"sessionId":session,"mode":"form","message":p["message"],"requestedSchema":p["requestedSchema"]}))).block_task().await
        }
        "item/tool/requestUserInput" => {
            let mut properties = Map::new();
            let mut required = Vec::new();
            for q in p["questions"].as_array().into_iter().flatten() {
                let id = field(q,"id")?;
                let mut schema = json!({"type":"string","title":q["header"],"description":q["question"]});
                if let Some(options) = q["options"].as_array().filter(|v|!v.is_empty()) {
                    schema["enum"] = json!(options.iter().map(|o|o["label"].clone()).collect::<Vec<_>>());
                }
                properties.insert(id.into(),schema); required.push(id);
            }
            let response = cx.send_request(message("elicitation/create",json!({"sessionId":session,"mode":"form","message":"Codex needs your input","requestedSchema":{"type":"object","properties":properties,"required":required}}))).block_task().await?;
            let answers: Map<String,Value> = if response["action"]=="accept" {
                response["content"].as_object().into_iter().flatten().map(|(k,v)|(k.clone(),json!({"answers":[v.as_str().unwrap_or("")]}))).collect()
            } else {Map::new()};
            Ok(json!({"answers":answers}))
        }
        "mcpServer/elicitation/request" => Ok(json!({"action":"cancel"})),
        _ => Err(Error::method_not_found()),
    }
}

/// Run as a separate stdio process; stdout is reserved for ACP.
pub async fn run() -> Result<()> {
    let mut child = Command::new(std::env::var_os("CODEX_PATH").unwrap_or_else(|| "codex".into()))
        .arg("app-server")
        .stdin(ProcessStdio::piped())
        .stdout(ProcessStdio::piped())
        .stderr(ProcessStdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(failure)?;
    let output = child.stdout.take().unwrap();
    let bridge = Arc::new(Bridge {
        input: AsyncMutex::new(child.stdin.take().unwrap()),
        next: AtomicU64::new(1),
        pending: Mutex::default(),
        sessions: Mutex::default(),
        questions: Mutex::default(),
        boolean_config: AtomicBool::new(false),
        limits: Mutex::default(),
    });
    let requests = bridge.clone();
    let notifications = bridge.clone();
    let reader = bridge.clone();
    let result = Agent
        .builder()
        .name("agentworks-codex-acp")
        .on_receive_request(
            async move |request: UntypedMessage, responder, cx| {
                let bridge = requests.clone();
                let client = cx.clone();
                cx.spawn(async move {
                    responder.respond_with_result(
                        bridge
                            .request(&request.method, request.params, &client)
                            .await,
                    )
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: UntypedMessage, cx| {
                if notification.method == "session/cancel" {
                    // Snapshot at receipt so an idle cancel cannot interrupt a later prompt.
                    if let Some(params) = notifications.cancel(notification.params)? {
                        let bridge = notifications.clone();
                        cx.spawn(
                            async move { bridge.rpc("turn/interrupt", params).await.map(|_| ()) },
                        )?;
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(Stdio::new(), move |cx: ConnectionTo<Client>| async move {
            tokio::select! {
                result = reader.read(output, cx.clone()) => result,
                _ = cx.incoming_closed() => Ok(()),
            }
        })
        .await;
    for (_, task) in bridge.questions.lock().unwrap().drain() {
        task.abort();
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}
