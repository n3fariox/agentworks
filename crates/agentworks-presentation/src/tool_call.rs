//! Tool identity and bounded, display-only summaries supplied to formatting hooks.
use serde_json::{Map, Value, json};

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

pub struct ToolCall {
    pub title: Option<String>,
    pub kind: Option<String>,
    pub raw_input: Value,
}

impl ToolCall {
    pub fn summary(&self) -> Value {
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

/// Accept nonempty hook output as a bounded tool-card heading.
pub fn header(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| bounded(text, 200))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_bound_unicode_text_and_exclude_nested_raw_payloads() {
        let call = ToolCall {
            title: Some("é".repeat(2100)),
            kind: None,
            raw_input: json!({
                "command": ["/bin/bash", "-lc", "cd /w && cargo test"],
                "description": "d".repeat(600),
                "path": "p".repeat(300),
                "payload": {"secret": "not display input"},
                "count": 3
            }),
        };
        let summary = call.summary();
        assert_eq!(summary["title"].as_str().unwrap().chars().count(), 2048);
        assert_eq!(summary["command"], "cd /w && cargo test");
        assert_eq!(summary["description"].as_str().unwrap().len(), 512);
        assert_eq!(summary["input"]["path"].as_str().unwrap().len(), 200);
        assert_eq!(summary["input"]["count"], 3);
        assert!(summary["input"]["payload"].is_null());
        assert_eq!(header(&"é".repeat(201)).unwrap().chars().count(), 200);
        assert_eq!(header(" \n "), None);
    }
}
