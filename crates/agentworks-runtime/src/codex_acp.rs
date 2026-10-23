//! Small ACP v1 bridge to the native Codex app-server. Codex owns execution and history.
use agent_client_protocol::{Agent, Client, ConnectionTo, Error, Stdio, UntypedMessage};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    process::Stdio as ProcessStdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
    sync::{Mutex as AsyncMutex, oneshot},
};

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
    model: Value,
    effort: Value,
    active: Option<ActiveTurn>,
}
struct ActiveTurn {
    id: Option<String>,
    cancelled: bool,
    done: Reply,
}
struct Bridge {
    input: AsyncMutex<ChildStdin>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    sessions: Mutex<HashMap<String, Session>>,
    // Dropping an SDK request sends $/cancel_request, withdrawing the client's UI.
    questions: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
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
                self.rpc("initialize", json!({"clientInfo":{"name":"agentworks","title":"Agentworks","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
                self.write(json!({"method":"initialized","params":{}}))
                    .await?;
                Ok(
                    json!({"protocolVersion":1,"agentInfo":{"name":"agentworks-codex-acp","version":env!("CARGO_PKG_VERSION")},
                    "agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"embeddedContext":true},"mcpCapabilities":{"http":true},"sessionCapabilities":{"list":{}}},
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
            "session/new" | "session/load" => {
                if p["additionalDirectories"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    return Err(invalid(
                        "Additional workspace directories are not implemented",
                    ));
                }
                if method == "session/load"
                    && self
                        .sessions
                        .lock()
                        .unwrap()
                        .get(field(&p, "sessionId")?)
                        .is_some_and(|s| s.active.is_some())
                {
                    return Err(invalid("Cannot reload an active session"));
                }
                let mut config = session_config(&p)?;
                let mut params = json!({"cwd":field(&p,"cwd")?,"config":config.take()});
                let native = if method == "session/load" {
                    params["threadId"] = json!(field(&p, "sessionId")?);
                    "thread/resume"
                } else {
                    "thread/start"
                };
                let response = self.rpc(native, params).await?;
                let id = field(&response["thread"], "id")?.to_owned();
                self.sessions.lock().unwrap().insert(
                    id.clone(),
                    Session {
                        model: response["model"].clone(),
                        effort: response["reasoningEffort"].clone(),
                        active: None,
                    },
                );
                if method == "session/load" {
                    // History is replayed as ACP notifications; the app decides whether to display it.
                    let history = self
                        .rpc("thread/read", json!({"threadId":id,"includeTurns":true}))
                        .await?;
                    for turn in history["thread"]["turns"].as_array().into_iter().flatten() {
                        for item in turn["items"].as_array().into_iter().flatten() {
                            replay_item(cx, &id, item)?;
                        }
                    }
                }
                let options = self.config_options(&id).await?;
                Ok(json!({"sessionId":id,"configOptions":options}))
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
            "session/set_config_option" => {
                let id = field(&p, "sessionId")?;
                let key = field(&p, "configId")?;
                if !matches!(key, "model" | "reasoning_effort") {
                    return Err(invalid("Unsupported config option"));
                }
                let options = self.config_options(id).await?;
                if !options.as_array().unwrap().iter().any(|o| {
                    o["id"] == key
                        && o["options"]
                            .as_array()
                            .is_some_and(|opts| opts.iter().any(|o| o["value"] == p["value"]))
                }) {
                    return Err(invalid("Unknown config value"));
                }
                {
                    let mut sessions = self.sessions.lock().unwrap();
                    let s = sessions
                        .get_mut(id)
                        .ok_or_else(|| invalid("Unknown session"))?;
                    if s.active.is_some() {
                        return Err(invalid("Cannot configure an active turn"));
                    }
                    if key == "model" {
                        s.model = p["value"].clone();
                        s.effort = Value::Null;
                    } else {
                        s.effort = p["value"].clone();
                    }
                }
                Ok(json!({"configOptions":self.config_options(id).await?}))
            }
            "session/prompt" => self.prompt(p).await,
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
        let current = models.iter().find(|m| m["model"] == s.model);
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
            json!({"id":"model","name":"Model","category":"model","type":"select","currentValue":s.model,"options":options}),
        ];
        if !efforts.is_empty() {
            result.push(json!({"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":s.effort.as_str().or_else(||current.and_then(|m|m["defaultReasoningEffort"].as_str())),"options":efforts}));
        }
        Ok(json!(result))
    }

    async fn prompt(&self, p: Value) -> Result<Value> {
        let id = field(&p, "sessionId")?;
        let input = prompt_input(&p["prompt"])?;
        let (tx, rx) = oneshot::channel();
        let (model, effort) = {
            let mut sessions = self.sessions.lock().unwrap();
            let s = sessions
                .get_mut(id)
                .ok_or_else(|| invalid("Unknown session"))?;
            if s.active.is_some() {
                return Err(invalid("A turn is already running"));
            }
            s.active = Some(ActiveTurn {
                id: None,
                cancelled: false,
                done: tx,
            });
            (s.model.clone(), s.effort.clone())
        };
        let started = self
            .rpc(
                "turn/start",
                json!({"threadId":id,"input":input,"model":model,"effort":effort}),
            )
            .await;
        let response = match started {
            Ok(v) => v,
            Err(e) => {
                self.sessions.lock().unwrap().get_mut(id).unwrap().active = None;
                return Err(e);
            }
        };
        let turn_id = field(&response["turn"], "id")?;
        let cancelled = {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(active) = sessions.get_mut(id).and_then(|s| s.active.as_mut()) {
                active.id = Some(turn_id.into());
                active.cancelled
            } else {
                false
            }
        };
        if cancelled {
            self.rpc("turn/interrupt", json!({"threadId":id,"turnId":turn_id}))
                .await?;
        }
        let completed = rx.await.map_err(|_| failure("Codex turn disconnected"))??;
        if completed["id"] != turn_id {
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
                    a.cancelled = true;
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
                } else {
                    let p = &v["params"];
                    let id = p["threadId"].as_str().unwrap_or("");
                    if method == "turn/completed" {
                        let active = self
                            .sessions
                            .lock()
                            .unwrap()
                            .get_mut(id)
                            .and_then(|s| s.active.take());
                        if let Some(active) = active {
                            let _ = active.done.send(Ok(p["turn"].clone()));
                        }
                    } else if self.sessions.lock().unwrap().contains_key(id) {
                        if let Some(event) = event_update(method, p) {
                            update(&cx, id, event)?;
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
        "item/agentMessage/delta"
        | "item/reasoning/textDelta"
        | "item/reasoning/summaryTextDelta" => Some(
            json!({"sessionUpdate":if method=="item/agentMessage/delta" {"agent_message_chunk"}else{"agent_thought_chunk"},"content":{"type":"text","text":p["delta"]}}),
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
        "mcpToolCall" | "dynamicToolCall" => "other",
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
        Some("agentMessage") => update(
            cx,
            id,
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":item["text"]}}),
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
