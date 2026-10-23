//! Small ACP v1 bridge to the native Claude Code CLI. Each ACP session runs one
//! `claude` process speaking the stream-json control protocol; Claude owns the agent
//! loop, permission policy and transcripts. History is read from those transcripts.
use agent_client_protocol::{Agent, Client, ConnectionTo, Error, Stdio, UntypedMessage};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio as ProcessStdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
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
fn name_values(values: &Value) -> Map<String, Value> {
    values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v["name"].as_str()?.into(), v["value"].clone())))
        .collect()
}
/// The cwd followed by any extra workspace roots, all absolute.
fn workspace_roots(p: &Value) -> Result<Vec<String>> {
    let mut roots = vec![field(p, "cwd")?.to_owned()];
    let extra = p
        .get("additionalDirectories")
        .or_else(|| p["_meta"].get("additionalRoots"));
    for root in extra.and_then(Value::as_array).into_iter().flatten() {
        let root = root
            .as_str()
            .ok_or_else(|| invalid("Workspace roots must be paths"))?;
        if !roots.iter().any(|r| r == root) {
            roots.push(root.into());
        }
    }
    if roots.iter().any(|r| !Path::new(r).is_absolute()) {
        return Err(invalid("Workspace roots must be absolute"));
    }
    Ok(roots)
}

/// Interactive-only commands that do nothing useful over ACP (same list as the npm adapter).
const UNSUPPORTED_COMMANDS: &[&str] = &[
    "clear",
    "cost",
    "keybindings-help",
    "login",
    "logout",
    "output-style:new",
    "release-notes",
    "todos",
];
const PAGE: usize = 1000;

#[derive(Default)]
struct State {
    models: Vec<Value>,
    model: String,
    mode: String,
    effort: String,
    allow_bypass: bool,
    turn: Option<Turn>,
    closed: bool,
    /// Tool uses by id, for presenting their results and permission requests.
    tools: HashMap<String, Value>,
    /// Tool calls shown without a result yet, closed when the turn ends.
    open: Vec<String>,
    /// API message ids whose text arrived as stream deltas.
    streamed: HashSet<String>,
    message_id: Option<String>,
    delivered: bool,
    used: Option<u64>,
    /// Prompt tokens of the streaming message; output tokens are added as they grow.
    prompt_tokens: u64,
    size: u64,
    last_model: Option<String>,
    last_error: Option<String>,
    /// TaskCreate/TaskUpdate state published as the ACP plan.
    tasks: Vec<(String, Value)>,
    /// Agent tool calls by id; their subagent's output is shown in the parent card.
    agents: HashMap<String, SubAgent>,
    /// The CLI reports idle; turns then end there, after any background subagents.
    state_events: bool,
}
struct Turn {
    cancelled: bool,
    /// The CLI went busy for this prompt, so its next idle ends it.
    running: bool,
    /// The latest `result`; a held turn may see several before idle.
    outcome: Option<Result<Value>>,
    done: Reply,
}
#[derive(Default)]
struct SubAgent {
    prompt: Option<String>,
    log: String,
    background: bool,
    done: bool,
}
struct Session {
    id: String,
    cwd: String,
    input: AsyncMutex<ChildStdin>,
    child: AsyncMutex<Child>,
    next: AtomicU64,
    pending: Mutex<HashMap<String, Reply>>,
    // Dropping an SDK request sends $/cancel_request, withdrawing the client's UI.
    questions: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    state: Mutex<State>,
}
struct Bridge {
    form: AtomicBool,
    terminal_output: AtomicBool,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

impl Session {
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
    async fn control(&self, subtype: &str, mut request: Value) -> Result<Value> {
        let id = format!("aw-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        request["subtype"] = json!(subtype);
        let result = async {
            self.write(json!({"type":"control_request","request_id":id,"request":request}))
                .await?;
            tokio::time::timeout(Duration::from_secs(60), rx)
                .await
                .map_err(|_| failure(format!("Claude {subtype} timed out")))?
                .map_err(|_| failure("Claude Code exited"))?
        }
        .await;
        self.pending.lock().unwrap().remove(&id);
        result
    }
    /// Learn the real context window; the next usage update carries it.
    fn refresh_window(self: &Arc<Self>) {
        let session = self.clone();
        tokio::spawn(async move {
            if let Ok(usage) = session.control("get_context_usage", json!({})).await {
                if let Some(size) = usage["rawMaxTokens"].as_u64().filter(|n| *n > 0) {
                    session.state.lock().unwrap().size = size;
                }
            }
        });
    }
    fn busy(&self) -> bool {
        self.state.lock().unwrap().turn.is_some()
    }
    async fn kill(&self) {
        for (_, task) in self.questions.lock().unwrap().drain() {
            task.abort();
        }
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    async fn read(
        self: Arc<Self>,
        output: ChildStdout,
        cx: ConnectionTo<Client>,
        bridge: Arc<Bridge>,
    ) {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let result = match v["type"].as_str() {
                Some("control_response") => {
                    let r = &v["response"];
                    if let Some(tx) = r["request_id"]
                        .as_str()
                        .and_then(|id| self.pending.lock().unwrap().remove(id))
                    {
                        let _ = tx.send(if r["subtype"] == "success" {
                            Ok(r.get("response").cloned().unwrap_or(json!({})))
                        } else {
                            Err(failure(
                                r["error"].as_str().unwrap_or("Claude request failed"),
                            ))
                        });
                    }
                    Ok(())
                }
                Some("control_request") => {
                    let id = v["request_id"].clone();
                    let key = id.to_string();
                    let (session, client, bridge) = (self.clone(), cx.clone(), bridge.clone());
                    self.questions
                        .lock()
                        .unwrap()
                        .retain(|_, task| !task.is_finished());
                    let task = tokio::spawn(async move {
                        let response = match session.answer(&v["request"], &client, &bridge).await {
                            Ok(r) => json!({"subtype":"success","request_id":id,"response":r}),
                            Err(e) => {
                                json!({"subtype":"error","request_id":id,"error":e.to_string()})
                            }
                        };
                        let _ = session
                            .write(json!({"type":"control_response","response":response}))
                            .await;
                    });
                    self.questions.lock().unwrap().insert(key, task);
                    Ok(())
                }
                Some("control_cancel_request") => {
                    if let Some(task) = self
                        .questions
                        .lock()
                        .unwrap()
                        .remove(&v["request_id"].to_string())
                    {
                        task.abort();
                    }
                    Ok(())
                }
                _ => self.event(&v, &cx, &bridge),
            };
            if result.is_err() {
                break;
            }
        }
        let turn = {
            let mut s = self.state.lock().unwrap();
            s.closed = true;
            s.turn.take()
        };
        self.pending.lock().unwrap().clear();
        if let Some(turn) = turn {
            let mut error = Error::internal_error().data(json!({"errorKind":"transport_lost"}));
            error.message = "Claude Code exited during the turn".into();
            let _ = turn.done.send(Err(error));
        }
    }

    /// Present a CLI request (permission, question, MCP form) to the ACP client.
    async fn answer(&self, r: &Value, cx: &ConnectionTo<Client>, bridge: &Bridge) -> Result<Value> {
        match r["subtype"].as_str() {
            Some("can_use_tool") => {
                let name = field(r, "tool_name")?;
                let input = &r["input"];
                let tool_id = r["tool_use_id"].as_str().unwrap_or_default();
                if name == "AskUserQuestion" && bridge.form.load(Ordering::Relaxed) {
                    return self.ask(input, tool_id, cx).await;
                }
                let suggestions = r["permission_suggestions"]
                    .as_array()
                    .filter(|s| !s.is_empty());
                let options = if name == "ExitPlanMode" {
                    let auto = self.supports_auto();
                    let mut options = vec![];
                    if auto {
                        options.push(json!({"optionId":"exit-plan-auto","name":"Yes, and use auto mode","kind":"allow_always"}));
                    }
                    options.push(json!({"optionId":"exit-plan-accept-edits","name":"Yes, auto-accept edits","kind":"allow_always"}));
                    options.push(json!({"optionId":"exit-plan-default","name":"Yes, manually approve edits","kind":"allow_once"}));
                    options.push(json!({"optionId":"reject","name":"No, keep planning","kind":"reject_once"}));
                    options
                } else {
                    let mut options =
                        vec![json!({"optionId":"allow-once","name":"Yes","kind":"allow_once"})];
                    if let Some(suggestions) = suggestions {
                        options.push(json!({"optionId":"allow-with-updates","name":always_label(suggestions),"kind":"allow_always"}));
                    }
                    options.push(json!({"optionId":"reject","name":"No","kind":"reject_once"}));
                    options
                };
                let mut tool = tool_call(name, tool_id, input, &self.cwd, bridge);
                if let Some(title) = r["title"].as_str() {
                    tool["title"] = json!(title);
                }
                for key in ["sessionUpdate", "_meta"] {
                    tool.as_object_mut().unwrap().remove(key);
                }
                let response = cx
                    .send_request(message(
                        "session/request_permission",
                        json!({"sessionId":self.id,"toolCall":tool,"options":options}),
                    ))
                    .block_task()
                    .await?;
                let chosen = response["outcome"]["optionId"].as_str().filter(|id| {
                    response["outcome"]["outcome"] == "selected"
                        && options.iter().any(|o| o["optionId"] == *id)
                });
                let allow = |updates: Option<Value>| {
                    let mut result =
                        json!({"behavior":"allow","updatedInput":input,"toolUseID":tool_id});
                    if let Some(updates) = updates {
                        result["updatedPermissions"] = updates;
                    }
                    result
                };
                Ok(match chosen {
                    Some("allow-once") => allow(None),
                    Some("allow-with-updates") => allow(suggestions.map(|s| json!(s))),
                    Some(id) if id.starts_with("exit-plan-") => {
                        let mode = match id {
                            "exit-plan-auto" => "auto",
                            "exit-plan-accept-edits" => "acceptEdits",
                            _ => "default",
                        };
                        self.set_mode_state(mode, cx)?;
                        allow(Some(
                            json!([{"type":"setMode","mode":mode,"destination":"session"}]),
                        ))
                    }
                    Some(_) if name == "ExitPlanMode" => {
                        json!({"behavior":"deny","message":"User chose to keep planning","interrupt":true,"toolUseID":tool_id})
                    }
                    Some(_) => {
                        json!({"behavior":"deny","message":"User refused permission to run tool","toolUseID":tool_id})
                    }
                    None => {
                        json!({"behavior":"deny","message":"The user cancelled this request","interrupt":true,"toolUseID":tool_id})
                    }
                })
            }
            Some("elicitation") if bridge.form.load(Ordering::Relaxed) && r["mode"] != "url" => {
                let response = cx
                    .send_request(message("elicitation/create",json!({"sessionId":self.id,"mode":"form","message":r["message"],"requestedSchema":r["requested_schema"]})))
                    .block_task()
                    .await?;
                let mut result = json!({"action":response["action"]});
                if response["action"] == "accept" {
                    result["content"] = response["content"].clone();
                }
                Ok(result)
            }
            Some("elicitation") => Ok(json!({"action":"decline"})),
            _ => Err(invalid("Unsupported control request")),
        }
    }

    /// AskUserQuestion as an ACP form; answers go back as the tool's updated input.
    async fn ask(&self, input: &Value, tool_id: &str, cx: &ConnectionTo<Client>) -> Result<Value> {
        let questions: Vec<&Value> = input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|q| {
                q["question"].is_string() && q["options"].as_array().is_some_and(|o| !o.is_empty())
            })
            .collect();
        if questions.is_empty() {
            return Ok(
                json!({"behavior":"deny","message":"AskUserQuestion called with no valid questions.","toolUseID":tool_id}),
            );
        }
        let single = questions.len() == 1;
        let mut properties = Map::new();
        for (i, q) in questions.iter().enumerate() {
            let choices: Vec<Value> = q["options"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| {
                    let mut choice = json!({"const":o["label"],"title":o["label"]});
                    if let Some(d) = o["description"].as_str() {
                        choice["description"] = json!(d);
                    }
                    choice
                })
                .collect();
            let mut schema = if q["multiSelect"] == true {
                json!({"type":"array","items":{"anyOf":choices}})
            } else {
                json!({"type":"string","oneOf":choices})
            };
            if let Some(header) = q["header"].as_str().filter(|h| !h.is_empty()) {
                schema["title"] = json!(header);
            }
            if !single {
                schema["description"] = q["question"].clone();
            }
            properties.insert(format!("question_{i}"), schema);
            properties.insert(format!("question_{i}_custom"), json!({"type":"string","title":"Other","description":"Type your own answer, or add a note to the option you chose above (optional)."}));
        }
        let text = if single {
            questions[0]["question"].as_str().unwrap()
        } else {
            "Please answer the following questions."
        };
        let response = cx
            .send_request(message("elicitation/create",json!({"sessionId":self.id,"toolCallId":tool_id,"mode":"form","message":text,"requestedSchema":{"type":"object","properties":properties}})))
            .block_task()
            .await?;
        let mut answers = Map::new();
        let mut notes = Map::new();
        match response["action"].as_str() {
            Some("accept") => {
                let content = &response["content"];
                for (i, q) in questions.iter().enumerate() {
                    let question = q["question"].as_str().unwrap().to_owned();
                    let custom = content[format!("question_{i}_custom")]
                        .as_str()
                        .unwrap_or("")
                        .trim();
                    let mut picks: Vec<String> = match &content[format!("question_{i}")] {
                        Value::Array(items) => items
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect(),
                        Value::String(s) if !s.is_empty() => vec![s.clone()],
                        _ => vec![],
                    };
                    if q["multiSelect"] == true && !custom.is_empty() {
                        picks.push(custom.to_owned());
                    } else if !custom.is_empty() {
                        if picks.is_empty() {
                            picks.push(custom.to_owned());
                        } else {
                            notes.insert(question.clone(), json!({"notes":custom}));
                        }
                    }
                    if !picks.is_empty() {
                        answers.insert(question, json!(picks.join(", ")));
                    }
                }
            }
            Some("decline") => {}
            _ => {
                return Ok(
                    json!({"behavior":"deny","message":"The user cancelled the question","interrupt":true,"toolUseID":tool_id}),
                );
            }
        }
        let mut updated = input.clone();
        updated["answers"] = json!(answers);
        if !notes.is_empty() {
            updated["annotations"] = json!(notes);
        }
        Ok(json!({"behavior":"allow","updatedInput":updated,"toolUseID":tool_id}))
    }

    fn supports_auto(&self) -> bool {
        let s = self.state.lock().unwrap();
        model_info(&s).is_some_and(|m| m["supportsAutoMode"] == true)
    }
    fn set_mode_state(&self, mode: &str, cx: &ConnectionTo<Client>) -> Result<()> {
        let options = {
            let mut s = self.state.lock().unwrap();
            if s.mode == mode {
                return Ok(());
            }
            s.mode = mode.into();
            config_options(&s)
        };
        update(
            cx,
            &self.id,
            json!({"sessionUpdate":"current_mode_update","currentModeId":mode}),
        )?;
        update(
            cx,
            &self.id,
            json!({"sessionUpdate":"config_option_update","configOptions":options}),
        )
    }
    fn usage(&self, cx: &ConnectionTo<Client>, extra: Value) -> Result<()> {
        let event = {
            let s = self.state.lock().unwrap();
            let mut event =
                json!({"sessionUpdate":"usage_update","used":s.used.unwrap_or(0),"size":s.size});
            if let Some(model) = &s.last_model {
                event["_meta"] = json!({"_claude/model":model});
            }
            for (k, v) in extra.as_object().into_iter().flatten() {
                if k == "_meta" {
                    if !event["_meta"].is_object() {
                        event["_meta"] = json!({});
                    }
                    event["_meta"]
                        .as_object_mut()
                        .unwrap()
                        .extend(v.as_object().unwrap().clone());
                } else {
                    event[k] = v.clone();
                }
            }
            event
        };
        update(cx, &self.id, event)
    }
    fn publish_plan(&self, cx: &ConnectionTo<Client>) -> Result<()> {
        let entries: Vec<Value> = self
            .state
            .lock()
            .unwrap()
            .tasks
            .iter()
            .map(|(_, t)| {
                let content = if t["status"] == "in_progress" {
                    t["activeForm"].as_str().or(t["subject"].as_str())
                } else {
                    t["subject"].as_str()
                };
                json!({"content":content.unwrap_or(""),"status":t["status"],"priority":"medium"})
            })
            .collect();
        update(
            cx,
            &self.id,
            json!({"sessionUpdate":"plan","entries":entries}),
        )
    }

    /// Translate one stream-json message into ACP session updates.
    fn event(&self, v: &Value, cx: &ConnectionTo<Client>, bridge: &Bridge) -> Result<()> {
        let id = &self.id;
        let parent = v["parent_tool_use_id"].as_str();
        match v["type"].as_str() {
            Some("stream_event") if parent.is_none() => {
                let e = &v["event"];
                match e["type"].as_str() {
                    Some("message_start") => {
                        let changed = {
                            let mut s = self.state.lock().unwrap();
                            s.message_id = e["message"]["id"].as_str().map(str::to_owned);
                            if let Some(model) = e["message"]["model"].as_str() {
                                s.last_model = Some(model.into());
                            }
                            let usage = &e["message"]["usage"];
                            s.prompt_tokens =
                                total_tokens(usage) - usage["output_tokens"].as_u64().unwrap_or(0);
                            let used = total_tokens(usage);
                            let changed = s.used != Some(used);
                            s.used = Some(used);
                            changed
                        };
                        if changed {
                            self.usage(cx, json!({}))?;
                        }
                    }
                    Some("content_block_delta") => {
                        let (kind, text) = match e["delta"]["type"].as_str() {
                            Some("text_delta") => ("agent_message_chunk", &e["delta"]["text"]),
                            Some("thinking_delta") => {
                                ("agent_thought_chunk", &e["delta"]["thinking"])
                            }
                            _ => return Ok(()),
                        };
                        let message_id = {
                            let mut s = self.state.lock().unwrap();
                            let message_id = s.message_id.clone();
                            if let Some(m) = &message_id {
                                s.streamed.insert(m.clone());
                            }
                            s.delivered = true;
                            message_id
                        };
                        update(
                            cx,
                            id,
                            json!({"sessionUpdate":kind,"messageId":message_id,"content":{"type":"text","text":text}}),
                        )?;
                    }
                    Some("message_delta") if e["usage"].is_object() => {
                        // Output tokens are cumulative; omitted input fields keep the start snapshot.
                        let changed = {
                            let mut s = self.state.lock().unwrap();
                            let used =
                                s.prompt_tokens + e["usage"]["output_tokens"].as_u64().unwrap_or(0);
                            let changed = s.used != Some(used);
                            s.used = Some(used);
                            changed
                        };
                        if changed {
                            self.usage(cx, json!({}))?;
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                let m = &v["message"];
                let message_id = m["id"].as_str().map(str::to_owned);
                let streamed = {
                    let mut s = self.state.lock().unwrap();
                    if parent.is_none() {
                        if let Some(error) = v["error"].as_str() {
                            s.last_error = Some(error.into());
                        }
                        if let Some(model) = m["model"].as_str().filter(|m| *m != "<synthetic>") {
                            s.last_model = Some(model.into());
                        }
                    }
                    message_id.as_ref().is_some_and(|m| s.streamed.contains(m))
                };
                for block in m["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some(kind @ ("text" | "thinking")) if parent.is_none() && !streamed => {
                            self.state.lock().unwrap().delivered = true;
                            let (kind, text) = if kind == "text" {
                                ("agent_message_chunk", &block["text"])
                            } else {
                                ("agent_thought_chunk", &block["thinking"])
                            };
                            update(
                                cx,
                                id,
                                json!({"sessionUpdate":kind,"messageId":message_id,"content":{"type":"text","text":text}}),
                            )?;
                        }
                        Some("text") if parent.is_some() => self.agent_text(
                            parent.unwrap(),
                            block["text"].as_str().unwrap_or(""),
                            cx,
                        )?,
                        Some("tool_use") => self.tool_use(block, parent, cx, bridge)?,
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        self.tool_result(block, &v["tool_use_result"], cx, bridge)?;
                    }
                }
            }
            Some("system") => match v["subtype"].as_str() {
                Some("session_state_changed") => {
                    let settle = {
                        let mut s = self.state.lock().unwrap();
                        s.state_events = true;
                        match (v["state"].as_str(), s.turn.as_mut()) {
                            (Some("running"), Some(turn)) => {
                                turn.running = true;
                                false
                            }
                            (Some("idle"), turn) => turn.is_none_or(|t| t.running),
                            _ => false,
                        }
                    };
                    if settle {
                        self.settle(cx)?;
                    }
                }
                Some("task_started") => {
                    let tool = v["tool_use_id"].as_str().unwrap_or_default();
                    if let Some(agent) = self.state.lock().unwrap().agents.get_mut(tool) {
                        agent.background = v["is_backgrounded"] == true;
                    }
                }
                Some("task_notification") => {
                    if let Some(tool) = v["tool_use_id"].as_str() {
                        let known = {
                            let mut s = self.state.lock().unwrap();
                            s.agents.get_mut(tool).map(|a| {
                                a.done = true;
                                if a.log.is_empty() {
                                    a.log = v["summary"].as_str().unwrap_or("").to_owned();
                                }
                            })
                        };
                        if known.is_some() {
                            let status = if v["status"] == "completed" {
                                "completed"
                            } else {
                                "failed"
                            };
                            self.agent_card(tool, status, cx)?;
                        }
                    }
                }
                Some("status") => {
                    if let Some(mode) = v["permissionMode"].as_str() {
                        self.set_mode_state(mode, cx)?;
                    }
                }
                Some("commands_changed") => {
                    update(
                        cx,
                        id,
                        json!({"sessionUpdate":"available_commands_update","availableCommands":commands(&v["commands"])}),
                    )?;
                }
                Some("local_command_output") => {
                    self.state.lock().unwrap().delivered = true;
                    update(
                        cx,
                        id,
                        json!({"sessionUpdate":"agent_message_chunk","messageId":v["uuid"],"content":{"type":"text","text":v["content"]}}),
                    )?;
                }
                Some("compact_boundary") => {
                    self.state.lock().unwrap().used = v["compact_metadata"]["post_tokens"].as_u64();
                    self.usage(cx, json!({}))?;
                }
                _ => {}
            },
            Some("rate_limit_event") => {
                let known = self.state.lock().unwrap().used.is_some();
                // A rejection can precede any usage in a fresh process; limit-resume needs it.
                if known || v["rate_limit_info"]["status"] != "allowed" {
                    self.usage(
                        cx,
                        json!({"_meta":{"_claude/rateLimit":v["rate_limit_info"]}}),
                    )?;
                }
            }
            Some("result") => self.finish(v, cx)?,
            _ => {}
        }
        Ok(())
    }

    fn tool_use(
        &self,
        block: &Value,
        parent: Option<&str>,
        cx: &ConnectionTo<Client>,
        bridge: &Bridge,
    ) -> Result<()> {
        let tool_id = block["id"].as_str().unwrap_or_default();
        let name = block["name"].as_str().unwrap_or_default();
        let input = &block["input"];
        {
            let mut s = self.state.lock().unwrap();
            s.tools.insert(
                tool_id.into(),
                json!({"name":name,"input":input,"parent":parent}),
            );
            s.open.push(tool_id.into());
        }
        if matches!(name, "Agent" | "Task") {
            let prompt = input["prompt"].as_str().map(str::to_owned);
            self.state.lock().unwrap().agents.insert(
                tool_id.into(),
                SubAgent {
                    prompt,
                    ..Default::default()
                },
            );
        }
        let mut call = tool_call(name, tool_id, input, &self.cwd, bridge);
        if let Some(parent) = parent {
            call["_meta"]["claudeCode"]["parentToolUseId"] = json!(parent);
        }
        update(cx, &self.id, call)?;
        if name == "TodoWrite" && parent.is_none() {
            let entries: Vec<Value> = input["todos"].as_array().into_iter().flatten().map(|t| {
                let content = if t["status"] == "in_progress" { t["activeForm"].as_str().or(t["content"].as_str()) } else { t["content"].as_str() };
                json!({"content":content.unwrap_or(""),"status":t["status"],"priority":"medium"})
            }).collect();
            update(
                cx,
                &self.id,
                json!({"sessionUpdate":"plan","entries":entries}),
            )?;
        } else if name == "TaskUpdate" && parent.is_none() {
            let changed = {
                let mut s = self.state.lock().unwrap();
                let task = input["taskId"].as_str().unwrap_or_default();
                if input["status"] == "deleted" {
                    s.tasks.retain(|(id, _)| id != task);
                    true
                } else if let Some((_, t)) = s.tasks.iter_mut().find(|(id, _)| id == task) {
                    for key in ["subject", "status", "activeForm"] {
                        if let Some(value) = input.get(key).filter(|v| v.is_string()) {
                            t[key] = value.clone();
                        }
                    }
                    true
                } else {
                    false
                }
            };
            if changed {
                self.publish_plan(cx)?;
            }
        }
        Ok(())
    }

    fn tool_result(
        &self,
        block: &Value,
        structured: &Value,
        cx: &ConnectionTo<Client>,
        bridge: &Bridge,
    ) -> Result<()> {
        let tool_id = block["tool_use_id"].as_str().unwrap_or_default();
        let tool = {
            let mut s = self.state.lock().unwrap();
            s.open.retain(|id| id != tool_id);
            s.tools.get(tool_id).cloned()
        };
        let tool = tool.unwrap_or_else(|| json!({"name":""}));
        let background = self
            .state
            .lock()
            .unwrap()
            .agents
            .get(tool_id)
            .is_some_and(|a| a.background && !a.done);
        if background && block["is_error"] != true {
            // The launch acknowledgement; the subagent reports through task_notification.
            return self.agent_card(tool_id, "in_progress", cx);
        }
        if tool["name"] == "TaskCreate" && block["is_error"] != true {
            let text = text_of(&block["content"]);
            if let Some((task, _)) = text
                .trim()
                .strip_prefix("Task #")
                .and_then(|t| t.split_once(" created successfully"))
            {
                let input = &tool["input"];
                self.state.lock().unwrap().tasks.push((task.into(), json!({"subject":input["subject"],"activeForm":input["activeForm"],"status":"pending"})));
                self.publish_plan(cx)?;
            }
        }
        for event in tool_result(
            &tool,
            tool_id,
            block,
            structured,
            bridge.terminal_output.load(Ordering::Relaxed),
        ) {
            let mut event = event;
            if let Some(parent) = tool["parent"].as_str() {
                event["_meta"]["claudeCode"]["parentToolUseId"] = json!(parent);
            }
            update(cx, &self.id, event)?;
        }
        Ok(())
    }

    fn finish(&self, v: &Value, cx: &ConnectionTo<Client>) -> Result<()> {
        let (cancelled, delivered, last_error) = {
            let mut s = self.state.lock().unwrap();
            let model = s.last_model.clone();
            if let Some(usage) = v["modelUsage"].as_object() {
                let window = usage
                    .iter()
                    .find(|(k, _)| model.as_ref().is_some_and(|m| k.starts_with(m.as_str())))
                    .or_else(|| (usage.len() == 1).then(|| usage.iter().next().unwrap()))
                    .and_then(|(_, u)| u["contextWindow"].as_u64())
                    .filter(|n| *n > 0);
                if let Some(window) = window {
                    s.size = window;
                }
            }
            s.streamed.clear();
            let delivered = std::mem::take(&mut s.delivered);
            (
                s.turn.as_ref().is_some_and(|t| t.cancelled),
                delivered,
                s.last_error.take(),
            )
        };
        if self.state.lock().unwrap().used.is_some() {
            self.usage(
                cx,
                json!({"cost":{"amount":v["total_cost_usd"],"currency":"USD"}}),
            )?;
        }
        // A local command such as /context answers in `result` without model output.
        if !delivered
            && v["subtype"] == "success"
            && v["is_error"] != true
            && v["usage"]["output_tokens"].as_u64().unwrap_or(0) == 0
        {
            if let Some(text) = v["result"].as_str().filter(|t| !t.is_empty()) {
                update(
                    cx,
                    &self.id,
                    json!({"sessionUpdate":"agent_message_chunk","messageId":v["uuid"],"content":{"type":"text","text":text}}),
                )?;
            }
        }
        let text = v["result"].as_str().map(str::to_owned).unwrap_or_else(|| {
            v["errors"]
                .as_array()
                .map(|e| {
                    e.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| v["subtype"].as_str().unwrap_or("error").to_owned())
        });
        let outcome = if cancelled {
            Ok(json!({"stopReason":"cancelled"}))
        } else if v["is_error"] == true {
            if text.contains("Please run /login") {
                Err(Error::auth_required())
            } else {
                let mut error = Error::internal_error().data(json!({"errorKind":last_error}));
                error.message = text;
                Err(error)
            }
        } else if v["stop_reason"] == "max_tokens" {
            Ok(json!({"stopReason":"max_tokens"}))
        } else if matches!(
            v["subtype"].as_str(),
            Some("success" | "error_during_execution")
        ) {
            Ok(json!({"stopReason":"end_turn"}))
        } else {
            Ok(json!({"stopReason":"max_turn_requests"}))
        };
        let settle = {
            let mut s = self.state.lock().unwrap();
            if let Some(turn) = s.turn.as_mut() {
                turn.outcome = Some(outcome);
            }
            !s.state_events
        };
        if settle { self.settle(cx) } else { Ok(()) }
    }

    /// End the prompt: close what never finished and report the latest outcome.
    fn settle(&self, cx: &ConnectionTo<Client>) -> Result<()> {
        let (turn, open, agents) = {
            let mut s = self.state.lock().unwrap();
            let agents: Vec<String> = s
                .agents
                .iter()
                .filter(|(_, a)| a.background && !a.done)
                .map(|(id, _)| id.clone())
                .collect();
            (s.turn.take(), std::mem::take(&mut s.open), agents)
        };
        let cancelled = turn.as_ref().is_some_and(|t| t.cancelled);
        for tool in open {
            let reason = if cancelled {
                "The turn was cancelled before this tool call returned a result."
            } else {
                "Claude ended the turn without returning a result for this tool."
            };
            update(
                cx,
                &self.id,
                json!({"sessionUpdate":"tool_call_update","toolCallId":tool,"status":"failed","content":[text_content(reason)]}),
            )?;
        }
        for agent in agents {
            self.agent_card(&agent, "failed", cx)?;
        }
        self.state.lock().unwrap().agents.clear();
        for (_, task) in self.questions.lock().unwrap().drain() {
            task.abort();
        }
        if let Some(turn) = turn {
            let outcome = match turn.outcome {
                _ if cancelled => Ok(json!({"stopReason":"cancelled"})),
                Some(outcome) => outcome,
                None => Ok(json!({"stopReason":"end_turn"})),
            };
            let _ = turn.done.send(outcome);
        }
        Ok(())
    }

    fn agent_text(&self, parent: &str, text: &str, cx: &ConnectionTo<Client>) -> Result<()> {
        let known = {
            let mut s = self.state.lock().unwrap();
            s.agents.get_mut(parent).map(|a| {
                if !a.log.is_empty() {
                    a.log.push_str("\n\n");
                }
                a.log.push_str(text);
            })
        };
        match known {
            Some(()) if !text.is_empty() => self.agent_card(parent, "in_progress", cx),
            _ => Ok(()),
        }
    }
    /// The Agent tool card: its prompt, then what the subagent has said so far.
    fn agent_card(&self, id: &str, status: &str, cx: &ConnectionTo<Client>) -> Result<()> {
        let content: Vec<Value> = {
            let s = self.state.lock().unwrap();
            let Some(agent) = s.agents.get(id) else {
                return Ok(());
            };
            agent
                .prompt
                .iter()
                .chain(Some(&agent.log).filter(|l| !l.is_empty()))
                .map(|t| text_content(t))
                .collect()
        };
        update(
            cx,
            &self.id,
            json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":status,"content":content}),
        )
    }
}

impl Bridge {
    fn session(&self, p: &Value) -> Result<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .get(field(p, "sessionId")?)
            .cloned()
            .ok_or_else(|| invalid("Unknown session"))
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
                let caps = &p["clientCapabilities"];
                self.form
                    .store(caps["elicitation"]["form"].is_object(), Ordering::Relaxed);
                self.terminal_output
                    .store(caps["_meta"]["terminal_output"] == true, Ordering::Relaxed);
                Ok(
                    json!({"protocolVersion":1,"agentInfo":{"name":"agentworks-claude-acp","title":"Claude Agent","version":env!("CARGO_PKG_VERSION")},
                    "agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"embeddedContext":true},"mcpCapabilities":{"http":true,"sse":true},
                        "sessionCapabilities":{"list":{},"resume":{},"close":{},"delete":{},"fork":{},"additionalDirectories":{}}},
                    "authMethods":[]}),
                )
            }
            "authenticate" => Err(invalid(
                "Use the host Claude sign-in (claude auth login) or ANTHROPIC_API_KEY",
            )),
            "session/new" | "session/resume" | "session/load" | "session/fork" => {
                self.open(method, p, cx).await
            }
            "session/close" | "session/delete" => {
                let id = field(&p, "sessionId")?;
                let live = self.sessions.lock().unwrap().get(id).cloned();
                if let Some(live) = live {
                    if live.busy() {
                        return Err(invalid(
                            "Cancel the active turn before closing or deleting its session",
                        ));
                    }
                    self.sessions.lock().unwrap().remove(id);
                    live.kill().await;
                }
                if method == "session/delete" {
                    if let Some(path) = find_transcript(id) {
                        std::fs::remove_file(&path).map_err(failure)?;
                        let _ = std::fs::remove_dir_all(path.with_extension(""));
                    }
                }
                Ok(json!({}))
            }
            "session/list" => list_sessions(p["cwd"].as_str(), p["cursor"].as_str()),
            "session/set_config_option" | "session/set_mode" => {
                let s = self.session(&p)?;
                let (key, value) = if method == "session/set_mode" {
                    ("mode", p["modeId"].clone())
                } else {
                    (field(&p, "configId")?, p["value"].clone())
                };
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("Unknown config value"))?
                    .to_owned();
                let offered = config_options(&s.state.lock().unwrap())
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|o| {
                        o["id"] == key
                            && o["options"].as_array().is_some_and(|opts| {
                                opts.iter().any(|o| o["value"] == value.as_str())
                            })
                    });
                if !offered {
                    return Err(invalid("Unknown config value"));
                }
                match key {
                    "mode" => {
                        let mode = if value == "auto" && !s.supports_auto() {
                            "acceptEdits"
                        } else {
                            value.as_str()
                        };
                        s.control("set_permission_mode", json!({"mode":mode}))
                            .await?;
                        s.set_mode_state(mode, cx)?;
                    }
                    "model" => {
                        s.control("set_model", json!({"model":value})).await?;
                        let (effort_reset, clamp) = {
                            let mut st = s.state.lock().unwrap();
                            st.model = value.clone();
                            st.size = default_window(model_info(&st), &value);
                            let effort_ok = st.effort == "default"
                                || efforts(model_info(&st)).iter().any(|e| e == &st.effort);
                            if !effort_ok {
                                st.effort = "default".into();
                            }
                            (
                                !effort_ok,
                                st.mode == "auto"
                                    && model_info(&st)
                                        .is_none_or(|m| m["supportsAutoMode"] != true),
                            )
                        };
                        if effort_reset {
                            s.control(
                                "apply_flag_settings",
                                json!({"settings":{"effortLevel":null}}),
                            )
                            .await?;
                        }
                        if clamp {
                            s.control("set_permission_mode", json!({"mode":"acceptEdits"}))
                                .await?;
                            s.set_mode_state("acceptEdits", cx)?;
                        }
                        s.refresh_window();
                    }
                    "effort" => {
                        let level = if value == "default" {
                            Value::Null
                        } else {
                            json!(value)
                        };
                        s.control(
                            "apply_flag_settings",
                            json!({"settings":{"effortLevel":level}}),
                        )
                        .await?;
                        s.state.lock().unwrap().effort = value.clone();
                    }
                    _ => return Err(invalid("Unsupported config option")),
                }
                let options = config_options(&s.state.lock().unwrap());
                update(
                    cx,
                    &s.id,
                    json!({"sessionUpdate":"config_option_update","configOptions":options}),
                )?;
                Ok(if method == "session/set_mode" {
                    json!({})
                } else {
                    json!({"configOptions":options})
                })
            }
            "session/prompt" => {
                let s = self.session(&p)?;
                let content = prompt_content(&p["prompt"])?;
                let (tx, rx) = oneshot::channel();
                {
                    let mut st = s.state.lock().unwrap();
                    if st.closed {
                        return Err(failure("Claude Code exited; reopen the session"));
                    }
                    if st.turn.is_some() {
                        return Err(invalid("A turn is already running"));
                    }
                    st.turn = Some(Turn {
                        cancelled: false,
                        running: false,
                        outcome: None,
                        done: tx,
                    });
                    st.delivered = false;
                    st.last_error = None;
                }
                let sent = s.write(json!({"type":"user","message":{"role":"user","content":content},"parent_tool_use_id":null,"session_id":s.id,"origin":{"kind":"human"}})).await;
                if let Err(e) = sent {
                    s.state.lock().unwrap().turn = None;
                    return Err(e);
                }
                rx.await.map_err(|_| failure("Claude turn disconnected"))?
            }
            _ => Err(Error::method_not_found()),
        }
    }

    async fn open(
        self: &Arc<Self>,
        method: &str,
        p: Value,
        cx: &ConnectionTo<Client>,
    ) -> Result<Value> {
        let roots = workspace_roots(&p)?;
        let cwd = roots[0].clone();
        if !Path::new(&cwd).is_dir() {
            return Err(invalid("cwd must be an existing directory"));
        }
        let requested = p["sessionId"].as_str().map(str::to_owned);
        let (id, resume) = match method {
            "session/new" => (uuid::Uuid::new_v4().to_string(), false),
            "session/fork" => (
                fork_transcript(
                    field(&p, "sessionId")?,
                    &cwd,
                    &p["_meta"]["jetbrains"]["air"]["fork"],
                )?,
                true,
            ),
            _ => {
                let id = requested.clone().unwrap_or_default();
                let live = self.sessions.lock().unwrap().get(&id).cloned();
                if let Some(live) = live {
                    if live.busy() {
                        return Err(invalid("Cannot load an active session"));
                    }
                    if method == "session/load" {
                        replay(&live, cx, self)?;
                    }
                    return Ok(session_response(&live));
                }
                if !project_dir(&cwd).join(format!("{id}.jsonl")).is_file() {
                    return Err(Error::resource_not_found(Some(id)));
                }
                (id, true)
            }
        };
        let settings = settings(&cwd);
        let allow_bypass = (unsafe { libc::geteuid() } != 0
            || std::env::var_os("IS_SANDBOX").is_some())
            && settings["permissions"]["disableBypassPermissionsMode"] != "disable";
        let mut command = Command::new(
            std::env::var_os("CLAUDE_CODE_EXECUTABLE").unwrap_or_else(|| "claude".into()),
        );
        command.args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--setting-sources",
            "user,project,local",
        ]);
        command.args([if resume { "--resume" } else { "--session-id" }, &id]);
        for root in &roots[1..] {
            command.args(["--add-dir", root]);
        }
        let servers = mcp_servers(&p)?;
        if !servers.is_empty() {
            command.args(["--mcp-config", &json!({"mcpServers":servers}).to_string()]);
        }
        if allow_bypass {
            command.arg("--allow-dangerously-skip-permissions");
        }
        if !self.form.load(Ordering::Relaxed) {
            command.args(["--disallowedTools", "AskUserQuestion"]);
        }
        let mut child = command
            .current_dir(&cwd)
            .env("CLAUDE_CODE_ENTRYPOINT", "sdk-rust")
            .env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1")
            .env_remove("CLAUDECODE")
            .stdin(ProcessStdio::piped())
            .stdout(ProcessStdio::piped())
            .stderr(ProcessStdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(failure)?;
        let output = child.stdout.take().unwrap();
        let session = Arc::new(Session {
            id: id.clone(),
            cwd: cwd.clone(),
            input: AsyncMutex::new(child.stdin.take().unwrap()),
            child: AsyncMutex::new(child),
            next: AtomicU64::new(1),
            pending: Mutex::default(),
            questions: Mutex::default(),
            state: Mutex::new(State {
                allow_bypass,
                effort: "default".into(),
                ..Default::default()
            }),
        });
        tokio::spawn(session.clone().read(output, cx.clone(), self.clone()));
        let init = match session
            .control("initialize", json!({"forwardSubagentText":true}))
            .await
        {
            Ok(init) => init,
            Err(e) => {
                session.kill().await;
                return Err(e);
            }
        };
        {
            let mut st = session.state.lock().unwrap();
            st.models = init["models"].as_array().cloned().unwrap_or_default();
            st.model = current_model(&st.models, &settings);
            st.mode = init["current_permission_mode"]
                .as_str()
                .unwrap_or("default")
                .into();
            if let Some(effort) = settings["effortLevel"]
                .as_str()
                .filter(|e| efforts(model_info(&st)).iter().any(|l| l == e))
            {
                st.effort = effort.into();
            }
            st.size = default_window(model_info(&st), &st.model.clone());
        }
        if session.state.lock().unwrap().mode == "auto" && !session.supports_auto() {
            session
                .control("set_permission_mode", json!({"mode":"acceptEdits"}))
                .await?;
            session.state.lock().unwrap().mode = "acceptEdits".into();
        }
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session.clone());
        session.refresh_window();
        if method == "session/load" {
            replay(&session, cx, self)?;
        }
        update(
            cx,
            &id,
            json!({"sessionUpdate":"available_commands_update","availableCommands":commands(&init["commands"])}),
        )?;
        Ok(session_response(&session))
    }
}

fn session_response(s: &Session) -> Value {
    let st = s.state.lock().unwrap();
    json!({"sessionId":s.id,"configOptions":config_options(&st),"modes":{"currentModeId":st.mode,"availableModes":modes(st.allow_bypass).iter().map(|(id,name,description)|json!({"id":id,"name":name,"description":description})).collect::<Vec<_>>()}})
}
fn modes(allow_bypass: bool) -> Vec<(&'static str, &'static str, &'static str)> {
    let mut modes = vec![
        ("default", "Manual", "Always ask before making changes"),
        (
            "acceptEdits",
            "Accept edits",
            "Automatically accept all file edits",
        ),
        ("plan", "Plan", "Create a plan before making changes"),
        ("auto", "Auto", "Claude handles permission decisions"),
    ];
    if allow_bypass {
        modes.push((
            "bypassPermissions",
            "Bypass permissions",
            "Accepts all permissions",
        ));
    }
    modes
}
fn model_info(s: &State) -> Option<&Value> {
    s.models.iter().find(|m| m["value"] == s.model.as_str())
}
fn efforts(info: Option<&Value>) -> Vec<String> {
    info.filter(|m| m["supportsEffort"] == true)
        .and_then(|m| m["supportedEffortLevels"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|l| l.as_str().map(str::to_owned))
        .collect()
}
fn default_window(info: Option<&Value>, model: &str) -> u64 {
    let resolved = info
        .and_then(|m| m["resolvedModel"].as_str())
        .unwrap_or(model);
    if resolved.contains("[1m]") || model.contains("[1m]") {
        1_000_000
    } else {
        200_000
    }
}
fn config_options(s: &State) -> Value {
    let mode_options: Vec<_> = modes(s.allow_bypass)
        .iter()
        .map(|(id, name, description)| json!({"value":id,"name":name,"description":description}))
        .collect();
    let mut models: Vec<_> = s
        .models
        .iter()
        .map(|m| json!({"value":m["value"],"name":m["displayName"],"description":m["description"]}))
        .collect();
    if model_info(s).is_none() && !s.model.is_empty() {
        models.push(json!({"value":s.model,"name":s.model}));
    }
    let mut options = vec![
        json!({"id":"mode","name":"Mode","description":"Session permission mode","category":"mode","type":"select","currentValue":s.mode,"options":mode_options}),
        json!({"id":"model","name":"Model","description":"AI model to use","category":"model","type":"select","currentValue":s.model,"options":models}),
    ];
    let levels = efforts(model_info(s));
    if !levels.is_empty() {
        let mut choices = vec![json!({"value":"default","name":"Default"})];
        choices.extend(levels.iter().map(|l| {
            let mut name: Vec<char> = l.chars().collect();
            name[0] = name[0].to_ascii_uppercase();
            json!({"value":l,"name":name.into_iter().collect::<String>()})
        }));
        options.push(json!({"id":"effort","name":"Effort","description":"Available effort levels for this model","category":"thought_level","type":"select","currentValue":s.effort,"options":choices}));
    }
    json!(options)
}
fn current_model(models: &[Value], settings: &Value) -> String {
    let preference = std::env::var("ANTHROPIC_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .or_else(|| settings["model"].as_str().map(str::to_owned));
    if let Some(preference) = preference {
        let found = models.iter().find(|m| {
            [&m["value"], &m["resolvedModel"]].iter().any(|v| {
                v.as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(&preference))
            })
        });
        return found
            .and_then(|m| m["value"].as_str())
            .unwrap_or(&preference)
            .to_owned();
    }
    models
        .first()
        .and_then(|m| m["value"].as_str())
        .unwrap_or("default")
        .to_owned()
}
fn commands(commands: &Value) -> Vec<Value> {
    commands
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let mut name = c["name"].as_str()?.to_owned();
            if UNSUPPORTED_COMMANDS.contains(&name.as_str()) {
                return None;
            }
            if let Some(prompt) = name.strip_suffix(" (MCP)") {
                name = format!("mcp:{prompt}");
            }
            let hint = match &c["argumentHint"] {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Array(a) if !a.is_empty() => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")),
                _ => None,
            };
            Some(json!({"name":name,"description":c["description"].as_str().unwrap_or(""),"input":hint.map(|h|json!({"hint":h}))}))
        })
        .collect()
}
fn mcp_servers(p: &Value) -> Result<Map<String, Value>> {
    let mut servers = Map::new();
    for server in p["mcpServers"].as_array().into_iter().flatten() {
        let value = match server["type"].as_str() {
            None | Some("stdio") => {
                json!({"type":"stdio","command":field(server,"command")?,"args":server["args"].as_array().cloned().unwrap_or_default(),"env":name_values(&server["env"])})
            }
            Some(kind @ ("http" | "sse")) => {
                json!({"type":kind,"url":field(server,"url")?,"headers":name_values(&server["headers"])})
            }
            _ => return Err(invalid("Unsupported MCP transport")),
        };
        servers.insert(field(server, "name")?.into(), value);
    }
    Ok(servers)
}
fn prompt_content(blocks: &Value) -> Result<Vec<Value>> {
    let mut content = vec![];
    let mut context = vec![];
    for b in blocks
        .as_array()
        .ok_or_else(|| invalid("prompt must be an array"))?
    {
        match b["type"].as_str() {
            Some("text") => {
                let text = field(b, "text")?;
                // /mcp:server:prompt args -> /server:prompt (MCP) args, as Claude names MCP prompts.
                let text = match text.strip_prefix("/mcp:").and_then(|t| t.split_once(':')) {
                    Some((server, rest)) if !server.contains(char::is_whitespace) => {
                        let (command, args) = rest.split_once(' ').map_or((rest, None), |(c, a)| (c, Some(a)));
                        format!("/{server}:{command} (MCP){}", args.map(|a| format!(" {a}")).unwrap_or_default())
                    }
                    _ => text.to_owned(),
                };
                content.push(json!({"type":"text","text":text}));
            }
            Some("image") => content.push(json!({"type":"image","source":{"type":"base64","data":field(b,"data")?,"media_type":field(b,"mimeType")?}})),
            Some("resource_link") => content.push(json!({"type":"text","text":field(b,"uri")?})),
            Some("resource") => {
                let uri = field(&b["resource"], "uri")?;
                content.push(json!({"type":"text","text":uri}));
                context.push(json!({"type":"text","text":format!("\n<context ref=\"{uri}\">\n{}\n</context>",field(&b["resource"],"text")?)}));
            }
            _ => return Err(invalid("Unsupported prompt content")),
        }
    }
    content.extend(context);
    Ok(content)
}

fn always_label(suggestions: &[Value]) -> String {
    let mut parts = vec![];
    let mut session = true;
    for s in suggestions {
        session &= s["destination"] == "session";
        match s["type"].as_str() {
            Some("addRules") => {
                parts.extend(s["rules"].as_array().into_iter().flatten().map(|r| {
                    let tool = r["toolName"].as_str().unwrap_or("tool");
                    match r["ruleContent"].as_str() {
                        Some(content) => format!("{tool}({content})"),
                        None => tool.to_owned(),
                    }
                }))
            }
            Some("addDirectories") => parts.extend(
                s["directories"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|d| d.as_str().map(|d| format!("{d}/"))),
            ),
            Some("setMode") if s["mode"] == "acceptEdits" => parts.push("all edits".into()),
            _ => {}
        }
    }
    let what = if parts.is_empty() || parts.join(", ").len() > 60 {
        "similar requests".into()
    } else {
        parts.join(", ")
    };
    format!(
        "Yes, and allow {what}{}",
        if session {
            " for this session"
        } else {
            " from now on"
        }
    )
}

fn text_content(text: &str) -> Value {
    json!({"type":"content","content":{"type":"text","text":text}})
}
fn display_path(path: &str, cwd: &str) -> String {
    Path::new(path)
        .strip_prefix(cwd)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| path.to_owned())
}
fn total_tokens(usage: &Value) -> u64 {
    [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]
    .iter()
    .map(|k| usage[*k].as_u64().unwrap_or(0))
    .sum()
}
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The ACP presentation of a Claude tool use, keyed by Claude's tool names.
fn tool_call(name: &str, id: &str, input: &Value, cwd: &str, bridge: &Bridge) -> Value {
    let s = |k: &str| input[k].as_str();
    let path = |k: &str| s(k).map(|p| display_path(p, cwd));
    let mut content = vec![];
    let mut locations = None;
    let mut meta = json!({"claudeCode":{"toolName":name}});
    let (title, kind) = match name {
        "Bash" | "PowerShell" => {
            if let Some(d) = s("description") {
                content.push(text_content(d));
            }
            if bridge.terminal_output.load(Ordering::Relaxed) {
                meta["terminal_info"] = json!({"terminal_id":id});
                content.push(json!({"type":"terminal","terminalId":id}));
            }
            (s("command").unwrap_or("Terminal").to_owned(), "execute")
        }
        "Read" => {
            let range = match (
                input["offset"].as_u64(),
                input["limit"].as_u64().filter(|l| *l > 0),
            ) {
                (offset, Some(limit)) => format!(
                    " ({} - {})",
                    offset.unwrap_or(1),
                    offset.unwrap_or(1) + limit - 1
                ),
                (Some(offset), None) => format!(" (from line {offset})"),
                _ => String::new(),
            };
            locations = s("file_path")
                .map(|p| json!([{"path":p,"line":input["offset"].as_u64().unwrap_or(1)}]));
            (
                format!("Read {}{range}", path("file_path").unwrap_or("File".into())),
                "read",
            )
        }
        "Write" | "Edit" => {
            let file = s("file_path").or(s("path"));
            if let Some(file) = file {
                locations = Some(json!([{"path":file}]));
                let (old, new) = if name == "Write" {
                    (Value::Null, input["content"].clone())
                } else {
                    (input["old_string"].clone(), input["new_string"].clone())
                };
                content.push(json!({"type":"diff","path":file,"oldText":old.as_str().filter(|o|!o.is_empty()),"newText":new.as_str().unwrap_or("")}));
            }
            (
                file.map(|f| format!("{name} {}", display_path(f, cwd)))
                    .unwrap_or_else(|| name.into()),
                "edit",
            )
        }
        "NotebookEdit" => {
            locations = s("notebook_path").map(|p| json!([{"path":p}]));
            (
                path("notebook_path")
                    .map(|p| format!("Edit notebook {p}"))
                    .unwrap_or("Edit notebook".into()),
                "edit",
            )
        }
        "Glob" => {
            locations = s("path").map(|p| json!([{"path":p}]));
            let mut title = "Find".to_owned();
            for k in ["path", "pattern"] {
                if let Some(v) = s(k) {
                    title.push_str(&format!(" `{v}`"));
                }
            }
            (title, "search")
        }
        "Grep" => {
            let mut title = "grep".to_owned();
            if let Some(glob) = s("glob") {
                title.push_str(&format!(" --include=\"{glob}\""));
            }
            if let Some(pattern) = s("pattern") {
                title.push_str(&format!(" \"{pattern}\""));
            }
            if let Some(path) = s("path") {
                title.push_str(&format!(" {path}"));
            }
            (title, "search")
        }
        "WebFetch" => (
            s("url")
                .map(|u| format!("Fetch {u}"))
                .unwrap_or("Fetch".into()),
            "fetch",
        ),
        "WebSearch" => (
            s("query")
                .map(|q| format!("Search \"{q}\""))
                .unwrap_or("Web search".into()),
            "fetch",
        ),
        "Agent" | "Task" => {
            if let Some(prompt) = s("prompt") {
                content.push(text_content(prompt));
            }
            (s("description").unwrap_or("Task").to_owned(), "think")
        }
        "TodoWrite" => ("Update TODOs".into(), "think"),
        "TaskCreate" => (
            s("subject")
                .map(|t| format!("Create task: {t}"))
                .unwrap_or("Create task".into()),
            "think",
        ),
        "TaskUpdate" => (
            s("subject")
                .map(|t| format!("Update task: {t}"))
                .unwrap_or("Update task".into()),
            "think",
        ),
        "TaskList" => ("List tasks".into(), "think"),
        "TaskGet" => ("Get task".into(), "think"),
        "ExitPlanMode" => {
            if let Some(plan) = s("plan") {
                content.push(text_content(plan));
            }
            ("Approve Plan".into(), "switch_mode")
        }
        "EnterPlanMode" => ("Enter plan mode".into(), "switch_mode"),
        "AskUserQuestion" => {
            let questions = input["questions"].as_array().cloned().unwrap_or_default();
            let single = (questions.len() == 1)
                .then(|| questions[0]["question"].as_str())
                .flatten();
            (
                single.unwrap_or("Asking for your input").to_owned(),
                "other",
            )
        }
        "Skill" => (
            s("skill")
                .map(|k| format!("Load skill: {k}"))
                .unwrap_or("Load skill".into()),
            "other",
        ),
        _ => {
            if input.as_object().is_some_and(|o| !o.is_empty()) {
                content.push(text_content(&format!(
                    "```json\n{}\n```",
                    serde_json::to_string_pretty(input).unwrap_or_default()
                )));
            }
            (
                if name.is_empty() {
                    "Unknown Tool".into()
                } else {
                    name.to_owned()
                },
                "other",
            )
        }
    };
    let mut call = json!({"sessionUpdate":"tool_call","toolCallId":id,"title":title,"kind":kind,"status":"pending","rawInput":input,"content":content,"_meta":meta});
    if let Some(locations) = locations {
        call["locations"] = locations;
    }
    call
}

fn tool_result(
    tool: &Value,
    id: &str,
    block: &Value,
    structured: &Value,
    terminal: bool,
) -> Vec<Value> {
    let error = block["is_error"] == true;
    let name = tool["name"].as_str().unwrap_or_default();
    let mut done = json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":if error {"failed"} else {"completed"},"_meta":{"claudeCode":{"toolName":name}}});
    if matches!(name, "Bash" | "PowerShell") {
        let (mut output, mut code) = (
            text_of(&block["content"]),
            if error { None } else { Some(0) },
        );
        if structured["stdout"].is_string() && structured["stderr"].is_string() {
            output = [&structured["stdout"], &structured["stderr"]]
                .iter()
                .filter_map(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if structured["interrupted"] == true {
                output.push_str("\n[Command was aborted before completion]");
                code = None;
            }
        }
        if error {
            code = output
                .strip_prefix("Exit code ")
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse::<i64>().ok());
        }
        if terminal {
            done["_meta"]["terminal_exit"] =
                json!({"terminal_id":id,"exit_code":code,"signal":null});
            return vec![
                json!({"sessionUpdate":"tool_call_update","toolCallId":id,"_meta":{"terminal_output_delta":{"terminal_id":id,"data":output}}}),
                done,
            ];
        }
        if !output.trim().is_empty() {
            done["content"] = json!([text_content(&format!(
                "```console\n{}\n```",
                output.trim_end()
            ))]);
        }
        return vec![done];
    }
    if matches!(name, "Edit" | "Write") && !error {
        return vec![done];
    }
    let content: Vec<Value> = match &block["content"] {
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|b| b["text"].as_str().map(str::to_owned))
            .collect(),
        _ => vec![],
    }
    .into_iter()
    .map(|t| text_content(&if error { format!("```\n{t}\n```") } else { t }))
    .collect();
    if !content.is_empty() {
        done["content"] = json!(content);
    }
    vec![done]
}

// Transcripts: <config>/projects/<sanitized cwd>/<session>.jsonl, as Claude Code writes them.
fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude")
        })
}
/// Claude's project directory name: non-alphanumerics become '-', long names get a hash.
fn project_dir(cwd: &str) -> PathBuf {
    let units: Vec<u16> = cwd.encode_utf16().collect();
    let name: String = units
        .iter()
        .map(|&u| {
            if (u as u8 as u16) == u && (u as u8).is_ascii_alphanumeric() {
                u as u8 as char
            } else {
                '-'
            }
        })
        .collect();
    let name = if name.len() <= 200 {
        name
    } else {
        let hash = units.iter().fold(0i32, |h, &u| {
            h.wrapping_shl(5).wrapping_sub(h).wrapping_add(u as i32)
        });
        format!("{}-{}", &name[..200], radix36(hash.unsigned_abs()))
    };
    config_dir().join("projects").join(name)
}
fn radix36(mut n: u32) -> String {
    let mut digits = vec![];
    loop {
        digits.push(std::char::from_digit(n % 36, 36).unwrap());
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}
fn find_transcript(id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
        return None;
    }
    std::fs::read_dir(config_dir().join("projects"))
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}
fn read_entries(path: &Path) -> Result<Vec<Value>> {
    Ok(std::fs::read_to_string(path)
        .map_err(failure)?
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect())
}
fn is_message(e: &Value) -> bool {
    matches!(e["type"].as_str(), Some("user" | "assistant" | "system"))
        && e["isSidechain"] != true
        && e["uuid"].is_string()
}
/// The conversation that ends at `leaf`, oldest first, across compaction boundaries.
fn chain<'a>(entries: &'a [Value], leaf: Option<&'a Value>) -> Vec<&'a Value> {
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| Some((e["uuid"].as_str()?, e)))
        .collect();
    let mut seen = HashSet::new();
    let mut out = vec![];
    let mut cursor = leaf;
    while let Some(e) = cursor.filter(|e| seen.insert(e["uuid"].as_str().unwrap_or(""))) {
        out.push(e);
        cursor = e["parentUuid"]
            .as_str()
            .or(e["logicalParentUuid"].as_str())
            .and_then(|p| by_id.get(p).copied());
    }
    out.reverse();
    out
}

fn replay(s: &Session, cx: &ConnectionTo<Client>, bridge: &Bridge) -> Result<()> {
    let Some(path) = find_transcript(&s.id) else {
        return Ok(());
    };
    let entries = read_entries(&path)?;
    let leaf = entries
        .iter()
        .rev()
        .find(|e| is_message(e) && e["type"] != "system");
    for e in chain(&entries, leaf) {
        let content = &e["message"]["content"];
        match e["type"].as_str() {
            Some("user") if e["isMeta"] != true && e["isCompactSummary"] != true => {
                let blocks = match content {
                    Value::String(text) => vec![json!({"type":"text","text":text})],
                    Value::Array(blocks) => blocks.clone(),
                    _ => vec![],
                };
                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or("");
                            if [
                                "<command-",
                                "<local-command-",
                                "<system-reminder>",
                                "Caveat: ",
                            ]
                            .iter()
                            .any(|p| text.starts_with(p))
                            {
                                continue;
                            }
                            update(
                                cx,
                                &s.id,
                                json!({"sessionUpdate":"user_message_chunk","messageId":e["uuid"],"content":{"type":"text","text":text}}),
                            )?;
                        }
                        Some("image") if block["source"]["type"] == "base64" => {
                            update(
                                cx,
                                &s.id,
                                json!({"sessionUpdate":"user_message_chunk","messageId":e["uuid"],"content":{"type":"image","data":block["source"]["data"],"mimeType":block["source"]["media_type"]}}),
                            )?;
                        }
                        Some("tool_result") => {
                            s.tool_result(&block, &e["toolUseResult"], cx, bridge)?
                        }
                        _ => {}
                    }
                }
            }
            Some("assistant") => {
                for block in content.as_array().into_iter().flatten() {
                    let (kind, text) = match block["type"].as_str() {
                        Some("text") => ("agent_message_chunk", &block["text"]),
                        Some("thinking") => ("agent_thought_chunk", &block["thinking"]),
                        Some("tool_use") => {
                            s.tool_use(block, None, cx, bridge)?;
                            continue;
                        }
                        _ => continue,
                    };
                    update(
                        cx,
                        &s.id,
                        json!({"sessionUpdate":kind,"messageId":e["message"]["id"],"content":{"type":"text","text":text}}),
                    )?;
                }
            }
            _ => {}
        }
    }
    // Replayed calls that never got a result stay as they were recorded.
    s.state.lock().unwrap().open.clear();
    Ok(())
}

/// Copy the conversation up to the fork boundary into a new transcript in `cwd`'s
/// project. Claude only writes a fork itself after its first prompt.
fn fork_transcript(source: &str, cwd: &str, boundary: &Value) -> Result<String> {
    let path =
        find_transcript(source).ok_or_else(|| Error::resource_not_found(Some(source.into())))?;
    let entries = read_entries(&path)?;
    let leaf = entries
        .iter()
        .rev()
        .find(|e| is_message(e) && e["type"] != "system");
    let target = if boundary.is_null() {
        leaf
    } else {
        if boundary["version"] != 1 {
            return Err(invalid("Unsupported fork boundary version"));
        }
        let id = field(boundary, "messageId")?;
        // Agentworks suffixes visible segments of one message with :segment:N.
        let protocol = id
            .rsplit_once(":segment:")
            .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map_or(id, |(m, _)| m);
        let current = chain(&entries, leaf);
        let matches = |e: &&Value| {
            e["type"] == "assistant" && (e["message"]["id"] == id || e["message"]["id"] == protocol)
        };
        current
            .iter()
            .rev()
            .copied()
            .find(matches)
            .or_else(|| entries.iter().rev().filter(|e| is_message(e)).find(matches))
            .map(Some)
            .ok_or_else(|| invalid("Fork boundary was not found; refusing to fork newer history"))?
    };
    let kept = chain(&entries, target);
    if kept.is_empty() {
        return Err(invalid("Session has no messages to fork"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let renamed: HashMap<&str, String> = kept
        .iter()
        .filter_map(|e| Some((e["uuid"].as_str()?, uuid::Uuid::new_v4().to_string())))
        .collect();
    let mut text = String::new();
    for e in kept {
        let mut e = e.clone();
        let original = e["uuid"].clone();
        for key in ["uuid", "parentUuid", "logicalParentUuid"] {
            e[key] = e[key]
                .as_str()
                .and_then(|u| renamed.get(u))
                .map_or(Value::Null, |u| json!(u));
        }
        e["sessionId"] = json!(id);
        e["isSidechain"] = json!(false);
        e["forkedFrom"] = json!({"sessionId":source,"messageUuid":original});
        text.push_str(&e.to_string());
        text.push('\n');
    }
    let dir = project_dir(cwd);
    std::fs::create_dir_all(&dir).map_err(failure)?;
    let temporary = dir.join(format!(".{id}.jsonl.tmp"));
    std::fs::write(&temporary, text).map_err(failure)?;
    std::fs::rename(&temporary, dir.join(format!("{id}.jsonl"))).map_err(failure)?;
    Ok(id)
}

/// Settings Claude reads for the model and effort, later files winning.
fn settings(cwd: &str) -> Value {
    let mut merged = Map::new();
    for path in [
        config_dir().join("settings.json"),
        Path::new(cwd).join(".claude/settings.json"),
        Path::new(cwd).join(".claude/settings.local.json"),
    ] {
        if let Some(Value::Object(values)) = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            merged.extend(values);
        }
    }
    json!(merged)
}

fn list_sessions(cwd: Option<&str>, cursor: Option<&str>) -> Result<Value> {
    let offset = match cursor {
        Some(c) => c
            .strip_prefix("offset:")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| invalid("Invalid cursor"))?,
        None => 0usize,
    };
    let dirs: Vec<PathBuf> = match cwd {
        Some(cwd) => vec![project_dir(cwd)],
        None => std::fs::read_dir(config_dir().join("projects"))
            .map(|d| d.flatten().map(|d| d.path()).collect())
            .unwrap_or_default(),
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|d| d.flatten())
        .map(|f| f.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jsonl")
                && !p.file_name().unwrap().to_string_lossy().starts_with('.')
        })
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let more = files.len() > offset + PAGE;
    let mut sessions = vec![];
    for (modified, path) in files.into_iter().skip(offset).take(PAGE) {
        let Some(summary) = summarize(&path) else {
            continue;
        };
        if cwd.is_some_and(|c| summary.0 != c) {
            continue;
        }
        let mut item = json!({"sessionId":path.file_stem().unwrap().to_string_lossy(),"cwd":summary.0,"updatedAt":rfc3339(modified)});
        if let Some(title) = summary.1 {
            item["title"] = json!(title.chars().take(200).collect::<String>());
        }
        sessions.push(item);
    }
    let mut response = json!({"sessions":sessions});
    if more {
        response["nextCursor"] = json!(format!("offset:{}", offset + PAGE));
    }
    Ok(response)
}
/// The cwd and title of a transcript from its first and last 64 KB.
fn summarize(path: &Path) -> Option<(String, Option<String>)> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 64 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut head = vec![0; CHUNK.min(size) as usize];
    file.read_exact(&mut head).ok()?;
    let mut tail = vec![];
    if size > CHUNK {
        file.seek(SeekFrom::Start(size - CHUNK)).ok()?;
        file.read_to_end(&mut tail).ok()?;
    }
    let parse = |bytes: &[u8]| -> Vec<Value> {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    };
    let head = parse(&head);
    let tail = if tail.is_empty() {
        head.clone()
    } else {
        parse(&tail)
    };
    if head.iter().find(|e| e["uuid"].is_string())?["isSidechain"] == true {
        return None;
    }
    let cwd = head.iter().find_map(|e| e["cwd"].as_str())?.to_owned();
    let title = ["customTitle", "aiTitle"]
        .iter()
        .find_map(|k| {
            tail.iter()
                .chain(head.iter())
                .rev()
                .find_map(|e| e[*k].as_str().filter(|t| !t.is_empty()))
        })
        .or_else(|| {
            tail.iter()
                .rev()
                .find(|e| e["type"] == "summary")
                .and_then(|e| e["summary"].as_str())
        })
        .map(str::to_owned)
        .or_else(|| {
            head.iter()
                .filter(|e| e["type"] == "user" && e["isMeta"] != true)
                .find_map(|e| {
                    let text = text_of(&e["message"]["content"]);
                    let text = text.trim();
                    (!text.is_empty() && !text.starts_with('<'))
                        .then(|| text.lines().next().unwrap_or("").to_owned())
                })
        });
    Some((cwd, title))
}
fn rfc3339(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Run as a separate stdio process; stdout is reserved for ACP.
pub async fn run() -> Result<()> {
    let bridge = Arc::new(Bridge {
        form: AtomicBool::new(false),
        terminal_output: AtomicBool::new(false),
        sessions: Mutex::default(),
    });
    let requests = bridge.clone();
    let notifications = bridge.clone();
    let result = Agent
        .builder()
        .name("agentworks-claude-acp")
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
                    let live = notifications
                        .session(&notification.params)
                        .ok()
                        .filter(|s| {
                            s.state
                                .lock()
                                .unwrap()
                                .turn
                                .as_mut()
                                .is_some_and(|t| !std::mem::replace(&mut t.cancelled, true))
                        });
                    if let Some(live) = live {
                        cx.spawn(
                            async move { live.control("interrupt", json!({})).await.map(|_| ()) },
                        )?;
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(Stdio::new(), move |cx: ConnectionTo<Client>| async move {
            cx.incoming_closed().await;
            Ok(())
        })
        .await;
    let sessions: Vec<_> = bridge
        .sessions
        .lock()
        .unwrap()
        .drain()
        .map(|(_, s)| s)
        .collect();
    for session in sessions {
        session.kill().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_directories_match_claude_code() {
        assert!(project_dir("/home/a/my.repo").ends_with("projects/-home-a-my-repo"));
        let long = format!("/{}", "x".repeat(250));
        let name = project_dir(&long)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.len() > 201);
        assert!(name.starts_with(&format!("-{}", "x".repeat(199))));
    }
    #[test]
    fn dates_are_rfc3339() {
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1_792_670_400);
        assert_eq!(rfc3339(t), "2026-10-22T12:00:00Z");
    }
    #[test]
    fn mcp_prompts_use_claude_names() {
        let content =
            prompt_content(&json!([{"type":"text","text":"/mcp:srv:review now"}])).unwrap();
        assert_eq!(content[0]["text"], "/srv:review (MCP) now");
    }
}
