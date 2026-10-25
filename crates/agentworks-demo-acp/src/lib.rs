//! A deliberately local demo, speaking the same ACP v1 stdio protocol as the real adapters.
//! It never calls a model or edits project files. Harness state is saved for resume tests.
//! Protocol output goes only to stdout.
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{Mutex as AsyncMutex, oneshot},
};

mod script;

struct Approval {
    resolve: oneshot::Sender<Value>,
    session: String,
    /// Elicitations resolve with the whole result; permissions with its outcome.
    raw: bool,
}

struct Demo {
    state_dir: PathBuf,
    script: script::Script,
    lifecycle: bool,
    elicit_form: AtomicBool,
    sessions: Mutex<HashMap<String, Value>>,
    approvals: Mutex<HashMap<String, Approval>>,
    mcp_configs: Mutex<HashMap<String, Value>>,
    // Like real harnesses, the client system prompt is held per connection, never saved.
    system_prompts: Mutex<HashMap<String, Option<String>>>,
    // Held by a running prompt, so close can wait for it.
    active_prompts: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
fn env_is(name: &str, value: &str) -> bool {
    env(name).is_some_and(|v| v == value)
}
async fn delay(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
/// Seconds rounded up, as `Math.ceil(Date.now()/1000)`.
fn now_ceil() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_secs() + u64::from(now.subsec_nanos() > 0)
}
/// `Date.prototype.toISOString`, with milliseconds.
fn iso_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        now.subsec_millis()
    )
}
/// JavaScript truthiness, for fields the original fixtures treat loosely.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn or<'a>(v: &'a Value, fallback: &'a str) -> Value {
    if truthy(v) {
        v.clone()
    } else {
        json!(fallback)
    }
}
fn ms(v: &Value, fallback: u64) -> u64 {
    v.as_f64().map_or(fallback, |n| n.max(0.0) as u64)
}
/// String prefix and length in UTF-16 units, as JavaScript counts them.
fn utf16_prefix(s: &str, units: usize) -> String {
    String::from_utf16_lossy(&s.encode_utf16().take(units).collect::<Vec<_>>())
}
/// Matches of `\s*\S+\s*` (with leading whitespace) or `\S+\s*` (without).
fn words(text: &str, leading: bool) -> Vec<&str> {
    let mut starts: Vec<usize> = text
        .char_indices()
        .filter(|&(i, c)| {
            !c.is_whitespace()
                && text[..i]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace)
        })
        .map(|(i, _)| i)
        .collect();
    if leading && let Some(first) = starts.first_mut() {
        *first = 0;
    }
    let ends = starts.iter().skip(1).copied().chain([text.len()]);
    starts.iter().zip(ends).map(|(&s, e)| &text[s..e]).collect()
}
fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}
fn runtime_root() -> PathBuf {
    std::env::var_os("AGENTWORKS_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")))
}
fn system_prompt(params: &Value) -> Option<String> {
    let text: Vec<&str> = params["systemPrompt"]["content"]
        .as_array()?
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let text = text.join("\n\n");
    (!text.is_empty()).then_some(text)
}
const MODELS: [(&str, &str); 2] = [
    ("demo/steady", "Steady (local demo)"),
    ("demo/quick", "Quick (local demo)"),
];
fn model_config(model: &Value, mode: &Value) -> Value {
    let options: Vec<_> = MODELS
        .iter()
        .map(|(value, name)| json!({"value":value,"name":name}))
        .collect();
    let mut config = vec![
        json!({"id":"model","name":"Model","category":"model","type":"select","currentValue":model,"options":options}),
    ];
    if env_is("AGENTWORKS_DEMO_MODES", "1") {
        let mode = if mode.is_null() {
            json!("default")
        } else {
            mode.clone()
        };
        config.push(json!({"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":mode,"options":[{"value":"default","name":"Manual"},{"value":"auto","name":"Auto"}]}));
    }
    json!(config)
}
fn rate_limit(fields: Value) -> Value {
    json!({"sessionUpdate":"usage_update","used":100,"size":1000,"_meta":{"_claude/rateLimit":fields}})
}
fn quota_error() -> (i64, &'static str, Option<Value>) {
    (
        -32603,
        "Demo structured quota rejection",
        Some(json!({"codexErrorInfo":"usageLimitExceeded"})),
    )
}

impl Demo {
    fn send(&self, mut value: Value) {
        value["jsonrpc"] = json!("2.0");
        let mut line = value.to_string();
        line.push('\n');
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
    }
    fn reply(&self, id: &Value, result: Value) {
        self.send(json!({"id":id,"result":result}));
    }
    fn error(&self, id: &Value, code: i64, message: &str, data: Option<Value>) {
        let mut error = json!({"code":code,"message":message});
        if let Some(data) = data {
            error["data"] = data;
        }
        self.send(json!({"id":id,"error":error}));
    }
    fn update(&self, session: &str, update: Value) {
        if !matches!(
            update["sessionUpdate"].as_str(),
            Some("available_commands_update" | "config_option_update")
        ) && let Some(s) = self.sessions.lock().unwrap().get_mut(session)
        {
            if !s["history"].is_array() {
                s["history"] = json!([]);
            }
            s["history"].as_array_mut().unwrap().push(update.clone());
        }
        self.send(
            json!({"method":"session/update","params":{"sessionId":session,"update":update}}),
        );
    }
    fn chunk(&self, session: &str, text: &str) {
        self.update(
            session,
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}),
        );
    }
    fn commands(&self, session: &str) {
        self.update(
            session,
            json!({"sessionUpdate":"available_commands_update","availableCommands":[
                {"name":"status","description":"Read-only demo status"},
                {"name":"usage","description":"Read-only demo usage"},
                {"name":"help","description":"Show demo help"},
                {"name":"$demo-skill","description":"Example skill entry; no model is called"}]}),
        );
    }
    fn save(&self, session: &str) {
        let Some(state) = self.sessions.lock().unwrap().get(session).cloned() else {
            return;
        };
        let file = self.state_dir.join(format!("{session}.json"));
        let tmp = self.state_dir.join(format!("{session}.json.tmp"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let written = options
            .open(&tmp)
            .and_then(|mut f| f.write_all(state.to_string().as_bytes()));
        if written.is_ok() {
            let _ = std::fs::rename(tmp, file);
        }
    }
    fn trace(&self, method: &str, session: &Value, phase: &str) {
        if !self.lifecycle && !env_is("AGENTWORKS_DEMO_TRACE", "1") {
            return;
        }
        let mut entry = json!({"method":method,"phase":phase,"pid":std::process::id()});
        if !session.is_null() {
            entry["sessionId"] = session.clone();
        }
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.state_dir.join("lifecycle.jsonl"))
            .and_then(|mut f| f.write_all(format!("{entry}\n").as_bytes()));
    }
    fn field(&self, session: &str, key: &str) -> Value {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .map_or(Value::Null, |s| s[key].clone())
    }
    fn set(&self, session: &str, key: &str, value: Value) {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(session) {
            s[key] = value;
        }
    }
    fn cancelled(&self, session: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .is_none_or(|s| s["cancelled"] == true)
    }
    fn stop_reason(&self, session: &str) -> Value {
        json!({"stopReason":if self.cancelled(session) {"cancelled"} else {"end_turn"}})
    }
    fn finish(&self, id: &Value, session: &str) {
        self.save(session);
        self.reply(id, self.stop_reason(session));
    }
    /// Sends a client request whose response resolves the returned receiver.
    fn request(
        &self,
        session: &str,
        method: &str,
        params: Value,
        raw: bool,
    ) -> (String, oneshot::Receiver<Value>) {
        let id = uuid::Uuid::new_v4().to_string();
        let (resolve, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(
            id.clone(),
            Approval {
                resolve,
                session: session.into(),
                raw,
            },
        );
        self.send(json!({"id":id,"method":method,"params":params}));
        (id, rx)
    }
    fn permission(&self, session: &str, tool_call: Value, deny: bool) -> oneshot::Receiver<Value> {
        let mut options = vec![json!({"optionId":"allow","name":"Allow once","kind":"allow_once"})];
        if deny {
            options.push(json!({"optionId":"deny","name":"Deny","kind":"reject_once"}));
        }
        self.request(
            session,
            "session/request_permission",
            json!({"sessionId":session,"toolCall":tool_call,"options":options}),
            false,
        )
        .1
    }
    fn cancel_approvals(&self, session: &str) {
        let mut approvals = self.approvals.lock().unwrap();
        let keys: Vec<String> = approvals
            .iter()
            .filter(|(_, a)| a.session == session)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            let a = approvals.remove(&key).unwrap();
            let _ = a.resolve.send(if a.raw {
                json!({"action":"cancel"})
            } else {
                json!({"outcome":"cancelled"})
            });
        }
    }
    fn mcp_spec(&self, session: &str) -> Option<Value> {
        self.mcp_configs
            .lock()
            .unwrap()
            .get(session)?
            .as_array()?
            .iter()
            .find(|s| s["name"] == "agentworks")
            .cloned()
    }

    async fn close_session(&self, id: &Value, params: &Value) {
        let session = params["sessionId"].as_str().unwrap_or_default();
        self.trace("session/close", &params["sessionId"], "request");
        match env("AGENTWORKS_DEMO_CLOSE").as_deref() {
            Some("hang") => return,
            Some("reject") => return self.error(id, -32603, "Demo close rejected", None),
            _ => {}
        }
        self.set(session, "cancelled", json!(true));
        self.cancel_approvals(session);
        let running = self.active_prompts.lock().unwrap().get(session).cloned();
        if let Some(running) = running {
            let _ = running.lock().await;
        }
        self.sessions.lock().unwrap().remove(session);
        self.trace("session/close", &params["sessionId"], "closed");
        self.reply(id, json!({}));
    }

    fn list(&self, params: &Value) -> Result<Value, ()> {
        let mut all: Vec<Value> = std::fs::read_dir(&self.state_dir)
            .map_err(|_| ())?
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let id = name.strip_suffix(".json").filter(|id| is_session_id(id))?;
                let s: Value =
                    serde_json::from_slice(&std::fs::read(self.state_dir.join(&name)).ok()?)
                        .ok()?;
                Some(json!({"sessionId":id,"cwd":s["cwd"],"title":or(&s["title"],"Demo conversation"),
                    "updatedAt":or(&s["updatedAt"],"2026-01-01T00:00:00Z")}))
            })
            .filter(|s| !truthy(&params["cwd"]) || s["cwd"] == params["cwd"])
            .collect();
        all.sort_by(|a, b| {
            let key = |s: &Value| s["updatedAt"].as_str().unwrap_or_default().to_owned();
            key(b).cmp(&key(a))
        });
        let offset = match &params["cursor"] {
            c if !truthy(c) => 0,
            Value::String(c) => c.trim().parse::<usize>().map_err(|_| ())?,
            Value::Number(n) => n.as_u64().ok_or(())? as usize,
            _ => return Err(()),
        };
        let page: Vec<_> = all.iter().skip(offset).take(10).cloned().collect();
        let mut result = json!({"sessions":page});
        if offset + 10 < all.len() {
            result["nextCursor"] = json!((offset + 10).to_string());
        }
        Ok(result)
    }

    fn open(self: &Arc<Self>, id: &Value, method: &str, params: &Value) -> Result<(), ()> {
        let session = params["sessionId"]
            .as_str()
            .filter(|s| is_session_id(s))
            .ok_or(())?;
        let file = std::fs::read(self.state_dir.join(format!("{session}.json"))).map_err(|_| ())?;
        let mut state: Value = serde_json::from_slice(&file).map_err(|_| ())?;
        if !state.is_object() || state["cwd"] != params["cwd"] {
            return Err(());
        }
        state["cancelled"] = json!(false);
        let history = state["history"].as_array().cloned().unwrap_or_default();
        let (fail_load, resume_delay) =
            (truthy(&state["failLoad"]), state["resumeDelayMs"].as_u64());
        let config = model_config(&state["model"], &state["mode"]);
        self.sessions.lock().unwrap().insert(session.into(), state);
        self.mcp_configs
            .lock()
            .unwrap()
            .insert(session.into(), or_array(&params["mcpServers"]));
        self.system_prompts
            .lock()
            .unwrap()
            .insert(session.into(), system_prompt(params));
        if method == "session/load" {
            for update in history {
                self.send(json!({"method":"session/update","params":{"sessionId":session,"update":update}}));
            }
            if fail_load {
                self.error(id, -32603, "Demo history load failed", None);
                return Ok(());
            }
        }
        let ready = {
            let (demo, id, session) = (self.clone(), id.clone(), session.to_owned());
            move || {
                demo.reply(&id, json!({"configOptions":config}));
                demo.commands(&session);
            }
        };
        // Deterministic native-opening race fixture; never delays ordinary demo sessions.
        match resume_delay.filter(|ms| (1..=2000).contains(ms)) {
            Some(ms) => {
                tokio::spawn(async move {
                    delay(ms).await;
                    ready();
                });
            }
            None => ready(),
        }
        Ok(())
    }

    fn handle(self: &Arc<Self>, message: Value) {
        let id = message["id"].clone();
        let has_id = message.get("id").is_some();
        let method = message["method"].as_str().unwrap_or_default();
        let params = match &message["params"] {
            Value::Null => json!({}),
            p => p.clone(),
        };
        let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
        if !self.lifecycle && env_is("AGENTWORKS_DEMO_TRACE", "1") {
            self.trace(method, &params["sessionId"], "request");
        }
        match method {
            "initialize" => {
                self.elicit_form.store(
                    truthy(&params["clientCapabilities"]["elicitation"]["form"]),
                    Ordering::Relaxed,
                );
                let mut sessions = json!({"resume":{},"list":{},"systemPrompt":{}});
                if self.lifecycle {
                    sessions["close"] = json!({});
                    sessions["delete"] = json!({});
                }
                let prompts = env("AGENTWORKS_DEMO_PROMPT_CAPS")
                    .and_then(|caps| serde_json::from_str(&caps).ok())
                    .unwrap_or_else(|| json!({"image":true,"embeddedContext":true}));
                self.reply(
                    &id,
                    json!({"protocolVersion":1,"agentInfo":{"name":"agentworks-demo","version":"0.1.0"},
                        "agentCapabilities":{"loadSession":true,"sessionCapabilities":sessions,"promptCapabilities":prompts},
                        "authMethods":[]}),
                );
            }
            "session/new" => {
                let session = uuid::Uuid::new_v4().to_string();
                self.mcp_configs
                    .lock()
                    .unwrap()
                    .insert(session.clone(), or_array(&params["mcpServers"]));
                self.system_prompts
                    .lock()
                    .unwrap()
                    .insert(session.clone(), system_prompt(&params));
                self.sessions.lock().unwrap().insert(
                    session.clone(),
                    json!({"cwd":params["cwd"],"turn":0,"cancelled":false,"model":"demo/steady"}),
                );
                self.trace("session/new", &json!(session), "request");
                self.save(&session);
                self.reply(
                    &id,
                    json!({"sessionId":session,"configOptions":model_config(&json!("demo/steady"), &Value::Null)}),
                );
                self.commands(&session);
            }
            "session/list" => match self.list(&params) {
                Ok(result) => self.reply(&id, result),
                Err(()) => self.error(&id, -32602, "Could not list demo sessions", None),
            },
            "session/load" | "session/resume" => {
                self.trace(method, &params["sessionId"], "request");
                if self.open(&id, method, &params).is_err() {
                    self.error(&id, -32002, "Saved demo session is unavailable", None);
                }
            }
            "session/set_config_option" => {
                let value = &params["value"];
                let exists = self.sessions.lock().unwrap().contains_key(&session);
                if exists
                    && params["configId"] == "mode"
                    && env_is("AGENTWORKS_DEMO_MODES", "1")
                    && (value == "default" || value == "auto")
                {
                    self.set(&session, "mode", value.clone());
                } else if exists
                    && params["configId"] == "model"
                    && MODELS.iter().any(|(m, _)| value == m)
                {
                    self.set(&session, "model", value.clone());
                } else {
                    return self.error(&id, -32602, "Unknown model", None);
                }
                self.save(&session);
                let config = model_config(
                    &self.field(&session, "model"),
                    &self.field(&session, "mode"),
                );
                self.reply(&id, json!({"configOptions":config}));
            }
            "session/close" => {
                if !self.lifecycle {
                    return self.error(&id, -32601, "Unsupported", None);
                }
                let demo = self.clone();
                tokio::spawn(async move { demo.close_session(&id, &params).await });
            }
            "session/delete" => {
                if !self.lifecycle {
                    return self.error(&id, -32601, "Unsupported", None);
                }
                self.trace(method, &params["sessionId"], "request");
                if env_is("AGENTWORKS_DEMO_DELETE", "reject") {
                    return self.error(&id, -32603, "Demo delete rejected", None);
                }
                if !is_session_id(&session) {
                    return self.error(&id, -32602, "Invalid session ID", None);
                }
                match std::fs::remove_file(self.state_dir.join(format!("{session}.json"))) {
                    Ok(()) => {
                        if env_is("AGENTWORKS_DEMO_DELETE", "drop") {
                            std::process::exit(1);
                        }
                        self.reply(&id, json!({}));
                    }
                    Err(e) => {
                        let code = if e.kind() == std::io::ErrorKind::NotFound {
                            -32002
                        } else {
                            -32603
                        };
                        self.error(&id, code, &e.to_string(), None);
                    }
                }
            }
            "session/prompt" => {
                self.trace(method, &params["sessionId"], "request");
                let running = Arc::new(AsyncMutex::new(()));
                let guard = running.clone().try_lock_owned().ok();
                self.active_prompts
                    .lock()
                    .unwrap()
                    .insert(session.clone(), running.clone());
                let demo = self.clone();
                tokio::spawn(async move {
                    let _running = guard;
                    if let Err(e) = demo.prompt(&id, &params).await {
                        demo.error(&id, -32603, &e, None);
                    }
                    let mut active = demo.active_prompts.lock().unwrap();
                    if active
                        .get(&session)
                        .is_some_and(|r| Arc::ptr_eq(r, &running))
                    {
                        active.remove(&session);
                    }
                });
            }
            "session/cancel" => {
                self.set(&session, "cancelled", json!(true));
                self.cancel_approvals(&session);
            }
            _ if has_id => self.error(&id, -32601, "Method not supported by demo", None),
            _ => {}
        }
    }

    async fn prompt(self: &Arc<Self>, id: &Value, params: &Value) -> Result<(), String> {
        let sid = params["sessionId"].as_str().unwrap_or_default();
        let turn = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(s) = sessions.get_mut(sid) else {
                drop(sessions);
                self.error(id, -32602, "Unknown session", None);
                return Ok(());
            };
            s["cancelled"] = json!(false);
            let turn = s["turn"].as_i64().unwrap_or(0) + 1;
            s["turn"] = json!(turn);
            turn
        };
        self.save(sid);
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        let text = blocks
            .iter()
            .filter(|p| p["type"] == "text")
            .map(|p| p["text"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(s) = self.sessions.lock().unwrap().get_mut(sid) {
            if !s["history"].is_array() {
                s["history"] = json!([]);
            }
            s["history"].as_array_mut().unwrap().push(
                json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":text}}),
            );
            if s["title"].is_null() {
                s["title"] = json!(utf16_prefix(&text, 80));
            }
            s["updatedAt"] = json!(iso_now());
        }
        self.save(sid);
        // U-07 fixture: `usage-status {json}` emits that usage_update deterministically; no model is called.
        if let Some(usage) = text.strip_prefix("usage-status ") {
            let mut update: Value = serde_json::from_str(usage).map_err(|e| e.to_string())?;
            let fields = update
                .as_object_mut()
                .ok_or("usage-status expects an object")?;
            let mut merged = Map::from_iter([("sessionUpdate".to_owned(), json!("usage_update"))]);
            merged.append(fields);
            self.update(sid, Value::Object(merged));
            self.chunk(sid, "usage reported");
            self.save(sid);
            self.reply(id, json!({"stopReason":"end_turn"}));
            return Ok(());
        }
        // Roles fixture: `role?` reports the system prompt this connection received.
        if text == "role?" {
            let prompt = self
                .system_prompts
                .lock()
                .unwrap()
                .get(sid)
                .cloned()
                .flatten();
            self.chunk(sid, prompt.as_deref().unwrap_or("No system prompt."));
            self.save(sid);
            self.reply(id, json!({"stopReason":"end_turn"}));
            return Ok(());
        }
        // Keys starting with `^` match prompts by prefix, for prompts composed by Agentworks such as inbox delivery.
        if let Some(steps) = self.script.find(&text) {
            self.scripted(sid, steps).await;
            self.finish(id, sid);
            return Ok(());
        }
        // U-05 fixture: `bazel demo` replays recorded Bazel tool output; no command is executed.
        if text == "bazel demo" {
            let tool_call_id = uuid::Uuid::new_v4().to_string();
            let events = std::fs::read(runtime_root().join("tests/fixtures/bazel-events.json"))
                .map_err(|e| e.to_string())?;
            let events: Vec<Value> = serde_json::from_slice(&events).map_err(|e| e.to_string())?;
            for event in events.into_iter().filter(|e| e["kind"] == "acp") {
                let mut update = event["data"].clone();
                update["toolCallId"] = json!(tool_call_id);
                self.update(sid, update);
                delay(10).await;
            }
            self.chunk(
                sid,
                "Replayed the recorded Bazel fixture. No command was executed.",
            );
            self.save(sid);
            self.reply(id, json!({"stopReason":"end_turn"}));
            return Ok(());
        }
        self.chunk(sid, &format!("### Local session, turn {turn}\n\n"));
        // Attachment fixture: report exactly which non-text blocks arrived (kind, type, size, URI).
        let attached: Vec<String> = blocks
            .iter()
            .filter(|p| p["type"] != "text")
            .map(describe_attachment)
            .collect();
        if !attached.is_empty() {
            self.chunk(
                sid,
                &format!("Received attachments:\n{}\n\n", attached.join("\n")),
            );
        }
        // B-02: deterministic provider-local read; no model/tool action is dispatched.
        if text == "/status" || text == "/usage" {
            delay(
                env("AGENTWORKS_DEMO_STATUS_DELAY_MS")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            )
            .await;
            let probe = self.field(sid, "probeMode");
            if probe == "transport-exit" {
                std::process::exit(1);
            }
            if truthy(&probe) {
                let mut fields = json!({"status":"rejected"});
                if probe != "unknown" {
                    fields["resetsAt"] = json!(if probe == "stale" { 1 } else { now_ceil() + 10 });
                }
                self.update(sid, rate_limit(fields));
            }
            self.chunk(sid, "Local status read.");
            self.finish(id, sid);
            return Ok(());
        }
        if text == "quota native-error" {
            self.error(
                id,
                -32603,
                "Trusted native quota",
                Some(json!({"codexErrorInfo":"usageLimitExceeded"})),
            );
            return Ok(());
        }
        if text == "quota active-approval" {
            self.update(
                sid,
                rate_limit(json!({"status":"rejected","resetsAt":now_ceil()+2})),
            );
            let tool_call = json!({"toolCallId":uuid::Uuid::new_v4().to_string(),"title":"Quota approval boundary","kind":"other","status":"pending"});
            let _ = self.permission(sid, tool_call, false).await;
            self.reply(id, self.stop_reason(sid));
            return Ok(());
        }
        if text == "quota repeat" {
            self.set(sid, "repeatQuota", json!(2));
        }
        let repeat = self.field(sid, "repeatQuota").as_i64().unwrap_or(0);
        let quota = [
            "quota known",
            "quota unknown",
            "quota multi",
            "quota repeat",
        ]
        .iter()
        .any(|p| text.starts_with(p));
        if quota || (text.starts_with("[Agentworks usage-limit continuation;") && repeat > 0) {
            if repeat > 0 {
                self.set(sid, "repeatQuota", json!(repeat - 1));
            }
            let probe = [
                ("probe-transport", "transport-exit"),
                ("probe-exhausted", "exhausted"),
                ("probe-unknown", "unknown"),
                ("probe-stale", "stale"),
            ]
            .iter()
            .find(|(needle, _)| text.contains(needle))
            .map_or(Value::Null, |(_, mode)| json!(mode));
            self.set(sid, "probeMode", probe);
            let reset = now_ceil() + 4;
            let mut fields = json!({"status":"rejected"});
            if !text.contains("unknown") {
                fields["resetsAt"] = json!(reset);
            }
            if text.contains("multi") {
                fields["overageStatus"] = json!("rejected");
                fields["overageResetsAt"] = json!(reset + 3);
            }
            self.update(sid, rate_limit(fields));
            self.save(sid);
            if text.contains("recovered") {
                self.reply(id, json!({"stopReason":"end_turn"}));
            } else {
                let (code, message, data) = quota_error();
                self.error(id, code, message, data);
            }
            return Ok(());
        }
        if text == "quota generic" {
            self.error(
                id,
                -32603,
                "rate limit quota resetsAt 9999999999 (injected prose)",
                None,
            );
            return Ok(());
        }
        if let Some(rest) = text.strip_prefix("mcp ") {
            let tool_call_id = uuid::Uuid::new_v4().to_string();
            let spec = self.mcp_spec(sid);
            self.update(
                sid,
                json!({"sessionUpdate":"tool_call","toolCallId":tool_call_id,"title":"Agentworks MCP","kind":"other","status":"in_progress"}),
            );
            let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err("Use mcp <tool> [json arguments]".into());
            }
            let args: Value = serde_json::from_str(if args.is_empty() { "{}" } else { args })
                .map_err(|e| e.to_string())?;
            let result = call_mcp(spec.as_ref(), name, args).await;
            let raw = result.to_string();
            self.update(
                sid,
                json!({"sessionUpdate":"tool_call_update","toolCallId":tool_call_id,
                    "status":if truthy(&result["isError"]) {"failed"} else {"completed"},"rawOutput":result,
                    "content":[{"type":"content","content":{"type":"text","text":raw}}]}),
            );
            self.chunk(sid, &format!("```json\n{raw}\n```\n"));
            self.save(sid);
            self.reply(id, json!({"stopReason":"end_turn"}));
            return Ok(());
        }
        // L-02: `question`, `question url` (unsupported mode) and `question withdraw` ($/cancel_request).
        let lower = text.to_lowercase();
        if lower.starts_with("question")
            && lower[8..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
        {
            if !self.elicit_form.load(Ordering::Relaxed) {
                self.chunk(sid, "The client did not advertise form elicitation.\n\n");
                self.save(sid);
                self.reply(id, json!({"stopReason":"end_turn"}));
                return Ok(());
            }
            let request = if lower.contains("url") {
                json!({"mode":"url","sessionId":sid,"elicitationId":uuid::Uuid::new_v4().to_string(),
                    "url":"https://example.invalid/login","message":"Sign in to the demo service"})
            } else {
                json!({"mode":"form","sessionId":sid,"message":"How should the demo project be set up?","requestedSchema":{"type":"object","properties":{
                    "question_0":{"type":"string","title":"Database","description":"Which database should the demo use?","oneOf":[{"const":"Postgres","title":"Postgres","description":"Server database"},{"const":"SQLite","title":"SQLite","description":"Single file"}]},
                    "question_0_custom":{"type":"string","title":"Other","description":"Type your own answer (optional).","maxLength":40},
                    "features":{"type":"array","title":"Features","items":{"anyOf":[{"const":"auth","title":"Authentication"},{"const":"search","title":"Search"},{"const":"billing","title":"Billing"}]},"maxItems":2},
                    "replicas":{"type":"integer","title":"Replicas","minimum":1,"maximum":5},
                    "contact":{"type":"string","title":"Contact email","format":"email"},
                    "confirm":{"type":"boolean","title":"Create sample data","default":true}},"required":["question_0","replicas"]}})
            };
            let (request_id, outcome) = self.request(sid, "elicitation/create", request, true);
            if lower.contains("withdraw") {
                delay(1500).await;
                self.send(json!({"method":"$/cancel_request","params":{"requestId":request_id}}));
                if let Some(a) = self.approvals.lock().unwrap().remove(&request_id) {
                    let _ = a.resolve.send(json!({"action":"cancel"}));
                }
            }
            let result = outcome.await.unwrap_or(Value::Null);
            if !self.cancelled(sid) {
                let answer = match result["action"].as_str() {
                    Some("accept") => format!("You answered: `{}`\n\n", result["content"]),
                    Some("decline") => "You skipped the question.\n\n".into(),
                    _ => "The question was cancelled.\n\n".into(),
                };
                self.chunk(sid, &answer);
            }
            self.finish(id, sid);
            return Ok(());
        }
        if lower.contains("approval") {
            let tool_call_id = uuid::Uuid::new_v4().to_string();
            self.update(
                sid,
                json!({"sessionUpdate":"tool_call","toolCallId":tool_call_id,"title":"Show a demo tool result","kind":"other","status":"pending","rawInput":{"demo":true}}),
            );
            let tool_call = json!({"toolCallId":tool_call_id,"title":"Show a demo tool result","kind":"other","status":"pending"});
            let decision = self
                .permission(sid, tool_call, true)
                .await
                .unwrap_or(Value::Null);
            if !self.cancelled(sid) {
                let allowed = decision["outcome"] == "selected" && decision["optionId"] == "allow";
                let result = if allowed {
                    "Demo output approved. No command was executed."
                } else {
                    "Demo operation denied."
                };
                self.update(
                    sid,
                    json!({"sessionUpdate":"tool_call_update","toolCallId":tool_call_id,"status":if allowed {"completed"} else {"failed"},
                        "content":[{"type":"content","content":{"type":"text","text":result}}]}),
                );
                self.chunk(
                    sid,
                    if allowed {
                        "You **approved** the demo operation.\n\n"
                    } else {
                        "You **denied** the demo operation.\n\n"
                    },
                );
            }
        }
        let cwd = self.field(sid, "cwd");
        let answer = format!(
            "This is the **demo agent**, connected through the Rust ACP client. No model API was called.\n\n\
             Your project is `{}`.\n\n\
             Try another message to check session continuity, **approval** to test a permission request, or **slow** to test cancellation and browser reconnects.\n\n\
             Open [README.md](README.md) to preview a local Markdown file.\n\n\
             > {}\n",
            cwd.as_str().unwrap_or_default(),
            text.replace('\n', "\n> ")
        );
        let pace = if lower.contains("slow") { 140 } else { 12 };
        for word in words(&answer, false) {
            if self.cancelled(sid) {
                break;
            }
            self.chunk(sid, word);
            delay(pace).await;
        }
        if !self.cancelled(sid) && lower.contains("image") {
            self.update(
                sid,
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"image","mimeType":"image/png",
                    "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="}}),
            );
        }
        self.finish(id, sid);
        Ok(())
    }

    /// README media fixture steps: {say} streams Markdown, {wait} pauses, {approve} asks
    /// permission for an earlier tool call, {mcp} calls a real Agentworks MCP tool, anything
    /// else is an ACP update. No command is executed.
    async fn scripted(self: &Arc<Self>, sid: &str, steps: &Value) {
        let mut seen = Map::new();
        for step in steps.as_array().into_iter().flatten() {
            if self.cancelled(sid) {
                break;
            }
            if let Some(say) = step.get("say") {
                for word in words(say.as_str().unwrap_or_default(), true) {
                    if self.cancelled(sid) {
                        break;
                    }
                    self.chunk(sid, word);
                    delay(ms(&step["pace"], 18)).await;
                }
            } else if let Some(wait) = step.get("wait") {
                delay(ms(wait, 0)).await;
            } else if let Some(approve) = step.get("approve") {
                let tool_call = json!({"toolCallId":approve,"title":step["title"],"kind":or(&step["kind"],"other"),"status":"pending"});
                let decision = self
                    .permission(sid, tool_call, true)
                    .await
                    .unwrap_or(Value::Null);
                if !(decision["outcome"] == "selected" && decision["optionId"] == "allow") {
                    self.update(
                        sid,
                        json!({"sessionUpdate":"tool_call_update","toolCallId":approve,"status":"failed"}),
                    );
                    break;
                }
            } else if let Some(tool) = step.get("mcp") {
                let tool_call_id = uuid::Uuid::new_v4().to_string();
                let arguments = match &step["arguments"] {
                    a if truthy(a) => a.clone(),
                    _ => json!({}),
                };
                let (args, failure) = match resolve_refs(&arguments, &seen) {
                    Ok(args) => (args, None),
                    Err(e) => (
                        step["arguments"].clone(),
                        Some(json!({"isError":true,"message":e})),
                    ),
                };
                self.update(
                    sid,
                    json!({"sessionUpdate":"tool_call","toolCallId":tool_call_id,"title":or(&step["title"],tool.as_str().unwrap_or_default()),
                        "kind":or(&step["kind"],"other"),"status":"in_progress","rawInput":args}),
                );
                let name = tool.as_str().unwrap_or_default();
                let result = match failure {
                    Some(failure) => failure,
                    None => call_mcp(self.mcp_spec(sid).as_ref(), name, args).await,
                };
                let text = result["content"]
                    .as_array()
                    .and_then(|c| c.iter().find(|c| c["type"] == "text"))
                    .and_then(|c| c["text"].as_str())
                    .map(str::to_owned);
                let parsed = match &result["structuredContent"] {
                    Value::Null => text.as_deref().and_then(|t| serde_json::from_str(t).ok()),
                    structured => Some(structured.clone()),
                };
                if let Some(parsed) = parsed {
                    seen.insert(name.to_owned(), parsed);
                }
                let shown = text
                    .or_else(|| result["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| result.to_string());
                self.update(
                    sid,
                    json!({"sessionUpdate":"tool_call_update","toolCallId":tool_call_id,
                        "status":if truthy(&result["isError"]) {"failed"} else {"completed"},"rawOutput":result,
                        "content":[{"type":"content","content":{"type":"text","text":shown}}]}),
                );
                delay(ms(&step["after"], 300)).await;
            } else {
                self.update(sid, step.clone());
            }
        }
    }
}

fn or_array(v: &Value) -> Value {
    if v.is_array() { v.clone() } else { json!([]) }
}

fn describe_attachment(p: &Value) -> String {
    match p["type"].as_str().unwrap_or_default() {
        "image" => {
            // Decoded size of the base64 data, as Buffer.from(data,'base64') counts it.
            let data = p["data"].as_str().unwrap_or_default();
            let chars = data
                .bytes()
                .filter(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_'))
                .count();
            format!(
                "- image {} {} bytes",
                p["mimeType"].as_str().unwrap_or("undefined"),
                chars * 3 / 4
            )
        }
        "resource" => {
            let r = &p["resource"];
            let text = r["text"].as_str().unwrap_or_default();
            format!(
                "- resource {} {} {} chars: {}",
                r["mimeType"].as_str().unwrap_or("undefined"),
                r["uri"].as_str().unwrap_or("undefined"),
                text.encode_utf16().count(),
                json!(utf16_prefix(text, 40))
            )
        }
        other => format!("- {other}"),
    }
}

/// Script placeholders name runtime IDs by title: `$peer:<title>` from list_sessions,
/// `$message-from:<title>` and `$sender:<title>` from read_inbox.
fn resolve_refs(value: &Value, seen: &Map<String, Value>) -> Result<Value, String> {
    match value {
        Value::Array(items) => items.iter().map(|v| resolve_refs(v, seen)).collect(),
        Value::Object(fields) => fields
            .iter()
            .map(|(k, v)| Ok((k.clone(), resolve_refs(v, seen)?)))
            .collect::<Result<Map<_, _>, String>>()
            .map(Value::Object),
        Value::String(s) => {
            let Some((kind, title)) =
                s.strip_prefix('$')
                    .and_then(|r| r.split_once(':'))
                    .filter(|(k, t)| {
                        matches!(*k, "peer" | "message-from" | "sender")
                            && !t.is_empty()
                            && !t.contains('\n')
                    })
            else {
                return Ok(value.clone());
            };
            let found = if kind == "peer" {
                seen.get("list_sessions")
                    .and_then(|l| l["sessions"].as_array())
                    .and_then(|s| s.iter().find(|p| p["title"] == title))
                    .map(|p| p["uri"].clone())
            } else {
                seen.get("read_inbox")
                    .and_then(|i| i["messages"].as_array())
                    .and_then(|m| m.iter().rev().find(|m| m["senderTitle"] == title))
                    .map(|m| m[if kind == "sender" { "senderUri" } else { "uri" }].clone())
            };
            found
                .filter(truthy)
                .ok_or_else(|| format!("Unresolved script reference {s}"))
        }
        _ => Ok(value.clone()),
    }
}

/// One MCP request through a short-lived stdio client, as the demo's tool calls need.
async fn call_mcp(spec: Option<&Value>, name: &str, args: Value) -> Value {
    let request = if name == "tools/list" {
        ("tools/list", json!({}))
    } else {
        ("tools/call", json!({"name":name,"arguments":args}))
    };
    let result = match spec {
        None => Err("No Agentworks MCP configuration".into()),
        Some(spec) => tokio::time::timeout(Duration::from_secs(60), mcp_request(spec, request))
            .await
            .unwrap_or_else(|_| Err("MCP error -32001: Request timed out".into())),
    };
    result.unwrap_or_else(|message| json!({"isError":true,"message":message}))
}

async fn mcp_request(spec: &Value, (method, params): (&str, Value)) -> Result<Value, String> {
    let mut command = Command::new(spec["command"].as_str().unwrap_or_default());
    command
        .args(
            spec["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .env_clear()
        // The MCP SDK's default inheritance, then the server's own variables.
        .envs(
            ["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"]
                .into_iter()
                .filter_map(|k| {
                    std::env::var(k)
                        .ok()
                        .filter(|v| !v.starts_with("()"))
                        .map(|v| (k, v))
                }),
        )
        .envs(
            spec["env"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| Some((e["name"].as_str()?, e["value"].as_str()?))),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next = 0;
    let mut call = async |method: &str, params: Value| -> Result<Value, String> {
        let id = next;
        next += 1;
        input
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .as_bytes(),
            )
            .await
            .map_err(|e| e.to_string())?;
        loop {
            let line = output
                .next_line()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("MCP error -32000: Connection closed")?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method").is_some() {
                // Server requests are not supported by this minimal client.
                if let Some(request) = message.get("id") {
                    let reply = json!({"jsonrpc":"2.0","id":request,"error":{"code":-32601,"message":"Method not found"}});
                    input
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .map_err(|e| e.to_string())?;
                }
                continue;
            }
            if message["id"] != id {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!(
                    "MCP error {}: {}",
                    error["code"],
                    error["message"].as_str().unwrap_or_default()
                ));
            }
            if method == "initialize" {
                input
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                    .await
                    .map_err(|e| e.to_string())?;
            }
            return Ok(message["result"].clone());
        }
    };
    call(
        "initialize",
        json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"agentworks-demo","version":"1.0.0"}}),
    )
    .await?;
    call(method, params).await
}

/// Run as a separate stdio process; stdout is reserved for ACP.
pub async fn run() -> std::io::Result<()> {
    let state_dir = env("AGENTWORKS_DEMO_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".agentworks/demo"));
    let state_dir = std::path::absolute(state_dir)?;
    std::fs::create_dir_all(&state_dir)?;
    // README media fixture: AGENTWORKS_DEMO_SCRIPT names a JSON file mapping exact prompts to scripted steps.
    let script = match env("AGENTWORKS_DEMO_SCRIPT") {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => script::Script::default(),
    };
    let demo = Arc::new(Demo {
        state_dir,
        script,
        lifecycle: env_is("AGENTWORKS_DEMO_LIFECYCLE", "1"),
        elicit_form: AtomicBool::new(false),
        sessions: Mutex::default(),
        approvals: Mutex::default(),
        mcp_configs: Mutex::default(),
        system_prompts: Mutex::default(),
        active_prompts: Mutex::default(),
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message.get("method").is_none() {
            let key = match &message["id"] {
                Value::String(id) => id.clone(),
                other => other.to_string(),
            };
            let waiting = demo.approvals.lock().unwrap().remove(&key);
            if let Some(a) = waiting {
                let result = &message["result"];
                let _ = a.resolve.send(if a.raw {
                    result.clone()
                } else {
                    result["outcome"].clone()
                });
            }
            continue;
        }
        demo.handle(message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_match_the_javascript_patterns() {
        assert_eq!(
            words("  one two\n three ", true),
            ["  one ", "two\n ", "three "]
        );
        assert_eq!(words("  one two", false), ["one ", "two"]);
        assert!(words("   ", true).is_empty());
    }

    #[test]
    fn refs_resolve_from_seen_results() {
        let seen = Map::from_iter([
            (
                "list_sessions".into(),
                json!({"sessions":[{"title":"Peer","uri":"aw://peer"}]}),
            ),
            (
                "read_inbox".into(),
                json!({"messages":[
                {"senderTitle":"Peer","uri":"aw://m1","senderUri":"aw://s1"},
                {"senderTitle":"Peer","uri":"aw://m2","senderUri":"aw://s2"}]}),
            ),
        ]);
        let args = json!({"to":["$peer:Peer"],"reply":"$message-from:Peer","sender":"$sender:Peer","text":"$other"});
        assert_eq!(
            resolve_refs(&args, &seen).unwrap(),
            json!({"to":["aw://peer"],"reply":"aw://m2","sender":"aw://s2","text":"$other"})
        );
        assert_eq!(
            resolve_refs(&json!("$peer:Missing"), &seen).unwrap_err(),
            "Unresolved script reference $peer:Missing"
        );
    }

    #[test]
    fn attachments_are_described_like_the_node_demo() {
        assert_eq!(
            describe_attachment(&json!({"type":"image","mimeType":"image/png","data":"aGVsbG8="})),
            "- image image/png 5 bytes"
        );
        assert_eq!(
            describe_attachment(
                &json!({"type":"resource","resource":{"mimeType":"text/plain","uri":"file:///a","text":"hi \"x\""}})
            ),
            "- resource text/plain file:///a 6 chars: \"hi \\\"x\\\"\""
        );
    }

    #[test]
    fn iso_dates_have_milliseconds() {
        let date = iso_now();
        assert_eq!(date.len(), 24);
        assert!(date.ends_with('Z') && date.as_bytes()[19] == b'.');
        assert!(now_ceil() > 1_700_000_000);
    }
}
