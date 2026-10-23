//! Compact tool events for browser snapshots, and tool bodies on demand.
//!
//! Tool output is most of a session's event bytes, and tool cards start
//! collapsed. Browser snapshots therefore keep each tool event's identity,
//! title, kind, status, locations and exit metadata, and drop `content`,
//! `rawInput`, `rawOutput` and streamed terminal output. Such events carry
//! `bodyOmitted: true`; expanding the card fetches the tool's stored events
//! from `GET /api/sessions/{id}/tools/{toolCallId}`. Live events are unchanged,
//! and nothing is removed from storage or from `GET /api/sessions/{id}`.

use crate::tool_headers;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};

/// Snapshot titles are shortened; the full title returns with the body.
const MAX_TITLE_CHARS: usize = 160;
/// Upper bound on events returned for one tool.
const MAX_BODY_EVENTS: usize = 50_000;

const BODY_FIELDS: [&str; 3] = ["content", "rawInput", "rawOutput"];
const META_BODY_FIELDS: [&str; 2] = ["terminal_output", "terminal_output_delta"];
const IDENTITY: [&str; 5] = [
    "sessionUpdate",
    "toolCallId",
    "acpSessionId",
    "parentSessionId",
    "bodyOmitted",
];

fn is_tool(event: &Value) -> bool {
    event["kind"] == "acp"
        && matches!(
            event["data"]["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        )
}

/// Strip body fields from one tool update. Returns the data and whether it
/// still says anything beyond which tool it belongs to.
fn strip(data: &Value) -> (Value, bool) {
    let mut data = data.clone();
    let Some(fields) = data.as_object_mut() else {
        return (data, false);
    };
    let mut omitted = false;
    for name in BODY_FIELDS {
        omitted |= fields.remove(name).is_some();
    }
    if let Some(meta) = fields.get_mut("_meta").and_then(Value::as_object_mut) {
        for name in META_BODY_FIELDS {
            omitted |= meta.remove(name).is_some();
        }
        // Claude repeats the tool result here.
        if let Some(claude) = meta.get_mut("claudeCode").and_then(Value::as_object_mut) {
            omitted |= claude.remove("toolResponse").is_some();
        }
        if meta.is_empty() {
            fields.remove("_meta");
        }
    }
    if let Some(title) = fields.get("title").and_then(Value::as_str)
        && let Some((end, _)) = title.char_indices().nth(MAX_TITLE_CHARS)
    {
        let short = format!("{}…", &title[..end]);
        fields.insert("title".into(), json!(short));
        omitted = true;
    }
    if omitted {
        fields.insert("bodyOmitted".into(), json!(true));
    }
    let meaningful = fields.keys().any(|k| !IDENTITY.contains(&k.as_str()));
    (data, meaningful)
}

/// Snapshot events with tool bodies removed. The first retained event of each
/// tool is always kept so its card exists; later updates that only carried
/// output are dropped. The current header goes on each tool's last event.
pub fn compact(
    events: &VecDeque<Value>,
    after: u64,
    headers: &HashMap<String, String>,
) -> Vec<Value> {
    // A resumed transcript may already have a loaded body. Keep each tool's
    // last update so output-only changes invalidate that body too.
    let latest: HashMap<String, u64> = events
        .iter()
        .filter(|e| is_tool(e))
        .filter_map(|e| Some((tool_headers::key(&e["data"])?, e["seq"].as_u64()?)))
        .collect();
    let mut seen = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    let mut last: HashMap<String, usize> = HashMap::new();
    let mut omitted = HashSet::new();
    for event in events {
        if event["seq"].as_u64().unwrap_or(0) <= after {
            if is_tool(event)
                && let Some(key) = tool_headers::key(&event["data"])
            {
                seen.insert(key);
            }
            continue;
        }
        if !is_tool(event) {
            out.push(event.clone());
            continue;
        }
        let key = tool_headers::key(&event["data"]);
        let first = key.as_ref().is_some_and(|k| seen.insert(k.clone()));
        let (data, meaningful) = strip(&event["data"]);
        if data["bodyOmitted"] == true
            && let Some(key) = &key
        {
            omitted.insert(key.clone());
        }
        let invalidate = after > 0
            && key
                .as_ref()
                .is_some_and(|k| latest.get(k) == event["seq"].as_u64().as_ref());
        if !(first || meaningful || invalidate) {
            continue;
        }
        if let Some(key) = key {
            last.insert(key, out.len());
        }
        out.push(json!({"seq": event["seq"], "kind": "acp", "data": data}));
    }
    for (key, index) in last {
        if omitted.contains(&key) {
            out[index]["data"]["bodyOmitted"] = json!(true);
        }
        if let Some(header) = headers.get(&key) {
            out[index]["data"]["header"] = json!(header);
        }
    }
    out
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyQuery {
    /// The tool's ACP session, for child-session tools and to disambiguate.
    acp_session: Option<String>,
}

/// All stored events of one tool, in order, with their full bodies.
pub async fn tool_body(
    State(app): State<crate::App>,
    Path((id, tool)): Path<(String, String)>,
    Query(query): Query<BodyQuery>,
) -> crate::ApiResult {
    let s = crate::session(&app, &id)?;
    let wanted = tool.clone();
    let matches = move |data: &Value| {
        data["toolCallId"] == wanted.as_str()
            && query
                .acp_session
                .as_deref()
                .map_or(data["parentSessionId"].is_null(), |acp| {
                    data["acpSessionId"] == acp
                })
    };
    // The retained window bounds the stored range to scan. If the tool's first
    // retained event is not its `tool_call`, earlier events may be stored.
    let (low, high) = {
        let data = s.data.lock().unwrap();
        let mut found = data
            .events
            .iter()
            .filter(|e| is_tool(e) && matches(&e["data"]));
        match found.next() {
            Some(first) => {
                let start = first["data"]["sessionUpdate"] == "tool_call";
                let low = if start {
                    first["seq"].as_u64().unwrap_or(0)
                } else {
                    0
                };
                let high = found.last().unwrap_or(first)["seq"]
                    .as_u64()
                    .unwrap_or(data.seq);
                (low, high)
            }
            None => (0, data.seq),
        }
    };
    // Stored payloads are compact JSON, so this prefilter avoids parsing unrelated rows.
    let needle = format!("\"toolCallId\":{}", json!(tool));
    let store = s.store.clone();
    let id = s.id.clone();
    let events = tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<Value>> {
        let store = store.lock().unwrap();
        let mut query = store.connection.prepare(
            "SELECT payload FROM events WHERE session_id=?1 AND seq BETWEEN ?2 AND ?3 AND instr(payload, ?4) > 0 ORDER BY seq",
        )?;
        let mut events = Vec::new();
        let rows = query.query_map(rusqlite::params![id, low as i64, high as i64, needle], |r| {
            r.get::<_, String>(0)
        })?;
        for row in rows {
            let Ok(event) = serde_json::from_str::<Value>(&row?) else {
                continue;
            };
            if is_tool(&event) && matches(&event["data"]) {
                events.push(event);
                if events.len() >= MAX_BODY_EVENTS {
                    break;
                }
            }
        }
        Ok(events)
    })
    .await
    .map_err(|_| crate::fail(StatusCode::INTERNAL_SERVER_ERROR, "Tool output lookup failed"))?
    .map_err(|e| {
        crate::fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Tool output could not be read: {e}"),
        )
    })?;
    if events.is_empty() {
        return Err(crate::fail(
            StatusCode::NOT_FOUND,
            "No stored output for this tool",
        ));
    }
    if events.len() >= MAX_BODY_EVENTS {
        return Err(crate::fail(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Tool output exceeds the event limit",
        ));
    }
    Ok(Json(json!({ "events": events })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acp(seq: u64, data: Value) -> Value {
        json!({"seq": seq, "kind": "acp", "data": data})
    }

    #[test]
    fn snapshots_keep_tool_cards_and_drop_bodies() {
        let long = "x".repeat(300);
        let events: VecDeque<Value> = [
            json!({"seq":1,"kind":"user","data":{"text":"hi"}}),
            acp(2, json!({"sessionUpdate":"tool_call","toolCallId":"a","title":long,"kind":"execute","status":"pending",
                "rawInput":{"command":long},"_meta":{"claudeCode":{"toolName":"Bash"}}})),
            acp(3, json!({"sessionUpdate":"tool_call_update","toolCallId":"a",
                "_meta":{"terminal_output_delta":{"terminal_id":"t","data":"output"}}})),
            acp(4, json!({"sessionUpdate":"tool_call_update","toolCallId":"a","status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":"result"}}],
                "_meta":{"terminal_exit":{"terminal_id":"t","exit_code":0},"claudeCode":{"toolName":"Bash","toolResponse":{"stdout":"result"}}}})),
            acp(5, json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}})),
        ]
        .into();
        let headers = HashMap::from([("a".to_owned(), "Run it".to_owned())]);
        let out = compact(&events, 0, &headers);
        assert_eq!(
            out.iter()
                .map(|e| e["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 4, 5]
        );
        let call = &out[1]["data"];
        assert_eq!(call["bodyOmitted"], true);
        assert!(call["rawInput"].is_null() && call["header"].is_null());
        assert_eq!(
            call["title"].as_str().unwrap().chars().count(),
            MAX_TITLE_CHARS + 1
        );
        assert_eq!(call["_meta"], json!({"claudeCode":{"toolName":"Bash"}}));
        let done = &out[2]["data"];
        assert_eq!(done["status"], "completed");
        assert!(done["content"].is_null());
        assert_eq!(
            done["header"], "Run it",
            "the header goes on the tool's last event"
        );
        assert_eq!(
            done["_meta"],
            json!({"terminal_exit":{"terminal_id":"t","exit_code":0},"claudeCode":{"toolName":"Bash"}})
        );
        assert_eq!(out[3], events[4], "other events are unchanged");

        // Resuming sends only newer events; a tool already seen keeps no forced card.
        let resumed = compact(&events, 2, &HashMap::new());
        assert_eq!(
            resumed
                .iter()
                .map(|e| e["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [4, 5]
        );
    }

    #[test]
    fn child_tools_are_tracked_separately() {
        let child = |seq, session: &str| {
            acp(
                seq,
                json!({"sessionUpdate":"tool_call_update","toolCallId":"a","acpSessionId":session,"parentSessionId":"root",
                "rawOutput":"x"}),
            )
        };
        let events: VecDeque<Value> = [child(1, "one"), child(2, "two"), child(3, "one")].into();
        let out = compact(&events, 0, &HashMap::new());
        assert_eq!(out.len(), 2, "the first event of each child tool is kept");
        assert_eq!(out[1]["data"]["acpSessionId"], "two");
    }

    #[test]
    fn resumed_output_only_updates_invalidate_loaded_bodies() {
        let events = [
            acp(
                1,
                json!({"sessionUpdate":"tool_call","toolCallId":"a","title":"Run"}),
            ),
            acp(
                2,
                json!({"sessionUpdate":"tool_call_update","toolCallId":"a","rawOutput":"first"}),
            ),
            acp(
                3,
                json!({"sessionUpdate":"tool_call_update","toolCallId":"a","rawOutput":"latest"}),
            ),
        ]
        .into();
        let out = compact(&events, 1, &HashMap::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["seq"], 3);
        assert_eq!(out[0]["data"]["bodyOmitted"], true);
        assert!(out[0]["data"]["rawOutput"].is_null());
    }

    #[test]
    fn dropped_output_marks_a_card_without_initial_body_fields() {
        let events = [
            acp(
                1,
                json!({"sessionUpdate":"tool_call","toolCallId":"a","title":"Run"}),
            ),
            acp(
                2,
                json!({"sessionUpdate":"tool_call_update","toolCallId":"a","rawOutput":"output"}),
            ),
        ]
        .into();
        let out = compact(&events, 0, &HashMap::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["data"]["bodyOmitted"], true);
    }
}
