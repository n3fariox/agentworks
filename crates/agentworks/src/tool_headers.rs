//! Tool card headers from Starlark `register_tool_header` hooks.
//!
//! Runs on the tool-progress interpreter thread. The latest title, kind and
//! `rawInput` of each tool are summarized into a bounded `ToolCall` record; when
//! that summary changes, the first matching hook that returns text supplies the
//! header. Headers are display only: they are published as unsequenced
//! `tool_header` events, attached to compacted snapshot events, never stored,
//! and rebuilt from the retained event window after a restart. Without a header
//! the browser shows the harness title.

use agentworks_starlark::engine::{Compiled, Engine};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Tools remembered per session; the oldest are dropped first.
const MAX_TOOLS_PER_SESSION: usize = 1000;
const MAX_HEADER_CHARS: usize = 200;

/// `session -> tool key -> (seq, header)`, read by snapshots.
pub type Headers = Arc<Mutex<HashMap<String, HashMap<String, (u64, String)>>>>;

/// Root tools are keyed by `toolCallId`; child-session tools by their ACP
/// session as well, because IDs are only unique within one ACP session.
pub fn key(data: &Value) -> Option<String> {
    let tool = data["toolCallId"].as_str()?;
    Some(match data["parentSessionId"].as_str() {
        Some(_) => format!(
            "{}\u{1f}{tool}",
            data["acpSessionId"].as_str().unwrap_or_default()
        ),
        None => tool.to_owned(),
    })
}

fn bounded(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

/// Shell argv as one command line; `sh -c script` forms show only the script.
fn command(raw: &Value) -> Option<String> {
    match raw {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [shell, flag, .., script]
                    if shell.ends_with("sh") && matches!(*flag, "-c" | "-lc") =>
                {
                    Some((*script).to_owned())
                }
                [] => None,
                _ => Some(parts.join(" ")),
            }
        }
        _ => None,
    }
}

struct Tool {
    seq: u64,
    title: Option<String>,
    kind: Option<String>,
    raw_input: Value,
    /// The `ToolCall` JSON last evaluated, so unchanged updates run nothing.
    evaluated: Option<Value>,
    parent: Option<(String, String)>,
}

impl Tool {
    fn summary(&self) -> Value {
        let raw = self.raw_input.as_object();
        let text = |name: &str, max: usize| {
            raw.and_then(|r| r.get(name))
                .and_then(Value::as_str)
                .map(|s| bounded(s, max))
        };
        let mut input = Map::new();
        for (name, value) in raw.into_iter().flatten() {
            if input.len() >= 24 || matches!(name.as_str(), "command" | "description") {
                continue;
            }
            match value {
                Value::String(s) => {
                    input.insert(bounded(name, 64), json!(bounded(s, 200)));
                }
                Value::Number(_) | Value::Bool(_) => {
                    input.insert(bounded(name, 64), value.clone());
                }
                _ => {}
            }
        }
        json!({
            "kind": self.kind.as_deref().unwrap_or("other"),
            "title": bounded(self.title.as_deref().unwrap_or(""), 2048),
            "command": raw.and_then(|r| r.get("command")).and_then(command).map(|c| bounded(&c, 2048)),
            "description": text("description", 512),
            "input": input,
        })
    }
}

pub struct HeaderTracker {
    engine: Arc<Engine>,
    pub scripts: Vec<Arc<Compiled>>,
    tools: HashMap<String, HashMap<String, Tool>>,
    senders: HashMap<String, broadcast::Sender<Value>>,
    /// `session -> (provider, cwd)` for re-evaluation after a reload.
    contexts: HashMap<String, (String, String)>,
    /// Script versions whose failure was already logged.
    failed: HashSet<String>,
    pub headers: Headers,
}

impl HeaderTracker {
    pub fn new(engine: Arc<Engine>, headers: Headers) -> Self {
        HeaderTracker {
            engine,
            scripts: Vec::new(),
            tools: HashMap::new(),
            senders: HashMap::new(),
            contexts: HashMap::new(),
            failed: HashSet::new(),
            headers,
        }
    }

    /// Apply one ACP tool update; returns a `tool_header` event when the header changed.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        session: &str,
        provider: &str,
        cwd: &str,
        seq: u64,
        data: &Value,
        tx: Option<&broadcast::Sender<Value>>,
    ) -> Option<Value> {
        let key = key(data)?;
        if let Some(tx) = tx {
            self.senders.insert(session.to_owned(), tx.clone());
        }
        if !self.contexts.contains_key(session) {
            self.contexts
                .insert(session.to_owned(), (provider.to_owned(), cwd.to_owned()));
        }
        let tools = self.tools.entry(session.to_owned()).or_default();
        let tool = tools.entry(key.clone()).or_insert_with(|| Tool {
            seq,
            title: None,
            kind: None,
            raw_input: Value::Null,
            evaluated: None,
            parent: None,
        });
        tool.seq = tool.seq.max(seq);
        // Output-only updates cannot change the header.
        let describes = ["title", "kind", "rawInput"]
            .iter()
            .any(|field| !data[field].is_null());
        if !describes && tool.evaluated.is_some() {
            return None;
        }
        if let Some(title) = data["title"].as_str() {
            tool.title = Some(title.to_owned());
        }
        if let Some(kind) = data["kind"].as_str() {
            tool.kind = Some(kind.to_owned());
        }
        if !data["rawInput"].is_null() {
            tool.raw_input = data["rawInput"].clone();
        }
        if let (Some(acp), Some(parent)) = (
            data["acpSessionId"].as_str(),
            data["parentSessionId"].as_str(),
        ) {
            tool.parent = Some((acp.to_owned(), parent.to_owned()));
        }
        if tools.len() > MAX_TOOLS_PER_SESSION
            && let Some(oldest) = tools
                .iter()
                .min_by_key(|(_, t)| t.seq)
                .map(|(k, _)| k.clone())
        {
            tools.remove(&oldest);
            if let Some(headers) = self.headers.lock().unwrap().get_mut(session) {
                headers.remove(&oldest);
            }
        }
        self.evaluate(session, provider, cwd, &key, data["toolCallId"].as_str()?)
    }

    fn evaluate(
        &mut self,
        session: &str,
        provider: &str,
        cwd: &str,
        key: &str,
        tool_call: &str,
    ) -> Option<Value> {
        let tool = self.tools.get_mut(session)?.get_mut(key)?;
        let summary = tool.summary();
        let context = json!({"provider": provider, "cwd": cwd.trim_end_matches('/'), "state": {}});
        if tool.evaluated.as_ref() == Some(&json!([context, summary])) {
            return None;
        }
        tool.evaluated = Some(json!([context, summary]));
        let (seq, parent) = (tool.seq, tool.parent.clone());
        let title = summary["title"].as_str().unwrap_or_default();
        let header = self.scripts.iter().find_map(|script| {
            if !script
                .text_pattern
                .as_ref()
                .is_some_and(|pattern| pattern.is_match(title))
            {
                return None;
            }
            match self.engine.call(script, &context, &summary) {
                Ok((value, _)) => value["result"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| bounded(s, MAX_HEADER_CHARS)),
                Err(error) => {
                    if self
                        .failed
                        .insert(format!("{}@{}", script.name, script.hash))
                    {
                        eprintln!("Tool header script failed; using the harness title: {error}");
                    }
                    None
                }
            }
        });
        let mut headers = self.headers.lock().unwrap();
        let session_headers = headers.entry(session.to_owned()).or_default();
        let previous = match &header {
            Some(header) => session_headers.insert(key.to_owned(), (seq, header.clone())),
            None => session_headers.remove(key),
        };
        if previous.map(|(_, h)| h) == header {
            return None;
        }
        let mut data = json!({"toolCallId": tool_call, "header": header});
        if let Some((acp, parent)) = parent {
            data["acpSessionId"] = json!(acp);
            data["parentSessionId"] = json!(parent);
        }
        Some(json!({"kind": "tool_header", "seq": seq, "data": data}))
    }

    /// Re-evaluate every remembered tool after the scripts changed and publish changes.
    pub fn reload(&mut self, scripts: Vec<Arc<Compiled>>) {
        self.scripts = scripts;
        let mut pending = Vec::new();
        for (session, tools) in &mut self.tools {
            for (key, tool) in tools.iter_mut() {
                tool.evaluated = None;
                pending.push((session.clone(), key.clone()));
            }
        }
        for (session, key) in pending {
            let Some((provider, cwd)) = self.contexts.get(&session).cloned() else {
                continue;
            };
            let tool_call = key.rsplit('\u{1f}').next().unwrap_or(&key).to_owned();
            if let Some(event) = self.evaluate(&session, &provider, &cwd, &key, &tool_call)
                && let Some(tx) = self.senders.get(&session)
            {
                let _ = tx.send(event);
            }
        }
    }

    pub fn forget(&mut self, session: &str) {
        self.tools.remove(session);
        self.senders.remove(session);
        self.contexts.remove(session);
        self.headers.lock().unwrap().remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> HeaderTracker {
        let engine = Arc::new(Engine::new().unwrap());
        let script = engine
            .compile_registered("tool_header.star", agentworks_starlark::TOOL_HEADER_SCRIPT)
            .unwrap();
        let mut tracker = HeaderTracker::new(engine, Headers::default());
        tracker.scripts = script.handlers.clone();
        tracker
    }

    #[test]
    fn headers_follow_raw_input_and_publish_only_changes() {
        let mut t = tracker();
        let command =
            "PID=$(ss -ltnp | grep ':4395 ') ; kill -INT $PID; git status --short | wc -l";
        let call =
            json!({"sessionUpdate":"tool_call","toolCallId":"a","title":command,"rawInput":{}});
        // Claude's first event has the command only as its title: keep the title.
        assert!(t.update("s", "claude", "/p", 1, &call, None).is_none());
        let described = json!({"sessionUpdate":"tool_call_update","toolCallId":"a","title":command,
            "rawInput":{"command":command,"description":"Stop stray server"}});
        let event = t.update("s", "claude", "/p", 2, &described, None).unwrap();
        assert_eq!(
            event,
            json!({"kind":"tool_header","seq":2,"data":{"toolCallId":"a","header":"Stop stray server"}})
        );
        let output = json!({"sessionUpdate":"tool_call_update","toolCallId":"a","_meta":{"terminal_output_delta":{"terminal_id":"t","data":"x"}}});
        assert!(
            t.update("s", "claude", "/p", 3, &output, None).is_none(),
            "unchanged input"
        );
        assert_eq!(t.headers.lock().unwrap()["s"]["a"].1, "Stop stray server");
        let plain = json!({"sessionUpdate":"tool_call","toolCallId":"b","title":"ls","rawInput":{"command":"ls"}});
        assert!(
            t.update("s", "codex", "/p", 4, &plain, None).is_none(),
            "the title is already short"
        );
        let codex = json!({"sessionUpdate":"tool_call","toolCallId":"c","title":command,"rawInput":{"command":command}});
        assert_eq!(
            t.update("s", "codex", "/p", 5, &codex, None).unwrap()["data"]["header"],
            "kill -INT $PID +2"
        );
    }

    #[test]
    fn child_tools_are_scoped_and_argv_commands_are_joined() {
        let mut t = tracker();
        let child = json!({"sessionUpdate":"tool_call","toolCallId":"a","title":"bash -lc 'cd /p && cargo test'",
            "rawInput":{"command":["/bin/bash","-lc","cd /p && cargo test"]},"acpSessionId":"child","parentSessionId":"root"});
        let event = t.update("s", "codex", "/p", 1, &child, None).unwrap();
        assert_eq!(
            event["data"],
            json!({"toolCallId":"a","header":"cargo test","acpSessionId":"child","parentSessionId":"root"})
        );
        assert!(t.headers.lock().unwrap()["s"].contains_key("child\u{1f}a"));
        t.reload(Vec::new());
        assert!(
            t.headers.lock().unwrap()["s"].is_empty(),
            "disabled scripts remove headers"
        );
        t.forget("s");
        assert!(t.tools.is_empty());
    }
}
