//! HTTP retrieval of stored tool bodies; snapshot projections live in presentation.
use agentworks_presentation::transcript::is_tool;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde_json::{Value, json};

/// Upper bound on events returned for one tool.
const MAX_BODY_EVENTS: usize = 50_000;

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
        let window = s.window(&data);
        let mut found = window.iter().filter(|e| is_tool(e) && matches(&e["data"]));
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
    let events =
        tokio::task::spawn_blocking(move || -> agentworks_database::StorageResult<Vec<Value>> {
            let store = store.lock().unwrap();
            store.tool_body_events(&id, low, high, &needle, MAX_BODY_EVENTS, |event| {
                is_tool(event) && matches(&event["data"])
            })
        })
        .await
        .map_err(|_| {
            crate::fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Tool output lookup failed",
            )
        })?
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
